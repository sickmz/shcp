mod agent;
mod pem;
use crate::{
    Event, HostConfig, NoticeLevel, SessionState, config,
    host_key::Handler,
    secrets,
    session::{Context as SessionContext, SecretAnswer},
};
use anyhow::{Context, Result, bail};
use russh::{
    MethodKind, MethodSet,
    client::{self, AuthResult, KeyboardInteractiveAuthResponse},
    keys::{self, PrivateKey, PrivateKeyWithHashAlg},
};
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use zeroize::Zeroizing;

async fn load(
    path: &Path,
    passphrase: Option<Zeroizing<String>>,
) -> Result<PrivateKey, keys::Error> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || {
        let password = passphrase.as_deref().map(|s| s.as_str());
        match keys::load_secret_key(&path, password) {
            Ok(key) => Ok(key),
            Err(error) => {
                let text = Zeroizing::new(std::fs::read_to_string(&path)?);
                if pem::encrypted(&text) {
                    pem::load(&text, password)
                } else {
                    Err(error)
                }
            }
        }
    })
    .await
    .map_err(|_| keys::Error::CouldNotReadKey)?
}
async fn saved_secret(account: String) -> Option<Zeroizing<String>> {
    tokio::task::spawn_blocking(move || secrets::load(&account))
        .await
        .ok()
        .flatten()
}
async fn forget_secret(account: String) {
    let _ = tokio::task::spawn_blocking(move || secrets::forget(&account)).await;
}
/// A secret that cannot be saved must not stop the connection: it is already known good.
/// Returns whether it was saved.
async fn save_secret(account: String, secret: Zeroizing<String>, ctx: &SessionContext) -> bool {
    let saved = tokio::task::spawn_blocking(move || secrets::save(&account, secret.as_str())).await;
    let error = match saved {
        Ok(Ok(())) => return true,
        Ok(Err(error)) => format!("{error:#}"),
        Err(error) => error.to_string(),
    };
    ctx.notice(
        NoticeLevel::Warning,
        format!("Could not remember it in the Credential Manager: {error}"),
    );
    false
}
pub(crate) use agent::stream as agent_stream;

