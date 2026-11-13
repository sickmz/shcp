//! The active theme and the catalog of available ones. Components read colors only from here.
//! Themes are VS Code color theme files: the app's own plus any in `<config_dir>/themes`, where
//! the ones installed from Open VSX end up too.
pub(crate) mod builtin;
pub(crate) mod icon_theme;
pub(crate) mod open_vsx;
mod vscode;

use crate::widgets::host_color::HostColor;
use gpui::{App, BoxShadow, Global, Hsla, Pixels, SharedString, WindowAppearance, hsla, point, px};
use shcp_core::{
    Density, EngineHandle, Settings,
    term::{Palette, Rgb},
};
use std::{
    path::{Path, PathBuf},
    sync::{
        OnceLock,
        atomic::{AtomicU32, Ordering},
    },
};

/// The Open VSX extension a theme was installed from.
#[derive(Clone)]
pub(crate) struct ThemeOrigin {
    pub extension: SharedString,
    pub extension_name: SharedString,
}

#[derive(Clone)]
pub(crate) struct Theme {
    pub name: gpui::SharedString,
    pub origin: Option<ThemeOrigin>,
    /// The file it was read from; `None` for the app's own themes.
    pub file: Option<PathBuf>,
    pub is_light: bool,
    pub bg: Hsla,
    pub surface: Hsla,
    pub elevated: Hsla,
    pub input_bg: Hsla,
    pub border: Hsla,
    /// A hairline that has to be seen: outlined buttons, the current card.
    pub strong_border: Hsla,
    pub text: Hsla,
    pub text_muted: Hsla,
    pub text_disabled: Hsla,
    pub accent: Hsla,
    pub primary: Hsla,
    pub on_primary: Hsla,
    pub success: Hsla,
    pub warning: Hsla,
    pub error: Hsla,
    pub hover: Hsla,
    /// Neutral fill of the current item in chrome: the active tab, the chosen host.
    pub active: Hsla,
    pub selected: Hsla,
    /// The accent as a line: outlines of idle host tiles and of what is on.
    pub lit_line: Hsla,
    pub scrim: Hsla,
    /// What the window shows between its panels.
    pub window_bg: Hsla,
    /// The groove the tabs sit in.
    pub track: Hsla,
    pub terminal_bg: Hsla,
    pub terminal_selection: Hsla,
    pub terminal_cursor: Hsla,
    /// Lightness and chroma any host's color is drawn at here. No chroma: the tiles are gray.
    pub lit_l: f32,
    pub lit_c: f32,
    pub palette: Palette,
}

impl Theme {
    /// This theme draws hosts in their colors. With the Mono accent they are all gray.
    pub(crate) fn shows_host_colors(&self) -> bool {
        self.lit_c > 0.
    }

    /// The color of a host, at the lightness this theme draws accents at.
    pub(crate) fn host_color(&self, color: HostColor) -> Hsla {
        let chroma = if color == HostColor::Gray {
            0.012
        } else {
            self.lit_c
        };
        builtin::oklch(self.lit_l, chroma, color.hue(), 1.)
    }

    /// The tile a session shows its host's color on, and the deck of a split one.
    pub(crate) fn tile_colors(&self) -> TileColors {
        TileColors {
            fill: Hsla {
                a: 0.08,
                ..self.text
            },
            back: Hsla {
                a: if self.is_light { 0.15 } else { 0.18 },
                ..self.text
            },
        }
    }

    /// The color under a row of the sidebar, made solid: the surface, or the fill of the current
    /// row over it. For what has to hide something on that row and still look like the row.
    pub(crate) fn row_fill(&self, current: bool) -> Hsla {
        if current {
            let solid = Hsla {
                a: 1.,
                ..self.active
            };
            vscode::mix(solid, self.surface, self.active.a)
        } else {
            self.surface
        }
    }
}

/// See `Theme::tile_colors`.
pub(crate) struct TileColors {
    /// The tile: see-through, so it reads the same on any row.
    pub fill: Hsla,
    /// The card behind the tile of a split session: see-through too, a little stronger.
    pub back: Hsla,
}

impl Global for Theme {}

pub(crate) trait ActiveTheme {
    fn theme(&self) -> &Theme;
}

impl ActiveTheme for App {
    fn theme(&self) -> &Theme {
        self.global::<Theme>()
    }
}

/// All themes the user can pick.
pub(crate) struct ThemeCatalog {
    pub themes: Vec<Theme>,
}

