//! Single-line text input (optionally masked) with a custom element that
//! shapes the text, scrolls horizontally to keep the caret visible and paints the caret.
mod ime;

use super::icon::{Icon, IconName};
use crate::theme::ActiveTheme as _;
use gpui::prelude::*;
use gpui::{
    App, Bounds, ClipboardItem, ContentMask, Context, CursorStyle, DispatchPhase, Element,
    ElementId, ElementInputHandler, EventEmitter, FocusHandle, Focusable, GlobalElementId,
    InspectorElementId, KeyDownEvent, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, PaintQuad, Pixels, ShapedLine, SharedString, Style, Task, TextRun, Window, div,
    fill, point, px, relative, size, transparent_black,
};
use std::time::Duration;

const CARET_BLINK: Duration = Duration::from_millis(500);

pub enum TextInputEvent {
    Changed,
    Submit,
    Cancel,
}

pub struct TextInput {
    focus: FocusHandle,
    text: String,
    /// Byte offset of the caret.
    cursor: usize,
    /// The other end of the selection, whose moving end is the caret.
    anchor: Option<usize>,
    /// The left button went down here and is still held.
    selecting: bool,
    caret_visible: bool,
    /// Runs while the field has the focus in the active window.
    caret_blink: Option<Task<()>>,
    /// Where the caret was when it last started blinking: moving it keeps it lit.
    caret_blink_from: (usize, usize),
    masked: bool,
    /// No box around it until it has the focus: for a field that sits in a toolbar.
    quiet: bool,
    /// No box at all: it sits in something that is already one.
    bare: bool,
    /// A word before the text that says what it takes, for the field a dialog is about.
    prompt: Option<&'static str>,
    marked: Option<std::ops::Range<usize>>,
    placeholder: SharedString,
    scroll_x: Pixels,
    last_line: Option<ShapedLine>,
    last_bounds: Option<Bounds<Pixels>>,
}

impl EventEmitter<TextInputEvent> for TextInput {}

impl Focusable for TextInput {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl TextInput {
    pub fn new(placeholder: &str, masked: bool, cx: &mut Context<Self>) -> Self {
        Self {
            focus: cx.focus_handle().tab_stop(true),
            text: String::new(),
            cursor: 0,
            anchor: None,
            selecting: false,
            caret_visible: true,
            caret_blink: None,
            caret_blink_from: (0, 0),
            masked,
            quiet: false,
            bare: false,
            prompt: None,
            marked: None,
            placeholder: SharedString::from(placeholder.to_string()),
            scroll_x: px(0.),
            last_line: None,
            last_bounds: None,
        }
    }

    pub fn bare(mut self) -> Self {
        self.bare = true;
        self
    }

    pub fn quiet(mut self) -> Self {
        self.quiet = true;
        self
    }

    /// Taller and in the terminal's font, after `prompt`.
    pub fn command(mut self, prompt: &'static str) -> Self {
        self.prompt = Some(prompt);
        self
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn set_placeholder(&mut self, placeholder: &str, cx: &mut Context<Self>) {
        if self.placeholder != placeholder {
            self.placeholder = SharedString::from(placeholder.to_string());
            cx.notify();
        }
    }

    pub fn set_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.text = text.to_string();
        self.cursor = self.text.len();
        self.anchor = None;
        self.marked = None;
        cx.notify();
    }

    pub fn focus(&self, window: &mut Window) {
        window.focus(&self.focus);
    }

    pub fn select_all(&mut self, cx: &mut Context<Self>) {
        self.anchor = Some(0);
        self.cursor = self.text.len();
        cx.notify();
    }

    /// What is selected, if anything.
    fn selection(&self) -> Option<std::ops::Range<usize>> {
        let anchor = self.anchor.filter(|anchor| *anchor != self.cursor)?;
        Some(anchor.min(self.cursor)..anchor.max(self.cursor))
    }

    /// Returns whether there was something to delete.
    fn delete_selection(&mut self) -> bool {
        let Some(range) = self.selection() else {
            return false;
        };
        self.cursor = range.start;
        self.text.replace_range(range, "");
        self.anchor = None;
        true
    }

