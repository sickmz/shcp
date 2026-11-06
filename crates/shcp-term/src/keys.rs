use crate::{Key, KeyInput};
pub(crate) fn encode(input: KeyInput, app_cursor: bool) -> Vec<u8> {
    let modifier = 1 + u8::from(input.shift) + 2 * u8::from(input.alt) + 4 * u8::from(input.ctrl);
    let sequence: String = match input.key {
        Key::Char(ch) => {
            let ch = if input.ctrl {
                match ch.to_ascii_uppercase() {
                    'A'..='Z' => char::from(ch.to_ascii_uppercase() as u8 - b'A' + 1),
                    '@' | ' ' | '2' => '\0',
                    '[' | '3' => '\x1b',
                    '\\' | '4' => '\x1c',
                    ']' | '5' => '\x1d',
                    '^' | '6' => '\x1e',
                    '_' | '7' | '/' => '\x1f',
                    '?' | '8' => '\x7f',
                    _ => ch,
                }
            } else {
                ch
            };
            return format!("{}{ch}", if input.alt { "\x1b" } else { "" }).into_bytes();
        }
        Key::Enter => "\r".into(),
        Key::Tab if input.shift => "\x1b[Z".into(),
        Key::Tab => "\t".into(),
        Key::Backspace => "\x7f".into(),
        Key::Escape => "\x1b".into(),
        Key::Up | Key::Down | Key::Right | Key::Left | Key::Home | Key::End => {
            let end = match input.key {
                Key::Up => 'A',
                Key::Down => 'B',
                Key::Right => 'C',
                Key::Left => 'D',
                Key::Home => 'H',
                _ => 'F',
            };
            return if modifier != 1 {
                format!("\x1b[1;{modifier}{end}")
            } else {
                format!("\x1b{}{end}", if app_cursor { 'O' } else { '[' })
            }
            .into_bytes();
        }
        Key::F(n @ 1..=4) => {
            let end = char::from(b'P' + n - 1);
            return if modifier == 1 {
                format!("\x1bO{end}")
            } else {
                format!("\x1b[1;{modifier}{end}")
            }
            .into_bytes();
        }
        other => {
            let code = match other {
                Key::Insert => 2,
                Key::Delete => 3,
                Key::PageUp => 5,
                Key::PageDown => 6,
                Key::F(5) => 15,
                Key::F(6) => 17,
                Key::F(7) => 18,
                Key::F(8) => 19,
                Key::F(9) => 20,
                Key::F(10) => 21,
                Key::F(11) => 23,
                Key::F(12) => 24,
                _ => return vec![],
            };
            return if modifier == 1 {
                format!("\x1b[{code}~")
            } else {
                format!("\x1b[{code};{modifier}~")
            }
            .into_bytes();
        }
    };
    format!("{}{sequence}", if input.alt { "\x1b" } else { "" }).into_bytes()
}
