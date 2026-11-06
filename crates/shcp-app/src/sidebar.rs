//! Left column: app name, the hosts in the user's order with their tabs under them, settings and
//! the state of an update.
use crate::motion;
use crate::state::AppState;
use crate::tabs::Face;
use crate::theme::{ActiveTheme as _, row_px};
use crate::title_bar;
use crate::updater::UpdateStatus;
use crate::widgets::button::{Button, ButtonStyle};
use crate::widgets::context_menu::{ContextMenu, MenuEntry, OpenMenu, handler};
use crate::widgets::host_tile::{HostLook, HostTile, TileState};
use crate::widgets::icon::{Icon, IconName};
use crate::widgets::pill::SlidingPill;
use crate::widgets::progress::ProgressRing;
use crate::widgets::status::badge;
use crate::widgets::tooltip::tooltip;
use gpui::prelude::*;
use gpui::{
    AnyElement, App, Bounds, ClickEvent, ClipboardItem, Context, Div, DragMoveEvent, Entity,
    EventEmitter, FocusHandle, Focusable, Hsla, KeyDownEvent, MouseButton, MouseDownEvent, Pixels,
    Point, SharedString, Stateful, Subscription, Window, WindowControlArea, div, px,
};
use shcp_core::{HostConfig, HostId, HostSource, SessionState};
use std::rc::Rc;

pub enum SidebarEvent {
    OpenTerminal(HostId),
    OpenFiles(HostId),
    Disconnect(HostId),
    EditHost(HostId),
    DuplicateHost(HostId),
    CustomizeHost(HostId),
    EditCommands(HostId),
    NewGroup(HostId),
    RenameGroup(String),
    Connect(HostId),
    SetUpKey(HostId),
    OpenSshConfig,
    NewHost,
    OpenSettings,
    HideSidebar,
    RestartForUpdate,
    /// A new terminal on a host.
    NewTab(HostId),
    ActivateTab(u64),
    CloseTab(u64),
    TabMenu(u64, Point<Pixels>),
    /// Puts a tab where another one is.
    /// A tab goes right before another one of its host, or right after it with `true`.
    MoveTab(u64, u64, bool),
}

/// What the sidebar needs to know of an open tab. The workspace owns the tabs.
#[derive(Clone, PartialEq)]
pub struct OpenTab {
    pub id: u64,
    pub host: HostId,
    pub title: String,
    /// The name the user gave it, instead of what it shows and where.
    pub name: Option<String>,
    /// Where it is, shortened for the row.
    pub folder: Option<String>,
    pub detail: String,
    pub face: Face,
    pub active: bool,
}

/// A tab on its way to another place among those of its host.
#[derive(Clone)]
struct TabDrag {
    id: u64,
    host: HostId,
    title: String,
    look: HostLook,
}

impl Render for TabDrag {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .h(px(30.))
            .px(px(10.))
            .flex()
            .items_center()
            .gap(px(8.))
            .rounded(px(8.))
            .border_1()
            .border_color(theme.strong_border)
            .bg(theme.elevated)
            .shadow(crate::theme::float_shadow())
            .font_family(crate::theme::ui_font())
            .text_size(crate::theme::ui_px(13.))
            .text_color(theme.text)
            .child(HostTile::new(self.look.clone()).small())
            .child(self.title.clone())
    }
}

fn section_label(text: &'static str, theme: &crate::theme::Theme) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .h(px(28.))
        .pl(px(6.))
        .text_size(crate::theme::ui_px(13.))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(theme.text_disabled)
        .child(text)
}

/// A host on its way to a group, and what follows the pointer meanwhile.
#[derive(Clone)]
struct HostDrag {
    id: HostId,
    alias: String,
}

/// A group on its way to another place.
#[derive(Clone)]
struct GroupDrag {
    name: String,
}

/// The row a drag would land next to.
#[derive(Clone, PartialEq)]
enum DropRow {
    Host(HostId),
    Tab(u64),
    Group(String),
}

/// What can be dragged along the list: each comes from a row of its own.
trait Dragged: 'static {
    fn row(&self) -> DropRow;
}

impl Dragged for HostDrag {
    fn row(&self) -> DropRow {
        DropRow::Host(self.id.clone())
    }
}

impl Dragged for TabDrag {
    fn row(&self) -> DropRow {
        DropRow::Tab(self.id)
    }
}

impl Dragged for GroupDrag {
    fn row(&self) -> DropRow {
        DropRow::Group(self.name.clone())
    }
}

/// Where a drag would land if let go now.
#[derive(Clone, PartialEq)]
struct DropLine {
    row: DropRow,
    after: bool,
    /// The row under the pointer: the line stays while the pointer does.
    over: Bounds<Pixels>,
}

/// The space between the rows of the list, where the line goes.
const ROW_GAP: Pixels = px(2.);

