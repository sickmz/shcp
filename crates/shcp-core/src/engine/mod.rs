mod config;
mod files;
use crate::{
    config::Store,
    edit::EditCommand,
    files::FileOp,
    session::{self, Command, Control, OpenTerminal},
    term::{Palette, TerminalEvent},
    transfer::{self, Job},
    *,
};
use anyhow::{Context as _, Result};
use async_channel::{Receiver, Sender};
use parking_lot::Mutex;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::runtime::{Handle, Runtime};
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

/// Dropping it lets the runtime thread wind down.
struct RuntimeOwner {
    handle: Handle,
    _stop: std::sync::mpsc::Sender<()>,
}
impl RuntimeOwner {
    fn new() -> Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("shcp-io")
            .build()?;
        let handle = runtime.handle().clone();
        let (stop, wait) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("shcp-runtime".into())
            .spawn(move || runtime_thread(runtime, wait))?;
        Ok(Self {
            handle,
            _stop: stop,
        })
    }
}
fn runtime_thread(runtime: Runtime, wait: std::sync::mpsc::Receiver<()>) {
    let _ = wait.recv();
    // Give cancellation cleanup time without blocking the UI thread that drops the handle.
    runtime.block_on(async { tokio::time::sleep(Duration::from_secs(2)).await });
    runtime.shutdown_timeout(Duration::from_secs(1));
}
/// The commands of each remote file open in a local editor.
type EditRegistry = HashMap<(SessionId, String), Sender<EditCommand>>;

struct Inner {
    runtime: RuntimeOwner,
    store: Mutex<Store>,
    events: Sender<Event>,
    next: AtomicU64,
    sessions: Mutex<HashMap<SessionId, Arc<Control>>>,
    terminals: Mutex<HashMap<TerminalId, TerminalHandle>>,
    /// Colors of the active theme, given to every terminal.
    palette: Mutex<Palette>,
    transfers: Arc<Mutex<HashMap<TransferId, CancellationToken>>>,
    /// Searches of remote subfolders under way.
    searches: Arc<Mutex<HashMap<RequestId, CancellationToken>>>,
    /// Remote files open in a local editor.
    edits: Arc<Mutex<EditRegistry>>,
    known_hosts_lock: Arc<Mutex<()>>,
}
impl Drop for Inner {
    fn drop(&mut self) {
        for control in self.sessions.get_mut().values() {
            control.context.cancel.cancel();
        }
        for token in self.transfers.lock().values() {
            token.cancel();
        }
        for terminal in self.terminals.get_mut().values() {
            terminal.close();
        }
    }
}
#[derive(Clone)]
pub struct EngineHandle(Arc<Inner>);

pub fn start() -> Result<(EngineHandle, Receiver<Event>)> {
    let mut store = Store::new()?;
    logging::init(&store.paths.log_dir)?;
    let (events, receiver) = async_channel::unbounded();
    let mut warnings = store.load();
    if let Err(error) = store.load_settings() {
        warnings.push(format!("Could not read settings.toml: {error:#}"));
    }
    for message in warnings {
        let _ = events.try_send(Event::Notice {
            level: NoticeLevel::Important,
            message,
        });
    }
    let edit_copies = crate::edit::root(&store.paths.data_dir);
    let engine = EngineHandle(Arc::new(Inner {
        runtime: RuntimeOwner::new()?,
        store: Mutex::new(store),
        events,
        next: AtomicU64::new(1),
        sessions: Mutex::new(HashMap::new()),
        terminals: Mutex::new(HashMap::new()),
        palette: Mutex::new(Palette::default()),
        transfers: Arc::new(Mutex::new(HashMap::new())),
        searches: Arc::new(Mutex::new(HashMap::new())),
        edits: Arc::new(Mutex::new(HashMap::new())),
        known_hosts_lock: Arc::new(Mutex::new(())),
    }));
    engine
        .0
        .runtime
        .handle
        .spawn(crate::edit::cleanup(edit_copies));
    Ok((engine, receiver))
}
impl EngineHandle {
    /// Disconnects without closing persistent shells. Call after the UI event loop has ended.
    pub fn shutdown(&self) {
        let controls: Vec<_> = self.0.sessions.lock().values().cloned().collect();
        for control in &controls {
            control.context.cancel.cancel();
        }
        for token in self.0.transfers.lock().values() {
            token.cancel();
        }
        let (finished, wait) = std::sync::mpsc::channel();
        self.0.runtime.handle.spawn(async move {
            while controls
                .iter()
                .any(|control| control.active.load(Ordering::SeqCst))
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            let _ = finished.send(());
        });
        if wait.recv_timeout(Duration::from_secs(5)).is_err() {
            tracing::warn!("Timed out waiting for SSH connections to disconnect");
        }
    }

