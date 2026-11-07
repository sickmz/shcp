//! Real connections on this machine: SSH servers in the test, a jump between them and a SOCKS
//! proxy through them, down to a service that echoes what it gets.
use super::*;
use russh::{
    Channel,
    keys::{PrivateKey, ssh_key::private::Ed25519Keypair},
    server::{self, Auth, ChannelOpenHandle, Msg, Session},
};
use std::{path::PathBuf, sync::atomic::AtomicU64};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

type Opened = Arc<Mutex<Vec<(String, u32)>>>;

/// What a test needs to act as the server after a client logged in.
#[derive(Clone, Default)]
struct ServerSide {
    handle: Arc<Mutex<Option<server::Handle>>>,
    /// The ports it was asked to listen on.
    listening: Opened,
    /// Commands it was asked to run, and the status it ends them with.
    commands: Arc<Mutex<Vec<String>>>,
}
impl ServerSide {
    fn handle(&self) -> server::Handle {
        self.handle.lock().clone().expect("nobody logged in")
    }
}

struct TestServer {
    opened: Opened,
    side: ServerSide,
}
impl server::Handler for TestServer {
    type Error = anyhow::Error;
    async fn auth_none(&mut self, _: &str) -> Result<Auth> {
        Ok(Auth::Accept)
    }
    async fn auth_succeeded(&mut self, session: &mut Session) -> Result<()> {
        *self.side.handle.lock() = Some(session.handle());
        Ok(())
    }
    async fn channel_open_session(
        &mut self,
        _: Channel<Msg>,
        reply: ChannelOpenHandle,
        _: &mut Session,
    ) -> Result<()> {
        reply.accept().await;
        Ok(())
    }
    async fn tcpip_forward(
        &mut self,
        address: &str,
        port: &mut u32,
        _: &mut Session,
    ) -> Result<bool> {
        // Port 1 stands for one the server refuses.
        self.side
            .listening
            .lock()
            .push((address.to_string(), *port));
        Ok(*port != 1)
    }
    async fn cancel_tcpip_forward(
        &mut self,
        address: &str,
        port: u32,
        _: &mut Session,
    ) -> Result<bool> {
        let mut listening = self.side.listening.lock();
        let before = listening.len();
        listening.retain(|entry| *entry != (address.to_string(), port));
        Ok(listening.len() < before)
    }
    async fn exec_request(
        &mut self,
        channel: russh::ChannelId,
        command: &[u8],
        session: &mut Session,
    ) -> Result<()> {
        let command = String::from_utf8_lossy(command).into_owned();
        self.side.commands.lock().push(command.clone());
        session.channel_success(channel)?;
        // A command with "fail" in it ends as a failed `cp` does.
        let status = if command.contains("fail") {
            session.extended_data(channel, 1, b"cp: cannot stat: no such file".to_vec())?;
            1
        } else {
            0
        };
        session.exit_status_request(channel, status)?;
        session.close(channel)?;
        Ok(())
    }
    async fn channel_open_direct_tcpip(
        &mut self,
        channel: Channel<Msg>,
        host: &str,
        port: u32,
        _: &str,
        _: u32,
        reply: ChannelOpenHandle,
        _: &mut Session,
    ) -> Result<()> {
        self.opened.lock().push((host.to_string(), port));
        // Dropping the reply refuses the channel, as a server does when nothing listens there.
        let Ok(mut socket) = TcpStream::connect((host, port as u16)).await else {
            return Ok(());
        };
        reply.accept().await;
        tokio::spawn(async move {
            let mut stream = channel.into_stream();
            let _ = tokio::io::copy_bidirectional(&mut socket, &mut stream).await;
        });
        Ok(())
    }
}

/// A server on a free port: its port, and the channels it was asked to open.
async fn ssh_server(seed: u8) -> (u16, Opened) {
    let (port, opened, _) = ssh_server_with_side(seed).await;
    (port, opened)
}

