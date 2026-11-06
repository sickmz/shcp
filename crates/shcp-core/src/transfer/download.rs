use super::*;
pub(super) async fn download(
    sftp: &SftpSession,
    remote: &str,
    local: &Path,
    cancel: &CancellationToken,
    p: &mut Progress,
) -> Result<()> {
    let name = remote
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .context("Remote path has no file name")?;
    files::safe_name(name)?;
    let top = remote.to_string();
    let mut stack = vec![(remote.to_string(), local.join(name))];
    let mut items = vec![];
    let mut skipped = vec![];
    while let Some((remote, local)) = stack.pop() {
        let mut metadata =
            cancellable(cancel, async { Ok(sftp.symlink_metadata(&remote).await?) }).await?;
        if metadata.file_type() == FileType::Symlink {
            // A link to a file brings the file. A link to a folder comes along only when it is
            // what the user picked: further down it could lead anywhere, even back here.
            let target = cancellable(cancel, async { Ok(sftp.metadata(&remote).await?) }).await;
            match target {
                Ok(target)
                    if target.file_type() == FileType::File
                        || (target.file_type() == FileType::Dir && remote == top) =>
                {
                    metadata = target;
                }
                _ if cancel.is_cancelled() => bail!("Transfer cancelled"),
                _ => {
                    skipped.push(remote);
                    continue;
                }
            }
        }
        match metadata.file_type() {
            FileType::Dir => {
                let entries =
                    cancellable(cancel, async { Ok(sftp.read_dir(&remote).await?) }).await?;
                for entry in entries {
                    let name = entry.file_name();
                    files::safe_name(&name)?;
                    stack.push((files::join(&remote, &name), local.join(name)));
                }
            }
            FileType::File => {}
            _ => bail!("Unsupported remote file: {remote}"),
        }
        items.push(Item {
            local,
            remote,
            dir: metadata.file_type() == FileType::Dir,
            size: metadata.len(),
        });
    }
    totals(p, &items);
    p.report_skipped(&skipped);
    for item in items {
        if item.dir {
            cancellable(cancel, async {
                check_local_path(&item.local).await?;
                Ok(tokio::fs::create_dir_all(&item.local).await?)
            })
            .await?;
        } else {
            download_file(sftp, &item, cancel, p).await?;
        }
    }
    Ok(())
}
async fn check_local_path(path: &Path) -> Result<()> {
    // Refuse existing symlinks in destination ancestors as well as at the leaf.
    for ancestor in path.ancestors() {
        match tokio::fs::symlink_metadata(ancestor).await {
            Ok(meta) if meta.is_symlink() => bail!(
                "Refusing to write through local symlink: {}",
                ancestor.display()
            ),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
/// How much of the partial file can be kept.
async fn resume_from(part: &Path, input: &mut russh_sftp::client::fs::File, size: u64) -> u64 {
    let part_size = tokio::fs::metadata(part).await.map_or(0, |m| m.len());
    if part_size == 0 || part_size > size {
        return 0;
    }
    let same = async {
        let mut local = tokio::fs::File::open(part).await?;
        same_tail(input, &mut local, part_size).await
    };
    match same.await {
        Ok(true) => part_size,
        Ok(false) => 0,
        Err(error) => {
            tracing::warn!(%error, path = %part.display(), "Could not check a partial download");
            0
        }
    }
}
async fn already_there(sftp: &SftpSession, item: &Item) -> Result<bool> {
    let Ok(meta) = tokio::fs::metadata(&item.local).await else {
        return Ok(false);
    };
    if meta.len() != item.size {
        return Ok(false);
    }
    let mut remote = sftp.open(item.remote.as_str()).await?;
    let mut local = tokio::fs::File::open(&item.local).await?;
    same_tail(&mut remote, &mut local, item.size).await
}
pub(super) async fn download_file(
    sftp: &SftpSession,
    item: &Item,
    cancel: &CancellationToken,
    p: &mut Progress,
) -> Result<()> {
    let mut part = item.local.clone().into_os_string();
    part.push(PART_SUFFIX);
    let part = PathBuf::from(part);
    check_local_path(&item.local).await?;
    check_local_path(&part).await?;
    if let Some(parent) = item.local.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    if p.retry && already_there(sftp, item).await.unwrap_or(false) {
        p.skip(item.size);
        p.info.files_done += 1;
        p.emit(false);
        return Ok(());
    }
    let mut input = cancellable(cancel, async { Ok(sftp.open(&item.remote).await?) }).await?;
    let offset = resume_from(&part, &mut input, item.size).await;
    let mut output = if offset > 0 {
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .open(&part)
            .await?;
        file.seek(SeekFrom::Start(offset)).await?;
        file
    } else {
        tokio::fs::File::create(&part).await?
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
        cancellable(cancel, async { Ok(output.flush().await?) }).await?;
        input.close().await?;
        Ok::<(), anyhow::Error>(())
    }
    .await;
    // Windows cannot rename or delete a file still open.
    drop(output.into_std().await);
    if let Err(error) = result {
        if p.cancelled_by_user(cancel)
            && let Err(error) = tokio::fs::remove_file(&part).await
        {
            tracing::warn!(%error, path = %part.display(), "Could not remove partial download");
        }
        return Err(error);
    }
    tokio::fs::rename(&part, &item.local)
        .await
        .with_context(|| format!("Replace {}", item.local.display()))?;
    p.info.files_done += 1;
    p.emit(false);
    Ok(())
}
