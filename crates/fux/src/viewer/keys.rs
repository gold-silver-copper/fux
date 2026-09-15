//! Key chords and terminal byte encoding.
//!
//! The terminal already produced the bytes a key would send to a program; `input::translate`
//! stores them verbatim in `KeyboardInput::text` (a control character for Ctrl-letter, an
//! `ESC`-prefixed sequence for Alt). Encoding therefore only re-derives the SS3 forms an
//! application-cursor-mode program expects and wraps pastes in bracketed-paste markers.
//!
//! Chords also have a text form, the one `fux.toml` uses (`prefix`, `[bindings]`): modifiers
//! `C-`, `M-`, `S-` before a printable key or a key name (`Esc`, `Space`, `Tab`, `Enter`,
//! `Backspace`, `DEL`, arrows, `Home`, `End`, `PageUp`, `PageDown`, `Insert`, `Delete`,
//! `F1`-`F12`) or a byte `0xHH`; [`KeyChord::parse`] reads it, `Display` writes it back.

use core::fmt;

use bevy_input::keyboard::{Key, KeyboardInput};

use crate::wire::Modes;

const ESC: u8 = 0x1b;

/// Key names in their canonical spelling; parsing is case-insensitive.
const NAMED: &[(&str, Key)] = &[
    ("Esc", Key::Escape),
    ("Tab", Key::Tab),
    ("Enter", Key::Enter),
    ("Backspace", Key::Backspace),
    ("Insert", Key::Insert),
    ("Delete", Key::Delete),
    ("Home", Key::Home),
    ("End", Key::End),
    ("PageUp", Key::PageUp),
    ("PageDown", Key::PageDown),
    ("Up", Key::ArrowUp),
    ("Down", Key::ArrowDown),
    ("Left", Key::ArrowLeft),
    ("Right", Key::ArrowRight),
    ("F1", Key::F1),
    ("F2", Key::F2),
    ("F3", Key::F3),
    ("F4", Key::F4),
    ("F5", Key::F5),
    ("F6", Key::F6),
    ("F7", Key::F7),
    ("F8", Key::F8),
    ("F9", Key::F9),
    ("F10", Key::F10),
    ("F11", Key::F11),
    ("F12", Key::F12),
];

/// Alternative spellings of the names above (and of `Space`).
const ALIASES: &[(&str, &str)] = &[
    ("Escape", "Esc"),
    ("Return", "Enter"),
    ("BS", "Backspace"),
    ("DEL", "Backspace"),
    ("PgUp", "PageUp"),
    ("PgDn", "PageDown"),
    ("Ins", "Insert"),
    ("Del", "Delete"),
];

