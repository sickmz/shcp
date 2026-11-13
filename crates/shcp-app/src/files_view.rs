//! Files tab: SFTP browser for one host (listing, navigation, selection, upload/download,
//! mkdir/rename/delete). Remote results arrive as core events re-emitted by `AppState`.
use crate::files_format;
use crate::modals::{
    ModalLayer, confirm::ConfirmModal, folder_picker::FolderPicker, text_prompt::TextPrompt,
};
use crate::state::AppState;
use crate::theme::icon_theme::file_icon;
use crate::theme::{ActiveTheme as _, Theme, row_px};
use crate::widgets::button::{Button, ButtonStyle};
use crate::widgets::context_menu::{ContextMenu, MenuAnchor, MenuEntry, OpenMenu, handler};
use crate::widgets::icon::{Icon, IconName};
use crate::widgets::scrollbar::{ScrollDrag, list_scrollbar};
use crate::widgets::text_input::{TextInput, TextInputEvent};
use crate::widgets::tile::{Mark, tile};
use gpui::prelude::*;
use gpui::{
    AnyElement, App, Bounds, ClickEvent, ClipboardItem, Context, Corner, Div, Entity, EventEmitter,
    ExternalPaths, FocusHandle, Focusable, KeyDownEvent, MouseButton, MouseDownEvent,
    PathPromptOptions, Pixels, Point, ScrollStrategy, SharedString, Stateful, Subscription,
    UniformListScrollHandle, WeakEntity, Window, anchored, canvas, deferred, div, img, point, px,
    uniform_list,
};
use shcp_core::{
    EngineHandle, EntryKind, Event, HostId, NoticeLevel, RemoteEntry, RequestId, SearchScope,
    SessionId, SessionState, TransferId, TransferState, archive_stem,
};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    collections::HashSet,
    fmt::Write as _,
    ops::Range,
    path::PathBuf,
    rc::Rc,
    time::SystemTime,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum SortKey {
    Name,
    Size,
    Modified,
}

const SIZE_W: f32 = 76.;
const MODIFIED_W: f32 = 92.;
const PERMS_W: f32 = 96.;
const COLUMN_GAP: f32 = 14.;
/// Below these widths, a column gives way.
const PERMS_FROM: f32 = 620.;
const MODIFIED_FROM: f32 = 400.;
/// Lines of a file the preview draws.
const PREVIEW_LINES: usize = 60;
/// Folders kept for the back button.
const HISTORY_LIMIT: usize = 100;
/// Height of a row before the density setting.
const ROW_H: f32 = 30.;
/// Letters typed closer than this search together.
const TYPE_AHEAD: Duration = Duration::from_millis(800);

pub(crate) enum FilesEvent {
    OpenTerminalHere(String),
    GoHereInTerminal(String),
    /// The files follow the folder of the terminal, or stop following it.
    ToggleFollow,
}

/// Where a key moves the cursor on a list of `len` rows.
fn moved_cursor(key: &str, cursor: Option<usize>, len: usize, page: usize) -> Option<usize> {
    let last = len.checked_sub(1)?;
    Some(match (key, cursor) {
        ("end", _) | ("up" | "pageup", None) => last,
        ("home", _) | (_, None) => 0,
        ("up", Some(at)) => at.saturating_sub(1),
        ("down", Some(at)) => (at + 1).min(last),
        ("pageup", Some(at)) => at.saturating_sub(page),
        ("pagedown", Some(at)) => (at + page).min(last),
        (_, Some(at)) => at.min(last),
    })
}

/// A name not among `taken`, for a copy: `notes.txt`, `notes copy.txt`, `notes copy 2.txt`.
fn copy_name(name: &str, taken: &HashSet<String>) -> String {
    if !taken.contains(name) {
        return name.to_string();
    }
    // A leading dot starts a hidden name, not an extension.
    let (stem, extension) = match name.rfind('.') {
        Some(dot) if dot > 0 => name.split_at(dot),
        _ => (name, ""),
    };
    let mut number = 1;
    loop {
        let candidate = match number {
            1 => format!("{stem} copy{extension}"),
            n => format!("{stem} copy {n}{extension}"),
        };
        if !taken.contains(&candidate) {
            return candidate;
        }
        number += 1;
    }
}

/// Cut and Copy of the selection, to paste it elsewhere.
fn cut_copy_entries(this: &WeakEntity<FilesView>, ready: bool) -> [MenuEntry; 2] {
    [
        MenuEntry::item(
            "Cut",
            handler(this.clone(), |v, _, cx| v.pick_selected(true, cx)),
        )
        .icon(IconName::Forward)
        .shortcut("Ctrl+X")
        .disabled(!ready),
        MenuEntry::item(
            "Copy",
            handler(this.clone(), |v, _, cx| v.pick_selected(false, cx)),
        )
        .icon(IconName::Copy)
        .shortcut("Ctrl+C")
        .disabled(!ready),
    ]
}

/// The folder and the name of a remote path.
fn split_path(path: &str) -> (&str, &str) {
    match path.trim_end_matches('/').rsplit_once('/') {
        Some(("", name)) => ("/", name),
        Some(parts) => parts,
        None => ("", path),
    }
}

/// Whether `folder` is `path` or inside it: a folder cannot go into itself.
fn is_inside(folder: &str, path: &str) -> bool {
    let path = path.trim_end_matches('/');
    folder == path || folder.starts_with(&format!("{path}/"))
}

/// Permission bits written in octal, as `chmod` takes them.
fn parse_mode(text: &str) -> Option<u32> {
    u32::from_str_radix(text.trim(), 8)
        .ok()
        .filter(|mode| *mode <= 0o7777)
}

/// The first name starting with what was typed, ignoring case.
fn first_starting_with<'a>(names: impl Iterator<Item = &'a str>, typed: &str) -> Option<usize> {
    let typed = typed.to_lowercase();
    names
        .enumerate()
        .find(|(_, name)| name.to_lowercase().starts_with(&typed))
        .map(|(index, _)| index)
}

pub(crate) struct FilesView {
    state: Entity<AppState>,
    modals: Entity<ModalLayer>,
    host: HostId,
    session: SessionId,
    path: String,
    /// Everything in the folder, sorted.
    all_entries: Vec<RemoteEntry>,
    /// The rows on screen: `all_entries` narrowed by the filter and the hidden files setting.
    entries: Vec<RemoteEntry>,
    show_hidden: bool,
    /// Folders to return to with the back button.
    history: Vec<String>,
    going_back: bool,
    filter_input: Entity<TextInput>,
    /// The field has the focus. Otherwise it shows only while it holds some text.
    filter_open: bool,
    /// While there is one, the rows are what it found instead of the folder.
    search: Option<FileSearch>,
    selected: HashSet<String>,
    anchor: Option<usize>,
    /// The row the arrow keys move from.
    cursor: Option<usize>,
    /// What was typed to jump to a name, and when the last letter came.
    typed: String,
    typed_at: Option<Instant>,
    list_scroll: UniformListScrollHandle,
    scroll_drag: ScrollDrag,
    /// Links being opened: what they point to decides how.
    pending_links: HashMap<RequestId, String>,
    /// The room left where the folder on screen is, when the server says.
    free_space: Option<u64>,
    /// And how much there is in all.
    total_space: Option<u64>,
    /// How wide the view is, which decides the columns and the details.
    width: Rc<Cell<f32>>,
    /// The preview of the selected file floats over the list.
    looking: bool,
    /// Opened from the menu, it fades in: the how-manyth opening, for its animation.
    look_fade: Option<u64>,
    look_count: u64,
    perms: Option<PermsOpen>,
    /// Where the permissions cell of each row on screen is, for the popover to open under it.
    perm_cells: Rc<RefCell<HashMap<String, Bounds<Pixels>>>>,
    /// The file the details are showing the start of.
    preview: Option<Preview>,
    /// For the menu of the selection, wherever its button is.
    selection_anchor: MenuAnchor,
    pending_space: Option<RequestId>,
    /// Where to go instead of the home folder, once connected.
    start_path: Option<String>,
    /// Paths picked with Ctrl+C or Ctrl+X, waiting for Ctrl+V, and whether they are to move.
    picked: Option<(Vec<String>, bool)>,
    loading: bool,
    focus: FocusHandle,
    home_requested: bool,
    pending_home: Option<RequestId>,
    pending_list: Option<RequestId>,
    pending_ops: HashMap<RequestId, &'static str>,
    /// Uploads started from this view → destination directory (refresh when done).
    uploads: HashMap<TransferId, String>,
    sort: SortKey,
    ascending: bool,
    menu: Option<OpenMenu>,
    more_anchor: MenuAnchor,
    /// The toolbar button that opened `menu`.
    menu_button: Option<ToolbarMenu>,
    /// The toolbar menu open when the mouse went down: that press closes it, and the click that
    /// follows on its button must not open it again.
    closing_menu: Option<ToolbarMenu>,
    /// The files follow the folder of the terminal. Set by the workspace, for the menu.
    pub follows: bool,
    _subscriptions: Vec<Subscription>,
}

/// The start of a file, on its way or here.
struct Preview {
    path: String,
    req: RequestId,
    head: Head,
}

/// What came of reading the start of a file.
enum Head {
    Loading,
    Text(String),
    NotText,
}

/// The permissions of an entry, open in their popover.
struct PermsOpen {
    path: String,
    /// Where to show them when the list has no cell for them, as on a narrow card.
    at: Point<Pixels>,
}

/// A search of the subfolders of the folder on screen.
struct FileSearch {
    req: RequestId,
    query: String,
    root: String,
    running: bool,
    /// It stopped at its limit, so there may be more.
    limited: bool,
    /// It also enters the folders the settings leave out.
    everywhere: bool,
    /// The folders it met and did not enter.
    skipped: Vec<String>,
    found: Vec<RemoteEntry>,
}

impl FileSearch {
    /// Where an entry is from the folder searched.
    fn relative<'a>(&self, entry: &'a RemoteEntry) -> &'a str {
        entry
            .path
            .strip_prefix(&self.root)
            .map_or(&entry.name, |rest| rest.trim_start_matches('/'))
    }
}

/// The menus that open from a button of the toolbar.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ToolbarMenu {
    Folder,
    Selection,
}

impl EventEmitter<FilesEvent> for FilesView {}

impl Focusable for FilesView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl FilesView {
    pub(crate) fn new(
        state: Entity<AppState>,
        modals: Entity<ModalLayer>,
        host: HostId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let session = state.update(cx, |s, cx| s.connect_host(&host, cx));
        let filter_input = cx.new(|cx| TextInput::new("Filter this folder", cx).toolbar());
        let filter_focus = filter_input.focus_handle(cx);
        let focus = cx.focus_handle();
        let show_hidden = state.read(cx).settings.show_hidden_files;
        let subscriptions = vec![
            cx.on_focus(&focus, window, |_, _, cx| cx.notify()),
            cx.on_blur(&focus, window, |_, _, cx| cx.notify()),
            cx.on_focus(&filter_focus, window, |_, _, cx| cx.notify()),
            cx.subscribe(&state, |this, _, event: &Event, cx| {
                this.on_event(event, cx)
            }),
            // The setting can change from another Files tab or the settings window.
            cx.observe(&state, |this, state, cx| {
                let show_hidden = state.read(cx).settings.show_hidden_files;
                if show_hidden != this.show_hidden {
                    this.show_hidden = show_hidden;
                    // A search only looked where this let it, so it has to look again.
                    match this
                        .search
                        .as_ref()
                        .map(|s| (s.query.clone(), s.everywhere))
                    {
                        Some((query, everywhere)) => this.start_search(query, everywhere, cx),
                        None => this.apply_filter(cx),
                    }
                }
            }),
            cx.subscribe_in(
                &filter_input,
                window,
                |this, input, event: &TextInputEvent, window, cx| match event {
                    TextInputEvent::Changed => this.filter_changed(cx),
                    // Enter goes beyond the folder: the filter already shows what is in it.
                    TextInputEvent::Submit => {
                        let query = input.read(cx).text().trim().to_string();
                        this.start_search(query, false, cx);
                        window.focus(&this.focus);
                    }
                    TextInputEvent::Cancel => {
                        input.update(cx, |input, cx| input.set_text("", cx));
                        this.filter_changed(cx);
                        window.focus(&this.focus);
                    }
                },
            ),
            cx.on_blur(&filter_focus, window, |this, _, cx| {
                this.filter_open = false;
                cx.notify();
            }),
        ];
        let mut this = Self {
            state,
            modals,
            host,
            session,
            path: String::new(),
            all_entries: Vec::new(),
            entries: Vec::new(),
            show_hidden,
            history: Vec::new(),
            going_back: false,
            filter_input,
            filter_open: false,
            search: None,
            selected: HashSet::new(),
            anchor: None,
            cursor: None,
            typed: String::new(),
            typed_at: None,
            list_scroll: UniformListScrollHandle::new(),
            scroll_drag: ScrollDrag::default(),
            pending_links: HashMap::new(),
            free_space: None,
            total_space: None,
            width: Rc::new(Cell::new(PERMS_FROM)),
            looking: false,
            look_fade: None,
            look_count: 0,
            perms: None,
            perm_cells: Default::default(),
            preview: None,
            selection_anchor: MenuAnchor::default(),
            pending_space: None,
            start_path: None,
            picked: None,
            loading: true,
            focus,
            home_requested: false,
            pending_home: None,
            pending_list: None,
            pending_ops: HashMap::new(),
            uploads: HashMap::new(),
            sort: SortKey::Name,
            ascending: true,
            menu: None,
            more_anchor: MenuAnchor::default(),
            menu_button: None,
            closing_menu: None,
            follows: true,
            _subscriptions: subscriptions,
        };
        if this.session_connected(cx) {
            this.load_home(cx);
        }
        this
    }

