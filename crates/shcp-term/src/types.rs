#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TerminalId(pub u64);
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridPoint {
    pub row: u16,
    pub col: u16,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorShape {
    Block,
    Beam,
    Underline,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectKind {
    Simple,
    Word,
    Line,
}
bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct CellFlags: u16 {
        const BOLD = 1; const ITALIC = 2; const UNDERLINE = 4; const STRIKE = 8;
        const DIM = 16; const INVERSE = 32; const WIDE = 64; const WIDE_SPACER = 128;
    }
}
#[derive(Debug, Clone)]
pub struct Cell {
    pub ch: char,
    pub fg: Rgb,
    pub bg: Rgb,
    pub flags: CellFlags,
}
#[derive(Debug, Clone, Copy)]
pub struct CursorInfo {
    pub point: GridPoint,
    pub shape: CursorShape,
}
#[derive(Debug, Clone)]
pub struct TermSnapshot {
    pub cols: u16,
    pub rows: u16,
    pub cells: Vec<Cell>,
    pub cursor: Option<CursorInfo>,
    pub display_offset: usize,
    pub selection: Option<(GridPoint, GridPoint)>,
    pub search_hits: Vec<SearchHit>,
    /// The cells of each web link on screen.
    pub links: Vec<(GridPoint, GridPoint)>,
}
/// A match of the running search, clamped to the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchHit {
    pub start: GridPoint,
    pub end: GridPoint,
    /// The match the search stopped on.
    pub current: bool,
}
/// A web link on screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalLink {
    pub url: String,
    pub start: GridPoint,
    pub end: GridPoint,
}
#[derive(Debug, Clone, Copy)]
pub struct KeyInput {
    pub key: Key,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}
#[derive(Debug, Clone, Copy)]
pub enum Key {
    Char(char),
    Enter,
    Tab,
    Backspace,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    F(u8),
}

/// Colors resolved by the emulator before a snapshot reaches the UI.
#[derive(Debug, Clone)]
pub struct Palette {
    pub ansi: [Rgb; 16],
    pub fg: Rgb,
    pub bg: Rgb,
    pub cursor: Rgb,
}
impl Default for Palette {
    fn default() -> Self {
        Self {
            ansi: [
                Rgb(40, 44, 51),
                Rgb(208, 114, 119),
                Rgb(161, 193, 129),
                Rgb(222, 193, 132),
                Rgb(116, 173, 232),
                Rgb(198, 148, 220),
                Rgb(112, 190, 198),
                Rgb(169, 175, 188),
                Rgb(92, 99, 112),
                Rgb(224, 129, 135),
                Rgb(177, 209, 145),
                Rgb(238, 209, 148),
                Rgb(132, 189, 248),
                Rgb(214, 164, 236),
                Rgb(128, 206, 214),
                Rgb(220, 224, 229),
            ],
            fg: Rgb(220, 224, 229),
            bg: Rgb(40, 44, 51),
            cursor: Rgb(220, 224, 229),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseAction {
    Press,
    Release,
    /// Pointer moved; `button` is the held button, if any.
    Move,
}
/// A mouse event in visible grid coordinates, reported to applications that enabled
/// mouse tracking (tmux, herdr, htop, vim with `mouse=a`, …).
#[derive(Debug, Clone, Copy)]
pub struct MouseInput {
    pub action: MouseAction,
    pub button: Option<MouseButton>,
    pub point: GridPoint,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}
