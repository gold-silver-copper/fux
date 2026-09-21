//! Direct DEC ANSI transition parser, based on Paul Williams's transition
//! model (https://vt100.net/emu/dec_ansi_parser), with UTF-8 ground decoding.
//! Ignored control strings retain no payload. No parser dependency is used.

use crate::{Error, Screen};

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
        if let Some(n) = self.values.get_mut(self.len - 1) {
            *n = n.saturating_mul(10).saturating_add(u16::from(byte - b'0'));
        }
    }
    fn separator(&mut self, colon: bool) -> bool {
        if self.len == self.values.len() {
            return false;
        }
        if let Some(sub) = self.sub.get_mut(self.len) {
            *sub = colon;
        }
        self.len += 1;
        true
    }
    pub fn groups(&self) -> impl Iterator<Item = &[u16]> {
        let mut start = 0;
        std::iter::from_fn(move || {
            if start >= self.len {
                return None;
            }
            let mut end = start + 1;
            while end < self.len && self.sub.get(end).copied().unwrap_or(false) {
                end += 1;
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

/// Incremental byte parser and its terminal state. Replies are delivered
/// synchronously to a caller-supplied sink; no unbounded response queue exists.
#[derive(Clone, Debug)]
pub struct Parser {
    screen: Screen,
    state: State,
    params: Parameters,
    intermediates: [u8; 2],
    intermediate_len: usize,
    ignoring: bool,
    utf8: [u8; 4],
    utf8_len: usize,
    utf8_need: usize,
}
impl Parser {
    pub fn new(rows: u16, cols: u16, history_lines: usize) -> Result<Self, Error> {
        Ok(Self {
            screen: Screen::new(rows, cols, history_lines)?,
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
    /// Process output while deliberately discarding terminal query replies.
    pub fn process(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.process_with_replies(bytes, |_| {})
    }
    pub fn process_with_replies(
        &mut self,
        bytes: &[u8],
        mut reply: impl FnMut(&[u8]),
    ) -> Result<(), Error> {
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
                self.byte(byte, &mut reply)?;
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
        if let Some(slot) = self.intermediates.get_mut(self.intermediate_len) {
            *slot = byte;
            self.intermediate_len += 1;
        } else {
            self.ignoring = true;
        }
    }

    fn ground(&mut self, byte: u8) -> Result<(), Error> {
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
            if valid_continuation {
                if let Some(slot) = self.utf8.get_mut(self.utf8_len) {
                    *slot = byte;
                }
                self.utf8_len += 1;
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
            0x00..=0x1f | 0x7f => self.screen.control(byte)?,
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

    fn byte(&mut self, byte: u8, reply: &mut impl FnMut(&[u8])) -> Result<(), Error> {
        if self.state == State::Ground {
            return self.ground(byte);
        }
        // Anywhere transitions: CAN/SUB cancel, ESC starts a new sequence.
        if matches!(byte, 0x18 | 0x1a) {
            self.state = State::Ground;
            return Ok(());
        }
        if byte == 0x1b {
            self.reset_sequence();
            self.state = State::Escape;
            return Ok(());
        }
        match self.state {
            State::Ground => self.ground(byte)?,
            State::OscString => {
                if byte == 7 {
                    self.state = State::Ground;
                }
            }
            State::DcsString => {
                if byte == 0x9c {
                    self.state = State::Ground;
                }
            }
            State::SosPmApcString | State::DcsIgnore => {}
            State::Escape | State::EscapeIntermediate => match byte {
                0x00..=0x1f => self.screen.control(byte)?,
                0x20..=0x2f => {
                    self.collect(byte);
                    self.state = State::EscapeIntermediate;
                }
                0x30..=0x7e => {
                    if self.state == State::Escape {
                        self.state = match byte {
                            b'[' => State::CsiEntry,
                            b'P' => State::DcsEntry,
                            b']' => State::OscString,
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
                            self.screen.control(byte)?;
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
                                if let Some(bytes) = self.screen.csi(
                                    &self.params,
                                    intermediates
                                        .get(..self.intermediate_len)
                                        .unwrap_or_default(),
                                    byte,
                                )? {
                                    reply(&bytes);
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
}
