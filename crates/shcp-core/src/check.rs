//! Tries a host's settings before they are saved. It goes through a real connection, as far as it
//! gets without a secret only the user has, and keeps nothing open: no terminal, no tunnel.
use crate::{
    session::{self, Context, Control},
    *,
};
use async_channel::{Receiver, Sender};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, atomic::Ordering},
};
use tokio::runtime::Handle;

#[derive(Debug, Clone)]
pub enum CheckUpdate {
    Step {
        step: ConnectStep,
        /// The jump host it is on. `None` on the host being checked.
        hop: Option<String>,
        /// What the step is working with: an address, a key.
        detail: Option<String>,
    },
    /// A server nobody trusted yet: it waits for `answer_host_key`.
    NewServer {
        host: String,
        key_type: String,
        fingerprint_sha256: String,
    },
    Finished(CheckOutcome),
}

#[derive(Debug, Clone)]
pub enum CheckOutcome {
    SignedIn,
    /// All is well up to a secret, which the connection will ask for.
    WillAsk(CheckQuestion),
    Failed(ConnectError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckQuestion {
    Passphrase(PathBuf),
    /// Who it is for, as the prompt says it.
    Password(String),
    Answer(String),
}

/// A check under way. Dropping it stops it.
pub struct ConnectionCheck {
    updates: Receiver<CheckUpdate>,
    context: Arc<Context>,
}

impl ConnectionCheck {
    pub fn updates(&self) -> Receiver<CheckUpdate> {
        self.updates.clone()
    }

    /// `accept` trusts the server from now on, as a connection does.
    pub fn answer_host_key(&self, accept: bool) {
        let prompts = &self.context.prompts;
        if prompts
            .kind
            .compare_exchange(1, 0, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            let _ = prompts.host_tx.try_send(accept);
        }
    }
}

impl Drop for ConnectionCheck {
    fn drop(&mut self) {
        self.context.cancel.cancel();
    }
}

/// The settings as they are tried: what opens something on arrival stays out of it.
pub(crate) fn draft(mut host: HostConfig, home: &Path) -> HostConfig {
    host.id = HostId(host.alias.clone());
    host.source = HostSource::App;
    host.local_forwards.clear();
    host.forward_agent = false;
    crate::config::ssh::expand_identity_files(&mut host, home);
    host
}

/// `context` must send its events to `events`, which keeps them away from the rest of the app:
/// this is no session anyone should see.
pub(crate) fn start(
    runtime: &Handle,
    context: Context,
    events: Receiver<Event>,
    concurrency: usize,
) -> ConnectionCheck {
    let (control, commands) = Control::new(context, concurrency);
    let context = control.context.clone();
    let (sender, updates) = async_channel::unbounded();
    runtime.spawn(session::run(control, commands));
    runtime.spawn(relay(events, context.clone(), sender));
    ConnectionCheck { updates, context }
}

async fn relay(events: Receiver<Event>, context: Arc<Context>, updates: Sender<CheckUpdate>) {
    // Why a key could not be used comes as a notice, and may be what explains a failure.
    let mut notice = None;
    while let Ok(event) = events.recv().await {
        let state = match event {
            Event::Session { state, .. } => state,
            Event::Notice { message, .. } => {
                notice = Some(message);
                continue;
            }
            _ => continue,
        };
        let Some(update) = translate(state, &mut notice) else {
            break;
        };
        let finished = matches!(update, CheckUpdate::Finished(_));
        let _ = updates.send(update).await;
        if finished {
            context.cancel.cancel();
            break;
        }
    }
}

/// `None` when the connection is gone without anything to say: it was stopped.
fn translate(state: SessionState, notice: &mut Option<String>) -> Option<CheckUpdate> {
    use CheckQuestion::{Answer, Passphrase, Password};
    let finished = |outcome| Some(CheckUpdate::Finished(outcome));
    match state {
        SessionState::Connecting { step, progress } => Some(CheckUpdate::Step {
            step,
            hop: progress
                .plan
                .get(progress.position as usize)
                .and_then(|planned| planned.hop.clone()),
            detail: progress.detail,
        }),
        SessionState::AwaitingHostKey {
            host,
            key_type,
            fingerprint_sha256,
            kind: HostKeyKind::Unknown,
        } => Some(CheckUpdate::NewServer {
            host,
            key_type,
            fingerprint_sha256,
        }),
        // A decision that deserves the whole prompt of a connection, not a line here.
        SessionState::AwaitingHostKey {
            host,
            kind: HostKeyKind::Changed,
            ..
        } => finished(CheckOutcome::Failed(ConnectError {
            kind: ConnectErrorKind::HostKey,
            message: format!("The key of {host} is not the one saved for it."),
            hint: Some(
                "The server may have been reinstalled, or someone may be in between. \
                 Connecting shows both keys."
                    .to_string(),
            ),
        })),
        SessionState::AwaitingPassphrase { key_path } => {
            finished(CheckOutcome::WillAsk(Passphrase(key_path)))
        }
        SessionState::AwaitingPassword { login } => {
            finished(CheckOutcome::WillAsk(Password(login)))
        }
        SessionState::AwaitingAnswer { prompt, .. } => {
            finished(CheckOutcome::WillAsk(Answer(prompt)))
        }
        SessionState::Connected => finished(CheckOutcome::SignedIn),
        SessionState::Failed { mut error } => {
            if error.hint.is_none() {
                error.hint = notice.take();
            }
            finished(CheckOutcome::Failed(error))
        }
        SessionState::Disconnected { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;
    use russh::{
        MethodKind, MethodSet,
        keys::{PrivateKey, ssh_key::private::Ed25519Keypair},
        server::{self, Auth},
    };
    use std::{collections::HashMap, time::Duration};
    use tokio::net::TcpListener;
    use tokio_util::sync::CancellationToken;

    #[derive(Clone, Default)]
    struct Seen(Arc<Mutex<Vec<String>>>);

    struct Server {
        /// Lets anyone in. Otherwise it takes nothing but a password.
        open: bool,
        seen: Seen,
    }
    impl server::Handler for Server {
        type Error = anyhow::Error;
        async fn auth_none(&mut self, _: &str) -> anyhow::Result<Auth> {
            Ok(if self.open {
                Auth::Accept
            } else {
                Auth::Reject {
                    proceed_with_methods: Some(MethodSet::from(&[MethodKind::Password][..])),
                    partial_success: false,
                }
            })
        }
        async fn auth_password(&mut self, _: &str, password: &str) -> anyhow::Result<Auth> {
            self.seen.0.lock().push(password.to_string());
            Ok(Auth::Reject {
                proceed_with_methods: None,
                partial_success: false,
            })
        }
    }

    async fn server(open: bool, seed: u8) -> (u16, Seen) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let config = Arc::new(server::Config {
            keys: vec![PrivateKey::from(Ed25519Keypair::from_seed(&[seed; 32]))],
            ..Default::default()
        });
        let seen = Seen::default();
        let shared = seen.clone();
        tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                let (config, seen) = (config.clone(), shared.clone());
                tokio::spawn(async move {
                    if let Ok(session) =
                        server::run_stream(config, socket, Server { open, seen }).await
                    {
                        let _ = session.await;
                    }
                });
            }
        });
        (port, seen)
    }

