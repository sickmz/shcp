//! Reads the servers `known_hosts` trusts and forgets them. OpenSSH uses this file too.
use crate::config;
use anyhow::{Context, Result, bail};
use russh::keys::{HashAlg, PublicKey};
use std::{fs, io::ErrorKind, path::Path};

/// One line of `known_hosts`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownHost {
    /// The names on it, or `None` when they are hashed and cannot be read.
    pub names: Option<String>,
    pub key_type: String,
    /// Empty when the key on the line cannot be read.
    pub fingerprint: String,
    /// `@revoked` or `@cert-authority`.
    pub marker: Option<String>,
    /// The line as it is in the file, to find it again.
    line: String,
}

fn parse_line(line: &str) -> Option<KnownHost> {
    let text = line.trim();
    if text.is_empty() || text.starts_with('#') {
        return None;
    }
    let mut fields = text.split_whitespace().peekable();
    let marker = fields
        .next_if(|field| field.starts_with('@'))
        .map(str::to_string);
    let (names, key_type, key) = (fields.next()?, fields.next()?, fields.next()?);
    let fingerprint = PublicKey::from_openssh(&format!("{key_type} {key}"))
        .map_or(String::new(), |key| {
            key.fingerprint(HashAlg::Sha256).to_string()
        });
    Some(KnownHost {
        names: (!names.starts_with('|')).then(|| names.replace(',', ", ")),
        key_type: key_type.to_string(),
        fingerprint,
        marker,
        line: line.to_string(),
    })
}

/// A file that does not exist yet trusts nobody.
pub(crate) fn read(path: &Path) -> Result<Vec<KnownHost>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text.lines().filter_map(parse_line).collect()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error).context("Read known_hosts"),
    }
}

/// Removes that line and leaves the others as they are.
pub(crate) fn forget(path: &Path, host: &KnownHost) -> Result<()> {
    let text = fs::read_to_string(path).context("Read known_hosts")?;
    let mut found = false;
    let mut output = String::with_capacity(text.len());
    for line in text.lines() {
        if !found && line == host.line {
            found = true;
        } else {
            output.push_str(line);
            output.push('\n');
        }
    }
    if !found {
        bail!("known_hosts changed in the meantime");
    }
    // A crash here must not leave half of the file.
    config::write_atomic(path, output.as_bytes()).context("Write known_hosts")
}
