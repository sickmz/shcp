//! Public-key authentication through a running SSH agent.
//! Windows: the OpenSSH agent service (`\\.\pipe\openssh-ssh-agent`, or `SSH_AUTH_SOCK`),
//! then Pageant. Unix: `SSH_AUTH_SOCK`.
use super::Outcome;
use crate::host_key::Handler;
use russh::{
    client::{self, AuthResult},
    keys::{self, PublicKey, agent::AgentIdentity, agent::client::AgentClient},
};
use std::path::{Path, PathBuf};

type Agent = AgentClient<Box<dyn keys::agent::client::AgentStream + Send + Unpin>>;

/// Most servers allow 6 attempts (`MaxAuthTries`); leave room for the identity files.
const MAX_AGENT_KEYS: usize = 4;

#[cfg(windows)]
async fn connect() -> Option<Agent> {
    let mut pipes = Vec::new();
    if let Ok(sock) = std::env::var("SSH_AUTH_SOCK")
        && sock.starts_with(r"\\")
    {
        pipes.push(sock);
    }
    pipes.push(r"\\.\pipe\openssh-ssh-agent".to_owned());
    for pipe in pipes {
        let attempt = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            AgentClient::connect_named_pipe(&pipe),
        )
        .await;
        match attempt {
            Ok(Ok(agent)) => {
                tracing::debug!("using ssh-agent at {pipe}");
                return Some(agent.dynamic());
            }
            Ok(Err(error)) => tracing::debug!("ssh-agent {pipe} unavailable: {error}"),
            Err(_) => tracing::debug!("ssh-agent {pipe} timed out"),
        }
    }
    match AgentClient::connect_pageant().await {
        Ok(agent) => {
            tracing::debug!("using Pageant");
            Some(agent.dynamic())
        }
        Err(error) => {
            tracing::debug!("Pageant unavailable: {error}");
            None
        }
    }
}

#[cfg(not(windows))]
async fn connect() -> Option<Agent> {
    match AgentClient::connect_env().await {
        Ok(agent) => Some(agent.dynamic()),
        Err(error) => {
            tracing::debug!("ssh-agent unavailable: {error}");
            None
        }
    }
}

/// A connection to the running agent, to hand over to a server that was allowed to use it.
pub(crate) async fn stream() -> Option<Box<dyn keys::agent::client::AgentStream + Send + Unpin>> {
    Some(connect().await?.into_inner())
}

/// Public key of an identity file: `<file>.pub`, else the (possibly encrypted) OpenSSH file,
/// whose public half is stored in clear.
fn identity_public_key(path: &Path) -> Option<PublicKey> {
    let mut pub_path = path.as_os_str().to_owned();
    pub_path.push(".pub");
    if let Ok(text) = std::fs::read_to_string(PathBuf::from(pub_path))
        && let Ok(key) = PublicKey::from_openssh(text.trim())
    {
        return Some(key);
    }
    let text = std::fs::read_to_string(path).ok()?;
    keys::ssh_key::PrivateKey::from_openssh(text.trim())
        .ok()
        .map(|key| key.public_key().clone())
}

/// Tries the agent's keys, those matching `identity_files` first, until the server takes one.
/// `note` hears the server's answer to each.
pub(super) async fn authenticate(
    handle: &mut client::Handle<Handler>,
    user: &str,
    identity_files: &[PathBuf],
    note: &mut impl FnMut(AuthResult) -> Outcome,
) -> Outcome {
    let Some(mut agent) = connect().await else {
        return Outcome::Rejected;
    };
    let identities = match agent.request_identities().await {
        Ok(identities) => identities,
        Err(error) => {
            tracing::warn!("ssh-agent: could not list identities: {error}");
            return Outcome::Rejected;
        }
    };
    if identities.is_empty() {
        tracing::debug!("ssh-agent holds no keys");
        return Outcome::Rejected;
    }
    let wanted: Vec<PublicKey> = identity_files
        .iter()
        .filter_map(|path| identity_public_key(path))
        .collect();
    let matches = |identity: &AgentIdentity| {
        let key = identity.public_key();
        wanted.iter().any(|w| w.key_data() == key.key_data())
    };
    let (mut ordered, rest): (Vec<_>, Vec<_>) = identities.into_iter().partition(|i| matches(i));
    ordered.extend(rest);
    ordered.truncate(MAX_AGENT_KEYS.max(wanted.len()));

    for identity in ordered {
        let key = identity.public_key().into_owned();
        let hash = if key.algorithm().is_rsa() {
            handle
                .best_supported_rsa_hash()
                .await
                .ok()
                .flatten()
                .flatten()
                .or(Some(keys::HashAlg::Sha512))
        } else {
            None
        };
        let comment = identity.comment().to_owned();
        match handle
            .authenticate_publickey_with(user, key, hash, &mut agent)
            .await
        {
            Ok(result) => match note(result) {
                Outcome::Rejected => tracing::debug!("ssh-agent key {comment} rejected"),
                outcome => {
                    tracing::info!("authenticated with ssh-agent key {comment}");
                    return outcome;
                }
            },
            Err(error) => {
                tracing::warn!("ssh-agent signing failed: {error}");
                return Outcome::Rejected;
            }
        }
    }
    Outcome::Rejected
}
