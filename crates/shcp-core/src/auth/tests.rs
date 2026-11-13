//! Logs in to a real SSH server in memory, whose methods each test decides.
use super::*;
use crate::{HostId, SessionId, test_support::Scratch};
use parking_lot::Mutex;
use russh::{
    keys::ssh_key::private::Ed25519Keypair,
    server::{self, Auth, PublicKeyOffer, Response, VerifiedPublicKey},
};
use std::{borrow::Cow, time::Duration};

/// What the test server accepts.
#[derive(Clone, Default)]
struct Rules {
    /// Methods it says it accepts at first.
    methods: Vec<MethodKind>,
    /// This password logs in.
    password: Option<&'static str>,
    /// Any public key is taken, but something else must follow.
    key_then: Option<MethodKind>,
    /// Questions asked and the answers that log in.
    questions: Vec<(&'static str, bool, &'static str)>,
}
#[derive(Default)]
struct Seen {
    passwords: Vec<String>,
    keys: usize,
}
struct Server {
    rules: Rules,
    seen: Arc<Mutex<Seen>>,
    key_done: bool,
}
impl Server {
    fn reject(&self) -> Auth {
        Auth::Reject {
            proceed_with_methods: Some(MethodSet::from(&self.remaining()[..])),
            partial_success: false,
        }
    }
    fn remaining(&self) -> Vec<MethodKind> {
        match (self.key_done, self.rules.key_then) {
            (true, Some(next)) => vec![next],
            _ => self.rules.methods.clone(),
        }
    }
    fn allowed(&self, kind: MethodKind) -> bool {
        self.remaining().contains(&kind)
    }
}
impl server::Handler for Server {
    type Error = anyhow::Error;
    async fn auth_none(&mut self, _: &str) -> Result<Auth> {
        Ok(self.reject())
    }
    async fn auth_password(&mut self, _: &str, password: &str) -> Result<Auth> {
        self.seen.lock().passwords.push(password.to_string());
        if self.allowed(MethodKind::Password) && self.rules.password == Some(password) {
            return Ok(Auth::Accept);
        }
        Ok(self.reject())
    }
    async fn auth_publickey_offered(&mut self, _: &str, _: PublicKeyOffer<'_>) -> Result<Auth> {
        if self.allowed(MethodKind::PublicKey) {
            Ok(Auth::Accept)
        } else {
            Ok(self.reject())
        }
    }
    async fn auth_publickey(&mut self, _: &str, _: VerifiedPublicKey<'_>) -> Result<Auth> {
        self.seen.lock().keys += 1;
        if self.rules.key_then.is_some() {
            self.key_done = true;
            return Ok(Auth::Reject {
                proceed_with_methods: Some(MethodSet::from(&self.remaining()[..])),
                partial_success: true,
            });
        }
        if self.allowed(MethodKind::PublicKey) {
            return Ok(Auth::Accept);
        }
        Ok(self.reject())
    }
    async fn auth_keyboard_interactive<'a>(
        &'a mut self,
        _: &str,
        _: &str,
        response: Option<Response<'a>>,
    ) -> Result<Auth> {
        if !self.allowed(MethodKind::KeyboardInteractive) {
            return Ok(self.reject());
        }
        let Some(response) = response else {
            let prompts: Vec<(Cow<'static, str>, bool)> = self
                .rules
                .questions
                .iter()
                .map(|(prompt, echo, _)| (Cow::Borrowed(*prompt), *echo))
                .collect();
            return Ok(Auth::Partial {
                name: Cow::Borrowed(""),
                instructions: Cow::Borrowed("Answer these"),
                prompts: Cow::Owned(prompts),
            });
        };
        let answers: Vec<String> = response
            .map(|answer| String::from_utf8_lossy(&answer).into_owned())
            .collect();
        self.seen.lock().passwords.extend(answers.clone());
        let right = answers.len() == self.rules.questions.len()
            && answers
                .iter()
                .zip(&self.rules.questions)
                .all(|(answer, (_, _, expected))| answer == expected);
        Ok(if right { Auth::Accept } else { self.reject() })
    }
}