    pub(crate) fn host(&self) -> &HostId {
        &self.host
    }

    /// The folder on screen; empty until the first listing arrives.
    pub(crate) fn path(&self) -> &str {
        &self.path
    }

    /// The keyboard is in the list or in the header's filter, which is outside the list's focus
    /// subtree: the card is the lit one.
    pub(crate) fn contains_focus(&self, window: &Window, cx: &App) -> bool {
        self.focus.contains_focused(window, cx)
            || self
                .filter_input
                .focus_handle(cx)
                .contains_focused(window, cx)
    }

    /// Its card keeps the tools in sight without the pointer, so nothing goes from under the
    /// user's hands: the filter has the keyboard or some text, or the menu of the tools is open.
    pub(crate) fn holds_tools(&self, window: &Window, cx: &App) -> bool {
        self.filter_input.focus_handle(cx).is_focused(window)
            || !self.filter_input.read(cx).text().trim().is_empty()
            || (self.menu.is_some() && self.menu_button == Some(ToolbarMenu::Folder))
    }

    fn session_connected(&self, cx: &App) -> bool {
        self.state.read(cx).session_connected(self.session)
    }

    /// A folder is showing and the connection is up: what acts on it can run.
    fn ready(&self, cx: &App) -> bool {
        !self.path.is_empty() && self.session_connected(cx)
    }

    fn notice(&self, level: NoticeLevel, message: String, cx: &mut Context<Self>) {
        self.state
            .update(cx, |s, cx| s.push_notice(level, message, cx));
    }

    // ---- backend requests ----

    fn load_home(&mut self, cx: &mut Context<Self>) {
        if self.home_requested {
            return;
        }
        self.home_requested = true;
        self.loading = true;
        if let Some(path) = self.start_path.take() {
            self.list_from_shell(path, cx);
            return;
        }
        self.pending_home = Some(self.state.read(cx).engine.remote_home(self.session));
        cx.notify();
    }

    /// Goes to a folder a shell said it is in. `~` stands for the home folder, which is where
    /// SFTP resolves relative paths.
    fn list_from_shell(&mut self, path: String, cx: &mut Context<Self>) {
        let path = match path.strip_prefix('~') {
            Some("") => ".".to_string(),
            Some(rest) => rest.trim_start_matches('/').to_string(),
            None => path,
        };
        self.pending_home = None;
        self.loading = true;
        self.pending_list = Some(self.state.read(cx).engine.list_dir(self.session, path));
        cx.notify();
    }

    /// Shows a folder as a shell names it, now or as soon as the connection is up.
    pub(crate) fn go_to(&mut self, path: String, cx: &mut Context<Self>) {
        if self.session_connected(cx) {
            self.home_requested = true;
            self.list_from_shell(path, cx);
        } else {
            self.start_path = Some(path);
        }
    }

    fn navigate(&mut self, path: String, cx: &mut Context<Self>) {
        if !self.session_connected(cx) {
            return;
        }
        if self.search.is_some() {
            self.close_search(cx);
        }
        // What floats over the list is about this folder.
        self.perms = None;
        self.looking = false;
        let path = if path.starts_with('/') {
            path
        } else {
            files_format::join(
                if self.path.is_empty() {
                    "/"
                } else {
                    &self.path
                },
                &path,
            )
        };
        self.pending_home = None;
        self.loading = true;
        self.pending_list = Some(self.state.read(cx).engine.list_dir(self.session, path));
        cx.notify();
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        if let Some((query, everywhere)) = self
            .search
            .as_ref()
            .map(|search| (search.query.clone(), search.everywhere))
        {
            self.start_search(query, everywhere, cx);
        } else if self.path.is_empty() {
            self.home_requested = false;
            self.load_home(cx);
        } else {
            self.navigate(self.path.clone(), cx);
        }
    }

    fn filter_changed(&mut self, cx: &mut Context<Self>) {
        let text = self.filter_input.read(cx).text().trim().to_string();
        if self.search.as_ref().is_some_and(|s| s.query != text) {
            self.end_search(cx);
        }
        self.apply_filter(cx);
    }

    /// Looks for a name in the folder on screen and everything under it. `everywhere` also enters
    /// the folders the settings leave out.
    fn start_search(&mut self, query: String, everywhere: bool, cx: &mut Context<Self>) {
        self.end_search(cx);
        if !query.is_empty() && self.ready(cx) {
            let root = self.path.clone();
            let state = self.state.read(cx);
            let scope = SearchScope {
                hidden: self.show_hidden,
                skip: if everywhere {
                    Vec::new()
                } else {
                    state.settings.search_skip_folders.clone()
                },
            };
            let req = state
                .engine
                .search_files(self.session, root.clone(), query.clone(), scope);
            self.search = Some(FileSearch {
                req,
                query,
                root,
                running: true,
                limited: false,
                everywhere,
                skipped: Vec::new(),
                found: Vec::new(),
            });
        }
        self.apply_filter(cx);
    }

    fn end_search(&mut self, cx: &mut Context<Self>) {
        if let Some(search) = self.search.take()
            && search.running
        {
            self.state.read(cx).engine.cancel_search(search.req);
        }
    }

    /// Back to the folder, and to all of it.
    fn close_search(&mut self, cx: &mut Context<Self>) {
        self.end_search(cx);
        self.filter_input
            .update(cx, |input, cx| input.set_text("", cx));
        self.apply_filter(cx);
    }

    /// Forgets the answers awaited from a connection that is gone.
    fn drop_pending(&mut self) {
        self.pending_home = None;
        self.pending_list = None;
        self.pending_ops.clear();
        self.uploads.clear();
    }

    /// Follows the host to a new connection, staying in the same folder.
    fn resume_on(&mut self, session: SessionId, cx: &mut Context<Self>) {
        self.session = session;
        self.drop_pending();
        self.refresh(cx);
    }

