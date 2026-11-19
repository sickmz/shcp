//! Built-in themes: dark, each grown from one color, the base of its content. The sidebar is that
//! color a little lighter in OKLCH, with the same chroma and hue; the rest follows from the base.
//! They take the user's accent, or bring their own.
use super::oklab::Oklch;
use super::{Theme, to_rgb};
use gpui::{Hsla, Rgba};
use shcp_core::Accent;
use shcp_core::term::Palette;

/// The hue of the blue accent.
const HUE: f32 = 268.;

/// How much lighter than the content the sidebar and the window around the cards are, in OKLCH
/// lightness from 0 to 1.
pub(crate) const SIDEBAR_LIFT: f32 = 0.009;

pub(crate) const BLACK: &str = "Black";
pub(crate) const TERRACOTTA: &str = "Terracotta";

/// Lightness and chroma of the accent, and lightness of what is written on it.
pub(super) const DARK_LIT: (f32, f32, f32) = (0.80, 0.135, 0.17);
pub(super) const LIGHT_LIT: (f32, f32, f32) = (0.50, 0.15, 0.985);

/// One of the app's themes.
struct Spec {
    name: &'static str,
    /// The terminals and the files, `0xRRGGBB`.
    base: u32,
    /// The theme's own accent, in OKLCH.
    accent: (f32, f32, f32),
}

/// In the order they are offered: Black first, then around the hues.
const SPECS: [Spec; 12] = [
    // Near-black neutral, a clear azure.
    Spec {
        name: BLACK,
        base: 0x0e0e0e,
        accent: (0.80, 0.10, 255.),
    },
    // Neutral gray with a breath of blue, a warm orange.
    Spec {
        name: "Graphite",
        base: 0x181a1b,
        accent: (0.78, 0.12, 55.),
    },
    // Cool blue-gray, a pale sky.
    Spec {
        name: "Slate",
        base: 0x141a1f,
        accent: (0.80, 0.09, 225.),
    },
    // Deep blue, brass.
    Spec {
        name: "Navy",
        base: 0x0e1625,
        accent: (0.86, 0.11, 90.),
    },
    // Blue-violet, lavender.
    Spec {
        name: "Ink",
        base: 0x131322,
        accent: (0.80, 0.10, 290.),
    },
    // Dusky purple, orchid.
    Spec {
        name: "Plum",
        base: 0x1e131d,
        accent: (0.80, 0.11, 335.),
    },
    // Burgundy, rose.
    Spec {
        name: "Wine",
        base: 0x221013,
        accent: (0.78, 0.11, 12.),
    },
    // Dark fired clay, terracotta.
    Spec {
        name: TERRACOTTA,
        base: 0x221510,
        accent: (0.74, 0.12, 40.),
    },
    // Coffee brown, caramel.
    Spec {
        name: "Espresso",
        base: 0x1c1610,
        accent: (0.82, 0.09, 72.),
    },
    // Dark olive, citron.
    Spec {
        name: "Olive",
        base: 0x181910,
        accent: (0.84, 0.11, 112.),
    },
    // Pine green, sage.
    Spec {
        name: "Forest",
        base: 0x101a13,
        accent: (0.82, 0.11, 155.),
    },
    // Deep teal, aqua.
    Spec {
        name: "Lagoon",
        base: 0x0b1b1c,
        accent: (0.82, 0.09, 195.),
    },
];

/// The names of the app's themes, in the order they are offered.
pub(crate) fn names() -> impl Iterator<Item = &'static str> {
    SPECS.iter().map(|spec| spec.name)
}

fn spec(name: &str) -> Option<&'static Spec> {
    SPECS.iter().find(|spec| spec.name == name)
}

fn hsla(rgb: [f32; 3], a: f32) -> Hsla {
    let [r, g, b] = rgb;
    Rgba { r, g, b, a }.into()
}

/// An OKLCH color, as sRGB in gpui's terms. What falls outside sRGB is clipped.
pub(crate) fn oklch(l: f32, c: f32, h: f32, a: f32) -> Hsla {
    hsla(Oklch::new(l, c, h).to_srgb().map(|v| v.clamp(0., 1.)), a)
}

