//! The tile that identifies a host wherever it is shown: its icon or text on its color.
use super::icon::{Icon, IconName};
use crate::theme::ActiveTheme as _;
use gpui::prelude::*;
use gpui::{
    Animation, AnimationExt as _, App, FontWeight, Hsla, SharedString, Window, div, hsla, px,
};
use shcp_core::HostAppearance;
use std::time::Duration;

/// Most characters a tile can hold.
pub const MAX_TEXT: usize = 2;

macro_rules! colors {
    ($($variant:ident => $slug:literal, $hue:literal, $saturation:literal, $oklch:literal),* $(,)?) => {
        /// A fixed palette, so the white glyph stays readable on every tile.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum HostColor { $($variant),* }
        impl HostColor {
            pub const ALL: &'static [HostColor] = &[$(HostColor::$variant),*];
            /// The name saved in the settings: keep it stable.
            pub fn slug(self) -> &'static str {
                match self { $(HostColor::$variant => $slug),* }
            }
            pub fn hsla(self) -> Hsla {
                match self { $(HostColor::$variant => hsla($hue / 360., $saturation, 0.45, 1.)),* }
            }
            /// Its hue in OKLCH: the light of the window while on this host.
            pub fn hue(self) -> f32 {
                match self { $(HostColor::$variant => $oklch),* }
            }
        }
    };
}

colors! {
    Red => "red", 0., 0.55, 25.,
    Orange => "orange", 24., 0.55, 48.,
    Amber => "amber", 45., 0.55, 85.,
    Green => "green", 142., 0.55, 145.,
    Teal => "teal", 168., 0.55, 185.,
    Cyan => "cyan", 190., 0.55, 215.,
    Blue => "blue", 217., 0.55, 255.,
    Violet => "violet", 262., 0.55, 305.,
    Pink => "pink", 330., 0.55, 350.,
    Gray => "gray", 0., 0., 268.,
}

