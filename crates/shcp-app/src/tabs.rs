//! A tab is a place on a host: its terminals, alone or with its files beside them.
use crate::files_view::FilesView;
use crate::terminal_panes::TerminalPanes;
use crate::widgets::context_menu::{MenuEntry, OpenMenu, handler};
use crate::widgets::icon::IconName;
use crate::workspace::Workspace;
use gpui::{Context, Entity, Pixels, Point, Subscription, Window};
use shcp_core::{HostId, SplitAxis};
use std::rc::Rc;

/// How a tab shows its place.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Face {
    Terminal,
    /// The terminals and, beside them, the files.
    Files,
}

impl Face {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Face::Terminal => "Terminal",
            Face::Files => "Files",
        }
    }

    pub(crate) fn icon(self) -> IconName {
        match self {
            Face::Terminal => IconName::Terminal,
            Face::Files => IconName::Folder,
        }
    }

    pub(crate) fn shows_files(self) -> bool {
        self == Face::Files
    }
}

pub(crate) struct Tab {
    pub id: u64,
    pub host: HostId,
    pub label: String,
    /// The name the user gave it, over anything else.
    pub name: Option<String>,
    pub face: Face,
    pub panes: Entity<TerminalPanes>,
    /// Made the first time the tab shows them.
    pub files: Option<Entity<FilesView>>,
    /// The files follow the folder of the terminal.
    pub linked: bool,
    /// With the files showing, the share of the terminals.
    pub both_ratio: f32,
    /// The folder of the terminal the files last went to.
    pub followed: Option<String>,
    /// Redraw the sidebar when what is in the tab changes.
    pub _observers: Vec<Subscription>,
}

/// The last name of a path, as a tab calls it.
fn folder_title(path: &str) -> Option<String> {
    match path.trim_end_matches('/') {
        "" if path.is_empty() => None,
        "" => Some("/".to_string()),
        "~" => Some("home".to_string()),
        trimmed => trimmed.rsplit('/').next().map(str::to_string),
    }
}

/// A folder as a tab's row shows it: from the home of `user` if inside it, and only its end if deep.
pub(crate) fn short_folder(folder: &str, user: &str) -> String {
    let home = if user == "root" {
        "/root".to_string()
    } else {
        format!("/home/{user}")
    };
    let folder = folder.trim_end_matches('/');
    let path = match folder.strip_prefix(&home) {
        Some("") => "~".to_string(),
        Some(rest) if rest.starts_with('/') => format!("~{rest}"),
        _ if folder.is_empty() => "/".to_string(),
        _ => folder.to_string(),
    };
    let names: Vec<&str> = path.split('/').filter(|name| !name.is_empty()).collect();
    match names.as_slice() {
        [.., parent, last] if names.len() > 3 => format!("…/{parent}/{last}"),
        _ => path,
    }
}

impl Tab {
    /// The folder of the terminal in focus, when the shell has said.
    pub(crate) fn terminal_folder(&self, cx: &gpui::App) -> Option<String> {
        self.panes.read(cx).active().read(cx).cwd()
    }

    /// The files take all the room when there is no shell to give the rest to: the server does
    /// not allow one, or it was closed.
    pub(crate) fn files_alone(&self, cx: &gpui::App) -> bool {
        self.face.shows_files() && self.files.is_some() && self.panes.read(cx).shells_gone(cx)
    }

    /// The folder the tab is at, as far as it is known.
    pub(crate) fn folder(&self, cx: &gpui::App) -> Option<String> {
        if self.face.shows_files()
            && let Some(files) = &self.files
        {
            let path = files.read(cx).path();
            if !path.is_empty() {
                return Some(path.to_string());
            }
        }
        self.terminal_folder(cx)
    }

    /// What the tab is called: the name given to it, the folder it is at, or what its terminal says.
    pub(crate) fn title(&self, cx: &gpui::App) -> String {
        if let Some(name) = &self.name {
            return name.clone();
        }
        if let Some(title) = self.folder(cx).as_deref().and_then(folder_title) {
            return title;
        }
        self.panes
            .read(cx)
            .title(cx)
            .unwrap_or_else(|| self.label.clone())
    }
}

