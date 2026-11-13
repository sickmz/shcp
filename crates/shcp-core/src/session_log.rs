//! What a terminal shows, written to a text file as it arrives. Colors and cursor movements are
//! left out, so it reads in any editor; a screen program like vim leaves little sense there.
use std::{
    fs::File,
    io::{self, Write as _},
    path::{Path, PathBuf},
};

pub(crate) fn folder(data_dir: &Path) -> PathBuf {
    data_dir.join("sessions")
}

/// Takes the terminal control sequences out of the output, which may be cut anywhere.
#[derive(Default)]
struct Cleaner {
    step: Step,
    /// A carriage return, whose meaning depends on what comes next.
    after_return: bool,
}

#[derive(Default, PartialEq, Eq)]
enum Step {
    #[default]
    Text,
    Escape,
    /// Like `ESC ( B`: one more byte ends it.
    EscapeArgument,
    Csi,
    /// OSC, DCS and the like: up to BEL or `ESC \`.
    Text8,
    Text8Escape,
}

impl Cleaner {
    fn clean(&mut self, bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(bytes.len());
        for &byte in bytes {
            match self.step {
                Step::Text => match byte {
                    0x1b => self.step = Step::Escape,
                    b'\r' => self.after_return = true,
                    0x08 => {
                        out.pop();
                    }
                    b'\n' | b'\t' => {
                        self.after_return = false;
                        out.push(byte);
                    }
                    0..=0x1f | 0x7f => {}
                    _ => {
                        // Text written over a line, as a progress bar does, gets its own line.
                        if std::mem::take(&mut self.after_return) {
                            out.push(b'\n');
                        }
                        out.push(byte);
                    }
                },
                Step::Escape => {
                    self.step = match byte {
                        b'[' => Step::Csi,
                        b']' | b'P' | b'X' | b'^' | b'_' => Step::Text8,
                        0x20..=0x2f => Step::EscapeArgument,
                        0x1b => Step::Escape,
                        _ => Step::Text,
                    }
                }
                Step::EscapeArgument => self.step = Step::Text,
                Step::Csi => {
                    if (0x40..=0x7e).contains(&byte) {
                        self.step = Step::Text;
                    }
                }
                Step::Text8 => match byte {
                    0x07 => self.step = Step::Text,
                    0x1b => self.step = Step::Text8Escape,
                    _ => {}
                },
                Step::Text8Escape => {
                    self.step = if byte == b'\\' {
                        Step::Text
                    } else {
                        Step::Text8
                    }
                }
            }
        }
        out
    }
}

pub(crate) struct SessionLog {
    file: File,
    cleaner: Cleaner,
}

impl SessionLog {
    /// A new file named after the host and the moment, which is in UTC.
    pub(crate) fn create(dir: &Path, alias: &str, seconds: u64) -> io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let name: String = alias
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let stamp = stamp(seconds);
        // Terminals opened in the same second each get their own file; up to 1000 get a number.
        let mut attempt = 0;
        loop {
            let suffix = match attempt {
                0 => String::new(),
                n => format!("-{n}"),
            };
            match File::create_new(dir.join(format!("{name}-{stamp}{suffix}.log"))) {
                Ok(file) => {
                    return Ok(Self {
                        file,
                        cleaner: Cleaner::default(),
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists && attempt < 1000 => {
                    attempt += 1;
                }
                Err(error) => return Err(error),
            }
        }
    }

    pub(crate) fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        let text = self.cleaner.clean(bytes);
        if text.is_empty() {
            return Ok(());
        }
        self.file.write_all(&text)
    }
}

/// `20261105-143000Z`.
fn stamp(seconds: u64) -> String {
    let (days, rest) = ((seconds / 86_400) as i64, seconds % 86_400);
    // Civil-from-days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}{month:02}{day:02}-{:02}{:02}{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clean(bytes: &[u8]) -> String {
        String::from_utf8(Cleaner::default().clean(bytes)).unwrap()
    }

    #[test]
    fn plain_text_and_line_ends_stay() {
        assert_eq!(clean(b"one\r\ntwo\n\tthree"), "one\ntwo\n\tthree");
        assert_eq!(clean("caff\u{e8}".as_bytes()), "caff\u{e8}");
    }

    #[test]
    fn colors_titles_and_modes_go() {
        assert_eq!(clean(b"\x1b[1;31mred\x1b[0m"), "red");
        assert_eq!(clean(b"\x1b]0;title\x07text"), "text");
        assert_eq!(clean(b"\x1b]7;file:///x\x1b\\text"), "text");
        assert_eq!(clean(b"\x1b[?2004h\x1b(Btext\x1b>"), "text");
        assert_eq!(clean(b"\x07bell"), "bell");
    }

    #[test]
    fn a_sequence_can_be_cut_between_reads() {
        let mut cleaner = Cleaner::default();
        let mut out = cleaner.clean(b"a\x1b[3");
        out.extend(cleaner.clean(b"1mb\x1b"));
        out.extend(cleaner.clean(b"]0;t\x1b"));
        out.extend(cleaner.clean(b"\\c\r"));
        out.extend(cleaner.clean(b"\nd"));
        assert_eq!(out, b"abc\nd");
    }

    #[test]
    fn a_rewritten_line_gets_a_line_of_its_own() {
        assert_eq!(clean(b"10%\r20%\r30%\r\n"), "10%\n20%\n30%\n");
    }

    #[test]
    fn backspace_removes_what_it_can() {
        assert_eq!(clean(b"abc\x08\x08d"), "ad");
        assert_eq!(clean(b"\x08a"), "a");
    }

    #[test]
    fn the_stamp_is_a_readable_date() {
        assert_eq!(stamp(0), "19700101-000000Z");
        assert_eq!(stamp(1_793_000_000), "20261026-073320Z");
    }

    #[test]
    fn files_are_named_after_the_host_and_never_replaced() {
        let scratch = crate::test_support::Scratch::new("session-log");
        // A folder that does not exist yet: creating the log makes it.
        let dir = scratch.0.join("logs");
        let mut first = SessionLog::create(&dir, "web/01 prod", 0).unwrap();
        let mut second = SessionLog::create(&dir, "web/01 prod", 0).unwrap();
        first.write(b"\x1b[31mone\x1b[0m\r\n").unwrap();
        second.write(b"two").unwrap();
        drop((first, second));
        let read = |name: &str| std::fs::read_to_string(dir.join(name)).unwrap();
        assert_eq!(read("web_01_prod-19700101-000000Z.log"), "one\n");
        assert_eq!(read("web_01_prod-19700101-000000Z-1.log"), "two");
    }
}
