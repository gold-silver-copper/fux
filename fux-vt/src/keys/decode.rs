//! Decoding a client's raw terminal input into keys, pastes, focus changes
//! and the terminal's answers to fux's questions (`outer`), as the server
//! reads it from the terminal or the client relays it; this is the only
//! decoder.
//!
//! The outer terminal is put in normal (not application) cursor and keypad
//! mode, so most keys arrive in one xterm encoding; the others a terminal
//! may send (SS3, `CSI 1 ~` and `CSI 7 ~`, modifyOtherKeys, the kitty
//! protocol) decode too. A lone Escape is only known to be one
//! when no more bytes follow within `ESCAPE_DELAY`; the server calls
//! `timeout` at the `deadline` the decoder reports. Mouse reports, in SGR
//! or the default encoding, decode to [`MouseEvent`]s; a host that asked
//! for none drops them.
//!
//! A terminal that speaks the kitty keyboard protocol has disambiguate and
//! alternate keys pushed (`outer`): it sends Escape, and keys with Ctrl or
//! Alt, as `CSI code:shifted:base ; mods u`, and its functional keys in
//! their kitty forms. Each decodes to the `KeyPress` a legacy terminal's
//! bytes for the same key decode to, wherever legacy bytes can tell the key
//! (Alt-Shift-1 is `M-!`, from the shifted key; Ctrl with a key off ASCII
//! is the base-layout key, as legacy terminals send it), so bindings match
//! alike; keys legacy bytes cannot tell apart stay apart (Shift-Enter,
//! Ctrl-I and Tab, Ctrl-[ and Escape). What the press leaves out (the
//! shifted and base-layout keys, Super, Hyper, Meta, the locks) the
//! keystroke keeps, for a pane in the protocol.
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
//!
//! A DCS answer (DECRPSS, `DCS 1 $ r … ST`, and XTGETTCAP's, `DCS 1 + r …
//! ST`) begins `ESC P`, as Alt-P does, so it is one only while an answer is
//! expected, and once it has begun as fux's answers do, `ESC P`, `0` or
//! `1`, then `$` or `+`. While one is expected, `ESC P` and `ESC P 1` wait
//! for the rest, as `ESC ]` does; otherwise `ESC P` is Alt-P at once,
//! costing no wait, however the bytes are split. fux's questions
//! are always expected (`Decoder::expect`), and a terminal answers them
//! before DA1, whose answer ends the expecting.
//!
//! An answer's string longer than any answer (`OSC_LIMIT`, `DCS_LIMIT`) is
//! dropped to its end as it arrives, none of it typed.
use crate::Rgb;
use crate::keys::colour::Scheme;
use crate::keys::encode::PASTE_END;
use crate::keys::mouse::{self, MouseEvent};
use crate::keys::{Direction, Key, KeyPress, Keystroke, Kitty, Modifiers};
use std::borrow::Cow;
use std::num::NonZeroU8;
use std::time::{Duration, Instant};

/// How long a lone Escape waits for the rest of a sequence.
pub const ESCAPE_DELAY: Duration = Duration::from_millis(35);
/// How long an answer from the terminal that has begun waits for the rest
/// of it. Answers are not typed, so the wait delays no key; it only keeps a
/// split one from being taken for keys.
const REPLY_DELAY: Duration = Duration::from_secs(1);
/// How long after fux asks the terminal a bare `ESC ]`, `ESC P`, `ESC P0`
/// or `ESC P1` waits as long as an answer begun (`REPLY_DELAY`), unless the
/// answer to DA1, asked last, comes first.
const REPLY_WINDOW: Duration = Duration::from_secs(1);
/// The longest OSC answer kept: `OSC 11 ; rgb:RRRR/GGGG/BBBB ST` is 29
/// bytes. A longer string is dropped.
const OSC_LIMIT: usize = 128;
/// The longest DCS answer kept: a DECRPSS of the longest pen,
/// `DCS 1 $ r 0;1;4:3;6;7;8;2;3;9;38:2::255:255:255;48:2::…;58:2::… m ST`,
/// is under 90 bytes, and XTGETTCAP's answer for `Smulx` under 50.
const DCS_LIMIT: usize = 256;
/// The largest paste delivered; a longer one is refused whole.
pub const PASTE_LIMIT: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
/// What a terminal's input decodes to.
pub enum Input {
    /// A key.
    Key(Keystroke),
    /// A bracketed paste, its text whole.
    Paste(String),
    /// A paste longer than the decoder's limit ([`PASTE_LIMIT`] unless
    /// [`Decoder::with_paste_limit`] set another), dropped whole.
    PasteTooLong,
    /// The terminal gained focus (`CSI I`).
    FocusIn,
    /// The terminal lost focus (`CSI O`).
    FocusOut,
    /// A mouse report, in SGR (`CSI < code ; column ; row M` or `m`) or the
    /// default encoding (`CSI M` and three bytes).
    Mouse(MouseEvent),
    /// An answer from the terminal, or a report it sends unasked.
    Reply(Reply),
}

/// What the terminal answers fux (`outer`), told from keys by its form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reply {
    /// `OSC 10 ; rgb:… ST`: the foreground.
    Foreground(Rgb),
    /// `OSC 11 ; rgb:… ST`: the background.
    Background(Rgb),
    /// `OSC 4 ; index ; rgb:… ST`: a palette entry.
    Palette {
        /// The entry, 0 to 255.
        index: u8,
        /// Its colour.
        rgb: Rgb,
    },
    /// `CSI ? 997 ; 1 n` (dark) or `; 2 n` (light): the answer to
    /// `CSI ? 996 n`, or a report of a change.
    Scheme(Scheme),
    /// DECRQM's answer for a DEC private mode: `CSI ? mode ; status $ y`.
    Mode {
        /// The mode asked about.
        mode: u16,
        /// DECRPM's status: 0 not recognised, 1 set, 2 reset, 3 permanently set, 4 permanently
        /// reset.
        status: u8,
    },
    /// The kitty keyboard protocol's flags: `CSI ? flags u`.
    KittyFlags(u8),
    /// Primary device attributes, `CSI ? … c`.
    Attributes,
    /// The terminal draws underline styles (`outer`): its terminfo has
    /// `Smulx`, as its XTGETTCAP answer says (`DCS 1 + r 536d756c78 = … ST`,
    /// `Smulx` in hex), or it kept the curly underline fux set, as its
    /// DECRPSS of the pen says (`DCS 1 $ r 0 ; 4:3 m ST`).
    UnderlineStyles,
}

/// Decodes a terminal's raw input, however it is split, into [`Input`]s.
pub struct Decoder {
    state: State,
    /// The longest paste kept.
    paste_limit: usize,
    /// Rounds of questions asked of the terminal whose last answer, DA1's,
    /// has not come.
    expecting: Option<Expecting>,
}

impl Default for Decoder {
    fn default() -> Decoder {
        Decoder::with_paste_limit(PASTE_LIMIT)
    }
}

