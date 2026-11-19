//! What surrounds the cards at the top of the window: the top of the sidebar, the header that
//! holds the tabs when they go across the window, the app menu and the host selector.
use super::*;
use crate::updater::UpdateStatus;
use crate::widgets::context_menu::{Handler, MenuEntry};
use crate::widgets::host_color::color_dot;
use crate::widgets::reorder::{Reorder, Slot, TabChip, on_release};
use crate::widgets::tile::text_lift;
use gpui::{DragMoveEvent, MouseDownEvent, Point, SharedString, canvas};
use shcp_core::TabStyle;

/// The menus that open from the top of the window.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ChromeMenu {
    App,
    Hosts,
}

/// What the top of the window holds.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Top {
    /// The tabs go across the window, in a header of their own.
    Tabs,
    /// The sidebar has the top left, and the cards reach up beside it.
    Cards,
    /// The sidebar is hidden: a strip with the app's mark and the way back to it.
    Strip,
}

/// Its own hitbox over a strip that moves the window, or a press on it would move the window.
fn hit(id: &'static str, child: impl IntoElement) -> Stateful<Div> {
    div().id(id).occlude().flex_none().child(child)
}

/// A strip across the top of the main column that moves the window, clear of the caption buttons.
/// The terminal's own layout, which it keeps to itself: the room above its first line
/// (`CONTENT_TOP_PADDING` in `terminal_view`) and its line height for a font size (in
/// `terminal_element`). Keep them in step.
const TERMINAL_TOP_PADDING: f32 = 14.;
const TERMINAL_LINE_HEIGHT: f32 = 1.56;

/// Above the header of the tabs, which is otherwise flush with the top of the window.
pub(super) const HEADER_TOP_GAP: f32 = 4.;

fn top_strip() -> Stateful<Div> {
    div()
        .id("title-strip")
        .occlude()
        .window_control_area(WindowControlArea::Drag)
        .h(px(title_bar::HEIGHT))
        .flex_none()
        .flex()
        .items_center()
        .pr(px(title_bar::strip_room()))
}

impl Workspace {
    /// How far the sidebar's list goes down for the title of its first section to sit on the
    /// first line of the terminal beside it: both under the top row, the card's header above that
    /// line, the sidebar's own top row above the title.
    pub(super) fn sidebar_list_drop(&self, cx: &App) -> f32 {
        let font = self.state.read(cx).settings.terminal_font_size;
        let line = (font * TERMINAL_LINE_HEIGHT).round().max(1.);
        let first_line = title_bar::CONTENT_INSET
            + title_bar::CARD_BORDER
            + f32::from(theme::row_px(panel::HEADER_HEIGHT))
            + TERMINAL_TOP_PADDING
            + line / 2.;
        let first_title =
            title_bar::HEIGHT + title_bar::CONTENT_INSET + crate::sidebar::first_title_middle();
        (first_line - first_title).max(0.)
    }

    pub(super) fn tab_style(&self, cx: &App) -> TabStyle {
        self.state.read(cx).settings.tab_style
    }

    /// The sidebar is to show: never with the tabs across the window.
    pub(super) fn sidebar_shown(&self, cx: &App) -> bool {
        self.sidebar_visible && self.tab_style(cx) == TabStyle::Vertical
    }

    pub(super) fn top(&self, cx: &App) -> Top {
        match self.tab_style(cx) {
            TabStyle::Horizontal => Top::Tabs,
            TabStyle::Vertical if self.sidebar_visible => Top::Cards,
            TabStyle::Vertical => Top::Strip,
        }
    }

    pub(super) fn toggle_tab_style(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.arm_sidebar(cx);
        self.chrome_menu = None;
        self.state.update(cx, AppState::toggle_tab_style);
        // The keyboard cannot stay in a sidebar that is gone.
        if !self.sidebar_shown(cx) && self.sidebar.focus_handle(cx).is_focused(window) {
            self.focus_active(window, cx);
        }
        cx.notify();
    }

