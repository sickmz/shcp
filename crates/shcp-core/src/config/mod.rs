pub(crate) mod ssh;
use crate::{AppPaths, HostConfig, HostId, HostSource, SavedTabs, Settings};
use anyhow::{Context, Result, bail};
use directories::{BaseDirs, ProjectDirs};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    io::Write as _,
    path::{Path, PathBuf},
};

pub(crate) struct Store {
    pub paths: AppPaths,
    pub hosts: Vec<HostConfig>,
    pub settings: Settings,
}
#[derive(Default, Serialize, Deserialize)]
struct HostsFile {
    #[serde(default)]
    hosts: Vec<HostConfig>,
}

pub(crate) fn home() -> Result<PathBuf> {
    Ok(BaseDirs::new()
        .context("Home directory unavailable")?
        .home_dir()
        .into())
}
/// A copy of what the user set up, to move to another PC or keep safe. Passwords stay in the
/// Credential Manager and `~/.ssh` is not touched: neither is in here.
#[derive(Serialize, Deserialize)]
struct Backup {
    version: u32,
    settings: Settings,
    #[serde(default)]
    hosts: Vec<HostConfig>,
}
const BACKUP_VERSION: u32 = 1;

/// What belongs to this PC and stays out of a backup: where the window is, when updates were
/// checked, and which secrets its Credential Manager holds.
fn keep_local(settings: &mut Settings, local: &Settings) {
    settings.window = local.window;
    settings.last_update_check = local.last_update_check;
    settings
        .saved_passphrases
        .clone_from(&local.saved_passphrases);
    settings.saved_passwords.clone_from(&local.saved_passwords);
}

/// Settings and backups saved before the bundled themes were renamed still use the old names.
fn rename_old_themes(settings: &mut Settings) {
    for name in [
        &mut settings.theme,
        &mut settings.dark_theme,
        &mut settings.light_theme,
    ] {
        match name.as_str() {
            "shcp Dark" => *name = "Dark".into(),
            "shcp Light" => *name = "Light".into(),
            _ => {}
        }
    }
}

