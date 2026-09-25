//! Direct DEC ANSI transition parser, based on Paul Williams's transition
//! model (reference and attribution in the crate README), with UTF-8 ground decoding.
//! Ignored control strings retain no payload. No parser dependency is used.

use crate::{Error, Screen};

#[cfg(test)]
#[path = "../tests/corpus/mod.rs"]
mod test_corpus;

#[derive(Clone, Debug)]
pub(crate) struct Parameters {
    values: [u16; 32],
    /// True when this value continues the previous colon-delimited group.
    sub: [bool; 32],
    len: usize,
}
impl Default for Parameters {
    fn default() -> Self {
        Self {
            values: [0; 32],
            sub: [false; 32],
            len: 1,
        }
    }
}
impl Parameters {
    fn digit(&mut self, byte: u8) {
        // A value too large for a u16 stays at its largest.
        if let Some(n) = self.len.checked_sub(1).and_then(|i| self.values.get_mut(i))
            && let Some(digit) = byte.checked_sub(b'0')
        {
            *n = n.saturating_mul(10).saturating_add(u16::from(digit));
        }
    }
    fn separator(&mut self, colon: bool) -> bool {
        let Some(len) = self.len.checked_add(1).filter(|n| *n <= self.values.len()) else {
            return false;
        };
        if let Some(sub) = self.sub.get_mut(self.len) {
            *sub = colon;
        }
        self.len = len;
        true
    }
    pub fn groups(&self) -> impl Iterator<Item = &[u16]> {
        let mut start = 0;
        std::iter::from_fn(move || {
            if start >= self.len {
                return None;
            }
            let mut end = start.checked_add(1)?;
            while end < self.len && self.sub.get(end).copied().unwrap_or(false) {
                end = end.checked_add(1)?;
            }
            let result = self.values.get(start..end);
            start = end;
            result
        })
    }
    pub fn first(&self, index: usize, default: u16) -> u16 {
        let value = self
            .groups()
            .nth(index)
            .and_then(|g| g.first())
            .copied()
            .unwrap_or(0);
        if value == 0 { default } else { value }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum State {
    #[default]
    Ground,
    Escape,
    EscapeIntermediate,
    CsiEntry,
    CsiParam,
    CsiIntermediate,
    CsiIgnore,
    DcsEntry,
    DcsParam,
    DcsIntermediate,
    DcsIgnore,
    DcsString,
    OscString,
    SosPmApcString,
}

/// The most OSC payload bytes retained for [`Event`] delivery. A longer OSC
/// string is consumed without an event; nothing beyond this is ever buffered.
pub const OSC_PAYLOAD_LIMIT: usize = 64 * 1024;

/// Opt-in outputs beyond the screen. The default (everything off) is fux's
/// policy: child output causes no title, bell or clipboard side effects, OSC
/// payloads are never retained, and only DSR 5n/6n and primary DA are answered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Options {
    /// Deliver OSC 0/1/2 (icon name / window title), OSC 52 (clipboard) and BEL
    /// as [`Event`]s. OSC payloads are buffered up to [`OSC_PAYLOAD_LIMIT`].
    pub events: bool,
    /// Also answer DECRQM (`CSI ? Ps $ p` and `CSI Ps $ p`), DECXCPR
    /// (`CSI ? 6 n`) and secondary device attributes (`CSI > c`).
    pub extended_replies: bool,
}

/// A side effect requested by child output. Only delivered with
/// [`Options::events`]; payloads are raw bytes, bounded by [`OSC_PAYLOAD_LIMIT`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Event<'a> {
    /// OSC 0 or OSC 2.
    Title(&'a [u8]),
    /// OSC 0 or OSC 1.
    IconName(&'a [u8]),
    /// BEL executed outside a control string.
    Bell,
    /// OSC 52 set request. `data` is the payload as sent (normally base64);
    /// a `?` query is not an event.
    Clipboard { selection: &'a [u8], data: &'a [u8] },
}

/// Receives terminal query replies and, with [`Options::events`], events. Both
/// default to discarding, so an implementation handles only what it needs.
pub trait Sink {
    fn reply(&mut self, _bytes: &[u8]) {}
    fn event(&mut self, _event: Event<'_>) {}
}

struct Replies<F>(F);
impl<F: FnMut(&[u8])> Sink for Replies<F> {
    fn reply(&mut self, bytes: &[u8]) {
        (self.0)(bytes);
    }
}

/// Incremental byte parser and its terminal state. Replies and events are
/// delivered synchronously to a caller-supplied sink; no unbounded queue exists.
#[derive(Clone, Debug)]
pub struct Parser {
    screen: Screen,
    options: Options,
    /// OSC payload, collected only with `options.events`, at most
    /// `OSC_PAYLOAD_LIMIT` bytes.
    osc: Vec<u8>,
    osc_overflow: bool,
    state: State,
    params: Parameters,
    intermediates: [u8; 2],
    intermediate_len: usize,
    ignoring: bool,
    utf8: [u8; 4],
    utf8_len: usize,
    utf8_need: usize,
}
#[cfg(test)]
mod tests;

impl Parser {
    pub fn new(rows: u16, cols: u16, history_lines: usize) -> Result<Self, Error> {
        Self::with_options(rows, cols, history_lines, Options::default())
    }
    pub fn with_options(
        rows: u16,
        cols: u16,
        history_lines: usize,
        options: Options,
    ) -> Result<Self, Error> {
        Ok(Self {
            screen: Screen::new(rows, cols, history_lines)?,
            options,
            osc: Vec::new(),
            osc_overflow: false,
            state: State::Ground,
            params: Parameters::default(),
            intermediates: [0; 2],
            intermediate_len: 0,
            ignoring: false,
            utf8: [0; 4],
            utf8_len: 0,
            utf8_need: 0,
        })
    }
    pub fn screen(&self) -> &Screen {
        &self.screen
    }
    pub fn resize(&mut self, rows: u16, cols: u16) -> Result<(), Error> {
        self.screen.resize(rows, cols)
    }
    pub fn options(&self) -> Options {
        self.options
    }
    /// Process output while deliberately discarding terminal query replies.
    pub fn process(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.process_with_replies(bytes, |_| {})
    }
    pub fn process_with_replies(
        &mut self,
        bytes: &[u8],
        reply: impl FnMut(&[u8]),
    ) -> Result<(), Error> {
        self.process_with(bytes, &mut Replies(reply))
    }
    /// Process output, delivering replies and (with [`Options::events`]) events.
    pub fn process_with(&mut self, bytes: &[u8], sink: &mut impl Sink) -> Result<(), Error> {
        if bytes.is_empty() {
            return Ok(());
        }
        self.screen.begin()?;
        let mut remaining = bytes;
        while let Some((&byte, tail)) = remaining.split_first() {
            if self.state == State::Ground && self.utf8_len == 0 && (0x20..=0x7e).contains(&byte) {
                let length = remaining
                    .iter()
                    .take_while(|b| (0x20..=0x7e).contains(*b))
                    .count();
                self.screen
                    .ascii(remaining.get(..length).unwrap_or_default())?;
                remaining = remaining.get(length..).unwrap_or_default();
            } else {
                self.byte(byte, sink)?;
                remaining = tail;
            }
        }
        Ok(())
    }
    fn reset_sequence(&mut self) {
        self.params = Parameters::default();
        self.intermediate_len = 0;
        self.ignoring = false;
    }
    fn collect(&mut self, byte: u8) {
        if let Some(slot) = self.intermediates.get_mut(self.intermediate_len)
            && let Some(len) = self.intermediate_len.checked_add(1)
        {
            *slot = byte;
            self.intermediate_len = len;
        } else {
            self.ignoring = true;
        }
    }

    fn ground(&mut self, byte: u8, sink: &mut impl Sink) -> Result<(), Error> {
        if self.utf8_len != 0 {
            let valid_continuation = (0x80..=0xbf).contains(&byte)
                && (self.utf8_len != 1
                    || match self.utf8.first().copied() {
                        Some(0xe0) => byte >= 0xa0,
                        Some(0xed) => byte < 0xa0,
                        Some(0xf0) => byte >= 0x90,
                        Some(0xf4) => byte < 0x90,
                        _ => true,
                    });
            // `utf8_need` is at most 4, so a continuation always has a slot.
            if valid_continuation
                && let Some(len) = self.utf8_len.checked_add(1)
                && let Some(slot) = self.utf8.get_mut(self.utf8_len)
            {
                *slot = byte;
                self.utf8_len = len;
                if self.utf8_len == self.utf8_need {
                    let scalar = self
                        .utf8
                        .get(..self.utf8_len)
                        .and_then(|s| std::str::from_utf8(s).ok())
                        .and_then(|s| s.chars().next());
                    self.utf8_len = 0;
                    if let Some(c) = scalar {
                        self.screen.print(c)?;
                    }
                }
                return Ok(());
            }
            // Discard the invalid prefix, then reprocess this byte; it can be ESC.
            self.utf8_len = 0;
        }
        match byte {
            0x1b => {
                self.reset_sequence();
                self.state = State::Escape;
            }
            0x00..=0x1f | 0x7f => self.control(byte, sink)?,
            0x20..=0x7e => self.screen.print(char::from(byte))?,
            0xc2..=0xf4 => {
                self.utf8_need = if byte < 0xe0 {
                    2
                } else if byte < 0xf0 {
                    3
                } else {
                    4
                };
                if let Some(slot) = self.utf8.first_mut() {
                    *slot = byte;
                }
                self.utf8_len = 1;
            }
            _ => {}
        }
        Ok(())
    }

    /// Execute a C0 control. BEL becomes an event when events are enabled.
    fn control(&mut self, byte: u8, sink: &mut impl Sink) -> Result<(), Error> {
        if byte == 7 && self.options.events {
            sink.event(Event::Bell);
        }
        self.screen.control(byte)
    }

    fn byte(&mut self, byte: u8, sink: &mut impl Sink) -> Result<(), Error> {
        if self.state == State::Ground {
            return self.ground(byte, sink);
        }
        // Anywhere transitions: CAN/SUB cancel, ESC starts a new sequence. ESC
        // also begins ST, so it completes a pending OSC string.
        if matches!(byte, 0x18 | 0x1a) {
            self.state = State::Ground;
            return Ok(());
        }
        if byte == 0x1b {
            if self.state == State::OscString {
                self.dispatch_osc(sink);
            }
            self.reset_sequence();
            self.state = State::Escape;
            return Ok(());
        }
        match self.state {
            State::Ground => self.ground(byte, sink)?,
            State::OscString => {
                if byte == 7 {
                    self.dispatch_osc(sink);
                    self.state = State::Ground;
                } else if self.options.events && !self.osc_overflow {
                    if self.osc.len() < OSC_PAYLOAD_LIMIT {
                        self.osc.push(byte);
                    } else {
                        self.osc.clear();
                        self.osc_overflow = true;
                    }
                }
            }
            State::DcsString => {
                if byte == 0x9c {
                    self.state = State::Ground;
                }
            }
            State::SosPmApcString | State::DcsIgnore => {}
            State::Escape | State::EscapeIntermediate => match byte {
                0x00..=0x1f => self.control(byte, sink)?,
                0x20..=0x2f => {
                    self.collect(byte);
                    self.state = State::EscapeIntermediate;
                }
                0x30..=0x7e => {
                    if self.state == State::Escape {
                        self.state = match byte {
                            b'[' => State::CsiEntry,
                            b'P' => State::DcsEntry,
                            b']' => {
                                self.osc.clear();
                                self.osc_overflow = false;
                                State::OscString
                            }
                            b'X' | b'^' | b'_' => State::SosPmApcString,
                            _ => State::Ground,
                        };
                    } else {
                        self.state = State::Ground;
                    }
                    if self.state == State::Ground && !self.ignoring {
                        let intermediates = self.intermediates;
                        self.screen.escape(
                            intermediates
                                .get(..self.intermediate_len)
                                .unwrap_or_default(),
                            byte,
                        )?;
                    }
                }
                _ => {}
            },
            State::CsiEntry
            | State::CsiParam
            | State::CsiIntermediate
            | State::CsiIgnore
            | State::DcsEntry
            | State::DcsParam
            | State::DcsIntermediate => {
                let dcs = matches!(
                    self.state,
                    State::DcsEntry | State::DcsParam | State::DcsIntermediate
                );
                let entry = matches!(self.state, State::CsiEntry | State::DcsEntry);
                let intermediate =
                    matches!(self.state, State::CsiIntermediate | State::DcsIntermediate);
                let ignore_state = if dcs {
                    State::DcsIgnore
                } else {
                    State::CsiIgnore
                };
                let param_state = if dcs {
                    State::DcsParam
                } else {
                    State::CsiParam
                };
                match byte {
                    0x00..=0x1f => {
                        if !dcs {
                            self.control(byte, sink)?;
                        }
                    }
                    0x20..=0x3f if self.state == State::CsiIgnore => {}
                    0x20..=0x2f => {
                        self.collect(byte);
                        self.state = if dcs {
                            State::DcsIntermediate
                        } else {
                            State::CsiIntermediate
                        };
                    }
                    0x30..=0x3f if intermediate => self.state = ignore_state,
                    b'0'..=b'9' => {
                        self.params.digit(byte);
                        self.state = param_state;
                    }
                    b';' | b':' => {
                        self.state = if self.params.separator(byte == b':') {
                            param_state
                        } else {
                            ignore_state
                        };
                    }
                    0x3c..=0x3f if entry => {
                        self.collect(byte);
                        self.state = param_state;
                    }
                    0x3c..=0x3f => self.state = ignore_state,
                    0x40..=0x7e => {
                        if dcs {
                            self.state = State::DcsString;
                        } else {
                            let dispatch = self.state != State::CsiIgnore && !self.ignoring;
                            self.state = State::Ground;
                            if dispatch {
                                let intermediates = self.intermediates;
                                let intermediates = intermediates
                                    .get(..self.intermediate_len)
                                    .unwrap_or_default();
                                let answer =
                                    match self.screen.csi(&self.params, intermediates, byte)? {
                                        Some(bytes) => Some(bytes),
                                        None if self.options.extended_replies => {
                                            self.extended_reply(intermediates, byte)
                                        }
                                        None => None,
                                    };
                                if let Some(bytes) = answer {
                                    sink.reply(&bytes);
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    /// Deliver the completed OSC string as events. Only 0/1/2/52 are recognized;
    /// an overflowed or unrecognized string is dropped.
    fn dispatch_osc(&mut self, sink: &mut impl Sink) {
        if !self.options.events || self.osc_overflow {
            return;
        }
        let payload = std::mem::take(&mut self.osc);
        let (command, rest) = match payload.iter().position(|b| *b == b';') {
            Some(i) => (
                payload.get(..i).unwrap_or_default(),
                payload
                    .get(i..)
                    .and_then(|r| r.get(1..))
                    .unwrap_or_default(),
            ),
            None => (payload.as_slice(), &[][..]),
        };
        match command {
            b"0" => {
                sink.event(Event::IconName(rest));
                sink.event(Event::Title(rest));
            }
            b"1" => sink.event(Event::IconName(rest)),
            b"2" => sink.event(Event::Title(rest)),
            b"52" => {
                if let Some(i) = rest.iter().position(|b| *b == b';') {
                    let selection = rest.get(..i).unwrap_or_default();
                    let data = rest.get(i..).and_then(|r| r.get(1..)).unwrap_or_default();
                    if data != b"?" {
                        sink.event(Event::Clipboard { selection, data });
                    }
                }
            }
            _ => {}
        }
        // Reuse the allocation for the next OSC string.
        self.osc = payload;
        self.osc.clear();
    }

    /// Replies enabled by [`Options::extended_replies`] for CSI sequences the
    /// screen does not answer itself.
    fn extended_reply(&self, intermediates: &[u8], byte: u8) -> Option<Vec<u8>> {
        let n = self.params.first(0, 0);
        match (intermediates, byte) {
            (b"?", b'n') if n == 6 => {
                let (row, col) = self.screen.cursor_position();
                Some(format!("\x1b[?{};{}R", u32::from(row) + 1, u32::from(col) + 1).into_bytes())
            }
            (b">", b'c') if n == 0 => Some(b"\x1b[>1;10;0c".to_vec()),
            (b"?$", b'p') => {
                let status = self.screen.private_mode_status(n);
                Some(format!("\x1b[?{n};{status}$y").into_bytes())
            }
            (b"$", b'p') => Some(format!("\x1b[{n};0$y").into_bytes()),
            _ => None,
        }
    }
}
