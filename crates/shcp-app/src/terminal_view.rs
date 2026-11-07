//! Terminal tab: owns a `TerminalHandle`, routes keyboard/mouse input to it and paints it
//! with `TerminalElement`. Shows a status overlay while the session is not usable.
mod ime;

use crate::actions::{CopySelection, FindInTerminal, PasteClipboard};
use crate::clipboard::{self, Clip};
use crate::login_prompt::present;
use crate::session_overlay::{self, Overlay, Surface};
use crate::state::AppState;
use crate::terminal_element::{SharedMetrics, TerminalElement};
use crate::terminal_input::{self, Input};
use crate::theme::ActiveTheme as _;
use crate::widgets::button::{Button, ButtonStyle};
use crate::widgets::icon::IconName;
use crate::widgets::scrollbar::{ScrollDrag, ScrollMetrics, scrollbar};
use crate::widgets::text_input::{TextInput, TextInputEvent};
use gpui::prelude::*;
use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, Entity, ExternalPaths, FocusHandle,
    Focusable, Font, FontFeatures, FontStyle, FontWeight, KeyDownEvent, ModifiersChangedEvent,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, ScrollWheelEvent,
    SharedString, Subscription, Task, Window, div, px,
};
use shcp_core::term::{self, GridPoint, MouseAction, MouseInput, SelectKind, TerminalLink};
use shcp_core::{
    Event, HostId, NoticeLevel, PasteFile, RequestId, SessionId, SessionState, TerminalHandle,
};
use std::{cell::Cell, collections::HashSet, rc::Rc, time::Duration};

struct Selecting {
    origin: GridPoint,
    started: bool,
}

pub struct TerminalView {
    state: Entity<AppState>,
    host: HostId,
    session: SessionId,
    terminal: TerminalHandle,
    focus: FocusHandle,
    metrics: SharedMetrics,
    selecting: Option<Selecting>,
    scroll_remainder: f32,
    composing: String,
    font_family: SharedString,
    /// The settings value `font_family` was resolved from.
    configured_font: String,
    /// Button pressed while the application owns the mouse (mouse tracking mode).
    reported_button: Option<term::MouseButton>,
    last_reported: Option<GridPoint>,
    was_focused: bool,
    scroll_drag: ScrollDrag,
    cursor_visible: bool,
    cursor_blink: Option<Task<()>>,
    /// Files on their way to the server; their path is pasted when they arrive.
    pending_files: HashSet<RequestId>,
    search_input: Entity<TextInput>,
    search_open: bool,
    /// The last search found nothing.
    search_missed: bool,
    /// The link under the pointer.
    hovered_link: Option<TerminalLink>,
    /// Ctrl is down, so a click on the link opens it.
    link_armed: bool,
    /// It has shown a shell, so it has output worth keeping in sight while disconnected.
    had_session: bool,
    _subscriptions: Vec<Subscription>,
}

/// The configured family if installed, else Consolas on Windows (always present).
fn resolve_font(configured: &str, cx: &App) -> SharedString {
    let installed = cx
        .text_system()
        .all_font_names()
        .iter()
        .any(|name| name.eq_ignore_ascii_case(configured));
    if cfg!(windows) && !installed {
        SharedString::from("Consolas")
    } else {
        SharedString::from(configured.to_string())
    }
}

