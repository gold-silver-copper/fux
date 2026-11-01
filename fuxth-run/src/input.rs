//! stdin: keys, lines and the whole of it, and the terminal it may be.
//!
//! The OS does what it already does. Ctrl-C is SIGINT, raised in the
//! program as `interrupt` at its next slice of steps or key wait; Ctrl-Z
//! is SIGTSTP, the terminal restored while the runner is stopped and its
//! key mode set again once it goes on. A key word puts the terminal in
//! the mode keys need (no line editing, no echo, `IEXTEN` and `IXON` off,
//! signals kept), set once by `stty` and kept from then; the terminal's
//! own modes come back on every way out. Off a terminal, stdin is read as
//! it comes, a key's escape sequence decoded however long it takes.
use crate::words::{END_OF_INPUT, INTERRUPT, IO, READ};
use fux_vt::keys::decode::{Decoder, Input as Decoded, Timing};
use fux_vt::keys::{Direction, Key, KeyPress};
use fuxix::Errno;
use fuxix::poll::{Events, PollFd, poll};
use fuxix::process::{Pid, Signal, kill};
use fuxix::signal::Pipe;
use fuxix::terminal::{Termios, attributes, set_attributes, window_size};
use fuxth::{Code, Failure};
use std::collections::VecDeque;
use std::io::IsTerminal;
use std::time::Instant;

/// The first key id: past every character's code.
const KEY: i64 = 0x11_0000;
const KEY_F1: i64 = 0x11_0010;
const SHIFT: i64 = 1 << 22;
const CTRL: i64 = 1 << 23;
const ALT: i64 = 1 << 24;

/// The runner's constant words: the keys that type no character, and the
/// modifiers' masks. `read-key` gives a character's code, or one of these
/// ids, with the masks of what was held or'd in.
pub const CONSTANTS: &[(&str, i64)] = &[
    ("key-up", KEY),
    ("key-down", 0x11_0001),
    ("key-left", 0x11_0002),
    ("key-right", 0x11_0003),
    ("key-home", 0x11_0004),
    ("key-end", 0x11_0005),
    ("key-page-up", 0x11_0006),
    ("key-page-down", 0x11_0007),
    ("key-insert", 0x11_0008),
    ("key-delete", 0x11_0009),
    ("key-f1", KEY_F1),
    ("key-f2", 0x11_0011),
    ("key-f3", 0x11_0012),
    ("key-f4", 0x11_0013),
    ("key-f5", 0x11_0014),
    ("key-f6", 0x11_0015),
    ("key-f7", 0x11_0016),
    ("key-f8", 0x11_0017),
    ("key-f9", 0x11_0018),
    ("key-f10", 0x11_0019),
    ("key-f11", 0x11_001a),
    ("key-f12", 0x11_001b),
    ("shift-mask", SHIFT),
    ("ctrl-mask", CTRL),
    ("alt-mask", ALT),
];

/// How long a lone Escape waits for more, as fux's own default
/// (`escape-delay-ms`), and an unfinished paste.
const TIMING: Timing = Timing::new(35, 5000);

/// The terminal's modes as found, and for keys.
struct Modes {
    cooked: Termios,
    raw: Termios,
}

pub struct Input {
    terminal: bool,
    /// Once set, the key mode lasts till the runner ends; else, in the
    /// REPL, only while a key word waits.
    stays_raw: bool,
    modes: Option<Modes>,
    raw: bool,
    decoder: Decoder,
    keys: VecDeque<i64>,
    /// Bytes read for lines, not yet taken.
    bytes: Vec<u8>,
    ended: bool,
    interrupt: Pipe,
    /// SIGTSTP, caught once the terminal is first in key mode.
    stop: Option<Pipe>,
}

impl Input {
    pub fn new(stays_raw: bool) -> Result<Self, Failure> {
        Ok(Self {
            terminal: std::io::stdin().is_terminal(),
            stays_raw,
            modes: None,
            raw: false,
            decoder: Decoder::new(TIMING),
            keys: VecDeque::new(),
            bytes: Vec::new(),
            ended: false,
            interrupt: Pipe::new(&[Signal::Int]).map_err(|e| errno("catching SIGINT", e))?,
            stop: None,
        })
    }

