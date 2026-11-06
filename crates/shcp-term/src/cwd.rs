//! The folder a shell is in, which only the shell knows. It can say so with OSC 7, which the
//! terminal emulator underneath ignores, and many prompts put it in the window title.

/// A longer sequence is not a path.
const MAX_BODY: usize = 4096;

enum Step {
    Ground,
    Escape,
    /// How many bytes of `7;` matched after `ESC ]`.
    Prefix(u8),
    Body,
    BodyEscape,
}

/// Finds OSC 7 in the output, which may be cut anywhere between two reads.
pub(crate) struct Osc7 {
    step: Step,
    body: Vec<u8>,
}

impl Default for Osc7 {
    fn default() -> Self {
        Self {
            step: Step::Ground,
            body: Vec::new(),
        }
    }
}

impl Osc7 {
    /// The last folder these bytes report.
    pub fn feed(&mut self, bytes: &[u8]) -> Option<String> {
        let mut found = None;
        for &byte in bytes {
            self.step = match self.step {
                Step::Ground | Step::Prefix(_) | Step::Escape if byte == 0x1b => Step::Escape,
                Step::Ground => Step::Ground,
                Step::Escape if byte == b']' => Step::Prefix(0),
                Step::Escape => Step::Ground,
                Step::Prefix(0) if byte == b'7' => Step::Prefix(1),
                Step::Prefix(1) if byte == b';' => {
                    self.body.clear();
                    Step::Body
                }
                Step::Prefix(_) => Step::Ground,
                Step::Body if byte == 0x07 => {
                    found = uri_path(&self.body).or(found);
                    Step::Ground
                }
                Step::Body if byte == 0x1b => Step::BodyEscape,
                Step::Body if self.body.len() >= MAX_BODY => Step::Ground,
                Step::Body => {
                    self.body.push(byte);
                    Step::Body
                }
                Step::BodyEscape if byte == b'\\' => {
                    found = uri_path(&self.body).or(found);
                    Step::Ground
                }
                Step::BodyEscape => Step::Ground,
            };
        }
        found
    }
}

/// The path of `file://host/path`, with its `%20`s undone.
fn uri_path(uri: &[u8]) -> Option<String> {
    let rest = uri.strip_prefix(b"file://")?;
    let start = rest.iter().position(|byte| *byte == b'/')?;
    let path = &rest[start..];
    let mut decoded = Vec::with_capacity(path.len());
    let mut bytes = path.iter().copied();
    while let Some(byte) = bytes.next() {
        if byte != b'%' {
            decoded.push(byte);
            continue;
        }
        let mut digits = bytes.clone();
        let high = digits.next().and_then(|d| (d as char).to_digit(16));
        let low = digits.next().and_then(|d| (d as char).to_digit(16));
        match (high, low) {
            (Some(high), Some(low)) => {
                decoded.push((high * 16 + low) as u8);
                bytes = digits;
            }
            _ => decoded.push(byte),
        }
    }
    String::from_utf8(decoded).ok()
}

/// The folder in a title like `user@host: ~/work`, the usual one from a prompt.
pub(crate) fn from_title(title: &str) -> Option<String> {
    let (who, path) = title.split_once(':')?;
    if !who.contains('@') || who.contains(char::is_whitespace) {
        return None;
    }
    let path = path.trim();
    (path.starts_with('~') || path.starts_with('/')).then(|| path.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc7_ends_with_bel_or_st() {
        let mut scanner = Osc7::default();
        assert_eq!(
            scanner
                .feed(b"text\x1b]7;file://box/var/log\x07more")
                .as_deref(),
            Some("/var/log")
        );
        assert_eq!(
            scanner.feed(b"\x1b]7;file://box/srv\x1b\\").as_deref(),
            Some("/srv")
        );
    }

    #[test]
    fn osc7_can_arrive_in_pieces() {
        let mut scanner = Osc7::default();
        assert_eq!(scanner.feed(b"\x1b"), None);
        assert_eq!(scanner.feed(b"]7"), None);
        assert_eq!(scanner.feed(b";file://box/ho"), None);
        assert_eq!(scanner.feed(b"me\x1b"), None);
        assert_eq!(scanner.feed(b"\\").as_deref(), Some("/home"));
    }

    #[test]
    fn osc7_paths_are_decoded() {
        let mut scanner = Osc7::default();
        assert_eq!(
            scanner
                .feed(b"\x1b]7;file:///a%20b/%C3%A8/100%\x07")
                .as_deref(),
            Some("/a b/\u{e8}/100%")
        );
    }

    #[test]
    fn other_sequences_are_left_alone() {
        let mut scanner = Osc7::default();
        assert_eq!(
            scanner.feed(b"\x1b]0;title\x07\x1b[31m7;file:///x\x07"),
            None
        );
        assert_eq!(scanner.feed(b"\x1b]7;http://box/x\x07"), None);
        assert_eq!(scanner.feed(b"\x1b]7;"), None);
        assert_eq!(scanner.feed(&vec![b'a'; MAX_BODY + 10]), None);
        assert_eq!(scanner.feed(b"\x07"), None);
    }

    #[test]
    fn a_title_with_a_folder_is_recognized() {
        assert_eq!(from_title("me@box: ~/work").as_deref(), Some("~/work"));
        assert_eq!(from_title("root@box:/var/log").as_deref(), Some("/var/log"));
        assert_eq!(from_title("me@box: ~").as_deref(), Some("~"));
    }

    #[test]
    fn other_titles_say_nothing() {
        for title in [
            "vim",
            "vim notes.txt: /tmp",
            "me@box",
            "top - 10:00:00",
            "me@box: vim",
        ] {
            assert_eq!(from_title(title), None, "{title}");
        }
    }
}