/// What the decoder is in the middle of, with what only that has: the
/// bytes and the start of a wait, the paste so far.
#[derive(Default)]
enum State {
    /// Nothing begun.
    #[default]
    Idle,
    /// A sequence or character begun, its bytes so far, waiting for the
    /// rest or its deadline; since when, once marked.
    Pending {
        held: Vec<u8>,
        since: Option<Instant>,
    },
    /// An answer's string past its limit (OSC or DCS), dropped as it
    /// arrives until it ends (`drop_to_end`), as an answer begun waits.
    Dropping {
        bel: bool,
        escape: bool,
        since: Option<Instant>,
    },
    /// Inside a bracketed paste, which no deadline cuts short: its bytes
    /// so far.
    Paste(Vec<u8>),
}

/// Questions asked of the terminal, `rounds` of them, waited for until
/// `until` unless DA1's answer, asked last in each, comes first.
#[derive(Clone, Copy)]
struct Expecting {
    rounds: NonZeroU8,
    until: Instant,
}

/// Drops `bytes` of an answer's string past its limit to its end, `bel` if
/// BEL ends it too (OSC; a DCS ends with ST alone), `escape` if the last
/// read ended with an ESC that may begin its ST; the bytes after it, if it
/// ends within them, else whether they end with such an ESC. An Escape that
/// ends it unfinished begins what follows, put back if the last read ended
/// with it.
#[cold]
fn drop_to_end(bel: bool, mut escape: bool, bytes: &[u8]) -> Result<Cow<'_, [u8]>, bool> {
    for (i, &byte) in bytes.iter().enumerate() {
        let rest = match (escape, byte) {
            (true, b'\\') => i.saturating_add(1),
            (false, 0x07) if bel => i.saturating_add(1),
            // ESC and something else: the string ends there, unfinished,
            // and the ESC begins what comes next.
            (true, _) => match i.checked_sub(1) {
                Some(escape) => escape,
                None => return Ok(Cow::Owned([b"\x1b", bytes].concat())),
            },
            (false, byte) => {
                escape = byte == 0x1b;
                continue;
            }
        };
        return Ok(Cow::Borrowed(bytes.get(rest..).unwrap_or_default()));
    }
    Err(escape)
}

/// Takes `bytes` of a bracketed paste, `text` its bytes so far; once its
/// end marker is among them, the paste, if no longer than `limit`, to `out`,
/// and the bytes after it. Past the limit, only what may begin the marker
/// is kept beyond it.
fn paste<'a>(
    mut text: Vec<u8>,
    bytes: &'a [u8],
    limit: usize,
    out: &mut Vec<Input>,
) -> Result<&'a [u8], Vec<u8>> {
    let (old, marker) = (text.len(), PASTE_END.len());
    text.extend_from_slice(bytes);
    // The marker may have begun in the bytes before these.
    let from = old.saturating_sub(marker.saturating_sub(1));
    let found =
        (from..text.len()).find(|&i| text.get(i..).is_some_and(|t| t.starts_with(PASTE_END)));
    let Some(end) = found else {
        let tail = text.len().saturating_sub(marker.saturating_sub(1));
        if tail > limit.saturating_add(1) {
            let kept = text.get(tail..).unwrap_or_default().to_vec();
            text.truncate(limit.saturating_add(1));
            text.extend(kept);
        }
        return Err(text);
    };
    let rest = end.saturating_add(marker).saturating_sub(old);
    text.truncate(end);
    out.push(if text.len() > limit {
        Input::PasteTooLong
    } else {
        // Moved as it is, unless it is not UTF-8.
        Input::Paste(
            String::from_utf8(text)
                .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned()),
        )
    });
    Ok(bytes.get(rest..).unwrap_or_default())
}

enum Step {
    /// Consumed this many bytes, producing an input or nothing.
    Done(usize, Option<Input>),
    /// Consumed this many bytes, the start of a bracketed paste.
    PasteStart(usize),
    /// The bytes are an answer's string past its limit, all of it: the rest
    /// of it is dropped as it comes, to its end (BEL too if `bel`, or ST).
    Overlong { bel: bool },
    /// The bytes are a prefix of something longer.
    Incomplete,
}

impl Decoder {
    /// A decoder that keeps pastes of up to `limit` bytes, rather than
    /// [`PASTE_LIMIT`]: a host that sends a long paste on in pieces. It
    /// holds that much while a paste arrives.
    pub fn with_paste_limit(limit: usize) -> Decoder {
        Decoder {
            state: State::Idle,
            paste_limit: limit,
            expecting: None,
        }
    }

    /// Whether decoding is waiting on a timeout, outside a paste: a lone
    /// Escape, an incomplete sequence, or an over-long string being dropped.
    pub fn waiting(&self) -> bool {
        matches!(self.state, State::Pending { .. } | State::Dropping { .. })
    }

    /// Records `now` as when decoding began waiting, if it is waiting on a
    /// sequence it was not waiting on when last marked.
    pub fn mark(&mut self, now: Instant) {
        if let State::Pending { since, .. } | State::Dropping { since, .. } = &mut self.state {
            since.get_or_insert(now);
        }
    }

    /// When `timeout` is due: `ESCAPE_DELAY` after decoding began waiting,
    /// as marked, or `REPLY_DELAY` if what waits is an answer begun, or a
    /// bare `ESC ]`, `ESC P`, `ESC P0` or `ESC P1` while an answer is
    /// expected; none while it is not waiting.
    pub fn deadline(&self) -> Option<Instant> {
        let (since, reply) = match &self.state {
            State::Pending { held, since } => (
                since,
                match held.as_slice() {
                    b"\x1b]" | b"\x1bP" | b"\x1bP0" | b"\x1bP1" => self.expecting.is_some(),
                    held => {
                        held.starts_with(b"\x1b]") || held.starts_with(b"\x1b[?") || dcs_begun(held)
                    }
                },
            ),
            State::Dropping { since, .. } => (since, true),
            State::Idle | State::Paste(_) => return None,
        };
        let wait = if reply { REPLY_DELAY } else { ESCAPE_DELAY };
        Some(after((*since)?, wait))
    }

    /// Fux has asked the terminal questions, ending with DA1, at `now`: until
    /// DA1's answer comes, or `REPLY_WINDOW` passes, a bare `ESC ]` (or the
    /// start of a DCS answer) waits as long as an answer begun.
    pub fn expect(&mut self, now: Instant) {
        let rounds = self
            .expecting
            .map_or(NonZeroU8::MIN, |e| e.rounds.saturating_add(1));
        let until = after(now, REPLY_WINDOW);
        self.expecting = Some(Expecting { rounds, until });
    }

    /// Forgets the answers expected if their window has passed by `now`.
    /// A DCS answer already begun keeps them expected until it ends, so
    /// that it is read as one however its pieces fall in time.
    pub fn expire(&mut self, now: Instant) {
        let begun = matches!(&self.state, State::Pending { held, .. } if dcs_begun(held));
        if !begun && self.expecting.is_some_and(|e| now >= e.until) {
            self.expecting = None;
        }
    }

    /// Decodes `bytes`, appending what they complete to `out`; an incomplete
    /// sequence waits for more, or for [`Decoder::timeout`].
    pub fn bytes(&mut self, bytes: &[u8], out: &mut Vec<Input>) {
        self.feed(bytes, false, out);
    }

    /// The deadline passed (`deadline`: an Escape's, or an answer's):
    /// whatever is pending is complete as it is, and a string being dropped
    /// is cut short, so what comes next is new.
    pub fn timeout(&mut self, out: &mut Vec<Input>) {
        self.feed(&[], true, out);
    }

