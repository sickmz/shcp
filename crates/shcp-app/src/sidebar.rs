//! Left column: the hosts in the user's order, by group, each with its open sessions under it in
//! the order of the tabs. A tile in the host's color says what a session shows; the name of a
//! host is bright while it is connected.
use crate::motion::Reveal;
use crate::state::AppState;
use crate::tabs::Face;
use crate::theme::{ActiveTheme as _, Theme, row_px};
use crate::widgets::button::{Button, ButtonStyle};
use crate::widgets::context_menu::{ContextMenu, MenuEntry, OpenMenu, handler};
use crate::widgets::icon::{Icon, IconName};
use crate::widgets::pill::SlidingPill;
use crate::widgets::reorder::{TabChip, on_release};
use crate::widgets::tile::{Mark, shell_tile, text_lift, tile};
use crate::widgets::tooltip::tooltip;
use gpui::prelude::*;
use gpui::{
    AnyElement, App, Bounds, Context, Div, DragMoveEvent, Entity, EventEmitter, FocusHandle,
    Focusable, Hsla, KeyDownEvent, MouseButton, MouseDownEvent, Pixels, Point, SharedString,
    Stateful, Subscription, Window, div, px,
};
use moving::{Moving, RowKey};
use shcp_core::{HostConfig, HostId, HostSource, SessionState};
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    rc::Rc,
};

mod moving;

pub(crate) enum SidebarEvent {
    OpenTerminal(HostId),
    OpenFiles(HostId),
    Disconnect(HostId),
    HostSettings(HostId),
    DuplicateHost(HostId),
    NewGroup(HostId),
    RenameGroup(String),
    Connect(HostId),
    /// To the tab of a host used last, or a terminal on it when it has none.
    GoToHost(HostId),
    ActivateTab(u64),
    CloseTab(u64),
    TabMenu(u64, Point<Pixels>),
    /// A tab goes to this place among the tabs of its host.
    ReorderTab(u64, usize),
}

/// What the sidebar needs to know of an open tab. The workspace owns the tabs.
#[derive(Clone)]
pub(crate) struct OpenTab {
    pub id: u64,
    pub host: HostId,
    pub title: String,
    /// The name the user gave it, instead of what it shows and where.
    pub name: Option<String>,
    /// Where it is, shortened for the row.
    pub folder: Option<String>,
    pub detail: String,
    pub face: Face,
    /// Its terminals are split in more than one.
    pub split: bool,
    pub active: bool,
}

/// The title of a part of the list. `first` has nothing above to keep away from.
fn section_label(text: &'static str, first: bool, theme: &Theme) -> Div {
    div()
        .relative()
        .flex()
        .flex_none()
        .items_center()
        .h(px(LABEL_HEIGHT))
        .when(!first, |this| this.mt(px(LABEL_SPACE)))
        .pl(LABEL_INSET)
        .text_size(crate::theme::ui_px(12.))
        .text_color(theme.text_disabled)
        .child(text)
}

/// A host being dragged to another place.
#[derive(Clone)]
struct HostDrag {
    id: HostId,
    alias: String,
}

/// A group being dragged to another place.
#[derive(Clone)]
struct GroupDrag {
    name: String,
}

/// The space between the rows of the list.
const ROW_GAP: Pixels = px(2.);
/// How tall the title of a part of the list is, and the room above it.
const LABEL_HEIGHT: f32 = 26.;
const LABEL_SPACE: f32 = 10.;
/// Where the titles and the dots of the hosts start.
const LABEL_INSET: Pixels = px(13.);
const SESSION_HEIGHT: f32 = 32.;
/// From the top of the list to the middle of its first row, a section's title: to line that title
/// up with something beside the sidebar.
pub(crate) fn first_title_middle() -> f32 {
    f32::from(ROW_GAP) + LABEL_HEIGHT / 2.
}

/// Where the sessions under a host start: in from the host, which heads them.
const SESSION_INSET: Pixels = px(24.);
const HOST_HEIGHT: f32 = 30.;
/// Buttons and whatever else is a control. The rows of the list are rounded alike.
const CONTROL_RADIUS: Pixels = px(crate::theme::RADIUS_CONTROL);

