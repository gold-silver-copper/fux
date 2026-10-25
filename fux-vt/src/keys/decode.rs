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
use crate::keys::colour::Scheme;
use crate::keys::encode::{CTRL_BACKSPACE, Control, PASTE_END, numbered_key};
use crate::keys::mouse::{self, MouseEvent};
use crate::keys::{
    ALT_BIT, CAPS_LOCK_BIT, CTRL_BIT, DISAMBIGUATE, Direction, Function, KITTY_FLAGS, Key,
    KeyPress, Keystroke, Kitty, Modifiers, SHIFT_BIT,
};
use crate::{ModeStatus, Rgb};
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
/// The longest OSC answer kept, with the BEL or the ESC that begins its
/// end: `OSC 11 ; rgb:RRRR/GGGG/BBBB ST` is 29 bytes. A longer string is
/// dropped.
const OSC_LIMIT: usize = 128;
/// The longest DCS answer kept, with the ESC that begins its ST: a
/// DECRPSS of the longest pen, `DCS 1 $ r 0;1;4:3;6;7;8;2;3;9;38:2::255:255:255;48:2::…;58:2::… m ST`,
/// is under 90 bytes, and XTGETTCAP's answer for `Smulx` under 50.
const DCS_LIMIT: usize = 256;
/// The longest sequence the decoder holds unfinished: `ESC P` and a DCS
/// answer's limit.
const HELD: usize = DCS_LIMIT + 2;
/// The largest paste delivered; a longer one is refused whole.
pub const PASTE_LIMIT: usize = 64 * 1024;
/// How long a bracketed paste waits for more of itself before it ends
/// unfinished: a terminal writes a paste at once, so only one that lost
/// its end marker, or a link stalled for that long, stops for so long.
/// Until then every byte is the paste's, as it must be: a paste never
/// becomes keys.
const PASTE_DELAY: Duration = Duration::from_secs(5);

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
        /// Its status.
        status: ModeStatus,
    },
    /// The kitty keyboard protocol's flags: `CSI ? flags u`.
    KittyFlags(u8),
    /// Primary device attributes, `CSI ? … c`: whether they name sixel
    /// graphics (`4`).
    Attributes {
        /// The terminal draws sixel graphics.
        sixel: bool,
    },
    /// The size of a cell in pixels, `CSI 6 ; height ; width t`: the answer
    /// to `CSI 16 t`.
    CellSize {
        /// A cell's width in pixels.
        width: u16,
        /// A cell's height in pixels.
        height: u16,
    },
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
    /// The terminal's latest kitty flags hold disambiguate: it sends
    /// Escape as `CSI 27 u`, so a bare ESC is a sequence begun, and waits
    /// for the rest of it however long it takes, never taken for Escape.
    escape_said: bool,
}

impl Default for Decoder {
    fn default() -> Self {
        Self::with_paste_limit(PASTE_LIMIT)
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
    /// An answer's string past its limit (OSC or DCS), or a `CSI ?` answer
    /// past its (`csi`), dropped as it arrives until it ends
    /// (`drop_to_end`), as an answer begun waits.
    Dropping {
        bel: bool,
        escape: bool,
        csi: bool,
        since: Option<Instant>,
    },
    /// Inside a bracketed paste: what it has kept, and since when no more
    /// of it has come, once marked. Its end marker ends it, or
    /// `PASTE_DELAY` without bytes, unfinished.
    Paste {
        pasted: Pasted,
        since: Option<Instant>,
    },
}

/// What a bracketed paste under way keeps.
enum Pasted {
    /// Its bytes so far, no more than the limit, but for the end of them
    /// that may begin its end marker.
    Text(Vec<u8>),
    /// It is past the limit, which was reported (`PasteTooLong`) at once:
    /// it is dropped as it comes, keeping only the end that may begin its
    /// end marker.
    TooLong(Vec<u8>),
}

/// A paste's text, whole: a paste if no longer than `limit`, else too
/// long.
fn finished(text: Vec<u8>, limit: usize) -> Input {
    if text.len() > limit {
        return Input::PasteTooLong;
    }
    // Moved as it is, unless it is not UTF-8.
    Input::Paste(
        String::from_utf8(text)
            .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned()),
    )
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
/// read ended with an ESC that may begin its ST, `csi` if it is a CSI,
/// which its final byte ends: the bytes after it, if it ends within them,
/// with whether an ESC, the last read's, begins them (an Escape that ends
/// the string unfinished begins what follows); else whether they end with
/// an ESC that may begin its ST.
#[cold]
fn drop_to_end(
    bel: bool,
    mut escape: bool,
    csi: bool,
    bytes: &[u8],
) -> Result<(bool, &[u8]), bool> {
    // Every byte is looked at, at most: those after the end are decoded
    // again, and counted again there.
    work!(keys, bytes.len());
    // A CSI past its window ends with its final byte, as one read whole does
    // (`csi_end`); any other byte begins what comes next.
    if csi {
        let rest = csi_end(bytes).ok_or(false)?;
        return Ok((false, bytes.get(rest..).unwrap_or_default()));
    }
    for (i, &byte) in bytes.iter().enumerate() {
        let rest = match (escape, byte) {
            (true, b'\\') => i.saturating_add(1),
            (false, 0x07) if bel => i.saturating_add(1),
            // ESC and something else: the string ends there, unfinished,
            // and the ESC begins what comes next.
            (true, _) => match i.checked_sub(1) {
                Some(escape) => escape,
                None => return Ok((true, bytes)),
            },
            (false, byte) => {
                escape = byte == 0x1b;
                continue;
            }
        };
        return Ok((false, bytes.get(rest..).unwrap_or_default()));
    }
    Err(escape)
}

/// Takes `bytes` of a bracketed paste, `pasted` what it kept so far;
/// once its end marker is among them, what it comes to, to `out`, and the
/// bytes after it; else what it keeps. Once it is past `limit`, by more
/// than the end that may begin the marker, it is reported too long, at
/// once, and dropped from then on. The marker is looked for in `bytes`
/// where they lie, and only what the paste keeps is copied: a read that
/// holds many pastes is never copied whole for each (THREATS.md B23).
fn paste<'a>(
    pasted: Pasted,
    bytes: &'a [u8],
    limit: usize,
    out: &mut Vec<Input>,
) -> Result<&'a [u8], Pasted> {
    let (mut text, too_long) = match pasted {
        Pasted::Text(text) => (text, false),
        Pasted::TooLong(tail) => (tail, true),
    };
    let marker = PASTE_END.len();
    // The marker begun in what was kept, and ended in these bytes: the
    // longest such start first, the earliest.
    let across = (1..marker).rev().find(|&k| {
        PASTE_END
            .split_at_checked(k)
            .is_some_and(|(head, tail)| text.ends_with(head) && bytes.starts_with(tail))
    });
    let found = match across {
        Some(k) => Some((text.len().saturating_sub(k), 0, marker.saturating_sub(k))),
        None => (0..bytes.len())
            .find(|&at| bytes.get(at..).is_some_and(|b| b.starts_with(PASTE_END)))
            .map(|at| (text.len(), at, at.saturating_add(marker))),
    };
    // The bytes searched: to the marker's end, or all of them.
    work!(keys, found.map_or(bytes.len(), |(_, _, rest)| rest));
    let Some((kept, taken, rest)) = found else {
        // No end yet: the paste keeps its bytes while it may still be
        // within its limit, else only the end that may begin the marker.
        let total = text.len().saturating_add(bytes.len());
        let tail = total.saturating_sub(marker.saturating_sub(1));
        if !too_long && tail <= limit {
            work!(keys, bytes.len());
            // Grown by doubling, as a `Vec` grows, but never past what the
            // paste may keep: its limit and the end that may begin the
            // marker (B23).
            if total > text.capacity() {
                let most = limit.saturating_add(marker.saturating_sub(1));
                let room = total.max(text.capacity().saturating_mul(2)).min(most);
                text.reserve_exact(room.saturating_sub(text.len()));
            }
            text.extend_from_slice(bytes);
            return Err(Pasted::Text(text));
        }
        if !too_long {
            out.push(Input::PasteTooLong);
        }
        let keep = marker.saturating_sub(1);
        let from_text = keep.saturating_sub(bytes.len()).min(text.len());
        let mut end = (text.get(text.len().saturating_sub(from_text)..))
            .unwrap_or_default()
            .to_vec();
        end.extend_from_slice(
            bytes
                .get(bytes.len().saturating_sub(keep)..)
                .unwrap_or_default(),
        );
        return Err(Pasted::TooLong(end));
    };
    if !too_long {
        // Copied only if the paste is within its limit with them.
        if kept.saturating_add(taken) > limit {
            out.push(Input::PasteTooLong);
        } else {
            text.truncate(kept);
            work!(keys, taken);
            text.extend_from_slice(bytes.get(..taken).unwrap_or_default());
            out.push(finished(text, limit));
        }
    }
    Ok(bytes.get(rest..).unwrap_or_default())
}