    fn on_event(&mut self, event: &Event, cx: &mut Context<Self>) {
        match event {
            // Another tab or the sidebar brought the host back: this tab follows.
            Event::Session {
                id,
                host,
                state: SessionState::Connected,
            } if *host == self.host
                && *id != self.session
                && self.state.read(cx).session_gone(self.session) =>
            {
                self.resume_on(*id, cx);
            }
            Event::Session { id, state, .. } if *id == self.session => {
                if matches!(state, SessionState::Connected) {
                    self.load_home(cx);
                } else if matches!(
                    state,
                    SessionState::Disconnected { .. } | SessionState::Failed { .. }
                ) {
                    self.drop_pending();
                    self.loading = false;
                }
                cx.notify();
            }
            Event::RemotePath { req, path } if Some(*req) == self.pending_home => {
                self.pending_home = None;
                match path {
                    Ok(path) => self.navigate(path.clone(), cx),
                    Err(error) => {
                        self.loading = false;
                        self.home_requested = false;
                        self.notice(
                            NoticeLevel::Error,
                            format!("Could not read home directory: {error}"),
                            cx,
                        );
                    }
                }
                cx.notify();
            }
            Event::RemoteKind { req, path, kind } if self.pending_links.contains_key(req) => {
                self.pending_links.remove(req);
                match kind {
                    Ok(EntryKind::Dir) => self.navigate(path.clone(), cx),
                    Ok(EntryKind::File) => {
                        self.state
                            .read(cx)
                            .engine
                            .edit_file(self.session, path.clone());
                    }
                    Ok(_) => self.notice(
                        NoticeLevel::Warning,
                        format!("{path} points to something that cannot be opened"),
                        cx,
                    ),
                    Err(error) => self.notice(
                        NoticeLevel::Error,
                        format!("Could not open {path}, maybe a broken link: {error}"),
                        cx,
                    ),
                }
            }
            Event::DirListing { req, path, entries } if Some(*req) == self.pending_list => {
                self.pending_list = None;
                self.loading = false;
                match entries {
                    Ok(entries) => self.show_listing(path.clone(), entries.clone(), cx),
                    Err(error) => {
                        self.going_back = false;
                        self.notice(
                            NoticeLevel::Error,
                            format!("Could not list {path}: {error}"),
                            cx,
                        );
                    }
                }
                cx.notify();
            }
            Event::RemoteSpace { req, free, total } if Some(*req) == self.pending_space => {
                self.pending_space = None;
                self.free_space = *free;
                self.total_space = *total;
                cx.notify();
            }
            Event::RemoteHead { req, text, .. } => {
                if let Some(preview) = self.preview.as_mut().filter(|p| p.req == *req) {
                    preview.head = match text {
                        Ok(Some(text)) => Head::Text(text.clone()),
                        _ => Head::NotText,
                    };
                    cx.notify();
                }
            }
            Event::SearchFound { req, entries } => {
                if let Some(search) = self.search.as_mut().filter(|s| s.req == *req) {
                    search.found.extend(entries.iter().cloned());
                    self.apply_filter(cx);
                }
            }
            Event::SearchDone { req, result } => {
                if let Some(search) = self.search.as_mut().filter(|s| s.req == *req) {
                    search.running = false;
                    match result {
                        Ok(end) => {
                            search.limited = end.limited;
                            search.skipped.clone_from(&end.skipped);
                        }
                        Err(error) => {
                            let message = format!("Could not search: {error}");
                            self.notice(NoticeLevel::Error, message, cx);
                        }
                    }
                    cx.notify();
                }
            }
            Event::OpDone { req, result } => {
                if let Some(label) = self.pending_ops.remove(req) {
                    if let Err(error) = result {
                        self.notice(NoticeLevel::Error, format!("{label} failed: {error}"), cx);
                    }
                    if self.pending_ops.is_empty() {
                        self.refresh(cx);
                    }
                }
            }
            // An upload retried from the transfers panel was not started here.
            Event::Transfer(info)
                if info.state == TransferState::Done
                    && info.direction == shcp_core::Direction::Upload
                    && info.host == self.host
                    && info.remote == self.path
                    && !self.uploads.contains_key(&info.id) =>
            {
                self.refresh(cx);
            }
            Event::Transfer(info) => {
                if let Some(dir) = self.uploads.get(&info.id).cloned() {
                    match info.state {
                        TransferState::Done => {
                            self.uploads.remove(&info.id);
                            if dir == self.path {
                                self.refresh(cx);
                            }
                        }
                        TransferState::Failed(_) | TransferState::Cancelled => {
                            self.uploads.remove(&info.id);
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    fn show_listing(
        &mut self,
        path: String,
        mut entries: Vec<RemoteEntry>,
        cx: &mut Context<Self>,
    ) {
        self.sort_entries(&mut entries);
        if path != self.path {
            if !self.path.is_empty() && !self.going_back {
                self.history.push(self.path.clone());
                if self.history.len() > HISTORY_LIMIT {
                    self.history.remove(0);
                }
            }
            self.filter_input
                .update(cx, |input, cx| input.set_text("", cx));
            self.selected.clear();
        } else {
            let names: HashSet<_> = entries.iter().map(|e| e.path.clone()).collect();
            self.selected.retain(|p| names.contains(p));
        }
        self.going_back = false;
        // Asked again each time: uploads and deletions change it, and so does another disk.
        self.pending_space = Some(
            self.state
                .read(cx)
                .engine
                .free_space(self.session, path.clone()),
        );
        self.path = path;
        self.all_entries = entries;
        self.apply_filter(cx);
    }

    /// Recomputes the visible rows; hidden rows leave the selection, so actions never touch them.
    fn apply_filter(&mut self, cx: &mut Context<Self>) {
        let query = self.filter_input.read(cx).text().trim().to_lowercase();
        let show_hidden = self.show_hidden;
        self.entries = if let Some(search) = &self.search {
            // Inside a hidden folder a file is hidden as well.
            let mut found: Vec<RemoteEntry> = search
                .found
                .iter()
                .filter(|e| {
                    show_hidden || !search.relative(e).split('/').any(|p| p.starts_with('.'))
                })
                .cloned()
                .collect();
            self.sort_entries(&mut found);
            found
        } else {
            self.all_entries
                .iter()
                .filter(|e| show_hidden || !e.name.starts_with('.'))
                .filter(|e| query.is_empty() || e.name.to_lowercase().contains(&query))
                .cloned()
                .collect()
        };
        let visible: HashSet<_> = self.entries.iter().map(|e| e.path.clone()).collect();
        self.selected.retain(|p| visible.contains(p));
        self.anchor = None;
        self.cursor = None;
        cx.notify();
    }

    /// Folders first, then the chosen column.
    fn sort_entries(&self, entries: &mut [RemoteEntry]) {
        let (key, ascending) = (self.sort, self.ascending);
        entries.sort_by(|a, b| {
            let by_column = match key {
                SortKey::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                SortKey::Size => a.size.cmp(&b.size),
                SortKey::Modified => a.modified.cmp(&b.modified),
            };
            let by_column = if ascending {
                by_column
            } else {
                by_column.reverse()
            };
            (a.kind != EntryKind::Dir)
                .cmp(&(b.kind != EntryKind::Dir))
                .then(by_column)
                .then_with(|| a.name.cmp(&b.name))
        });
    }

    fn set_sort(&mut self, key: SortKey, cx: &mut Context<Self>) {
        if self.sort == key {
            self.ascending = !self.ascending;
        } else {
            self.sort = key;
            self.ascending = true;
        }
        let mut entries = std::mem::take(&mut self.all_entries);
        self.sort_entries(&mut entries);
        self.all_entries = entries;
        self.apply_filter(cx);
    }

    // ---- context menu ----

    fn on_row_right_click(
        &mut self,
        index: usize,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
        window.focus(&self.focus);
        let Some(entry) = self.entries.get(index) else {
            return;
        };
        if !self.selected.contains(&entry.path) {
            self.selected = HashSet::from([entry.path.clone()]);
            self.anchor = Some(index);
        }
        self.open_menu(event.position, Some(index), cx);
    }

    fn open_menu(&mut self, position: Point<Pixels>, row: Option<usize>, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        let ready = self.ready(cx);
        let selection = self.selected.len();
        let mut entries = Vec::new();
        if let Some(entry) = row.and_then(|i| self.entries.get(i)) {
            entries.extend(Self::open_entries(entry, row, selection, ready, &this));
            entries.extend(self.row_entries(entry, selection, ready, position, &this, cx));
        }
        entries.push(
            MenuEntry::item(
                "Paste",
                handler(this.clone(), |v, _, cx| v.paste_picked(cx)),
            )
            .icon(IconName::Copy)
            .shortcut("Ctrl+V")
            .disabled(!ready || self.picked.is_none()),
        );
        // What creates something here belongs to the free space, not to a file.
        if row.is_none() {
            entries.extend(self.add_entries(cx));
            entries.extend(self.upload_entries(cx));
        }
        entries.extend([
            MenuEntry::Separator,
            self.hidden_files_entry(this.clone()),
            MenuEntry::item("Refresh", handler(this.clone(), |v, _, cx| v.refresh(cx)))
                .icon(IconName::Refresh)
                .shortcut("F5"),
        ]);
        if row.is_none() {
            let path = self.path.clone();
            entries.push(
                MenuEntry::item(
                    "Open terminal here",
                    handler(this.clone(), move |_, _, cx: &mut Context<Self>| {
                        cx.emit(FilesEvent::OpenTerminalHere(path.clone()))
                    }),
                )
                .icon(IconName::Terminal)
                .disabled(!ready),
            );
            entries.push(
                MenuEntry::item(
                    "Copy current path",
                    handler(this, |v, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(v.path.clone()))
                    }),
                )
                .icon(IconName::Copy),
            );
        } else {
            entries.push(MenuEntry::Separator);
            entries.push(
                MenuEntry::item("Delete", handler(this, FilesView::delete))
                    .icon(IconName::Trash)
                    .shortcut("Del")
                    .danger()
                    .disabled(!ready),
            );
        }
        self.menu = Some(OpenMenu::at(position, entries));
        self.menu_button = None;
        cx.notify();
    }

    /// What opens the entry under the pointer, when it is the only one selected.
    fn open_entries(
        entry: &RemoteEntry,
        row: Option<usize>,
        selection: usize,
        ready: bool,
        this: &WeakEntity<Self>,
    ) -> Vec<MenuEntry> {
        let mut entries = Vec::new();
        if entry.kind == EntryKind::Dir && selection == 1 {
            let path = entry.path.clone();
            entries.push(
                MenuEntry::item(
                    "Open terminal here",
                    handler(this.clone(), move |_, _, cx: &mut Context<Self>| {
                        cx.emit(FilesEvent::OpenTerminalHere(path.clone()))
                    }),
                )
                .icon(IconName::Terminal)
                .disabled(!ready),
            );
            let path = entry.path.clone();
            entries.push(
                MenuEntry::item(
                    "Go here in terminal",
                    handler(this.clone(), move |_, _, cx: &mut Context<Self>| {
                        cx.emit(FilesEvent::GoHereInTerminal(path.clone()))
                    }),
                )
                .icon(IconName::Terminal)
                .disabled(!ready),
            );
        }
        if matches!(entry.kind, EntryKind::Dir | EntryKind::Symlink) && selection == 1 {
            entries.push(
                MenuEntry::item(
                    "Open",
                    handler(this.clone(), move |v, _, cx| {
                        if let Some(i) = row {
                            v.open(i, cx)
                        }
                    }),
                )
                .icon(IconName::Folder)
                .shortcut("Enter"),
            );
        }
        if entry.kind == EntryKind::File && selection == 1 {
            entries.push(
                MenuEntry::item(
                    "Edit",
                    handler(this.clone(), move |v, _, cx| {
                        if let Some(i) = row {
                            v.open(i, cx)
                        }
                    }),
                )
                .icon(IconName::File)
                .shortcut("Enter")
                .disabled(!ready),
            );
            entries.push(
                MenuEntry::item(
                    "Preview",
                    handler(this.clone(), |v, _, cx| v.open_look(true, cx)),
                )
                .icon(IconName::Eye)
                .shortcut("Space"),
            );
        }
        entries
    }

    /// What acts on the selected entries.
    fn row_entries(
        &self,
        entry: &RemoteEntry,
        selection: usize,
        ready: bool,
        position: Point<Pixels>,
        this: &WeakEntity<Self>,
        cx: &mut Context<Self>,
    ) -> Vec<MenuEntry> {
        let mut entries = Vec::new();
        entries.push(
            MenuEntry::item(
                if selection > 1 {
                    format!("Download {selection} items")
                } else {
                    "Download".to_string()
                },
                handler(this.clone(), |v, _, cx| v.download(cx)),
            )
            .icon(IconName::Download)
            .disabled(!ready),
        );
        entries.push(
            MenuEntry::item("Rename", handler(this.clone(), FilesView::rename))
                .icon(IconName::Pencil)
                .shortcut("F2")
                .disabled(!ready || selection != 1),
        );
        entries.push(
            MenuEntry::item("Move to", handler(this.clone(), FilesView::move_selected))
                .icon(IconName::FolderUp)
                .disabled(!ready),
        );
        entries.extend(cut_copy_entries(this, ready));
        entries.extend(self.archive_entries(cx));
        // One entry the server gave permissions for: its grid of toggles, under its cell.
        if selection == 1 && entry.permissions.is_some() {
            let path = entry.path.clone();
            entries.push(
                MenuEntry::item(
                    "Permissions…",
                    handler(this.clone(), move |v, _, cx| {
                        v.open_perms(path.clone(), position, cx)
                    }),
                )
                .icon(IconName::Lock)
                .disabled(!ready),
            );
        }
        // Typed in octal: also for several entries, and for the special bits.
        entries.push(
            MenuEntry::item(
                "Permissions in octal…",
                handler(this.clone(), FilesView::change_permissions),
            )
            .icon(IconName::Settings)
            .disabled(!ready),
        );
        entries.push(
            MenuEntry::item(
                "Copy path",
                handler(this.clone(), |v, _, cx| v.copy_paths(cx)),
            )
            .icon(IconName::Copy),
        );
        entries.push(MenuEntry::Separator);
        entries
    }

    /// What creates something in this folder.
    fn add_entries(&self, cx: &mut Context<Self>) -> Vec<MenuEntry> {
        let this = cx.entity().downgrade();
        let ready = self.ready(cx);
        vec![
            MenuEntry::item("New file", handler(this.clone(), FilesView::new_file))
                .icon(IconName::FilePlus)
                .disabled(!ready),
            MenuEntry::item("New folder", handler(this, FilesView::new_folder))
                .icon(IconName::FolderPlus)
                .disabled(!ready),
        ]
    }

    /// What brings something from this PC into this folder.
    fn upload_entries(&self, cx: &mut Context<Self>) -> Vec<MenuEntry> {
        let this = cx.entity().downgrade();
        let ready = self.ready(cx);
        vec![
            MenuEntry::item(
                "Upload files",
                handler(this.clone(), |v, _, cx| v.pick_and_upload(false, cx)),
            )
            .icon(IconName::Upload)
            .disabled(!ready),
            MenuEntry::item(
                "Upload folder",
                handler(this, |v, _, cx| v.pick_and_upload(true, cx)),
            )
            .icon(IconName::FolderUp)
            .disabled(!ready),
        ]
    }

    /// What acts on the folder, behind the one menu button of the card: what is used most first.
    fn folder_entries(&self, cx: &mut Context<Self>) -> Vec<MenuEntry> {
        let this = cx.entity().downgrade();
        let connected = self.session_connected(cx);
        let ready = !self.path.is_empty() && connected;
        // A check says what is on; what is off shows nothing there.
        let checked = |entry: MenuEntry, on: bool| {
            if on {
                entry.icon(IconName::Check)
            } else {
                entry
            }
        };
        let mut entries = self.add_entries(cx);
        entries.extend(self.upload_entries(cx));
        entries.extend([
            MenuEntry::Separator,
            checked(
                MenuEntry::item(
                    "Follow terminal",
                    handler(this.clone(), |_, _, cx| cx.emit(FilesEvent::ToggleFollow)),
                ),
                self.follows,
            ),
            {
                let here = self.path.clone();
                MenuEntry::item(
                    "Go here in terminal",
                    handler(this.clone(), move |_, _, cx: &mut Context<Self>| {
                        cx.emit(FilesEvent::GoHereInTerminal(here.clone()))
                    }),
                )
                .icon(IconName::Terminal)
                .disabled(!ready)
            },
            MenuEntry::Separator,
        ]);
        entries.extend([
            self.hidden_files_entry(this.clone()),
            MenuEntry::item("Refresh", handler(this.clone(), |v, _, cx| v.refresh(cx)))
                .icon(IconName::Refresh)
                .shortcut("F5")
                .disabled(!connected),
            MenuEntry::item(
                "Home folder",
                handler(this.clone(), |v, _, cx| {
                    v.home_requested = false;
                    v.load_home(cx);
                }),
            )
            .icon(IconName::Home)
            .disabled(!connected),
            MenuEntry::item(
                "Copy current path",
                handler(this.clone(), |v, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(v.path.clone()))
                }),
            )
            .icon(IconName::Copy),
            MenuEntry::Separator,
            MenuEntry::item("Paste", handler(this, |v, _, cx| v.paste_picked(cx)))
                .icon(IconName::Copy)
                .shortcut("Ctrl+V")
                .disabled(!ready || self.picked.is_none()),
        ]);
        entries
    }

    /// What acts on the selection, besides the buttons of the selection bar.
    fn selection_entries(&self, cx: &mut Context<Self>) -> Vec<MenuEntry> {
        let this = cx.entity().downgrade();
        let ready = self.ready(cx);
        let mut entries = Vec::new();
        let single = (self.selected.len() == 1)
            .then(|| {
                self.entries
                    .iter()
                    .position(|e| self.selected.contains(&e.path))
            })
            .flatten();
        if let Some((row, entry)) = single.and_then(|i| Some((i, self.entries.get(i)?))) {
            let open = |label: &'static str, icon| {
                MenuEntry::item(
                    label,
                    handler(this.clone(), move |v, _, cx| v.open(row, cx)),
                )
                .icon(icon)
                .shortcut("Enter")
            };
            match entry.kind {
                EntryKind::Dir | EntryKind::Symlink => entries.push(open("Open", IconName::Folder)),
                _ => entries.push(open("Edit", IconName::File).disabled(!ready)),
            }
            entries.push(MenuEntry::Separator);
        }
        entries.extend(cut_copy_entries(&this, ready));
        entries.extend([
            MenuEntry::item("Move to", handler(this.clone(), FilesView::move_selected))
                .icon(IconName::FolderUp)
                .disabled(!ready),
            MenuEntry::item(
                "Permissions in octal…",
                handler(this.clone(), FilesView::change_permissions),
            )
            .icon(IconName::Settings)
            .disabled(!ready),
            MenuEntry::item("Copy path", handler(this, |v, _, cx| v.copy_paths(cx)))
                .icon(IconName::Copy),
        ]);
        entries.extend(self.archive_entries(cx));
        entries
    }

    /// Shows or hides the dot files.
    fn hidden_files_entry(&self, this: WeakEntity<Self>) -> MenuEntry {
        MenuEntry::item(
            if self.show_hidden {
                "Hide hidden files"
            } else {
                "Show hidden files"
            },
            handler(this, |v, _, cx| v.toggle_hidden(cx)),
        )
        .icon(if self.show_hidden {
            IconName::EyeOff
        } else {
            IconName::Eye
        })
        .shortcut("Ctrl+H")
    }

    /// What makes an archive of the selection, or opens the archive selected.
    fn archive_entries(&self, cx: &mut Context<Self>) -> Vec<MenuEntry> {
        let this = cx.entity().downgrade();
        let ready = self.ready(cx);
        let mut entries = vec![
            MenuEntry::item("Compress", handler(this.clone(), FilesView::compress))
                .icon(IconName::FilePlus)
                .disabled(!ready),
        ];
        if let [only] = self.selected_entries().as_slice()
            && only.kind == EntryKind::File
            && archive_stem(&only.name).is_some()
        {
            entries.push(
                MenuEntry::item("Extract here", handler(this, |v, _, cx| v.extract(cx)))
                    .icon(IconName::FolderPlus)
                    .disabled(!ready),
            );
        }
        entries
    }

    fn copy_paths(&mut self, cx: &mut Context<Self>) {
        let text = self.selected_paths().join("\n");
        if !text.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    // ---- selection ----

    fn selected_entries(&self) -> Vec<RemoteEntry> {
        self.entries
            .iter()
            .filter(|e| self.selected.contains(&e.path))
            .cloned()
            .collect()
    }

    /// Starts what a modal asked for, unless the view has moved to another connection meanwhile,
    /// and waits for the answers under `label`.
    fn run_op<I: IntoIterator<Item = RequestId>>(
        &mut self,
        session: SessionId,
        label: &'static str,
        cx: &mut Context<Self>,
        start: impl FnOnce(&mut Self, &EngineHandle, &mut Context<Self>) -> I,
    ) {
        if self.session != session {
            return;
        }
        let engine = self.state.read(cx).engine.clone();
        for req in start(self, &engine, cx) {
            self.pending_ops.insert(req, label);
        }
    }

    fn selected_paths(&self) -> Vec<String> {
        self.entries
            .iter()
            .filter(|e| self.selected.contains(&e.path))
            .map(|e| e.path.clone())
            .collect()
    }

    fn on_row_click(
        &mut self,
        index: usize,
        event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The list background clears the selection on click; rows must not reach it.
        cx.stop_propagation();
        window.focus(&self.focus);
        let Some(entry) = self.entries.get(index) else {
            return;
        };
        let modifiers = event.modifiers();
        self.cursor = Some(index);
        if modifiers.shift
            && let Some(anchor) = self.anchor
        {
            let (from, to) = (anchor.min(index), anchor.max(index));
            self.selected = self.entries[from..=to]
                .iter()
                .map(|e| e.path.clone())
                .collect();
        } else if modifiers.control || modifiers.platform {
            if !self.selected.remove(&entry.path) {
                self.selected.insert(entry.path.clone());
            }
            self.anchor = Some(index);
        } else {
            self.selected = HashSet::from([entry.path.clone()]);
            self.anchor = Some(index);
            if event.click_count() >= 2 {
                self.open(index, cx);
            }
        }
        cx.notify();
    }

    /// Folders are entered, files open in the editor.
    fn open(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(entry) = self.entries.get(index) else {
            return;
        };
        let (kind, path) = (entry.kind, entry.path.clone());
        match kind {
            EntryKind::Dir => self.navigate(path, cx),
            // It may point to a file or to a folder: the server knows which.
            EntryKind::Symlink if self.session_connected(cx) => {
                let req = self
                    .state
                    .read(cx)
                    .engine
                    .entry_kind(self.session, path.clone());
                self.pending_links.insert(req, path);
            }
            EntryKind::File if self.session_connected(cx) => {
                self.state.read(cx).engine.edit_file(self.session, path)
            }
            _ => {}
        }
    }

    /// How many rows fit on screen.
    fn page_rows(&self) -> usize {
        let height = self.list_scroll.0.borrow().base_handle.bounds().size.height;
        ((f32::from(height) / f32::from(row_px(ROW_H))) as usize)
            .saturating_sub(1)
            .max(1)
    }

    /// The row the keys start from: the cursor, or the first selected.
    fn cursor_row(&self) -> Option<usize> {
        self.cursor
            .filter(|at| *at < self.entries.len())
            .or_else(|| {
                self.entries
                    .iter()
                    .position(|e| self.selected.contains(&e.path))
            })
    }

    /// Puts the cursor on a row. With `extend` the selection reaches it from the anchor.
    fn put_cursor(&mut self, at: usize, extend: bool, cx: &mut Context<Self>) {
        let Some(entry) = self.entries.get(at) else {
            return;
        };
        if extend {
            let anchor = *self.anchor.get_or_insert(self.cursor.unwrap_or(at));
            let (from, to) = (anchor.min(at), anchor.max(at));
            self.selected = self.entries[from..=to]
                .iter()
                .map(|e| e.path.clone())
                .collect();
        } else {
            self.selected = HashSet::from([entry.path.clone()]);
            self.anchor = Some(at);
        }
        self.cursor = Some(at);
        // A row already on screen stays where it is.
        self.list_scroll.scroll_to_item(at, ScrollStrategy::Top);
        cx.notify();
    }

    /// A letter jumps to the name starting with it, and more letters narrow it down.
    fn type_ahead(&mut self, text: &str, cx: &mut Context<Self>) {
        if self.typed_at.is_none_or(|at| at.elapsed() > TYPE_AHEAD) {
            self.typed.clear();
        }
        self.typed_at = Some(Instant::now());
        self.typed.push_str(text);
        let names = self.entries.iter().map(|e| e.name.as_str());
        if let Some(at) = first_starting_with(names, &self.typed) {
            self.put_cursor(at, false, cx);
        }
    }

    fn toggle_hidden(&mut self, cx: &mut Context<Self>) {
        self.state.update(cx, AppState::toggle_hidden_files);
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = &event.keystroke;
        if key.key == "escape" && self.menu.take().is_some() {
            cx.notify();
            cx.stop_propagation();
            return;
        }
        // What floats over the list goes first, the topmost before the other.
        if key.key == "escape" && (self.perms.is_some() || self.looking) {
            if self.perms.take().is_none() {
                self.close_look(cx);
            }
            cx.notify();
            cx.stop_propagation();
            return;
        }
        if !self.session_connected(cx) {
            return;
        }
        match key.key.as_str() {
            // The list's own key: in the filter a space is text.
            "space"
                if self.focus.is_focused(window)
                    && !key.modifiers.control
                    && !key.modifiers.alt =>
            {
                if self.looking {
                    self.close_look(cx);
                } else {
                    self.open_look(false, cx);
                }
            }
            "escape" if !self.selected.is_empty() => self.clear_selection(cx),
            "escape" if self.search.is_some() => self.close_search(cx),
            "enter" if self.selected.len() == 1 => {
                if let Some(index) = self
                    .entries
                    .iter()
                    .position(|e| self.selected.contains(&e.path))
                {
                    self.open(index, cx);
                }
            }
            "h" if key.modifiers.control => self.toggle_hidden(cx),
            "backspace" => self.go_up(cx),
            "left" if key.modifiers.alt => self.go_back(cx),
            "f" if key.modifiers.control => self.open_filter(window, cx),
            "f5" => self.refresh(cx),
            "f2" => self.rename(window, cx),
            "delete" => self.delete(window, cx),
            "up" | "down" | "home" | "end" | "pageup" | "pagedown"
                if !key.modifiers.control && !key.modifiers.alt =>
            {
                // The permissions belong to the row the cursor leaves; the preview follows it.
                self.perms = None;
                let page = self.page_rows();
                if let Some(at) =
                    moved_cursor(&key.key, self.cursor_row(), self.entries.len(), page)
                {
                    self.put_cursor(at, key.modifiers.shift, cx);
                }
            }
            "c" if key.modifiers.control && key.modifiers.shift => self.copy_paths(cx),
            "c" if key.modifiers.control => self.pick_selected(false, cx),
            "x" if key.modifiers.control => self.pick_selected(true, cx),
            "v" if key.modifiers.control => self.paste_picked(cx),
            "a" if key.modifiers.control => {
                self.selected = self.entries.iter().map(|e| e.path.clone()).collect();
                cx.notify();
            }
            _ => {
                // In a field of the toolbar a key is text for that field: keeping it here would
                // also stop the platform from delivering the character.
                if !self.focus.is_focused(window) {
                    return;
                }
                let typed = key
                    .key_char
                    .as_deref()
                    .filter(|_| !key.modifiers.control && !key.modifiers.alt)
                    .filter(|text| !text.trim().is_empty() && !text.chars().any(char::is_control));
                let Some(text) = typed.map(str::to_string) else {
                    return;
                };
                self.type_ahead(&text, cx);
            }
        }
        cx.stop_propagation();
    }

    // ---- toolbar actions ----

    fn clear_selection(&mut self, cx: &mut Context<Self>) {
        self.selected.clear();
        self.anchor = None;
        cx.notify();
    }

    fn open_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.filter_open = true;
        self.filter_input.read(cx).focus(window);
        cx.notify();
    }

    fn go_up(&mut self, cx: &mut Context<Self>) {
        if self.path != "/" && !self.path.is_empty() {
            self.navigate(files_format::parent(&self.path), cx);
        }
    }

    fn go_back(&mut self, cx: &mut Context<Self>) {
        if let Some(path) = self.history.pop() {
            self.going_back = true;
            self.navigate(path, cx);
        }
    }

    fn upload_paths_to(&mut self, paths: Vec<PathBuf>, dir: String, cx: &mut Context<Self>) {
        if dir.is_empty() || !self.session_connected(cx) {
            return;
        }
        let engine = self.state.read(cx).engine.clone();
        for path in paths {
            let id = engine.upload(self.session, path, dir.clone());
            self.uploads.insert(id, dir.clone());
        }
    }

    fn pick_and_upload(&mut self, directories: bool, cx: &mut Context<Self>) {
        let session = self.session;
        let dir = self.path.clone();
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: !directories,
            directories,
            multiple: true,
            prompt: Some("Upload".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = picked.await {
                this.update(cx, |this, cx| {
                    if this.session == session {
                        this.upload_paths_to(paths, dir, cx);
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    /// Keeps the selection to paste it elsewhere, moving it if `cut`.
    fn pick_selected(&mut self, cut: bool, cx: &mut Context<Self>) {
        let paths = self.selected_paths();
        if !paths.is_empty() {
            self.picked = Some((paths, cut));
            cx.notify();
        }
    }

    /// Brings what was picked to the folder on screen. A copy gets a free name, but a move never
    /// replaces or renames anything.
    fn paste_picked(&mut self, cx: &mut Context<Self>) {
        let Some((paths, cut)) = self.picked.clone() else {
            return;
        };
        if self.path.is_empty() {
            return;
        }
        let mut taken: HashSet<String> = self.all_entries.iter().map(|e| e.name.clone()).collect();
        let engine = self.state.read(cx).engine.clone();
        for from in paths {
            let (folder, name) = split_path(&from);
            let name = name.to_string();
            if is_inside(&self.path, &from) {
                self.notice(
                    NoticeLevel::Warning,
                    format!("{name} cannot go inside itself"),
                    cx,
                );
                continue;
            }
            if cut && folder == self.path {
                continue;
            }
            if cut && taken.contains(&name) {
                self.notice(
                    NoticeLevel::Warning,
                    format!("{name} was not moved: there is already one here"),
                    cx,
                );
                continue;
            }
            let target = copy_name(&name, &taken);
            let to = files_format::join(&self.path, &target);
            taken.insert(target);
            let (req, label) = if cut {
                (engine.rename(self.session, from, to), "Move")
            } else {
                (engine.copy(self.session, from, to), "Copy")
            };
            self.pending_ops.insert(req, label);
        }
        // What moved is no longer there to move again; a copy can be pasted many times.
        if cut {
            self.picked = None;
        }
        cx.notify();
    }

    /// Asks for a folder and moves the selection there. Nothing gets replaced: a server refuses to
    /// rename onto a name in use, and that is the error the user reads.
    fn move_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = self.selected_paths();
        if paths.is_empty() {
            return;
        }
        let title = match paths.as_slice() {
            [only] => format!("Move {} to folder", split_path(only).1),
            many => format!("Move {} items to folder", many.len()),
        };
        let (this, session, state) = (cx.entity(), self.session, self.state.clone());
        let start = self.path.clone();
        let sources = paths.clone();
        let refusal = move |folder: &str| {
            if sources.iter().any(|from| is_inside(folder, from)) {
                Some("A folder cannot go inside itself")
            } else if sources.iter().all(|from| split_path(from).0 == folder) {
                Some("Already in this folder")
            } else {
                None
            }
        };
        let picker = cx.new(|cx| {
            FolderPicker::new(
                state,
                session,
                title,
                "Move here",
                start,
                refusal,
                move |folder, _, cx| {
                    this.update(cx, |this, cx| {
                        this.run_op(session, "Move", cx, |_, engine, _| {
                            paths
                                .into_iter()
                                .filter_map(|from| {
                                    let (origin, name) = split_path(&from);
                                    if origin == folder {
                                        return None;
                                    }
                                    let to = files_format::join(&folder, name);
                                    Some(engine.rename(session, from, to))
                                })
                                .collect::<Vec<_>>()
                        });
                    });
                },
                window,
                cx,
            )
        });
        self.modals
            .update(cx, |layer, cx| layer.show(picker, window, cx));
    }

    /// Asks for a name and makes an archive of the selection in this folder, with `tar` or `zip` on
    /// the server, which picks by the ending of the name.
    fn compress(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let selected = self.selected_entries();
        let suggested = match selected.as_slice() {
            [] => return,
            [only] => only.name.clone(),
            _ => self
                .folder_name()
                .filter(|name| *name != "/")
                .unwrap_or("archive")
                .to_string(),
        };
        let inside = format!("{}/", self.path.trim_end_matches('/'));
        let names: Vec<String> = selected
            .iter()
            .map(|entry| {
                entry
                    .path
                    .strip_prefix(&inside)
                    .unwrap_or(&entry.name)
                    .to_string()
            })
            .collect();
        let (this, session, folder) = (cx.entity(), self.session, self.path.clone());
        let prompt = cx.new(|cx| {
            TextPrompt::new(
                "Compress to archive (.tar.gz, .zip…)",
                "Compress",
                &format!("{suggested}.tar.gz"),
                move |name, _, cx| {
                    this.update(cx, |this, cx| {
                        this.run_op(session, "Compress", cx, |this, engine, cx| {
                            let name = if archive_stem(&name).is_some() {
                                name
                            } else {
                                format!("{name}.tar.gz")
                            };
                            let archive = files_format::join(&folder, &name);
                            this.notice(NoticeLevel::Info, format!("Compressing to {name}…"), cx);
                            Some(engine.compress(session, folder, names, archive))
                        });
                    });
                },
                window,
                cx,
            )
        });
        self.modals
            .update(cx, |layer, cx| layer.show(prompt, window, cx));
    }

    /// Opens the selected archive in a new folder next to it, so that nothing gets replaced.
    fn extract(&mut self, cx: &mut Context<Self>) {
        let selected = self.selected_entries();
        let [only] = selected.as_slice() else { return };
        let Some(stem) = archive_stem(&only.name) else {
            return;
        };
        let taken: HashSet<String> = self.all_entries.iter().map(|e| e.name.clone()).collect();
        let name = copy_name(stem, &taken);
        let folder = files_format::join(&files_format::parent(&only.path), &name);
        self.notice(NoticeLevel::Info, format!("Extracting to {name}…"), cx);
        let req = self
            .state
            .read(cx)
            .engine
            .extract(self.session, only.path.clone(), folder);
        self.pending_ops.insert(req, "Extract");
    }

    fn change_permissions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let selected = self.selected_entries();
        let Some(first) = selected.first() else {
            return;
        };
        let current = first.permissions.map_or(0o644, |mode| mode & 0o7777);
        let title = match selected.as_slice() {
            [only] => format!("Permissions of {}, in octal", only.name),
            many => format!("Permissions of {} items, in octal", many.len()),
        };
        let paths = self.selected_paths();
        let (this, session) = (cx.entity(), self.session);
        let prompt = cx.new(|cx| {
            TextPrompt::new(
                &title,
                "Apply",
                &format!("{current:o}"),
                move |text, _, cx| {
                    this.update(cx, |this, cx| {
                        let Some(mode) = parse_mode(&text) else {
                            this.notice(
                                NoticeLevel::Warning,
                                format!(
                                    "{text} is not a permission: try something like 644 or 755"
                                ),
                                cx,
                            );
                            return;
                        };
                        this.run_op(session, "Change permissions", cx, |_, engine, _| {
                            Some(engine.chmod(session, paths, mode))
                        });
                    });
                },
                window,
                cx,
            )
            .any_text()
        });
        self.modals
            .update(cx, |layer, cx| layer.show(prompt, window, cx));
    }

    fn download(&mut self, cx: &mut Context<Self>) {
        let selected = self.selected_entries();
        if selected.is_empty() {
            return;
        }
        let session = self.session;
        // A chosen folder that is gone, say on a drive not plugged in, falls back on asking.
        let folder = PathBuf::from(&self.state.read(cx).settings.download_folder);
        if folder.is_dir() {
            let engine = self.state.read(cx).engine.clone();
            for entry in selected {
                engine.download(session, entry.path, folder.clone());
            }
            return;
        }
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Download here".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(dirs))) = picked.await
                && let Some(dir) = dirs.into_iter().next()
            {
                this.update(cx, |this, cx| {
                    let engine = this.state.read(cx).engine.clone();
                    for entry in selected {
                        engine.download(session, entry.path, dir.clone());
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    fn new_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.prompt_new(
            "New file",
            "Create file",
            EngineHandle::create_file,
            window,
            cx,
        );
    }

    fn new_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.prompt_new(
            "New folder",
            "Create folder",
            EngineHandle::mkdir,
            window,
            cx,
        );
    }

    /// Asks for a name, then creates it in this folder.
    fn prompt_new(
        &mut self,
        title: &'static str,
        operation: &'static str,
        create: fn(&EngineHandle, SessionId, String) -> RequestId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let this = cx.entity();
        let dir = self.path.clone();
        let session = self.session;
        let prompt = cx.new(|cx| {
            TextPrompt::new(
                title,
                "Create",
                "",
                move |name, _, cx| {
                    this.update(cx, |this, cx| {
                        this.run_op(session, operation, cx, |_, engine, _| {
                            Some(create(engine, session, files_format::join(&dir, &name)))
                        });
                    });
                },
                window,
                cx,
            )
        });
        self.modals
            .update(cx, |layer, cx| layer.show(prompt, window, cx));
    }

    fn rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let selected = self.selected_entries();
        let [entry] = selected.as_slice() else { return };
        let this = cx.entity();
        // Its own folder: among search results that is not the one on screen.
        let (from, dir) = (entry.path.clone(), files_format::parent(&entry.path));
        let session = self.session;
        let prompt = cx.new(|cx| {
            TextPrompt::new(
                "Rename",
                "Rename",
                &entry.name,
                move |name, _, cx| {
                    this.update(cx, |this, cx| {
                        this.run_op(session, "Rename", cx, |_, engine, _| {
                            Some(engine.rename(session, from, files_format::join(&dir, &name)))
                        });
                    });
                },
                window,
                cx,
            )
        });
        self.modals
            .update(cx, |layer, cx| layer.show(prompt, window, cx));
    }

    fn delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let selected = self.selected_entries();
        if selected.is_empty() {
            return;
        }
        let paths = self.selected_paths();
        let this = cx.entity();
        let session = self.session;
        let run = move |cx: &mut App| {
            this.update(cx, |this, cx| {
                this.run_op(session, "Delete", cx, |_, engine, _| {
                    paths
                        .into_iter()
                        .map(|path| engine.remove(session, path, true))
                        .collect::<Vec<_>>()
                });
            });
        };
        if !self.state.read(cx).settings.confirm_on_delete {
            run(cx);
            return;
        }
        let message = match selected.as_slice() {
            [one] => format!(
                "Delete \"{}\"{}?",
                one.name,
                if one.kind == EntryKind::Dir {
                    " and everything in it"
                } else {
                    ""
                }
            ),
            many => format!(
                "Delete {} items? Folders are removed with their contents.",
                many.len()
            ),
        };
        let confirm =
            cx.new(|cx| ConfirmModal::new("Delete", message, "Delete", move |_, cx| run(cx), cx));
        self.modals
            .update(cx, |layer, cx| layer.show(confirm, window, cx));
    }

    // ---- rendering ----

    /// A button with a menu under it. A press while its menu is open only closes it.
    fn menu_button(&self, which: ToolbarMenu, button: Button, cx: &mut Context<Self>) -> Div {
        let anchor = match which {
            ToolbarMenu::Folder => &self.more_anchor,
            ToolbarMenu::Selection => &self.selection_anchor,
        };
        anchor
            .wrap(button.on_click(cx.listener(move |this, _, window, cx| {
                if this.closing_menu == Some(which) {
                    return;
                }
                window.focus(&this.focus);
                let (anchor, entries) = match which {
                    ToolbarMenu::Folder => (this.more_anchor.clone(), this.folder_entries(cx)),
                    ToolbarMenu::Selection => {
                        (this.selection_anchor.clone(), this.selection_entries(cx))
                    }
                };
                this.menu = Some(OpenMenu::below(&anchor, entries));
                this.menu_button = Some(which);
                cx.notify();
            })))
            .capture_any_mouse_down(cx.listener(|this, _, _, _| {
                this.closing_menu = this.menu.as_ref().and(this.menu_button);
            }))
    }

    /// Back and up, at the start of the header of its card.
    pub(crate) fn nav_buttons(&self, cx: &mut Context<Self>) -> AnyElement {
        let connected = self.session_connected(cx);
        let ready = !self.path.is_empty() && connected;
        div()
            .id("files-nav")
            .flex()
            .flex_none()
            .items_center()
            .gap(px(1.))
            .child(
                Button::new("back")
                    .icon(IconName::ArrowLeft)
                    .tooltip("Back (Alt+Left)")
                    .style(ButtonStyle::Ghost)
                    .disabled(!connected || self.history.is_empty())
                    .on_click(cx.listener(|this, _, _, cx| this.go_back(cx))),
            )
            .child(
                Button::new("up")
                    .icon(IconName::ArrowUp)
                    .tooltip("Parent folder (Backspace)")
                    .style(ButtonStyle::Ghost)
                    .disabled(!ready || self.path == "/")
                    .on_click(cx.listener(|this, _, _, cx| this.go_up(cx))),
            )
            .into_any_element()
    }

    /// What acts on the folder, at the end of the header of its card.
    pub(crate) fn tools(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(1.))
            .child(
                div()
                    .w(px(160.))
                    .mr(px(4.))
                    .flex_none()
                    .child(self.filter_input.clone()),
            )
            .child(
                self.menu_button(
                    ToolbarMenu::Folder,
                    Button::new("folder-more")
                        .icon(IconName::More)
                        .tooltip("New, upload, details and more")
                        .style(ButtonStyle::Ghost),
                    cx,
                ),
            )
            .into_any_element()
    }

    /// Says what a search of the subfolders is doing, and ends it.
    fn search_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let search = self.search.as_ref()?;
        let theme = cx.theme().clone();
        let count = search.found.len();
        let found = if count == 1 {
            "1 found".to_string()
        } else {
            format!("{count} found")
        };
        let text = if search.running {
            format!("Searching \"{}\" in {}… {found}", search.query, search.root)
        } else if search.limited {
            format!(
                "{found} for \"{}\" in {}. The search stopped there, so there may be more",
                search.query, search.root
            )
        } else {
            format!("{found} for \"{}\" in {}", search.query, search.root)
        };
        let can_widen = !search.running && !search.skipped.is_empty();
        let text = if can_widen {
            format!("{text}. Skipped {}", search.skipped.join(", "))
        } else {
            text
        };
        let running = search.running;
        Some(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(px(8.))
                .h(row_px(32.))
                .px(px(12.))
                .border_b_1()
                .border_color(theme.border)
                .text_size(crate::theme::ui_px(13.))
                .text_color(theme.text_muted)
                .when(running, |this| {
                    this.child(Icon::new(IconName::Spinner).color(theme.accent).spin())
                })
                .child(div().flex_1().min_w_0().truncate().child(text))
                .when(can_widen, |this| {
                    this.child(
                        Button::new("search-everywhere")
                            .label("Search everywhere")
                            .style(ButtonStyle::Ghost)
                            .compact()
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(search) = this.search.as_ref() {
                                    let query = search.query.clone();
                                    this.start_search(query, true, cx);
                                }
                            })),
                    )
                })
                .when(running, |this| {
                    this.child(
                        Button::new("search-stop")
                            .label("Stop")
                            .style(ButtonStyle::Ghost)
                            .compact()
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(search) = this.search.as_mut() {
                                    search.running = false;
                                    let req = search.req;
                                    this.state.read(cx).engine.cancel_search(req);
                                }
                                cx.notify();
                            })),
                    )
                })
                .child(
                    Button::new("search-close")
                        .icon(IconName::Close)
                        .tooltip("Back to the folder (Esc)")
                        .style(ButtonStyle::Ghost)
                        .compact()
                        .on_click(cx.listener(|this, _, _, cx| this.close_search(cx))),
                )
                .into_any_element(),
        )
    }

    /// What acts on several selected files. For one, that is in the details.
    fn selection_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let count = self.selected.len();
        let ready = self.ready(cx);
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(4.))
            .h(row_px(40.))
            .px(px(8.))
            .border_b_1()
            .border_color(theme.border)
            .child(
                Button::new("clear-selection")
                    .icon(IconName::Close)
                    .tooltip("Clear selection (Esc)")
                    .style(ButtonStyle::Ghost)
                    .on_click(cx.listener(|this, _, _, cx| this.clear_selection(cx))),
            )
            .child(
                div()
                    .flex_none()
                    .ml(px(4.))
                    .text_size(crate::theme::ui_px(13.))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .child(format!("{count} selected")),
            )
            .children(self.selected_size().map(|size| {
                div()
                    .flex_none()
                    .text_size(crate::theme::ui_px(13.))
                    .text_color(theme.text_muted)
                    .child(size)
            }))
            .child(div().flex_1().min_w_0())
            .child(
                Button::new("download")
                    .icon(IconName::Download)
                    .label("Download")
                    .style(ButtonStyle::Ghost)
                    .disabled(!ready)
                    .on_click(cx.listener(|this, _, _, cx| this.download(cx))),
            )
            .when(count == 1, |this| {
                this.child(
                    Button::new("rename")
                        .icon(IconName::Pencil)
                        .label("Rename")
                        .tooltip("Rename (F2)")
                        .style(ButtonStyle::Ghost)
                        .disabled(!ready)
                        .on_click(cx.listener(|this, _, window, cx| this.rename(window, cx))),
                )
            })
            .child(
                Button::new("delete")
                    .icon(IconName::Trash)
                    .label("Delete")
                    .tooltip("Delete (Del)")
                    .style(ButtonStyle::GhostDanger)
                    .disabled(!ready)
                    .on_click(cx.listener(|this, _, window, cx| this.delete(window, cx))),
            )
            .child(
                self.menu_button(
                    ToolbarMenu::Selection,
                    Button::new("selection-more")
                        .icon(IconName::More)
                        .tooltip("More")
                        .style(ButtonStyle::Ghost),
                    cx,
                )
                .ml(px(8.)),
            )
    }

    /// The current path as clickable folders.
    pub(crate) fn crumbs(&self, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let hover = theme.track;
        let mut folders: Vec<(String, String)> = Vec::new();
        let mut current = String::new();
        for name in self.path.split('/').filter(|name| !name.is_empty()) {
            current.push('/');
            current.push_str(name);
            folders.push((name.to_string(), current.clone()));
        }
        // Long paths keep the root and the last folders; the tooltip has all of it.
        let hidden = folders.len().saturating_sub(4);
        let last = folders.len().checked_sub(1);
        let connected = self.session_connected(cx);
        let press = theme.active;
        let segment = |index: usize, label: String, target: String| {
            div()
                .id(("crumb", index))
                .flex_none()
                .px(px(6.))
                .py(px(3.))
                .rounded(px(crate::theme::RADIUS_SMALL))
                .cursor_pointer()
                .hover(move |s| s.bg(hover))
                .active(move |s| s.bg(press))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if connected {
                        this.navigate(target.clone(), cx);
                    }
                }))
                .child(label)
        };
        let separator = || {
            div()
                .flex_none()
                .font_family(crate::theme::mono_font())
                .text_size(crate::theme::ui_px(13.))
                .text_color(theme.text_disabled)
                .child("/")
        };
        let mut crumbs: Vec<AnyElement> =
            vec![segment(0, "/".to_string(), "/".to_string()).into_any_element()];
        if hidden > 0 {
            crumbs.push(div().flex_none().px(px(5.)).child("…").into_any_element());
        }
        for (index, (label, target)) in folders.into_iter().enumerate().skip(hidden) {
            // The root is a slash already: "/ home / matteo".
            if index > 0 {
                crumbs.push(separator().into_any_element());
            }
            let is_last = Some(index) == last;
            crumbs.push(
                segment(index + 1, label, target)
                    .when(is_last, |this| {
                        this.font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(theme.text)
                    })
                    .into_any_element(),
            );
        }
        div()
            .id("breadcrumb")
            .min_w_0()
            .h(px(28.))
            .gap(px(1.))
            .flex()
            .items_center()
            .overflow_hidden()
            .rounded(px(crate::theme::RADIUS_SMALL))
            .text_size(crate::theme::ui_px(13.))
            .text_color(theme.text_muted)
            .tooltip(crate::widgets::tooltip::tooltip(self.path.clone()))
            .children(crumbs)
            .into_any_element()
    }

    fn render_row(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let entry = &self.entries[index];
        let selected = self.selected.contains(&entry.path);
        let shown_name = match &self.search {
            Some(search) => search.relative(entry).to_string(),
            None => entry.name.clone(),
        };
        let (hover, press) = (theme.hover, theme.active);
        let drop_target = gpui::Hsla {
            a: 0.35,
            ..theme.accent
        };
        let width = self.width.get();
        // A link keeps its own icon: being one matters more than what it points to.
        let themed = match entry.kind {
            EntryKind::Symlink => None,
            EntryKind::Dir => file_icon(&entry.name, true, cx),
            _ => file_icon(&entry.name, false, cx),
        };
        let mark = entry_mark(entry, themed, &theme);
        // The rows of the list fill it, and each keeps a little room from its sides.
        div()
            .w_full()
            .h(row_px(ROW_H))
            .px(px(6.))
            .child(
                div()
                    .id(("file-row", index))
                    .size_full()
                    .pl(px(6.))
                    .pr(px(8.))
                    .flex()
                    .items_center()
                    .gap(px(COLUMN_GAP))
                    .rounded(px(crate::theme::RADIUS_CONTROL))
                    .cursor_pointer()
                    // Cut: it is on its way out, as in Explorer.
                    .when(
                        self.picked
                            .as_ref()
                            .is_some_and(|(paths, cut)| *cut && paths.contains(&entry.path)),
                        |this| this.opacity(0.5),
                    )
                    .when(selected, |this| this.bg(theme.selected))
                    .when(!selected, |this| {
                        this.hover(move |s| s.bg(hover))
                            .active(move |s| s.bg(press))
                    })
                    // Dropped on a folder, files go inside it instead of where the list is.
                    .when(entry.kind == EntryKind::Dir, |this| {
                        let target = entry.path.clone();
                        this.drag_over::<ExternalPaths>(move |s, _, _, _| s.bg(drop_target))
                            .on_drop(cx.listener(move |this, paths: &ExternalPaths, _, cx| {
                                this.upload_paths_to(paths.paths().to_vec(), target.clone(), cx);
                            }))
                    })
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        this.on_row_click(index, event, window, cx)
                    }))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            this.on_row_right_click(index, event, window, cx)
                        }),
                    )
                    .child(name_cell(entry, mark, shown_name))
                    .child(size_cell(entry, &theme))
                    .when(width >= MODIFIED_FROM, |this| {
                        this.child(modified_cell(entry, index, &theme))
                    })
                    .when(width >= PERMS_FROM, |this| {
                        this.child(self.perm_cell(entry, index, &theme, cx))
                    }),
            )
            .into_any_element()
    }

    /// What to click to change the permissions: a small control, with the popover under it.
    fn perm_cell(
        &self,
        entry: &RemoteEntry,
        index: usize,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Div {
        let cells = self.perm_cells.clone();
        let key = entry.path.clone();
        let path = entry.path.clone();
        let open = self.perms.as_ref().is_some_and(|open| open.path == path);
        let known = entry.permissions.is_some();
        let (hover, raised) = (theme.hover, theme.active);
        div().w(px(PERMS_W)).flex().flex_none().child(
            div()
                .flex_none()
                .on_children_prepainted(move |children, _, _| {
                    if let Some(cell) = children.first() {
                        cells.borrow_mut().insert(key.clone(), *cell);
                    }
                })
                .child(
                    div()
                        .id(("file-perms", index))
                        .h(px(22.))
                        .ml(px(-6.))
                        .px(px(6.))
                        .flex()
                        .items_center()
                        .gap(px(5.))
                        .rounded(px(crate::theme::RADIUS_SMALL))
                        .font_family(crate::theme::mono_font())
                        .text_size(crate::theme::ui_px(13.))
                        .when(open, |this| this.bg(raised))
                        .when(known, |this| {
                            this.cursor_pointer()
                                .hover(move |style| style.bg(hover))
                                .tooltip(crate::widgets::tooltip::tooltip("Change permissions"))
                                .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                                    cx.stop_propagation();
                                    this.selected = HashSet::from([path.clone()]);
                                    this.anchor = Some(index);
                                    this.open_perms(path.clone(), event.position(), cx);
                                }))
                        })
                        .children(permission_triples(entry, theme)),
                ),
        )
    }
}

