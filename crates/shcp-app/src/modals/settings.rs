//! Settings dialog: categories on the left, every change applied and saved immediately.
#[path = "settings_hosts.rs"]
mod hosts;
use super::theme_browser::{ThemeBrowser, ThemeBrowserEvent};
use super::{
    ModalLayer, ModalView,
    confirm::ConfirmModal,
    host_commands::HostCommandsModal,
    host_editor::HostEditor,
    host_settings::{HostPage, HostSettingsModal},
    key_setup::KeySetupModal,
};
use crate::actions::{self, SHORTCUTS, Shortcut};
use crate::state::AppState;
use crate::theme::icon_theme::{IconTheme, IconThemeCatalog};
use crate::theme::open_vsx::ThemeKind;
use crate::theme::{ActiveTheme as _, Theme, ThemeCatalog, ui_px};
use crate::updater::{UpdateStatus, current_version, open_releases_page};
use crate::widgets::button::{Button, ButtonStyle};
use crate::widgets::icon::{Icon, IconName};
use crate::widgets::text_input::{TextInput, TextInputEvent};
use crate::widgets::tooltip::tooltip;
use gpui::prelude::*;
use gpui::{
    AnyElement, AnyView, App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable,
    KeyDownEvent, PathPromptOptions, ScrollHandle, SharedString, Subscription, Task, Window,
    anchored, deferred, div, img, px, relative,
};
use shcp_core::{Accent, CursorStyle, Density, HostId, KnownHost, NoticeLevel, Settings};
use std::{path::PathBuf, rc::Rc, time::Duration};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Appearance,
    /// Reached from Appearance, which stays marked in the menu.
    ThemeBrowser,
    IconThemeBrowser,
    /// Reached from Connections, likewise.
    KnownHosts,
    Hosts,
    Terminal,
    Connections,
    Forwarding,
    Files,
    Keyboard,
    About,
}

/// The lists that open from a control on a page.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Dropdown {
    Font,
    Theme,
    IconTheme,
}

const PAGES: [(Page, &str, IconName); 7] = [
    (Page::Appearance, "Appearance", IconName::Palette),
    (Page::Hosts, "Hosts", IconName::Settings),
    (Page::Terminal, "Terminal", IconName::Terminal),
    (Page::Connections, "Connections", IconName::Plug),
    (Page::Forwarding, "Port forwarding", IconName::Forward),
    (Page::Files, "Files & transfers", IconName::Folder),
    (Page::Keyboard, "Keyboard", IconName::Keyboard),
];

/// Keys that belong to the mouse or to one view, which cannot be changed.
const FIXED_SHORTCUTS: [(&str, &str); 8] = [
    ("Open a link in the terminal", "Ctrl+click"),
    (
        "Menu of a terminal, or swap with the selected one",
        "Ctrl+right click",
    ),
    ("Copy text selected in the terminal", "Ctrl+C"),
    ("Files: cut, copy and paste", "Ctrl+X / Ctrl+C / Ctrl+V"),
    ("Files: filter", "Ctrl+F"),
    ("Files: search the subfolders", "Ctrl+F, a name, Enter"),
    ("Files: show hidden files", "Ctrl+H"),
    ("Files: go through the list", "Arrows, or type a name"),
];

/// Applies one value of a setting (from a click) to the settings and saves them.
pub(super) type Setter<T> = Rc<dyn Fn(T, &mut Window, &mut App)>;
#[derive(Clone, Copy)]
enum HostSection {
    Overview,
    Editor(HostPage),
}

struct HostDetail {
    host: HostId,
    section: HostSection,
    view: AnyView,
    focus: FocusHandle,
    key_setup: Option<Entity<KeySetupModal>>,
    dirty: Box<dyn Fn(&App) -> bool>,
    _subscriptions: Vec<Subscription>,
}

#[derive(Clone, Copy)]
enum Destination {
    Back,
    Category(Page),
}

pub struct SettingsModal {
    state: Entity<AppState>,
    modals: Entity<ModalLayer>,
    host_detail: Option<HostDetail>,
    list_scroll: ScrollHandle,
    detail_scroll: ScrollHandle,
    page: Page,
    search: Entity<TextInput>,
    editor: Entity<TextInput>,
    skip_folders: Entity<TextInput>,
    theme_browser: Entity<ThemeBrowser>,
    icon_theme_browser: Entity<ThemeBrowser>,
    /// Waits for typing to pause before applying the editor.
    editor_commit: Option<Task<()>>,
    /// Waits for typing to pause before applying the folders a search skips.
    skip_folders_commit: Option<Task<()>>,
    /// Every installed font family, to tell whether the configured one is there.
    installed_fonts: Vec<String>,
    /// What the font list offers; found the first time it opens, since that loads every font.
    monospace_fonts: Option<Rc<Vec<SharedString>>>,
    open_dropdown: Option<Dropdown>,
    dropdown_trigger_hovered: bool,
    /// The shortcut waiting for its new keys.
    capturing: Option<&'static str>,
    /// What the Trusted servers page lists, read when it opens.
    known_hosts: Vec<KnownHost>,
    /// The server whose Remove button was pressed once and now asks to be sure.
    forgetting: Option<KnownHost>,
    error: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<DismissEvent> for SettingsModal {}
impl EventEmitter<super::CancelEvent> for SettingsModal {}

impl Focusable for SettingsModal {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.host_detail.as_ref().map_or_else(
            || self.search.focus_handle(cx),
            |detail| {
                detail
                    .key_setup
                    .as_ref()
                    .map_or_else(|| detail.focus.clone(), |view| view.focus_handle(cx))
            },
        )
    }
}

