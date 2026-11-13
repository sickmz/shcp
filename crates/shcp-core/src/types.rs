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
impl HostConfig {
    /// Who logs in where, as `user@hostname`.
    pub fn target(&self) -> String {
        format!("{}@{}", self.user, self.hostname)
    }
}
/// The user signed in to this PC: who a host without a user logs in as.
pub fn local_user() -> String {
    std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_default()
}
/// Keepalives that may go unanswered before the connection counts as lost, when a host does not
/// say.
pub const DEFAULT_SERVER_ALIVE_COUNT_MAX: u32 = 3;
impl Default for HostConfig {
    fn default() -> Self {
        Self {
            id: HostId::default(),
            alias: String::new(),
            hostname: String::new(),
            port: 22,
            user: local_user(),
            identity_files: vec![],
            proxy_command: None,
            proxy_jump: None,
            forward_agent: false,
            local_forwards: vec![],
            server_alive_interval: None,
            server_alive_count_max: DEFAULT_SERVER_ALIVE_COUNT_MAX,
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
    /// What tells this port from the others of its host, for `Settings::port_names`: which side
    /// listens and on which port. Where it goes can change and the name stays.
    pub fn name_key(&self) -> String {
        let side = match self.kind {
            ForwardKind::Local => 'L',
            ForwardKind::Remote => 'R',
            ForwardKind::Dynamic => 'D',
        };
        format!("{side}{}", self.bind_port)
    }

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
    /// The smoked glass of the themes that have one: the shaded surfaces and the light along
    /// their edges. Off, every theme is flat.
    pub glass_effects: bool,
    /// The highlight glides to the tab or host picked with the mouse, instead of jumping there.
    pub animated_selection: bool,
    /// Open the `LocalForward` tunnels on connecting; otherwise they wait for a click, so the
    /// ports stay free for other SSH clients reading the same config.
    pub auto_open_forwards: bool,
    /// Open the transfers panel when a transfer starts.
    pub auto_open_transfers: bool,
    /// List files and folders whose name starts with a dot in the Files tab.
    pub show_hidden_files: bool,
    /// Names of the folders a search of the subfolders does not enter.
    pub search_skip_folders: Vec<String>,
    /// Show the details of the selection in a card under the files. Off until asked for.
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
    /// Hosts whose terminals end with the connection, instead of going on with tmux on the server.
    pub plain_terminal_hosts: BTreeSet<String>,
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
    /// What the user calls a forwarded port, by host id and then by `ForwardSpec::name_key`. Here
    /// and not in the port itself: one from `~/.ssh/config` is read-only, and a name must not
    /// make two ports different.
    pub port_names: BTreeMap<String, BTreeMap<String, String>>,
    /// Commands the palette can run.
    pub snippets: Vec<Snippet>,
}
/// The look these settings are brought up to once, by `Settings::migrate_style`.
const STYLE_VERSION: u32 = 1;

impl Settings {
    /// New terminals of this host should go on after the connection. Without tabs that come back
    /// there would be no way to find them again.
    pub fn keeps_terminals(&self, host: &str) -> bool {
        self.restore_tabs && !self.plain_terminal_hosts.contains(host)
    }
    /// Brings settings written for an older look up to the current one, once: what still has the
    /// old default takes the new one, and anything the user chose stays.
    pub(crate) fn migrate_style(&mut self) {
        if self.style_version >= STYLE_VERSION {
            return;
        }
        let renamed = |name: &mut String| match name.as_str() {
            "Dark" | "Light" => *name = "Nocturne".into(),
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
        let move_in_set = |set: &mut BTreeSet<String>| {
            if set.remove(&old.0) {
                set.insert(new.0.clone());
            }
        };
        move_in_set(&mut self.collapsed_hosts);
        move_in_set(&mut self.hidden_hosts);
        move_in_set(&mut self.session_logs);
        move_in_set(&mut self.plain_terminal_hosts);
        move_in_set(&mut self.key_login_offered);
        fn move_in_map<V>(map: &mut BTreeMap<String, V>, old: &HostId, new: &HostId) {
            if let Some(value) = map.remove(&old.0) {
                map.insert(new.0.clone(), value);
            }
        }
        move_in_map(&mut self.host_appearance, old, new);
        move_in_map(&mut self.host_groups, old, new);
        move_in_map(&mut self.startup_commands, old, new);
        if let Some(specs) = self.extra_forwards.remove(&old.0) {
            let saved = self.extra_forwards.entry(new.0.clone()).or_default();
            for spec in specs {
                if !saved.contains(&spec) {
                    saved.push(spec);
                }
            }
        }
        if let Some(names) = self.port_names.remove(&old.0) {
            let kept = self.port_names.entry(new.0.clone()).or_default();
            for (key, name) in names {
                kept.entry(key).or_insert(name);
            }
        }
        for snippet in &mut self.snippets {
            if snippet.host.as_deref() == Some(&old.0) {
                snippet.host = Some(new.0.clone());
            }
        }
    }
    /// What the user calls this port of `host`, if anything.
    pub fn port_name(&self, host: &HostId, spec: &ForwardSpec) -> Option<&str> {
        self.port_names
            .get(&host.0)?
            .get(&spec.name_key())
            .map(String::as_str)
    }
    /// Names this port of `host`. An empty name takes it away.
    pub fn set_port_name(&mut self, host: &HostId, spec: &ForwardSpec, name: &str) {
        let (key, name) = (spec.name_key(), name.trim());
        if name.is_empty() {
            if let Some(names) = self.port_names.get_mut(&host.0) {
                names.remove(&key);
                if names.is_empty() {
                    self.port_names.remove(&host.0);
                }
            }
        } else {
            self.port_names
                .entry(host.0.clone())
                .or_default()
                .insert(key, name.to_string());
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
    Terminal {
        /// Its name on the server, where its shell may still be running.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<String>,
    },
    Split {
        axis: SplitAxis,
        ratio: f32,
        first: Box<SavedPane>,
        second: Box<SavedPane>,
    },
}
impl SavedPane {
    /// The names of its terminals on the server.
    pub fn keys(&self) -> Vec<String> {
        match self {
            Self::Terminal { key } => key.iter().cloned().collect(),
            Self::Split { first, second, .. } => {
                let mut keys = first.keys();
                keys.extend(second.keys());
                keys
            }
        }
    }
    /// Without the terminals that have no name or fail `keep`; a neighbour takes their room.
    pub fn retain_keys(&self, keep: &dyn Fn(&str) -> bool) -> Option<Self> {
        match self {
            Self::Terminal { key } => {
                key.as_deref()
                    .filter(|key| keep(key))
                    .map(|key| Self::Terminal {
                        key: Some(key.to_string()),
                    })
            }
            Self::Split {
                axis,
                ratio,
                first,
                second,
            } => match (first.retain_keys(keep), second.retain_keys(keep)) {
                (Some(first), Some(second)) => Some(Self::Split {
                    axis: *axis,
                    ratio: *ratio,
                    first: Box::new(first),
                    second: Box::new(second),
                }),
                (Some(only), None) | (None, Some(only)) => Some(only),
                (None, None) => None,
            },
        }
    }
}
/// How a terminal stays on the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Persistence {
    /// Its name there.
    pub key: String,
    /// It should be there already: if it is gone, it was closed and comes to an end here too.
    pub known: bool,
}
/// A tab as the PCs on a host share it. Which one is active, and the folder its files are at, stay
/// with each PC.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SharedTab {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The files show next to the terminals.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub files: bool,
    /// With the files showing, the share of the terminals.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files_ratio: Option<f32>,
    pub layout: SavedPane,
}
/// The tabs of a host, kept on the server for all the PCs using it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SharedTabs {
    /// The run of the app that wrote this, which has no need to read it back.
    pub writer: String,
    pub tabs: Vec<SharedTab>,
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
/// What the user picked for a host: its color. Unset, or one the app does not know, keeps the
/// default.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HostAppearance {
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
            light_theme: "Nocturne".into(),
            icon_theme: String::new(),
            density: Density::Default,
            accent: Accent::Blue,
            style_version: STYLE_VERSION,
            translucent_window: false,
            glass_effects: true,
            animated_selection: true,
            auto_open_forwards: false,
            auto_open_transfers: true,
            show_hidden_files: false,
            // Only names that are never a folder of someone's own: `target` or `build` can be.
            search_skip_folders: [
                ".git",
                ".hg",
                ".svn",
                "node_modules",
                "__pycache__",
                ".venv",
                ".cache",
                ".next",
                ".nuxt",
                ".gradle",
                ".terraform",
                ".tox",
                ".pytest_cache",
                ".mypy_cache",
            ]
            .map(String::from)
            .to_vec(),
            show_file_details: false,
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
            plain_terminal_hosts: BTreeSet::new(),
            key_login_offered: BTreeSet::new(),
            window: None,
            keybindings: BTreeMap::new(),
            extra_forwards: BTreeMap::new(),
            host_appearance: BTreeMap::new(),
            host_groups: BTreeMap::new(),
            startup_commands: BTreeMap::new(),
            port_names: BTreeMap::new(),
            snippets: Vec::new(),
        }
    }
}
/// What a search of the subfolders leaves out.
#[derive(Debug, Clone, Default)]
pub struct SearchScope {
    /// Also look at names starting with a dot, and inside folders named so.
    pub hidden: bool,
    /// Names of the folders it does not enter. They can still be found themselves.
    pub skip: Vec<String>,
}
/// How a search of the subfolders ended.
#[derive(Debug, Clone, Default)]
pub struct SearchEnd {
    /// It stopped at its limit, so there may be more.
    pub limited: bool,
    /// The names of the folders it met and did not enter because of `SearchScope::skip`.
    pub skipped: Vec<String>,
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
    /// The terminal was closed on another PC, or no longer exists: it goes from here too.
    TerminalClosed {
        id: TerminalId,
    },
    /// What runs on a host that was just connected. Comes for every connection that can keep
    /// terminals, even with nothing running, before any terminal attaches.
    SharedServer {
        host: HostId,
        /// The keys of the terminals running.
        live: Vec<String>,
        /// How the last PC to change them had them.
        tabs: Option<SharedTabs>,
    },
    /// A PC changed the tabs of a host.
    SharedTabsChanged {
        host: HostId,
        tabs: SharedTabs,
    },
    /// The server has a tmux that can keep terminals running.
    TmuxFound {
        host: HostId,
    },
    /// The server said it has no tmux that can keep terminals running.
    TmuxMissing {
        host: HostId,
    },
    /// The shell the user logs in with on the host of a session, by its name: `bash`, `zsh`. Comes
    /// once per connection, and not at all when the server does not say. It is the login shell,
    /// not what runs in a terminal later.
    LoginShell {
        session: SessionId,
        shell: String,
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
    /// A search of the subfolders ended.
    SearchDone {
        req: RequestId,
        result: Result<SearchEnd, String>,
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
    /// A warning that stays until it is closed: missing it can cost data or work.
    Important,
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
impl TransferState {
    /// Waiting to start or under way.
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Queued | Self::Running)
    }
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

    fn look(color: &str) -> HostAppearance {
        HostAppearance {
            color: Some(color.into()),
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
                color: Some("red".into()),
            },
        );
        settings.host_appearance.insert(
            "pi".into(),
            HostAppearance {
                color: Some("teal".into()),
            },
        );
        let text = toml::to_string_pretty(&settings).unwrap();
        let read: Settings = toml::from_str(&text).unwrap();
        assert_eq!(read.host_appearance, settings.host_appearance);
        assert_eq!(read.extra_forwards, settings.extra_forwards);
    }

