//! The tile that stands for a session wherever it shows: what runs there in the color of its
//! host, on a neutral rounded rectangle slightly wider than tall.
use super::icon::{Icon, IconName};
use super::tooltip::tooltip;
use crate::theme::{Theme, row_px};
use gpui::prelude::*;
use gpui::{AnyElement, App, Div, ElementId, Hsla, Pixels, SharedString, div, px};
use std::{cell::RefCell, collections::HashMap};

const WIDTH: f32 = 22.;
const HEIGHT: f32 = 18.;
const RADIUS: Pixels = px(crate::theme::RADIUS_SMALL);
/// The deck of a split session: how far the card behind the tile is, up and to the right, and
/// the line that parts the two.
const DECK_STEP: f32 = 2.5;
const GAP: f32 = 1.;

/// What a tile shows.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum Mark {
    Icon(IconName),
    /// A short label, like the type of a file: the tile widens for one of three or four letters.
    Label(SharedString),
}

thread_local! {
    /// What `text_lift` found, by font family and size.
    static LIFTS: RefCell<HashMap<(String, u32), Pixels>> = RefCell::default();
}

/// How far text in `family` at `size`, beside an icon or a tile, goes up to look centred on it.
/// gpui centres a line on the font's ascent and descent: the middle of the line is half their
/// difference above the baseline, while the middle of lowercase letters is half the x-height
/// above it. Icons are centred on their box, so the text is lifted by the difference, from the
/// font's own metrics. In whole pixels, rounded down: gpui puts glyphs on whole pixels by
/// rounding their position down, so a lift of 1.4 would show as 2.
pub(crate) fn text_lift(family: &str, size: Pixels, cx: &App) -> Pixels {
    let key = (family.to_string(), f32::from(size).to_bits());
    if let Some(lift) = LIFTS.with(|lifts| lifts.borrow().get(&key).copied()) {
        return lift;
    }
    let system = cx.text_system();
    let font = system.resolve_font(&gpui::font(family.to_string()));
    let ascent = f32::from(system.ascent(font, size));
    // Below the baseline, whichever sign the platform gives it.
    let descent = f32::from(system.descent(font, size)).abs();
    let x_height = f32::from(system.x_height(font, size));
    let lift = if x_height > 0. {
        px(((ascent - descent) / 2. - x_height / 2.).max(0.).floor())
    } else {
        px(0.)
    };
    LIFTS.with(|lifts| lifts.borrow_mut().insert(key, lift));
    lift
}

/// What the tooltip of a session's tile says: the shell the server named, and root when it is.
fn shell_tip(shell: Option<&str>, user: &str) -> Option<String> {
    match (shell, user == "root") {
        (Some(shell), true) => Some(format!("root · {shell}")),
        (None, true) => Some("root".to_string()),
        (Some(shell), false) => Some(shell.to_string()),
        (None, false) => None,
    }
}

/// The tile of a session: the terminal prompt, whatever the shell. The tooltip names the shell,
/// and root. `shell` is the login shell the server named, not what the user started later.
pub(crate) fn shell_tile(
    id: impl Into<ElementId>,
    shell: Option<&str>,
    user: &str,
    color: Hsla,
    deck: Option<Hsla>,
    theme: &Theme,
) -> AnyElement {
    let tile = tile(Mark::Icon(IconName::Terminal), color, deck, theme);
    match shell_tip(shell, user) {
        Some(tip) => tile.id(id).tooltip(tooltip(tip)).into_any_element(),
        None => tile.into_any_element(),
    }
}

/// `mark` in `color` on a tile. A split session gives `deck`, the color of the row under it: one
/// more card shows behind the tile, up and to the right, whatever the number of its terminals.
/// The tile itself stays where and how it is on any other row, so the tiles of a list line up.
pub(crate) fn tile(mark: Mark, color: Hsla, deck: Option<Hsla>, theme: &Theme) -> Div {
    let (width, height) = (row_px(WIDTH), row_px(HEIGHT));
    let colors = theme.tile_colors();
    let face = div()
        .relative()
        .min_w(width)
        .h(height)
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(RADIUS)
        .bg(colors.fill);
    let face = match mark {
        Mark::Icon(icon) => face.child(Icon::new(icon).size(row_px(13.)).color(color)),
        Mark::Label(label) => face
            .px(px(4.))
            .font_family(crate::theme::mono_font())
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_size(crate::theme::ui_px(11.))
            .text_color(color)
            .child(label),
    };
    let Some(under) = deck else {
        return face;
    };
    div()
        .relative()
        .w(width)
        .h(height)
        .flex_none()
        .child(
            div()
                .absolute()
                .left(px(DECK_STEP))
                .top(px(-DECK_STEP))
                .w(width)
                .h(height)
                .rounded(RADIUS)
                .bg(colors.back),
        )
        // The tile hides the card behind it, and a line in the color of the row parts the two.
        // Over that color its see-through fill reads as it does on any other row.
        .child(
            div()
                .absolute()
                .left(px(-GAP))
                .top(px(-GAP))
                .p(px(GAP))
                .rounded(RADIUS + px(GAP))
                .bg(under)
                .child(face),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tooltip_names_the_shell_and_root() {
        assert_eq!(shell_tip(Some("bash"), "deploy").as_deref(), Some("bash"));
        assert_eq!(
            shell_tip(Some("zsh"), "root").as_deref(),
            Some("root · zsh")
        );
        assert_eq!(shell_tip(None, "root").as_deref(), Some("root"));
        assert_eq!(shell_tip(None, "deploy"), None);
    }
}
