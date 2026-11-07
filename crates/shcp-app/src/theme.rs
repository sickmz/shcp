//! The active theme and the catalog of available ones. Components read colors only from here.
//! Themes are VS Code color theme files: the app's own plus any in `<config_dir>/themes`, where
//! the ones installed from Open VSX end up too.
pub mod builtin;
pub mod icon_theme;
pub mod open_vsx;
mod vscode;

use crate::widgets::host_color::HostColor;
use gpui::{
    App, BoxShadow, Global, Hsla, Pixels, SharedString, WindowAppearance,
    WindowBackgroundAppearance, hsla, point, px,
};
use shcp_core::{Density, EngineHandle, Settings, term::Palette};
use std::{
    path::{Path, PathBuf},
    sync::{
        OnceLock,
        atomic::{AtomicU32, Ordering},
    },
};

/// The Open VSX extension a theme was installed from.
#[derive(Clone)]
pub struct ThemeOrigin {
    pub extension: SharedString,
    pub extension_name: SharedString,
}

#[derive(Clone)]
pub struct Theme {
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
    /// What the window shows between its panels: `surface`, see-through if asked for.
    pub window_bg: Hsla,
    /// How much of the window's backdrop a panel lets through: 1 for none.
    pub panel_alpha: f32,
    /// The groove the tabs sit in.
    pub track: Hsla,
    /// Fill of the current item in chrome.
    pub raised: Hsla,
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
    pub fn shows_host_colors(&self) -> bool {
        self.lit_c > 0.
    }

    /// The color of a host, at the lightness this theme draws accents at.
    pub fn host_color(&self, color: HostColor) -> Hsla {
        let chroma = if color == HostColor::Gray {
            0.012
        } else {
            self.lit_c
        };
        builtin::oklch(self.lit_l, chroma, color.hue(), 1.)
    }
}

impl Global for Theme {}

pub trait ActiveTheme {
    fn theme(&self) -> &Theme;
}

impl ActiveTheme for App {
    fn theme(&self) -> &Theme {
        self.global::<Theme>()
    }
}

/// All themes the user can pick.
pub struct ThemeCatalog {
    pub themes: Vec<Theme>,
}

impl Global for ThemeCatalog {}

impl ThemeCatalog {
    /// Reads the bundled themes and `dir`. A user theme replaces a bundled one of the same name.
    /// Returns the catalog and one message per file that could not be read.
    pub fn load(dir: &Path) -> (Self, Vec<String>) {
        let mut themes: Vec<Theme> = [builtin::NOCTURNE, builtin::PORCELAIN]
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
pub fn missing_theme(settings: &Settings, cx: &App) -> Option<String> {
    let name = active_name(settings, cx.window_appearance());
    let themes = &cx.global::<ThemeCatalog>().themes;
    (!themes.iter().any(|t| t.name == name)).then(|| name.to_string())
}

/// The app's own themes take the accent asked for; a theme from a file keeps the one it has.
pub fn with_accent(theme: Theme, settings: &Settings) -> Theme {
    if theme.file.is_none() {
        builtin::build(&theme.name, settings.accent)
    } else {
        theme
    }
}

/// Makes `settings` take effect: theme, terminal colors, font and row scale.
pub fn apply(settings: &Settings, engine: &EngineHandle, cx: &mut App) {
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
        .unwrap_or_else(vscode::fallback);
    theme = with_accent(theme, settings);
    if settings.translucent_window {
        theme.window_bg.a = if theme.is_light { 0.9 } else { 0.88 };
        theme.panel_alpha = if theme.is_light { 0.96 } else { 0.95 };
    }
    engine.set_terminal_palette(theme.palette.clone());
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

/// Corner radius of the floating panels.
pub const PANEL_RADIUS: f32 = 12.;
/// Radius for what fills a panel up to its edge, so it stays inside the panel's border.
pub const PANEL_INNER_RADIUS: f32 = 11.;

/// What the window asks of the system behind it.
pub fn window_background(settings: &Settings) -> WindowBackgroundAppearance {
    if settings.translucent_window {
        WindowBackgroundAppearance::Blurred
    } else {
        WindowBackgroundAppearance::Opaque
    }
}

/// The soft shadow of what floats over everything else: menus, modals, tooltips.
pub fn float_shadow() -> Vec<BoxShadow> {
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
pub fn load_fonts(cx: &App) {
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
pub const MONO_FAMILY: &str = "Geist Mono";

static UI_SCALE: AtomicU32 = AtomicU32::new(1f32.to_bits());
static ROW_SCALE: AtomicU32 = AtomicU32::new(1f32.to_bits());

/// A UI font size, given for the default interface font size (14) and scaled to the setting.
pub fn ui_px(size: f32) -> Pixels {
    px((size * f32::from_bits(UI_SCALE.load(Ordering::Relaxed))).round())
}

/// A row or bar height, given for default density and scaled to the setting.
pub fn row_px(size: f32) -> Pixels {
    px((size * f32::from_bits(ROW_SCALE.load(Ordering::Relaxed))).round())
}

/// UI font family for the platform.
pub fn ui_font() -> &'static str {
    if let Some(font) = UI_FONT.get() {
        return font;
    }
    if cfg!(windows) {
        "Segoe UI"
    } else {
        ".SystemUIFont"
    }
}

/// Monospace font for fixed-width UI text (permissions, fingerprints).
pub fn mono_font() -> &'static str {
    if UI_FONT.get().is_some() {
        MONO_FAMILY
    } else if cfg!(windows) {
        "Consolas"
    } else {
        "monospace"
    }
}