    /// Ctrl-Z, done; Ctrl-C, as the failure to raise.
    pub fn check(&mut self) -> Result<(), Failure> {
        if self.stop.as_ref().is_some_and(drained) {
            self.suspend()?;
        }
        match drained(&self.interrupt) {
            true => Err(Failure::new(Code::Host(INTERRUPT), "interrupted (Ctrl-C)")),
            false => Ok(()),
        }
    }

    /// `read-key`: the next key, waiting for it.
    pub fn key(&mut self) -> Result<i64, Failure> {
        self.keys_mode(Self::next_key)
    }

    /// `key-ready`: whether `read-key` would not wait.
    pub fn key_ready(&mut self) -> Result<bool, Failure> {
        self.keys_mode(|input| {
            input.decode_bytes();
            if input.keys.is_empty() && !input.ended && input.wait(Some(Instant::now()))? {
                input.fill_keys()?;
            }
            if input.keys.is_empty()
                && input.terminal
                && input
                    .decoder
                    .deadline()
                    .is_some_and(|d| d <= Instant::now())
            {
                input.timeout();
            }
            Ok(!input.keys.is_empty() || input.ended)
        })
    }

    /// `accept`: the next line, of at most `most` characters; the rest of
    /// a longer one is left to read. A terminal edits and echoes it.
    pub fn accept(&mut self, most: usize) -> Result<String, Failure> {
        let raw = self.raw;
        self.cook()?;
        let line = self.line();
        if raw {
            self.make_raw()?;
        }
        let line = line?.ok_or_else(ended)?;
        let Some((kept, rest)) =
            (line.char_indices().nth(most)).and_then(|(at, _)| line.split_at_checked(at))
        else {
            return Ok(line);
        };
        self.bytes = [rest.as_bytes(), b"\n", &self.bytes].concat();
        Ok(kept.to_owned())
    }

    /// The next line, without its newline; the last, if it has none; none
    /// at the end.
    pub fn line(&mut self) -> Result<Option<String>, Failure> {
        loop {
            if let Some(at) = self.bytes.iter().position(|&b| b == b'\n') {
                let rest = self
                    .bytes
                    .get(at.saturating_add(1)..)
                    .unwrap_or_default()
                    .to_vec();
                let mut line = std::mem::replace(&mut self.bytes, rest);
                line.truncate(at);
                return text(line).map(Some);
            }
            if self.ended {
                return match self.bytes.is_empty() {
                    true => Ok(None),
                    false => text(std::mem::take(&mut self.bytes)).map(Some),
                };
            }
            if self.wait(None)? {
                let mut buffer = [0; 4096];
                let n = self.read(&mut buffer)?;
                self.bytes.extend(buffer.get(..n).unwrap_or_default());
            }
        }
    }

    /// All of stdin, to its end: a program.
    pub fn all(&mut self) -> Result<String, Failure> {
        let mut all = Vec::new();
        while let Some(line) = self.line()? {
            all.extend(line.into_bytes());
            all.push(b'\n');
        }
        text(all)
    }

    /// The terminal's rows and columns: stdin's, else `LINES` and
    /// `COLUMNS`.
    pub fn screen_size(&self) -> Option<(i64, i64)> {
        if self.terminal
            && let Some((size, _)) = window_size(std::io::stdin())
        {
            return Some((i64::from(size.rows.get()), i64::from(size.cols.get())));
        }
        let var = |name| {
            std::env::var(name)
                .ok()?
                .parse::<i64>()
                .ok()
                .filter(|n| *n > 0)
        };
        Some((var("LINES")?, var("COLUMNS")?))
    }

    /// `run` in key mode, which the REPL leaves after it.
    fn keys_mode<T>(
        &mut self,
        run: impl FnOnce(&mut Self) -> Result<T, Failure>,
    ) -> Result<T, Failure> {
        self.make_raw()?;
        let ran = run(self);
        if !self.stays_raw {
            self.cook()?;
        }
        ran
    }