enum Step {
    /// Consumed this many bytes, producing an input or nothing.
    Done(usize, Option<Input>),
    /// Consumed this many bytes, the start of a bracketed paste.
    PasteStart(usize),
    /// The bytes are an answer's string past its limit, all of it: the rest
    /// of it is dropped as it comes, to its end (BEL too if `bel`, or ST).
    /// A CSI past its window (`csi`) ends at its final byte.
    /// `escape` if the bytes end with an ESC, which may begin the ST.
    Overlong { bel: bool, csi: bool, escape: bool },
    /// The bytes are a prefix of something longer.
    Incomplete,
}

impl Decoder {
    /// A decoder that keeps pastes of up to `limit` bytes, rather than
    /// [`PASTE_LIMIT`]: a host that sends a long paste on in pieces. It
    /// holds that much while a paste arrives.
    pub fn with_paste_limit(limit: usize) -> Self {
        Self {
            state: State::Idle,
            paste_limit: limit,
            expecting: None,
            escape_said: false,
        }
    }

    /// Records `now` as when decoding began waiting, if it is waiting on a
    /// sequence it was not waiting on when last marked, or on a paste that
    /// has had bytes since.
    pub fn mark(&mut self, now: Instant) {
        match &mut self.state {
            State::Pending { since, .. }
            | State::Dropping { since, .. }
            | State::Paste { since, .. } => {
                since.get_or_insert(now);
            }
            State::Idle => {}
        }
    }

    /// When `timeout` is due: `ESCAPE_DELAY` after decoding began waiting,
    /// as marked, or `REPLY_DELAY` if what waits is an answer or a character
    /// begun, or a bare `ESC ]`, `ESC P`, `ESC P0` or `ESC P1` while an answer is
    /// expected; `PASTE_DELAY` after a paste's last bytes; none while it is
    /// not waiting, nor for a key begun once the terminal said it sends
    /// Escape as a sequence of its own.
    pub fn deadline(&self) -> Option<Instant> {
        let (since, wait) = match &self.state {
            State::Pending { held, since } => {
                // A character begun, after an Escape or not, waits for its
                // bytes as long as an answer: a slow link may split it.
                let reply = match held.as_slice() {
                    held if utf8_begun(held.strip_prefix(b"\x1b").unwrap_or(held)) => true,
                    b"\x1b]" | b"\x1bP" | b"\x1bP0" | b"\x1bP1" => self.expecting.is_some(),
                    held => {
                        held.starts_with(b"\x1b]") || held.starts_with(b"\x1b[?") || dcs_begun(held)
                    }
                };
                match (reply, self.escape_said) {
                    (true, _) => (since, REPLY_DELAY),
                    (false, false) => (since, ESCAPE_DELAY),
                    (false, true) => return None,
                }
            }
            State::Dropping { since, .. } => (since, REPLY_DELAY),
            State::Paste { since, .. } => (since, PASTE_DELAY),
            State::Idle => return None,
        };
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
        let from = out.len();
        self.feed(bytes, false, out);
        let flags = |input: &Input| match *input {
            Input::Reply(Reply::KittyFlags(flags)) => Some(flags),
            Input::Key(_)
            | Input::Paste(_)
            | Input::PasteTooLong
            | Input::FocusIn
            | Input::FocusOut
            | Input::Mouse(_)
            | Input::Reply(_) => None,
        };
        let new = out.get(from..).unwrap_or_default();
        if let Some(latest) = new.iter().rev().find_map(flags) {
            self.escape_said = latest & DISAMBIGUATE != 0;
        }
    }

    /// The deadline passed (`deadline`: an Escape's, an answer's, or a
    /// paste's): whatever is pending is complete as it is, and a string
    /// being dropped or a paste is cut short, so what comes next is new.
    pub fn timeout(&mut self, out: &mut Vec<Input>) {
        self.feed(&[], true, out);
    }