/// What the test user answers, in order. `None` presses Esc.
type Script = Vec<Option<&'static str>>;

struct Run {
    result: Result<()>,
    seen: Arc<Mutex<Seen>>,
    /// What the user was asked, in order.
    asked: Vec<SessionState>,
}

fn test_key(seed: u8) -> PrivateKey {
    PrivateKey::from(Ed25519Keypair::from_seed(&[seed; 32]))
}

async fn login(name: &str, rules: Rules, script: Script, with_key: bool) -> Run {
    let dir = Scratch::new(&format!("auth-{name}"));
    let identity = dir.0.join("id");
    let identity_files = if with_key {
        let text = test_key(1)
            .to_openssh(russh::keys::ssh_key::LineEnding::LF)
            .unwrap();
        std::fs::write(&identity, text.as_bytes()).unwrap();
        vec![identity]
    } else {
        // A file that is not a key: the default keys of whoever runs the tests must stay out.
        std::fs::write(&identity, "not a key").unwrap();
        vec![identity]
    };
    let host = HostConfig {
        id: HostId(name.into()),
        alias: name.into(),
        // Nothing saved in the Credential Manager goes by this name.
        hostname: format!("shcp-auth-test-{name}.invalid"),
        user: "tester".into(),
        identity_files,
        ..Default::default()
    };
    let (events, receiver) = async_channel::unbounded();
    let context = Arc::new(SessionContext::new(
        SessionId(1),
        host.clone(),
        vec![],
        dir.paths(),
        events,
        Arc::new(Mutex::new(())),
    ));
    let seen = Arc::new(Mutex::new(Seen::default()));
    let config = Arc::new(server::Config {
        keys: vec![test_key(2)],
        auth_rejection_time: Duration::ZERO,
        auth_rejection_time_initial: Some(Duration::ZERO),
        ..Default::default()
    });
    let (client_side, server_side) = tokio::io::duplex(1 << 16);
    let handler = Server {
        rules,
        seen: seen.clone(),
        key_done: false,
    };
    let server = tokio::spawn(async move {
        if let Ok(session) = server::run_stream(config, server_side, handler).await {
            let _ = session.await;
        }
    });
    // Plays the user: accepts the host key and answers what is asked.
    let user = {
        let context = context.clone();
        tokio::spawn(async move {
            let mut asked = Vec::new();
            let mut script = script.into_iter();
            while let Ok(event) = receiver.recv().await {
                let Event::Session { state, .. } = event else {
                    continue;
                };
                match &state {
                    SessionState::AwaitingHostKey { .. } => {
                        context.prompts.host_tx.send(true).await.unwrap();
                    }
                    SessionState::AwaitingPassword { .. } | SessionState::AwaitingAnswer { .. } => {
                        asked.push(state.clone());
                        let Some(answer) = script.next() else {
                            context.cancel.cancel();
                            continue;
                        };
                        let answer = answer.map(|secret| SecretAnswer {
                            secret: Zeroizing::new(secret.to_string()),
                            remember: false,
                        });
                        context.prompts.password_tx.send(answer).await.unwrap();
                    }
                    _ => {}
                }
            }
            asked
        })
    };
    let handler = Handler {
        context: context.clone(),
        host: host.clone(),
        is_target: true,
    };
    let result = tokio::time::timeout(Duration::from_secs(20), async {
        let mut handle =
            client::connect_stream(Arc::new(client::Config::default()), client_side, handler)
                .await?;
        authenticate(&mut handle, &context, &host).await
    })
    .await
    .expect("the login hung");
    server.abort();
    // The client's task may keep its handler for a while, and with it the sender.
    context.events.close();
    let asked = user.await.unwrap();
    Run {
        result,
        seen,
        asked,
    }
}

