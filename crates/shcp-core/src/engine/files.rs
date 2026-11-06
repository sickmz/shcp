use super::*;
impl EngineHandle {
    fn file(&self, session: SessionId, op: FileOp) -> RequestId {
        let req = RequestId(self.next());
        let control = self.control(session);
        let events = self.0.events.clone();
        self.0.runtime.handle.spawn(async move {
            let result = async { control.context("Session does not exist")?.files().await }.await;
            let event = match result {
                Ok(files) => crate::files::execute(&files, req, op).await,
                Err(error) => match op {
                    FileOp::Home => Event::RemotePath {
                        req,
                        path: Err(error.to_string()),
                    },
                    FileOp::List(path) => Event::DirListing {
                        req,
                        path,
                        entries: Err(error.to_string()),
                    },
                    FileOp::Kind(path) => Event::RemoteKind {
                        req,
                        path,
                        kind: Err(error.to_string()),
                    },
                    FileOp::FreeSpace(_) => Event::RemoteSpace {
                        req,
                        free: None,
                        total: None,
                    },
                    FileOp::ReadHead(path) => Event::RemoteHead {
                        req,
                        path,
                        text: Err(error.to_string()),
                    },
                    _ => Event::OpDone {
                        req,
                        result: Err(error.to_string()),
                    },
                },
            };
            let _ = events.try_send(event);
        });
        req
    }
    pub fn remote_home(&self, s: SessionId) -> RequestId {
        self.file(s, FileOp::Home)
    }
    pub fn list_dir(&self, s: SessionId, path: String) -> RequestId {
        self.file(s, FileOp::List(path))
    }
    /// Asks what a path really is; `Event::RemoteKind` tells.
    pub fn entry_kind(&self, s: SessionId, path: String) -> RequestId {
        self.file(s, FileOp::Kind(path))
    }
    /// Asks how much room is left where a path is; `Event::RemoteSpace` tells.
    pub fn free_space(&self, s: SessionId, path: String) -> RequestId {
        self.file(s, FileOp::FreeSpace(path))
    }
    /// Reads the start of a file for a preview; `Event::RemoteHead` tells.
    pub fn read_head(&self, s: SessionId, path: String) -> RequestId {
        self.file(s, FileOp::ReadHead(path))
    }
    /// How many remote files are open in a local editor, whose saves stop with the app.
    pub fn open_edits(&self) -> usize {
        self.0.edits.lock().len()
    }
    /// Sets the permission bits of these paths.
    pub fn chmod(&self, s: SessionId, paths: Vec<String>, mode: u32) -> RequestId {
        self.file(s, FileOp::Chmod(paths, mode))
    }
    /// Copies a file or folder to a full new path, on the server.
    pub fn copy(&self, s: SessionId, from: String, to: String) -> RequestId {
        self.file(s, FileOp::Copy(from, to))
    }
    /// Puts these names, relative to `folder`, in a new archive at a full path, on the server.
    pub fn compress(
        &self,
        s: SessionId,
        folder: String,
        names: Vec<String>,
        archive: String,
    ) -> RequestId {
        self.file(
            s,
            FileOp::Compress {
                folder,
                names,
                archive,
            },
        )
    }
    /// Unpacks an archive into a new folder at a full path, on the server.
    pub fn extract(&self, s: SessionId, archive: String, folder: String) -> RequestId {
        self.file(s, FileOp::Extract { archive, folder })
    }
    pub fn mkdir(&self, s: SessionId, path: String) -> RequestId {
        self.file(s, FileOp::Mkdir(path))
    }
    /// Creates an empty file; fails if something already has that path.
    pub fn create_file(&self, s: SessionId, path: String) -> RequestId {
        self.file(s, FileOp::CreateFile(path))
    }
    pub fn rename(&self, s: SessionId, from: String, to: String) -> RequestId {
        self.file(s, FileOp::Rename(from, to))
    }
    pub fn remove(&self, s: SessionId, path: String, recursive: bool) -> RequestId {
        self.file(s, FileOp::Remove(path, recursive))
    }
    /// Looks for a name in the subfolders of `root`. `Event::SearchFound` brings what it finds and
    /// `Event::SearchDone` ends it.
    pub fn search_files(&self, s: SessionId, root: String, query: String) -> RequestId {
        let req = RequestId(self.next());
        let control = self.control(s);
        let cancel = control
            .as_ref()
            .map(|c| c.context.cancel.child_token())
            .unwrap_or_default();
        self.0.searches.lock().insert(req, cancel.clone());
        let registry = self.0.searches.clone();
        let events = self.0.events.clone();
        self.0.runtime.handle.spawn(async move {
            let result = async {
                let control = control.context("Session does not exist")?;
                let files = control.files().await?;
                let sftp = files.get().await?;
                crate::files::search(&sftp, &root, &query, req, &events, &cancel).await
            }
            .await
            .map_err(|error: anyhow::Error| format!("{error:#}"));
            registry.lock().remove(&req);
            let _ = events.try_send(Event::SearchDone { req, result });
        });
        req
    }
    pub fn cancel_search(&self, req: RequestId) {
        if let Some(cancel) = self.0.searches.lock().get(&req) {
            cancel.cancel();
        }
    }
    /// Lets a key log in to the server, creating it if `key` is no file yet. `Event::OpDone` tells
    /// how it went.
    pub fn install_key(&self, s: SessionId, key: PathBuf, passphrase: Option<String>) -> RequestId {
        let req = RequestId(self.next());
        let control = self.control(s);
        let events = self.0.events.clone();
        self.0.runtime.handle.spawn(async move {
            let result = async {
                let control = control.context("Session does not exist")?;
                let files = control.files().await?;
                let comment = format!(
                    "{}@{}",
                    std::env::var("USERNAME").unwrap_or_else(|_| "shcp".into()),
                    std::env::var("COMPUTERNAME").unwrap_or_else(|_| "shcp".into()),
                );
                let line = tokio::task::spawn_blocking(move || {
                    let passphrase = passphrase.map(Zeroizing::new);
                    crate::key_setup::public_line(
                        &key,
                        passphrase.as_ref().map(|secret| secret.as_str()),
                        &comment,
                    )
                })
                .await??;
                crate::key_setup::install(files.ssh(), &line).await
            }
            .await
            .map_err(|error: anyhow::Error| format!("{error:#}"));
            let _ = events.try_send(Event::OpDone { req, result });
        });
        req
    }
    /// Puts a file in a private folder of the server; `Event::PastedFile` tells its path.
    pub fn paste_file(&self, s: SessionId, file: PasteFile) -> RequestId {
        let req = RequestId(self.next());
        let control = self.control(s);
        let events = self.0.events.clone();
        self.0.runtime.handle.spawn(async move {
            let result = async {
                let control = control.context("Session does not exist")?;
                let files = control.files().await?;
                let sftp = files.get().await?;
                crate::paste::upload(&sftp, file, req.0).await
            }
            .await
            .map_err(|error| format!("{error:#}"));
            let _ = events.try_send(Event::PastedFile { req, result });
        });
        req
    }
    /// Opens a remote file in the local editor and writes every save back to the server.
    pub fn edit_file(&self, s: SessionId, remote: String) {
        let editor = self.settings().editor;
        let key = (s, remote.clone());
        let mut edits = self.0.edits.lock();
        if let Some(commands) = edits.get(&key)
            && commands
                .try_send(EditCommand::Reopen(editor.clone()))
                .is_ok()
        {
            return;
        }
        let Some(control) = self.control(s) else {
            return;
        };
        let (commands, receiver) = async_channel::unbounded();
        edits.insert(key.clone(), commands);
        // Session ids restart with the app: the time keeps an old copy from being overwritten.
        let started = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs());
        let dir = crate::edit::root(&self.0.store.lock().paths.data_dir)
            .join(format!("{started}-{}", self.next()));
        let registry = self.0.edits.clone();
        self.0.runtime.handle.spawn(async move {
            crate::edit::run(control, remote, dir, editor, receiver).await;
            registry.lock().remove(&key);
        });
    }
    /// Replaces the server version of a file being edited that changed on the server.
    pub fn overwrite_edit(&self, s: SessionId, remote: String) {
        if let Some(commands) = self.0.edits.lock().get(&(s, remote)) {
            let _ = commands.try_send(EditCommand::Overwrite);
        }
    }
    fn transfer(
        &self,
        session: SessionId,
        local: PathBuf,
        remote: String,
        direction: Direction,
        retry: bool,
    ) -> TransferId {
        let id = TransferId(self.next());
        let control = self.control(session);
        let host = control
            .as_ref()
            .map(|c| c.context.host.id.clone())
            .unwrap_or_default();
        let cancel = control
            .as_ref()
            .map(|c| c.context.cancel.child_token())
            .unwrap_or_default();
        let label = match direction {
            Direction::Upload => local
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| local.display().to_string()),
            Direction::Download => remote
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .unwrap_or(&remote)
                .into(),
        };
        let info = TransferInfo {
            id,
            session,
            host,
            direction,
            local: local.clone(),
            remote: remote.clone(),
            label,
            bytes_done: 0,
            bytes_total: None,
            files_done: 0,
            files_total: None,
            bytes_per_sec: 0,
            state: TransferState::Queued,
        };
        self.emit(Event::Transfer(info.clone()));
        self.0.transfers.lock().insert(id, cancel.clone());
        let registry = self.0.transfers.clone();
        let events = self.0.events.clone();
        self.0.runtime.handle.spawn(async move {
            transfer::run(
                Job {
                    info,
                    local,
                    remote,
                    cancel,
                    retry,
                },
                control,
                events,
            )
            .await;
            registry.lock().remove(&id);
        });
        id
    }
    pub fn upload(&self, s: SessionId, local: PathBuf, remote_dir: String) -> TransferId {
        self.transfer(s, local, remote_dir, Direction::Upload, false)
    }
    pub fn download(&self, s: SessionId, remote: String, local_dir: PathBuf) -> TransferId {
        self.transfer(s, local_dir, remote, Direction::Download, false)
    }
    /// Starts a failed transfer again: it continues from what already arrived.
    pub fn retry_transfer(&self, s: SessionId, info: &TransferInfo) -> TransferId {
        self.transfer(
            s,
            info.local.clone(),
            info.remote.clone(),
            info.direction,
            true,
        )
    }
    pub fn cancel_transfer(&self, t: TransferId) {
        if let Some(cancel) = self.0.transfers.lock().get(&t) {
            cancel.cancel();
        }
    }
}
