//! Left column: the open sessions, one row each in the order of the tabs, then the hosts in the
//! user's order, settings and the state of an update. A tile in the host's color says which host
//! a session is on, a dot how a host is.
use crate::motion::Reveal;
use crate::state::AppState;
use crate::tabs::Face;
use crate::theme::{ActiveTheme as _, Theme, row_px};
use crate::title_bar;
use crate::updater::UpdateStatus;
use crate::widgets::button::{Button, ButtonStyle};
use crate::widgets::context_menu::{ContextMenu, MenuEntry, OpenMenu, handler};
use crate::widgets::host_color::color_dot;
use crate::widgets::icon::{Icon, IconName};
use crate::widgets::pill::SlidingPill;
use crate::widgets::progress::ProgressRing;
use crate::widgets::tile::shell_tile;
use crate::widgets::tooltip::tooltip;
use gpui::prelude::*;
use gpui::{
    AnyElement, App, Bounds, ClickEvent, Context, Div, DragMoveEvent, Entity, EventEmitter,
    FocusHandle, Focusable, Hsla, KeyDownEvent, MouseButton, MouseDownEvent, Pixels, Point,
    SharedString, Stateful, Subscription, Window, WindowControlArea, div, px,
};
use shcp_core::{HostConfig, HostId, HostSource, SessionState};
use std::collections::HashMap;
use std::rc::Rc;

pub enum SidebarEvent {
    OpenTerminal(HostId),
    OpenFiles(HostId),
    Disconnect(HostId),
    HostSettings(HostId),
    DuplicateHost(HostId),
    NewGroup(HostId),
    RenameGroup(String),
    Connect(HostId),
    NewHost,
    OpenSettings,
    RestartForUpdate,
    /// From its own button: the sidebar goes.
    HideSidebar,
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
    /// Its terminals are split in more than one.
    pub split: bool,
    pub active: bool,
}

/// A tab on its way to another place among the sessions.
#[derive(Clone)]
struct TabDrag {
    id: u64,
    title: String,
}

impl Render for TabDrag {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .h(px(30.))
            .px(px(10.))
            .flex()
            .items_center()
            .rounded_full()
            .map(|this| {
                crate::panel::floating(this, theme, crate::panel::CAPSULE, theme.strong_border)
            })
            .shadow(crate::theme::float_shadow())
            .font_family(crate::theme::ui_font())
            .text_size(crate::theme::ui_px(13.))
            .text_color(theme.text)
            .child(self.title.clone())
    }
}