async fn ssh_server_with_side(seed: u8) -> (u16, Opened, ServerSide) {
    let side = ServerSide::default();
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let opened = Opened::default();
    let config = Arc::new(server::Config {
        keys: vec![PrivateKey::from(Ed25519Keypair::from_seed(&[seed; 32]))],
        ..Default::default()
    });
    let seen = opened.clone();
    let shared = side.clone();
    tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            let (config, opened, side) = (config.clone(), seen.clone(), shared.clone());
            tokio::spawn(async move {
                if let Ok(session) =
                    server::run_stream(config, socket, TestServer { opened, side }).await
                {
                    let _ = session.await;
                }
            });
        }
    });
    (port, opened, side)
}

/// Sends back whatever arrives.
async fn echo_service() -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let (mut read, mut write) = socket.split();
                let _ = tokio::io::copy(&mut read, &mut write).await;
            });
        }
    });
    port
}

/// A port nothing listens on.
async fn closed_port() -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    listener.local_addr().unwrap().port()
}

fn test_host(alias: &str, port: u16) -> HostConfig {
    HostConfig {
        id: HostId(alias.into()),
        alias: alias.into(),
        hostname: "127.0.0.1".into(),
        port,
        user: "tester".into(),
        ..Default::default()
    }
}

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "shcp-session-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A session context whose user accepts every host key.
fn context(dir: &Scratch, host: HostConfig, jumps: Vec<HostConfig>) -> Arc<Context> {
    let (events, receiver) = async_channel::unbounded();
    let context = Arc::new(Context {
        id: SessionId(1),
        host,
        jumps,
        remote_forwards: Mutex::new(HashMap::new()),
        paths: AppPaths {
            config_dir: dir.0.clone(),
            data_dir: dir.0.clone(),
            log_dir: dir.0.clone(),
            known_hosts: dir.0.join("known_hosts"),
            ssh_config: dir.0.join("config"),
        },
        events,
        cancel: CancellationToken::new(),
        prompts: Prompts::new(),
        known_hosts_lock: Arc::new(Mutex::new(())),
        use_agent: false,
        open_forwards: false,
        persist_terminals: false,
        tmux: Default::default(),
    });
    let answers = context.prompts.host_tx.clone();
    tokio::spawn(async move {
        while let Ok(event) = receiver.recv().await {
            if let Event::Session {
                state: SessionState::AwaitingHostKey { .. },
                ..
            } = event
            {
                let _ = answers.send(true).await;
            }
        }
    });
    context
}

/// Connects within a time limit, so a hang fails the test.
async fn connected(context: &Arc<Context>) -> Result<(Ssh, Vec<Ssh>)> {
    let mut proxy = None;
    tokio::time::timeout(Duration::from_secs(20), connect(context, &mut proxy))
        .await
        .expect("the connection hung")
}

#[test]
fn a_direct_connection_plans_its_steps_in_order() {
    let host = test_host("target", 22);
    let plan: Vec<_> = connection_plan(&[], &host, false)
        .into_iter()
        .map(|(_, step)| step)
        .collect();
    assert_eq!(
        plan,
        [
            ConnectStep::Resolving,
            ConnectStep::Connecting,
            ConnectStep::Handshake,
            ConnectStep::VerifyingHostKey,
            ConnectStep::Authenticating,
        ]
    );
}

#[test]
fn a_proxy_command_takes_the_place_of_resolving_and_connecting() {
    let host = HostConfig {
        proxy_command: Some("nc %h %p".into()),
        ..test_host("target", 22)
    };
    let plan = connection_plan(&[], &host, false);
    assert_eq!(plan[0], (0, ConnectStep::StartingProxy));
    assert_eq!(plan[1], (0, ConnectStep::Handshake));
}

