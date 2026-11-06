use crate::{Palette, Rgb};
use alacritty_terminal::{
    term::color::Colors,
    vte::ansi::{Color, NamedColor},
};
pub(crate) fn indexed(palette: &Palette, index: usize) -> Rgb {
    match index {
        0..=15 => palette.ansi[index],
        16..=231 => {
            let n = index - 16;
            let component = |n| if n == 0 { 0 } else { (55 + n * 40) as u8 };
            Rgb(component(n / 36), component((n / 6) % 6), component(n % 6))
        }
        232..=255 => {
            let v = (8 + (index - 232) * 10) as u8;
            Rgb(v, v, v)
        }
        n if n == NamedColor::Background as usize => palette.bg,
        n if n == NamedColor::Cursor as usize => palette.cursor,
        _ => palette.fg,
    }
}
pub(crate) fn resolve(palette: &Palette, colors: &Colors, color: Color) -> Rgb {
    match color {
        Color::Spec(rgb) => Rgb(rgb.r, rgb.g, rgb.b),
        Color::Indexed(i) => colors[i as usize]
            .map(|c| Rgb(c.r, c.g, c.b))
            .unwrap_or_else(|| indexed(palette, i as usize)),
        Color::Named(n) => colors[n as usize]
            .map(|c| Rgb(c.r, c.g, c.b))
            .unwrap_or_else(|| {
                let i = n as usize;
                // Bright/dim foreground variants fall back to the palette foreground.
                if i >= NamedColor::DimBlack as usize && i <= NamedColor::DimWhite as usize {
                    let Rgb(r, g, b) = palette.ansi[i - NamedColor::DimBlack as usize];
                    Rgb(r / 2, g / 2, b / 2)
                } else {
                    indexed(palette, i)
                }
            }),
    }
}