/// A row of what folds, in the list: as tall as `open` says of `height`. With its gap too, so that
/// the rows under it come up without a jump at the end. It fades later than it shrinks: fading
/// along, a row with nothing moving under it would be gone before anyone saw it go.
fn folding(row: impl IntoElement, height: Pixels, open: f32) -> Div {
    let moving = open < 1.;
    div()
        .relative()
        .flex_none()
        .when(moving, |this| {
            this.h(height * open)
                .mb(-ROW_GAP * (1. - open))
                .opacity(open.sqrt())
        })
        // Clipped only on its way.
        .child(
            div()
                .when(moving, |this| this.h_full().overflow_hidden())
                .child(row),
        )
}

pub(crate) struct Sidebar {
    state: Entity<AppState>,
    selected: Option<HostId>,
    pub(crate) open: Vec<OpenTab>,
    pill: SlidingPill,
    /// List the hidden hosts too, dimmed, to show them again.
    show_hidden: bool,
    /// The arrow keys are moving the selection, so it shows.
    keyboard: bool,
    /// What folds under each group, by its name, for it to glide open and closed.
    folds: HashMap<String, Reveal>,
    /// A session, a host or a group on its way to another place.
    moving: Option<Moving>,
    /// Where each row was last drawn while nothing moved: the places a drag starts from.
    row_bounds: Rc<RefCell<HashMap<RowKey, Bounds<Pixels>>>>,
    /// For this frame, while something moves: how far each row is drawn from its place, and the
    /// rows of what moves, which stay faint where they were.
    shifts: HashMap<RowKey, f32>,
    lifted: HashSet<RowKey>,
    focus: FocusHandle,
    menu: Option<OpenMenu>,
    _subscription: Subscription,
}

impl EventEmitter<SidebarEvent> for Sidebar {}

impl Focusable for Sidebar {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Sidebar {
    pub(crate) fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let subscription = cx.observe(&state, |_, _, cx| cx.notify());
        Self {
            selected: {
                let state = state.read(cx);
                let first = state.hosts.iter().find(|h| !state.is_host_hidden(&h.id));
                first.map(|h| h.id.clone())
            },
            open: Vec::new(),
            pill: SlidingPill::default(),
            show_hidden: false,
            keyboard: false,
            folds: HashMap::new(),
            moving: None,
            row_bounds: Rc::default(),
            shifts: HashMap::new(),
            lifted: HashSet::new(),
            state,
            focus: cx.focus_handle(),
            menu: None,
            _subscription: subscription,
        }
    }

    pub(crate) fn selected(&self) -> Option<&HostId> {
        self.selected.as_ref()
    }

    /// The hosts in sections, in the user's order: those without a group, then each group. They stay
    /// where they are whatever is open on them.
    fn sections(&self, cx: &App) -> Vec<(Option<String>, Vec<HostConfig>)> {
        let state = self.state.read(cx);
        let hosts: Vec<HostConfig> = state
            .ordered_hosts()
            .into_iter()
            .filter(|host| {
                self.show_hidden
                    || !state.is_host_hidden(&host.id)
                    || self.open.iter().any(|tab| tab.host == host.id)
            })
            .collect();
        std::iter::once(None)
            .chain(state.groups().into_iter().map(Some))
            .map(|group| {
                let members = hosts
                    .iter()
                    .filter(|host| state.host_group(&host.id).map(str::to_string) == group)
                    .cloned()
                    .collect::<Vec<_>>();
                (group, members)
            })
            .filter(|(_, members)| !members.is_empty())
            .collect()
    }

    /// Whether a host's row shows: a closed group shows only its header.
    fn shows_host(&self, group: Option<&str>, cx: &App) -> bool {
        let collapsed = &self.state.read(cx).settings.collapsed_groups;
        group.is_none_or(|g| !collapsed.contains(g))
    }

