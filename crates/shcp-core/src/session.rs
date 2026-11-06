use crate::{files::Files, host_key::Handler, proxy::Proxy, term::TerminalCommand, *};
use anyhow::{Context as _, Result, bail};
use async_channel::{Receiver, Sender};
use parking_lot::Mutex;
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, Ordering},
    },
    time::Duration,
};
use tokio::{
    net::TcpStream,
    sync::{Semaphore, watch},
};
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

pub(crate) type Ssh = russh::client::Handle<Handler>;
/// What the user typed for an encrypted key or a password.
pub(crate) struct SecretAnswer {
    pub secret: Zeroizing<String>,
    pub remember: bool,
}
pub(crate) struct Prompts {
    pub kind: AtomicU8,
    pub host_tx: Sender<bool>,
    pub host_rx: Receiver<bool>,
    pub pass_tx: Sender<Option<SecretAnswer>>,
    pub pass_rx: Receiver<Option<SecretAnswer>>,
    pub password_tx: Sender<Option<SecretAnswer>>,
    pub password_rx: Receiver<Option<SecretAnswer>>,
}
impl Prompts {
    pub fn new() -> Self {
        let (host_tx, host_rx) = async_channel::bounded(1);
        let (pass_tx, pass_rx) = async_channel::bounded(1);
        let (password_tx, password_rx) = async_channel::bounded(1);
        Self {
            kind: AtomicU8::new(0),
            host_tx,
            host_rx,
            pass_tx,
            pass_rx,
            password_tx,
            password_rx,
        }
    }
}
pub(crate) struct Context {
    pub id: SessionId,
    pub host: HostConfig,
    /// Hosts to go through on the way to `host`, in order.
    pub jumps: Vec<HostConfig>,
    /// The server's ports listening for this PC, by port.
    pub remote_forwards: Mutex<HashMap<u32, Arc<forward::RemoteForward>>>,
    pub paths: AppPaths,
    pub events: Sender<Event>,
    pub cancel: CancellationToken,
    pub prompts: Prompts,
    pub known_hosts_lock: Arc<Mutex<()>>,
    pub use_agent: bool,
    pub open_forwards: bool,
}
impl Context {
    pub fn emit(&self, event: Event) {
        let _ = self.events.try_send(event);
    }
    pub fn state(&self, state: SessionState) {
        tracing::info!(session = self.id.0, host = %self.host.alias, ?state, "Session state changed");
        self.emit(Event::Session {
            id: self.id,
            host: self.host.id.clone(),
            state,
        });
    }
    pub fn notice(&self, level: NoticeLevel, message: String) {
        tracing::info!(?level, %message);
        self.emit(Event::Notice { level, message });
    }
    /// `host` is the machine the step is on: the session's host or a jump host on the way to it.
    pub fn step(&self, host: &HostConfig, step: ConnectStep, detail: Option<String>) {
        let hop = self
            .jumps
            .iter()
            .position(|jump| {
                jump.hostname == host.hostname && jump.port == host.port && jump.user == host.user
            })
            .unwrap_or(self.jumps.len());
        let plan = connection_plan(&self.jumps, &self.host, self.opens_forwards());
        let position = plan
            .iter()
            .position(|planned| *planned == (hop, step))
            .unwrap_or(plan.len());
        self.state(SessionState::Connecting {
            step,
            progress: ConnectProgress {
                position: position as u32,
                plan: plan
                    .into_iter()
                    .map(|(hop, step)| PlannedStep {
                        step,
                        hop: self.jumps.get(hop).map(|jump| jump.alias.clone()),
                    })
                    .collect(),
                detail,
            },
        });
    }
    fn opens_forwards(&self) -> bool {
        self.open_forwards && !self.host.local_forwards.is_empty()
    }
}
/// The steps of a connection in order, each with the index of the machine it is on: the jump hosts
/// and then the session's host.
fn connection_plan(
    jumps: &[HostConfig],
    host: &HostConfig,
    opens_forwards: bool,
) -> Vec<(usize, ConnectStep)> {
    let mut plan = Vec::new();
    for (index, hop) in jumps.iter().chain(std::iter::once(host)).enumerate() {
        let reach: &[ConnectStep] = if index > 0 {
            &[ConnectStep::Connecting]
        } else if hop.proxy_command.is_some() {
            &[ConnectStep::StartingProxy]
        } else {
            &[ConnectStep::Resolving, ConnectStep::Connecting]
        };
        let steps = reach.iter().copied().chain([
            ConnectStep::Handshake,
            ConnectStep::VerifyingHostKey,
            ConnectStep::Authenticating,
        ]);
        plan.extend(steps.map(|step| (index, step)));
    }
    if opens_forwards {
        plan.push((jumps.len(), ConnectStep::OpeningForwards));
    }
    plan
}
type Ready = Option<Result<Arc<Files>, String>>;
pub(crate) struct Control {
    pub context: Arc<Context>,
    pub active: AtomicBool,
    pub commands: Sender<Command>,
    ready: watch::Sender<Ready>,
    pub transfers: Semaphore,
    transfer_limit: u32,
}
pub(crate) struct OpenTerminal {
    pub terminal: TerminalHandle,
    pub input: Receiver<TerminalCommand>,
    /// Where to write what it shows, when asked to.
    pub log_dir: Option<std::path::PathBuf>,
}
pub(crate) enum Command {
    OpenTerminal(OpenTerminal),
    /// Starts one more local forward (a retry, or a failed one moved to another port).
    StartForward(ForwardSpec),
    StopForward(ForwardSpec),
}
impl Control {
    pub fn new(context: Context, concurrency: usize) -> (Arc<Self>, Receiver<Command>) {
        let (commands, rx) = async_channel::unbounded();
        let (ready, _) = watch::channel(None);
        (
            Arc::new(Self {
                context: Arc::new(context),
                active: AtomicBool::new(true),
                commands,
                ready,
                transfers: Semaphore::new(concurrency),
                transfer_limit: concurrency as u32,
            }),
            rx,
        )
    }
    pub async fn files(&self) -> Result<Arc<Files>> {
        let mut ready = self.ready.subscribe();
        loop {
            if let Some(result) = ready.borrow_and_update().clone() {
                return result.map_err(anyhow::Error::msg);
            }
            tokio::select! { biased;
                _ = self.context.cancel.cancelled() => bail!("Session disconnected"),
                result = ready.changed() => result.context("Session stopped")?,
            }
        }
    }
}
pub(crate) async fn run(control: Arc<Control>, commands: Receiver<Command>) {
    let ctx = &control.context;
    let mut proxy = None;
    let connection = tokio::select! { biased;
        _ = ctx.cancel.cancelled() => Err(anyhow::anyhow!("Connection cancelled")),
        result = connect(ctx, &mut proxy) => result,
    };
    let mut tasks = tokio::task::JoinSet::new();
    match connection {
        // The jump hosts carry the connection, so they live as long as it does.
        Ok((ssh, _jumps)) => {
            let ssh = Arc::new(ssh);
            let files = Arc::new(Files::new(ssh.clone()));
            if ctx.opens_forwards() {
                let count = ctx.host.local_forwards.len();
                let tunnels = if count == 1 { "tunnel" } else { "tunnels" };
                ctx.step(
                    &ctx.host,
                    ConnectStep::OpeningForwards,
                    Some(format!("{count} {tunnels}")),
                );
            }
            // Stops each tunnel on its own, by local address.
            let mut tunnels: HashMap<(String, u16, ForwardKind), CancellationToken> =
                HashMap::new();
            for spec in &ctx.host.local_forwards {
                if ctx.open_forwards {
                    let token = ctx.cancel.child_token();
                    tunnels.insert(
                        (spec.bind_addr.clone(), spec.bind_port, spec.kind),
                        token.clone(),
                    );
                    tasks.spawn(forward::listen(
                        ctx.clone(),
                        ssh.clone(),
                        spec.clone(),
                        token,
                    ));
                } else {
                    // Listed as closed, so the sidebar can offer to open it.
                    ctx.emit(Event::Forward {
                        session: ctx.id,
                        spec: spec.clone(),
                        state: ForwardState::Stopped,
                        active_conns: 0,
                    });
                }
            }
            control.ready.send_replace(Some(Ok(files)));
            ctx.state(SessionState::Connected);
            let mut monitor = tokio::time::interval(Duration::from_millis(250));
            let reason = loop {
                tokio::select! { biased;
                    _ = ctx.cancel.cancelled() => break None,
                    _ = monitor.tick() => if ssh.is_closed() { break Some("SSH connection closed by the server".to_string()); },
                    command = commands.recv() => match command {
                        Ok(Command::OpenTerminal(command)) => {
                            tasks.spawn(terminal::run(ctx.clone(), ssh.clone(), command, ctx.cancel.child_token()));
                        }
                        Ok(Command::StartForward(spec)) => {
                            let token = ctx.cancel.child_token();
                            tunnels.insert((spec.bind_addr.clone(), spec.bind_port, spec.kind), token.clone());
                            tasks.spawn(forward::listen(ctx.clone(), ssh.clone(), spec, token));
                        }
                        Ok(Command::StopForward(spec)) => {
                            if let Some(token) = tunnels.remove(&(spec.bind_addr, spec.bind_port, spec.kind)) {
                                token.cancel();
                            }
                        }
                        Err(_) => break None,
                    },
                    Some(result) = tasks.join_next(), if !tasks.is_empty() => if let Err(error) = result { ctx.notice(NoticeLevel::Error, format!("Session worker failed: {error}")); },
                }
            };
            ctx.cancel.cancel();
            control.ready.send_replace(Some(Err(reason
                .clone()
                .unwrap_or_else(|| "Session disconnected".into()))));
            // Worker cancellation closes local listeners, sockets and terminal channels.
            while tasks.join_next().await.is_some() {}
            // Transfer workers hold permits until partial-file cleanup is complete.
            let _ = tokio::time::timeout(
                Duration::from_secs(35),
                control.transfers.acquire_many(control.transfer_limit),
            )
            .await;
            let _ = tokio::time::timeout(
                Duration::from_secs(2),
                ssh.disconnect(russh::Disconnect::ByApplication, "Disconnected", "en"),
            )
            .await;
            ctx.state(SessionState::Disconnected { reason });
        }
        Err(error) => {
            let cancelled = ctx.cancel.is_cancelled();
            let message = format!("{error:#}");
            control.ready.send_replace(Some(Err(message.clone())));
            if cancelled {
                ctx.state(SessionState::Disconnected { reason: None });
            } else {
                let connect_error = if let Some(proxy) = &mut proxy {
                    proxy.settle_stderr().await;
                    proxy.error(&message)
                } else if let Some(command) = first_hop(ctx).proxy_command.as_deref() {
                    let not_found = error.chain().any(|e| {
                        e.downcast_ref::<std::io::Error>()
                            .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound)
                    });
                    proxy::classify(command, &message, "", not_found)
                } else {
                    ConnectError {
                        kind: if message.contains("Host key") || message.contains("known_hosts") {
                            ConnectErrorKind::HostKey
                        } else if message.contains("authentication") {
                            ConnectErrorKind::Authentication
                        } else {
                            ConnectErrorKind::Transport
                        },
                        message,
                        hint: None,
                    }
                };
                tracing::warn!(session = ctx.id.0, message = %connect_error.message, "Connection failed");
                // Publish failure only once connect() can no longer reuse this session.
                control.active.store(false, Ordering::SeqCst);
                ctx.cancel.cancel();
                ctx.state(SessionState::Failed {
                    error: connect_error,
                });
            }
            ctx.cancel.cancel();
        }
    }
    commands.close();
    while let Ok(command) = commands.try_recv() {
        // A queued forward has nothing to clean up: its session is gone.
        if let Command::OpenTerminal(command) = command {
            command.terminal.finish();
            ctx.emit(Event::TerminalExited {
                id: command.terminal.id(),
                status: None,
            });
        }
    }
    drop(proxy);
    control.active.store(false, Ordering::SeqCst);
}
/// Seconds between keepalives of a host that does not set them. Without any, a connection lost
/// while the PC sleeps or the network changes would look alive forever. `ServerAliveInterval 0`
/// still turns them off.
const DEFAULT_KEEPALIVE: u32 = 30;

