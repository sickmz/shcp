//! Root view: the sidebar with the sessions and hosts, the cards of the open tab, the dock, the
//! status row, notices and modals.
use crate::actions::*;
use crate::dock_panel::{DockEvent, DockPanel, DockTab};
use crate::files_view::{FilesEvent, FilesView};
use crate::login_prompt::{LoginAsk, LoginPrompt};
use crate::modals::{
    ModalLayer,
    confirm::ConfirmModal,
    host_commands::HostCommandsModal,
    host_editor::{ConnectHandler, HostEditor},
    key_setup::KeySetupModal,
    login::LoginModal,
    palette::{CommandPalette, PaletteItem},
    settings::SettingsModal,
    text_prompt::TextPrompt,
};
use crate::motion::{self, Beat, HoverFade};
use crate::notices;
use crate::panel;
use crate::session_overlay::{self, Overlay, Surface};
use crate::sidebar::{OpenTab, Sidebar, SidebarEvent};
use crate::state::{AppState, NoticeAction};
use crate::tabs::{self, Face, Tab};
use crate::terminal_panes::{PanesEvent, TerminalPanes};
use crate::terminal_view::TerminalView;
use crate::theme::{self, ActiveTheme as _};
use crate::title_bar::{self, WindowControls};
use crate::widgets::button::{Button, ButtonStyle};
use crate::widgets::context_menu::{ContextMenu, OpenMenu, handler};
use crate::widgets::icon::Icon;
use crate::widgets::icon::IconName;
use crate::widgets::progress::ProgressRing;
use crate::widgets::status::status_dot;
use crate::widgets::tile::{Mark, tile};
use crate::widgets::tooltip::tooltip;
use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, CursorStyle, DismissEvent, Div, Entity, FocusHandle, Focusable,
    MouseButton, MouseMoveEvent, MouseUpEvent, Stateful, Subscription, Task, WeakEntity, Window,
    WindowBounds, WindowControlArea, div, px, relative,
};
use shcp_core::{
    ConnectErrorKind, Event, ForwardState, HostId, NoticeLevel, SavedPane, SavedTab, SavedTabs,
    SavedWindow, SessionId, SessionState, TransferState,
};
use shcp_core::{HostConfig, SharedTab, SharedTabs, TerminalId};
use std::{
    cell::Cell,
    collections::{HashMap, HashSet},
    rc::Rc,
    time::Duration,
};

/// Seconds to wait before each automatic attempt to connect again; the last one repeats.
const RECONNECT_DELAYS: [u64; 5] = [2, 5, 10, 20, 30];
/// Then it stops: about nine minutes without a connection.
const RECONNECT_ATTEMPTS: u32 = 20;
const STATUS_BAR_HEIGHT: f32 = 32.;
/// With both views showing, neither gets less than this share.
const MIN_BOTH_RATIO: f32 = 0.15;

/// A share of the width for the files beside the terminal, kept within bounds. `None` when it is
/// no number.
fn both_ratio(ratio: f32) -> Option<f32> {
    ratio
        .is_finite()
        .then(|| ratio.clamp(MIN_BOTH_RATIO, 1. - MIN_BOTH_RATIO))
}

/// Types `cd` to a folder in a terminal, and runs it.
fn cd(terminal: &Entity<TerminalView>, folder: &str, cx: &mut App) {
    let command = format!("cd {}", shcp_core::shell_quote(folder));
    terminal.update(cx, |terminal, cx| terminal.run_command(&command, cx));
}

fn reconnect_delay(attempt: u32) -> Duration {
    let index = (attempt.saturating_sub(1) as usize).min(RECONNECT_DELAYS.len() - 1);
    Duration::from_secs(RECONNECT_DELAYS[index])
}

pub(crate) struct Workspace {
    pub(crate) state: Entity<AppState>,
    sidebar: Entity<Sidebar>,
    dock: Entity<DockPanel>,
    modals: Entity<ModalLayer>,
    pub(crate) tabs: Vec<Tab>,
    active: Option<usize>,
    sidebar_visible: bool,
    dock_visible: bool,
    resizing_sidebar: bool,
    pub(crate) tab_menu: Option<OpenMenu>,
    next_tab_id: u64,
    focus: FocusHandle,
    /// Waiting for the next automatic attempt to connect a host again.
    reconnect_timers: HashMap<HostId, Task<()>>,
    /// What `tabs.toml` holds.
    saved_tabs: SavedTabs,
    /// No tab was open yet: the empty state makes its entrance. Closing the last tab, later,
    /// brings it back at once.
    first_empty_state: bool,
    /// The notice is expanded to the full text of all of them.
    pub(crate) notices_open: bool,
    /// The pointer is on the notice, not expanded.
    pub(crate) notice_hovered: bool,
    /// How far the notice is expanded, on its way there or back.
    pub(crate) notices_reveal: motion::Reveal,
    /// The notice that was on the label when it last expanded.
    pub(crate) notice_folded: Option<u64>,
    /// The user agreed to close despite what is running.
    close_confirmed: bool,
    /// The question is on screen, so another click on the X adds nothing.
    close_asked: bool,
    /// The host connecting in order to set up a key.
    key_setup_waiting: Option<HostId>,
    /// The divider between terminals and files is being dragged.
    resizing_both: bool,
    /// How much of the sidebar and of the dock shows, on their way in or out.
    sidebar_reveal: motion::Reveal,
    dock_reveal: motion::Reveal,
    /// How tall the dock is when open.
    dock_height: Rc<Cell<f32>>,
    /// The tab whose view the mouse last changed, and how many times: its content fades in.
    face_fade: Option<(u64, u64)>,
    /// The tools of the files card, coming and going with the pointer.
    files_fade: HoverFade,
    /// Where the two views were last drawn, to turn a drag into a share.
    both_bounds: Rc<Cell<Option<gpui::Bounds<gpui::Pixels>>>>,
    /// This run of the app, as the other PCs on a host see it.
    writer: String,
    /// How the tabs of each host are as far as the other PCs know.
    shared: HashMap<HostId, SharedTabs>,
    /// A change from another PC, waiting for a divider to be let go of.
    pending_shared: HashMap<HostId, SharedTabs>,
    /// Hosts whose tabs are in step with the server. Until then what is here may be old, and must
    /// not overwrite what is there.
    surveyed: HashSet<HostId>,
    _subscriptions: Vec<Subscription>,
}

impl Workspace {
    pub(crate) fn new(
        state: Entity<AppState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let sidebar = cx.new(|cx| Sidebar::new(state.clone(), cx));
        let dock = cx.new(|cx| DockPanel::new(state.clone(), window, cx));
        let modals = cx.new(|_| ModalLayer::default());
        let subscriptions = vec![
            cx.subscribe_in(&sidebar, window, Self::on_sidebar_event),
            cx.subscribe_in(&state, window, Self::on_core_event),
            cx.observe(&state, |_, _, cx| cx.notify()),
            cx.observe(&modals, |_, _, cx| cx.notify()),
            // The selected host decides what the Ports grid shows.
            cx.observe(&sidebar, |_, _, cx| cx.notify()),
            cx.subscribe(&dock, |this, _, _: &DockEvent, cx| {
                this.arm_dock(cx);
                this.dock_visible = false;
                cx.notify();
            }),
            // "Sync with Windows": the system switched between light and dark.
            cx.observe_window_appearance(window, |this, _, cx| {
                let (settings, engine) = {
                    let state = this.state.read(cx);
                    (state.settings.clone(), state.engine.clone())
                };
                crate::theme::apply(&settings, &engine, cx);
            }),
        ];
        let workspace = Self {
            state,
            sidebar,
            dock,
            modals,
            tabs: Vec::new(),
            active: None,
            sidebar_visible: true,
            dock_visible: false,
            resizing_sidebar: false,
            tab_menu: None,
            next_tab_id: 1,
            focus: cx.focus_handle(),
            reconnect_timers: HashMap::new(),
            saved_tabs: SavedTabs::default(),
            first_empty_state: true,
            notices_open: false,
            notice_hovered: false,
            notices_reveal: motion::Reveal::new(false),
            notice_folded: None,
            close_confirmed: false,
            close_asked: false,
            key_setup_waiting: None,
            resizing_both: false,
            sidebar_reveal: motion::Reveal::new(true),
            dock_reveal: motion::Reveal::new(false),
            dock_height: Default::default(),
            face_fade: None,
            files_fade: HoverFade::default(),
            both_bounds: Default::default(),
            writer: shcp_core::new_terminal_key(),
            shared: HashMap::new(),
            pending_shared: HashMap::new(),
            surveyed: HashSet::new(),
            _subscriptions: subscriptions,
        };
        let this = cx.weak_entity();
        window.on_window_should_close(cx, move |window, cx| {
            this.update(cx, |this, cx| this.on_close_requested(window, cx))
                .unwrap_or(true)
        });
        window.focus(&workspace.sidebar.focus_handle(cx));
        workspace
    }

    // ---- closing ----

    /// Remembers where the window is, and asks first if closing would interrupt something.
    fn on_close_requested(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let saved = saved_window(window);
        self.state.update(cx, |s, cx| s.remember_window(saved, cx));
        if self.close_confirmed {
            return true;
        }
        let Some(interrupted) = self.state.read(cx).closing_would_stop() else {
            return true;
        };
        if self.close_asked {
            return false;
        }
        self.close_asked = true;
        let this = cx.weak_entity();
        let modal = cx.new(|cx| {
            ConfirmModal::new(
                "Close shcp?",
                format!("If you close now, {interrupted}."),
                "Close",
                move |window, cx| {
                    this.update(cx, |this, _| this.close_confirmed = true).ok();
                    window.remove_window();
                },
                cx,
            )
        });
        self._subscriptions
            .push(cx.subscribe(&modal, |this, _, _: &DismissEvent, _| {
                this.close_asked = false
            }));
        self.modals.update(cx, |m, cx| m.show(modal, window, cx));
        false
    }

    // ---- tabs of the last run ----

