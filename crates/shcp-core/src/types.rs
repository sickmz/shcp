use crate::TerminalId;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    time::SystemTime,
};

macro_rules! id {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(pub u64);
    };
}
id!(SessionId);
id!(RequestId);
id!(TransferId);

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct HostId(pub String);
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum HostSource {
    SshConfig,
    #[default]
    App,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HostConfig {
    #[serde(skip)]
    pub id: HostId,
    pub alias: String,
    pub hostname: String,
    pub port: u16,
    pub user: String,
    pub identity_files: Vec<PathBuf>,
    pub proxy_command: Option<String>,
    /// Hosts to go through, like `ProxyJump`: comma-separated `[user@]host[:port]` or aliases.
    /// When set, `proxy_command` is ignored.
    pub proxy_jump: Option<String>,
    /// Lets the server use this PC's SSH agent, like `ForwardAgent yes`. Whoever is root there can
    /// then log in elsewhere as the user for as long as the connection lasts.
    pub forward_agent: bool,
    pub local_forwards: Vec<ForwardSpec>,
    pub server_alive_interval: Option<u32>,
    pub server_alive_count_max: u32,
    #[serde(skip)]
    pub source: HostSource,
}
impl Default for HostConfig {
    fn default() -> Self {
        Self {
            id: HostId::default(),
            alias: String::new(),
            hostname: String::new(),
            port: 22,
            user: std::env::var("USERNAME")
                .or_else(|_| std::env::var("USER"))
                .unwrap_or_default(),
            identity_files: vec![],
            proxy_command: None,
            proxy_jump: None,
            forward_agent: false,
            local_forwards: vec![],
            server_alive_interval: None,
            server_alive_count_max: 3,
            source: HostSource::App,
        }
    }
}
/// What a tunnel does with its two ends.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForwardKind {
    /// `LocalForward`: listens on this PC, and `remote_*` is where the server connects.
    #[default]
    Local,
    /// `RemoteForward`: `bind_*` is where the server listens, and `remote_*` is where this PC
    /// connects.
    Remote,
    /// `DynamicForward`: a SOCKS proxy on this PC. Each connection says where it goes, and the
    /// `remote_*` fields stay empty.
    Dynamic,
}
impl ForwardKind {
    fn is_local(&self) -> bool {
        *self == Self::Local
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ForwardSpec {
    pub bind_addr: String,
    pub bind_port: u16,
    pub remote_host: String,
    pub remote_port: u16,
    #[serde(default, skip_serializing_if = "ForwardKind::is_local")]
    pub kind: ForwardKind,
}
impl ForwardSpec {
    pub fn socks(bind_addr: String, bind_port: u16) -> Self {
        Self {
            bind_addr,
            bind_port,
            remote_host: String::new(),
            remote_port: 0,
            kind: ForwardKind::Dynamic,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AppPaths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub log_dir: PathBuf,
    pub known_hosts: PathBuf,
    pub ssh_config: PathBuf,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub ui_font_size: f32,
    pub terminal_font_family: String,
    pub terminal_font_size: f32,
    pub scrollback_lines: usize,
    pub transfer_concurrency: usize,
    pub confirm_on_delete: bool,
    /// Try keys from the running SSH agent (Windows OpenSSH agent, Pageant) before key files.
    pub use_ssh_agent: bool,
    /// Width of the host sidebar in pixels.
    pub sidebar_width: f32,
    /// Name of the active theme.
    pub theme: String,
    /// Follow the system light/dark mode, using `dark_theme` and `light_theme`.
    pub sync_theme: bool,
    pub dark_theme: String,
    pub light_theme: String,
    /// Name of the file icon theme; empty uses the app's own icons.
    pub icon_theme: String,
    pub density: Density,
    /// The color of what is on or chosen, in the app's own themes.
    pub accent: Accent,
    /// Which look the rest of these settings were last brought up to. Missing in older files.
    #[serde(default)]
    pub style_version: u32,
    /// Lets the desktop show through the window, blurred.
    pub translucent_window: bool,
    /// The highlight glides to the tab or host picked with the mouse, instead of jumping there.
    pub animated_selection: bool,
    /// Open the `LocalForward` tunnels on connecting; otherwise they wait for a click, so the
    /// ports stay free for other SSH clients reading the same config.
    pub auto_open_forwards: bool,
    /// Open the transfers panel when a transfer starts.
    pub auto_open_transfers: bool,
    /// List files and folders whose name starts with a dot in the Files tab.
    pub show_hidden_files: bool,
    /// Show the details of the selection beside the files, when there is room.
    pub show_file_details: bool,
    /// How wide those details are.
    pub file_details_width: f32,
    /// Program that opens remote files for editing; empty picks one automatically.
    pub editor: String,
    /// Connect again by itself when a connection with open tabs is lost.
    pub auto_reconnect: bool,
    /// Ask GitHub once a day whether a newer version is out.
    pub check_for_updates: bool,
    /// When that was last done, in seconds since the Unix epoch.
    pub last_update_check: u64,
    /// Open the tabs of the last run at startup.
    pub restore_tabs: bool,
    /// Ctrl+V pastes in a terminal. Off, it reaches the program, as vim's visual block needs.
    pub ctrl_v_pastes: bool,
    /// The shape of the cursor in terminals.
    pub cursor_style: CursorStyle,
    /// The cursor of the focused terminal blinks.
    pub cursor_blink: bool,
    /// Where downloads go without asking; empty asks each time.
    pub download_folder: String,
    /// Key files with a passphrase in the Credential Manager, which cannot be listed by app.
    /// The passphrases themselves are never here.
    pub saved_passphrases: BTreeSet<String>,
    /// The `user@host:port` with a password in the Credential Manager, likewise.
    pub saved_passwords: BTreeSet<String>,
    /// Ids of the hosts the sidebar does not list.
    pub hidden_hosts: BTreeSet<String>,
    /// Groups of the sidebar that show only their header.
    pub collapsed_groups: BTreeSet<String>,
    /// Hosts of the sidebar that show only their row, without their tabs.
    pub collapsed_hosts: BTreeSet<String>,
    /// Hosts in the order the user put them, by id. Those not in here follow.
    pub host_order: Vec<String>,
    /// Sidebar groups in the order the user put them. Those not in here follow, by name.
    pub group_order: Vec<String>,
    /// Hosts whose new terminals write what they show to a file.
    pub session_logs: BTreeSet<String>,
    /// Hosts already told that a key could replace their password.
    pub key_login_offered: BTreeSet<String>,
    /// Where the window was when the app closed. Among the last fields, being a table.
    pub window: Option<SavedWindow>,
    /// Shortcuts the user changed, by action. Empty text: the action has none.
    pub keybindings: BTreeMap<String, String>,
    /// Tunnels added from the sidebar, by host id. Kept apart from `~/.ssh/config`, which other
    /// SSH clients read too. Last fields: TOML writes tables after plain values.
    pub extra_forwards: BTreeMap<String, Vec<ForwardSpec>>,
    /// How hosts look, by host id. Here and not in `HostConfig`, because `~/.ssh/config` hosts
    /// are read-only.
    pub host_appearance: BTreeMap<String, HostAppearance>,
    /// Sidebar group of each host that has one, by host id.
    pub host_groups: BTreeMap<String, String>,
    /// Sent to every new terminal of a host, by host id.
    pub startup_commands: BTreeMap<String, String>,
    /// Commands the palette can run.
    pub snippets: Vec<Snippet>,
}
/// The look these settings are brought up to once, by `Settings::migrate_style`.
pub const STYLE_VERSION: u32 = 1;

impl Settings {
    /// Brings settings written for an older look up to the current one, once: what still has the
    /// old default takes the new one, and anything the user chose stays.
    pub fn migrate_style(&mut self) {
        if self.style_version >= STYLE_VERSION {
            return;
        }
        let renamed = |name: &mut String| match name.as_str() {
            "Dark" => *name = "Nocturne".into(),
            "Light" => *name = "Porcelain".into(),
            _ => {}
        };
        renamed(&mut self.theme);
        renamed(&mut self.dark_theme);
        renamed(&mut self.light_theme);
        if self.ui_font_size == 13. {
            self.ui_font_size = 14.;
        }
        if self.terminal_font_family == "Cascadia Mono" {
            self.terminal_font_family = "Geist Mono".into();
        }
        if self.terminal_font_size == 12. {
            self.terminal_font_size = 13.;
        }
        if self.sidebar_width == 260. {
            self.sidebar_width = 248.;
        }
        self.translucent_window = false;
        self.style_version = STYLE_VERSION;
    }
    /// Gives a group another name, keeping its place, its hosts and whether it is closed. Taking a
    /// name already in use joins the two, where the other one is.
    pub fn rename_group(&mut self, old: &str, new: &str) {
        let new = new.trim();
        if new.is_empty() || new == old {
            return;
        }
        let joins = self.host_groups.values().any(|group| group == new);
        for group in self.host_groups.values_mut() {
            if group == old {
                *group = new.to_string();
            }
        }
        if joins {
            self.group_order.retain(|group| group != old);
            self.collapsed_groups.remove(old);
            return;
        }
        for group in &mut self.group_order {
            if group == old {
                *group = new.to_string();
            }
        }
        if self.collapsed_groups.remove(old) {
            self.collapsed_groups.insert(new.to_string());
        }
    }
    /// Moves what is remembered for a host to its new id.
    pub fn rename_host(&mut self, old: &HostId, new: &HostId) {
        if old == new {
            return;
        }
        for id in &mut self.host_order {
            if *id == old.0 {
                id.clone_from(&new.0);
            }
        }
        if self.collapsed_hosts.remove(&old.0) {
            self.collapsed_hosts.insert(new.0.clone());
        }
        if self.hidden_hosts.remove(&old.0) {
            self.hidden_hosts.insert(new.0.clone());
        }
        if self.session_logs.remove(&old.0) {
            self.session_logs.insert(new.0.clone());
        }
        if self.key_login_offered.remove(&old.0) {
            self.key_login_offered.insert(new.0.clone());
        }
        if let Some(specs) = self.extra_forwards.remove(&old.0) {
            let saved = self.extra_forwards.entry(new.0.clone()).or_default();
            for spec in specs {
                if !saved.contains(&spec) {
                    saved.push(spec);
                }
            }
        }
        if let Some(appearance) = self.host_appearance.remove(&old.0) {
            self.host_appearance.insert(new.0.clone(), appearance);
        }
        if let Some(group) = self.host_groups.remove(&old.0) {
            self.host_groups.insert(new.0.clone(), group);
        }
        if let Some(command) = self.startup_commands.remove(&old.0) {
            self.startup_commands.insert(new.0.clone(), command);
        }
        for snippet in &mut self.snippets {
            if snippet.host.as_deref() == Some(&old.0) {
                snippet.host = Some(new.0.clone());
            }
        }
    }
    /// The commands the palette offers on a host.
    pub fn snippets_for<'a>(&'a self, host: &'a HostId) -> impl Iterator<Item = &'a Snippet> {
        self.snippets
            .iter()
            .filter(|snippet| snippet.host.as_ref().is_none_or(|h| *h == host.0))
    }
}
/// The place of the window, in logical pixels. When maximized, where it goes back to.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SavedWindow {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub maximized: bool,
}
/// A saved command. Without a host it is offered everywhere.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Snippet {
    pub name: String,
    pub command: String,
    pub host: Option<String>,
}
impl Snippet {
    /// An empty command offered on one host.
    pub fn default_for_host(host: &HostId) -> Self {
        Self {
            host: Some(host.0.clone()),
            ..Self::default()
        }
    }
}
/// How a tab's terminals are arranged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SplitAxis {
    /// Side by side.
    Row,
    /// One above the other.
    Column,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "pane", rename_all = "snake_case")]