    fn next_key(&mut self) -> Result<i64, Failure> {
        loop {
            self.decode_bytes();
            if let Some(key) = self.keys.pop_front() {
                return Ok(key);
            }
            if self.ended {
                self.timeout();
                return self.keys.pop_front().ok_or_else(ended);
            }
            // Off a terminal, a sequence begun waits for the rest however
            // long it takes.
            let deadline = self.decoder.deadline().filter(|_| self.terminal);
            match self.wait(deadline)? {
                true => self.fill_keys()?,
                false => self.timeout(),
            }
        }
    }

    /// Bytes read for lines, decoded as keys.
    fn decode_bytes(&mut self) {
        if !self.bytes.is_empty() {
            let bytes = std::mem::take(&mut self.bytes);
            self.decode(&bytes);
        }
    }

    fn fill_keys(&mut self) -> Result<(), Failure> {
        let mut buffer = [0; 4096];
        let n = self.read(&mut buffer)?;
        self.decode(buffer.get(..n).unwrap_or_default());
        Ok(())
    }

    fn decode(&mut self, bytes: &[u8]) {
        let mut inputs = Vec::new();
        self.decoder.bytes(bytes, &mut inputs);
        self.decoder.mark(Instant::now());
        self.taken(inputs);
    }

    /// What is pending decoded as it is: a lone Escape.
    fn timeout(&mut self) {
        let mut inputs = Vec::new();
        self.decoder.timeout(&mut inputs);
        self.taken(inputs);
    }

    /// The keys among `inputs`; a paste is its characters.
    fn taken(&mut self, inputs: Vec<Decoded>) {
        for input in inputs {
            match input {
                Decoded::Key(stroke) => self.keys.push_back(value(stroke.press)),
                Decoded::Paste(text) => self
                    .keys
                    .extend(text.chars().map(|c| i64::from(u32::from(c)))),
                Decoded::PasteTooLong
                | Decoded::FocusIn
                | Decoded::FocusOut
                | Decoded::Mouse(_)
                | Decoded::Reply(_) => {}
            }
        }
    }

    /// Reads what stdin has: none at its end.
    fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Failure> {
        loop {
            match fuxix::io::read(std::io::stdin(), buffer) {
                Ok(n) => {
                    self.ended |= n == 0;
                    return Ok(n);
                }
                Err(Errno::INTR) => self.check()?,
                Err(e) => return Err(errno("reading stdin", e)),
            }
        }
    }

    /// Waits until stdin can be read, or `until`: whether it can. Ctrl-C
    /// fails it, and Ctrl-Z stops the runner meanwhile.
    fn wait(&mut self, until: Option<Instant>) -> Result<bool, Failure> {
        loop {
            let timeout = until.map(|until| until.saturating_duration_since(Instant::now()));
            let stdin = std::io::stdin();
            let mut fds = vec![
                PollFd::new(&stdin, Events::IN),
                PollFd::new(self.interrupt.reader(), Events::IN),
            ];
            fds.extend(
                self.stop
                    .as_ref()
                    .map(|stop| PollFd::new(stop.reader(), Events::IN)),
            );
            let ready = match poll(&mut fds, timeout) {
                Ok(ready) => ready,
                Err(Errno::INTR) => 1,
                Err(e) => return Err(errno("waiting for stdin", e)),
            };
            let readable = fds.first().is_some_and(|fd| !fd.revents().is_empty());
            drop(fds);
            self.check()?;
            if readable {
                return Ok(true);
            }
            if ready == 0 {
                return Ok(false);
            }
        }
    }

    /// Stopped, as Ctrl-Z asks, with the terminal's own modes meanwhile.
    fn suspend(&mut self) -> Result<(), Failure> {
        let raw = self.raw;
        self.cook()?;
        let me = i32::try_from(std::process::id())
            .ok()
            .and_then(Pid::from_raw);
        if let Some(me) = me {
            kill(me, Signal::Stop).map_err(|e| errno("stopping", e))?;
        }
        if raw {
            self.make_raw()?;
        }
        Ok(())
    }

