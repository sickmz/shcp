//! Runs transfers against an in-memory SFTP server that behaves like OpenSSH where it matters:
//! a rename does not replace an existing file.
use super::*;
use crate::{HostId, SessionId, TransferId, test_support::Scratch};
use parking_lot::Mutex;
use russh_sftp::protocol::{
    Attrs, Data, File, FileAttributes, Handle, Name, OpenFlags, Status, StatusCode, Version,
};
use std::collections::HashMap;

#[derive(Default)]
struct Disk {
    files: HashMap<String, Vec<u8>>,
    /// Link and where it points. A target that is not there makes it broken.
    links: HashMap<String, String>,
    /// Folders and their contents, by name.
    dirs: HashMap<String, Vec<String>>,
    /// Handles of folders being listed, which give their contents once.
    listings: HashMap<String, Vec<String>>,
    modes: HashMap<String, u32>,
    handles: HashMap<String, String>,
    next: u64,
    bytes_written: u64,
    bytes_read: u64,
    /// Writes and reads fail once this many bytes have moved.
    fail_after: Option<u64>,
}
#[derive(Clone, Default)]
struct Server(Arc<Mutex<Disk>>);

fn ok(id: u32) -> Status {
    Status {
        id,
        status_code: StatusCode::Ok,
        error_message: String::new(),
        language_tag: String::new(),
    }
}

fn attrs(data: &[u8], mode: Option<u32>) -> FileAttributes {
    let mut attrs = FileAttributes::empty();
    attrs.size = Some(data.len() as u64);
    attrs.permissions = Some(0o100000 | mode.unwrap_or(0o644));
    attrs
}

impl russh_sftp::server::Handler for Server {
    type Error = StatusCode;
    fn unimplemented(&self) -> StatusCode {
        StatusCode::OpUnsupported
    }
    async fn init(&mut self, _: u32, _: HashMap<String, String>) -> Result<Version, StatusCode> {
        Ok(Version::new())
    }
    async fn open(
        &mut self,
        id: u32,
        path: String,
        flags: OpenFlags,
        _: FileAttributes,
    ) -> Result<Handle, StatusCode> {
        let mut disk = self.0.lock();
        let path = disk.links.get(&path).cloned().unwrap_or(path);
        if !disk.files.contains_key(&path) && !flags.contains(OpenFlags::CREATE) {
            return Err(StatusCode::NoSuchFile);
        }
        let file = disk.files.entry(path.clone()).or_default();
        if flags.contains(OpenFlags::TRUNCATE) {
            file.clear();
        }
        disk.next += 1;
        let handle = disk.next.to_string();
        disk.handles.insert(handle.clone(), path);
        Ok(Handle { id, handle })
    }
    async fn close(&mut self, id: u32, handle: String) -> Result<Status, StatusCode> {
        self.0.lock().handles.remove(&handle);
        Ok(ok(id))
    }
    async fn read(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        len: u32,
    ) -> Result<Data, StatusCode> {
        let mut disk = self.0.lock();
        if disk
            .fail_after
            .is_some_and(|limit| disk.bytes_read >= limit)
        {
            return Err(StatusCode::Failure);
        }
        let path = disk.handles.get(&handle).ok_or(StatusCode::Failure)?;
        let file = disk.files.get(path).ok_or(StatusCode::NoSuchFile)?;
        let start = offset as usize;
        if start >= file.len() {
            return Err(StatusCode::Eof);
        }
        let end = (start + len as usize).min(file.len());
        let data = file[start..end].to_vec();
        disk.bytes_read += data.len() as u64;
        Ok(Data { id, data })
    }
    async fn write(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        data: Vec<u8>,
    ) -> Result<Status, StatusCode> {
        let mut disk = self.0.lock();
        if disk
            .fail_after
            .is_some_and(|limit| disk.bytes_written >= limit)
        {
            return Err(StatusCode::Failure);
        }
        disk.bytes_written += data.len() as u64;
        let path = disk
            .handles
            .get(&handle)
            .cloned()
            .ok_or(StatusCode::Failure)?;
        let file = disk.files.get_mut(&path).ok_or(StatusCode::NoSuchFile)?;
        let end = offset as usize + data.len();
        if file.len() < end {
            file.resize(end, 0);
        }
        file[offset as usize..end].copy_from_slice(&data);
        Ok(ok(id))
    }
    async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, StatusCode> {
        let disk = self.0.lock();
        if disk.links.contains_key(&path) {
            let mut attrs = FileAttributes::empty();
            attrs.size = Some(0);
            attrs.permissions = Some(0o120777);
            return Ok(Attrs { id, attrs });
        }
        Self::attrs_of(&disk, id, &path)
    }
    async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, StatusCode> {
        let disk = self.0.lock();
        let target = disk.links.get(&path).unwrap_or(&path);
        Self::attrs_of(&disk, id, target)
    }
    async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, StatusCode> {
        let mut disk = self.0.lock();
        let target = disk
            .links
            .get(&path)
            .cloned()
            .unwrap_or_else(|| path.clone());
        let names = disk
            .dirs
            .get(&target)
            .cloned()
            .ok_or(StatusCode::NoSuchFile)?;
        disk.next += 1;
        let handle = disk.next.to_string();
        disk.listings.insert(handle.clone(), names);
        Ok(Handle { id, handle })
    }
    async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, StatusCode> {
        let names = self
            .0
            .lock()
            .listings
            .remove(&handle)
            .ok_or(StatusCode::Eof)?;
        let files = names
            .into_iter()
            .map(|filename| File {
                longname: filename.clone(),
                filename,
                attrs: FileAttributes::empty(),
            })
            .collect();
        Ok(Name { id, files })
    }
    async fn fstat(&mut self, id: u32, handle: String) -> Result<Attrs, StatusCode> {
        let path = self.0.lock().handles.get(&handle).cloned();
        self.lstat(id, path.ok_or(StatusCode::Failure)?).await
    }
    async fn setstat(
        &mut self,
        id: u32,
        path: String,
        attrs: FileAttributes,
    ) -> Result<Status, StatusCode> {
        if let Some(mode) = attrs.permissions {
            self.0.lock().modes.insert(path, mode);
        }
        Ok(ok(id))
    }
    async fn remove(&mut self, id: u32, path: String) -> Result<Status, StatusCode> {
        let mut disk = self.0.lock();
        disk.modes.remove(&path);
        match disk.files.remove(&path) {
            Some(_) => Ok(ok(id)),
            None => Err(StatusCode::NoSuchFile),
        }
    }
    async fn rename(&mut self, id: u32, from: String, to: String) -> Result<Status, StatusCode> {
        let mut disk = self.0.lock();
        if disk.files.contains_key(&to) {
            return Err(StatusCode::Failure);
        }
        let file = disk.files.remove(&from).ok_or(StatusCode::NoSuchFile)?;
        disk.files.insert(to.clone(), file);
        if let Some(mode) = disk.modes.remove(&from) {
            disk.modes.insert(to, mode);
        }
        Ok(ok(id))
    }
}

