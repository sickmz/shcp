//! Editing a remote file in a local program: a private copy is opened in the editor and every
//! save is written back to the server until the session ends.
use crate::{Event, NoticeLevel, files, session::Control};
use anyhow::{Context as _, Result, bail};
use async_channel::Receiver;
use russh_sftp::{
    client::{SftpSession, error::Error as SftpError, fs::Metadata},
    protocol::{FileType, StatusCode},
};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::{Duration, SystemTime},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    time::MissedTickBehavior,
};

/// Every copy is held in memory for a moment, and a bigger file is rarely edited by hand.
const MAX_SIZE: u64 = 50 * 1024 * 1024;
/// Copies are kept across restarts for a while: one may hold changes the server never got.
const KEEP_COPIES: Duration = Duration::from_secs(24 * 60 * 60);

pub(crate) enum EditCommand {
    /// The user opened the file again, with the editor set at that moment.
    Reopen(String),
    /// The user chose to replace the version on the server.
    Overwrite,
}

/// Identifies one version of a file; two saves in the same second with the same size look alike
/// on the server, where times have a resolution of one second.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: Option<SystemTime>,
}

struct Edit {
    control: Arc<Control>,
    remote: String,
    name: String,
    local: PathBuf,
    /// Local version the server has.
    synced: Stamp,
    /// Server version the local copy started from.
    server: Stamp,
    /// Local version seen on the last check: a save is written back once it stops changing.
    seen: Option<Stamp>,
    /// Local version already reported as not saved; only another save or a choice retries it.
    parked: Option<Stamp>,
    /// Replace the server version even if it changed.
    force: bool,
}

pub(crate) async fn run(
    control: Arc<Control>,
    remote: String,
    dir: PathBuf,
    editor: String,
    commands: Receiver<EditCommand>,
) {
    let mut edit = match Edit::open(control.clone(), remote, dir).await {
        Ok(edit) => edit,
        Err(error) => {
            control.context.notice(
                NoticeLevel::Error,
                format!("Could not open file: {error:#}"),
            );
            return;
        }
    };
    edit.launch(&editor);
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        tokio::select! { biased;
            _ = control.context.cancel.cancelled() => break,
            command = commands.recv() => match command {
                Ok(EditCommand::Reopen(editor)) => edit.reopen(&editor).await,
                Ok(EditCommand::Overwrite) => {
                    edit.force = true;
                    edit.parked = None;
                }
                Err(_) => break,
            },
            _ = tick.tick() => edit.poll().await,
        }
    }
    edit.finish().await;
}

impl Edit {
    async fn open(control: Arc<Control>, remote: String, dir: PathBuf) -> Result<Self> {
        let name = remote
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .context("Remote path has no file name")?
            .to_string();
        files::safe_name(&name)?;
        let local = dir.join(&name);
        let sftp = control.files().await?.get().await?;
        let (server, synced) = download(&sftp, &remote, &local).await?;
        Ok(Self {
            control,
            remote,
            name,
            local,
            synced,
            server,
            seen: None,
            parked: None,
            force: false,
        })
    }

    fn notice(&self, level: NoticeLevel, message: String) {
        self.control.context.notice(level, message);
    }

    fn launch(&self, editor: &str) {
        let program = if editor.trim().is_empty() {
            default_editor()
        } else {
            PathBuf::from(editor.trim())
        };
        // Never "open with the default app": for a .bat or .exe from the server that means run it.
        let started = tokio::process::Command::new(&program)
            .arg(&self.local)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        if let Err(error) = started {
            self.notice(
                NoticeLevel::Error,
                format!(
                    "Could not start {}: {error}. Choose the editor in Settings > Files.",
                    program.display()
                ),
            );
        }
    }

    async fn reopen(&mut self, editor: &str) {
        // Without local changes, show what is on the server now.
        let unchanged = matches!(local_stamp(&self.local).await, Ok(stamp) if stamp == self.synced);
        if unchanged && let Err(error) = self.refresh().await {
            self.notice(
                NoticeLevel::Warning,
                format!("Could not refresh {}: {error:#}", self.name),
            );
        }
        self.launch(editor);
    }

    async fn refresh(&mut self) -> Result<()> {
        let sftp = self.control.files().await?.get().await?;
        let metadata = sftp.symlink_metadata(&self.remote).await?;
        if remote_stamp(&metadata) != self.server {
            (self.server, self.synced) = download(&sftp, &self.remote, &self.local).await?;
        }
        Ok(())
    }

