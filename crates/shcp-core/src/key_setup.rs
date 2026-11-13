//! Logging in with a key: what `ssh-keygen` and `ssh-copy-id` do.
use crate::{
    files::{run_on_server, shell_quote},
    session::Ssh,
    ssh_keys::public_path,
};
use anyhow::{Context, Result};
use russh::keys::{self, Algorithm, PrivateKey, PublicKey, ssh_key::LineEnding};
use std::{fs, io::Write, path::Path};

/// The public half of the key at `path`, as a line of `authorized_keys`. A key that is not there
/// yet is created.
pub(crate) fn public_line(path: &Path, passphrase: Option<&str>, comment: &str) -> Result<String> {
    if !path.exists() {
        return generate(path, passphrase, comment);
    }
    if let Ok(text) = fs::read_to_string(public_path(path)) {
        let line = text.trim();
        PublicKey::from_openssh(line).context("Read the public key next to the key file")?;
        return Ok(line.to_string());
    }
    let key = keys::load_secret_key(path, passphrase)
        .with_context(|| format!("Read {}", path.display()))?;
    Ok(key.public_key().to_openssh()?)
}

fn generate(path: &Path, passphrase: Option<&str>, comment: &str) -> Result<String> {
    if let Some(folder) = path.parent() {
        fs::create_dir_all(folder).with_context(|| format!("Create {}", folder.display()))?;
    }
    let mut rng = rand::rng();
    let mut key = PrivateKey::random(&mut rng, Algorithm::Ed25519)?;
    key.set_comment(comment);
    let line = key.public_key().to_openssh()?;
    if let Some(passphrase) = passphrase {
        key = key.encrypt(&mut rng, passphrase)?;
    }
    let private = key.to_openssh(LineEnding::LF)?;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    // On Windows the file takes the permissions of the folder, which OpenSSH accepts in ~/.ssh.
    let mut file = options
        .open(path)
        .with_context(|| format!("Create {}", path.display()))?;
    file.write_all(private.as_bytes())?;
    file.sync_all()?;
    let public = public_path(path);
    fs::write(&public, format!("{line}\n"))
        .with_context(|| format!("Create {}", public.display()))?;
    Ok(line)
}

/// Adds the key to `authorized_keys` unless it is there, with the permissions `sshd` wants.
pub(crate) async fn install(ssh: &Ssh, line: &str) -> Result<()> {
    let key = shell_quote(line);
    let file = "\"$HOME/.ssh/authorized_keys\"";
    let script = format!(
        "umask 077; mkdir -p \"$HOME/.ssh\" && touch {file} && \
         {{ grep -qxF -- {key} {file} || {{ \
         if [ -n \"$(tail -c 1 {file})\" ]; then echo >> {file}; fi; \
         printf '%s\\n' {key} >> {file}; }}; }} && \
         chmod 700 \"$HOME/.ssh\" && chmod 600 {file}"
    );
    // Whatever the login shell is: this is written for sh.
    let command = format!("sh -c {}", shell_quote(&script));
    run_on_server(ssh, command, "install a key").await
}