#[tokio::test]
async fn a_proxy_reports_its_first_byte_once() {
    let host = HostConfig {
        proxy_command: Some("echo hello".into()),
        ..test_host("target", 22)
    };
    let (_proxy, mut stream) = proxy::spawn(&host).unwrap();
    let reported = Arc::new(AtomicU64::new(0));
    let counter = reported.clone();
    stream.on_first_byte(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    });
    assert_eq!(reported.load(Ordering::SeqCst), 0);
    let mut text = Vec::new();
    stream.read_to_end(&mut text).await.unwrap();
    assert!(!text.is_empty());
    assert_eq!(reported.load(Ordering::SeqCst), 1);
}

#[test]
fn each_jump_host_adds_its_steps_and_forwards_come_last() {
    let (jump, host) = (test_host("jump", 22), test_host("target", 22));
    let plan = connection_plan(std::slice::from_ref(&jump), &host, true);
    assert_eq!(plan.len(), 10);
    assert_eq!(plan[5], (1, ConnectStep::Connecting));
    assert_eq!(plan[9], (1, ConnectStep::OpeningForwards));
}

#[tokio::test]
async fn a_connection_goes_through_its_jump_hosts() {
    let dir = Scratch::new();
    let (first, first_opened) = ssh_server(1).await;
    let (second, second_opened) = ssh_server(2).await;
    let (target, target_opened) = ssh_server(3).await;
    let echo = echo_service().await;
    let ctx = context(
        &dir,
        test_host("target", target),
        vec![test_host("first", first), test_host("second", second)],
    );
    let (ssh, jumps) = connected(&ctx).await.unwrap();
    assert_eq!(jumps.len(), 2);
    // Each machine opened the way to the next, and nothing went around them.
    assert_eq!(
        *first_opened.lock(),
        [("127.0.0.1".to_string(), second as u32)]
    );
    assert_eq!(
        *second_opened.lock(),
        [("127.0.0.1".to_string(), target as u32)]
    );
    // Every machine had its key checked, under its own name.
    let known = std::fs::read_to_string(&ctx.paths.known_hosts).unwrap();
    for port in [first, second, target] {
        assert!(known.contains(&format!("[127.0.0.1]:{port} ")), "{known}");
    }
    // Data really flows through the whole chain.
    let channel = ssh
        .channel_open_direct_tcpip("127.0.0.1", echo as u32, "127.0.0.1", 0)
        .await
        .unwrap();
    assert_eq!(
        *target_opened.lock(),
        [("127.0.0.1".to_string(), echo as u32)]
    );
    let mut stream = channel.into_stream();
    stream.write_all(b"through the jump").await.unwrap();
    let mut answer = [0; 16];
    stream.read_exact(&mut answer).await.unwrap();
    assert_eq!(&answer, b"through the jump");
}

#[tokio::test]
async fn a_jump_host_that_cannot_be_reached_is_named() {
    let dir = Scratch::new();
    let (target, _) = ssh_server(1).await;
    let closed = closed_port().await;
    let ctx = context(
        &dir,
        test_host("target", target),
        vec![test_host("bastion", closed)],
    );
    let error = connected(&ctx).await.err().expect("nothing listens there");
    assert!(format!("{error:#}").contains("Jump host bastion"));
}

#[tokio::test]
async fn a_target_the_jump_host_cannot_reach_is_an_error() {
    let dir = Scratch::new();
    let (bastion, _) = ssh_server(1).await;
    let closed = closed_port().await;
    let ctx = context(
        &dir,
        test_host("target", closed),
        vec![test_host("bastion", bastion)],
    );
    let error = connected(&ctx).await.err().expect("nothing listens there");
    assert!(format!("{error:#}").contains(&format!("127.0.0.1:{closed}")));
}

/// A remote forward from `port` on the server to `target` on this PC.
fn remote_spec(port: u16, target: u16) -> ForwardSpec {
    ForwardSpec {
        bind_addr: "127.0.0.1".into(),
        bind_port: port,
        remote_host: "127.0.0.1".into(),
        remote_port: target,
        kind: ForwardKind::Remote,
    }
}