/// A straight line in the gap above or below a row.
fn insertion_line(after: bool, color: Hsla) -> Div {
    div()
        .absolute()
        .left_0()
        .right_0()
        .h(ROW_GAP)
        .map(|this| {
            if after {
                this.bottom(-ROW_GAP)
            } else {
                this.top(-ROW_GAP)
            }
        })
        .bg(color)
}

impl Render for GroupDrag {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .px(px(10.))
            .py(px(4.))
            .rounded(px(7.))
            .border_1()
            .border_color(theme.border)
            .bg(theme.elevated)
            .shadow(crate::theme::float_shadow())
            .font_family(crate::theme::ui_font())
            .text_size(crate::theme::ui_px(13.))
            .text_color(theme.text)
            .child(self.name.clone())
    }
}

impl Render for HostDrag {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .px(px(10.))
            .py(px(4.))
            .rounded(px(7.))
            .border_1()
            .border_color(theme.border)
            .bg(theme.elevated)
            .shadow(crate::theme::float_shadow())
            .text_size(crate::theme::ui_px(13.))
            .text_color(theme.text)
            .child(self.alias.clone())
    }
}

pub struct Sidebar {
    state: Entity<AppState>,
    selected: Option<HostId>,
    pub(crate) open: Vec<OpenTab>,
    pill: SlidingPill,
    /// List the hidden hosts too, dimmed, to show them again.
    show_hidden: bool,
    /// The host whose + is under the pointer: that row does not light up too.
    over_add: Option<HostId>,
    /// The arrow keys are moving the selection, so it shows.
    keyboard: bool,
    /// The host whose terminal the last click opened.
    opened_by_click: Option<HostId>,
    /// What the mouse last opened: its rows come in instead of being there at once.
    opening_host: Option<HostId>,
    opening_group: Option<String>,
    drop_line: Option<DropLine>,
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
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
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
            over_add: None,
            keyboard: false,
            opened_by_click: None,
            opening_host: None,
            opening_group: None,
            drop_line: None,
            state,
            focus: cx.focus_handle(),
            menu: None,
            _subscription: subscription,
        }
    }

    pub fn selected(&self) -> Option<&HostId> {
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
    fn shows_host(&self, group: &Option<String>, cx: &App) -> bool {
        let collapsed = &self.state.read(cx).settings.collapsed_groups;
        group.as_ref().is_none_or(|g| !collapsed.contains(g))
    }

    /// The hosts the list shows right now, in that order.
    fn listed_hosts(&self, cx: &App) -> Vec<HostConfig> {
        self.sections(cx)
            .into_iter()
            .flat_map(|(group, hosts)| {
                hosts
                    .into_iter()
                    .filter(|_| self.shows_host(&group, cx))
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    fn toggle_group(&mut self, group: &str, cx: &mut Context<Self>) {
        let settings = &self.state.read(cx).settings;
        self.opening_group = (settings.animated_selection
            && settings.collapsed_groups.contains(group))
        .then(|| group.to_string());
        self.state
            .update(cx, |state, cx| state.toggle_group_collapsed(group, cx));
        // The selection cannot stay on a row that is gone.
        let listed = self.listed_hosts(cx);
        if !listed.iter().any(|h| Some(&h.id) == self.selected.as_ref()) {
            self.selected = listed.first().map(|h| h.id.clone());
        }
        cx.notify();
    }

    /// Whether a row let go on this side of `target` would stay exactly where it is on screen: right
    /// next to itself, or on itself. A move between sections always changes something.
    fn lands_where_it_is(
        &self,
        dragged: &DropRow,
        target: &DropRow,
        after: bool,
        cx: &App,
    ) -> bool {
        let siblings: Vec<DropRow> = match target {
            DropRow::Host(id) => self
                .sections(cx)
                .into_iter()
                .map(|(_, hosts)| hosts)
                .find(|hosts| hosts.iter().any(|host| host.id == *id))
                .unwrap_or_default()
                .into_iter()
                .map(|host| DropRow::Host(host.id))
                .collect(),
            DropRow::Tab(id) => {
                let host = self
                    .open
                    .iter()
                    .find(|tab| tab.id == *id)
                    .map(|tab| &tab.host);
                self.open
                    .iter()
                    .filter(|tab| Some(&tab.host) == host)
                    .map(|tab| DropRow::Tab(tab.id))
                    .collect()
            }
            DropRow::Group(_) => self
                .sections(cx)
                .into_iter()
                .filter_map(|(group, _)| group.map(DropRow::Group))
                .collect(),
        };
        let at = |row: &DropRow| siblings.iter().position(|sibling| sibling == row);
        let (Some(from), Some(to)) = (at(dragged), at(target)) else {
            return false;
        };
        let place = to + usize::from(after);
        place == from || place == from + 1
    }

    /// Moves the line to this row while a drag of `T` is over it. `side` gets what is dragged and
    /// whether the pointer is on the lower half, and says where it would land, if anywhere.
    fn track_row<T: Dragged>(
        element: Stateful<Div>,
        side: impl Fn(&T, bool) -> Option<(DropRow, bool)> + 'static,
        cx: &Context<Self>,
    ) -> Stateful<Div> {
        element.on_drag_move(cx.listener(move |this, event: &DragMoveEvent<T>, _, cx| {
            let pointer = event.event.position;
            // Half the gap each: no spot between two rows is without a line.
            if !event.bounds.dilate(ROW_GAP / 2.).contains(&pointer) {
                return;
            }
            let lower = pointer.y > event.bounds.center().y;
            let dragged = event.drag(cx);
            let line = side(dragged, lower)
                .filter(|(row, after)| !this.lands_where_it_is(&dragged.row(), row, *after, cx))
                .map(|(row, after)| {
                    let (row, after) = this.one_spot_between_groups(row, after, cx);
                    DropLine {
                        row,
                        after,
                        over: event.bounds,
                    }
                });
            if this.drop_line != line {
                this.drop_line = line;
                cx.notify();
            }
        }))
    }

    /// Past a group and before the next one are the same place: it gets one line, above the next.
    fn one_spot_between_groups(&self, row: DropRow, after: bool, cx: &App) -> (DropRow, bool) {
        let next = match &row {
            DropRow::Group(name) if after => {
                let sections = self.sections(cx);
                let mut groups = sections.iter().filter_map(|(group, _)| group.as_ref());
                groups.find(|group| *group == name);
                groups.next().cloned()
            }
            _ => None,
        };
        match next {
            Some(next) => (DropRow::Group(next), false),
            None => (row, after),
        }
    }

    /// The line goes once the pointer is off its row, and not onto another one.
    fn leave_row<T: 'static>(
        &mut self,
        event: &DragMoveEvent<T>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let pointer = event.event.position;
        let left = self
            .drop_line
            .as_ref()
            .is_some_and(|line| !line.over.dilate(ROW_GAP / 2.).contains(&pointer));
        if left {
            self.drop_line = None;
            cx.notify();
        }
    }

    /// A group dragged over a row of another one lands past that one, whichever row it is.
    fn track_past_group(
        &self,
        element: Stateful<Div>,
        host: &HostId,
        cx: &Context<Self>,
    ) -> Stateful<Div> {
        let state = self.state.read(cx);
        let group = state
            .host(host)
            .and_then(|_| state.host_group(host))
            .map(str::to_string);
        Self::track_row(
            element,
            move |_: &GroupDrag, _| group.clone().map(|group| (DropRow::Group(group), true)),
            cx,
        )
    }

    /// Whether the line past the group of this host is up, for the last row of that group.
    fn line_past_group(&self, host: &HostId, cx: &App) -> Option<bool> {
        let group = self.state.read(cx).host_group(host)?.to_string();
        self.line_on(&DropRow::Group(group)).filter(|after| *after)
    }

    /// Whether the line is on this row and, if so, on which side.
    fn line_on(&self, row: &DropRow) -> Option<bool> {
        let line = self.drop_line.as_ref()?;
        (line.row == *row).then_some(line.after)
    }

    /// The selection cannot stay on a row that is gone.
    fn keep_selection_listed(&mut self, cx: &mut Context<Self>) {
        let listed = self.listed_hosts(cx);
        if !listed.iter().any(|h| Some(&h.id) == self.selected.as_ref()) {
            self.selected = listed.first().map(|h| h.id.clone());
        }
    }

    fn render_group_header(&self, group: &str, count: usize, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let accent = theme.accent;
        let dragged = GroupDrag {
            name: group.to_string(),
        };
        let target = group.to_string();
        let collapsed = self
            .state
            .read(cx)
            .settings
            .collapsed_groups
            .contains(group);
        // Past an open group is under its last row.
        let line = self
            .line_on(&DropRow::Group(target.clone()))
            .filter(|after| !after || collapsed);
        let name = group.to_string();
        let menu_group = group.to_string();
        let brighter = theme.text_muted;
        div()
            .id(SharedString::from(format!("group-{group}")))
            .on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
            .map(|this| {
                Self::track_row(
                    this,
                    // An open group goes on past its title: the rows under it take that side.
                    move |_: &GroupDrag, lower| {
                        Some((DropRow::Group(target.clone()), lower && collapsed))
                    },
                    cx,
                )
            })
            .relative()
            .children(line.map(|after| insertion_line(after, accent)))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(5.))
            .h(px(26.))
            .mt(px(10.))
            .px(px(6.))
            .rounded(px(6.))
            .cursor_pointer()
            .text_size(crate::theme::ui_px(13.))
            .font_weight(gpui::FontWeight::MEDIUM)
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
            .child(
                Icon::new(if collapsed {
                    IconName::ChevronRight
                } else {
                    IconName::ChevronDown
                })
                .size(px(12.)),
            )
            .child(div().flex_1().min_w_0().truncate().child(group.to_string()))
            .child(
                div()
                    .flex_none()
                    .font_family(crate::theme::mono_font())
                    .font_weight(gpui::FontWeight::NORMAL)
                    .text_size(crate::theme::ui_px(12.))
                    .child(count.to_string()),
            )
            .into_any_element()
    }

    fn toggle_hidden(&mut self, host: &HostId, cx: &mut Context<Self>) {
        self.state
            .update(cx, |state, cx| state.toggle_host_hidden(host, cx));
        // With nothing left hidden there is no button to leave this mode.
        if self.state.read(cx).settings.hidden_hosts.is_empty() {
            self.show_hidden = false;
        }
        let listed = self.listed_hosts(cx);
        if !listed.iter().any(|h| Some(&h.id) == self.selected.as_ref()) {
            self.selected = listed.first().map(|h| h.id.clone());
        }
        cx.notify();
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
                cx.emit(SidebarEvent::OpenTerminal(hosts[i].id.clone()));
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
        let active = state.host_session(&host.id).is_some_and(|s| {
            !matches!(
                s.state,
                SessionState::Disconnected { .. } | SessionState::Failed { .. }
            )
        });
        let emit = |event: fn(HostId) -> SidebarEvent, id: HostId| {
            handler(this.clone(), move |_, _, cx| cx.emit(event(id.clone())))
        };
        let id = host.id.clone();
        let target = format!("{}@{}", host.user, host.hostname);
        let mut entries = vec![
            MenuEntry::item(
                "Open terminal",
                emit(SidebarEvent::OpenTerminal, id.clone()),
            )
            .icon(IconName::Terminal)
            .shortcut("Enter"),
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
        entries.push(
            MenuEntry::item(
                "Copy user@host",
                Rc::new(move |_, cx: &mut App| {
                    cx.write_to_clipboard(ClipboardItem::new_string(target.clone()))
                }),
            )
            .icon(IconName::Copy),
        );
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
        entries.push(MenuEntry::Separator);
        entries.push(
            MenuEntry::item("Commands", emit(SidebarEvent::EditCommands, id.clone()))
                .icon(IconName::Play),
        );
        entries.push(
            MenuEntry::item("Customize", emit(SidebarEvent::CustomizeHost, id.clone()))
                .icon(IconName::Palette),
        );
        let logging = state.logs_sessions(&host.id);
        let log_id = host.id.clone();
        entries.push(
            MenuEntry::item(
                if logging {
                    "Stop logging terminals to a file"
                } else {
                    "Log terminals to a file"
                },
                handler(this.clone(), move |view, _, cx| {
                    view.state
                        .update(cx, |state, cx| state.toggle_session_log(&log_id, cx));
                }),
            )
            .icon(IconName::File),
        );
        entries.push(
            MenuEntry::item("Set up key login", emit(SidebarEvent::SetUpKey, id.clone()))
                .icon(IconName::Lock),
        );
        entries.push(
            MenuEntry::item("Duplicate", emit(SidebarEvent::DuplicateHost, id.clone()))
                .icon(IconName::Copy),
        );
        match host.source {
            HostSource::App => entries.push(
                MenuEntry::item("Edit host", emit(SidebarEvent::EditHost, id))
                    .icon(IconName::Pencil),
            ),
            HostSource::SshConfig => entries.push(
                MenuEntry::item(
                    "Open ~/.ssh/config",
                    handler(this.clone(), |_, _, cx| {
                        cx.emit(SidebarEvent::OpenSshConfig)
                    }),
                )
                .icon(IconName::Pencil),
            ),
        }
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

    /// How the tile of a host reads its connection.
    fn tile_state(&self, id: &HostId, cx: &App) -> TileState {
        let state = self.state.read(cx);
        if state.reconnect_attempt(id).is_some() {
            return TileState::Lost;
        }
        match state.host_session(id).map(|s| &s.state) {
            Some(SessionState::Connected) => TileState::Live,
            Some(SessionState::Connecting { .. })
            | Some(SessionState::AwaitingHostKey { .. })
            | Some(SessionState::AwaitingPassphrase { .. })
            | Some(SessionState::AwaitingPassword { .. })
            | Some(SessionState::AwaitingAnswer { .. }) => TileState::Connecting,
            _ => TileState::Idle,
        }
    }

    /// The badge of a host that needs something, in place of anything else on its right.
    fn host_badge(&self, id: &HostId, cx: &App) -> Option<AnyElement> {
        let theme = cx.theme();
        match self.state.read(cx).host_session(id).map(|s| &s.state) {
            Some(SessionState::Failed { .. }) => {
                Some(badge("Failed", theme.error).into_any_element())
            }
            Some(
                SessionState::AwaitingHostKey { .. }
                | SessionState::AwaitingPassphrase { .. }
                | SessionState::AwaitingPassword { .. }
                | SessionState::AwaitingAnswer { .. },
            ) => Some(badge("Needs you", theme.warning).into_any_element()),
            _ => None,
        }
    }

    fn status_text(&self, id: &HostId, cx: &App) -> String {
        match self.state.read(cx).host_session(id).map(|s| &s.state) {
            Some(SessionState::Connected) => "Connected".to_string(),
            Some(SessionState::Connecting { .. }) => "Connecting".to_string(),
            Some(SessionState::Failed { error }) => format!("Failed: {}", error.message),
            Some(SessionState::Disconnected { reason: Some(r) }) => format!("Disconnected: {r}"),
            Some(SessionState::Disconnected { reason: None }) | None => "Not connected".into(),
            Some(_) => "Waiting for your answer".into(),
        }
    }

    /// A host's row, at its place in the list. Its tabs go right under it.
    /// `ends_section` is the last host of its section: a group dropped past it lands under it.
    fn render_host(
        &self,
        host: &HostConfig,
        ends_section: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let state = self.state.read(cx);
        let id = host.id.clone();
        let hidden = state.is_host_hidden(&id);
        let look = state.host_look(&id);
        let has_tabs = self.open.iter().any(|tab| tab.host == id);
        let focused = self.selected.as_ref() == Some(&id) && self.keyboard;
        let source = if host.source == HostSource::SshConfig {
            " · ssh"
        } else {
            ""
        };
        let status = self.status_text(&id, cx);
        let sub = match state.reconnect_attempt(&id) {
            Some(attempt) => Some(format!("Reconnecting (attempt {attempt})")),
            None => host.proxy_jump.as_ref().map(|via| format!("via {via}")),
        };
        let badge = self.host_badge(&id, cx);
        let tag = look
            .tag
            .filter(|_| has_tabs && sub.is_none() && badge.is_none());
        let (hover, accent) = (theme.hover, theme.accent);
        let lit = theme.host_color(look.color);
        let dragged = HostDrag {
            id: id.clone(),
            alias: host.alias.clone(),
        };
        let (click, add, target) = (id.clone(), id.clone(), id.clone());
        let collapsed = state.settings.collapsed_hosts.contains(&id.0);
        // Its open tabs follow it: past this host is under the last of them, not right here.
        let tabs_follow = has_tabs && !collapsed;
        let line = self
            .line_on(&DropRow::Host(id.clone()))
            .filter(|after| !after || !tabs_follow)
            .or_else(|| {
                self.line_past_group(&id, cx)
                    .filter(|_| ends_section && !tabs_follow)
            });
        let group = SharedString::from(format!("host-row-{}", id.0));
        let menu = host.clone();
        let on_add = self.over_add.as_ref() == Some(&id);
        let add_hover = id.clone();
        div()
            .id(SharedString::from(format!("host-row-{}", id.0)))
            .group(group.clone())
            .relative()
            .on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
            .map(|this| {
                Self::track_row(
                    this,
                    move |_: &HostDrag, lower| {
                        Some((DropRow::Host(target.clone()), lower && !tabs_follow))
                    },
                    cx,
                )
            })
            .map(|this| self.track_past_group(this, &id, cx))
            .children(line.map(|after| insertion_line(after, accent)))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(10.))
            .w_full()
            .h(row_px(40.))
            .px(px(6.))
            .rounded(px(9.))
            .cursor_pointer()
            .when(hidden, |this| this.opacity(0.55))
            .when(focused, |this| this.bg(theme.hover))
            .when(!on_add, |this| this.hover(move |style| style.bg(hover)))
            .tooltip(tooltip(format!(
                "{}@{}{source}\n{status}",
                host.user, host.hostname
            )))
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                // A double click that opened a terminal must not close its row right away.
                let opened_by_first =
                    event.click_count() > 1 && this.opened_by_click.as_ref() == Some(&click);
                if event.click_count() == 1 {
                    this.opened_by_click = None;
                }
                if opened_by_first {
                    return;
                }
                this.keyboard = false;
                this.selected = Some(click.clone());
                // With tabs, it opens and closes them. With none, there is something to open.
                if this.open.iter().any(|tab| tab.host == click) {
                    let settings = &this.state.read(cx).settings;
                    this.opening_host = (settings.animated_selection
                        && settings.collapsed_hosts.contains(&click.0))
                    .then(|| click.clone());
                    this.state
                        .update(cx, |state, cx| state.toggle_host_collapsed(&click, cx));
                } else {
                    this.opened_by_click = Some(click.clone());
                    cx.emit(SidebarEvent::OpenTerminal(click.clone()));
                }
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
            .child(HostTile::new(look).state(self.tile_state(&id, cx)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .truncate()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(if has_tabs {
                                theme.text
                            } else {
                                theme.text_muted
                            })
                            .child(host.alias.clone()),
                    )
                    .children(sub.map(|sub| {
                        div()
                            .truncate()
                            .text_size(crate::theme::ui_px(12.))
                            .text_color(theme.text_disabled)
                            .child(sub)
                    })),
            )
            .children(badge)
            .children(tag.map(|tag| {
                div()
                    .flex_none()
                    .child(crate::widgets::status::env_tag(tag, lit))
            }))
            .child(
                div()
                    .id(SharedString::from(format!("host-add-{}", id.0)))
                    // Not occluding: the pointer must stay "on the row" here, or the button would
                    // hide itself. So it keeps its own clicks.
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_hover(cx.listener(move |this, over: &bool, _, cx| {
                        if *over {
                            this.over_add = Some(add_hover.clone());
                        } else if this.over_add.as_ref() == Some(&add_hover) {
                            this.over_add = None;
                        }
                        cx.notify();
                    }))
                    .flex_none()
                    .invisible()
                    .group_hover(group, |style| style.visible())
                    .child(
                        Button::new(SharedString::from(format!("new-tab-{}", id.0)))
                            .icon(IconName::Plus)
                            .tooltip("New terminal (Ctrl+Shift+T)")
                            .style(ButtonStyle::Ghost)
                            .compact()
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.stop_propagation();
                                cx.emit(SidebarEvent::NewTab(add.clone()))
                            })),
                    ),
            )
            .when(has_tabs, |this| {
                this.child(
                    div()
                        .size(px(16.))
                        // Right by the +, closer than the row spaces the rest.
                        .ml(px(-9.))
                        .flex()
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .child(
                            Icon::new(if collapsed {
                                IconName::ChevronRight
                            } else {
                                IconName::ChevronDown
                            })
                            .size(px(13.))
                            .color(theme.text_disabled),
                        ),
                )
            })
            .into_any_element()
    }

    /// `last` is the last open row of its host: a host dropped past that host lands under it. With
    /// `ends_section` too, so does a group dropped past the group of that host.
    fn render_tab(
        &self,
        tab: &OpenTab,
        last: bool,
        ends_section: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let look = self.state.read(cx).host_look(&tab.host);
        let listed = self.state.read(cx).host(&tab.host).is_some();
        let id = tab.id;
        let group = SharedString::from(format!("sidebar-tab-{id}"));
        let hover = theme.hover;
        let press = theme.active;
        let accent = theme.accent;
        let same_host = tab.host.clone();
        let owner = tab.host.clone();
        let line = self
            .line_on(&DropRow::Tab(id))
            .or_else(|| {
                self.line_on(&DropRow::Host(tab.host.clone()))
                    .filter(|after| *after && last)
            })
            .or_else(|| {
                self.line_past_group(&tab.host, cx)
                    .filter(|_| last && ends_section)
            });
        let dragged = TabDrag {
            id,
            host: tab.host.clone(),
            title: tab.title.clone(),
            look,
        };
        div()
            .id(("sidebar-tab", id as usize))
            .group(group.clone())
            .flex()
            .flex_none()
            .items_center()
            .gap(px(10.))
            .h(row_px(36.))
            // Under the name of the host, and clear of the close button on the right.
            .pl(px(38.))
            .pr(px(32.))
            .rounded(px(9.))
            .cursor_pointer()
            .text_color(if tab.active {
                theme.text
            } else {
                theme.text_muted
            })
            .when(!tab.active, |this| {
                this.hover(move |style| style.bg(hover))
                    .active(move |style| style.bg(press))
            })
            .tooltip(tooltip(tab.detail.clone()))
            .on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
            .map(|this| {
                Self::track_row(
                    this,
                    move |dragged: &TabDrag, lower| {
                        (dragged.host == same_host).then_some((DropRow::Tab(id), lower))
                    },
                    cx,
                )
            })
            // A host dragged over the tabs of another one lands past them all.
            .map(|this| {
                Self::track_row(
                    this,
                    move |_: &HostDrag, _| listed.then(|| (DropRow::Host(owner.clone()), true)),
                    cx,
                )
            })
            .map(|this| self.track_past_group(this, &tab.host, cx))
            .children(line.map(|after| insertion_line(after, accent)))
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
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .gap(px(7.))
                    .child(
                        div()
                            .flex_none()
                            .child(
                                Icon::new(tab.face.icon())
                                    .size(px(14.))
                                    .color(if tab.active {
                                        theme.text
                                    } else {
                                        theme.text_muted
                                    }),
                            ),
                    )
                    .children(tab.name.clone().map(|name| {
                        div()
                            .min_w_0()
                            .truncate()
                            .when(tab.active, |this| {
                                this.font_weight(gpui::FontWeight::MEDIUM)
                            })
                            .child(name)
                    }))
                    .children(
                        tab.folder
                            .clone()
                            .filter(|_| tab.name.is_none())
                            .map(|folder| {
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .font_family(crate::theme::mono_font())
                                    .text_size(crate::theme::ui_px(13.))
                                    .text_color(theme.text_disabled)
                                    .child(folder)
                            }),
                    ),
            )
            .child(
                div()
                    .id(("sidebar-tab-close", id as usize))
                    .absolute()
                    .right(px(7.))
                    .size(px(22.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.))
                    .invisible()
                    .group_hover(group, |style| style.visible())
                    .hover(move |style| style.bg(press))
                    .tooltip(tooltip("Close (Ctrl+Shift+W, middle-click)"))
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.stop_propagation();
                        cx.emit(SidebarEvent::CloseTab(id));
                    }))
                    .child(Icon::new(IconName::Close).size(px(12.))),
            )
            .relative()
            .into_any_element()
    }

    /// Says where an update stands, and takes it to the next step. Nothing while up to date.
    fn update_button(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        type Click = Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>;
        let theme = cx.theme().clone();
        let icon = |name: IconName, color| Icon::new(name).color(color).into_any_element();
        let install = |cx: &mut Context<Self>| -> Click {
            Box::new(cx.listener(|this, _, _, cx| {
                this.state.update(cx, |state, cx| state.install_update(cx));
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
                    Some(Box::new(cx.listener(|_, _, _, cx| {
                        cx.emit(SidebarEvent::RestartForUpdate)
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
                .h(px(26.))
                .flex()
                .flex_none()
                .items_center()
                .gap(px(5.))
                .px(px(8.))
                .rounded(px(7.))
                .text_size(crate::theme::ui_px(13.))
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
}

impl Render for Sidebar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // A drag let go anywhere ends here too, with or without a drop.
        if !cx.has_active_drag() {
            self.drop_line = None;
        }
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
        // A group's title has air around it; the tile of a host fills its row.
        let starts_with_host = sections.first().is_some_and(|(group, _)| group.is_none());
        let show_hidden = self.show_hidden;
        let mut rows: Vec<AnyElement> = Vec::new();
        let mut current_child = None;
        let mut position = 0;
        for (group, hosts) in &sections {
            if let Some(group) = group {
                rows.push(self.render_group_header(group, hosts.len(), cx));
            }
            for (at_host, host) in hosts.iter().enumerate() {
                position += 1;
                if !self.shows_host(group, cx) {
                    continue;
                }
                let ends_section = at_host + 1 == hosts.len();
                let row = self.render_host(host, ends_section, cx);
                let entering = group.is_some() && *group == self.opening_group;
                rows.push(if entering {
                    motion::enter(
                        div().flex_none().child(row),
                        ("host-row-in", position as u64),
                        motion::ROW_IN,
                    )
                    .into_any_element()
                } else {
                    row
                });
                if self
                    .state
                    .read(cx)
                    .settings
                    .collapsed_hosts
                    .contains(&host.id.0)
                {
                    continue;
                }
                let tabs: Vec<&OpenTab> = open.iter().filter(|tab| tab.host == host.id).collect();
                for (at, tab) in tabs.iter().enumerate() {
                    // Among the children of the list, the pill comes first.
                    if tab.active {
                        current_child = Some(rows.len() + 1);
                    }
                    let row = self.render_tab(tab, at + 1 == tabs.len(), ends_section, cx);
                    let entering = self.opening_host.as_ref() == Some(&host.id)
                        || (group.is_some() && *group == self.opening_group);
                    rows.push(if entering {
                        motion::enter(
                            div().flex_none().child(row),
                            ("tab-row-in", tab.id),
                            motion::ROW_IN,
                        )
                        .into_any_element()
                    } else {
                        row
                    });
                }
            }
        }
        // A tab can outlive its host, removed from the config meanwhile.
        let known = self.state.read(cx);
        let orphans: Vec<OpenTab> = open
            .iter()
            .filter(|tab| known.host(&tab.host).is_none())
            .cloned()
            .collect();
        if !orphans.is_empty() {
            rows.push(section_label("No longer in the list", &theme).into_any_element());
            for tab in &orphans {
                if tab.active {
                    current_child = Some(rows.len() + 1);
                }
                rows.push(self.render_tab(tab, false, false, cx));
            }
        }
        let update = self.update_button(cx);
        let ghost = ButtonStyle::Ghost;
        let raised = theme.track;
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
            .pb(px(8.))
            .text_size(crate::theme::ui_px(13.))
            .child(
                div()
                    .id("sidebar-header")
                    .occlude()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .h(px(title_bar::HEIGHT))
                    .pl(px(6.))
                    .window_control_area(WindowControlArea::Drag)
                    .child(title_bar::logo(px(19.), theme.text))
                    .child(
                        // Occluding: this header drags the window everywhere else.
                        div()
                            .id("sidebar-header-actions")
                            .occlude()
                            .flex_none()
                            .child(
                                Button::new("hide-sidebar")
                                    .icon(IconName::PanelLeft)
                                    .tooltip("Hide sidebar (Ctrl+Shift+B)")
                                    .style(ghost)
                                    .on_click(cx.listener(|_, _, _, cx| {
                                        cx.emit(SidebarEvent::HideSidebar)
                                    })),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    // On the line of the bar beside it, and close to the list it names.
                    .mt(row_px(crate::workspace::LOCATION_HEIGHT / 2.) - px(7.))
                    .h(px(14.))
                    .pl(px(6.))
                    .text_size(crate::theme::ui_px(11.))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme.text_disabled)
                    // gpui has no letter spacing.
                    .gap(px(1.))
                    .children("HOSTS".chars().map(|letter| div().child(letter.to_string()))),
            )
            .child(
                div()
                    .on_children_prepainted(self.pill.measure(current_child))
                    .id("host-list")
                    .relative()
                    .flex_1()
                    .min_h_0()
                    // The drop line of the first and of the last row needs a gap there too.
                    .when(starts_with_host, |this| this.mt(px(8.) - ROW_GAP).pt(ROW_GAP))
                    .mb(px(8.) - ROW_GAP)
                    .pb(ROW_GAP)
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap(ROW_GAP)
                    .on_drag_move(cx.listener(Self::leave_row::<HostDrag>))
                    .on_drag_move(cx.listener(Self::leave_row::<TabDrag>))
                    .on_drag_move(cx.listener(Self::leave_row::<GroupDrag>))
                    // Here and not on the rows: the line can also be in the gap between two.
                    .on_drop(cx.listener(|this, dragged: &HostDrag, _, cx| {
                        if let Some(DropLine {
                            row: DropRow::Host(target),
                            after,
                            ..
                        }) = this.drop_line.take()
                        {
                            this.state.update(cx, |state, cx| {
                                state.place_host(&dragged.id, &target, after, cx)
                            });
                            this.keep_selection_listed(cx);
                            cx.notify();
                        }
                    }))
                    .on_drop(cx.listener(|this, dragged: &TabDrag, _, cx| {
                        if let Some(DropLine {
                            row: DropRow::Tab(target),
                            after,
                            ..
                        }) = this.drop_line.take()
                        {
                            cx.emit(SidebarEvent::MoveTab(dragged.id, target, after));
                        }
                    }))
                    .on_drop(cx.listener(|this, dragged: &GroupDrag, _, cx| {
                        if let Some(DropLine {
                            row: DropRow::Group(target),
                            after,
                            ..
                        }) = this.drop_line.take()
                        {
                            this.state.update(cx, |state, cx| {
                                state.place_group(&dragged.name, &target, after, cx)
                            });
                        }
                    }))
                    .child(self.pill.element(&theme, 9.))
                    .children(rows)
                    .when(hosts_empty, |this| {
                        this.child(div().p(px(8.)).text_color(theme.text_muted).child(
                            if all_hosts == 0 {
                                "No hosts. Add one with the + below, or define Host entries in ~/.ssh/config."
                            } else {
                                "All hosts are hidden."
                            },
                        ))
                    })
                    .when(hidden > 0, |this| {
                        this.child(
                            div()
                                .id("toggle-hidden-hosts")
                                .flex()
                                .flex_none()
                                .items_center()
                                .h(px(26.))
                                .mt(px(2.))
                                .px(px(6.))
                                .rounded(px(6.))
                                .cursor_pointer()
                                .text_size(crate::theme::ui_px(13.))
                                .text_color(theme.text_disabled)
                                .hover(move |style| style.bg(raised))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.show_hidden = !this.show_hidden;
                                    let listed = this.listed_hosts(cx);
                                    if !listed.iter().any(|h| Some(&h.id) == this.selected.as_ref())
                                    {
                                        this.selected = listed.first().map(|h| h.id.clone());
                                    }
                                    cx.notify();
                                }))
                                .child(if show_hidden {
                                    "Stop listing hidden hosts".to_string()
                                } else if hidden == 1 {
                                    "1 hidden host".to_string()
                                } else {
                                    format!("{hidden} hidden hosts")
                                }),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(2.))
                    .child(
                        Button::new("settings")
                            .icon(IconName::Settings)
                            .label("Settings")
                            .tooltip("Settings (Ctrl+,)")
                            .style(ghost)
                            .on_click(
                                cx.listener(|_, _, _, cx| cx.emit(SidebarEvent::OpenSettings)),
                            ),
                    )
                    .child(div().flex_1())
                    .children(update)
                    .child(
                        Button::new("new-host")
                            .icon(IconName::Plus)
                            .tooltip("New host (Ctrl+Shift+N)")
                            .style(ghost)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(SidebarEvent::NewHost))),
                    ),
            )
            .children(self.menu.clone().map(|menu| {
                let this = cx.entity().downgrade();
                ContextMenu::new(
                    "host-menu",
                    menu,
                    Rc::new(move |_, cx| {
                        this.update(cx, |this, cx| {
                            this.menu = None;
                            cx.notify();
                        })
                        .ok();
                    }),
                )
            }))
    }
}