impl Server {
    fn attrs_of(disk: &Disk, id: u32, path: &str) -> Result<Attrs, StatusCode> {
        if disk.dirs.contains_key(path) {
            let mut attrs = FileAttributes::empty();
            attrs.size = Some(0);
            attrs.permissions = Some(0o040755);
            return Ok(Attrs { id, attrs });
        }
        let file = disk.files.get(path).ok_or(StatusCode::NoSuchFile)?;
        Ok(Attrs {
            id,
            attrs: attrs(file, disk.modes.get(path).copied()),
        })
    }
}

async fn connect(server: &Server) -> SftpSession {
    let (client, stream) = tokio::io::duplex(1 << 20);
    russh_sftp::server::run(stream, server.clone()).await;
    SftpSession::new(client).await.unwrap()
}

const SIZE: usize = 3_000_000;

/// Bigger than the copy buffer and the write window, and different everywhere.
fn content(seed: u8) -> Vec<u8> {
    (0..SIZE as u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8 ^ seed)
        .collect()
}

/// What is still on its way to the server reaches it before the test goes on.
async fn settle() {
    tokio::time::sleep(Duration::from_millis(100)).await;
}

fn progress(direction: Direction, retry: bool, session: CancellationToken) -> Progress {
    let (events, receiver) = async_channel::unbounded();
    // Nobody listens: sending must keep working.
    std::mem::forget(receiver);
    Progress {
        info: TransferInfo {
            id: TransferId(1),
            session: SessionId(1),
            host: HostId::default(),
            direction,
            local: PathBuf::new(),
            remote: String::new(),
            label: String::new(),
            bytes_done: 0,
            bytes_total: None,
            files_done: 0,
            files_total: None,
            bytes_per_sec: 0,
            state: TransferState::Running,
        },
        events,
        start: Instant::now(),
        last: Instant::now(),
        skipped: 0,
        retry,
        session,
    }
}

