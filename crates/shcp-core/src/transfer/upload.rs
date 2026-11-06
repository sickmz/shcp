use super::*;
use russh_sftp::{client::fs::Metadata, protocol::OpenFlags};
/// More than the SFTP client can have on its way at once (16 writes of 32 KiB).
const WRITE_WINDOW: u64 = 1024 * 1024;
pub(super) async fn upload(
    sftp: &SftpSession,
    local: &Path,
    remote: &str,
    cancel: &CancellationToken,
    p: &mut Progress,
) -> Result<()> {
    let name = local
        .file_name()
        .and_then(|s| s.to_str())
        .context("Local path has no UTF-8 file name")?;
    files::safe_name(name)?;
    let top = local.to_path_buf();
    let mut stack = vec![(local.to_path_buf(), files::join(remote, name))];
    let mut items = vec![];
    let mut skipped = vec![];
    while let Some((local, remote)) = stack.pop() {
        let mut metadata = cancellable(cancel, async {
            Ok(tokio::fs::symlink_metadata(&local).await?)
        })
        .await?;
        if metadata.is_symlink() {
            // A link to a file brings the file. A link to a folder comes along only when it is
            // what the user picked: further down it could lead anywhere, even back here.
            match tokio::fs::metadata(&local).await {
                Ok(target) if target.is_file() || (target.is_dir() && local == top) => {
                    metadata = target;
                }
                _ => {
                    skipped.push(local.display().to_string());
                    continue;
                }
            }
        }
        if metadata.is_dir() {
            let mut dir = tokio::fs::read_dir(&local).await?;
            while let Some(entry) =
                cancellable(cancel, async { Ok(dir.next_entry().await?) }).await?
            {
                let name = entry
                    .file_name()
                    .into_string()
                    .map_err(|_| anyhow::anyhow!("Local file name is not valid UTF-8"))?;
                files::safe_name(&name)?;
                stack.push((entry.path(), files::join(&remote, &name)));
            }
        } else if !metadata.is_file() {
            bail!("Unsupported local file: {}", local.display());
        }
        items.push(Item {
            local,
            remote,
            dir: metadata.is_dir(),
            size: metadata.len(),
        });
    }
    totals(p, &items);
    p.report_skipped(&skipped);
    for item in items {
        if item.dir {
            cancellable(cancel, ensure_remote_dir(sftp, &item.remote)).await?;
        } else {
            upload_file(sftp, &item, cancel, p).await?;
        }
    }
    Ok(())
}
async fn ensure_remote_dir(sftp: &SftpSession, path: &str) -> Result<()> {
    match sftp.create_dir(path).await {
        Ok(()) => Ok(()),
        Err(error) => match sftp.symlink_metadata(path).await {
            Ok(meta) if meta.file_type() == FileType::Dir => Ok(()),
            _ => Err(error).context("Create remote directory"),
        },
    }
}
/// Refuses what must not be written to. Returns the size of a regular file, if there is one.
async fn writable_size(sftp: &SftpSession, path: &str) -> Result<Option<u64>> {
    match sftp.symlink_metadata(path).await {
        Ok(meta) => match meta.file_type() {
            FileType::File => Ok(Some(meta.len())),
            FileType::Dir => bail!("A folder has the name of the file: {path}"),
            // Never write through a pre-existing remote symlink.
            _ => bail!("Refusing to overwrite remote symlink: {path}"),
        },
        Err(_) => Ok(None),
    }
}
/// How much of the partial file can be kept.
async fn resume_from(
    sftp: &SftpSession,
    part: &str,
    part_size: u64,
    input: &mut tokio::fs::File,
    size: u64,
) -> u64 {
    // Writes are pipelined: a server that refused one may have taken those sent after it, so
    // the end of the partial file can have holes. It is written again.
    let keep = part_size.saturating_sub(WRITE_WINDOW);
    if keep == 0 || part_size > size {
        return 0;
    }
    let same = async {
        let mut remote = sftp.open(part).await?;
        same_tail(&mut remote, input, keep).await
    };
    match same.await {
        Ok(true) => keep,
        Ok(false) => 0,
        Err(error) => {
            tracing::warn!(%error, part, "Could not check a partial upload");
            0
        }
    }
}
/// Puts the complete file in place. A plain SFTP rename does not replace a file, so the old one
/// steps aside first and comes back if anything fails.
async fn commit(sftp: &SftpSession, part: &str, target: &str, existing: bool) -> Result<()> {
    if !existing {
        return sftp
            .rename(part, target)
            .await
            .context("Rename the uploaded file");
    }
    let meta = sftp.symlink_metadata(target).await?;
    if let Some(mode) = meta.permissions {
        let mut same_mode = Metadata::empty();
        same_mode.permissions = Some(mode & 0o7777);
        if let Err(error) = sftp.set_metadata(part, same_mode).await {
            tracing::warn!(%error, target, "Could not keep the permissions of a replaced file");
        }
    }
    let old = format!("{target}.shcp-old");
    let _ = sftp.remove_file(old.as_str()).await;
    sftp.rename(target, old.as_str())
        .await
        .context("Move the old file aside")?;
    if let Err(error) = sftp.rename(part, target).await {
        if let Err(back) = sftp.rename(old.as_str(), target).await {
            tracing::error!(%back, target, %old, "Could not put the old file back");
        }
        return Err(error).context("Rename the uploaded file");
    }
    if let Err(error) = sftp.remove_file(old.as_str()).await {
        tracing::warn!(%error, %old, "Could not remove the replaced file");
    }
    Ok(())
}
pub(super) async fn upload_file(
    sftp: &SftpSession,
    item: &Item,
    cancel: &CancellationToken,
    p: &mut Progress,
) -> Result<()> {
    let mut input = tokio::fs::File::open(&item.local).await?;
    let existing = writable_size(sftp, &item.remote).await?;
    if p.retry && existing == Some(item.size) {
        let same = async {
            let mut remote = sftp.open(item.remote.as_str()).await?;
            same_tail(&mut remote, &mut input, item.size).await
        };
        if same.await.unwrap_or(false) {
            p.skip(item.size);
            p.info.files_done += 1;
            p.emit(false);
            return Ok(());
        }
    }
    let part = format!("{}{PART_SUFFIX}", item.remote);
    let part_size = writable_size(sftp, &part).await?.unwrap_or(0);
    let offset = resume_from(sftp, &part, part_size, &mut input, item.size).await;
    // Complete OPEN before observing cancellation so every created file can be removed.
    let mut output = if offset > 0 {
        let mut file = sftp
            .open_with_flags(part.as_str(), OpenFlags::WRITE)
            .await?;
        file.seek(SeekFrom::Start(offset)).await?;
        file
    } else {
        sftp.create(part.as_str()).await?
    };
    input.seek(SeekFrom::Start(offset)).await?;
    p.skip(offset);
    let result = async {
        let mut buffer = vec![0; 256 * 1024];
        loop {
            let n = cancellable(cancel, async { Ok(input.read(&mut buffer).await?) }).await?;
            if n == 0 {
                break;
            }
            cancellable(cancel, async { Ok(output.write_all(&buffer[..n]).await?) }).await?;
            p.info.bytes_done += n as u64;
            p.emit(false);
        }
        cancellable(cancel, async { Ok(output.shutdown().await?) }).await?;
        Ok::<(), anyhow::Error>(())
    }
    .await;
    if result.is_err() {
        let _ = output.shutdown().await;
    }
    drop(output);
    if let Err(error) = result {
        if p.cancelled_by_user(cancel)
            && let Err(error) = sftp.remove_file(part.as_str()).await
        {
            tracing::warn!(%error, %part, "Could not remove partial upload");
        }
        return Err(error);
    }
    commit(sftp, &part, &item.remote, existing.is_some()).await?;
    p.info.files_done += 1;
    p.emit(false);
    Ok(())
}