impl SettingsModal {
    pub fn new(
        state: Entity<AppState>,
        modals: Entity<ModalLayer>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| TextInput::new("Search settings", false, cx));
        let installed_fonts = cx.text_system().all_font_names();
        let editor_path = state.read(cx).settings.editor.clone();
        let editor = cx.new(|cx| {
            let mut input = TextInput::new("VS Code or Notepad", false, cx);
            input.set_text(&editor_path, cx);
            input
        });
        let skip_names = state.read(cx).settings.search_skip_folders.join(", ");
        let skip_folders = cx.new(|cx| {
            let mut input = TextInput::new("node_modules, .git", false, cx);
            input.set_text(&skip_names, cx);
            input
        });
        let themes_dir = state.read(cx).engine.paths().config_dir.join("themes");
        let theme_browser =
            cx.new(|cx| ThemeBrowser::new(ThemeKind::Color, themes_dir, window, cx));
        let icon_themes_dir = state.read(cx).engine.paths().config_dir.join("icon-themes");
        let icon_theme_browser =
            cx.new(|cx| ThemeBrowser::new(ThemeKind::Icon, icon_themes_dir, window, cx));
        let mut subscriptions =
            vec![
                cx.subscribe_in(
                    &theme_browser,
                    window,
                    |this, _, event: &ThemeBrowserEvent, window, cx| {
                        this.on_browser_event(event, window, cx)
                    },
                ),
                cx.subscribe_in(
                    &icon_theme_browser,
                    window,
                    |this, _, event: &ThemeBrowserEvent, window, cx| {
                        this.on_browser_event(event, window, cx)
                    },
                ),
                cx.subscribe_in(
                    &search,
                    window,
                    |this, input, event: &TextInputEvent, _, cx| match event {
                        TextInputEvent::Cancel if this.open_dropdown.is_some() => {
                            this.open_dropdown = None;
                            cx.notify();
                        }
                        TextInputEvent::Cancel if !input.read(cx).text().is_empty() => {
                            input.update(cx, |input, cx| input.set_text("", cx));
                            cx.notify();
                        }
                        TextInputEvent::Cancel => this.close(cx),
                        TextInputEvent::Changed | TextInputEvent::Submit => cx.notify(),
                    },
                ),
                cx.subscribe_in(&editor, window, |this, _, event: &TextInputEvent, _, cx| {
                    match event {
                        TextInputEvent::Changed => this.schedule_editor_commit(cx),
                        TextInputEvent::Submit => this.commit_editor(cx),
                        TextInputEvent::Cancel => this.close(cx),
                    }
                }),
                cx.subscribe_in(
                    &skip_folders,
                    window,
                    |this, _, event: &TextInputEvent, _, cx| match event {
                        TextInputEvent::Changed => this.schedule_skip_folders_commit(cx),
                        TextInputEvent::Submit => this.commit_skip_folders(cx),
                        TextInputEvent::Cancel => this.close(cx),
                    },
                ),
            ];
        subscriptions.push(cx.observe(&state, |_, _, cx| cx.notify()));
        Self {
            state,
            modals,
            host_detail: None,
            list_scroll: ScrollHandle::new(),
            detail_scroll: ScrollHandle::new(),
            page: Page::Appearance,
            search,
            editor,
            skip_folders,
            theme_browser,
            icon_theme_browser,
            editor_commit: None,
            skip_folders_commit: None,
            installed_fonts,
            monospace_fonts: None,
            open_dropdown: None,
            dropdown_trigger_hovered: false,
            capturing: None,
            known_hosts: Vec::new(),
            forgetting: None,
            error: None,
            _subscriptions: subscriptions,
        }
    }

    /// The keys pressed while a shortcut waits for new ones. Nothing else gets to see them.
    fn on_capture_key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.capturing else {
            return;
        };
        cx.stop_propagation();
        let stroke = &event.keystroke;
        let bare = !stroke.modifiers.control && !stroke.modifiers.alt && !stroke.modifiers.shift;
        match stroke.key.as_str() {
            "escape" if bare => self.capturing = None,
            "backspace" | "delete" if bare => {
                self.capturing = None;
                self.change(
                    |s| {
                        s.keybindings.insert(id.to_string(), String::new());
                    },
                    cx,
                );
            }
            _ => {
                let keys = stroke.unparse();
                if !actions::acceptable(&keys) {
                    self.error = Some(format!(
                        "{} would get in the way of typing: use Ctrl or Alt with it",
                        actions::pretty(&keys)
                    ));
                    cx.notify();
                    return;
                }
                self.capturing = None;
                let taken = actions::taken_by(&keys, id, &self.state.read(cx).settings);
                self.change(
                    |s| {
                        s.keybindings.insert(id.to_string(), keys.clone());
                    },
                    cx,
                );
                if let Some(other) = taken {
                    self.error = Some(format!(
                        "{} is also the shortcut for \"{other}\": only one of them will work",
                        actions::pretty(&keys)
                    ));
                }
            }
        }
        cx.notify();
    }

    /// The keys of a shortcut: click and press new ones.
    fn shortcut_control(
        &self,
        shortcut: &'static Shortcut,
        settings: &Settings,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let capturing = self.capturing == Some(shortcut.id);
        let keys = shortcut.label_of_keys(settings);
        let label = if capturing {
            "Press keys…".to_string()
        } else if keys.is_empty() {
            "None".to_string()
        } else {
            keys
        };
        let custom = settings.keybindings.contains_key(shortcut.id);
        let id = shortcut.id;
        div()
            .flex()
            .items_center()
            .gap(px(4.))
            .when(custom, |this| {
                this.child(
                    Button::new(SharedString::from(format!("reset-{id}")))
                        .icon(IconName::Restore)
                        .tooltip("Back to the original keys")
                        .style(ButtonStyle::Ghost)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.change(
                                |s| {
                                    s.keybindings.remove(id);
                                },
                                cx,
                            );
                        })),
                )
            })
            .child(
                Button::new(SharedString::from(format!("keys-{id}")))
                    .label(label)
                    .style(if capturing {
                        ButtonStyle::Primary
                    } else {
                        ButtonStyle::Filled
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.capturing = if this.capturing == Some(id) {
                            None
                        } else {
                            Some(id)
                        };
                        this.error = None;
                        cx.notify();
                    })),
            )
            .into_any_element()
    }

    fn choose_download_folder(&mut self, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Download here".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(folders))) = picked.await
                && let Some(folder) = folders.into_iter().next()
            {
                this.update(cx, |this, cx| {
                    let folder = folder.display().to_string();
                    this.change(|s| s.download_folder = folder, cx);
                })
                .ok();
            }
        })
        .detach();
    }

    fn export_backup(&mut self, cx: &mut Context<Self>) {
        let folder = std::env::var_os("USERPROFILE").map_or_else(
            || self.state.read(cx).engine.paths().config_dir,
            PathBuf::from,
        );
        let picked = cx.prompt_for_new_path(&folder, Some("shcp-backup.toml"));
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(path))) = picked.await {
                this.update(cx, |this, cx| {
                    let (level, message) = match this.state.read(cx).engine.export_backup(&path) {
                        Ok(()) => (
                            NoticeLevel::Info,
                            format!("Settings and hosts saved in {}", path.display()),
                        ),
                        Err(error) => (
                            NoticeLevel::Error,
                            format!("Could not save the backup: {error:#}"),
                        ),
                    };
                    this.state
                        .update(cx, |state, cx| state.push_notice(level, message, cx));
                })
                .ok();
            }
        })
        .detach();
    }

    fn import_backup(&mut self, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Import".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = picked.await
                && let Some(path) = paths.into_iter().next()
            {
                this.update(cx, |this, cx| {
                    let imported = this
                        .state
                        .update(cx, |state, cx| state.import_backup(&path, cx));
                    let (level, message) = match imported {
                        Ok(count) => (
                            NoticeLevel::Info,
                            format!("Settings replaced and {count} hosts imported"),
                        ),
                        Err(error) => (
                            NoticeLevel::Error,
                            format!("Nothing was imported: {error:#}"),
                        ),
                    };
                    this.state
                        .update(cx, |state, cx| state.push_notice(level, message, cx));
                    let editor = this.state.read(cx).settings.editor.clone();
                    this.editor
                        .update(cx, |input, cx| input.set_text(&editor, cx));
                    let skip_names = this.state.read(cx).settings.search_skip_folders.join(", ");
                    this.skip_folders
                        .update(cx, |input, cx| input.set_text(&skip_names, cx));
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    fn change(&mut self, change: impl FnOnce(&mut Settings), cx: &mut Context<Self>) {
        let mut settings = self.state.read(cx).settings.clone();
        change(&mut settings);
        let saved = self
            .state
            .update(cx, |state, cx| state.update_settings(settings, cx));
        self.error = saved
            .err()
            .map(|error| format!("Could not save settings: {error:#}"));
        cx.notify();
    }

    fn setter<T: 'static>(
        &self,
        apply: impl Fn(&mut Settings, T) + 'static,
        cx: &mut Context<Self>,
    ) -> Setter<T> {
        let this = cx.entity().downgrade();
        let apply = Rc::new(apply);
        Rc::new(move |value, _, cx| {
            let apply = apply.clone();
            this.update(cx, |this, cx| this.change(|s| apply(s, value), cx))
                .ok();
        })
    }

    fn schedule_editor_commit(&mut self, cx: &mut Context<Self>) {
        self.editor_commit = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(500))
                .await;
            this.update(cx, |this, cx| this.commit_editor(cx)).ok();
        }));
    }

    fn commit_editor(&mut self, cx: &mut Context<Self>) {
        self.editor_commit = None;
        let editor = self.editor.read(cx).text().trim().to_string();
        if editor != self.state.read(cx).settings.editor {
            self.change(|s| s.editor = editor, cx);
        }
    }

    fn schedule_skip_folders_commit(&mut self, cx: &mut Context<Self>) {
        self.skip_folders_commit = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(500))
                .await;
            this.update(cx, |this, cx| this.commit_skip_folders(cx))
                .ok();
        }));
    }

    fn commit_skip_folders(&mut self, cx: &mut Context<Self>) {
        self.skip_folders_commit = None;
        let names: Vec<String> = self
            .skip_folders
            .read(cx)
            .text()
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(String::from)
            .collect();
        if names != self.state.read(cx).settings.search_skip_folders {
            self.change(|s| s.search_skip_folders = names, cx);
        }
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        self.commit_editor(cx);
        self.commit_skip_folders(cx);
        cx.emit(DismissEvent);
    }

    fn toggle_dropdown(&mut self, which: Dropdown, cx: &mut Context<Self>) {
        self.open_dropdown = if self.open_dropdown == Some(which) {
            None
        } else {
            Some(which)
        };
        if self.open_dropdown == Some(Dropdown::Font) && self.monospace_fonts.is_none() {
            self.monospace_fonts = Some(Rc::new(monospace_fonts(&self.installed_fonts, cx)));
        }
        cx.notify();
    }

    /// A control showing `current`, which opens a list of `items` below it.
    fn dropdown(
        &self,
        which: Dropdown,
        id: &'static str,
        current: AnyElement,
        items: impl FnOnce(&mut Context<Self>) -> Vec<AnyElement>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let list = (self.open_dropdown == Some(which)).then(|| {
            let items = items(cx);
            deferred(
                anchored().snap_to_window_with_margin(px(8.)).child(
                    div()
                        .id(SharedString::from(format!("{id}-list")))
                        .occlude()
                        .w(px(260.))
                        .max_h(px(280.))
                        .overflow_y_scroll()
                        .mt(px(4.))
                        .p(px(4.))
                        .flex()
                        .flex_col()
                        .rounded(px(9.))
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.elevated)
                        .shadow(crate::theme::float_shadow())
                        .text_size(ui_px(13.))
                        .text_color(theme.text)
                        // The trigger closes it by itself: closing here too would reopen it.
                        .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                            if !this.dropdown_trigger_hovered {
                                this.open_dropdown = None;
                                cx.notify();
                            }
                        }))
                        .children(items),
                ),
            )
            .with_priority(1)
        });
        div()
            .w(px(220.))
            .flex()
            .flex_col()
            .child(
                div()
                    .id(id)
                    .h(px(28.))
                    .pl(px(8.))
                    .pr(px(6.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(6.))
                    .rounded(px(6.))
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.input_bg)
                    .text_color(theme.text)
                    .cursor_pointer()
                    .on_hover(cx.listener(|this, hovered: &bool, _, _| {
                        this.dropdown_trigger_hovered = *hovered;
                    }))
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_dropdown(which, cx)))
                    .child(current)
                    .child(Icon::new(IconName::ChevronDown).color(theme.text_muted)),
            )
            .children(list)
            .into_any_element()
    }

    /// The configured font, opening a list of the installed monospace ones.
    fn font_picker(&self, family: &str, cx: &mut Context<Self>) -> AnyElement {
        let current = div()
            .min_w_0()
            .truncate()
            .font_family(SharedString::from(family.to_string()))
            .child(family.to_string())
            .into_any_element();
        let fonts = self.monospace_fonts.clone();
        let family = family.to_string();
        self.dropdown(
            Dropdown::Font,
            "terminal-font",
            current,
            move |cx| {
                let theme = cx.theme().clone();
                let fonts = fonts.unwrap_or_default();
                let mut items: Vec<AnyElement> = fonts
                    .iter()
                    .enumerate()
                    .map(|(index, name)| {
                        let chosen = name.to_string();
                        dropdown_item(
                            &theme,
                            ("terminal-font-item", index),
                            name.eq_ignore_ascii_case(&family),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.open_dropdown = None;
                            let chosen = chosen.clone();
                            this.change(|s| s.terminal_font_family = chosen, cx);
                        }))
                        .child(
                            // Written in itself, to show what it looks like.
                            div()
                                .min_w_0()
                                .truncate()
                                .font_family(name.clone())
                                .child(name.clone()),
                        )
                        .into_any_element()
                    })
                    .collect();
                if items.is_empty() {
                    items.push(
                        div()
                            .p(px(8.))
                            .text_color(theme.text_muted)
                            .child("No monospace fonts found")
                            .into_any_element(),
                    );
                }
                items
            },
            cx,
        )
    }

    fn open_path(&self, path: PathBuf, cx: &mut Context<Self>) {
        if path.exists() {
            cx.open_with_system(&path);
        } else if let Some(parent) = path.parent() {
            cx.open_with_system(parent);
        }
    }

    fn open_known_hosts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.page = Page::KnownHosts;
        self.forgetting = None;
        self.search.update(cx, |input, cx| input.set_text("", cx));
        self.search.read(cx).focus(window);
        self.reload_known_hosts(cx);
    }

    fn reload_known_hosts(&mut self, cx: &mut Context<Self>) {
        match self.state.read(cx).engine.known_hosts() {
            Ok(hosts) => self.known_hosts = hosts,
            Err(error) => self.error = Some(format!("{error:#}")),
        }
        cx.notify();
    }

    /// The first press on Remove asks to press again.
    fn forget_known_host(&mut self, host: KnownHost, cx: &mut Context<Self>) {
        if self.forgetting.as_ref() != Some(&host) {
            self.forgetting = Some(host);
            cx.notify();
            return;
        }
        self.forgetting = None;
        if let Err(error) = self.state.read(cx).engine.forget_known_host(&host) {
            self.error = Some(format!("{error:#}"));
        }
        self.reload_known_hosts(cx);
    }

    fn known_hosts_rows(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme().clone();
        let mut rows: Vec<AnyElement> = vec![
            div()
                .pb(px(8.))
                .text_size(ui_px(12.))
                .text_color(theme.text_muted)
                .child(format!(
                    "From {}, which OpenSSH reads too. A server you remove asks to confirm its key at the next connection",
                    self.state.read(cx).engine.paths().known_hosts.display()
                ))
                .into_any_element(),
        ];
        if self.known_hosts.is_empty() {
            rows.push(
                div()
                    .text_color(theme.text_muted)
                    .child("No trusted servers yet")
                    .into_any_element(),
            );
        }
        for (index, host) in self.known_hosts.iter().enumerate() {
            let name = host
                .names
                .clone()
                .unwrap_or_else(|| "Hashed name".to_string());
            let name = match &host.marker {
                Some(marker) => format!("{name} ({marker})"),
                None => name,
            };
            let confirming = self.forgetting.as_ref() == Some(host);
            let target = host.clone();
            rows.push(
                div()
                    .flex()
                    .items_center()
                    .gap(px(16.))
                    .min_h(px(48.))
                    .py(px(6.))
                    .border_b_1()
                    .border_color(theme.hover)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(div().truncate().child(name))
                            .child(
                                div()
                                    .truncate()
                                    .font_family(crate::theme::mono_font())
                                    .text_size(ui_px(11.))
                                    .text_color(theme.text_muted)
                                    .child(format!("{} {}", host.key_type, host.fingerprint)),
                            ),
                    )
                    .child(
                        Button::new(("forget-known-host", index))
                            .label(if confirming {
                                "Really remove"
                            } else {
                                "Remove"
                            })
                            .style(if confirming {
                                ButtonStyle::Danger
                            } else {
                                ButtonStyle::Filled
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.forget_known_host(target.clone(), cx)
                            })),
                    )
                    .into_any_element(),
            );
        }
        rows
    }

    fn themes_dir(&self, cx: &App) -> PathBuf {
        self.state.read(cx).engine.paths().config_dir.join("themes")
    }

    fn on_browser_event(
        &mut self,
        event: &ThemeBrowserEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            ThemeBrowserEvent::Back => {
                self.page = Page::Appearance;
                self.search.read(cx).focus(window);
                cx.notify();
            }
            ThemeBrowserEvent::Changed { removed } => {
                self.reload_themes(cx);
                self.forget_themes(removed, cx);
            }
        }
    }

    fn reload_themes(&mut self, cx: &mut Context<Self>) {
        let (catalog, mut errors) = ThemeCatalog::load(&self.themes_dir(cx));
        cx.set_global(catalog);
        let icon_themes_dir = self
            .state
            .read(cx)
            .engine
            .paths()
            .config_dir
            .join("icon-themes");
        let (icons, icon_errors) = IconThemeCatalog::load(&icon_themes_dir);
        cx.set_global(icons);
        errors.extend(icon_errors);
        let (settings, engine) = {
            let state = self.state.read(cx);
            (state.settings.clone(), state.engine.clone())
        };
        crate::theme::apply(&settings, &engine, cx);
        self.error = (!errors.is_empty()).then(|| errors.join("\n"));
        cx.notify();
    }

    /// Stops the settings from naming themes that were just removed.
    fn forget_themes(&mut self, removed: &[String], cx: &mut Context<Self>) {
        let is_removed = |name: &str| removed.iter().any(|r| r == name);
        let settings = &self.state.read(cx).settings;
        if !is_removed(&settings.theme)
            && !is_removed(&settings.dark_theme)
            && !is_removed(&settings.light_theme)
            && !is_removed(&settings.icon_theme)
        {
            return;
        }
        let defaults = Settings::default();
        self.change(
            |s| {
                if is_removed(&s.icon_theme) {
                    s.icon_theme = defaults.icon_theme;
                }
                if is_removed(&s.theme) {
                    s.theme = defaults.theme;
                }
                if is_removed(&s.dark_theme) {
                    s.dark_theme = defaults.dark_theme;
                }
                if is_removed(&s.light_theme) {
                    s.light_theme = defaults.light_theme;
                }
            },
            cx,
        );
    }

    fn open_theme_browser(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.page = Page::ThemeBrowser;
        self.search.update(cx, |input, cx| input.set_text("", cx));
        self.theme_browser
            .update(cx, |browser, cx| browser.open(window, cx));
        cx.notify();
    }

    fn open_icon_theme_browser(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.page = Page::IconThemeBrowser;
        self.search.update(cx, |input, cx| input.set_text("", cx));
        self.icon_theme_browser
            .update(cx, |browser, cx| browser.open(window, cx));
        cx.notify();
    }
}