async fn send(server: &Server, local: &Path, retry: bool) -> Result<Progress> {
    let sftp = connect(server).await;
    let session = CancellationToken::new();
    let cancel = session.child_token();
    let mut p = progress(Direction::Upload, retry, session);
    upload(&sftp, local, "/srv", &cancel, &mut p).await?;
    Ok(p)
}

async fn fetch(server: &Server, local_dir: &Path, retry: bool) -> Result<Progress> {
    let sftp = connect(server).await;
    let session = CancellationToken::new();
    let cancel = session.child_token();
    let mut p = progress(Direction::Download, retry, session);
    download(&sftp, "/srv/f", local_dir, &cancel, &mut p).await?;
    Ok(p)
}

fn remote(server: &Server, path: &str) -> Option<Vec<u8>> {
    server.0.lock().files.get(path).cloned()
}

fn put(server: &Server, path: &str, data: &[u8]) {
    server.0.lock().files.insert(path.into(), data.to_vec());
}

/// A folder with a file, a link to a file, a link to a folder and a link to nothing.
fn folder_with_links(server: &Server) {
    let mut disk = server.0.lock();
    disk.files.insert("/srv/f/plain".into(), b"plain".to_vec());
    disk.files
        .insert("/elsewhere/target".into(), b"linked".to_vec());
    disk.dirs.insert("/elsewhere/dir".into(), vec![]);
    disk.links
        .insert("/srv/f/to-file".into(), "/elsewhere/target".into());
    disk.links
        .insert("/srv/f/to-dir".into(), "/elsewhere/dir".into());
    disk.links.insert("/srv/f/broken".into(), "/nowhere".into());
    disk.dirs.insert(
        "/srv/f".into(),
        ["plain", "to-file", "to-dir", "broken"]
            .map(String::from)
            .to_vec(),
    );
}

#[tokio::test]
async fn download_of_a_folder_copes_with_links() {
    let (server, dir) = (Server::default(), Scratch::new("transfer-test"));
    folder_with_links(&server);
    fetch(&server, &dir.0, false).await.unwrap();
    let local = dir.0.join("f");
    assert_eq!(std::fs::read(local.join("plain")).unwrap(), b"plain");
    assert_eq!(std::fs::read(local.join("to-file")).unwrap(), b"linked");
    assert!(!local.join("to-dir").exists());
    assert!(!local.join("broken").exists());
}

#[tokio::test]
async fn download_says_which_links_stayed_behind() {
    let (server, dir) = (Server::default(), Scratch::new("transfer-test"));
    folder_with_links(&server);
    let sftp = connect(&server).await;
    let session = CancellationToken::new();
    let cancel = session.child_token();
    let (events, receiver) = async_channel::unbounded();
    let mut p = progress(Direction::Download, false, session);
    p.events = events;
    download(&sftp, "/srv/f", &dir.0, &cancel, &mut p)
        .await
        .unwrap();
    let warning = std::iter::from_fn(|| receiver.try_recv().ok()).find_map(|event| match event {
        Event::Notice { message, .. } => Some(message),
        _ => None,
    });
    assert!(warning.unwrap().contains("and 1 more"));
}

#[tokio::test]
async fn download_of_a_picked_link_to_a_folder_follows_it() {
    let (server, dir) = (Server::default(), Scratch::new("transfer-test"));
    {
        let mut disk = server.0.lock();
        disk.files.insert("/real/inside".into(), b"inside".to_vec());
        disk.dirs.insert("/real".into(), vec!["inside".into()]);
        disk.links.insert("/srv/f".into(), "/real".into());
    }
    // The listing names files by the path asked for, like a server does through a link.
    server
        .0
        .lock()
        .links
        .insert("/srv/f/inside".into(), "/real/inside".into());
    fetch(&server, &dir.0, false).await.unwrap();
    assert_eq!(
        std::fs::read(dir.0.join("f").join("inside")).unwrap(),
        b"inside"
    );
}

#[tokio::test]
async fn upload_creates_the_file_and_leaves_nothing_else() {
    let (server, dir) = (Server::default(), Scratch::new("transfer-test"));
    let data = content(1);
    send(&server, &dir.file("f", &data), false).await.unwrap();
    assert!(remote(&server, "/srv/f") == Some(data));
    assert_eq!(server.0.lock().files.len(), 1);
}

