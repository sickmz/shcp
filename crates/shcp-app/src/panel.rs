//! The floating panels of the main area.
use crate::theme::{PANEL_RADIUS, Theme};
use gpui::prelude::*;
use gpui::{Div, Hsla, div, px};

/// A rounded card. Children filling it up to its edge must round their own bottom corners with
/// `PANEL_INNER_RADIUS`: gpui clips to rectangles only.
pub fn frame(theme: &Theme, bg: Hsla) -> Div {
    div()
        .relative()
        .flex()
        .flex_col()
        .min_w_0()
        .rounded(px(PANEL_RADIUS))
        .border_1()
        .border_color(theme.border)
        // A flat fill: gpui does not dither, and a faint gradient this long shows up as bands.
        .bg(Hsla {
            a: theme.panel_alpha,
            ..bg
        })
}