/// Waits until a condition holds.
async fn eventually(what: &str, condition: impl Fn() -> bool) {
    for _ in 0..200 {
        if condition() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("{what} never happened");
}

#[tokio::test]
async fn a_remote_forward_brings_connections_from_the_server_to_this_pc() {
    let dir = Scratch::new();
    let (server_port, _, side) = ssh_server_with_side(1).await;
    let echo = echo_service().await;
    let ctx = context(&dir, test_host("target", server_port), vec![]);
    let (ssh, _jumps) = connected(&ctx).await.unwrap();
    let cancel = CancellationToken::new();
    let forward = tokio::spawn(forward::listen(
        ctx.clone(),
        Arc::new(ssh),
        remote_spec(9000, echo),
        cancel.clone(),
    ));
    let listening = side.listening.clone();
    eventually("the server listening", move || {
        *listening.lock() == [("127.0.0.1".to_string(), 9000)]
    })
    .await;

    // Someone connects to the port on the server: it reaches the service on this PC.
    let channel = side
        .handle()
        .channel_open_forwarded_tcpip("127.0.0.1", 9000, "10.0.0.1", 5000)
        .await
        .unwrap();
    let mut stream = channel.into_stream();
    stream.write_all(b"from the server").await.unwrap();
    let mut answer = [0; 15];
    stream.read_exact(&mut answer).await.unwrap();
    assert_eq!(&answer, b"from the server");

    // A port nobody asked for is refused.
    assert!(
        side.handle()
            .channel_open_forwarded_tcpip("127.0.0.1", 9001, "10.0.0.1", 5000)
            .await
            .is_err()
    );

    // Closing it tells the server to stop listening.
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), forward)
        .await
        .expect("the forward did not stop")
        .unwrap();
    assert!(side.listening.lock().is_empty());
    assert!(ctx.remote_forwards.lock().is_empty());
}

#[tokio::test]
async fn a_remote_forward_the_server_refuses_says_so() {
    let dir = Scratch::new();
    let (server_port, _, _) = ssh_server_with_side(1).await;
    let (events, receiver) = async_channel::unbounded();
    let mut ctx = Arc::into_inner(context(&dir, test_host("target", server_port), vec![])).unwrap();
    ctx.events = events;
    let ctx = Arc::new(ctx);
    let answers = ctx.prompts.host_tx.clone();
    let _ = answers.try_send(true);
    let (ssh, _jumps) = connected(&ctx).await.unwrap();
    forward::listen(
        ctx.clone(),
        Arc::new(ssh),
        remote_spec(1, 80),
        CancellationToken::new(),
    )
    .await;
    let failed = std::iter::from_fn(|| receiver.try_recv().ok()).any(|event| {
        matches!(
            event,
            Event::Forward {
                state: ForwardState::Failed(_),
                ..
            }
        )
    });
    assert!(failed);
    assert!(ctx.remote_forwards.lock().is_empty());
}

#[tokio::test]
async fn the_agent_stays_closed_unless_the_host_allows_it() {
    let dir = Scratch::new();
    let (server_port, _, side) = ssh_server_with_side(1).await;
    let ctx = context(&dir, test_host("target", server_port), vec![]);
    let (_ssh, _jumps) = connected(&ctx).await.unwrap();
    assert!(side.handle().channel_open_agent().await.is_err());
}