/// What an attempt came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Outcome {
    Success,
    /// Accepted, but the server wants another method as well.
    Partial,
    Rejected,
}
/// What the server still lets the client try. It says so after every attempt.
struct Methods(MethodSet);
impl Methods {
    fn allows(&self, kind: MethodKind) -> bool {
        // An empty list says nothing: only a server that broke the protocol sends one.
        self.0.is_empty() || self.0.contains(&kind)
    }
    fn names(&self) -> String {
        self.0
            .iter()
            .map(<&str>::from)
            .collect::<Vec<_>>()
            .join(", ")
    }
    fn note(&mut self, result: AuthResult) -> Outcome {
        match result {
            AuthResult::Success => Outcome::Success,
            AuthResult::Failure {
                remaining_methods,
                partial_success,
            } => {
                self.0 = remaining_methods;
                if partial_success {
                    Outcome::Partial
                } else {
                    Outcome::Rejected
                }
            }
        }
    }
}
/// Asks the user something that is answered through the password channel.
async fn ask(ctx: &SessionContext, state: SessionState) -> Result<Option<SecretAnswer>> {
    ctx.prompts.kind.store(3, Ordering::SeqCst);
    ctx.state(state);
    let answer = tokio::select! {
        _ = ctx.cancel.cancelled() => bail!("Connection cancelled"),
        answer = ctx.prompts.password_rx.recv() => answer?,
    };
    ctx.prompts.kind.store(0, Ordering::SeqCst);
    Ok(answer)
}
/// Who a password is for, as the prompt says it. The user alone when it is the host asked for.
fn login_name(ctx: &SessionContext, host: &HostConfig) -> String {
    if host.id == ctx.host.id {
        host.user.clone()
    } else {
        format!("{}@{}", host.user, host.alias)
    }
}
pub(crate) async fn authenticate(
    handle: &mut client::Handle<Handler>,
    ctx: &Arc<SessionContext>,
    host: &HostConfig,
) -> Result<()> {
    ctx.step(host, crate::ConnectStep::Authenticating, None);
    // Never accepted by a real server, but the refusal lists the methods it does accept.
    let mut methods = Methods(MethodSet::empty());
    if methods.note(handle.authenticate_none(&host.user).await?) == Outcome::Success {
        return Ok(());
    }
    // A key taken halfway: what follows is a second factor, which no key would spare.
    let mut key_taken = false;
    if methods.allows(MethodKind::PublicKey) {
        match public_key(handle, ctx, host, &mut methods).await? {
            Outcome::Success => return Ok(()),
            Outcome::Partial => key_taken = true,
            Outcome::Rejected => {}
        }
    }
    let target = format!("{}@{}:{}", host.user, host.hostname, host.port);
    let account = secrets::password_account(&target);
    let mut saved = saved_secret(account.clone()).await;
    let logins = Logins {
        ctx,
        host,
        account,
        target,
        used_password: AtomicBool::new(false),
    };
    loop {
        let outcome = if methods.allows(MethodKind::Password) {
            logins.password(handle, &mut methods, &mut saved).await?
        } else if methods.allows(MethodKind::KeyboardInteractive) {
            logins
                .keyboard_interactive(handle, &mut methods, &mut saved)
                .await?
        } else {
            bail!(
                "SSH authentication failed: {} accepts only {}, and it took none of the keys \
                 available here",
                host.alias,
                methods.names()
            );
        };
        if outcome == Outcome::Success {
            if !key_taken && host.id == ctx.host.id && logins.used_password.load(Ordering::SeqCst) {
                ctx.emit(Event::PasswordLogin {
                    host: host.id.clone(),
                });
            }
            return Ok(());
        }
    }
}
/// The agent's keys, then the identity files. Stops at the first key the server takes.
async fn public_key(
    handle: &mut client::Handle<Handler>,
    ctx: &Arc<SessionContext>,
    host: &HostConfig,
    methods: &mut Methods,
) -> Result<Outcome> {
    let mut paths = host.identity_files.clone();
    if paths.is_empty() {
        let ssh = config::home()?.join(".ssh");
        for name in ["id_ed25519", "id_ecdsa", "id_rsa"] {
            let path = ssh.join(name);
            if path.exists() {
                paths.push(path);
            }
        }
    }
    // Like OpenSSH: keys held by the agent (Windows OpenSSH agent, Pageant) come first, so an
    // encrypted identity file already unlocked in the agent never asks for its passphrase.
    if ctx.use_agent {
        ctx.step(
            host,
            crate::ConnectStep::Authenticating,
            Some("SSH agent".to_string()),
        );
        let outcome = agent::authenticate(handle, &host.user, &paths, &mut |result| {
            methods.note(result)
        })
        .await;
        if outcome != Outcome::Rejected {
            return Ok(outcome);
        }
    }
    for path in paths {
        if !methods.allows(MethodKind::PublicKey) {
            break;
        }
        let mut key = load(&path, None).await;
        // PEM and OpenSSH formats report encryption through either error variant.
        let pem_encrypted = key.is_err()
            && tokio::fs::read_to_string(&path)
                .await
                .is_ok_and(|text| pem::encrypted(&text));
        let encrypted = pem_encrypted
            || matches!(
                &key,
                Err(keys::Error::KeyIsEncrypted)
                    | Err(keys::Error::SshKey(keys::ssh_key::Error::Encrypted))
            );
        let account = secrets::passphrase_account(&path);
        if encrypted && let Some(saved) = saved_secret(account.clone()).await {
            key = load(&path, Some(saved)).await;
            if key.is_err() {
                // The key's passphrase changed: the old one is of no use any more.
                forget_secret(account.clone()).await;
                ctx.emit(Event::PassphraseSaved {
                    key: path.clone(),
                    saved: false,
                });
                ctx.notice(
                    NoticeLevel::Warning,
                    format!(
                        "The saved passphrase of {} no longer works. Enter it again.",
                        path.display()
                    ),
                );
            }
        }
        if encrypted && key.is_err() {
            loop {
                ctx.prompts.kind.store(2, Ordering::SeqCst);
                ctx.state(SessionState::AwaitingPassphrase {
                    key_path: path.clone(),
                });
                let answer = tokio::select! { _ = ctx.cancel.cancelled() => bail!("Connection cancelled"), answer = ctx.prompts.pass_rx.recv() => answer? };
                ctx.prompts.kind.store(0, Ordering::SeqCst);
                let Some(answer) = answer else {
                    break;
                };
                let remembered = answer.remember.then(|| answer.secret.clone());
                key = load(&path, Some(answer.secret)).await;
                if key.is_ok() {
                    if let Some(passphrase) = remembered
                        && save_secret(account.clone(), passphrase, ctx).await
                    {
                        ctx.emit(Event::PassphraseSaved {
                            key: path.clone(),
                            saved: true,
                        });
                    }
                    break;
                }
                // Asking again is what tells the user: the prompt says the answer was wrong.
            }
        }
        let key = match key {
            Ok(key) => key,
            Err(e) => {
                ctx.notice(
                    NoticeLevel::Warning,
                    format!("Could not load {}: {e}", path.display()),
                );
                continue;
            }
        };
        let name = path.file_name().unwrap_or(path.as_os_str()).display();
        ctx.step(
            host,
            crate::ConnectStep::Authenticating,
            Some(format!("key {name}")),
        );
        let hash = if key.algorithm().is_rsa() {
            handle
                .best_supported_rsa_hash()
                .await?
                .flatten()
                .or(Some(keys::HashAlg::Sha512))
        } else {
            None
        };
        let key = PrivateKeyWithHashAlg::new(Arc::new(key), hash);
        let outcome = methods.note(handle.authenticate_publickey(&host.user, key).await?);
        if outcome != Outcome::Rejected {
            return Ok(outcome);
        }
    }
    Ok(Outcome::Rejected)
}
/// The methods that ask the user, and the password they may share.
struct Logins<'a> {
    ctx: &'a Arc<SessionContext>,
    host: &'a HostConfig,
    account: String,
    target: String,
    /// A password went to the server, typed or saved.
    used_password: AtomicBool,
}
impl Logins<'_> {
    /// The saved password was refused: trying it again could lock the account.
    async fn forget_password(&self, saved: &mut Option<Zeroizing<String>>) {
        if saved.take().is_none() {
            return;
        }
        forget_secret(self.account.clone()).await;
        self.ctx.emit(Event::PasswordSaved {
            target: self.target.clone(),
            saved: false,
        });
        self.ctx.notice(
            NoticeLevel::Warning,
            format!(
                "The saved password of {} no longer works. Enter it again.",
                self.target
            ),
        );
    }
    async fn remember_password(&self, secret: Zeroizing<String>) {
        if save_secret(self.account.clone(), secret, self.ctx).await {
            self.ctx.emit(Event::PasswordSaved {
                target: self.target.clone(),
                saved: true,
            });
        }
    }
    /// Asks until the password is taken or the server stops allowing passwords.
    async fn password(
        &self,
        handle: &mut client::Handle<Handler>,
        methods: &mut Methods,
        saved: &mut Option<Zeroizing<String>>,
    ) -> Result<Outcome> {
        while methods.allows(MethodKind::Password) {
            let answer = match saved.clone() {
                Some(secret) => SecretAnswer {
                    secret,
                    remember: false,
                },
                None => {
                    let state = SessionState::AwaitingPassword {
                        login: login_name(self.ctx, self.host),
                    };
                    ask(self.ctx, state)
                        .await?
                        .context("Password authentication cancelled")?
                }
            };
            self.used_password.store(true, Ordering::SeqCst);
            self.ctx.step(
                self.host,
                crate::ConnectStep::Authenticating,
                Some("password".to_string()),
            );
            let result = handle
                .authenticate_password(&self.host.user, answer.secret.as_str())
                .await?;
            let outcome = methods.note(result);
            if outcome != Outcome::Rejected {
                if answer.remember {
                    self.remember_password(answer.secret).await;
                }
                return Ok(outcome);
            }
            // Asking again is what tells the user: the prompt says the answer was wrong.
            self.forget_password(saved).await;
        }
        Ok(Outcome::Rejected)
    }
    /// One run of the server's questions. A password among them can come from the store.
    async fn keyboard_interactive(
        &self,
        handle: &mut client::Handle<Handler>,
        methods: &mut Methods,
        saved: &mut Option<Zeroizing<String>>,
    ) -> Result<Outcome> {
        let mut used_saved = false;
        let mut remember = None;
        let mut reply = handle
            .authenticate_keyboard_interactive_start(self.host.user.as_str(), None)
            .await?;
        loop {
            match reply {
                KeyboardInteractiveAuthResponse::Success => {
                    if let Some(secret) = remember {
                        self.remember_password(secret).await;
                    }
                    return Ok(Outcome::Success);
                }
                KeyboardInteractiveAuthResponse::Failure {
                    remaining_methods,
                    partial_success,
                } => {
                    methods.0 = remaining_methods;
                    if partial_success {
                        if let Some(secret) = remember {
                            self.remember_password(secret).await;
                        }
                        return Ok(Outcome::Partial);
                    }
                    if used_saved {
                        self.forget_password(saved).await;
                    }
                    return Ok(Outcome::Rejected);
                }
                KeyboardInteractiveAuthResponse::InfoRequest {
                    instructions,
                    prompts,
                    ..
                } => {
                    let mut responses = Vec::with_capacity(prompts.len());
                    for (index, prompt) in prompts.iter().enumerate() {
                        let is_password =
                            !prompt.echo && prompt.prompt.to_lowercase().contains("password");
                        if is_password {
                            self.used_password.store(true, Ordering::SeqCst);
                        }
                        if is_password && let Some(secret) = saved.as_ref() {
                            used_saved = true;
                            responses.push(secret.to_string());
                            continue;
                        }
                        let state = SessionState::AwaitingAnswer {
                            prompt: prompt.prompt.clone(),
                            instructions: if index == 0 {
                                instructions.clone()
                            } else {
                                String::new()
                            },
                            echo: prompt.echo,
                            can_remember: is_password,
                        };
                        let answer = ask(self.ctx, state)
                            .await?
                            .context("Keyboard-interactive authentication cancelled")?;
                        if answer.remember && is_password {
                            remember = Some(answer.secret.clone());
                        }
                        responses.push(answer.secret.to_string());
                    }
                    self.ctx.step(
                        self.host,
                        crate::ConnectStep::Authenticating,
                        Some("keyboard-interactive".to_string()),
                    );
                    reply = handle
                        .authenticate_keyboard_interactive_respond(responses)
                        .await?;
                }
            }
        }
    }
}
#[cfg(test)]
mod tests;