/// The icon of an entry, or for a file without one a label of its type.
fn entry_mark(entry: &RemoteEntry, themed: Option<PathBuf>, theme: &Theme) -> AnyElement {
    let executable =
        entry.kind == EntryKind::File && entry.permissions.is_some_and(|mode| mode & 0o100 != 0);
    let (accent, muted, lit_fill) = (theme.accent, theme.text_muted, theme.selected);
    match (themed, entry.kind) {
        (Some(path), kind) => {
            let fallback = match kind {
                EntryKind::Dir => (IconName::Folder, accent),
                _ => (IconName::File, muted),
            };
            img(path)
                .size(px(16.))
                .with_fallback(move || Icon::new(fallback.0).color(fallback.1).into_any_element())
                .into_any_element()
        }
        (None, EntryKind::Dir) => Icon::new(IconName::Folder)
            .size(px(16.))
            .color(accent)
            .into_any_element(),
        (None, EntryKind::Symlink) => Icon::new(IconName::Link)
            .size(px(14.))
            .color(muted)
            .into_any_element(),
        (None, _) => div()
            .w(px(32.))
            .h(px(18.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(crate::theme::RADIUS_SMALL))
            .font_family(crate::theme::mono_font())
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_size(px(9.5))
            .text_color(if executable { accent } else { muted })
            .bg(if executable {
                lit_fill
            } else {
                gpui::Hsla {
                    a: 0.075,
                    ..theme.text
                }
            })
            .child(files_format::badge(&entry.name))
            .into_any_element(),
    }
}

/// The mark and the name of an entry.
fn name_cell(entry: &RemoteEntry, mark: AnyElement, shown_name: String) -> Div {
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .items_center()
        .gap(px(9.))
        // As wide as a label, so names line up whatever marks them.
        .child(
            div()
                .w(px(32.))
                .flex()
                .flex_none()
                .justify_center()
                .child(mark),
        )
        .child(
            div()
                .min_w_0()
                .truncate()
                .when(entry.kind == EntryKind::Dir, |this| {
                    this.font_weight(gpui::FontWeight::MEDIUM)
                })
                .child(shown_name),
        )
}

fn size_cell(entry: &RemoteEntry, theme: &Theme) -> Div {
    let (number, unit) = files_format::size_parts(entry);
    div()
        .w(px(SIZE_W))
        .flex()
        .flex_none()
        .justify_end()
        .gap(px(4.))
        .font_family(crate::theme::mono_font())
        .text_size(crate::theme::ui_px(13.))
        .text_color(theme.text_muted)
        .child(number)
        .child(div().text_color(theme.text_disabled).child(unit))
}

fn modified_cell(entry: &RemoteEntry, index: usize, theme: &Theme) -> Stateful<Div> {
    div()
        .id(("file-modified", index))
        .w(px(MODIFIED_W))
        .flex_none()
        .text_size(crate::theme::ui_px(13.))
        .text_color(theme.text_muted)
        .tooltip(crate::widgets::tooltip::tooltip(format!(
            "{}\n{}",
            files_format::modified(entry.modified),
            files_format::modified_utc(entry.modified),
        )))
        .child(files_format::relative(entry.modified, SystemTime::now()))
}

impl FilesView {
    fn header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let column = |key: SortKey, label: &'static str, this: &Self| {
            let hover = theme.text;
            let chevron = if this.ascending {
                IconName::ChevronUp
            } else {
                IconName::ChevronDown
            };
            div()
                .id(SharedString::from(label))
                .flex()
                .items_center()
                .gap(px(3.))
                .cursor_pointer()
                .hover(move |s| s.text_color(hover))
                .when(this.sort == key, |s| s.text_color(theme.text_muted))
                .on_click(cx.listener(move |this, _, _, cx| this.set_sort(key, cx)))
                .child(label)
                .when(this.sort == key, |s| {
                    s.child(Icon::new(chevron).size(px(11.)).color(theme.text_muted))
                })
        };
        let width = self.width.get();
        div()
            .h(row_px(30.))
            .pl(px(12.))
            .pr(px(14.))
            .flex()
            .items_center()
            .gap(px(COLUMN_GAP))
            .flex_none()
            .text_size(crate::theme::ui_px(13.))
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(theme.text_disabled)
            .child(div().flex_1().child(column(SortKey::Name, "Name", self)))
            .child(
                div()
                    .w(px(SIZE_W))
                    .flex_none()
                    .flex()
                    .justify_end()
                    .child(column(SortKey::Size, "Size", self)),
            )
            .when(width >= MODIFIED_FROM, |this| {
                this.child(div().w(px(MODIFIED_W)).flex_none().child(column(
                    SortKey::Modified,
                    "Modified",
                    self,
                )))
            })
            .when(width >= PERMS_FROM, |this| {
                this.child(div().w(px(PERMS_W)).flex_none().child("Permissions"))
            })
    }

    /// The one file selected, which Space would preview.
    fn previewable(&self) -> Option<&RemoteEntry> {
        self.inspected()
            .filter(|entry| entry.kind != EntryKind::Dir)
    }

    /// What the status bar says after the summary, dimmer: how to preview the file selected.
    pub(crate) fn summary_hint(&self) -> Option<&'static str> {
        (!self.path.is_empty() && self.previewable().is_some()).then_some("Space to preview")
    }

    /// For the status bar: the selection or what the folder holds, and the room left on the server.
    /// Nothing until the folder has loaded.
    pub(crate) fn summary(&self) -> Option<String> {
        if self.path.is_empty() {
            return None;
        }
        // One file: what it is, for the hint after it to open its preview.
        if let Some(file) = self.previewable() {
            return Some(format!(
                "{} · {}",
                file.name,
                files_format::bytes(file.size)
            ));
        }
        let mut text = if self.selected.is_empty() {
            self.contents_summary()
        } else {
            let count = self.selected.len();
            match self.selected_size() {
                Some(size) => format!("{count} selected, {size}"),
                None => format!("{count} selected"),
            }
        };
        if let Some(free) = self.free_space {
            let _ = write!(text, ", {} free", files_format::bytes(free));
        }
        Some(text)
    }

    fn contents_summary(&self) -> String {
        if self.search.is_some() {
            return format!("{} results", self.entries.len());
        }
        let folders = self
            .entries
            .iter()
            .filter(|e| e.kind == EntryKind::Dir)
            .count();
        let files = self.entries.len() - folders;
        let mut text = format!("{folders} folders, {files} files");
        if !self.show_hidden {
            let hidden = self
                .all_entries
                .iter()
                .filter(|e| e.name.starts_with('.'))
                .count();
            if hidden > 0 {
                let _ = write!(text, ", {hidden} hidden");
            }
        }
        text
    }

    /// The name of the folder on screen, once there is one.
    fn folder_name(&self) -> Option<&str> {
        match self.path.as_str() {
            "" => None,
            "/" => Some("/"),
            path => path.rsplit('/').find(|name| !name.is_empty()),
        }
    }

    /// How much the selected files weigh, if any of them has a size.
    fn selected_size(&self) -> Option<String> {
        let selected = self.selected_entries();
        let bytes: u64 = selected
            .iter()
            .filter(|e| e.kind != EntryKind::Dir)
            .map(|e| e.size)
            .sum();
        (bytes > 0).then(|| files_format::bytes(bytes))
    }
}