    /// Opens the tabs of the last run. Their hosts may be gone by now: those are left out.
    pub(crate) fn restore_tabs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (saved, enabled) = {
            let state = self.state.read(cx);
            (state.engine.load_tabs(), state.settings.restore_tabs)
        };
        self.saved_tabs = saved.clone();
        // Before any terminal starts: a new shell after a restart of the server goes there.
        let folders = saved.folders.clone().into_iter().collect();
        self.state
            .update(cx, |state, _| state.terminal_folders = folders);
        if !enabled {
            return;
        }
        let known = |host: &str, this: &Self, cx: &Context<Self>| {
            this.state
                .read(cx)
                .host(&HostId(host.to_string()))
                .is_some()
        };
        let active_tab = saved
            .active
            .and_then(|index| saved.tabs.get(index))
            .cloned();
        let mut active = None;
        for tab in &saved.tabs {
            let opened = match tab {
                SavedTab::Terminal {
                    host,
                    layout,
                    both,
                    ratio,
                    name,
                } if known(host, self, cx) => {
                    self.open_terminal_with(HostId(host.clone()), layout, true, window, cx);
                    // Saved split, from before the files kept to one terminal: no files.
                    if *both
                        && let Some(index) = self.active
                        && !self.tabs[index].panes.read(cx).is_split()
                    {
                        self.set_face(index, Face::Files, window, cx);
                        if let Some(ratio) = ratio.and_then(both_ratio) {
                            self.tabs[index].both_ratio = ratio;
                        }
                    }
                    Some(name)
                }
                SavedTab::Files { host, name } if known(host, self, cx) => {
                    self.open_files(HostId(host.clone()), window, cx);
                    Some(name)
                }
                _ => None,
            };
            if let Some(name) = opened
                && let Some(opened_tab) = self.active.and_then(|index| self.tabs.get_mut(index))
            {
                opened_tab.name.clone_from(name);
            }
            let opened = opened.is_some();
            if opened && active.is_none() && active_tab.as_ref() == Some(tab) {
                active = Some(self.tabs.len() - 1);
            }
        }
        if let Some(index) = active {
            self.activate(index, window, cx);
        }
    }

    fn current_saved_tabs(&self, cx: &Context<Self>) -> SavedTabs {
        SavedTabs {
            active: self.active,
            tabs: self
                .tabs
                .iter()
                .map(|tab| SavedTab::Terminal {
                    host: tab.host.0.clone(),
                    layout: tab.panes.read(cx).saved(cx),
                    both: tab.face.shows_files(),
                    ratio: tab.face.shows_files().then_some(tab.both_ratio),
                    name: tab.name.clone(),
                })
                .collect(),
            folders: self
                .tabs
                .iter()
                .flat_map(|tab| tab.panes.read(cx).folders(cx))
                .collect(),
        }
    }

    // ---- tabs shared with other PCs ----

    /// The tabs of a host as they are here. In a set order, since each PC has its own.
    fn shared_now(&self, host: &HostId, cx: &App) -> SharedTabs {
        let mut tabs: Vec<SharedTab> = self
            .tabs
            .iter()
            .filter(|tab| tab.host == *host)
            .filter_map(|tab| {
                let layout = tab.panes.read(cx).saved(cx).retain_keys(&|_| true)?;
                let files = tab.face.shows_files();
                Some(SharedTab {
                    name: tab.name.clone(),
                    files,
                    files_ratio: files.then_some(tab.both_ratio),
                    layout,
                })
            })
            .collect();
        tabs.sort_by_cached_key(|tab| tab.layout.keys().into_iter().min());
        SharedTabs {
            writer: self.writer.clone(),
            tabs,
        }
    }

    /// Tells the other PCs on each host what changed here. It is written as long as the app has
    /// something to say: the first terminal of a host that gets there takes it along.
    fn publish_shared(&mut self, cx: &mut Context<Self>) {
        if self.resizing_both {
            return;
        }
        self.shared
            .retain(|host, _| self.tabs.iter().any(|tab| tab.host == *host));
        let mut hosts: Vec<HostId> = Vec::new();
        for tab in &self.tabs {
            if !hosts.contains(&tab.host) && self.surveyed.contains(&tab.host) {
                hosts.push(tab.host.clone());
            }
        }
        for host in hosts {
            let tabs: Vec<&Tab> = self.tabs.iter().filter(|tab| tab.host == host).collect();
            if tabs.iter().any(|tab| tab.panes.read(cx).is_dragging()) {
                continue;
            }
            let now = self.shared_now(&host, cx);
            if self
                .shared
                .get(&host)
                .is_some_and(|known| known.tabs == now.tabs)
            {
                continue;
            }
            let Some(terminal) = tabs
                .iter()
                .find_map(|tab| tab.panes.read(cx).live_terminal(cx))
            else {
                continue;
            };
            self.state.read(cx).engine.publish_tabs(terminal, &now);
            self.shared.insert(host, now);
        }
    }

    /// A host was just connected. What it says wins over what this PC remembers, except for new
    /// terminals of this PC, which it has not heard of.
    fn on_shared_server(
        &mut self,
        host: &HostId,
        live: &[String],
        tabs: Option<&SharedTabs>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.state.read(cx).keeps_terminals(host) {
            return;
        }
        self.surveyed.insert(host.clone());
        let live: HashSet<String> = live.iter().cloned().collect();
        let mut shared = tabs.cloned().unwrap_or_default();
        // A server with nothing running has restarted: the tabs stay, with new shells.
        if !live.is_empty() {
            shared.tabs = shared
                .tabs
                .iter()
                .filter_map(|tab| {
                    let layout = tab.layout.retain_keys(&|key| live.contains(key))?;
                    Some(SharedTab {
                        layout,
                        ..tab.clone()
                    })
                })
                .collect();
        }
        let mut here = HashSet::new();
        let mut gone = HashSet::new();
        for tab in self.tabs.iter().filter(|tab| tab.host == *host) {
            let panes = tab.panes.read(cx);
            here.extend(panes.keys(cx));
            if !live.is_empty() {
                gone.extend(panes.missing_keys(&live, cx));
            }
        }
        // Running there and shown nowhere, like a terminal closed while the connection was down.
        let listed: HashSet<String> = shared
            .tabs
            .iter()
            .flat_map(|tab| tab.layout.keys())
            .collect();
        let mut lost: Vec<&String> = live
            .iter()
            .filter(|key| !listed.contains(*key) && !here.contains(*key))
            .collect();
        lost.sort();
        for key in lost {
            shared.tabs.push(SharedTab {
                name: None,
                files: false,
                files_ratio: None,
                layout: SavedPane::Terminal {
                    key: Some(key.clone()),
                },
            });
        }
        self.apply_shared(host, &shared, &gone, window, cx);
        self.shared.insert(host.clone(), shared);
    }

    /// A PC changed the tabs of a host.
    fn on_shared_tabs(
        &mut self,
        host: &HostId,
        tabs: &SharedTabs,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // What this PC wrote comes back late, when it may have moved on.
        if tabs.writer == self.writer || !self.state.read(cx).keeps_terminals(host) {
            return;
        }
        let dragging = self.resizing_both
            || self
                .tabs
                .iter()
                .filter(|tab| tab.host == *host)
                .any(|tab| tab.panes.read(cx).is_dragging());
        if dragging {
            self.pending_shared.insert(host.clone(), tabs.clone());
            return;
        }
        self.pending_shared.remove(host);
        let previous = self.shared.get(host);
        if previous.is_some_and(|known| known.tabs == tabs.tabs) {
            return;
        }
        let listed: HashSet<String> = tabs.tabs.iter().flat_map(|tab| tab.layout.keys()).collect();
        // Only what the other PC knew and took out was closed. The rest is too new for it to know.
        let gone: HashSet<String> = previous
            .iter()
            .flat_map(|known| known.tabs.iter())
            .flat_map(|tab| tab.layout.keys())
            .filter(|key| !listed.contains(key))
            .collect();
        self.apply_shared(host, tabs, &gone, window, cx);
        self.shared.insert(host.clone(), tabs.clone());
    }

    /// Makes the tabs of a host follow `shared`, leaving the keyboard and the active tab alone.
    fn apply_shared(
        &mut self,
        host: &HostId,
        shared: &SharedTabs,
        gone: &HashSet<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let active = self.active_tab().map(|t| t.id);
        let mut matched = Vec::new();
        for remote in &shared.tabs {
            let keys: HashSet<String> = remote.layout.keys().into_iter().collect();
            let local = self.tabs.iter().position(|tab| {
                tab.host == *host
                    && !matched.contains(&tab.id)
                    && tab.panes.read(cx).keys(cx).iter().any(|k| keys.contains(k))
            });
            match local {
                Some(index) => {
                    let tab = &mut self.tabs[index];
                    tab.name.clone_from(&remote.name);
                    matched.push(tab.id);
                    let panes = tab.panes.clone();
                    panes.update(cx, |panes, cx| {
                        panes.apply_layout(&remote.layout, gone, window, cx)
                    });
                    self.apply_shared_face(index, remote, window, cx);
                }
                None => {
                    self.open_terminal_with(host.clone(), &remote.layout, false, window, cx);
                    if let Some(tab) = self.tabs.last_mut() {
                        tab.name.clone_from(&remote.name);
                        matched.push(tab.id);
                    }
                    self.apply_shared_face(self.tabs.len() - 1, remote, window, cx);
                }
            }
        }
        // The other tabs of the host are new here, unless what they showed was closed.
        let others: Vec<u64> = self
            .tabs
            .iter()
            .filter(|tab| tab.host == *host && !matched.contains(&tab.id))
            .map(|tab| tab.id)
            .collect();
        for id in others {
            let Some(index) = self.tab_index(id) else {
                continue;
            };
            let panes = self.tabs[index].panes.clone();
            if !panes.update(cx, |panes, cx| panes.drop_keys(gone, window, cx)) {
                self.drop_tab(index, false, window, cx);
            }
        }
        if let Some(index) = active.and_then(|id| self.tab_index(id))
            && self.active != Some(index)
        {
            self.activate(index, window, cx);
        }
        cx.notify();
    }

    /// Shows a tab the way another PC does: with its files or not, and how much room they take.
    fn apply_shared_face(
        &mut self,
        index: usize,
        remote: &SharedTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let face = if remote.files {
            Face::Files
        } else {
            Face::Terminal
        };
        // A split tab keeps its terminals alone, whatever another PC shows.
        let split = self.tabs[index].panes.read(cx).is_split();
        if self.tabs[index].face != face && !(face.shows_files() && split) {
            self.set_face(index, face, window, cx);
        }
        if let Some(ratio) = remote.files_ratio.and_then(both_ratio) {
            self.tabs[index].both_ratio = ratio;
        }
    }

    /// A terminal was closed on another PC, or its shell is not on the server any more.
    fn on_terminal_closed(&mut self, id: TerminalId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self
            .tabs
            .iter()
            .position(|tab| tab.panes.read(cx).has_terminal(id, cx))
        else {
            return;
        };
        let panes = self.tabs[index].panes.clone();
        if !panes.update(cx, |panes, cx| panes.remove_terminal(id, window, cx)) {
            self.drop_tab(index, false, window, cx);
        }
    }

    fn apply_pending_shared(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for (host, tabs) in std::mem::take(&mut self.pending_shared) {
            self.on_shared_tabs(&host, &tabs, window, cx);
        }
    }

    /// Writes the tabs when they changed, so they are there however the app ends.
    fn save_tabs(&mut self, cx: &mut Context<Self>) {
        let tabs = self.current_saved_tabs(cx);
        if tabs == self.saved_tabs {
            return;
        }
        let saved = self.state.read(cx).engine.save_tabs(&tabs);
        if let Err(error) = saved {
            let message = format!("Could not save tabs.toml: {error:#}");
            let state = self.state.clone();
            // Deferred: this runs while rendering.
            cx.defer(move |cx| {
                state.update(cx, |state, cx| {
                    state.push_notice(NoticeLevel::Important, message, cx)
                })
            });
        }
        self.saved_tabs = tabs;
    }

    // ---- reconnecting ----

    /// Something still needs the host: open tabs, or tunnels that were open.
    fn host_in_use(&self, host: &HostId, cx: &Context<Self>) -> bool {
        self.tabs.iter().any(|tab| tab.host == *host) || self.state.read(cx).has_lost_forwards(host)
    }

    fn stop_reconnecting(&mut self, host: &HostId, cx: &mut Context<Self>) {
        self.reconnect_timers.remove(host);
        self.state
            .update(cx, |state, cx| state.set_reconnect_attempt(host, None, cx));
    }

    fn schedule_reconnect(&mut self, host: &HostId, attempt: u32, cx: &mut Context<Self>) {
        self.state.update(cx, |state, cx| {
            state.set_reconnect_attempt(host, Some(attempt), cx)
        });
        let target = host.clone();
        let delay = reconnect_delay(attempt);
        let timer = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            this.update(cx, |this, cx| {
                this.reconnect_timers.remove(&target);
                if this.host_in_use(&target, cx) {
                    this.state.update(cx, |state, cx| {
                        state.connect_host(&target, cx);
                    });
                } else {
                    this.stop_reconnecting(&target, cx);
                }
            })
            .ok();
        });
        self.reconnect_timers.insert(host.clone(), timer);
    }

    fn on_session_for_reconnect(
        &mut self,
        id: SessionId,
        host: &HostId,
        session: &SessionState,
        cx: &mut Context<Self>,
    ) {
        let attempt = self.state.read(cx).reconnect_attempt(host);
        match session {
            SessionState::Connected => {
                if attempt.is_some() {
                    self.stop_reconnecting(host, cx);
                    let message = format!("{}: reconnected", self.alias(host, cx));
                    self.state.update(cx, |state, cx| {
                        state.push_notice(NoticeLevel::Info, message, cx)
                    });
                }
            }
            // Disconnected on purpose.
            SessionState::Disconnected { reason: None } => self.stop_reconnecting(host, cx),
            SessionState::Disconnected { reason: Some(_) } => {
                // A session already replaced by a newer one does not matter any more.
                let latest = self.state.read(cx).host_session_id(host) == Some(id);
                let wanted =
                    self.state.read(cx).settings.auto_reconnect && self.host_in_use(host, cx);
                if latest && wanted && attempt.is_none() {
                    self.schedule_reconnect(host, 1, cx);
                }
            }
            SessionState::Failed { error } => {
                let Some(attempt) = attempt else {
                    return;
                };
                let temporary = matches!(
                    error.kind,
                    ConnectErrorKind::Transport | ConnectErrorKind::InstanceUnreachable
                );
                if temporary && attempt < RECONNECT_ATTEMPTS {
                    self.schedule_reconnect(host, attempt + 1, cx);
                } else {
                    // Waiting will not fix this: the panel now shows the error and a button.
                    self.stop_reconnecting(host, cx);
                    let message = format!("{}: {}", self.alias(host, cx), error.message);
                    self.state.update(cx, |state, cx| {
                        state.push_notice(NoticeLevel::Error, message, cx)
                    });
                }
            }
            _ => {}
        }
    }

    // ---- events ----

    fn on_sidebar_event(
        &mut self,
        _: &Entity<Sidebar>,
        event: &SidebarEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            SidebarEvent::OpenTerminal(host) => self.open_terminal(host.clone(), window, cx),
            SidebarEvent::OpenFiles(host) => self.open_files(host.clone(), window, cx),
            SidebarEvent::Disconnect(host) => self.state.update(cx, |s, _| s.disconnect_host(host)),
            SidebarEvent::HostSettings(host) => self.host_settings(host, window, cx),
            SidebarEvent::DuplicateHost(host) => self.duplicate_host(host, window, cx),
            SidebarEvent::NewGroup(host) => {
                let state = self.state.clone();
                let host = host.clone();
                let modal = cx.new(|cx| {
                    TextPrompt::new(
                        "New group",
                        "Move here",
                        "",
                        move |group, _, cx| {
                            state.update(cx, |state, cx| {
                                state.set_host_group(&host, Some(group), cx)
                            });
                        },
                        window,
                        cx,
                    )
                    .any_text()
                });
                self.modals.update(cx, |m, cx| m.show(modal, window, cx));
            }
            SidebarEvent::RenameGroup(group) => {
                let state = self.state.clone();
                let old = group.clone();
                let modal = cx.new(|cx| {
                    TextPrompt::new(
                        "Rename group",
                        "Rename",
                        group,
                        move |name, _, cx| {
                            state.update(cx, |state, cx| state.rename_group(&old, &name, cx));
                        },
                        window,
                        cx,
                    )
                    .any_text()
                });
                self.modals.update(cx, |m, cx| m.show(modal, window, cx));
            }
            SidebarEvent::Connect(host) => {
                self.state.update(cx, |s, cx| {
                    s.connect_host(host, cx);
                });
            }
            SidebarEvent::NewHost => self.new_host(window, cx),
            SidebarEvent::OpenSettings => self.open_settings(window, cx),
            SidebarEvent::RestartForUpdate => self.confirm_update_restart(window, cx),
            SidebarEvent::HideSidebar => {
                self.arm_sidebar(cx);
                self.toggle_sidebar(window, cx);
            }
            SidebarEvent::ActivateTab(id) => {
                if let Some(index) = self.tab_index(*id) {
                    self.activate(index, window, cx);
                }
            }
            SidebarEvent::CloseTab(id) => {
                if let Some(index) = self.tab_index(*id) {
                    self.close_tab(index, window, cx);
                }
            }
            SidebarEvent::TabMenu(id, position) => tabs::tab_menu(self, *id, *position, cx),
            SidebarEvent::MoveTab(id, target, after) => {
                let at = |wanted: u64| self.tab_index(wanted);
                if let (Some(from), Some(target)) = (at(*id), at(*target)) {
                    let place = target + usize::from(*after);
                    // Once the tab is out, what follows it is one place back.
                    self.move_tab(*id, if from < place { place - 1 } else { place }, cx);
                }
            }
        }
    }

    /// From one of the app's themes to the other, leaving the system's light and dark out of it.
    fn toggle_theme(&mut self, cx: &mut Context<Self>) {
        let target = if cx.theme().name == theme::builtin::PRUNE {
            theme::builtin::NOCTURNE
        } else {
            theme::builtin::PRUNE
        };
        self.state.update(cx, |state, cx| {
            let mut settings = state.settings.clone();
            settings.theme = target.to_string();
            settings.sync_theme = false;
            if let Err(error) = state.update_settings(settings, cx) {
                state.push_notice(
                    NoticeLevel::Error,
                    format!("Could not save settings: {error:#}"),
                    cx,
                );
            }
        });
    }

    /// The key goes through a connection. Without one, it comes first: its login prompt would stay
    /// behind any modal already open, so the setup appears once it is up.
    fn set_up_key(&mut self, host: &HostId, window: &mut Window, cx: &mut Context<Self>) {
        let session = self.state.update(cx, |s, cx| s.connect_host(host, cx));
        if self.state.read(cx).session_connected(session) {
            self.show_key_setup(host, session, window, cx);
        } else {
            self.key_setup_waiting = Some(host.clone());
        }
    }

    /// Says once per host that a key could replace its password, whether or not it gets taken up.
    fn offer_key_login(&mut self, host: &HostId, cx: &mut Context<Self>) {
        if self
            .state
            .read(cx)
            .settings
            .key_login_offered
            .contains(&host.0)
        {
            return;
        }
        let alias = self.alias(host, cx);
        let (this, id) = (cx.weak_entity(), host.clone());
        self.state.update(cx, |state, cx| {
            state.remember_key_login_offered(&id, cx);
            state.push_action_notice(
                NoticeLevel::Info,
                format!("{alias} logs in with a password. A key would spare you typing it."),
                NoticeAction {
                    label: "Set up key login",
                    run: Rc::new(move |window, cx| {
                        this.update(cx, |this, cx| this.set_up_key(&id, window, cx))
                            .ok();
                    }),
                },
                cx,
            );
        });
    }

    fn show_key_setup(
        &mut self,
        host: &HostId,
        session: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(config) = self.state.read(cx).host(host).cloned() else {
            return;
        };
        let state = self.state.clone();
        let modal = cx.new(|cx| KeySetupModal::new(state, config, session, window, cx));
        self.modals.update(cx, |m, cx| m.show(modal, window, cx));
    }

    /// Restarting closes what is running: with something to lose, it asks first.
    fn confirm_update_restart(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (connections, transfers, restore) = {
            let state = self.state.read(cx);
            (
                state.connected_sessions(),
                state.active_transfers(),
                state.settings.restore_tabs,
            )
        };
        let state = self.state.clone();
        if connections == 0 && transfers == 0 {
            state.update(cx, AppState::restart_into_update);
            return;
        }
        let mut lost = Vec::new();
        match connections {
            0 => {}
            1 => lost.push("1 connection will close".to_string()),
            n => lost.push(format!("{n} connections will close")),
        }
        match transfers {
            0 => {}
            1 => lost.push("1 transfer will stop".to_string()),
            n => lost.push(format!("{n} transfers will stop")),
        }
        let after = if restore {
            "Your tabs open again after the restart."
        } else {
            "Your tabs will not open again."
        };
        let message = format!("{}. {after}", lost.join(" and "));
        let modal = cx.new(|cx| {
            ConfirmModal::new(
                "Restart shcp?",
                message,
                "Restart",
                move |_, cx| state.update(cx, AppState::restart_into_update),
                cx,
            )
        });
        self.modals.update(cx, |m, cx| m.show(modal, window, cx));
    }

    /// The prompt goes in the panel of a tab of the host. With no tab, in a modal.
    fn on_session_for_login(
        &mut self,
        session: SessionId,
        host: &HostId,
        state: &SessionState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (engine, retry) = {
            let app = self.state.read(cx);
            (
                app.engine.clone(),
                app.session(session).is_some_and(|s| s.retrying),
            )
        };
        let Some(ask) = LoginAsk::from_state(state) else {
            self.state
                .update(cx, |s, cx| s.clear_login_prompt(host, session, cx));
            return;
        };
        let prompt = cx.new(|cx| LoginPrompt::new(engine, session, ask, retry, window, cx));
        self.state
            .update(cx, |s, cx| s.set_login_prompt(host, prompt.clone(), cx));
        if self.modals.update(cx, |modals, cx| {
            modals.inline_login(host, &prompt, window, cx)
        }) {
            return;
        }
        if self.modals.read(cx).is_open() || !self.tabs.iter().any(|tab| tab.host == *host) {
            self.show_login_modal(host, prompt, session, window, cx);
        }
    }

    fn show_login_modal(
        &self,
        host: &HostId,
        prompt: Entity<LoginPrompt>,
        session: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let alias = self.alias(host, cx);
        let modal = cx.new(|cx| LoginModal::new(alias, prompt, cx));
        self.modals
            .update(cx, |m, cx| m.show_front(modal, Some(session), window, cx));
    }

    fn on_core_event(
        &mut self,
        _: &Entity<AppState>,
        event: &Event,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            Event::Session { id, host, state } => {
                if !matches!(state, SessionState::Connected) {
                    self.surveyed.remove(host);
                }
                self.on_session_for_reconnect(*id, host, state, cx);
                self.modals
                    .update(cx, |m, cx| m.dismiss_session(*id, window, cx));
                self.on_session_for_login(*id, host, state, window, cx);
                if self.key_setup_waiting.as_ref() == Some(host) {
                    match state {
                        SessionState::Connected => {
                            self.key_setup_waiting = None;
                            self.show_key_setup(host, *id, window, cx);
                        }
                        SessionState::Failed { .. } | SessionState::Disconnected { .. } => {
                            self.key_setup_waiting = None;
                        }
                        _ => {}
                    }
                }
            }
            Event::TerminalTitle { .. }
            | Event::TerminalExited { .. }
            | Event::TerminalFolder { .. } => cx.notify(),
            Event::TerminalClosed { id } => self.on_terminal_closed(*id, window, cx),
            Event::SharedServer { host, live, tabs } => {
                self.on_shared_server(host, live, tabs.as_ref(), window, cx);
            }
            Event::SharedTabsChanged { host, tabs } => {
                self.on_shared_tabs(host, tabs, window, cx);
            }
            Event::EditConflict { session, remote } => {
                let name = remote.rsplit('/').next().unwrap_or(remote);
                let message = format!(
                    "\"{name}\" changed on the server after you opened it. \
                     Overwrite it with your version? Otherwise your save stays only on this PC."
                );
                let engine = self.state.read(cx).engine.clone();
                let (session, remote) = (*session, remote.clone());
                let modal = cx.new(|cx| {
                    ConfirmModal::new(
                        "File changed on the server",
                        message,
                        "Overwrite",
                        move |_, _| engine.overwrite_edit(session, remote),
                        cx,
                    )
                });
                self.modals.update(cx, |m, cx| m.show(modal, window, cx));
            }
            Event::PasswordLogin { host } => self.offer_key_login(host, cx),
            // First sight of a transfer: open the dock.
            Event::Transfer(info)
                if info.state == TransferState::Queued
                    && !self.dock_visible
                    && self.state.read(cx).settings.auto_open_transfers =>
            {
                self.arm_dock(cx);
                self.dock_visible = true;
                self.dock
                    .update(cx, |dock, cx| dock.show(DockTab::Transfers, cx));
                cx.notify();
            }
            _ => {}
        }
    }

    // ---- tabs ----

    fn push_tab(
        &mut self,
        host: HostId,
        label: String,
        panes: Entity<TerminalPanes>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        let observers = vec![cx.observe(&panes, |_, _, cx| cx.notify())];
        self.tabs.push(Tab {
            id,
            host,
            label,
            name: None,
            face: Face::Terminal,
            panes,
            files: None,
            linked: true,
            both_ratio: 0.5,
            followed: None,
            _observers: observers,
        });
        self.activate(self.tabs.len() - 1, window, cx);
    }

    fn new_panes(
        &mut self,
        host: &HostId,
        layout: &SavedPane,
        known: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<TerminalPanes> {
        let state = self.state.clone();
        let host = host.clone();
        let panes = cx.new(|cx| TerminalPanes::new(state, host, layout, known, window, cx));
        self._subscriptions
            .push(cx.subscribe_in(&panes, window, Self::on_panes_event));
        panes
    }

    fn new_files(
        &mut self,
        host: &HostId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<FilesView> {
        let (state, modals) = (self.state.clone(), self.modals.clone());
        let host = host.clone();
        let view = cx.new(|cx| FilesView::new(state, modals, host, window, cx));
        self._subscriptions
            .push(cx.subscribe_in(&view, window, Self::on_files_event));
        view
    }

    /// Shows a tab's place another way. The files are made the first time, at the folder of the
    /// terminal. The terminal is left alone: it may be running something, and a `cd` typed into
    /// that would go to the program.
    pub(crate) fn set_face(
        &mut self,
        index: usize,
        face: Face,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        // The files sit beside one terminal: a split tab keeps its terminals alone.
        if face.shows_files() && tab.panes.read(cx).is_split() {
            let message = "The files go beside one terminal: close the other terminals of this \
                           tab first";
            self.state.update(cx, |state, cx| {
                state.push_notice(NoticeLevel::Info, message.to_string(), cx)
            });
            return;
        }
        let host = tab.host.clone();
        let previous = tab.face;
        if face.shows_files() && self.tabs[index].files.is_none() {
            let files = self.new_files(&host, window, cx);
            // The tab names its folder, and the status bar counts what is in it.
            let observer = cx.observe(&files, |_, _, cx| cx.notify());
            let tab = &mut self.tabs[index];
            tab._observers.push(observer);
            tab.files = Some(files);
        }
        let tab = &mut self.tabs[index];
        tab.face = face;
        // From the terminal to the files: to where the terminal is.
        if face.shows_files() && previous == Face::Terminal {
            let folder = tab.terminal_folder(cx);
            tab.followed.clone_from(&folder);
            if let (Some(files), Some(folder)) = (tab.files.clone(), folder) {
                files.update(cx, |files, cx| files.go_to(folder, cx));
            }
        }
        if self.active == Some(index) {
            self.focus_active(window, cx);
        }
        cx.notify();
    }

    /// Which view the keyboard goes back to with the `Ctrl+\`` cycle.
    fn cycle_face(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.active else {
            return;
        };
        let next = match self.tabs[index].face {
            Face::Terminal => Face::Files,
            Face::Files => Face::Terminal,
        };
        self.set_face(index, next, window, cx);
    }

    fn animated(&self, cx: &App) -> bool {
        self.state.read(cx).settings.animated_selection
    }

    fn arm_sidebar(&mut self, cx: &App) {
        if self.animated(cx) {
            self.sidebar_reveal.arm();
        }
    }

    fn arm_dock(&mut self, cx: &App) {
        if self.animated(cx) {
            self.dock_reveal.arm();
        }
    }

    /// The mouse expands or folds the notice: it glides, if the setting allows it.
    pub(crate) fn show_notices(&mut self, open: bool, cx: &mut Context<Self>) {
        if self.notices_open == open {
            return;
        }
        if self.animated(cx) {
            self.notices_reveal.arm();
        }
        self.notices_open = open;
        // The label goes, and will not hear the pointer leave.
        self.notice_hovered = false;
        if open {
            self.notice_folded = self.state.read(cx).notices.last().map(|notice| notice.id);
        }
        cx.notify();
    }

    /// The files go where the terminal goes.
    fn follow_terminal(&mut self, cx: &mut Context<Self>) {
        let Some(tab) = self.active.and_then(|index| self.tabs.get_mut(index)) else {
            return;
        };
        if !tab.face.shows_files() || !tab.linked {
            return;
        }
        let folder = tab.terminal_folder(cx);
        if folder == tab.followed {
            return;
        }
        tab.followed.clone_from(&folder);
        if let (Some(files), Some(folder)) = (tab.files.clone(), folder) {
            // Deferred: this runs while rendering.
            cx.defer(move |cx| files.update(cx, |files, cx| files.go_to(folder, cx)));
        }
    }

    fn alias(&self, host: &HostId, cx: &Context<Self>) -> String {
        self.state.read(cx).alias(host)
    }

    pub(crate) fn open_terminal(
        &mut self,
        host: HostId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_terminal_with(host, &SavedPane::Terminal { key: None }, false, window, cx);
    }

    fn open_terminal_with(
        &mut self,
        host: HostId,
        layout: &SavedPane,
        known: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let label = self.alias(&host, cx);
        let panes = self.new_panes(&host, layout, known, window, cx);
        self.push_tab(host, label, panes, window, cx);
    }

    /// What the terminals of a tab ask of it: its files, or to go.
    fn on_panes_event(
        &mut self,
        panes: &Entity<TerminalPanes>,
        event: &PanesEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.tabs.iter().position(|tab| tab.panes == *panes) else {
            return;
        };
        match event {
            // Next to the terminal it was asked from.
            PanesEvent::Browse => {
                self.activate(index, window, cx);
                self.set_face(index, Face::Files, window, cx);
            }
            PanesEvent::ToggleFiles => {
                self.activate(index, window, cx);
                self.toggle_files(index, window, cx);
            }
            PanesEvent::CloseTab => self.close_tab(index, window, cx),
            PanesEvent::ShellClosed => {
                if self.active == Some(index) {
                    self.focus_active(window, cx);
                }
                cx.notify();
            }
        }
    }

    /// Terminal actions from the file browser.
    fn on_files_event(
        &mut self,
        view: &Entity<FilesView>,
        event: &FilesEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            FilesEvent::OpenTerminalHere(folder) => {
                let host = view.read(cx).host().clone();
                self.open_terminal_in(host, folder, window, cx);
            }
            FilesEvent::GoHereInTerminal(folder) => {
                let Some(tab) = self
                    .tabs
                    .iter()
                    .find(|tab| tab.files.as_ref() == Some(view))
                else {
                    return;
                };
                let terminal = tab.panes.read(cx).active().clone();
                cd(&terminal, folder, cx);
                window.focus(&terminal.focus_handle(cx));
            }
        }
    }

    /// The files follow the folder of the terminal, or stop following it.
    fn toggle_link(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(tab) = self.tabs.get_mut(index) {
            tab.linked = !tab.linked;
            // Back on: the files catch up with the terminal at the next draw.
            tab.followed = None;
            cx.notify();
        }
    }

    /// A new terminal tab on a host, starting in a folder.
    fn open_terminal_in(
        &mut self,
        host: HostId,
        folder: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_terminal(host, window, cx);
        if let Some(panes) = self.active_panes()
            && !folder.is_empty()
        {
            let terminal = panes.read(cx).active().clone();
            cd(&terminal, folder, cx);
        }
    }

    /// Asks for a name for a tab.
    pub(crate) fn rename_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        let (id, title) = (tab.id, tab.title(cx));
        let this = cx.weak_entity();
        let prompt = cx.new(|cx| {
            TextPrompt::new(
                "Rename tab",
                "Rename",
                &title,
                move |name, _, cx| {
                    this.update(cx, |this, cx| {
                        // By id: tabs may have moved while the question was open.
                        if let Some(index) = this.tab_index(id) {
                            this.set_tab_name(index, Some(name), cx);
                        }
                    })
                    .ok();
                },
                window,
                cx,
            )
            .any_text()
        });
        self.modals.update(cx, |m, cx| m.show(prompt, window, cx));
    }

    pub(crate) fn set_tab_name(
        &mut self,
        index: usize,
        name: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if let Some(tab) = self.tabs.get_mut(index) {
            tab.name = name;
            cx.notify();
        }
    }

    /// A new app host starting as a copy of this one.
    fn duplicate_host(&mut self, id: &HostId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(host) = self.state.read(cx).host(id).cloned() else {
            return;
        };
        let (state, modals) = (self.state.clone(), self.modals.clone());
        let connect = self.host_connector(cx);
        let editor = cx.new(|cx| {
            HostEditor::duplicate(state, modals.clone(), host, window, cx).on_connect(connect)
        });
        modals.update(cx, |m, cx| m.show(editor, window, cx));
    }

    fn active_tab(&self) -> Option<&Tab> {
        self.active.and_then(|i| self.tabs.get(i))
    }

    /// Where the tab with this id is now.
    pub(crate) fn tab_index(&self, id: u64) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
    }

    /// The terminals of the active tab, if there is one.
    fn active_panes(&self) -> Option<Entity<TerminalPanes>> {
        Some(self.active_tab()?.panes.clone())
    }

    fn edit_commands(&mut self, host: &HostId, window: &mut Window, cx: &mut Context<Self>) {
        let (state, host) = (self.state.clone(), host.clone());
        let modal = cx.new(|cx| HostCommandsModal::new(state, host, window, cx));
        self.modals.update(cx, |m, cx| m.show(modal, window, cx));
    }

    pub(crate) fn open_files(&mut self, host: HostId, window: &mut Window, cx: &mut Context<Self>) {
        let existing = self
            .tabs
            .iter()
            .position(|t| t.host == host && t.face.shows_files());
        if let Some(index) = existing {
            return self.activate(index, window, cx);
        }
        self.open_terminal(host, window, cx);
        if let Some(index) = self.active {
            self.set_face(index, Face::Files, window, cx);
            // Asked for the files, so that is where the keyboard goes.
            if let Some(files) = &self.tabs[index].files {
                window.focus(&files.focus_handle(cx));
            }
        }
    }

    fn activate(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }
        self.active = Some(index);
        self.focus_active(window, cx);
        cx.notify();
    }

    fn focus_active(&self, window: &mut Window, cx: &mut Context<Self>) {
        let tab = self.active_tab();
        let handle = match tab {
            Some(tab) => match &tab.files {
                Some(files) if tab.files_alone(cx) => files.focus_handle(cx),
                _ => tab.panes.focus_handle(cx),
            },
            None => self.sidebar.focus_handle(cx),
        };
        window.focus(&handle);
    }

    /// Closes the terminal in focus; the tab goes with its last one.
    fn close_pane_or_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(active) = self.active else {
            return;
        };
        let Some(panes) = self.active_panes() else {
            self.close_tab(active, window, cx);
            return;
        };
        // The last terminal: with the files beside it they stay, as the button of its card does.
        panes.update(cx, |panes, cx| {
            if !panes.close_active(window, cx) {
                panes.close_last(cx);
            }
        });
    }

    pub(crate) fn close_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.drop_tab(index, true, window, cx);
    }

    /// Takes a tab away, ending what runs in it if `kill`. Otherwise it was closed elsewhere.
    fn drop_tab(&mut self, index: usize, kill: bool, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }
        let tab = self.tabs.remove(index);
        if kill {
            tab.panes.read(cx).close(cx);
        } else {
            tab.panes.read(cx).detach(cx);
        }
        self.active = match self.active {
            _ if self.tabs.is_empty() => None,
            Some(active) if active > index => Some(active - 1),
            Some(active) if active == index => {
                Some(index.saturating_sub(1).min(self.tabs.len() - 1))
            }
            other => other,
        };
        self.focus_active(window, cx);
        // That was the last place the host's prompt could be answered in.
        if !self.tabs.iter().any(|t| t.host == tab.host)
            && let Some(prompt) = self.state.read(cx).login_prompt(&tab.host, cx)
        {
            let session = prompt.read(cx).session();
            self.show_login_modal(&tab.host, prompt, session, window, cx);
        }
        cx.notify();
    }

    /// Moves a tab to `to`, keeping the same tab active.
    fn move_tab(&mut self, id: u64, to: usize, cx: &mut Context<Self>) {
        let Some(from) = self.tab_index(id) else {
            return;
        };
        if from == to || to >= self.tabs.len() {
            return;
        }
        let active_id = self.active_tab().map(|t| t.id);
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);
        self.active = active_id.and_then(|id| self.tab_index(id));
        cx.notify();
    }

    /// Host for tab-relative commands: the active tab's, else the selected sidebar host.
    fn current_host(&self, cx: &Context<Self>) -> Option<HostId> {
        self.active
            .and_then(|i| self.tabs.get(i))
            .map(|t| t.host.clone())
            .or_else(|| self.sidebar.read(cx).selected().cloned())
    }

    fn step_tab(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let len = self.tabs.len() as isize;
        if let (Some(active), true) = (self.active, len > 0) {
            self.activate(
                (active as isize + delta).rem_euclid(len) as usize,
                window,
                cx,
            );
        }
    }

    // ---- hosts ----

    fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.state.clone();
        let modals = self.modals.clone();
        let modal = cx.new(|cx| SettingsModal::new(state, modals, window, cx));
        self.modals.update(cx, |m, cx| m.show(modal, window, cx));
    }

    /// The host menu opens its page inside Settings, with navigation back to Hosts.
    fn host_settings(&mut self, host: &HostId, window: &mut Window, cx: &mut Context<Self>) {
        let (state, host) = (self.state.clone(), host.clone());
        let modals = self.modals.clone();
        let modal = cx.new(|cx| SettingsModal::for_host(state, modals, host, window, cx));
        self.modals.update(cx, |m, cx| m.show(modal, window, cx));
    }

    fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // The title bar stays clickable while a modal is open.
        if self.modals.read(cx).is_open() {
            return;
        }
        let items = self.palette_items(cx);
        let palette = cx.new(|cx| CommandPalette::new(items, window, cx));
        self.modals.update(cx, |m, cx| m.show(palette, window, cx));
    }

    fn palette_items(&self, cx: &mut Context<Self>) -> Vec<PaletteItem> {
        let this = cx.entity().downgrade();
        let mut items = Vec::new();
        self.tab_palette_items(&this, &mut items, cx);
        self.host_palette_items(&this, &mut items, cx);
        self.command_palette_items(&this, &mut items, cx);
        self.action_palette_items(this, &mut items, cx);
        items
    }

    /// The open tabs but the active one.
    fn tab_palette_items(
        &self,
        this: &WeakEntity<Self>,
        items: &mut Vec<PaletteItem>,
        cx: &mut Context<Self>,
    ) {
        let active_id = self.active_tab().map(|t| t.id);
        for tab in self.tabs.iter().filter(|tab| Some(tab.id) != active_id) {
            let id = tab.id;
            let state = self.state.read(cx);
            let alias = state.alias(&tab.host);
            let detail = format!("{alias}, {}", tab.face.label().to_lowercase());
            let title = tab.title(cx);
            items.push(PaletteItem {
                section: "Open tabs".into(),
                title: title.into(),
                detail: Some(detail.into()),
                icon: tab.face.icon(),
                dot: None,
                tint: Some(state.host_color(&tab.host)),
                shortcut: None,
                run: handler(this.clone(), move |w, window, cx| {
                    if let Some(index) = w.tab_index(id) {
                        w.activate(index, window, cx);
                    }
                }),
                alt: None,
            });
        }
    }

    /// Every host, to open a terminal on it, or its files.
    fn host_palette_items(
        &self,
        this: &WeakEntity<Self>,
        items: &mut Vec<PaletteItem>,
        cx: &mut Context<Self>,
    ) {
        let hosts = self.state.read(cx).hosts.clone();
        for host in hosts {
            let state = self.state.read(cx);
            let group = state
                .host_group(&host.id)
                .map_or(String::new(), |group| format!(" · {group}"));
            let target = format!("{}{group}", host.target());
            let connected = matches!(
                state.host_session(&host.id).map(|s| &s.state),
                Some(SessionState::Connected)
            );
            let color = state.host_color(&host.id);
            let (terminal, files) = (host.id.clone(), host.id.clone());
            items.push(PaletteItem {
                section: "Hosts".into(),
                title: host.alias.clone().into(),
                detail: Some(target.into()),
                icon: IconName::Terminal,
                dot: Some((color, connected)),
                tint: None,
                shortcut: None,
                run: handler(this.clone(), move |w, window, cx| {
                    w.open_terminal(terminal.clone(), window, cx)
                }),
                alt: Some(handler(this.clone(), move |w, window, cx| {
                    w.open_files(files.clone(), window, cx)
                })),
            });
        }
    }

    /// Saved commands of the host in the active terminal, ready to run.
    fn command_palette_items(
        &self,
        this: &WeakEntity<Self>,
        items: &mut Vec<PaletteItem>,
        cx: &mut Context<Self>,
    ) {
        if let Some(panes) = self.active_panes()
            && let Some(tab) = self.active_tab()
        {
            let state = self.state.read(cx);
            let alias = state.alias(&tab.host);
            let section: gpui::SharedString = format!("Saved commands on {alias}").into();
            let snippets: Vec<_> = state.settings.snippets_for(&tab.host).cloned().collect();
            for snippet in snippets {
                let panes = panes.clone();
                let command = snippet.command.clone();
                items.push(PaletteItem {
                    section: section.clone(),
                    title: snippet.name.clone().into(),
                    detail: Some(snippet.command.clone().into()),
                    icon: IconName::Play,
                    dot: None,
                    tint: None,
                    shortcut: None,
                    run: Rc::new(move |_, cx| {
                        let view = panes.read(cx).active().clone();
                        view.update(cx, |view, cx| view.run_command(&command, cx));
                    }),
                    alt: None,
                });
            }
            let host = tab.host.clone();
            items.push(PaletteItem {
                section,
                title: "Commands of this host".into(),
                detail: None,
                icon: IconName::Pencil,
                dot: None,
                tint: None,
                shortcut: None,
                run: handler(this.clone(), move |w, window, cx| {
                    w.edit_commands(&host, window, cx)
                }),
                alt: None,
            });
        }
    }

    /// The actions of the app.
    fn action_palette_items(
        &self,
        this: WeakEntity<Self>,
        items: &mut Vec<PaletteItem>,
        cx: &mut Context<Self>,
    ) {
        let mut command = |title: &'static str, icon: IconName, shortcut, run| {
            items.push(PaletteItem {
                section: "Actions".into(),
                title: title.into(),
                detail: None,
                icon,
                dot: None,
                tint: None,
                shortcut,
                run,
                alt: None,
            });
        };
        // The files sit beside one terminal: no splitting while they show.
        let files_shown = self.active_tab().is_some_and(|tab| tab.face.shows_files());
        if let Some(panes) = self.active_panes().filter(|_| !files_shown) {
            for (title, icon, shortcut, axis) in tabs::SPLITS {
                let panes = panes.clone();
                command(
                    title,
                    icon,
                    Some(shortcut),
                    Rc::new(move |window, cx| {
                        panes.update(cx, |panes, cx| panes.split(axis, window, cx));
                    }),
                );
            }
        }
        command(
            "Settings",
            IconName::Settings,
            Some("Ctrl+,"),
            handler(this.clone(), Workspace::open_settings),
        );
        command(
            "New host",
            IconName::Plus,
            Some("Ctrl+Shift+N"),
            handler(this.clone(), Workspace::new_host),
        );
        let (title, icon) = if cx.theme().name == theme::builtin::PRUNE {
            ("Switch to Nocturne", IconName::Moon)
        } else {
            ("Switch to Prune", IconName::Sun)
        };
        command(
            title,
            icon,
            None,
            handler(this.clone(), |w, _, cx| w.toggle_theme(cx)),
        );
        command(
            "Toggle sidebar",
            IconName::PanelLeft,
            Some("Ctrl+Shift+B"),
            handler(this.clone(), Workspace::toggle_sidebar),
        );
        command(
            "Toggle transfers",
            IconName::Swap,
            Some("Ctrl+Shift+J"),
            handler(this.clone(), |w, _, cx| {
                w.toggle_dock(DockTab::Transfers, cx)
            }),
        );
        command(
            "Toggle ports",
            IconName::Link,
            None,
            handler(this.clone(), |w, _, cx| w.toggle_dock(DockTab::Ports, cx)),
        );
        command(
            "Reload ~/.ssh/config",
            IconName::Refresh,
            None,
            handler(this, |w, _, cx| {
                w.state.read(cx).engine.reload_hosts();
            }),
        );
    }

    fn toggle_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sidebar_visible = !self.sidebar_visible;
        if self.sidebar_visible {
            window.focus(&self.sidebar.focus_handle(cx));
        } else {
            self.focus_active(window, cx);
        }
        cx.notify();
    }

    /// Shows a dock tab, or hides the dock if that tab is already showing.
    fn toggle_dock(&mut self, tab: DockTab, cx: &mut Context<Self>) {
        if self.dock_visible && self.dock.read(cx).tab() == tab {
            self.dock_visible = false;
        } else {
            self.dock_visible = true;
            self.dock.update(cx, |dock, cx| dock.show(tab, cx));
        }
        cx.notify();
    }

    /// Closes every tab except `keep`.
    pub(crate) fn close_others(
        &mut self,
        keep: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(keep_id) = self.tabs.get(keep).map(|t| t.id) else {
            return;
        };
        let doomed: Vec<u64> = self
            .tabs
            .iter()
            .filter(|t| t.id != keep_id)
            .map(|t| t.id)
            .collect();
        for id in doomed {
            if let Some(index) = self.tab_index(id) {
                self.close_tab(index, window, cx);
            }
        }
        if let Some(index) = self.tab_index(keep_id) {
            self.activate(index, window, cx);
        }
    }

    fn new_host(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (state, modals) = (self.state.clone(), self.modals.clone());
        let connect = self.host_connector(cx);
        let editor = cx
            .new(|cx| HostEditor::new(state, modals.clone(), None, window, cx).on_connect(connect));
        modals.update(cx, |m, cx| m.show(editor, window, cx));
    }

    /// What "Save & connect" does with the host it just saved: the same as opening any host.
    fn host_connector(&self, cx: &mut Context<Self>) -> ConnectHandler {
        let workspace = cx.entity().downgrade();
        Rc::new(move |host, window, cx| {
            workspace
                .update(cx, |workspace, cx| {
                    workspace.open_terminal(host, window, cx)
                })
                .ok();
        })
    }

    /// Reads `~/.ssh/config` again. With nothing in it, opens it so that hosts can be added there.
    fn import_ssh_config(&mut self, cx: &mut Context<Self>) {
        let engine = self.state.read(cx).engine.clone();
        engine.reload_hosts();
        if !engine.hosts().is_empty() {
            return;
        }
        let path = engine.paths().ssh_config;
        self.state.update(cx, |state, cx| {
            state.push_notice(
                NoticeLevel::Info,
                format!("No Host entries in {}", path.display()),
                cx,
            )
        });
        if path.exists() {
            cx.open_with_system(&path);
        } else if let Some(folder) = path.parent().filter(|folder| folder.exists()) {
            cx.open_with_system(folder);
        }
    }

    // ---- rendering ----

    /// Under the panel: the connection of this tab, what the folder holds, and the dock's buttons.
    fn status_bar(&self, sidebar_amount: f32, cx: &Context<Self>) -> Option<AnyElement> {
        let tab = self.active_tab()?;
        let state = self.state.read(cx);
        let theme = cx.theme();
        let session = state.host_session(&tab.host);
        let reconnecting = state.reconnect_attempt(&tab.host).is_some();
        let text = if reconnecting {
            "Reconnecting"
        } else {
            match session.map(|session| &session.state) {
                Some(SessionState::Connected) => "Connected",
                Some(SessionState::Failed { .. }) => "Failed",
                Some(SessionState::Disconnected { .. }) | None => "Not connected",
                Some(SessionState::Connecting { .. }) => "Connecting",
                Some(_) => "Waiting for you",
            }
        };
        let target = state.host(&tab.host).map(HostConfig::target);
        let (summary, hint) = match (&tab.files, tab.face.shows_files()) {
            (Some(view), true) => {
                let view = view.read(cx);
                (view.summary(), view.summary_hint())
            }
            _ => (None, None),
        };
        Some(
            div()
                .h(px(STATUS_BAR_HEIGHT))
                .flex_none()
                .flex()
                .items_center()
                .gap(px(12.))
                .pl(px(6. + 8. * (1. - sidebar_amount)))
                .pr(px(10.))
                .text_size(crate::theme::ui_px(13.))
                .text_color(theme.text_disabled)
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(px(7.))
                        .text_color(theme.text_muted)
                        .child(status_dot(theme, session.map(|session| &session.state)))
                        .child(text),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        // A file selected stands out from the hint after it.
                        .when(hint.is_some(), |this| this.text_color(theme.text_muted))
                        .child(summary.or(target).unwrap_or_default()),
                )
                .children(hint.map(|hint| {
                    div()
                        .flex_none()
                        .text_color(theme.text_disabled)
                        .child(hint)
                }))
                .child(div().flex_1().min_w_0())
                .child(self.dock_buttons(&tab.host, cx))
                .into_any_element(),
        )
    }

    /// The buttons that show the dock's tabs.
    fn dock_buttons(&self, host: &HostId, cx: &Context<Self>) -> impl IntoElement {
        let state = self.state.read(cx);
        let theme = cx.theme();
        let dock_tab = self.dock.read(cx).tab();
        let session = state.host_session(host);
        let open_tunnels = session.map_or(0, |session| {
            session
                .forwards
                .iter()
                .filter(|forward| matches!(forward.state, ForwardState::Listening))
                .count()
        });
        let pill = |id: &'static str, shown: bool| -> Stateful<Div> {
            let (active, hover, text) = (theme.track, theme.track, theme.text);
            div()
                .id(id)
                .h(px(24.))
                .min_w(px(26.))
                .px(px(6.))
                .justify_center()
                .flex()
                .flex_none()
                .items_center()
                .gap(px(6.))
                .rounded(px(crate::theme::RADIUS_CONTROL))
                .cursor_pointer()
                .text_color(theme.text_muted)
                .when(shown, |this| this.bg(active).text_color(text))
                .hover(move |style| style.bg(hover).text_color(text))
        };
        let count = |text: String| {
            div()
                .font_family(crate::theme::mono_font())
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_size(crate::theme::ui_px(12.))
                .text_color(theme.accent)
                .child(text)
        };
        let ports_shown = self.dock_visible && dock_tab == DockTab::Ports;
        let transfers_shown = self.dock_visible && dock_tab == DockTab::Transfers;
        let running = state.active_transfers();
        let percent = state.transfers_percent();
        let leading = match percent {
            Some(percent) if running > 0 => ProgressRing::new(percent, px(14.)).into_any_element(),
            _ => Icon::new(IconName::Swap).size(px(13.)).into_any_element(),
        };
        let figure = match (running, percent) {
            (0, _) => None,
            (_, Some(percent)) => Some(format!("{percent}%")),
            (n, None) => Some(n.to_string()),
        };
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(4.))
            .child(
                pill("toggle-ports", ports_shown)
                    .tooltip(tooltip("Ports"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.arm_dock(cx);
                        this.toggle_dock(DockTab::Ports, cx)
                    }))
                    .child(Icon::new(IconName::Link).size(px(13.)))
                    .when(open_tunnels > 0, |this| {
                        this.child(count(open_tunnels.to_string()))
                    }),
            )
            .child(
                pill("toggle-transfers", transfers_shown)
                    .tooltip(tooltip(if transfers_shown {
                        "Hide the transfers (Ctrl+Shift+J)"
                    } else {
                        "Transfers (Ctrl+Shift+J)"
                    }))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.arm_dock(cx);
                        this.toggle_dock(DockTab::Transfers, cx)
                    }))
                    .child(leading)
                    .children(figure.map(count)),
            )
    }

    /// Shows the files beside the terminals, or hides them. From the mouse: the view fades in.
    fn toggle_files(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        // The files are all that is left: hiding them leaves nothing, so the tab goes.
        if tab.files_alone(cx) {
            self.close_tab(index, window, cx);
            return;
        }
        let face = if tab.face.shows_files() {
            Face::Terminal
        } else {
            Face::Files
        };
        if self.animated(cx) {
            let count = self.face_fade.map_or(0, |(_, count)| count + 1);
            self.face_fade = Some((tab.id, count));
        }
        self.set_face(index, face, window, cx);
    }

    /// The card of a tab's files: where they are, and what acts on them. What the details used to
    /// show now floats over the list, or sits in the status bar.
    fn files_card(
        &mut self,
        index: usize,
        files: Entity<FilesView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let tab = &self.tabs[index];
        let color = theme.host_color(self.state.read(cx).host_color(&tab.host));
        let hold = files.read(cx).holds_tools(window, cx);
        let tools_shown = self.files_fade.value(hold);
        if self.files_fade.moving() {
            window.request_animation_frame();
        }
        let (nav, crumbs, tools) = files.update(cx, |view, cx| {
            (view.nav_buttons(cx), view.crumbs(cx), view.tools(cx))
        });
        let alone = self.tabs.get(index).is_some_and(|tab| tab.files_alone(cx));
        let close = Button::new("files-close")
            .icon(IconName::Close)
            .tooltip(if alone {
                "Close tab (Ctrl+Shift+W)"
            } else {
                "Hide the files (Ctrl+Shift+E shows them)"
            })
            .style(ButtonStyle::Ghost)
            .on_click(cx.listener(move |this, _, window, cx| this.toggle_files(index, window, cx)))
            .into_any_element();
        let header = panel::header(
            tools_shown,
            vec![
                tile(Mark::Icon(IconName::Folder), color, None, &theme).into_any_element(),
                crumbs,
            ],
            vec![nav, tools, close],
        );
        // Alone, a card has nothing to stand out from.
        let focused =
            !self.tabs[index].files_alone(cx) && files.read(cx).contains_focus(window, cx);
        panel::frame(&theme, theme.bg, focused)
            .id("files-card")
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                let animated = this.animated(cx);
                this.files_fade.hover(*hovered, animated);
                cx.notify();
            }))
            .size_full()
            .overflow_hidden()
            .child(header)
            .child(div().flex_1().min_h_0().w_full().child(files))
            .into_any_element()
    }

    fn empty_state(&self, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let shortcut = |keys: &'static str, what: &'static str| {
            div()
                .w(px(320.))
                .h(px(28.))
                .flex()
                .items_center()
                .justify_between()
                .child(what)
                .child(
                    div()
                        .flex()
                        .gap(px(3.))
                        .children(keys.split('+').map(|key| {
                            div()
                                .h(px(20.))
                                .min_w(px(20.))
                                .px(px(5.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(crate::theme::RADIUS_SMALL))
                                .border_1()
                                .border_color(theme.border)
                                .bg(theme.surface)
                                .text_size(crate::theme::ui_px(12.))
                                .text_color(theme.text_muted)
                                .child(key)
                        })),
                )
        };
        let no_hosts = self.state.read(cx).hosts.is_empty();
        // One entrance, seen once per run: logo and title lead, the text follows them, and the
        // shortcuts only fade, as a block. All done in 290 ms.
        const LEAD: Beat = Beat::new(240, 8.);
        const FOLLOW: Beat = Beat::new(220, 5.).after(40);
        const SHORTCUTS: Beat = Beat::new(200, 0.).after(90);
        let intro = self.first_empty_state;
        let beat = |element: Div, id: &'static str, beat: Beat| {
            if intro {
                motion::enter(element, id, beat).into_any_element()
            } else {
                element.into_any_element()
            }
        };
        let hint = if no_hosts {
            "Add your first server, or bring in the Host entries of ~/.ssh/config"
        } else {
            "Click a host to open a terminal, or right-click it for more"
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .text_size(crate::theme::ui_px(13.))
            .text_color(theme.text_muted)
            .child(beat(
                div()
                    .pb(px(14.))
                    .child(title_bar::logo(px(40.), theme.text)),
                "empty-logo",
                LEAD,
            ))
            .child(beat(
                div()
                    .pb(px(4.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_size(crate::theme::ui_px(18.))
                    .text_color(theme.text)
                    .child(if no_hosts {
                        "No hosts yet"
                    } else {
                        "No open tabs"
                    }),
                "empty-title",
                LEAD,
            ))
            .child(beat(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(14.))
                    .pb(px(22.))
                    .child(hint)
                    .when(no_hosts, |this| {
                        this.child(
                            div()
                                .flex()
                                .gap(px(8.))
                                .child(
                                    Button::new("empty-new-host")
                                        .icon(IconName::Plus)
                                        .label("Add host")
                                        .style(ButtonStyle::Primary)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.new_host(window, cx)
                                        })),
                                )
                                .child(
                                    Button::new("empty-import-config")
                                        .icon(IconName::FolderUp)
                                        .label("Import from ~/.ssh/config")
                                        .on_click(
                                            cx.listener(|this, _, _, cx| {
                                                this.import_ssh_config(cx)
                                            }),
                                        ),
                                ),
                        )
                    }),
                "empty-hint",
                FOLLOW,
            ))
            // Only what works with no tab open; the rest is on the tooltips and in the palette.
            .child(beat(
                div()
                    .child(shortcut("Ctrl+Shift+K", "Search everything"))
                    .child(shortcut("Ctrl+Shift+N", "New host"))
                    .when(!no_hosts, |this| {
                        this.child(shortcut("Ctrl+Shift+T", "New terminal"))
                            .child(shortcut("Ctrl+Shift+E", "Browse files"))
                    })
                    .child(shortcut("Ctrl+Shift+B", "Toggle sidebar"))
                    .child(shortcut("Ctrl+,", "Settings")),
                "empty-shortcuts",
                SHORTCUTS,
            ))
            .into_any_element()
    }

    /// The cards of the tab: one per terminal and, beside them, one for the files. A connection
    /// still on its way covers them all.
    fn tab_content(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(index) = self.active.filter(|index| *index < self.tabs.len()) else {
            return self.empty_state(cx);
        };
        let tab = &self.tabs[index];
        let theme = cx.theme().clone();
        let ratio = tab.both_ratio;
        let linked = tab.linked;
        let bounds = self.both_bounds.clone();
        let alone = tab.files_alone(cx);
        let panes = tab.panes.clone();
        let state = self.state.read(cx);
        let session = state.host_session(&tab.host);
        let focus = if tab.face.shows_files() {
            tab.files.as_ref().map(|files| files.focus_handle(cx))
        } else {
            None
        }
        .unwrap_or_else(|| panes.read(cx).active().focus_handle(cx));
        let reconnecting = panes.read(cx).had_session()
            || tab
                .files
                .as_ref()
                .is_some_and(|files| !files.read(cx).path().is_empty());
        let heading = state.host(&tab.host).map(session_overlay::heading);
        let mut overlay = session_overlay::with_reconnect(
            session_overlay::overlay_for(session),
            state.reconnect_attempt(&tab.host),
        );
        let steps = session_overlay::steps_of(session);
        let shell_steps = session_overlay::shell_steps(session);
        if let Some(prompt) = state.login_prompt(&tab.host, cx) {
            let own = crate::login_prompt::present(&prompt, cx.entity_id(), &focus, window, cx);
            overlay = Some(Overlay::Login { steps, prompt, own });
        }
        if overlay.is_none() && !tab.face.shows_files() && panes.read(cx).initial_shell_pending(cx)
        {
            overlay = Some(Overlay::StartingShell { steps: shell_steps });
        }
        let retry_panes = panes.clone();
        let host = tab.host.clone();
        let overlay = overlay.map(|overlay| {
            session_overlay::render(
                overlay,
                Surface {
                    heading,
                    reconnecting,
                    docked: false,
                    background: theme.scrim,
                    focus,
                    shell_step: !tab.face.shows_files(),
                    owner: cx.entity_id(),
                },
                &theme,
                window,
                cx,
                cx.listener(move |this, _, _, cx| {
                    this.stop_reconnecting(&host, cx);
                    retry_panes.update(cx, TerminalPanes::retry);
                }),
            )
        });
        let files = tab.files.clone().filter(|_| tab.face.shows_files());
        let content = match files {
            Some(files) if alone => self.files_card(index, files, window, cx),
            Some(files) => {
                let files = self.files_card(index, files, window, cx);
                div()
                    .size_full()
                    .relative()
                    .flex()
                    .child(
                        gpui::canvas(move |area, _, _| bounds.set(Some(area)), |_, _, _, _| {})
                            .absolute()
                            .size_full(),
                    )
                    .child(div().flex_none().w(relative(ratio)).h_full().child(panes))
                    .child(
                        // The gap between the terminals and the files is what to grab.
                        div()
                            // Not occluding: the window must keep hearing the pointer on it.
                            .id("both-divider")
                            .w(px(panel::CARD_GAP))
                            .h_full()
                            .flex_none()
                            .cursor(CursorStyle::ResizeLeftRight)
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| {
                                    this.resizing_both = true;
                                    cx.stop_propagation();
                                    cx.notify();
                                }),
                            )
                            .on_click(cx.listener(|this, event: &gpui::ClickEvent, _, cx| {
                                if event.click_count() >= 2
                                    && let Some(tab) =
                                        this.active.and_then(|i| this.tabs.get_mut(i))
                                {
                                    tab.both_ratio = 0.5;
                                    cx.notify();
                                }
                            })),
                    )
                    .child(div().flex_1().min_w_0().h_full().child(files))
                    // Last, so it sits over both cards and not under the files.
                    .child(link_bridge(
                        &theme,
                        linked,
                        ratio,
                        cx.listener(move |this, _, _, cx| this.toggle_link(index, cx)),
                    ))
                    .into_any_element()
            }
            None => panes.into_any_element(),
        };
        div()
            .relative()
            .size_full()
            .child(content)
            .children(overlay)
            .into_any_element()
    }

    /// In the middle of the top of the window: leads to the palette, where typing happens. Around
    /// it the strip still moves the window. Only words at rest, so the top stays quiet; the
    /// capsule shows under the pointer.
    fn search_field(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let keys = short_keys(&crate::actions::with_current_keys("Ctrl+Shift+K", cx));
        let (hover, pressed, edge) = (theme.hover, theme.active, theme.border);
        div()
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .h(px(title_bar::HEIGHT))
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .id("window-search")
                    .occlude()
                    .max_w(relative(0.4))
                    .h(px(26.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    // A way to go somewhere: a capsule, as the rows of the sidebar, once the
                    // pointer is on it. The edge is there all along, unseen, so nothing moves.
                    .rounded_full()
                    .border_1()
                    .border_color(gpui::transparent_black())
                    .cursor_pointer()
                    .text_size(crate::theme::ui_px(12.5))
                    .text_color(theme.text_muted)
                    .hover(move |style| style.bg(hover).border_color(edge))
                    .active(move |style| style.bg(pressed).border_color(edge))
                    .on_click(cx.listener(|this, _, window, cx| this.open_palette(window, cx)))
                    .child(
                        Icon::new(IconName::Search)
                            .size(px(13.))
                            .color(theme.text_muted),
                    )
                    .child(div().min_w_0().truncate().child("Go to anything"))
                    .child(
                        div()
                            .flex_none()
                            .ml(px(4.))
                            .h(px(17.))
                            .px(px(5.))
                            .flex()
                            .items_center()
                            .rounded(px(4.))
                            .border_1()
                            .border_color(theme.border)
                            .font_family(crate::theme::mono_font())
                            .text_size(crate::theme::ui_px(11.))
                            .text_color(theme.text_disabled)
                            .child(keys),
                    ),
            )
    }

    /// The divider follows the pointer. A button let go where nothing listens ends the drag here.
    fn drag_both_divider(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if event.pressed_button != Some(MouseButton::Left) {
            self.resizing_both = false;
            cx.notify();
            return;
        }
        let Some(area) = self.both_bounds.get() else {
            return;
        };
        let share = f32::from(event.position.x - area.origin.x) / f32::from(area.size.width);
        if let Some(share) = both_ratio(share)
            && let Some(tab) = self.active.and_then(|index| self.tabs.get_mut(index))
        {
            tab.both_ratio = share;
            cx.notify();
        }
    }
}

