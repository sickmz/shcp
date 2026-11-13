//! Terminals that outlive the app and the connection, kept alive by tmux on the server.
//!
//! Each terminal is one tmux session with one pane, on a tmux server of its own socket. tmux control
//! mode hands over what the program writes, untouched, so the terminal here works as with a plain
//! shell: its own scrollback, search and selection. Several PCs can be on a session at once.
//!
//! The tabs and splits belong to the app, but how they are arranged is kept in a tmux option, in
//! hex, which tells every terminal of every PC when it changes. A server without a recent tmux gets
//! plain shells instead.
use crate::{
    Event, NoticeLevel, Persistence, SharedTabs, files,
    session::{Context, OpenTerminal, Ssh},
    session_log::SessionLog,
    term::TerminalCommand,
    terminal,
};
use anyhow::Result;
use parking_lot::Mutex;
use russh::ChannelMsg;
use std::{
    collections::HashSet,
    fmt::Write as _,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::time::{Instant, sleep, timeout, timeout_at};

/// What the start command prints when it cannot go on: no tmux, or one older than 3.2. Older ones
/// send output to a control client without any limit.
const NO_TMUX: &str = "SHCP_NO_TMUX";
/// Until tmux first answers. A login shell looking for it can be slow, and a server that runs
/// something else instead of the command never answers.
const START_TIMEOUT: Duration = Duration::from_secs(15);
/// Until the screen and the history arrive, which over a slow link takes a while.
const REPLY_TIMEOUT: Duration = Duration::from_secs(60);
/// A connection does not wait for its terminals to be listed any longer than this.
const SURVEY_TIMEOUT: Duration = Duration::from_secs(10);
/// Finds tmux, under `sh`: `$T` is not executable when there is none.
const FIND_TMUX: &str = "T=$(command -v tmux || \"${SHELL:-/bin/sh}\" -lc \"command -v tmux\" \
                         2>/dev/null | tail -n 1);";
/// What `tmux -V` says when it is too old, as a pattern of `case`.
const OLD_TMUX: &str = "\"tmux \"[012].*|\"tmux 3.\"[01]|\"tmux 3.\"[01][a-z]";
/// What `report_keeping` prints when the server has what it takes.
const TMUX_USABLE: &str = "SHCP_TMUX_USABLE";
/// The line between the terminals and the tabs in what `survey` reads.
const SURVEY_TABS: &str = "SHCP_TABS";
const LAYOUT_SUBSCRIPTION: &str = "shcp-layout";
/// Time for tmux to end a closed terminal before its channel goes away.
const KILL_WAIT: Duration = Duration::from_secs(2);
const RENAME_WAIT: Duration = Duration::from_millis(300);
const MAX_HISTORY: usize = 50_000;
const KEYS_PER_LINE: usize = 512;
const PASTE_PER_LINE: usize = 16 * 1024;
/// What a new shell takes from the connection it starts on, instead of the one tmux started on.
const ENVIRONMENT: &str = "DISPLAY KRB5CCNAME SSH_ASKPASS SSH_AGENT_PID SSH_CONNECTION SSH_CLIENT \
                           WINDOWID XAUTHORITY LANG LC_ALL LC_CTYPE XDG_RUNTIME_DIR XDG_SESSION_ID \
                           DBUS_SESSION_BUS_ADDRESS";
/// Everything `PaneInfo` reads, in its order.
const INFO_FORMAT: &str = "#{pane_width} #{pane_height} #{history_size} #{cursor_x} #{cursor_y} \
                           #{cursor_flag} #{alternate_on} #{alternate_saved_x} #{alternate_saved_y} \
                           #{scroll_region_upper} #{scroll_region_lower} #{keypad_cursor_flag} \
                           #{keypad_flag} #{origin_flag} #{wrap_flag} #{insert_flag} \
                           #{mouse_standard_flag} #{mouse_button_flag} #{mouse_all_flag} \
                           #{mouse_utf8_flag} #{mouse_sgr_flag} #{@shcp_started}";

/// A name for a terminal that lasts: it finds its tmux session again after a restart.
pub fn new_terminal_key() -> String {
    static COUNT: AtomicU64 = AtomicU64::new(0);
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    format!("{time:x}{:x}", COUNT.fetch_add(1, Ordering::Relaxed))
}

fn session_name(key: &str) -> String {
    let key: String = key.chars().filter(char::is_ascii_alphanumeric).collect();
    format!("shcp-{key}")
}

/// What a session is called on its way out, which tells every PC on it that it was closed and did
/// not just end.
fn closing_name(name: &str) -> String {
    name.replacen("shcp-", "shcp-closing-", 1)
}

/// What a connection knows about the tmux of its host.
#[derive(Default)]
pub(crate) struct HostState {
    /// The sessions there when it connected, when tmux was running.
    live: Mutex<Option<HashSet<String>>>,
    /// The user knows that tmux could not be used on this connection.
    told: AtomicBool,
}

/// The user just asked for the terminals of this host to last: says whether the server can do it.
pub(crate) async fn report_keeping(ctx: Arc<Context>, ssh: Arc<Ssh>) {
    let command = format!(
        "sh -c '{FIND_TMUX} [ -x \"$T\" ] || {{ echo {NO_TMUX}; exit 0; }}; \
         case \"$(\"$T\" -V 2>/dev/null)\" in {OLD_TMUX}) echo {NO_TMUX}; exit 0;; esac; \
         echo {TMUX_USABLE}'"
    );
    let output = timeout(SURVEY_TIMEOUT, files::read_all(&ssh, command))
        .await
        .unwrap_or_default();
    let output = String::from_utf8_lossy(&output);
    let says = |word: &str| output.lines().any(|line| line == word);
    let alias = &ctx.host.alias;
    if says(TMUX_USABLE) {
        ctx.emit(Event::TmuxFound {
            host: ctx.host.id.clone(),
        });
        ctx.notice(
            NoticeLevel::Info,
            format!("New terminals on {alias} keep running when shcp closes"),
        );
    } else if says(NO_TMUX) {
        ctx.emit(Event::TmuxMissing {
            host: ctx.host.id.clone(),
        });
    } else {
        ctx.notice(
            NoticeLevel::Warning,
            format!("Could not check whether {alias} has tmux"),
        );
    }
}

/// tmux is there but this terminal could not use it, which may pass. Says so once per connection.
pub(crate) fn tell_unavailable(ctx: &Context) {
    if !ctx.tmux.told.swap(true, Ordering::Relaxed) {
        ctx.notice(
            NoticeLevel::Important,
            format!(
                "Terminals on {} end when shcp closes: tmux did not start on the server",
                ctx.host.alias
            ),
        );
    }
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn from_hex(text: &[u8]) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    text.chunks(2)
        .map(|pair| {
            let digit = |byte: u8| char::from(byte).to_digit(16);
            Some((digit(pair[0])? * 16 + digit(pair[1])?) as u8)
        })
        .collect()
}

/// The tabs as they go in the tmux option: nothing in hex means anything to tmux.
pub(crate) fn encode_tabs(tabs: &SharedTabs) -> Option<String> {
    let text = toml::to_string(tabs).ok()?;
    Some(to_hex(text.as_bytes()))
}

fn decode_tabs(hex: &[u8]) -> Option<SharedTabs> {
    let text = String::from_utf8(from_hex(hex)?).ok()?;
    toml::from_str(&text).ok()
}

/// Reads what runs on the server, before any terminal of this connection attaches: one that
/// should be there and is not was closed from elsewhere, and one that attached first would have
/// started tmux and make the server look in use.
pub(crate) async fn survey(ctx: &Context, ssh: &Ssh) {
    let command = format!(
        "sh -c '{FIND_TMUX} [ -x \"$T\" ] || exit 0; \
         \"$T\" -u -L shcp list-sessions -F \"#{{session_name}}\" 2>/dev/null || exit 0; \
         echo {SURVEY_TABS}; \"$T\" -L shcp show-options -gqv @shcp_layout'"
    );
    let output = timeout(SURVEY_TIMEOUT, files::read_all(ssh, command))
        .await
        .unwrap_or_default();
    let (names, tabs) = parse_survey(&output);
    let live = names
        .iter()
        .filter_map(|name| name.strip_prefix("shcp-"))
        .map(str::to_string)
        .collect();
    if !names.is_empty() {
        *ctx.tmux.live.lock() = Some(names.into_iter().collect());
    }
    ctx.emit(Event::SharedServer {
        host: ctx.host.id.clone(),
        live,
        tabs,
    });
}

/// A shell's startup files may have printed anything, so only what can be ours counts.
fn parse_survey(output: &[u8]) -> (Vec<String>, Option<SharedTabs>) {
    let text = String::from_utf8_lossy(output);
    let mut lines = text.lines();
    let mut names = Vec::new();
    for line in lines.by_ref() {
        if line == SURVEY_TABS {
            break;
        }
        if line.starts_with("shcp-") && !line.starts_with("shcp-closing-") {
            names.push(line.to_string());
        }
    }
    let tabs = lines.find_map(|line| decode_tabs(line.as_bytes()));
    (names, tabs)
}

/// Runs under any login shell: all of it goes through `sh`, on one line and with no `'` inside.
/// The shell starts without tmux's own variables, so `tmux` in it reaches the user's server, not
/// this one, and programs behave as on a plain connection.
fn start_command(name: &str, cols: u16, rows: u16, history: usize, agent: bool) -> String {
    let (link, agent) = if agent {
        (
            "S=\"$HOME/.ssh/shcp-agent-$(uname -n).sock\"; [ -n \"$SSH_AUTH_SOCK\" ] && \
             ln -sfn \"$SSH_AUTH_SOCK\" \"$S\" 2>/dev/null; ",
            " -e \"SSH_AUTH_SOCK=$S\"",
        )
    } else {
        ("", "")
    };
    let history = history.min(MAX_HISTORY);
    // With several PCs on a terminal it has the size of the smallest, so no PC has it cut off.
    format!(
        "sh -c '{FIND_TMUX} [ -x \"$T\" ] || {{ echo {NO_TMUX}; exit 0; }}; \
         case \"$(\"$T\" -V 2>/dev/null)\" in {OLD_TMUX}) \
         echo {NO_TMUX}; exit 0;; esac; {link}exec \"$T\" -u -L shcp -f /dev/null -C \
         set -g status off \\; set -g automatic-rename off \\; set -g window-size smallest \\; \
         set -g history-limit {history} \\; set -g update-environment \"{ENVIRONMENT}\" \\; \
         new-session -A -s {name} -x {cols} -y {rows}{agent} \
         env -u TMUX -u TMUX_PANE -u TERM_PROGRAM -u TERM_PROGRAM_VERSION TERM=xterm-256color \
         \"${{SHELL:-/bin/sh}}\" -l'"
    )
}

#[derive(Debug, PartialEq)]
enum Message {
    /// What the program wrote.
    Output(Vec<u8>),
    /// The answer to a command. `control` is false for those on the command line that started tmux.
    Reply {
        control: bool,
        ok: bool,
        body: Vec<u8>,
    },
    /// A line outside the protocol, like what a shell's startup files print.
    Text(Vec<u8>),
    Exit,
    /// The terminal has this many columns and rows now.
    Resized(u16, u16),
    /// The value of the option with the tabs, in hex.
    Tabs(Vec<u8>),
    /// A session has a new name. It could be another terminal's: every client hears of them all.
    Renamed(Vec<u8>),
}

/// The size in `%layout-change @1 b25d,80x24,0,0,0 ...`.
fn layout_size(rest: &[u8]) -> Option<(u16, u16)> {
    let text = std::str::from_utf8(rest).ok()?;
    let layout = text.split(' ').nth(1)?;
    let (cols, rows) = layout.split(',').nth(1)?.split_once('x')?;
    Some((cols.parse().ok()?, rows.parse().ok()?))
}

/// What follows the first space, where a message has its id.
fn after_id(rest: &[u8]) -> &[u8] {
    let start = rest
        .iter()
        .position(|byte| *byte == b' ')
        .map_or(0, |at| at + 1);
    &rest[start..]
}

struct Block {
    /// The time, number and flags, which its last line repeats.
    guard: Vec<u8>,
    body: Vec<u8>,
    empty: bool,
}

/// Cuts what tmux sends into messages. It may arrive cut anywhere.
#[derive(Default)]
struct Parser {
    line: Vec<u8>,
    block: Option<Block>,
}

impl Parser {
    fn feed(&mut self, mut data: &[u8], out: &mut Vec<Message>) {
        while let Some(end) = data.iter().position(|byte| *byte == b'\n') {
            self.line.extend_from_slice(&data[..end]);
            let line = std::mem::take(&mut self.line);
            self.handle(line, out);
            data = &data[end + 1..];
        }
        self.line.extend_from_slice(data);
    }

    fn handle(&mut self, line: Vec<u8>, out: &mut Vec<Message>) {
        if let Some(block) = &mut self.block {
            // What a command returns is free text: a line that only looks like the end is text.
            let end = line
                .strip_prefix(b"%end ")
                .map(|guard| (true, guard))
                .or_else(|| line.strip_prefix(b"%error ").map(|guard| (false, guard)));
            if let Some((ok, guard)) = end
                && guard == block.guard
            {
                let control = guard.ends_with(b" 1");
                let body = std::mem::take(&mut block.body);
                self.block = None;
                out.push(Message::Reply { control, ok, body });
                return;
            }
            if !block.empty {
                block.body.push(b'\n');
            }
            block.body.extend_from_slice(&line);
            block.empty = false;
        } else if let Some(guard) = line.strip_prefix(b"%begin ") {
            self.block = Some(Block {
                guard: guard.to_vec(),
                body: Vec::new(),
                empty: true,
            });
        } else if let Some(rest) = line.strip_prefix(b"%output ") {
            out.push(Message::Output(decode_output(after_id(rest))));
        } else if line == b"%exit" || line.starts_with(b"%exit ") {
            out.push(Message::Exit);
        } else if let Some(rest) = line.strip_prefix(b"%layout-change ") {
            if let Some((cols, rows)) = layout_size(rest) {
                out.push(Message::Resized(cols, rows));
            }
        } else if let Some(rest) = line.strip_prefix(b"%session-renamed ") {
            out.push(Message::Renamed(after_id(rest).to_vec()));
        } else if let Some(rest) = line.strip_prefix(b"%subscription-changed ") {
            // `name $1 - - - : value`
            if rest.starts_with(LAYOUT_SUBSCRIPTION.as_bytes())
                && let Some(at) = rest.windows(3).position(|part| part == b" : ")
            {
                out.push(Message::Tabs(rest[at + 3..].to_vec()));
            }
        } else if !line.starts_with(b"%") {
            out.push(Message::Text(line));
        }
    }
}

/// tmux writes bytes below space and the backslash as `\ooo`. The rest is as the program wrote it.
fn decode_output(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut index = 0;
    while index < data.len() {
        let digits = data.get(index + 1..index + 4);
        match (data[index], digits) {
            (b'\\', Some(digits)) if digits.iter().all(|digit| (b'0'..=b'7').contains(digit)) => {
                let value = digits
                    .iter()
                    .fold(0u32, |value, digit| value * 8 + u32::from(digit - b'0'));
                out.push(value as u8);
                index += 4;
            }
            (byte, _) => {
                out.push(byte);
                index += 1;
            }
        }
    }
    out
}

/// One line of commands, which tmux runs with nothing from the program in between. So what it wrote
/// before the last answer is in the captures, and what it writes after comes as output.
fn attach_batch(size: Option<(u16, u16)>) -> String {
    let mut commands = Vec::new();
    if let Some((cols, rows)) = size {
        commands.push(format!("refresh-client -C {cols}x{rows}"));
    }
    commands.extend([
        format!("list-panes -F '{INFO_FORMAT}'"),
        "display-message -p '#{pane_path}'".to_string(),
        "display-message -p '#{pane_title}'".to_string(),
        "set-option @shcp_started 1".to_string(),
        format!("refresh-client -B '{LAYOUT_SUBSCRIPTION}::#{{@shcp_layout}}'"),
        "capture-pane -p -e -J -S - -E -1".to_string(),
        "capture-pane -p -e -J -S 0 -E -".to_string(),
        "capture-pane -p -e -J -a -q -S - -E -".to_string(),
        "capture-pane -p -P -C".to_string(),
    ]);
    let mut line = commands.join(" ; ");
    line.push('\n');
    line
}
/// How many answers `attach_batch` gets.
fn attach_replies(size: Option<(u16, u16)>) -> usize {
    9 + usize::from(size.is_some())
}

/// The state of a pane as tmux reports it.
#[derive(Debug, Default, PartialEq)]
struct PaneInfo {
    width: u16,
    height: u16,
    history: usize,
    cursor_x: u16,
    cursor_y: u16,
    cursor_visible: bool,
    alternate: bool,
    saved_x: u16,
    saved_y: u16,
    region_upper: u16,
    region_lower: u16,
    keypad_cursor: bool,
    keypad: bool,
    origin: bool,
    wrap: bool,
    insert: bool,
    /// The private modes of the mouse that are on.
    mouse: Vec<u16>,
    /// The session was there before this attach, so its shell has been running.
    existing: bool,
}

impl PaneInfo {
    fn parse(body: &[u8]) -> Option<Self> {
        let text = String::from_utf8_lossy(body);
        // Split on each space: a format with nothing to say leaves an empty field.
        let mut fields = text.lines().next()?.split(' ');
        let mut next = || fields.next().unwrap_or_default();
        let width: u16 = next().parse().ok()?;
        let height: u16 = next().parse().ok()?;
        if width == 0 || height == 0 {
            return None;
        }
        let history = next().parse().unwrap_or(0);
        let cursor_x = next().parse().unwrap_or(0);
        let cursor_y = next().parse().unwrap_or(0);
        let cursor_visible = next() != "0";
        let alternate = next() == "1";
        let saved_x = next().parse().unwrap_or(0);
        let saved_y = next().parse().unwrap_or(0);
        let region_upper = next().parse().unwrap_or(0);
        let region_lower = next().parse().unwrap_or(height - 1);
        let keypad_cursor = next() == "1";
        let keypad = next() == "1";
        let origin = next() == "1";
        let wrap = next() != "0";
        let insert = next() == "1";
        let mouse = [1000, 1002, 1003, 1005, 1006]
            .into_iter()
            .filter(|_| next() == "1")
            .collect();
        let existing = !next().is_empty();
        Some(Self {
            width,
            height,
            history,
            cursor_x,
            cursor_y,
            cursor_visible,
            alternate,
            saved_x,
            saved_y,
            region_upper,
            region_lower,
            keypad_cursor,
            keypad,
            origin,
            wrap,
            insert,
            mouse,
            existing,
        })
    }
}

/// What the commands of `attach_batch` returned.
struct Captures<'a> {
    path: &'a [u8],
    title: &'a [u8],
    history: &'a [u8],
    screen: &'a [u8],
    primary: &'a [u8],
    /// A sequence the program had only started writing: its end comes as output.
    pending: &'a [u8],
}