    /// Goes on with what the decoder was in the middle of, with `bytes`
    /// after it; `flush` once the deadline has passed.
    fn feed(&mut self, bytes: &[u8], flush: bool, out: &mut Vec<Input>) {
        let (rest, since) = match std::mem::take(&mut self.state) {
            State::Idle => (Cow::Borrowed(bytes), None),
            State::Pending { mut held, since } => {
                held.extend_from_slice(bytes);
                (Cow::Owned(held), since)
            }
            State::Dropping { bel, escape, since } => match drop_to_end(bel, escape, bytes) {
                Ok(rest) => (rest, None),
                Err(_) if flush => return,
                Err(escape) => {
                    self.state = State::Dropping { bel, escape, since };
                    return;
                }
            },
            State::Paste(text) => match paste(text, bytes, self.paste_limit, out) {
                Ok(rest) => (Cow::Borrowed(rest), None),
                Err(text) => {
                    self.state = State::Paste(text);
                    return;
                }
            },
        };
        self.state = self.run(&rest, since, flush, out);
    }

    /// Decodes `bytes`, with nothing begun before them but what they begin
    /// with, waiting since `since` if that is so; what is begun after them.
    fn run(
        &mut self,
        mut bytes: &[u8],
        mut since: Option<Instant>,
        flush: bool,
        out: &mut Vec<Input>,
    ) -> State {
        while !bytes.is_empty() {
            let rest = match decode(bytes, flush, self.expecting.is_some()) {
                Step::Done(n, input) => {
                    if input == Some(Input::Reply(Reply::Attributes)) {
                        self.expecting = self.expecting.and_then(|e| {
                            let rounds = NonZeroU8::new(e.rounds.get().saturating_sub(1))?;
                            Some(Expecting { rounds, ..e })
                        });
                    }
                    out.extend(input);
                    bytes.get(n..)
                }
                Step::PasteStart(n) => {
                    let text = bytes.get(n..).unwrap_or_default();
                    match paste(Vec::new(), text, self.paste_limit, out) {
                        Ok(rest) => Some(rest),
                        Err(text) => return State::Paste(text),
                    }
                }
                Step::Overlong { bel } => {
                    let escape = false;
                    return State::Dropping { bel, escape, since };
                }
                Step::Incomplete => {
                    let held = bytes.to_vec();
                    return State::Pending { held, since };
                }
            };
            // The sequence waited on, if any, is done: another's wait
            // begins when marked.
            bytes = rest.unwrap_or_default();
            since = None;
        }
        State::Idle
    }
}

fn press(key: Key, mods: Modifiers) -> Option<Input> {
    Some(Input::Key(KeyPress::new(key, mods).into()))
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

/// One control byte or character, without an Escape prefix, and how many
/// bytes it takes; none while it is incomplete.
fn single(bytes: &[u8], flush: bool) -> Option<(usize, KeyPress)> {
    let first = *bytes.first()?;
    let (key, mods) = match first {
        0x0d => (Key::Enter, Modifiers::NONE),
        0x09 => (Key::Tab, Modifiers::NONE),
        0x7f => (Key::Backspace, Modifiers::NONE),
        0x1b => (Key::Escape, Modifiers::NONE),
        0x00 => (Key::Char(' '), Modifiers::CTRL),
        // Ctrl-A to Ctrl-Z are the letters with bits 5 and 6 cleared.
        0x01..=0x1a => (Key::Char(char::from(first | 0x60)), Modifiers::CTRL),
        0x1c => (Key::Char('\\'), Modifiers::CTRL),
        0x1d => (Key::Char(']'), Modifiers::CTRL),
        0x1e => (Key::Char('^'), Modifiers::CTRL),
        0x1f => (Key::Char('_'), Modifiers::CTRL),
        0x20..=0x7e => (Key::Char(char::from(first)), Modifiers::NONE),
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
                    return None;
                }
            }
            let take = need.min(bytes.len());
            let text = bytes.get(..take).unwrap_or_default();
            return Some(
                match std::str::from_utf8(text)
                    .ok()
                    .and_then(|s| s.chars().next())
                {
                    Some(c) => (take, KeyPress::plain(Key::Char(c))),
                    None => (1, KeyPress::plain(Key::Char('\u{fffd}'))),
                },
            );
        }
    };
    Some((1, KeyPress::new(key, mods)))
}

/// One input from the start of `bytes`; `flush` once the deadline has
/// passed, `answers` while the terminal's answers are expected.
fn decode(bytes: &[u8], flush: bool, answers: bool) -> Step {
    let Some(&first) = bytes.first() else {
        return Step::Incomplete;
    };
    if first != 0x1b {
        return single(bytes, flush).map_or(Step::Incomplete, |(n, key)| {
            Step::Done(n, Some(Input::Key(key.into())))
        });
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
            None => Step::Done(2, press(Key::Char('O'), Modifiers::ALT)),
            // A byte no final key names: Alt-O, then the byte.
            Some(&last) => match final_key(last, Modifiers::NONE) {
                Some(input) => Step::Done(3, Some(input)),
                None => Step::Done(2, press(Key::Char('O'), Modifiers::ALT)),
            },
        },
        // Escape Escape: an Escape, then decode the second one on its own.
        0x1b => Step::Done(1, press(Key::Escape, Modifiers::NONE)),
        // An answer only while one is expected: else `ESC P` is Alt-P at
        // once, and a string that began with it would be an answer read
        // whole and keys read in pieces.
        b'P' if answers && dcs_begun(bytes) => dcs(bytes, flush),
        // What may yet begin an answer waits for the rest while one is
        // expected.
        b'P' if answers && !flush && matches!(bytes.get(2..), Some(b"" | b"0" | b"1")) => {
            Step::Incomplete
        }
        // The Escape and what followed it; within `bytes`, so exact.
        _ => match single(bytes.get(1..).unwrap_or_default(), flush) {
            Some((n, KeyPress { key, mods })) => {
                let mods = Modifiers { alt: true, ..mods };
                Step::Done(n.saturating_add(1), press(key, mods))
            }
            None => Step::Incomplete,
        },
    }
}

/// The key a final letter names, after `SS3` or a `CSI` with parameters
/// (`CSI 1 ; m A`). The same letters are written in `encode.rs` (`legacy`
/// and `kitty`), which send them; `M`, Enter here, comes only after SS3,
/// as `CSI M` is a mouse report.
fn final_key(last: u8, mods: Modifiers) -> Option<Input> {
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
#[cold]
fn osc(bytes: &[u8], flush: bool) -> Step {
    let alt_bracket = || Step::Done(2, press(Key::Char(']'), Modifiers::ALT));
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
        // Cut short: dropped.
        None if flush => return Step::Done(bytes.len(), None),
        // Longer than any answer: dropped, the rest of it as it comes.
        None if body.len() > OSC_LIMIT => return Step::Overlong { bel: true },
        None => return Step::Incomplete,
    };
    // Within `bytes`: the string, its two-byte opening and its end.
    let consumed = consumed.unwrap_or(bytes.len()).min(bytes.len());
    // A string past the limit is no answer, whether it came whole or not.
    let reply = payload
        .filter(|p| p.len() <= OSC_LIMIT)
        .and_then(colour_reply)
        .map(Input::Reply);
    Step::Done(consumed, reply)
}

