//! Files pasted or dropped on a terminal: the program there runs on the server and cannot see this
//! PC, so the file goes to a private folder of the server and the terminal receives its path.
use crate::{PasteFile, files};
use anyhow::{Context as _, Result, bail};
use russh_sftp::{
    client::{SftpSession, fs::Metadata},
    protocol::FileType,
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::AsyncWriteExt;

const MAX_SIZE: u64 = 50 * 1024 * 1024;
const KEEP_FILES: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Uploads the file and returns its path on the server.
pub(crate) async fn upload(sftp: &SftpSession, file: PasteFile, unique: u64) -> Result<String> {
    let (name, data) = match file {
        PasteFile::Bytes { name, data } => (name, data),
        PasteFile::Local(path) => {
            let metadata = tokio::fs::metadata(&path).await?;
            if !metadata.is_file() {
                bail!("{} is not a file", path.display());
            }
            if metadata.len() > MAX_SIZE {
                bail!("{} is larger than 50 MB: use the Files tab", path.display());
            }
            let name = path
                .file_name()
                .context("The file has no name")?
                .to_string_lossy()
                .into_owned();
            (name, tokio::fs::read(&path).await?)
        }
    };
    if data.len() as u64 > MAX_SIZE {
        bail!("{name} is larger than 50 MB");
    }
    let dir = private_dir(sftp).await?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let path = files::join(&dir, &format!("{now}-{unique}-{}", safe_name(&name)));
    let mut output = sftp.create(&path).await?;
    output.write_all(&data).await?;
    output.shutdown().await?;
    drop(output);
    remove_old(sftp, &dir).await;
    Ok(path)
}

async fn private_dir(sftp: &SftpSession) -> Result<String> {
    let home = sftp.canonicalize(".").await?;
    let cache = files::join(&home, ".cache");
    let shcp = files::join(&cache, "shcp");
    let dir = files::join(&shcp, "paste");
    // Parents may already exist: only the last check tells whether all this worked.
    let _ = sftp.create_dir(cache).await;
    let _ = sftp.create_dir(shcp).await;
    if sftp.create_dir(dir.as_str()).await.is_ok() {
        let mut private = Metadata::empty();
        private.permissions = Some(0o700);
        sftp.set_metadata(dir.as_str(), private).await?;
    }
    let metadata = sftp.symlink_metadata(dir.as_str()).await?;
    if metadata.file_type() != FileType::Dir {
        bail!("{dir} is not a folder");
    }
    Ok(dir)
}

/// Names reach the command line, so only characters that mean nothing to a shell stay.
fn safe_name(name: &str) -> String {
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let safe = safe.trim_start_matches(['.', '-']);
    if safe.is_empty() {
        "file".to_string()
    } else {
        safe.to_string()
    }
}

/// Failing here must not fail the paste: the old files simply stay for now.
async fn remove_old(sftp: &SftpSession, dir: &str) {
    let Ok(entries) = sftp.read_dir(dir).await else {
        return;
    };
    for entry in entries {
        let metadata = entry.metadata();
        let old = metadata
            .modified()
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age > KEEP_FILES);
        if old && metadata.file_type() == FileType::File {
            let path = files::join(dir, &entry.file_name());
            if let Err(error) = sftp.remove_file(path).await {
                tracing::debug!(%error, "Could not remove an old pasted file");
            }
        }
    }
}
