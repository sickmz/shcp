//! Key passphrases and host passwords the user chose to remember, kept in the Windows Credential
//! Manager: encrypted for this Windows account and never written to shcp's own files.
use anyhow::Result;
use std::path::Path;
use zeroize::Zeroizing;

const SERVICE: &str = "shcp";

/// Where the passphrase of a key file is kept.
pub(crate) fn passphrase_account(key: &Path) -> String {
    format!("passphrase:{}", key.display())
}

/// Where the password of `user@host:port` is kept.
pub(crate) fn password_account(target: &str) -> String {
    format!("password:{target}")
}

fn entry(account: &str) -> keyring::Result<keyring::Entry> {
    keyring::Entry::new(SERVICE, account)
}

/// A remembered secret. A failure only means asking the user.
pub(crate) fn load(account: &str) -> Option<Zeroizing<String>> {
    match entry(account).and_then(|entry| entry.get_password()) {
        Ok(secret) => Some(Zeroizing::new(secret)),
        Err(keyring::Error::NoEntry) => None,
        Err(error) => {
            tracing::warn!(%error, account, "Could not read a saved secret");
            None
        }
    }
}

pub(crate) fn save(account: &str, secret: &str) -> Result<()> {
    entry(account)?.set_password(secret)?;
    Ok(())
}

pub(crate) fn forget(account: &str) {
    match entry(account).and_then(|entry| entry.delete_credential()) {
        Ok(()) | Err(keyring::Error::NoEntry) => {}
        Err(error) => tracing::warn!(%error, account, "Could not remove a saved secret"),
    }
}
