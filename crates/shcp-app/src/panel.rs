//! The floating panels of the main area: every terminal and every folder is a card of its own.
use crate::theme::{RADIUS_CARD, Theme, row_px};
use gpui::prelude::*;
use gpui::{AnyElement, Div, Hsla, div, px};

/// The room between two cards, which is also where a drag resizes them.
pub(crate) const CARD_GAP: f32 = 6.;
/// How tall the header of a card is, at default density.
const HEADER_HEIGHT: f32 = 38.;

/// A rounded card. Children filling it up to its edge must round their own bottom corners with
/// `PANEL_INNER_RADIUS`: gpui clips to rectangles only. `focused`: it has the keyboard among
/// other cards, and its line is brighter, in the text's own color: never the accent's.
pub(crate) fn frame(theme: &Theme, bg: Hsla, focused: bool) -> Div {
    div()
        .relative()
        .flex()
        .flex_col()
        .min_w_0()
        .rounded(px(RADIUS_CARD))
        .border_1()
        .border_color(if focused {
            Hsla {
                a: FOCUSED_LINE,
                ..theme.text
            }
        } else {
            theme.border
        })
        .bg(bg)
}

/// How much of the text's color the line of the focused card takes.
const FOCUSED_LINE: f32 = 0.35;

/// The surface of what floats over the window or stands out of it: a menu, a tooltip, a dialog,
/// a field. The raised fill and `border`.
pub(crate) fn floating<E: Styled>(this: E, theme: &Theme, border: Hsla) -> E {
    this.border_1().border_color(border).bg(theme.elevated)
}

/// A card that floats over the rest: rounded as a card, with the surface of [`floating`] and a
/// shadow.
pub(crate) fn floating_card<E: Styled>(this: E, theme: &Theme, border: Hsla) -> E {
    floating(this.rounded(px(RADIUS_CARD)), theme, border).shadow(crate::theme::float_shadow())
}

/// The top of a card: what it shows at the left, what acts on it at the right. The tools show as
/// much as `tools_shown` says, from 0 to 1: see `motion::HoverFade`. Hidden, they keep their room,
/// so nothing moves when they come, and take no clicks.
pub(crate) fn header(tools_shown: f32, lead: Vec<AnyElement>, tools: Vec<AnyElement>) -> Div {
    div()
        .h(row_px(HEADER_HEIGHT))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(2.))
        .pl(px(8.))
        .pr(px(5.))
        .text_size(crate::theme::ui_px(13.))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .gap(px(9.))
                .overflow_hidden()
                .children(lead),
        )
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(px(2.))
                .map(|this| {
                    if tools_shown <= 0. {
                        this.invisible()
                    } else {
                        this.opacity(tools_shown)
                    }
                })
                .children(tools),
        )
}