    /// Goes on with what the decoder was in the middle of, with `bytes`
    /// after it; `flush` once the deadline has passed.
    fn feed(&mut self, mut bytes: &[u8], flush: bool, out: &mut Vec<Input>) {
        loop {
            self.state = match std::mem::take(&mut self.state) {
                State::Idle => {
                    self.state = self.run(bytes, None, flush, out);
                    return;
                }
                State::Pending { mut held, since } => {
                    // A held sequence is decided within `HELD` + 1 bytes of
                    // its start: only those are copied to it, and the rest
                    // is read where it lies (THREATS.md B24).
                    let room = (HELD.saturating_add(1)).saturating_sub(held.len());
                    let (piece, rest) = bytes.split_at_checked(room).unwrap_or((bytes, &[]));
                    work!(keys, piece.len());
                    held.extend_from_slice(piece);
                    let state = self.run(&held, since, flush && rest.is_empty(), out);
                    if rest.is_empty() {
                        self.state = state;
                        return;
                    }
                    match state {
                        // A sequence begun in the piece, left held, is read
                        // again from where it begins, with the rest.
                        State::Pending { held: begun, .. } if begun.len() <= piece.len() => {
                            let start = piece.len().saturating_sub(begun.len());
                            bytes = bytes.get(start..).unwrap_or_default();
                            State::Idle
                        }
                        state @ (State::Idle
                        | State::Pending { .. }
                        | State::Dropping { .. }
                        | State::Paste { .. }) => {
                            bytes = rest;
                            state
                        }
                    }
                }
                State::Dropping {
                    bel,
                    escape,
                    csi,
                    since,
                } => match drop_to_end(bel, escape, csi, bytes) {
                    Ok((escape, rest)) => {
                        bytes = rest;
                        match escape {
                            true => State::Pending {
                                held: vec![0x1b],
                                since: None,
                            },
                            false => State::Idle,
                        }
                    }
                    Err(_) if flush => return,
                    Err(escape) => {
                        self.state = State::Dropping {
                            bel,
                            escape,
                            csi,
                            since,
                        };
                        return;
                    }
                },
                State::Paste { pasted, since } => {
                    match paste(pasted, bytes, self.paste_limit, out) {
                        Ok(rest) => {
                            bytes = rest;
                            State::Idle
                        }
                        Err(Pasted::Text(text)) if flush => {
                            out.push(finished(text, self.paste_limit));
                            return;
                        }
                        // One reported too long comes to nothing more.
                        Err(Pasted::TooLong(_)) if flush => return,
                        Err(pasted) => {
                            // Bytes of it came: its wait begins again when marked.
                            let since = since.filter(|_| bytes.is_empty());
                            self.state = State::Paste { pasted, since };
                            return;
                        }
                    }
                }
            };
        }
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
            let step = decode(bytes, flush, self.expecting.is_some());
            // What the step read: all the bytes, unless it ended within them.
            work!(
                keys,
                match step {
                    Step::Done(n, _) | Step::PasteStart(n) => n,
                    Step::Overlong { .. } | Step::Incomplete => bytes.len(),
                }
            );
            let rest = match step {
                Step::Done(n, input) => {
                    if matches!(input, Some(Input::Reply(Reply::Attributes { .. }))) {
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
                    match paste(Pasted::Text(Vec::new()), text, self.paste_limit, out) {
                        Ok(rest) => Some(rest),
                        Err(pasted) => {
                            return State::Paste {
                                pasted,
                                since: None,
                            };
                        }
                    }
                }
                Step::Overlong { bel, csi, escape } => {
                    return State::Dropping {
                        bel,
                        escape,
                        csi,
                        since,
                    };
                }
                Step::Incomplete => {
                    work!(keys, bytes.len());
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

/// The key decoder's part of the invariant checker (THREATS.md section 6).
#[cfg(feature = "checks")]
impl Decoder {
    /// 35: an unfinished sequence held is no longer than the longest
    /// answer read, 258 bytes (B24), and a paste keeps at most its limit
    /// and the end that may begin its marker (B23).
    pub fn check(&self) -> crate::Checked {
        let mut report = crate::Checked::default();
        let held = HELD;
        let found = match &self.state {
            State::Pending { held: bytes, .. } => crate::check::ensure(bytes.len() <= held, || {
                format!(
                    "{} bytes of a sequence held, {held} at most (B24)",
                    bytes.len()
                )
            }),
            State::Idle | State::Dropping { .. } | State::Paste { .. } => Ok(()),
        };
        report.record("keys.held", "", found);
        let room = (self.paste_limit).saturating_add(PASTE_END.len().saturating_sub(1));
        let found = match &self.state {
            State::Paste {
                pasted: Pasted::Text(text) | Pasted::TooLong(text),
                ..
            } => crate::check::ensure(text.capacity() <= room, || {
                format!(
                    "room for {} bytes of paste, {room} at most (B23)",
                    text.capacity()
                )
            }),
            State::Idle | State::Pending { .. } | State::Dropping { .. } => Ok(()),
        };
        report.record("keys.paste", "", found);
        report
    }
}

fn press(key: Key, mods: Modifiers) -> Option<Input> {
    Some(Input::Key(KeyPress::new(key, mods).into()))
}

/// Whether `bytes` are the beginning of a character as UTF-8 may begin
/// one, well-formed (Unicode 17.0, 3.9, Table 3-7) but short of its bytes.
fn utf8_begun(bytes: &[u8]) -> bool {
    let Some((&first, rest)) = bytes.split_first() else {
        return false;
    };
    let (need, second) = match first {
        0xc2..=0xdf => (2, 0x80..=0xbf),
        0xe0 => (3, 0xa0..=0xbf),
        0xe1..=0xec | 0xee..=0xef => (3, 0x80..=0xbf),
        0xed => (3, 0x80..=0x9f),
        0xf0 => (4, 0x90..=0xbf),
        0xf1..=0xf3 => (4, 0x80..=0xbf),
        0xf4 => (4, 0x80..=0x8f),
        _ => return false,
    };
    let mut rest = rest.iter();
    bytes.len() < need
        && rest.next().is_none_or(|b| second.contains(b))
        && rest.all(|b| (0x80..=0xbf).contains(b))
}

/// One control byte or character, without an Escape prefix, and how many
/// bytes it takes; none while it is incomplete (until `flush`). Always
/// inlined into `decode`, as most keys are one byte.
#[inline(always)]
fn single(bytes: &[u8], flush: bool) -> Option<(usize, KeyPress)> {
    let first = *bytes.first()?;
    if let Some(control) = Control::of(first.into()) {
        return Some((1, KeyPress::plain(control.key())));
    }
    let (key, mods) = match first {
        0x00 => (Key::Char(' '), Modifiers::CTRL),
        // Ctrl-A to Ctrl-Z are the letters with bits 5 and 6 cleared.
        0x01..=0x1a => (Key::Char(char::from(first | 0x60)), Modifiers::CTRL),
        // Ctrl-\ to Ctrl-_ are those characters with bit 6 cleared.
        0x1c..=0x1f => (Key::Char(char::from(first | 0x40)), Modifiers::CTRL),
        0x20..=0x7e => (Key::Char(char::from(first)), Modifiers::NONE),
        _ => {
            let need = match first {
                0xc0..=0xdf => 2,
                0xe0..=0xef => 3,
                0xf0..=0xf7 => 4,
                _ => 1,
            };
            // A well-formed beginning waits for the rest until the deadline,
            // then is one U+FFFD.
            if !flush && utf8_begun(bytes) {
                return None;
            }
            let take = need.min(bytes.len());
            let text = bytes.get(..take).unwrap_or_default();
            // Ill-formed, it is one U+FFFD for its maximal subpart (Unicode
            // 3.9, "U+FFFD Substitution of Maximal Subparts"), which is
            // what std's `error_len` measures.
            return Some(match str::from_utf8(text) {
                Ok(s) => match s.chars().next() {
                    Some(c) => (take, KeyPress::plain(Key::Char(c))),
                    None => (1, KeyPress::plain(Key::Char('\u{fffd}'))),
                },
                Err(e) => {
                    let subpart = e.error_len().unwrap_or(take).max(1);
                    (subpart, KeyPress::plain(Key::Char('\u{fffd}')))
                }
            });
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
        b'O' => ss3(bytes, flush),
        // An Escape before a whole CSI or SS3 key is that key with Alt, as
        // libtermkey reads an Escape prefix, and rxvt and PuTTY send Alt with
        // the arrows; before anything else, or alone, an Escape, then the
        // second decoded on its own, as vim's users type `ESC ESC`.
        0x1b => escape_prefix(bytes, flush),
        // An answer only while one is expected: else `ESC P` is Alt-P at
        // once, and a string that began with it would be an answer read
        // whole and keys read in pieces. Its string ends with ST (`ESC \`);
        // an answer fux uses becomes a `Reply`, and anything else, or
        // anything cut short at the deadline or past `DCS_LIMIT`, is
        // dropped, never typed.
        b'P' if answers && dcs_begun(bytes) => answer(bytes, flush, false, DCS_LIMIT, dcs_reply),
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

/// `ESC O`: an SS3 key; else Alt-O, then what follows, once the deadline
/// has passed if nothing follows yet.
fn ss3(bytes: &[u8], flush: bool) -> Step {
    match bytes.get(2).map(|&last| final_key(last, Modifiers::NONE)) {
        None if !flush => Step::Incomplete,
        Some(Some(input)) => Step::Done(3, Some(input)),
        // A byte no final key names, or none at a flush: Alt-O, then the
        // byte.
        None | Some(None) => Step::Done(2, press(Key::Char('O'), Modifiers::ALT)),
    }
}

/// `ESC ESC`: an Escape before a whole CSI or SS3 key is that key with Alt;
/// before anything else, or alone, an Escape, and what follows is decoded
/// on its own. It calls `csi` and `ss3`, not `decode`, so that `decode` is
/// no recursive function and stays inlined into the decoder's loop.
#[cold]
#[inline(never)]
fn escape_prefix(bytes: &[u8], flush: bool) -> Step {
    let rest = bytes.get(1..).unwrap_or_default();
    let step = match bytes.get(2) {
        // `CSI ?` begins an answer, never a key: the Escape is no part of
        // it, and is not held while the answer is (B24).
        Some(b'[') if bytes.get(3) == Some(&b'?') => {
            return Step::Done(1, press(Key::Escape, Modifiers::NONE));
        }
        Some(b'[') => csi(rest, flush),
        Some(b'O') => ss3(rest, flush),
        None if !flush => return Step::Incomplete,
        _ => return Step::Done(1, press(Key::Escape, Modifiers::NONE)),
    };
    match step {
        // A whole key, more than the `ESC [` or `ESC O` that alone is Alt-[
        // or Alt-O.
        Step::Done(n, Some(Input::Key(mut stroke))) if n > 2 => {
            stroke.press.mods.alt = true;
            Step::Done(n.saturating_add(1), Some(Input::Key(stroke)))
        }
        // Past a key's window (32 bytes after `ESC [`) it is no key, and
        // the Escape is not held while it is (B24).
        Step::Incomplete if rest.len() <= 2 + 32 => Step::Incomplete,
        Step::Done(..) | Step::PasteStart(_) | Step::Overlong { .. } | Step::Incomplete => {
            Step::Done(1, press(Key::Escape, Modifiers::NONE))
        }
    }
}

/// The key a final letter names, after `SS3` or a `CSI` with parameters
/// (`CSI 1 ; m A`). The same letters are written in `encode.rs` (`legacy`
/// and `kitty`), which send them; `M`, Enter here, comes only after SS3,
/// as `CSI M` is a mouse report.
fn final_key(last: u8, mods: Modifiers) -> Option<Input> {
    let key = match last {
        b'M' => Key::Enter,
        _ => numbered_key(1, char::from(last))?,
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
    let body = bytes.get(2..).unwrap_or_default();
    match body.first() {
        None if !flush => return Step::Incomplete,
        Some(first) if first.is_ascii_digit() => {}
        _ => return Step::Done(2, press(Key::Char(']'), Modifiers::ALT)),
    }
    answer(bytes, flush, true, OSC_LIMIT, colour_reply)
}

/// The string of an answer in `bytes`, after its two-byte opening: ended
/// by ST (`ESC \`), or by BEL too if `bel`; what `reply` reads in it, if
/// it is at most `limit` bytes.
fn answer(
    bytes: &[u8],
    flush: bool,
    bel: bool,
    limit: usize,
    reply: fn(&[u8]) -> Option<Reply>,
) -> Step {
    let body = bytes.get(2..).unwrap_or_default();
    let end = body.iter().position(|b| *b == 0x1b || bel && *b == 0x07);
    let (payload, consumed) = match end.map(|i| (i, body.get(i).copied())) {
        Some((i, Some(0x07))) => (body.get(..i), i.checked_add(3)),
        Some((i, _)) => match body.get(i.saturating_add(1)) {
            Some(b'\\') => (body.get(..i), i.checked_add(4)),
            // ESC and something else: the string ends there, unfinished,
            // and the ESC begins what comes next.
            Some(_) => return Step::Done(i.saturating_add(2), None),
            None if flush => return Step::Done(bytes.len(), None),
            // The string and the ESC that may begin its ST past the limit:
            // no answer, whatever follows, and no more is held (B24).
            None if i >= limit => {
                return Step::Overlong {
                    bel,
                    csi: false,
                    escape: true,
                };
            }
            None => return Step::Incomplete,
        },
        // Cut short: dropped.
        None if flush => return Step::Done(bytes.len(), None),
        // Longer than any answer: dropped, the rest of it as it comes.
        None if body.len() > limit => {
            return Step::Overlong {
                bel,
                csi: false,
                escape: false,
            };
        }
        None => return Step::Incomplete,
    };
    // Within `bytes`: the string, its two-byte opening and its end.
    let consumed = consumed.unwrap_or(bytes.len()).min(bytes.len());
    // A string past the limit is no answer, whether it came whole or not:
    // with the BEL or the ESC that begins its end, at most `limit` bytes,
    // so that one held unfinished is never more than its opening and
    // `limit` (B24).
    let reply = payload
        .filter(|p| p.len() < limit)
        .and_then(reply)
        .map(Input::Reply);
    Step::Done(consumed, reply)
}

/// Whether `bytes` begin as fux's DCS answers do: `ESC P`, `0` or `1`, then
/// `$` (DECRPSS) or `+` (XTGETTCAP).
fn dcs_begun(bytes: &[u8]) -> bool {
    matches!(bytes.get(..4), Some([0x1b, b'P', b'0' | b'1', b'$' | b'+']))
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
            let index = str::from_utf8(params.next()?).ok()?.parse().ok()?;
            let rgb = Rgb::parse(params.next()?)?;
            Reply::Palette { index, rgb }
        }
        _ => return None,
    };
    params.next().is_none().then_some(reply)
}

/// `6 ; height ; width`: the size of a cell, answering `CSI 16 t`.
fn cell_size(params: &[u8]) -> Option<Reply> {
    let text = str::from_utf8(params).ok()?;
    let mut numbers = text.split(';');
    if numbers.next()? != "6" {
        return None;
    }
    let height = numbers.next()?.parse().ok().filter(|n| *n > 0)?;
    let width = numbers.next()?.parse().ok().filter(|n| *n > 0)?;
    numbers
        .next()
        .is_none()
        .then_some(Reply::CellSize { width, height })
}

/// A CSI answer from the terminal, `params` from its `?` on, ending with
/// `last`.
fn csi_reply(params: &[u8], last: u8) -> Option<Reply> {
    let text = str::from_utf8(params.get(1..)?).ok()?;
    match last {
        b'c' => Some(Reply::Attributes {
            sixel: text.split(';').any(|attribute| attribute == "4"),
        }),
        b'u' => {
            let flags = text.parse::<u32>().ok()? & u32::from(KITTY_FLAGS);
            Some(Reply::KittyFlags(u8::try_from(flags).unwrap_or(0)))
        }
        // A scheme's report, but for its `CSI` and its `n`.
        b'n' => (Scheme::ALL.into_iter())
            .find(|s| s.report().get(2..).and_then(|r| r.strip_suffix(b"n")) == Some(params))
            .map(Reply::Scheme),
        b'y' => {
            let (mode, status) = text.strip_suffix('$')?.split_once(';')?;
            let status = status.parse().ok()?;
            Some(Reply::Mode {
                mode: mode.parse().ok()?,
                status: ModeStatus::ALL.into_iter().find(|s| s.number() == status)?,
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
    let text = str::from_utf8(params.get(1..)?).ok()?;
    let mut numbers = text.split(';').map(|n| n.parse::<u32>().ok());
    let (code, x, y) = (numbers.next()??, numbers.next()??, numbers.next()??);
    if numbers.next().is_some() {
        return None;
    }
    mouse::event(code, x, y, release)
}

/// Where the rest of a CSI, `bytes`, ends: past its final byte, dropped
/// with it; or at a byte no control sequence holds (ECMA-48 5.4, 5.5: an
/// ESC, a control), which begins what comes next. `None` while neither has
/// come.
fn csi_end(bytes: &[u8]) -> Option<usize> {
    let end = bytes.iter().position(|b| !(0x20..=0x3f).contains(b))?;
    let final_byte = bytes.get(end).is_some_and(|b| (0x40..=0x7e).contains(b));
    Some(end.saturating_add(usize::from(final_byte)))
}

/// A CSI longer than its window (a key's, or a `CSI ?` answer's), `body`
/// its bytes after `ESC [`: no key nor answer, dropped through its final
/// byte once that is here, as an over-long OSC or DCS answer is dropped
/// to its end, none of it typed (B25); held until then, up to
/// `DCS_LIMIT` bytes, past which the rest is dropped as it comes, to its
/// final byte, and at a timeout what is held is dropped.
#[cold]
#[inline(never)]
fn long_answer(body: &[u8], flush: bool) -> Step {
    match csi_end(body) {
        Some(end) => Step::Done(end.saturating_add(2), None),
        None if flush => Step::Done(body.len().saturating_add(2), None),
        // Past what is held: the rest of it is dropped as it comes, to its
        // final byte, however it is split (THREATS.md B25).
        None if body.len() > DCS_LIMIT => Step::Overlong {
            bel: false,
            csi: true,
            escape: false,
        },
        None => Step::Incomplete,
    }
}

/// A CSI sequence: `ESC [ params final`. Too long a sequence is dropped.
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
    // A byte that can be no part of a control sequence (ECMA-48 5.4:
    // parameters 03/00 to 03/15, intermediates 02/00 to 02/15, the final
    // 04/00 to 07/14), an ESC (5.5) among them, ends it unfinished there:
    // `ESC [` alone is Alt-[, a sequence with parameters is dropped, and the
    // byte begins the next key.
    let first = window.iter().position(|b| !(0x20..=0x3f).contains(b));
    if let Some(at) = first
        && window.get(at).is_some_and(|b| !(0x40..=0x7e).contains(b))
    {
        return if at == 0 {
            Step::Done(2, press(Key::Char('['), Modifiers::ALT))
        } else {
            Step::Done(at.saturating_add(2), None)
        };
    }
    let Some(end) = first else {
        if window.len() > limit {
            return long_answer(body, flush);
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
    let last = body.get(end).copied().unwrap_or(0);
    if params.first() == Some(&b'<') {
        // An SGR mouse report: `CSI < code ; column ; row M`, `m` a release.
        return Step::Done(consumed, sgr_mouse(params, last).map(Input::Mouse));
    }
    if last == b't' {
        // No key ends in `t`: a window report, of which fux reads the cell
        // size alone.
        return Step::Done(consumed, cell_size(params).map(Input::Reply));
    }
    if params.first() == Some(&b'?') {
        // Answers fux does not use are dropped.
        return Step::Done(consumed, csi_reply(params, last).map(Input::Reply));
    }
    let groups = parts(params);
    let part = |group: usize, i: usize| groups.get(group)?.get(i).copied().flatten();
    let first = part(0, 0).unwrap_or(0);
    let modifier = part(1, 0).unwrap_or(1);
    // The kitty protocol's event type: a release is not a key press. fux
    // asks for none (`outer`), but a terminal may send them anyway.
    if part(1, 1) == Some(3) {
        return Step::Done(consumed, None);
    }
    // An xterm modifier parameter is `1 + bits`.
    let mods = Modifiers::from_bits(modifier.saturating_sub(1));
    // Every modifier bit, the kitty protocol's beyond xterm's three too.
    let bits = u8::try_from(modifier.saturating_sub(1)).unwrap_or(u8::MAX);
    let input = match last {
        // `CSI M` is a mouse report: Enter comes only after SS3.
        b'M' => None,
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
            4 | 8 => press(Key::End, mods),
            // F1 to F4 as some terminals number them.
            11..=14 => u8::try_from(first.wrapping_sub(10))
                .ok()
                .and_then(Function::new)
                .and_then(|f| press(Key::F(f), mods)),
            // xterm modifyOtherKeys: `CSI 27 ; mod ; code ~`.
            27 => part(2, 0)
                .and_then(|code| coded(code, mods, |c| (c, mods)))
                .map(|press| Input::Key(press.into())),
            // The others `encode.rs` sends (its `Form::Numbered`).
            _ => u8::try_from(first)
                .ok()
                .and_then(|n| press(numbered_key(n, '~')?, mods)),
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
            // The key's own code, which `typed` shifts.
            let unshifted = Modifiers {
                shift: false,
                ..mods
            };
            let typed = |c| (typed(c, &kitty), unshifted);
            let stroke = coded(first, mods, typed).map(|press| Keystroke {
                press,
                kitty: Some(kitty),
            });
            return Step::Done(consumed, stroke.map(Input::Key));
        }
        // The arrows, Home, End and F1 to F4 (`final_key`), or nothing.
        _ => final_key(last, mods),
    };
    // Modifiers beyond xterm's three (Super, Hyper, Meta, the locks) are
    // kept for a pane in the kitty protocol.
    let input = match input {
        Some(Input::Key(stroke)) if bits & !(SHIFT_BIT | ALT_BIT | CTRL_BIT) != 0 => {
            Some(Input::Key(Keystroke {
                kitty: Some(Kitty {
                    mods: bits,
                    ..Kitty::default()
                }),
                ..stroke
            }))
        }
        other => other,
    };
    Step::Done(consumed, input)
}

/// The first three parts of each of the first three parameters of a CSI
/// key, `params`, its parameters `;`-separated and their parts
/// `:`-separated, each a number if it is one (an empty part is none). No
/// key reads more, and nothing is allocated.
fn parts(params: &[u8]) -> [[Option<u32>; 3]; 3] {
    let mut groups = [[None; 3]; 3];
    for (group, text) in groups.iter_mut().zip(params.split(|&b| b == b';')) {
        for (part, text) in group.iter_mut().zip(text.split(|&b| b == b':')) {
            *part = str::from_utf8(text).ok().and_then(|t| t.parse().ok());
        }
    }
    groups
}

/// The key that `code` names in both forms that carry one, xterm's
/// modifyOtherKeys (`CSI 27 ; mods ; code ~`) and `CSI code ; mods u`,
/// held with `mods`: Enter, Tab, Escape and Backspace by their controls, 8
/// as Ctrl-Backspace (BS, which Ctrl makes of Backspace); a kitty
/// functional key by its number in the Private Use Area; else the
/// character `char_of` reads it as, with its modifiers, unless that is
/// another control, which names no key.
fn coded(
    code: u32,
    mods: Modifiers,
    char_of: impl FnOnce(char) -> (char, Modifiers),
) -> Option<KeyPress> {
    if code == u32::from(CTRL_BACKSPACE) {
        let mods = Modifiers { ctrl: true, ..mods };
        return Some(KeyPress::new(Key::Backspace, mods));
    }
    let key = match Control::of(code) {
        Some(control) => control.key(),
        None => match code {
            0xe000..=0xf8ff => functional(code)?,
            _ => {
                let (c, mods) = char_of(char::from_u32(code)?);
                return (!c.is_control()).then(|| KeyPress::new(Key::Char(c), mods));
            }
        },
    };
    Some(KeyPress::new(key, mods))
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
        .filter(|_| kitty.mods & (ALT_BIT | CTRL_BIT) != 0 && !c.is_ascii());
    let shift = kitty.mods & SHIFT_BIT != 0;
    // Caps Lock capitalizes a letter, as in a legacy terminal's bytes.
    let upper = shift != (kitty.mods & CAPS_LOCK_BIT != 0);
    let letter = |c: char| if upper { c.to_ascii_uppercase() } else { c };
    match base {
        Some(base) => letter(base),
        None if shift => kitty.shifted.and_then(char::from_u32).unwrap_or(letter(c)),
        None => letter(c),
    }
}

/// A kitty functional key, numbered in the Private Use Area, as the key
/// fux knows it by: a keypad key is the key it copies, as legacy terminals
/// send it. Keys fux has no name for (keypad Begin, the media, lock and
/// modifier keys) are none, and dropped.
fn functional(code: u32) -> Option<Key> {
    let key = match code {
        57376..=57398 => Key::F(Function::new(u8::try_from(code.checked_sub(57363)?).ok()?)?),
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

    /// Whether `d` waits on a timeout, once marked: a lone Escape, an
    /// incomplete sequence, an over-long string being dropped, or a paste
    /// under way.
    fn waiting(d: &mut Decoder) -> bool {
        d.mark(Instant::now());
        d.deadline().is_some()
    }
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
            (b"\x1d", "C-]"),
            (b"\x1e", "C-^"),
            (b"\x1f", "C-_"),
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
            (b"\x1b[27;2;97~", "S-a"),
        ] {
            assert_eq!(all(bytes), vec![key(name)], "{bytes:?}");
        }
        assert_eq!(presses(all(b"\x1b[105;5u")), vec![name_press("C-i")]);
    }

    fn name_press(name: &str) -> Option<KeyPress> {
        name.parse().ok()
    }

    /// Typed ill-formed UTF-8 is one U+FFFD a maximal subpart, as the
    /// Unicode Standard 17.0, 3.9's Tables 3-8 to 3-11 give it, whole or
    /// split anywhere: each table's bytes type its "Output" row.
    #[test]
    fn ill_formed_utf8_is_one_replacement_a_maximal_subpart() {
        let fffd = '\u{fffd}';
        let tables: [(&[u8], &[char]); 4] = [
            (
                b"\xc0\xaf\xe0\x80\xbf\xf0\x81\x82\x41",
                &[fffd, fffd, fffd, fffd, fffd, fffd, fffd, fffd, 'A'],
            ),
            (
                b"\xed\xa0\x80\xed\xbf\xbf\xed\xaf\x41",
                &[fffd, fffd, fffd, fffd, fffd, fffd, fffd, fffd, 'A'],
            ),
            (
                b"\xf4\x91\x92\x93\xff\x41\x80\xbf\x42",
                &[fffd, fffd, fffd, fffd, fffd, 'A', fffd, fffd, 'B'],
            ),
            (
                b"\xe1\x80\xe2\xf0\x91\x92\xf1\xbf\x41",
                &[fffd, fffd, fffd, fffd, 'A'],
            ),
        ];
        for (bytes, output) in tables {
            let expected: Vec<Input> = (output.iter())
                .map(|&c| Input::Key(KeyPress::plain(Key::Char(c)).into()))
                .collect();
            assert_eq!(all(bytes), expected, "{bytes:x?}");
            split_anywhere(bytes, false, &expected);
        }
    }

    /// A character typed a byte at a time, as a slow link or a terminal
    /// that writes each byte apart sends it, waits for its bytes as long as
    /// an answer (`REPLY_DELAY`), alone or after an Escape (Alt); at its
    /// deadline it is one U+FFFD, so it never holds the keys after it. A
    /// byte that cannot go on with it gives the maximal subpart's U+FFFD at
    /// once.
    #[test]
    fn a_character_split_across_reads_waits_for_its_bytes() {
        for (bytes, expected) in [
            ("\u{1f600}".as_bytes(), key("\u{1f600}")),
            ("\x1b\u{1f600}".as_bytes(), key("M-\u{1f600}")),
        ] {
            // An Escape and the lead byte come in one read, as Alt sends
            // them; the rest a byte a read.
            let first = if bytes.first() == Some(&0x1b) { 2 } else { 1 };
            let (head, tail) = bytes.split_at_checked(first).unwrap_or((bytes, &[]));
            let reads = std::iter::once(head).chain(tail.iter().map(std::slice::from_ref));
            let mut d = Decoder::default();
            let mut out = Vec::new();
            let t0 = Instant::now();
            for read in reads {
                d.bytes(read, &mut out);
                d.mark(t0);
                if out.is_empty() {
                    assert_eq!(d.deadline(), Some(after(t0, REPLY_DELAY)), "{bytes:x?}");
                }
            }
            assert_eq!(out, vec![expected], "{bytes:x?}");
        }
        let mut d = Decoder::default();
        let mut out = Vec::new();
        d.bytes(b"\xe4\xb8", &mut out);
        d.bytes(b"A", &mut out);
        assert_eq!(out, vec![key("\u{fffd}"), key("A")]);
        // Cut short by its deadline: a mouse report's ESC after it is still
        // one.
        out.clear();
        d.bytes(b"\xe9", &mut out);
        d.timeout(&mut out);
        assert_eq!(out, vec![key("\u{fffd}")]);
        d.bytes(b"\x1b[<0;10;5M", &mut out);
        let press = crate::keys::mouse::MouseAction::Press(crate::keys::mouse::MouseButton::Left);
        let (mods, row, col) = (Modifiers::NONE, 4, 9);
        let click = Input::Mouse(MouseEvent {
            action: press,
            mods,
            row,
            col,
        });
        assert_eq!(out, vec![key("\u{fffd}"), click]);
    }

    /// xterm's modifyOtherKeys form names a key by the code `CSI code ;
    /// mods u` does: Enter, Tab, Escape and Backspace too, and 8 as
    /// Ctrl-Backspace, which `encode.rs` sends for it.
    #[test]
    fn both_coded_forms_name_the_same_keys() {
        for (code, mods, name) in [
            (13, 5, "C-Enter"),
            (13, 2, "S-Enter"),
            (9, 5, "C-Tab"),
            (27, 3, "M-Escape"),
            (127, 5, "C-BSpace"),
            (127, 3, "M-BSpace"),
            (8, 5, "C-BSpace"),
            (8, 6, "C-S-BSpace"),
            // BS is Ctrl's whatever else is held, none or Alt.
            (8, 1, "C-BSpace"),
            (8, 3, "C-M-BSpace"),
            (97, 5, "C-a"),
        ] {
            for form in [
                format!("\x1b[27;{mods};{code}~"),
                format!("\x1b[{code};{mods}u"),
            ] {
                let press = presses(all(form.as_bytes()));
                assert_eq!(press, vec![name_press(name)], "{form:?}");
            }
        }
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

    /// A bracketed paste whose end marker never comes ends `PASTE_DELAY`
    /// after its last bytes: what came of it is the paste, or, past the
    /// limit, reported too long as soon as it is, the rest dropped; and
    /// what is typed after it is keys again.
    #[test]
    fn an_unfinished_paste_ends_at_its_deadline() {
        let ms = Duration::from_millis;
        let t0 = Instant::now();
        let mut d = Decoder::with_paste_limit(8);
        let mut out = Vec::new();
        d.bytes(b"\x1b[200~abc", &mut out);
        d.mark(t0);
        assert_eq!(d.deadline(), Some(t0 + PASTE_DELAY));
        // More of it: its wait begins again.
        d.bytes(b"de", &mut out);
        d.mark(t0 + ms(1000));
        assert_eq!(d.deadline(), Some(t0 + ms(1000) + PASTE_DELAY));
        assert!(out.is_empty());
        d.timeout(&mut out);
        d.bytes(b"k", &mut out);
        assert_eq!(out, vec![Input::Paste("abcde".into()), key("k")]);
        let mut out = Vec::new();
        d.bytes(b"\x1b[200~0123456789abcdef", &mut out);
        assert_eq!(out, vec![Input::PasteTooLong]);
        d.mark(t0);
        d.bytes(b"more", &mut out);
        d.mark(t0 + ms(1000));
        assert_eq!(d.deadline(), Some(t0 + ms(1000) + PASTE_DELAY));
        d.timeout(&mut out);
        d.bytes(b"k", &mut out);
        assert_eq!(out, vec![Input::PasteTooLong, key("k")]);
    }

    #[test]
    fn escape_waits_for_the_deadline() {
        let mut d = Decoder::default();
        let mut out = Vec::new();
        d.bytes(b"\x1b", &mut out);
        assert!(out.is_empty());
        assert!(waiting(&mut d));
        d.timeout(&mut out);
        assert_eq!(out, vec![key("Escape")]);
        assert!(!waiting(&mut d));
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
            (b"\x1b[97;68u", "M-a"),
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
            // F13 to F35 (the kitty spec's "Functional key definitions").
            (b"\x1b[57376u", "F13"),
            (b"\x1b[57398;5u", "C-F35"),
            (b"\x1b[97;5:2u", "C-a"),
        ] {
            assert_eq!(presses(all(bytes)), vec![name_press(name)], "{bytes:?}");
        }
        // A release is not a press, nor a key fux has no name for (a media
        // key, a modifier alone).
        for bytes in [
            &b"\x1b[97;5:3u"[..],
            b"\x1b[1;1:3A",
            b"\x1b[57428u",
            b"\x1b[57441;2u",
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
        assert!(!waiting(&mut d));
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
                let legacy = KeyMode::Legacy {
                    application: false,
                    keypad: false,
                    other_keys: None,
                    backarrow: false,
                };
                if decoded(legacy) != vec![Some(press)] {
                    // Legacy bytes cannot tell this key (S-Enter is Enter).
                    continue;
                }
                let kitty = KeyMode::Kitty {
                    alternate: true,
                    report: Report::Disambiguated,
                    backarrow: false,
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
                status: ModeStatus::Reset,
            }),
            Input::Reply(Reply::Scheme(Scheme::Dark)),
            key("c"),
            Input::Reply(Reply::Scheme(Scheme::Light)),
            Input::Reply(Reply::KittyFlags(5)),
            Input::Reply(Reply::Attributes { sixel: false }),
            key("Up"),
            key("d"),
        ];
        assert_eq!(all(stream), expected);
        for expecting in [false, true] {
            split_anywhere(stream, expecting, &expected);
        }
        // Of the flags, only the spec's five are read.
        assert_eq!(all(b"\x1b[?999u"), [Input::Reply(Reply::KittyFlags(7))]);
        // Colours fux does not ask for or cannot read are dropped; answers
        // it does not use reach the session, which ignores them: no keys.
        assert_eq!(
            all(b"\x1b[?1;2c\x1b]12;rgb:0/0/0\x07\x1b]11;red\x07\x1b]4;256;rgb:0/0/0\x07\x1b]4;x;rgb:0/0/0\x07\x1b]4;1\x07\x1b[?6;1$yx"),
            vec![
                Input::Reply(Reply::Attributes { sixel: false }),
                Input::Reply(Reply::Mode {
                    mode: 6,
                    status: ModeStatus::Set,
                }),
                key("x")
            ]
        );
    }

    /// While the terminal's latest flags hold disambiguate, it sends Escape
    /// as `CSI 27 u`: a bare ESC is a sequence begun, which waits for its
    /// rest with no deadline, however late it comes. Flags without it, even
    /// after flags with it, say nothing of Escape.
    #[test]
    fn an_escape_the_terminal_sends_as_csi_u_is_never_waited_out() {
        let t0 = Instant::now();
        let waits = |answers: &[&[u8]]| {
            let mut d = Decoder::default();
            let mut out = Vec::new();
            for flags in answers {
                d.bytes(flags, &mut out);
            }
            d.bytes(b"\x1b", &mut out);
            d.mark(t0);
            let due = d.deadline();
            d.bytes(b"[27u", &mut out);
            let escape = matches!(out.pop(), Some(Input::Key(k)) if k.press.key == Key::Escape);
            (due, escape)
        };
        let waited = (Some(t0 + ESCAPE_DELAY), true);
        assert_eq!(waits(&[b"\x1b[?0u"]), waited);
        assert_eq!(waits(&[b"\x1b[?5u"]), (None, true));
        assert_eq!(waits(&[b"\x1b[?5u", b"\x1b[?0u"]), waited);
        assert_eq!(waits(&[b"\x1b[?5u\x1b[?0u"]), waited);
        assert_eq!(waits(&[b"\x1b[?0u\x1b[?1u"]), (None, true));
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

    /// A byte that can be no part of a control sequence (ECMA-48 5.4: its
    /// parameter bytes are 03/00 to 03/15, intermediates 02/00 to 02/15, the
    /// final 04/00 to 07/14) ends a CSI unfinished there, as an ESC does
    /// (5.5): `ESC [` alone is Alt-[, with parameters it is dropped, and the
    /// byte begins the next key, at once, not at the deadline.
    #[test]
    fn a_byte_no_control_sequence_holds_ends_a_csi_and_begins_the_next_key() {
        assert_eq!(
            all("\x1b[\u{e9}a".as_bytes()),
            vec![key("M-["), key("\u{e9}"), key("a")]
        );
        assert_eq!(all(b"\x1b[12\ra"), vec![key("Enter"), key("a")]);
        assert_eq!(all(b"\x1b[1;2\x7fA"), vec![key("BSpace"), key("A")]);
        let mut d = Decoder::default();
        let mut out = Vec::new();
        d.bytes(b"\x1b[\r", &mut out);
        assert_eq!(out, vec![key("M-["), key("Enter")], "before the deadline");
        split_anywhere(
            "\x1b[\u{e9}a".as_bytes(),
            false,
            &[key("M-["), key("\u{e9}"), key("a")],
        );
    }

    /// An Escape before a whole CSI or SS3 key is that key with Alt, as
    /// libtermkey reads it (`termkey.c`, an Escape prefix) and as rxvt and
    /// PuTTY send Alt with the arrows; `ESC ESC` alone, and before anything
    /// else, stays an Escape and what follows, as vim's users type it.
    #[test]
    fn an_escape_before_a_key_sequence_is_alt() {
        assert_eq!(all(b"\x1b\x1b[A"), vec![key("M-Up")]);
        assert_eq!(all(b"\x1b\x1bOB"), vec![key("M-Down")]);
        assert_eq!(all(b"\x1b\x1b[1;5C"), vec![key("C-M-Right")]);
        assert_eq!(all(b"\x1b\x1b[3~"), vec![key("M-Delete")]);
        assert_eq!(all(b"\x1b\x1b"), vec![key("Escape"), key("Escape")]);
        assert_eq!(all(b"\x1b\x1bx"), vec![key("Escape"), key("M-x")]);
        assert_eq!(all(b"\x1b\x1b[I"), vec![key("Escape"), Input::FocusIn]);
        split_anywhere(b"\x1b\x1b[A", false, &[key("M-Up")]);
        // Cut short at the deadline, `ESC [` and `ESC O` are no whole key
        // but Alt-[ and Alt-O: the Escape before them is an Escape.
        assert_eq!(all(b"\x1b\x1b["), vec![key("Escape"), key("M-[")]);
        assert_eq!(all(b"\x1b\x1bO"), vec![key("Escape"), key("M-O")]);
    }

    /// A CSI past its window that an ESC or a control cuts short ends there,
    /// unfinished, and the byte begins what comes next (ECMA-48 5.5), whole
    /// or split: the Up after a long one is typed.
    #[test]
    fn an_over_long_csi_cut_short_by_an_esc_drops_only_itself() {
        let mut long = b"\x1b[".to_vec();
        long.extend(std::iter::repeat_n(b'1', 40));
        long.extend_from_slice(b"\x1b[A");
        assert_eq!(all(&long), vec![key("Up")]);
        let mut held = b"\x1b[".to_vec();
        held.extend(std::iter::repeat_n(b'1', 300));
        held.extend_from_slice(b"\x1b[A");
        assert_eq!(all(&held), vec![key("Up")]);
        split_anywhere(&long, false, &[key("Up")]);
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
    /// 5 u`, xterm's `CSI 27 ; 5 ; 1 ~`), or whose kitty alternate key
    /// that it would type is one (a base-layout key 1 with Ctrl, a shifted
    /// key 1), is no key: a key name cannot say it, so no binding could
    /// match it and nothing could name it back.
    #[test]
    fn a_key_report_of_a_control_character_is_no_key() {
        for report in [
            &b"\x1b[u"[..],
            b"\x1b[1;5u",
            b"\x1b[0;5u",
            b"\x1b[27;5;1~",
            b"\x1b[1089::1;5u",
            b"\x1b[97:1;2u",
        ] {
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
        assert_eq!(out, vec![Input::Reply(Reply::Attributes { sixel: false })]);
        let mut d = Decoder::default();
        d.expect(t0);
        d.expire(t0 + REPLY_WINDOW);
        assert_eq!(waits(&mut d, b"\x1b]", &mut out), Some(ESCAPE_DELAY));
        // Two rounds of questions: the first DA1 answer leaves the second
        // expected, and the second ends it.
        let mut d = Decoder::default();
        d.expect(t0);
        d.expect(t0);
        let da1 = b"\x1b[?1;2c\x1b]";
        assert_eq!(waits(&mut d, da1, &mut out), Some(REPLY_DELAY));
        d.timeout(&mut out);
        assert_eq!(waits(&mut d, da1, &mut out), Some(ESCAPE_DELAY));
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
        assert!(!waiting(&mut d));
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
        use MouseButton::Left;
        let left = |action, row, col| {
            Input::Mouse(MouseEvent {
                action,
                mods: Modifiers::NONE,
                row,
                col,
            })
        };
        assert_eq!(
            all(b"\x1b[M !!a"),
            vec![left(MouseAction::Press(Left), 0, 0), key("a")]
        );
        assert_eq!(
            all(b"\x1b[<0;10;5Ma\x1b[<0;10;5m"),
            vec![
                left(MouseAction::Press(Left), 4, 9),
                key("a"),
                left(MouseAction::Release(Some(Left)), 4, 9)
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
    fn a_key_csi_at_the_end_of_its_window_waits_for_its_final_byte() {
        // A kitty key whose parameters fill the window, 32 bytes, its final
        // byte in the next read: one key (the window's guard).
        let mut d = Decoder::default();
        let mut out = Vec::new();
        let digits: Vec<u8> = std::iter::repeat_n(b'0', 30).chain(*b"97").collect();
        d.bytes(&[b"\x1b[".as_slice(), &digits].concat(), &mut out);
        d.bytes(b"u", &mut out);
        d.timeout(&mut out);
        assert!(
            matches!(out.as_slice(), [Input::Key(k)] if k.press.key == Key::Char('a')),
            "{out:?}"
        );
    }

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
        // A `CSI ?` answer past what is held, dropped through its final
        // byte: at the parent, read in pieces, its digits were typed.
        let mut answer = b"\x1b[?".to_vec();
        answer.extend(std::iter::repeat_n(b'1', DCS_LIMIT + 20));
        streams.push((
            [answer.as_slice(), b"cxy"].concat(),
            vec![key("x"), key("y")],
        ));
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

/// The key decoder's checks, each shown a decoder with its one fault.
#[cfg(all(test, feature = "checks"))]
mod faults {
    use super::*;

    #[test]
    fn a_long_sequence_held_or_a_paste_with_room_past_its_limit_are_faults() {
        let mut decoder = Decoder::default();
        assert!(decoder.check().is_clean());
        decoder.state = State::Pending {
            held: vec![b'1'; DCS_LIMIT.saturating_add(3)],
            since: None,
        };
        assert!(decoder.check().faults_of("keys.held").next().is_some());
        decoder.state = State::Paste {
            pasted: Pasted::Text(Vec::with_capacity(PASTE_LIMIT.saturating_mul(2))),
            since: None,
        };
        assert!(decoder.check().faults_of("keys.paste").next().is_some());
    }
}