impl Store {
    pub(crate) fn export_backup(&self, path: &Path) -> Result<()> {
        let mut settings = self.settings.clone();
        keep_local(&mut settings, &Settings::default());
        write(
            path,
            &Backup {
                version: BACKUP_VERSION,
                settings,
                hosts: self.app_hosts()?,
            },
        )
    }
    /// Replaces the settings and adds the hosts, replacing those with the same name. Nothing
    /// changes if the file is not good. Returns the warnings of reloading and the hosts imported.
    pub(crate) fn import_backup(&mut self, path: &Path) -> Result<(Vec<String>, usize)> {
        let text = fs::read_to_string(path).with_context(|| format!("Read {}", path.display()))?;
        let mut backup: Backup = toml::from_str(&text).context("It is not a shcp backup")?;
        if backup.version > BACKUP_VERSION {
            bail!("It comes from a newer version of shcp");
        }
        keep_local(&mut backup.settings, &self.settings);
        rename_old_themes(&mut backup.settings);
        validate_settings(&backup.settings)?;
        for host in &mut backup.hosts {
            normalize(host);
            validate_host(host).with_context(|| format!("Host {}", host.alias))?;
        }
        let count = backup.hosts.len();
        let mut hosts = self.app_hosts()?;
        hosts.retain(|host| !backup.hosts.iter().any(|new| new.alias == host.alias));
        hosts.extend(backup.hosts);
        self.write_hosts(hosts)?;
        self.save_settings(backup.settings)?;
        Ok((self.load(), count))
    }
    pub(crate) fn new() -> Result<Self> {
        let dirs = ProjectDirs::from("dev", "shcp", "shcp")
            .context("Application directories unavailable")?;
        let home = home()?;
        let paths = AppPaths {
            config_dir: dirs.config_dir().into(),
            data_dir: dirs.data_local_dir().into(),
            log_dir: dirs.data_local_dir().join("logs"),
            known_hosts: home.join(".ssh/known_hosts"),
            ssh_config: home.join(".ssh/config"),
        };
        fs::create_dir_all(&paths.config_dir).context("Create configuration directory")?;
        Ok(Self {
            paths,
            hosts: vec![],
            settings: Settings::default(),
        })
    }
    pub(crate) fn load(&mut self) -> Vec<String> {
        let mut warnings = Vec::new();
        let mut hosts = BTreeMap::new();
        let home = home();
        match read_optional(&self.paths.ssh_config) {
            Ok(Some(text)) => match &home {
                Ok(home) => {
                    let (parsed, notices) = ssh::parse(&text, home);
                    warnings.extend(notices);
                    for host in parsed {
                        hosts.insert(host.id.clone(), host);
                    }
                }
                Err(error) => warnings.push(error.to_string()),
            },
            Ok(None) => {}
            Err(error) => warnings.push(error.to_string()),
        }
        match self.app_hosts() {
            Ok(app) => {
                for mut host in app {
                    normalize(&mut host);
                    if let Err(error) = validate_host(&host) {
                        warnings.push(format!("Host {}: {error}", host.alias));
                        continue;
                    }
                    if let Ok(home) = &home {
                        ssh::expand_identity_files(&mut host, home);
                    }
                    hosts.insert(host.id.clone(), host);
                }
            }
            Err(error) => warnings.push(format!("Could not read hosts.toml: {error:#}")),
        }
        self.hosts = hosts.into_values().collect();
        warnings
    }
    fn hosts_path(&self) -> PathBuf {
        self.paths.config_dir.join("hosts.toml")
    }
    fn write_hosts(&self, hosts: Vec<HostConfig>) -> Result<()> {
        write(&self.hosts_path(), &HostsFile { hosts })
    }
    fn app_hosts(&self) -> Result<Vec<HostConfig>> {
        Ok(match read_optional(&self.hosts_path())? {
            Some(text) => toml::from_str::<HostsFile>(&text)?.hosts,
            None => vec![],
        })
    }
    pub(crate) fn load_settings(&mut self) -> Result<()> {
        if let Some(text) = read_optional(&self.paths.config_dir.join("settings.toml"))? {
            let mut settings: Settings = toml::from_str(&text)?;
            rename_old_themes(&mut settings);
            settings.migrate_style();
            validate_settings(&settings)?;
            self.settings = settings;
        }
        Ok(())
    }
    pub(crate) fn save_host(&mut self, mut host: HostConfig) -> Result<Vec<String>> {
        if host.source != HostSource::App {
            bail!("SSH config hosts are read-only");
        }
        normalize(&mut host);
        validate_host(&host)?;
        let mut hosts = self.app_hosts()?;
        hosts.retain(|h| h.alias != host.alias);
        hosts.push(host);
        self.write_hosts(hosts)?;
        Ok(self.load())
    }
    pub(crate) fn delete_host(&mut self, id: &HostId) -> Result<Vec<String>> {
        if !self
            .hosts
            .iter()
            .any(|h| &h.id == id && h.source == HostSource::App)
        {
            bail!("Only app hosts can be deleted");
        }
        let mut hosts = self.app_hosts()?;
        hosts.retain(|h| h.alias != id.0);
        self.write_hosts(hosts)?;
        Ok(self.load())
    }
    pub(crate) fn save_settings(&mut self, settings: Settings) -> Result<()> {
        validate_settings(&settings)?;
        write(&self.paths.config_dir.join("settings.toml"), &settings)?;
        self.settings = settings;
        Ok(())
    }
    pub(crate) fn load_tabs(&self) -> Result<SavedTabs> {
        Ok(
            match read_optional(&self.paths.config_dir.join("tabs.toml"))? {
                Some(text) => toml::from_str(&text)?,
                None => SavedTabs::default(),
            },
        )
    }
    pub(crate) fn save_tabs(&self, tabs: &SavedTabs) -> Result<()> {
        write(&self.paths.config_dir.join("tabs.toml"), tabs)
    }
}
fn normalize(host: &mut HostConfig) {
    host.id = HostId(host.alias.clone());
    host.source = HostSource::App;
    if host.hostname.is_empty() {
        host.hostname = host.alias.clone();
    }
}
fn validate_host(host: &HostConfig) -> Result<()> {
    if host.alias.trim().is_empty() || host.alias.contains(['\n', '\r', '*', '?']) {
        bail!("Host alias must be nonempty and contain no wildcards or newlines");
    }
    if host.hostname.is_empty() || host.port == 0 || host.user.is_empty() {
        bail!("Hostname, user and a nonzero port are required");
    }
    Ok(())
}
pub(crate) fn validate_settings(settings: &Settings) -> Result<()> {
    if !settings.ui_font_size.is_finite()
        || !settings.terminal_font_size.is_finite()
        || settings.ui_font_size <= 0.
        || settings.terminal_font_size <= 0.
    {
        bail!("Font sizes must be positive and finite");
    }
    if !settings.sidebar_width.is_finite() || settings.sidebar_width < 0. {
        bail!("Sidebar width must be a positive number");
    }
    if settings.scrollback_lines > 1_000_000 {
        bail!("Scrollback must be at most 1000000 lines");
    }
    if settings.transfer_concurrency == 0 || settings.transfer_concurrency > 64 {
        bail!("Transfer concurrency must be between 1 and 64");
    }
    Ok(())
}

