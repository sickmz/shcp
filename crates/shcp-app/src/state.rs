//! `AppState`: the UI-side mirror of the backend (hosts, sessions, forwards, transfers, notices).
//! Every core `Event` is applied here and then re-emitted so views can react.
use crate::login_prompt::LoginPrompt;
use crate::updater::UpdateStatus;
use crate::widgets::context_menu::Handler;
use crate::widgets::host_color::HostColor;
use gpui::{App, Context, Entity, EventEmitter};
use shcp_core::{
    ConnectProgress, ConnectStep, EngineHandle, Event, ForwardSpec, ForwardState, HostAppearance,
    HostConfig, HostId, NoticeLevel, SessionId, SessionState, Settings, Snippet, TransferId,
    TransferInfo, TransferState,
};
use std::{
    cell::Cell,
    collections::{BTreeMap, HashMap, HashSet},
    rc::Rc,
    time::{Duration, Instant},
};

#[derive(Clone)]
pub struct ForwardInfo {
    pub spec: ForwardSpec,
    pub state: ForwardState,
    pub active_conns: u32,
}

/// The least a step of the connection stays on screen, for it to be read. It delays only what is
/// drawn: the connection never waits for it, and a shell that is ready appears at once.
const STEP_PACE: Duration = Duration::from_millis(180);
/// How often a notice past its time looks whether it can go.
const NOTICE_HELD_CHECK: Duration = Duration::from_secs(1);

pub struct SessionInfo {
    pub state: SessionState,
    pub forwards: Vec<ForwardInfo>,
    /// The step the connection reached last. It stays once the connection moves on, for a prompt
    /// or a failure to say where it stopped.
    pub step: Option<(ConnectStep, ConnectProgress)>,
    /// What each step of the connection worked with, by its position.
    pub step_details: BTreeMap<u32, String>,
    /// The step on screen. It follows the connection one at a time, however fast that goes.
    pub shown_step: u32,
    shown_since: Instant,
    pacing: bool,
    /// The secret asked for last, and whether the answer before this one was wrong.
    last_asked: Option<String>,
    pub retrying: bool,
}

impl SessionInfo {
    fn new(state: SessionState) -> Self {
        Self {
            state,
            forwards: Vec::new(),
            step: None,
            step_details: BTreeMap::new(),
            shown_step: 0,
            shown_since: Instant::now(),
            pacing: false,
            last_asked: None,
            retrying: false,
        }
    }
}

/// A button on a notice.
#[derive(Clone)]
pub struct NoticeAction {
    pub label: &'static str,
    pub run: Handler,
}

pub struct Notice {
    pub id: u64,
    pub level: NoticeLevel,
    pub message: String,
    pub action: Option<NoticeAction>,
    /// Dismissed, and fading out before it is removed.
    pub leaving: bool,
}

pub struct AppState {
    pub engine: EngineHandle,
    pub hosts: Vec<HostConfig>,
    pub settings: Settings,
    pub sessions: BTreeMap<SessionId, SessionInfo>,
    /// Latest session of each host.
    host_sessions: HashMap<HostId, SessionId>,
    /// Host of the active tab, or the one selected in the sidebar: what the Ports grid shows.
    pub active_host: Option<HostId>,
    /// Tunnels closing after being removed: their last event must not list them again.
    forgotten: HashSet<(SessionId, ForwardSpec)>,
    pub transfers: Vec<TransferInfo>,
    /// What the label in the panel has to say, oldest first.
    pub notices: Vec<Notice>,
    next_notice: u64,
    /// The pointer is on a notice or the list is open, so none goes by itself.
    pub notices_held: Rc<Cell<bool>>,
    pub update: UpdateStatus,
    login_prompts: HashMap<HostId, Entity<LoginPrompt>>,
    /// Hosts being reconnected automatically, with the attempt under way or coming next.
    reconnecting: HashMap<HostId, u32>,
    /// Tunnels that were open when a connection was lost: they open again with the next one.
    lost_forwards: HashMap<HostId, Vec<ForwardSpec>>,
}

impl EventEmitter<Event> for AppState {}

/// Where a name is in a saved order. What is not there goes after, keeping its place among equals.
fn saved_position(order: &[String], name: &str) -> usize {
    order
        .iter()
        .position(|saved| saved == name)
        .unwrap_or(usize::MAX)
}

