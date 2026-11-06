//! xterm mouse reporting (X10/normal/button/any-event tracking, SGR and UTF-8 encodings).
use crate::{MouseAction, MouseButton, MouseInput};
use alacritty_terminal::term::TermMode;

/// Encodes `input` for the current terminal `mode`; `None` when the application does not
/// want this kind of event.
pub(crate) fn encode(input: MouseInput, mode: TermMode) -> Option<Vec<u8>> {
    if !mode.intersects(TermMode::MOUSE_MODE) {
        return None;
    }
    let base = match (input.action, input.button) {
        (MouseAction::Move, None) if mode.contains(TermMode::MOUSE_MOTION) => 3 + 32,
        (MouseAction::Move, None) => return None,
        (MouseAction::Move, Some(MouseButton::WheelUp | MouseButton::WheelDown)) => return None,
        (MouseAction::Move, Some(button)) => {
            if !mode.intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION) {
                return None;
            }
            button_code(button) + 32
        }
        (MouseAction::Release, Some(MouseButton::WheelUp | MouseButton::WheelDown)) => {
            return None;
        }
        (_, Some(button)) => button_code(button),
        (_, None) => return None,
    };
    let modifiers = 4 * u8::from(input.shift) + 8 * u8::from(input.alt) + 16 * u8::from(input.ctrl);
    let code = base + modifiers;
    let (col, row) = (input.point.col as u32 + 1, input.point.row as u32 + 1);
    if mode.contains(TermMode::SGR_MOUSE) {
        let end = if input.action == MouseAction::Release {
            'm'
        } else {
            'M'
        };
        return Some(format!("\x1b[<{code};{col};{row}{end}").into_bytes());
    }
    // Legacy encodings report every release as button 3.
    let code = if input.action == MouseAction::Release {
        3 + modifiers
    } else {
        code
    };
    let mut bytes = b"\x1b[M".to_vec();
    bytes.push(32 + code);
    for value in [col, row] {
        if mode.contains(TermMode::UTF8_MOUSE) {
            let ch = char::from_u32(32 + value)?;
            let mut buf = [0; 4];
            bytes.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
        } else {
            bytes.push(u8::try_from(32 + value).ok()?);
        }
    }
    Some(bytes)
}

fn button_code(button: MouseButton) -> u8 {
    match button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
        MouseButton::WheelUp => 64,
        MouseButton::WheelDown => 65,
    }
}
