//! Key chords and terminal byte encoding.
//!
//! The terminal already produced the bytes a key would send to a program; `input::translate`
//! stores them verbatim in `KeyboardInput::text` (a control character for Ctrl-letter, an
//! `ESC`-prefixed sequence for Alt). Encoding therefore only re-derives the SS3 forms an
//! application-cursor-mode program expects and wraps pastes in bracketed-paste markers.

use bevy_input::keyboard::{Key, KeyboardInput};

use crate::wire::Modes;

const ESC: u8 = 0x1b;

/// A key as bindings see it: the logical key plus the Ctrl/Alt state the terminal encoded.
///
/// Ctrl and Alt are derived from the produced text, so they are only meaningful for
/// `Key::Character` chords (Ctrl-b, Alt-x); named keys (arrows, function keys) bind unmodified.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct KeyChord {
    pub key: Key,
    pub ctrl: bool,
    pub alt: bool,
}

impl KeyChord {
    pub fn plain(key: Key) -> Self {
        Self {
            key,
            ctrl: false,
            alt: false,
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
        }
    }
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
