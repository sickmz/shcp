//! Picks the color of a host, inside the modals that deal with hosts.
use crate::theme::{ActiveTheme as _, ui_px};
use crate::widgets::button::keep_activation_keys;
use crate::widgets::host_color::HostColor;
use gpui::prelude::*;
use gpui::{Context, Window, div, px};
use shcp_core::HostAppearance;

/// The controls, for a modal to embed. It changes nothing by itself: the modal reads `appearance`.
pub(crate) struct AppearancePicker {
    color: HostColor,
}

impl AppearancePicker {
    pub(crate) fn new(appearance: &HostAppearance) -> Self {
        Self {
            color: appearance
                .color
                .as_deref()
                .and_then(HostColor::from_slug)
                .unwrap_or(HostColor::Gray),
        }
    }

    pub(crate) fn color(&self) -> HostColor {
        self.color
    }

    pub(crate) fn appearance(&self) -> HostAppearance {
        HostAppearance {
            color: Some(self.color.slug().to_string()),
        }
    }
}

impl Render for AppearancePicker {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (muted, ring) = (cx.theme().text_muted, cx.theme().text);
        let shown = cx.theme().shows_host_colors();
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
                    .on_key_down(keep_activation_keys)
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
            // A color can still be picked: it shows as soon as the accent lets it.
            .when(!shown, |this| {
                this.child(
                    div()
                        .text_size(ui_px(12.))
                        .text_color(muted)
                        .child("The Mono accent shows every host in gray. The color you pick here appears with any other accent, under Appearance in the settings."),
                )
            })
    }
}
