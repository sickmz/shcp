use crate::{
    ConnectStep, HostConfig, HostKeyKind, SessionState, config,
    session::{Context as SessionContext, PromptKind},
};
use anyhow::{Context, Result, bail};
use russh::{
    Channel,
    client::{self, ChannelOpenHandle, Msg, Session},
    keys::{self, HashAlg, PublicKey, PublicKeyOrCertificate},
};
use std::{fmt::Write as _, fs, path::Path, sync::Arc};

pub(crate) struct Handler {
    pub context: Arc<SessionContext>,
    /// The machine at the other end: the session's host, or a jump host on the way to it.
    pub host: HostConfig,
    /// True only for the session's final host: a jump host gets no tunnels and no agent.
    pub is_target: bool,
}
impl client::Handler for Handler {
    type Error = anyhow::Error;
    async fn check_server_key(&mut self, key: &PublicKeyOrCertificate) -> Result<bool> {
        let key = key.public_key();
        let ctx = &self.context;
        let host = &self.host;
        ctx.step(
            host,
            ConnectStep::VerifyingHostKey,
            Some(key.algorithm().to_string()),
        );
        let path = &ctx.paths.known_hosts;
        // Fail closed on unreadable files (russh's helper alone treats open failures as unknown).
        config::read_if_exists(path).context("Read known_hosts")?;
        let records = keys::known_hosts::known_host_keys_path(&host.hostname, host.port, path)?;
        if records.iter().any(|(_, k)| k.key_data() == key.key_data()) {
            return Ok(true);
        }
        let kind = if records.is_empty() {
            HostKeyKind::Unknown
        } else {
            HostKeyKind::Changed
        };
        ctx.prompts.begin(PromptKind::HostKey);
        ctx.state(SessionState::AwaitingHostKey {
            host: host.hostname.clone(),
            key_type: key.algorithm().to_string(),
            fingerprint_sha256: key.fingerprint(HashAlg::Sha256).to_string(),
            kind,
        });
        let accepted = tokio::select! { _ = ctx.cancel.cancelled() => false, result = ctx.prompts.host_rx.recv() => result.unwrap_or(false) };
        ctx.prompts.end();
        if !accepted {
            bail!("Host key was not accepted");
        }
        // Serialize writes from concurrent sessions. No lock is held across an await.
        let _guard = ctx.known_hosts_lock.lock();
        save(path, &host.hostname, host.port, &key, &records)?;
        Ok(true)
    }
    /// A connection to one of the server's ports that listen for this PC. Dropping `reply`
    /// refuses it, as for a port nobody asked to open.
    async fn server_channel_open_forwarded_tcpip(
        &mut self,
        channel: Channel<Msg>,
        _address: &str,
        port: u32,
        _originator: &str,
        _originator_port: u32,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<()> {
        if !self.is_target {
            tracing::warn!(host = %self.host.alias, "Refused a jump host's remote forward");
            return Ok(());
        }
        let forward = self.context.remote_forwards.lock().get(&port).cloned();
        if let Some(forward) = forward {
            crate::forward::accept_remote(self.context.clone(), forward, channel, reply).await;
        }
        Ok(())
    }
    /// The server wants to use this PC's agent. It gets it only on the machine the user allowed,
    /// so a jump host on the way can never ask for it.
    async fn server_channel_open_agent_forward(
        &mut self,
        channel: Channel<Msg>,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<()> {
        if !self.host.forward_agent || !self.is_target {
            tracing::warn!(host = %self.host.alias, "Refused a request for the SSH agent");
            return Ok(());
        }
        let Some(mut agent) = crate::auth::agent::stream().await else {
            tracing::warn!("The server asked for the SSH agent, but none is running");
            return Ok(());
        };
        reply.accept().await;
        tokio::spawn(async move {
            let mut stream = channel.into_stream();
            let _ = tokio::io::copy_bidirectional(&mut agent, &mut stream).await;
        });
        Ok(())
    }
}
fn save(
    path: &Path,
    host: &str,
    port: u16,
    key: &PublicKey,
    records: &[(usize, PublicKey)],
) -> Result<()> {
    if records.is_empty() {
        keys::known_hosts::learn_known_hosts_path(host, port, key, path)?;
        return Ok(());
    }
    let text = fs::read_to_string(path).context("Read known_hosts before replacing host key")?;
    let name = if port == 22 {
        host.into()
    } else {
        format!("[{host}]:{port}")
    };
    let mut output = String::new();
    for (index, line) in text.lines().enumerate() {
        if records.iter().any(|(n, _)| *n == index + 1) {
            // Preserve other hosts sharing an unhashed comma-separated record.
            if let Some((hosts, rest)) = line.split_once(char::is_whitespace) {
                let remaining = hosts
                    .split(',')
                    .filter(|h| *h != name && !h.starts_with('|'))
                    .collect::<Vec<_>>();
                if !remaining.is_empty() {
                    output.push_str(&remaining.join(","));
                    output.push(' ');
                    output.push_str(rest);
                    output.push('\n');
                }
            }
        } else {
            output.push_str(line);
            output.push('\n');
        }
    }
    writeln!(output, "{name} {}", key.to_openssh()?)?;
    // OpenSSH reads this file too: a crash here must not leave half of it.
    config::write_atomic(path, output.as_bytes()).context("Replace host key in known_hosts")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::Scratch;

    const OLD: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIJdD7y3aLq454yWBdwLWbieU1ebz9/cu7/QEXn9OIeZJ";
    const NEW: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIA6rWI3G1sz07DnfFlrouTcysQlj2P+jpNSOEWD9OJ3X";

    fn key(data: &str) -> PublicKey {
        PublicKey::from_openssh(&format!("ssh-ed25519 {data}")).unwrap()
    }

    #[test]
    fn replacing_a_key_keeps_the_rest_of_the_file_and_leaves_no_temporary() {
        let dir = Scratch::new("known-hosts");
        let path = dir.0.join("known_hosts");
        fs::write(
            &path,
            format!(
                "other ssh-ed25519 {OLD}\ntarget,alias ssh-ed25519 {OLD}\nlast ssh-ed25519 {OLD}\n"
            ),
        )
        .unwrap();
        save(&path, "target", 22, &key(NEW), &[(2, key(OLD))]).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        let names: Vec<_> = text
            .lines()
            .map(|line| line.split_whitespace().next().unwrap())
            .collect();
        assert_eq!(names, ["other", "alias", "last", "target"]);
        assert!(text.lines().last().unwrap().ends_with(NEW));
        assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);
    }
}
