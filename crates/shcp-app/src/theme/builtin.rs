//! Built-in themes: cool Nocturne and warm Prune, both in smoked glass unless the user turns
//! the effects off.
//! All keep the user's configurable accent.
use super::{Theme, to_rgb};
use gpui::{Hsla, Rgba};
use shcp_core::Accent;
use shcp_core::term::Palette;

/// The hue of the grays, and of the blue accent.
pub(crate) const HUE: f32 = 268.;
/// How much of that hue the grays take.
const TINT: f32 = 0.18;

pub(crate) const NOCTURNE: &str = "Nocturne";
pub(crate) const PRUNE: &str = "Prune";

/// Lightness and chroma of the accent, and lightness of what is written on it.
pub(super) const DARK_LIT: (f32, f32, f32) = (0.80, 0.135, 0.17);
pub(super) const LIGHT_LIT: (f32, f32, f32) = (0.50, 0.15, 0.985);

/// An OKLCH color, as sRGB in gpui's terms. What falls outside sRGB is clipped.
pub(crate) fn oklch(l: f32, c: f32, h: f32, a: f32) -> Hsla {
    let radians = h.to_radians();
    let (ca, cb) = (c * radians.cos(), c * radians.sin());
    let cube = |v: f32| v * v * v;
    let lms_l = cube(l + 0.396_337_78 * ca + 0.215_803_76 * cb);
    let lms_m = cube(l - 0.105_561_346 * ca - 0.063_854_17 * cb);
    let lms_s = cube(l - 0.089_484_18 * ca - 1.291_485_5 * cb);
    let gamma = |v: f32| {
        let v = v.clamp(0., 1.);
        if v <= 0.003_130_8 {
            12.92 * v
        } else {
            1.055 * v.powf(1. / 2.4) - 0.055
        }
    };
    Rgba {
        r: gamma(4.076_741_7 * lms_l - 3.307_711_6 * lms_m + 0.230_969_94 * lms_s),
        g: gamma(-1.268_438 * lms_l + 2.609_757_4 * lms_m - 0.341_319_38 * lms_s),
        b: gamma(-0.004_196_086_3 * lms_l - 0.703_418_6 * lms_m + 1.707_614_7 * lms_s),
        a,
    }
    .into()
}

/// An accent as a theme draws it, and what is written on it.
struct Lit {
    l: f32,
    c: f32,
    h: f32,
    on_l: f32,
    on_c: f32,
}

