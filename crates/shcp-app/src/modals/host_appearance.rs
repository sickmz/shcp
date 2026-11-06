//! Picks how a host looks. A modal of its own too, because `HostEditor` never opens for the
//! read-only `~/.ssh/config` hosts.
use super::{button_row, card, title};
use crate::state::AppState;
use crate::theme::{ActiveTheme as _, ui_px};
use crate::widgets::button::{Button, ButtonStyle};
use crate::widgets::host_tile::{HostColor, HostLook, HostTile, MAX_TEXT, clip_text};
use crate::widgets::icon::{HOST_ICONS, IconName};
use crate::widgets::text_input::{TextInput, TextInputEvent};
use gpui::prelude::*;
use gpui::{
    App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, Hsla, Subscription,
    Window, div, px,
};
use shcp_core::{HostAppearance, HostId};

/// The icon, text and color controls, for a modal to embed. It changes nothing by itself: the
/// modal reads `appearance` when saving.
pub struct AppearancePicker {
    /// Nothing while there is text on the tile.
    icon: Option<IconName>,
    color: HostColor,
    text: Entity<TextInput>,
    _subscription: Subscription,
}

impl AppearancePicker {
    pub fn new(appearance: &HostAppearance, cx: &mut Context<Self>) -> Self {
        let saved_text = clip_text(appearance.text.as_deref().unwrap_or(""));
        let text = cx.new(|cx| {
            let mut input = TextInput::new("DB", false, cx);
            input.set_text(&saved_text, cx);
            input
        });
        let subscription = cx.subscribe(&text, Self::on_text_event);
        Self {
            // As on the tile: text wins over an icon.
            icon: appearance
                .icon
                .as_deref()
                .and_then(IconName::from_slug)
                .filter(|_| saved_text.is_empty()),
            color: appearance
                .color
                .as_deref()
                .and_then(HostColor::from_slug)
                .unwrap_or(HostColor::Gray),
            text,
            _subscription: subscription,
        }
    }

    pub fn input(&self) -> &Entity<TextInput> {
        &self.text
    }

    pub fn appearance(&self, cx: &App) -> HostAppearance {
        HostAppearance {
            icon: self.icon.map(|icon| icon.slug().to_string()),
            text: Some(clip_text(self.text.read(cx).text())).filter(|text| !text.is_empty()),
            color: Some(self.color.slug().to_string()),
        }
    }

    pub fn look(&self, cx: &App) -> HostLook {
        HostLook::resolve("", None, Some(&self.appearance(cx)))
    }

    fn on_text_event(
        &mut self,
        input: Entity<TextInput>,
        event: &TextInputEvent,
        cx: &mut Context<Self>,
    ) {
        if !matches!(event, TextInputEvent::Changed) {
            return;
        }
        let typed = input.read(cx).text().to_string();
        let kept: String = typed.chars().take(MAX_TEXT).collect();
        if kept != typed {
            input.update(cx, |input, cx| input.set_text(&kept, cx));
        }
        if !kept.trim().is_empty() {
            self.icon = None;
        }
        cx.notify();
    }

    fn pick_icon(&mut self, icon: IconName, cx: &mut Context<Self>) {
        self.icon = Some(icon);
        self.text.update(cx, |input, cx| input.set_text("", cx));
        cx.notify();
    }

    fn swatch(
        &self,
        index: usize,
        color: HostColor,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let selected = self.color == color;
        let ring = cx.theme().text;
        div()
            .id(("host-color", index))
            .size(px(24.))
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .rounded(px(7.))
            .bg(color.hsla())
            .cursor_pointer()
            .focusable()
            .tab_stop(true)
            .when(selected, |this| this.border_2().border_color(ring))
            .on_key_down(|event, _, cx| {
                // Enter/Space belong to the focused swatch, whose click fires on key-up.
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    cx.stop_propagation();
                }
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                this.color = color;
                cx.notify();
            }))
    }
}