/// Across the gap between the terminal and its files, halfway down: whether the files follow the
/// folder of the terminal, and a click to change it. A capsule that bridges the two cards while
/// they go together; broken, and plain, when they do not.
fn link_bridge(
    theme: &crate::theme::Theme,
    linked: bool,
    // The share of the terminals, where the gap starts.
    ratio: f32,
    toggle: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    const WIDTH: f32 = 34.;
    const HEIGHT: f32 = 20.;
    let (fill, edge, icon, tip) = if linked {
        (
            gpui::Hsla {
                a: 0.16,
                ..theme.accent
            },
            gpui::Hsla {
                a: 0.45,
                ..theme.accent
            },
            theme.accent,
            "The files follow the folder of the terminal",
        )
    } else {
        (
            theme.bg,
            theme.strong_border,
            theme.text_disabled,
            "The files stay where you take them",
        )
    };
    let hover = theme.hover;
    div()
        .id("link-faces")
        .occlude()
        .absolute()
        .top(relative(0.5))
        .mt(px(-HEIGHT / 2.))
        .left(relative(ratio))
        .ml(px((panel::CARD_GAP - WIDTH) / 2.))
        .w(px(WIDTH))
        .h(px(HEIGHT))
        .rounded_full()
        // The ground of the window under the tint: what is behind it, like the edge of a card,
        // does not show through.
        .bg(theme.bg)
        .border_1()
        .border_color(edge)
        .cursor_pointer()
        .tooltip(tooltip(tip))
        // Its own press: not the start of a resize of the gap under it.
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(toggle)
        .child(
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(fill)
                .when(!linked, move |this| {
                    this.hover(move |style| style.bg(hover))
                })
                .child(
                    Icon::new(if linked {
                        IconName::Chain
                    } else {
                        IconName::Unlink
                    })
                    .size(px(12.))
                    .color(icon),
                ),
        )
        .into_any_element()
}