    fn next(&self) -> u64 {
        self.0.next.fetch_add(1, Ordering::Relaxed)
    }
    fn emit(&self, event: Event) {
        let _ = self.0.events.try_send(event);
    }
    fn warnings(&self, warnings: Vec<String>) {
        for message in warnings {
            self.emit(Event::Notice {
                level: NoticeLevel::Important,
                message,
            });
        }
    }
    /// Reports a session that failed before it could start.
    fn fail(&self, id: SessionId, host: &HostId, message: String) {
        self.emit(Event::Session {
            id,
            host: host.clone(),
            state: SessionState::Failed {
                error: ConnectError {
                    kind: ConnectErrorKind::Other,
                    message,
                    hint: None,
                },
            },
        });
    }
    pub fn connect(&self, host: &HostId) -> SessionId {
        let mut sessions = self.0.sessions.lock();
        if let Some(control) = sessions.values().find(|c| {
            &c.context.host.id == host
                && c.active.load(Ordering::SeqCst)
                && !c.context.cancel.is_cancelled()
        }) {
            return control.context.id;
        }
        let id = SessionId(self.next());
        let store = self.0.store.lock();
        let Some(mut config) = store.hosts.iter().find(|h| &h.id == host).cloned() else {
            self.fail(id, host, format!("Host {} does not exist", host.0));
            return id;
        };
        for spec in store
            .settings
            .extra_forwards
            .get(&host.0)
            .into_iter()
            .flatten()
        {
            if !config
                .local_forwards
                .iter()
                .any(|f| f.bind_port == spec.bind_port && f.kind == spec.kind)
            {
                config.local_forwards.push(spec.clone());
            }
        }
        let jumps = match crate::config::ssh::jump_chain(&config, &store.hosts) {
            Ok(jumps) => jumps,
            Err(error) => {
                self.fail(id, host, format!("{error:#}"));
                return id;
            }
        };
        let persist_terminals = store.settings.keeps_terminals(&config.id.0);
        let context = session::Context {
            use_agent: store.settings.use_ssh_agent,
            open_forwards: store.settings.auto_open_forwards,
            persist_terminals,
            ..session::Context::new(
                id,
                config,
                jumps,
                store.paths.clone(),
                self.0.events.clone(),
                self.0.known_hosts_lock.clone(),
            )
        };
        let (control, commands) = Control::new(context, store.settings.transfer_concurrency);
        sessions.insert(id, control.clone());
        self.0.runtime.handle.spawn(session::run(control, commands));
        id
    }
    /// Tries settings that need not be saved yet. Dropping what it returns stops it.
    pub fn check_host(&self, host: HostConfig) -> Result<ConnectionCheck> {
        let store = self.0.store.lock();
        let host = crate::check::draft(host, &crate::config::home()?);
        let jumps = crate::config::ssh::jump_chain(&host, &store.hosts)?;
        let (events, receiver) = async_channel::unbounded();
        let context = session::Context {
            use_agent: store.settings.use_ssh_agent,
            ..session::Context::new(
                SessionId(self.next()),
                host,
                jumps,
                store.paths.clone(),
                events,
                self.0.known_hosts_lock.clone(),
            )
        };
        Ok(crate::check::start(
            &self.0.runtime.handle,
            context,
            receiver,
            store.settings.transfer_concurrency,
        ))
    }
    fn control(&self, s: SessionId) -> Option<Arc<Control>> {
        self.0.sessions.lock().get(&s).cloned()
    }
    pub fn disconnect(&self, s: SessionId) {
        if let Some(control) = self.control(s) {
            control.context.cancel.cancel();
        }
    }
    pub fn answer_host_key(&self, s: SessionId, accept: bool) {
        if let Some(c) = self.control(s) {
            c.context.prompts.answer_host_key(accept);
        }
    }
    /// `remember` keeps the passphrase in the Credential Manager, once it proves right.
    pub fn answer_passphrase(&self, s: SessionId, passphrase: Option<String>, remember: bool) {
        let value = passphrase.map(|passphrase| session::SecretAnswer {
            secret: Zeroizing::new(passphrase),
            remember,
        });
        if let Some(c) = self.control(s) {
            c.context.prompts.answer_passphrase(value);
        }
    }
    /// `remember` keeps the password in the Credential Manager, once the server accepts it.
    pub fn answer_password(&self, s: SessionId, password: Option<String>, remember: bool) {
        let value = password.map(|password| session::SecretAnswer {
            secret: Zeroizing::new(password),
            remember,
        });
        if let Some(c) = self.control(s) {
            c.context.prompts.answer_password(value);
        }
    }
    /// With `persist`, the shell lives on the server under that name, if the server has what it
    /// takes, and the terminal finds it again when opened with the same name.
    pub fn open_terminal(
        &self,
        s: SessionId,
        cols: u16,
        rows: u16,
        persist: Option<Persistence>,
    ) -> TerminalHandle {
        let id = TerminalId(self.next());
        let events = self.0.events.clone();
        let notify = Arc::new(move |event| {
            let _ = events.try_send(match event {
                TerminalEvent::Changed => Event::TerminalChanged(id),
                TerminalEvent::Title(title) => Event::TerminalTitle { id, title },
                TerminalEvent::Copy(text) => Event::TerminalCopy { text },
            });
        });
        let history = self.0.store.lock().settings.scrollback_lines;
        let (terminal, input) = TerminalHandle::new(
            id,
            cols,
            rows,
            history,
            self.0.palette.lock().clone(),
            notify,
        );
        self.attach_terminal(s, &terminal, input, persist);
        terminal
    }
    /// Gives a terminal whose shell is gone a new shell, keeping what it shows. With `persist`, its
    /// old shell if that is still on the server.
    pub fn reopen_terminal(
        &self,
        s: SessionId,
        terminal: &TerminalHandle,
        note: &str,
        persist: Option<Persistence>,
    ) {
        let input = terminal.reattach(note);
        self.attach_terminal(s, terminal, input, persist);
    }
    fn attach_terminal(
        &self,
        s: SessionId,
        terminal: &TerminalHandle,
        input: Receiver<term::TerminalCommand>,
        persist: Option<Persistence>,
    ) {
        self.0
            .terminals
            .lock()
            .insert(terminal.id(), terminal.clone());
        let control = self.control(s);
        let (log_dir, startup, history) = {
            let store = self.0.store.lock();
            let host = control.as_ref().map(|control| &control.context.host.id.0);
            let log_dir = host
                .filter(|host| store.settings.session_logs.contains(*host))
                .map(|_| crate::session_log::folder(&store.paths.data_dir));
            let startup = host.and_then(|host| store.settings.startup_commands.get(host).cloned());
            (log_dir, startup, store.settings.scrollback_lines)
        };
        let sent = control.as_ref().is_some_and(|control| {
            control.active.load(Ordering::SeqCst)
                && control
                    .commands
                    .try_send(Command::OpenTerminal(OpenTerminal {
                        terminal: terminal.clone(),
                        input,
                        log_dir,
                        persist,
                        history,
                        startup,
                    }))
                    .is_ok()
        });
        if !sent {
            terminal.finish();
            self.emit(Event::TerminalExited {
                id: terminal.id(),
                status: None,
            });
        }
    }
    /// Recolors open terminals and sets the colors of future ones.
    pub fn set_terminal_palette(&self, palette: &Palette) {
        *self.0.palette.lock() = palette.clone();
        for terminal in self.0.terminals.lock().values() {
            terminal.set_palette(palette.clone());
        }
    }
    /// Starts a local forward on a live session, e.g. retrying one that failed to bind.
    pub fn start_forward(&self, s: SessionId, spec: ForwardSpec) {
        if let Some(control) = self.control(s) {
            let _ = control.commands.try_send(Command::StartForward(spec));
        }
    }
    /// Closes one local forward of a live session.
    pub fn stop_forward(&self, s: SessionId, spec: ForwardSpec) {
        if let Some(control) = self.control(s) {
            let _ = control.commands.try_send(Command::StopForward(spec));
        }
    }
    /// Tells the user whether the server of a live session can keep terminals running.
    pub fn report_keeping_terminals(&self, s: SessionId) {
        if let Some(control) = self.control(s) {
            let _ = control.commands.try_send(Command::ReportKeeping);
        }
    }
    pub fn close_terminal(&self, t: TerminalId) {
        if let Some(terminal) = self.0.terminals.lock().remove(&t) {
            terminal.close();
        }
    }
    /// Lets go of a terminal without ending what runs in it.
    pub fn detach_terminal(&self, t: TerminalId) {
        if let Some(terminal) = self.0.terminals.lock().remove(&t) {
            terminal.finish();
        }
    }
    /// Tells the other PCs on a host how its tabs are now, through one of its terminals.
    pub fn publish_tabs(&self, t: TerminalId, tabs: &SharedTabs) {
        if let Some(terminal) = self.0.terminals.lock().get(&t)
            && let Some(hex) = crate::tmux::encode_tabs(tabs)
        {
            terminal.publish(hex);
        }
    }
}