/// The list with `item` right before `target`, or right after it, wherever it came from.
fn moved(list: &[String], item: &String, target: &String, after: bool) -> Vec<String> {
    if item == target || !list.contains(item) {
        return list.to_vec();
    }
    let mut result: Vec<String> = list.iter().filter(|name| *name != item).cloned().collect();
    let Some(at) = result.iter().position(|name| name == target) else {
        return list.to_vec();
    };
    result.insert(at + usize::from(after), item.clone());
    result
}

impl AppState {
    pub fn new(engine: EngineHandle) -> Self {
        Self {
            hosts: engine.hosts(),
            settings: engine.settings(),
            engine,
            sessions: BTreeMap::new(),
            host_sessions: HashMap::new(),
            active_host: None,
            forgotten: HashSet::new(),
            transfers: Vec::new(),
            notices: Vec::new(),
            next_notice: 1,
            notices_held: Rc::default(),
            update: UpdateStatus::Idle,
            login_prompts: HashMap::new(),
            reconnecting: HashMap::new(),
            lost_forwards: HashMap::new(),
        }
    }

    // ---- queries ----

    pub fn reconnect_attempt(&self, host: &HostId) -> Option<u32> {
        self.reconnecting.get(host).copied()
    }

    /// The host had tunnels open when its connection was lost.
    pub fn has_lost_forwards(&self, host: &HostId) -> bool {
        self.lost_forwards.contains_key(host)
    }

    pub fn host_group(&self, host: &HostId) -> Option<&str> {
        self.settings.host_groups.get(&host.0).map(String::as_str)
    }

    /// Every group in use, in the order the user put them; the others follow by name.
    pub fn groups(&self) -> Vec<String> {
        let names: std::collections::BTreeSet<_> =
            self.settings.host_groups.values().cloned().collect();
        let mut names: Vec<String> = names.into_iter().collect();
        names.sort_by_key(|name| saved_position(&self.settings.group_order, name));
        names
    }

    /// Every host, in the order the user put them; the others follow as they are read.
    pub fn ordered_hosts(&self) -> Vec<HostConfig> {
        let mut hosts = self.hosts.clone();
        hosts.sort_by_key(|host| saved_position(&self.settings.host_order, &host.id.0));
        hosts
    }

    /// Puts a host where another one is, in its group.
    pub fn place_host(
        &mut self,
        host: &HostId,
        target: &HostId,
        after: bool,
        cx: &mut Context<Self>,
    ) {
        if host == target {
            return;
        }
        let order: Vec<String> = self
            .ordered_hosts()
            .into_iter()
            .map(|host| host.id.0)
            .collect();
        self.settings.host_order = moved(&order, &host.0, &target.0, after);
        let group = self.host_group(target).map(str::to_string);
        // Saves the order as well.
        self.set_host_group(host, group, cx);
    }

    /// Puts a group right before another one, or right after it.
    pub fn place_group(&mut self, group: &str, target: &str, after: bool, cx: &mut Context<Self>) {
        let order = self.groups();
        self.settings.group_order = moved(&order, &group.to_string(), &target.to_string(), after);
        self.persist_settings(cx);
        cx.notify();
    }

    pub fn host(&self, id: &HostId) -> Option<&HostConfig> {
        self.hosts.iter().find(|h| &h.id == id)
    }

    pub fn host_session(&self, host: &HostId) -> Option<&SessionInfo> {
        self.host_sessions
            .get(host)
            .and_then(|id| self.sessions.get(id))
    }

    pub fn host_session_id(&self, host: &HostId) -> Option<SessionId> {
        self.host_sessions.get(host).copied()
    }

    pub fn session(&self, id: SessionId) -> Option<&SessionInfo> {
        self.sessions.get(&id)
    }

    pub fn active_transfers(&self) -> usize {
        self.transfers
            .iter()
            .filter(|t| matches!(t.state, TransferState::Queued | TransferState::Running))
            .count()
    }

    /// How far the transfers under way have come, when the size of every one of them is known.
    pub fn transfers_percent(&self) -> Option<u8> {
        let (mut done, mut total) = (0u64, 0u64);
        for transfer in self
            .transfers
            .iter()
            .filter(|t| matches!(t.state, TransferState::Queued | TransferState::Running))
        {
            let size = transfer.bytes_total?;
            done += transfer.bytes_done.min(size);
            total += size;
        }
        (total > 0).then(|| (done as f64 / total as f64 * 100.) as u8)
    }

    // ---- commands ----

