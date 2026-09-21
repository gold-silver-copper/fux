//! Byte encodings for keys and mouse events delivered to a PTY.
use crate::protocol::{Direction, Key, Modifiers, MouseAction, MouseButton};
use fux_vt::{MouseProtocolEncoding, MouseProtocolMode as MouseMode};

/// xterm mouse bytes for a pane-relative one-based cell, or `None` when the
/// application's protocol mode does not want this event.
pub(crate) fn mouse_bytes(
    screen: &fux_vt::Screen,
    action: MouseAction,
    button: MouseButton,
    (col, row): (u16, u16),
    modifiers: Modifiers,
) -> Option<Vec<u8>> {
    let mode = screen.mouse_protocol_mode();
    let release = action == MouseAction::Release;
    let motion = action == MouseAction::Move;
    if release && mode == MouseMode::Press
        || motion
            && (matches!(mode, MouseMode::Press | MouseMode::PressRelease)
                || mode == MouseMode::ButtonMotion && button == MouseButton::None)
    {
        return None;
    }
    let mut code = match action {
        MouseAction::ScrollUp => 64,
        MouseAction::ScrollDown => 65,
        _ => match button {
            MouseButton::Left => 0,
            MouseButton::Middle => 1,
            MouseButton::Right => 2,
            MouseButton::None => 3,
        },
    };
    if motion {
        code += 32;
    }
    if modifiers.shift {
        code += 4;
    }
    if modifiers.alt {
        code += 8;
    }
    if modifiers.ctrl {
        code += 16;
    }
    let (rows, cols) = screen.size();
    if row > rows || col > cols {
        return None;
    }
    if screen.mouse_protocol_encoding() == MouseProtocolEncoding::Sgr {
        Some(
            format!(
                "\x1b[<{code};{col};{row}{}",
                if release { 'm' } else { 'M' }
            )
            .into_bytes(),
        )
    } else if col <= 223 && row <= 223 {
        // A legacy release is button 3 with the same modifier bits as the press.
        let byte = if release { (code & !3) | 3 } else { code };
        Some(vec![
            27,
            b'[',
            b'M',
            byte as u8 + 32,
            col as u8 + 32,
            row as u8 + 32,
        ])
    } else {
        None
    }
}

/// xterm key bytes. Every `Key` has an encoding; function keys beyond F12
/// cannot be constructed from input and encode as nothing.
pub(crate) fn key_bytes(key: Key, modifiers: Modifiers, application: bool) -> Vec<u8> {
    let Modifiers { ctrl, alt, shift } = modifiers;
    let modifier = 1 + usize::from(shift) + 2 * usize::from(alt) + 4 * usize::from(ctrl);
    let csi = |code: u8, final_byte: char| {
        if modifier > 1 {
            format!("\x1b[{code};{modifier}{final_byte}")
        } else {
            format!("\x1b[{code}{final_byte}")
        }
        .into_bytes()
    };
    // Cursor-style keys share one shape: `ESC [ final`, `ESC O final` in
    // application mode or for F1..F4, and `ESC [ 1 ; mod final` when modified.
    let cursor = match key {
        Key::Arrow(Direction::Up) => Some(('A', false)),
        Key::Arrow(Direction::Down) => Some(('B', false)),
        Key::Arrow(Direction::Right) => Some(('C', false)),
        Key::Arrow(Direction::Left) => Some(('D', false)),
        Key::Home => Some(('H', false)),
        Key::End => Some(('F', false)),
        Key::F(n @ 1..=4) => Some((char::from(b'P' + n - 1), true)),
        _ => None,
    };
    if let Some((final_byte, function)) = cursor {
        return if modifier > 1 {
            csi(1, final_byte)
        } else {
            let prefix = if application || function { 'O' } else { '[' };
            format!("\x1b{prefix}{final_byte}").into_bytes()
        };
    }
    let mut bytes = match key {
        Key::Enter => vec![13],
        Key::Tab if shift => b"\x1b[Z".to_vec(),
        Key::Tab => vec![9],
        Key::Escape => vec![27],
        Key::Backspace => vec![127],
        Key::Insert => csi(2, '~'),
        Key::Delete => csi(3, '~'),
        Key::PageUp => csi(5, '~'),
        Key::PageDown => csi(6, '~'),
        Key::F(n) => {
            let codes = [15, 17, 18, 19, 20, 21, 23, 24];
            match usize::from(n).checked_sub(5).and_then(|i| codes.get(i)) {
                Some(code) => csi(*code, '~'),
                None => Vec::new(),
            }
        }
        Key::Char(c) if ctrl && c.is_ascii() => vec![control_byte(c)],
        Key::Char(c) => c.to_string().into_bytes(),
        // Handled by the cursor table above.
        Key::Arrow(_) | Key::Home | Key::End => Vec::new(),
    };
    if alt && !bytes.starts_with(&[27]) {
        bytes.insert(0, 27);
    }
    bytes
}

