//! File transfers. Data goes to a `.shcp-part` file next to the destination, which takes the
//! destination's place only when complete: a transfer that stops midway leaves the existing file
//! untouched, and the next one continues from what arrived.
mod download;
#[cfg(test)]
mod tests;
mod upload;
use crate::{Direction, Event, NoticeLevel, TransferInfo, TransferState, files, session::Control};
use anyhow::{Context, Result, bail};
use download::download;
use russh_sftp::{client::SftpSession, protocol::FileType};
use std::{
    io::SeekFrom,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
    sync::Semaphore,
};
use tokio_util::sync::CancellationToken;
use upload::upload;

const PART_SUFFIX: &str = ".shcp-part";
/// How much of the end of a partial file must match the source to continue from it.
const TAIL_CHECK: u64 = 256 * 1024;

pub(crate) struct Job {
    pub info: TransferInfo,
    pub cancel: CancellationToken,
    /// A second attempt: destinations that already match the source are skipped.
    pub retry: bool,
}
struct Progress {
    info: TransferInfo,
    events: async_channel::Sender<Event>,
    start: Instant,
    last: Instant,
    /// Bytes that were already there, which must not count for the speed.
    skipped: u64,
    retry: bool,
    /// Cancelled when the connection ends; the transfer's own token is cancelled by that too.
    session: CancellationToken,
}
impl Progress {
    fn emit(&mut self, force: bool) {
        if !force && self.last.elapsed() < Duration::from_millis(100) {
            return;
        }
        let sent = self.info.bytes_done.saturating_sub(self.skipped);
        self.info.bytes_per_sec =
            (sent as f64 / self.start.elapsed().as_secs_f64().max(0.001)) as u64;
        let _ = self.events.try_send(Event::Transfer(self.info.clone()));
        self.last = Instant::now();
    }
    /// Says which links stayed behind: a link to a folder could lead anywhere, even back here.
    fn report_skipped(&mut self, skipped: &[String]) {
        let Some(first) = skipped.first() else {
            return;
        };
        let others = match skipped.len() {
            1 => String::new(),
            count => format!(" and {} more", count - 1),
        };
        let _ = self.events.try_send(Event::Notice {
            level: NoticeLevel::Important,
            message: format!(
                "{}: skipped {first}{others}, a symbolic link to a folder or to nothing",
                self.info.label
            ),
        });
    }
    /// Counts bytes that need no sending.
    fn skip(&mut self, bytes: u64) {
        self.info.bytes_done += bytes;
        self.skipped += bytes;
    }
    /// Counts a file that was already there in full.
    fn file_skipped(&mut self, size: u64) {
        self.skip(size);
        self.file_done();
    }
    fn file_done(&mut self) {
        self.info.files_done += 1;
        self.emit(false);
    }
    /// The user pressed cancel: nobody wants the partial file. After a lost connection it stays.
    fn cancelled_by_user(&self, cancel: &CancellationToken) -> bool {
        cancel.is_cancelled() && !self.session.is_cancelled()
    }
}

pub(crate) async fn run(
    job: Job,
    control: Option<Arc<Control>>,
    events: async_channel::Sender<Event>,
) {
    let session = control
        .as_ref()
        .map(|c| c.context.cancel.clone())
        .unwrap_or_default();
    // Progress takes the info, which it updates while these are read.
    let (local, remote) = (job.info.local.clone(), job.info.remote.clone());
    let mut progress = Progress {
        info: job.info,
        events,
        start: Instant::now(),
        last: Instant::now(),
        skipped: 0,
        retry: job.retry,
        session,
    };
    let result = queued(&local, &remote, &job.cancel, control, &mut progress).await;
    progress.info.state = if progress.cancelled_by_user(&job.cancel) {
        TransferState::Cancelled
    } else if progress.session.is_cancelled() {
        TransferState::Failed("Connection closed".into())
    } else {
        match result {
            Ok(()) => TransferState::Done,
            Err(e) => TransferState::Failed(format!("{e:#}")),
        }
    };
    progress.emit(true);
}
async fn queued(
    local: &Path,
    remote: &str,
    cancel: &CancellationToken,
    control: Option<Arc<Control>>,
    progress: &mut Progress,
) -> Result<()> {
    let control = control.context("Session does not exist")?;
    let files = cancellable(cancel, control.files()).await?;
    let semaphore: &Semaphore = &control.transfers;
    let _permit = cancellable(cancel, async { Ok(semaphore.acquire().await?) }).await?;
    progress.info.state = TransferState::Running;
    progress.start = Instant::now();
    progress.emit(true);
    let sftp = cancellable(cancel, files.get()).await?;
    match progress.info.direction {
        Direction::Upload => upload(&sftp, local, remote, cancel, progress).await,
        Direction::Download => download(&sftp, remote, local, cancel, progress).await,
    }
}
async fn cancellable<T>(
    cancel: &CancellationToken,
    future: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    tokio::select! { biased; _ = cancel.cancelled() => bail!("Transfer cancelled"), result = future => result }
}
/// Copies the rest of `input` to `output`, counting it as it goes. Finishing `output` is up to
/// the caller.
async fn pump<R, W>(
    input: &mut R,
    output: &mut W,
    cancel: &CancellationToken,
    p: &mut Progress,
) -> Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut buffer = vec![0; 256 * 1024];
    loop {
        let n = cancellable(cancel, async { Ok(input.read(&mut buffer).await?) }).await?;
        if n == 0 {
            return Ok(());
        }
        cancellable(cancel, async { Ok(output.write_all(&buffer[..n]).await?) }).await?;
        p.info.bytes_done += n as u64;
        p.emit(false);
    }
}
struct Item {
    local: PathBuf,
    remote: String,
    dir: bool,
    size: u64,
}
fn totals(p: &mut Progress, items: &[Item]) {
    p.info.bytes_total = items
        .iter()
        .filter(|i| !i.dir)
        .try_fold(0u64, |total, item| total.checked_add(item.size));
    p.info.files_total = u32::try_from(items.iter().filter(|i| !i.dir).count()).ok();
    p.emit(false);
}
/// Whether the last bytes before `len` are the same in both files. A copy never written to in
/// between matches here; one of another version of the file almost certainly does not.
async fn same_tail<A, B>(a: &mut A, b: &mut B, len: u64) -> Result<bool>
where
    A: tokio::io::AsyncRead + tokio::io::AsyncSeek + Unpin,
    B: tokio::io::AsyncRead + tokio::io::AsyncSeek + Unpin,
{
    let count = len.min(TAIL_CHECK);
    let from = SeekFrom::Start(len - count);
    a.seek(from).await?;
    b.seek(from).await?;
    let (mut left, mut right) = (vec![0; count as usize], vec![0; count as usize]);
    a.read_exact(&mut left).await?;
    b.read_exact(&mut right).await?;
    Ok(left == right)
}