    /// Connects (or reuses the live session of) a host and returns its session id.
    pub fn connect_host(&mut self, host: &HostId, cx: &mut Context<Self>) -> SessionId {
        let id = self.engine.connect(host);
        let fresh = !self.sessions.contains_key(&id);
        self.sessions.entry(id).or_insert_with(|| {
            SessionInfo::new(SessionState::Connecting {
                step: ConnectStep::Resolving,
                progress: ConnectProgress::default(),
            })
        });
        self.host_sessions.insert(host.clone(), id);
        if fresh {
            cx.emit(Event::Session {
                id,
                host: host.clone(),
                state: SessionState::Connecting {
                    step: ConnectStep::Resolving,
                    progress: ConnectProgress::default(),
                },
            });
        }
        cx.notify();
        id
    }

    pub fn disconnect_host(&mut self, host: &HostId) {
        if let Some(id) = self.host_sessions.get(host) {
            self.engine.disconnect(*id);
        }
    }

    pub fn set_active_host(&mut self, host: Option<HostId>, cx: &mut Context<Self>) {
        if self.active_host != host {
            self.active_host = host;
            cx.notify();
        }
    }

    /// Opens a closed tunnel, or tries again one that could not listen.
    pub fn open_forward(&mut self, session: SessionId, spec: &ForwardSpec, cx: &mut Context<Self>) {
        if let Some(forward) = self
            .sessions
            .get_mut(&session)
            .and_then(|info| info.forwards.iter_mut().find(|f| &f.spec == spec))
        {
            forward.state = ForwardState::Stopped;
        }
        self.engine.start_forward(session, spec.clone());
        cx.notify();
    }

    /// Opens a new tunnel on a live session and remembers it for the host.
    pub fn add_forward(
        &mut self,
        host: &HostId,
        session: SessionId,
        spec: ForwardSpec,
        cx: &mut Context<Self>,
    ) {
        let Some(info) = self.sessions.get_mut(&session) else {
            return;
        };
        if info
            .forwards
            .iter()
            .any(|f| f.spec.bind_port == spec.bind_port && f.spec.kind == spec.kind)
        {
            let message = format!("Port {} is already in the list", spec.bind_port);
            self.push_notice(NoticeLevel::Warning, message, cx);
            return;
        }
        info.forwards.push(ForwardInfo {
            spec: spec.clone(),
            state: ForwardState::Stopped,
            active_conns: 0,
        });
        self.settings
            .extra_forwards
            .entry(host.0.clone())
            .or_default()
            .push(spec.clone());
        self.persist_settings(cx);
        self.engine.start_forward(session, spec);
        cx.notify();
    }

    pub fn is_remembered(&self, host: &HostId, spec: &ForwardSpec) -> bool {
        self.settings
            .extra_forwards
            .get(&host.0)
            .is_some_and(|saved| saved.contains(spec))
    }

    /// Closes a tunnel added from the sidebar and stops remembering it.
    pub fn forget_forward(
        &mut self,
        host: &HostId,
        session: SessionId,
        spec: &ForwardSpec,
        cx: &mut Context<Self>,
    ) {
        if let Some(saved) = self.settings.extra_forwards.get_mut(&host.0) {
            saved.retain(|s| s != spec);
            if saved.is_empty() {
                self.settings.extra_forwards.remove(&host.0);
            }
        }
        self.persist_settings(cx);
        if let Some(info) = self.sessions.get_mut(&session)
            && let Some(index) = info.forwards.iter().position(|f| &f.spec == spec)
        {
            if matches!(
                info.forwards[index].state,
                ForwardState::Listening | ForwardState::ServerDown
            ) {
                self.forgotten.insert((session, spec.clone()));
                self.engine.stop_forward(session, spec.clone());
            }
            info.forwards.remove(index);
        }
        cx.notify();
    }

    pub fn set_reconnect_attempt(
        &mut self,
        host: &HostId,
        attempt: Option<u32>,
        cx: &mut Context<Self>,
    ) {
        match attempt {
            Some(attempt) => {
                self.reconnecting.insert(host.clone(), attempt);
            }
            None => {
                self.reconnecting.remove(host);
            }
        }
        cx.notify();
    }

    /// Removes every saved passphrase and password from the Credential Manager.
    pub fn forget_secrets(&mut self, cx: &mut Context<Self>) {
        for key in std::mem::take(&mut self.settings.saved_passphrases) {
            self.engine.forget_passphrase(key);
        }
        for target in std::mem::take(&mut self.settings.saved_passwords) {
            self.engine.forget_password(target);
        }
        self.persist_settings(cx);
        cx.notify();
    }

    pub fn remember_key_login_offered(&mut self, host: &HostId, cx: &mut Context<Self>) {
        self.settings.key_login_offered.insert(host.0.clone());
        self.persist_settings(cx);
    }