/// Every cell of the icon grid is as wide, whatever its button holds.
fn glyph_slot(button: Button) -> impl IntoElement {
    div()
        .w(px(30.))
        .flex()
        .flex_none()
        .justify_center()
        .child(button)
}

fn section(label: &'static str, muted: Hsla) -> impl IntoElement {
    div().text_size(ui_px(12.)).text_color(muted).child(label)
}

impl Render for AppearancePicker {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().text_muted;
        let has_text = !self.text.read(cx).text().trim().is_empty();
        let current = self.icon.unwrap_or(IconName::Server);
        let icons: Vec<_> = HOST_ICONS
            .iter()
            .copied()
            .enumerate()
            .map(|(index, icon)| {
                glyph_slot(
                    Button::new(("host-glyph", index))
                        .keyboard_focus()
                        .icon(icon)
                        .style(ButtonStyle::Ghost)
                        .active(!has_text && current == icon)
                        .on_click(cx.listener(move |this, _, _, cx| this.pick_icon(icon, cx))),
                )
            })
            .collect();
        let colors: Vec<_> = HostColor::ALL
            .iter()
            .copied()
            .enumerate()
            .map(|(index, color)| self.swatch(index, color, cx))
            .collect();
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(section("Icon", muted))
            .child(div().flex().flex_wrap().gap(px(4.)).children(icons))
            .child(section("Or your own, up to two characters", muted))
            .child(div().w(px(72.)).child(self.text.clone()))
            .child(section("Color", muted))
            .child(div().flex().flex_wrap().gap(px(8.)).children(colors))
    }
}

pub struct HostAppearanceModal {
    state: Entity<AppState>,
    host: HostId,
    alias: String,
    picker: Entity<AppearancePicker>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<DismissEvent> for HostAppearanceModal {}

impl Focusable for HostAppearanceModal {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.picker.read(cx).input().focus_handle(cx)
    }
}

impl HostAppearanceModal {
    pub fn new(
        state: Entity<AppState>,
        host: HostId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (alias, appearance) = {
            let state = state.read(cx);
            let alias = state
                .host(&host)
                .map_or(host.0.clone(), |h| h.alias.clone());
            (alias, state.host_appearance(&host))
        };
        let picker = cx.new(|cx| AppearancePicker::new(&appearance, cx));
        let input = picker.read(cx).input().clone();
        let subscriptions = vec![
            cx.observe(&picker, |_, _, cx| cx.notify()),
            cx.subscribe_in(
                &input,
                window,
                |this, _, event: &TextInputEvent, _, cx| match event {
                    TextInputEvent::Submit => this.save(cx),
                    TextInputEvent::Cancel => cx.emit(DismissEvent),
                    TextInputEvent::Changed => {}
                },
            ),
        ];
        Self {
            state,
            host,
            alias,
            picker,
            _subscriptions: subscriptions,
        }
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let appearance = self.picker.read(cx).appearance(cx);
        self.state.update(cx, |state, cx| {
            state.set_host_appearance(&self.host, appearance, cx)
        });
        cx.emit(DismissEvent);
    }
}

impl Render for HostAppearanceModal {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let look = self.picker.read(cx).look(cx);
        card(&theme, 420.)
            .child(title(&theme, "Customize host"))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(HostTile::new(look).large())
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(div().truncate().child(self.alias.clone()))
                            .child(
                                div()
                                    .truncate()
                                    .text_size(ui_px(12.))
                                    .text_color(theme.text_muted)
                                    .child("Shown in the sidebar, tabs and palette"),
                            ),
                    ),
            )
            .child(self.picker.clone())
            .child(
                button_row()
                    .child(
                        Button::new("cancel")
                            .keyboard_focus()
                            .label("Cancel")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(DismissEvent))),
                    )
                    .child(
                        Button::new("save")
                            .keyboard_focus()
                            .label("Save")
                            .style(ButtonStyle::Primary)
                            .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
                    ),
            )
    }
}

impl super::ModalView for HostAppearanceModal {}