#[tokio::test]
async fn upload_replaces_a_file_and_keeps_its_permissions() {
    let (server, dir) = (Server::default(), Scratch::new("transfer-test"));
    put(&server, "/srv/f", b"old");
    server.0.lock().modes.insert("/srv/f".into(), 0o750);
    let data = content(1);
    send(&server, &dir.file("f", &data), false).await.unwrap();
    assert!(remote(&server, "/srv/f") == Some(data));
    assert_eq!(server.0.lock().files.len(), 1);
    assert_eq!(server.0.lock().modes["/srv/f"] & 0o7777, 0o750);
}

#[tokio::test]
async fn a_failed_upload_keeps_the_old_file_and_the_next_one_continues() {
    let (server, dir) = (Server::default(), Scratch::new("transfer-test"));
    put(&server, "/srv/f", b"old");
    let data = content(1);
    let local = dir.file("f", &data);
    server.0.lock().fail_after = Some(2_000_000);
    assert!(send(&server, &local, false).await.is_err());
    settle().await;
    assert_eq!(remote(&server, "/srv/f").unwrap(), b"old");
    let kept = remote(&server, "/srv/f.shcp-part").unwrap().len() as u64;
    assert!(kept >= 2_000_000 && kept < data.len() as u64);

    {
        let mut disk = server.0.lock();
        disk.fail_after = None;
        disk.bytes_written = 0;
    }
    let p = send(&server, &local, true).await.unwrap();
    assert!(remote(&server, "/srv/f") == Some(data.clone()));
    assert!(remote(&server, "/srv/f.shcp-part").is_none());
    assert_eq!(p.info.bytes_done, data.len() as u64);
    assert_eq!(p.skipped, kept - 1024 * 1024);
    assert_eq!(server.0.lock().bytes_written, data.len() as u64 - p.skipped);
}

#[tokio::test]
async fn holes_at_the_end_of_a_partial_upload_do_not_reach_the_file() {
    let (server, dir) = (Server::default(), Scratch::new("transfer-test"));
    let data = content(1);
    let mut part = data[..2_500_000].to_vec();
    // A write the server refused, with later ones taken.
    part[2_000_000..2_100_000].fill(0);
    put(&server, "/srv/f.shcp-part", &part);
    let p = send(&server, &dir.file("f", &data), true).await.unwrap();
    assert!(p.skipped > 0);
    assert!(remote(&server, "/srv/f") == Some(data));
}

#[tokio::test]
async fn a_partial_upload_of_another_file_is_not_continued() {
    let (server, dir) = (Server::default(), Scratch::new("transfer-test"));
    let data = content(1);
    put(&server, "/srv/f.shcp-part", &content(2)[..2_500_000]);
    let p = send(&server, &dir.file("f", &data), false).await.unwrap();
    assert!(remote(&server, "/srv/f") == Some(data));
    assert_eq!(p.skipped, 0);
}

#[tokio::test]
async fn cancelling_an_upload_removes_the_partial_and_keeps_the_old_file() {
    let (server, dir) = (Server::default(), Scratch::new("transfer-test"));
    put(&server, "/srv/f", b"old");
    let local = dir.file("f", content(1));
    let sftp = connect(&server).await;
    let session = CancellationToken::new();
    let cancel = session.child_token();
    let mut p = progress(Direction::Upload, false, session);
    let item = Item {
        local,
        remote: "/srv/f".into(),
        dir: false,
        size: SIZE as u64,
    };
    cancel.cancel();
    assert!(
        upload::upload_file(&sftp, &item, &cancel, &mut p)
            .await
            .is_err()
    );
    assert_eq!(remote(&server, "/srv/f").unwrap(), b"old");
    assert!(remote(&server, "/srv/f.shcp-part").is_none());
}

#[tokio::test]
async fn a_lost_connection_keeps_the_partial_upload() {
    let (server, dir) = (Server::default(), Scratch::new("transfer-test"));
    put(&server, "/srv/f", b"old");
    let local = dir.file("f", content(1));
    let sftp = connect(&server).await;
    let session = CancellationToken::new();
    let cancel = session.child_token();
    let mut p = progress(Direction::Upload, false, session.clone());
    let item = Item {
        local,
        remote: "/srv/f".into(),
        dir: false,
        size: SIZE as u64,
    };
    session.cancel();
    assert!(
        upload::upload_file(&sftp, &item, &cancel, &mut p)
            .await
            .is_err()
    );
    assert!(!p.cancelled_by_user(&cancel));
    assert_eq!(remote(&server, "/srv/f").unwrap(), b"old");
    assert!(remote(&server, "/srv/f.shcp-part").is_some());
}

