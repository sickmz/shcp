//! The color of a host, and the dot that shows it wherever the host is listed.
use gpui::prelude::*;
use gpui::{Div, Hsla, div, hsla, px};

macro_rules! colors {
    ($($variant:ident => $slug:literal, $hue:literal, $saturation:literal, $oklch:literal),* $(,)?) => {
        /// A fixed palette.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub(crate) enum HostColor { $($variant),* }
        impl HostColor {
            pub(crate) const ALL: &'static [HostColor] = &[$(HostColor::$variant),*];
            /// The name saved in the settings: keep it stable.
            pub(crate) fn slug(self) -> &'static str {
                match self { $(HostColor::$variant => $slug),* }
            }
            pub(crate) fn hsla(self) -> Hsla {
                match self { $(HostColor::$variant => hsla($hue / 360., $saturation, 0.45, 1.)),* }
            }
            /// Its hue in OKLCH: the light of the window while on this host.
            pub(crate) fn hue(self) -> f32 {
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
    pub(crate) fn from_slug(slug: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|color| color.slug() == slug)
    }
}

/// Filled when the host is connected, an outline at rest.
pub(crate) fn color_dot(color: Hsla, filled: bool) -> Div {
    let dot = div().size(px(7.)).flex_none().rounded_full();
    if filled {
        dot.bg(color)
    } else {
        dot.border_1().border_color(Hsla { a: 0.7, ..color })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_color_survives_its_saved_name() {
        for color in HostColor::ALL {
            assert_eq!(HostColor::from_slug(color.slug()), Some(*color));
        }
    }

    #[test]
    fn an_unknown_name_is_no_color() {
        assert_eq!(HostColor::from_slug("no-such-color"), None);
    }
}
