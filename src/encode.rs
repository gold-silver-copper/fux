//! Byte encodings for keys and pastes delivered to a pane's PTY, in the
//! pane's own modes (application cursor keys, bracketed paste).
use crate::keys::{Direction, Key, KeyPress, Modifiers};
use std::io::Write;

pub const PASTE_START: &[u8] = b"\x1b[200~";
pub const PASTE_END: &[u8] = b"\x1b[201~";

/// Appends a paste as the pane should receive it to `out`: framed if it
/// asked for bracketed paste, with any end marker inside the text removed so
/// the text cannot end the paste early.
pub fn paste(text: &str, bracketed: bool, out: &mut Vec<u8>) {
    if !bracketed {
        return out.extend_from_slice(text.as_bytes());
    }
    out.extend_from_slice(PASTE_START);
    for piece in text.split("\x1b[201~") {
        out.extend_from_slice(piece.as_bytes());
    }
    out.extend_from_slice(PASTE_END);
}

/// Appends a key's xterm bytes to `out`. Every key has an encoding.
pub fn key_bytes(press: KeyPress, application: bool, out: &mut Vec<u8>) {
    let KeyPress { key, mods } = press;
    let Modifiers { ctrl, alt, shift } = mods;
    // xterm's modifier parameter: 1 plus a bit for each, so at most 8.
    let bits = [(shift, 1), (alt, 2), (ctrl, 4)]
        .into_iter()
        .filter(|(on, _)| *on)
        .fold(0usize, |bits, (_, bit)| bits | bit);
    let modifier = bits.saturating_add(1);
    let csi = |out: &mut Vec<u8>, code: u8, final_byte: char| {
        let _ = if modifier > 1 {
            write!(out, "\x1b[{code};{modifier}{final_byte}")
        } else {
            write!(out, "\x1b[{code}{final_byte}")
        };
    };
    // Cursor-style keys share one shape: `ESC [ final`, `ESC O final` in
    // application mode or for F1..F4, and `ESC [ 1 ; mod final` when
    // modified. Alt adds nothing to them.
    let cursor = |out: &mut Vec<u8>, final_byte: char, function: bool| {
        if modifier > 1 {
            csi(out, 1, final_byte);
        } else {
            let prefix = if application || function { 'O' } else { '[' };
            let _ = write!(out, "\x1b{prefix}{final_byte}");
        }
    };
    let start = out.len();
    match key {
        Key::Arrow(Direction::Up) => return cursor(out, 'A', false),
        Key::Arrow(Direction::Down) => return cursor(out, 'B', false),
        Key::Arrow(Direction::Right) => return cursor(out, 'C', false),
        Key::Arrow(Direction::Left) => return cursor(out, 'D', false),
        Key::Home => return cursor(out, 'H', false),
        Key::End => return cursor(out, 'F', false),
        Key::F(1) => return cursor(out, 'P', true),
        Key::F(2) => return cursor(out, 'Q', true),
        Key::F(3) => return cursor(out, 'R', true),
        Key::F(4) => return cursor(out, 'S', true),
        Key::Enter => out.push(13),
        Key::Tab if shift => out.extend_from_slice(b"\x1b[Z"),
        Key::Tab => out.push(9),
        Key::Escape => out.push(27),
        Key::Backspace => out.push(127),
        Key::Insert => csi(out, 2, '~'),
        Key::Delete => csi(out, 3, '~'),
        Key::PageUp => csi(out, 5, '~'),
        Key::PageDown => csi(out, 6, '~'),
        Key::F(n) => {
            let codes = [15, 17, 18, 19, 20, 21, 23, 24];
            match usize::from(n).checked_sub(5).and_then(|i| codes.get(i)) {
                Some(code) => csi(out, *code, '~'),
                None => out.push(27),
            }
        }
        Key::Char(c) if ctrl && c.is_ascii() => out.push(control_byte(c)),
        Key::Char(c) => out.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes()),
    }
    // Alt is an Escape before the key, unless it begins with one already.
    if alt && out.get(start) != Some(&27) {
        out.push(27);
        // The key's bytes and the Escape after them: at least one to turn.
        if let Some(key) = out.get_mut(start..) {
            key.rotate_right(1);
        }
    }
}