    fn insert(&mut self, s: &str, cx: &mut Context<Self>) {
        let s: String = s.chars().filter(|c| !c.is_control()).collect();
        if s.is_empty() {
            return;
        }
        self.delete_selection();
        self.anchor = None;
        self.text.insert_str(self.cursor, &s);
        self.cursor += s.len();
        self.changed(cx);
    }

    fn changed(&mut self, cx: &mut Context<Self>) {
        cx.emit(TextInputEvent::Changed);
        cx.notify();
    }

    fn prev_boundary(&self) -> usize {
        self.text[..self.cursor]
            .char_indices()
            .next_back()
            .map_or(0, |(i, _)| i)
    }

    fn next_boundary(&self) -> usize {
        self.text[self.cursor..]
            .chars()
            .next()
            .map_or(self.cursor, |c| self.cursor + c.len_utf8())
    }

    fn backspace(&mut self, cx: &mut Context<Self>) {
        if !self.delete_selection() {
            if self.cursor == 0 {
                return;
            }
            let start = self.prev_boundary();
            self.text.replace_range(start..self.cursor, "");
            self.cursor = start;
            self.anchor = None;
        }
        self.changed(cx);
    }

    fn delete(&mut self, cx: &mut Context<Self>) {
        if !self.delete_selection() {
            let end = self.next_boundary();
            if end == self.cursor {
                return;
            }
            self.text.replace_range(self.cursor..end, "");
            self.anchor = None;
        }
        self.changed(cx);
    }

    /// `extend` keeps the selection's other end where it is, or starts one here.
    fn move_to(&mut self, cursor: usize, extend: bool, cx: &mut Context<Self>) {
        if extend {
            self.anchor.get_or_insert(self.cursor);
        } else {
            self.anchor = None;
        }
        self.cursor = cursor;
        cx.notify();
    }

    fn byte_at_x(&self, x: Pixels) -> Option<usize> {
        let (line, bounds) = (self.last_line.as_ref()?, self.last_bounds?);
        let index = line.closest_index_for_x(x - bounds.left() + self.scroll_x);
        Some(self.display_to_byte(index))
    }

    fn drag_to(&mut self, x: Pixels, cx: &mut Context<Self>) {
        if let Some(byte) = self.byte_at_x(x)
            && byte != self.cursor
        {
            self.cursor = byte;
            cx.notify();
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let key = &event.keystroke;
        // AltGr is reported as ctrl+alt: that is typing, not a shortcut.
        let alt_gr = key.modifiers.control && key.modifiers.alt;
        let command = (key.modifiers.control && !alt_gr) || key.modifiers.platform;
        let extend = key.modifiers.shift;
        match key.key.as_str() {
            "enter" => cx.emit(TextInputEvent::Submit),
            "escape" => cx.emit(TextInputEvent::Cancel),
            "backspace" => self.backspace(cx),
            "delete" => self.delete(cx),
            "left" => {
                let to = match self.selection() {
                    Some(selected) if !extend => selected.start,
                    _ => self.prev_boundary(),
                };
                self.move_to(to, extend, cx)
            }
            "right" => {
                let to = match self.selection() {
                    Some(selected) if !extend => selected.end,
                    _ => self.next_boundary(),
                };
                self.move_to(to, extend, cx)
            }
            "home" => self.move_to(0, extend, cx),
            "end" => self.move_to(self.text.len(), extend, cx),
            "a" if command => self.select_all(cx),
            "v" if command => {
                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    self.insert(&text, cx);
                }
            }
            "c" | "x" if command && !self.masked => {
                if let Some(selected) = self.selection() {
                    cx.write_to_clipboard(ClipboardItem::new_string(
                        self.text[selected].to_string(),
                    ));
                    if key.key == "x" {
                        self.backspace(cx);
                    }
                }
            }
            _ if command || key.modifiers.alt && !alt_gr => return,
            // Let the platform deliver text, spaces, dead keys and IME commits.
            _ => return,
        }
        cx.stop_propagation();
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus);
        let Some(byte) = self.byte_at_x(event.position.x) else {
            return;
        };
        match event.click_count {
            // A masked field has no words to show.
            2 if !self.masked => {
                let word = word_at(&self.text, byte);
                self.anchor = Some(word.start);
                self.cursor = word.end;
            }
            2.. => self.select_all(cx),
            _ => {
                if event.modifiers.shift {
                    self.anchor.get_or_insert(self.cursor);
                } else {
                    self.anchor = Some(byte);
                }
                self.cursor = byte;
                self.selecting = true;
            }
        }
        cx.notify();
    }

