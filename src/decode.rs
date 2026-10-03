//! Decoding a client's raw terminal input into keys, pastes, focus changes
//! and the terminal's answers to fux's questions (`outer`). The client is a
//! dumb pipe; this is the only decoder.
//!
//! The outer terminal is in normal (not application) cursor and keypad mode,
//! so each key has one xterm encoding. A lone Escape is only known to be one
//! when no more bytes follow within `ESCAPE_DELAY`; the server calls
//! `timeout` at the `deadline` the decoder reports. Mouse sequences, which a
//! correctly configured outer terminal never sends, are dropped.
//!
//! The terminal's answers are told from keys by their form, which no key
//! has: a CSI with `?` (`CSI ? 997 ; 1 n`, `CSI ? 2031 ; 2 $ y`, DA1), or an
//! OSC string (`OSC 11 ; rgb:… ST`). An answer cut short waits
//! `REPLY_DELAY` for its end, as a slow link may split one, and is then
//! dropped, never typed. `ESC ]` with nothing after it waits, as `ESC [`
//! and `ESC O` do, `ESCAPE_DELAY`, or `REPLY_DELAY` while an answer is
//! expected (`Decoder::expect`), and is then Alt-]; with a digit after it,
//! it is an answer begun. How input is split never changes what it decodes
//! to; only how long a wait lasts.
use crate::bytes::ByteQueue;
use crate::keys::{Direction, Key, KeyPress, Modifiers};
use crate::outer::{Rgb, Scheme};
use std::time::{Duration, Instant};

/// How long a lone Escape waits for the rest of a sequence.
pub const ESCAPE_DELAY: Duration = Duration::from_millis(35);
/// How long an answer from the terminal that has begun waits for the rest
/// of it. Answers are not typed, so the wait delays no key; it only keeps a
/// split one from being taken for keys.
pub const REPLY_DELAY: Duration = Duration::from_secs(1);
/// How long after fux asks the terminal a bare `ESC ]` is taken for the
/// start of an answer, unless the answer to DA1, asked last, comes first.
pub const REPLY_WINDOW: Duration = Duration::from_secs(1);
/// The longest OSC answer kept: `OSC 11 ; rgb:RRRR/GGGG/BBBB ST` is 29
/// bytes. A longer string is dropped.
const OSC_LIMIT: usize = 128;
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
    /// An answer from the terminal, or a report it sends unasked.
    Reply(Reply),
}

/// What the terminal answers fux (`outer`), told from keys by its form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reply {
    /// `OSC 10 ; rgb:… ST` (foreground) or `OSC 11 ; …` (background).
    Colour { number: u8, rgb: Rgb },
    /// `CSI ? 997 ; 1 n` (dark) or `; 2 n` (light): the answer to
    /// `CSI ? 996 n`, or a report of a change.
    Scheme(Scheme),
    /// DECRQM's answer for a DEC private mode: `CSI ? mode ; status $ y`.
    Mode { mode: u16, status: u8 },
    /// The kitty keyboard protocol's flags: `CSI ? flags u`.
    KittyFlags(u8),
    /// Primary device attributes, `CSI ? … c`.
    Attributes,
}

#[derive(Default)]
pub struct Decoder {
    /// Bytes of an incomplete sequence or character.
    pending: ByteQueue,
    /// Inside a bracketed paste: its bytes so far, capped one past the limit.
    paste: Option<Vec<u8>>,
    /// The paste's end marker, as far as it has arrived.
    marker: usize,
    /// When decoding began waiting on a timeout, as `mark` found it.
    since: Option<Instant>,
    /// Rounds of questions asked of the terminal whose last answer, DA1's,
    /// has not come, and until when they are waited for.
    expected: u8,
    expected_until: Option<Instant>,
}

const PASTE_END: &[u8] = b"\x1b[201~";

enum Step {
    /// Consumed this many bytes, producing an input or nothing.
    Done(usize, Option<Input>),
    /// Consumed this many bytes, the start of a bracketed paste.
    PasteStart(usize),
    /// The pending bytes are a prefix of something longer.
    Incomplete,
}