fn term_button(button: MouseButton) -> Option<term::MouseButton> {
    match button {
        MouseButton::Left => Some(term::MouseButton::Left),
        MouseButton::Middle => Some(term::MouseButton::Middle),
        MouseButton::Right => Some(term::MouseButton::Right),
        _ => None,
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl TerminalView {
    pub fn new(
        state: Entity<AppState>,
        host: HostId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let configured_font = state.read(cx).settings.terminal_font_family.clone();
        let font_family = resolve_font(&configured_font, cx);
        let session = state.update(cx, |s, cx| s.connect_host(&host, cx));
        let terminal = state.read(cx).engine.open_terminal(session, 80, 24);
        let search_input = cx.new(|cx| TextInput::new("Find", false, cx));
        let focus = cx.focus_handle();
        let subscriptions = vec![
            cx.on_focus(&focus, window, |_, _, cx| cx.notify()),
            cx.on_blur(&focus, window, |this, _, cx| {
                this.cursor_blink = None;
                this.cursor_visible = true;
                cx.notify();
            }),
            cx.subscribe(&state, |this, _, event: &Event, cx| {
                if let Event::PastedFile { req, result } = event {
                    if this.pending_files.remove(req) {
                        this.on_file_pasted(result, cx);
                    }
                    return;
                }
                // Another tab or the sidebar brought the host back: this tab follows.
                if let Event::Session {
                    id,
                    host,
                    state: SessionState::Connected,
                } = event
                    && *host == this.host
                    && *id != this.session
                    && this.session_is_gone(cx)
                {
                    this.attach(*id, cx);
                    return;
                }
                let relevant = match event {
                    Event::TerminalChanged(id) | Event::TerminalExited { id, .. } => {
                        *id == this.terminal.id()
                    }
                    Event::Session { id, .. } => *id == this.session,
                    Event::TerminalTitle { id, .. } => *id == this.terminal.id(),
                    _ => false,
                };
                if relevant {
                    cx.notify();
                }
            }),
            cx.subscribe_in(
                &search_input,
                window,
                |this, input, event: &TextInputEvent, window, cx| match event {
                    TextInputEvent::Changed => {
                        let query = input.read(cx).text().to_string();
                        this.search_missed = !this.terminal.search(&query);
                        cx.notify();
                    }
                    TextInputEvent::Submit => this.search_step(true, cx),
                    TextInputEvent::Cancel => this.close_search(window, cx),
                },
            ),
        ];
        let this = Self {
            state,
            host,
            session,
            terminal,
            focus,
            metrics: Rc::new(Cell::new(None)),
            selecting: None,
            scroll_remainder: 0.,
            composing: String::new(),
            font_family,
            configured_font,
            reported_button: None,
            last_reported: None,
            was_focused: false,
            scroll_drag: ScrollDrag::default(),
            cursor_visible: true,
            cursor_blink: None,
            pending_files: HashSet::new(),
            search_input,
            search_open: false,
            search_missed: false,
            hovered_link: None,
            link_armed: false,
            had_session: false,
            _subscriptions: subscriptions,
        };
        this.send_startup_command(cx);
        this
    }

    pub fn area(&self) -> Option<Bounds<Pixels>> {
        self.metrics.get().map(|m| m.bounds)
    }

    /// The terminal or its search bar has the keyboard.
    pub fn contains_focus(&self, window: &Window, cx: &App) -> bool {
        self.focus.contains_focused(window, cx)
            || self
                .search_input
                .focus_handle(cx)
                .contains_focused(window, cx)
    }

    pub fn search_is_open(&self) -> bool {
        self.search_open
    }

    pub fn selected_text(&self) -> Option<String> {
        self.terminal.selection_text().filter(|t| !t.is_empty())
    }

    /// Types a command and presses Enter.
    pub fn run_command(&mut self, command: &str, cx: &mut Context<Self>) {
        self.reset_cursor_blink(cx);
        self.terminal.text(&format!("{command}\r"));
        cx.notify();
    }

    fn send_startup_command(&self, cx: &App) {
        let settings = &self.state.read(cx).settings;
        if let Some(command) = settings.startup_commands.get(&self.host.0) {
            // It waits in the queue until the shell is up.
            self.terminal.text(&format!("{command}\r"));
        }
    }

    fn session_is_gone(&self, cx: &App) -> bool {
        self.state.read(cx).session(self.session).is_none_or(|s| {
            matches!(
                s.state,
                SessionState::Disconnected { .. } | SessionState::Failed { .. }
            )
        })
    }

    /// The connection is up and this terminal has no shell: it ended, or the server refused one.
    pub fn shell_gone(&self, cx: &App) -> bool {
        !self.terminal.is_alive() && !self.session_is_gone(cx)
    }

    /// A new shell on `session`, in the same terminal: what it showed stays in the scrollback.
    fn attach(&mut self, session: SessionId, cx: &mut Context<Self>) {
        self.session = session;
        let engine = self.state.read(cx).engine.clone();
        engine.reopen_terminal(session, &self.terminal, "reconnected");
        self.composing.clear();
        self.reported_button = None;
        self.send_startup_command(cx);
        cx.notify();
    }

    pub fn title(&self) -> Option<String> {
        self.terminal.title().filter(|t| !t.is_empty())
    }

    /// The folder the shell is in, as far as it says. `~` stands for its home.
    pub fn cwd(&self) -> Option<String> {
        self.terminal.cwd()
    }

    /// Releases the backend terminal; call when the tab is closed.
    pub fn close(&self, cx: &App) {
        self.state
            .read(cx)
            .engine
            .close_terminal(self.terminal.id());
    }

    fn retry(&mut self, cx: &mut Context<Self>) {
        // Still connecting from an automatic attempt: this terminal is already queued on it.
        let session = self
            .state
            .update(cx, |s, cx| s.connect_host(&self.host, cx));
        if session != self.session || !self.terminal.is_alive() {
            self.attach(session, cx);
        }
    }

    fn open_search(&mut self, _: &FindInTerminal, window: &mut Window, cx: &mut Context<Self>) {
        // What is selected is usually what to look for.
        let selected = self.terminal.selection_text().filter(|t| !t.contains('\n'));
        if let Some(text) = selected.filter(|t| !t.is_empty()) {
            self.search_input
                .update(cx, |input, cx| input.set_text(&text, cx));
        }
        self.search_open = true;
        let query = self.search_input.read(cx).text().to_string();
        self.search_missed = !self.terminal.search(&query);
        self.search_input.read(cx).focus(window);
        cx.notify();
    }

    fn close_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_open = false;
        self.search_missed = false;
        self.terminal.search_clear();
        self.terminal.scroll_to_bottom();
        window.focus(&self.focus);
        cx.notify();
    }

    fn search_step(&mut self, older: bool, cx: &mut Context<Self>) {
        self.search_missed = !self.terminal.search_step(older);
        cx.notify();
    }

    /// Follows the pointer: the link under it stands out and says where it goes. Ctrl makes it
    /// clickable. A selection being dragged has nothing to do with links.
    fn update_hovered_link(&mut self, position: Point<Pixels>, ctrl: bool, cx: &mut Context<Self>) {
        let inside = self
            .metrics
            .get()
            .is_some_and(|m| m.bounds.contains(&position));
        let link = (inside && self.selecting.is_none())
            .then(|| self.grid_point(position))
            .flatten()
            .and_then(|point| self.terminal.link_at(point));
        if link != self.hovered_link || ctrl != self.link_armed {
            self.hovered_link = link;
            self.link_armed = ctrl;
            cx.notify();
        }
    }

    /// Keep the caret visible while typing, then resume blinking after a full interval.
    fn reset_cursor_blink(&mut self, cx: &mut Context<Self>) {
        self.cursor_visible = true;
        if self.cursor_blink.is_some() {
            self.start_cursor_blink(cx);
        }
        cx.notify();
    }

    fn start_cursor_blink(&mut self, cx: &mut Context<Self>) {
        self.cursor_visible = true;
        self.cursor_blink = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(500))
                    .await;
                if this
                    .update(cx, |this, cx| {
                        this.cursor_visible = !this.cursor_visible;
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.reset_cursor_blink(cx);
        // With text selected Ctrl+C copies it, as in Windows Terminal; otherwise it interrupts.
        let keys = &event.keystroke;
        if keys.key == "c"
            && keys.modifiers.control
            && !keys.modifiers.shift
            && !keys.modifiers.alt
            && let Some(text) = self.terminal.selection_text().filter(|t| !t.is_empty())
        {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            self.terminal.select_clear();
            cx.notify();
            cx.stop_propagation();
            return;
        }
        match terminal_input::from_keystroke(&event.keystroke) {
            // Platform text input handles layout, dead keys, AltGr and IME commits.
            Some(Input::Text) => return,
            Some(Input::Key(key)) => self.terminal.key(key),
            None => return,
        }
        cx.notify();
        cx.stop_propagation();
    }

    fn copy(&mut self, _: &CopySelection, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.terminal.selection_text().filter(|t| !t.is_empty()) {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    /// `false`: the clipboard has nothing to paste.
    pub fn paste_clipboard(&mut self, cx: &mut Context<Self>) -> bool {
        self.reset_cursor_blink(cx);
        match clipboard::read(cx) {
            Some(Clip::Text(text)) => self.terminal.paste(&text),
            Some(Clip::Image { name, data }) => {
                let name = name.to_string();
                self.send_file(&name.clone(), PasteFile::Bytes { name, data }, cx);
            }
            None => return false,
        }
        cx.notify();
        true
    }

    fn paste(&mut self, _: &PasteClipboard, _window: &mut Window, cx: &mut Context<Self>) {
        // With nothing to paste Ctrl+V keeps its meaning for the program in the terminal.
        if !self.paste_clipboard(cx) {
            self.terminal.text("\x16");
        }
    }

    fn send_file(&mut self, label: &str, file: PasteFile, cx: &mut Context<Self>) {
        let message = format!("Sending {label} to the server…");
        let session = self.session;
        let req = self.state.update(cx, |state, cx| {
            state.push_notice(NoticeLevel::Info, message, cx);
            state.engine.paste_file(session, file)
        });
        self.pending_files.insert(req);
    }

    fn on_file_pasted(&mut self, result: &Result<String, String>, cx: &mut Context<Self>) {
        match result {
            // The space keeps several dropped files apart.
            Ok(path) => self.terminal.paste(&format!("{path} ")),
            Err(error) => {
                let message = format!("Could not send the file to the server: {error}");
                self.state.update(cx, |state, cx| {
                    state.push_notice(NoticeLevel::Error, message, cx)
                });
            }
        }
        cx.notify();
    }

    fn grid_point(&self, position: Point<Pixels>) -> Option<GridPoint> {
        let m = self.metrics.get()?;
        let col = ((position.x - m.bounds.left()) / m.cell_width).floor();
        let row = ((position.y - m.bounds.top()) / m.line_height).floor();
        Some(GridPoint {
            col: col.clamp(0., (m.cols - 1) as f32) as u16,
            row: row.clamp(0., (m.rows - 1) as f32) as u16,
        })
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus);
        self.reset_cursor_blink(cx);
        if event.button == MouseButton::Left
            && event.modifiers.control
            && let Some(link) = self
                .grid_point(event.position)
                .and_then(|point| self.terminal.link_at(point))
        {
            cx.open_url(&link.url);
            cx.stop_propagation();
            return;
        }
        // Applications with mouse tracking (herdr, tmux, htop…) get the click; Shift keeps
        // local selection and paste available, as in other terminals.
        if self.terminal.mouse_mode()
            && !event.modifiers.shift
            && let (Some(button), Some(point)) =
                (term_button(event.button), self.grid_point(event.position))
        {
            let input = MouseInput {
                action: MouseAction::Press,
                button: Some(button),
                point,
                ctrl: event.modifiers.control,
                alt: event.modifiers.alt,
                shift: false,
            };
            if self.terminal.mouse(input) {
                self.terminal.select_clear();
                self.reported_button = Some(button);
                self.last_reported = Some(point);
                cx.stop_propagation();
                return;
            }
        }
        if event.button == MouseButton::Middle {
            return;
        }
        if event.button == MouseButton::Right {
            self.paste_clipboard(cx);
            return;
        }
        let Some(point) = self.grid_point(event.position) else {
            return;
        };
        match event.click_count {
            1 => {
                self.terminal.select_clear();
                self.selecting = Some(Selecting {
                    origin: point,
                    started: false,
                });
            }
            count => {
                let kind = if count == 2 {
                    SelectKind::Word
                } else {
                    SelectKind::Line
                };
                self.terminal.select_begin(point, kind);
                self.selecting = Some(Selecting {
                    origin: point,
                    started: true,
                });
            }
        }
        cx.notify();
    }

    pub fn mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.update_hovered_link(event.position, event.modifiers.control, cx);
        if self.reported_button.is_some()
            || (self.selecting.is_none() && self.terminal.mouse_mode() && !event.modifiers.shift)
        {
            let inside = self
                .metrics
                .get()
                .is_some_and(|m| m.bounds.contains(&event.position));
            let Some(point) = self.grid_point(event.position) else {
                return;
            };
            if (self.reported_button.is_none() && !inside) || self.last_reported == Some(point) {
                return;
            }
            self.last_reported = Some(point);
            self.terminal.mouse(MouseInput {
                action: MouseAction::Move,
                button: self.reported_button,
                point,
                ctrl: event.modifiers.control,
                alt: event.modifiers.alt,
                shift: false,
            });
            return;
        }
        if event.pressed_button != Some(MouseButton::Left) {
            self.selecting = None;
            return;
        }
        let Some(point) = self.grid_point(event.position) else {
            return;
        };
        let Some(selecting) = &mut self.selecting else {
            return;
        };
        if !selecting.started {
            if point == selecting.origin {
                return;
            }
            self.terminal
                .select_begin(selecting.origin, SelectKind::Simple);
            selecting.started = true;
        }
        self.terminal.select_update(point);
        cx.notify();
    }

    pub fn mouse_up(&mut self, event: &MouseUpEvent, _cx: &mut Context<Self>) {
        self.selecting = None;
        if let Some(button) = self.reported_button.take()
            && term_button(event.button) == Some(button)
            && let Some(point) = self.grid_point(event.position)
        {
            self.terminal.mouse(MouseInput {
                action: MouseAction::Release,
                button: Some(button),
                point,
                ctrl: event.modifiers.control,
                alt: event.modifiers.alt,
                shift: false,
            });
        }
    }

    fn on_scroll(
        &mut self,
        event: &ScrollWheelEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(m) = self.metrics.get() else { return };
        self.hovered_link = None;
        self.scroll_remainder += event.delta.pixel_delta(m.line_height).y / m.line_height;
        let lines = self.scroll_remainder.trunc();
        self.scroll_remainder -= lines;
        if lines != 0.
            && self.terminal.mouse_mode()
            && !event.modifiers.shift
            && let Some(point) = self.grid_point(event.position)
        {
            let button = if lines > 0. {
                term::MouseButton::WheelUp
            } else {
                term::MouseButton::WheelDown
            };
            for _ in 0..(lines.abs() as u32).min(50) {
                self.terminal.mouse(MouseInput {
                    action: MouseAction::Press,
                    button: Some(button),
                    point,
                    ctrl: event.modifiers.control,
                    alt: event.modifiers.alt,
                    shift: false,
                });
            }
            return;
        }
        if lines != 0. {
            self.terminal.scroll(lines as i32);
            cx.notify();
        }
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let configured = self.state.read(cx).settings.terminal_font_family.clone();
        if configured != self.configured_font {
            self.font_family = resolve_font(&configured, cx);
            self.configured_font = configured;
            self.metrics.set(None);
        }
        let state = self.state.read(cx);
        let font = Font {
            family: self.font_family.clone(),
            features: FontFeatures::default(),
            fallbacks: None,
            // Geist Mono has no hinting, and light on dark its strokes come out soft at regular
            // weight. Dark on light gpui thickens them itself, and more would blur them.
            weight: if self.font_family.as_ref() == crate::theme::MONO_FAMILY
                && !cx.theme().is_light
            {
                FontWeight::MEDIUM
            } else {
                FontWeight::NORMAL
            },
            style: FontStyle::Normal,
        };
        let font_size = px(state.settings.terminal_font_size);
        let overlay = if self.terminal.is_alive() {
            session_overlay::overlay_for(state.session(self.session))
        } else {
            session_overlay::overlay_for(state.session(self.session))
                .or(Some(Overlay::Closed("Shell exited".into())))
        };
        // Only for a tab that lost its session: a shell that exited needs the user.
        let overlay = if self.session_is_gone(cx) {
            session_overlay::with_reconnect(overlay, state.reconnect_attempt(&self.host))
        } else {
            overlay
        };
        let shell_started = self.terminal.shell_started();
        self.had_session |= shell_started
            && matches!(
                state.session(self.session).map(|s| &s.state),
                Some(SessionState::Connected)
            );
        let heading = state.host(&self.host).map(session_overlay::heading);
        let steps = session_overlay::steps_of(state.host_session(&self.host));
        let shell_steps = session_overlay::shell_steps(state.host_session(&self.host));
        // A retry may have started a new session: what it asks is for this tab as well.
        let overlay = match state.login_prompt(&self.host, cx) {
            Some(prompt) => {
                let own = present(&prompt, cx.entity_id(), &self.focus, window, cx);
                Some(Overlay::Login { steps, prompt, own })
            }
            None => overlay,
        };
        // A terminal that already showed a shell gets its new one without a word.
        let overlay = overlay.or_else(|| {
            (!shell_started && !self.had_session && self.terminal.is_alive())
                .then_some(Overlay::StartingShell { steps: shell_steps })
        });
        let focused = self.focus.is_focused(window);
        if focused != self.was_focused {
            self.was_focused = focused;
            self.terminal.focus_changed(focused);
        }
        let settings = &self.state.read(cx).settings;
        let (blinks, cursor_shape) = (
            settings.cursor_blink,
            match settings.cursor_style {
                shcp_core::CursorStyle::Program => None,
                shcp_core::CursorStyle::Block => Some(term::CursorShape::Block),
                shcp_core::CursorStyle::Bar => Some(term::CursorShape::Beam),
                shcp_core::CursorStyle::Underline => Some(term::CursorShape::Underline),
            },
        );
        if blinks && focused && window.is_window_active() && overlay.is_none() {
            if self.cursor_blink.is_none() {
                self.start_cursor_blink(cx);
            }
        } else {
            self.cursor_blink = None;
            self.cursor_visible = true;
        }
        let element = TerminalElement {
            view: cx.entity(),
            terminal: self.terminal.clone(),
            font,
            font_size,
            focused,
            cursor_visible: self.cursor_visible,
            cursor_shape,
            metrics: self.metrics.clone(),
            theme: theme.clone(),
            hovered_link: self.hovered_link.as_ref().map(|l| (l.start, l.end)),
        };
        let link_url = self.hovered_link.as_ref().map(|link| link.url.clone());
        // The search bar stays out of the "Terminal" key context: Ctrl+V there goes to its field.
        let terminal = div()
            .id("terminal")
            .key_context("Terminal")
            .when(link_url.is_some() && self.link_armed, |this| {
                this.cursor(CursorStyle::PointingHand)
            })
            .on_modifiers_changed(
                cx.listener(|this, event: &ModifiersChangedEvent, window, cx| {
                    let position = window.mouse_position();
                    this.update_hovered_link(position, event.modifiers.control, cx);
                }),
            )
            .on_action(cx.listener(Self::open_search))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                window.focus(&this.focus);
                for path in paths.paths() {
                    let label = path.file_name().map_or_else(
                        || path.display().to_string(),
                        |n| n.to_string_lossy().into_owned(),
                    );
                    this.send_file(&label, PasteFile::Local(path.clone()), cx);
                }
            }))
            .drag_over::<ExternalPaths>(|style, _, _, _| style.opacity(0.7))
            .track_focus(&self.focus)
            .size_full()
            .relative()
            // No fill: the panel around it shows through, as it does for a cell without a color.
            .on_key_down(cx.listener(Self::on_key_down))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::paste))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Middle, cx.listener(Self::on_mouse_down))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            // Lined up with the panel header's text.
            .child(
                div()
                    .size_full()
                    .px(px(18.))
                    .pt(px(14.))
                    .pb(px(18.))
                    .child(element),
            );
        // Outside the terminal: a key pressed in the prompt must never reach the shell.
        let overlay = overlay.map(|overlay| {
            session_overlay::render(
                overlay,
                Surface {
                    heading,
                    docked: self.had_session,
                    background: theme.terminal_bg,
                    focus: self.focus.clone(),
                    extra_steps: 1,
                    owner: cx.entity_id(),
                },
                &theme,
                window,
                cx,
                cx.listener(|this, _, window, cx| {
                    this.retry(cx);
                    window.focus(&this.focus);
                }),
            )
        });
        let bar = {
            let (read, moved) = (self.terminal.clone(), self.terminal.clone());
            scrollbar(
                self.scroll_drag.clone(),
                theme.text_muted,
                move |_| {
                    let (offset, history, rows) = read.scroll_position();
                    (history > 0).then(|| ScrollMetrics {
                        position: 1. - offset as f32 / history as f32,
                        visible: rows as f32 / (rows + history) as f32,
                    })
                },
                move |position, _, _| {
                    let (_, history, _) = moved.scroll_position();
                    moved.scroll_to_offset(((1. - position) * history as f32).round() as usize);
                },
            )
        };
        // The search bar is part of this terminal, and a drag ends only where the bar can see it.
        let show_bar = focused
            || self
                .search_input
                .read(cx)
                .focus_handle(cx)
                .is_focused(window)
            || self.scroll_drag.get().is_some();
        div()
            .size_full()
            .relative()
            .child(terminal)
            .when(show_bar, |this| this.child(bar))
            .children(overlay)
            .when_some(link_url, |this, url| {
                this.child(
                    // Shows where a link really goes: its text can say anything. The outer box
                    // gives the text a width to shorten to; alone it would shrink to nothing.
                    div()
                        .absolute()
                        .left(px(8.))
                        .right(px(8.))
                        .bottom(px(8.))
                        .flex()
                        .child(
                            div()
                                .min_w_0()
                                .px(px(8.))
                                .py(px(3.))
                                .truncate()
                                .rounded(px(6.))
                                .border_1()
                                .border_color(theme.border)
                                .bg(theme.elevated)
                                .text_size(crate::theme::ui_px(12.))
                                .text_color(theme.text_muted)
                                .child(format!("Ctrl+click to open {url}")),
                        ),
                )
            })
            .when(self.search_open, |this| {
                let missed = self.search_missed;
                this.child(
                    div()
                        .id("terminal-search")
                        .occlude()
                        .absolute()
                        .top(px(8.))
                        .right(px(16.))
                        .w(px(340.))
                        .p(px(4.))
                        .flex()
                        .items_center()
                        .gap(px(2.))
                        .rounded(px(8.))
                        .border_1()
                        .border_color(if missed { theme.error } else { theme.border })
                        .bg(theme.elevated)
                        .shadow(crate::theme::float_shadow())
                        .text_size(crate::theme::ui_px(12.))
                        .capture_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                            if event.keystroke.key == "enter" && event.keystroke.modifiers.shift {
                                this.search_step(false, cx);
                                cx.stop_propagation();
                            }
                        }))
                        .child(div().flex_1().min_w_0().child(self.search_input.clone()))
                        .when(missed, |this| {
                            this.child(
                                div()
                                    .flex_none()
                                    .px(px(4.))
                                    .text_color(theme.error)
                                    .child("No results"),
                            )
                        })
                        .child(
                            Button::new("search-older")
                                .icon(IconName::ChevronUp)
                                .tooltip("Previous match (Enter)")
                                .style(ButtonStyle::Ghost)
                                .on_click(cx.listener(|this, _, _, cx| this.search_step(true, cx))),
                        )
                        .child(
                            Button::new("search-newer")
                                .icon(IconName::ChevronDown)
                                .tooltip("Next match (Shift+Enter)")
                                .style(ButtonStyle::Ghost)
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.search_step(false, cx)),
                                ),
                        )
                        .child(
                            Button::new("search-close")
                                .icon(IconName::Close)
                                .tooltip("Close (Esc)")
                                .style(ButtonStyle::Ghost)
                                .on_click(
                                    cx.listener(|this, _, window, cx| {
                                        this.close_search(window, cx)
                                    }),
                                ),
                        ),
                )
            })
    }
}
