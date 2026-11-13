//! Command palette: type to filter hosts, open tabs and commands, Enter runs the selected one.
use crate::theme::{ActiveTheme as _, Theme, ui_px};
use crate::widgets::context_menu::Handler;
use crate::widgets::host_color::{HostColor, color_dot};
use crate::widgets::icon::{Icon, IconName};
use crate::widgets::text_input::{TextInput, TextInputEvent};
use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, DismissEvent, Div, Entity, EventEmitter, FocusHandle, Focusable,
    KeyDownEvent, MouseButton, ScrollHandle, SharedString, Subscription, Window, div, px,
};

pub(crate) struct PaletteItem {
    /// The heading it goes under.
    pub section: SharedString,
    pub title: SharedString,
    pub detail: Option<SharedString>,
    pub icon: IconName,
    /// Instead of the icon on a host: its color, and whether it is connected.
    pub dot: Option<(HostColor, bool)>,
    /// The icon in the color of the host it belongs to.
    pub tint: Option<HostColor>,
    pub shortcut: Option<&'static str>,
    pub run: Handler,
    /// What Shift+Enter does instead: the files of a host, where Enter opens its terminal.
    pub alt: Option<Handler>,
}

pub(crate) struct CommandPalette {
    items: Vec<PaletteItem>,
    /// Indexes into `items` matching the query.
    visible: Vec<usize>,
    selected: usize,
    input: Entity<TextInput>,
    scroll: ScrollHandle,
    _subscription: Subscription,
}

impl EventEmitter<DismissEvent> for CommandPalette {}
impl EventEmitter<super::CancelEvent> for CommandPalette {}

impl Focusable for CommandPalette {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }
}

/// Every word of the query must appear in the title or the detail.
fn matches(item: &PaletteItem, query: &str) -> bool {
    let text = format!(
        "{} {}",
        item.title.to_lowercase(),
        item.detail
            .as_ref()
            .map_or(String::new(), |d| d.to_lowercase())
    );
    query.split_whitespace().all(|word| text.contains(word))
}

impl CommandPalette {
    pub(crate) fn new(
        items: Vec<PaletteItem>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| TextInput::new("Hosts, tabs, commands, actions", cx).bare());
        let subscription = cx.subscribe_in(
            &input,
            window,
            |this, _, event: &TextInputEvent, window, cx| match event {
                TextInputEvent::Changed => this.refilter(cx),
                TextInputEvent::Submit => this.run_row(this.selected, false, window, cx),
                TextInputEvent::Cancel => cx.emit(super::CancelEvent),
            },
        );
        let visible = (0..items.len()).collect();
        Self {
            items,
            visible,
            selected: 0,
            input,
            scroll: ScrollHandle::new(),
            _subscription: subscription,
        }
    }

    fn refilter(&mut self, cx: &mut Context<Self>) {
        let query = self.input.read(cx).text().to_lowercase();
        self.visible = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| matches(item, &query))
            .map(|(index, _)| index)
            .collect();
        self.selected = 0;
        self.scroll.scroll_to_item(0);
        cx.notify();
    }

    fn run_row(&mut self, row: usize, alt: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = self.visible.get(row).and_then(|i| self.items.get(*i)) else {
            return;
        };
        let run = item
            .alt
            .clone()
            .filter(|_| alt)
            .unwrap_or_else(|| item.run.clone());
        cx.emit(DismissEvent);
        // After the dismissal restored focus, so the command can move it.
        window.defer(cx, move |window, cx| run(window, cx));
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let count = self.visible.len();
        if count == 0 {
            return;
        }
        self.selected = match event.keystroke.key.as_str() {
            "down" => (self.selected + 1) % count,
            "up" => (self.selected + count - 1) % count,
            _ => return,
        };
        self.scroll.scroll_to_item(self.child_of(self.selected));
        cx.stop_propagation();
        cx.notify();
    }

    /// Where a row is among the children of the list, whose headings count too. The first heading
    /// goes with its row, so the list can scroll all the way back.
    fn child_of(&self, row: usize) -> usize {
        let mut headings = 0;
        let mut section: Option<&SharedString> = None;
        for index in self.visible.iter().take(row + 1) {
            let current = &self.items[*index].section;
            if section != Some(current) {
                headings += 1;
                section = Some(current);
            }
        }
        if row == 0 { 0 } else { row + headings }
    }
}

impl CommandPalette {
    /// The visible items, under the heading of each section.
    fn rows(&self, theme: &Theme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut rows: Vec<AnyElement> = Vec::new();
        let mut section: Option<SharedString> = None;
        for (row, index) in self.visible.iter().enumerate() {
            let Some(item) = self.items.get(*index) else {
                continue;
            };
            if section.as_ref() != Some(&item.section) {
                section = Some(item.section.clone());
                rows.push(section_heading(theme, item.section.clone()));
            }
            rows.push(self.item_row(row, item, theme, cx));
        }
        rows
    }