fn read_optional(path: &Path) -> Result<Option<String>> {
    read_if_exists(path).with_context(|| format!("Read {}", path.display()))
}
/// The text of a file, `None` when there is no such file.
pub(crate) fn read_if_exists(path: &Path) -> std::io::Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}
fn write(path: &Path, value: &impl Serialize) -> Result<()> {
    write_atomic(path, toml::to_string_pretty(value)?.as_bytes())
        .with_context(|| format!("Write {}", path.display()))
}
/// Replaces a file in one step: whoever reads it, after a crash too, finds the old one or the new.
pub(crate) fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(format!(".{}.tmp", std::process::id()));
    let temporary = PathBuf::from(temporary);
    let result = (|| {
        let mut file = fs::File::create(&temporary)?;
        file.write_all(data)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Scratch, paths_in};

    /// A store whose files are all in a folder of their own in `scratch`.
    fn scratch_store(scratch: &Scratch, name: &str) -> Store {
        let dir = scratch.0.join(name);
        fs::create_dir_all(&dir).unwrap();
        Store {
            paths: paths_in(&dir),
            hosts: vec![],
            settings: Settings::default(),
        }
    }
    fn host(alias: &str, hostname: &str) -> HostConfig {
        HostConfig {
            alias: alias.into(),
            hostname: hostname.into(),
            user: "me".into(),
            ..Default::default()
        }
    }

    #[test]
    fn old_theme_names_become_the_new_ones_and_others_stay() {
        let scratch = Scratch::new("backup-themes");
        let mut store = scratch_store(&scratch, "store");
        fs::write(
            store.paths.config_dir.join("settings.toml"),
            "theme = \"shcp Light\"\ndark_theme = \"shcp Dark\"\nlight_theme = \"Solarized\"\n",
        )
        .unwrap();
        store.load_settings().unwrap();
        assert_eq!(store.settings.theme, "Nocturne");
        assert_eq!(store.settings.dark_theme, "Nocturne");
        assert_eq!(store.settings.light_theme, "Solarized");
    }

    #[test]
    fn a_backup_moves_hosts_and_settings_but_not_what_belongs_to_the_pc() {
        let scratch = Scratch::new("backup-move");
        let file = scratch.0.join("backup.toml");
        let mut source = scratch_store(&scratch, "source");
        source.save_host(host("web", "web.example")).unwrap();
        let mut settings = Settings {
            terminal_font_size: 19.,
            window: Some(crate::SavedWindow {
                x: 1.,
                y: 2.,
                width: 3.,
                height: 4.,
                maximized: false,
            }),
            ..Default::default()
        };
        settings.saved_passwords.insert("me@web:22".into());
        settings
            .keybindings
            .insert("new_terminal".into(), "ctrl-y".into());
        settings.snippets.push(crate::Snippet {
            name: "uptime".into(),
            command: "uptime".into(),
            host: None,
        });
        source.save_settings(settings).unwrap();
        source.export_backup(&file).unwrap();
        let text = fs::read_to_string(&file).unwrap();
        assert!(!text.contains("me@web:22"), "{text}");

        let mut target = scratch_store(&scratch, "target");
        target.save_host(host("web", "old.example")).unwrap();
        target.save_host(host("kept", "kept.example")).unwrap();
        let mut local = Settings::default();
        local.saved_passphrases.insert("local-key".into());
        target.save_settings(local).unwrap();
        let (warnings, count) = target.import_backup(&file).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(count, 1);
        let find = |alias: &str| target.hosts.iter().find(|h| h.alias == alias).unwrap();
        assert_eq!(find("web").hostname, "web.example");
        assert_eq!(find("kept").hostname, "kept.example");
        assert_eq!(target.settings.terminal_font_size, 19.);
        assert_eq!(target.settings.snippets.len(), 1);
        assert_eq!(target.settings.keybindings["new_terminal"], "ctrl-y");
        assert!(target.settings.window.is_none());
        assert!(target.settings.saved_passwords.is_empty());
        assert!(target.settings.saved_passphrases.contains("local-key"));
    }

    #[test]
    fn a_bad_file_changes_nothing() {
        let scratch = Scratch::new("backup-bad");
        let file = scratch.0.join("bad.toml");
        let mut store = scratch_store(&scratch, "store");
        store.save_host(host("web", "web.example")).unwrap();
        for text in [
            "not toml at all [",
            "version = 1\n",
            "version = 99\n[settings]\n",
        ] {
            fs::write(&file, text).unwrap();
            assert!(store.import_backup(&file).is_err(), "{text}");
        }
        assert_eq!(store.app_hosts().unwrap().len(), 1);
    }
}