/// A key as bindings see it: the logical key plus the modifier state the terminal encoded.
///
/// Ctrl and Alt are derived from the produced text, so they are only meaningful for
/// `Key::Character` chords (Ctrl-b, Alt-x). Shift is only meaningful for named keys, where the
/// terminal encodes it in the sequence itself (`CSI Z` for Shift-Tab, the `;2` modifier parameter
/// for shifted arrows and function keys); a shifted character already arrives as that character.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct KeyChord {
    pub key: Key,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl KeyChord {
    pub fn plain(key: Key) -> Self {
        Self {
            key,
            ctrl: false,
            alt: false,
            shift: false,
        }
    }

    pub fn shift(key: Key) -> Self {
        Self {
            shift: true,
            ..Self::plain(key)
        }
    }

    pub fn character(c: char) -> Self {
        let mut buf = [0u8; 4];
        Self::plain(Key::Character((*c.encode_utf8(&mut buf)).into()))
    }

    pub fn ctrl(c: char) -> Self {
        Self {
            ctrl: true,
            ..Self::character(c)
        }
    }

    pub fn of(input: &KeyboardInput) -> Self {
        let named = !matches!(input.logical_key, Key::Character(_));
        let text: &[u8] = input.text.as_ref().map_or(&[], |t| t.as_bytes());
        let (alt, body) = match text.split_first() {
            Some((&ESC, rest)) if !named && !rest.is_empty() => (true, rest),
            _ => (false, text),
        };
        let ctrl =
            !named && body.len() == 1 && body.first().is_some_and(|b| *b < 0x20 || *b == 0x7f);
        Self {
            key: input.logical_key.clone(),
            ctrl,
            alt,
            shift: named && csi_shifted(text),
        }
    }

    /// Reads the text form: `[C-][M-][S-]<key>` where `<key>` is one printable character,
    /// `Space`, a name from the module list, `S-Tab`'s alias `BackTab`, or a byte `0xHH`
    /// (control bytes become `C-<letter>`, `0x1b` `Esc`, `0x7f` `Backspace`). `S-` is only
    /// meaningful on named keys: a shifted character already arrives as that character.
    pub fn parse(text: &str) -> Option<Self> {
        let mut rest = text;
        let (mut ctrl, mut alt, mut shift) = (false, false, false);
        while let Some((modifier, key)) = rest.split_once('-') {
            if key.is_empty() {
                break;
            }
            match modifier {
                "C" => ctrl = true,
                "M" => alt = true,
                "S" => shift = true,
                _ => break,
            }
            rest = key;
        }
        if let Some(hex) = rest.strip_prefix("0x") {
            if ctrl || alt || shift {
                return None;
            }
            return Self::from_byte(u8::from_str_radix(hex, 16).ok()?);
        }
        let mut chars = rest.chars();
        let key = match (chars.next()?, chars.next()) {
            (c, None) if !c.is_whitespace() && !c.is_control() => {
                if shift {
                    return None;
                }
                let mut buf = [0u8; 4];
                Key::Character((*c.encode_utf8(&mut buf)).into())
            }
            _ if rest.eq_ignore_ascii_case("Space") => {
                if shift {
                    return None;
                }
                Key::Character(" ".into())
            }
            _ if rest.eq_ignore_ascii_case("BackTab") => {
                shift = true;
                Key::Tab
            }
            _ => {
                if ctrl || alt {
                    return None;
                }
                let canonical = ALIASES
                    .iter()
                    .find(|(alias, _)| alias.eq_ignore_ascii_case(rest))
                    .map_or(rest, |(_, name)| name);
                NAMED
                    .iter()
                    .find(|(name, _)| name.eq_ignore_ascii_case(canonical))?
                    .1
                    .clone()
            }
        };
        Some(Self {
            key,
            ctrl,
            alt,
            shift,
        })
    }

    /// The chord a terminal sends as one byte.
    fn from_byte(byte: u8) -> Option<Self> {
        Some(match byte {
            0x09 => Self::plain(Key::Tab),
            0x0d => Self::plain(Key::Enter),
            0x1b => Self::plain(Key::Escape),
            0x7f => Self::plain(Key::Backspace),
            0x00 => Self::ctrl(' '),
            0x01..=0x1a => Self::ctrl(char::from(b'a' + byte - 1)),
            0x1c..=0x1f => Self::ctrl(char::from(b'\\' + byte - 0x1c)),
            0x20..=0x7e => Self::character(char::from(byte)),
            _ => return None,
        })
    }

    /// Appends the bytes a terminal sends for this chord in normal cursor mode; `false` when
    /// the chord has no byte form (a shifted or modified named key beyond Shift-Tab).
    pub fn bytes(&self, out: &mut Vec<u8>) -> bool {
        match &self.key {
            Key::Character(c) => {
                if self.alt {
                    out.push(ESC);
                }
                if self.ctrl {
                    let byte = match c.as_bytes() {
                        [b' '] => 0x00,
                        [b @ (b'@'..=b'_' | b'a'..=b'z')] => b & 0x1f,
                        _ => return false,
                    };
                    out.push(byte);
                } else {
                    out.extend_from_slice(c.as_bytes());
                }
                true
            }
            key => {
                if self.ctrl || self.alt {
                    return false;
                }
                let bytes: &[u8] = match (key, self.shift) {
                    (Key::Escape, false) => b"\x1b",
                    (Key::Tab, false) => b"\t",
                    (Key::Tab, true) => b"\x1b[Z",
                    (Key::Enter, false) => b"\r",
                    (Key::Backspace, false) => b"\x7f",
                    (Key::ArrowUp, false) => b"\x1b[A",
                    (Key::ArrowDown, false) => b"\x1b[B",
                    (Key::ArrowRight, false) => b"\x1b[C",
                    (Key::ArrowLeft, false) => b"\x1b[D",
                    (Key::Home, false) => b"\x1b[H",
                    (Key::End, false) => b"\x1b[F",
                    (Key::Insert, false) => b"\x1b[2~",
                    (Key::Delete, false) => b"\x1b[3~",
                    (Key::PageUp, false) => b"\x1b[5~",
                    (Key::PageDown, false) => b"\x1b[6~",
                    _ => return false,
                };
                out.extend_from_slice(bytes);
                true
            }
        }
    }
}