fn push_lines(out: &mut Vec<u8>, text: &[u8]) {
    for (index, line) in text.split(|byte| *byte == b'\n').enumerate() {
        if index > 0 {
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(line);
    }
}

fn push_cursor(out: &mut Vec<u8>, x: u16, y: u16) {
    let (row, column) = (u32::from(y) + 1, u32::from(x) + 1);
    out.extend_from_slice(format!("\x1b[{row};{column}H").as_bytes());
}

/// Default colors, and back from line drawing, which a capture can leave on.
const RESET_STYLE: &[u8] = b"\x1b[0m\x0f";

/// What to give the terminal so it shows what tmux has. tmux has no way to tell the cursor shape
/// before 3.6, nor what the program did with character sets: those stay as they start.
fn restore_stream(info: &PaneInfo, captures: &Captures) -> Vec<u8> {
    let mut out = Vec::new();
    if info.existing {
        // Everything starts over, and line drawing in the captures comes as shift out.
        out.extend_from_slice(b"\x1bc\x1b)0");
        if info.history > 0 {
            push_lines(&mut out, captures.history);
            out.extend_from_slice(b"\r\n");
            out.extend_from_slice(RESET_STYLE);
        }
        if info.alternate {
            push_lines(&mut out, captures.primary);
            out.extend_from_slice(RESET_STYLE);
            push_cursor(&mut out, info.saved_x, info.saved_y);
            out.extend_from_slice(b"\x1b[?1049h\x1b[H");
        }
        push_lines(&mut out, captures.screen);
        out.extend_from_slice(RESET_STYLE);
        if info.region_upper != 0 || info.region_lower != info.height - 1 {
            let (top, bottom) = (info.region_upper + 1, info.region_lower + 1);
            out.extend_from_slice(format!("\x1b[{top};{bottom}r").as_bytes());
        }
        if info.keypad_cursor {
            out.extend_from_slice(b"\x1b[?1h");
        }
        if info.keypad {
            out.extend_from_slice(b"\x1b=");
        }
        if !info.wrap {
            out.extend_from_slice(b"\x1b[?7l");
        }
        if info.insert {
            out.extend_from_slice(b"\x1b[4h");
        }
        for mode in &info.mouse {
            out.extend_from_slice(format!("\x1b[?{mode}h").as_bytes());
        }
        if info.origin {
            // Now the cursor counts from the top of the region.
            out.extend_from_slice(b"\x1b[?6h");
            push_cursor(
                &mut out,
                info.cursor_x,
                info.cursor_y.saturating_sub(info.region_upper),
            );
        } else {
            push_cursor(&mut out, info.cursor_x, info.cursor_y);
        }
        if !info.cursor_visible {
            out.extend_from_slice(b"\x1b[?25l");
        }
    } else {
        // A new shell: what the terminal shows stays, followed by what the shell has said so far.
        let mut screen = captures.screen;
        while let Some(rest) = screen.strip_suffix(b"\n") {
            screen = rest;
        }
        push_lines(&mut out, screen);
    }
    if !captures.title.is_empty() {
        out.extend_from_slice(b"\x1b]2;");
        out.extend_from_slice(captures.title);
        out.extend_from_slice(b"\x1b\\");
    }
    if captures.path.starts_with(b"file://") {
        out.extend_from_slice(b"\x1b]7;");
        out.extend_from_slice(captures.path);
        out.extend_from_slice(b"\x1b\\");
    }
    out.extend_from_slice(captures.pending);
    out
}

/// Bytes for the program, in hex: tmux takes nothing in them for a command or a key of its own.
fn send_keys(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() * 3 + 16);
    for chunk in bytes.chunks(KEYS_PER_LINE) {
        out.extend_from_slice(b"send-keys -H");
        for byte in chunk {
            out.extend_from_slice(format!(" {byte:02x}").as_bytes());
        }
        out.push(b'\n');
    }
    out
}