/// One test, since it sets where the agent is for the whole process.
#[cfg(windows)]
#[tokio::test]
async fn an_allowed_agent_reaches_the_server_but_never_a_jump_host() {
    use tokio::net::windows::named_pipe::ServerOptions;
    // Stands for the agent: it echoes.
    let name = format!(r"\\.\pipe\shcp-test-agent-{}", std::process::id());
    let mut pipe = ServerOptions::new().create(&name).unwrap();
    // SAFETY: no other test reads this variable: their contexts do not use the agent.
    unsafe { std::env::set_var("SSH_AUTH_SOCK", &name) };
    let serve_name = name.clone();
    tokio::spawn(async move {
        loop {
            pipe.connect().await.unwrap();
            let mut connected = pipe;
            pipe = ServerOptions::new().create(&serve_name).unwrap();
            tokio::spawn(async move {
                let (mut read, mut write) = tokio::io::split(&mut connected);
                let _ = tokio::io::copy(&mut read, &mut write).await;
            });
        }
    });

    let dir = Scratch::new();
    let (bastion, _, bastion_side) = ssh_server_with_side(1).await;
    let (target, _, target_side) = ssh_server_with_side(2).await;
    let mut jump = test_host("bastion", bastion);
    // Even with the setting on: it is not the machine the session is for.
    jump.forward_agent = true;
    let mut host = test_host("target", target);
    host.forward_agent = true;
    let ctx = context(&dir, host, vec![jump]);
    let (_ssh, _jumps) = connected(&ctx).await.unwrap();

    assert!(bastion_side.handle().channel_open_agent().await.is_err());
    let channel = target_side.handle().channel_open_agent().await.unwrap();
    let mut stream = channel.into_stream();
    stream.write_all(b"sign this").await.unwrap();
    let mut answer = [0; 9];
    stream.read_exact(&mut answer).await.unwrap();
    assert_eq!(&answer, b"sign this");
}

#[tokio::test]
async fn a_copy_runs_cp_with_every_path_quoted_and_reports_what_it_says() {
    let dir = Scratch::new();
    let (server_port, _, side) = ssh_server_with_side(1).await;
    let ctx = context(&dir, test_host("target", server_port), vec![]);
    let (ssh, _jumps) = connected(&ctx).await.unwrap();
    crate::files::copy_on_server(&ssh, "/a b/it's", "/c/d")
        .await
        .unwrap();
    assert_eq!(
        *side.commands.lock(),
        [r"cp -a -- '/a b/it'\''s' '/c/d'".to_string()]
    );
    let error = crate::files::copy_on_server(&ssh, "/fail", "/c")
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("cannot stat"), "{error:#}");
}

/// Connects to the proxy as soon as it listens.
async fn socks_client(port: u16) -> TcpStream {
    for _ in 0..100 {
        if let Ok(stream) = TcpStream::connect(("127.0.0.1", port)).await {
            return stream;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the SOCKS proxy never listened");
}

/// A SOCKS 5 "connect", up to the status the proxy answers.
async fn socks_connect(stream: &mut TcpStream, port: u16) -> u8 {
    stream.write_all(&[5, 1, 0]).await.unwrap();
    let mut greeting = [0; 2];
    stream.read_exact(&mut greeting).await.unwrap();
    assert_eq!(greeting, [5, 0]);
    let name = b"localhost";
    stream
        .write_all(&[5, 1, 0, 3, name.len() as u8])
        .await
        .unwrap();
    stream.write_all(name).await.unwrap();
    stream.write_all(&port.to_be_bytes()).await.unwrap();
    let mut reply = [0; 10];
    stream.read_exact(&mut reply).await.unwrap();
    reply[1]
}

#[tokio::test]
async fn a_socks_proxy_reaches_services_through_the_connection() {
    let dir = Scratch::new();
    let (server_port, opened) = ssh_server(1).await;
    let echo = echo_service().await;
    let closed = closed_port().await;
    let ctx = context(&dir, test_host("target", server_port), vec![]);
    let (ssh, _jumps) = connected(&ctx).await.unwrap();
    let proxy_port = closed_port().await;
    let spec = ForwardSpec::socks("127.0.0.1".into(), proxy_port);
    let cancel = CancellationToken::new();
    let proxy = tokio::spawn(forward::listen(
        ctx.clone(),
        Arc::new(ssh),
        spec,
        cancel.clone(),
    ));

    let mut stream = socks_client(proxy_port).await;
    assert_eq!(socks_connect(&mut stream, echo).await, 0);
    stream.write_all(b"via socks").await.unwrap();
    let mut answer = [0; 9];
    stream.read_exact(&mut answer).await.unwrap();
    assert_eq!(&answer, b"via socks");
    // The server was told the name: it resolves it, not this PC.
    assert_eq!(*opened.lock(), [("localhost".to_string(), echo as u32)]);

    // A service that is down gets the client a refusal, and the proxy goes on.
    let mut refused = socks_client(proxy_port).await;
    assert_eq!(socks_connect(&mut refused, closed).await, 5);
    let mut again = socks_client(proxy_port).await;
    assert_eq!(socks_connect(&mut again, echo).await, 0);

    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), proxy)
        .await
        .expect("the proxy did not stop")
        .unwrap();
    assert!(TcpStream::connect(("127.0.0.1", proxy_port)).await.is_err());
}

