//! Links under the pointer: OSC 8 hyperlinks and URLs written as plain text.
//!
//! Only web addresses open: a server decides what an OSC 8 link points to, and it must not be
//! able to start a local program through `file:` or another scheme handler.
use super::*;
use alacritty_terminal::{
    index::Boundary,
    term::cell::{Flags, Hyperlink},
};
use std::ops::Range;

fn is_web(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    (lower.starts_with("http://") || lower.starts_with("https://"))
        && !url.chars().any(char::is_control)
}

/// Every URL in a line of text. Runs on what is on screen at each redraw, so it stays cheap.
fn find_urls(text: &[char]) -> Vec<Range<usize>> {
    let is_url_char = |c: char| {
        !c.is_whitespace() && !c.is_control() && !matches!(c, '<' | '>' | '"' | '`' | '{' | '}')
    };
    let mut urls = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let rest = &text[start..];
        let scheme = ["https://", "http://"].into_iter().find(|scheme| {
            rest.len() > scheme.len()
                && rest
                    .iter()
                    .zip(scheme.chars())
                    .all(|(a, b)| a.to_ascii_lowercase() == b)
        });
        let after_word = start > 0 && text[start - 1].is_alphanumeric();
        let Some(scheme) = scheme.filter(|_| !after_word) else {
            start += 1;
            continue;
        };
        let mut end = start + scheme.len();
        while end < text.len() && is_url_char(text[end]) {
            end += 1;
        }
        end = trim_end(&text[start..end]) + start;
        if end > start + scheme.len() {
            urls.push(start..end);
        }
        start = end.max(start + 1);
    }
    urls
}

/// The URL covering `at` in a line of text.
fn find_url(text: &[char], at: usize) -> Option<Range<usize>> {
    find_urls(text)
        .into_iter()
        .find(|range| range.contains(&at))
}

/// The cells of a line that may wrap over several rows, without the spacers of wide characters.
fn logical_line(term: &Term<Listener>, first: Point, last: Point) -> Vec<(Point, char)> {
    let mut cells = Vec::new();
    let mut cursor = first;
    loop {
        let cell = &term.grid()[cursor];
        if !cell
            .flags
            .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
        {
            cells.push((cursor, cell.c));
        }
        if cursor == last {
            return cells;
        }
        cursor = cursor.add(term, Boundary::None, 1);
    }
}

/// The cells around `index` that carry the same hyperlink as it: same target, same id.
fn hyperlink_run(
    term: &Term<Listener>,
    cells: &[(Point, char)],
    index: usize,
    link: &Hyperlink,
) -> Range<usize> {
    let same = |p: Point| {
        term.grid()[p]
            .hyperlink()
            .is_some_and(|other| other.uri() == link.uri() && other.id() == link.id())
    };
    let mut start = index;
    while start > 0 && same(cells[start - 1].0) {
        start -= 1;
    }
    let mut end = index + 1;
    while end < cells.len() && same(cells[end].0) {
        end += 1;
    }
    start..end
}

/// Where the web links on screen are, to mark them.
pub(crate) fn visible_links(term: &Term<Listener>) -> Vec<(GridPoint, GridPoint)> {
    let screen = Screen::of(term);
    let (top, bottom) = screen.lines();
    let mut links = Vec::new();
    let mut line = top;
    while line <= bottom {
        let at = Point::new(line, Column(0));
        let last = term.line_search_right(at);
        let cells = logical_line(term, term.line_search_left(at), last);
        let text: Vec<char> = cells.iter().map(|(_, c)| *c).collect();
        let mut ranges = find_urls(&text);
        let mut index = 0;
        while index < cells.len() {
            let Some(link) = term.grid()[cells[index].0].hyperlink() else {
                index += 1;
                continue;
            };
            // The cell before is not part of it: a run ends where another link starts.
            let run = hyperlink_run(term, &cells, index, &link);
            index = run.end;
            if is_web(link.uri()) {
                ranges.push(run);
            }
        }
        for range in ranges {
            let (start, end) = (cells[range.start].0, cells[range.end - 1].0);
            // The rest of a wrapped line can be off screen.
            if end.line >= top && start.line <= bottom {
                links.push((screen.point(start), screen.point(end)));
            }
        }
        line = last.line + 1;
    }
    links
}