#[tokio::test]
async fn a_retried_upload_skips_what_already_matches_but_not_what_differs() {
    let (server, dir) = (Server::default(), Scratch::new("transfer-test"));
    let data = content(1);
    let local = dir.file("f", &data);
    put(&server, "/srv/f", &data);
    let p = send(&server, &local, true).await.unwrap();
    assert_eq!(server.0.lock().bytes_written, 0);
    assert_eq!(p.info.files_done, 1);

    // Same size, other content.
    put(&server, "/srv/f", &content(2));
    send(&server, &local, true).await.unwrap();
    assert!(remote(&server, "/srv/f") == Some(data));
}

#[tokio::test]
async fn a_new_upload_never_skips_a_file_of_the_same_size() {
    let (server, dir) = (Server::default(), Scratch::new("transfer-test"));
    let data = content(1);
    put(&server, "/srv/f", &data);
    send(&server, &dir.file("f", &data), false).await.unwrap();
    assert_eq!(server.0.lock().bytes_written, data.len() as u64);
}

#[tokio::test]
async fn download_creates_the_file_and_leaves_nothing_else() {
    let (server, dir) = (Server::default(), Scratch::new("transfer-test"));
    let data = content(1);
    put(&server, "/srv/f", &data);
    fetch(&server, &dir.0, false).await.unwrap();
    assert!(std::fs::read(dir.0.join("f")).unwrap() == data);
    assert!(!dir.0.join("f.shcp-part").exists());
}

#[tokio::test]
async fn download_replaces_a_local_file() {
    let (server, dir) = (Server::default(), Scratch::new("transfer-test"));
    let data = content(1);
    put(&server, "/srv/f", &data);
    dir.file("f", b"old");
    fetch(&server, &dir.0, false).await.unwrap();
    assert!(std::fs::read(dir.0.join("f")).unwrap() == data);
}

#[tokio::test]
async fn a_failed_download_keeps_the_old_file_and_the_next_one_continues() {
    let (server, dir) = (Server::default(), Scratch::new("transfer-test"));
    let data = content(1);
    put(&server, "/srv/f", &data);
    dir.file("f", b"old");
    server.0.lock().fail_after = Some(2_000_000);
    assert!(fetch(&server, &dir.0, false).await.is_err());
    assert_eq!(std::fs::read(dir.0.join("f")).unwrap(), b"old");
    let kept = std::fs::metadata(dir.0.join("f.shcp-part")).unwrap().len();
    assert!(kept > 0 && kept < data.len() as u64);

    server.0.lock().fail_after = None;
    let p = fetch(&server, &dir.0, true).await.unwrap();
    assert!(std::fs::read(dir.0.join("f")).unwrap() == data);
    assert!(!dir.0.join("f.shcp-part").exists());
    assert_eq!(p.skipped, kept);
    assert_eq!(p.info.bytes_done, data.len() as u64);
}

#[tokio::test]
async fn a_partial_download_of_another_file_is_not_continued() {
    let (server, dir) = (Server::default(), Scratch::new("transfer-test"));
    let data = content(1);
    put(&server, "/srv/f", &data);
    dir.file("f.shcp-part", &content(2)[..300_000]);
    let p = fetch(&server, &dir.0, false).await.unwrap();
    assert!(std::fs::read(dir.0.join("f")).unwrap() == data);
    assert_eq!(p.skipped, 0);
}

#[tokio::test]
async fn cancelling_a_download_removes_the_partial_and_keeps_the_old_file() {
    let (server, dir) = (Server::default(), Scratch::new("transfer-test"));
    put(&server, "/srv/f", &content(1));
    dir.file("f", b"old");
    let sftp = connect(&server).await;
    let session = CancellationToken::new();
    let cancel = session.child_token();
    let mut p = progress(Direction::Download, false, session);
    let item = Item {
        local: dir.0.join("f"),
        remote: "/srv/f".into(),
        dir: false,
        size: SIZE as u64,
    };
    cancel.cancel();
    assert!(
        download::download_file(&sftp, &item, &cancel, &mut p)
            .await
            .is_err()
    );
    assert_eq!(std::fs::read(dir.0.join("f")).unwrap(), b"old");
    assert!(!dir.0.join("f.shcp-part").exists());
}

#[tokio::test]
async fn a_retried_download_skips_a_matching_file() {
    let (server, dir) = (Server::default(), Scratch::new("transfer-test"));
    let data = content(1);
    put(&server, "/srv/f", &data);
    dir.file("f", &data);
    let p = fetch(&server, &dir.0, true).await.unwrap();
    assert_eq!(p.skipped, data.len() as u64);
    assert_eq!(p.info.files_done, 1);
}