    /// Lit for a full interval from now, then on and off.
    fn start_caret_blink(&mut self, cx: &mut Context<Self>) {
        self.caret_visible = true;
        self.caret_blink = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(CARET_BLINK).await;
                let blinked = this.update(cx, |this, cx| {
                    this.caret_visible = !this.caret_visible;
                    cx.notify();
                });
                if blinked.is_err() {
                    break;
                }
            }
        }));
    }

    /// The text actually shaped: bullets when masked.
    fn display(&self) -> String {
        if self.masked {
            "•".repeat(self.text.chars().count())
        } else {
            self.text.clone()
        }
    }

    fn byte_to_display(&self, byte: usize) -> usize {
        if self.masked {
            self.text[..byte].chars().count() * '•'.len_utf8()
        } else {
            byte
        }
    }

    fn display_to_byte(&self, index: usize) -> usize {
        if !self.masked {
            return index.min(self.text.len());
        }
        let chars = index / '•'.len_utf8();
        self.text
            .char_indices()
            .nth(chars)
            .map_or(self.text.len(), |(i, _)| i)
    }
}

/// The word around a position in a text. Only spaces and path separators end one, so that a double
/// click picks a whole folder or file name.
fn word_at(text: &str, byte: usize) -> std::ops::Range<usize> {
    let ends = |c: char| c.is_whitespace() || matches!(c, '/' | '\\');
    let start = text[..byte].rfind(ends).map_or(0, |index| {
        index + text[index..].chars().next().map_or(0, char::len_utf8)
    });
    let end = text[byte..]
        .find(ends)
        .map_or(text.len(), |index| byte + index);
    start..end
}

impl Render for TextInput {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus.is_focused(window);
        if focused && window.is_window_active() {
            let caret = (self.cursor, self.text.len());
            if self.caret_blink.is_none() || caret != self.caret_blink_from {
                self.caret_blink_from = caret;
                self.start_caret_blink(cx);
            }
        } else {
            self.caret_blink = None;
            self.caret_visible = true;
        }
        let theme = cx.theme();
        div()
            .track_focus(&self.focus)
            .key_context("TextInput")
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .cursor(CursorStyle::IBeam)
            .w_full()
            .flex()
            .items_center()
            .h(px(28.))
            .px(px(8.))
            .rounded(px(7.))
            .border_1()
            .border_color(if self.bare {
                transparent_black()
            } else if focused {
                theme.accent
            } else {
                theme.border
            })
            .when(!self.quiet && !self.bare, |this| this.bg(theme.input_bg))
            .when(self.bare, |this| {
                this.h(px(30.))
                    .px(px(0.))
                    .text_size(crate::theme::ui_px(15.))
            })
            .text_size(crate::theme::ui_px(13.))
            .text_color(theme.text)
            .when(self.quiet, |this| {
                this.gap(px(6.))
                    .child(Icon::new(IconName::Search).size(px(13.)).color(if focused {
                        theme.text_muted
                    } else {
                        theme.text_disabled
                    }))
            })
            .when_some(self.prompt, |this, prompt| {
                this.h(px(38.))
                    .px(px(11.))
                    .gap(px(9.))
                    .font_family(crate::theme::mono_font())
                    .text_size(crate::theme::ui_px(14.))
                    .child(
                        div()
                            .flex_none()
                            .text_color(theme.text_disabled)
                            .child(prompt),
                    )
            })
            .child(TextElement { input: cx.entity() })
    }
}

struct TextElement {
    input: gpui::Entity<TextInput>,
}