    /// The terminal in key mode, if stdin is one.
    fn make_raw(&mut self) -> Result<(), Failure> {
        if self.raw || !self.terminal {
            return Ok(());
        }
        let raw = match &self.modes {
            Some(modes) => modes.raw.clone(),
            None => {
                let failed = |e| errno("reading the terminal's modes", e);
                let cooked = attributes(std::io::stdin()).map_err(failed)?;
                self.stop =
                    Some(Pipe::new(&[Signal::Tstp]).map_err(|e| errno("catching SIGTSTP", e))?);
                stty()?;
                let raw = attributes(std::io::stdin()).map_err(failed)?;
                self.modes = Some(Modes {
                    cooked,
                    raw: raw.clone(),
                });
                raw
            }
        };
        set_attributes(std::io::stdin(), &raw)
            .map_err(|e| errno("setting the terminal's modes", e))?;
        self.raw = true;
        Ok(())
    }

    /// The terminal in its own modes again.
    fn cook(&mut self) -> Result<(), Failure> {
        if let (true, Some(modes)) = (self.raw, &self.modes) {
            set_attributes(std::io::stdin(), &modes.cooked)
                .map_err(|e| errno("restoring the terminal's modes", e))?;
            self.raw = false;
        }
        Ok(())
    }
}

impl Drop for Input {
    fn drop(&mut self) {
        let _ = self.cook();
    }
}

/// Puts the terminal on stdin in key mode, as `stty` does it.
fn stty() -> Result<(), Failure> {
    let ran = std::process::Command::new("stty")
        .args([
            "-icanon", "-echo", "-iexten", "-ixon", "min", "1", "time", "0",
        ])
        .stdin(std::process::Stdio::inherit())
        .output()
        .map_err(|e| Failure::new(Code::Host(IO), format!("stty: {e}")))?;
    match ran.status.success() {
        true => Ok(()),
        false => {
            let said = String::from_utf8_lossy(&ran.stderr);
            Err(Failure::new(
                Code::Host(IO),
                format!("stty: {}", said.trim()),
            ))
        }
    }
}

/// A key's value: what a terminal sends for it, Ctrl and a letter as its
/// control character; or its id; with the masks of the modifiers it
/// doesn't say.
fn value(press: KeyPress) -> i64 {
    let KeyPress { key, mods } = press;
    let mut ctrl = mods.ctrl;
    let n = match key {
        Key::Char(c) => {
            let code = i64::from(u32::from(c));
            if ctrl && matches!(c, ' ' | '@'..='_' | '`'..='z') {
                ctrl = false;
                code & 0x1f
            } else {
                code
            }
        }
        Key::Enter => 13,
        Key::Tab => 9,
        Key::Escape => 27,
        Key::Backspace => 127,
        Key::Arrow(Direction::Up) => KEY,
        Key::Arrow(Direction::Down) => 0x11_0001,
        Key::Arrow(Direction::Left) => 0x11_0002,
        Key::Arrow(Direction::Right) => 0x11_0003,
        Key::Home => 0x11_0004,
        Key::End => 0x11_0005,
        Key::PageUp => 0x11_0006,
        Key::PageDown => 0x11_0007,
        Key::Insert => 0x11_0008,
        Key::Delete => 0x11_0009,
        Key::F(f) => KEY_F1.saturating_add(i64::from(f.number()).saturating_sub(1)),
    };
    let mask = |on: bool, mask: i64| if on { mask } else { 0 };
    n | mask(mods.shift, SHIFT) | mask(ctrl, CTRL) | mask(mods.alt, ALT)
}

/// Whether a signal's pipe had a byte, which it is emptied of.
fn drained(pipe: &Pipe) -> bool {
    let mut any = false;
    let mut buffer = [0; 64];
    while fuxix::io::read(pipe.reader(), &mut buffer).is_ok_and(|n| n > 0) {
        any = true;
    }
    any
}

fn text(bytes: Vec<u8>) -> Result<String, Failure> {
    String::from_utf8(bytes).map_err(|_| Failure::new(Code::Host(READ), "stdin: not UTF-8 text"))
}

fn ended() -> Failure {
    Failure::new(Code::Host(END_OF_INPUT), "stdin ended")
}

fn errno(doing: &str, error: Errno) -> Failure {
    let why = std::io::Error::from_raw_os_error(error.raw());
    Failure::new(Code::Host(IO), format!("{doing}: {why}"))
}