pub enum SavedPane {
    Terminal,
    Split {
        axis: SplitAxis,
        ratio: f32,
        first: Box<SavedPane>,
        second: Box<SavedPane>,
    },
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SavedTab {
    Terminal {
        host: String,
        layout: SavedPane,
        /// The files show next to the terminals.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        both: bool,
        /// With both showing, the share of the terminals.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ratio: Option<f32>,
        /// The name the user gave the tab.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    Files {
        host: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
}
/// The tabs to open at the next start.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedTabs {
    pub active: Option<usize>,
    pub tabs: Vec<SavedTab>,
}
/// What the user picked for a host's tile. An unset field keeps the default; the names are the
/// app's own, and one it does not know also keeps the default.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HostAppearance {
    pub icon: Option<String>,
    /// Short text drawn instead of an icon.
    pub text: Option<String>,
    pub color: Option<String>,
}
/// The shape of the terminal cursor.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CursorStyle {
    /// What the program in the terminal asks for, as vim does between its modes.
    #[default]
    Program,
    Block,
    Bar,
    Underline,
}
/// Height of list rows and bars.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Density {
    Compact,
    #[default]
    Default,
    Comfortable,
}
/// The accents on offer in the app's own themes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Accent {
    #[default]
    Blue,
    Violet,
    Pink,
    Red,
    Orange,
    Amber,
    Green,
    Teal,
    /// No color: white on a dark theme, black on a light one.
    Mono,
}
impl Accent {
    pub const ALL: [Accent; 9] = [
        Accent::Blue,
        Accent::Violet,
        Accent::Pink,
        Accent::Red,
        Accent::Orange,
        Accent::Amber,
        Accent::Green,
        Accent::Teal,
        Accent::Mono,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Accent::Blue => "Blue",
            Accent::Violet => "Violet",
            Accent::Pink => "Pink",
            Accent::Red => "Red",
            Accent::Orange => "Orange",
            Accent::Amber => "Amber",
            Accent::Green => "Green",
            Accent::Teal => "Teal",
            Accent::Mono => "Mono",
        }
    }
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            ui_font_size: 14.,
            terminal_font_family: "Geist Mono".into(),
            terminal_font_size: 13.,
            scrollback_lines: 10_000,
            transfer_concurrency: 2,
            confirm_on_delete: true,
            use_ssh_agent: true,
            sidebar_width: 248.,
            theme: "Nocturne".into(),
            sync_theme: false,
            dark_theme: "Nocturne".into(),
            light_theme: "Porcelain".into(),
            icon_theme: String::new(),
            density: Density::Default,
            accent: Accent::Blue,
            style_version: STYLE_VERSION,
            translucent_window: false,
            animated_selection: true,
            auto_open_forwards: false,
            auto_open_transfers: true,
            show_hidden_files: false,
            show_file_details: true,
            file_details_width: 344.,
            editor: String::new(),
            auto_reconnect: true,
            check_for_updates: true,
            last_update_check: 0,
            restore_tabs: true,
            ctrl_v_pastes: true,
            cursor_style: CursorStyle::Program,
            cursor_blink: true,
            download_folder: String::new(),
            saved_passphrases: BTreeSet::new(),
            saved_passwords: BTreeSet::new(),
            hidden_hosts: BTreeSet::new(),
            collapsed_groups: BTreeSet::new(),
            collapsed_hosts: BTreeSet::new(),
            host_order: Vec::new(),
            group_order: Vec::new(),
            session_logs: BTreeSet::new(),
            key_login_offered: BTreeSet::new(),
            window: None,
            keybindings: BTreeMap::new(),
            extra_forwards: BTreeMap::new(),
            host_appearance: BTreeMap::new(),
            host_groups: BTreeMap::new(),
            startup_commands: BTreeMap::new(),
            snippets: Vec::new(),
        }
    }
}
#[derive(Debug, Clone)]
pub enum Event {
    HostsChanged,
    Session {
        id: SessionId,
        host: HostId,
        state: SessionState,
    },
    Forward {
        session: SessionId,
        spec: ForwardSpec,
        state: ForwardState,
        active_conns: u32,
    },
    TerminalChanged(TerminalId),
    TerminalTitle {
        id: TerminalId,
        title: String,
    },
    /// A terminal application asked to copy this text to the clipboard.
    TerminalCopy {
        text: String,
    },
    TerminalExited {
        id: TerminalId,
        status: Option<u32>,
    },
    RemotePath {
        req: RequestId,
        path: Result<String, String>,
    },
    DirListing {
        req: RequestId,
        path: String,
        entries: Result<Vec<RemoteEntry>, String>,
    },
    /// What a path really is, through a symbolic link.
    RemoteKind {
        req: RequestId,
        path: String,
        kind: Result<EntryKind, String>,
    },
    OpDone {
        req: RequestId,
        result: Result<(), String>,
    },
    /// How much room is left where a path is. `None`: the server does not say.
    RemoteSpace {
        req: RequestId,
        free: Option<u64>,
        total: Option<u64>,
    },
    /// The first bytes of a file. `None`: they are not text.
    RemoteHead {
        req: RequestId,
        path: String,
        text: Result<Option<String>, String>,
    },
    /// More of what a search of the subfolders found.
    SearchFound {
        req: RequestId,
        entries: Vec<RemoteEntry>,
    },
    /// A search of the subfolders ended. `Ok(true)`: it stopped at its limit, so there may be more.
    SearchDone {
        req: RequestId,
        result: Result<bool, String>,
    },
    Transfer(TransferInfo),
    /// A file pasted on a terminal reached the server: `result` is its path there.
    PastedFile {
        req: RequestId,
        result: Result<String, String>,
    },
    /// A save of a file being edited was held back: the file changed on the server.
    EditConflict {
        session: SessionId,
        remote: String,
    },
    /// A key file's passphrase entered the Credential Manager, or left it for being wrong.
    PassphraseSaved {
        key: PathBuf,
        saved: bool,
    },
    /// The same for the password of `user@host:port`.
    PasswordSaved {
        target: String,
        saved: bool,
    },
    /// A host took a password where a key could have spared it.
    PasswordLogin {
        host: HostId,
    },
    Notice {
        level: NoticeLevel,
        message: String,
    },
}
/// What a terminal sends to the server when a file is pasted or dropped on it.
#[derive(Debug, Clone)]
pub enum PasteFile {
    Bytes { name: String, data: Vec<u8> },
    Local(PathBuf),
}
#[derive(Debug, Clone)]
pub enum SessionState {
    Connecting {
        step: ConnectStep,
        progress: ConnectProgress,
    },
    AwaitingHostKey {
        host: String,
        key_type: String,
        fingerprint_sha256: String,
        kind: HostKeyKind,
    },
    AwaitingPassphrase {
        key_path: PathBuf,
    },
    AwaitingPassword {
        /// Who it is for, as the prompt says it.
        login: String,
    },
    /// A question of a keyboard-interactive login: a password, a code from a phone...
    AwaitingAnswer {
        prompt: String,
        instructions: String,
        /// The answer can be shown as it is typed.
        echo: bool,
        /// It is a password, which the Credential Manager can keep.
        can_remember: bool,
    },
    Connected,
    Disconnected {
        reason: Option<String>,
    },
    Failed {
        error: ConnectError,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectStep {
    Resolving,
    Connecting,
    StartingProxy,
    Handshake,
    VerifyingHostKey,
    Authenticating,
    OpeningForwards,
}
/// Where a step stands in the whole connection, jump hosts included.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConnectProgress {
    /// How many steps came before this one.
    pub position: u32,
    /// All the steps in order. Empty until the session has reported its first one.
    pub plan: Vec<PlannedStep>,
    /// What the step is working with: an address, a key, a login method.
    pub detail: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedStep {
    pub step: ConnectStep,
    /// The jump host it is on. `None` on the session's own host.
    pub hop: Option<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostKeyKind {
    Unknown,
    Changed,
}
#[derive(Debug, Clone)]
pub struct ConnectError {
    pub kind: ConnectErrorKind,
    pub message: String,
    pub hint: Option<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectErrorKind {
    ProxyNotFound,
    SsmPluginMissing,
    AwsCredentials,
    InstanceUnreachable,
    Transport,
    HostKey,
    Authentication,
    Other,
}
#[derive(Debug, Clone)]
pub enum ForwardState {
    Listening,
    /// Still listening locally, but the last connection found nothing on the server.
    ServerDown,
    /// Another program listens on the local port; `owner` is its name when known.
    InUse {
        owner: Option<String>,
    },
    Failed(String),
    Stopped,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeLevel {
    Info,
    Warning,
    Error,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
    Other,
}
#[derive(Debug, Clone)]
pub struct RemoteEntry {
    pub name: String,
    pub path: String,
    pub kind: EntryKind,
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub permissions: Option<u32>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Upload,
    Download,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferState {
    Queued,
    Running,
    Done,
    Failed(String),
    Cancelled,
}
#[derive(Debug, Clone)]
pub struct TransferInfo {
    pub id: TransferId,
    pub session: SessionId,
    pub host: HostId,
    pub direction: Direction,
    /// Upload: the file or folder sent. Download: the folder it lands in.
    pub local: PathBuf,
    /// Upload: the folder it lands in. Download: the file or folder fetched.
    pub remote: String,
    pub label: String,
    pub bytes_done: u64,
    pub bytes_total: Option<u64>,
    pub files_done: u32,
    pub files_total: Option<u32>,
    pub bytes_per_sec: u64,
    pub state: TransferState,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grouped() -> Settings {
        let mut settings = Settings::default();
        for (host, group) in [("a", "work"), ("b", "work"), ("c", "home")] {
            settings.host_groups.insert(host.into(), group.into());
        }
        settings.group_order = vec!["work".into(), "home".into()];
        settings.collapsed_groups.insert("work".into());
        settings
    }

    #[test]
    fn a_renamed_group_keeps_its_place_hosts_and_state() {
        let mut settings = grouped();
        settings.rename_group("work", "  office ");
        assert_eq!(settings.host_groups["a"], "office");
        assert_eq!(settings.host_groups["b"], "office");
        assert_eq!(settings.host_groups["c"], "home");
        assert_eq!(settings.group_order, ["office", "home"]);
        assert!(settings.collapsed_groups.contains("office"));
        assert!(!settings.collapsed_groups.contains("work"));
    }

    #[test]
    fn a_name_in_use_joins_the_two_groups() {
        let mut settings = grouped();
        settings.rename_group("work", "home");
        assert_eq!(settings.host_groups["a"], "home");
        assert_eq!(settings.group_order, ["home"]);
        assert!(settings.collapsed_groups.is_empty());
    }

    #[test]
    fn an_empty_name_changes_nothing() {
        let mut settings = grouped();
        settings.rename_group("work", "   ");
        assert_eq!(settings.host_groups["a"], "work");
    }

    #[test]
    fn older_settings_take_the_new_look_once_and_keep_what_was_chosen() {
        let mut old: Settings = toml::from_str(
            "theme = \"Dark\"\nlight_theme = \"Solarized\"\nui_font_size = 13.0\n\
             terminal_font_family = \"Cascadia Mono\"\nterminal_font_size = 15.0\n\
             translucent_window = true\n",
        )
        .unwrap();
        assert_eq!(old.style_version, 0);
        old.migrate_style();
        assert_eq!(old.theme, "Nocturne");
        assert_eq!(old.light_theme, "Solarized");
        assert_eq!(old.ui_font_size, 14.);
        assert_eq!(old.terminal_font_family, "Geist Mono");
        assert_eq!(old.terminal_font_size, 15.);
        assert!(!old.translucent_window);
        // From now on the choices are the user's, old defaults included.
        old.terminal_font_family = "Cascadia Mono".into();
        old.translucent_window = true;
        old.migrate_style();
        assert_eq!(old.terminal_font_family, "Cascadia Mono");
        assert!(old.translucent_window);
    }

    #[test]
    fn a_tab_saved_before_both_views_existed_shows_its_terminal() {
        let saved: SavedTabs = toml::from_str(
            "[[tabs]]\nkind = \"terminal\"\nhost = \"web\"\n[tabs.layout]\npane = \"terminal\"\n",
        )
        .unwrap();
        assert!(matches!(
            saved.tabs[0],
            SavedTab::Terminal { both: false, .. }
        ));
    }

    fn spec(port: u16) -> ForwardSpec {
        ForwardSpec {
            bind_addr: "127.0.0.1".into(),
            bind_port: port,
            remote_host: "localhost".into(),
            remote_port: port,
            kind: ForwardKind::Local,
        }
    }

    fn look(icon: &str) -> HostAppearance {
        HostAppearance {
            icon: Some(icon.into()),
            ..HostAppearance::default()
        }
    }

    #[test]
    fn settings_with_appearance_survive_toml() {
        let mut settings = Settings::default();
        settings
            .extra_forwards
            .insert("prod.db".into(), vec![spec(8080)]);
        settings.host_appearance.insert(
            "prod.db".into(),
            HostAppearance {
                icon: Some("database".into()),
                text: None,
                color: Some("red".into()),
            },
        );
        settings.host_appearance.insert(
            "pi".into(),
            HostAppearance {
                text: Some("Pi".into()),
                ..HostAppearance::default()
            },
        );
        let text = toml::to_string_pretty(&settings).unwrap();
        let read: Settings = toml::from_str(&text).unwrap();
        assert_eq!(read.host_appearance, settings.host_appearance);
        assert_eq!(read.extra_forwards, settings.extra_forwards);
    }

    #[test]
    fn settings_saved_before_appearance_still_load() {
        let read: Settings = toml::from_str("ui_font_size = 15.0\n").unwrap();
        assert!(read.host_appearance.is_empty());
        assert!(read.auto_reconnect);
        assert!(read.animated_selection);
        assert!(read.restore_tabs);
    }

    #[test]
    fn settings_with_everything_survive_toml() {
        let mut settings = Settings::default();
        settings.collapsed_groups.insert("prod".into());
        settings.host_groups.insert("db".into(), "prod".into());
        settings
            .startup_commands
            .insert("db".into(), "cd /app".into());
        settings.snippets.push(Snippet {
            name: "Logs".into(),
            command: "tail -f /var/log/app.log".into(),
            host: Some("db".into()),
        });
        settings.snippets.push(Snippet {
            name: "Top".into(),
            command: "top".into(),
            host: None,
        });
        settings
            .extra_forwards
            .insert("db".into(), vec![spec(5432)]);
        let text = toml::to_string_pretty(&settings).unwrap();
        let read: Settings = toml::from_str(&text).unwrap();
        assert_eq!(read.snippets, settings.snippets);
        assert_eq!(read.host_groups, settings.host_groups);
        assert_eq!(read.startup_commands, settings.startup_commands);
        assert_eq!(read.collapsed_groups, settings.collapsed_groups);
    }

    #[test]
    fn saved_tabs_survive_toml() {
        let tabs = SavedTabs {
            active: Some(1),
            tabs: vec![
                SavedTab::Files {
                    host: "a".into(),
                    name: Some("logs".into()),
                },
                SavedTab::Terminal {
                    both: false,
                    ratio: None,
                    host: "b".into(),
                    layout: SavedPane::Split {
                        axis: SplitAxis::Row,
                        ratio: 0.25,
                        first: Box::new(SavedPane::Terminal),
                        second: Box::new(SavedPane::Split {
                            axis: SplitAxis::Column,
                            ratio: 0.5,
                            first: Box::new(SavedPane::Terminal),
                            second: Box::new(SavedPane::Terminal),
                        }),
                    },
                    name: None,
                },
                SavedTab::Terminal {
                    both: false,
                    ratio: None,
                    host: "c".into(),
                    layout: SavedPane::Terminal,
                    name: Some("database".into()),
                },
            ],
        };
        let text = toml::to_string_pretty(&tabs).unwrap();
        assert_eq!(toml::from_str::<SavedTabs>(&text).unwrap(), tabs);
        let empty = toml::to_string_pretty(&SavedTabs::default()).unwrap();
        assert_eq!(
            toml::from_str::<SavedTabs>(&empty).unwrap(),
            SavedTabs::default()
        );
    }

    #[test]
    fn snippets_for_a_host_include_the_global_ones() {
        let mut settings = Settings::default();
        let snippet = |name: &str, host: Option<&str>| Snippet {
            name: name.into(),
            command: name.into(),
            host: host.map(str::to_string),
        };
        settings.snippets = vec![
            snippet("all", None),
            snippet("mine", Some("a")),
            snippet("other", Some("b")),
        ];
        let host = HostId("a".into());
        let names: Vec<_> = settings
            .snippets_for(&host)
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(names, ["all", "mine"]);
    }

    #[test]
    fn rename_moves_groups_and_commands() {
        let (old, new) = (HostId("old".into()), HostId("new".into()));
        let mut settings = Settings::default();
        settings.host_groups.insert("old".into(), "prod".into());
        settings.startup_commands.insert("old".into(), "ls".into());
        settings.snippets.push(Snippet {
            name: "x".into(),
            command: "x".into(),
            host: Some("old".into()),
        });
        settings.rename_host(&old, &new);
        assert_eq!(settings.host_groups["new"], "prod");
        assert_eq!(settings.startup_commands["new"], "ls");
        assert_eq!(settings.snippets[0].host.as_deref(), Some("new"));
    }

    #[test]
    fn rename_moves_everything_remembered_for_the_host() {
        let (old, new) = (HostId("old".into()), HostId("new".into()));
        let mut settings = Settings::default();
        settings.hidden_hosts.insert("old".into());
        settings.extra_forwards.insert("old".into(), vec![spec(1)]);
        settings.host_appearance.insert("old".into(), look("cloud"));
        settings.rename_host(&old, &new);
        assert!(settings.hidden_hosts.contains("new"));
        assert!(!settings.hidden_hosts.contains("old"));
        assert_eq!(settings.extra_forwards["new"], vec![spec(1)]);
        assert!(!settings.extra_forwards.contains_key("old"));
        assert_eq!(settings.host_appearance["new"], look("cloud"));
        assert!(!settings.host_appearance.contains_key("old"));
    }

    #[test]
    fn rename_onto_a_known_id_keeps_its_tunnels() {
        let (old, new) = (HostId("old".into()), HostId("new".into()));
        let mut settings = Settings::default();
        settings
            .extra_forwards
            .insert("old".into(), vec![spec(1), spec(2)]);
        settings.extra_forwards.insert("new".into(), vec![spec(2)]);
        settings.rename_host(&old, &new);
        assert_eq!(settings.extra_forwards["new"], vec![spec(2), spec(1)]);
    }

    #[test]
    fn rename_to_the_same_id_changes_nothing() {
        let id = HostId("same".into());
        let mut settings = Settings::default();
        settings.hidden_hosts.insert("same".into());
        settings
            .host_appearance
            .insert("same".into(), look("cloud"));
        settings.rename_host(&id, &id);
        assert!(settings.hidden_hosts.contains("same"));
        assert_eq!(settings.host_appearance["same"], look("cloud"));
    }
}