impl Global for ThemeCatalog {}

impl ThemeCatalog {
    /// Reads the bundled themes and `dir`. A user theme replaces a bundled one of the same name.
    /// Returns the catalog and one message per file that could not be read.
    pub(crate) fn load(dir: &Path) -> (Self, Vec<String>) {
        let mut themes: Vec<Theme> = [builtin::NOCTURNE, builtin::PRUNE]
            .into_iter()
            .map(|name| builtin::build(name, shcp_core::Accent::default()))
            .collect();
        let mut errors = Vec::new();
        let mut files: Vec<_> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension().is_some_and(|e| {
                    e.eq_ignore_ascii_case("json") || e.eq_ignore_ascii_case("jsonc")
                })
            })
            .collect();
        files.sort();
        for path in files {
            let stem = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let parsed = std::fs::read_to_string(&path)
                .map_err(anyhow::Error::from)
                .and_then(|text| vscode::parse(&text, &stem));
            match parsed {
                Ok(mut theme) => {
                    theme.file = Some(path.clone());
                    themes.retain(|t| t.name != theme.name);
                    themes.push(theme);
                }
                Err(error) => errors.push(format!("Theme {}: {error:#}", path.display())),
            }
        }
        (Self { themes }, errors)
    }
}

/// Where the color themes of the user are, the ones from Open VSX among them.
pub(crate) fn themes_dir(config_dir: &Path) -> PathBuf {
    config_dir.join("themes")
}

/// Where the file icon themes are, one folder each.
pub(crate) fn icon_themes_dir(config_dir: &Path) -> PathBuf {
    config_dir.join("icon-themes")
}

/// Reads the color and icon themes into the app. Returns one message per file that could not be
/// read.
pub(crate) fn load_catalogs(config_dir: &Path, cx: &mut App) -> Vec<String> {
    let (catalog, mut errors) = ThemeCatalog::load(&themes_dir(config_dir));
    cx.set_global(catalog);
    let (icons, icon_errors) = icon_theme::IconThemeCatalog::load(&icon_themes_dir(config_dir));
    cx.set_global(icons);
    errors.extend(icon_errors);
    errors
}

/// Name of the theme to show under the current settings and system appearance.
fn active_name(settings: &Settings, appearance: WindowAppearance) -> &str {
    if !settings.sync_theme {
        return &settings.theme;
    }
    match appearance {
        WindowAppearance::Light | WindowAppearance::VibrantLight => &settings.light_theme,
        WindowAppearance::Dark | WindowAppearance::VibrantDark => &settings.dark_theme,
    }
}

/// The theme the settings ask for, when it is not available: removed, or no longer bundled.
pub(crate) fn missing_theme(settings: &Settings, cx: &App) -> Option<String> {
    let name = active_name(settings, cx.window_appearance());
    let themes = &cx.global::<ThemeCatalog>().themes;
    (!themes.iter().any(|t| t.name == name)).then(|| name.to_string())
}

/// The app's own themes take the accent asked for; a theme from a file keeps the one it has.
pub(crate) fn with_accent(theme: Theme, settings: &Settings) -> Theme {
    if theme.file.is_none() {
        builtin::build(&theme.name, settings.accent)
    } else {
        theme
    }
}

/// Makes `settings` take effect: theme, terminal colors, font and row scale.
pub(crate) fn apply(settings: &Settings, engine: &EngineHandle, cx: &mut App) {
    let appearance = cx.window_appearance();
    let name = active_name(settings, appearance);
    let themes = &cx.global::<ThemeCatalog>().themes;
    // A saved theme may be gone: keep the light or dark mode the system asks for, if it asks.
    let wants_light = settings.sync_theme
        && matches!(
            appearance,
            WindowAppearance::Light | WindowAppearance::VibrantLight
        );
    let mut theme = themes
        .iter()
        .find(|t| t.name == name)
        .or_else(|| themes.iter().find(|t| t.is_light == wants_light))
        .or(themes.first())
        .cloned()
        // `ThemeCatalog::load` always holds the bundled themes, so this is never reached.
        .unwrap_or_else(|| builtin::build(builtin::NOCTURNE, shcp_core::Accent::default()));
    theme = with_accent(theme, settings);
    engine.set_terminal_palette(&theme.palette);
    cx.set_global(theme);
    // A theme that is gone leaves the app's own icons.
    let icons = cx
        .global::<icon_theme::IconThemeCatalog>()
        .themes
        .iter()
        .find(|theme| theme.name == settings.icon_theme)
        .cloned();
    cx.set_global(icon_theme::ActiveIconTheme(icons));
    let ui_scale = settings.ui_font_size / 14.;
    let density = match settings.density {
        Density::Compact => 0.86,
        Density::Default => 1.,
        Density::Comfortable => 1.18,
    };
    UI_SCALE.store(ui_scale.to_bits(), Ordering::Relaxed);
    ROW_SCALE.store((density * ui_scale.max(1.)).to_bits(), Ordering::Relaxed);
    cx.refresh_windows();
}