/// The three groups of a permission string, each letter on its own: dashes step back, and the right
/// to run a file stands out.
fn permission_triples(entry: &RemoteEntry, theme: &crate::theme::Theme) -> Vec<Div> {
    let text = files_format::permissions(entry);
    let letters: Vec<char> = text.chars().skip(1).collect();
    let is_dir = entry.kind == EntryKind::Dir;
    letters
        .chunks(3)
        .map(|group| {
            div().flex().children(group.iter().map(|letter| {
                div()
                    .text_color(match letter {
                        '-' => gpui::Hsla {
                            a: 0.7,
                            ..theme.text_disabled
                        },
                        'x' if !is_dir => theme.accent,
                        _ => theme.text_muted,
                    })
                    .child(letter.to_string())
            }))
        })
        .collect()
}

/// A line of a file, its key and its comments told apart at a glance.
fn preview_line(line: &str, theme: &crate::theme::Theme) -> Div {
    let line = line.trim_end();

    let trimmed = line.trim_start();
    let indent = &line[..line.len() - trimmed.len()];
    let comment = trimmed.starts_with('#') || trimmed.starts_with("//");
    let key_end = trimmed
        .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '-' || c == '.' || c == '"'))
        .unwrap_or(trimmed.len());
    let (key, rest) = trimmed.split_at(key_end);
    let is_key = !comment
        && !key.is_empty()
        && !key.chars().all(|c| c.is_numeric() || c == '.')
        && key
            .chars()
            .next()
            .is_some_and(|c| c.is_alphabetic() || c == '"' || c == '_')
        && (!rest.is_empty());
    let line_height = gpui::relative(1.6);
    let cell = |text: String, color| {
        div()
            .flex_none()
            .whitespace_nowrap()
            .line_height(line_height)
            .text_color(color)
            .child(text)
    };
    let expanded = indent.replace('\t', "    ");
    div()
        .flex()
        .flex_none()
        .min_h(px(16.))
        .children((!expanded.is_empty()).then(|| cell(expanded, theme.text_muted)))
        .when(comment, |this| {
            this.child(cell(trimmed.to_string(), theme.text_disabled))
        })
        .when(!comment && is_key, |this| {
            this.child(cell(key.to_string(), theme.accent))
                .child(cell(rest.to_string(), theme.text_muted))
        })
        .when(!comment && !is_key, |this| {
            this.child(cell(trimmed.to_string(), theme.text_muted))
        })
}