/// The ways to split a terminal: label, icon, shortcut and axis.
pub(crate) const SPLITS: [(&str, IconName, &str, SplitAxis); 2] = [
    (
        "Split right",
        IconName::SplitRight,
        "Ctrl+Shift+D",
        SplitAxis::Row,
    ),
    (
        "Split down",
        IconName::SplitDown,
        "Ctrl+Shift+S",
        SplitAxis::Column,
    ),
];

pub(crate) fn tab_menu(
    workspace: &mut Workspace,
    id: u64,
    position: Point<Pixels>,
    cx: &mut Context<Workspace>,
) {
    let this = cx.entity().downgrade();
    let Some(index) = workspace.tab_index(id) else {
        return;
    };
    let count = workspace.tabs.len();
    let tab = &workspace.tabs[index];
    let host = tab.host.clone();
    let named = tab.name.is_some();
    let panes = tab.panes.clone();
    let (h1, h2) = (host.clone(), host);
    let mut entries = vec![
        MenuEntry::item(
            "New terminal",
            handler(this.clone(), move |w, window, cx| {
                w.open_terminal(h1.clone(), window, cx)
            }),
        )
        .icon(IconName::Terminal)
        .shortcut("Ctrl+Shift+T"),
        MenuEntry::item(
            "Browse files",
            handler(this.clone(), move |w, window, cx| {
                w.open_files(h2.clone(), window, cx)
            }),
        )
        .icon(IconName::Folder)
        .shortcut("Ctrl+Shift+E"),
    ];
    // The files sit beside one terminal.
    let files_shown = workspace.tabs[index].face.shows_files();
    for (label, icon, shortcut, axis) in SPLITS {
        let panes = panes.clone();
        entries.push(
            MenuEntry::item(
                label,
                Rc::new(move |window: &mut Window, cx: &mut gpui::App| {
                    panes.update(cx, |panes, cx| panes.split(axis, window, cx));
                }),
            )
            .icon(icon)
            .shortcut(shortcut)
            .disabled(files_shown),
        );
    }
    entries.push(MenuEntry::Separator);
    entries.push(
        MenuEntry::item(
            "Rename",
            handler(this.clone(), move |w, window, cx| {
                w.rename_tab(index, window, cx)
            }),
        )
        .icon(IconName::Pencil),
    );
    if named {
        entries.push(MenuEntry::item(
            "Use the automatic name",
            handler(this.clone(), move |w, _, cx| {
                w.set_tab_name(index, None, cx)
            }),
        ));
    }
    entries.extend([
        MenuEntry::Separator,
        MenuEntry::item(
            "Close",
            handler(this.clone(), move |w, window, cx| {
                w.close_tab(index, window, cx)
            }),
        )
        .icon(IconName::Close)
        .shortcut("Ctrl+Shift+W"),
        MenuEntry::item(
            "Close others",
            handler(this, move |w, window, cx| w.close_others(index, window, cx)),
        )
        .disabled(count < 2),
    ]);
    workspace.tab_menu = Some(OpenMenu::at(position, entries));
    cx.notify();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_reads_from_home_and_keeps_its_end() {
        assert_eq!(short_folder("/home/ubuntu", "ubuntu"), "~");
        assert_eq!(short_folder("/home/ubuntu/api/", "ubuntu"), "~/api");
        assert_eq!(short_folder("/home/ubuntu2", "ubuntu"), "/home/ubuntu2");
        assert_eq!(short_folder("/root/x", "root"), "~/x");
        assert_eq!(short_folder("/", "ubuntu"), "/");
        assert_eq!(short_folder("~/api", "ubuntu"), "~/api");
        assert_eq!(short_folder("/var/www/app", "deploy"), "/var/www/app");
        assert_eq!(
            short_folder("/var/www/app/logs/old", "deploy"),
            "…/logs/old"
        );
    }

    #[test]
    fn a_tab_is_named_after_its_folder() {
        assert_eq!(folder_title("/var/www/app").as_deref(), Some("app"));
        assert_eq!(folder_title("/var/www/app/").as_deref(), Some("app"));
        assert_eq!(folder_title("/").as_deref(), Some("/"));
        assert_eq!(folder_title("~").as_deref(), Some("home"));
        assert_eq!(folder_title("~/logs").as_deref(), Some("logs"));
        assert_eq!(folder_title(""), None);
    }
}