/// The families whose characters all have one width: others would break the terminal's columns.
/// Fonts drawing only symbols have no letters to measure and stay out too.
fn monospace_fonts(installed: &[String], cx: &App) -> Vec<SharedString> {
    let text_system = cx.text_system();
    installed
        .iter()
        // Names of the UI font, and of the sideways variants of East Asian fonts.
        .filter(|name| !name.starts_with(['.', '@']))
        .filter(|name| {
            let font_id = text_system.resolve_font(&gpui::font(name.to_string()));
            let width = |c| {
                text_system
                    .advance(font_id, px(100.), c)
                    .ok()
                    .map(|size| f32::from(size.width))
            };
            let Some(first) = width('i') else {
                return false;
            };
            ['W', 'm', '.']
                .into_iter()
                .all(|c| width(c).is_some_and(|other| (other - first).abs() < 0.01))
        })
        .map(|name| SharedString::from(name.clone()))
        .collect()
}

pub(super) fn section(theme: &Theme, label: &'static str) -> AnyElement {
    div()
        .pt(px(16.))
        .pb(px(6.))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_size(ui_px(11.))
        .text_color(theme.text_disabled)
        .child(label)
        .into_any_element()
}

pub(super) fn row(
    theme: &Theme,
    label: impl Into<SharedString>,
    help: Option<SharedString>,
    control: impl IntoElement,
) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap(px(16.))
        .min_h(px(48.))
        .py(px(8.))
        .border_b_1()
        .border_color(theme.hover)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(div().text_color(theme.text).child(label.into()))
                .children(help.map(|h| {
                    div()
                        .text_size(ui_px(12.))
                        .text_color(theme.text_muted)
                        .child(h)
                })),
        )
        .child(div().flex_none().child(control))
        .into_any_element()
}