    /// Moves a host to a group, or out of any with `None`.
    pub fn set_host_group(&mut self, host: &HostId, group: Option<String>, cx: &mut Context<Self>) {
        match group
            .map(|g| g.trim().to_string())
            .filter(|g| !g.is_empty())
        {
            Some(group) => {
                self.settings.host_groups.insert(host.0.clone(), group);
            }
            None => {
                self.settings.host_groups.remove(&host.0);
            }
        }
        let groups = self.groups();
        self.settings
            .collapsed_groups
            .retain(|name| groups.contains(name));
        self.persist_settings(cx);
        cx.notify();
    }

    pub fn rename_group(&mut self, old: &str, new: &str, cx: &mut Context<Self>) {
        self.settings.rename_group(old, new);
        self.persist_settings(cx);
        cx.notify();
    }

    pub fn toggle_host_collapsed(&mut self, host: &HostId, cx: &mut Context<Self>) {
        if !self.settings.collapsed_hosts.remove(&host.0) {
            self.settings.collapsed_hosts.insert(host.0.clone());
        }
        self.persist_settings(cx);
        cx.notify();
    }

    /// Shows the tabs of a host: one of them is about to be on screen.
    pub fn expand_host(&mut self, host: &HostId, cx: &mut Context<Self>) {
        if self.settings.collapsed_hosts.remove(&host.0) {
            self.persist_settings(cx);
            cx.notify();
        }
    }

    pub fn toggle_group_collapsed(&mut self, group: &str, cx: &mut Context<Self>) {
        if !self.settings.collapsed_groups.remove(group) {
            self.settings.collapsed_groups.insert(group.to_string());
        }
        self.persist_settings(cx);
        cx.notify();
    }

    /// Replaces the startup command of a host and the commands the host can see.
    pub fn set_host_commands(
        &mut self,
        host: &HostId,
        startup: String,
        snippets: Vec<Snippet>,
        cx: &mut Context<Self>,
    ) {
        if startup.is_empty() {
            self.settings.startup_commands.remove(&host.0);
        } else {
            self.settings
                .startup_commands
                .insert(host.0.clone(), startup);
        }
        self.settings
            .snippets
            .retain(|s| s.host.as_ref().is_some_and(|h| *h != host.0));
        self.settings.snippets.extend(snippets);
        self.persist_settings(cx);
        cx.notify();
    }

    /// Starts a failed transfer again, on a new connection if its own is gone.
    pub fn retry_transfer(&mut self, id: TransferId, cx: &mut Context<Self>) {
        let Some(index) = self.transfers.iter().position(|t| t.id == id) else {
            return;
        };
        let info = self.transfers.remove(index);
        let session = self.connect_host(&info.host, cx);
        self.engine.retry_transfer(session, &info);
        cx.notify();
    }

    pub fn is_host_hidden(&self, host: &HostId) -> bool {
        self.settings.hidden_hosts.contains(&host.0)
    }

    /// Hides a host from the sidebar, or shows it again, and remembers the choice.
    pub fn toggle_host_hidden(&mut self, host: &HostId, cx: &mut Context<Self>) {
        if !self.settings.hidden_hosts.remove(&host.0) {
            self.settings.hidden_hosts.insert(host.0.clone());
        }
        self.persist_settings(cx);
        cx.notify();
    }

    pub fn host_appearance(&self, host: &HostId) -> HostAppearance {
        self.settings
            .host_appearance
            .get(&host.0)
            .cloned()
            .unwrap_or_default()
    }

    /// The color of a host, gray unless picked. A tab can outlive its host: it keeps gray then.
    pub fn host_color(&self, host: &HostId) -> HostColor {
        self.settings
            .host_appearance
            .get(&host.0)
            .and_then(|appearance| appearance.color.as_deref())
            .and_then(HostColor::from_slug)
            .unwrap_or(HostColor::Gray)
    }

    /// Remembers how a host looks; the default look leaves nothing behind in the settings.
    pub fn set_host_appearance(
        &mut self,
        host: &HostId,
        appearance: HostAppearance,
        cx: &mut Context<Self>,
    ) {
        if self.host_appearance(host) == appearance {
            return;
        }
        if appearance == HostAppearance::default() {
            self.settings.host_appearance.remove(&host.0);
        } else {
            self.settings
                .host_appearance
                .insert(host.0.clone(), appearance);
        }
        self.persist_settings(cx);
        cx.notify();
    }

    /// A host's id is its alias: keep what is remembered for it when it changes.
    pub fn host_renamed(&mut self, old: &HostId, new: &HostId, cx: &mut Context<Self>) {
        self.settings.rename_host(old, new);
        self.persist_settings(cx);
        cx.notify();
    }