#[tokio::test]
async fn a_key_only_server_never_asks_for_a_password() {
    let rules = Rules {
        methods: vec![MethodKind::PublicKey],
        ..Default::default()
    };
    // The key is taken, so this one logs in without a word.
    let run = login("key-ok", rules.clone(), vec![], true).await;
    run.result.unwrap();
    assert!(run.asked.is_empty());

    // No usable key: it says why, instead of asking for a password the server would refuse.
    let run = login("key-none", rules, vec![], false).await;
    let error = format!("{:#}", run.result.unwrap_err());
    assert!(error.contains("accepts only publickey"), "{error}");
    assert!(error.contains("authentication"), "{error}");
    assert!(run.asked.is_empty());
    assert!(run.seen.lock().passwords.is_empty());
}

#[tokio::test]
async fn a_wrong_password_is_asked_again() {
    let rules = Rules {
        methods: vec![MethodKind::Password],
        password: Some("right"),
        ..Default::default()
    };
    let run = login("password", rules, vec![Some("wrong"), Some("right")], false).await;
    run.result.unwrap();
    assert_eq!(run.asked.len(), 2);
    assert!(matches!(
        &run.asked[0],
        SessionState::AwaitingPassword { login } if login == "tester"
    ));
    assert_eq!(run.seen.lock().passwords, ["wrong", "right"]);
}

#[tokio::test]
async fn pressing_esc_on_a_password_stops_the_login() {
    let rules = Rules {
        methods: vec![MethodKind::Password],
        password: Some("right"),
        ..Default::default()
    };
    let run = login("esc", rules, vec![None], false).await;
    assert!(format!("{:#}", run.result.unwrap_err()).contains("cancelled"));
}

#[tokio::test]
async fn keyboard_interactive_shows_each_question() {
    let rules = Rules {
        methods: vec![MethodKind::KeyboardInteractive],
        questions: vec![("Password: ", false, "secret"), ("Your name? ", true, "me")],
        ..Default::default()
    };
    let run = login("keyboard", rules, vec![Some("secret"), Some("me")], false).await;
    run.result.unwrap();
    assert_eq!(run.asked.len(), 2);
    match &run.asked[0] {
        SessionState::AwaitingAnswer {
            prompt,
            instructions,
            echo,
            can_remember,
        } => {
            assert_eq!(prompt, "Password: ");
            assert_eq!(instructions, "Answer these");
            assert!(!echo);
            assert!(can_remember);
        }
        other => panic!("asked {other:?}"),
    }
    match &run.asked[1] {
        SessionState::AwaitingAnswer {
            echo, can_remember, ..
        } => {
            assert!(echo);
            assert!(!can_remember);
        }
        other => panic!("asked {other:?}"),
    }
}

#[tokio::test]
async fn keyboard_interactive_tries_again_after_a_wrong_answer() {
    let rules = Rules {
        methods: vec![MethodKind::KeyboardInteractive],
        questions: vec![("Verification code: ", false, "123456")],
        ..Default::default()
    };
    let run = login(
        "keyboard-retry",
        rules,
        vec![Some("000000"), Some("123456")],
        false,
    )
    .await;
    run.result.unwrap();
    assert_eq!(run.asked.len(), 2);
}

#[tokio::test]
async fn a_key_can_be_followed_by_a_code() {
    let rules = Rules {
        methods: vec![MethodKind::PublicKey],
        key_then: Some(MethodKind::KeyboardInteractive),
        questions: vec![("Verification code: ", false, "123456")],
        ..Default::default()
    };
    let run = login("two-factors", rules, vec![Some("123456")], true).await;
    run.result.unwrap();
    assert_eq!(run.seen.lock().keys, 1);
    assert_eq!(run.asked.len(), 1);
}

#[tokio::test]
async fn a_key_can_be_followed_by_a_password() {
    let rules = Rules {
        methods: vec![MethodKind::PublicKey],
        key_then: Some(MethodKind::Password),
        password: Some("right"),
        ..Default::default()
    };
    let run = login("key-password", rules, vec![Some("right")], true).await;
    run.result.unwrap();
    assert!(matches!(
        &run.asked[0],
        SessionState::AwaitingPassword { .. }
    ));
}
