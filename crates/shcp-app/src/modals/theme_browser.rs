//! Settings page that finds color or file icon themes on Open VSX and installs or removes them.
//! It reaches the network only once opened.
use super::settings::section;
use crate::theme::icon_theme::IconThemeCatalog;
use crate::theme::open_vsx::{self, Extension, ThemeKind};
use crate::theme::{ActiveTheme as _, Theme, ThemeCatalog, ThemeOrigin, ui_px};
use crate::widgets::button::{Button, ButtonStyle};
use crate::widgets::icon::{Icon, IconName};
use crate::widgets::status::badge;
use crate::widgets::text_input::{TYPING_PAUSE, TextInput, TextInputEvent};
use gpui::prelude::*;
use gpui::{AnyElement, Context, Entity, EventEmitter, SharedString, Subscription, Task, Window};
use gpui::{div, px};
use std::fmt::Write as _;
use std::path::PathBuf;

pub(crate) enum ThemeBrowserEvent {
    Back,
    /// The themes folder changed. `removed` names the themes that are gone.
    Changed {
        removed: Vec<String>,
    },
}

enum Results {
    NotLoaded,
    Loading,
    Ready(Vec<Extension>),
    Failed(String),
}

/// One installed extension, from the themes it left in the folder.
struct InstalledExtension {
    id: SharedString,
    name: SharedString,
    themes: usize,
}

pub(crate) struct ThemeBrowser {
    kind: ThemeKind,
    themes_dir: PathBuf,
    input: Entity<TextInput>,
    results: Results,
    /// Dropping a task forgets its answer, so only the latest search or install reports back.
    search: Option<Task<()>>,
    install: Option<Task<()>>,
    installing: Option<String>,
    notice: Option<(String, bool)>,
    _subscription: Subscription,
}

impl EventEmitter<ThemeBrowserEvent> for ThemeBrowser {}

fn compact_count(value: u64) -> String {
    match value {
        0..1_000 => value.to_string(),
        1_000..1_000_000 => format!("{:.0}K", value as f64 / 1_000.),
        _ => format!("{:.1}M", value as f64 / 1_000_000.),
    }
}

fn plural(count: usize) -> String {
    if count == 1 {
        "1 theme".to_string()
    } else {
        format!("{count} themes")
    }
}

fn installed_extensions<'a>(
    origins: impl Iterator<Item = &'a ThemeOrigin>,
) -> Vec<InstalledExtension> {
    let mut extensions: Vec<InstalledExtension> = Vec::new();
    for origin in origins {
        match extensions.iter_mut().find(|e| e.id == origin.extension) {
            Some(extension) => extension.themes += 1,
            None => extensions.push(InstalledExtension {
                id: origin.extension.clone(),
                name: origin.extension_name.clone(),
                themes: 1,
            }),
        }
    }
    extensions.sort_by(|a, b| a.name.cmp(&b.name));
    extensions
}

impl ThemeBrowser {
    pub(crate) fn new(
        kind: ThemeKind,
        themes_dir: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let placeholder = match kind {
            ThemeKind::Color => "Search themes on Open VSX",
            ThemeKind::Icon => "Search file icon themes on Open VSX",
        };
        let input = cx.new(|cx| TextInput::new(placeholder, cx));
        let subscription = cx.subscribe_in(
            &input,
            window,
            |this, input, event: &TextInputEvent, _, cx| match event {
                TextInputEvent::Changed => this.start_search(true, cx),
                TextInputEvent::Submit => this.start_search(false, cx),
                TextInputEvent::Cancel if !input.read(cx).text().is_empty() => {
                    input.update(cx, |input, cx| input.set_text("", cx));
                    this.start_search(false, cx);
                }
                TextInputEvent::Cancel => cx.emit(ThemeBrowserEvent::Back),
            },
        );
        Self {
            kind,
            themes_dir,
            input,
            results: Results::NotLoaded,
            search: None,
            install: None,
            installing: None,
            notice: None,
            _subscription: subscription,
        }
    }

