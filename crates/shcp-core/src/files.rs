use crate::{EntryKind, Event, RemoteEntry, RequestId, session::Ssh};
use anyhow::{Context, Result, bail};
use russh_sftp::{
    client::{SftpSession, fs::Metadata},
    protocol::{FileType, OpenFlags},
};
use std::{collections::VecDeque, sync::Arc};
use tokio::{io::AsyncWriteExt, sync::OnceCell};
use tokio_util::sync::CancellationToken;

pub(crate) struct Files {
    ssh: Arc<Ssh>,
    sftp: OnceCell<Arc<SftpSession>>,
}
impl Files {
    pub fn new(ssh: Arc<Ssh>) -> Self {
        Self {
            ssh,
            sftp: OnceCell::new(),
        }
    }
    pub fn ssh(&self) -> &Ssh {
        &self.ssh
    }
    pub async fn get(&self) -> Result<Arc<SftpSession>> {
        self.sftp
            .get_or_try_init(|| async {
                let mut channel = self.ssh.channel_open_session().await?;
                channel.request_subsystem(true, "sftp").await?;
                loop {
                    match channel.wait().await {
                        Some(russh::ChannelMsg::Success) => break,
                        Some(russh::ChannelMsg::Failure | russh::ChannelMsg::Close) | None => {
                            bail!("The server rejected the SFTP subsystem")
                        }
                        _ => {}
                    }
                }
                let sftp = SftpSession::new(channel.into_stream())
                    .await
                    .context("Initialize SFTP")?;
                sftp.set_timeout(30);
                Ok(Arc::new(sftp))
            })
            .await
            .cloned()
    }
}
#[derive(Debug)]
pub(crate) enum FileOp {
    Home,
    List(String),
    Mkdir(String),
    /// Creates an empty file, never touching one already there.
    CreateFile(String),
    Rename(String, String),
    Remove(String, bool),
    /// What a path really is, through a symbolic link.
    Kind(String),
    /// How much room is left where a path is.
    FreeSpace(String),
    /// The first bytes of a file, for a preview.
    ReadHead(String),
    /// Sets the permission bits of each path.
    Chmod(Vec<String>, u32),
    /// Copies a file or folder to a full new path, on the server.
    Copy(String, String),
    /// Puts these names, relative to the folder, in a new archive at a full path.
    Compress {
        folder: String,
        names: Vec<String>,
        archive: String,
    },
    /// Unpacks an archive into a new folder at a full path.
    Extract {
        archive: String,
        folder: String,
    },
}
/// How much of a file a preview reads.
const PREVIEW_BYTES: u64 = 16 * 1024;

/// What a preview can show of these bytes: nothing, if they are not text. They may stop in the
/// middle of a character.
fn preview_text(bytes: &[u8]) -> Option<String> {
    if bytes.contains(&0) {
        return None;
    }
    let text = String::from_utf8_lossy(bytes);
    let broken = text
        .chars()
        .filter(|c| *c == char::REPLACEMENT_CHARACTER)
        .count();
    (broken <= 4).then(|| text.into_owned())
}

