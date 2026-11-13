use crate::{
    Event, ForwardKind, ForwardSpec, ForwardState,
    session::{Context, Ssh},
    socks,
};
use parking_lot::Mutex;
use std::{sync::Arc, time::Duration};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

/// A local program that connects to the SOCKS port and then says nothing goes away after this.
const SOCKS_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) async fn listen(
    ctx: Arc<Context>,
    ssh: Arc<Ssh>,
    spec: ForwardSpec,
    cancel: CancellationToken,
) {
    if spec.kind == ForwardKind::Remote {
        return listen_remote(ctx, ssh, spec, cancel).await;
    }
    let listener = match TcpListener::bind((spec.bind_addr.as_str(), spec.bind_port)).await {
        Ok(listener) => listener,
        Err(error) if is_in_use(&error) => {
            let owner = port_owner(spec.bind_port).await;
            emit(&ctx, &spec, ForwardState::InUse { owner }, 0);
            return;
        }
        Err(error) => {
            emit(
                &ctx,
                &spec,
                ForwardState::Failed(bind_error(&spec, &error)),
                0,
            );
            return;
        }
    };
    let active = Arc::new(Active::default());
    let mut connections = tokio::task::JoinSet::new();
    emit(&ctx, &spec, ForwardState::Listening, 0);
    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => break,
            result = listener.accept() => match result {
                Ok((mut socket, peer)) => {
                    let (ssh, ctx, spec, active, cancel) = (ssh.clone(), ctx.clone(), spec.clone(), active.clone(), cancel.clone());
                    connections.spawn(async move {
                        let request = if spec.kind == ForwardKind::Dynamic {
                            let request = tokio::select! {
                                _ = cancel.cancelled() => return,
                                result = tokio::time::timeout(SOCKS_TIMEOUT, socks::accept(&mut socket)) => result,
                            };
                            match request {
                                Ok(Ok(request)) => Some(request),
                                Ok(Err(error)) => {
                                    tracing::debug!(%error, "SOCKS request refused");
                                    return;
                                }
                                Err(_) => {
                                    tracing::debug!("SOCKS client said nothing");
                                    return;
                                }
                            }
                        } else {
                            None
                        };
                        let (host, port) = match &request {
                            Some(request) => (request.host.clone(), request.port),
                            None => (spec.remote_host.clone(), spec.remote_port),
                        };
                        let channel = tokio::select! {
                            _ = cancel.cancelled() => return,
                            result = ssh.channel_open_direct_tcpip(host, port as u32, peer.ip().to_string(), peer.port() as u32) => result,
                        };
                        let channel = match channel {
                            Ok(channel) => channel,
                            Err(error) => {
                                tracing::debug!(%error, port, "Forward refused");
                                match &request {
                                    // One site that does not answer says nothing about the tunnel.
                                    Some(request) => {
                                        let _ = socks::reply(&mut socket, request.version, false).await;
                                    }
                                    None => emit(&ctx, &spec, ForwardState::ServerDown, active.count()),
                                }
                                return;
                            }
                        };
                        if let Some(request) = &request
                            && socks::reply(&mut socket, request.version, true).await.is_err()
                        {
                            return;
                        }
                        active.opened(&ctx, &spec);
                        let mut stream = channel.into_stream();
                        tokio::select! { _ = cancel.cancelled() => {}, result = tokio::io::copy_bidirectional(&mut socket, &mut stream) => { if let Err(error) = result { tracing::debug!(%error, "Forward connection closed"); } } }
                        active.closed(&ctx, &spec, !cancel.is_cancelled());
                    });
                }
                Err(error) => { emit(&ctx, &spec, ForwardState::Failed(error.to_string()), active.count()); break; }
            },
            Some(_) = connections.join_next(), if !connections.is_empty() => {},
        }
    }
    drop(listener);
    connections.abort_all();
    while connections.join_next().await.is_some() {}
    emit(&ctx, &spec, ForwardState::Stopped, 0);
}

/// Open connections of one forward. Each change is reported under the lock, so the counts
/// arrive in order.
#[derive(Default)]
struct Active(Mutex<u32>);

impl Active {
    fn count(&self) -> u32 {
        *self.0.lock()
    }

    fn opened(&self, ctx: &Context, spec: &ForwardSpec) {
        let mut count = self.0.lock();
        *count += 1;
        emit(ctx, spec, ForwardState::Listening, *count);
    }

    /// `report` is false while the forward closes: its `Stopped` comes next.
    fn closed(&self, ctx: &Context, spec: &ForwardSpec, report: bool) {
        let mut count = self.0.lock();
        *count -= 1;
        if report {
            emit(ctx, spec, ForwardState::Listening, *count);
        }
    }
}

/// A port of the server listening for this PC.
pub(crate) struct RemoteForward {
    spec: ForwardSpec,
    active: Active,
    cancel: CancellationToken,
}

