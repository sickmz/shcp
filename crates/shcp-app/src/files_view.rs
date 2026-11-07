//! Files tab: SFTP browser for one host (listing, navigation, selection, upload/download,
//! mkdir/rename/delete). Remote results arrive as core events re-emitted by `AppState`.
use crate::files_format as format;
use crate::login_prompt::present;
use crate::modals::{
    ModalLayer, confirm::ConfirmModal, folder_picker::FolderPicker, text_prompt::TextPrompt,
};
use crate::motion::Reveal;
use crate::session_overlay;
use crate::state::AppState;
use crate::theme::icon_theme::file_icon;
use crate::theme::{ActiveTheme as _, row_px};
use crate::widgets::button::{Button, ButtonStyle};
use crate::widgets::context_menu::{ContextMenu, MenuAnchor, MenuEntry, OpenMenu, handler};
use crate::widgets::icon::{Icon, IconName};
use crate::widgets::progress::ProgressBar;
use crate::widgets::scrollbar::{ScrollDrag, ScrollMetrics, scrollbar};
use crate::widgets::text_input::{TextInput, TextInputEvent};
use gpui::prelude::*;
use gpui::{
    AnyElement, App, ClickEvent, ClipboardItem, Context, Div, Entity, EventEmitter, ExternalPaths,
    FocusHandle, Focusable, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    PathPromptOptions, Pixels, Point, ScrollStrategy, SharedString, Subscription,
    UniformListScrollHandle, Window, canvas, div, img, point, px, uniform_list,
};
use shcp_core::{
    EngineHandle, EntryKind, Event, HostId, NoticeLevel, RemoteEntry, RequestId, SessionId,
    SessionState, TransferId, TransferState, archive_stem,
};
use std::{
    cell::Cell,
    collections::HashMap,
    collections::HashSet,
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
/// The details beside the list, on their own or next to a terminal.
const INSPECTOR_W: f32 = 344.;
/// From this width the details lay themselves out for the room they have.
const WIDE_DETAILS: f32 = 520.;
/// What dragging the line leaves to each side.
const MIN_DETAILS: f32 = 260.;
const MIN_LIST: f32 = 320.;
/// Below these widths, a column or the details give way.
const INSPECTOR_FROM: f32 = 600.;
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

pub enum FilesEvent {
    OpenTerminalHere(String),
}

/// Where a key moves the cursor on a list of `len` rows.
fn moved_cursor(key: &str, cursor: Option<usize>, len: usize, page: usize) -> Option<usize> {
    let last = len.checked_sub(1)?;
    Some(match (key, cursor) {
        ("home", _) => 0,
        ("end", _) => last,
        ("up", None) | ("pageup", None) => last,
        (_, None) => 0,
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

pub struct FilesView {
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
    /// Where the view ends on the right, to turn a drag of the line into a width.
    right_edge: Rc<Cell<f32>>,
    /// The width of the details while their line is being dragged.
    dragging_details: Option<f32>,
    /// How much of the details shows.
    details_reveal: Reveal,
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
    add_anchor: MenuAnchor,
    upload_anchor: MenuAnchor,
    more_anchor: MenuAnchor,
    /// The toolbar button that opened `menu`.
    menu_button: Option<ToolbarMenu>,
    /// The toolbar menu open when the mouse went down: that press closes it, and the click that
    /// follows on its button must not open it again.
    closing_menu: Option<ToolbarMenu>,
    _subscriptions: Vec<Subscription>,
}

/// The start of a file, on its way or here.
struct Preview {
    path: String,
    req: RequestId,
    /// `None` while waiting, or for what is not text.
    text: Option<String>,
}

/// A search of the subfolders of the folder on screen.
struct FileSearch {
    req: RequestId,
    query: String,
    root: String,
    running: bool,
    /// It stopped at its limit, so there may be more.
    limited: bool,
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
    Add,
    Upload,
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
    pub fn new(
        state: Entity<AppState>,
        modals: Entity<ModalLayer>,
        host: HostId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let session = state.update(cx, |s, cx| s.connect_host(&host, cx));
        let filter_input = cx.new(|cx| TextInput::new("Filter this folder", false, cx).quiet());
        let filter_focus = filter_input.focus_handle(cx);
        let show_hidden = state.read(cx).settings.show_hidden_files;
        let details_open = state.read(cx).settings.show_file_details;
        let subscriptions = vec![
            cx.subscribe(&state, |this, _, event: &Event, cx| {
                this.on_event(event, cx)
            }),
            // The setting can change from another Files tab or the settings window.
            cx.observe(&state, |this, state, cx| {
                let show_hidden = state.read(cx).settings.show_hidden_files;
                if show_hidden != this.show_hidden {
                    this.show_hidden = show_hidden;
                    this.apply_filter(cx);
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
                        this.start_search(query, cx);
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
            width: Rc::new(Cell::new(INSPECTOR_FROM)),
            right_edge: Rc::new(Cell::new(0.)),
            dragging_details: None,
            details_reveal: Reveal::new(details_open),
            preview: None,
            selection_anchor: MenuAnchor::default(),
            pending_space: None,
            start_path: None,
            picked: None,
            loading: true,
            focus: cx.focus_handle(),
            home_requested: false,
            pending_home: None,
            pending_list: None,
            pending_ops: HashMap::new(),
            uploads: HashMap::new(),
            sort: SortKey::Name,
            ascending: true,
            menu: None,
            add_anchor: MenuAnchor::default(),
            upload_anchor: MenuAnchor::default(),
            more_anchor: MenuAnchor::default(),
            menu_button: None,
            closing_menu: None,
            _subscriptions: subscriptions,
        };
        if this.session_connected(cx) {
            this.load_home(cx);
        }
        this
    }

    pub fn host(&self) -> &HostId {
        &self.host
    }

    /// The folder on screen; empty until the first listing arrives.
    pub fn path(&self) -> &str {
        &self.path
    }

    fn session_connected(&self, cx: &App) -> bool {
        self.state
            .read(cx)
            .session(self.session)
            .is_some_and(|s| matches!(s.state, SessionState::Connected))
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
    pub fn go_to(&mut self, path: String, cx: &mut Context<Self>) {
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
        let path = if path.starts_with('/') {
            path
        } else {
            format::join(
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
        if let Some(query) = self.search.as_ref().map(|search| search.query.clone()) {
            self.start_search(query, cx);
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

    /// Looks for a name in the folder on screen and everything under it.
    fn start_search(&mut self, query: String, cx: &mut Context<Self>) {
        self.end_search(cx);
        if !query.is_empty() && !self.path.is_empty() && self.session_connected(cx) {
            let root = self.path.clone();
            let req =
                self.state
                    .read(cx)
                    .engine
                    .search_files(self.session, root.clone(), query.clone());
            self.search = Some(FileSearch {
                req,
                query,
                root,
                running: true,
                limited: false,
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

    fn session_is_gone(&self, cx: &App) -> bool {
        self.state.read(cx).session(self.session).is_none_or(|s| {
            matches!(
                s.state,
                SessionState::Disconnected { .. } | SessionState::Failed { .. }
            )
        })
    }

    /// Follows the host to a new connection, staying in the same folder.
    fn resume_on(&mut self, session: SessionId, cx: &mut Context<Self>) {
        self.session = session;
        self.pending_home = None;
        self.pending_list = None;
        self.pending_ops.clear();
        self.uploads.clear();
        self.refresh(cx);
    }

    fn reconnect(&mut self, cx: &mut Context<Self>) {
        self.session = self
            .state
            .update(cx, |s, cx| s.connect_host(&self.host, cx));
        self.home_requested = false;
        self.pending_home = None;
        self.pending_list = None;
        self.pending_ops.clear();
        self.uploads.clear();
        self.path.clear();
        self.all_entries.clear();
        self.entries.clear();
        self.history.clear();
        self.going_back = false;
        self.search = None;
        self.free_space = None;
        self.total_space = None;
        self.pending_space = None;
        self.filter_input
            .update(cx, |input, cx| input.set_text("", cx));
        self.selected.clear();
        self.anchor = None;
        self.loading = false;
        if self.session_connected(cx) {
            self.load_home(cx);
        }
        cx.notify();
    }

    fn on_event(&mut self, event: &Event, cx: &mut Context<Self>) {
        match event {
            // Another tab or the sidebar brought the host back: this tab follows.
            Event::Session {
                id,
                host,
                state: SessionState::Connected,
            } if *host == self.host && *id != self.session && self.session_is_gone(cx) => {
                self.resume_on(*id, cx);
            }
            Event::Session { id, state, .. } if *id == self.session => {
                if matches!(state, SessionState::Connected) {
                    self.load_home(cx);
                } else if matches!(
                    state,
                    SessionState::Disconnected { .. } | SessionState::Failed { .. }
                ) {
                    self.pending_home = None;
                    self.pending_list = None;
                    self.pending_ops.clear();
                    self.uploads.clear();
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
                    preview.text = text.clone().ok().flatten();
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
                        Ok(limited) => search.limited = *limited,
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
        self.anchor = None;
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
            self.anchor = None;
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
        let ready = !self.path.is_empty() && self.session_connected(cx);
        let selection = self.selected.len();
        let mut entries = Vec::new();
        if let Some(entry) = row.and_then(|i| self.entries.get(i)) {
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
            }
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
                MenuEntry::item("Rename", handler(this.clone(), |v, w, cx| v.rename(w, cx)))
                    .icon(IconName::Pencil)
                    .shortcut("F2")
                    .disabled(!ready || selection != 1),
            );
            entries.push(
                MenuEntry::item(
                    "Move to",
                    handler(this.clone(), |v, w, cx| v.move_selected(w, cx)),
                )
                .icon(IconName::FolderUp)
                .disabled(!ready),
            );
            entries.push(
                MenuEntry::item(
                    "Cut",
                    handler(this.clone(), |v, _, cx| v.pick_selected(true, cx)),
                )
                .icon(IconName::Forward)
                .shortcut("Ctrl+X")
                .disabled(!ready),
            );
            entries.push(
                MenuEntry::item(
                    "Copy",
                    handler(this.clone(), |v, _, cx| v.pick_selected(false, cx)),
                )
                .icon(IconName::Copy)
                .shortcut("Ctrl+C")
                .disabled(!ready),
            );
            entries.extend(self.archive_entries(cx));
            entries.push(
                MenuEntry::item(
                    "Permissions",
                    handler(this.clone(), |v, w, cx| v.change_permissions(w, cx)),
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
            MenuEntry::item(
                if self.show_hidden {
                    "Hide hidden files"
                } else {
                    "Show hidden files"
                },
                handler(this.clone(), |v, _, cx| v.toggle_hidden(cx)),
            )
            .icon(if self.show_hidden {
                IconName::EyeOff
            } else {
                IconName::Eye
            })
            .shortcut("Ctrl+H"),
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
                MenuEntry::item("Delete", handler(this, |v, w, cx| v.delete(w, cx)))
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

    /// What creates something in this folder.
    fn add_entries(&self, cx: &mut Context<Self>) -> Vec<MenuEntry> {
        let this = cx.entity().downgrade();
        let ready = !self.path.is_empty() && self.session_connected(cx);
        vec![
            MenuEntry::item(
                "New file",
                handler(this.clone(), |v, w, cx| v.new_file(w, cx)),
            )
            .icon(IconName::FilePlus)
            .disabled(!ready),
            MenuEntry::item("New folder", handler(this, |v, w, cx| v.new_folder(w, cx)))
                .icon(IconName::FolderPlus)
                .disabled(!ready),
        ]
    }

    /// What brings something from this PC into this folder.
    fn upload_entries(&self, cx: &mut Context<Self>) -> Vec<MenuEntry> {
        let this = cx.entity().downgrade();
        let ready = !self.path.is_empty() && self.session_connected(cx);
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

    /// What acts on the folder, less often than the buttons in view.
    fn folder_entries(&self, cx: &mut Context<Self>) -> Vec<MenuEntry> {
        let this = cx.entity().downgrade();
        let connected = self.session_connected(cx);
        let ready = !self.path.is_empty() && connected;
        vec![
            MenuEntry::item(
                if self.show_hidden {
                    "Hide hidden files"
                } else {
                    "Show hidden files"
                },
                handler(this.clone(), |v, _, cx| v.toggle_hidden(cx)),
            )
            .icon(if self.show_hidden {
                IconName::EyeOff
            } else {
                IconName::Eye
            })
            .shortcut("Ctrl+H"),
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
        ]
    }

    /// What acts on the selection, besides the buttons of the selection bar.
    fn selection_entries(&self, cx: &mut Context<Self>) -> Vec<MenuEntry> {
        let this = cx.entity().downgrade();
        let ready = !self.path.is_empty() && self.session_connected(cx);
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
            if entry.kind == EntryKind::Dir {
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
            }
            entries.push(MenuEntry::Separator);
        }
        entries.extend([
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
            MenuEntry::item(
                "Move to",
                handler(this.clone(), |v, w, cx| v.move_selected(w, cx)),
            )
            .icon(IconName::FolderUp)
            .disabled(!ready),
            MenuEntry::item(
                "Permissions",
                handler(this.clone(), |v, w, cx| v.change_permissions(w, cx)),
            )
            .icon(IconName::Settings)
            .disabled(!ready),
            MenuEntry::item("Copy path", handler(this, |v, _, cx| v.copy_paths(cx)))
                .icon(IconName::Copy),
        ]);
        entries.extend(self.archive_entries(cx));
        entries
    }

    /// What makes an archive of the selection, or opens the archive selected.
    fn archive_entries(&self, cx: &mut Context<Self>) -> Vec<MenuEntry> {
        let this = cx.entity().downgrade();
        let ready = !self.path.is_empty() && self.session_connected(cx);
        let mut entries = vec![
            MenuEntry::item(
                "Compress",
                handler(this.clone(), |v, w, cx| v.compress(w, cx)),
            )
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
        let text = self
            .selected_entries()
            .into_iter()
            .map(|e| e.path)
            .collect::<Vec<_>>()
            .join("\n");
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

    fn list_metrics(handle: &UniformListScrollHandle) -> Option<ScrollMetrics> {
        let base = handle.0.borrow().base_handle.clone();
        let max = f32::from(base.max_offset().height);
        let height = f32::from(base.bounds().size.height);
        (max > 0.).then(|| ScrollMetrics {
            position: -f32::from(base.offset().y) / max,
            visible: height / (height + max),
        })
    }

    fn toggle_hidden(&mut self, cx: &mut Context<Self>) {
        self.state.update(cx, |s, cx| s.toggle_hidden_files(cx));
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = &event.keystroke;
        if key.key == "escape" && self.menu.take().is_some() {
            cx.notify();
            cx.stop_propagation();
            return;
        }
        if !self.session_connected(cx) {
            return;
        }
        match key.key.as_str() {
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
            self.navigate(format::parent(&self.path), cx);
        }
    }

    fn go_back(&mut self, cx: &mut Context<Self>) {
        if let Some(path) = self.history.pop() {
            self.going_back = true;
            self.navigate(path, cx);
        }
    }

    fn upload_paths(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        self.upload_paths_to(paths, self.path.clone(), cx);
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
                    if this.session == session && this.session_connected(cx) {
                        let engine = &this.state.read(cx).engine;
                        for path in paths {
                            let id = engine.upload(session, path, dir.clone());
                            this.uploads.insert(id, dir.clone());
                        }
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    /// Keeps the selection to paste it elsewhere, moving it if `cut`.
    fn pick_selected(&mut self, cut: bool, cx: &mut Context<Self>) {
        let paths: Vec<String> = self
            .selected_entries()
            .into_iter()
            .map(|entry| entry.path)
            .collect();
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
            let to = format::join(&self.path, &target);
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
        let paths: Vec<String> = self
            .selected_entries()
            .into_iter()
            .map(|entry| entry.path)
            .collect();
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
                        if this.session != session {
                            return;
                        }
                        let engine = this.state.read(cx).engine.clone();
                        for from in paths {
                            let (origin, name) = split_path(&from);
                            if origin != folder {
                                let to = format::join(&folder, name);
                                let req = engine.rename(session, from, to);
                                this.pending_ops.insert(req, "Move");
                            }
                        }
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
                        if this.session != session {
                            return;
                        }
                        let name = if archive_stem(&name).is_some() {
                            name
                        } else {
                            format!("{name}.tar.gz")
                        };
                        let archive = format::join(&folder, &name);
                        this.notice(NoticeLevel::Info, format!("Compressing to {name}…"), cx);
                        let req = this
                            .state
                            .read(cx)
                            .engine
                            .compress(session, folder, names, archive);
                        this.pending_ops.insert(req, "Compress");
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
        let folder = format::join(&format::parent(&only.path), &name);
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
        let paths: Vec<String> = selected.iter().map(|entry| entry.path.clone()).collect();
        let (this, session) = (cx.entity(), self.session);
        let prompt = cx.new(|cx| {
            TextPrompt::new(
                &title,
                "Apply",
                &std::format!("{current:o}"),
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
                        if this.session != session {
                            return;
                        }
                        let req = this.state.read(cx).engine.chmod(session, paths, mode);
                        this.pending_ops.insert(req, "Change permissions");
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
                        if this.session != session {
                            return;
                        }
                        let req = create(
                            &this.state.read(cx).engine,
                            session,
                            format::join(&dir, &name),
                        );
                        this.pending_ops.insert(req, operation);
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
        let (from, dir) = (entry.path.clone(), format::parent(&entry.path));
        let session = self.session;
        let prompt = cx.new(|cx| {
            TextPrompt::new(
                "Rename",
                "Rename",
                &entry.name,
                move |name, _, cx| {
                    this.update(cx, |this, cx| {
                        if this.session != session {
                            return;
                        }
                        let req = this.state.read(cx).engine.rename(
                            session,
                            from,
                            format::join(&dir, &name),
                        );
                        this.pending_ops.insert(req, "Rename");
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
        let paths: Vec<String> = selected.iter().map(|e| e.path.clone()).collect();
        let this = cx.entity();
        let session = self.session;
        let run = move |cx: &mut App| {
            this.update(cx, |this, cx| {
                if this.session != session {
                    return;
                }
                let engine = this.state.read(cx).engine.clone();
                for path in paths {
                    let req = engine.remove(session, path, true);
                    this.pending_ops.insert(req, "Delete");
                }
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
            ToolbarMenu::Add => &self.add_anchor,
            ToolbarMenu::Upload => &self.upload_anchor,
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
                    ToolbarMenu::Add => (this.add_anchor.clone(), this.add_entries(cx)),
                    ToolbarMenu::Upload => (this.upload_anchor.clone(), this.upload_entries(cx)),
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

    /// Back and up, at the start of the location bar.
    pub fn nav_buttons(&self, cx: &mut Context<Self>) -> AnyElement {
        let connected = self.session_connected(cx);
        let ready = !self.path.is_empty() && connected;
        div()
            .id("files-nav")
            .occlude()
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

    /// What acts on the folder, at the end of the location bar.
    pub fn tools(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(1.))
            .child(
                div()
                    .w(px(150.))
                    .mr(px(4.))
                    .flex_none()
                    .child(self.filter_input.clone()),
            )
            .child(
                self.menu_button(
                    ToolbarMenu::Add,
                    Button::new("add")
                        .icon(IconName::Plus)
                        .tooltip("New file or folder")
                        .style(ButtonStyle::Ghost),
                    cx,
                ),
            )
            .child(
                self.menu_button(
                    ToolbarMenu::Upload,
                    Button::new("upload")
                        .icon(IconName::Upload)
                        .tooltip("Upload files or a folder")
                        .style(ButtonStyle::Ghost),
                    cx,
                ),
            )
            .child(
                self.menu_button(
                    ToolbarMenu::Folder,
                    Button::new("folder-more")
                        .icon(IconName::More)
                        .tooltip("More")
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
        let ready = !self.path.is_empty() && self.session_connected(cx);
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
    pub fn crumbs(&self, cx: &Context<Self>) -> AnyElement {
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
                .rounded(px(6.))
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
                        this.font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme.text)
                    })
                    .into_any_element(),
            );
        }
        div()
            .id("breadcrumb")
            .occlude()
            .min_w_0()
            .h(px(28.))
            .gap(px(1.))
            .flex()
            .items_center()
            .overflow_hidden()
            .rounded(px(6.))
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
        let width = self.list_width(cx);
        // A link keeps its own icon: being one matters more than what it points to.
        let themed = match entry.kind {
            EntryKind::Symlink => None,
            EntryKind::Dir => file_icon(&entry.name, true, cx),
            _ => file_icon(&entry.name, false, cx),
        };
        let executable = entry.kind == EntryKind::File
            && entry.permissions.is_some_and(|mode| mode & 0o100 != 0);
        let (accent, muted, lit_fill) = (theme.accent, theme.text_muted, theme.selected);
        let mark = match (themed, entry.kind) {
            (Some(path), kind) => {
                let fallback = match kind {
                    EntryKind::Dir => (IconName::Folder, accent),
                    _ => (IconName::File, muted),
                };
                img(path)
                    .size(px(16.))
                    .with_fallback(move || {
                        Icon::new(fallback.0).color(fallback.1).into_any_element()
                    })
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
                .rounded(px(5.))
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
                .child(format::badge(&entry.name))
                .into_any_element(),
        };
        let (number, unit) = format::size_parts(entry);
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
                    .rounded(px(7.))
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
                    .child(
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
                            ),
                    )
                    .child(
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
                            .child(div().text_color(theme.text_disabled).child(unit)),
                    )
                    .when(width >= MODIFIED_FROM, |this| {
                        this.child(
                            div()
                                .id(("file-modified", index))
                                .w(px(MODIFIED_W))
                                .flex_none()
                                .text_size(crate::theme::ui_px(13.))
                                .text_color(theme.text_muted)
                                .tooltip(crate::widgets::tooltip::tooltip(format!(
                                    "{}\n{}",
                                    format::modified(entry.modified),
                                    format::modified_utc(entry.modified),
                                )))
                                .child(format::relative(entry.modified, SystemTime::now())),
                        )
                    })
                    .when(width >= PERMS_FROM, |this| {
                        this.child(
                            div()
                                .w(px(PERMS_W))
                                .flex()
                                .flex_none()
                                .gap(px(5.))
                                .font_family(crate::theme::mono_font())
                                .text_size(crate::theme::ui_px(13.))
                                .children(permission_triples(entry, &theme)),
                        )
                    }),
            )
            .into_any_element()
    }
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
        let width = self.list_width(cx);
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

    /// For the status bar: the selection or what the folder holds, and the room left on the server.
    /// Nothing until the folder has loaded.
    pub fn summary(&self) -> Option<String> {
        if self.path.is_empty() {
            return None;
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
            text.push_str(&format!(", {} free", format::bytes(free)));
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
                text.push_str(&format!(", {hidden} hidden"));
            }
        }
        text
    }

    /// The name of the folder on screen, once there is one.
    pub fn folder_name(&self) -> Option<&str> {
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
        (bytes > 0).then(|| format::bytes(bytes))
    }
}

/// The three groups of a permission string, each letter on its own: dashes step back, and the right
/// to run a file stands out.
fn permission_triples(entry: &RemoteEntry, theme: &crate::theme::Theme) -> Vec<Div> {
    let text = format::permissions(entry);
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
    let row = div().flex().flex_none().h(px(0.)).min_h(px(0.));
    let _ = row;
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
    /// The one file or folder the details are about: what is selected, if it is one.
    fn inspected(&self) -> Option<&RemoteEntry> {
        if self.selected.len() != 1 {
            return None;
        }
        let path = self.selected.iter().next()?;
        self.entries.iter().find(|entry| &entry.path == path)
    }

    /// Asks for the start of the file the details are about, when it changes.
    fn want_preview(&mut self, cx: &mut Context<Self>) {
        let wanted = self
            .inspected()
            .filter(|entry| entry.kind == EntryKind::File && entry.size > 0)
            .map(|entry| entry.path.clone());
        let Some(path) = wanted else {
            self.preview = None;
            return;
        };
        if self.width.get() < INSPECTOR_FROM
            || !self.state.read(cx).settings.show_file_details
            || self.preview.as_ref().is_some_and(|p| p.path == path)
            || !self.session_connected(cx)
        {
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
            text: None,
        });
    }

    /// How wide the details are, if they show: the width chosen, as far as the view allows.
    fn details_width(&self, cx: &App) -> Option<f32> {
        let width = self.width.get();
        let settings = &self.state.read(cx).settings;
        (width >= INSPECTOR_FROM && settings.show_file_details).then(|| self.details_size(cx))
    }

    /// The width chosen for the details, as far as the view allows, whether they show or not.
    fn details_size(&self, cx: &App) -> f32 {
        let settings = &self.state.read(cx).settings;
        self.dragging_details
            .unwrap_or(settings.file_details_width)
            .clamp(MIN_DETAILS, (self.width.get() - MIN_LIST).max(MIN_DETAILS))
    }

    /// What is left for the list. Its columns go by this.
    fn list_width(&self, cx: &App) -> f32 {
        let width = self.width.get();
        if width >= INSPECTOR_FROM {
            width - self.details_size(cx) * self.details_reveal.value()
        } else {
            width
        }
    }

    /// The line follows the pointer. A button let go where nothing listens ends the drag here.
    fn drag_details_line(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if event.pressed_button != Some(MouseButton::Left) {
            self.end_details_drag(cx);
            return;
        }
        self.dragging_details = Some(self.right_edge.get() - f32::from(event.position.x));
        cx.notify();
    }

    fn end_details_drag(&mut self, cx: &mut Context<Self>) {
        let width = self.details_width(cx);
        if self.dragging_details.take().is_some()
            && let Some(width) = width
        {
            self.state
                .update(cx, |state, cx| state.set_file_details_width(width, cx));
        }
        cx.notify();
    }

    /// The line between the list and the details, with more room than a hairline to grab it by.
    /// Not occluding: the view must keep hearing the pointer on it.
    fn details_line(&self, details: f32, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id("details-line")
            .absolute()
            .top_0()
            .bottom_0()
            .right(px(details - 4.))
            .w(px(7.))
            .cursor(gpui::CursorStyle::ResizeLeftRight)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    this.dragging_details = Some(details);
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .on_click(cx.listener(|this, event: &ClickEvent, _, cx| {
                if event.click_count() >= 2 {
                    this.state.update(cx, |state, cx| {
                        state.set_file_details_width(INSPECTOR_W, cx)
                    });
                }
            }))
            .into_any_element()
    }

    /// A circle on the line. With the details closed, half of one against the edge.
    /// It follows the details on their way, and flattens against the edge in the last stretch.
    fn details_toggle(&self, showing: f32, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let open = showing >= 14.;
        div()
            .id("toggle-file-details")
            .occlude()
            .absolute()
            .top(gpui::relative(0.5))
            .mt(px(-14.))
            .h(px(28.))
            .map(|this| {
                if open {
                    this.right(px(showing - 14.)).w(px(28.)).rounded_full()
                } else {
                    this.right_0().w(px(14. + showing)).rounded_l(px(14.))
                }
            })
            .flex()
            .items_center()
            .justify_center()
            .bg(theme.bg)
            .border_1()
            .border_color(if open {
                theme.lit_line
            } else {
                theme.strong_border
            })
            .when(!open, |this| this.border_r_0())
            .cursor_pointer()
            .tooltip(crate::widgets::tooltip::tooltip(if open {
                "Hide details"
            } else {
                "Show details"
            }))
            .on_click(cx.listener(|this, _, _, cx| {
                if this.state.read(cx).settings.animated_selection {
                    this.details_reveal.arm();
                }
                this.state
                    .update(cx, |state, cx| state.toggle_file_details(cx));
                cx.notify();
            }))
            .child(if open {
                Icon::new(IconName::ChevronRight)
                    .size(px(13.))
                    .color(theme.accent)
            } else {
                Icon::new(IconName::ChevronLeft)
                    .size(px(11.))
                    .color(theme.text_muted)
            })
            .into_any_element()
    }

    /// Flips one permission of the file in the details.
    fn toggle_permission(&mut self, path: String, bit: u32, cx: &mut Context<Self>) {
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
            .chmod(self.session, vec![path.clone()], mode);
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

    fn fact(label: &'static str, value: String, theme: &crate::theme::Theme) -> Div {
        div()
            .w_full()
            .flex()
            .justify_between()
            .gap(px(16.))
            .text_size(crate::theme::ui_px(13.))
            .child(
                div()
                    .flex_none()
                    .text_color(theme.text_disabled)
                    .child(label),
            )
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(theme.text)
                    .child(value),
            )
    }

    fn inspector_head(
        name: String,
        folder: String,
        mark: AnyElement,
        theme: &crate::theme::Theme,
    ) -> Div {
        div()
            .flex()
            .flex_none()
            .items_start()
            .gap(px(11.))
            .px(px(16.))
            .pt(px(14.))
            .pb(px(12.))
            .child(
                div()
                    .size(px(40.))
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .rounded(px(11.))
                    .child(mark),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_size(crate::theme::ui_px(15.))
                            .text_color(theme.text)
                            .child(name),
                    )
                    .child(
                        div()
                            .truncate()
                            .font_family(crate::theme::mono_font())
                            .text_size(crate::theme::ui_px(12.))
                            .text_color(theme.text_disabled)
                            .child(folder),
                    ),
            )
    }

    /// Beside the list: what the selected file or folder is, or the folder on screen.
    fn inspector(&mut self, details: f32, cx: &mut Context<Self>) -> AnyElement {
        let wide = details >= WIDE_DETAILS;
        let theme = cx.theme().clone();
        let ready = !self.path.is_empty() && self.session_connected(cx);
        let frame = div()
            .id("file-details")
            .h_full()
            .flex()
            .flex_col()
            .border_l_1()
            .border_color(theme.border)
            .overflow_y_scroll()
            .w(px(details))
            .flex_none();
        let Some(entry) = self.inspected().cloned() else {
            let folder = self.path.clone();
            let name = self.folder_name().unwrap_or("").to_string();
            let target = folder.clone();
            let mark = Icon::new(IconName::Folder)
                .size(px(22.))
                .color(theme.accent)
                .into_any_element();
            let used = self
                .free_space
                .zip(self.total_space)
                .map(|(free, total)| (free, total, 1. - free as f32 / total as f32));
            return frame
                .child(Self::inspector_head(name, folder, mark, &theme))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_none()
                        .px(px(16.))
                        .pb(px(14.))
                        .when(wide, |this| this.items_start())
                        .child(
                            Button::new("details-terminal-here")
                                .icon(IconName::Terminal)
                                .label("Open terminal here")
                                .style(ButtonStyle::Primary)
                                .disabled(!ready)
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    cx.emit(FilesEvent::OpenTerminalHere(target.clone()))
                                })),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_none()
                        .px(px(16.))
                        .child(Self::fact("Contents", self.contents_summary(), &theme)),
                )
                .children(used.map(|(free, total, used)| {
                    div()
                        .flex_none()
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .px(px(16.))
                        .pt(px(16.))
                        .child(Self::fact(
                            "Free space",
                            format!("{} of {}", format::bytes(free), format::bytes(total)),
                            &theme,
                        ))
                        .child(ProgressBar::new(used))
                }))
                .into_any_element();
        };
        let is_dir = entry.kind == EntryKind::Dir;
        let row = self.entries.iter().position(|e| e.path == entry.path);
        let mark = if is_dir {
            Icon::new(IconName::Folder)
                .size(px(22.))
                .color(theme.accent)
                .into_any_element()
        } else {
            div()
                .size(px(40.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(11.))
                .bg(gpui::Hsla {
                    a: 0.075,
                    ..theme.text
                })
                .font_family(crate::theme::mono_font())
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_size(crate::theme::ui_px(12.))
                .text_color(theme.text_muted)
                .child(format::badge(&entry.name))
                .into_any_element()
        };
        let preview_text = self
            .preview
            .as_ref()
            .filter(|preview| preview.path == entry.path)
            .and_then(|preview| preview.text.clone());
        let lines: Vec<String> = preview_text
            .map(|text| {
                text.lines()
                    .take(PREVIEW_LINES)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let folder = entry.path.clone();
        let primary = Button::new("details-open")
            .icon(if is_dir {
                IconName::Terminal
            } else {
                IconName::ExternalLink
            })
            .label(if is_dir {
                "Open terminal here"
            } else {
                "Edit in local editor"
            })
            .style(ButtonStyle::Primary)
            .disabled(!ready)
            .on_click(cx.listener(move |this, _, _, cx| {
                if is_dir {
                    cx.emit(FilesEvent::OpenTerminalHere(folder.clone()));
                } else if let Some(row) = row {
                    this.open(row, cx);
                }
            }));
        // In a narrow column the main action has a row of its own. With room, they all share one.
        let fit = |button: Button| if wide { button } else { button.share() };
        let (above, beside) = if wide {
            (None, Some(primary))
        } else {
            (Some(primary.whole()), None)
        };
        let actions = div()
            .flex()
            .flex_none()
            .flex_col()
            .gap(px(6.))
            .px(px(16.))
            .pb(px(14.))
            .children(above)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .children(beside)
                    .child(fit(Button::new("details-download")
                        .icon(IconName::Download)
                        .label("Download")
                        .disabled(!ready)
                        .on_click(cx.listener(|this, _, _, cx| this.download(cx)))))
                    .child(fit(Button::new("details-copy-path")
                        .icon(IconName::Copy)
                        .label("Copy path")
                        .on_click(cx.listener(|this, _, _, cx| this.copy_paths(cx)))))
                    .child(
                        self.menu_button(
                            ToolbarMenu::Selection,
                            Button::new("details-more")
                                .icon(IconName::More)
                                .tooltip("More")
                                .style(ButtonStyle::Ghost),
                            cx,
                        ),
                    ),
            );
        let preview_height = if wide { px(560.) } else { px(232.) };
        let preview_size = if wide { 12.5 } else { 11. };
        let preview = (!lines.is_empty()).then(|| {
            div().flex_none().px(px(16.)).child(
                div()
                    .id("file-preview")
                    .max_h(preview_height)
                    .overflow_y_scroll()
                    .overflow_x_hidden()
                    .px(px(12.))
                    .py(px(10.))
                    .rounded(px(9.))
                    .border_1()
                    .border_color(theme.border)
                    .bg(gpui::Hsla {
                        a: 0.04,
                        ..theme.text
                    })
                    .font_family(crate::theme::mono_font())
                    .text_size(px(preview_size))
                    .flex()
                    .flex_col()
                    .children(lines.iter().map(|line| preview_line(line, &theme))),
            )
        });
        let mut facts = div()
            .flex()
            .flex_none()
            .flex_col()
            .gap(px(6.))
            .px(px(16.))
            .pt(px(14.))
            .when(wide, |this| this.max_w(px(492.)));
        if !is_dir {
            facts = facts.child(Self::fact("Size", format::bytes(entry.size), &theme));
        }
        facts = facts.child(Self::fact(
            "Modified",
            format::modified(entry.modified),
            &theme,
        ));
        let grid = entry.permissions.map(|mode| {
            let path = entry.path.clone();
            let octal = format!("{:o}", mode & 0o777);
            let bit = |who: usize, what: usize, this: &mut Self, cx: &mut Context<Self>| {
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
                let _ = this;
                div()
                    .id(("permission", who * 3 + what))
                    .w(px(34.))
                    .h(px(24.))
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(if on { lit_line } else { border })
                    .when(on, |this| this.bg(lit_fill))
                    .when(ready, |this| {
                        this.cursor_pointer()
                            .hover(move |style| style.border_color(line))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.toggle_permission(path.clone(), mask, cx)
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
                .m(px(16.))
                .pt(px(14.))
                .border_t_1()
                .border_color(theme.border)
                .text_size(crate::theme::ui_px(13.))
                .when(wide, |this| this.max_w(px(460.)))
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
                let (a, b, c) = (
                    bit(who, 0, self, cx),
                    bit(who, 1, self, cx),
                    bit(who, 2, self, cx),
                );
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
        });
        frame
            .child(Self::inspector_head(
                entry.name.clone(),
                format::parent(&entry.path),
                mark,
                &theme,
            ))
            .child(actions)
            .children(preview)
            .child(facts)
            .children(grid)
            .into_any_element()
    }
}

impl Render for FilesView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        self.want_preview(cx);
        let width = self.width.get();
        let measured = self.width.clone();
        let right_edge = self.right_edge.clone();
        let details = self.details_width(cx);
        let details_size = self.details_size(cx);
        let shown = self.state.read(cx).settings.show_file_details;
        self.details_reveal.set(shown);
        if self.details_reveal.moving() {
            window.request_animation_frame();
        }
        // How much of them is on screen right now, on their way in or out.
        let details_now = (width >= INSPECTOR_FROM)
            .then(|| details_size * self.details_reveal.value())
            .filter(|width| *width >= 1.);
        let list_width = width - details_now.unwrap_or(0.);
        let overlay = session_overlay::with_reconnect(
            session_overlay::overlay_for(self.state.read(cx).session(self.session)),
            self.state.read(cx).reconnect_attempt(&self.host),
        );
        let heading = self
            .state
            .read(cx)
            .host(&self.host)
            .map(session_overlay::heading);
        let steps = session_overlay::steps_of(self.state.read(cx).host_session(&self.host));
        // A retry may have started a new session: what it asks is for this tab as well.
        let overlay = match self.state.read(cx).login_prompt(&self.host, cx) {
            Some(prompt) => {
                let own = present(&prompt, cx.entity_id(), &self.focus, window, cx);
                Some(session_overlay::Overlay::Login { steps, prompt, own })
            }
            None => overlay,
        };
        // Outside the view below: a key pressed in the prompt must not act on the files.
        let overlay = overlay.map(|overlay| {
            session_overlay::render(
                overlay,
                session_overlay::Surface {
                    heading,
                    docked: !self.path.is_empty(),
                    background: theme.bg,
                    focus: self.focus.clone(),
                    extra_steps: 0,
                    owner: cx.entity_id(),
                },
                &theme,
                window,
                cx,
                cx.listener(|this, _, _, cx| this.reconnect(cx)),
            )
        });
        let drop_highlight = theme.selected;
        let bar = {
            let (metrics, moved) = (self.list_scroll.clone(), self.list_scroll.clone());
            let this = cx.entity().downgrade();
            scrollbar(
                self.scroll_drag.clone(),
                theme.text_muted,
                move |_| Self::list_metrics(&metrics),
                move |position, _, cx| {
                    let base = moved.0.borrow().base_handle.clone();
                    let max = f32::from(base.max_offset().height);
                    base.set_offset(point(px(0.), px(-position * max)));
                    this.update(cx, |_, cx| cx.notify()).ok();
                },
            )
        };
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
            .when(self.dragging_details.is_some(), |this| {
                this.cursor(gpui::CursorStyle::ResizeLeftRight)
                    .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                        this.drag_details_line(event, cx)
                    }))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _: &gpui::MouseUpEvent, _, cx| {
                            this.end_details_drag(cx)
                        }),
                    )
            })
            .on_key_down(cx.listener(Self::on_key_down))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                this.upload_paths(paths.paths().to_vec(), cx)
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
                        right_edge.set(f32::from(bounds.right()));
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
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .flex()
                    .child(
                        div()
                            .h_full()
                            .flex()
                            .flex_col()
                            .min_w_0()
                            .flex_1()
                            // Clear of the half circle that opens the details again.
                            .when(width >= INSPECTOR_FROM, |this| {
                                this.pr(px(14. * (1. - self.details_reveal.value())))
                            })
                            .child(self.header(cx))
                            .child(
                                div()
                                    .id("file-list-area")
                                    .relative()
                                    .flex_1()
                                    .min_h_0()
                                    .w_full()
                                    .on_click(cx.listener(
                                        |this, event: &ClickEvent, window, cx| {
                                            // Clicks that reach here missed every row: clear the selection.
                                            window.focus(&this.focus);
                                            if !event.modifiers().control
                                                && !event.modifiers().shift
                                            {
                                                this.selected.clear();
                                                this.anchor = None;
                                                cx.notify();
                                            }
                                        },
                                    ))
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
                                            cx.processor(
                                                |this, range: Range<usize>, _window, cx| {
                                                    range
                                                        .map(|i| this.render_row(i, cx))
                                                        .collect::<Vec<_>>()
                                                },
                                            ),
                                        )
                                        .track_scroll(self.list_scroll.clone())
                                        .size_full(),
                                    )
                                    .child(bar),
                            ),
                    )
                    .when_some(
                        details_now.filter(|_| !self.path.is_empty()),
                        |this, now| {
                            // Cut off on its way, keeping its layout instead of squeezing it.
                            this.child(
                                div()
                                    .h_full()
                                    .flex_none()
                                    .overflow_hidden()
                                    .w(px(now))
                                    .child(self.inspector(details_size, cx)),
                            )
                        },
                    )
                    .when_some(
                        details.filter(|_| !self.path.is_empty() && !self.details_reveal.moving()),
                        |this, details| this.relative().child(self.details_line(details, cx)),
                    )
                    .when(width >= INSPECTOR_FROM && !self.path.is_empty(), |this| {
                        this.relative()
                            .child(self.details_toggle(details_now.unwrap_or(0.), cx))
                    }),
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
                    std::rc::Rc::new(move |_, cx| {
                        this.update(cx, |this, cx| {
                            this.menu = None;
                            cx.notify();
                        })
                        .ok();
                    }),
                )
            }));
        div().relative().size_full().child(view).children(overlay)
    }
}

#[cfg(test)]
#[path = "files_view_tests.rs"]
mod tests;
