//! Decoding a client's raw terminal input into keys, pastes and focus
//! changes. The client is a dumb pipe; this is the only decoder.
//!
//! The outer terminal is in normal (not application) cursor and keypad mode,
//! so each key has one xterm encoding. A lone Escape is only known to be one
//! when no more bytes follow within `ESCAPE_DELAY`; the server calls
//! `timeout` at the deadline `pending_since` reports. Mouse sequences, which a
//! correctly configured outer terminal never sends, are dropped.
use crate::keys::{Direction, Key, KeyPress, Modifiers};
use std::time::Duration;

/// How long a lone Escape waits for the rest of a sequence.
pub const ESCAPE_DELAY: Duration = Duration::from_millis(35);
/// The largest paste delivered; a longer one is refused whole.
pub const PASTE_LIMIT: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    Key(KeyPress),
    Paste(String),
    /// A paste longer than `PASTE_LIMIT`, dropped whole.
    PasteTooLong,
    FocusIn,
    FocusOut,
}

#[derive(Default)]
pub struct Decoder {
    /// Bytes of an incomplete sequence or character.
    pending: Vec<u8>,
    /// Inside a bracketed paste: its bytes so far, capped one past the limit.
    paste: Option<Vec<u8>>,
    /// The paste's end marker, as far as it has arrived.
    marker: usize,
}

const PASTE_END: &[u8] = b"\x1b[201~";

enum Step {
    /// Consumed this many bytes, producing an input or nothing.
    Done(usize, Option<Input>),
    /// The pending bytes are a prefix of something longer.
    Incomplete,
}

impl Decoder {
    /// Whether decoding is waiting on a timeout: a lone Escape or an
    /// incomplete sequence, outside a paste.
    pub fn waiting(&self) -> bool {
        self.paste.is_none() && !self.pending.is_empty()
    }

    pub fn bytes(&mut self, bytes: &[u8], out: &mut Vec<Input>) {
        for &byte in bytes {
            if let Some(paste) = &mut self.paste {
                // Match the end marker incrementally; a partial marker that
                // breaks off is paste text after all.
                let expected = PASTE_END.get(self.marker).copied();
                if Some(byte) == expected {
                    // `expected` is there, so the marker is short of its end.
                    self.marker = self.marker.saturating_add(1);
                    if self.marker == PASTE_END.len() {
                        self.marker = 0;
                        let text = std::mem::take(paste);
                        self.paste = None;
                        out.push(if text.len() > PASTE_LIMIT {
                            Input::PasteTooLong
                        } else {
                            Input::Paste(String::from_utf8_lossy(&text).into_owned())
                        });
                    }
                    continue;
                }
                let held = PASTE_END.get(..self.marker).unwrap_or_default();
                // An Escape can begin the marker again, so it is held too.
                let text = if byte == 0x1b { None } else { Some(&byte) };
                for &b in held.iter().chain(text) {
                    if paste.len() <= PASTE_LIMIT {
                        paste.push(b);
                    }
                }
                self.marker = usize::from(byte == 0x1b);
                continue;
            }
            self.pending.push(byte);
            self.drain(out, false);
        }
    }

    /// The Escape deadline passed: whatever is pending is complete as it is.
    pub fn timeout(&mut self, out: &mut Vec<Input>) {
        if self.paste.is_none() {
            self.drain(out, true);
        }
    }

    fn drain(&mut self, out: &mut Vec<Input>, flush: bool) {
        while !self.pending.is_empty() && self.paste.is_none() {
            match decode(&self.pending, flush) {
                Step::Done(n, input) => {
                    // Nearly always the whole sequence; else what follows it.
                    if n >= self.pending.len() {
                        self.pending.clear();
                    } else {
                        self.pending = self.pending.get(n..).unwrap_or_default().to_vec();
                    }
                    if input.as_ref() == Some(&Input::Paste(String::new())) {
                        // The start marker: switch to paste mode.
                        self.paste = Some(Vec::new());
                        // Bytes after the marker in this batch are paste text.
                        let rest = std::mem::take(&mut self.pending);
                        self.bytes(&rest, out);
                        return;
                    }
                    if let Some(input) = input {
                        out.push(input);
                    }
                }
                Step::Incomplete => return,
            }
        }
    }
}

fn press(key: Key, mods: Modifiers) -> Option<Input> {
    Some(Input::Key(KeyPress::new(key, mods)))
}

