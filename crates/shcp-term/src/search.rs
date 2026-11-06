//! Find in the terminal: the whole scrollback is searched, matches on screen are reported in
//! the snapshot.
use super::*;
use alacritty_terminal::{
    index::{Boundary, Direction},
    term::search::{Match, RegexIter, RegexSearch},
};

pub(crate) struct Search {
    regex: RegexSearch,
    current: Option<Match>,
}

/// The query is plain text, not a regex.
fn literal(query: &str) -> String {
    let mut out = String::with_capacity(query.len());
    for c in query.chars() {
        if r"\.+*?()|[]{}^$#&-~".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Output or a resize may have moved the grid since this point was found.
fn clamp(term: &Term<Listener>, point: Point) -> Point {
    Point::new(
        point
            .line
            .max(term.topmost_line())
            .min(term.bottommost_line()),
        point.column.min(term.last_column()),
    )
}

fn jump(state: &mut State, origin: Point, direction: Direction) -> bool {
    let State { term, search, .. } = state;
    let Some(search) = search else {
        return false;
    };
    let origin = clamp(term, origin);
    search.current = term.search_next(&mut search.regex, origin, direction, Side::Left, None);
    match &search.current {
        Some(found) => {
            term.scroll_to_point(*found.start());
            true
        }
        None => false,
    }
}

impl TerminalHandle {
    /// Starts a search, or refines the running one, staying near the current match. Returns
    /// whether the text was found; an empty query ends the search.
    pub fn search(&self, query: &str) -> bool {
        if query.is_empty() {
            self.search_clear();
            return true;
        }
        let mut state = self.0.state.lock();
        let Ok(regex) = RegexSearch::new(&literal(query)) else {
            state.search = None;
            drop(state);
            self.changed();
            return false;
        };
        let origin = state
            .search
            .as_ref()
            .and_then(|search| search.current.as_ref())
            .map(|found| *found.end())
            .unwrap_or_else(|| Point::new(state.term.bottommost_line(), state.term.last_column()));
        state.search = Some(Search {
            regex,
            current: None,
        });
        let found = jump(&mut state, origin, Direction::Left);
        drop(state);
        self.changed();
        found
    }

    /// Moves to the next match, wrapping around; `older` goes up the scrollback.
    pub fn search_step(&self, older: bool) -> bool {
        let mut state = self.0.state.lock();
        let Some(search) = &state.search else {
            return false;
        };
        let current = search.current.clone();
        let term = &state.term;
        let bottom = Point::new(term.bottommost_line(), term.last_column());
        let (origin, direction) = match (current, older) {
            (Some(found), true) => (
                clamp(term, *found.start()).sub(term, Boundary::None, 1),
                Direction::Left,
            ),
            (Some(found), false) => (
                clamp(term, *found.end()).add(term, Boundary::None, 1),
                Direction::Right,
            ),
            (None, _) => (bottom, Direction::Left),
        };
        let found = jump(&mut state, origin, direction);
        drop(state);
        self.changed();
        found
    }

    pub fn search_clear(&self) {
        self.0.state.lock().search = None;
        self.changed();
    }
}

/// The matches on screen.
pub(crate) fn visible_hits(state: &mut State) -> Vec<SearchHit> {
    let State { term, search, .. } = state;
    let Some(search) = search else {
        return Vec::new();
    };
    let offset = term.grid().display_offset() as i32;
    let rows = term.screen_lines() as i32;
    let cols = term.columns() as u16;
    let start = Point::new(Line(-offset), Column(0));
    let end = Point::new(Line(rows - 1 - offset), term.last_column());
    let current = search.current.clone();
    let hits: Vec<_> = RegexIter::new(start, end, Direction::Right, term, &mut search.regex)
        .map(|found| SearchHit {
            start: visible_point(*found.start(), offset, rows, cols),
            end: visible_point(*found.end(), offset, rows, cols),
            current: current.as_ref() == Some(&found),
        })
        .collect();
    hits
}

/// Clamped to the screen, for what starts or ends off it.
pub(crate) fn visible_point(point: Point, offset: i32, rows: i32, cols: u16) -> GridPoint {
    let row = point.line.0 + offset;
    if row < 0 {
        GridPoint { row: 0, col: 0 }
    } else if row >= rows {
        GridPoint {
            row: (rows - 1) as u16,
            col: cols - 1,
        }
    } else {
        GridPoint {
            row: row as u16,
            col: (point.column.0 as u16).min(cols - 1),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terminal() -> TerminalHandle {
        let notify = Arc::new(|_| {});
        let (terminal, _input) =
            TerminalHandle::new(TerminalId(1), 40, 5, 100, Palette::default(), notify);
        terminal
    }

    fn lines(terminal: &TerminalHandle, count: usize) {
        for index in 0..count {
            terminal.feed(format!("line {index}\r\n").as_bytes());
        }
    }

    #[test]
    fn finds_text_in_the_scrollback_and_scrolls_to_it() {
        let terminal = terminal();
        lines(&terminal, 50);
        assert!(terminal.search("line 3\u{20}"));
        let snapshot = terminal.snapshot();
        assert!(snapshot.display_offset > 0);
        let hit = snapshot.search_hits.iter().find(|h| h.current).unwrap();
        assert_eq!(hit.end.col - hit.start.col, 6);
    }

    #[test]
    fn missing_text_is_reported_and_clearing_removes_the_hits() {
        let terminal = terminal();
        lines(&terminal, 3);
        assert!(!terminal.search("absent"));
        assert!(terminal.snapshot().search_hits.is_empty());
        assert!(terminal.search("line"));
        assert_eq!(terminal.snapshot().search_hits.len(), 3);
        terminal.search_clear();
        assert!(terminal.snapshot().search_hits.is_empty());
    }

    #[test]
    fn stepping_walks_every_match_and_wraps() {
        let terminal = terminal();
        lines(&terminal, 3);
        assert!(terminal.search("line"));
        let row = |terminal: &TerminalHandle| {
            let snapshot = terminal.snapshot();
            snapshot
                .search_hits
                .iter()
                .find(|h| h.current)
                .map(|h| h.start.row)
                .unwrap()
        };
        assert_eq!(row(&terminal), 2);
        assert!(terminal.search_step(true));
        assert_eq!(row(&terminal), 1);
        assert!(terminal.search_step(true));
        assert_eq!(row(&terminal), 0);
        assert!(terminal.search_step(true));
        assert_eq!(row(&terminal), 2);
        assert!(terminal.search_step(false));
        assert_eq!(row(&terminal), 0);
    }

    #[test]
    fn regex_characters_are_plain_text() {
        let terminal = terminal();
        terminal.feed(b"a.b axb (c)\r\n");
        assert!(terminal.search("a.b"));
        assert_eq!(terminal.snapshot().search_hits.len(), 1);
        assert!(terminal.search("(c)"));
        assert_eq!(terminal.snapshot().search_hits.len(), 1);
    }

    #[test]
    fn a_match_older_than_the_history_does_not_panic() {
        let terminal = terminal();
        lines(&terminal, 20);
        assert!(terminal.search("line 0"));
        lines(&terminal, 500);
        terminal.search_step(true);
        terminal.search_step(false);
        terminal.resize(10, 3);
        terminal.search_step(true);
        terminal.snapshot();
    }
}