struct Prepaint {
    line: ShapedLine,
    caret: PaintQuad,
    selection: Option<PaintQuad>,
    scroll_x: Pixels,
}

impl IntoElement for TextElement {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TextElement {
    type RequestLayoutState = ();
    type PrepaintState = Prepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = window.line_height().into();
        style.min_size.width = px(0.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _state: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Prepaint {
        let theme = cx.theme().clone();
        let input = self.input.read(cx);
        let style = window.text_style();
        let empty = input.text.is_empty();
        let (text, color) = if empty {
            (input.placeholder.clone(), theme.text_disabled)
        } else {
            (SharedString::from(input.display()), style.color)
        };
        let run = TextRun {
            len: text.len(),
            font: style.font(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line = window
            .text_system()
            .shape_line(text, font_size, &[run], None);
        let caret_x = if empty {
            px(0.)
        } else {
            line.x_for_index(input.byte_to_display(input.cursor))
        };
        let width = bounds.size.width;
        let mut scroll_x = input.scroll_x;
        if caret_x - scroll_x > width - px(2.) {
            scroll_x = caret_x - width + px(2.);
        }
        if caret_x < scroll_x {
            scroll_x = caret_x;
        }
        if line.width <= width {
            scroll_x = px(0.);
        }
        let selection = input.selection().filter(|_| !empty).map(|selected| {
            let start = line.x_for_index(input.byte_to_display(selected.start));
            let end = line.x_for_index(input.byte_to_display(selected.end));
            fill(
                Bounds::new(
                    point(bounds.left() + start - scroll_x, bounds.top()),
                    size(end - start, bounds.size.height),
                ),
                theme.terminal_selection,
            )
        });
        let caret = fill(
            Bounds::new(
                point(bounds.left() + caret_x - scroll_x, bounds.top() + px(1.)),
                size(px(1.), bounds.size.height - px(2.)),
            ),
            theme.text,
        );
        Prepaint {
            line,
            caret,
            selection,
            scroll_x,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _state: &mut (),
        prepaint: &mut Prepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus = self.input.read(cx).focus.clone();
        window.handle_input(
            &focus,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        let focused = focus.is_focused(window);
        let caret_visible = self.input.read(cx).caret_visible;
        let line_height = window.line_height();
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            if let Some(selection) = prepaint.selection.take() {
                window.paint_quad(selection);
            }
            let origin = point(bounds.left() - prepaint.scroll_x, bounds.top());
            let _ = prepaint.line.paint(origin, line_height, window, cx);
            if focused && caret_visible {
                window.paint_quad(prepaint.caret.clone());
            }
        });
        // On the window, because a selection goes on when the pointer leaves the field.
        let input = self.input.clone();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
            if phase == DispatchPhase::Bubble && input.read(cx).selecting {
                input.update(cx, |input, cx| {
                    if event.pressed_button == Some(MouseButton::Left) {
                        input.drag_to(event.position.x, cx);
                    } else {
                        input.selecting = false;
                    }
                });
            }
        });
        let input = self.input.clone();
        window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
            if phase == DispatchPhase::Bubble && input.read(cx).selecting {
                input.update(cx, |input, _| input.selecting = false);
            }
        });
        let line = prepaint.line.clone();
        let scroll_x = prepaint.scroll_x;
        self.input.update(cx, |input, _| {
            input.last_line = Some(line);
            input.last_bounds = Some(bounds);
            input.scroll_x = scroll_x;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::word_at;

    #[test]
    fn a_word_ends_at_path_separators_and_spaces() {
        let path = "/home/ubuntu/my-repo.git/src";
        assert_eq!(&path[word_at(path, 8)], "ubuntu");
        assert_eq!(&path[word_at(path, 15)], "my-repo.git");
        assert_eq!(&path[word_at(path, path.len())], "src");
        assert_eq!(&"two words"[word_at("two words", 1)], "two");
    }

    #[test]
    fn a_word_can_hold_any_character() {
        let path = "/città/è";
        assert_eq!(&path[word_at(path, 3)], "città");
        assert_eq!(&path[word_at(path, path.len())], "è");
    }
}