    /// Called when the page is shown: the first time it lists the most popular themes.
    pub(crate) fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.read(cx).focus(window);
        if matches!(self.results, Results::NotLoaded) {
            self.start_search(false, cx);
        }
    }

    fn start_search(&mut self, after_typing: bool, cx: &mut Context<Self>) {
        let query = self.input.read(cx).text().trim().to_string();
        let kind = self.kind;
        self.results = Results::Loading;
        self.search = Some(cx.spawn(async move |this, cx| {
            if after_typing {
                cx.background_executor().timer(TYPING_PAUSE).await;
            }
            let found = cx
                .background_executor()
                .spawn(async move { open_vsx::search(&query, kind) })
                .await;
            this.update(cx, |this, cx| {
                this.results = match found {
                    Ok(extensions) => Results::Ready(extensions),
                    Err(error) => Results::Failed(format!("{error:#}")),
                };
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn start_install(&mut self, extension: Extension, cx: &mut Context<Self>) {
        self.installing = Some(extension.id.clone());
        self.notice = None;
        let dir = self.themes_dir.clone();
        let kind = self.kind;
        self.install = Some(cx.spawn(async move |this, cx| {
            let name = extension.display_name.clone();
            let installed = cx
                .background_executor()
                .spawn(async move { open_vsx::install(&extension, kind, &dir) })
                .await;
            this.update(cx, |this, cx| {
                this.installing = None;
                match installed {
                    Ok(installed) => {
                        let mut notice = format!(
                            "{name}: {} installed. Pick it under Appearance",
                            plural(installed.written)
                        );
                        if installed.skipped > 0 {
                            let _ = write!(
                                notice,
                                ". Skipped {} shcp cannot use",
                                plural(installed.skipped)
                            );
                        }
                        this.notice = Some((notice, false));
                        cx.emit(ThemeBrowserEvent::Changed {
                            removed: Vec::new(),
                        });
                    }
                    Err(error) => {
                        this.notice = Some((format!("{name}: {error:#}"), true));
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn remove(&mut self, extension: &str, cx: &mut Context<Self>) {
        let themes: Vec<(String, PathBuf)> = match self.kind {
            ThemeKind::Color => cx
                .global::<ThemeCatalog>()
                .themes
                .iter()
                .filter(|t| t.origin.as_ref().is_some_and(|o| o.extension == extension))
                .filter_map(|t| Some((t.name.to_string(), t.file.clone()?)))
                .collect(),
            ThemeKind::Icon => cx
                .global::<IconThemeCatalog>()
                .themes
                .iter()
                .filter(|t| t.origin.extension == extension)
                .map(|t| (t.name.clone(), t.dir.clone()))
                .collect(),
        };
        let mut removed = Vec::new();
        self.notice = None;
        for (name, file) in themes {
            let gone = match self.kind {
                ThemeKind::Color => std::fs::remove_file(&file),
                ThemeKind::Icon => std::fs::remove_dir_all(&file),
            };
            match gone {
                Ok(()) => removed.push(name),
                Err(error) => {
                    self.notice = Some((format!("Could not remove {name}: {error}"), true));
                }
            }
        }
        cx.emit(ThemeBrowserEvent::Changed { removed });
        cx.notify();
    }

    fn row(
        theme: &Theme,
        title: SharedString,
        detail: String,
        description: Option<String>,
        action: AnyElement,
    ) -> AnyElement {
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(12.))
            .py(px(7.))
            .border_b_1()
            .border_color(theme.hover)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(div().truncate().text_color(theme.text).child(title))
                    .child(
                        div()
                            .truncate()
                            .text_size(ui_px(12.))
                            .text_color(theme.text_muted)
                            .child(detail),
                    )
                    .children(description.filter(|d| !d.is_empty()).map(|d| {
                        div()
                            .truncate()
                            .text_size(ui_px(12.))
                            .text_color(theme.text_disabled)
                            .child(d)
                    })),
            )
            .child(div().flex_none().child(action))
            .into_any_element()
    }

    fn result_row(
        &self,
        index: usize,
        extension: &Extension,
        theme: &Theme,
        installed_themes: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let detail = format!(
            "{} · {} · {} downloads · {}",
            extension.publisher,
            extension.license,
            compact_count(extension.downloads),
            plural(extension.theme_count)
        );
        let action = if self.installing.as_deref() == Some(extension.id.as_str()) {
            Icon::new(IconName::Spinner)
                .size(px(14.))
                .color(theme.accent)
                .spin()
                .into_any_element()
        } else if installed_themes >= extension.theme_count {
            badge("Installed", theme.success).into_any_element()
        } else {
            let extension = extension.clone();
            Button::new(("install-theme", index))
                .icon(IconName::Download)
                // Some of its themes were removed one by one: this brings them back.
                .label(if installed_themes > 0 {
                    "Reinstall"
                } else {
                    "Install"
                })
                .disabled(self.installing.is_some())
                .on_click(
                    cx.listener(move |this, _, _, cx| this.start_install(extension.clone(), cx)),
                )
                .into_any_element()
        };
        Self::row(
            theme,
            extension.display_name.clone().into(),
            detail,
            Some(extension.description.clone()),
            action,
        )
    }
}

impl Render for ThemeBrowser {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let installed = match self.kind {
            ThemeKind::Color => installed_extensions(
                cx.global::<ThemeCatalog>()
                    .themes
                    .iter()
                    .filter_map(|t| t.origin.as_ref()),
            ),
            ThemeKind::Icon => installed_extensions(
                cx.global::<IconThemeCatalog>()
                    .themes
                    .iter()
                    .map(|t| &t.origin),
            ),
        };
        let (nothing_found, about) = match self.kind {
            ThemeKind::Color => (
                "No color themes with an open license match your search",
                "Themes are published by their authors on open-vsx.org. \
                 shcp lists those with an open license and uses only their colors.",
            ),
            ThemeKind::Icon => (
                "No file icon themes with an open license match your search",
                "Themes are published by their authors on open-vsx.org. shcp lists those with an \
                 open license and uses only their SVG pictures: a theme made of a font cannot be \
                 installed.",
            ),
        };
        let installed_rows: Vec<AnyElement> = installed
            .iter()
            .enumerate()
            .map(|(index, extension)| {
                let id = extension.id.clone();
                Self::row(
                    &theme,
                    extension.name.clone(),
                    format!("{} · {}", extension.id, plural(extension.themes)),
                    None,
                    Button::new(("remove-theme", index))
                        .icon(IconName::Trash)
                        .label("Remove")
                        .style(ButtonStyle::Ghost)
                        .on_click(cx.listener(move |this, _, _, cx| this.remove(&id, cx)))
                        .into_any_element(),
                )
            })
            .collect();
        let message = |text: String, color| {
            div()
                .py(px(10.))
                .text_size(ui_px(12.))
                .text_color(color)
                .child(text)
                .into_any_element()
        };
        let results: Vec<AnyElement> = match &self.results {
            Results::NotLoaded => Vec::new(),
            Results::Loading => vec![
                div()
                    .py(px(10.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .text_size(ui_px(12.))
                    .text_color(theme.text_muted)
                    .child(
                        Icon::new(IconName::Spinner)
                            .size(px(14.))
                            .color(theme.accent)
                            .spin(),
                    )
                    .child("Searching Open VSX…")
                    .into_any_element(),
            ],
            Results::Failed(error) => vec![message(error.clone(), theme.error)],
            Results::Ready(extensions) if extensions.is_empty() => {
                vec![message(nothing_found.to_string(), theme.text_muted)]
            }
            Results::Ready(extensions) => extensions
                .clone()
                .iter()
                .enumerate()
                .map(|(index, extension)| {
                    let installed_themes = installed
                        .iter()
                        .find(|i| i.id.eq_ignore_ascii_case(&extension.id))
                        .map_or(0, |i| i.themes);
                    self.result_row(index, extension, &theme, installed_themes, cx)
                })
                .collect(),
        };
        div()
            .flex()
            .flex_col()
            .child(
                div()
                    .pt(px(4.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        Button::new("theme-browser-back")
                            .icon(IconName::ArrowLeft)
                            .tooltip("Back to Appearance")
                            .style(ButtonStyle::Ghost)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(ThemeBrowserEvent::Back))),
                    )
                    .child(div().flex_1().min_w_0().child(self.input.clone())),
            )
            .children(self.notice.as_ref().map(|(text, failed)| {
                div()
                    .pt(px(10.))
                    .text_size(ui_px(12.))
                    .text_color(if *failed { theme.error } else { theme.success })
                    .child(text.clone())
            }))
            .when(!installed_rows.is_empty(), |this| {
                this.child(section(&theme, "INSTALLED"))
                    .children(installed_rows)
            })
            .child(section(&theme, "OPEN VSX"))
            .children(results)
            .child(
                div()
                    .pt(px(12.))
                    .text_size(ui_px(12.))
                    .text_color(theme.text_disabled)
                    .child(about),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_are_compact() {
        assert_eq!(compact_count(999), "999");
        assert_eq!(compact_count(1_500), "2K");
        assert_eq!(compact_count(1_055_521), "1.1M");
    }
}