    struct Scratch(PathBuf);
    impl Scratch {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("shcp-check-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        fn known_hosts(&self) -> PathBuf {
            self.0.join("known_hosts")
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn check(dir: &Scratch, port: u16) -> ConnectionCheck {
        let host = HostConfig {
            alias: "draft".into(),
            hostname: "127.0.0.1".into(),
            port,
            user: "tester".into(),
            ..Default::default()
        };
        let (events, receiver) = async_channel::unbounded();
        let context = Context {
            id: SessionId(1),
            host: draft(host, &dir.0),
            jumps: Vec::new(),
            remote_forwards: Mutex::new(HashMap::new()),
            paths: AppPaths {
                config_dir: dir.0.clone(),
                data_dir: dir.0.clone(),
                log_dir: dir.0.clone(),
                known_hosts: dir.known_hosts(),
                ssh_config: dir.0.join("config"),
            },
            events,
            cancel: CancellationToken::new(),
            prompts: session::Prompts::new(),
            known_hosts_lock: Arc::new(Mutex::new(())),
            use_agent: false,
            open_forwards: false,
            persist_terminals: false,
            tmux: Default::default(),
        };
        start(&Handle::current(), context, receiver, 1)
    }

    /// What it reports until it finishes, trusting a new server if told to.
    async fn outcome(check: &ConnectionCheck, trust: bool) -> (Vec<CheckUpdate>, CheckOutcome) {
        let updates = check.updates();
        let mut seen = Vec::new();
        loop {
            let update = tokio::time::timeout(Duration::from_secs(20), updates.recv())
                .await
                .expect("the check hung")
                .expect("the check stopped without saying how it went");
            match &update {
                CheckUpdate::Finished(outcome) => return (seen, outcome.clone()),
                CheckUpdate::NewServer { .. } if trust => check.answer_host_key(true),
                _ => {}
            }
            seen.push(update);
        }
    }

    #[tokio::test]
    async fn a_server_that_lets_you_in_signs_in_and_is_trusted() {
        let dir = Scratch::new("signs-in");
        let (port, _) = server(true, 1).await;
        let check = check(&dir, port);
        let (seen, outcome) = outcome(&check, true).await;
        assert!(matches!(outcome, CheckOutcome::SignedIn));
        assert!(
            seen.iter()
                .any(|update| matches!(update, CheckUpdate::NewServer { .. }))
        );
        let saved = std::fs::read_to_string(dir.known_hosts()).unwrap();
        assert!(saved.contains(&port.to_string()));
    }

    #[tokio::test]
    async fn a_password_is_never_asked_for_or_sent() {
        let dir = Scratch::new("password");
        let (port, seen) = server(false, 2).await;
        let check = check(&dir, port);
        let (_, outcome) = outcome(&check, true).await;
        assert!(matches!(
            outcome,
            CheckOutcome::WillAsk(CheckQuestion::Password(ref login)) if login == "tester"
        ));
        assert!(seen.0.lock().is_empty());
    }

    #[tokio::test]
    async fn nothing_listening_is_a_failure_with_a_reason() {
        let dir = Scratch::new("closed");
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let check = check(&dir, port);
        let (_, outcome) = outcome(&check, false).await;
        let CheckOutcome::Failed(error) = outcome else {
            panic!("it should have failed: {outcome:?}");
        };
        assert_eq!(error.kind, ConnectErrorKind::Transport);
        assert!(!error.message.is_empty());
    }

    #[tokio::test]
    async fn stopping_at_a_new_server_trusts_nothing() {
        let dir = Scratch::new("stopped");
        let (port, _) = server(true, 3).await;
        let check = check(&dir, port);
        let updates = check.updates();
        loop {
            let update = tokio::time::timeout(Duration::from_secs(20), updates.recv())
                .await
                .unwrap()
                .unwrap();
            if matches!(update, CheckUpdate::NewServer { .. }) {
                break;
            }
        }
        drop(check);
        // The stop goes through the session, which then says no more.
        let ended = tokio::time::timeout(Duration::from_secs(20), async {
            while updates.recv().await.is_ok() {}
        })
        .await;
        assert!(ended.is_ok(), "the check kept going");
        assert!(!dir.known_hosts().exists());
    }

    #[tokio::test]
    async fn a_changed_key_fails_and_stays_as_it_was() {
        let dir = Scratch::new("changed");
        let (port, _) = server(true, 4).await;
        let old = PrivateKey::from(Ed25519Keypair::from_seed(&[99; 32]));
        let saved = format!(
            "[127.0.0.1]:{port} {}\n",
            old.public_key().to_openssh().unwrap()
        );
        std::fs::write(dir.known_hosts(), &saved).unwrap();
        let check = check(&dir, port);
        let (_, outcome) = outcome(&check, true).await;
        let CheckOutcome::Failed(error) = outcome else {
            panic!("it should have failed: {outcome:?}");
        };
        assert_eq!(error.kind, ConnectErrorKind::HostKey);
        assert_eq!(std::fs::read_to_string(dir.known_hosts()).unwrap(), saved);
    }

    #[test]
    fn a_draft_opens_nothing_on_arrival() {
        let host = HostConfig {
            alias: "draft".into(),
            forward_agent: true,
            local_forwards: vec![ForwardSpec::socks("127.0.0.1".into(), 1080)],
            identity_files: vec![PathBuf::from("~/.ssh/key")],
            ..Default::default()
        };
        let host = draft(host, Path::new("/home/me"));
        assert!(!host.forward_agent);
        assert!(host.local_forwards.is_empty());
        assert_eq!(host.id, HostId("draft".into()));
        assert_eq!(
            host.identity_files,
            [Path::new("/home/me").join(".ssh/key")]
        );
    }
}