    /// The hosts the list shows right now, in that order.
    fn listed_hosts(&self, cx: &App) -> Vec<HostConfig> {
        self.sections(cx)
            .into_iter()
            .flat_map(|(group, hosts)| {
                hosts
                    .into_iter()
                    .filter(|_| self.shows_host(group.as_deref(), cx))
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// The mouse is about to open or close this: it glides, if the setting allows it.
    fn arm_fold(&mut self, group: &str, cx: &App) {
        if self.state.read(cx).settings.animated_selection
            && let Some(reveal) = self.folds.get_mut(group)
        {
            reveal.arm();
        }
    }

    /// How much of what folds under a group shows now, from 0 to 1.
    fn fold(&mut self, group: &str, open: bool) -> f32 {
        let reveal = self
            .folds
            .entry(group.to_string())
            .or_insert_with(|| Reveal::rows(open));
        reveal.set(open);
        reveal.value()
    }

    fn toggle_group(&mut self, group: &str, cx: &mut Context<Self>) {
        self.arm_fold(group, cx);
        self.state
            .update(cx, |state, cx| state.toggle_group_collapsed(group, cx));
        self.keep_selection_listed(cx);
        cx.notify();
    }

    /// The selection cannot stay on a row that is gone.
    fn keep_selection_listed(&mut self, cx: &mut Context<Self>) {
        let listed = self.listed_hosts(cx);
        if !listed.iter().any(|h| Some(&h.id) == self.selected.as_ref()) {
            self.selected = listed.first().map(|h| h.id.clone());
        }
    }

    /// `first` is the first row of the list: it has nothing above to keep away from.
    fn render_group_header(&self, group: &str, first: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let dragged = GroupDrag {
            name: group.to_string(),
        };
        let chip_color = theme.text_muted;
        let view = cx.entity().downgrade();
        let collapsed = self
            .state
            .read(cx)
            .settings
            .collapsed_groups
            .contains(group);
        let name = group.to_string();
        let menu_group = group.to_string();
        let brighter = theme.text_muted;
        let header = div()
            .id(SharedString::from(format!("group-{group}")))
            .on_drag(dragged, move |dragged, grabbed, _, cx| {
                view.update(cx, |this, cx| this.start_group_drag(&dragged.name, cx))
                    .ok();
                cx.new(|_| TabChip {
                    id: 0,
                    icon: IconName::Folder,
                    color: chip_color,
                    title: dragged.name.clone().into(),
                    grabbed,
                })
            })
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.))
            .h(px(LABEL_HEIGHT))
            .when(!first, |this| this.mt(px(LABEL_SPACE)))
            .pl(LABEL_INSET)
            .pr(px(8.))
            .rounded(CONTROL_RADIUS)
            .cursor_pointer()
            .text_size(crate::theme::ui_px(12.))
            .text_color(theme.text_disabled)
            .hover(move |style| style.text_color(brighter))
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_group(&name, cx)))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    let renamed = menu_group.clone();
                    let view = cx.entity().downgrade();
                    let entries = vec![
                        MenuEntry::item(
                            "Rename group",
                            handler(view, move |_, _, cx| {
                                cx.emit(SidebarEvent::RenameGroup(renamed.clone()))
                            }),
                        )
                        .icon(IconName::Pencil),
                    ];
                    this.menu = Some(OpenMenu::at(event.position, entries));
                    cx.notify();
                }),
            )
            .child(div().flex_1().min_w_0().truncate().child(group.to_string()))
            // No color of its own: it brightens with the title.
            .child(
                Icon::new(if collapsed {
                    IconName::ChevronRight
                } else {
                    IconName::ChevronDown
                })
                .size(px(12.)),
            );
        self.place_row(RowKey::Title(Some(group.to_string())), header)
            .into_any_element()
    }

    fn toggle_hidden(&mut self, host: &HostId, cx: &mut Context<Self>) {
        self.state
            .update(cx, |state, cx| state.toggle_host_hidden(host, cx));
        // With nothing left hidden there is no button to leave this mode.
        if self.state.read(cx).settings.hidden_hosts.is_empty() {
            self.show_hidden = false;
        }
        self.keep_selection_listed(cx);
        cx.notify();
    }

    /// To the session of a host used last, or a terminal on it when it has none.
    fn go_to_host(&mut self, host: &HostId, cx: &mut Context<Self>) {
        cx.emit(SidebarEvent::GoToHost(host.clone()));
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let hosts = self.listed_hosts(cx);
        let index = self
            .selected
            .as_ref()
            .and_then(|s| hosts.iter().position(|h| &h.id == s));
        if event.keystroke.key == "escape" && self.menu.is_some() {
            self.menu = None;
            cx.notify();
            cx.stop_propagation();
            return;
        }
        let next = match (event.keystroke.key.as_str(), index) {
            ("enter", Some(i)) => {
                self.go_to_host(&hosts[i].id, cx);
                cx.stop_propagation();
                return;
            }
            ("down", Some(i)) => hosts.get(i + 1),
            ("down", None) => hosts.first(),
            ("up", Some(i)) if i > 0 => hosts.get(i - 1),
            _ => return,
        };
        if let Some(host) = next {
            self.selected = Some(host.id.clone());
            self.keyboard = true;
            cx.notify();
        }
        cx.stop_propagation();
    }

    fn host_menu(&mut self, host: &HostConfig, position: Point<Pixels>, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        let state = self.state.read(cx);
        let active = state
            .host_session_id(&host.id)
            .is_some_and(|id| !state.session_gone(id));
        let emit = |event: fn(HostId) -> SidebarEvent, id: HostId| {
            handler(this.clone(), move |_, _, cx| cx.emit(event(id.clone())))
        };
        let id = host.id.clone();
        let mut entries = vec![
            MenuEntry::item(
                "Open terminal",
                emit(SidebarEvent::OpenTerminal, id.clone()),
            )
            .icon(IconName::Terminal),
            MenuEntry::item("Browse files", emit(SidebarEvent::OpenFiles, id.clone()))
                .icon(IconName::Folder),
            MenuEntry::Separator,
        ];
        if !active {
            entries.push(
                MenuEntry::item("Connect", emit(SidebarEvent::Connect, id.clone()))
                    .icon(IconName::Power),
            );
        }
        let hidden = state.is_host_hidden(&host.id);
        let hide_id = host.id.clone();
        entries.push(
            MenuEntry::item(
                if hidden {
                    "Show in sidebar"
                } else {
                    "Hide from sidebar"
                },
                handler(this.clone(), move |view, _, cx| {
                    view.toggle_hidden(&hide_id, cx)
                }),
            )
            .icon(if hidden {
                IconName::Eye
            } else {
                IconName::EyeOff
            }),
        );
        entries.push(MenuEntry::Separator);
        let current = state.host_group(&host.id).map(str::to_string);
        let mut places = Vec::new();
        for group in state.groups() {
            if Some(&group) == current.as_ref() {
                continue;
            }
            let target = host.id.clone();
            places.push(
                MenuEntry::item(
                    group.clone(),
                    handler(this.clone(), move |view, _, cx| {
                        view.state.update(cx, |state, cx| {
                            state.set_host_group(&target, Some(group.clone()), cx)
                        });
                    }),
                )
                .icon(IconName::Folder),
            );
        }
        if current.is_some() {
            let target = host.id.clone();
            places.push(
                MenuEntry::item(
                    "Ungrouped",
                    handler(this.clone(), move |view, _, cx| {
                        view.state
                            .update(cx, |state, cx| state.set_host_group(&target, None, cx));
                        view.keep_selection_listed(cx);
                    }),
                )
                .icon(IconName::FolderUp),
            );
        }
        if !places.is_empty() {
            places.push(MenuEntry::Separator);
        }
        places.push(
            MenuEntry::item("New group", emit(SidebarEvent::NewGroup, id.clone()))
                .icon(IconName::FolderPlus),
        );
        entries.push(MenuEntry::submenu("Move to", places).icon(IconName::Folder));
        entries.push(
            MenuEntry::item("Duplicate", emit(SidebarEvent::DuplicateHost, id.clone()))
                .icon(IconName::Copy),
        );
        entries.push(
            MenuEntry::item("Host settings", emit(SidebarEvent::HostSettings, id))
                .icon(IconName::Settings),
        );
        if active {
            entries.push(MenuEntry::Separator);
            entries.push(
                MenuEntry::item(
                    "Disconnect",
                    emit(SidebarEvent::Disconnect, host.id.clone()),
                )
                .icon(IconName::Power),
            );
        }
        self.menu = Some(OpenMenu::at(position, entries));
        cx.notify();
    }

    /// What a host needs from the user, and its color. A failure on the way to trying again is
    /// not one yet.
    fn host_alert(&self, id: &HostId, cx: &App) -> Option<(&'static str, Hsla)> {
        let theme = cx.theme();
        let state = self.state.read(cx);
        match state.host_session(id).map(|s| &s.state) {
            Some(SessionState::Failed { .. }) if state.reconnect_attempt(id).is_none() => {
                Some(("failed", theme.error))
            }
            Some(
                SessionState::AwaitingHostKey { .. }
                | SessionState::AwaitingPassphrase { .. }
                | SessionState::AwaitingPassword { .. }
                | SessionState::AwaitingAnswer { .. },
            ) => Some(("needs you", theme.warning)),
            _ => None,
        }
    }

    fn status_text(&self, id: &HostId, cx: &App) -> String {
        let state = self.state.read(cx);
        if let Some(attempt) = state.reconnect_attempt(id) {
            return format!("Reconnecting (attempt {attempt})");
        }
        match state.host_session(id).map(|s| &s.state) {
            Some(SessionState::Connected) => "Connected".to_string(),
            Some(SessionState::Connecting { .. }) => "Connecting".to_string(),
            Some(SessionState::Failed { error }) => format!("Failed: {}", error.message),
            Some(SessionState::Disconnected { reason: Some(r) }) => format!("Disconnected: {r}"),
            Some(SessionState::Disconnected { reason: None }) | None => "Not connected".into(),
            Some(_) => "Waiting for your answer".into(),
        }
    }

    /// A host's row, at its place in the list. A click goes to its first session, or opens one.
    /// `ends_section` is the last host of its section: a group dropped past it lands under it.
    fn render_host(&self, host: &HostConfig, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let state = self.state.read(cx);
        let id = host.id.clone();
        let hidden = state.is_host_hidden(&id);
        let has_tabs = self.open.iter().any(|tab| tab.host == id);
        let focused = self.selected.as_ref() == Some(&id) && self.keyboard;
        let source = if host.source == HostSource::SshConfig {
            " · ssh"
        } else {
            ""
        };
        let route = host.proxy_jump.as_ref().map(|via| format!("via {via}"));
        let status = self.status_text(&id, cx);
        let text_note = |text: String, color: Hsla| {
            div()
                .flex_none()
                .text_size(crate::theme::ui_px(12.))
                .text_color(color)
                .child(text)
        };
        // What needs the user comes first, then the way there, which the tooltip says too.
        let note = match (self.host_alert(&id, cx), &route) {
            (Some((text, color)), _) => Some(text_note(text.to_string(), color)),
            (None, Some(route)) => Some(text_note(route.clone(), theme.text_disabled)),
            (None, None) => None,
        };
        // Bright while connected, dim otherwise; what needs the user says so in its note.
        let connected = matches!(
            state.host_session(&id).map(|session| &session.state),
            Some(SessionState::Connected)
        );
        let dragged = HostDrag {
            id: id.clone(),
            alias: host.alias.clone(),
        };
        let (click, add) = (id.clone(), id.clone());
        let group = SharedString::from(format!("host-row-{}", id.0));
        let menu = host.clone();
        let hover = theme.hover;
        let chip_color = theme.host_color(state.host_color(&id));
        let view = cx.entity().downgrade();
        let row = div()
            .id(SharedString::from(format!("host-row-{}", id.0)))
            .group(group.clone())
            .relative()
            // The chip shows the host alone; its sessions come along.
            .on_drag(dragged, move |dragged, grabbed, _, cx| {
                view.update(cx, |this, cx| this.start_host_drag(&dragged.id, cx))
                    .ok();
                cx.new(|_| TabChip {
                    id: 0,
                    icon: IconName::Server,
                    color: chip_color,
                    title: dragged.alias.clone().into(),
                    grabbed,
                })
            })
            .flex()
            .flex_none()
            .items_center()
            .gap(px(11.))
            .w_full()
            .h(row_px(HOST_HEIGHT))
            .pl(LABEL_INSET)
            .pr(px(6.))
            .rounded(CONTROL_RADIUS)
            .cursor_pointer()
            .when(hidden, |this| this.opacity(0.55))
            .map(|this| {
                if focused {
                    this.bg(hover)
                } else {
                    this.hover(move |style| style.bg(hover))
                }
            })
            .tooltip(tooltip(format!(
                "{}{}{source}\n{status}",
                host.target(),
                route.map_or(String::new(), |route| format!(" {route}")),
            )))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.keyboard = false;
                this.selected = Some(click.clone());
                this.pill.arm(&this.state.read(cx).settings);
                this.go_to_host(&click, cx);
                cx.notify();
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.selected = Some(menu.id.clone());
                    window.focus(&this.focus);
                    this.host_menu(&menu, event.position, cx);
                }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(if connected {
                        theme.text
                    } else {
                        theme.text_muted
                    })
                    .when(connected && has_tabs, |this| {
                        this.font_weight(gpui::FontWeight::MEDIUM)
                    })
                    .child(host.alias.clone()),
            )
            // Holds the note, or at least the room of the +, which takes its place under the
            // pointer: a long name stays clear of it.
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_end()
                    .h_full()
                    .min_w(px(24.))
                    .children(note.map(|note| note.group_hover(group.clone(), Styled::invisible)))
                    .child(
                        div()
                            .id(SharedString::from(format!("host-add-{}", id.0)))
                            // Not occluding: the pointer must stay "on the row" here, or the button would
                            // hide itself. So it keeps its own clicks.
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .absolute()
                            // Its + where the note ends: the button is wider than what it shows.
                            .right(px(-3.))
                            .top_0()
                            .bottom_0()
                            .flex()
                            .items_center()
                            .invisible()
                            .group_hover(group, Styled::visible)
                            .child(
                                Button::new(SharedString::from(format!("new-tab-{}", id.0)))
                                    .icon(IconName::Plus)
                                    .tooltip("New terminal (Ctrl+Shift+T)")
                                    .style(ButtonStyle::Ghost)
                                    .compact()
                                    .on_click(cx.listener(move |_, _, _, cx| {
                                        cx.stop_propagation();
                                        cx.emit(SidebarEvent::OpenTerminal(add.clone()))
                                    })),
                            ),
                    ),
            );
        self.place_row(RowKey::Host(id), row).into_any_element()
    }

    /// A session: its tile and where it is, under its host and moved among its tabs. `with_host`:
    /// it is not under its host, gone from the config, so the row names it.
    fn render_session(&self, tab: &OpenTab, with_host: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let state = self.state.read(cx);
        // A tab can outlive its host, removed from the config meanwhile.
        let host = state.alias(&tab.host);
        let color = theme.host_color(state.host_color(&tab.host));
        let user = state
            .host(&tab.host)
            .map(|host| host.user.clone())
            .unwrap_or_default();
        let shell = state
            .host_session(&tab.host)
            .and_then(|session| session.login_shell.clone());
        let id = tab.id;
        let group = SharedString::from(format!("sidebar-tab-{id}"));
        let (hover, press, text) = (theme.hover, theme.active, theme.text);
        // A tab without a name or a known folder still has its title.
        let (place, in_mono) = match (&tab.name, &tab.folder) {
            (Some(name), _) => (name.clone(), false),
            (None, Some(folder)) => (folder.clone(), true),
            (None, None) => (tab.title.clone(), false),
        };
        let chip = TabChip {
            id,
            icon: tab.face.icon(),
            color,
            title: place.clone().into(),
            grabbed: Point::default(),
        };
        let moving = self.moving.is_some();
        let view = cx.entity().downgrade();
        let row = div()
            .id(("sidebar-tab", id as usize))
            .group(group.clone())
            .relative()
            // The pill stays put during a drag: the current row carries its own fill.
            .when(moving && tab.active, |this| this.bg(press))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(10.))
            .w_full()
            .h(row_px(SESSION_HEIGHT))
            .pl(if with_host { px(5.) } else { SESSION_INSET })
            .pr(px(6.))
            .rounded(CONTROL_RADIUS)
            .cursor_pointer()
            .text_color(if tab.active {
                theme.text
            } else {
                theme.text_muted
            })
            // The current one sits on the pill.
            .when(!tab.active, |this| {
                this.hover(move |style| style.bg(hover).text_color(text))
                    .active(move |style| style.bg(press))
            })
            .tooltip(tooltip(format!("{}\n{}", tab.detail, tab.face.label())))
            .on_drag(chip, move |chip, grabbed, _, cx| {
                view.update(cx, |this, cx| this.start_tab_drag(chip.id, cx))
                    .ok();
                cx.new(|_| TabChip {
                    grabbed,
                    ..chip.clone()
                })
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                this.pill.arm(&this.state.read(cx).settings);
                cx.emit(SidebarEvent::ActivateTab(id));
            }))
            .on_mouse_up(
                MouseButton::Middle,
                cx.listener(move |_, _, _, cx| cx.emit(SidebarEvent::CloseTab(id))),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |_, event: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    cx.emit(SidebarEvent::TabMenu(id, event.position));
                }),
            )
            // The prompt for terminals, the folder for a tab that shows its files.
            .child(if tab.face.shows_files() {
                tile(Mark::Icon(tab.face.icon()), color, None, &theme).into_any_element()
            } else {
                shell_tile(
                    ("session-tile", id as usize),
                    shell.as_deref(),
                    &user,
                    color,
                    tab.split.then(|| theme.row_fill(tab.active)),
                    &theme,
                )
            })
            .when(with_host, |this| {
                this.child(
                    div()
                        .relative()
                        .top(-text_lift(
                            crate::theme::ui_font(),
                            crate::theme::ui_px(13.),
                            cx,
                        ))
                        .min_w_0()
                        .truncate()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .child(host),
                )
            })
            .child(
                div()
                    .relative()
                    // A path is tall marks, a slash and descenders: its line is already centred
                    // on the tile, and a lift would put it above.
                    .top(-if in_mono {
                        px(0.)
                    } else {
                        text_lift(crate::theme::ui_font(), crate::theme::ui_px(13.), cx)
                    })
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .when(with_host, |this| this.text_color(theme.text_disabled))
                    .when(in_mono, |this| {
                        this.font_family(crate::theme::mono_font())
                            .text_size(crate::theme::ui_px(12.))
                    })
                    .child(place),
            )
            .child(
                div()
                    .id(("sidebar-tab-close", id as usize))
                    .size(px(20.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(CONTROL_RADIUS)
                    .text_color(theme.text_muted)
                    .when(!tab.active, |this| {
                        this.invisible().group_hover(group, Styled::visible)
                    })
                    .hover(move |style| style.bg(press).text_color(text))
                    .tooltip(tooltip("Close (Ctrl+Shift+W, middle-click)"))
                    // A press here stays a press, not the start of a drag of the row.
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.stop_propagation();
                        cx.emit(SidebarEvent::CloseTab(id));
                    }))
                    .child(Icon::new(IconName::Close).size(px(12.))),
            );
        self.place_row(RowKey::Session(id), row).into_any_element()
    }
}