impl Decoder {
    /// Whether decoding is waiting on a timeout: a lone Escape or an
    /// incomplete sequence, outside a paste.
    pub fn waiting(&self) -> bool {
        self.paste.is_none() && !self.pending.is_empty()
    }

    /// Records `now` as when decoding began waiting, if it is waiting and
    /// was not when last marked.
    pub fn mark(&mut self, now: Instant) {
        if self.waiting() && self.since.is_none() {
            self.since = Some(now);
        }
    }

    /// When `timeout` is due: `ESCAPE_DELAY` after decoding began waiting,
    /// as marked, or `REPLY_DELAY` if what waits is an answer begun, or a
    /// bare `ESC ]` while an answer is expected; none while it is not
    /// waiting.
    pub fn deadline(&self) -> Option<Instant> {
        let since = self.since.filter(|_| self.waiting())?;
        let pending = self.pending.as_slice();
        let reply = match pending {
            b"\x1b]" => self.expected > 0,
            _ => pending.starts_with(b"\x1b]") || pending.starts_with(b"\x1b[?"),
        };
        Some(crate::after(
            since,
            if reply { REPLY_DELAY } else { ESCAPE_DELAY },
        ))
    }

    /// Fux has asked the terminal questions, ending with DA1, at `now`: until
    /// DA1's answer comes, or `REPLY_WINDOW` passes, a bare `ESC ]` waits as
    /// long as an answer begun.
    pub fn expect(&mut self, now: Instant) {
        self.expected = self.expected.saturating_add(1);
        self.expected_until = Some(crate::after(now, REPLY_WINDOW));
    }

    /// Forgets the answers expected if their window has passed by `now`.
    pub fn expire(&mut self, now: Instant) {
        if self.expected_until.is_some_and(|until| now >= until) {
            self.expected = 0;
            self.expected_until = None;
        }
    }

    pub fn bytes(&mut self, bytes: &[u8], out: &mut Vec<Input>) {
        self.feed(bytes, out);
        // Waiting that stops and starts again within these bytes goes on.
        if !self.waiting() {
            self.since = None;
        }
    }

    fn feed(&mut self, bytes: &[u8], out: &mut Vec<Input>) {
        for (i, &byte) in bytes.iter().enumerate() {
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
                            // Moved as it is, unless it is not UTF-8.
                            Input::Paste(String::from_utf8(text).unwrap_or_else(|e| {
                                String::from_utf8_lossy(e.as_bytes()).into_owned()
                            }))
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
            // Outside a paste, the rest at once: a sequence is decoded with
            // what came with it, so `ESC ]` and a digit are an answer begun,
            // not Alt-] and a key. A paste it starts takes what follows.
            self.pending.push(bytes.get(i..).unwrap_or_default());
            self.drain(out, false);
            return;
        }
    }

    /// The Escape deadline passed: whatever is pending is complete as it is.
    pub fn timeout(&mut self, out: &mut Vec<Input>) {
        if self.paste.is_none() {
            self.drain(out, true);
        }
        if !self.waiting() {
            self.since = None;
        }
    }