pub(super) fn switch(theme: &Theme, id: &'static str, on: bool, set: Setter<bool>) -> AnyElement {
    let track = if on { theme.accent } else { theme.border };
    let knob = if on {
        theme.on_primary
    } else {
        theme.text_muted
    };
    div()
        .id(id)
        .w(px(34.))
        .h(px(18.))
        .p(px(2.))
        .flex()
        .when(on, |s| s.justify_end())
        .rounded_full()
        .bg(track)
        .cursor_pointer()
        .on_click(move |_, window, cx| set(!on, window, cx))
        .child(div().size(px(14.)).rounded_full().bg(knob))
        .into_any_element()
}

fn segmented<T: Copy + PartialEq + 'static>(
    theme: &Theme,
    id: &'static str,
    options: &[(T, &'static str)],
    value: T,
    set: Setter<T>,
) -> AnyElement {
    div()
        .flex()
        .flex_none()
        .p(px(2.))
        .gap(px(1.))
        .rounded(px(9.))
        .bg(theme.track)
        .children(options.iter().enumerate().map(|(index, (option, label))| {
            let (option, set) = (*option, set.clone());
            let active = option == value;
            let text = theme.text;
            div()
                .id((id, index))
                .h(px(26.))
                .px(px(10.))
                .flex()
                .items_center()
                .rounded(px(7.))
                .text_size(ui_px(13.))
                .cursor_pointer()
                .when(active, |s| {
                    s.bg(theme.bg)
                        .border_1()
                        .border_color(theme.border)
                        .text_color(theme.text)
                })
                .when(!active, |s| {
                    s.text_color(theme.text_muted)
                        .hover(move |style| style.text_color(text))
                })
                .on_click(move |_, window, cx| set(option, window, cx))
                .child(*label)
        }))
        .into_any_element()
}

/// − value + control; `dec` and `inc` are the already clamped next values.
fn stepper<T: Copy + 'static>(
    theme: &Theme,
    id: &'static str,
    value: String,
    dec: T,
    inc: T,
    set: Setter<T>,
) -> AnyElement {
    let (hover, press) = (theme.hover, theme.active);
    let step = |key: &'static str, label: &'static str, next: T, set: Setter<T>| {
        div()
            .id(SharedString::from(format!("{id}-{key}")))
            .w(px(28.))
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .text_color(theme.text_muted)
            .cursor_pointer()
            .hover(move |s| s.bg(hover))
            .active(move |s| s.bg(press))
            .on_click(move |_, window, cx| set(next, window, cx))
            .child(label)
    };
    div()
        .h(px(28.))
        .flex()
        .items_center()
        .rounded(px(6.))
        .border_1()
        .border_color(theme.border)
        .bg(theme.input_bg)
        .child(step("dec", "−", dec, set.clone()))
        .child(
            div()
                .min_w(px(52.))
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .border_l_1()
                .border_r_1()
                .border_color(theme.border)
                .text_color(theme.text)
                .child(value),
        )
        .child(step("inc", "+", inc, set))
        .into_any_element()
}

/// A choice in a dropdown, with a check when it is the current one.
fn dropdown_item(
    ui: &Theme,
    id: impl Into<gpui::ElementId>,
    current: bool,
) -> gpui::Stateful<gpui::Div> {
    let (hover, press) = (ui.hover, ui.active);
    div()
        .id(id)
        .flex_none()
        .h(px(28.))
        .px(px(8.))
        .flex()
        .items_center()
        .gap(px(8.))
        .rounded(px(5.))
        .cursor_pointer()
        .hover(move |style| style.bg(hover))
        .active(move |style| style.bg(press))
        .child(div().w(px(14.)).flex_none().when(current, |this| {
            this.child(Icon::new(IconName::Check).size(px(13.)))
        }))
}

/// Something to do from a dropdown, below its choices.
fn dropdown_action(
    ui: &Theme,
    id: &'static str,
    icon: IconName,
    label: &'static str,
) -> gpui::Stateful<gpui::Div> {
    let (hover, press) = (ui.hover, ui.active);
    div()
        .id(id)
        .flex_none()
        .h(px(28.))
        .px(px(8.))
        .flex()
        .items_center()
        .gap(px(8.))
        .rounded(px(5.))
        .text_color(ui.text_muted)
        .cursor_pointer()
        .hover(move |style| style.bg(hover))
        .active(move |style| style.bg(press))
        .child(
            div()
                .w(px(14.))
                .flex_none()
                .child(Icon::new(icon).size(px(13.)).color(ui.text_muted)),
        )
        .child(label)
}

fn dropdown_divider(ui: &Theme) -> AnyElement {
    div()
        .flex_none()
        .h(px(1.))
        .my(px(4.))
        .bg(ui.border)
        .into_any_element()
}

/// A preview and a name, as a dropdown shows its current value.
fn dropdown_value(preview: impl IntoElement, name: impl Into<SharedString>) -> AnyElement {
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .items_center()
        .gap(px(8.))
        .child(preview)
        .child(div().min_w_0().truncate().child(name.into()))
        .into_any_element()
}

/// Selects an icon theme by name; an empty one is the app's own icons.
fn pick_icon_theme(
    name: String,
    cx: &mut Context<SettingsModal>,
) -> impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static {
    cx.listener(move |this, _, _, cx| {
        this.open_dropdown = None;
        let name = name.clone();
        this.change(|s| s.icon_theme = name, cx);
    })
}

/// What an icon theme makes of a few common entries. `None`: the app's own icons.
fn icon_samples(ui: &Theme, item: Option<&IconTheme>) -> impl IntoElement + use<> {
    const SAMPLES: [(&str, bool); 3] = [("src", true), ("main.rs", false), ("config.json", false)];
    let samples: Vec<AnyElement> = SAMPLES
        .iter()
        .map(|(name, is_dir)| {
            let own = if *is_dir {
                IconName::Folder
            } else {
                IconName::File
            };
            let color = if *is_dir { ui.accent } else { ui.text_muted };
            match item.and_then(|theme| theme.icon(name, *is_dir, ui.is_light)) {
                Some(path) => img(path).size(px(14.)).flex_none().into_any_element(),
                None => Icon::new(own).size(px(14.)).color(color).into_any_element(),
            }
        })
        .collect();
    div().flex().flex_none().gap(px(4.)).children(samples)
}