impl FilesView {
    /// The one file or folder selected, if it is one: what the preview is about.
    fn inspected(&self) -> Option<&RemoteEntry> {
        if self.selected.len() != 1 {
            return None;
        }
        let path = self.selected.iter().next()?;
        self.entries.iter().find(|entry| &entry.path == path)
    }

    /// Asks for the start of the file the preview is about, when it changes. Only while the
    /// preview is open.
    fn want_preview(&mut self, cx: &mut Context<Self>) {
        let wanted = self
            .looked_at()
            .filter(|entry| entry.kind != EntryKind::Dir && entry.size > 0)
            .map(|entry| entry.path.clone());
        let Some(path) = wanted else {
            self.preview = None;
            return;
        };
        if self.preview.as_ref().is_some_and(|p| p.path == path) || !self.session_connected(cx) {
            return;
        }
        let req = self
            .state
            .read(cx)
            .engine
            .read_head(self.session, path.clone());
        self.preview = Some(Preview {
            path,
            req,
            head: Head::Loading,
        });
    }

    /// What the preview shows, while it is open: the one entry selected.
    fn looked_at(&self) -> Option<&RemoteEntry> {
        self.looking.then(|| self.inspected()).flatten()
    }

    /// Opens the preview of the file selected; a folder has none. From the menu it may fade in,
    /// from the keyboard it is just there.
    fn open_look(&mut self, fade: bool, cx: &mut Context<Self>) {
        if self
            .inspected()
            .is_none_or(|entry| entry.kind == EntryKind::Dir)
        {
            return;
        }
        self.looking = true;
        self.look_fade = (fade && self.state.read(cx).settings.animated_selection).then(|| {
            self.look_count += 1;
            self.look_count
        });
        cx.notify();
    }