    #[test]
    fn hosts_saved_with_an_icon_or_text_keep_their_color() {
        let read: Settings = toml::from_str(
            "[host_appearance.nuc]\ntext = \"NU\"\ncolor = \"green\"\n\n\
             [host_appearance.ec2]\nicon = \"zap\"\ncolor = \"red\"\n",
        )
        .unwrap();
        assert_eq!(read.host_appearance["nuc"], look("green"));
        assert_eq!(read.host_appearance["ec2"], look("red"));
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
                        first: Box::new(SavedPane::Terminal {
                            key: Some("k1".into()),
                        }),
                        second: Box::new(SavedPane::Split {
                            axis: SplitAxis::Column,
                            ratio: 0.5,
                            first: Box::new(SavedPane::Terminal { key: None }),
                            second: Box::new(SavedPane::Terminal {
                                key: Some("k2".into()),
                            }),
                        }),
                    },
                    name: None,
                },
                SavedTab::Terminal {
                    both: false,
                    ratio: None,
                    host: "c".into(),
                    layout: SavedPane::Terminal { key: None },
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
    fn tabs_saved_before_terminals_had_a_name_still_open() {
        let tabs: SavedTabs = toml::from_str(
            "[[tabs]]\nkind = \"terminal\"\nhost = \"a\"\n[tabs.layout]\npane = \"terminal\"\n",
        )
        .unwrap();
        assert!(matches!(
            &tabs.tabs[0],
            SavedTab::Terminal {
                layout: SavedPane::Terminal { key: None },
                ..
            }
        ));
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
        settings.host_appearance.insert("old".into(), look("teal"));
        settings.rename_host(&old, &new);
        assert!(settings.hidden_hosts.contains("new"));
        assert!(!settings.hidden_hosts.contains("old"));
        assert_eq!(settings.extra_forwards["new"], vec![spec(1)]);
        assert!(!settings.extra_forwards.contains_key("old"));
        assert_eq!(settings.host_appearance["new"], look("teal"));
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
        settings.host_appearance.insert("same".into(), look("teal"));
        settings.rename_host(&id, &id);
        assert!(settings.hidden_hosts.contains("same"));
        assert_eq!(settings.host_appearance["same"], look("teal"));
    }
}
