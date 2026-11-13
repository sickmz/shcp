//! The tile that stands for a session wherever it shows: what runs there in the color of its
//! host, on a neutral rounded rectangle slightly wider than tall.
use super::icon::{Icon, IconName};
use super::tooltip::tooltip;
use crate::theme::{Theme, row_px};
use gpui::prelude::*;
use gpui::{AnyElement, Div, ElementId, Hsla, Pixels, SharedString, div, px};

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
    /// A few characters, as a prompt shows them.
    Glyph(&'static str),
    /// A short label, like the type of a file: the tile widens for one of three or four letters.
    Label(SharedString),
}

/// What the tile of a session says of its shell: `#` for root whatever the shell, the prompt of a
/// shell it knows, the terminal icon for any other or none. `shell` is the login shell the server
/// named, not what the user started later.
fn shell_mark(shell: Option<&str>, user: &str) -> Mark {
    if user == "root" {
        return Mark::Glyph("#");
    }
    match shell {
        Some("bash") => Mark::Glyph("$"),
        Some("zsh") => Mark::Glyph("%"),
        Some("fish") => Mark::Glyph("~>"),
        Some("pwsh" | "powershell") => Mark::Glyph("PS"),
        _ => Mark::Icon(IconName::Terminal),
    }
}

/// The tile of a session, saying its shell when known.
pub(crate) fn shell_tile(
    id: impl Into<ElementId>,
    shell: Option<&str>,
    user: &str,
    color: Hsla,
    deck: Option<Hsla>,
    theme: &Theme,
) -> AnyElement {
    let tile = tile(shell_mark(shell, user), color, deck, theme);
    match shell {
        Some(shell) => tile
            .id(id)
            .tooltip(tooltip(shell.to_string()))
            .into_any_element(),
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
        Mark::Glyph(glyph) => face
            .font_family(crate::theme::mono_font())
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_size(crate::theme::ui_px(11.))
            .text_color(color)
            .child(glyph),
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
    fn a_known_shell_shows_its_prompt() {
        assert_eq!(shell_mark(Some("bash"), "deploy"), Mark::Glyph("$"));
        assert_eq!(shell_mark(Some("zsh"), "deploy"), Mark::Glyph("%"));
        assert_eq!(shell_mark(Some("fish"), "deploy"), Mark::Glyph("~>"));
        assert_eq!(shell_mark(Some("pwsh"), "deploy"), Mark::Glyph("PS"));
        assert_eq!(shell_mark(Some("powershell"), "deploy"), Mark::Glyph("PS"));
    }

    #[test]
    fn root_is_root_whatever_the_shell() {
        assert_eq!(shell_mark(Some("zsh"), "root"), Mark::Glyph("#"));
        assert_eq!(shell_mark(None, "root"), Mark::Glyph("#"));
    }

    #[test]
    fn any_other_shell_keeps_the_icon() {
        let icon = Mark::Icon(IconName::Terminal);
        assert_eq!(shell_mark(Some("sh"), "deploy"), icon);
        assert_eq!(shell_mark(Some("dash"), "deploy"), icon);
        assert_eq!(shell_mark(None, "deploy"), icon);
    }
}
