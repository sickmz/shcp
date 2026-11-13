//! Runtime-independent interface for the shcp desktop client.
mod auth;
mod check;
mod config;
mod edit;
mod engine;
mod files;
mod forward;
mod host_key;
mod key_setup;
mod known_hosts;
mod logging;
mod login_shell;
mod paste;
mod proxy;
mod secrets;
mod session;
mod session_log;
mod socks;
mod ssh_command;
mod ssh_keys;
mod terminal;
mod tmux;
mod transfer;
mod types;

pub use check::{CheckOutcome, CheckQuestion, CheckUpdate, ConnectionCheck};
pub use engine::{EngineHandle, start};
pub use files::{archive_stem, shell_quote};
pub use known_hosts::KnownHost;
pub use logging::CRASH_LOG;
pub use shcp_term as term;
pub use shcp_term::{TerminalHandle, TerminalId};
pub use ssh_command::{SshCommand, address_text, is_ssh_command, suggested_host_name};
pub use ssh_keys::{USUAL_KEYS, find_ssh_keys};
pub use tmux::new_terminal_key;
pub use types::*;

/// Starts a console program without opening a window for it.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Seconds since the Unix epoch, 0 for a clock set before it.
fn unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

#[cfg(test)]
pub(crate) mod test_support {
    use crate::AppPaths;
    use std::{
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
    };

    /// A folder of its own in the temporary folder, removed with what is in it when dropped.
    pub(crate) struct Scratch(pub(crate) PathBuf);
    impl Scratch {
        /// `name` tells whose folder it is; the process and a counter keep it apart from others.
        pub(crate) fn new(name: &str) -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let dir = std::env::temp_dir().join(format!(
                "shcp-{name}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::SeqCst)
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        /// Writes a file at `relative`, making the folders on the way.
        pub(crate) fn file(&self, relative: &str, data: impl AsRef<[u8]>) -> PathBuf {
            let path = self.0.join(relative);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&path, data).unwrap();
            path
        }
        /// Where the app would keep everything, all in this folder.
        pub(crate) fn paths(&self) -> AppPaths {
            paths_in(&self.0)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    /// Where the app would keep everything, all in `dir`.
    pub(crate) fn paths_in(dir: &Path) -> AppPaths {
        AppPaths {
            config_dir: dir.to_path_buf(),
            data_dir: dir.to_path_buf(),
            log_dir: dir.to_path_buf(),
            known_hosts: dir.join("known_hosts"),
            ssh_config: dir.join("config"),
        }
    }
}