#[tokio::test]
async fn stopping_a_remote_forward_closes_its_connections() {
    let dir = Scratch::new();
    let (server_port, _, side) = ssh_server_with_side(1).await;
    let echo = echo_service().await;
    let ctx = context(&dir, test_host("target", server_port), vec![]);
    let (ssh, _jumps) = connected(&ctx).await.unwrap();
    let ssh = Arc::new(ssh);
    let cancel = CancellationToken::new();
    let forward = tokio::spawn(forward::listen(
        ctx.clone(),
        ssh.clone(),
        remote_spec(9000, echo),
        cancel.clone(),
    ));
    eventually("listening", || !side.listening.lock().is_empty()).await;
    let channel = side
        .handle()
        .channel_open_forwarded_tcpip("127.0.0.1", 9000, "10.0.0.1", 5000)
        .await
        .unwrap();
    let mut stream = channel.into_stream();
    stream.write_all(b"before").await.unwrap();
    let mut answer = [0; 6];
    stream.read_exact(&mut answer).await.unwrap();
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), forward)
        .await
        .unwrap()
        .unwrap();
    assert!(ctx.remote_forwards.lock().is_empty());
    let closed = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut answer))
        .await
        .expect("Stopped remote channel must close promptly");
    assert!(
        matches!(closed, Ok(0)) || closed.is_err(),
        "Stopped remote tunnel still carries traffic"
    );
    // Stopping the tunnel must not achieve this by closing the shared SSH session.
    let channel = ssh
        .channel_open_direct_tcpip("127.0.0.1", echo as u32, "127.0.0.1", 0)
        .await
        .unwrap();
    let mut other = channel.into_stream();
    other.write_all(b"alive!").await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), other.read_exact(&mut answer))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&answer, b"alive!");
    ctx.cancel.cancel();
}

#[tokio::test]
async fn a_jump_host_cannot_use_the_remote_forward_of_the_target() {
    let dir = Scratch::new();
    let (bastion_port, _, bastion_side) = ssh_server_with_side(1).await;
    let (target_port, _, target_side) = ssh_server_with_side(2).await;
    let echo = echo_service().await;
    let ctx = context(
        &dir,
        test_host("target", target_port),
        vec![test_host("bastion", bastion_port)],
    );
    let (ssh, _jumps) = connected(&ctx).await.unwrap();
    let ssh = Arc::new(ssh);
    let cancel = CancellationToken::new();
    let forward = tokio::spawn(forward::listen(
        ctx.clone(),
        ssh.clone(),
        remote_spec(9000, echo),
        cancel.clone(),
    ));
    eventually("target listening", || {
        !target_side.listening.lock().is_empty()
    })
    .await;
    let result = bastion_side
        .handle()
        .channel_open_forwarded_tcpip("127.0.0.1", 9000, "10.0.0.1", 5000)
        .await;
    let reached = if let Ok(channel) = result {
        let mut stream = channel.into_stream();
        stream.write_all(b"bastion").await.unwrap();
        let mut answer = [0; 7];
        let read =
            tokio::time::timeout(Duration::from_secs(2), stream.read_exact(&mut answer)).await;
        matches!(read, Ok(Ok(_))) && answer == *b"bastion"
    } else {
        false
    };
    cancel.cancel();
    forward.await.unwrap();
    ctx.cancel.cancel();
    assert!(
        !reached,
        "Jump host reached a local service through a forward authorized only for target"
    );
}