impl fmt::Display for KeyChord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.ctrl {
            f.write_str("C-")?;
        }
        if self.alt {
            f.write_str("M-")?;
        }
        if self.shift {
            f.write_str("S-")?;
        }
        match &self.key {
            Key::Character(c) if c == " " => f.write_str("Space"),
            Key::Character(c) => f.write_str(c),
            key => match NAMED.iter().find(|(_, k)| k == key) {
                Some((name, _)) => f.write_str(name),
                None => write!(f, "{key:?}"),
            },
        }
    }
}

/// Whether a named key's CSI sequence carries Shift: `CSI Z` (Shift-Tab) or a `;<p>` modifier
/// parameter whose shift bit (bit 0 of `p - 1`) is set.
fn csi_shifted(text: &[u8]) -> bool {
    let Some(body) = text.strip_prefix(b"\x1b[") else {
        return false;
    };
    if body == b"Z" {
        return true;
    }
    let Some(semicolon) = body.iter().position(|b| *b == b';') else {
        return false;
    };
    let param = body
        .get(semicolon + 1..)
        .unwrap_or_default()
        .iter()
        .take_while(|b| b.is_ascii_digit())
        .fold(0u8, |acc, b| {
            acc.saturating_mul(10).saturating_add(b.wrapping_sub(b'0'))
        });
    param > 0 && (param - 1) & 1 != 0
}

/// Appends the bytes a terminal program should receive for `input`; `false` if the key produces
/// no bytes (a bare modifier, a media key).
pub fn encode(input: &KeyboardInput, modes: Modes, out: &mut Vec<u8>) -> bool {
    let Some(text) = input.text.as_ref() else {
        return false;
    };
    let bytes = text.as_bytes();
    if modes.application_cursor {
        let ss3 = match input.logical_key {
            Key::ArrowUp => Some(b'A'),
            Key::ArrowDown => Some(b'B'),
            Key::ArrowRight => Some(b'C'),
            Key::ArrowLeft => Some(b'D'),
            Key::Home => Some(b'H'),
            Key::End => Some(b'F'),
            _ => None,
        };
        // Only the unmodified CSI form switches to SS3; modified arrows keep `CSI 1;m X`.
        if let Some(letter) = ss3
            && bytes.len() == 3
            && bytes.starts_with(&[ESC, b'['])
        {
            out.extend_from_slice(&[ESC, b'O', letter]);
            return true;
        }
    }
    if bytes.is_empty() {
        return false;
    }
    out.extend_from_slice(bytes);
    true
}

/// Appends pasted text, wrapped in bracketed-paste markers when the program asked for them.
pub fn encode_paste(text: &str, modes: Modes, out: &mut Vec<u8>) {
    if modes.bracketed_paste {
        out.extend_from_slice(b"\x1b[200~");
        out.extend_from_slice(text.as_bytes());
        out.extend_from_slice(b"\x1b[201~");
    } else {
        out.extend_from_slice(text.as_bytes());
    }
}