/// The machine the connection starts from.
fn first_hop(ctx: &Context) -> &HostConfig {
    ctx.jumps.first().unwrap_or(&ctx.host)
}

/// Connects to each jump host through the one before, and last to the host.
async fn connect(ctx: &Arc<Context>, proxy: &mut Option<Proxy>) -> Result<(Ssh, Vec<Ssh>)> {
    let mut jumps: Vec<Ssh> = Vec::new();
    for hop in &ctx.jumps {
        let ssh = open_hop(ctx, hop, jumps.last(), proxy, false)
            .await
            .with_context(|| format!("Jump host {}", hop.alias))?;
        jumps.push(ssh);
    }
    let ssh = open_hop(ctx, &ctx.host, jumps.last(), proxy, true).await?;
    Ok((ssh, jumps))
}

async fn open_hop(
    ctx: &Arc<Context>,
    hop: &HostConfig,
    through: Option<&Ssh>,
    proxy: &mut Option<Proxy>,
    is_target: bool,
) -> Result<Ssh> {
    let config = Arc::new(russh::client::Config {
        keepalive_interval: Some(hop.server_alive_interval.unwrap_or(DEFAULT_KEEPALIVE))
            .filter(|v| *v > 0)
            .map(|v| Duration::from_secs(v as u64)),
        keepalive_max: hop.server_alive_count_max as usize,
        ..Default::default()
    });
    let handler = Handler {
        context: ctx.clone(),
        host: hop.clone(),
        is_target,
    };
    let mut ssh = if let Some(through) = through {
        ctx.step(hop, ConnectStep::Connecting, None);
        let channel = through
            .channel_open_direct_tcpip(hop.hostname.as_str(), hop.port as u32, "127.0.0.1", 0)
            .await
            .with_context(|| format!("Open a channel to {}:{}", hop.hostname, hop.port))?;
        ctx.step(hop, ConnectStep::Handshake, None);
        russh::client::connect_stream(config, channel.into_stream(), handler).await?
    } else if hop.proxy_command.is_some() {
        ctx.step(hop, ConnectStep::StartingProxy, None);
        let (child, mut stream) = proxy::spawn(hop)?;
        *proxy = Some(child);
        let (reporter, reached) = (ctx.clone(), hop.clone());
        stream.on_first_byte(move || reporter.step(&reached, ConnectStep::Handshake, None));
        russh::client::connect_stream(config, stream, handler).await?
    } else {
        ctx.step(hop, ConnectStep::Resolving, Some(hop.hostname.clone()));
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        let addresses: Vec<_> = tokio::time::timeout_at(
            deadline,
            tokio::net::lookup_host((hop.hostname.as_str(), hop.port)),
        )
        .await
        .context("SSH connection timed out")??
        .collect();
        let address = addresses.first().map(ToString::to_string);
        ctx.step(hop, ConnectStep::Connecting, address);
        let stream = tokio::time::timeout_at(deadline, TcpStream::connect(&addresses[..]))
            .await
            .context("SSH connection timed out")??;
        stream.set_nodelay(true)?;
        ctx.step(hop, ConnectStep::Handshake, None);
        russh::client::connect_stream(config, stream, handler).await?
    };
    auth::authenticate(&mut ssh, ctx, hop).await?;
    Ok(ssh)
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod tests;