/// Modifiers from an xterm modifier parameter (`1 + bits`).
fn modifiers(param: u32) -> Modifiers {
    let bits = param.saturating_sub(1);
    Modifiers {
        shift: bits & 1 != 0,
        alt: bits & 2 != 0,
        ctrl: bits & 4 != 0,
    }
}

/// One control byte or character, without an Escape prefix.
fn single(bytes: &[u8], flush: bool) -> Step {
    let Some(&first) = bytes.first() else {
        return Step::Incomplete;
    };
    let ctrl = Modifiers {
        ctrl: true,
        ..Modifiers::NONE
    };
    let input = match first {
        0x0d => press(Key::Enter, Modifiers::NONE),
        0x09 => press(Key::Tab, Modifiers::NONE),
        0x7f => press(Key::Backspace, Modifiers::NONE),
        0x1b => press(Key::Escape, Modifiers::NONE),
        0x00 => press(Key::Char(' '), ctrl),
        // Ctrl-A to Ctrl-Z are the letters with bits 5 and 6 cleared.
        0x01..=0x1a => press(Key::Char(char::from(first | 0x60)), ctrl),
        0x1c => press(Key::Char('\\'), ctrl),
        0x1d => press(Key::Char(']'), ctrl),
        0x1e => press(Key::Char('^'), ctrl),
        0x1f => press(Key::Char('_'), ctrl),
        0x20..=0x7e => press(Key::Char(char::from(first)), Modifiers::NONE),
        _ => {
            let need = match first {
                0xc0..=0xdf => 2,
                0xe0..=0xef => 3,
                0xf0..=0xf7 => 4,
                _ => 1,
            };
            if bytes.len() < need && !flush {
                // Stop at a byte that cannot continue the character.
                let broken = bytes.iter().skip(1).any(|b| b & 0xc0 != 0x80);
                if !broken {
                    return Step::Incomplete;
                }
            }
            let take = need.min(bytes.len());
            let text = bytes.get(..take).unwrap_or_default();
            return match std::str::from_utf8(text)
                .ok()
                .and_then(|s| s.chars().next())
            {
                Some(c) => Step::Done(take, press(Key::Char(c), Modifiers::NONE)),
                None => Step::Done(1, press(Key::Char('\u{fffd}'), Modifiers::NONE)),
            };
        }
    };
    Step::Done(1, input)
}

fn decode(bytes: &[u8], flush: bool) -> Step {
    let Some(&first) = bytes.first() else {
        return Step::Incomplete;
    };
    if first != 0x1b {
        return single(bytes, flush);
    }
    let Some(&second) = bytes.get(1) else {
        return if flush {
            Step::Done(1, press(Key::Escape, Modifiers::NONE))
        } else {
            Step::Incomplete
        };
    };
    match second {
        b'[' => csi(bytes, flush),
        b'O' => match bytes.get(2) {
            None if !flush => Step::Incomplete,
            None => Step::Done(2, press(Key::Char('O'), alt())),
            Some(&last) => Step::Done(3, ss3(last, Modifiers::NONE)),
        },
        // Escape Escape: an Escape, then decode the second one on its own.
        0x1b => Step::Done(1, press(Key::Escape, Modifiers::NONE)),
        _ => match single(bytes.get(1..).unwrap_or_default(), flush) {
            // The Escape and what followed it; within `bytes`, so exact.
            Step::Done(n, Some(Input::Key(KeyPress { key, mods }))) => {
                let mods = Modifiers { alt: true, ..mods };
                Step::Done(n.saturating_add(1), press(key, mods))
            }
            Step::Done(n, other) => Step::Done(n.saturating_add(1), other),
            Step::Incomplete => Step::Incomplete,
        },
    }
}

fn alt() -> Modifiers {
    Modifiers {
        alt: true,
        ..Modifiers::NONE
    }
}

fn ss3(last: u8, mods: Modifiers) -> Option<Input> {
    let key = match last {
        b'A' => Key::Arrow(Direction::Up),
        b'B' => Key::Arrow(Direction::Down),
        b'C' => Key::Arrow(Direction::Right),
        b'D' => Key::Arrow(Direction::Left),
        b'H' => Key::Home,
        b'F' => Key::End,
        b'P' => Key::F(1),
        b'Q' => Key::F(2),
        b'R' => Key::F(3),
        b'S' => Key::F(4),
        b'M' => Key::Enter,
        _ => return None,
    };
    press(key, mods)
}

