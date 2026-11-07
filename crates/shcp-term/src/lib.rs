//! Shared terminal grid and runtime-neutral input handling.
mod colors;
mod cwd;
mod keys;
mod link;
mod mouse;
mod search;
mod snapshot;
mod types;
use alacritty_terminal::{
    Term,
    event::{Event as AlEvent, EventListener, WindowSize},
    grid::{Dimensions, Scroll},
    index::{Column, Line, Point, Side},
    selection::{Selection, SelectionType},
    term::{ClipboardType, Config, TermMode},
    vte::ansi::Processor,
};
use parking_lot::Mutex;
use std::sync::Arc;
pub use types::*;

/// Internal transport bridge used by shcp-core; it has no executor dependency.
#[doc(hidden)]
#[derive(Debug)]
pub enum TerminalCommand {
    Write(Vec<u8>),
    /// Only from a terminal that is `managed`: what wraps the text is up to the other side.
    Paste(String),
    /// For the other PCs on the same shell.
    Publish(String),
    Resize(u16, u16),
    Close,
}
#[doc(hidden)]
pub enum TerminalEvent {
    Changed,
    Title(String),
    /// The application asked to copy this text (OSC 52).
    Copy(String),
}
#[derive(Clone)]
struct Listener(Arc<Mutex<Vec<AlEvent>>>);
impl EventListener for Listener {
    fn send_event(&self, event: AlEvent) {
        self.0.lock().push(event);
    }
}
struct Size(u16, u16);
impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.1 as usize
    }
    fn screen_lines(&self) -> usize {
        self.1 as usize
    }
    fn columns(&self) -> usize {
        self.0 as usize
    }
}
struct State {
    term: Term<Listener>,
    parser: Processor,
    palette: Palette,
    title: Option<String>,
    /// The folder of the shell as far as known: `~` stands for its home.
    cwd: Option<String>,
    /// The shell reports its folder itself, so titles are not looked at.
    reports_cwd: bool,
    osc7: cwd::Osc7,
    alive: bool,
    /// The server has given it a shell, whether or not the shell has said anything yet.
    shell_started: bool,
    dirty: bool,
    search: Option<search::Search>,
    /// Something between here and the program sees its output too and answers it, so an answer
    /// from here would be a second one.
    managed: bool,
    /// How big it can be on screen, once the app has said. A managed terminal is the size the
    /// other side decides, which another PC may keep smaller.
    requested: Option<(u16, u16)>,
}
struct Inner {
    id: TerminalId,
    state: Mutex<State>,
    events: Listener,
    /// Replaced when the terminal moves to a new shell.
    input: Mutex<async_channel::Sender<TerminalCommand>>,
    notify: Arc<dyn Fn(TerminalEvent) + Send + Sync>,
}
#[derive(Clone)]
pub struct TerminalHandle(Arc<Inner>);
impl std::fmt::Debug for TerminalHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("TerminalHandle").field(&self.id()).finish()
    }
}
impl TerminalHandle {
    #[doc(hidden)]
    pub fn new(
        id: TerminalId,
        cols: u16,
        rows: u16,
        scrollback: usize,
        palette: Palette,
        notify: Arc<dyn Fn(TerminalEvent) + Send + Sync>,
    ) -> (Self, async_channel::Receiver<TerminalCommand>) {
        let listener = Listener(Arc::new(Mutex::new(vec![])));
        let term = Term::new(
            Config {
                scrolling_history: scrollback,
                ..Default::default()
            },
            &Size(cols.max(2), rows.max(1)),
            listener.clone(),
        );
        let (input, receiver) = async_channel::unbounded();
        (
            Self(Arc::new(Inner {
                id,
                state: Mutex::new(State {
                    term,
                    parser: Processor::new(),
                    palette,
                    title: None,
                    cwd: None,
                    reports_cwd: false,
                    osc7: cwd::Osc7::default(),
                    alive: true,
                    shell_started: false,
                    dirty: false,
                    search: None,
                    managed: false,
                    requested: None,
                }),
                events: listener,
                input: Mutex::new(input),
                notify,
            })),
            receiver,
        )
    }
    #[doc(hidden)]
    pub fn dimensions(&self) -> (u16, u16) {
        let s = self.0.state.lock();
        (s.term.columns() as u16, s.term.screen_lines() as u16)
    }
    pub fn id(&self) -> TerminalId {
        self.0.id
    }
    fn send(&self, command: TerminalCommand) {
        let _ = self.0.input.lock().try_send(command);
    }
    /// Gets the terminal ready for a new shell, keeping the scrollback. The previous program may
    /// have died in the alternate screen or with mouse tracking on: those modes are switched off.
    #[doc(hidden)]
    pub fn reattach(&self, note: &str) -> async_channel::Receiver<TerminalCommand> {
        let (input, receiver) = async_channel::unbounded();
        *self.0.input.lock() = input;
        {
            let mut state = self.0.state.lock();
            state.alive = true;
            // A new shell starts somewhere else.
            state.cwd = None;
            state.reports_cwd = false;
        }
        self.feed(
            b"\x1b[?1049l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1004l\x1b[?1005l\x1b[?1006l\
              \x1b[?2004l\x1b[?1l\x1b>\x1b[?25h\x1b[0m\r\n",
        );
        self.feed(format!("\x1b[2m--- {note} ---\x1b[0m\r\n").as_bytes());
        receiver
    }
    fn changed(&self) {
        let mut state = self.0.state.lock();
        if state.dirty {
            return;
        }
        state.dirty = true;
        // Serialize notification with snapshot's dirty acknowledgement.
        (self.0.notify)(TerminalEvent::Changed);
    }
    /// Replaces the colors; cells are re-resolved on the next snapshot.
    pub fn set_palette(&self, palette: Palette) {
        self.0.state.lock().palette = palette;
        self.changed();
    }
    pub fn resize(&self, cols: u16, rows: u16) {
        let (cols, rows) = (cols.max(2), rows.max(1));
        let mut state = self.0.state.lock();
        let changed = state.requested != Some((cols, rows));
        state.requested = Some((cols, rows));
        if state.managed {
            drop(state);
            if changed {
                self.send(TerminalCommand::Resize(cols, rows));
            }
            return;
        }
        if state.term.columns() == cols as usize && state.term.screen_lines() == rows as usize {
            return;
        }
        state.term.resize(Size(cols, rows));
        drop(state);
        self.send(TerminalCommand::Resize(cols, rows));
        self.changed();
    }
    /// Takes the size the other side decided, which it needs no telling about.
    #[doc(hidden)]
    pub fn fit(&self, cols: u16, rows: u16) {
        let (cols, rows) = (cols.max(2), rows.max(1));
        let mut state = self.0.state.lock();
        if state.term.columns() == cols as usize && state.term.screen_lines() == rows as usize {
            return;
        }
        state.term.resize(Size(cols, rows));
        drop(state);
        self.changed();
    }
    #[doc(hidden)]
    pub fn requested(&self) -> Option<(u16, u16)> {
        self.0.state.lock().requested
    }
    /// With no one else to decide its size any more, it goes back to the one it was asked for.
    #[doc(hidden)]
    pub fn set_managed(&self, managed: bool) {
        let mut state = self.0.state.lock();
        state.managed = managed;
        if !managed && let Some((cols, rows)) = state.requested {
            state.term.resize(Size(cols, rows));
        }
    }
    #[doc(hidden)]
    pub fn publish(&self, text: String) {
        self.send(TerminalCommand::Publish(text));
    }
    fn write(&self, bytes: Vec<u8>) {
        self.scroll_to_bottom();
        if self.is_alive() {
            self.send(TerminalCommand::Write(bytes));
        }
    }
    pub fn key(&self, key: KeyInput) {
        let app = self
            .0
            .state
            .lock()
            .term
            .mode()
            .contains(TermMode::APP_CURSOR);
        self.write(keys::encode(key, app));
    }
    /// True when the application asked for mouse events (the UI then forwards clicks,
    /// drags and the wheel instead of selecting/scrolling locally; Shift overrides).
    pub fn mouse_mode(&self) -> bool {
        self.0
            .state
            .lock()
            .term
            .mode()
            .intersects(TermMode::MOUSE_MODE)
    }
    /// Reports a mouse event to the application. Returns false when it was not wanted.
    pub fn mouse(&self, input: MouseInput) -> bool {
        let mode = *self.0.state.lock().term.mode();
        match mouse::encode(input, mode) {
            Some(bytes) => {
                if self.is_alive() {
                    self.send(TerminalCommand::Write(bytes));
                }
                true
            }
            None => false,
        }
    }
    /// Focus in/out reports (`CSI I` / `CSI O`) for applications that enabled them.
    pub fn focus_changed(&self, focused: bool) {
        let wanted = self
            .0
            .state
            .lock()
            .term
            .mode()
            .contains(TermMode::FOCUS_IN_OUT);
        if wanted && self.is_alive() {
            let bytes = if focused {
                b"\x1b[I".to_vec()
            } else {
                b"\x1b[O".to_vec()
            };
            self.send(TerminalCommand::Write(bytes));
        }
    }
    pub fn text(&self, s: &str) {
        self.write(s.as_bytes().to_vec());
    }
    pub fn paste(&self, s: &str) {
        let (bracketed, managed) = {
            let state = self.0.state.lock();
            (
                state.term.mode().contains(TermMode::BRACKETED_PASTE),
                state.managed,
            )
        };
        if managed {
            self.scroll_to_bottom();
            if self.is_alive() {
                self.send(TerminalCommand::Paste(s.replace('\x1b', "")));
            }
            return;
        }
        let text = s.replace("\r\n", "\r").replace('\n', "\r");
        // Strip ESC in bracketed paste to prevent pasted text terminating the envelope.
        self.write(if bracketed {
            format!("\x1b[200~{}\x1b[201~", text.replace('\x1b', "")).into_bytes()
        } else {
            text.into_bytes()
        });
    }
    pub fn scroll(&self, delta_lines: i32) {
        let mut state = self.0.state.lock();
        if state
            .term
            .mode()
            .intersects(TermMode::ALT_SCREEN | TermMode::MOUSE_MODE)
        {
            let app = state.term.mode().contains(TermMode::APP_CURSOR);
            drop(state);
            let key = KeyInput {
                key: if delta_lines > 0 { Key::Up } else { Key::Down },
                ctrl: false,
                alt: false,
                shift: false,
            };
            for _ in 0..delta_lines.unsigned_abs().min(1000) {
                self.write(keys::encode(key, app));
            }
        } else {
            state.term.scroll_display(Scroll::Delta(delta_lines));
            drop(state);
            self.changed();
        }
    }
    pub fn scroll_to_bottom(&self) {
        let mut state = self.0.state.lock();
        if state.term.grid().display_offset() == 0 {
            return;
        }
        state.term.scroll_display(Scroll::Bottom);
        drop(state);
        self.changed();
    }
    pub fn select_begin(&self, point: GridPoint, kind: SelectKind) {
        let mut s = self.0.state.lock();
        let point = grid_point(&s, point);
        s.term.selection = Some(Selection::new(
            match kind {
                SelectKind::Simple => SelectionType::Simple,
                SelectKind::Word => SelectionType::Semantic,
                SelectKind::Line => SelectionType::Lines,
            },
            point,
            Side::Left,
        ));
        drop(s);
        self.changed();
    }
    pub fn select_update(&self, point: GridPoint) {
        let mut s = self.0.state.lock();
        let point = grid_point(&s, point);
        if let Some(selection) = &mut s.term.selection {
            selection.update(point, Side::Right);
        }
        drop(s);
        self.changed();
    }
    pub fn select_clear(&self) {
        self.0.state.lock().term.selection = None;
        self.changed();
    }
    pub fn selection_text(&self) -> Option<String> {
        self.0.state.lock().term.selection_to_string()
    }
    pub fn title(&self) -> Option<String> {
        self.0.state.lock().title.clone()
    }
    /// The folder of the shell as far as known. `~` stands for its home.
    pub fn cwd(&self) -> Option<String> {
        self.0.state.lock().cwd.clone()
    }
    /// How far up the view is, how much history there is above the screen, and screen lines.
    pub fn scroll_position(&self) -> (usize, usize, usize) {
        let state = self.0.state.lock();
        let grid = state.term.grid();
        (
            grid.display_offset(),
            grid.history_size(),
            state.term.screen_lines(),
        )
    }
    /// Shows the screen this many lines up from the bottom.
    pub fn scroll_to_offset(&self, offset: usize) {
        let mut state = self.0.state.lock();
        let current = state.term.grid().display_offset() as i32;
        state
            .term
            .scroll_display(Scroll::Delta(offset as i32 - current));
        drop(state);
        self.changed();
    }
    pub fn is_alive(&self) -> bool {
        self.0.state.lock().alive
    }
    pub fn shell_started(&self) -> bool {
        self.0.state.lock().shell_started
    }
    #[doc(hidden)]
    pub fn mark_shell_started(&self) {
        self.0.state.lock().shell_started = true;
        self.changed();
    }
    #[doc(hidden)]
    pub fn finish(&self) {
        self.0.state.lock().alive = false;
        self.0.input.lock().close();
        self.changed();
    }
    #[doc(hidden)]
    pub fn close(&self) {
        self.send(TerminalCommand::Close);
        self.finish();
    }
    #[doc(hidden)]
    pub fn feed(&self, bytes: &[u8]) {
        {
            let mut s = self.0.state.lock();
            if let Some(path) = s.osc7.feed(bytes) {
                s.cwd = Some(path);
                s.reports_cwd = true;
            }
            let State { term, parser, .. } = &mut *s;
            parser.advance(term, bytes);
        }
        let events = std::mem::take(&mut *self.0.events.0.lock());
        let managed = self.0.state.lock().managed;
        for event in events {
            match event {
                AlEvent::PtyWrite(_) | AlEvent::TextAreaSizeRequest(_) if managed => {}
                AlEvent::PtyWrite(text) => {
                    self.send(TerminalCommand::Write(text.into_bytes()));
                }
                AlEvent::Title(title) => {
                    {
                        let mut state = self.0.state.lock();
                        state.title = Some(title.clone());
                        if !state.reports_cwd
                            && let Some(path) = cwd::from_title(&title)
                        {
                            state.cwd = Some(path);
                        }
                    }
                    (self.0.notify)(TerminalEvent::Title(title));
                }
                AlEvent::ResetTitle => {
                    self.0.state.lock().title = None;
                    (self.0.notify)(TerminalEvent::Title(String::new()));
                }
                // Windows has no primary selection, so only the real clipboard counts. Reading
                // the clipboard stays denied: a server must not see what is on this PC.
                AlEvent::ClipboardStore(ClipboardType::Clipboard, text) => {
                    (self.0.notify)(TerminalEvent::Copy(text));
                }
                AlEvent::ColorRequest(index, format) => {
                    let s = self.0.state.lock();
                    let color = s.term.colors()[index].unwrap_or_else(|| {
                        let Rgb(r, g, b) = colors::indexed(&s.palette, index);
                        alacritty_terminal::vte::ansi::Rgb { r, g, b }
                    });
                    drop(s);
                    self.send(TerminalCommand::Write(format(color).into_bytes()));
                }
                AlEvent::TextAreaSizeRequest(format) => {
                    let s = self.0.state.lock();
                    let size = WindowSize {
                        num_lines: s.term.screen_lines() as u16,
                        num_cols: s.term.columns() as u16,
                        cell_width: 0,
                        cell_height: 0,
                    };
                    drop(s);
                    self.send(TerminalCommand::Write(format(size).into_bytes()));
                }
                _ => {}
            }
        }
        self.changed();
    }
}
fn grid_point(s: &State, p: GridPoint) -> Point {
    Point::new(
        Line(
            (p.row as usize).min(s.term.screen_lines() - 1) as i32
                - s.term.grid().display_offset() as i32,
        ),
        Column((p.col as usize).min(s.term.columns() - 1)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terminal() -> (
        TerminalHandle,
        Arc<Mutex<Vec<String>>>,
        async_channel::Receiver<TerminalCommand>,
    ) {
        let copied = Arc::new(Mutex::new(Vec::new()));
        let sink = copied.clone();
        let notify = Arc::new(move |event| {
            if let TerminalEvent::Copy(text) = event {
                sink.lock().push(text);
            }
        });
        let palette = Palette {
            ansi: [Rgb(0, 0, 0); 16],
            fg: Rgb(255, 255, 255),
            bg: Rgb(0, 0, 0),
            cursor: Rgb(255, 255, 255),
        };
        let (terminal, input) = TerminalHandle::new(TerminalId(1), 80, 24, 0, palette, notify);
        (terminal, copied, input)
    }

    #[test]
    fn reattach_keeps_the_scrollback_and_drops_the_old_modes() {
        let (terminal, _copied, old_input) = terminal();
        terminal.feed(b"before\r\n\x1b[?1049h\x1b[?1000h");
        assert!(terminal.mouse_mode());
        terminal.finish();
        assert!(!terminal.is_alive());
        let input = terminal.reattach("reconnected");
        assert!(terminal.is_alive());
        assert!(!terminal.mouse_mode());
        let snapshot = terminal.snapshot();
        let text: String = snapshot.cells.iter().map(|c| c.ch).collect();
        assert!(text.contains("before"));
        assert!(text.contains("--- reconnected ---"));
        terminal.text("ls");
        assert!(matches!(input.try_recv(), Ok(TerminalCommand::Write(_))));
        assert!(old_input.is_closed());
    }

    #[test]
    fn a_managed_terminal_leaves_the_answers_to_the_other_side() {
        let (terminal, _copied, input) = terminal();
        terminal.feed(b"\x1b[6n");
        assert!(matches!(input.try_recv(), Ok(TerminalCommand::Write(_))));
        terminal.set_managed(true);
        terminal.feed(b"\x1b[6n\x1b[c\x1b[18t");
        assert!(input.try_recv().is_err());
    }

    #[test]
    fn a_managed_terminal_only_changes_size_when_told() {
        let (terminal, _copied, input) = terminal();
        terminal.set_managed(true);
        terminal.resize(120, 40);
        assert!(matches!(
            input.try_recv(),
            Ok(TerminalCommand::Resize(120, 40))
        ));
        assert_eq!(terminal.dimensions(), (80, 24));
        // The same again is nothing new, even when the answer was another size.
        terminal.fit(100, 30);
        terminal.resize(120, 40);
        assert!(input.try_recv().is_err());
        assert_eq!(terminal.dimensions(), (100, 30));
        terminal.set_managed(false);
        assert_eq!(terminal.dimensions(), (120, 40));
    }

    #[test]
    fn a_managed_terminal_hands_over_a_paste_as_it_is() {
        let (terminal, _copied, input) = terminal();
        terminal.set_managed(true);
        terminal.paste("a\nb\x1b");
        assert!(matches!(
            input.try_recv(),
            Ok(TerminalCommand::Paste(text)) if text == "a\nb"
        ));
    }

    #[test]
    fn osc52_copy_reaches_the_clipboard() {
        let (terminal, copied, _input) = terminal();
        // "héllo" in base64, with both terminators applications use.
        terminal.feed(b"\x1b]52;c;aMOpbGxv\x07");
        terminal.feed(b"\x1b]52;c;aMOpbGxv\x1b\\");
        assert_eq!(*copied.lock(), ["héllo", "héllo"]);
    }

    #[test]
    fn osc52_without_a_target_means_the_clipboard() {
        let (terminal, copied, _input) = terminal();
        terminal.feed(b"\x1b]52;;aGVsbG8=\x07");
        assert_eq!(*copied.lock(), ["hello"]);
    }

    #[test]
    fn osc52_primary_selection_is_ignored() {
        let (terminal, copied, _input) = terminal();
        terminal.feed(b"\x1b]52;p;aGVsbG8=\x07");
        assert!(copied.lock().is_empty());
    }

    #[test]
    fn osc52_cannot_read_the_clipboard() {
        let (terminal, copied, input) = terminal();
        terminal.feed(b"\x1b]52;c;?\x07");
        assert!(copied.lock().is_empty());
        // No answer goes back to the server.
        assert!(input.try_recv().is_err());
    }
}