/// The title of a part of the list. `first` has nothing above to keep away from.
fn section_label(text: &'static str, first: bool, theme: &Theme) -> Div {
    div()
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
/// How tall the title of a part of the list is, and the room above it.
const LABEL_HEIGHT: f32 = 26.;
const LABEL_SPACE: f32 = 10.;
/// Where the titles and the dots of the hosts start.
const LABEL_INSET: Pixels = px(13.);
const SESSION_HEIGHT: f32 = 32.;
const HOST_HEIGHT: f32 = 30.;
/// Buttons and whatever else is a control. The rows you go somewhere with are capsules.
const CONTROL_RADIUS: Pixels = px(crate::theme::RADIUS_CONTROL);

/// What has rows folding under it.
#[derive(Clone, PartialEq, Eq, Hash)]
enum Fold {
    Group(String),
}

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
        // Clipped only on its way: at rest the drop line goes past the row.
        .child(
            div()
                .when(moving, |this| this.h_full().overflow_hidden())
                .child(row),
        )
}

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
            .rounded_full()
            .map(|this| crate::panel::floating(this, theme, crate::panel::CAPSULE, theme.border))
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
            .rounded_full()
            .map(|this| crate::panel::floating(this, theme, crate::panel::CAPSULE, theme.border))
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
    /// The arrow keys are moving the selection, so it shows.
    keyboard: bool,
    /// What folds under each group and host, for it to glide open and closed.
    folds: HashMap<Fold, Reveal>,
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
            keyboard: false,
            folds: HashMap::new(),
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

    /// The mouse is about to open or close this: it glides, if the setting allows it.
    fn arm_fold(&mut self, fold: Fold, cx: &App) {
        if self.state.read(cx).settings.animated_selection
            && let Some(reveal) = self.folds.get_mut(&fold)
        {
            reveal.arm();
        }
    }

    /// How much of what folds under a group or a host shows now, from 0 to 1.
    fn fold(&mut self, fold: Fold, open: bool) -> f32 {
        let reveal = self.folds.entry(fold).or_insert_with(|| Reveal::rows(open));
        reveal.set(open);
        reveal.value()
    }

    fn toggle_group(&mut self, group: &str, cx: &mut Context<Self>) {
        self.arm_fold(Fold::Group(group.to_string()), cx);
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
            DropRow::Tab(_) => self.open.iter().map(|tab| DropRow::Tab(tab.id)).collect(),
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

    /// `first` is the first row of the list: it has nothing above to keep away from.
    fn render_group_header(&self, group: &str, first: bool, cx: &mut Context<Self>) -> AnyElement {
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
            .gap(px(8.))
            .h(px(LABEL_HEIGHT))
            .when(!first, |this| this.mt(px(LABEL_SPACE)))
            .pl(LABEL_INSET)
            .pr(px(8.))
            .rounded_full()
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

    /// To the first session of a host, or a terminal on it when it has none.
    fn go_to_host(&mut self, host: &HostId, cx: &mut Context<Self>) {
        match self.open.iter().find(|tab| tab.host == *host) {
            Some(tab) => cx.emit(SidebarEvent::ActivateTab(tab.id)),
            None => cx.emit(SidebarEvent::OpenTerminal(host.clone())),
        }
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

    /// How a host is: filled in its color while it has sessions, faint while they wait for the
    /// connection, a plain ring when it has none. What needs the user takes the color of what
    /// says so.
    fn host_dot(&self, id: &HostId, color: Hsla, cx: &App) -> Div {
        if let Some((_, alert)) = self.host_alert(id, cx) {
            return color_dot(alert, true);
        }
        if !self.open.iter().any(|tab| tab.host == *id) {
            return color_dot(cx.theme().text_disabled, false);
        }
        let state = self.state.read(cx);
        let waiting = state.reconnect_attempt(id).is_some()
            || state.host_session(id).is_some_and(|s| {
                !matches!(
                    s.state,
                    SessionState::Connected | SessionState::Disconnected { .. }
                )
            });
        color_dot(
            if waiting {
                Hsla { a: 0.45, ..color }
            } else {
                color
            },
            true,
        )
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
        let accent = theme.accent;
        let lit = theme.host_color(state.host_color(&id));
        let dragged = HostDrag {
            id: id.clone(),
            alias: host.alias.clone(),
        };
        let (click, add, target) = (id.clone(), id.clone(), id.clone());
        let line = self
            .line_on(&DropRow::Host(id.clone()))
            .or_else(|| self.line_past_group(&id, cx).filter(|_| ends_section));
        let group = SharedString::from(format!("host-row-{}", id.0));
        let menu = host.clone();
        let hover = theme.hover;
        div()
            .id(SharedString::from(format!("host-row-{}", id.0)))
            .group(group.clone())
            .relative()
            .on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
            .map(|this| {
                Self::track_row(
                    this,
                    move |_: &HostDrag, lower| Some((DropRow::Host(target.clone()), lower)),
                    cx,
                )
            })
            .map(|this| self.track_past_group(this, &id, cx))
            .children(line.map(|after| insertion_line(after, accent)))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(11.))
            .w_full()
            .h(row_px(HOST_HEIGHT))
            .pl(LABEL_INSET)
            .pr(px(6.))
            .rounded_full()
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
                "{}@{}{}{source}\n{status}",
                host.user,
                host.hostname,
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
            .child(self.host_dot(&id, lit, cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(if has_tabs {
                        theme.text
                    } else {
                        theme.text_muted
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
                    .children(
                        note.map(|note| note.group_hover(group.clone(), |style| style.invisible())),
                    )
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
                    ),
            )
            .into_any_element()
    }

    /// A session: its tile, the name of its host and where it is. Rows of every host, in the
    /// order of the tabs, and dragged anywhere among them.
    fn render_session(&self, tab: &OpenTab, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let state = self.state.read(cx);
        // A tab can outlive its host, removed from the config meanwhile.
        let host = state
            .host(&tab.host)
            .map_or(tab.host.0.clone(), |host| host.alias.clone());
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
        let accent = theme.accent;
        let line = self.line_on(&DropRow::Tab(id));
        let dragged = TabDrag {
            id,
            title: tab.title.clone(),
        };
        // A tab without a name or a known folder still has its title.
        let (place, in_mono) = match (&tab.name, &tab.folder) {
            (Some(name), _) => (name.clone(), false),
            (None, Some(folder)) => (folder.clone(), true),
            (None, None) => (tab.title.clone(), false),
        };
        div()
            .id(("sidebar-tab", id as usize))
            .group(group.clone())
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(9.))
            .h(row_px(SESSION_HEIGHT))
            .pl(px(5.))
            .pr(px(6.))
            .rounded_full()
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
            .on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
            .map(|this| {
                Self::track_row(
                    this,
                    move |_: &TabDrag, lower| Some((DropRow::Tab(id), lower)),
                    cx,
                )
            })
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
            .child(shell_tile(
                ("session-tile", id as usize),
                shell.as_deref(),
                &user,
                color,
                tab.split.then(|| theme.row_fill(tab.active)),
                &theme,
            ))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .child(host),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(theme.text_disabled)
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
                        this.invisible().group_hover(group, |style| style.visible())
                    })
                    .hover(move |style| style.bg(press).text_color(text))
                    .tooltip(tooltip("Close (Ctrl+Shift+W, middle-click)"))
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.stop_propagation();
                        cx.emit(SidebarEvent::CloseTab(id));
                    }))
                    .child(Icon::new(IconName::Close).size(px(12.))),
            )
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
                .rounded(CONTROL_RADIUS)
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
        let show_hidden = self.show_hidden;
        let mut rows: Vec<AnyElement> = Vec::new();
        let mut current_child = None;
        if !open.is_empty() {
            rows.push(section_label("Sessions", true, &theme).into_any_element());
        }
        for tab in &open {
            // Among the children of the list, the pill comes first.
            if tab.active {
                current_child = Some(rows.len() + 1);
            }
            rows.push(self.render_session(tab, cx));
        }
        for (group, hosts) in &sections {
            let first = rows.is_empty();
            match group {
                Some(group) => rows.push(self.render_group_header(group, first, cx)),
                None => rows.push(section_label("Hosts", first, &theme).into_any_element()),
            }
            // Closing, what folds is still there until it is gone.
            let shows_hosts = self.shows_host(group, cx);
            let group_open = match group {
                Some(group) => self.fold(Fold::Group(group.clone()), shows_hosts),
                None => 1.,
            };
            if group_open == 0. {
                continue;
            }
            for (at_host, host) in hosts.iter().enumerate() {
                let ends_section = at_host + 1 == hosts.len();
                let row = self.render_host(host, ends_section, cx);
                rows.push(folding(row, row_px(HOST_HEIGHT), group_open).into_any_element());
            }
        }
        if self.folds.values().any(Reveal::moving) {
            window.request_animation_frame();
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
                    // The app's mark: part of what moves the window.
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_size(crate::theme::ui_px(13.5))
                            .text_color(theme.text)
                            .child(title_bar::logo(px(18.), theme.text))
                            .child("shcp"),
                    )
                    // Its own hitbox over the strip, or the press would move the window.
                    .child(
                        div().id("sidebar-hide").occlude().child(
                            Button::new("hide-sidebar")
                                .icon(IconName::PanelLeft)
                                .tooltip("Hide sidebar (Ctrl+Shift+B)")
                                .style(ghost)
                                .on_click(
                                    cx.listener(|_, _, _, cx| cx.emit(SidebarEvent::HideSidebar)),
                                ),
                        ),
                    ),
            )
            .child(
                div()
                    .on_children_prepainted(self.pill.measure(current_child))
                    .id("host-list")
                    .relative()
                    .flex_1()
                    .min_h_0()
                    // The drop line of the first and of the last row needs a gap there too.
                    .pt(ROW_GAP)
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
                    .child(
                        self.pill
                            .neutral(&theme, f32::from(row_px(SESSION_HEIGHT)) / 2.),
                    )
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