/// A theme's main colors, as overlapping dots.
fn theme_dots(ui: &Theme, item: &Theme) -> impl IntoElement + use<> {
    let border = ui.border;
    let dots = [item.bg, item.surface, item.accent, item.success]
        .into_iter()
        .enumerate()
        .map(move |(position, color)| {
            div()
                .size(px(12.))
                .when(position > 0, |dot| dot.ml(px(-3.)))
                .rounded_full()
                .border_1()
                // Keeps a dot that matches the dialog's own background visible.
                .border_color(border)
                .bg(color)
        });
    div().flex().flex_none().children(dots)
}

impl SettingsModal {
    /// A row for each host, which opens its settings. Their names are not known in advance.
    fn host_rows(&self, query: &str, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme().clone();
        let hosts = self.state.read(cx).hosts.clone();
        hosts
            .into_iter()
            .filter(|host| {
                query.is_empty()
                    || host.alias.to_lowercase().contains(query)
                    || host.hostname.to_lowercase().contains(query)
            })
            .enumerate()
            .map(|(index, host)| {
                let help =
                    shcp_core::address_text(Some(&host.user), &host.hostname, Some(host.port));
                let id = host.id;
                row(
                    &theme,
                    host.alias,
                    Some(help.into()),
                    Button::new(("host-edit", index))
                        .keyboard_focus()
                        .label("Edit")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_host_detail(id.clone(), window, cx)
                        })),
                )
            })
            .collect()
    }

    /// Rows of one page, keeping those that match the lowercase `query`.
    fn page_rows(&self, page: Page, query: &str, cx: &mut Context<Self>) -> Vec<AnyElement> {
        if page == Page::Hosts {
            return self.host_rows(query, cx);
        }
        let theme = cx.theme().clone();
        let settings = self.state.read(cx).settings.clone();
        let shown = |label: &str, help: &str| {
            query.is_empty()
                || label.to_lowercase().contains(query)
                || help.to_lowercase().contains(query)
        };
        let mut rows = Vec::new();
        let mut add = |label: &'static str, help: &str, control: AnyElement| {
            if shown(label, help) {
                let help = (!help.is_empty()).then(|| SharedString::from(help.to_string()));
                rows.push(row(&theme, label, help, control));
            }
        };
        match page {
            Page::Appearance => {
                add(
                    "Interface font size",
                    "",
                    stepper(
                        &theme,
                        "ui-font",
                        settings.ui_font_size.to_string(),
                        (settings.ui_font_size - 1.).max(9.),
                        (settings.ui_font_size + 1.).min(24.),
                        self.setter(|s, v| s.ui_font_size = v, cx),
                    ),
                );
                add(
                    "Density",
                    "Height of lists and bars",
                    segmented(
                        &theme,
                        "density",
                        &[
                            (Density::Compact, "Compact"),
                            (Density::Default, "Default"),
                            (Density::Comfortable, "Comfortable"),
                        ],
                        settings.density,
                        self.setter(|s, v| s.density = v, cx),
                    ),
                );
                add(
                    "Translucent window",
                    "Lets the desktop show through, blurred",
                    switch(
                        &theme,
                        "translucent-window",
                        settings.translucent_window,
                        self.setter(|s, v| s.translucent_window = v, cx),
                    ),
                );
                add(
                    "Animations",
                    "Panels, views and the highlight glide when you use the mouse, instead of jumping",
                    switch(
                        &theme,
                        "animated-selection",
                        settings.animated_selection,
                        self.setter(|s, v| s.animated_selection = v, cx),
                    ),
                );
                add(
                    "Sync with Windows",
                    &format!(
                        "{} when dark, {} when light. Picking a theme above sets the one for its mode",
                        settings.dark_theme, settings.light_theme
                    ),
                    switch(
                        &theme,
                        "sync-theme",
                        settings.sync_theme,
                        self.setter(|s, v| s.sync_theme = v, cx),
                    ),
                );
            }
            Page::Terminal => {
                let family = &settings.terminal_font_family;
                let installed = self
                    .installed_fonts
                    .iter()
                    .any(|name| name.eq_ignore_ascii_case(family));
                let font_help = if installed {
                    "The monospace fonts installed on this PC".to_string()
                } else {
                    format!("{family} is not installed, so Consolas is used instead")
                };
                let picker = self.font_picker(family, cx);
                add("Font", &font_help, picker);
                add(
                    "Font size",
                    "Also Ctrl+= / Ctrl+- / Ctrl+0",
                    stepper(
                        &theme,
                        "term-font",
                        settings.terminal_font_size.to_string(),
                        (settings.terminal_font_size - 1.).max(8.),
                        (settings.terminal_font_size + 1.).min(32.),
                        self.setter(|s, v| s.terminal_font_size = v, cx),
                    ),
                );
                add(
                    "Cursor shape",
                    "Auto lets the program in the terminal choose, as vim does between its modes",
                    segmented(
                        &theme,
                        "cursor-style",
                        &[
                            (CursorStyle::Program, "Auto"),
                            (CursorStyle::Block, "Block"),
                            (CursorStyle::Bar, "Bar"),
                            (CursorStyle::Underline, "Underline"),
                        ],
                        settings.cursor_style,
                        self.setter(|s, v| s.cursor_style = v, cx),
                    ),
                );
                add(
                    "Cursor blink",
                    "",
                    switch(
                        &theme,
                        "cursor-blink",
                        settings.cursor_blink,
                        self.setter(|s, v| s.cursor_blink = v, cx),
                    ),
                );
                add(
                    "Scrollback lines",
                    "Applies to new terminals",
                    stepper(
                        &theme,
                        "scrollback",
                        settings.scrollback_lines.to_string(),
                        settings.scrollback_lines.saturating_sub(1000).max(1000),
                        (settings.scrollback_lines + 1000).min(1_000_000),
                        self.setter(|s, v| s.scrollback_lines = v, cx),
                    ),
                );
                add(
                    "Ctrl+V pastes",
                    "Off: Ctrl+V reaches the program, as vim's visual block needs. Ctrl+Shift+V always pastes",
                    switch(
                        &theme,
                        "ctrl-v-pastes",
                        settings.ctrl_v_pastes,
                        self.setter(|s, v| s.ctrl_v_pastes = v, cx),
                    ),
                );
            }
            Page::Connections => {
                add(
                    "Use SSH agent",
                    "Keys loaded in the Windows OpenSSH agent or Pageant are tried first, so no passphrase is asked",
                    switch(
                        &theme,
                        "use-agent",
                        settings.use_ssh_agent,
                        self.setter(|s, v| s.use_ssh_agent = v, cx),
                    ),
                );
                let (keys, passwords) = (
                    settings.saved_passphrases.len(),
                    settings.saved_passwords.len(),
                );
                let state = self.state.clone();
                add(
                    "Saved passwords and passphrases",
                    &if keys + passwords == 0 {
                        "None. Tick Remember when a host or a key asks for one".to_string()
                    } else {
                        format!(
                            "{passwords} passwords and {keys} passphrases, in the Windows Credential Manager"
                        )
                    },
                    Button::new("forget-secrets")
                        .label("Forget all")
                        .disabled(keys + passwords == 0)
                        .on_click(move |_, _, cx| {
                            state.update(cx, |state, cx| state.forget_secrets(cx))
                        })
                        .into_any_element(),
                );
                let ssh_config = self.state.read(cx).engine.paths().ssh_config;
                let state = self.state.clone();
                add(
                    "SSH config",
                    "Hosts also come from ~/.ssh/config. Reload reads it again after you change it",
                    div()
                        .flex()
                        .flex_none()
                        .gap(px(6.))
                        .child(
                            Button::new("open-ssh-config")
                                .label("Open")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.open_path(ssh_config.clone(), cx)
                                })),
                        )
                        .child(
                            Button::new("reload-ssh-config")
                                .label("Reload")
                                .on_click(move |_, _, cx| state.read(cx).engine.reload_hosts()),
                        )
                        .into_any_element(),
                );
                add(
                    "Trusted servers",
                    "The server keys saved in known_hosts. Remove one whose key changed",
                    Button::new("manage-known-hosts")
                        .label("Manage…")
                        .on_click(
                            cx.listener(|this, _, window, cx| this.open_known_hosts(window, cx)),
                        )
                        .into_any_element(),
                );
                add(
                    "Reconnect automatically",
                    "When a connection with open tabs is lost. Terminals keep what they show and get a new shell",
                    switch(
                        &theme,
                        "auto-reconnect",
                        settings.auto_reconnect,
                        self.setter(|s, v| s.auto_reconnect = v, cx),
                    ),
                );
                add(
                    "Reopen tabs at startup",
                    "The tabs and split terminals of the last run connect again when shcp starts",
                    switch(
                        &theme,
                        "restore-tabs",
                        settings.restore_tabs,
                        self.setter(|s, v| s.restore_tabs = v, cx),
                    ),
                );
            }
            Page::Forwarding => {
                add(
                    "Open tunnels when connecting",
                    "Off: click a port in the sidebar to open it. Leave off if another SSH client uses the same ports. Applies from the next connection",
                    switch(
                        &theme,
                        "auto-open-forwards",
                        settings.auto_open_forwards,
                        self.setter(|s, v| s.auto_open_forwards = v, cx),
                    ),
                );
            }
            Page::Files => {
                add(
                    "Show hidden files",
                    "Names starting with a dot. Ctrl+H in the Files tab toggles it",
                    switch(
                        &theme,
                        "show-hidden",
                        settings.show_hidden_files,
                        self.setter(|s, v| s.show_hidden_files = v, cx),
                    ),
                );
                add(
                    "Folders a search skips",
                    "Names separated by commas. Pressing Enter in the filter does not look inside them",
                    div()
                        .w(px(220.))
                        .child(self.skip_folders.clone())
                        .into_any_element(),
                );
                add(
                    "Editor for remote files",
                    "Full path of the program. Empty: VS Code if installed, otherwise Notepad",
                    div()
                        .w(px(220.))
                        .child(self.editor.clone())
                        .into_any_element(),
                );
                let folder = settings.download_folder.clone();
                add(
                    "Download folder",
                    &if folder.is_empty() {
                        "Asks each time".to_string()
                    } else {
                        folder.clone()
                    },
                    div()
                        .flex()
                        .gap(px(4.))
                        .when(!folder.is_empty(), |this| {
                            this.child(
                                Button::new("download-ask")
                                    .label("Ask each time")
                                    .style(ButtonStyle::Ghost)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.change(|s| s.download_folder.clear(), cx);
                                    })),
                            )
                        })
                        .child(
                            Button::new("download-choose")
                                .icon(IconName::Folder)
                                .label("Choose…")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.choose_download_folder(cx);
                                })),
                        )
                        .into_any_element(),
                );
                add(
                    "Open transfers panel on new transfer",
                    "Ctrl+Shift+J or the button above the terminal toggle it anytime",
                    switch(
                        &theme,
                        "auto-transfers",
                        settings.auto_open_transfers,
                        self.setter(|s, v| s.auto_open_transfers = v, cx),
                    ),
                );
                add(
                    "Parallel transfers",
                    "Applies from the next connection",
                    stepper(
                        &theme,
                        "concurrency",
                        settings.transfer_concurrency.to_string(),
                        settings.transfer_concurrency.saturating_sub(1).max(1),
                        (settings.transfer_concurrency + 1).min(16),
                        self.setter(|s, v| s.transfer_concurrency = v, cx),
                    ),
                );
                add(
                    "Confirm before deleting",
                    "",
                    switch(
                        &theme,
                        "confirm-delete",
                        settings.confirm_on_delete,
                        self.setter(|s, v| s.confirm_on_delete = v, cx),
                    ),
                );
            }
            Page::Keyboard => {
                add(
                    "Change a shortcut",
                    "Click its keys and press new ones. Backspace leaves it without, Esc keeps it as it is",
                    div().into_any_element(),
                );
                for shortcut in SHORTCUTS {
                    let control = self.shortcut_control(shortcut, &settings, cx);
                    add(shortcut.label, "", control);
                }
                for (label, keys) in FIXED_SHORTCUTS {
                    add(
                        label,
                        "",
                        div()
                            .font_family(crate::theme::mono_font())
                            .text_size(ui_px(12.))
                            .text_color(theme.text_muted)
                            .child(keys)
                            .into_any_element(),
                    );
                }
            }
            Page::Hosts
            | Page::ThemeBrowser
            | Page::IconThemeBrowser
            | Page::KnownHosts
            | Page::About => {}
        }
        rows
    }

    fn theme_row(&self, query: &str, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !query.is_empty() && !"theme color dark light".contains(query) {
            return None;
        }
        let theme = cx.theme().clone();
        let settings = self.state.read(cx).settings.clone();
        let themes: Vec<Theme> = cx
            .global::<ThemeCatalog>()
            .themes
            .iter()
            .map(|item| crate::theme::with_accent(item.clone(), &settings))
            .collect();
        let themes_dir = self.themes_dir(cx);
        let current = dropdown_value(theme_dots(&theme, &theme), theme.name.clone());
        let picker = self.dropdown(
            Dropdown::Theme,
            "theme",
            current,
            move |cx| {
                let ui = cx.theme().clone();
                let mut items: Vec<AnyElement> = themes
                    .iter()
                    .enumerate()
                    .map(|(index, item)| {
                        let (name, is_light) = (item.name.to_string(), item.is_light);
                        dropdown_item(&ui, ("theme-item", index), item.name == ui.name)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.open_dropdown = None;
                                let name = name.clone();
                                this.change(
                                    |s| {
                                        s.theme = name.clone();
                                        if is_light {
                                            s.light_theme = name;
                                        } else {
                                            s.dark_theme = name;
                                        }
                                    },
                                    cx,
                                );
                            }))
                            .child(theme_dots(&ui, item))
                            .child(div().min_w_0().truncate().child(item.name.clone()))
                            .into_any_element()
                    })
                    .collect();
                items.push(dropdown_divider(&ui));
                items.push(
                    dropdown_action(&ui, "browse-themes", IconName::Search, "Browse themes…")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.open_dropdown = None;
                            this.open_theme_browser(window, cx);
                        }))
                        .into_any_element(),
                );
                items.push(
                    dropdown_action(&ui, "open-themes", IconName::Folder, "Open themes folder")
                        .tooltip(tooltip(
                            "Drop VS Code theme .json files here, then reload the themes",
                        ))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.open_dropdown = None;
                            let _ = std::fs::create_dir_all(&themes_dir);
                            cx.open_with_system(&themes_dir);
                            cx.notify();
                        }))
                        .into_any_element(),
                );
                items.push(
                    dropdown_action(&ui, "reload-themes", IconName::Refresh, "Reload themes")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.open_dropdown = None;
                            this.reload_themes(cx);
                        }))
                        .into_any_element(),
                );
                items
            },
            cx,
        );
        Some(row(&theme, "Theme", None, picker))
    }

    fn accent_row(&self, query: &str, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !query.is_empty() && !"accent color".contains(query) {
            return None;
        }
        let theme = cx.theme().clone();
        // A theme from a file brings its own accent.
        let open = theme.file.is_none();
        let chosen = self.state.read(cx).settings.accent;
        let swatches = Accent::ALL.into_iter().enumerate().map(|(index, accent)| {
            let color = crate::theme::builtin::accent_color(accent, theme.is_light);
            let set = self.setter(|s, v| s.accent = v, cx);
            div()
                .id(("accent", index))
                .size(px(22.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .border_2()
                .border_color(if open && accent == chosen {
                    theme.text
                } else {
                    gpui::transparent_black()
                })
                .when(open, |this| {
                    this.cursor_pointer()
                        .tooltip(tooltip(accent.label()))
                        .on_click(move |_, window, cx| set(accent, window, cx))
                })
                .child(div().size(px(14.)).rounded_full().bg(color))
        });
        let control = div()
            .flex()
            .gap(px(2.))
            .when(!open, |this| this.opacity(0.4))
            .children(swatches);
        let help = (!open).then(|| SharedString::from("This theme brings its own"));
        Some(row(&theme, "Accent", help, control))
    }

    fn icon_theme_row(&self, query: &str, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !query.is_empty() && !"file icons theme folder".contains(query) {
            return None;
        }
        let theme = cx.theme().clone();
        let themes = cx.global::<IconThemeCatalog>().themes.clone();
        let selected = self.state.read(cx).settings.icon_theme.clone();
        // A name that is no longer there falls back on the app's own icons.
        let active = themes
            .iter()
            .find(|item| item.name == selected)
            .map(std::sync::Arc::as_ref);
        let current = dropdown_value(
            icon_samples(&theme, active),
            active.map_or("Default".to_string(), |item| item.name.clone()),
        );
        let default_active = active.is_none();
        let picker = self.dropdown(
            Dropdown::IconTheme,
            "icon-theme",
            current,
            move |cx| {
                let ui = cx.theme().clone();
                let mut items = vec![
                    dropdown_item(&ui, ("icon-theme-item", 0usize), default_active)
                        .on_click(pick_icon_theme(String::new(), cx))
                        .child(icon_samples(&ui, None))
                        .child("Default")
                        .into_any_element(),
                ];
                items.extend(themes.iter().enumerate().map(|(index, item)| {
                    dropdown_item(&ui, ("icon-theme-item", index + 1), item.name == selected)
                        .on_click(pick_icon_theme(item.name.clone(), cx))
                        .child(icon_samples(&ui, Some(item)))
                        .child(div().min_w_0().truncate().child(item.name.clone()))
                        .into_any_element()
                }));
                items.push(dropdown_divider(&ui));
                items.push(
                    dropdown_action(
                        &ui,
                        "browse-icon-themes",
                        IconName::Search,
                        "Browse file icon themes…",
                    )
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_dropdown = None;
                        this.open_icon_theme_browser(window, cx);
                    }))
                    .into_any_element(),
                );
                items
            },
            cx,
        );
        Some(row(&theme, "File icons", None, picker))
    }

    fn updates(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme().clone();
        let state = self.state.read(cx);
        let status = state.update.clone();
        let check_daily = state.settings.check_for_updates;
        let set_check_daily = self.setter(|s, v| s.check_for_updates = v, cx);
        let (text, color, spinning) = match &status {
            UpdateStatus::Idle => (
                format!("shcp {} is installed", current_version()),
                theme.text_muted,
                false,
            ),
            UpdateStatus::Checking => ("Checking for updates…".to_string(), theme.text_muted, true),
            UpdateStatus::UpToDate => (
                format!("shcp {} is the latest version", current_version()),
                theme.text_muted,
                false,
            ),
            UpdateStatus::Available(release) => (
                format!("shcp {} is available", release.version),
                theme.text,
                false,
            ),
            UpdateStatus::Downloading { version, percent } => (
                match percent {
                    Some(percent) => format!("Downloading shcp {version}: {percent}%"),
                    None => format!("Downloading shcp {version}…"),
                },
                theme.text_muted,
                true,
            ),
            UpdateStatus::Ready { version, .. } => (
                format!("shcp {version} starts the next time you open the app"),
                theme.text,
                false,
            ),
            UpdateStatus::Failed(error) | UpdateStatus::DownloadFailed { error, .. } => {
                (error.clone(), theme.error, false)
            }
        };
        let button =
            |id: &'static str, label: &'static str| Button::new(id).keyboard_focus().label(label);
        let action = match &status {
            UpdateStatus::Checking | UpdateStatus::Downloading { .. } => None,
            UpdateStatus::Available(_) => Some(
                button("install-update", "Update")
                    .style(ButtonStyle::Primary)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.state.update(cx, |s, cx| s.install_update(cx));
                    })),
            ),
            UpdateStatus::Ready { .. } => Some(button("restart-update", "Restart now").on_click(
                cx.listener(|this, _, _, cx| {
                    this.state.update(cx, |s, cx| s.restart_into_update(cx));
                }),
            )),
            UpdateStatus::DownloadFailed { .. } => Some(button("retry-update", "Retry").on_click(
                cx.listener(|this, _, _, cx| {
                    this.state.update(cx, |s, cx| s.install_update(cx));
                }),
            )),
            UpdateStatus::Idle | UpdateStatus::UpToDate | UpdateStatus::Failed(_) => Some(
                button("check-update", "Check now").on_click(cx.listener(|this, _, _, cx| {
                    this.state.update(cx, |s, cx| s.check_for_update(true, cx));
                })),
            ),
        };
        let failed = matches!(
            status,
            UpdateStatus::Failed(_) | UpdateStatus::DownloadFailed { .. }
        );
        vec![
            section(&theme, "UPDATES"),
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .py(px(6.))
                .children(spinning.then(|| {
                    Icon::new(IconName::Spinner)
                        .size(px(14.))
                        .color(theme.accent)
                        .spin()
                }))
                .child(div().flex_1().min_w_0().text_color(color).child(text))
                .when(failed, |this| {
                    this.child(
                        button("open-releases", "Open page")
                            .style(ButtonStyle::Ghost)
                            .on_click(|_, _, cx| open_releases_page(cx)),
                    )
                })
                .children(action)
                .into_any_element(),
            row(
                &theme,
                "Check for updates",
                Some("Asks GitHub once a day. Updates are installed only when you say so".into()),
                switch(&theme, "check-updates", check_daily, set_check_daily),
            ),
        ]
    }

    fn about(&self, cx: &mut Context<Self>) -> AnyElement {
        let updates = self.updates(cx);
        let cx = &*cx;
        let theme = cx.theme().clone();
        let paths = self.state.read(cx).engine.paths();
        let session_logs = self.state.read(cx).engine.session_logs_dir();
        let open = |id: &'static str, icon: IconName, label: &'static str, path: PathBuf| {
            Button::new(id)
                .keyboard_focus()
                .icon(icon)
                .label(label)
                .on_click(cx.listener(move |this, _, _, cx| this.open_path(path.clone(), cx)))
        };
        div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .child(
                div()
                    .text_color(theme.text_muted)
                    .child(format!("shcp {}", env!("CARGO_PKG_VERSION"))),
            )
            .children(updates)
            .child(section(&theme, "FOLDERS"))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(6.))
                    .child(open(
                        "open-config",
                        IconName::Folder,
                        "Config folder",
                        paths.config_dir.clone(),
                    ))
                    .child(open(
                        "open-logs",
                        IconName::File,
                        "Logs",
                        paths.log_dir.clone(),
                    ))
                    .child(open(
                        "open-session-logs",
                        IconName::File,
                        "Terminal logs",
                        session_logs,
                    )),
            )
            .child(section(&theme, "BACKUP"))
            .child(
                div()
                    .text_size(ui_px(12.))
                    .text_color(theme.text_muted)
                    .pb(px(6.))
                    .child(
                        "Settings and the hosts added in the app, to move to another PC. Passwords, \
                         themes and ~/.ssh stay out. Importing replaces the settings, and hosts \
                         with the same name",
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(6.))
                    .child(
                        Button::new("export-backup")
                            .keyboard_focus()
                            .icon(IconName::Upload)
                            .label("Export…")
                            .on_click(cx.listener(|this, _, _, cx| this.export_backup(cx))),
                    )
                    .child(
                        Button::new("import-backup")
                            .keyboard_focus()
                            .icon(IconName::Download)
                            .label("Import…")
                            .on_click(cx.listener(|this, _, _, cx| this.import_backup(cx))),
                    ),
            )
            .into_any_element()
    }

    fn nav_item(
        &self,
        page: Page,
        label: &'static str,
        icon: IconName,
        searching: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let active = (self.host_detail.is_some() && page == Page::Hosts)
            || (!searching
                && (self.page == page
                    || (matches!(self.page, Page::ThemeBrowser | Page::IconThemeBrowser)
                        && page == Page::Appearance)
                    || (self.page == Page::KnownHosts && page == Page::Connections)));
        let (hover, press) = (theme.hover, theme.active);
        div()
            .id(label)
            .h(px(30.))
            .flex_none()
            .px(px(9.))
            .flex()
            .items_center()
            .gap(px(9.))
            .rounded(px(6.))
            .cursor_pointer()
            .text_color(if active { theme.text } else { theme.text_muted })
            .when(active, |s| s.bg(theme.selected))
            .when(!active, |s| {
                s.hover(move |s| s.bg(hover)).active(move |s| s.bg(press))
            })
            .on_click(cx.listener(move |this, _, window, cx| {
                this.navigate(Destination::Category(page), window, cx);
            }))
            .child(Icon::new(icon).color(if active {
                theme.accent
            } else {
                theme.text_muted
            }))
            .child(label)
            .into_any_element()
    }

    fn search_results(&self, query: &str, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme().clone();
        let mut results: Vec<AnyElement> = Vec::new();
        if let Some(themes) = self.theme_row(query, cx) {
            results.push(themes);
        }
        results.extend(self.accent_row(query, cx));
        if let Some(themes) = self.icon_theme_row(query, cx) {
            results.push(themes);
        }
        for (page, label, _) in PAGES {
            let rows = self.page_rows(page, query, cx);
            if !rows.is_empty() {
                results.push(
                    div()
                        .pt(px(16.))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme.text_muted)
                        .child(label)
                        .into_any_element(),
                );
                results.extend(rows);
            }
        }
        if results.is_empty() {
            results.push(
                div()
                    .pt(px(16.))
                    .text_color(theme.text_muted)
                    .child("No settings match your search")
                    .into_any_element(),
            );
        }
        results
    }
}