/// xterm's control-key byte. Masking works for letters and the punctuation
/// that shares a column with a C0 control, but the decoder names the controls
/// above Ctrl-Z after digits (Ctrl-4 is 0x1c), and xterm sends the digit itself
/// for the digits that have no control.
fn control_byte(c: char) -> u8 {
    match c {
        '2' => 0,
        '3' => 0x1b,
        '4'..='7' => 0x1c + (c as u8 - b'4'),
        '8' | '?' => 0x7f,
        '0' | '1' | '9' => c as u8,
        _ => (c.to_ascii_uppercase() as u8) & 0x1f,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: Modifiers = Modifiers {
        ctrl: true,
        alt: true,
        shift: true,
    };

    #[test]
    fn modified_keys_preserve_xterm_protocol_semantics() {
        for (key, plain, modified) in [
            (Key::F(1), "\x1bOP", "\x1b[1;8P"),
            (Key::F(4), "\x1bOS", "\x1b[1;8S"),
            (Key::F(5), "\x1b[15~", "\x1b[15;8~"),
            (Key::Insert, "\x1b[2~", "\x1b[2;8~"),
            (Key::Home, "\x1b[H", "\x1b[1;8H"),
        ] {
            assert_eq!(
                key_bytes(key, Modifiers::default(), false),
                plain.as_bytes()
            );
            assert_eq!(key_bytes(key, ALL, false), modified.as_bytes());
        }
        let left = Key::Arrow(Direction::Left);
        assert_eq!(key_bytes(left, Modifiers::default(), true), b"\x1bOD");
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::default()
        };
        assert_eq!(key_bytes(left, ctrl, true), b"\x1b[1;5D");
        let ctrl_shift = Modifiers {
            ctrl: true,
            shift: true,
            alt: false,
        };
        assert_eq!(key_bytes(Key::F(1), ctrl_shift, false), b"\x1b[1;6P");
        let alt = Modifiers {
            alt: true,
            ..Modifiers::default()
        };
        assert_eq!(key_bytes(Key::F(12), alt, false), b"\x1b[24;3~");
        assert_eq!(key_bytes(Key::Char('c'), ctrl, false), vec![3]);
        assert!(key_bytes(Key::F(13), Modifiers::default(), false).is_empty());
    }

    #[test]
    fn legacy_mouse_release_keeps_modifiers_like_sgr() {
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::default()
        };
        let at = |request: &[u8], action, modifiers| {
            let mut parser = fux_vt::Parser::new(24, 80, 0).ok()?;
            parser.process(request).ok()?;
            mouse_bytes(
                parser.screen(),
                action,
                MouseButton::Left,
                (1, 2),
                modifiers,
            )
        };
        let legacy: &[u8] = b"\x1b[?1000h";
        assert_eq!(
            at(legacy, MouseAction::Press, ctrl),
            Some(b"\x1b[M0!\"".to_vec())
        );
        assert_eq!(
            at(legacy, MouseAction::Release, ctrl),
            Some(b"\x1b[M3!\"".to_vec())
        );
        assert_eq!(
            at(legacy, MouseAction::Release, Modifiers::default()),
            Some(b"\x1b[M#!\"".to_vec())
        );
        assert_eq!(
            at(b"\x1b[?1000h\x1b[?1006h", MouseAction::Release, ctrl),
            Some(b"\x1b[<16;1;2m".to_vec())
        );
    }

    #[test]
    fn control_bytes_follow_xterm_for_every_c0_control() {
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::default()
        };
        // The outer terminal's decoder names each C0 control by the key xterm
        // sends it for; re-encoding must produce the same byte it received.
        for (c, byte) in [
            (' ', 0x00),
            ('2', 0x00),
            ('a', 0x01),
            ('Z', 0x1a),
            ('3', 0x1b),
            ('[', 0x1b),
            ('4', 0x1c),
            ('\\', 0x1c),
            ('5', 0x1d),
            (']', 0x1d),
            ('6', 0x1e),
            ('^', 0x1e),
            ('7', 0x1f),
            ('_', 0x1f),
            ('8', 0x7f),
            ('?', 0x7f),
            ('0', b'0'),
            ('9', b'9'),
        ] {
            assert_eq!(key_bytes(Key::Char(c), ctrl, false), vec![byte], "{c:?}");
        }
    }
}
