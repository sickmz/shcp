//! Builds a `Theme` from a VS Code color theme file (JSON with comments and trailing commas).
use super::builtin::{DARK_LIT, LIGHT_LIT};
use super::{Theme, rgb_hex, to_rgb};
use anyhow::{Context, Result};
use gpui::{Hsla, Rgba, SharedString, hsla, rgb, rgba};
use serde_json::{Map, Value};
use shcp_core::term::{Palette, Rgb};

/// Removes `//` and `/* */` comments and trailing commas, leaving strings untouched.
pub(super) fn strip_jsonc(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            match c {
                '\\' => out.extend(chars.next()),
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match (c, chars.peek().copied()) {
            ('"', _) => {
                in_string = true;
                out.push(c);
            }
            ('/', Some('/')) => {
                for next in chars.by_ref() {
                    if next == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            ('/', Some('*')) => {
                chars.next();
                let mut previous = ' ';
                for next in chars.by_ref() {
                    if previous == '*' && next == '/' {
                        break;
                    }
                    previous = next;
                }
            }
            ('}' | ']', _) => {
                let kept = out.trim_end().len();
                if out[..kept].ends_with(',') {
                    out.truncate(kept - 1);
                }
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

/// Parses `#rgb`, `#rgba`, `#rrggbb` or `#rrggbbaa` into 0xRRGGBBAA.
fn parse_hex(value: &str) -> Option<u32> {
    let hex = value.strip_prefix('#')?;
    if !hex.is_ascii() {
        return None;
    }
    let number = u32::from_str_radix(hex, 16).ok()?;
    let nibble = |shift: u32| (number >> shift) & 0xf;
    let wide = |n: u32| n * 0x11;
    match hex.len() {
        3 => Some(wide(nibble(8)) << 24 | wide(nibble(4)) << 16 | wide(nibble(0)) << 8 | 0xff),
        4 => Some(
            wide(nibble(12)) << 24 | wide(nibble(8)) << 16 | wide(nibble(4)) << 8 | wide(nibble(0)),
        ),
        6 => Some(number << 8 | 0xff),
        8 => Some(number),
        _ => None,
    }
}

struct Colors<'a>(&'a Map<String, Value>);

impl Colors<'_> {
    /// First key holding a valid color.
    fn raw(&self, keys: &[&str]) -> Option<u32> {
        keys.iter()
            .find_map(|key| self.0.get(*key)?.as_str().and_then(parse_hex))
    }
    fn get(&self, keys: &[&str]) -> Option<Hsla> {
        self.raw(keys).map(|value| rgba(value).into())
    }
    /// First key holding a color that is mostly opaque, so it stands on its own.
    fn solid(&self, keys: &[&str]) -> Option<Hsla> {
        keys.iter().find_map(|key| {
            self.raw(&[*key])
                .filter(|value| value & 0xff >= 0x80)
                .map(|value| rgba(value).into())
        })
    }
    fn ansi(&self, keys: &[&str], default: u32) -> Rgb {
        let value = self.raw(keys).unwrap_or(default << 8 | 0xff);
        Rgb((value >> 24) as u8, (value >> 16) as u8, (value >> 8) as u8)
    }
}

fn opaque(color: Hsla) -> Hsla {
    Hsla { a: 1., ..color }
}

fn alpha(color: Hsla, a: f32) -> Hsla {
    Hsla { a, ..color }
}

/// `amount` of `a` over the rest of `b`, opaque.
pub(super) fn mix(a: Hsla, b: Hsla, amount: f32) -> Hsla {
    let (a, b) = (a.to_rgb(), b.to_rgb());
    let blend = |x: f32, y: f32| x * amount + y * (1. - amount);
    Rgba {
        r: blend(a.r, b.r),
        g: blend(a.g, b.g),
        b: blend(a.b, b.b),
        a: 1.,
    }
    .into()
}

/// Black or white, whichever reads better on `color`.
fn contrast(color: Hsla) -> Hsla {
    hsla(0., 0., if color.l > 0.6 { 0.07 } else { 1. }, 1.)
}

const ANSI_DARK: [u32; 16] = [
    0x000000, 0xcd3131, 0x0dbc79, 0xe5e510, 0x2472c8, 0xbc3fbc, 0x11a8cd, 0xe5e5e5, 0x666666,
    0xf14c4c, 0x23d18b, 0xf5f543, 0x3b8eea, 0xd670d6, 0x29b8db, 0xe5e5e5,
];
const ANSI_LIGHT: [u32; 16] = [
    0x000000, 0xcd3131, 0x00bc00, 0x949800, 0x0451a5, 0xbc05bc, 0x0598bc, 0x555555, 0x666666,
    0xcd3131, 0x14ce14, 0xb5ba00, 0x0451a5, 0xbc05bc, 0x0598bc, 0xa5a5a5,
];
const ANSI_KEYS: [&str; 16] = [
    "terminal.ansiBlack",
    "terminal.ansiRed",
    "terminal.ansiGreen",
    "terminal.ansiYellow",
    "terminal.ansiBlue",
    "terminal.ansiMagenta",
    "terminal.ansiCyan",
    "terminal.ansiWhite",
    "terminal.ansiBrightBlack",
    "terminal.ansiBrightRed",
    "terminal.ansiBrightGreen",
    "terminal.ansiBrightYellow",
    "terminal.ansiBrightBlue",
    "terminal.ansiBrightMagenta",
    "terminal.ansiBrightCyan",
    "terminal.ansiBrightWhite",
];

fn build(name: SharedString, kind: Option<&str>, colors: &Colors) -> Theme {
    let bg = opaque(
        colors
            .solid(&["editor.background"])
            .unwrap_or(rgb(0x1e1e1e).into()),
    );
    let is_light = kind.map_or(bg.l > 0.5, |k| k.to_lowercase().contains("light"));
    let text = opaque(
        colors
            .solid(&["editor.foreground", "foreground"])
            .unwrap_or(if is_light {
                rgb(0x333333).into()
            } else {
                rgb(0xd4d4d4).into()
            }),
    );
    let surface = opaque(
        colors
            .solid(&["sideBar.background", "editorGroupHeader.tabsBackground"])
            .unwrap_or(mix(text, bg, 0.04)),
    );
    let elevated = opaque(
        colors
            .solid(&["editorWidget.background", "dropdown.background"])
            .unwrap_or(surface),
    );
    let border = colors
        .solid(&[
            "panel.border",
            "editorGroup.border",
            "sideBar.border",
            "contrastBorder",
        ])
        // A hairline: translucent, so it reads the same on every layer it crosses.
        .unwrap_or(alpha(text, if is_light { 0.13 } else { 0.1 }));
    let accent = opaque(
        colors
            .solid(&["focusBorder", "textLink.foreground", "button.background"])
            .unwrap_or(rgb(if is_light { 0x0969da } else { 0x4493f8 }).into()),
    );
    let primary = opaque(colors.solid(&["button.background"]).unwrap_or(accent));
    let defaults = if is_light { &ANSI_LIGHT } else { &ANSI_DARK };
    let ansi: [Rgb; 16] = std::array::from_fn(|i| colors.ansi(&[ANSI_KEYS[i]], defaults[i]));
    let color = |color: Rgb| -> Hsla { rgb(rgb_hex(color)).into() };
    let terminal_bg = opaque(colors.solid(&["terminal.background"]).unwrap_or(bg));
    let terminal_fg = opaque(colors.solid(&["terminal.foreground"]).unwrap_or(text));
    let terminal_cursor = opaque(
        colors
            .solid(&["terminalCursor.foreground", "editorCursor.foreground"])
            .unwrap_or(terminal_fg),
    );
    Theme {
        name,
        origin: None,
        file: None,
        is_light,
        bg,
        surface,
        elevated,
        input_bg: opaque(colors.solid(&["input.background"]).unwrap_or(bg)),
        border,
        strong_border: Hsla {
            a: (border.a * 1.8).min(1.),
            ..border
        },
        text,
        text_muted: colors
            .solid(&["descriptionForeground"])
            .unwrap_or(mix(text, bg, 0.7)),
        text_disabled: colors
            .solid(&["disabledForeground"])
            .unwrap_or(mix(text, bg, 0.45)),
        accent,
        primary,
        on_primary: colors
            .solid(&["button.foreground"])
            .unwrap_or(contrast(primary)),
        success: colors
            .solid(&["gitDecoration.addedResourceForeground"])
            .unwrap_or(color(ansi[2])),
        warning: colors
            .solid(&["editorWarning.foreground", "list.warningForeground"])
            .unwrap_or(color(ansi[3])),
        error: colors
            .solid(&["editorError.foreground", "errorForeground"])
            .unwrap_or(color(ansi[1])),
        hover: alpha(text, 0.07),
        active: alpha(text, 0.11),
        selected: alpha(accent, 0.2),
        lit_line: alpha(accent, 0.42),
        scrim: hsla(0., 0., 0., if is_light { 0.28 } else { 0.5 }),
        window_bg: surface,
        track: alpha(text, if is_light { 0.06 } else { 0.05 }),
        terminal_bg,
        terminal_selection: colors
            .get(&["terminal.selectionBackground", "editor.selectionBackground"])
            .unwrap_or(alpha(accent, 0.35)),
        terminal_cursor,
        lit_l: if is_light { LIGHT_LIT.0 } else { DARK_LIT.0 },
        lit_c: if is_light { LIGHT_LIT.1 } else { DARK_LIT.1 },
        palette: Palette {
            ansi,
            fg: to_rgb(terminal_fg),
            bg: to_rgb(terminal_bg),
            cursor: to_rgb(terminal_cursor),
        },
    }
}

/// Parses a theme file; `default_name` is used when it has no `name`.
pub(super) fn parse(text: &str, default_name: &str) -> Result<Theme> {
    let value: Value = serde_json::from_str(&strip_jsonc(text)).context("Invalid JSON")?;
    let colors = value
        .get("colors")
        .and_then(Value::as_object)
        .context("No \"colors\" section")?;
    let name = value
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or(default_name)
        .to_string();
    let kind = value.get("type").and_then(Value::as_str);
    let mut theme = build(name.into(), kind, &Colors(colors));
    theme.origin = value.get(super::open_vsx::ORIGIN_KEY).and_then(|origin| {
        let text = |key: &str| origin.get(key)?.as_str().map(str::to_string);
        Some(super::ThemeOrigin {
            extension: text("extension")?.into(),
            extension_name: text("displayName")?.into(),
        })
    });
    Ok(theme)
}