/// A CSI sequence: `ESC [ params final`. Too long a sequence is dropped.
fn csi(bytes: &[u8], flush: bool) -> Step {
    let body = bytes.get(2..).unwrap_or_default();
    // Legacy mouse: `ESC [ M` and three raw bytes.
    if body.first() == Some(&b'M') {
        return if body.len() >= 4 {
            Step::Done(6, None)
        } else if flush {
            Step::Done(bytes.len(), None)
        } else {
            Step::Incomplete
        };
    }
    let Some(end) = body.iter().position(|b| (0x40..=0x7e).contains(b)) else {
        if body.len() > 32 || flush {
            // Not a sequence fux can use: an Escape and `[` were typed, or it
            // is garbage. At a timeout, treat it as Alt-[ and the rest.
            return if body.len() > 32 {
                Step::Done(bytes.len(), None)
            } else {
                Step::Done(2, press(Key::Char('['), alt()))
            };
        }
        return Step::Incomplete;
    };
    // `ESC [`, the parameters and the final byte; within `bytes`, so exact.
    let consumed = end.saturating_add(3);
    let params = body.get(..end).unwrap_or_default();
    let last = body.get(end).copied().unwrap_or(0);
    if params.first() == Some(&b'<') || params.first() == Some(&b'?') {
        // SGR mouse reports and private replies are dropped.
        return Step::Done(consumed, None);
    }
    let numbers: Vec<u32> = std::str::from_utf8(params)
        .unwrap_or("")
        .split(';')
        .map(|p| p.split(':').next().unwrap_or("").parse().unwrap_or(0))
        .collect();
    let first = numbers.first().copied().unwrap_or(0);
    let mods = modifiers(numbers.get(1).copied().unwrap_or(1));
    // `CSI n ~` numbers the function keys with gaps: F1 is `first - base`.
    let function = |base: u32| {
        let n = u8::try_from(first.checked_sub(base)?).ok()?;
        press(Key::F(n), mods)
    };
    let input = match last {
        b'A' | b'B' | b'C' | b'D' | b'H' | b'F' | b'P' | b'Q' | b'R' | b'S' => ss3(last, mods),
        b'Z' => press(
            Key::Tab,
            Modifiers {
                shift: true,
                ..mods
            },
        ),
        b'I' => Some(Input::FocusIn),
        b'O' => Some(Input::FocusOut),
        b'~' => match first {
            200 => Some(Input::Paste(String::new())),
            1 | 7 => press(Key::Home, mods),
            2 => press(Key::Insert, mods),
            3 => press(Key::Delete, mods),
            4 | 8 => press(Key::End, mods),
            5 => press(Key::PageUp, mods),
            6 => press(Key::PageDown, mods),
            11..=15 => function(10),
            17..=21 => function(11),
            23 | 24 => function(12),
            // xterm modifyOtherKeys: `CSI 27 ; mod ; code ~`.
            27 => numbers
                .get(2)
                .and_then(|code| char::from_u32(*code))
                .and_then(|c| press(Key::Char(c), mods)),
            _ => None,
        },
        // `CSI code ; mod u`.
        b'u' => char::from_u32(first).and_then(|c| match c {
            '\r' => press(Key::Enter, mods),
            '\t' => press(Key::Tab, mods),
            '\x1b' => press(Key::Escape, mods),
            '\x7f' => press(Key::Backspace, mods),
            c => press(Key::Char(c), mods),
        }),
        _ => None,
    };
    Step::Done(consumed, input)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(bytes: &[u8]) -> Vec<Input> {
        let mut d = Decoder::default();
        let mut out = Vec::new();
        d.bytes(bytes, &mut out);
        d.timeout(&mut out);
        out
    }
    fn key(name: &str) -> Input {
        Input::Key(name.parse().unwrap_or(KeyPress::char('?')))
    }

    #[test]
    fn every_key_sequence_decodes() {
        for (bytes, name) in [
            (&b"a"[..], "a"),
            (b"A", "A"),
            (b"\r", "Enter"),
            (b"\t", "Tab"),
            (b"\x7f", "BSpace"),
            (b"\x02", "C-b"),
            (b"\x00", "C-Space"),
            (b"\x1c", "C-\\"),
            (b"\x1b[A", "Up"),
            (b"\x1b[B", "Down"),
            (b"\x1b[C", "Right"),
            (b"\x1b[D", "Left"),
            (b"\x1bOA", "Up"),
            (b"\x1b[1;5D", "C-Left"),
            (b"\x1b[1;3A", "M-Up"),
            (b"\x1b[1;2B", "S-Down"),
            (b"\x1b[H", "Home"),
            (b"\x1b[F", "End"),
            (b"\x1b[1~", "Home"),
            (b"\x1b[4~", "End"),
            (b"\x1b[2~", "Insert"),
            (b"\x1b[3~", "Delete"),
            (b"\x1b[5~", "PageUp"),
            (b"\x1b[6~", "PageDown"),
            (b"\x1b[5;5~", "C-PageUp"),
            (b"\x1bOP", "F1"),
            (b"\x1b[1;2S", "S-F4"),
            (b"\x1b[15~", "F5"),
            (b"\x1b[17~", "F6"),
            (b"\x1b[21~", "F10"),
            (b"\x1b[23~", "F11"),
            (b"\x1b[24~", "F12"),
            (b"\x1b[Z", "BTab"),
            (b"\x1bx", "M-x"),
            (b"\x1b\x7f", "M-BSpace"),
            (b"\x1b\x02", "C-M-b"),
            (b"\xe7\x95\x8c", "\u{754c}"),
            (b"\x1b[27;5;105~", "C-i"),
            (b"\x1b[105;5u", "C-i"),
        ] {
            assert_eq!(all(bytes), vec![key(name)], "{bytes:?}");
        }
    }

    #[test]
    fn escape_waits_for_the_deadline() {
        let mut d = Decoder::default();
        let mut out = Vec::new();
        d.bytes(b"\x1b", &mut out);
        assert!(out.is_empty());
        assert!(d.waiting());
        d.timeout(&mut out);
        assert_eq!(out, vec![key("Escape")]);
        assert!(!d.waiting());
        // Escape followed in time by `[A` is an arrow, not Escape.
        let mut out = Vec::new();
        d.bytes(b"\x1b", &mut out);
        d.bytes(b"[", &mut out);
        d.bytes(b"A", &mut out);
        assert_eq!(out, vec![key("Up")]);
        // Two Escapes are two Escapes.
        assert_eq!(all(b"\x1b\x1b"), vec![key("Escape"), key("Escape")]);
    }

    #[test]
    fn a_sequence_split_at_every_byte_decodes_the_same() {
        let stream: &[u8] = b"ab\x1b[1;5Cx\xe7\x95\x8c\x1b[200~p\x1bq\x1b[201~\x1b[I\x1b[15~z";
        let whole = all(stream);
        assert_eq!(
            whole,
            vec![
                key("a"),
                key("b"),
                key("C-Right"),
                key("x"),
                key("\u{754c}"),
                Input::Paste("p\x1bq".into()),
                Input::FocusIn,
                key("F5"),
                key("z"),
            ]
        );
        for split in 1..stream.len() {
            let (a, b) = stream.split_at_checked(split).unwrap_or((stream, &[]));
            let mut d = Decoder::default();
            let mut out = Vec::new();
            d.bytes(a, &mut out);
            d.bytes(b, &mut out);
            d.timeout(&mut out);
            assert_eq!(out, whole, "split at {split}");
        }
    }

    #[test]
    fn mouse_reports_are_dropped_and_pastes_are_bounded() {
        assert_eq!(all(b"\x1b[M !!a"), vec![key("a")]);
        assert_eq!(all(b"\x1b[<0;10;5Ma"), vec![key("a")]);
        let mut long = b"\x1b[200~".to_vec();
        long.extend(std::iter::repeat_n(b'x', PASTE_LIMIT + 10));
        long.extend_from_slice(b"\x1b[201~k");
        assert_eq!(all(&long), vec![Input::PasteTooLong, key("k")]);
        let mut exact = b"\x1b[200~".to_vec();
        exact.extend(std::iter::repeat_n(b'x', PASTE_LIMIT));
        exact.extend_from_slice(b"\x1b[201~");
        assert!(matches!(all(&exact).first(), Some(Input::Paste(t)) if t.len() == PASTE_LIMIT));
        // A paste never becomes keys, even with a command key inside.
        assert_eq!(
            all(b"\x1b[200~\x02d\x1b[201~"),
            vec![Input::Paste("\x02d".into())]
        );
    }
}
