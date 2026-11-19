//! The small pill for states that want attention.
use gpui::prelude::*;
use gpui::{Div, Hsla, SharedString, div, px};

pub(crate) fn badge(text: impl Into<SharedString>, color: Hsla) -> Div {
    div()
        .h(px(18.))
        .px(px(6.))
        .flex()
        .flex_none()
        .items_center()
        .rounded(px(crate::theme::RADIUS_SMALL))
        .border_1()
        .border_color(Hsla { a: 0.3, ..color })
        .bg(Hsla { a: 0.14, ..color })
        .text_size(crate::theme::ui_px(11.))
        .text_color(color)
        .whitespace_nowrap()
        .child(text.into())
}