/// Asks the server to listen, and stops it when cancelled. Connections arrive at the client
/// handler, which passes them to `accept_remote`.
async fn listen_remote(
    ctx: Arc<Context>,
    ssh: Arc<Ssh>,
    spec: ForwardSpec,
    cancel: CancellationToken,
) {
    let port = spec.bind_port as u32;
    let forward = Arc::new(RemoteForward {
        spec: spec.clone(),
        active: Active::default(),
        cancel: cancel.child_token(),
    });
    // Closing or failing the listener also closes every connection it accepted.
    let _connections_guard = forward.cancel.clone().drop_guard();
    // Before asking: the first connection can arrive as soon as the server agrees.
    ctx.remote_forwards.lock().insert(port, forward);
    let opened = tokio::select! {
        _ = cancel.cancelled() => {
            ctx.remote_forwards.lock().remove(&port);
            emit(&ctx, &spec, ForwardState::Stopped, 0);
            return;
        }
        result = ssh.tcpip_forward(spec.bind_addr.as_str(), port) => result,
    };
    if let Err(error) = opened {
        ctx.remote_forwards.lock().remove(&port);
        tracing::debug!(%error, port, "Remote forward refused");
        let message = format!(
            "The server refused to listen on port {port}. It may be in use, or its \
             AllowTcpForwarding setting may forbid it"
        );
        emit(&ctx, &spec, ForwardState::Failed(message), 0);
        return;
    }
    emit(&ctx, &spec, ForwardState::Listening, 0);
    cancel.cancelled().await;
    ctx.remote_forwards.lock().remove(&port);
    if let Err(error) = ssh
        .cancel_tcpip_forward(spec.bind_addr.as_str(), port)
        .await
    {
        tracing::debug!(%error, port, "Could not close a remote forward");
    }
    emit(&ctx, &spec, ForwardState::Stopped, 0);
}

/// A connection that came through the server: it goes on to the program on this PC.
pub(crate) async fn accept_remote(
    ctx: Arc<Context>,
    forward: Arc<RemoteForward>,
    channel: russh::Channel<russh::client::Msg>,
    reply: russh::client::ChannelOpenHandle,
) {
    let spec = &forward.spec;
    let target = (spec.remote_host.as_str(), spec.remote_port);
    let socket = tokio::select! { biased;
        _ = forward.cancel.cancelled() => return,
        _ = ctx.cancel.cancelled() => return,
        socket = tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(target)) => socket,
    };
    let Ok(Ok(mut socket)) = socket else {
        tracing::debug!(port = spec.remote_port, "Nothing answers on this PC");
        emit(&ctx, spec, ForwardState::ServerDown, forward.active.count());
        return;
    };
    reply.accept().await;
    forward.active.opened(&ctx, spec);
    tokio::spawn(async move {
        let mut stream = channel.into_stream();
        tokio::select! { biased;
            _ = forward.cancel.cancelled() => {},
            _ = ctx.cancel.cancelled() => {},
            result = tokio::io::copy_bidirectional(&mut socket, &mut stream) => {
                if let Err(error) = result {
                    tracing::debug!(%error, "Remote forward connection closed");
                }
            }
        }
        let report = !ctx.cancel.is_cancelled() && !forward.cancel.is_cancelled();
        forward.active.closed(&ctx, &forward.spec, report);
    });
}

fn emit(ctx: &Context, spec: &ForwardSpec, state: ForwardState, active_conns: u32) {
    ctx.emit(Event::Forward {
        session: ctx.id,
        spec: spec.clone(),
        state,
        active_conns,
    });
}

fn is_in_use(error: &std::io::Error) -> bool {
    // std maps WSAEADDRINUSE to this kind.
    error.kind() == std::io::ErrorKind::AddrInUse
}

/// Name of the program listening on a local port, from the tools every Windows has.
async fn port_owner(port: u16) -> Option<String> {
    let listeners = tokio::process::Command::new("netstat")
        .args(["-ano", "-p", "TCP"])
        .creation_flags(crate::CREATE_NO_WINDOW)
        .output()
        .await
        .ok()?;
    let listeners = String::from_utf8_lossy(&listeners.stdout);
    let suffix = format!(":{port}");
    // Columns: protocol, local address, remote address, state (localized), process id.
    let pid = listeners.lines().find_map(|line| {
        let columns: Vec<_> = line.split_whitespace().collect();
        match columns.as_slice() {
            [_, local, remote, _, pid] if local.ends_with(&suffix) && remote.ends_with(":0") => {
                pid.parse::<u32>().ok()
            }
            _ => None,
        }
    })?;
    let filter = format!("PID eq {pid}");
    let process = tokio::process::Command::new("tasklist")
        .args(["/FI", filter.as_str(), "/FO", "CSV", "/NH"])
        .creation_flags(crate::CREATE_NO_WINDOW)
        .output()
        .await
        .ok()?;
    // Output: "ssh.exe","1234",...
    String::from_utf8_lossy(&process.stdout)
        .split('"')
        .nth(1)
        .map(str::to_string)
}

/// Readable reason for a failed local bind, with the usual Windows causes spelled out.
fn bind_error(spec: &ForwardSpec, error: &std::io::Error) -> String {
    let address = format!("{}:{}", spec.bind_addr, spec.bind_port);
    // std maps WSAEACCES to `PermissionDenied`.
    match error.kind() {
        std::io::ErrorKind::PermissionDenied => format!(
            "Windows blocks port {address} (reserved by Hyper-V/WSL or needs permissions; see `netsh int ipv4 show excludedportrange protocol=tcp`)"
        ),
        _ => format!("Could not listen on {address}: {error}"),
    }
}