/// xterm's control-key byte. Masking works for letters and the punctuation
/// that shares a column with a C0 control; the controls above Ctrl-Z are
/// named after digits too (Ctrl-4 is 0x1c), and xterm sends the digit itself
/// for the digits that have no control.
fn control_byte(c: char) -> u8 {
    match c {
        '2' => 0,
        '3' => 0x1b,
        '4' => 0x1c,
        '5' => 0x1d,
        '6' => 0x1e,
        '7' => 0x1f,
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
    fn press(key: Key, mods: Modifiers) -> KeyPress {
        KeyPress { key, mods }
    }
    /// A key's bytes, appended after others, which stay as they were.
    fn encoded(press: KeyPress, application: bool) -> Vec<u8> {
        let mut out = b"before".to_vec();
        key_bytes(press, application, &mut out);
        assert!(out.starts_with(b"before"), "{press:?}");
        out.get(6..).map(<[u8]>::to_vec).unwrap_or_default()
    }
    fn pasted(text: &str, bracketed: bool) -> Vec<u8> {
        let mut out = Vec::new();
        paste(text, bracketed, &mut out);
        out
    }

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
                encoded(press(key, Modifiers::NONE), false),
                plain.as_bytes()
            );
            assert_eq!(encoded(press(key, ALL), false), modified.as_bytes());
        }
        let left = Key::Arrow(Direction::Left);
        assert_eq!(encoded(press(left, Modifiers::NONE), true), b"\x1bOD");
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::NONE
        };
        assert_eq!(encoded(press(left, ctrl), true), b"\x1b[1;5D");
        let ctrl_shift = Modifiers {
            ctrl: true,
            shift: true,
            alt: false,
        };
        assert_eq!(encoded(press(Key::F(1), ctrl_shift), false), b"\x1b[1;6P");
        let alt = Modifiers {
            alt: true,
            ..Modifiers::NONE
        };
        assert_eq!(encoded(press(Key::F(12), alt), false), b"\x1b[24;3~");
        assert_eq!(encoded(press(Key::Char('c'), ctrl), false), vec![3]);
        assert_eq!(encoded(press(Key::Char('x'), alt), false), b"\x1bx");
    }

    #[test]
    fn control_bytes_follow_xterm_for_every_c0_control() {
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::NONE
        };
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
            assert_eq!(
                encoded(press(Key::Char(c), ctrl), false),
                vec![byte],
                "{c:?}"
            );
        }
    }

    /// Every key, under all eight modifier sets and both cursor modes, has a
    /// non-empty encoding, and a plain character is its own UTF-8.
    #[test]
    fn key_bytes_is_total_and_never_empty() {
        let mut keys = vec![
            Key::Enter,
            Key::Tab,
            Key::Escape,
            Key::Backspace,
            Key::Delete,
            Key::Insert,
            Key::Home,
            Key::End,
            Key::PageUp,
            Key::PageDown,
        ];
        keys.extend(Direction::ALL.map(Key::Arrow));
        keys.extend((1..=12).map(Key::F));
        for code in [
            0x20u32, 0x61, 0x5a, 0x30, 0x7e, 0xe9, 0x203c, 0x1f600, 0x300,
        ] {
            if let Some(c) = char::from_u32(code) {
                keys.push(Key::Char(c));
            }
        }
        for &key in &keys {
            for bits in 0u8..8 {
                let mods = Modifiers {
                    ctrl: bits & 1 != 0,
                    alt: bits & 2 != 0,
                    shift: bits & 4 != 0,
                };
                for application in [false, true] {
                    let bytes = encoded(press(key, mods), application);
                    assert!(!bytes.is_empty(), "{key:?} {mods:?}");
                    if let Key::Char(c) = key
                        && mods.is_empty()
                    {
                        let mut buffer = [0u8; 4];
                        assert_eq!(bytes, c.encode_utf8(&mut buffer).as_bytes());
                    }
                }
            }
        }
    }

    #[test]
    fn a_bracketed_paste_cannot_end_itself_early() {
        assert_eq!(pasted("a\x1b[201~b", false), b"a\x1b[201~b");
        assert_eq!(pasted("a\x1b[201~b", true), b"\x1b[200~ab\x1b[201~");
    }
}