    pub(crate) fn persist_settings(&mut self, cx: &mut Context<Self>) {
        if let Err(error) = self.engine.save_settings(self.settings.clone()) {
            self.push_notice(
                NoticeLevel::Error,
                format!("Could not save settings: {error:#}"),
                cx,
            );
        }
    }

    pub fn cancel_transfer(&mut self, id: TransferId) {
        self.engine.cancel_transfer(id);
    }

    pub fn clear_completed_transfers(&mut self, cx: &mut Context<Self>) {
        self.transfers
            .retain(|t| matches!(t.state, TransferState::Queued | TransferState::Running));
        cx.notify();
    }

    pub fn push_notice(&mut self, level: NoticeLevel, message: String, cx: &mut Context<Self>) {
        // What matters stays until it is closed: it may be read later, and it may need acting on.
        let linger = match level {
            NoticeLevel::Info => Some(Duration::from_secs(5)),
            NoticeLevel::Warning => Some(Duration::from_secs(10)),
            NoticeLevel::Important | NoticeLevel::Error => None,
        };
        self.add_notice(level, message, None, linger, cx);
    }

    /// Stays until the user chooses or closes it.
    pub fn push_action_notice(
        &mut self,
        level: NoticeLevel,
        message: String,
        action: NoticeAction,
        cx: &mut Context<Self>,
    ) {
        self.add_notice(level, message, Some(action), None, cx);
    }