/// Whether `bytes` begin as fux's DCS answers do: `ESC P`, `0` or `1`, then
/// `$` (DECRPSS) or `+` (XTGETTCAP).
fn dcs_begun(bytes: &[u8]) -> bool {
    matches!(bytes.get(..4), Some([0x1b, b'P', b'0' | b'1', b'$' | b'+']))
}

/// A DCS answer begun (`dcs_begun`): its string, ended by ST (`ESC \`). An
/// answer fux uses becomes a `Reply`; anything else, or anything cut short
/// at the deadline or past `DCS_LIMIT`, is dropped, never typed.
#[cold]
fn dcs(bytes: &[u8], flush: bool) -> Step {
    let body = bytes.get(2..).unwrap_or_default();
    let (payload, consumed) = match body.iter().position(|b| *b == 0x1b) {
        Some(i) => match body.get(i.saturating_add(1)) {
            Some(b'\\') => (body.get(..i), i.checked_add(4)),
            // ESC and something else: the string ends there, unfinished,
            // and the ESC begins what comes next.
            Some(_) => return Step::Done(i.saturating_add(2), None),
            None if flush => return Step::Done(bytes.len(), None),
            None => return Step::Incomplete,
        },
        None if flush => return Step::Done(bytes.len(), None),
        None if body.len() > DCS_LIMIT => return Step::Overlong { bel: false },
        None => return Step::Incomplete,
    };
    // Within `bytes`: the string, its two-byte opening and its end.
    let consumed = consumed.unwrap_or(bytes.len()).min(bytes.len());
    let reply = payload
        .filter(|p| p.len() <= DCS_LIMIT)
        .and_then(dcs_reply)
        .map(Input::Reply);
    Step::Done(consumed, reply)
}

/// What a DCS answer says that fux uses: that the terminal draws underline
/// styles, from XTGETTCAP's answer for `Smulx` (`1+r536d756c78=…`, the
/// name in hex, either case), or from DECRPSS's of the pen holding `4:3`
/// (`1$r0;4:3m`). Any other answer, an invalid one (`0$r`, `0+r…`) among
/// them, says nothing.
fn dcs_reply(payload: &[u8]) -> Option<Reply> {
    if let Some(pen) = payload
        .strip_prefix(b"1$r")
        .and_then(|p| p.strip_suffix(b"m"))
    {
        return pen
            .split(|b| *b == b';')
            .any(|group| group == b"4:3")
            .then_some(Reply::UnderlineStyles);
    }
    let name = payload.strip_prefix(b"1+r")?.split(|b| *b == b'=').next()?;
    name.eq_ignore_ascii_case(b"536d756c78")
        .then_some(Reply::UnderlineStyles)
}

/// `10 ; rgb:…` or `11 ; rgb:…`: the terminal's foreground or background;
/// `4 ; index ; rgb:…`: one of its palette entries.
fn colour_reply(payload: &[u8]) -> Option<Reply> {
    let mut params = payload.split(|b| *b == b';');
    let reply = match params.next()? {
        b"10" => Reply::Foreground(Rgb::parse(params.next()?)?),
        b"11" => Reply::Background(Rgb::parse(params.next()?)?),
        b"4" => {
            let index = std::str::from_utf8(params.next()?).ok()?.parse().ok()?;
            let rgb = Rgb::parse(params.next()?)?;
            Reply::Palette { index, rgb }
        }
        _ => return None,
    };
    params.next().is_none().then_some(reply)
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

/// The event a default-encoding mouse report's three bytes describe, each
/// value plus 32. Mouse reports are rare among keys: kept out of `csi`'s
/// way.
#[cold]
#[inline(never)]
fn default_mouse(code: u8, x: u8, y: u8) -> Option<MouseEvent> {
    let value = |b: u8| u32::from(b).checked_sub(32);
    mouse::event(value(code)?, value(x)?, value(y)?, false)
}

/// The event an SGR mouse report's parameters (after `<`) and final byte
/// describe; `None` for anything else.
#[cold]
#[inline(never)]
fn sgr_mouse(params: &[u8], last: u8) -> Option<MouseEvent> {
    let release = match last {
        b'M' => false,
        b'm' => true,
        _ => return None,
    };
    let text = std::str::from_utf8(params.get(1..)?).ok()?;
    let mut numbers = text.split(';').map(|n| n.parse::<u32>().ok());
    let (code, x, y) = (numbers.next()??, numbers.next()??, numbers.next()??);
    if numbers.next().is_some() {
        return None;
    }
    mouse::event(code, x, y, release)
}

/// A CSI sequence: `ESC [ params final`. Too long a sequence is dropped.
/// A CSI ended unfinished by an ESC among `params`, its bytes after
/// `ESC [`.
#[cold]
#[inline(never)]
fn unfinished(params: &[u8]) -> Step {
    let at = params.iter().position(|&b| b == 0x1b).unwrap_or(0);
    if at == 0 {
        Step::Done(2, press(Key::Char('['), Modifiers::ALT))
    } else {
        Step::Done(at.saturating_add(2), None)
    }
}

/// A `CSI ?` answer longer than any answer, `body` its bytes after `ESC [`:
/// dropped through its final byte once that is here, as an over-long OSC
/// or DCS answer is dropped to its end; held until then, up to
/// `DCS_LIMIT` bytes, past which (or at a timeout) what is held is dropped.
#[cold]
#[inline(never)]
fn long_answer(body: &[u8], flush: bool) -> Step {
    match body.iter().position(|b| (0x40..=0x7e).contains(b)) {
        Some(end) => Step::Done(end.saturating_add(3), None),
        None if flush || body.len() > DCS_LIMIT => Step::Done(body.len().saturating_add(2), None),
        None => Step::Incomplete,
    }
}

fn csi(bytes: &[u8], flush: bool) -> Step {
    let body = bytes.get(2..).unwrap_or_default();
    // A mouse report in the default encoding: `ESC [ M` and three raw
    // bytes, each value plus 32.
    if body.first() == Some(&b'M') {
        return if let Some(&[code, x, y]) = body.get(1..4) {
            Step::Done(6, default_mouse(code, x, y).map(Input::Mouse))
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
            return if answer {
                long_answer(body, flush)
            } else {
                // Not a sequence fux can use: garbage.
                Step::Done(window.len().saturating_add(2), None)
            };
        }
        if !flush {
            return Step::Incomplete;
        }
        // At a timeout, an answer begun is dropped, never typed; anything
        // else was an Escape and `[` typed: Alt-[ and the rest.
        return if answer {
            Step::Done(bytes.len(), None)
        } else {
            Step::Done(2, press(Key::Char('['), Modifiers::ALT))
        };
    };
    // `ESC [`, the parameters and the final byte; within `bytes`, so exact.
    let consumed = end.saturating_add(3);
    let params = body.get(..end).unwrap_or_default();
    // An ESC among them ended the sequence unfinished there (ECMA-48,
    // 5.5): `ESC [` alone is Alt-[, a sequence with parameters is dropped,
    // and the ESC begins the next key. The byte taken for the final one
    // then follows the ESC, so the byte before it is the one looked at.
    if params.last() == Some(&0x1b) {
        return unfinished(params);
    }
    let last = body.get(end).copied().unwrap_or(0);
    if params.first() == Some(&b'<') {
        // An SGR mouse report: `CSI < code ; column ; row M`, `m` a release.
        return Step::Done(consumed, sgr_mouse(params, last).map(Input::Mouse));
    }
    if params.first() == Some(&b'?') {
        // Answers fux does not use are dropped.
        return Step::Done(consumed, csi_reply(params, last).map(Input::Reply));
    }
    // Each parameter with its colon-separated parts; an empty part is none.
    let groups: Vec<Vec<Option<u32>>> = std::str::from_utf8(params)
        .unwrap_or("")
        .split(';')
        .map(|p| p.split(':').map(|n| n.parse().ok()).collect())
        .collect();
    let part = |group: usize, i: usize| groups.get(group)?.get(i).copied().flatten();
    let first = part(0, 0).unwrap_or(0);
    let modifier = part(1, 0).unwrap_or(1);
    // The kitty protocol's event type: a release is not a key press. fux
    // asks for none (`outer`), but a terminal may send them anyway.
    if part(1, 1) == Some(3) {
        return Step::Done(consumed, None);
    }
    let mods = modifiers(modifier);
    // Every modifier bit, the kitty protocol's beyond xterm's three too.
    let bits = u8::try_from(modifier.saturating_sub(1)).unwrap_or(u8::MAX);
    // `CSI n ~` numbers the function keys with gaps, each run from its
    // base: the key is F(`first - base`). `encode.rs` sends the same
    // numbers (its `Form::Numbered`).
    let function = |base: u32| {
        let n = u8::try_from(first.checked_sub(base)?).ok()?;
        press(Key::F(n), mods)
    };
    let input = match last {
        b'A' | b'B' | b'C' | b'D' | b'H' | b'F' | b'P' | b'Q' | b'R' | b'S' => {
            final_key(last, mods)
        }
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
            // `PASTE_START`'s number.
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
            // A control character's code names no key.
            27 => part(2, 0)
                .and_then(char::from_u32)
                .filter(|c| !c.is_control())
                .and_then(|c| press(Key::Char(c), mods)),
            _ => None,
        },
        // `CSI code ; mod u`: xterm's formatOtherKeys, and the kitty
        // protocol's form, `CSI code:shifted:base ; mods u`.
        b'u' => {
            let kitty = Kitty {
                code: Some(first),
                shifted: part(0, 1),
                base: part(0, 2),
                mods: bits,
            };
            let key = match first {
                13 => Some(Key::Enter),
                9 => Some(Key::Tab),
                27 => Some(Key::Escape),
                127 => Some(Key::Backspace),
                // The Unicode Private Use Area: the kitty protocol's
                // functional keys.
                0xe000..=0xf8ff => functional(first),
                // A control character's code names no key.
                _ => char::from_u32(first)
                    .map(|c| typed(c, &kitty))
                    .filter(|c| !c.is_control())
                    .map(Key::Char),
            };
            let stroke = key.map(|key| Keystroke {
                press: KeyPress::new(key, mods),
                kitty: Some(kitty),
            });
            return Step::Done(consumed, stroke.map(Input::Key));
        }
        _ => None,
    };
    // Modifiers beyond xterm's three (Super, Hyper, Meta, the locks) are
    // kept for a pane in the kitty protocol.
    let input = match input {
        Some(Input::Key(stroke)) if bits & !7 != 0 => Some(Input::Key(Keystroke {
            kitty: Some(Kitty {
                code: None,
                shifted: None,
                base: None,
                mods: bits,
            }),
            ..stroke
        })),
        other => other,
    };
    Step::Done(consumed, input)
}

