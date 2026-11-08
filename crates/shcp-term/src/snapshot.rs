use super::*;
use alacritty_terminal::{term::cell::Flags, vte::ansi::CursorShape as AlCursor};
impl TerminalHandle {
    pub fn snapshot(&self) -> TermSnapshot {
        let mut state = self.0.state.lock();
        state.dirty = false;
        if let Some(frozen) = &state.frozen {
            return frozen.clone();
        }
        let search_hits = search::visible_hits(&mut state);
        let links = link::visible_links(&state.term);
        let content = state.term.renderable_content();
        let cols = state.term.columns() as u16;
        let rows = state.term.screen_lines() as u16;
        let cells = content
            .display_iter
            .map(|cell| {
                let mut fg = colors::resolve(&state.palette, content.colors, cell.fg);
                let mut bg = colors::resolve(&state.palette, content.colors, cell.bg);
                let flags = cell.flags;
                if flags.contains(Flags::INVERSE) {
                    std::mem::swap(&mut fg, &mut bg);
                }
                let mut style = CellFlags::empty();
                for (source, target) in [
                    (Flags::BOLD, CellFlags::BOLD),
                    (Flags::ITALIC, CellFlags::ITALIC),
                    (Flags::ALL_UNDERLINES, CellFlags::UNDERLINE),
                    (Flags::STRIKEOUT, CellFlags::STRIKE),
                    (Flags::DIM, CellFlags::DIM),
                    (Flags::INVERSE, CellFlags::INVERSE),
                    (Flags::WIDE_CHAR, CellFlags::WIDE),
                    (
                        Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER,
                        CellFlags::WIDE_SPACER,
                    ),
                ] {
                    if flags.intersects(source) {
                        style.insert(target);
                    }
                }
                Cell {
                    ch: if flags.contains(Flags::HIDDEN) {
                        ' '
                    } else {
                        cell.c
                    },
                    fg,
                    bg,
                    flags: style,
                }
            })
            .collect();
        let visible = |point: Point| {
            let row = point.line.0 + content.display_offset as i32;
            (row >= 0 && row < rows as i32).then_some(GridPoint {
                row: row.max(0) as u16,
                col: point.column.0 as u16,
            })
        };
        let shape = match content.cursor.shape {
            AlCursor::Block | AlCursor::HollowBlock => Some(CursorShape::Block),
            AlCursor::Beam => Some(CursorShape::Beam),
            AlCursor::Underline => Some(CursorShape::Underline),
            AlCursor::Hidden => None,
        };
        let cursor = shape.and_then(|shape| {
            visible(content.cursor.point).map(|point| CursorInfo { point, shape })
        });
        let selection = content
            .selection
            .filter(|s| {
                s.end.line.0 + content.display_offset as i32 >= 0
                    && s.start.line.0 + (content.display_offset as i32) < rows as i32
            })
            .map(|s| {
                let clamp = |p: Point| {
                    let row = p.line.0 + content.display_offset as i32;
                    if row < 0 {
                        GridPoint { row: 0, col: 0 }
                    } else if row >= rows as i32 {
                        GridPoint {
                            row: rows - 1,
                            col: cols - 1,
                        }
                    } else {
                        GridPoint {
                            row: row as u16,
                            col: (p.column.0 as u16).min(cols - 1),
                        }
                    }
                };
                (clamp(s.start), clamp(s.end))
            });
        TermSnapshot {
            cols,
            rows,
            cells,
            cursor,
            display_offset: content.display_offset,
            selection,
            search_hits,
            links,
        }
    }
}
