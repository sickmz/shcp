use anyhow::Result;
use std::{
    io::Write as _,
    path::{Path, PathBuf},
    sync::OnceLock,
    time::{SystemTime, UNIX_EPOCH},
};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;
static LOGGING: OnceLock<WorkerGuard> = OnceLock::new();
/// A crash log beyond this starts over: the last ones are what is looked for.
const MAX_CRASH_LOG: u64 = 1024 * 1024;
pub const CRASH_LOG: &str = "crash.log";

/// Writes every panic to a file of its own, at once: a daily log may not get to flush.
fn record_panics(dir: PathBuf) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let text = format!(
            "{} thread {}: {info}\n{}\n\n",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_secs()),
            thread.name().unwrap_or("?"),
            std::backtrace::Backtrace::force_capture()
        );
        tracing::error!("{text}");
        let path = dir.join(CRASH_LOG);
        let start_over = std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_CRASH_LOG);
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(!start_over)
            .write(start_over)
            .truncate(start_over)
            .open(&path);
        if let Ok(mut file) = file {
            let _ = file.write_all(text.as_bytes());
        }
        previous(info);
    }));
}
pub(crate) fn init(path: &Path) -> Result<()> {
    if LOGGING.get().is_some() {
        return Ok(());
    }
    std::fs::create_dir_all(path)?;
    record_panics(path.to_path_buf());
    let appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("shcp")
        .filename_suffix("log")
        .max_log_files(14)
        .build(path)?;
    let (writer, guard) = tracing_appender::non_blocking(appender);
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(writer)
        .finish();
    // A host application may already have installed a subscriber.
    if tracing::subscriber::set_global_default(subscriber).is_ok() {
        let _ = LOGGING.set(guard);
    }
    Ok(())
}