    async fn poll(&mut self) {
        // Editors replace files in steps, so a missing or locked file is simply checked again.
        let Ok(current) = local_stamp(&self.local).await else {
            return;
        };
        if current == self.synced {
            self.seen = None;
            self.parked = None;
            self.force = false;
            return;
        }
        if self.seen != Some(current) {
            self.seen = Some(current);
            return;
        }
        if self.parked == Some(current) {
            return;
        }
        let Ok(data) = tokio::fs::read(&self.local).await else {
            return;
        };
        match self.save(&data).await {
            Ok(true) => {
                self.synced = current;
                self.parked = None;
                self.notice(
                    NoticeLevel::Info,
                    format!("Saved {} to the server", self.name),
                );
            }
            Ok(false) => {
                self.parked = Some(current);
                self.control.context.emit(Event::EditConflict {
                    session: self.control.context.id,
                    remote: self.remote.clone(),
                });
            }
            Err(error) => {
                self.parked = Some(current);
                self.notice(
                    NoticeLevel::Error,
                    format!(
                        "Could not save {} to the server, save it again to retry: {error:#}. \
                         Your copy is in {}",
                        self.name,
                        self.local.display()
                    ),
                );
            }
        }
        self.force = false;
    }

    /// Writes in place, which keeps the owner and mode of the file. `false`: the server version
    /// changed since it was read, nothing was written. Unlike uploads a failure removes nothing:
    /// the file on the server is what the user is editing.
    async fn save(&mut self, data: &[u8]) -> Result<bool> {
        let sftp = self.control.files().await?.get().await?;
        let current = match sftp.symlink_metadata(&self.remote).await {
            Ok(metadata) if metadata.file_type() == FileType::File => Some(remote_stamp(&metadata)),
            Ok(_) => bail!("It is no longer a regular file on the server"),
            Err(SftpError::Status(status)) if status.status_code == StatusCode::NoSuchFile => None,
            Err(error) => return Err(error.into()),
        };
        if !self.force && current != Some(self.server) {
            return Ok(false);
        }
        let mut output = sftp.create(&self.remote).await?;
        output.write_all(data).await?;
        output.shutdown().await?;
        drop(output);
        self.server = remote_stamp(&sftp.symlink_metadata(&self.remote).await?);
        Ok(true)
    }

    /// The copy stays on disk: the editor may still have it open, and it may hold unsaved work.
    async fn finish(&self) {
        let unsaved = !matches!(local_stamp(&self.local).await, Ok(stamp) if stamp == self.synced);
        if unsaved {
            self.notice(
                NoticeLevel::Important,
                format!(
                    "Changes to {} did not reach the server. Your copy is in {}",
                    self.name,
                    self.local.display()
                ),
            );
        } else {
            self.notice(
                NoticeLevel::Important,
                format!(
                    "Disconnected: saves to {} no longer reach the server. Open it again after reconnecting.",
                    self.name
                ),
            );
        }
    }
}

async fn download(sftp: &SftpSession, remote: &str, local: &Path) -> Result<(Stamp, Stamp)> {
    let metadata = sftp.symlink_metadata(remote).await?;
    if metadata.file_type() != FileType::File {
        bail!("{remote} is not a regular file");
    }
    if metadata.len() > MAX_SIZE {
        bail!("{remote} is larger than 50 MB: download it instead");
    }
    let mut input = sftp.open(remote).await?;
    let mut data = Vec::new();
    (&mut input)
        .take(MAX_SIZE + 1)
        .read_to_end(&mut data)
        .await?;
    input.close().await?;
    if data.len() as u64 > MAX_SIZE {
        bail!("{remote} is larger than 50 MB: download it instead");
    }
    if let Some(parent) = local.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(local, &data).await?;
    Ok((remote_stamp(&metadata), local_stamp(local).await?))
}

fn remote_stamp(metadata: &Metadata) -> Stamp {
    Stamp {
        len: metadata.len(),
        modified: metadata.modified().ok(),
    }
}

async fn local_stamp(path: &Path) -> std::io::Result<Stamp> {
    let metadata = tokio::fs::metadata(path).await?;
    Ok(Stamp {
        len: metadata.len(),
        modified: metadata.modified().ok(),
    })
}

#[cfg(windows)]
fn default_editor() -> PathBuf {
    const VS_CODE: &str = r"Microsoft VS Code\Code.exe";
    let user = std::env::var_os("LOCALAPPDATA")
        .map(|dir| PathBuf::from(dir).join("Programs").join(VS_CODE));
    let system = std::env::var_os("ProgramFiles").map(|dir| PathBuf::from(dir).join(VS_CODE));
    [user, system]
        .into_iter()
        .flatten()
        .find(|path| path.exists())
        .unwrap_or_else(|| "notepad.exe".into())
}

#[cfg(not(windows))]
fn default_editor() -> PathBuf {
    "xdg-open".into()
}

pub(crate) fn root(data_dir: &Path) -> PathBuf {
    data_dir.join("edit")
}

/// Removes the copies of earlier runs once they are old enough.
pub(crate) async fn cleanup(root: PathBuf) {
    let Ok(mut dirs) = tokio::fs::read_dir(&root).await else {
        return;
    };
    while let Ok(Some(dir)) = dirs.next_entry().await {
        let old = dir
            .metadata()
            .await
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age > KEEP_COPIES);
        let path = dir.path();
        if old && let Err(error) = tokio::fs::remove_dir_all(&path).await {
            tracing::warn!(%error, path = %path.display(), "Could not remove an old edit copy");
        }
    }
}