/// Drops the punctuation that closes a sentence or wraps the URL.
fn trim_end(url: &[char]) -> usize {
    let mut end = url.len();
    while end > 0 {
        let count = |c: char| url[..end].iter().filter(|x| **x == c).count();
        let wrapping = match url[end - 1] {
            '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '*' | '_' | '~' => true,
            ')' => count(')') > count('('),
            ']' => count(']') > count('['),
            _ => false,
        };
        if !wrapping {
            break;
        }
        end -= 1;
    }
    end
}

impl TerminalHandle {
    /// The web link at a visible cell, if any.
    pub fn link_at(&self, point: GridPoint) -> Option<TerminalLink> {
        let state = self.0.state.lock();
        let at = grid_point(&state, point);
        let term = &state.term;
        let cells = logical_line(term, term.line_search_left(at), term.line_search_right(at));
        let index = cells.iter().rposition(|(p, _)| *p <= at)?;
        let hyperlink = term.grid()[cells[index].0].hyperlink();
        let (range, url) = if let Some(link) = hyperlink {
            (
                hyperlink_run(term, &cells, index, &link),
                link.uri().to_string(),
            )
        } else {
            let text: Vec<char> = cells.iter().map(|(_, c)| *c).collect();
            let range = find_url(&text, index)?;
            let url = text[range.clone()].iter().collect();
            (range, url)
        };
        if !is_web(&url) {
            return None;
        }
        let screen = Screen::of(term);
        Some(TerminalLink {
            url,
            start: screen.point(cells[range.start].0),
            end: screen.point(cells[range.end - 1].0),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url_at(text: &str, at: usize) -> Option<String> {
        let chars: Vec<char> = text.chars().collect();
        find_url(&chars, at).map(|range| chars[range].iter().collect())
    }

    fn terminal(cols: u16) -> TerminalHandle {
        test_terminal(cols, 5)
    }

    #[test]
    fn finds_the_url_under_the_position() {
        let text = "see https://example.com/a?b=1 now";
        assert_eq!(url_at(text, 2), None);
        assert_eq!(
            url_at(text, 4).as_deref(),
            Some("https://example.com/a?b=1")
        );
        assert_eq!(
            url_at(text, 28).as_deref(),
            Some("https://example.com/a?b=1")
        );
        assert_eq!(url_at(text, 30), None);
    }

    #[test]
    fn trailing_punctuation_stays_out_and_balanced_brackets_stay_in() {
        assert_eq!(url_at("http://a.io.", 0).as_deref(), Some("http://a.io"));
        assert_eq!(url_at("(http://a.io)", 1).as_deref(), Some("http://a.io"));
        assert_eq!(
            url_at("http://a.io/x_(y)", 0).as_deref(),
            Some("http://a.io/x_(y)")
        );
    }

    #[test]
    fn only_a_whole_scheme_counts() {
        assert_eq!(url_at("xhttp://a.io", 3), None);
        assert_eq!(url_at("http://", 0), None);
        assert_eq!(url_at("file:///c:/x", 3), None);
    }

    #[test]
    fn a_url_wrapped_over_two_rows_is_one_link() {
        let terminal = terminal(20);
        terminal.feed(b"https://example.com/long/path\r\n");
        let top = terminal.link_at(GridPoint { row: 0, col: 3 }).unwrap();
        let bottom = terminal.link_at(GridPoint { row: 1, col: 3 }).unwrap();
        assert_eq!(top.url, "https://example.com/long/path");
        assert_eq!(bottom.url, top.url);
        assert_eq!(top.start, GridPoint { row: 0, col: 0 });
        assert_eq!(top.end, GridPoint { row: 1, col: 8 });
    }

    #[test]
    fn hyperlinks_use_their_target_and_their_own_cells() {
        let terminal = terminal(40);
        terminal.feed(b"go \x1b]8;;https://example.com\x1b\\here\x1b]8;;\x1b\\ now\r\n");
        let link = terminal.link_at(GridPoint { row: 0, col: 4 }).unwrap();
        assert_eq!(link.url, "https://example.com");
        assert_eq!(link.start.col, 3);
        assert_eq!(link.end.col, 6);
        assert!(terminal.link_at(GridPoint { row: 0, col: 9 }).is_none());
    }

    #[test]
    fn hyperlinks_to_other_schemes_do_not_open() {
        let terminal = terminal(40);
        terminal.feed(b"\x1b]8;;file:///c:/windows/calc.exe\x1b\\click\x1b]8;;\x1b\\\r\n");
        assert!(terminal.link_at(GridPoint { row: 0, col: 1 }).is_none());
    }
}