    /// To the tab of a host used last, or a terminal on it when it has none: what a click on the
    /// host in the sidebar or in the selector does.
    pub(super) fn go_to_host(&mut self, host: HostId, window: &mut Window, cx: &mut Context<Self>) {
        let recent = self
            .recent_tabs
            .get(&host)
            .and_then(|id| self.tab_index(*id))
            .filter(|index| self.tabs[*index].host == host);
        match recent.or_else(|| self.tabs.iter().position(|tab| tab.host == host)) {
            Some(index) => self.activate(index, window, cx),
            None => self.open_terminal(host, window, cx),
        }
    }

    /// A terminal on the current host; without hosts, the way to add one.
    fn new_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.current_host(cx) {
            Some(host) => self.open_terminal(host, window, cx),
            None => self.new_host(window, cx),
        }
    }

    /// As the X of the window does, asking first if closing would interrupt something.
    fn close_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.on_close_requested(window, cx) {
            window.remove_window();
        }
    }

    // ---- menus ----

    /// Opens a menu under its button, or closes it.
    fn toggle_chrome_menu(&mut self, kind: ChromeMenu, cx: &mut Context<Self>) {
        let anchor = match kind {
            ChromeMenu::App => self.menu_anchor.clone(),
            ChromeMenu::Hosts => self.host_anchor.clone(),
        };
        // The press on the button already closed the menu: the click must not open it again.
        let closed_by_this_press = self
            .chrome_menu_closed
            .take()
            .is_some_and(|(closed, at)| closed == kind && anchor.contains(at));
        if closed_by_this_press {
            return;
        }
        let entries = match kind {
            ChromeMenu::App => self.app_menu_entries(cx),
            ChromeMenu::Hosts => self.host_menu_entries(cx),
        };
        self.chrome_menu = Some((kind, OpenMenu::below_left(&anchor, entries)));
        cx.notify();
    }

    pub(super) fn chrome_menu_dismiss(&self, cx: &mut Context<Self>) -> Handler {
        let this = cx.entity().downgrade();
        Rc::new(move |window, cx| {
            let at = window.mouse_position();
            this.update(cx, |this, cx| {
                if let Some((kind, _)) = this.chrome_menu.take() {
                    this.chrome_menu_closed = Some((kind, at));
                }
                cx.notify();
            })
            .ok();
        })
    }

    fn app_menu_entries(&self, cx: &mut Context<Self>) -> Vec<MenuEntry> {
        let this = cx.entity().downgrade();
        let horizontal = self.tab_style(cx) == TabStyle::Horizontal;
        let no_host = self.current_host(cx).is_none();
        let files_shown = self.active_tab().is_some_and(|tab| tab.face.shows_files());
        let panes = self.active_panes();
        let font = |change: Option<f32>| {
            handler(this.clone(), move |w, _, cx| {
                w.state.update(cx, |state, cx| match change {
                    Some(delta) => state.change_font_size(delta, cx),
                    None => state.reset_font_size(cx),
                })
            })
        };
        let (theme_label, theme_icon) = if cx.theme().name == theme::builtin::TERRACOTTA {
            ("Switch to Black", IconName::Moon)
        } else {
            ("Switch to Terracotta", IconName::Sun)
        };
        let view = vec![
            MenuEntry::item(
                "Toggle tab style",
                handler(this.clone(), Self::toggle_tab_style),
            )
            .shortcut("Ctrl+Shift+L"),
            MenuEntry::item(
                "Toggle sidebar",
                handler(this.clone(), |w, window, cx| {
                    w.arm_sidebar(cx);
                    w.toggle_sidebar(window, cx);
                }),
            )
            .icon(IconName::PanelLeft)
            .shortcut("Ctrl+Shift+B")
            .disabled(horizontal),
            MenuEntry::item(
                "Toggle transfers",
                handler(this.clone(), |w, _, cx| {
                    w.toggle_dock(DockTab::Transfers, cx)
                }),
            )
            .icon(IconName::Swap)
            .shortcut("Ctrl+Shift+J"),
            MenuEntry::item(
                "Toggle ports",
                handler(this.clone(), |w, _, cx| w.toggle_dock(DockTab::Ports, cx)),
            )
            .icon(IconName::Link)
            .shortcut("Ctrl+Shift+O"),
            MenuEntry::Separator,
            MenuEntry::item("Zoom in", font(Some(1.))).shortcut("Ctrl+="),
            MenuEntry::item("Zoom out", font(Some(-1.))).shortcut("Ctrl+-"),
            MenuEntry::item("Reset zoom", font(None)).shortcut("Ctrl+0"),
            MenuEntry::Separator,
            MenuEntry::item(
                theme_label,
                handler(this.clone(), |w, _, cx| w.toggle_theme(cx)),
            )
            .icon(theme_icon),
        ];
        // The files sit beside one terminal: no splitting while they show.
        let no_split = panes.is_none() || files_shown;
        let mut pane: Vec<MenuEntry> = tabs::SPLITS
            .into_iter()
            .map(|(label, icon, shortcut, axis)| {
                let panes = panes.clone();
                MenuEntry::item(
                    label,
                    Rc::new(move |window: &mut Window, cx: &mut App| {
                        if let Some(panes) = &panes {
                            panes.update(cx, |panes, cx| panes.split(axis, window, cx));
                        }
                    }),
                )
                .icon(icon)
                .shortcut(shortcut)
                .disabled(no_split)
            })
            .collect();
        pane.push(
            MenuEntry::item(
                "Close terminal",
                handler(this.clone(), Self::close_pane_or_tab),
            )
            .icon(IconName::Close)
            .shortcut("Ctrl+Shift+W")
            .disabled(self.active.is_none()),
        );
        vec![
            MenuEntry::item("New terminal", handler(this.clone(), Self::new_tab))
                .icon(IconName::Terminal)
                .shortcut("Ctrl+Shift+T")
                .disabled(no_host),
            MenuEntry::item(
                "Browse files",
                handler(this.clone(), |w, window, cx| {
                    if let Some(host) = w.current_host(cx) {
                        w.open_files(host, window, cx);
                    }
                }),
            )
            .icon(IconName::Folder)
            .shortcut("Ctrl+Shift+E")
            .disabled(no_host),
            MenuEntry::item("New host…", handler(this.clone(), Self::new_host))
                .icon(IconName::Plus)
                .shortcut("Ctrl+Shift+N"),
            MenuEntry::Separator,
            MenuEntry::submenu("View", view),
            MenuEntry::submenu("Terminal", pane).icon(IconName::SplitRight),
            MenuEntry::Separator,
            MenuEntry::item(
                "Command palette…",
                handler(this.clone(), Self::open_palette),
            )
            .icon(IconName::Command)
            .shortcut("Ctrl+Shift+P"),
            MenuEntry::item("Settings…", handler(this.clone(), Self::open_settings))
                .icon(IconName::Settings)
                .shortcut("Ctrl+,"),
            self.update_entry(&this, cx),
            MenuEntry::Separator,
            MenuEntry::item("Close window", handler(this, Self::close_window)).shortcut("Alt+F4"),
        ]
    }

    /// Where an update stands, and its next step. The sidebar says it too, when it shows.
    fn update_entry(&self, this: &WeakEntity<Self>, cx: &App) -> MenuEntry {
        let waiting =
            |label: &'static str| MenuEntry::item(label, Rc::new(|_, _| {})).disabled(true);
        match &self.state.read(cx).update {
            UpdateStatus::Available(release) => MenuEntry::item(
                format!("Download shcp {}", release.version),
                handler(this.clone(), |w, _, cx| {
                    w.state.update(cx, AppState::install_update)
                }),
            )
            .icon(IconName::Download),
            UpdateStatus::DownloadFailed { release, .. } => MenuEntry::item(
                format!("Retry the download of shcp {}", release.version),
                handler(this.clone(), |w, _, cx| {
                    w.state.update(cx, AppState::install_update)
                }),
            )
            .icon(IconName::Download),
            UpdateStatus::Downloading { .. } => waiting("Downloading the update"),
            UpdateStatus::Ready { version, .. } => MenuEntry::item(
                format!("Restart into shcp {version}"),
                handler(this.clone(), Self::confirm_update_restart),
            )
            .icon(IconName::Refresh),
            UpdateStatus::Checking => waiting("Checking for updates"),
            UpdateStatus::Idle | UpdateStatus::UpToDate | UpdateStatus::Failed(_) => {
                MenuEntry::item(
                    "Check for updates…",
                    handler(this.clone(), |w, _, cx| {
                        w.update_check_asked = true;
                        w.state
                            .update(cx, |state, cx| state.check_for_update(true, cx));
                    }),
                )
            }
        }
    }

    /// After a check from the menu, says what came of it: the sidebar shows an update only, and
    /// may be hidden.
    pub(super) fn report_update_check(&mut self, cx: &mut Context<Self>) {
        if !self.update_check_asked {
            return;
        }
        let (level, text) = match &self.state.read(cx).update {
            UpdateStatus::UpToDate => (NoticeLevel::Info, "shcp is up to date".to_string()),
            UpdateStatus::Available(release) => (
                NoticeLevel::Info,
                format!("shcp {} is out: the menu downloads it", release.version),
            ),
            UpdateStatus::Failed(error) => (
                NoticeLevel::Error,
                format!("Could not check for updates: {error}"),
            ),
            UpdateStatus::Checking => return,
            _ => {
                self.update_check_asked = false;
                return;
            }
        };
        self.update_check_asked = false;
        self.state
            .update(cx, |state, cx| state.push_notice(level, text, cx));
    }

    /// The hosts, those of each group in a submenu of its own, and the way to add one.
    fn host_menu_entries(&self, cx: &mut Context<Self>) -> Vec<MenuEntry> {
        let this = cx.entity().downgrade();
        let current = self.current_host(cx);
        let state = self.state.read(cx);
        let entry = |host: &HostConfig| {
            let id = host.id.clone();
            let entry = MenuEntry::item(
                host.alias.clone(),
                handler(this.clone(), move |w, window, cx| {
                    w.go_to_host(id.clone(), window, cx)
                }),
            );
            if current.as_ref() == Some(&host.id) {
                entry.icon(IconName::Check)
            } else {
                entry
            }
        };
        // As the sidebar lists them: a hidden host only while it has tabs.
        let listed: Vec<HostConfig> = state
            .ordered_hosts()
            .into_iter()
            .filter(|host| {
                !state.is_host_hidden(&host.id) || self.tabs.iter().any(|tab| tab.host == host.id)
            })
            .collect();
        let mut entries: Vec<MenuEntry> = listed
            .iter()
            .filter(|host| state.host_group(&host.id).is_none())
            .map(entry)
            .collect();
        for group in state.groups() {
            let members: Vec<MenuEntry> = listed
                .iter()
                .filter(|host| state.host_group(&host.id) == Some(group.as_str()))
                .map(entry)
                .collect();
            if !members.is_empty() {
                entries.push(MenuEntry::submenu(group, members).icon(IconName::Folder));
            }
        }
        if !entries.is_empty() {
            entries.push(MenuEntry::Separator);
        }
        entries.push(
            MenuEntry::item("New host…", handler(this, Self::new_host))
                .icon(IconName::Plus)
                .shortcut("Ctrl+Shift+N"),
        );
        entries
    }

    // ---- buttons ----

    /// Three lines: the app menu.
    fn menu_button(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let open = matches!(self.chrome_menu, Some((ChromeMenu::App, _)));
        hit(
            "app-menu-hit",
            self.menu_anchor.wrap(
                Button::new("app-menu")
                    .icon(IconName::Menu)
                    .tooltip("Menu")
                    .style(ButtonStyle::Ghost)
                    .compact()
                    .active(open)
                    .on_click(
                        cx.listener(|this, _, _, cx| this.toggle_chrome_menu(ChromeMenu::App, cx)),
                    ),
            ),
        )
    }

    fn palette_button(cx: &mut Context<Self>) -> impl IntoElement + use<> {
        hit(
            "palette-hit",
            Button::new("open-palette")
                .icon(IconName::Command)
                .tooltip("Command palette (Ctrl+Shift+P)")
                .style(ButtonStyle::Ghost)
                .compact()
                .on_click(cx.listener(|this, _, window, cx| this.open_palette(window, cx))),
        )
    }

    // ---- the top of the window ----

    /// The top of the sidebar: the app's mark and menu, the palette and a new terminal. It moves the
    /// window, and lines up with the headers of the cards beside it.
    pub(super) fn sidebar_header(
        &self,
        theme: &theme::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        div()
            .id("sidebar-header")
            .occlude()
            .window_control_area(WindowControlArea::Drag)
            .flex_none()
            .flex()
            .items_center()
            .gap(px(4.))
            .h(px(title_bar::HEIGHT + title_bar::CONTENT_INSET))
            .pt(px(title_bar::CONTENT_INSET))
            .pl(px(16.))
            .pr(px(2.))
            .child(div().mr(px(4.)).child(title_bar::logo(px(18.), theme.text)))
            .child(self.menu_button(cx))
            .child(div().flex_1())
            .child(Self::palette_button(cx))
    }

    /// Over the main column: the header of the tabs, a strip while the sidebar is hidden, or only
    /// the inset above the cards.
    pub(super) fn top_of_main(&self, theme: &theme::Theme, cx: &mut Context<Self>) -> AnyElement {
        match self.top(cx) {
            Top::Tabs => self.tabs_header(theme, cx).into_any_element(),
            Top::Strip => top_strip()
                .pl(px(16.))
                .gap(px(4.))
                .child(div().mr(px(4.)).child(title_bar::logo(px(18.), theme.text)))
                .child(self.menu_button(cx))
                .child(hit(
                    "show-sidebar-hit",
                    Button::new("show-sidebar")
                        .icon(IconName::PanelLeft)
                        .tooltip("Show sidebar (Ctrl+Shift+B)")
                        .style(ButtonStyle::Ghost)
                        .compact()
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.arm_sidebar(cx);
                            this.toggle_sidebar(window, cx)
                        })),
                ))
                .child(Self::palette_button(cx))
                .into_any_element(),
            // Without a tab there is no card up there: all the height of the sidebar's top moves
            // the window.
            Top::Cards => div()
                .id("title-strip")
                .occlude()
                .window_control_area(WindowControlArea::Drag)
                .flex_none()
                .h(px(if self.active.is_some() {
                    title_bar::CONTENT_INSET
                } else {
                    title_bar::HEIGHT + title_bar::CONTENT_INSET
                }))
                .into_any_element(),
        }
    }

    /// The tabs across the window: the app's mark and menu, the palette, the host, then the tabs
    /// and a new one. What is left moves the window.
    fn tabs_header(&self, theme: &theme::Theme, cx: &mut Context<Self>) -> Stateful<Div> {
        top_strip()
            // Clear of the edge of the window, as the cards are of the header.
            .h(px(title_bar::HEIGHT + HEADER_TOP_GAP))
            .pt(px(HEADER_TOP_GAP))
            .pl(px(16.))
            .gap(px(4.))
            .child(div().mr(px(4.)).child(title_bar::logo(px(18.), theme.text)))
            .child(self.menu_button(cx))
            .child(
                div()
                    .flex_none()
                    .mx(px(4.))
                    .w(px(1.))
                    .h(px(16.))
                    .bg(theme.border),
            )
            .child(Self::palette_button(cx))
            .child(self.host_selector(theme, cx))
            .child(
                div()
                    .id("header-tabs")
                    .ml(px(4.))
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(2.))
                    .overflow_hidden()
                    .on_drag_move(cx.listener(|this, event: &DragMoveEvent<TabChip>, _, cx| {
                        let Some(reorder) = &mut this.strip_reorder else {
                            return;
                        };
                        let pointer = event.event.position;
                        // Some way off the strip still counts: a tab is dropped among the others.
                        reorder.inside = event.bounds.dilate(px(32.)).contains(&pointer);
                        if reorder.aim(f32::from(pointer.x)) {
                            cx.notify();
                        }
                    }))
                    .children(self.header_tabs(theme, cx))
                    .when(self.strip_reorder.is_some(), |this| {
                        let view = cx.entity().downgrade();
                        this.child(on_release(move |_, cx| {
                            view.update(cx, |this, cx| this.finish_strip_reorder(cx))
                                .ok();
                        }))
                    }),
            )
            .child(hit(
                "new-tab-hit",
                Button::new("new-tab")
                    .icon(IconName::Plus)
                    .tooltip("New terminal (Ctrl+Shift+T)")
                    .style(ButtonStyle::Ghost)
                    .compact()
                    .on_click(cx.listener(|this, _, window, cx| this.new_tab(window, cx))),
            ))
    }

    /// A tab of the header starts moving: the tabs of its host are where they were last drawn.
    fn start_strip_reorder(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(host) = self
            .tab_index(id)
            .map(|index| self.tabs[index].host.clone())
        else {
            return;
        };
        let bounds = self.strip_bounds.borrow();
        let slots: Option<Vec<Slot>> = self
            .tabs
            .iter()
            .filter(|tab| tab.host == host)
            .map(|tab| {
                bounds.get(&tab.id).map(|area| {
                    Slot::item(tab.id, f32::from(area.origin.x), f32::from(area.size.width))
                })
            })
            .collect();
        let animated = self.animated(cx);
        let reorder = slots.and_then(|slots| Reorder::start(id, slots, animated));
        drop(bounds);
        self.strip_reorder = reorder;
        cx.notify();
    }

    /// Let go: over the strip, the tab takes its new place among those of its host.
    fn finish_strip_reorder(&mut self, cx: &mut Context<Self>) {
        let Some(reorder) = self.strip_reorder.take() else {
            return;
        };
        if let Some(place) = reorder.moved_to() {
            self.reorder_tab(reorder.dragged(), place.index, cx);
        }
        cx.notify();
    }

    /// The bottom of the sidebar: a new host and the state of an update at the left, the dock's
    /// buttons at the right.
    pub(super) fn sidebar_footer(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(2.))
            .pl(px(10.))
            .pr(px(2.))
            .pb(px(title_bar::CONTENT_INSET))
            .child(
                Button::new("new-host")
                    .icon(IconName::ServerAdd)
                    .tooltip("New host (Ctrl+Shift+N)")
                    .style(ButtonStyle::Ghost)
                    .compact()
                    .on_click(cx.listener(|this, _, window, cx| this.new_host(window, cx))),
            )
            .children(self.update_button(cx))
            .child(div().flex_1())
            .child(self.dock_buttons(cx))
    }

    /// Says where an update stands, and takes it to the next step. Nothing while up to date.
    fn update_button(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        type Click = Box<dyn Fn(&gpui::ClickEvent, &mut Window, &mut App)>;
        let theme = cx.theme().clone();
        let icon = |name: IconName, color| Icon::new(name).color(color).into_any_element();
        let install = |cx: &mut Context<Self>| -> Click {
            Box::new(cx.listener(|this, _, _, cx| {
                this.state.update(cx, AppState::install_update);
            }))
        };
        let (leading, label, tooltip_text, color, click): (_, _, _, _, Option<Click>) =
            match self.state.read(cx).update.clone() {
                UpdateStatus::Available(release) => (
                    icon(IconName::Download, theme.text_muted),
                    release.version.clone(),
                    format!("Download shcp {}", release.version),
                    theme.text_muted,
                    Some(install(cx)),
                ),
                UpdateStatus::Downloading { version, percent } => (
                    match percent {
                        Some(percent) => ProgressRing::new(percent, px(14.)).into_any_element(),
                        None => Icon::new(IconName::Spinner)
                            .color(theme.accent)
                            .spin()
                            .into_any_element(),
                    },
                    percent.map_or(String::new(), |percent| format!("{percent}%")),
                    format!("Downloading shcp {version}"),
                    theme.text_muted,
                    None,
                ),
                UpdateStatus::Ready { version, .. } => (
                    icon(IconName::Refresh, theme.text_muted),
                    "Restart".to_string(),
                    format!(
                        "shcp {version} is ready. It also starts the next time you open the app"
                    ),
                    theme.text_muted,
                    Some(Box::new(cx.listener(|this, _, window, cx| {
                        this.confirm_update_restart(window, cx)
                    }))),
                ),
                UpdateStatus::DownloadFailed { release, error } => (
                    icon(IconName::Alert, theme.error),
                    "Retry".to_string(),
                    format!("Could not download shcp {}: {error}", release.version),
                    theme.error,
                    Some(install(cx)),
                ),
                UpdateStatus::Idle
                | UpdateStatus::Checking
                | UpdateStatus::UpToDate
                | UpdateStatus::Failed(_) => return None,
            };
        let (hover, press) = (theme.hover, theme.active);
        Some(
            div()
                .id("update-button")
                .h(px(24.))
                .flex()
                .flex_none()
                .items_center()
                .gap(px(5.))
                .px(px(8.))
                .rounded(px(theme::RADIUS_CONTROL))
                .text_size(theme::ui_px(13.))
                .text_color(color)
                .tooltip(tooltip(tooltip_text))
                .when_some(click, |this, click| {
                    this.cursor_pointer()
                        .hover(move |style| style.bg(hover))
                        .active(move |style| style.bg(press))
                        .on_click(click)
                })
                .child(leading)
                .when(!label.is_empty(), |this| this.child(label))
                .into_any_element(),
        )
    }

    /// The current host and how to reach it, opening the list of all of them.
    fn host_selector(
        &self,
        theme: &theme::Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let state = self.state.read(cx);
        let host = self.current_host(cx);
        let config = host.as_ref().and_then(|id| state.host(id));
        // The button shows the name; its tooltip says the group and where the host is.
        let (name, detail) = match (&host, config) {
            (Some(id), Some(config)) => (
                config.alias.clone(),
                [
                    state.host_group(id).map(str::to_string),
                    Some(config.target()),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join("\n"),
            ),
            (Some(id), None) => (id.0.clone(), String::new()),
            (None, _) => ("No host".to_string(), String::new()),
        };
        let tip = if detail.is_empty() {
            "Hosts".to_string()
        } else {
            format!("{name}\n{detail}")
        };
        // In the host's color, faint while not connected; a dot says what needs the user, or
        // that it is on its way.
        let mark = host.as_ref().map(|id| {
            let color = theme.host_color(state.host_color(id));
            let faint = gpui::Hsla { a: 0.5, ..color };
            let session = state.host_session(id).map(|session| &session.state);
            let reconnecting = state.reconnect_attempt(id).is_some();
            let badge = match session {
                Some(SessionState::Failed { .. }) if !reconnecting => Some(theme.error),
                Some(
                    SessionState::AwaitingHostKey { .. }
                    | SessionState::AwaitingPassphrase { .. }
                    | SessionState::AwaitingPassword { .. }
                    | SessionState::AwaitingAnswer { .. },
                ) => Some(theme.warning),
                Some(SessionState::Connecting { .. }) => Some(faint),
                _ if reconnecting => Some(faint),
                _ => None,
            };
            let connected = matches!(session, Some(SessionState::Connected));
            div()
                .relative()
                .flex_none()
                .child(
                    Icon::new(IconName::Server)
                        .size(px(16.))
                        .color(if connected { color } else { faint }),
                )
                .children(badge.map(|badge| {
                    div()
                        .absolute()
                        .right(px(-2.))
                        .bottom(px(-2.))
                        .child(color_dot(badge, true))
                }))
        });
        let open = matches!(self.chrome_menu, Some((ChromeMenu::Hosts, _)));
        let (hover, press) = (theme.hover, theme.active);
        hit(
            "host-selector-hit",
            self.host_anchor.wrap(
                div()
                    .id("host-selector")
                    // As tall and round as the tabs, with room around what it shows.
                    .h(px(28.))
                    .max_w(px(260.))
                    .pl(px(10.))
                    .pr(px(8.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .rounded(px(theme::RADIUS_CONTROL))
                    .cursor_pointer()
                    .when(open, |this| this.bg(press))
                    .hover(move |style| style.bg(hover))
                    .active(move |style| style.bg(press))
                    .tooltip(tooltip(tip))
                    .on_click(
                        cx.listener(|this, _, _, cx| {
                            this.toggle_chrome_menu(ChromeMenu::Hosts, cx)
                        }),
                    )
                    .children(mark)
                    .child(
                        div()
                            .relative()
                            .top(-text_lift(theme::ui_font(), theme::ui_px(13.), cx))
                            .min_w_0()
                            .truncate()
                            .text_size(theme::ui_px(13.))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme.text)
                            .child(name),
                    )
                    .child(
                        Icon::new(IconName::ChevronDown)
                            .size(px(12.))
                            .color(theme.text_disabled),
                    ),
            ),
        )
    }

    /// One per tab, in order: what it shows in the color of its host, and its title. The current
    /// one is filled; a close button shows on it and under the pointer.
    fn header_tabs(&self, theme: &theme::Theme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        // As the sidebar has them, from this frame: those of the host in the selector.
        let host = self.current_host(cx);
        let open = self.sidebar.read(cx).open.clone();
        open.into_iter()
            .filter(|tab| Some(&tab.host) == host.as_ref())
            .map(|tab| {
                let id = tab.id;
                let color = theme.host_color(self.state.read(cx).host_color(&tab.host));
                let group = SharedString::from(format!("top-tab-{id}"));
                let (hover, press, text) = (theme.hover, theme.active, theme.text);
                let chip = TabChip {
                    id,
                    icon: tab.face.icon(),
                    color,
                    title: tab.title.clone().into(),
                    grabbed: Point::default(),
                };
                // While a tab moves, the others are drawn where they make way, and it leaves its
                // place faint behind it. Their places are kept from before the drag.
                let reordering = self.strip_reorder.is_some();
                let offset = self
                    .strip_reorder
                    .as_ref()
                    .map_or(0., |reorder| reorder.offset(id));
                let lifted = self
                    .strip_reorder
                    .as_ref()
                    .is_some_and(|reorder| reorder.dragged() == id);
                let bounds = self.strip_bounds.clone();
                let view = cx.entity().downgrade();
                div()
                    .id(("top-tab", id as usize))
                    .group(group.clone())
                    .occlude()
                    .relative()
                    .flex_none()
                    .when(offset != 0., |this| this.left(px(offset)))
                    .when(lifted, |this| this.opacity(0.35))
                    .child(
                        canvas(
                            move |area, _, _| {
                                if !reordering {
                                    bounds.borrow_mut().insert(id, area);
                                }
                            },
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .size_full(),
                    )
                    .on_drag(chip, move |chip, grabbed, _, cx| {
                        view.update(cx, |this, cx| this.start_strip_reorder(chip.id, cx))
                            .ok();
                        cx.new(|_| TabChip {
                            grabbed,
                            ..chip.clone()
                        })
                    })
                    .h(px(28.))
                    .min_w(px(72.))
                    .max_w(px(200.))
                    .pl(px(8.))
                    .pr(px(4.))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .rounded(px(theme::RADIUS_CONTROL))
                    .cursor_pointer()
                    .text_size(theme::ui_px(13.))
                    .text_color(if tab.active {
                        theme.text
                    } else {
                        theme.text_muted
                    })
                    .map(|this| {
                        if tab.active {
                            this.bg(press)
                        } else {
                            this.hover(move |style| style.bg(hover).text_color(text))
                        }
                    })
                    .tooltip(tooltip(format!("{}\n{}", tab.detail, tab.face.label())))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if let Some(index) = this.tab_index(id) {
                            this.activate(index, window, cx);
                        }
                    }))
                    .on_mouse_up(
                        MouseButton::Middle,
                        cx.listener(move |this, _, window, cx| {
                            if let Some(index) = this.tab_index(id) {
                                this.close_tab(index, window, cx);
                            }
                        }),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            tabs::tab_menu(this, id, event.position, cx);
                        }),
                    )
                    .child(Icon::new(tab.face.icon()).size(px(14.)).color(color))
                    .child(
                        div()
                            .relative()
                            .top(-text_lift(theme::ui_font(), theme::ui_px(13.), cx))
                            .min_w_0()
                            .truncate()
                            .child(tab.title),
                    )
                    .child(
                        div()
                            .id(("top-tab-close", id as usize))
                            .size(px(18.))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(theme::RADIUS_SMALL))
                            .text_color(theme.text_muted)
                            .when(!tab.active, |this| {
                                this.invisible().group_hover(group, Styled::visible)
                            })
                            .hover(move |style| style.bg(hover).text_color(text))
                            .tooltip(tooltip("Close (Ctrl+Shift+W, middle-click)"))
                            // A press here stays a press, not the start of a drag of the tab.
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                if let Some(index) = this.tab_index(id) {
                                    this.close_tab(index, window, cx);
                                }
                            }))
                            .child(Icon::new(IconName::Close).size(px(12.))),
                    )
                    .into_any_element()
            })
            .collect()
    }
}