impl Sidebar {
    /// Each group with its hosts, and under each host its open sessions in the order of the tabs.
    /// Sessions whose host is gone from the config come last. With the place of the active
    /// session among the children of the list.
    fn list_rows(
        &mut self,
        sections: &[(Option<String>, Vec<HostConfig>)],
        open: &[OpenTab],
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> (Vec<AnyElement>, Option<usize>) {
        let mut rows: Vec<AnyElement> = Vec::new();
        let mut current_child = None;
        for (group, hosts) in sections {
            let first = rows.is_empty();
            match group {
                Some(group) => rows.push(self.render_group_header(group, first, cx)),
                None => rows.push(
                    self.place_row(RowKey::Title(None), section_label("Hosts", first, theme))
                        .into_any_element(),
                ),
            }
            // Closing, what folds is still there until it is gone.
            let shows_hosts = self.shows_host(group.as_deref(), cx);
            let group_open = match group {
                Some(group) => self.fold(group, shows_hosts),
                None => 1.,
            };
            if group_open == 0. {
                continue;
            }
            for host in hosts {
                let row = self.render_host(host, cx);
                rows.push(folding(row, row_px(HOST_HEIGHT), group_open).into_any_element());
                for tab in open.iter().filter(|tab| tab.host == host.id) {
                    // Among the children of the list, the pill comes first.
                    if tab.active {
                        current_child = Some(rows.len() + 1);
                    }
                    let row = self.render_session(tab, false, cx);
                    rows.push(folding(row, row_px(SESSION_HEIGHT), group_open).into_any_element());
                }
            }
        }
        let listed: HashSet<&HostId> = sections
            .iter()
            .flat_map(|(_, hosts)| hosts.iter().map(|host| &host.id))
            .collect();
        let others: Vec<&OpenTab> = open
            .iter()
            .filter(|tab| !listed.contains(&tab.host))
            .collect();
        if !others.is_empty() {
            let first = rows.is_empty();
            rows.push(section_label("Other sessions", first, theme).into_any_element());
            for tab in others {
                if tab.active {
                    current_child = Some(rows.len() + 1);
                }
                rows.push(self.render_session(tab, true, cx));
            }
        }
        (rows, current_child)
    }

    /// The scrolling list of sessions and hosts, where rows are moved.
    fn host_list(
        &self,
        rows: Vec<AnyElement>,
        current_child: Option<usize>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        div()
            .on_children_prepainted(self.pill.measure(current_child))
            .id("host-list")
            .relative()
            .flex_1()
            .min_h_0()
            .pt(ROW_GAP)
            .mb(px(8.) - ROW_GAP)
            .pb(ROW_GAP)
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(ROW_GAP)
            // Whatever moves, the pointer over the list says where it would go.
            .on_drag_move(cx.listener(|this, event: &DragMoveEvent<TabChip>, _, cx| {
                this.follow_pointer(event.event.position, event.bounds, cx)
            }))
            .on_drag_move(cx.listener(|this, event: &DragMoveEvent<HostDrag>, _, cx| {
                this.follow_pointer(event.event.position, event.bounds, cx)
            }))
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<GroupDrag>, _, cx| {
                    this.follow_pointer(event.event.position, event.bounds, cx)
                }),
            )
            .child(self.pill.neutral(theme, crate::theme::RADIUS_CONTROL))
            .children(rows)
            .when(self.moving.is_some(), |this| {
                let view = cx.entity().downgrade();
                this.child(on_release(move |_, cx| {
                    view.update(cx, |this, cx| this.finish_moving(cx)).ok();
                }))
            })
    }

    fn hidden_toggle(
        theme: &Theme,
        hidden: usize,
        show_hidden: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let raised = theme.track;
        div()
            .id("toggle-hidden-hosts")
            .flex()
            .flex_none()
            .items_center()
            .h(px(26.))
            .mt(px(6.))
            .pl(LABEL_INSET)
            .pr(px(8.))
            .rounded(CONTROL_RADIUS)
            .cursor_pointer()
            .text_size(crate::theme::ui_px(12.))
            .text_color(theme.text_disabled)
            .hover(move |style| style.bg(raised))
            .on_click(cx.listener(|this, _, _, cx| {
                this.show_hidden = !this.show_hidden;
                this.keep_selection_listed(cx);
                cx.notify();
            }))
            .child(if show_hidden {
                "Stop listing hidden hosts".to_string()
            } else if hidden == 1 {
                "1 hidden host".to_string()
            } else {
                format!("{hidden} hidden hosts")
            })
    }
}