impl HostColor {
    pub fn from_slug(slug: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|color| color.slug() == slug)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Glyph {
    Icon(IconName),
    Text(SharedString),
}

/// What a host's tile shows, with the defaults filled in.
#[derive(Debug, Clone, PartialEq)]
pub struct HostLook {
    pub glyph: Glyph,
    pub color: HostColor,
    /// The short tag that travels with the host: its environment.
    pub tag: Option<&'static str>,
}

/// Whether a name holds a word: `prod-db`, `Production`, `ec2.local`, `db1`, but not `product`.
fn has_word(name: &str, wanted: &[&str]) -> bool {
    let name = name.to_lowercase();
    let whole = name.split(|c: char| !c.is_alphanumeric());
    whole
        .chain(words(&name).iter().map(String::as_str))
        .any(|word| wanted.contains(&word))
}

const PRODUCTION: &[&str] = &["prod", "production", "prd", "live"];
const STAGING: &[&str] = &["staging", "stage", "stg", "preprod", "uat"];

/// The words of a name: anything else separates them, and so does going from letters to digits.
fn words(alias: &str) -> Vec<String> {
    let mut words: Vec<String> = Vec::new();
    let mut current = String::new();
    for c in alias.chars() {
        let continues = current
            .chars()
            .last()
            .is_none_or(|last| last.is_numeric() == c.is_numeric());
        if (!c.is_alphanumeric() || !continues) && !current.is_empty() {
            words.push(std::mem::take(&mut current));
        }
        if c.is_alphanumeric() {
            current.push(c);
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

/// What fits on a tile.
pub fn clip_text(text: &str) -> String {
    text.trim().chars().take(MAX_TEXT).collect()
}

impl HostLook {
    /// Text wins over an icon, and whatever is missing or unknown keeps the default: a gray tile
    /// with a server. The name and the group only say which environment it is.
    pub fn resolve(alias: &str, group: Option<&str>, appearance: Option<&HostAppearance>) -> Self {
        let text = appearance
            .and_then(|a| a.text.as_deref())
            .map(clip_text)
            .filter(|text| !text.is_empty());
        let icon = appearance
            .and_then(|a| a.icon.as_deref())
            .and_then(IconName::from_slug);
        let glyph = match (text, icon) {
            (Some(text), _) => Glyph::Text(text.into()),
            (None, Some(icon)) => Glyph::Icon(icon),
            (None, None) => Glyph::Icon(IconName::Server),
        };
        let production =
            has_word(alias, PRODUCTION) || group.is_some_and(|g| has_word(g, PRODUCTION));
        let staging = has_word(alias, STAGING) || group.is_some_and(|g| has_word(g, STAGING));
        let color = appearance
            .and_then(|a| a.color.as_deref())
            .and_then(HostColor::from_slug)
            .unwrap_or(HostColor::Gray);
        let tag = if production {
            Some("PROD")
        } else if staging {
            Some("STAGING")
        } else {
            None
        };
        Self { glyph, color, tag }
    }
}

#[derive(Clone, Copy)]
struct Metrics {
    size: f32,
    radius: f32,
    icon: f32,
    /// Text size for one character and for more.
    text: (f32, f32),
}

const SMALL: Metrics = Metrics {
    size: 18.,
    radius: 5.,
    icon: 11.,
    text: (10., 8.),
};
const REGULAR: Metrics = Metrics {
    size: 22.,
    radius: 6.,
    icon: 12.,
    text: (11., 9.),
};
const LARGE: Metrics = Metrics {
    size: 40.,
    radius: 11.,
    icon: 22.,
    text: (20., 16.),
};

/// What a tile says of the connection: an outline at rest, filled when connected.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TileState {
    Idle,
    Live,
    /// On its way, and pulsing.
    Connecting,
    /// Lost, and trying again.
    Lost,
}

#[derive(IntoElement)]
pub struct HostTile {
    look: HostLook,
    metrics: Metrics,
    state: TileState,
}

impl HostTile {
    /// Filled, as a host looks on its own. Lists say with `state` how the connection is.
    pub fn new(look: HostLook) -> Self {
        Self {
            look,
            metrics: REGULAR,
            state: TileState::Live,
        }
    }
    pub fn state(mut self, state: TileState) -> Self {
        self.state = state;
        self
    }
    /// For the palette and what follows the pointer.
    pub fn small(mut self) -> Self {
        self.metrics = SMALL;
        self
    }
    /// For the preview while customizing.
    pub fn large(mut self) -> Self {
        self.metrics = LARGE;
        self
    }
}

impl RenderOnce for HostTile {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Metrics {
            size,
            radius,
            icon,
            text,
        } = self.metrics;
        let theme = cx.theme();
        let color = theme.host_color(self.look.color);
        let (fill, ink, line) = match self.state {
            TileState::Live => (Some(color), theme.on_primary, None),
            TileState::Lost => (None, theme.warning, Some(theme.warning)),
            TileState::Idle | TileState::Connecting => {
                (None, color, Some(Hsla { a: 0.42, ..color }))
            }
        };
        let pulsing = matches!(self.state, TileState::Lost | TileState::Connecting);
        let tile = div()
            .size(px(size))
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .rounded(px(radius))
            .when_some(fill, |tile, fill| tile.bg(fill))
            .when_some(line, |tile, line| tile.border_1().border_color(line))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(ink)
            .map(|tile| match self.look.glyph {
                Glyph::Icon(name) => tile.child(Icon::new(name).size(px(icon)).color(ink)),
                Glyph::Text(glyph) => {
                    let one = glyph.chars().count() < 2;
                    tile.text_size(px(if one { text.0 } else { text.1 }))
                        .child(glyph)
                }
            });
        if pulsing {
            tile.with_animation(
                "tile-pulse",
                Animation::new(Duration::from_millis(1200)).repeat(),
                |tile, delta| {
                    let wave = (delta * std::f32::consts::TAU).cos() * 0.5 + 0.5;
                    tile.opacity(0.45 + 0.55 * wave)
                },
            )
            .into_any_element()
        } else {
            tile.into_any_element()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn appearance(icon: Option<&str>, text: Option<&str>, color: Option<&str>) -> HostAppearance {
        HostAppearance {
            icon: icon.map(str::to_string),
            text: text.map(str::to_string),
            color: color.map(str::to_string),
        }
    }

    #[test]
    fn a_name_says_nothing_about_the_look() {
        let look = HostLook::resolve("postgres-prod", Some("Databases"), None);
        assert_eq!(look.glyph, Glyph::Icon(IconName::Server));
        assert_eq!(look.color, HostColor::Gray);
    }

    #[test]
    fn production_and_staging_are_told_by_name_or_group() {
        let tag = |alias, group| HostLook::resolve(alias, group, None).tag;
        assert_eq!(tag("web-01", Some("Production")), Some("PROD"));
        assert_eq!(tag("staging", None), Some("STAGING"));
        assert_eq!(tag("product-site", None), None);
    }

    #[test]
    fn red_is_only_a_color() {
        let picked = appearance(None, None, Some("red"));
        assert_eq!(HostLook::resolve("nas", None, Some(&picked)).tag, None);
    }

    #[test]
    fn text_wins_over_icon_and_is_clipped() {
        let picked = appearance(Some("database"), Some(" prod "), Some("red"));
        let look = HostLook::resolve("x", None, Some(&picked));
        assert_eq!(look.glyph, Glyph::Text("pr".into()));
        assert_eq!(look.color, HostColor::Red);
    }

    #[test]
    fn unknown_names_keep_the_default() {
        let picked = appearance(Some("no-such-icon"), Some("   "), Some("no-such-color"));
        assert_eq!(
            HostLook::resolve("nas", None, Some(&picked)),
            HostLook::resolve("nas", None, None)
        );
    }

    #[test]
    fn every_host_icon_and_color_survives_its_saved_name() {
        for icon in super::super::icon::HOST_ICONS {
            assert_eq!(IconName::from_slug(icon.slug()), Some(*icon));
        }
        for color in HostColor::ALL {
            assert_eq!(HostColor::from_slug(color.slug()), Some(*color));
        }
    }
}