/// A paste goes through a tmux buffer because tmux knows whether the program wants it bracketed.
/// The terminal here may not: before 3.7 tmux cannot tell it after a restart.
fn paste_commands(name: &str, text: &str) -> Vec<u8> {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut out = Vec::new();
    let mut chunk = String::new();
    let mut first = true;
    let mut flush = |chunk: &mut String, out: &mut Vec<u8>| {
        let append = if first { "" } else { "-a " };
        first = false;
        out.extend_from_slice(format!("set-buffer {append}-b {name} -- \"{chunk}\"\n").as_bytes());
        chunk.clear();
    };
    for ch in text.chars() {
        match ch {
            '\0' => {}
            '\\' => chunk.push_str("\\\\"),
            '"' => chunk.push_str("\\\""),
            '$' => chunk.push_str("\\$"),
            '\n' => chunk.push_str("\\n"),
            ch if (ch as u32) < 0x20 || ch == '\x7f' => {
                let _ = write!(chunk, "\\{:03o}", ch as u32);
            }
            ch => chunk.push(ch),
        }
        if chunk.len() >= PASTE_PER_LINE {
            flush(&mut chunk, &mut out);
        }
    }
    if !chunk.is_empty() {
        flush(&mut chunk, &mut out);
    }
    if !out.is_empty() {
        out.extend_from_slice(format!("paste-buffer -d -p -b {name}\n").as_bytes());
    }
    out
}