    fn drain(&mut self, out: &mut Vec<Input>, flush: bool) {
        while !self.pending.is_empty() && self.paste.is_none() {
            match decode(self.pending.as_slice(), flush) {
                Step::Done(n, input) => {
                    // Nearly always the whole sequence; else what follows it
                    // stays, without moving.
                    self.pending.take(n);
                    if input == Some(Input::Reply(Reply::Attributes)) {
                        self.expected = self.expected.saturating_sub(1);
                    }
                    if let Some(input) = input {
                        out.push(input);
                    }
                }
                Step::PasteStart(n) => {
                    self.pending.take(n);
                    self.paste = Some(Vec::new());
                    // Bytes after the marker in this batch are paste text.
                    let rest = std::mem::take(&mut self.pending);
                    self.feed(rest.as_slice(), out);
                    return;
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

/// One input from the start of `bytes`; `flush` once the deadline has
/// passed.
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
        b']' => osc(bytes, flush),
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
            Step::PasteStart(n) => Step::PasteStart(n.saturating_add(1)),
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

/// `ESC ]`: an OSC string, an answer from the terminal, if a digit follows
/// it (no key sends one); else Alt-], once the deadline has passed if
/// nothing follows yet. The string ends with BEL or ST (`ESC \`); a colour
/// answer becomes a `Reply`, and anything else, or anything cut short at
/// the deadline or past `OSC_LIMIT`, is dropped.
fn osc(bytes: &[u8], flush: bool) -> Step {
    let alt_bracket = || Step::Done(2, press(Key::Char(']'), alt()));
    let body = bytes.get(2..).unwrap_or_default();
    match body.first() {
        None if !flush => return Step::Incomplete,
        Some(first) if first.is_ascii_digit() => {}
        _ => return alt_bracket(),
    }
    let end = body.iter().position(|b| matches!(b, 0x07 | 0x1b));
    let (payload, consumed) = match end.map(|i| (i, body.get(i).copied())) {
        Some((i, Some(0x07))) => (body.get(..i), i.checked_add(3)),
        Some((i, _)) => match body.get(i.saturating_add(1)) {
            Some(b'\\') => (body.get(..i), i.checked_add(4)),
            // ESC and something else: the string ends there, unfinished,
            // and the ESC begins what comes next.
            Some(_) => return Step::Done(i.saturating_add(2), None),
            None if flush => return Step::Done(bytes.len(), None),
            None => return Step::Incomplete,
        },
        // Longer than any answer, or cut short: dropped.
        None if flush || body.len() > OSC_LIMIT => return Step::Done(bytes.len(), None),
        None => return Step::Incomplete,
    };
    // Within `bytes`: the string, its two-byte opening and its end.
    let consumed = consumed.unwrap_or(bytes.len()).min(bytes.len());
    let reply = payload.and_then(colour_reply).map(Input::Reply);
    Step::Done(consumed, reply)
}

/// `10 ; rgb:…` or `11 ; rgb:…`: the terminal's foreground or background.
fn colour_reply(payload: &[u8]) -> Option<Reply> {
    let split = payload.iter().position(|b| *b == b';')?;
    let (number, spec) = payload.split_at_checked(split)?;
    let number = match number {
        b"10" => 10,
        b"11" => 11,
        _ => return None,
    };
    let rgb = Rgb::parse(spec.get(1..)?)?;
    Some(Reply::Colour { number, rgb })
}

/// A CSI answer from the terminal, `params` from its `?` on, ending with
/// `last`.
fn csi_reply(params: &[u8], last: u8) -> Option<Reply> {
    let text = std::str::from_utf8(params.get(1..)?).ok()?;
    match last {
        b'c' => Some(Reply::Attributes),
        b'u' => {
            let flags = text.parse::<u32>().ok()?;
            Some(Reply::KittyFlags(u8::try_from(flags).unwrap_or(u8::MAX)))
        }
        b'n' => match text {
            "997;1" => Some(Reply::Scheme(Scheme::Dark)),
            "997;2" => Some(Reply::Scheme(Scheme::Light)),
            _ => None,
        },
        b'y' => {
            let (mode, status) = text.strip_suffix('$')?.split_once(';')?;
            Some(Reply::Mode {
                mode: mode.parse().ok()?,
                status: status.parse().ok()?,
            })
        }
        _ => None,
    }
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
    // An answer from the terminal (`?`) may be longer than any key's.
    let answer = body.first() == Some(&b'?');
    let limit = if answer { 64 } else { 32 };
    // A sequence with no final byte within the limit is dropped as far as
    // one byte past it, whatever follows.
    let window = body.get(..=limit).unwrap_or(body);
    let Some(end) = window.iter().position(|b| (0x40..=0x7e).contains(b)) else {
        if window.len() > limit {
            // Not a sequence fux can use: garbage.
            return Step::Done(window.len().saturating_add(2), None);
        }
        if !flush {
            return Step::Incomplete;
        }
        // At a timeout, an answer begun is dropped, never typed; anything
        // else was an Escape and `[` typed: Alt-[ and the rest.
        return if answer {
            Step::Done(bytes.len(), None)
        } else {
            Step::Done(2, press(Key::Char('['), alt()))
        };
    };
    // `ESC [`, the parameters and the final byte; within `bytes`, so exact.
    let consumed = end.saturating_add(3);
    let params = body.get(..end).unwrap_or_default();
    let last = body.get(end).copied().unwrap_or(0);
    if params.first() == Some(&b'<') {
        // SGR mouse reports are dropped.
        return Step::Done(consumed, None);
    }
    if params.first() == Some(&b'?') {
        // Answers fux does not use are dropped.
        return Step::Done(consumed, csi_reply(params, last).map(Input::Reply));
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
            200 => return Step::PasteStart(consumed),
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
    use crate::outer::{Rgb, Scheme};

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

    /// The deadline runs from when decoding began waiting, through bytes
    /// that leave it waiting, even across a paste; a wait that ends and
    /// begins again runs from the new start.
    #[test]
    fn the_deadline_runs_from_when_waiting_began() {
        let t0 = Instant::now();
        let ms = Duration::from_millis;
        let mut d = Decoder::default();
        let mut out = Vec::new();
        d.mark(t0);
        assert_eq!(d.deadline(), None, "not waiting");
        d.bytes(b"\x1b", &mut out);
        d.mark(t0);
        assert_eq!(d.deadline(), Some(t0 + ESCAPE_DELAY));
        d.bytes(b"[", &mut out);
        d.mark(t0 + ms(10));
        assert_eq!(
            d.deadline(),
            Some(t0 + ESCAPE_DELAY),
            "still the first wait"
        );
        d.bytes(b"200~pasted\x1b[201~\x1b", &mut out);
        d.mark(t0 + ms(20));
        assert_eq!(d.deadline(), Some(t0 + ESCAPE_DELAY), "through a paste");
        d.timeout(&mut out);
        assert_eq!(d.deadline(), None);
        d.bytes(b"\x1b", &mut out);
        d.mark(t0 + ms(30));
        assert_eq!(d.deadline(), Some(t0 + ms(30) + ESCAPE_DELAY));
        d.bytes(b"a", &mut out);
        d.bytes(b"\x1b", &mut out);
        d.mark(t0 + ms(40));
        assert_eq!(d.deadline(), Some(t0 + ms(40) + ESCAPE_DELAY), "a new wait");
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

    fn colour(number: u8, r: u16, g: u16, b: u16) -> Input {
        Input::Reply(Reply::Colour {
            number,
            rgb: Rgb { r, g, b },
        })
    }

    /// Each answer fux asks for (`outer`), in the forms terminals give
    /// (ctlseqs: OSC 10/11 answered `rgb:RRRR/GGGG/BBBB`, ended as asked;
    /// DECRQM `CSI ? Ps ; Pm $ y`; DA1 `CSI ? … c`; the kitty flags
    /// `CSI ? flags u`; contour's `CSI ? 997 ; 1|2 n`), among keys, whole
    /// and split at every byte, with an answer expected or not.
    #[test]
    fn answers_are_told_from_keys_however_they_arrive() {
        let stream: &[u8] = b"a\x1b]10;rgb:ffff/ffff/ffff\x1b\\b\x1b]11;rgb:1e/1e/20\x07\
            \x1b[?2031;2$y\x1b[?997;1nc\x1b[?997;2n\x1b[?5u\x1b[?62;22;52c\x1b[Ad";
        let expected = vec![
            key("a"),
            colour(10, 0xffff, 0xffff, 0xffff),
            key("b"),
            colour(11, 0x1e1e, 0x1e1e, 0x2020),
            Input::Reply(Reply::Mode {
                mode: 2031,
                status: 2,
            }),
            Input::Reply(Reply::Scheme(Scheme::Dark)),
            key("c"),
            Input::Reply(Reply::Scheme(Scheme::Light)),
            Input::Reply(Reply::KittyFlags(5)),
            Input::Reply(Reply::Attributes),
            key("Up"),
            key("d"),
        ];
        assert_eq!(all(stream), expected);
        for expecting in [false, true] {
            for split in 1..stream.len() {
                let (a, b) = stream.split_at_checked(split).unwrap_or((stream, &[]));
                let mut d = Decoder::default();
                if expecting {
                    d.expect(Instant::now());
                }
                let mut out = Vec::new();
                d.bytes(a, &mut out);
                d.bytes(b, &mut out);
                d.timeout(&mut out);
                assert_eq!(out, expected, "split at {split}, expecting {expecting}");
            }
        }
        // Colours fux does not ask for or cannot read are dropped; answers
        // it does not use reach the session, which ignores them: no keys.
        assert_eq!(
            all(b"\x1b[?1;2c\x1b]12;rgb:0/0/0\x07\x1b]11;#000\x07\x1b[?6;1$yx"),
            vec![
                Input::Reply(Reply::Attributes),
                Input::Reply(Reply::Mode { mode: 6, status: 1 }),
                key("x")
            ]
        );
    }

    /// An answer cut short waits `REPLY_DELAY`, not `ESCAPE_DELAY`, for the
    /// rest, and at the deadline is dropped, never typed; a lone Escape
    /// still waits `ESCAPE_DELAY` and is a key.
    #[test]
    fn an_answer_cut_short_is_dropped_not_typed() {
        let t0 = Instant::now();
        for part in [&b"\x1b]11;rgb:1e1e/"[..], b"\x1b[?99", b"\x1b[?", b"\x1b]1"] {
            let mut d = Decoder::default();
            let mut out = Vec::new();
            d.bytes(part, &mut out);
            d.mark(t0);
            assert_eq!(d.deadline(), Some(t0 + REPLY_DELAY), "{part:?}");
            d.timeout(&mut out);
            assert!(out.is_empty(), "{part:?}: {out:?}");
            d.bytes(b"k", &mut out);
            assert_eq!(out, vec![key("k")]);
        }
        let mut d = Decoder::default();
        let mut out = Vec::new();
        d.bytes(b"\x1b", &mut out);
        d.mark(t0);
        assert_eq!(d.deadline(), Some(t0 + ESCAPE_DELAY));
        // An OSC string longer than any answer is dropped as it arrives.
        let mut long = b"\x1b]11;".to_vec();
        long.extend(std::iter::repeat_n(b'x', 200));
        long.extend_from_slice(b"\x07y");
        assert_eq!(all(&long), vec![key("y")]);
    }

    /// `ESC ]` alone is Alt-] once `ESCAPE_DELAY` passes, as `ESC [` is
    /// Alt-[, or `REPLY_DELAY` while an answer is expected; with anything but
    /// a digit after it, at once. The answer to DA1, asked last, ends the
    /// expecting; so does `REPLY_WINDOW` passing.
    #[test]
    fn escape_bracket_waits_longer_while_an_answer_is_expected() {
        let alt_bracket = key("M-]");
        assert_eq!(all(b"\x1b]"), vec![alt_bracket.clone()]);
        assert_eq!(all(b"\x1b]x"), vec![alt_bracket.clone(), key("x")]);
        let t0 = Instant::now();
        let waits = |d: &mut Decoder, bytes: &[u8], out: &mut Vec<Input>| {
            d.bytes(bytes, out);
            d.mark(t0);
            d.deadline().map(|at| at.duration_since(t0))
        };
        let mut d = Decoder::default();
        let mut out = Vec::new();
        assert_eq!(waits(&mut d, b"\x1b]", &mut out), Some(ESCAPE_DELAY));
        d.timeout(&mut out);
        assert_eq!(out, vec![alt_bracket.clone()]);
        let mut d = Decoder::default();
        d.expect(t0);
        let mut out = Vec::new();
        assert_eq!(waits(&mut d, b"\x1b]", &mut out), Some(REPLY_DELAY));
        d.timeout(&mut out);
        assert_eq!(out, vec![alt_bracket.clone()]);
        // DA1's answer: no more answers to wait for.
        out.clear();
        assert_eq!(
            waits(&mut d, b"\x1b[?1;2c\x1b]", &mut out),
            Some(ESCAPE_DELAY)
        );
        assert_eq!(out, vec![Input::Reply(Reply::Attributes)]);
        let mut d = Decoder::default();
        d.expect(t0);
        d.expire(t0 + REPLY_WINDOW);
        assert_eq!(waits(&mut d, b"\x1b]", &mut out), Some(ESCAPE_DELAY));
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