    fn close_look(&mut self, cx: &mut Context<Self>) {
        self.looking = false;
        self.look_fade = None;
        cx.notify();
    }

    /// The preview, floating over the list in the middle of the card: what the file is, and its
    /// first lines. The selection moving with the arrows takes it along.
    fn render_look(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let entry = self.looked_at()?.clone();
        let theme = cx.theme().clone();
        let is_dir = entry.kind == EntryKind::Dir;
        let mark = if is_dir {
            Mark::Icon(IconName::Folder)
        } else {
            Mark::Label(files_format::badge(&entry.name).into())
        };
        let meta = if is_dir {
            format!("Folder · {}", files_format::modified(entry.modified))
        } else {
            format!(
                "{} · {}",
                files_format::bytes(entry.size),
                files_format::modified(entry.modified)
            )
        };
        let preview = self.preview.as_ref().filter(|p| p.path == entry.path);
        let empty = |text: &'static str| {
            div()
                .flex()
                .items_center()
                .justify_center()
                .min_h(px(120.))
                .font_family(crate::theme::ui_font())
                .text_size(crate::theme::ui_px(13.))
                .text_color(theme.text_disabled)
                .child(text)
        };
        let body = match preview.map(|p| &p.head) {
            _ if is_dir => empty("Folders have no preview"),
            _ if entry.size == 0 => empty("This file is empty"),
            Some(Head::Text(text)) => div().flex().flex_col().children(
                text.lines()
                    .take(PREVIEW_LINES)
                    .map(|line| preview_line(line, &theme)),
            ),
            Some(Head::NotText) => empty("No preview: this file is not text"),
            Some(Head::Loading) | None => empty("Loading…"),
        };
        let header = div()
            .h(px(42.))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(10.))
            .pl(px(10.))
            .pr(px(6.))
            .text_size(crate::theme::ui_px(13.))
            .child(tile(mark, theme.text_muted, None, &theme))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme.text)
                    .child(entry.name.clone()),
            )
            .child(
                div()
                    .flex_none()
                    .whitespace_nowrap()
                    .text_size(crate::theme::ui_px(12.5))
                    .text_color(theme.text_disabled)
                    .child(meta),
            )
            .child(div().flex_1())
            .child(
                div()
                    .flex_none()
                    .px(px(5.))
                    .py(px(2.))
                    .rounded(px(crate::theme::RADIUS_SMALL))
                    .border_1()
                    .border_color(theme.strong_border)
                    .font_family(crate::theme::mono_font())
                    .text_size(crate::theme::ui_px(10.5))
                    .text_color(theme.text_disabled)
                    .child("Space"),
            )
            .child(
                Button::new("quick-look-close")
                    .icon(IconName::Close)
                    .tooltip("Close (Space, Esc)")
                    .style(ButtonStyle::Ghost)
                    .on_click(cx.listener(|this, _, _, cx| this.close_look(cx))),
            );
        let panel = div()
            .id("quick-look")
            .occlude()
            .w(px(540.))
            .max_w_full()
            .max_h_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .map(|this| crate::panel::floating_card(this, &theme, theme.strong_border))
            // A press inside belongs to the preview: the list under it keeps its selection.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .child(header)
            .child(
                div()
                    .id("quick-look-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .mx(px(4.))
                    .mb(px(4.))
                    .px(px(14.))
                    .py(px(12.))
                    .rounded(px(crate::theme::RADIUS_CONTROL))
                    .bg(gpui::Hsla {
                        a: 0.04,
                        ..theme.text
                    })
                    .font_family(crate::theme::mono_font())
                    .text_size(crate::theme::ui_px(12.5))
                    .child(body),
            );
        let panel = match self.look_fade {
            Some(count) => crate::motion::enter(
                panel,
                ("quick-look-in", count as usize),
                crate::motion::MENU,
            )
            .into_any_element(),
            None => panel.into_any_element(),
        };
        // Takes no pointer itself: only the panel in its middle does.
        Some(
            div()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .px(px(16.))
                .py(px(36.))
                .child(panel)
                .into_any_element(),
        )
    }

    /// Opens the permissions of an entry, under its cell in the list or, where the list does not
    /// show them, at `at`.
    fn open_perms(&mut self, path: String, at: Point<Pixels>, cx: &mut Context<Self>) {
        self.menu = None;
        self.perms = Some(PermsOpen { path, at });
        cx.notify();
    }

    /// The permissions of one entry, floating under its cell: the grid of toggles the details
    /// had.
    fn render_perms(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let open = self.perms.as_ref()?;
        let entry = self
            .entries
            .iter()
            .find(|entry| entry.path == open.path)?
            .clone();
        let (position, corner) = match self.perm_cells.borrow().get(&entry.path) {
            Some(cell) => (
                cell.bottom_right() + point(px(0.), px(4.)),
                Corner::TopRight,
            ),
            None => (open.at, Corner::TopLeft),
        };
        let grid = self.permissions_grid(&entry, cx)?;
        let theme = cx.theme().clone();
        let panel = div()
            .id("permissions-popover")
            .occlude()
            .w(px(264.))
            .p(px(10.))
            .flex()
            .flex_col()
            .gap(px(8.))
            .map(|this| crate::panel::floating_card(this, &theme, theme.strong_border))
            .text_size(crate::theme::ui_px(13.))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.perms = None;
                cx.notify();
            }))
            .child(
                div()
                    .px(px(2.))
                    .truncate()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme.text)
                    .child(entry.name.clone()),
            )
            .child(grid);
        Some(
            deferred(
                anchored()
                    .anchor(corner)
                    .position(position)
                    .snap_to_window_with_margin(px(8.))
                    .child(crate::motion::enter(
                        panel,
                        "permissions-in",
                        crate::motion::MENU,
                    )),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }

    /// Flips one permission of an entry.
    fn toggle_permission(&mut self, path: &str, bit: u32, cx: &mut Context<Self>) {
        let Some(mode) = self
            .all_entries
            .iter()
            .find(|entry| entry.path == path)
            .and_then(|entry| entry.permissions)
        else {
            return;
        };
        let mode = (mode ^ bit) & 0o7777;
        let req = self
            .state
            .read(cx)
            .engine
            .chmod(self.session, vec![path.to_string()], mode);
        self.pending_ops.insert(req, "Change permissions");
        // The listing that follows says the same, a moment later.
        for entry in self
            .all_entries
            .iter_mut()
            .chain(self.entries.iter_mut())
            .filter(|entry| entry.path == path)
        {
            entry.permissions = Some(mode);
        }
        cx.notify();
    }

    /// Who may read, write and run an entry, as a grid of toggles. Each changes its one bit on
    /// the server at once. `None` when the server did not say.
    fn permissions_grid(&self, entry: &RemoteEntry, cx: &mut Context<Self>) -> Option<Div> {
        let theme = cx.theme().clone();
        let ready = self.ready(cx);
        entry.permissions.map(|mode| {
            let path = entry.path.clone();
            let octal = format!("{:o}", mode & 0o777);
            let bit = |who: usize, what: usize, cx: &mut Context<Self>| {
                let mask = 1u32 << (8 - (who * 3 + what));
                let on = mode & mask != 0;
                let path = path.clone();
                let (lit_fill, lit_line, accent, border, line) = {
                    let theme = cx.theme();
                    (
                        theme.selected,
                        theme.lit_line,
                        theme.accent,
                        theme.border,
                        theme.strong_border,
                    )
                };
                div()
                    .id(("permission", who * 3 + what))
                    .w(px(34.))
                    .h(px(24.))
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .rounded(px(crate::theme::RADIUS_SMALL))
                    .border_1()
                    .border_color(if on { lit_line } else { border })
                    .when(on, |this| this.bg(lit_fill))
                    .when(ready, |this| {
                        this.cursor_pointer()
                            .hover(move |style| style.border_color(line))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.toggle_permission(&path, mask, cx)
                            }))
                    })
                    .when(on, |this| {
                        this.child(Icon::new(IconName::Check).size(px(12.)).color(accent))
                    })
            };
            let heading = |text: &'static str, theme: &crate::theme::Theme| {
                div()
                    .w(px(34.))
                    .flex_none()
                    .flex()
                    .justify_center()
                    .font_family(crate::theme::mono_font())
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_size(crate::theme::ui_px(12.))
                    .text_color(theme.text_disabled)
                    .child(text)
            };
            let mut grid = div()
                .flex()
                .flex_none()
                .flex_col()
                .gap(px(4.))
                .text_size(crate::theme::ui_px(13.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .child(
                            div()
                                .flex_1()
                                .flex()
                                .gap(px(5.))
                                .text_color(theme.text_disabled)
                                .child("Permissions")
                                .child(
                                    div()
                                        .font_family(crate::theme::mono_font())
                                        .font_weight(gpui::FontWeight::MEDIUM)
                                        .text_color(theme.accent)
                                        .child(octal),
                                ),
                        )
                        .child(heading("read", &theme))
                        .child(heading("write", &theme))
                        .child(heading("run", &theme)),
                );
            for (who, label) in ["Owner", "Group", "Others"].into_iter().enumerate() {
                let (a, b, c) = (bit(who, 0, cx), bit(who, 1, cx), bit(who, 2, cx));
                grid = grid.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .child(div().flex_1().text_color(theme.text_muted).child(label))
                        .child(a)
                        .child(b)
                        .child(c),
                );
            }
            grid
        })
    }
}