pub(crate) enum Outcome {
    /// The server has no tmux that can do this: a plain shell is the way.
    NoTmux,
    /// It never got to a tmux session, for some other reason: a plain shell for now.
    Unavailable,
    Ended,
    /// It was closed by the user on another PC, so it goes from here without a word.
    Closed,
}

/// What comes from tmux once the terminal is showing.
struct Live<'a> {
    ctx: &'a Context,
    terminal: &'a crate::TerminalHandle,
    log: Option<SessionLog>,
    closing: Vec<u8>,
    closed: bool,
}

impl Live<'_> {
    fn handle(&mut self, message: Message) {
        match message {
            Message::Output(data) => {
                terminal::show(self.ctx, self.terminal, &mut self.log, &data);
            }
            Message::Resized(cols, rows) => self.terminal.fit(cols, rows),
            Message::Tabs(hex) => {
                if let Some(tabs) = decode_tabs(&hex) {
                    self.ctx.emit(Event::SharedTabsChanged {
                        host: self.ctx.host.id.clone(),
                        tabs,
                    });
                }
            }
            Message::Renamed(name) => self.closed |= name == self.closing,
            _ => {}
        }
    }
}

/// Connects a terminal to its tmux session, creating it when it is not there, until the session
/// ends or the channel goes.
pub(crate) async fn run(
    ctx: &Context,
    ssh: &Ssh,
    command: &OpenTerminal,
    persist: &Persistence,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<Outcome> {
    let terminal = &command.terminal;
    let name = session_name(&persist.key);
    let closing = closing_name(&name);
    // The server is in use without this terminal, so the user closed it from somewhere. A server
    // with nothing on it, instead, has restarted: the tabs stay, with new shells in them.
    if persist.known
        && ctx
            .tmux
            .live
            .lock()
            .as_ref()
            .is_some_and(|live| !live.contains(&name))
    {
        return Ok(Outcome::Closed);
    }
    let (cols, rows) = terminal.requested().unwrap_or(terminal.dimensions());
    let opened = tokio::select! { biased;
        _ = cancel.cancelled() => return Ok(Outcome::Ended),
        opened = ssh.channel_open_session() => opened,
    };
    let Ok(channel) = opened else {
        return Ok(Outcome::Unavailable);
    };
    let (mut reader, writer) = channel.split();
    let session = async {
        if ctx.host.forward_agent {
            // A server that refuses it can still give a session, as with a plain shell.
            writer.agent_forward(false).await?;
        }
        let start = start_command(&name, cols, rows, command.history, ctx.host.forward_agent);
        if writer.exec(true, start).await.is_err() {
            return Ok(Outcome::Unavailable);
        }
        loop {
            match reader.wait().await {
                Some(ChannelMsg::Success) => break,
                Some(ChannelMsg::Failure | ChannelMsg::Close) | None => {
                    return Ok(Outcome::Unavailable);
                }
                _ => {}
            }
        }
        let mut parser = Parser::default();
        let mut messages = Vec::new();
        let mut replies: Vec<(bool, Vec<u8>)> = Vec::new();
        let mut expected = None;
        let mut sized = false;
        let mut restored_size = None;
        let mut deadline = Instant::now() + START_TIMEOUT;
        let rest = 'handshake: loop {
            match timeout_at(deadline, reader.wait()).await {
                Ok(Some(ChannelMsg::Data { data })) => parser.feed(&data, &mut messages),
                Ok(Some(ChannelMsg::Close) | None) | Err(_) => return Ok(Outcome::Unavailable),
                Ok(Some(_)) => {}
            }
            let mut drained = std::mem::take(&mut messages).into_iter();
            while let Some(message) = drained.next() {
                match message {
                    Message::Text(line) if line == NO_TMUX.as_bytes() => {
                        return Ok(Outcome::NoTmux);
                    }
                    Message::Exit => return Ok(Outcome::Unavailable),
                    // tmux is up: anything it reads from here on runs after the session exists.
                    Message::Reply { control: false, .. } if expected.is_none() => {
                        let size = terminal.requested();
                        sized = size.is_some();
                        writer.data_bytes(attach_batch(size).into_bytes()).await?;
                        expected = Some(attach_replies(size));
                        deadline = Instant::now() + REPLY_TIMEOUT;
                    }
                    Message::Reply {
                        control: true,
                        ok,
                        body,
                    } => {
                        replies.push((ok, body));
                        if replies.len() == usize::from(sized) + 1 {
                            // PaneInfo is authoritative for events before its snapshot.
                            restored_size = None;
                        }
                        if Some(replies.len()) == expected {
                            break 'handshake drained.collect::<Vec<_>>();
                        }
                    }
                    Message::Resized(cols, rows) => {
                        // A restored pane can resize while its screen is being captured.
                        // Keep that size instead of losing it before the live reader starts.
                        restored_size = Some((cols, rows));
                    }
                    _ => {}
                }
            }
        };
        let [info, path, title, _, _, history, screen, primary, pending] =
            &replies[usize::from(sized)..]
        else {
            return Ok(Outcome::Unavailable);
        };
        let Some(info) = info.0.then(|| PaneInfo::parse(&info.1)).flatten() else {
            return Ok(Outcome::Unavailable);
        };
        ctx.emit(Event::TmuxFound {
            host: ctx.host.id.clone(),
        });
        terminal.set_managed(true);
        // What tmux decided, which another PC may have a say in.
        terminal.fit(info.width, info.height);
        if persist.known && !info.existing {
            terminal.feed(
                b"\x1b[2m--- The previous shell is gone: the server restarted ---\x1b[0m\r\n",
            );
        }
        let pending = decode_output(&pending.1);
        terminal.feed(&restore_stream(
            &info,
            &Captures {
                path: &path.1,
                title: &title.1,
                history: &history.1,
                screen: &screen.1,
                primary: &primary.1,
                pending: &pending,
            },
        ));
        if let Some((cols, rows)) = restored_size {
            terminal.fit(cols, rows);
        }
        terminal.mark_shell_started();
        terminal.reveal();
        if !info.existing
            && let Some(startup) = &command.startup
        {
            writer
                .data_bytes(send_keys(format!("{startup}\r").as_bytes()))
                .await?;
        }
        let mut live = Live {
            ctx,
            terminal,
            log: terminal::open_log(ctx, command.log_dir.clone()),
            closing: closing.clone().into_bytes(),
            closed: false,
        };
        let write_task = async {
            while let Ok(command) = command.input.recv().await {
                let line = match command {
                    TerminalCommand::Write(bytes) => send_keys(&bytes),
                    TerminalCommand::Paste(text) => paste_commands(&name, &text),
                    TerminalCommand::Resize(cols, rows) => {
                        format!("refresh-client -C {cols}x{rows}\n").into_bytes()
                    }
                    TerminalCommand::Publish(hex) => {
                        format!("set-option -g @shcp_layout {hex}\n").into_bytes()
                    }
                    TerminalCommand::Close => {
                        // The user closed this terminal: unlike leaving, that ends what runs in it.
                        let rename = format!("rename-session {closing}\n");
                        writer.data_bytes(rename.into_bytes()).await?;
                        // tmux tells of the new name after the command, and only to clients still on
                        // the session: the other PCs must hear of it before it ends.
                        sleep(RENAME_WAIT).await;
                        writer.data_bytes(&b"kill-session\n"[..]).await?;
                        sleep(KILL_WAIT).await;
                        break;
                    }
                };
                // An empty line would make tmux drop this client.
                if !line.is_empty() {
                    writer.data_bytes(line).await?;
                }
            }
            let _ = writer.eof().await;
            let _ = writer.close().await;
            Ok::<(), russh::Error>(())
        };
        let read_task = async {
            for message in rest {
                live.handle(message);
            }
            while let Some(message) = reader.wait().await {
                match message {
                    ChannelMsg::Data { data } => {
                        parser.feed(&data, &mut messages);
                        for message in messages.drain(..) {
                            live.handle(message);
                        }
                    }
                    ChannelMsg::Close => break,
                    _ => {}
                }
            }
        };
        tokio::select! {
            result = write_task => result?,
            () = read_task => tracing::debug!(session = ctx.id.0, "tmux session channel closed"),
        }
        Ok(if live.closed {
            Outcome::Closed
        } else {
            Outcome::Ended
        })
    };
    let result = tokio::select! { biased;
        _ = cancel.cancelled() => Ok(Outcome::Ended),
        result = session => result,
    };
    // An empty control-mode command detaches this client, leaving its shell alive.
    // Wait for the remote channel to end before letting the SSH connection go away.
    let _ = timeout(Duration::from_secs(2), async {
        let _ = writer.data_bytes(&b"\n"[..]).await;
        let _ = writer.eof().await;
        while let Some(message) = reader.wait().await {
            if matches!(message, ChannelMsg::Close) {
                break;
            }
        }
    })
    .await;
    let _ = timeout(Duration::from_secs(1), writer.close()).await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(chunks: &[&[u8]]) -> Vec<Message> {
        let mut parser = Parser::default();
        let mut out = Vec::new();
        for chunk in chunks {
            parser.feed(chunk, &mut out);
        }
        out
    }

    #[test]
    fn output_is_decoded_even_when_cut_between_reads() {
        assert_eq!(
            parse(&[b"%output %1 a\\033[", b"0m\\134 \xc3\xa8\\015\\012\n"]),
            [Message::Output(b"a\x1b[0m\\ \xc3\xa8\r\n".to_vec())]
        );
    }

    #[test]
    fn a_reply_keeps_its_lines_and_says_who_asked() {
        assert_eq!(
            parse(&[b"%begin 1 2 0\n%end 1 2 0\n%begin 1 3 1\none\n\ntwo\n%end 1 3 1\n"]),
            [
                Message::Reply {
                    control: false,
                    ok: true,
                    body: vec![],
                },
                Message::Reply {
                    control: true,
                    ok: true,
                    body: b"one\n\ntwo".to_vec(),
                },
            ]
        );
    }

    #[test]
    fn text_that_looks_like_the_protocol_stays_text_inside_a_reply() {
        assert_eq!(
            parse(&[b"%begin 1 3 1\n%end 9 9 1\n%output %1 x\n%exit\n%error 1 3 1\n"]),
            [Message::Reply {
                control: true,
                ok: false,
                body: b"%end 9 9 1\n%output %1 x\n%exit".to_vec(),
            }]
        );
    }

    #[test]
    fn lines_outside_the_protocol_are_only_text() {
        assert_eq!(
            parse(&[b"Welcome!\nSHCP_NO_TMUX\n%sessions-changed\n%exit server exited\n"]),
            [
                Message::Text(b"Welcome!".to_vec()),
                Message::Text(NO_TMUX.as_bytes().to_vec()),
                Message::Exit,
            ]
        );
    }

    #[test]
    fn a_terminal_hears_of_its_size_its_tabs_and_names() {
        assert_eq!(
            parse(
                &[b"%layout-change @1 b25d,120x40,0,0,0 b25d,120x40,0,0,0 *\n\
                  %subscription-changed shcp-layout $1 - - - : 6162\n\
                  %subscription-changed other $1 - - - : 6162\n\
                  %session-renamed $1 shcp-closing-a\n"]
            ),
            [
                Message::Resized(120, 40),
                Message::Tabs(b"6162".to_vec()),
                Message::Renamed(b"shcp-closing-a".to_vec()),
            ]
        );
    }

    #[test]
    fn tabs_survive_the_trip_through_tmux() {
        let tabs = SharedTabs {
            writer: "pc".into(),
            tabs: vec![crate::SharedTab {
                name: Some("logs è".into()),
                files: true,
                files_ratio: Some(0.4),
                layout: crate::SavedPane::Split {
                    axis: crate::SplitAxis::Row,
                    ratio: 0.3,
                    first: Box::new(crate::SavedPane::Terminal {
                        key: Some("a".into()),
                    }),
                    second: Box::new(crate::SavedPane::Terminal {
                        key: Some("b".into()),
                    }),
                },
            }],
        };
        let hex = encode_tabs(&tabs).unwrap();
        assert!(hex.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_eq!(decode_tabs(hex.as_bytes()), Some(tabs));
        assert_eq!(decode_tabs(b"zz"), None);
        assert_eq!(decode_tabs(b"abc"), None);
    }

    #[test]
    fn the_survey_keeps_only_what_can_be_ours() {
        let tabs = SharedTabs::default();
        let output = format!(
            "Welcome!\nshcp-a\nshcp-closing-b\nwork\nshcp-c\n{SURVEY_TABS}\nnoise\n{}\n",
            encode_tabs(&tabs).unwrap()
        );
        assert_eq!(
            parse_survey(output.as_bytes()),
            (vec!["shcp-a".to_string(), "shcp-c".to_string()], Some(tabs))
        );
        assert_eq!(parse_survey(b""), (vec![], None));
    }

    #[test]
    fn keys_go_in_hex_and_nothing_gives_no_line() {
        assert_eq!(send_keys(b"l\x03"), b"send-keys -H 6c 03\n");
        assert!(send_keys(b"").is_empty());
        let long = send_keys(&[b'a'; KEYS_PER_LINE + 1]);
        assert_eq!(long.iter().filter(|byte| **byte == b'\n').count(), 2);
    }

    #[test]
    fn a_paste_is_quoted_for_tmux() {
        let commands = String::from_utf8(paste_commands("b", "a\"$\\\r\nb\x1b;")).unwrap();
        assert_eq!(
            commands,
            "set-buffer -b b -- \"a\\\"\\$\\\\\\nb\\033;\"\npaste-buffer -d -p -b b\n"
        );
        assert!(paste_commands("b", "").is_empty());
    }

    #[test]
    fn a_long_paste_is_appended_in_pieces() {
        let commands = String::from_utf8(paste_commands("b", &"x".repeat(PASTE_PER_LINE + 1)));
        let commands = commands.unwrap();
        let lines: Vec<_> = commands.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("set-buffer -b b -- "));
        assert!(lines[1].starts_with("set-buffer -a -b b -- "));
    }

    #[test]
    fn the_start_command_has_no_quote_of_its_own() {
        let command = start_command("shcp-1", 80, 24, 1_000_000, true);
        let inner = command
            .strip_prefix("sh -c '")
            .and_then(|rest| rest.strip_suffix('\''))
            .unwrap();
        assert!(!inner.contains('\'') && !inner.contains('\n'));
        assert!(inner.contains(&format!("history-limit {MAX_HISTORY} ")));
        assert!(inner.contains("-s shcp-1 -x 80 -y 24 -e "));
    }

    #[test]
    fn the_name_of_a_session_cannot_break_a_command() {
        assert_eq!(session_name("a1;'b 2"), "shcp-a1b2");
    }

    #[test]
    fn empty_formats_keep_the_rest_in_place() {
        let info = PaneInfo::parse(b"120 40 7 3 5 1 0   0 39 0 0 0 1 0 1 0 0 0 1 1").unwrap();
        assert_eq!((info.width, info.height, info.history), (120, 40, 7));
        assert_eq!((info.cursor_x, info.cursor_y), (3, 5));
        assert!(info.cursor_visible && info.wrap && !info.alternate);
        assert_eq!(info.region_lower, 39);
        assert_eq!(info.mouse, [1000, 1006]);
        assert!(info.existing);
        let new = PaneInfo::parse(b"80 24 0 0 0 1 0   0 23 0 0 0 1 0 0 0 0 0 0 ").unwrap();
        assert!(!new.existing);
        assert!(PaneInfo::parse(b"no pane").is_none());
    }

    #[test]
    fn a_program_in_the_alternate_screen_comes_back_on_top_of_the_shell() {
        let info = PaneInfo {
            width: 80,
            height: 2,
            history: 1,
            cursor_x: 1,
            cursor_y: 1,
            cursor_visible: true,
            alternate: true,
            saved_y: 1,
            region_lower: 1,
            wrap: true,
            existing: true,
            ..Default::default()
        };
        let stream = restore_stream(
            &info,
            &Captures {
                path: b"file://host/tmp",
                title: b"t",
                history: b"old",
                screen: b"vim\n~",
                primary: b"$ ls\n$ vim",
                pending: b"\x1b[",
            },
        );
        assert_eq!(
            stream,
            b"\x1bc\x1b)0old\r\n\x1b[0m\x0f$ ls\r\n$ vim\x1b[0m\x0f\x1b[2;1H\x1b[?1049h\x1b[Hvim\r\n~\
              \x1b[0m\x0f\x1b[2;2H\x1b]2;t\x1b\\\x1b]7;file://host/tmp\x1b\\\x1b["
                .as_slice()
        );
    }

    #[test]
    fn a_new_shell_leaves_what_the_terminal_shows() {
        let info = PaneInfo {
            width: 80,
            height: 3,
            wrap: true,
            ..Default::default()
        };
        let stream = restore_stream(
            &info,
            &Captures {
                path: b"",
                title: b"",
                history: b"",
                screen: b"$ \n\n",
                primary: b"",
                pending: b"",
            },
        );
        assert_eq!(stream, b"$ ");
    }
}