/// An OKLCH color brought into sRGB with its lightness and hue.
fn fit(color: Oklch, a: f32) -> Hsla {
    hsla(color.to_srgb_in_gamut(), a)
}

/// The sidebar and the window around the cards for a content of `base`: `SIDEBAR_LIFT` lighter,
/// with the same chroma and hue.
pub(crate) fn sidebar_of(base: Oklch) -> Oklch {
    base.lighter(SIDEBAR_LIFT)
}

/// An accent as a theme draws it, and what is written on it.
struct Lit {
    l: f32,
    c: f32,
    h: f32,
    on_l: f32,
    on_c: f32,
}

/// `own`: the accent the theme brings, for `Accent::Theme`.
fn lit(accent: Accent, is_light: bool, own: Option<(f32, f32, f32)>) -> Lit {
    let h = match accent {
        Accent::Theme => match own {
            Some((l, c, h)) => {
                return Lit {
                    l,
                    c,
                    h,
                    on_l: DARK_LIT.2,
                    on_c: 0.03,
                };
            }
            None => HUE,
        },
        Accent::Blue | Accent::Mono => HUE,
        Accent::Violet => 305.,
        Accent::Pink => 350.,
        Accent::Red => 25.,
        Accent::Orange => 55.,
        Accent::Amber => 85.,
        Accent::Green => 150.,
        Accent::Teal => 195.,
    };
    if accent == Accent::Mono {
        let (l, on_l) = if is_light {
            (0.17, 0.985)
        } else {
            (0.97, 0.17)
        };
        return Lit {
            l,
            c: 0.,
            h,
            on_l,
            on_c: 0.,
        };
    }
    let (l, c, on_l) = if is_light { LIGHT_LIT } else { DARK_LIT };
    Lit {
        l,
        c,
        h,
        on_l,
        on_c: 0.03,
    }
}

/// The color of an accent on `theme`, for showing the choice. A theme's own accent is the one
/// of the app's theme by that name; a theme from a file shows the one it has.
pub(crate) fn accent_color(accent: Accent, theme: &Theme) -> Hsla {
    if accent == Accent::Theme && theme.file.is_some() {
        return theme.accent;
    }
    let lit = lit(
        accent,
        theme.is_light,
        spec(&theme.name).map(|spec| spec.accent),
    );
    oklch(lit.l, lit.c, lit.h, 1.)
}

/// The app's theme `name` in `accent`. A name that is not one of them gives Black.
pub(crate) fn build(name: &str, accent: Accent) -> Theme {
    from_spec(spec(name).unwrap_or(&SPECS[0]), accent)
}

