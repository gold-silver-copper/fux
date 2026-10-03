//! Byte encodings for keys and pastes delivered to a pane's PTY, in the
//! pane's own modes (application cursor keys, the kitty keyboard protocol,
//! modifyOtherKeys, bracketed paste).
use crate::keys::{Direction, Key, KeyPress, Keystroke, Modifiers};
use std::io::Write;

/// How a pane's program asked for its keys.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeyMode {
    /// Application cursor keys (DECCKM).
    pub application: bool,
    /// The kitty keyboard protocol's flags in force on the pane's screen.
    pub kitty: u8,
    /// xterm's modifyOtherKeys level, if set.
    pub other_keys: Option<u8>,
}

impl KeyMode {
    /// The mode a pane's screen is in.
    pub fn of(screen: &fux_vt::Screen) -> KeyMode {
        KeyMode {
            application: screen.application_cursor(),
            kitty: screen.kitty_keyboard_flags(),
            other_keys: screen.modify_other_keys(),
        }
    }

    /// Legacy keys, in normal or application cursor mode.
    pub fn legacy(application: bool) -> KeyMode {
        KeyMode {
            application,
            ..KeyMode::default()
        }
    }
}

/// The kitty protocol's progressive enhancements
/// (`references/modern/kitty_keyboard_protocol.html`).
const DISAMBIGUATE: u8 = 1;
const ALTERNATE_KEYS: u8 = 4;
const ALL_KEYS: u8 = 8;
const TEXT: u8 = 16;
/// The kitty protocol's lock modifiers, Caps Lock and Num Lock.
const LOCKS: u8 = 64 | 128;

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

/// Appends a key's bytes, as the pane asked for them in `mode`, to `out`:
/// the kitty keyboard protocol's, if its flags disambiguate or report all
/// keys; else xterm's modifyOtherKeys, if it is set and applies to the key;
/// else legacy xterm bytes, which every key has.
pub fn key_bytes(stroke: Keystroke, mode: KeyMode, out: &mut Vec<u8>) {
    if mode.kitty & (DISAMBIGUATE | ALL_KEYS) != 0 {
        return kitty(stroke, mode.kitty, out);
    }
    if let Some(level @ 1..) = mode.other_keys
        && other_keys(stroke, level, out)
    {
        return;
    }
    legacy(stroke.press, mode.application, out);
}