/// A terminal color as `0xRRGGBB`.
pub(crate) fn rgb_hex(color: Rgb) -> u32 {
    (color.0 as u32) << 16 | (color.1 as u32) << 8 | color.2 as u32
}

/// A color as the terminal takes it, without its alpha.
fn to_rgb(color: Hsla) -> Rgb {
    let gpui::Rgba { r, g, b, .. } = color.to_rgb();
    let byte = |v: f32| (v.clamp(0., 1.) * 255.).round() as u8;
    Rgb(byte(r), byte(g), byte(b))
}

// The scale of corner radii. Something inset by about 4 inside a surface takes the next step
// down. What you go somewhere with, the rows of the sidebar and the command field, is a capsule
// (`rounded_full`) instead.
/// Cards, modals, menus and whatever else floats.
pub(crate) const RADIUS_CARD: f32 = 12.;
/// Buttons, inputs, menu items and the rows of a list inside a surface.
pub(crate) const RADIUS_CONTROL: f32 = 8.;
/// Tiles, chips, key caps and tiny buttons.
pub(crate) const RADIUS_SMALL: f32 = 6.;
/// Radius for what fills a card up to its edge, so it stays inside the card's border.
pub(crate) const PANEL_INNER_RADIUS: f32 = RADIUS_CARD - 1.;

/// The soft shadow of what floats over everything else: menus, modals, tooltips.
pub(crate) fn float_shadow() -> Vec<BoxShadow> {
    let shadow = |alpha, y: f32, blur: f32| BoxShadow {
        color: hsla(0., 0., 0., alpha),
        offset: point(px(0.), px(y)),
        blur_radius: px(blur),
        spread_radius: px(0.),
    };
    vec![shadow(0.18, 2., 6.), shadow(0.32, 14., 36.)]
}

static UI_FONT: OnceLock<&'static str> = OnceLock::new();

/// The interface and terminal fonts travel inside the exe. Should they fail to load, the system's
/// take over.
pub(crate) fn load_fonts(cx: &App) {
    let fonts = vec![
        std::borrow::Cow::Borrowed(include_bytes!("../assets/fonts/InstrumentSans.ttf").as_slice()),
        std::borrow::Cow::Borrowed(include_bytes!("../assets/fonts/GeistMono.ttf").as_slice()),
    ];
    if cx.text_system().add_fonts(fonts).is_ok()
        && cx
            .text_system()
            .all_font_names()
            .iter()
            .any(|font| font == UI_FAMILY)
    {
        UI_FONT.set(UI_FAMILY).ok();
    }
}

const UI_FAMILY: &str = "Instrument Sans";
pub(crate) const MONO_FAMILY: &str = "Geist Mono";

static UI_SCALE: AtomicU32 = AtomicU32::new(1f32.to_bits());
static ROW_SCALE: AtomicU32 = AtomicU32::new(1f32.to_bits());

/// A UI font size, given for the default interface font size (14) and scaled to the setting.
pub(crate) fn ui_px(size: f32) -> Pixels {
    px((size * f32::from_bits(UI_SCALE.load(Ordering::Relaxed))).round())
}

/// A row or bar height, given for default density and scaled to the setting.
pub(crate) fn row_px(size: f32) -> Pixels {
    px((size * f32::from_bits(ROW_SCALE.load(Ordering::Relaxed))).round())
}

/// UI font family for the platform.
pub(crate) fn ui_font() -> &'static str {
    UI_FONT.get().map_or("Segoe UI", |font| font)
}

/// Monospace font for fixed-width UI text (permissions, fingerprints).
pub(crate) fn mono_font() -> &'static str {
    if UI_FONT.get().is_some() {
        MONO_FAMILY
    } else {
        "Consolas"
    }
}