impl Render for FilesView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        // The preview closes once the selection is no longer one entry.
        if self.looking && self.inspected().is_none() {
            self.looking = false;
        }
        self.want_preview(cx);
        let measured = self.width.clone();
        let list_width = self.width.get();
        let drop_highlight = theme.selected;
        let bar = list_scrollbar(
            &self.list_scroll,
            self.scroll_drag.clone(),
            theme.text_muted,
            cx.entity().downgrade(),
        );
        let empty_message = if let Some(search) = &self.search {
            (self.entries.is_empty() && !search.running).then_some(if search.found.is_empty() {
                "Nothing found in this folder or under it"
            } else {
                "Only hidden files found: Ctrl+H shows them"
            })
        } else if self.loading {
            None
        } else if self.entries.is_empty() && !self.all_entries.is_empty() {
            if self.filter_input.read(cx).text().trim().is_empty() {
                Some("Only hidden files here: Ctrl+H shows them")
            } else {
                Some("Nothing here matches. Enter searches the subfolders too")
            }
        } else if self.entries.is_empty() && !self.path.is_empty() {
            Some("This folder is empty")
        } else {
            None
        };
        let view = div()
            .id("files-view")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key_down))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                this.upload_paths_to(paths.paths().to_vec(), this.path.clone(), cx)
            }))
            .drag_over::<ExternalPaths>(move |style, _, _, _| style.bg(drop_highlight))
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .text_color(theme.text)
            .child(
                // Knows how wide the view is. A change shows at the next frame.
                canvas(
                    move |bounds, window, _| {
                        let width = f32::from(bounds.size.width);
                        if (measured.get() - width).abs() > 0.5 {
                            measured.set(width);
                            window.refresh();
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .when(self.selected.len() > 1, |this| {
                this.child(self.selection_bar(cx))
            })
            .children(self.search_bar(cx))
            .child(
                div().flex_1().min_h_0().w_full().flex().child(
                    div()
                        .h_full()
                        .flex()
                        .flex_col()
                        .min_w_0()
                        .flex_1()
                        .child(self.header(cx))
                        .child(
                            div()
                                .id("file-list-area")
                                .relative()
                                .flex_1()
                                .min_h_0()
                                .w_full()
                                .on_click(cx.listener(|this, event: &ClickEvent, window, cx| {
                                    // Clicks that reach here missed every row: clear the selection.
                                    window.focus(&this.focus);
                                    if !event.modifiers().control && !event.modifiers().shift {
                                        this.selected.clear();
                                        this.anchor = None;
                                        cx.notify();
                                    }
                                }))
                                .on_mouse_down(
                                    MouseButton::Right,
                                    cx.listener(|this, event: &MouseDownEvent, window, cx| {
                                        window.focus(&this.focus);
                                        this.open_menu(event.position, None, cx);
                                    }),
                                )
                                .child(
                                    uniform_list(
                                        "file-list",
                                        self.entries.len(),
                                        cx.processor(|this, range: Range<usize>, _window, cx| {
                                            range
                                                .map(|i| this.render_row(i, cx))
                                                .collect::<Vec<_>>()
                                        }),
                                    )
                                    .track_scroll(self.list_scroll.clone())
                                    .size_full(),
                                )
                                .child(bar),
                        ),
                ),
            )
            .children(empty_message.map(|m| {
                div()
                    .absolute()
                    .top(px(120.))
                    .left_0()
                    .w(px(list_width))
                    .flex()
                    .justify_center()
                    .text_color(theme.text_muted)
                    .child(m)
            }))
            .children(self.menu.clone().map(|menu| {
                let this = cx.entity().downgrade();
                ContextMenu::new(
                    "files-menu",
                    menu,
                    crate::widgets::context_menu::dismiss(this, |view| &mut view.menu),
                )
            }));
        let look = self.render_look(cx);
        let perms = self.render_perms(cx);
        div()
            .relative()
            .size_full()
            .child(view)
            .children(look)
            .children(perms)
    }
}

#[cfg(test)]
#[path = "files_view_tests.rs"]
mod tests;
