//! Keeps the exe up to date from the GitHub releases. Windows lets a running exe be renamed, so
//! the new one takes its place at once and starts the next time the app does. The app never
//! restarts by itself: that would close the user's connections.
use crate::net::{self, Service};
use crate::state::{AppState, NoticeAction};
use anyhow::{Context as _, Result, bail, ensure};
use async_channel::Sender;
use gpui::{App, Context};
use serde_json::Value;
use shcp_core::NoticeLevel;
use std::{
    ffi::OsString,
    io::Read as _,
    path::{Path, PathBuf},
    rc::Rc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const LATEST_RELEASE: &str = "https://api.github.com/repos/sickmz/shcp/releases/latest";
const RELEASES_PAGE: &str = "https://github.com/sickmz/shcp/releases/latest";
/// Assets come only from here. Every other address in the answer is ignored.
const DOWNLOAD_PREFIX: &str = "https://github.com/sickmz/shcp/releases/download/";
/// The version of this build.
pub(crate) const CURRENT: &str = env!("CARGO_PKG_VERSION");
/// The releases hold only this build.
const SUPPORTED: bool = cfg!(all(windows, target_arch = "x86_64"));

const CHECK_EVERY: Duration = Duration::from_secs(24 * 60 * 60);
/// The app can stay open for days: this is how often it sees whether a check is due.
const LOOK_EVERY: Duration = Duration::from_secs(60 * 60);
/// Not while the window is coming up.
const FIRST_LOOK_AFTER: Duration = Duration::from_secs(5);

const MAX_JSON_BYTES: u64 = 1024 * 1024;
const MAX_CHECKSUM_BYTES: u64 = 1024;
const MAX_EXE_BYTES: u64 = 256 * 1024 * 1024;
const DOWNLOAD_CHUNK_BYTES: usize = 64 * 1024;
/// A progress the eye can follow, without redrawing the window for every piece.
const REPORT_EVERY: Duration = Duration::from_millis(150);

#[derive(Clone)]
pub(crate) struct Release {
    pub version: String,
    exe_url: String,
    checksum_url: String,
}

#[derive(Clone)]
pub(crate) enum UpdateStatus {
    Idle,
    Checking,
    UpToDate,
    Available(Release),
    Downloading {
        version: String,
        /// `None` while the size of the download is unknown.
        percent: Option<u8>,
    },
    /// In place of the old exe, waiting for the app to start again.
    Ready {
        version: String,
        exe: PathBuf,
    },
    /// The check did not get an answer.
    Failed(String),
    /// The release is known: it can be tried again.
    DownloadFailed {
        release: Release,
        error: String,
    },
}

const GITHUB: Service = Service {
    name: "GitHub",
    rate_limited: &[403, 429],
    accept: Some("application/vnd.github+json"),
};

fn version_numbers(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version.split('.').map(|part| part.parse::<u64>().ok());
    let numbers = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(numbers)
}

/// `None` when the release is not newer than `current`.
fn newer_release(release: &Value, current: &str) -> Result<Option<Release>> {
    let tag = release
        .get("tag_name")
        .and_then(Value::as_str)
        .context("GitHub sent an unreadable release")?;
    let version = tag.strip_prefix('v').unwrap_or(tag);
    let (Some(latest), Some(mine)) = (version_numbers(version), version_numbers(current)) else {
        bail!("The version {tag} cannot be compared with {current}");
    };
    if latest <= mine {
        return Ok(None);
    }
    let asset = |name: String| {
        release
            .get("assets")?
            .as_array()?
            .iter()
            .find(|asset| asset.get("name").and_then(Value::as_str) == Some(&name))?
            .get("browser_download_url")?
            .as_str()
            .filter(|url| url.starts_with(DOWNLOAD_PREFIX))
            .map(str::to_string)
    };
    let exe = format!("shcp-{version}-x86_64.exe");
    let (Some(exe_url), Some(checksum_url)) = (asset(exe.clone()), asset(format!("{exe}.sha256")))
    else {
        bail!("The release {tag} does not have the expected files");
    };
    Ok(Some(Release {
        version: version.to_string(),
        exe_url,
        checksum_url,
    }))
}

fn fetch_latest() -> Result<Option<Release>> {
    let release = GITHUB.get_json(
        &net::agent(Duration::from_secs(30)),
        LATEST_RELEASE,
        MAX_JSON_BYTES,
    )?;
    newer_release(&release, CURRENT)
}

fn verify_download(exe: &[u8], checksum: &[u8]) -> Result<()> {
    ensure!(
        net::sha256_matches(exe, checksum),
        "The download failed its integrity check"
    );
    ensure!(exe.starts_with(b"MZ"), "The download is not a program");
    Ok(())
}

/// `shcp.exe` next to `shcp.exe.old`, whatever the exe was renamed to.
fn sidecar(exe: &Path, suffix: &str) -> PathBuf {
    let mut name = OsString::from(exe.as_os_str());
    name.push(suffix);
    PathBuf::from(name)
}

/// Puts `bytes` in the place of `exe`, keeping the old one aside. On failure `exe` is as it was.
fn replace_exe(exe: &Path, bytes: &[u8]) -> Result<()> {
    let new = sidecar(exe, ".new");
    let old = sidecar(exe, ".old");
    std::fs::write(&new, bytes)
        .with_context(|| format!("Could not write next to {}", exe.display()))?;
    let _ = std::fs::remove_file(&old);
    if let Err(error) = std::fs::rename(exe, &old) {
        let _ = std::fs::remove_file(&new);
        return Err(error).with_context(|| format!("Could not replace {}", exe.display()));
    }
    if let Err(error) = std::fs::rename(&new, exe) {
        let _ = std::fs::rename(&old, exe);
        let _ = std::fs::remove_file(&new);
        return Err(error).with_context(|| format!("Could not replace {}", exe.display()));
    }
    Ok(())
}

fn percent_of(received: u64, total: u64) -> u8 {
    if total == 0 {
        return 0;
    }
    (received.saturating_mul(100) / total).min(100) as u8
}

/// Reads the exe piece by piece, saying how far it got when the server tells its size.
fn download_exe(agent: &ureq::Agent, url: &str, report: &Sender<u8>) -> Result<Vec<u8>> {
    let mut response = GITHUB.call(agent.get(url))?;
    let total = response.body().content_length();
    let mut reader = response
        .body_mut()
        .with_config()
        .limit(MAX_EXE_BYTES)
        .reader();
    let mut bytes = Vec::new();
    let mut chunk = vec![0; DOWNLOAD_CHUNK_BYTES];
    let mut reported = None;
    let mut last_report = Instant::now();
    loop {
        let read = reader
            .read(&mut chunk)
            .context("The download stopped before the end")?;
        if read == 0 {
            return Ok(bytes);
        }
        bytes.extend_from_slice(&chunk[..read]);
        let Some(total) = total else {
            continue;
        };
        let percent = percent_of(bytes.len() as u64, total);
        if reported != Some(percent)
            && (reported.is_none() || last_report.elapsed() >= REPORT_EVERY)
        {
            let _ = report.try_send(percent);
            reported = Some(percent);
            last_report = Instant::now();
        }
    }
}

fn download_and_replace(release: &Release, exe: &Path, report: &Sender<u8>) -> Result<()> {
    let agent = net::agent(Duration::from_secs(10 * 60));
    let bytes = download_exe(&agent, &release.exe_url, report)?;
    let checksum = GITHUB.get(&agent, &release.checksum_url, MAX_CHECKSUM_BYTES)?;
    verify_download(&bytes, &checksum)?;
    replace_exe(exe, &bytes)
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

impl AppState {
    /// Runs for as long as the app does.
    pub(crate) fn start_update_checks(&mut self, cx: &mut Context<Self>) {
        if !SUPPORTED {
            return;
        }
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .spawn(async move {
                    if let Ok(exe) = std::env::current_exe() {
                        let _ = std::fs::remove_file(sidecar(&exe, ".old"));
                    }
                })
                .await;
            cx.background_executor().timer(FIRST_LOOK_AFTER).await;
            loop {
                let alive = this.update(cx, |state, cx| {
                    if state.update_check_due() {
                        state.check_for_update(false, cx);
                    }
                });
                if alive.is_err() {
                    break;
                }
                cx.background_executor().timer(LOOK_EVERY).await;
            }
        })
        .detach();
    }

    fn update_check_due(&self) -> bool {
        // A build from the sources has no release to follow.
        !cfg!(debug_assertions)
            && self.settings.check_for_updates
            && matches!(
                self.update,
                UpdateStatus::Idle | UpdateStatus::UpToDate | UpdateStatus::Available(_)
            )
            && now().saturating_sub(self.settings.last_update_check) >= CHECK_EVERY.as_secs()
    }

    /// `manual`: asked for by the user, who gets to see a failure as well.
    pub(crate) fn check_for_update(&mut self, manual: bool, cx: &mut Context<Self>) {
        if matches!(
            self.update,
            UpdateStatus::Checking | UpdateStatus::Downloading { .. } | UpdateStatus::Ready { .. }
        ) {
            return;
        }
        if !SUPPORTED {
            self.update = UpdateStatus::Failed("Updates are for the Windows build".into());
            cx.notify();
            return;
        }
        self.update = UpdateStatus::Checking;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let found = cx
                .background_executor()
                .spawn(async move { fetch_latest() })
                .await;
            this.update(cx, |state, cx| {
                match found {
                    Ok(Some(release)) => {
                        state.settings.last_update_check = now();
                        state.persist_settings(cx);
                        state.update = UpdateStatus::Available(release);
                    }
                    Ok(None) => {
                        state.settings.last_update_check = now();
                        state.persist_settings(cx);
                        state.update = UpdateStatus::UpToDate;
                    }
                    Err(error) if manual => {
                        state.update = UpdateStatus::Failed(format!("{error:#}"));
                    }
                    // The next look tries again: being offline is not worth a message.
                    Err(_) => state.update = UpdateStatus::Idle,
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn install_update(&mut self, cx: &mut Context<Self>) {
        let release = match &self.update {
            UpdateStatus::Available(release) | UpdateStatus::DownloadFailed { release, .. } => {
                release.clone()
            }
            _ => return,
        };
        let version = release.version.clone();
        self.update = UpdateStatus::Downloading {
            version: version.clone(),
            percent: None,
        };
        cx.notify();
        let (report, reports) = async_channel::unbounded::<u8>();
        cx.spawn(async move |this, cx| {
            while let Ok(reported) = reports.recv().await {
                let alive = this.update(cx, |state, cx| {
                    if let UpdateStatus::Downloading { percent, .. } = &mut state.update {
                        *percent = Some(reported);
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
        cx.spawn(async move |this, cx| {
            let target = release.clone();
            let replaced = cx
                .background_executor()
                .spawn(async move {
                    // Asked now: once renamed, the running exe reports its new name.
                    let exe = std::env::current_exe().context("Could not find the running exe")?;
                    download_and_replace(&target, &exe, &report)?;
                    anyhow::Ok(exe)
                })
                .await;
            this.update(cx, |state, cx| {
                match replaced {
                    Ok(exe) => state.update = UpdateStatus::Ready { version, exe },
                    Err(error) => state.update_failed(release, format!("{error:#}"), cx),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn update_failed(&mut self, release: Release, error: String, cx: &mut Context<Self>) {
        self.update = UpdateStatus::DownloadFailed {
            release,
            error: error.clone(),
        };
        self.push_action_notice(
            NoticeLevel::Error,
            format!("Could not update shcp: {error}"),
            NoticeAction {
                label: "Open page",
                run: Rc::new(|_, cx: &mut App| cx.open_url(RELEASES_PAGE)),
            },
            cx,
        );
    }

    pub(crate) fn restart_into_update(&mut self, cx: &mut Context<Self>) {
        let UpdateStatus::Ready { exe, .. } = &self.update else {
            return;
        };
        match std::process::Command::new(exe).arg(AFTER_UPDATE).spawn() {
            Ok(_) => cx.quit(),
            Err(error) => self.push_notice(
                NoticeLevel::Error,
                format!("Could not restart shcp: {error}. The update starts the next time"),
                cx,
            ),
        }
    }
}

/// Tells a new version that the one starting it is about to close.
pub(crate) const AFTER_UPDATE: &str = "--after-update";

pub(crate) fn open_releases_page(cx: &mut App) {
    cx.open_url(RELEASES_PAGE);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use sha2::{Digest, Sha256};

    fn release(tag: &str, exe: &str) -> Value {
        json!({
            "tag_name": tag,
            "assets": [
                { "name": exe, "browser_download_url": format!("{DOWNLOAD_PREFIX}{tag}/{exe}") },
                {
                    "name": format!("{exe}.sha256"),
                    "browser_download_url": format!("{DOWNLOAD_PREFIX}{tag}/{exe}.sha256"),
                },
            ],
        })
    }

    #[test]
    fn progress_stays_between_nothing_and_everything() {
        assert_eq!(percent_of(0, 200), 0);
        assert_eq!(percent_of(50, 200), 25);
        assert_eq!(percent_of(200, 200), 100);
        // A server can send more than it said.
        assert_eq!(percent_of(300, 200), 100);
        assert_eq!(percent_of(10, 0), 0);
    }

    #[test]
    fn versions_compare_as_numbers() {
        let found = newer_release(&release("v0.10.0", "shcp-0.10.0-x86_64.exe"), "0.9.0");
        assert_eq!(found.unwrap().unwrap().version, "0.10.0");
    }

    #[test]
    fn the_same_or_an_older_version_is_not_an_update() {
        let same = release("v0.2.0", "shcp-0.2.0-x86_64.exe");
        assert!(newer_release(&same, "0.2.0").unwrap().is_none());
        assert!(newer_release(&same, "0.3.0").unwrap().is_none());
    }

    #[test]
    fn a_release_without_the_expected_files_is_refused() {
        let other = release("v0.3.0", "something-else.exe");
        assert!(newer_release(&other, "0.2.0").is_err());
    }

    #[test]
    fn an_asset_from_another_address_is_refused() {
        let foreign = json!({
            "tag_name": "v0.3.0",
            "assets": [
                { "name": "shcp-0.3.0-x86_64.exe", "browser_download_url": "https://evil.test/a" },
                {
                    "name": "shcp-0.3.0-x86_64.exe.sha256",
                    "browser_download_url": "https://evil.test/b",
                },
            ],
        });
        assert!(newer_release(&foreign, "0.2.0").is_err());
    }

    #[test]
    fn a_version_that_is_not_three_numbers_is_refused() {
        let odd = release("nightly", "shcp-nightly-x86_64.exe");
        assert!(newer_release(&odd, "0.2.0").is_err());
    }

    #[test]
    fn a_download_must_match_its_checksum_and_be_a_program() {
        let exe = b"MZ program";
        let hash: String = Sha256::digest(exe)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert!(verify_download(exe, format!("{hash}  shcp.exe\n").as_bytes()).is_ok());
        assert!(verify_download(exe, b"0000  shcp.exe\n").is_err());
        let text = b"not a program";
        let hash: String = Sha256::digest(text)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert!(verify_download(text, hash.as_bytes()).is_err());
    }

    #[test]
    fn replacing_keeps_the_old_exe_aside_and_can_happen_again() {
        let dir = std::env::temp_dir().join(format!("shcp-update-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("shcp.exe");
        std::fs::write(&exe, b"one").unwrap();

        replace_exe(&exe, b"two").unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"two");
        assert_eq!(std::fs::read(sidecar(&exe, ".old")).unwrap(), b"one");
        assert!(!sidecar(&exe, ".new").exists());

        replace_exe(&exe, b"three").unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"three");
        assert_eq!(std::fs::read(sidecar(&exe, ".old")).unwrap(), b"two");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_failed_replacement_leaves_nothing_changed() {
        let dir = std::env::temp_dir().join(format!("shcp-no-update-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // There is no exe to move aside.
        let exe = dir.join("shcp.exe");
        assert!(replace_exe(&exe, b"new").is_err());
        assert!(!exe.exists());
        assert!(!sidecar(&exe, ".new").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