impl Render for Sidebar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // A drag let go anywhere ends here too, with or without a drop.
        if !cx.has_active_drag() {
            self.moving = None;
        }
        if self.moving.as_ref().is_some_and(Moving::gliding) {
            window.request_animation_frame();
        }
        self.measure_moves(cx);
        let theme = cx.theme().clone();
        let state = self.state.read(cx);
        let width = state.settings.sidebar_width;
        let all_hosts = state.hosts.len();
        let hidden = state
            .hosts
            .iter()
            .filter(|host| state.is_host_hidden(&host.id))
            .count();
        let sections = self.sections(cx);
        let open = self.open.clone();
        let hosts_empty = sections.is_empty() && open.is_empty();
        let show_hidden = self.show_hidden;
        let (rows, current_child) = self.list_rows(&sections, &open, &theme, cx);
        // The pill stays put while a session moves: the rows carry their own fill meanwhile.
        let current_child = current_child.filter(|_| self.moving.is_none());
        if self.folds.values().any(Reveal::moving) {
            window.request_animation_frame();
        }
        div()
            .id("sidebar")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key_down))
            .w(px(width))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .pl(px(10.))
            .pr(px(2.))
            .text_size(crate::theme::ui_px(13.))
            .child(
                self.host_list(rows, current_child, &theme, cx)
                    .when(hosts_empty, |this| {
                        this.child(empty_hint(&theme, all_hosts))
                    })
                    .when(hidden > 0, |this| {
                        this.child(Self::hidden_toggle(&theme, hidden, show_hidden, cx))
                    }),
            )
            .children(self.menu.clone().map(|menu| {
                let this = cx.entity().downgrade();
                ContextMenu::new(
                    "host-menu",
                    menu,
                    crate::widgets::context_menu::dismiss(this, |view| &mut view.menu),
                )
            }))
    }
}

/// Says why the list is empty.
fn empty_hint(theme: &Theme, all_hosts: usize) -> impl IntoElement + use<> {
    div()
        .p(px(8.))
        .text_color(theme.text_muted)
        .child(if all_hosts == 0 {
            "No hosts. Add one with the button below, or define Host entries in ~/.ssh/config."
        } else {
            "All hosts are hidden."
        })
}