    fn item_row(
        &self,
        row: usize,
        item: &PaletteItem,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selected = row == self.selected;
        let icon_color = match item.tint {
            Some(color) => theme.host_color(color),
            None if selected => theme.text,
            None => theme.text_disabled,
        };
        div()
            .id(("palette-row", row))
            .h(px(36.))
            .flex_none()
            .px(px(10.))
            .flex()
            .items_center()
            .gap(px(10.))
            .rounded(px(crate::theme::RADIUS_CONTROL))
            .cursor_pointer()
            .when(selected, |this| this.bg(theme.active))
            .on_mouse_move(cx.listener(move |this, _, _, cx| {
                if this.selected != row {
                    this.selected = row;
                    cx.notify();
                }
            }))
            .on_click(
                cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                    this.run_row(row, event.modifiers().shift, window, cx)
                }),
            )
            .child(
                // As wide for all, so titles line up.
                div()
                    .w(px(22.))
                    .flex()
                    .flex_none()
                    .justify_center()
                    .child(match item.dot {
                        Some((color, connected)) => {
                            color_dot(theme.host_color(color), connected).into_any_element()
                        }
                        None => Icon::new(item.icon).color(icon_color).into_any_element(),
                    }),
            )
            .child(
                div()
                    .flex_none()
                    .max_w(px(320.))
                    .truncate()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child(item.title.clone()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(ui_px(12.))
                    .text_color(theme.text_disabled)
                    .children(item.detail.clone()),
            )
            .children(item.shortcut.map(|keys| {
                div()
                    .flex_none()
                    .font_family(crate::theme::mono_font())
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_size(ui_px(11.))
                    .text_color(theme.text_disabled)
                    .child(crate::actions::with_current_keys(keys, cx).replace('+', " "))
            }))
            .into_any_element()
    }

    /// The search field.
    fn search_header(&self, theme: &Theme) -> Div {
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(10.))
            .h(px(50.))
            .px(px(16.))
            .border_b_1()
            .border_color(theme.border)
            .child(
                Icon::new(IconName::Search)
                    .size(px(16.))
                    .color(theme.text_disabled),
            )
            .child(div().flex_1().min_w_0().child(self.input.clone()))
    }
}

impl Render for CommandPalette {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let any_alt = self
            .visible
            .get(self.selected)
            .and_then(|index| self.items.get(*index))
            .is_some_and(|item| item.alt.is_some());
        let rows = self.rows(&theme, cx);
        let empty = rows.is_empty();
        // Fills the layer so the card sits near the top; clicking the backdrop closes it.
        div()
            .size_full()
            .flex()
            .justify_center()
            .pt(px(72.))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _, _, cx| cx.emit(super::CancelEvent)),
            )
            .child(
                div()
                    .w(px(640.))
                    .max_h(px(560.))
                    .flex()
                    .flex_col()
                    .map(|this| crate::panel::floating_card(this, &theme, theme.strong_border))
                    .overflow_hidden()
                    .text_size(ui_px(13.))
                    .text_color(theme.text)
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                        // Before the field, whose Enter does not tell Shift apart.
                        if event.keystroke.key == "enter" && event.keystroke.modifiers.shift {
                            this.run_row(this.selected, true, window, cx);
                            cx.stop_propagation();
                        }
                    }))
                    .on_key_down(cx.listener(Self::on_key_down))
                    .child(self.search_header(&theme))
                    .child(
                        div()
                            .id("palette-list")
                            .flex_1()
                            .min_h_0()
                            .flex()
                            .flex_col()
                            .px(px(6.))
                            .pt(px(4.))
                            .pb(px(8.))
                            .overflow_y_scroll()
                            .track_scroll(&self.scroll)
                            .children(rows)
                            .when(empty, |this| {
                                this.child(
                                    div()
                                        .p(px(10.))
                                        .text_color(theme.text_muted)
                                        .child("No matches"),
                                )
                            }),
                    )
                    .child(footer(&theme, any_alt)),
            )
    }
}

fn section_heading(theme: &Theme, section: SharedString) -> AnyElement {
    div()
        .flex_none()
        .px(px(10.))
        .pt(px(10.))
        .pb(px(4.))
        .text_size(ui_px(12.))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(theme.text_disabled)
        .child(section)
        .into_any_element()
}

/// Keys and what they do, under the list.
fn key_hint(keys: &'static str, what: &'static str) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(5.))
        .child(
            div()
                .font_family(crate::theme::mono_font())
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_size(ui_px(11.))
                .child(keys),
        )
        .child(what)
}

fn footer(theme: &Theme, any_alt: bool) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(16.))
        .px(px(16.))
        .py(px(8.))
        .border_t_1()
        .border_color(theme.border)
        .text_size(ui_px(12.))
        .text_color(theme.text_disabled)
        .child(key_hint("Enter", if any_alt { "Terminal" } else { "Run" }))
        .when(any_alt, |this| this.child(key_hint("Shift Enter", "Files")))
        .child(div().flex_1())
        .child(key_hint("Esc", "Close"))
}

impl super::ModalView for CommandPalette {
    const ENTRANCE: bool = false;
}
