use super::*;
impl EngineHandle {
    pub fn hosts(&self) -> Vec<HostConfig> {
        self.0.store.lock().hosts.clone()
    }
    pub fn reload_hosts(&self) {
        let warnings = self.0.store.lock().load();
        self.warnings(warnings);
        self.emit(Event::HostsChanged);
    }
    pub fn save_host(&self, host: HostConfig) -> Result<()> {
        let warnings = self.0.store.lock().save_host(host)?;
        self.warnings(warnings);
        self.emit(Event::HostsChanged);
        Ok(())
    }
    pub fn delete_host(&self, id: &HostId) -> Result<()> {
        let warnings = self.0.store.lock().delete_host(id)?;
        self.warnings(warnings);
        self.emit(Event::HostsChanged);
        Ok(())
    }
    pub fn export_backup(&self, path: &std::path::Path) -> Result<()> {
        self.0.store.lock().export_backup(path)
    }
    /// Returns how many hosts came in.
    pub fn import_backup(&self, path: &std::path::Path) -> Result<usize> {
        let (warnings, count) = self.0.store.lock().import_backup(path)?;
        self.warnings(warnings);
        self.emit(Event::HostsChanged);
        Ok(count)
    }
    /// The servers `known_hosts` trusts.
    pub fn known_hosts(&self) -> Result<Vec<KnownHost>> {
        let path = self.0.store.lock().paths.known_hosts.clone();
        let _guard = self.0.known_hosts_lock.lock();
        crate::known_hosts::read(&path)
    }
    /// Stops trusting a server: the next connection asks about its key again.
    pub fn forget_known_host(&self, host: &KnownHost) -> Result<()> {
        let path = self.0.store.lock().paths.known_hosts.clone();
        let _guard = self.0.known_hosts_lock.lock();
        crate::known_hosts::forget(&path, host)
    }
    /// Where terminals of hosts with a session log write.
    pub fn session_logs_dir(&self) -> PathBuf {
        crate::session_log::folder(&self.0.store.lock().paths.data_dir)
    }
    pub fn settings(&self) -> Settings {
        self.0.store.lock().settings.clone()
    }
    pub fn save_settings(&self, s: Settings) -> Result<()> {
        self.0.store.lock().save_settings(s)
    }
    /// Removes the passphrase of a key file from the Credential Manager.
    pub fn forget_passphrase(&self, key: &str) {
        let account = crate::secrets::passphrase_account(std::path::Path::new(key));
        self.0.runtime.handle.spawn_blocking(move || {
            crate::secrets::forget(&account);
        });
    }
    /// Removes the password of `user@host:port` from the Credential Manager.
    pub fn forget_password(&self, target: &str) {
        let account = crate::secrets::password_account(target);
        self.0.runtime.handle.spawn_blocking(move || {
            crate::secrets::forget(&account);
        });
    }
    /// The tabs of the last run; a damaged file only loses them, with a notice.
    pub fn load_tabs(&self) -> SavedTabs {
        match self.0.store.lock().load_tabs() {
            Ok(tabs) => tabs,
            Err(error) => {
                self.emit(Event::Notice {
                    level: NoticeLevel::Important,
                    message: format!("Could not read tabs.toml: {error:#}"),
                });
                SavedTabs::default()
            }
        }
    }
    pub fn save_tabs(&self, tabs: &SavedTabs) -> Result<()> {
        self.0.store.lock().save_tabs(tabs)
    }
    pub fn paths(&self) -> AppPaths {
        self.0.store.lock().paths.clone()
    }
}
