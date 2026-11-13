use crate::{
    EntryKind, Event, RemoteEntry, RequestId, SearchEnd, SearchScope,
    session::{self, Ssh},
};
use anyhow::{Context, Result, bail};
use russh_sftp::{
    client::{SftpSession, fs::Metadata},
    protocol::{FileType, OpenFlags},
};
use std::{
    collections::{BTreeSet, HashSet, VecDeque},
    fmt::Write as _,
    sync::Arc,
    time::{Duration, UNIX_EPOCH},
};
use tokio::{io::AsyncWriteExt, sync::OnceCell, task::JoinSet};
use tokio_util::sync::CancellationToken;

pub(crate) struct Files {
    ssh: Arc<Ssh>,
    sftp: OnceCell<Arc<SftpSession>>,
}
impl Files {
    pub(crate) fn new(ssh: Arc<Ssh>) -> Self {
        Self {
            ssh,
            sftp: OnceCell::new(),
        }
    }
    pub(crate) fn ssh(&self) -> &Ssh {
        &self.ssh
    }
    pub(crate) async fn get(&self) -> Result<Arc<SftpSession>> {
        self.sftp
            .get_or_try_init(|| async {
                let mut channel = self.ssh.channel_open_session().await?;
                channel.request_subsystem(true, "sftp").await?;
                session::accepted(&mut channel, "The server rejected the SFTP subsystem").await?;
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

/// Runs one file operation. `files` is the session's, or why it has none: each operation reports
/// that the same way as its own failure.
pub(crate) async fn execute(files: Result<Arc<Files>>, req: RequestId, op: FileOp) -> Event {
    let result = async {
        let files = files?;
        let sftp = files.get().await?;
        Ok::<_, anyhow::Error>((files, sftp))
    }
    .await;
    match op {
        FileOp::Home => Event::RemotePath {
            req,
            path: async { Ok(result?.1.canonicalize(".").await?) }
                .await
                .map_err(display),
        },
        FileOp::List(path) => {
            let listing = async {
                let (_, sftp) = result?;
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
                let metadata = result?.1.metadata(path.as_str()).await?;
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
            let info = async { Ok::<_, anyhow::Error>(result?.1.fs_info(path).await?) }.await;
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
                let file = result?.1.open(path.as_str()).await?;
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
                let (files, sftp) = result?;
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
                    FileOp::Copy(from, to) => copy_on_server(files.ssh(), &from, &to).await?,
                    FileOp::Compress {
                        folder,
                        names,
                        archive,
                    } => {
                        let command = compress_command(&folder, &names, &archive)?;
                        run_on_server(files.ssh(), command, "compress").await?;
                    }
                    FileOp::Extract { archive, folder } => {
                        let command = extract_command(&archive, &folder)?;
                        run_on_server(files.ssh(), command, "extract").await?;
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
/// Runs a command on the server and collects what it writes; empty when it cannot run.
pub(crate) async fn read_all(ssh: &Ssh, command: String) -> Vec<u8> {
    let mut output = Vec::new();
    let Ok(mut channel) = ssh.channel_open_session().await else {
        return output;
    };
    if channel.exec(true, command).await.is_err() {
        return output;
    }
    while let Some(message) = channel.wait().await {
        match message {
            russh::ChannelMsg::Data { data } => output.extend_from_slice(&data),
            russh::ChannelMsg::Failure | russh::ChannelMsg::Close => break,
            _ => {}
        }
    }
    output
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
/// Where a search of the subfolders stops. The folders count only over SFTP.
const MAX_SEARCH_RESULTS: usize = 1000;
const MAX_SEARCH_FOLDERS: usize = 20_000;
/// Folders a search reads at once: each one costs a few round trips over SFTP.
const SEARCH_READS: usize = 16;
/// What `find` prints once it has shown it can do the job, ahead of what it finds.
const FIND_READY: &[u8] = b"shcp-find\0";
/// How long a server gets to run `find` before the search goes over SFTP instead.
const FIND_START: Duration = Duration::from_secs(10);
/// More than this without an end is no line of `find`, or noise ahead of the first one.
const MAX_FIND_LINE: usize = 64 * 1024;
/// Finds the names containing `query` under `root`, ignoring case and sending them as they come.
/// Links are not followed, and a folder that cannot be read is skipped. `root` itself is always
/// read, whatever its name. `find` on the server does it in a moment; where there is none that
/// can list what the rows show, it goes over SFTP.
pub(crate) async fn search(
    files: &Files,
    root: &str,
    query: &str,
    scope: &SearchScope,
    req: RequestId,
    events: &async_channel::Sender<Event>,
    cancel: &CancellationToken,
) -> Result<SearchEnd> {
    let query = query.to_lowercase();
    if root.starts_with('/')
        && let Some(end) =
            search_with_find(files.ssh(), root, &query, scope, req, events, cancel).await
    {
        return Ok(end);
    }
    if cancel.is_cancelled() {
        return Ok(SearchEnd::default());
    }
    let sftp = files.get().await?;
    search_over_sftp(sftp, root, &query, scope, req, events, cancel).await
}
/// A name as `find` reads it, with nothing in it taken for a wildcard.
fn find_literal(name: &str) -> String {
    let mut literal = String::with_capacity(name.len());
    for c in name.chars() {
        if matches!(c, '*' | '?' | '[' | ']' | '\\') {
            literal.push('\\');
        }
        literal.push(c);
    }
    literal
}
/// What `find -iname` looks for. Whether it folds the case of a letter beyond ASCII depends on the
/// locale of the server, so such a letter matches anything here and `search_with_find` checks the
/// names itself.
fn find_pattern(query: &str) -> String {
    let parts: Vec<String> = query
        .split(|c: char| !c.is_ascii() && (c.is_lowercase() || c.is_uppercase()))
        .map(find_literal)
        .collect();
    format!("*{}*", parts.join("*"))
}
/// Each line tells why it is there, then what a row shows. The first `find` checks that `-printf`
/// exists: BusyBox and BSD have none. `exec` lets a signal reach the second one.
fn find_command(root: &str, query: &str, scope: &SearchScope) -> String {
    const ROW: &str = r"\t%y\t%s\t%T@\t%m\t%p\0";
    let root = shell_quote(root);
    let pattern = shell_quote(&find_pattern(query));
    // The kernel makes up what is in these, without end.
    let mut leave_out = String::from("'(' -path /proc -o -path /sys -o -path /dev ')' -prune -o ");
    if !scope.hidden {
        leave_out.push_str("-name '.*' -prune -o ");
    }
    if !scope.skip.is_empty() {
        let names: Vec<String> = scope
            .skip
            .iter()
            .map(|name| format!("-name {}", shell_quote(&find_literal(name))))
            .collect();
        let _ = write!(
            leave_out,
            "-type d '(' {} ')' -prune -printf 's{ROW}' -o ",
            names.join(" -o ")
        );
    }
    let check = format!(r"find -H {root} -maxdepth 0 -printf 'shcp-find\0'");
    let search = format!("find -H {root} -mindepth 1 {leave_out}-iname {pattern} -printf 'm{ROW}'");
    sh(&format!("{check} && exec {search}"))
}
/// Reads a line of `find`: whether it is a folder left out, and the row.
fn find_line(line: &[u8]) -> Option<(bool, RemoteEntry)> {
    let line = String::from_utf8_lossy(line);
    let mut fields = line.splitn(6, '\t');
    let left_out = fields.next()? == "s";
    let kind = match fields.next()? {
        "d" => EntryKind::Dir,
        "f" => EntryKind::File,
        "l" => EntryKind::Symlink,
        _ => EntryKind::Other,
    };
    let size = fields.next()?.parse().ok()?;
    let modified = fields
        .next()?
        .split('.')
        .next()
        .and_then(|seconds| seconds.parse().ok())
        .and_then(|seconds| UNIX_EPOCH.checked_add(Duration::from_secs(seconds)));
    let permissions = u32::from_str_radix(fields.next()?, 8).ok();
    let path = fields.next()?.to_string();
    let name = path.rsplit('/').next().filter(|name| !name.is_empty())?;
    Some((
        left_out,
        RemoteEntry {
            name: name.to_string(),
            path: path.clone(),
            kind,
            size,
            modified,
            permissions,
        },
    ))
}
/// Searches with `find` on the server. `None`: it could not, and nothing was found yet.
async fn search_with_find(
    ssh: &Ssh,
    root: &str,
    query: &str,
    scope: &SearchScope,
    req: RequestId,
    events: &async_channel::Sender<Event>,
    cancel: &CancellationToken,
) -> Option<SearchEnd> {
    let mut channel = ssh.channel_open_session().await.ok()?;
    channel
        .exec(true, find_command(root, query, scope))
        .await
        .ok()?;
    // An account limited to SFTP may be waiting for what comes in.
    channel.eof().await.ok()?;
    let mut pending = Vec::new();
    let mut ready = false;
    let (mut found, mut skipped, mut status) = (0, BTreeSet::new(), None);
    let give_up = tokio::time::sleep(FIND_START);
    tokio::pin!(give_up);
    // Without a signal `find` would go on until it next has something to say.
    let stop = async |channel: &russh::Channel<_>| {
        let _ = channel.signal(russh::Sig::TERM).await;
        let _ = channel.close().await;
    };
    loop {
        let message = tokio::select! {
            _ = cancel.cancelled() => {
                stop(&channel).await;
                return Some(SearchEnd { limited: false, skipped: skipped.into_iter().collect() });
            }
            _ = &mut give_up, if !ready => {
                stop(&channel).await;
                return None;
            }
            message = channel.wait() => message,
        };
        match message {
            Some(russh::ChannelMsg::Data { data }) => {
                pending.extend_from_slice(&data);
                if !ready {
                    let at = pending
                        .windows(FIND_READY.len())
                        .position(|bytes| bytes == FIND_READY);
                    let Some(at) = at else {
                        if pending.len() > MAX_FIND_LINE {
                            stop(&channel).await;
                            return None;
                        }
                        continue;
                    };
                    // A login script may have said something first.
                    pending.drain(..at + FIND_READY.len());
                    ready = true;
                }
                let complete = pending.iter().rposition(|b| *b == 0).map_or(0, |at| at + 1);
                let mut matches = Vec::new();
                for line in pending[..complete].split(|b| *b == 0) {
                    let Some((left_out, entry)) = find_line(line) else {
                        continue;
                    };
                    if left_out {
                        skipped.insert(entry.name.clone());
                    }
                    if found + matches.len() < MAX_SEARCH_RESULTS
                        && entry.name.to_lowercase().contains(query)
                    {
                        matches.push(entry);
                    }
                }
                pending.drain(..complete);
                if !matches.is_empty() {
                    found += matches.len();
                    let _ = events.try_send(Event::SearchFound {
                        req,
                        entries: matches,
                    });
                }
                if found >= MAX_SEARCH_RESULTS || pending.len() > MAX_FIND_LINE {
                    stop(&channel).await;
                    return Some(SearchEnd {
                        limited: true,
                        skipped: skipped.into_iter().collect(),
                    });
                }
            }
            Some(russh::ChannelMsg::ExitStatus { exit_status }) => status = Some(exit_status),
            Some(russh::ChannelMsg::Failure) if !ready => return None,
            Some(russh::ChannelMsg::Close) | None => break,
            _ => {}
        }
    }
    ready.then(|| SearchEnd {
        // 1 is a folder it could not read. Anything else, or nothing: it did not get to the end.
        limited: !matches!(status, Some(0 | 1)),
        skipped: skipped.into_iter().collect(),
    })
}
async fn search_over_sftp(
    sftp: Arc<SftpSession>,
    root: &str,
    query: &str,
    scope: &SearchScope,
    req: RequestId,
    events: &async_channel::Sender<Event>,
    cancel: &CancellationToken,
) -> Result<SearchEnd> {
    let skip: HashSet<&str> = scope.skip.iter().map(String::as_str).collect();
    let mut skipped = BTreeSet::new();
    let mut folders = VecDeque::from([root.to_string()]);
    let mut reading = JoinSet::new();
    let (mut visited, mut found) = (0, 0);
    loop {
        while reading.len() < SEARCH_READS && visited < MAX_SEARCH_FOLDERS {
            let Some(folder) = folders.pop_front() else {
                break;
            };
            visited += 1;
            let sftp = sftp.clone();
            reading.spawn(async move {
                let listed = list(&sftp, &folder).await;
                (folder, listed)
            });
        }
        // Reads left behind go on to the end, so that each closes its folder on the server.
        let read = tokio::select! {
            _ = cancel.cancelled() => {
                reading.detach_all();
                return Ok(SearchEnd { limited: false, skipped: skipped.into_iter().collect() });
            }
            read = reading.join_next() => read,
        };
        let Some(read) = read else {
            return Ok(SearchEnd {
                limited: !folders.is_empty(),
                skipped: skipped.into_iter().collect(),
            });
        };
        let (folder, listed) = read.context("Read a folder")?;
        let entries = match listed {
            Ok(entries) => entries,
            Err(error) if folder == root => return Err(error),
            Err(error) => {
                tracing::debug!(%folder, %error, "Search skipped a folder");
                continue;
            }
        };
        let entries = entries
            .into_iter()
            .filter(|entry| scope.hidden || !entry.name.starts_with('.'));
        let mut matches = Vec::new();
        for entry in entries {
            if entry.kind == EntryKind::Dir {
                if skip.contains(entry.name.as_str()) {
                    skipped.insert(entry.name.clone());
                } else {
                    folders.push_back(entry.path.clone());
                }
            }
            if matches.len() < MAX_SEARCH_RESULTS - found
                && entry.name.to_lowercase().contains(query)
            {
                matches.push(entry);
            }
        }
        if !matches.is_empty() {
            found += matches.len();
            let _ = events.try_send(Event::SearchFound {
                req,
                entries: matches,
            });
        }
        if found >= MAX_SEARCH_RESULTS {
            reading.detach_all();
            return Ok(SearchEnd {
                limited: true,
                skipped: skipped.into_iter().collect(),
            });
        }
    }
}
/// The last name of a remote path, a trailing `/` aside.
pub(crate) fn file_name(path: &str) -> Option<&str> {
    path.trim_end_matches('/').rsplit('/').next()
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
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{find_line, find_literal, find_pattern, without_repeats};
    use crate::EntryKind;

    #[test]
    fn wildcards_in_a_name_are_taken_as_they_are() {
        assert_eq!(find_literal(r"a*b?[c]\d"), r"a\*b\?\[c\]\\d");
    }

    #[test]
    fn a_letter_whose_case_may_not_fold_matches_anything() {
        assert_eq!(find_pattern("report"), "*report*");
        assert_eq!(find_pattern("caffè bar"), "*caff* bar*");
        assert_eq!(find_pattern("日本語"), "*日本語*");
    }

    #[test]
    fn a_line_of_find_becomes_a_row() {
        let line = b"s\td\t4096\t1700000000.5\t755\t/srv/app/node_modules";
        let (left_out, entry) = find_line(line).unwrap();
        assert!(left_out);
        assert_eq!(entry.name, "node_modules");
        assert_eq!(entry.path, "/srv/app/node_modules");
        assert_eq!(entry.kind, EntryKind::Dir);
        assert_eq!(entry.size, 4096);
        assert_eq!(entry.permissions, Some(0o755));
        assert!(entry.modified.is_some());
    }

    #[test]
    fn a_tab_in_a_path_stays_in_the_path() {
        let (left_out, entry) = find_line(b"m\tf\t1\t1\t644\t/srv/a\tb").unwrap();
        assert!(!left_out);
        assert_eq!(entry.name, "a\tb");
    }

    #[test]
    fn what_is_no_line_of_find_is_left_out() {
        assert!(find_line(b"").is_none());
        assert!(find_line(b"Welcome to the server").is_none());
    }

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
