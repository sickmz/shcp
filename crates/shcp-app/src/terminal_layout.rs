//! Turns a `TermSnapshot` into background rectangles and text segments.
//! Pure data: no gpui types, so the element only has to shape and paint.
use shcp_core::term::{CellFlags, GridPoint, Rgb, TermSnapshot};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct CellStyle {
    pub fg: Rgb,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub dim: bool,
}

pub(crate) struct BgRect {
    pub row: u16,
    pub col: u16,
    pub cols: u16,
    pub color: Rgb,
}

/// A run of cells on one row that is shaped as one gpui line.
/// Wide (double-width) characters get a segment of their own.
pub(crate) struct Segment {
    pub row: u16,
    pub col: u16,
    pub text: String,
    /// (byte length, style) per styled run.
    pub runs: Vec<(usize, CellStyle)>,
    pub wide: bool,
}

pub(crate) struct Layout {
    pub backgrounds: Vec<BgRect>,
    pub segments: Vec<Segment>,
}

fn printable(ch: char) -> char {
    if ch.is_control() { ' ' } else { ch }
}

/// `block_cursor`: cell that is covered by a filled cursor; its text is drawn with `cursor_fg`.
pub(crate) fn build(
    snapshot: &TermSnapshot,
    default_bg: Rgb,
    block_cursor: Option<GridPoint>,
    cursor_fg: Rgb,
) -> Layout {
    let cols = snapshot.cols as usize;
    let mut backgrounds = Vec::new();
    let mut segments = Vec::new();
    for row in 0..snapshot.rows {
        let cells = &snapshot.cells[row as usize * cols..(row as usize + 1) * cols];
        push_backgrounds(&mut backgrounds, row, cells, default_bg);
        push_segments(&mut segments, row, cells, block_cursor, cursor_fg);
    }
    Layout {
        backgrounds,
        segments,
    }
}

fn push_backgrounds(
    out: &mut Vec<BgRect>,
    row: u16,
    cells: &[shcp_core::term::Cell],
    default_bg: Rgb,
) {
    for (col, cell) in cells.iter().enumerate() {
        if cell.bg == default_bg {
            continue;
        }
        match out.last_mut() {
            Some(last)
                if last.row == row
                    && last.color == cell.bg
                    && last.col + last.cols == col as u16 =>
            {
                last.cols += 1
            }
            _ => out.push(BgRect {
                row,
                col: col as u16,
                cols: 1,
                color: cell.bg,
            }),
        }
    }
}

fn push_segments(
    out: &mut Vec<Segment>,
    row: u16,
    cells: &[shcp_core::term::Cell],
    block_cursor: Option<GridPoint>,
    cursor_fg: Rgb,
) {
    let mut current: Option<Segment> = None;
    let flush = |segment: Option<Segment>, out: &mut Vec<Segment>| {
        if let Some(mut segment) = segment {
            trim_trailing_spaces(&mut segment);
            if !segment.text.is_empty() {
                out.push(segment);
            }
        }
    };
    for (col, cell) in cells.iter().enumerate() {
        if cell.flags.contains(CellFlags::WIDE_SPACER) {
            continue;
        }
        let under_cursor = block_cursor
            == Some(GridPoint {
                row,
                col: col as u16,
            });
        let style = CellStyle {
            fg: if under_cursor { cursor_fg } else { cell.fg },
            bold: cell.flags.contains(CellFlags::BOLD),
            italic: cell.flags.contains(CellFlags::ITALIC),
            underline: cell.flags.contains(CellFlags::UNDERLINE),
            strike: cell.flags.contains(CellFlags::STRIKE),
            dim: cell.flags.contains(CellFlags::DIM),
        };
        let ch = printable(cell.ch);
        if cell.flags.contains(CellFlags::WIDE) {
            flush(current.take(), out);
            out.push(Segment {
                row,
                col: col as u16,
                text: ch.to_string(),
                runs: vec![(ch.len_utf8(), style)],
                wide: true,
            });
            continue;
        }
        let segment = current.get_or_insert_with(|| Segment {
            row,
            col: col as u16,
            text: String::new(),
            runs: Vec::new(),
            wide: false,
        });
        segment.text.push(ch);
        match segment.runs.last_mut() {
            Some((len, last)) if *last == style => *len += ch.len_utf8(),
            _ => segment.runs.push((ch.len_utf8(), style)),
        }
    }
    flush(current.take(), out);
}

fn trim_trailing_spaces(segment: &mut Segment) {
    let trimmed = segment.text.trim_end_matches(' ').len();
    let mut excess = segment.text.len() - trimmed;
    segment.text.truncate(trimmed);
    while excess > 0 {
        let Some((len, _)) = segment.runs.last_mut() else {
            break;
        };
        if *len <= excess {
            excess -= *len;
            segment.runs.pop();
        } else {
            *len -= excess;
            excess = 0;
        }
    }
}
