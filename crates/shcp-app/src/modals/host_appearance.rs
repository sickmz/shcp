//! Picks the color of a host. A modal of its own too, because `HostEditor` never opens for the
//! read-only `~/.ssh/config` hosts.
use super::{button_row, card, title};
use crate::state::AppState;
use crate::theme::{ActiveTheme as _, ui_px};
use crate::widgets::button::{Button, ButtonStyle};
use crate::widgets::host_color::HostColor;
use gpui::prelude::*;
use gpui::{
    App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, KeyDownEvent,
    Subscription, Window, div, px,
};
use shcp_core::{HostAppearance, HostId};

/// The controls, for a modal to embed. It changes nothing by itself: the modal reads `appearance`
/// when saving.
pub struct AppearancePicker {
    color: HostColor,
}

impl AppearancePicker {
    pub fn new(appearance: &HostAppearance) -> Self {
        Self {
            color: appearance
                .color
                .as_deref()
                .and_then(HostColor::from_slug)
                .unwrap_or(HostColor::Gray),
        }
    }

    pub fn color(&self) -> HostColor {
        self.color
    }

    pub fn appearance(&self) -> HostAppearance {
        HostAppearance {
            color: Some(self.color.slug().to_string()),
        }
    }
}

impl Render for AppearancePicker {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (muted, ring) = (cx.theme().text_muted, cx.theme().text);
        let colors: Vec<_> = HostColor::ALL
            .iter()
            .copied()
            .enumerate()
            .map(|(index, color)| {
                let selected = self.color == color;
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
            })
            .collect();
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(div().text_size(ui_px(12.)).text_color(muted).child("Color"))
            .child(div().flex().flex_wrap().gap(px(8.)).children(colors))
    }
}

pub struct HostAppearanceModal {
    state: Entity<AppState>,
    host: HostId,
    alias: String,
    picker: Entity<AppearancePicker>,
    focus: FocusHandle,
    _subscription: Subscription,
}

impl EventEmitter<DismissEvent> for HostAppearanceModal {}

impl Focusable for HostAppearanceModal {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl HostAppearanceModal {
    pub fn new(state: Entity<AppState>, host: HostId, cx: &mut Context<Self>) -> Self {
        let (alias, appearance) = {
            let state = state.read(cx);
            let alias = state
                .host(&host)
                .map_or(host.0.clone(), |h| h.alias.clone());
            (alias, state.host_appearance(&host))
        };
        let picker = cx.new(|_| AppearancePicker::new(&appearance));
        let subscription = cx.observe(&picker, |_, _, cx| cx.notify());
        Self {
            state,
            host,
            alias,
            picker,
            focus: cx.focus_handle(),
            _subscription: subscription,
        }
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let appearance = self.picker.read(cx).appearance();
        self.state.update(cx, |state, cx| {
            state.set_host_appearance(&self.host, appearance, cx);
        });
        cx.emit(DismissEvent);
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "escape" => cx.emit(DismissEvent),
            "enter" => self.save(cx),
            _ => return,
        }
        cx.stop_propagation();
    }
}

impl Render for HostAppearanceModal {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        card(&theme, 420.)
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key_down))
            .child(title(&theme, format!("Customize {}", self.alias)))
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
