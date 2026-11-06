//! gpui keystroke → terminal input.
use gpui::Keystroke;
use shcp_core::term::{Key, KeyInput};

pub enum Input {
    Key(KeyInput),
    Text,
}

fn named(key: &str) -> Option<Key> {
    Some(match key {
        "enter" => Key::Enter,
        "tab" => Key::Tab,
        "backspace" => Key::Backspace,
        "escape" => Key::Escape,
        "up" => Key::Up,
        "down" => Key::Down,
        "left" => Key::Left,
        "right" => Key::Right,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        "insert" => Key::Insert,
        "delete" => Key::Delete,
        "space" => Key::Char(' '),
        _ => {
            let n: u8 = key.strip_prefix('f')?.parse().ok()?;
            Key::F(n)
        }
    })
}

/// Returns `None` for keystrokes the terminal ignores (e.g. lone modifiers).
pub fn from_keystroke(keystroke: &Keystroke) -> Option<Input> {
    let m = keystroke.modifiers;
    let named = named(&keystroke.key);
    let is_char = matches!(named, Some(Key::Char(_)) | None);
    let text = keystroke.key_char.as_ref().filter(|t| !t.is_empty());
    // Plain typing, shifted symbols and AltGr (reported as ctrl+alt) arrive as text.
    if text.is_some() && is_char && (!m.control && !m.alt || m.control && m.alt) && !m.platform {
        return Some(Input::Text);
    }
    let key = named.or_else(|| {
        let mut chars = keystroke.key.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) => Some(Key::Char(c)),
            _ => None,
        }
    })?;
    Some(Input::Key(KeyInput {
        key,
        ctrl: m.control,
        alt: m.alt,
        shift: m.shift,
    }))
}