fn from_spec(spec: &Spec, accent: Accent) -> Theme {
    let base = Oklch::from_hex(spec.base);
    let Oklch { c, h, .. } = base;
    // The base at another lightness: what stands on the content or the sidebar.
    let shade = |dl: f32| fit(base.lighter(dl), 1.);
    let content: Hsla = gpui::rgb(spec.base).into();
    let sidebar = fit(sidebar_of(base), 1.);
    // Text takes a little of the base's color, the quieter grays a little more.
    let ink_c = (c * 0.35).min(0.012);
    let gray_c = (c * 0.6).min(0.022);
    let ink = fit(Oklch::new(0.95, ink_c, h), 1.);
    let muted = fit(Oklch::new(0.82, gray_c, h), 1.);
    let raise = shade(0.065);
    let Lit {
        l: lit_l,
        c: lit_c,
        h: lit_h,
        on_l,
        on_c,
    } = lit(accent, false, Some(spec.accent));
    let lit = |a: f32| fit(Oklch::new(lit_l, lit_c, lit_h), a);
    let color = |l: f32, c: f32, h: f32| fit(Oklch::new(l, c, h), 1.);
    let green = color(0.82, 0.13, 152.);
    let red = color(0.74, 0.15, 25.);
    let amber = color(0.84, 0.12, 85.);
    let line = |a: f32| fit(Oklch::new(0.95, c.min(0.012), h), a);
    Theme {
        name: spec.name.into(),
        origin: None,
        file: None,
        is_light: false,
        bg: content,
        surface: sidebar,
        elevated: raise,
        input_bg: shade(-0.025),
        border: line(0.095),
        strong_border: line(0.17),
        text: ink,
        text_muted: muted,
        text_disabled: fit(Oklch::new(0.70, gray_c, h), 1.),
        accent: lit(1.),
        primary: lit(1.),
        on_primary: fit(Oklch::new(on_l, on_c, lit_h), 1.),
        success: green,
        warning: amber,
        error: red,
        hover: Hsla { a: 0.055, ..ink },
        active: shade(0.1),
        selected: lit(0.15),
        lit_line: lit(0.42),
        scrim: fit(Oklch::new(0.1, c * 0.5, h), 0.46),
        window_bg: sidebar,
        track: raise,
        // Terminals and files alike sit on the base.
        terminal_bg: content,
        terminal_selection: lit(0.3),
        // White in any accent: the accent colors the app, not what is typed.
        terminal_cursor: ink,
        lit_l: DARK_LIT.0,
        lit_c,
        palette: Palette {
            ansi: [
                to_rgb(shade(0.17)),
                to_rgb(red),
                to_rgb(green),
                to_rgb(amber),
                to_rgb(color(0.78, 0.1, 250.)),
                to_rgb(color(0.78, 0.11, 320.)),
                to_rgb(color(0.82, 0.09, 195.)),
                to_rgb(muted),
                to_rgb(fit(Oklch::new(0.62, gray_c, h), 1.)),
                to_rgb(color(0.8, 0.14, 25.)),
                to_rgb(color(0.88, 0.13, 152.)),
                to_rgb(color(0.9, 0.12, 85.)),
                to_rgb(color(0.85, 0.09, 250.)),
                to_rgb(color(0.85, 0.1, 320.)),
                to_rgb(color(0.88, 0.08, 195.)),
                to_rgb(ink),
            ],
            fg: to_rgb(fit(Oklch::new(0.94, ink_c, h), 1.)),
            // Exactly the card: default cells stay transparent to it.
            bg: to_rgb(content),
            cursor: to_rgb(ink),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shcp_core::term::Rgb;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.01
    }

    #[test]
    fn oklch_matches_known_colors() {
        let white = oklch(1., 0., 0., 1.).to_rgb();
        assert!(close(white.r, 1.) && close(white.g, 1.) && close(white.b, 1.));
        let black = oklch(0., 0., 0., 1.).to_rgb();
        assert!(close(black.r, 0.) && close(black.g, 0.) && close(black.b, 0.));
        // sRGB red is oklch(0.628 0.2577 29.23).
        let red = oklch(0.628, 0.2577, 29.23, 1.).to_rgb();
        assert!(close(red.r, 1.) && close(red.g, 0.) && close(red.b, 0.));
    }

    /// Relative luminance, for the contrast the mockup promises.
    fn luminance(color: Hsla) -> f32 {
        let Rgba { r, g, b, .. } = color.to_rgb();
        let linear = |v: f32| {
            if v <= 0.040_45 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
    }

    fn contrast(a: Hsla, b: Hsla) -> f32 {
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    fn rgb(color: Rgb) -> Hsla {
        gpui::rgb(super::super::rgb_hex(color)).into()
    }

    fn oklch_of(color: Hsla) -> Oklch {
        let Rgba { r, g, b, .. } = color.to_rgb();
        Oklch::from_srgb([r, g, b])
    }

    /// The color as the screen shows it: 8 bits a channel.
    fn bytes(color: Hsla) -> [u8; 3] {
        let Rgb(r, g, b) = to_rgb(color);
        [r, g, b]
    }

    #[test]
    fn there_are_twelve_dark_themes_black_first() {
        let names: Vec<_> = names().collect();
        assert_eq!(names.len(), 12);
        assert_eq!(names[0], BLACK);
        assert!(names.contains(&TERRACOTTA));
        for (index, name) in names.iter().enumerate() {
            assert!(!names[index + 1..].contains(name), "{name} twice");
            let theme = build(name, Accent::Theme);
            assert_eq!(theme.name.as_ref(), *name);
            assert!(!theme.is_light);
        }
        assert_eq!(build("Nocturne", Accent::Theme).name.as_ref(), BLACK);
    }

    /// The rule the themes are made by: the content is the base; the sidebar is the base in
    /// OKLCH with 0.009 more lightness and the same chroma and hue.
    #[test]
    fn the_sidebar_is_the_base_a_little_lighter() {
        for spec in &SPECS {
            let base = Oklch::from_hex(spec.base);
            assert!(sidebar_of(base).in_gamut(), "{}", spec.name);
            for accent in Accent::ALL {
                let theme = build(spec.name, accent);
                let name = spec.name;
                assert_eq!(theme.bg, Hsla::from(gpui::rgb(spec.base)), "{name}");
                assert_eq!(theme.terminal_bg, theme.bg, "{name}");
                assert_eq!(theme.palette.bg, to_rgb(theme.bg), "{name}");
                assert_eq!(theme.window_bg, theme.surface, "{name}");
                // Exactly, before the screen rounds it.
                let sidebar = oklch_of(theme.surface);
                assert!((sidebar.l - base.l - 0.009).abs() < 1e-4, "{name}");
                assert!((sidebar.c - base.c).abs() < 1e-4, "{name}");
                if base.c > 0.002 {
                    assert!((sidebar.h - base.h).abs() < 0.1, "{name}");
                }
                // And as the screen shows it, 8 bits a channel.
                let expected = hsla(sidebar_of(base).to_srgb_in_gamut(), 1.);
                assert_eq!(bytes(theme.surface), bytes(expected), "{name}");
                let shown = oklch_of(rgb(to_rgb(theme.surface)));
                assert!((shown.l - base.l - 0.009).abs() < 0.0025, "{name}");
                assert!((shown.c - base.c).abs() < 0.0025, "{name}");
                if base.c > 0.01 {
                    assert!((shown.h - base.h).abs() < 3., "{name}");
                }
                assert_ne!(bytes(theme.surface), bytes(theme.bg), "{name}");
            }
        }
    }

    #[test]
    fn text_stays_readable() {
        for name in names() {
            for accent in Accent::ALL {
                let theme = build(name, accent);
                assert!(contrast(theme.text, theme.bg) >= 7., "{name}");
                assert!(contrast(theme.text, theme.surface) >= 7., "{name}");
                assert!(contrast(theme.text_muted, theme.surface) >= 4.5, "{name}");
                assert!(contrast(theme.text_muted, theme.elevated) >= 4.5, "{name}");
                assert!(contrast(theme.text_disabled, theme.bg) >= 4.5, "{name}");
                assert!(contrast(theme.accent, theme.bg) >= 4.5, "{name} {accent:?}");
                assert!(
                    contrast(theme.on_primary, theme.primary) >= 4.5,
                    "{name} {accent:?}"
                );
            }
        }
    }

    #[test]
    fn terminal_colors_stay_readable() {
        for name in names() {
            let theme = build(name, Accent::Theme);
            let Palette { ansi, fg, bg, .. } = theme.palette;
            let bg = rgb(bg);
            assert!(contrast(rgb(fg), bg) >= 7., "{name}");
            for (index, color) in ansi.into_iter().enumerate() {
                let ratio = contrast(rgb(color), bg);
                match index {
                    // Black is meant to sit close to the background.
                    0 => assert!(ratio >= 1.4, "{name} black {ratio}"),
                    // Bright black: the dim text of prompts and suggestions.
                    8 => assert!(ratio >= 3., "{name} bright black {ratio}"),
                    _ => assert!(ratio >= 4.5, "{name} color {index} {ratio}"),
                }
            }
        }
    }
}
