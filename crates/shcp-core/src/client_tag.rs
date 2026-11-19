//! Tells which tmux clients on a server this PC left there. Each copy of the app marks the clients
//! it starts with the id of this installation and the id of its own run. A copy that was killed
//! leaves its clients behind, and a server that never heard the connection end keeps them as if it
//! were alive: the next connection from this installation closes those whose run is over, and
//! leaves alone those of other PCs, of copies still running, and any it did not mark.
//!
//! Which runs are still on is told by a file per run, held locked by its copy while it runs. More
//! than one copy can run at once, as while an update replaces the one before it.
use std::{
    fs::{self, File, OpenOptions, TryLockError},
    io::Write as _,
    path::{Path, PathBuf},
    time::Duration,
};

/// Where the id of this installation is kept, in the local data folder: a roaming profile does not
/// take it to another PC. It is no secret, only a name for this PC's clients.
const INSTALL_FILE: &str = "install-id";
/// Where each running copy holds the file of its run.
const RUNS_DIR: &str = "runs";
/// A run file no copy holds is removed once it is this old: a copy just starting may not have
/// locked its own yet.
const STALE_AFTER: Duration = Duration::from_secs(60);

/// What marks the clients of this run.
#[derive(Clone, Debug)]
pub(crate) struct ClientTag {
    pub install: String,
    pub run: String,
    runs: PathBuf,
}

impl ClientTag {
    /// The runs of this installation that are on, this one among them. `None` when that cannot be
    /// told, and then no client is closed for being of a run that is over.
    pub(crate) fn live_runs(&self) -> Option<Vec<String>> {
        let mut live = live_runs(&self.runs)?;
        if !live.contains(&self.run) {
            live.push(self.run.clone());
        }
        Some(live)
    }
}

/// Held while this copy runs: its locked file says the run is on.
pub(crate) struct RunHold {
    path: PathBuf,
    _file: File,
}

impl Drop for RunHold {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// The tag of this run, and what keeps it known as on. `None` when either cannot be had: the
/// clients then go unmarked, as before there were tags, and none is ever closed for its tag.
pub(crate) fn start(data_dir: &Path) -> Option<(ClientTag, RunHold)> {
    let computer = std::env::var("COMPUTERNAME").unwrap_or_default();
    let install = install_id(data_dir, &computer)?;
    let runs = data_dir.join(RUNS_DIR);
    let run = new_id();
    let hold = hold_run(&runs, &run)?;
    Some((ClientTag { install, run, runs }, hold))
}

/// A new id: random, in lowercase hex, which goes into a shell command as it is.
pub(crate) fn new_id() -> String {
    format!("{:032x}", rand::random::<u128>())
}

/// What an id read back must look like before it goes into a shell command.
pub(crate) fn valid_id(id: &str) -> bool {
    (16..=64).contains(&id.len())
        && id
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// The id of this installation, made the first time. The name of the computer is kept beside it:
/// a data folder copied to another PC gets a new id there, so the two never close each other's
/// clients.
fn install_id(data_dir: &Path, computer: &str) -> Option<String> {
    let path = data_dir.join(INSTALL_FILE);
    let read = |path: &Path| {
        let text = fs::read_to_string(path).ok()?;
        let mut lines = text.lines();
        let id = lines.next()?.trim();
        let name = lines.next().unwrap_or_default().trim();
        (valid_id(id) && name == computer).then(|| id.to_string())
    };
    if let Some(id) = read(&path) {
        return Some(id);
    }
    fs::create_dir_all(data_dir).ok()?;
    let id = new_id();
    // Written whole beside it, then put in place: a copy reading at that moment sees the old file
    // or the new one, never half of one.
    let temporary = data_dir.join(format!("{INSTALL_FILE}.{id}"));
    fs::write(&temporary, format!("{id}\n{computer}\n")).ok()?;
    let renamed = fs::rename(&temporary, &path);
    let _ = fs::remove_file(&temporary);
    renamed.ok()?;
    // Two copies making it at once: the one in place counts for both.
    read(&path)
}

/// Marks `run` as on for as long as what it returns is held. Clears the files of runs long over.
fn hold_run(runs: &Path, run: &str) -> Option<RunHold> {
    fs::create_dir_all(runs).ok()?;
    clear_stale(runs);
    let path = runs.join(run);
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&path)
        .ok()?;
    // Waits out a copy that is only looking at it.
    if file.lock().is_err() {
        let _ = fs::remove_file(&path);
        return None;
    }
    let _ = writeln!(file, "{}", std::process::id());
    Some(RunHold { path, _file: file })
}

/// Whether some copy holds the file of a run. Not knowing counts as held.
fn held(path: &Path) -> bool {
    let Ok(file) = File::open(path) else {
        return false;
    };
    match file.try_lock_shared() {
        Ok(()) => false,
        Err(TryLockError::WouldBlock) => true,
        Err(TryLockError::Error(_)) => true,
    }
}

/// The runs whose files are held: their copies are on.
fn live_runs(runs: &Path) -> Option<Vec<String>> {
    let entries = fs::read_dir(runs).ok()?;
    let mut live = Vec::new();
    for entry in entries {
        let entry = entry.ok()?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if valid_id(name) && held(&entry.path()) {
            live.push(name.to_string());
        }
    }
    Some(live)
}

fn clear_stale(runs: &Path) {
    let Ok(entries) = fs::read_dir(runs) else {
        return;
    };
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age >= STALE_AFTER);
        if old && !held(&entry.path()) {
            let _ = fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::Scratch;

    #[test]
    fn ids_are_random_hex_that_can_go_in_a_command() {
        let (one, two) = (new_id(), new_id());
        assert_ne!(one, two);
        assert!(valid_id(&one) && one.len() == 32);
        for bad in [
            "",
            "abc",
            "ABCDEF0123456789",
            "0123456789abcdeg",
            "0123456789abcde f",
        ] {
            assert!(!valid_id(bad), "{bad}");
        }
        assert!(!valid_id(&"a".repeat(65)));
    }

    #[test]
    fn the_install_id_is_made_once_and_kept() {
        let dir = Scratch::new("install-id");
        let first = install_id(&dir.0, "PC1").unwrap();
        assert_eq!(install_id(&dir.0, "PC1").as_deref(), Some(first.as_str()));
        let text = fs::read_to_string(dir.0.join(INSTALL_FILE)).unwrap();
        assert_eq!(text, format!("{first}\nPC1\n"));
        // Nothing is left beside it.
        assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);
    }