/// The character a legacy terminal sends for the kitty-protocol key `c`,
/// so that both decode alike: with Ctrl or Alt, a key off ASCII is its
/// base-layout key, if that is ASCII (Ctrl and С on a Cyrillic layout is
/// `C-c`); with Shift, the shifted key, else a letter's capital (Alt-Shift-1
/// is `M-!`, Alt-Shift-a `M-A`), as with Caps Lock.
fn typed(c: char, kitty: &Kitty) -> char {
    let base = kitty
        .base
        .and_then(char::from_u32)
        .filter(char::is_ascii)
        .filter(|_| kitty.mods & 6 != 0 && !c.is_ascii());
    let shift = kitty.mods & 1 != 0;
    // Caps Lock capitalizes a letter, as in a legacy terminal's bytes.
    let caps = kitty.mods & 64 != 0;
    let letter = |c: char| {
        if shift != caps {
            c.to_ascii_uppercase()
        } else {
            c
        }
    };
    match base {
        Some(base) => letter(base),
        None if shift => kitty.shifted.and_then(char::from_u32).unwrap_or(letter(c)),
        None => letter(c),
    }
}

/// A kitty functional key, numbered in the Private Use Area, as the key
/// fux knows it by: a keypad key is the key it copies, as legacy terminals
/// send it. Keys fux has no name for (F13 to F35, the media, lock and
/// modifier keys) are none, and dropped.
fn functional(code: u32) -> Option<Key> {
    let key = match code {
        57399..=57408 => return char::from_digit(code.checked_sub(57399)?, 10).map(Key::Char),
        57409 => Key::Char('.'),
        57410 => Key::Char('/'),
        57411 => Key::Char('*'),
        57412 => Key::Char('-'),
        57413 => Key::Char('+'),
        57414 => Key::Enter,
        57415 => Key::Char('='),
        57416 => Key::Char(','),
        57417 => Key::Arrow(Direction::Left),
        57418 => Key::Arrow(Direction::Right),
        57419 => Key::Arrow(Direction::Up),
        57420 => Key::Arrow(Direction::Down),
        57421 => Key::PageUp,
        57422 => Key::PageDown,
        57423 => Key::Home,
        57424 => Key::End,
        57425 => Key::Insert,
        57426 => Key::Delete,
        _ => return None,
    };
    Some(key)
}