/// The kitty keyboard protocol's bytes for a key, with `flags`, which
/// disambiguate (1) or report all keys (8)
/// (`references/modern/kitty_keyboard_protocol.html`):
/// - a key that types text, with no modifier but Shift, sends its text,
///   unless all keys are reported; then `CSI code ; mods u`, the code the
///   unshifted key's, with the text as its third parameter if asked (16);
/// - any other text key is `CSI code ; mods u`, with its shifted and
///   base-layout keys if asked (alternate keys, 4) and known;
/// - Enter, Tab and Backspace send their legacy byte unless modified (or all
///   keys are reported), Escape is `CSI 27 u`;
/// - the functional keys are `CSI 1 ; mods X` or `CSI X` (arrows, Home, End,
///   F1, F2, F4), or `CSI n ; mods ~` (Insert, Delete, the page keys, F3 as
///   13, F5 to F12), never SS3; a keypad key is its own `CSI n u`.
///
/// The modifiers are the terminal's, Super, Hyper, Meta and the locks
/// among them, if it spoke the protocol; else the press's, a capital letter
/// counting as shifted. Only presses are sent: fux reports no repeats or
/// releases (2), which the spec lets a terminal leave out.
fn kitty(stroke: Keystroke, flags: u8, out: &mut Vec<u8>) {
    let KeyPress { key, mods } = stroke.press;
    let exact = stroke.kitty;
    let all = flags & ALL_KEYS != 0;
    let capital = matches!(key, Key::Char(c) if c.is_ascii_uppercase()) && exact.is_none();
    let bits = exact.map_or_else(|| xterm_bits(mods) | u8::from(capital), |k| k.mods);
    let held = bits & !LOCKS;
    // `;mods`, left out when there are none, unless a parameter follows.
    let modifier = |out: &mut Vec<u8>, more: bool| {
        if bits != 0 || more {
            let _ = write!(out, ";{}", u16::from(bits).saturating_add(1));
        }
    };
    let csi_u = |out: &mut Vec<u8>, code: u32| {
        let _ = write!(out, "\x1b[{code}");
        modifier(out, false);
        out.push(b'u');
    };
    // A keypad key the terminal named is sent as it named it.
    if let Some(code @ 57399..=57427) = exact.and_then(|k| k.code) {
        return csi_u(out, code);
    }
    // Enter, Tab and Backspace: the legacy byte, which is also the code.
    let legacy_or = |out: &mut Vec<u8>, byte: u8| {
        if held == 0 && !all {
            out.push(byte);
        } else {
            csi_u(out, u32::from(byte));
        }
    };
    let cursor = |out: &mut Vec<u8>, final_byte: char| {
        if bits == 0 {
            let _ = write!(out, "\x1b[{final_byte}");
        } else {
            let _ = write!(out, "\x1b[1");
            modifier(out, false);
            let _ = write!(out, "{final_byte}");
        }
    };
    let tilde = |out: &mut Vec<u8>, code: u8| {
        let _ = write!(out, "\x1b[{code}");
        modifier(out, false);
        out.push(b'~');
    };
    match key {
        Key::Char(c) => {
            let types_text = held & !1 == 0;
            if types_text && !all {
                return out.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
            }
            let unshifted = if capital { c.to_ascii_lowercase() } else { c };
            let code = exact.and_then(|k| k.code).unwrap_or(u32::from(unshifted));
            let shifted = exact.and_then(|k| k.shifted).or_else(|| {
                let shifted = if unshifted.is_ascii_lowercase() {
                    unshifted.to_ascii_uppercase()
                } else {
                    c
                };
                (u32::from(shifted) != code).then_some(u32::from(shifted))
            });
            let _ = write!(out, "\x1b[{code}");
            if flags & ALTERNATE_KEYS != 0 {
                let shifted = shifted.filter(|_| bits & 1 != 0);
                let base = exact.and_then(|k| k.base);
                match (shifted, base) {
                    (Some(s), Some(b)) => drop(write!(out, ":{s}:{b}")),
                    (Some(s), None) => drop(write!(out, ":{s}")),
                    (None, Some(b)) => drop(write!(out, "::{b}")),
                    (None, None) => {}
                }
            }
            let text = (all && flags & TEXT != 0 && types_text).then_some(u32::from(c));
            modifier(out, text.is_some());
            if let Some(text) = text {
                let _ = write!(out, ";{text}");
            }
            out.push(b'u');
        }
        Key::Enter => legacy_or(out, 0x0d),
        Key::Tab => legacy_or(out, 0x09),
        Key::Backspace => legacy_or(out, 0x7f),
        Key::Escape => csi_u(out, 27),
        Key::Arrow(Direction::Up) => cursor(out, 'A'),
        Key::Arrow(Direction::Down) => cursor(out, 'B'),
        Key::Arrow(Direction::Right) => cursor(out, 'C'),
        Key::Arrow(Direction::Left) => cursor(out, 'D'),
        Key::Home => cursor(out, 'H'),
        Key::End => cursor(out, 'F'),
        Key::F(1) => cursor(out, 'P'),
        Key::F(2) => cursor(out, 'Q'),
        Key::F(3) => tilde(out, 13),
        Key::F(4) => cursor(out, 'S'),
        Key::Insert => tilde(out, 2),
        Key::Delete => tilde(out, 3),
        Key::PageUp => tilde(out, 5),
        Key::PageDown => tilde(out, 6),
        Key::F(n) => match usize::from(n).checked_sub(5).and_then(|i| F_CODES.get(i)) {
            Some(code) => tilde(out, *code),
            // F13 to F35 are numbered from 57376; fux names none past F12.
            None => csi_u(
                out,
                57376_u32.saturating_add(u32::from(n.saturating_sub(13))),
            ),
        },
    }
}

/// The `CSI n ~` numbers of F5 to F12.
const F_CODES: [u8; 8] = [15, 17, 18, 19, 20, 21, 23, 24];

/// Shift, Alt and Ctrl as xterm's and kitty's modifier bits.
fn xterm_bits(mods: Modifiers) -> u8 {
    u8::from(mods.shift) | u8::from(mods.alt) << 1 | u8::from(mods.ctrl) << 2
}

