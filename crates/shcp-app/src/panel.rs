//! The floating panels of the main area: every terminal and every folder is a card of its own.
use crate::theme::{RADIUS_CARD, Theme, row_px};
use gpui::prelude::*;
use gpui::{AnyElement, Div, Hsla, div, px};

/// The room between two cards, which is also where a drag resizes them.
pub(crate) const CARD_GAP: f32 = 6.;
/// How tall the header of a card is, at default density.
const HEADER_HEIGHT: f32 = 38.;

/// A rounded card. Children filling it up to its edge must round their own bottom corners with
/// `PANEL_INNER_RADIUS`: gpui clips to rectangles only.
pub(crate) fn frame(theme: &Theme, bg: Hsla) -> Div {
    div()
        .relative()
        .flex()
        .flex_col()
        .min_w_0()
        .rounded(px(RADIUS_CARD))
        .border_1()
        .border_color(theme.border)
        // A flat fill: gpui does not dither, and a faint gradient this long shows up as bands.
        .bg(Hsla {
            a: theme.panel_alpha,
            ..bg
        })
}

/// The `radius` of what is `rounded_full`, for `floating`.
pub(crate) const CAPSULE: f32 = 999.;

/// The surface of what floats over the window or stands out of it: a menu, a tooltip, a dialog,
/// a field. In a theme with a material it is a lit card of it, opaque, with `radius` for its
/// corners, the same as `this` is rounded with; otherwise the raised fill and `border`. To call
/// before `this` gets its children: the material goes under them.
pub(crate) fn floating<E: Styled + ParentElement>(
    this: E,
    theme: &Theme,
    radius: f32,
    border: Hsla,
) -> E {
    match theme.material() {
        Some(material) => this.relative().child(crate::terminal_material::layers(
            crate::terminal_material::Material {
                radius,
                ..material.opaque()
            },
            true,
            true,
        )),
        None => this.border_1().border_color(border).bg(theme.elevated),
    }
}

/// A card that floats over the rest: rounded as a card, with the surface of [`floating`] and a
/// shadow. To call before `this` gets its children, as [`floating`].
pub(crate) fn floating_card<E: Styled + ParentElement>(this: E, theme: &Theme, border: Hsla) -> E {
    floating(this.rounded(px(RADIUS_CARD)), theme, RADIUS_CARD, border)
        .shadow(crate::theme::float_shadow())
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