impl Render for SettingsModal {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let query = self.search.read(cx).text().trim().to_lowercase();
        let searching = !query.is_empty();
        let settings_file = self
            .state
            .read(cx)
            .engine
            .paths()
            .config_dir
            .join("settings.toml");
        let host_alias: Option<SharedString> = self.host_detail.as_ref().map(|detail| {
            self.state
                .read(cx)
                .host(&detail.host)
                .map_or(detail.host.0.clone(), |host| host.alias.clone())
                .into()
        });
        let back_label: SharedString = if self
            .host_detail
            .as_ref()
            .is_some_and(|detail| matches!(detail.section, HostSection::Overview))
        {
            "Hosts".into()
        } else {
            host_alias.clone().unwrap_or_else(|| "Hosts".into())
        };
        let title: SharedString = if let Some(detail) = &self.host_detail {
            match detail.section {
                HostSection::Overview => host_alias.clone().unwrap_or_else(|| "Host".into()),
                HostSection::Editor(HostPage::Connection) => "Connection".into(),
                HostSection::Editor(HostPage::Commands) => "Commands".into(),
                HostSection::Editor(HostPage::KeyLogin) => "Key login".into(),
            }
        } else if searching {
            "Search results".into()
        } else if self.page == Page::About {
            "About".into()
        } else if self.page == Page::ThemeBrowser {
            "Browse themes".into()
        } else if self.page == Page::IconThemeBrowser {
            "Browse file icon themes".into()
        } else if self.page == Page::KnownHosts {
            "Trusted servers".into()
        } else {
            PAGES
                .iter()
                .find(|(page, ..)| *page == self.page)
                .map_or("Settings", |(_, label, _)| *label)
                .into()
        };
        let content: Vec<AnyElement> = if let Some(detail) = &self.host_detail {
            vec![detail.view.clone().into_any_element()]
        } else if searching {
            self.search_results(&query, cx)
        } else if self.page == Page::About {
            vec![self.about(cx)]
        } else if self.page == Page::ThemeBrowser {
            vec![self.theme_browser.clone().into_any_element()]
        } else if self.page == Page::IconThemeBrowser {
            vec![self.icon_theme_browser.clone().into_any_element()]
        } else if self.page == Page::KnownHosts {
            self.known_hosts_rows(cx)
        } else {
            let mut content: Vec<AnyElement> = Vec::new();
            if self.page == Page::Appearance {
                content.extend(self.theme_row("", cx));
                content.extend(self.accent_row("", cx));
                content.extend(self.icon_theme_row("", cx));
            }
            content.extend(self.page_rows(self.page, "", cx));
            content
        };
        let nav: Vec<_> = PAGES
            .iter()
            .map(|(page, label, icon)| self.nav_item(*page, label, *icon, searching, cx))
            .collect();
        let about = self.nav_item(Page::About, "About", IconName::Info, searching, cx);
        div()
            .w(px(820.))
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .h(px(580.))
            .max_w(relative(0.96))
            .max_h(relative(0.96))
            .flex()
            .rounded(px(15.))
            .border_1()
            .border_color(theme.strong_border)
            .bg(theme.elevated)
            .shadow(crate::theme::float_shadow())
            .text_size(ui_px(13.))
            .text_color(theme.text)
            .capture_key_down(cx.listener(Self::on_capture_key))
            .child(
                div()
                    .w(px(204.))
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap(px(1.))
                    .p(px(8.))
                    .rounded_l(px(14.))
                    .bg(theme.surface)
                    .border_r_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .pb(px(10.))
                            .when(self.host_detail.is_none(), |this| {
                                this.child(self.search.clone())
                            })
                            .when(self.host_detail.is_some(), |this| this.h(px(38.))),
                    )
                    .children(nav)
                    .child(div().flex_1())
                    .child(about),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_none()
                            .gap(px(8.))
                            .w_full()
                            .min_w_0()
                            .pt(px(14.))
                            .pb(px(if self.host_detail.is_some() { 12. } else { 6. }))
                            .pl(px(24.))
                            .pr(px(12.))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .gap(px(12.))
                                    .when(self.host_detail.is_some(), |this| {
                                        this.child(
                                            div().min_w_0().ml(px(-10.)).child(
                                                Button::new("settings-host-back")
                                                    .keyboard_focus()
                                                    .icon(IconName::ArrowLeft)
                                                    .label(back_label)
                                                    .style(ButtonStyle::Ghost)
                                                    .on_click(cx.listener(
                                                        |this, _, window, cx| {
                                                            this.navigate(
                                                                Destination::Back,
                                                                window,
                                                                cx,
                                                            )
                                                        },
                                                    )),
                                            ),
                                        )
                                    })
                                    .when(self.host_detail.is_none(), |this| {
                                        this.child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .truncate()
                                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                                .text_size(ui_px(17.))
                                                .child(title.clone()),
                                        )
                                    })
                                    .child(
                                        Button::new("settings-close")
                                            .keyboard_focus()
                                            .icon(IconName::Close)
                                            .tooltip("Close (Esc)")
                                            .style(ButtonStyle::Ghost)
                                            .on_click(cx.listener(|_, _, _, cx| {
                                                cx.emit(super::CancelEvent)
                                            })),
                                    ),
                            )
                            .when(
                                self.host_detail.as_ref().is_some_and(|detail| {
                                    matches!(detail.section, HostSection::Editor(_))
                                }),
                                |this| {
                                    this.child(
                                        div()
                                            .w_full()
                                            .min_w_0()
                                            .flex()
                                            .flex_col()
                                            .gap(px(4.))
                                            .pr(px(12.))
                                            .child(
                                                div()
                                                    .w_full()
                                                    .min_w_0()
                                                    .flex_none()
                                                    .whitespace_normal()
                                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                                    .text_size(ui_px(17.))
                                                    .child(title),
                                            ),
                                    )
                                },
                            ),
                    )
                    .child(
                        // Not a flex column: that would squeeze the rows instead of scrolling.
                        div()
                            .id("settings-content")
                            .track_scroll(if self.host_detail.is_some() {
                                &self.detail_scroll
                            } else {
                                &self.list_scroll
                            })
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .px(px(24.))
                            .pb(px(16.))
                            .children(content),
                    )
                    .child(
                        div()
                            .h(px(44.))
                            .flex_none()
                            .pl(px(24.))
                            .pr(px(12.))
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .border_t_1()
                            .border_color(theme.border)
                            .text_size(ui_px(12.))
                            .when_some(self.error.clone(), |this, error| {
                                this.child(Icon::new(IconName::Warning).color(theme.error))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .text_color(theme.error)
                                            .child(error),
                                    )
                            })
                            .when(
                                self.error.is_none()
                                    && !self.host_detail.as_ref().is_some_and(|detail| {
                                        matches!(detail.section, HostSection::Editor(_))
                                    }),
                                |this| {
                                    this.child(Icon::new(IconName::Check).color(theme.success))
                                        .child(
                                            div()
                                                .flex_1()
                                                .text_color(theme.text_muted)
                                                .child("Changes are saved automatically"),
                                        )
                                },
                            )
                            .when(
                                self.host_detail.as_ref().is_some_and(|detail| {
                                    matches!(detail.section, HostSection::Editor(_))
                                }),
                                |this| {
                                    this.child(
                                        div()
                                            .flex_1()
                                            .text_color(theme.text_muted)
                                            .child("Save to apply changes"),
                                    )
                                },
                            )
                            .child(
                                Button::new("open-settings-file")
                                    .label("Open settings.toml")
                                    .style(ButtonStyle::Ghost)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.open_path(settings_file.clone(), cx)
                                    })),
                            ),
                    ),
            )
    }
}

impl super::ModalView for SettingsModal {
    fn inline_login(
        &mut self,
        host: &HostId,
        prompt: Entity<crate::login_prompt::LoginPrompt>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(detail) = &self.host_detail else {
            return false;
        };
        let Some(key_setup) = detail.key_setup.clone().filter(|_| detail.host == *host) else {
            return false;
        };
        key_setup.update(cx, |view, cx| view.show_login(prompt, window, cx));
        cx.notify();
        true
    }
    fn has_unsaved_changes(&self, _: &[String], cx: &App) -> bool {
        self.host_has_edits(cx)
    }
    fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.open_dropdown.is_some() {
            self.open_dropdown = None;
            cx.notify();
        } else {
            self.cancel_key_login(window, cx);
            self.close(cx);
        }
    }
}