    #[test]
    fn a_copied_data_folder_gets_its_own_id_on_another_pc() {
        let dir = Scratch::new("install-id-moved");
        let first = install_id(&dir.0, "PC1").unwrap();
        let second = install_id(&dir.0, "PC2").unwrap();
        assert_ne!(first, second);
        assert_eq!(install_id(&dir.0, "PC2").as_deref(), Some(second.as_str()));
    }

    #[test]
    fn a_damaged_install_file_is_replaced() {
        let dir = Scratch::new("install-id-damaged");
        dir.file(INSTALL_FILE, "not an id; rm -rf /\nPC1\n");
        let id = install_id(&dir.0, "PC1").unwrap();
        assert!(valid_id(&id));
    }

    #[test]
    fn a_run_is_on_while_its_copy_holds_it() {
        let dir = Scratch::new("runs");
        let runs = dir.0.join(RUNS_DIR);
        let (one, two) = (new_id(), new_id());
        let first = hold_run(&runs, &one).unwrap();
        let second = hold_run(&runs, &two).unwrap();
        // A file nobody holds, as a killed copy leaves it.
        let dead = new_id();
        dir.file(&format!("{RUNS_DIR}/{dead}"), "1\n");
        let mut live = live_runs(&runs).unwrap();
        live.sort();
        let mut expected = vec![one.clone(), two.clone()];
        expected.sort();
        assert_eq!(live, expected);
        drop(first);
        assert_eq!(live_runs(&runs).unwrap(), std::slice::from_ref(&two));
        drop(second);
        assert!(live_runs(&runs).unwrap().is_empty());
        // Too recent to be cleared: a copy may be about to lock it.
        clear_stale(&runs);
        assert!(runs.join(&dead).exists());
    }

    #[test]
    fn this_run_always_counts_as_on() {
        let dir = Scratch::new("runs-tag");
        let (tag, hold) = start(&dir.0).unwrap();
        assert_eq!(tag.live_runs().unwrap(), std::slice::from_ref(&tag.run));
        drop(hold);
        assert_eq!(tag.live_runs().unwrap(), std::slice::from_ref(&tag.run));
        let gone = ClientTag {
            runs: dir.0.join("missing"),
            ..tag
        };
        assert!(gone.live_runs().is_none());
    }
}