fn lit(accent: Accent, is_light: bool) -> Lit {
    let h = match accent {
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

/// The color of an accent, for showing the choice.
pub(crate) fn accent_color(accent: Accent, is_light: bool) -> Hsla {
    let lit = lit(accent, is_light);
    oklch(lit.l, lit.c, lit.h, 1.)
}

pub(crate) fn build(name: &str, accent: Accent) -> Theme {
    match name {
        PRUNE => prune(accent),
        _ => nocturne(accent),
    }
}

/// The smoked-glass reference as theme tokens: plum charcoal, brown shadows and a restrained
/// copper selection. Its cards and its window take the material too: see `Theme::material`.
fn prune(accent: Accent) -> Theme {
    let mut theme = nocturne(accent);
    let color = |hex| -> Hsla { gpui::rgb(hex).into() };
    let ink = color(0xece3df);
    theme.name = PRUNE.into();
    // One plum, at three depths: a modal's side column, on `surface`, and its content, on
    // `elevated`, must read as two shades of one color and not as a gray beside a purple.
    theme.bg = color(0x2e272b);
    theme.surface = color(0x2b2428);
    theme.elevated = color(0x362c32);
    theme.input_bg = color(0x211d1f);
    theme.border = Hsla { a: 0.095, ..ink };
    theme.strong_border = Hsla { a: 0.17, ..ink };
    theme.text = ink;
    theme.text_muted = color(0xc2b2ac);
    theme.text_disabled = color(0xa59690);
    theme.hover = Hsla { a: 0.055, ..ink };
    theme.active = color(0x402e2b);
    theme.scrim = Hsla {
        a: 0.46,
        ..color(0x211d1f)
    };
    theme.window_bg = color(0x211d1f);
    theme.track = color(0x362c32);
    // The reference's surface where no tint of the material reaches: see `Theme::material`.
    theme.terminal_bg = color(0x22201e);
    // Keep the engine's default background exactly aligned with the UI: default
    // cells must remain transparent to the shared panel material.
    theme.palette.bg = to_rgb(theme.terminal_bg);
    theme.palette.fg = to_rgb(ink);
    theme.terminal_cursor = ink;
    theme.palette.cursor = to_rgb(ink);
    theme.palette.ansi[0] = to_rgb(color(0x514443));
    theme.palette.ansi[7] = to_rgb(color(0xc2b2ac));
    theme.palette.ansi[8] = to_rgb(color(0x9a8279));
    theme.palette.ansi[15] = to_rgb(ink);
    theme
}

fn nocturne(accent: Accent) -> Theme {
    let (tint, h) = (TINT, HUE);
    let n = 0.015 * tint;
    let Lit {
        l: lit_l,
        c: lit_c,
        h: accent,
        on_l,
        on_c,
    } = lit(accent, false);
    let window = oklch(0.178, n, h, 1.);
    let stage = oklch(0.132, n, h, 1.);
    let raise = oklch(0.232, n, h, 1.);
    let ink = oklch(0.97, 0.006 * tint, h, 1.);
    let green = oklch(0.82, 0.13, 152., 1.);
    let red = oklch(0.74, 0.15, 25., 1.);
    let amber = oklch(0.84, 0.12, 85., 1.);
    let blue = oklch(0.78, 0.1, 250., 1.);
    let magenta = oklch(0.78, 0.11, 320., 1.);
    let cyan = oklch(0.82, 0.09, 195., 1.);
    let lit = oklch(lit_l, lit_c, accent, 1.);
    let foreground = oklch(0.96, 0.006 * tint, h, 1.);
    Theme {
        name: NOCTURNE.into(),
        origin: None,
        file: None,
        is_light: false,
        bg: stage,
        surface: window,
        elevated: raise,
        input_bg: stage,
        border: oklch(0.95, 0.012, h, 0.095),
        strong_border: oklch(0.95, 0.012, h, 0.17),
        text: ink,
        text_muted: oklch(0.84, 0.01 * tint, h, 1.),
        text_disabled: oklch(0.72, 0.012 * tint, h, 1.),
        accent: lit,
        primary: lit,
        on_primary: oklch(on_l, on_c, accent, 1.),
        success: green,
        warning: amber,
        error: red,
        hover: Hsla { a: 0.055, ..ink },
        active: oklch(0.28, n, h, 1.),
        selected: oklch(lit_l, lit_c, accent, 0.15),
        lit_line: oklch(lit_l, lit_c, accent, 0.42),
        scrim: oklch(0.1, 0.01, h, 0.46),
        window_bg: window,
        panel_alpha: 1.,
        track: raise,
        terminal_bg: stage,
        terminal_selection: oklch(lit_l, lit_c, accent, 0.3),
        // White in any accent: the accent colors the app, not what is typed.
        terminal_cursor: ink,
        lit_l: DARK_LIT.0,
        lit_c,
        palette: Palette {
            ansi: [
                to_rgb(oklch(0.32, n, h, 1.)),
                to_rgb(red),
                to_rgb(green),
                to_rgb(amber),
                to_rgb(blue),
                to_rgb(magenta),
                to_rgb(cyan),
                to_rgb(oklch(0.82, 0.01 * tint, h, 1.)),
                to_rgb(oklch(0.6, 0.014 * tint, h, 1.)),
                to_rgb(oklch(0.8, 0.14, 25., 1.)),
                to_rgb(oklch(0.88, 0.13, 152., 1.)),
                to_rgb(oklch(0.9, 0.12, 85., 1.)),
                to_rgb(oklch(0.85, 0.09, 250., 1.)),
                to_rgb(oklch(0.85, 0.1, 320., 1.)),
                to_rgb(oklch(0.88, 0.08, 195., 1.)),
                to_rgb(ink),
            ],
            fg: to_rgb(foreground),
            bg: to_rgb(stage),
            cursor: to_rgb(ink),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn text_stays_readable() {
        for name in [NOCTURNE, PRUNE] {
            for accent in Accent::ALL {
                let theme = build(name, accent);
                assert!(contrast(theme.text, theme.bg) >= 7., "{name}");
                assert!(contrast(theme.text_muted, theme.surface) >= 4.5, "{name}");
                assert!(contrast(theme.text_disabled, theme.bg) >= 4.5, "{name}");
                assert!(contrast(theme.accent, theme.bg) >= 4.5, "{name} {accent:?}");
                assert!(
                    contrast(theme.on_primary, theme.primary) >= 4.5,
                    "{name} {accent:?}"
                );
            }
        }
    }
}