pub(crate) async fn execute(files: &Files, req: RequestId, op: FileOp) -> Event {
    let result = files.get().await;
    match op {
        FileOp::Home => Event::RemotePath {
            req,
            path: async { Ok(result?.canonicalize(".").await?) }
                .await
                .map_err(display),
        },
        FileOp::List(path) => {
            let listing = async {
                let sftp = result?;
                let canonical = sftp.canonicalize(&path).await?;
                let entries = list(&sftp, &canonical).await?;
                Ok::<_, anyhow::Error>((canonical, entries))
            }
            .await;
            match listing {
                Ok((path, entries)) => Event::DirListing {
                    req,
                    path,
                    entries: Ok(entries),
                },
                Err(error) => Event::DirListing {
                    req,
                    path,
                    entries: Err(display(error)),
                },
            }
        }
        FileOp::Kind(path) => {
            let kind = async {
                let metadata = result?.metadata(path.as_str()).await?;
                Ok::<_, anyhow::Error>(match metadata.file_type() {
                    FileType::Dir => EntryKind::Dir,
                    FileType::File => EntryKind::File,
                    _ => EntryKind::Other,
                })
            }
            .await
            .map_err(display);
            Event::RemoteKind { req, path, kind }
        }
        FileOp::FreeSpace(path) => {
            let info = async { Ok::<_, anyhow::Error>(result?.fs_info(path).await?) }.await;
            let sizes = info.ok().flatten().map(|info| {
                let unit = match info.fragment_size {
                    0 => info.block_size,
                    size => size,
                };
                (
                    info.blocks_avail.saturating_mul(unit),
                    info.blocks.saturating_mul(unit),
                )
            });
            Event::RemoteSpace {
                req,
                free: sizes.map(|(free, _)| free),
                total: sizes.map(|(_, total)| total).filter(|total| *total > 0),
            }
        }
        FileOp::ReadHead(path) => {
            let text = async {
                use tokio::io::AsyncReadExt as _;
                let file = result?.open(path.as_str()).await?;
                let mut bytes = Vec::with_capacity(PREVIEW_BYTES as usize);
                file.take(PREVIEW_BYTES).read_to_end(&mut bytes).await?;
                Ok::<_, anyhow::Error>(preview_text(&bytes))
            }
            .await
            .map_err(display);
            Event::RemoteHead { req, path, text }
        }
        op => {
            let result = async {
                let sftp = result?;
                match op {
                    FileOp::Mkdir(path) => sftp.create_dir(path).await?,
                    FileOp::CreateFile(path) => create_file(&sftp, path).await?,
                    FileOp::Rename(from, to) => sftp.rename(from, to).await?,
                    FileOp::Remove(path, recursive) => remove(&sftp, path, recursive).await?,
                    FileOp::Chmod(paths, mode) => {
                        for path in paths {
                            let mut metadata = Metadata::empty();
                            metadata.permissions = Some(mode & 0o7777);
                            sftp.set_metadata(path, metadata).await?;
                        }
                    }
                    FileOp::Copy(from, to) => copy_on_server(&files.ssh, &from, &to).await?,
                    FileOp::Compress {
                        folder,
                        names,
                        archive,
                    } => {
                        let command = compress_command(&folder, &names, &archive)?;
                        run_on_server(&files.ssh, command, "compress").await?;
                    }
                    FileOp::Extract { archive, folder } => {
                        let command = extract_command(&archive, &folder)?;
                        run_on_server(&files.ssh, command, "extract").await?;
                    }
                    _ => {}
                }
                Ok(())
            }
            .await
            .map_err(display);
            Event::OpDone { req, result }
        }
    }
}
/// `EXCLUDE` is what protects an existing file; the check first only gives a clearer error, since
/// SFTP servers usually answer it with a bare "Failure".
async fn create_file(sftp: &SftpSession, path: String) -> Result<()> {
    if sftp.try_exists(path.as_str()).await? {
        bail!("{path} already exists");
    }
    let mut file = sftp
        .open_with_flags(
            path,
            OpenFlags::CREATE | OpenFlags::EXCLUDE | OpenFlags::WRITE,
        )
        .await?;
    file.shutdown().await?;
    Ok(())
}
fn display(error: anyhow::Error) -> String {
    without_repeats(&format!("{error:#}"))
}
/// An SFTP error says its status and then the server's message, which is often the same words.
fn without_repeats(text: &str) -> String {
    let mut parts: Vec<&str> = text.split(": ").collect();
    parts.dedup();
    parts.join(": ")
}
/// A path as one word for a POSIX shell, whatever is in it.
pub fn shell_quote(path: &str) -> String {
    format!("'{}'", path.replace('\'', "'\\''"))
}
/// How much of what a command says is kept for the error.
const MAX_COMMAND_ERROR: usize = 4096;
/// SFTP cannot copy: `cp` on the server does it, without moving the data through this PC.
pub(crate) async fn copy_on_server(ssh: &Ssh, from: &str, to: &str) -> Result<()> {
    let command = format!("cp -a -- {} {}", shell_quote(from), shell_quote(to));
    run_on_server(ssh, command, "copy").await
}
/// The archives that can be made and opened, by the ending of their name.
const ARCHIVES: [(&str, char); 8] = [
    (".tar.gz", 'z'),
    (".tgz", 'z'),
    (".tar.bz2", 'j'),
    (".tbz2", 'j'),
    (".tar.xz", 'J'),
    (".txz", 'J'),
    (".tar", ' '),
    (".zip", 'Z'),
];
fn archive_kind(name: &str) -> Option<(&'static str, char)> {
    let lower = name.to_lowercase();
    ARCHIVES
        .iter()
        .copied()
        .find(|(ending, _)| lower.ends_with(ending))
}
/// The name of an archive without its ending, or `None` if it is not one.
pub fn archive_stem(name: &str) -> Option<&str> {
    let (ending, _) = archive_kind(name)?;
    name.get(..name.len() - ending.len())
        .filter(|stem| !stem.is_empty())
}
/// Written for sh, whatever the login shell is.
fn sh(script: &str) -> String {
    format!("sh -c {}", shell_quote(script))
}
/// What is on the command line starts with `./`, so that no name reads as an option. A failed
/// archive is removed: the check first says it is not somebody else's.
fn compress_command(folder: &str, names: &[String], archive: &str) -> Result<String> {
    let Some((_, kind)) = archive_kind(archive) else {
        bail!("{archive} does not end like an archive: try .tar.gz or .zip");
    };
    let target = shell_quote(archive);
    let names = names
        .iter()
        .map(|name| shell_quote(&format!("./{name}")))
        .collect::<Vec<_>>()
        .join(" ");
    let pack = match kind {
        'Z' => format!("zip -q -r {target} {names}"),
        ' ' => format!("tar -cf {target} {names}"),
        flag => format!("tar -c{flag}f {target} {names}"),
    };
    Ok(sh(&format!(
        "if [ -e {target} ]; then echo {exists} >&2; exit 1; fi; \
         cd {folder} && {pack} || {{ status=$?; rm -f {target}; exit $status; }}",
        exists = shell_quote(&format!("{archive} already exists")),
        folder = shell_quote(folder),
    )))
}
/// The folder is new, so nothing gets replaced however the archive is made, and a failure can take
/// it away.
fn extract_command(archive: &str, folder: &str) -> Result<String> {
    let Some((_, kind)) = archive_kind(archive) else {
        bail!("{archive} does not end like an archive");
    };
    let source = shell_quote(archive);
    let target = shell_quote(folder);
    let unpack = match kind {
        'Z' => format!("unzip -q {source}"),
        ' ' => format!("tar -xf {source}"),
        flag => format!("tar -x{flag}f {source}"),
    };
    Ok(sh(&format!(
        "mkdir {target} || exit 1; (cd {target} && {unpack}) || \
         {{ status=$?; rm -rf {target}; exit $status; }}"
    )))
}
/// Runs a command on the server for what SFTP cannot do, named by `what`. A server that allows only
/// SFTP says no, and that is what the user reads.
pub(crate) async fn run_on_server(ssh: &Ssh, command: String, what: &str) -> Result<()> {
    let mut channel = ssh.channel_open_session().await?;
    channel.exec(true, command).await?;
    let (mut status, mut said) = (None, Vec::new());
    while let Some(message) = channel.wait().await {
        match message {
            russh::ChannelMsg::ExtendedData { data, .. } if said.len() < MAX_COMMAND_ERROR => {
                said.extend_from_slice(&data);
            }
            russh::ChannelMsg::ExitStatus { exit_status } => status = Some(exit_status),
            russh::ChannelMsg::Failure => {
                bail!("This server allows no commands, and SFTP alone cannot {what}")
            }
            russh::ChannelMsg::Close => break,
            _ => {}
        }
    }
    match status {
        Some(0) => Ok(()),
        Some(code) => {
            let said = String::from_utf8_lossy(&said);
            let said = said.trim();
            if said.is_empty() {
                bail!("The command to {what} ended with status {code}")
            }
            bail!("{said}")
        }
        None => bail!("The server did not say whether it managed to {what}"),
    }
}
pub(crate) async fn list(sftp: &SftpSession, path: &str) -> Result<Vec<RemoteEntry>> {
    let mut entries = vec![];
    for entry in sftp.read_dir(path).await? {
        let metadata = entry.metadata();
        entries.push(RemoteEntry {
            name: entry.file_name(),
            path: entry.path(),
            kind: match entry.file_type() {
                FileType::Dir => EntryKind::Dir,
                FileType::File => EntryKind::File,
                FileType::Symlink => EntryKind::Symlink,
                _ => EntryKind::Other,
            },
            size: metadata.len(),
            modified: metadata.modified().ok(),
            permissions: metadata.permissions,
        });
    }
    entries.sort_by(|a, b| {
        (a.kind != EntryKind::Dir, &a.name).cmp(&(b.kind != EntryKind::Dir, &b.name))
    });
    Ok(entries)
}
/// Where a search of the subfolders stops: it goes over SFTP, one folder at a time.
const MAX_SEARCH_RESULTS: usize = 1000;
const MAX_SEARCH_FOLDERS: usize = 20_000;
/// Finds the names containing `query` under `root`, ignoring case and sending them as they come.
/// Links are not followed, and a folder that cannot be read is skipped. Tells whether it stopped
/// at a limit.
pub(crate) async fn search(
    sftp: &SftpSession,
    root: &str,
    query: &str,
    req: RequestId,
    events: &async_channel::Sender<Event>,
    cancel: &CancellationToken,
) -> Result<bool> {
    let query = query.to_lowercase();
    let mut folders = VecDeque::from([root.to_string()]);
    let (mut visited, mut found) = (0, 0);
    while let Some(folder) = folders.pop_front() {
        if visited == MAX_SEARCH_FOLDERS || found >= MAX_SEARCH_RESULTS {
            return Ok(true);
        }
        visited += 1;
        let listed = tokio::select! {
            _ = cancel.cancelled() => return Ok(false),
            listed = list(sftp, &folder) => listed,
        };
        let entries = match listed {
            Ok(entries) => entries,
            Err(error) if folder == root => return Err(error),
            Err(error) => {
                tracing::debug!(%folder, %error, "Search skipped a folder");
                continue;
            }
        };
        folders.extend(
            entries
                .iter()
                .filter(|entry| entry.kind == EntryKind::Dir)
                .map(|entry| entry.path.clone()),
        );
        let matches: Vec<RemoteEntry> = entries
            .into_iter()
            .filter(|entry| entry.name.to_lowercase().contains(&query))
            .take(MAX_SEARCH_RESULTS - found)
            .collect();
        if !matches.is_empty() {
            found += matches.len();
            let _ = events.try_send(Event::SearchFound {
                req,
                entries: matches,
            });
        }
    }
    Ok(false)
}
pub(crate) fn join(parent: &str, child: &str) -> String {
    if parent.ends_with('/') {
        format!("{parent}{child}")
    } else if parent.is_empty() {
        child.into()
    } else {
        format!("{parent}/{child}")
    }
}
async fn remove(sftp: &SftpSession, path: String, recursive: bool) -> Result<()> {
    if matches!(path.trim_end_matches('/'), "" | "." | "..") {
        bail!("Refusing to remove a root or current directory");
    }
    let mut stack = vec![(path, false)];
    while let Some((path, visited)) = stack.pop() {
        // lstat prevents following symlinks during recursive deletion.
        if visited {
            sftp.remove_dir(path).await?;
            continue;
        }
        let metadata = sftp.symlink_metadata(&path).await?;
        if metadata.file_type() == FileType::Dir {
            if recursive {
                let entries = sftp.read_dir(&path).await?;
                stack.push((path.clone(), true));
                for entry in entries {
                    safe_name(&entry.file_name())?;
                    stack.push((join(&path, &entry.file_name()), false));
                }
            } else {
                sftp.remove_dir(path).await?;
            }
        } else {
            sftp.remove_file(path).await?;
        }
    }
    Ok(())
}
/// A remote server must never be able to turn a directory entry into a local path traversal.
pub(crate) fn safe_name(name: &str) -> Result<()> {
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\', ':', '\0']) {
        bail!("Unsafe remote file name: {name:?}");
    }
    #[cfg(windows)]
    {
        let stem = name
            .split('.')
            .next()
            .unwrap_or_default()
            .to_ascii_uppercase();
        if name.ends_with(['.', ' '])
            || name.contains(['<', '>', '"', '|', '?', '*'])
            || matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || (stem.len() == 4
                && (stem.starts_with("COM") || stem.starts_with("LPT"))
                && stem.as_bytes()[3].is_ascii_digit())
        {
            bail!("File name is not supported on Windows: {name:?}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::without_repeats;

    #[test]
    fn a_message_repeated_in_a_row_is_said_once() {
        assert_eq!(
            without_repeats("Permission denied: Permission denied"),
            "Permission denied"
        );
    }

    #[test]
    fn different_parts_stay_as_they_are() {
        assert_eq!(
            without_repeats("Open /etc: No such file: try again"),
            "Open /etc: No such file: try again"
        );
    }
}