/// Keys as the search field shows them, short: `Ctrl+Shift+K` is `Ctrl ⇧ K`, Shift taking its
/// symbol, every key apart.
fn short_keys(keys: &str) -> String {
    keys.split('+')
        .map(|key| if key == "Shift" { "⇧" } else { key })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The place of the window now, in the form the settings keep.
fn saved_window(window: &Window) -> SavedWindow {
    let (bounds, maximized) = match window.window_bounds() {
        WindowBounds::Windowed(bounds) | WindowBounds::Fullscreen(bounds) => (bounds, false),
        WindowBounds::Maximized(bounds) => (bounds, true),
    };
    SavedWindow {
        x: f32::from(bounds.origin.x),
        y: f32::from(bounds.origin.y),
        width: f32::from(bounds.size.width),
        height: f32::from(bounds.size.height),
        maximized,
    }
}

impl Focusable for Workspace {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Workspace {
    /// Plain data for the sidebar, which draws right after this.
    fn sync_open_tabs(&self, cx: &mut Context<Self>) {
        let active_id = self.active_tab().map(|t| t.id);
        let open: Vec<OpenTab> = self
            .tabs
            .iter()
            .map(|tab| {
                let state = self.state.read(cx);
                let target = state
                    .host(&tab.host)
                    .map_or(tab.host.0.clone(), HostConfig::target);
                let user = state.host(&tab.host).map_or("", |h| h.user.as_str());
                OpenTab {
                    id: tab.id,
                    host: tab.host.clone(),
                    name: tab.name.clone(),
                    folder: tab
                        .folder(cx)
                        .map(|folder| tabs::short_folder(&folder, user)),
                    title: tab.title(cx),
                    detail: match tab.folder(cx) {
                        Some(folder) => format!("{folder}\n{target}"),
                        None => target,
                    },
                    face: tab.face,
                    split: tab.panes.read(cx).is_split(),
                    active: Some(tab.id) == active_id,
                }
            })
            .collect();
        self.sidebar.update(cx, |sidebar, _| sidebar.open = open);
        // For the button of the terminals' cards, which draw right after this too.
        for tab in &self.tabs {
            let shown = tab.face.shows_files();
            tab.panes.update(cx, |panes, _| panes.files_shown = shown);
        }
    }

    /// The active host, the saved tabs and the shared ones, from this frame.
    fn sync_shared(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // The active host is known only here. Deferred: an entity cannot change mid-render.
        let host = self.current_host(cx);
        if self.state.read(cx).active_host != host {
            let state = self.state.clone();
            cx.defer(move |cx| state.update(cx, |state, cx| state.set_active_host(host, cx)));
        }
        if !self.resizing_both {
            self.save_tabs(cx);
        }
        self.publish_shared(cx);
        let dragging =
            self.resizing_both || self.tabs.iter().any(|tab| tab.panes.read(cx).is_dragging());
        if !dragging && !self.pending_shared.is_empty() {
            cx.defer_in(window, |this, window, cx| {
                this.apply_pending_shared(window, cx)
            });
        }
    }

    /// Whether the notices are open, and kept while they are or the pointer is on one.
    fn sync_notices(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.first_empty_state &= self.tabs.is_empty();
        self.notices_open &= self.state.read(cx).notices.iter().any(|n| !n.leaving);
        self.notice_hovered &= !self.state.read(cx).notices.is_empty();
        self.notices_reveal.set(self.notices_open);
        if self.notices_reveal.moving() {
            window.request_animation_frame();
        }
        self.state
            .read(cx)
            .notices_held
            .set(self.notices_open || self.notice_hovered);
    }

    /// The active tab's content, fading in when its view was chosen with the mouse.
    fn faded_tab_content(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let content = self.tab_content(window, cx);
        // A view chosen with the mouse fades in. Only then: tabs and the keyboard change at once.
        let fade = self
            .active_tab()
            .and_then(|tab| self.face_fade.filter(|(id, _)| *id == tab.id))
            .map(|(_, count)| count);
        match fade {
            Some(count) => motion::enter(
                div().size_full().child(content),
                ("tab-face-in", count),
                motion::FACE,
            )
            .into_any_element(),
            None => content,
        }
    }

    /// The window's own actions.
    fn with_actions(el: Stateful<Div>, cx: &mut Context<Self>) -> Stateful<Div> {
        el.on_action(cx.listener(|this, _: &NewTerminal, window, cx| {
            if let Some(host) = this.current_host(cx) {
                this.open_terminal(host, window, cx);
            }
        }))
        .on_action(cx.listener(|this, _: &OpenFiles, window, cx| {
            if let Some(host) = this.current_host(cx) {
                this.open_files(host, window, cx);
            }
        }))
        .on_action(cx.listener(|this, _: &CloseTab, window, cx| this.close_pane_or_tab(window, cx)))
        .on_action(cx.listener(|this, _: &NextTab, window, cx| this.step_tab(1, window, cx)))
        .on_action(cx.listener(|this, _: &PrevTab, window, cx| this.step_tab(-1, window, cx)))
        .on_action(
            cx.listener(|this, _: &ToggleSidebar, window, cx| this.toggle_sidebar(window, cx)),
        )
        .on_action(
            cx.listener(|this, _: &ToggleTransfers, _, cx| {
                this.toggle_dock(DockTab::Transfers, cx)
            }),
        )
        .on_action(cx.listener(|this, _: &OpenPalette, window, cx| this.open_palette(window, cx)))
        .on_action(cx.listener(|this, _: &CycleFace, window, cx| this.cycle_face(window, cx)))
        .on_action(cx.listener(|this, _: &NewHost, window, cx| {
            this.modals.update(cx, |m, _| m.skip_next_entrance());
            this.new_host(window, cx);
        }))
        .on_action(cx.listener(|this, _: &OpenSettings, window, cx| {
            this.modals.update(cx, |m, _| m.skip_next_entrance());
            this.open_settings(window, cx);
        }))
        .on_action(cx.listener(|this, _: &ResetFont, _, cx| {
            this.state.update(cx, AppState::reset_font_size)
        }))
    }

    /// While a divider is dragged, the pointer anywhere in the window moves it.
    fn with_resize_drags(&self, el: Stateful<Div>, cx: &mut Context<Self>) -> Stateful<Div> {
        el.when(self.resizing_both, |this| {
            this.cursor(CursorStyle::ResizeLeftRight)
                .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                    this.drag_both_divider(event, cx)
                }))
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseUpEvent, _, cx| {
                        this.resizing_both = false;
                        cx.notify();
                    }),
                )
        })
        .when(self.resizing_sidebar, |this| {
            this.cursor(CursorStyle::ResizeLeftRight)
                .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                    let x = f32::from(event.position.x);
                    this.state
                        .update(cx, |s, cx| s.set_sidebar_width(x, false, cx));
                }))
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseUpEvent, _, cx| {
                        this.resizing_sidebar = false;
                        let width = this.state.read(cx).settings.sidebar_width;
                        this.state
                            .update(cx, |s, cx| s.set_sidebar_width(width, true, cx));
                        cx.notify();
                    }),
                )
        })
    }

    /// The sidebar, clipped while it slides, with the handle that resizes it.
    fn sidebar_column(
        &self,
        theme: &theme::Theme,
        sidebar_size: f32,
        sidebar_amount: f32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        div()
            .h_full()
            .flex_none()
            .overflow_hidden()
            .w(px(sidebar_size * sidebar_amount))
            .child(
                div()
                    .h_full()
                    .w(px(sidebar_size))
                    .flex()
                    .child(self.sidebar.clone())
                    .child(
                        // Drag handle on the sidebar edge; double-click resets the width.
                        div()
                            .id("sidebar-resize")
                            .group("sidebar-resize")
                            .w(px(8.))
                            .h_full()
                            .flex_none()
                            .flex()
                            .justify_center()
                            .cursor(CursorStyle::ResizeLeftRight)
                            .child(
                                div()
                                    .w(px(2.))
                                    .h_full()
                                    .when(self.resizing_sidebar, |this| this.bg(theme.accent))
                                    .group_hover("sidebar-resize", {
                                        let color = theme.border;
                                        move |s| s.bg(color)
                                    }),
                            )
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| {
                                    this.resizing_sidebar = true;
                                    cx.notify();
                                }),
                            )
                            .on_click(cx.listener(|this, event: &gpui::ClickEvent, _, cx| {
                                if event.click_count() >= 2 {
                                    let width = shcp_core::Settings::default().sidebar_width;
                                    this.state
                                        .update(cx, |s, cx| s.set_sidebar_width(width, true, cx));
                                }
                            })),
                    ),
            )
    }

    /// The title strip, the cards with the notices over them, the dock and the status bar.
    fn main_column(
        &self,
        theme: &theme::Theme,
        content: AnyElement,
        status_bar: Option<AnyElement>,
        sidebar_amount: f32,
        dock_amount: f32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let has_status_bar = status_bar.is_some();
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .id("title-strip")
                    .occlude()
                    .h(px(title_bar::HEIGHT))
                    .flex_none()
                    .flex()
                    .items_center()
                    .pr(px(title_bar::captions_width() + 8.))
                    .window_control_area(WindowControlArea::Drag),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .pr(px(8.))
                    .when(!has_status_bar, |this| this.pb(px(8.)))
                    .pl(px(8. * (1. - sidebar_amount)))
                    .child(
                        // The cards, and the notices over them.
                        div()
                            .relative()
                            .flex_1()
                            .min_h_0()
                            .child(content)
                            .children(notices::label(self, cx))
                            .children(notices::list(self, cx)),
                    )
                    .when(dock_amount > 0., |this| {
                        this.child(self.dock_card(theme, dock_amount))
                    }),
            )
            .children(status_bar)
    }

    /// A card of its own under the others.
    fn dock_card(&self, theme: &theme::Theme, dock_amount: f32) -> impl IntoElement + use<> {
        let measured = self.dock_height.clone();
        let inner = panel::frame(theme, theme.bg, false)
            .flex_none()
            .overflow_hidden()
            .child(self.dock.clone())
            .child(
                gpui::canvas(
                    move |area, _, _| measured.set(f32::from(area.size.height)),
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            );
        // Full height when settled: its content can grow.
        div()
            .flex_none()
            .overflow_hidden()
            .mt(px(panel::CARD_GAP * dock_amount))
            .when(dock_amount < 1., |this| {
                this.h(px(self.dock_height.get() * dock_amount))
            })
            .child(inner)
    }

    fn sidebar_toggle(cx: &mut Context<Self>) -> impl IntoElement + use<> {
        div()
            .id("window-sidebar-toggle")
            .absolute()
            .top_0()
            .left(px(16. + (19. - 28.) / 2.))
            .h(px(title_bar::HEIGHT))
            .flex()
            .items_center()
            .occlude()
            .child(
                Button::new("toggle-sidebar")
                    .icon(IconName::PanelLeft)
                    .tooltip("Show sidebar (Ctrl+Shift+B)")
                    .style(ButtonStyle::Ghost)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.arm_sidebar(cx);
                        this.toggle_sidebar(window, cx)
                    })),
            )
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.follow_terminal(cx);
        self.sync_open_tabs(cx);
        self.sync_shared(window, cx);
        self.sync_notices(window, cx);

        let theme = cx.theme().clone();
        self.sidebar_reveal.set(self.sidebar_visible);
        self.dock_reveal.set(self.dock_visible);
        let moving = self.sidebar_reveal.moving() || self.dock_reveal.moving();
        motion::set_layout_moving(moving);
        if moving {
            window.request_animation_frame();
        }
        let sidebar_amount = self.sidebar_reveal.value();
        let dock_amount = self.dock_reveal.value();
        let sidebar_size = self.state.read(cx).settings.sidebar_width + 8.;
        let content = self.faded_tab_content(window, cx);
        let status_bar = self.status_bar(sidebar_amount, cx);
        div()
            .id("workspace")
            .key_context("Workspace")
            .track_focus(&self.focus)
            .relative()
            .size_full()
            .flex()
            .bg(theme.window_bg)
            .text_color(theme.text)
            .text_size(crate::theme::ui_px(14.))
            .font_family(crate::theme::ui_font())
            .map(|this| Self::with_actions(this, cx))
            .map(|this| self.with_resize_drags(this, cx))
            .on_action(cx.listener(|this, _: &IncreaseFont, _, cx| {
                this.state.update(cx, |s, cx| s.change_font_size(1., cx))
            }))
            .on_action(cx.listener(|this, _: &DecreaseFont, _, cx| {
                this.state.update(cx, |s, cx| s.change_font_size(-1., cx))
            }))
            .when(sidebar_amount > 0., |this| {
                this.child(self.sidebar_column(&theme, sidebar_size, sidebar_amount, cx))
            })
            .child(self.main_column(&theme, content, status_bar, sidebar_amount, dock_amount, cx))
            // With the sidebar hidden, at the left of the top strip. Open, it has its own.
            .when(!self.sidebar_visible, |this| {
                this.child(Self::sidebar_toggle(cx))
            })
            .children(self.tab_menu.clone().map(|menu| {
                let this = cx.entity().downgrade();
                ContextMenu::new(
                    "tab-menu",
                    menu,
                    crate::widgets::context_menu::dismiss(this, |view| &mut view.tab_menu),
                )
            }))
            .child(self.search_field(cx))
            .child(self.modals.clone())
            .child(WindowControls)
    }
}

#[cfg(test)]
mod short_keys_tests {
    use super::short_keys;

    #[test]
    fn shift_takes_its_symbol() {
        assert_eq!(short_keys("Ctrl+Shift+K"), "Ctrl ⇧ K");
        assert_eq!(short_keys("Ctrl+Alt+K"), "Ctrl Alt K");
        assert_eq!(short_keys("Shift+F1"), "⇧ F1");
    }
}