    fn add_notice(
        &mut self,
        level: NoticeLevel,
        message: String,
        action: Option<NoticeAction>,
        linger: Option<Duration>,
        cx: &mut Context<Self>,
    ) {
        let id = self.next_notice;
        self.next_notice += 1;
        // Said again, it moves to the front instead of piling up.
        self.notices.retain(|notice| notice.message != message);
        self.notices.push(Notice {
            id,
            level,
            message,
            action,
            leaving: false,
        });
        cx.notify();
        if let Some(linger) = linger {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(linger).await;
                // Not while it is being read: it goes shortly after the pointer leaves.
                while this
                    .update(cx, |state, _| state.notices_held.get())
                    .unwrap_or(false)
                {
                    cx.background_executor().timer(NOTICE_HELD_CHECK).await;
                }
                this.update(cx, |state, cx| state.dismiss_notice(id, cx))
                    .ok();
            })
            .detach();
        }
    }

    /// Both the close button and the timer end up here: only the first one counts.
    pub fn dismiss_notice(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(notice) = self.notices.iter_mut().find(|n| n.id == id && !n.leaving) else {
            return;
        };
        notice.leaving = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(crate::motion::NOTICE_OUT.total())
                .await;
            this.update(cx, |state, cx| {
                state.notices.retain(|n| n.id != id);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// What the host's connection is asking the user, unless it was just answered.
    pub fn login_prompt(&self, host: &HostId, cx: &App) -> Option<Entity<LoginPrompt>> {
        self.login_prompts
            .get(host)
            .filter(|prompt| !prompt.read(cx).answered())
            .cloned()
    }

    pub fn set_login_prompt(
        &mut self,
        host: &HostId,
        prompt: Entity<LoginPrompt>,
        cx: &mut Context<Self>,
    ) {
        self.login_prompts.insert(host.clone(), prompt);
        cx.notify();
    }

    /// The session moved on. An older session of the host ending changes nothing.
    pub fn clear_login_prompt(
        &mut self,
        host: &HostId,
        session: SessionId,
        cx: &mut Context<Self>,
    ) {
        let stale = self
            .login_prompts
            .get(host)
            .is_some_and(|prompt| prompt.read(cx).session() == session);
        if stale {
            self.login_prompts.remove(host);
            cx.notify();
        }
    }

    pub fn clear_notices(&mut self, cx: &mut Context<Self>) {
        let ids: Vec<u64> = self.notices.iter().map(|n| n.id).collect();
        for id in ids {
            self.dismiss_notice(id, cx);
        }
    }

    /// Replaces and persists all settings.
    pub fn update_settings(
        &mut self,
        settings: Settings,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<()> {
        self.engine.save_settings(settings.clone())?;
        let keys_changed = settings.keybindings != self.settings.keybindings
            || settings.ctrl_v_pastes != self.settings.ctrl_v_pastes;
        self.settings = settings;
        if keys_changed {
            crate::actions::bind_keys(&self.settings, cx);
        }
        crate::theme::apply(&self.settings, &self.engine, cx);
        cx.notify();
        Ok(())
    }

    /// Replaces settings and hosts with those of a backup. Returns how many hosts came in.
    pub fn import_backup(
        &mut self,
        path: &std::path::Path,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<usize> {
        let count = self.engine.import_backup(path)?;
        self.settings = self.engine.settings();
        crate::actions::bind_keys(&self.settings, cx);
        crate::theme::apply(&self.settings, &self.engine, cx);
        cx.notify();
        Ok(count)
    }

    pub fn logs_sessions(&self, host: &HostId) -> bool {
        self.settings.session_logs.contains(&host.0)
    }

    /// Starts or stops writing the new terminals of a host to files.
    pub fn toggle_session_log(&mut self, host: &HostId, cx: &mut Context<Self>) {
        let on = !self.settings.session_logs.remove(&host.0);
        if on {
            self.settings.session_logs.insert(host.0.clone());
        }
        if let Err(error) = self.engine.save_settings(self.settings.clone()) {
            self.push_notice(
                NoticeLevel::Error,
                format!("Could not save settings: {error:#}"),
                cx,
            );
        } else {
            let message = if on {
                format!(
                    "New terminals of {} are written to {}. Whatever they show goes there, \
                     secrets too",
                    host.0,
                    self.engine.session_logs_dir().display()
                )
            } else {
                format!(
                    "New terminals of {} are no longer written to a file",
                    host.0
                )
            };
            self.push_notice(NoticeLevel::Info, message, cx);
        }
        cx.notify();
    }

    /// Remembers where the window is, for the next start.
    pub fn remember_window(&mut self, window: shcp_core::SavedWindow, cx: &mut Context<Self>) {
        if self.settings.window == Some(window) {
            return;
        }
        self.settings.window = Some(window);
        if let Err(error) = self.engine.save_settings(self.settings.clone()) {
            self.push_notice(
                NoticeLevel::Error,
                format!("Could not save settings: {error:#}"),
                cx,
            );
        }
    }

    /// What closing the app now would interrupt, in words. `None` when nothing.
    pub fn closing_would_stop(&self) -> Option<String> {
        let transfers = self
            .transfers
            .iter()
            .filter(|t| matches!(t.state, TransferState::Queued | TransferState::Running))
            .count();
        let edits = self.engine.open_edits();
        let transfers = match transfers {
            0 => None,
            1 => Some("1 transfer is still running, and can continue later".to_string()),
            count => Some(format!(
                "{count} transfers are still running, and can continue later"
            )),
        };
        let edits = match edits {
            0 => None,
            1 => Some("saves of 1 file open in your editor stop reaching the server".to_string()),
            count => Some(format!(
                "saves of {count} files open in your editor stop reaching the server"
            )),
        };
        match (transfers, edits) {
            (None, None) => None,
            (Some(one), None) | (None, Some(one)) => Some(one),
            (Some(first), Some(second)) => Some(format!("{first}, and {second}")),
        }
    }

    /// Toggles dot files in every Files tab and persists the choice.
    pub fn set_file_details_width(&mut self, width: f32, cx: &mut Context<Self>) {
        self.settings.file_details_width = width;
        self.persist_settings(cx);
        cx.notify();
    }

    pub fn toggle_file_details(&mut self, cx: &mut Context<Self>) {
        self.settings.show_file_details = !self.settings.show_file_details;
        self.persist_settings(cx);
        cx.notify();
    }

    pub fn toggle_hidden_files(&mut self, cx: &mut Context<Self>) {
        self.settings.show_hidden_files = !self.settings.show_hidden_files;
        if let Err(error) = self.engine.save_settings(self.settings.clone()) {
            self.push_notice(
                NoticeLevel::Error,
                format!("Could not save settings: {error:#}"),
                cx,
            );
        }
        cx.notify();
    }

    /// Live sidebar resize; `persist` writes the final width.
    pub fn set_sidebar_width(&mut self, width: f32, persist: bool, cx: &mut Context<Self>) {
        self.settings.sidebar_width = width.clamp(180., 560.);
        if persist && let Err(error) = self.engine.save_settings(self.settings.clone()) {
            self.push_notice(
                NoticeLevel::Error,
                format!("Could not save settings: {error:#}"),
                cx,
            );
        }
        cx.notify();
    }

    pub fn reset_font_size(&mut self, cx: &mut Context<Self>) {
        let delta = Settings::default().terminal_font_size - self.settings.terminal_font_size;
        self.change_font_size(delta, cx);
    }

    /// Changes the terminal font size and persists it.
    pub fn change_font_size(&mut self, delta: f32, cx: &mut Context<Self>) {
        let size = (self.settings.terminal_font_size + delta).clamp(8., 32.);
        if size == self.settings.terminal_font_size {
            return;
        }
        self.settings.terminal_font_size = size;
        if let Err(error) = self.engine.save_settings(self.settings.clone()) {
            self.push_notice(
                NoticeLevel::Error,
                format!("Could not save settings: {error:#}"),
                cx,
            );
        }
        cx.notify();
    }

    // ---- core events ----

    pub fn apply(&mut self, event: Event, cx: &mut Context<Self>) {
        // Terminal and file results do not touch our own state: skip the global re-render.
        let touches_state = !matches!(
            event,
            Event::TerminalChanged(_)
                | Event::TerminalTitle { .. }
                | Event::TerminalCopy { .. }
                | Event::TerminalExited { .. }
                | Event::RemotePath { .. }
                | Event::RemoteKind { .. }
                | Event::DirListing { .. }
                | Event::OpDone { .. }
                | Event::RemoteSpace { .. }
                | Event::RemoteHead { .. }
                | Event::SearchFound { .. }
                | Event::SearchDone { .. }
                | Event::EditConflict { .. }
                | Event::PastedFile { .. }
                | Event::PasswordLogin { .. }
        );
        match &event {
            Event::HostsChanged => self.hosts = self.engine.hosts(),
            Event::Session { id, host, state } => self.apply_session(*id, host, state, cx),
            Event::Forward {
                session,
                spec,
                state,
                active_conns,
            } => {
                let key = (*session, spec.clone());
                if self.forgotten.contains(&key) {
                    if matches!(state, ForwardState::Stopped) {
                        self.forgotten.remove(&key);
                    }
                } else if let Some(info) = self.sessions.get_mut(session) {
                    match info.forwards.iter_mut().find(|f| &f.spec == spec) {
                        Some(f) => {
                            f.state = state.clone();
                            f.active_conns = *active_conns;
                        }
                        None => info.forwards.push(ForwardInfo {
                            spec: spec.clone(),
                            state: state.clone(),
                            active_conns: *active_conns,
                        }),
                    }
                }
            }
            Event::Transfer(info) => match self.transfers.iter_mut().find(|t| t.id == info.id) {
                Some(t) => *t = info.clone(),
                None => self.transfers.push(info.clone()),
            },
            Event::PassphraseSaved { key, saved } => {
                let key = key.display().to_string();
                if *saved {
                    self.settings.saved_passphrases.insert(key);
                } else {
                    self.settings.saved_passphrases.remove(&key);
                }
                self.persist_settings(cx);
            }
            Event::PasswordSaved { target, saved } => {
                if *saved {
                    self.settings.saved_passwords.insert(target.clone());
                } else {
                    self.settings.saved_passwords.remove(target);
                }
                self.persist_settings(cx);
            }
            Event::Notice { level, message } => self.push_notice(*level, message.clone(), cx),
            Event::TerminalCopy { text } => {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(text.clone()));
            }
            Event::TerminalChanged(_)
            | Event::TerminalTitle { .. }
            | Event::TerminalExited { .. }
            | Event::RemotePath { .. }
            | Event::RemoteKind { .. }
            | Event::DirListing { .. }
            | Event::OpDone { .. }
            | Event::RemoteSpace { .. }
            | Event::RemoteHead { .. }
            | Event::SearchFound { .. }
            | Event::SearchDone { .. }
            | Event::EditConflict { .. }
            | Event::PastedFile { .. }
            | Event::PasswordLogin { .. } => {}
        }
        cx.emit(event);
        if touches_state {
            cx.notify();
        }
    }

    /// Moves the step on screen towards the connection's, until it gets there.
    fn pace_steps(id: SessionId, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(STEP_PACE).await;
                let behind = this.update(cx, |this, cx| {
                    let info = this.sessions.get_mut(&id)?;
                    let real = info.step.as_ref()?.1.position;
                    if info.shown_step < real {
                        info.shown_step += 1;
                        info.shown_since = Instant::now();
                        cx.notify();
                    }
                    let behind = info.shown_step < real;
                    info.pacing = behind;
                    Some(behind)
                });
                if !matches!(behind, Ok(Some(true))) {
                    break;
                }
            }
        })
        .detach();
    }

    fn apply_session(
        &mut self,
        id: SessionId,
        host: &HostId,
        state: &SessionState,
        cx: &mut Context<Self>,
    ) {
        let info = self
            .sessions
            .entry(id)
            .or_insert_with(|| SessionInfo::new(state.clone()));
        info.state = state.clone();
        if let SessionState::Connecting { step, progress } = state {
            info.step = Some((*step, progress.clone()));
            if progress.position == 0 {
                info.step_details.clear();
                info.shown_step = 0;
            }
            if let Some(detail) = &progress.detail {
                info.step_details.insert(progress.position, detail.clone());
            }
            let real = progress.position;
            if info.shown_step < real && info.shown_since.elapsed() >= STEP_PACE {
                info.shown_step += 1;
                info.shown_since = Instant::now();
            }
            if info.shown_step < real && !info.pacing {
                info.pacing = true;
                Self::pace_steps(id, cx);
            }
        }
        // A failure says where the connection really stopped.
        if let (SessionState::Failed { .. }, Some((_, progress))) = (state, &info.step) {
            info.shown_step = progress.position;
        }
        let asked = match state {
            SessionState::AwaitingPassword { login } => Some(format!("password {login}")),
            SessionState::AwaitingAnswer { prompt, .. } => Some(prompt.clone()),
            SessionState::AwaitingPassphrase { key_path } => Some(key_path.display().to_string()),
            _ => None,
        };
        if asked.is_some() {
            info.retrying = info.last_asked == asked;
            info.last_asked = asked;
        }
        if matches!(
            state,
            SessionState::Disconnected { reason: Some(_) } | SessionState::Failed { .. }
        ) {
            let open: Vec<_> = info
                .forwards
                .iter()
                .filter(|f| matches!(f.state, ForwardState::Listening | ForwardState::ServerDown))
                .map(|f| f.spec.clone())
                .collect();
            if !open.is_empty() {
                self.lost_forwards.insert(host.clone(), open);
            }
        }
        // Disconnecting on purpose closes the tunnels for good.
        if matches!(state, SessionState::Disconnected { reason: None }) {
            self.lost_forwards.remove(host);
        }
        if matches!(
            state,
            SessionState::Disconnected { .. } | SessionState::Failed { .. }
        ) {
            info.forwards.clear();
        }
        if matches!(state, SessionState::Connected)
            && let Some(specs) = self.lost_forwards.remove(host)
            // Otherwise the new connection has just opened them all.
            && !self.settings.auto_open_forwards
        {
            for spec in specs {
                self.engine.start_forward(id, spec);
            }
        }
        let current = self.host_sessions.entry(host.clone()).or_insert(id);
        if id >= *current {
            *current = id;
        }
        let alias = self.host(host).map_or(host.0.clone(), |h| h.alias.clone());
        match state {
            // The panel says it is trying again: no notice for each attempt.
            SessionState::Failed { .. } if self.reconnecting.contains_key(host) => {}
            // Not an error that stays: the panel and the sidebar keep saying it.
            SessionState::Failed { error } => {
                self.push_notice(
                    NoticeLevel::Warning,
                    format!("{alias}: {}", error.message),
                    cx,
                );
            }
            SessionState::Disconnected {
                reason: Some(reason),
            } => {
                self.push_notice(
                    NoticeLevel::Warning,
                    format!("{alias} disconnected: {reason}"),
                    cx,
                );
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod order_tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| name.to_string()).collect()
    }

    #[test]
    fn a_move_lands_on_the_chosen_side_of_the_target_in_both_directions() {
        let list = names(&["a", "b", "c", "d"]);
        let before =
            |item: &str, target: &str| moved(&list, &item.to_string(), &target.to_string(), false);
        let after =
            |item: &str, target: &str| moved(&list, &item.to_string(), &target.to_string(), true);
        assert_eq!(before("a", "c"), names(&["b", "a", "c", "d"]));
        assert_eq!(after("a", "c"), names(&["b", "c", "a", "d"]));
        assert_eq!(before("d", "b"), names(&["a", "d", "b", "c"]));
        assert_eq!(after("d", "b"), names(&["a", "b", "d", "c"]));
        assert_eq!(after("a", "d"), names(&["b", "c", "d", "a"]));
        assert_eq!(before("d", "a"), names(&["d", "a", "b", "c"]));
        assert_eq!(before("b", "c"), list);
        assert_eq!(after("b", "a"), list);
        assert_eq!(after("b", "b"), list);
        assert_eq!(before("x", "b"), list);
    }

    #[test]
    fn what_has_no_saved_place_goes_last() {
        let order = names(&["b", "a"]);
        assert_eq!(saved_position(&order, "b"), 0);
        assert_eq!(saved_position(&order, "a"), 1);
        assert_eq!(saved_position(&order, "c"), usize::MAX);
    }
}
