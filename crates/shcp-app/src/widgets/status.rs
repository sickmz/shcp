//! Session status as a dot, and the small pill for states that want attention.
use super::icon::{Icon, IconName};
use crate::theme::Theme;
use gpui::prelude::*;
use gpui::{AnyElement, Div, Hsla, SharedString, div, px};
use shcp_core::SessionState;

/// Filled when something is up, a ring when nothing is, a spinner while connecting.
pub fn status_dot(theme: &Theme, state: Option<&SessionState>) -> AnyElement {
    let dot = div().size(px(7.)).flex_none().rounded_full();
    match state {
        Some(SessionState::Connected) => dot.bg(theme.accent).into_any_element(),
        Some(SessionState::Failed { .. }) => dot.bg(theme.error).into_any_element(),
        Some(SessionState::Connecting { .. }) => Icon::new(IconName::Spinner)
            .size(px(11.))
            .color(theme.warning)
            .spin()
            .into_any_element(),
        Some(SessionState::Disconnected { .. }) | None => dot
            .border_1()
            .border_color(theme.text_disabled)
            .into_any_element(),
        Some(_) => dot.bg(theme.warning).into_any_element(),
    }
}

/// The tag that travels with a host: PROD, STAGING.
pub fn env_tag(text: &'static str, color: Hsla) -> Div {
    div()
        .h(px(16.))
        .px(px(5.))
        .flex()
        .flex_none()
        .items_center()
        .rounded(px(4.))
        .border_1()
        .border_color(Hsla { a: 0.42, ..color })
        .font_family(crate::theme::mono_font())
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_size(px(9.5))
        .text_color(color)
        .child(text)
}

pub fn badge(text: impl Into<SharedString>, color: Hsla) -> Div {
    div()
        .h(px(18.))
        .px(px(6.))
        .flex()
        .flex_none()
        .items_center()
        .rounded(px(5.))
        .border_1()
        .border_color(Hsla { a: 0.3, ..color })
        .bg(Hsla { a: 0.14, ..color })
        .text_size(crate::theme::ui_px(11.))
        .text_color(color)
        .whitespace_nowrap()
        .child(text.into())
}