/// `from` plus `wait`, or `from` itself if that would overflow the clock.
fn after(from: Instant, wait: Duration) -> Instant {
    from.checked_add(wait).unwrap_or(from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Rgb;
    use crate::keys::colour::Scheme;

    fn all(bytes: &[u8]) -> Vec<Input> {
        let mut d = Decoder::default();
        let mut out = Vec::new();
        d.bytes(bytes, &mut out);
        d.timeout(&mut out);
        out
    }
    /// `stream` split in two at every byte decodes to `expected`, an answer
    /// to a question asked just before awaited if `expecting`.
    fn split_anywhere(stream: &[u8], expecting: bool, expected: &[Input]) {
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
    fn key(name: &str) -> Input {
        Input::Key(name.parse().unwrap_or(KeyPress::char('?')).into())
    }
    /// The presses among `inputs`, what fux matches.
    fn presses(inputs: Vec<Input>) -> Vec<Option<KeyPress>> {
        inputs
            .into_iter()
            .map(|input| match input {
                Input::Key(stroke) => Some(stroke.press),
                Input::Paste(_)
                | Input::PasteTooLong
                | Input::FocusIn
                | Input::FocusOut
                | Input::Mouse(_)
                | Input::Reply(_) => None,
            })
            .collect()
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
        ] {
            assert_eq!(all(bytes), vec![key(name)], "{bytes:?}");
        }
        assert_eq!(presses(all(b"\x1b[105;5u")), vec![name_press("C-i")]);
    }

    fn name_press(name: &str) -> Option<KeyPress> {
        name.parse().ok()
    }

    /// The deadline runs from when decoding began waiting, through bytes
    /// that leave the same sequence waiting; a wait for another sequence
    /// runs from the read it began in, even one that ended the first wait.
    #[test]
    fn the_deadline_runs_from_when_waiting_began() {
        let ms = Duration::from_millis;
        let t0 = Instant::now();
        let mut d = Decoder::default();
        let mut out = Vec::new();
        // The deadline after `bytes` read `at` ms after `t0`, from `t0`.
        let mut due = |bytes: &[u8], at| {
            d.bytes(bytes, &mut out);
            d.mark(t0 + ms(at));
            d.deadline().map(|due| due.duration_since(t0))
        };
        assert_eq!(due(b"", 0), None, "not waiting");
        assert_eq!(due(b"\x1b", 0), Some(ESCAPE_DELAY));
        assert_eq!(due(b"[", 10), Some(ESCAPE_DELAY), "still the first wait");
        let pasted = due(b"200~pasted\x1b[201~\x1b", 20);
        assert_eq!(pasted, Some(ms(20) + ESCAPE_DELAY), "after a paste");
        assert_eq!(due(b"a", 30), None);
        assert_eq!(due(b"\x1b", 40), Some(ms(40) + ESCAPE_DELAY));
        // Up, then an Escape begun in the same read: its rest has its own
        // `ESCAPE_DELAY` to come, not what was left of Up's.
        assert_eq!(due(b"[A\x1b", 70), Some(ms(70) + ESCAPE_DELAY));
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
        split_anywhere(stream, false, &whole);
    }

    /// The kitty protocol's forms, as a terminal sends them with
    /// disambiguate and alternate keys pushed (the spec's "Disambiguate
    /// escape codes", "Key codes", "Modifiers", "Event types" and
    /// "Functional key definitions"), decode to the press a legacy
    /// terminal's bytes for the same key do, where those can tell the key:
    /// Ctrl-Shift-a is `C-a` as 0x01 is, Alt-Shift-1 `M-!` as `ESC !` is,
    /// Ctrl and a Cyrillic С `C-c`, Caps Lock with Alt and a `M-A`; and to
    /// the key itself where they cannot (Shift-Enter, Ctrl-I, Ctrl-[).
    #[test]
    fn kitty_forms_decode_to_the_presses_legacy_bytes_do() {
        for (bytes, name) in [
            (&b"\x1b[27u"[..], "Escape"),
            (b"\x1b[27;3u", "M-Escape"),
            (b"\x1b[13;2u", "S-Enter"),
            (b"\x1b[9;2u", "BTab"),
            (b"\x1b[127;5u", "C-BSpace"),
            (b"\x1b[98;5u", "C-b"),
            (b"\x1b[105;5u", "C-i"),
            (b"\x1b[91;5u", "C-["),
            (b"\x1b[97:65;6u", "C-a"),
            (b"\x1b[97:65;4u", "M-A"),
            (b"\x1b[97;4u", "M-A"),
            (b"\x1b[49:33;4u", "M-!"),
            (b"\x1b[1089::99;5u", "C-c"),
            (b"\x1b[1089:1057:99;6u", "C-c"),
            (b"\x1b[1089::99;3u", "M-c"),
            (b"\x1b[97;69u", "C-a"),
            (b"\x1b[97;67u", "M-A"),
            (b"\x1b[97;9u", "a"),
            (b"\x1b[32;5u", "C-Space"),
            (b"\x1b[120;7u", "C-M-x"),
            (b"\x1b[1;65A", "Up"),
            (b"\x1b[1;5D", "C-Left"),
            (b"\x1b[P", "F1"),
            (b"\x1b[13~", "F3"),
            (b"\x1b[1;5S", "C-F4"),
            (b"\x1b[57414u", "Enter"),
            (b"\x1b[57399;5u", "C-0"),
            (b"\x1b[57419u", "Up"),
            (b"\x1b[97;5:2u", "C-a"),
        ] {
            assert_eq!(presses(all(bytes)), vec![name_press(name)], "{bytes:?}");
        }
        // A release is not a press, nor a key fux has no name for (a media
        // key, a modifier alone, F13).
        for bytes in [
            &b"\x1b[97;5:3u"[..],
            b"\x1b[1;1:3A",
            b"\x1b[57428u",
            b"\x1b[57441;2u",
            b"\x1b[57376u",
        ] {
            assert_eq!(all(bytes), vec![], "{bytes:?}");
        }
        // What the press leaves out, the stroke keeps, for a pane in the
        // protocol.
        assert_eq!(
            all(b"\x1b[97:65;6u"),
            vec![Input::Key(Keystroke {
                press: name_press("C-a").unwrap_or(KeyPress::char('?')),
                kitty: Some(Kitty {
                    code: Some(97),
                    shifted: Some(65),
                    base: None,
                    mods: 5,
                }),
            })]
        );
        assert_eq!(
            all(b"\x1b[1;9A"),
            vec![Input::Key(Keystroke {
                press: KeyPress::plain(Key::Arrow(Direction::Up)),
                kitty: Some(Kitty {
                    code: None,
                    shifted: None,
                    base: None,
                    mods: 8,
                }),
            })]
        );
        // Escape is whole at once: no wait for the rest of a sequence.
        let mut d = Decoder::default();
        let mut out = Vec::new();
        d.bytes(b"\x1b[27u", &mut out);
        assert!(!d.waiting());
        assert_eq!(presses(out), vec![name_press("Escape")]);
        // Split anywhere, among pastes and focus changes, alike.
        let stream: &[u8] = b"\x1b[98;5ud\x1b[200~p\x1b[201~\x1b[I\x1b[13;2u\x1b[27u";
        let whole = all(stream);
        assert_eq!(whole.len(), 6);
        split_anywhere(stream, false, &whole);
    }

    /// fux's prefix, bindings, overlays and copy mode match decoded presses,
    /// which send-keys names name: every key that legacy bytes can tell
    /// decodes to the same press from the kitty protocol's bytes, with the
    /// flags fux pushes (disambiguate and alternate keys). The kitty bytes
    /// are the spec's for the key, as fux writes them for a pane with those
    /// flags (`encode::tests`).
    #[test]
    fn every_key_decodes_alike_with_and_without_the_kitty_protocol() {
        use crate::keys::encode::{KeyMode, Report, key_bytes};
        let mut names = crate::keys::all_names();
        names.extend(
            (' '..='~')
                .filter(|c| !c.is_ascii_uppercase())
                .map(String::from),
        );
        let mut compared = 0;
        for name in &names {
            for prefix in ["", "C-", "M-", "S-", "C-M-", "C-S-", "M-S-", "C-M-S-"] {
                let Ok(press) = format!("{prefix}{name}").parse::<KeyPress>() else {
                    continue;
                };
                let decoded = |mode: KeyMode| {
                    let mut bytes = Vec::new();
                    key_bytes(press.into(), mode, &mut bytes);
                    presses(all(&bytes))
                };
                if decoded(KeyMode::legacy(false)) != vec![Some(press)] {
                    // Legacy bytes cannot tell this key (S-Enter is Enter).
                    continue;
                }
                let kitty = KeyMode::Kitty {
                    alternate: true,
                    report: Report::Disambiguated,
                };
                assert_eq!(decoded(kitty), vec![Some(press)], "{prefix}{name}");
                compared += 1;
            }
        }
        assert!(compared > 500, "{compared}");
    }

    /// Each answer fux asks for (`outer`), in the forms terminals give
    /// (ctlseqs: OSC 10/11 answered `rgb:RRRR/GGGG/BBBB`, ended as asked;
    /// DECRQM `CSI ? Ps ; Pm $ y`; DA1 `CSI ? … c`; the kitty flags
    /// `CSI ? flags u`; contour's `CSI ? 997 ; 1|2 n`), among keys, whole
    /// and split at every byte, with an answer expected or not.
    #[test]
    fn answers_are_told_from_keys_however_they_arrive() {
        let stream: &[u8] = b"a\x1b]10;rgb:ffff/ffff/ffff\x1b\\b\x1b]11;rgb:1e/1e/20\x07\
            \x1b]4;1;rgb:cdcd/0000/0000\x1b\\\x1b]4;15;rgb:f/f/f\x07\
            \x1b[?2031;2$y\x1b[?997;1nc\x1b[?997;2n\x1b[?5u\x1b[?62;22;52c\x1b[Ad";
        let palette = |index, r, g, b| {
            Input::Reply(Reply::Palette {
                index,
                rgb: Rgb { r, g, b },
            })
        };
        let expected = vec![
            key("a"),
            Input::Reply(Reply::Foreground([0xff; 3].into())),
            key("b"),
            Input::Reply(Reply::Background([0x1e, 0x1e, 0x20].into())),
            palette(1, 0xcd, 0, 0),
            palette(15, 0xff, 0xff, 0xff),
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
            split_anywhere(stream, expecting, &expected);
        }
        // Colours fux does not ask for or cannot read are dropped; answers
        // it does not use reach the session, which ignores them: no keys.
        assert_eq!(
            all(b"\x1b[?1;2c\x1b]12;rgb:0/0/0\x07\x1b]11;red\x07\x1b]4;256;rgb:0/0/0\x07\x1b]4;x;rgb:0/0/0\x07\x1b]4;1\x07\x1b[?6;1$yx"),
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
        // An OSC string longer than any answer is dropped as it arrives,
        // waiting `REPLY_DELAY` for its end as an answer cut short does.
        let mut long = b"\x1b]11;".to_vec();
        long.extend(std::iter::repeat_n(b'x', 200));
        let mut d = Decoder::default();
        d.bytes(&long, &mut out);
        d.mark(t0);
        assert_eq!(d.deadline(), Some(t0 + REPLY_DELAY));
        long.extend_from_slice(b"\x07y");
        assert_eq!(all(&long), vec![key("y")]);
    }

    /// An ESC inside a CSI ends it unfinished, as ECMA-48 (5.5) has it, and
    /// begins what follows: `ESC [` with nothing
    /// before it is Alt-[, a sequence with parameters is dropped. The key
    /// after it is not lost.
    #[test]
    fn an_escape_inside_a_csi_ends_it_and_begins_the_next_key() {
        assert_eq!(all(b"\x1b[\x1b[A"), vec![key("M-["), key("Up")]);
        assert_eq!(all(b"\x1b[12\x1b[A"), vec![key("Up")]);
    }

    /// `ESC O` and a byte no final key names is Alt-O, then that byte, as
    /// `ESC` and any other byte is: neither is lost.
    #[test]
    fn escape_o_and_a_byte_it_does_not_name_is_alt_o_and_the_byte() {
        assert_eq!(all(b"\x1bOx"), vec![key("M-O"), key("x")]);
        assert_eq!(all(b"\x1bO\x1b[A"), vec![key("M-O"), key("Up")]);
        assert_eq!(all(b"\x1bOA"), vec![key("Up")], "a key SS3 names");
    }

    /// A `CSI ?` answer longer than any answer is dropped through its
    /// final byte, as an over-long OSC or DCS answer is: none of it typed.
    #[test]
    fn an_over_long_csi_answer_is_dropped_through_its_final_byte() {
        let mut long = b"\x1b[?".to_vec();
        long.extend(std::iter::repeat_n(b'1', 70));
        long.extend_from_slice(b"ck");
        assert_eq!(all(&long), vec![key("k")]);
    }

    /// A DCS answer begun while answers are expected is read whole though
    /// the window for them passes before its end: what it decodes to does
    /// not depend on where the time between its pieces falls.
    #[test]
    fn a_dcs_answer_begun_in_its_window_is_read_whole_after_it() {
        let t0 = Instant::now();
        let answer: &[u8] = b"\x1bP1$r0;4:3m\x1b\\";
        let mut whole = Decoder::default();
        let mut expected = Vec::new();
        whole.expect(t0);
        whole.bytes(answer, &mut expected);
        let (first, rest) = answer.split_at_checked(8).unwrap_or((answer, &[]));
        let mut d = Decoder::default();
        let mut out = Vec::new();
        d.expect(t0);
        d.expire(t0 + Duration::from_millis(900));
        d.bytes(first, &mut out);
        d.expire(t0 + REPLY_WINDOW + Duration::from_millis(50));
        d.bytes(rest, &mut out);
        assert_eq!(out, expected);
    }

    /// A key report whose code is a control character (`CSI u`, `CSI 1 ;
    /// 5 u`, xterm's `CSI 27 ; 5 ; 1 ~`) is no key: a key name cannot say
    /// it, so no binding could match it and nothing could name it back.
    #[test]
    fn a_key_report_of_a_control_character_is_no_key() {
        for report in [&b"\x1b[u"[..], b"\x1b[1;5u", b"\x1b[0;5u", b"\x1b[27;5;1~"] {
            assert_eq!(all(report), vec![], "{report:?}");
        }
        let pressed = |input: &Input| {
            if let Input::Key(stroke) = input {
                Some(stroke.press)
            } else {
                None
            }
        };
        let letter: Vec<_> = all(b"\x1b[97;5u").iter().map(pressed).collect();
        assert_eq!(
            letter,
            vec![Some(KeyPress::new(Key::Char('a'), Modifiers::CTRL))],
            "a letter's code"
        );
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

    /// The DCS answers fux asks for (`outer::styles!`), in the forms
    /// terminals give them: XTGETTCAP's (ctlseqs: `DCS 1 + r Pt ST`, the
    /// name in hex, `=` and the value in hex; `DCS 0 + r Pt ST` for a name
    /// the terminal does not have) and DECRPSS's (`DCS 1 $ r Pt ST`, the
    /// pen as the SGR that sets it). Either says the terminal draws styles,
    /// `Smulx` known or `4:3` kept; anything else says nothing and is
    /// dropped. Among keys, whole and split at every byte while expected;
    /// when not, they are keys.
    #[test]
    fn dcs_answers_are_told_from_keys_however_they_arrive() {
        let stream: &[u8] = b"a\x1bP1+r536d756c78=5c455b343a25703125646d\x1b\\b\
            \x1bP1$r0;4m\x1b\\\x1bP1$r0;4:3m\x1b\\c\x1bP0+r536D756C78\x1b\\\
            \x1bP1+r536D756C78\x1b\\\x1bP0$r\x1b\\\x1bP1+r5463\x1b\\\x1bP1$r4:3m\x1b\\d";
        let styles = || Input::Reply(Reply::UnderlineStyles);
        let expected = vec![
            key("a"),
            styles(),
            key("b"),
            styles(),
            key("c"),
            styles(),
            styles(),
            key("d"),
        ];
        split_anywhere(stream, true, &expected);
        // Not expected, they are keys, however they are split: an answer
        // read whole would else be one, and read in pieces keys.
        let keys = all(stream);
        assert!(keys.contains(&key("M-P")), "{keys:?}");
        split_anywhere(stream, false, &keys);
        // An answer cut short waits `REPLY_DELAY`, then is dropped; one past
        // `DCS_LIMIT` is dropped as it arrives.
        let t0 = Instant::now();
        let mut d = Decoder::default();
        d.expect(t0);
        let mut out = Vec::new();
        d.bytes(b"\x1bP1$r0;4", &mut out);
        d.mark(t0);
        assert_eq!(d.deadline(), Some(t0 + REPLY_DELAY));
        d.timeout(&mut out);
        assert!(out.is_empty(), "{out:?}");
        let mut long = b"\x1bP1+r".to_vec();
        long.extend(std::iter::repeat_n(b'5', DCS_LIMIT + 10));
        long.extend_from_slice(b"\x1b\\y");
        let mut d = Decoder::default();
        d.expect(t0);
        let mut out = Vec::new();
        d.bytes(&long, &mut out);
        d.timeout(&mut out);
        assert_eq!(out, vec![key("y")]);
    }

    /// `ESC P` is Alt-P at once, unless an answer is
    /// expected: then it, and `ESC P 1`, wait `REPLY_DELAY` for the rest of
    /// one, and are Alt-P and the key after, if it does not come. What
    /// does not begin as an answer is keys at once.
    #[test]
    fn alt_p_waits_only_while_an_answer_is_expected() {
        let alt_p = key("M-P");
        assert_eq!(all(b"\x1bP"), vec![alt_p.clone()]);
        assert_eq!(all(b"\x1bP1"), vec![alt_p.clone(), key("1")]);
        assert_eq!(all(b"\x1bPx"), vec![alt_p.clone(), key("x")]);
        assert_eq!(all(b"\x1bP1x"), vec![alt_p.clone(), key("1"), key("x")]);
        let t0 = Instant::now();
        let mut d = Decoder::default();
        let mut out = Vec::new();
        d.bytes(b"\x1bP", &mut out);
        assert_eq!(out, vec![alt_p.clone()], "not waiting");
        assert!(!d.waiting());
        for pending in [&b"\x1bP"[..], b"\x1bP1"] {
            let mut d = Decoder::default();
            d.expect(t0);
            let mut out = Vec::new();
            d.bytes(pending, &mut out);
            d.mark(t0);
            assert!(out.is_empty(), "{pending:?}");
            assert_eq!(d.deadline(), Some(t0 + REPLY_DELAY), "{pending:?}");
            d.timeout(&mut out);
            assert_eq!(out.first(), Some(&alt_p), "{pending:?}");
        }
        let mut d = Decoder::default();
        d.expect(t0);
        let mut out = Vec::new();
        d.bytes(b"\x1bPx", &mut out);
        assert_eq!(out, vec![alt_p, key("x")]);
    }

    #[test]
    fn mouse_reports_are_events_and_pastes_are_bounded() {
        use crate::keys::mouse::{MouseAction, MouseButton};
        let left = |action, row, col| {
            Input::Mouse(MouseEvent {
                action,
                button: Some(MouseButton::Left),
                mods: Modifiers::NONE,
                row,
                col,
            })
        };
        assert_eq!(
            all(b"\x1b[M !!a"),
            vec![left(MouseAction::Press, 0, 0), key("a")]
        );
        assert_eq!(
            all(b"\x1b[<0;10;5Ma\x1b[<0;10;5m"),
            vec![
                left(MouseAction::Press, 4, 9),
                key("a"),
                left(MouseAction::Release, 4, 9)
            ]
        );
        // What no report is: dropped, never typed.
        for junk in [
            &b"\x1b[<0;0;5M"[..],
            b"\x1b[<0;1M",
            b"\x1b[<0;1;1;1M",
            b"\x1b[<0;1;1x",
            b"\x1b[<192;1;1M",
            b"\x1b[<0;1;65537M",
            b"\x1b[<1:2;1;1M",
            b"\x1b[M\x1f!!",
            b"\x1b[M  !",
        ] {
            assert_eq!(all(junk), vec![], "{junk:?}");
        }
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
        // Another limit, larger or smaller.
        let pasted = |limit: usize, n: usize| {
            let mut bytes = b"\x1b[200~".to_vec();
            bytes.extend(std::iter::repeat_n(b'x', n));
            bytes.extend_from_slice(b"\x1b[201~");
            let mut out = Vec::new();
            Decoder::with_paste_limit(limit).bytes(&bytes, &mut out);
            out
        };
        let big = PASTE_LIMIT * 4;
        assert!(matches!(pasted(big, big).first(), Some(Input::Paste(t)) if t.len() == big));
        assert_eq!(pasted(big, big + 1), vec![Input::PasteTooLong]);
        assert_eq!(pasted(3, 4), vec![Input::PasteTooLong]);
        assert_eq!(pasted(3, 3), vec![Input::Paste("xxx".into())]);
    }

    /// An answer's string past its limit is dropped to its end however its
    /// bytes are read, and none of it is typed.
    #[test]
    fn a_string_past_its_limit_is_dropped_alike_however_it_is_read() {
        // Read as answers are, while one is expected.
        let pieced = |bytes: &[u8], size: usize| {
            let mut d = Decoder::default();
            d.expect(Instant::now());
            let mut out = Vec::new();
            let mut rest = bytes;
            while !rest.is_empty() {
                let (piece, tail) = rest
                    .split_at_checked(size.min(rest.len()))
                    .unwrap_or((rest, &[]));
                d.bytes(piece, &mut out);
                rest = tail;
            }
            d.timeout(&mut out);
            out
        };
        let mut osc = b"\x1b]11;".to_vec();
        osc.extend(std::iter::repeat_n(b'0', OSC_LIMIT + 20));
        let mut dcs = b"\x1bP1+r".to_vec();
        dcs.extend(std::iter::repeat_n(b'5', DCS_LIMIT + 20));
        let mut streams = Vec::new();
        for (string, ends) in [
            (&osc, vec![&b"\x07"[..], b"\x1b\\"]),
            (&dcs, vec![b"\x1b\\"]),
        ] {
            for end in ends {
                streams.push((
                    [string.as_slice(), end, b"xy"].concat(),
                    vec![key("x"), key("y")],
                ));
            }
            // Ended by an Escape that begins a key, or not at all.
            streams.push(([string.as_slice(), b"\x1bx"].concat(), vec![key("M-x")]));
            streams.push(([string.as_slice(), b"\xeb"].concat(), vec![]));
        }
        for (stream, tail) in &streams {
            let whole = pieced(stream, stream.len());
            assert!(whole.ends_with(tail), "{stream:?}: {whole:?}");
            for size in 1..=stream.len() {
                assert_eq!(
                    pieced(stream, size),
                    whole,
                    "{stream:?} in pieces of {size}"
                );
            }
        }
    }
}