/// xterm's modifyOtherKeys bytes for a key, `CSI 27 ; mods ; code ~`
/// (formatOtherKeys 0, xterm's default), if the key is an ordinary one
/// (a character, Enter, Tab, Escape, Backspace) and `level` applies to it;
/// else nothing, and false, for the legacy bytes. As ctlseqs says ("Alt and
/// Meta Keys"):
/// - level 1: "the usual shift- and control-modifiers work as expected,
///   but other modifiers (such as alt- and meta-modifiers) cause ordinary
///   keys to be encoded as if they were function-keys": a key with Alt;
/// - level 2: "all of the modifiers apply" (Shift-Tab is `CSI 27 ; 2 ; 9 ~`):
///   a key with any modifier, except a character with Shift alone, which is
///   its text. That is fux's one departure, and xterm's own: its input.c
///   sends `!` for Shift-1, and fux cannot tell Shift from Caps Lock on a
///   typed letter, which the client's terminal sends as text either way;
/// - level 3, which xterm extends to unmodified keys, is taken as 2.
///
/// The code is the character typed: Ctrl-Shift-a is 65, as xterm sends the
/// key's symbol.
fn other_keys(stroke: Keystroke, level: u8, out: &mut Vec<u8>) -> bool {
    let KeyPress { key, mods } = stroke.press;
    let kitty_shift = stroke.kitty.is_some_and(|k| k.mods & 1 != 0);
    let shift = mods.shift || kitty_shift || matches!(key, Key::Char(c) if c.is_ascii_uppercase());
    let bits = xterm_bits(Modifiers { shift, ..mods });
    let code = match key {
        Key::Enter => 13,
        Key::Tab => 9,
        Key::Escape => 27,
        Key::Backspace => 127,
        Key::Char(c) if shift => u32::from(c.to_ascii_uppercase()),
        Key::Char(c) => u32::from(c),
        Key::Delete
        | Key::Insert
        | Key::Arrow(_)
        | Key::Home
        | Key::End
        | Key::PageUp
        | Key::PageDown
        | Key::F(_) => return false,
    };
    let text = matches!(key, Key::Char(_)) && !mods.ctrl && !mods.alt;
    let applies = match level {
        1 => mods.alt,
        _ => bits != 0 && !text,
    };
    if applies {
        let _ = write!(out, "\x1b[27;{};{code}~", u16::from(bits).saturating_add(1));
    }
    applies
}

/// Legacy xterm bytes for a key, as fux has always sent them. Every key has
/// an encoding.
fn legacy(press: KeyPress, application: bool, out: &mut Vec<u8>) {
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
        Key::F(n) => match usize::from(n).checked_sub(5).and_then(|i| F_CODES.get(i)) {
            Some(code) => csi(out, *code, '~'),
            None => out.push(27),
        },
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
        key_bytes(press.into(), KeyMode::legacy(application), &mut out);
        assert!(out.starts_with(b"before"), "{press:?}");
        out.get(6..).map(<[u8]>::to_vec).unwrap_or_default()
    }
    /// The bytes for a named key, or a stroke, in `mode`, as text.
    fn named(name: &str, mode: KeyMode) -> String {
        let press = name.parse::<KeyPress>().unwrap_or(KeyPress::char('?'));
        stroke(press.into(), mode)
    }
    fn stroke(stroke: Keystroke, mode: KeyMode) -> String {
        let mut out = Vec::new();
        key_bytes(stroke, mode, &mut out);
        String::from_utf8_lossy(&out).into_owned()
    }
    fn kitty_mode(flags: u8) -> KeyMode {
        KeyMode {
            kitty: flags,
            ..KeyMode::default()
        }
    }
    /// A key as a kitty-protocol terminal reports it.
    fn reported(
        name: &str,
        code: u32,
        shifted: Option<u32>,
        base: Option<u32>,
        mods: u8,
    ) -> Keystroke {
        Keystroke {
            press: name.parse().unwrap_or(KeyPress::char('?')),
            kitty: Some(crate::keys::Kitty {
                code: Some(code),
                shifted,
                base,
                mods,
            }),
        }
    }

    /// With disambiguate (1), as the kitty spec's "Disambiguate escape
    /// codes" and "Functional key definitions" say: text keys send their
    /// text; Escape and keys with Ctrl or Alt are `CSI code ; mods u`;
    /// Enter, Tab and Backspace stay legacy unless modified; the functional
    /// keys take the `CSI 1 ; mods X` and `CSI n ; mods ~` forms, never SS3,
    /// even in application cursor mode; F3 is `CSI 13 ~`.
    #[test]
    fn disambiguated_keys_follow_the_kitty_spec() {
        let mode = kitty_mode(1);
        for (name, bytes) in [
            ("a", "a"),
            ("A", "A"),
            ("界", "界"),
            ("Escape", "\x1b[27u"),
            ("M-Escape", "\x1b[27;3u"),
            ("Enter", "\r"),
            ("S-Enter", "\x1b[13;2u"),
            ("C-Enter", "\x1b[13;5u"),
            ("Tab", "\t"),
            ("BTab", "\x1b[9;2u"),
            ("BSpace", "\x7f"),
            ("C-BSpace", "\x1b[127;5u"),
            ("M-BSpace", "\x1b[127;3u"),
            ("C-a", "\x1b[97;5u"),
            ("C-i", "\x1b[105;5u"),
            ("C-m", "\x1b[109;5u"),
            ("C-[", "\x1b[91;5u"),
            ("M-[", "\x1b[91;3u"),
            ("M-a", "\x1b[97;3u"),
            ("C-M-a", "\x1b[97;7u"),
            ("M-A", "\x1b[97;4u"),
            ("C-Space", "\x1b[32;5u"),
            ("M-!", "\x1b[33;3u"),
            ("Up", "\x1b[A"),
            ("C-Up", "\x1b[1;5A"),
            ("S-Left", "\x1b[1;2D"),
            ("Home", "\x1b[H"),
            ("End", "\x1b[F"),
            ("F1", "\x1b[P"),
            ("F2", "\x1b[Q"),
            ("F3", "\x1b[13~"),
            ("S-F3", "\x1b[13;2~"),
            ("F4", "\x1b[S"),
            ("F5", "\x1b[15~"),
            ("C-F12", "\x1b[24;5~"),
            ("Insert", "\x1b[2~"),
            ("C-Delete", "\x1b[3;5~"),
            ("PageUp", "\x1b[5~"),
            ("PageDown", "\x1b[6~"),
        ] {
            assert_eq!(named(name, mode), bytes, "{name}");
        }
        let application = KeyMode {
            application: true,
            ..mode
        };
        assert_eq!(named("Up", application), "\x1b[A");
        // Flags without disambiguate or all keys change nothing.
        assert_eq!(named("C-i", kitty_mode(4)), "\t");
        assert_eq!(named("Escape", kitty_mode(2)), "\x1b");
    }

    /// What a kitty-protocol terminal said beyond the press is given back:
    /// Shift with Ctrl, which the press folds away (ctrl+shift+i is
    /// `CSI 105 ; 6 u`, the spec's "Legacy text keys" table), Super and the
    /// locks, a keypad key's own number; with alternate keys (4), the
    /// shifted and base-layout keys ("Key codes").
    #[test]
    fn a_reported_key_is_given_back_as_reported() {
        let ctrl_shift_i = reported("C-i", 105, Some(73), None, 5);
        assert_eq!(stroke(ctrl_shift_i, kitty_mode(1)), "\x1b[105;6u");
        assert_eq!(stroke(ctrl_shift_i, kitty_mode(5)), "\x1b[105:73;6u");
        let super_a = reported("a", 97, None, None, 8);
        assert_eq!(stroke(super_a, kitty_mode(1)), "\x1b[97;9u");
        // Ctrl and С on a Cyrillic layout, whose base-layout key is c.
        let cyrillic = reported("C-c", 1089, None, Some(99), 4);
        assert_eq!(stroke(cyrillic, kitty_mode(1)), "\x1b[1089;5u");
        assert_eq!(stroke(cyrillic, kitty_mode(5)), "\x1b[1089::99;5u");
        let keypad_enter = reported("Enter", 57414, None, None, 0);
        assert_eq!(stroke(keypad_enter, kitty_mode(1)), "\x1b[57414u");
        assert_eq!(stroke(keypad_enter, KeyMode::default()), "\r");
        let caps_ctrl_a = reported("C-a", 97, None, None, 4 | 64);
        assert_eq!(stroke(caps_ctrl_a, kitty_mode(1)), "\x1b[97;69u");
        // A legacy pane gets the press's bytes, as before.
        assert_eq!(stroke(ctrl_shift_i, KeyMode::default()), "\t");
        assert_eq!(stroke(cyrillic, KeyMode::default()), "\x03");
        // Alternate keys from a legacy press: a capital's shifted key.
        assert_eq!(named("M-A", kitty_mode(5)), "\x1b[97:65;4u");
    }

    /// Report all keys (8): text keys too are `CSI code ; mods u`, with the
    /// text as a third parameter if asked (16; the spec's "Text as code
    /// points": shift+a is `CSI 97 ; 2 ; 65 u`); Enter, Tab and Backspace
    /// are escapes.
    #[test]
    fn reporting_all_keys_escapes_text_keys() {
        for (flags, name, bytes) in [
            (9, "a", "\x1b[97u"),
            (9, "A", "\x1b[97;2u"),
            (13, "A", "\x1b[97:65;2u"),
            (25, "a", "\x1b[97;1;97u"),
            (25, "A", "\x1b[97;2;65u"),
            (25, "C-a", "\x1b[97;5u"),
            (9, "Enter", "\x1b[13u"),
            (9, "Tab", "\x1b[9u"),
            (9, "BSpace", "\x1b[127u"),
            (8, "Escape", "\x1b[27u"),
            (8, "Up", "\x1b[A"),
        ] {
            assert_eq!(named(name, kitty_mode(flags)), bytes, "{flags} {name}");
        }
    }

    /// modifyOtherKeys, as ctlseqs's "Alt and Meta Keys" says: at level 1
    /// Alt makes an ordinary key `CSI 27 ; mods ; code ~` (alt-Tab is
    /// `CSI 27 ; 3 ; 9 ~`) and Shift and Ctrl work as usual; at level 2 every
    /// modifier does (shift-Tab is `CSI 27 ; 2 ; 9 ~`), but a character with
    /// Shift alone is its text. Other keys keep their legacy bytes, and the
    /// kitty flags, when set, win.
    #[test]
    fn modify_other_keys_follows_ctlseqs() {
        let level = |n| KeyMode {
            other_keys: Some(n),
            ..KeyMode::default()
        };
        for (n, name, bytes) in [
            (1, "M-Tab", "\x1b[27;3;9~"),
            (1, "M-a", "\x1b[27;3;97~"),
            (1, "C-a", "\x01"),
            (1, "BTab", "\x1b[Z"),
            (1, "S-Enter", "\r"),
            (1, "a", "a"),
            (2, "BTab", "\x1b[27;2;9~"),
            (2, "S-Enter", "\x1b[27;2;13~"),
            (2, "C-Enter", "\x1b[27;5;13~"),
            (2, "M-Escape", "\x1b[27;3;27~"),
            (2, "C-BSpace", "\x1b[27;5;127~"),
            (2, "C-a", "\x1b[27;5;97~"),
            (2, "C-i", "\x1b[27;5;105~"),
            (2, "M-x", "\x1b[27;3;120~"),
            (2, "M-A", "\x1b[27;4;65~"),
            (2, "C-Space", "\x1b[27;5;32~"),
            (2, "a", "a"),
            (2, "A", "A"),
            (2, "Enter", "\r"),
            (2, "Escape", "\x1b"),
            (2, "C-Up", "\x1b[1;5A"),
            (2, "F5", "\x1b[15~"),
            (3, "C-a", "\x1b[27;5;97~"),
        ] {
            assert_eq!(named(name, level(n)), bytes, "level {n} {name}");
        }
        let ctrl_shift_a = reported("C-a", 97, Some(65), None, 5);
        assert_eq!(stroke(ctrl_shift_a, level(2)), "\x1b[27;6;65~");
        let both = KeyMode {
            kitty: 1,
            other_keys: Some(2),
            ..KeyMode::default()
        };
        assert_eq!(named("C-a", both), "\x1b[97;5u");
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
    /// non-empty encoding, and a plain character is its own UTF-8; in every
    /// pane mode too, but where all keys are escapes.
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
                for (kitty, other_keys) in
                    [(1, None), (5, None), (31, None), (0, Some(1)), (0, Some(2))]
                {
                    let mode = KeyMode {
                        application: false,
                        kitty,
                        other_keys,
                    };
                    let mut bytes = Vec::new();
                    key_bytes(press(key, mods).into(), mode, &mut bytes);
                    assert!(!bytes.is_empty(), "{key:?} {mods:?} {mode:?}");
                    if let Key::Char(c) = key
                        && mods.is_empty()
                        && kitty & ALL_KEYS == 0
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
