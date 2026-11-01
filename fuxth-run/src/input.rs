//! stdin: keys, lines and the whole of it, the terminal it may be, and
//! the signals that reach the runner.
//!
//! The OS does what it already does. Ctrl-C is SIGINT, raised in the
//! program as `interrupt` (-28) between slices of steps, after a word
//! the runner waited on, and while it waits for input. A key word puts
//! the terminal in the mode keys need (no line editing, no echo, `IEXTEN`
//! and `IXON` off, signals kept), set once by `stty` and kept from then;
//! from then on Ctrl-Z (SIGTSTP) stops the runner with the terminal's own
//! modes back, set again as it goes on, and SIGTERM, SIGHUP and SIGQUIT
//! end it with them back. Every other way out puts them back too. Off a
//! terminal, stdin is read as it comes, a key's escape sequence decoded
//! however long it takes.
//!
//! Keys and lines take from one queue of bytes read, in order: a key is
//! decoded from as few bytes as make it, so what follows is still there
//! for a line.
use crate::words::{IO, OUTPUT, READ};
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
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
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

/// What a write takes once stdout is ready without waiting: a pipe's
/// atomic write, `PIPE_BUF`, at its smallest (macOS's).
pub const PIPE_WRITE: usize = 512;

/// The signals that end the runner once the terminal was in key mode,
/// with its own modes put back first.
const ENDING: [Signal; 3] = [Signal::Term, Signal::Hup, Signal::Quit];

/// Why a wait stopped short: a failure to raise in the program (Ctrl-C
/// among them), or a signal that ends the runner.
pub enum Stop {
    Failed(Failure),
    Signal(Signal),
}

impl From<Failure> for Stop {
    fn from(failure: Failure) -> Self {
        Stop::Failed(failure)
    }
}

/// The terminal's modes as found, and for keys once `stty` made them;
/// and whether its own modes read a CR typed as a newline (`ICRNL`).
struct Modes {
    cooked: Termios,
    raw: Option<Termios>,
    crnl: bool,
}

pub struct Input {
    terminal: bool,
    /// Once set, the key mode lasts till the runner ends; else, in the
    /// REPL, only while a key word waits.
    stays_raw: bool,
    modes: Option<Modes>,
    /// The terminal may be out of its own modes: they are put back.
    raw: bool,
    decoder: Decoder,
    /// Keys decoded and not yet taken: a paste's characters after the
    /// first.
    keys: VecDeque<i64>,
    /// The bytes given the decoder since the last key it made: those it
    /// holds, a key begun, which a line read takes back.
    held: Vec<u8>,
    /// Bytes read and not yet taken.
    bytes: VecDeque<u8>,
    /// How many of `bytes`, from the front, the terminal gave in key
    /// mode, without its own modes' CR to newline: a line read takes a
    /// CR among them as the newline those modes would have made it.
    raw_read: usize,
    ended: bool,
    interrupt: Pipe,
    /// SIGTSTP, then each of `ENDING`, caught once the terminal is first
    /// in key mode.
    caught: Vec<(Signal, Pipe)>,
    /// What a wait that could not raise it stopped for, raised next.
    pending: Option<Stop>,
}

impl Input {
    pub fn new(stays_raw: bool) -> Result<Self, Failure> {
        Ok(Self {
            terminal: std::io::stdin().is_terminal(),
            stays_raw,
            modes: None,
            raw: false,
            // A paste of any length is kept whole: no limit of the runner's.
            decoder: Decoder::with_paste_limit(TIMING, usize::MAX),
            keys: VecDeque::new(),
            held: Vec::new(),
            bytes: VecDeque::new(),
            raw_read: 0,
            ended: false,
            interrupt: Pipe::new(&[Signal::Int]).map_err(|e| errno("catching SIGINT", e))?,
            caught: Vec::new(),
            pending: None,
        })
    }

    /// Ctrl-Z, done; Ctrl-C, as the failure to raise; an ending signal,
    /// the terminal's modes put back.
    pub fn check(&mut self) -> Result<(), Stop> {
        if let Some(stop) = self.pending.take() {
            return Err(stop);
        }
        if let Some(signal) = self.ending()? {
            return Err(Stop::Signal(signal));
        }
        let stop = self
            .caught
            .iter()
            .find(|(signal, _)| *signal == Signal::Tstp);
        if stop.is_some_and(|(_, pipe)| drained(pipe)) {
            self.suspend()?;
        }
        match drained(&self.interrupt) {
            true => Err(Stop::Failed(interrupted())),
            false => Ok(()),
        }
    }

    /// An ending signal that came, the terminal's modes put back.
    fn ending(&mut self) -> Result<Option<Signal>, Failure> {
        let ending = self
            .caught
            .iter()
            .filter(|(signal, _)| ENDING.contains(signal));
        // Each pipe drained, whichever came.
        let came = ending.fold(None, |came, (signal, pipe)| match drained(pipe) {
            true => Some(*signal),
            false => came,
        });
        if came.is_some() {
            self.cook()?;
        }
        Ok(came)
    }

    /// Once the ending signals are caught (a key word changed the
    /// terminal), waits until stdout takes `PIPE_WRITE` bytes without
    /// waiting, so that a write held up by a reader that doesn't read can't
    /// hold off SIGTERM. One that comes meanwhile puts the terminal back,
    /// fails the write, and ends the run at the next check.
    pub fn writable(&mut self) -> Result<(), Failure> {
        if self.caught.is_empty() {
            return Ok(());
        }
        loop {
            if let Some(Stop::Signal(signal)) = &self.pending {
                let why = format!("writing the output: ended by SIG{}", signal.name());
                return Err(Failure::new(Code::Host(OUTPUT), why));
            }
            let stdout = std::io::stdout();
            let mut fds = vec![PollFd::new(&stdout, Events::OUT)];
            let ending = self
                .caught
                .iter()
                .filter(|(signal, _)| ENDING.contains(signal));
            fds.extend(ending.map(|(_, pipe)| PollFd::new(pipe.reader(), Events::IN)));
            match poll(&mut fds, None) {
                Ok(_) | Err(Errno::INTR) => {}
                Err(e) => return Err(errno("waiting to write the output", e)),
            }
            let ready = fds.first().is_some_and(|fd| !fd.revents().is_empty());
            drop(fds);
            if let Some(signal) = self.ending()? {
                self.pending = Some(Stop::Signal(signal));
            } else if ready {
                return Ok(());
            }
        }
    }

    /// Whether the ending signals are caught, and writes wait for stdout.
    pub fn watching(&self) -> bool {
        !self.caught.is_empty()
    }

    /// Raises `stop` at the next check: for a wait whose caller can only
    /// fail (`include`).
    pub fn defer(&mut self, stop: Stop) {
        self.pending = Some(stop);
    }

    /// The stop `defer` holds, if a check hasn't raised it yet.
    pub fn deferred(&mut self) -> Option<Stop> {
        self.pending.take()
    }

    /// `read-key`: the next key, waiting for it.
    pub fn key(&mut self) -> Result<i64, Stop> {
        self.keys_mode(Self::next_key)
    }

    /// `key-ready`: whether `read-key` would not wait.
    pub fn key_ready(&mut self) -> Result<bool, Stop> {
        self.keys_mode(|input| {
            loop {
                if !input.keys.is_empty() {
                    return Ok(true);
                }
                if let Some(byte) = input.take() {
                    input.decode(byte);
                    continue;
                }
                let late = input
                    .decoder
                    .deadline()
                    .is_some_and(|d| d <= Instant::now());
                if input.ended || (input.terminal && late) {
                    input.timeout();
                    return Ok(!input.keys.is_empty() || input.ended);
                }
                if !input.wait(std::io::stdin().as_fd(), Some(Instant::now()))? {
                    return Ok(false);
                }
                input.fill()?;
            }
        })
    }

    /// `accept`: the next line, of at most `most` characters; the rest of
    /// a longer one is left to read. A terminal edits and echoes it.
    pub fn accept(&mut self, most: usize) -> Result<String, Stop> {
        let raw = self.raw;
        self.cook()?;
        let line = self.line(most);
        if raw {
            self.make_raw()?;
        }
        Ok(line?.ok_or_else(ended)?)
    }

    /// The next line, of at most `most` characters, without its newline;
    /// the last, if it has none; none at the end.
    pub fn line(&mut self, most: usize) -> Result<Option<String>, Stop> {
        // Keys decoded but not taken come first, then the bytes of a key
        // begun, as they were read, then the bytes after them; the decoder
        // starts again.
        let keys = std::mem::take(&mut self.keys);
        let typed: String = keys
            .iter()
            .filter_map(|&k| u32::try_from(k).ok().and_then(char::from_u32))
            .collect();
        let held = std::mem::take(&mut self.held);
        self.decoder = Decoder::with_paste_limit(TIMING, usize::MAX);
        for &byte in typed.as_bytes().iter().chain(&held).rev() {
            self.bytes.push_front(byte);
            // Decoded from what the terminal gave in key mode.
            if self.terminal {
                self.raw_read = self.raw_read.saturating_add(1);
            }
        }
        loop {
            let crnl = self.modes.as_ref().is_some_and(|modes| modes.crnl);
            let raw = if crnl { self.raw_read } else { 0 };
            if let Some((end, newline)) = line_end(&self.bytes, most, raw) {
                let line: Vec<u8> = (0..end).filter_map(|_| self.take()).collect();
                if newline {
                    self.take();
                }
                return Ok(Some(text(line)?));
            }
            if self.ended {
                self.raw_read = 0;
                return match self.bytes.is_empty() {
                    true => Ok(None),
                    false => Ok(Some(text(std::mem::take(&mut self.bytes).into())?)),
                };
            }
            if self.wait(std::io::stdin().as_fd(), None)? {
                self.fill()?;
            }
        }
    }

    /// All of stdin, to its end, as it is: a program.
    pub fn all(&mut self) -> Result<String, Stop> {
        while !self.ended {
            if self.wait(std::io::stdin().as_fd(), None)? {
                self.fill()?;
            }
        }
        self.raw_read = 0;
        Ok(text(std::mem::take(&mut self.bytes).into())?)
    }

    /// The next byte read and not yet taken.
    fn take(&mut self) -> Option<u8> {
        self.raw_read = self.raw_read.saturating_sub(1);
        self.bytes.pop_front()
    }

    /// The text of the file at `path`, or why there is none. A file that
    /// may make a read wait (a FIFO, `<(…)`) is read as stdin is, so that
    /// Ctrl-C reaches the program meanwhile; a plain file at once.
    pub fn file(&mut self, path: &Path) -> Result<Result<String, String>, Stop> {
        let meta = match std::fs::metadata(path) {
            Ok(meta) => meta,
            Err(e) => return Ok(Err(e.to_string())),
        };
        if meta.is_file() {
            let read = std::fs::read(path).map_err(|e| e.to_string());
            return Ok(read.and_then(|bytes| String::from_utf8(bytes).map_err(|_| unreadable())));
        }
        let open = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path);
        let file = match open {
            Ok(file) => file,
            Err(e) => return Ok(Err(e.to_string())),
        };
        let mut bytes = Vec::new();
        let mut buffer = [0; 4096];
        loop {
            if !self.wait(file.as_fd(), None)? {
                continue;
            }
            match fuxix::io::read(&file, &mut buffer) {
                Ok(0) => break,
                Ok(n) => bytes.extend(buffer.get(..n).unwrap_or_default()),
                Err(Errno::AGAIN | Errno::INTR) => {}
                Err(e) => return Ok(Err(std::io::Error::from_raw_os_error(e.raw()).to_string())),
            }
        }
        Ok(String::from_utf8(bytes).map_err(|_| unreadable()))
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
    fn keys_mode<T>(&mut self, run: impl FnOnce(&mut Self) -> Result<T, Stop>) -> Result<T, Stop> {
        self.make_raw()?;
        let ran = run(self);
        if !self.stays_raw {
            self.cook()?;
        }
        ran
    }

    fn next_key(&mut self) -> Result<i64, Stop> {
        loop {
            if let Some(key) = self.keys.pop_front() {
                return Ok(key);
            }
            if let Some(byte) = self.take() {
                self.decode(byte);
                continue;
            }
            if self.ended {
                self.timeout();
                return Ok(self.keys.pop_front().ok_or_else(ended)?);
            }
            // Off a terminal, a sequence begun waits for the rest however
            // long it takes.
            let deadline = self.decoder.deadline().filter(|_| self.terminal);
            match self.wait(std::io::stdin().as_fd(), deadline)? {
                true => self.fill()?,
                false => self.timeout(),
            }
        }
    }

    /// One more byte for the decoder, which keeps what it decodes.
    fn decode(&mut self, byte: u8) {
        let mut inputs = Vec::new();
        self.decoder.bytes(&[byte], &mut inputs);
        self.decoder.mark(Instant::now());
        self.held.push(byte);
        // Waiting, as its deadline says, the decoder holds bytes: those
        // since its last key, or, if this byte made one and it still waits
        // (`ESC ESC`), this byte alone, which begins the next.
        match (self.decoder.deadline().is_some(), inputs.is_empty()) {
            (false, _) => self.held.clear(),
            (true, false) => self.held = vec![byte],
            (true, true) => {}
        }
        self.taken(inputs);
    }

    /// What is pending decoded as it is: a lone Escape.
    fn timeout(&mut self) {
        let mut inputs = Vec::new();
        self.decoder.timeout(&mut inputs);
        self.held.clear();
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

    /// Reads what stdin has into the bytes not yet taken; its end, at none.
    fn fill(&mut self) -> Result<(), Stop> {
        let mut buffer = [0; 4096];
        loop {
            match fuxix::io::read(std::io::stdin(), &mut buffer) {
                Ok(n) => {
                    self.ended |= n == 0;
                    self.bytes.extend(buffer.get(..n).unwrap_or_default());
                    // A key read reads only once every byte is taken.
                    if self.raw {
                        self.raw_read = self.bytes.len();
                    }
                    return Ok(());
                }
                Err(Errno::INTR) => self.check()?,
                Err(e) => return Err(errno("reading stdin", e).into()),
            }
        }
    }

    /// Waits until `fd` can be read, or `until`: whether it can. Ctrl-C
    /// and the ending signals stop it, and Ctrl-Z stops the runner
    /// meanwhile.
    fn wait(&mut self, fd: BorrowedFd<'_>, until: Option<Instant>) -> Result<bool, Stop> {
        loop {
            let timeout = until.map(|until| until.saturating_duration_since(Instant::now()));
            let mut fds = vec![
                PollFd::new(&fd, Events::IN),
                PollFd::new(self.interrupt.reader(), Events::IN),
            ];
            fds.extend(
                self.caught
                    .iter()
                    .map(|(_, pipe)| PollFd::new(pipe.reader(), Events::IN)),
            );
            let ready = match poll(&mut fds, timeout) {
                Ok(ready) => ready,
                Err(Errno::INTR) => 1,
                Err(e) => return Err(errno("waiting for input", e).into()),
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

    /// The terminal in key mode, if stdin is one. The first time, its own
    /// modes are kept, then the signals caught, then `stty` run.
    fn make_raw(&mut self) -> Result<(), Failure> {
        if self.raw || !self.terminal {
            return Ok(());
        }
        if self.modes.is_none() {
            let cooked = attributes(std::io::stdin())
                .map_err(|e| errno("reading the terminal's modes", e))?;
            let crnl = crnl()?;
            self.modes = Some(Modes {
                cooked,
                raw: None,
                crnl,
            });
            for signal in std::iter::once(Signal::Tstp).chain(ENDING) {
                let pipe = Pipe::new(&[signal]).map_err(|e| errno("catching a signal", e))?;
                self.caught.push((signal, pipe));
            }
        }
        let raw = match self.modes.as_ref().and_then(|modes| modes.raw.clone()) {
            Some(raw) => raw,
            None => {
                // From here the terminal may change: its own modes are put
                // back on any way out, a failed `stty` among them.
                self.raw = true;
                let made = stty().and_then(|()| {
                    attributes(std::io::stdin())
                        .map_err(|e| errno("reading the terminal's modes", e))
                });
                let raw = match made {
                    Ok(raw) => raw,
                    Err(failure) => {
                        let _ = self.cook();
                        return Err(failure);
                    }
                };
                if let Some(modes) = &mut self.modes {
                    modes.raw = Some(raw.clone());
                }
                raw
            }
        };
        self.raw = true;
        set_attributes(std::io::stdin(), &raw).map_err(|e| errno("setting the terminal's modes", e))
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

/// Where the line at the start of `bytes` ends: before its newline (or a
/// CR among the first `raw`), or before its character past `most`; and
/// whether a newline ends it. None while neither is there yet.
fn line_end(bytes: &VecDeque<u8>, most: usize, raw: usize) -> Option<(usize, bool)> {
    let mut chars = 0usize;
    for (at, &byte) in bytes.iter().enumerate() {
        if byte == b'\n' || (byte == b'\r' && at < raw) {
            return Some((at, true));
        }
        // A character starts at any byte but a UTF-8 continuation.
        if byte & 0xc0 != 0x80 {
            if chars == most {
                return Some((at, false));
            }
            chars = chars.saturating_add(1);
        }
    }
    None
}

/// Whether the terminal on stdin reads a CR typed as a newline, as
/// `stty -a` says (`icrnl` or `-icrnl`).
fn crnl() -> Result<bool, Failure> {
    let ran = std::process::Command::new("stty")
        .arg("-a")
        .stdin(std::process::Stdio::inherit())
        .output()
        .map_err(|e| Failure::new(Code::Host(IO), format!("stty -a: {e}")))?;
    if !ran.status.success() {
        let said = String::from_utf8_lossy(&ran.stderr);
        return Err(Failure::new(
            Code::Host(IO),
            format!("stty -a: {}", said.trim()),
        ));
    }
    let said = String::from_utf8_lossy(&ran.stdout);
    Ok(said.split_whitespace().any(|flag| flag == "icrnl"))
}

/// Puts the terminal on stdin in key mode, as `stty` does it.
fn stty() -> Result<(), Failure> {
    let ran = std::process::Command::new("stty")
        .args([
            "-icanon", "-echo", "-iexten", "-ixon", "-icrnl", "min", "1", "time", "0",
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

pub fn interrupted() -> Failure {
    Failure::new(Code::Interrupt, "interrupted (Ctrl-C)")
}

fn text(bytes: Vec<u8>) -> Result<String, Failure> {
    String::from_utf8(bytes).map_err(|_| Failure::new(Code::Host(READ), "stdin: not UTF-8 text"))
}

fn unreadable() -> String {
    "not UTF-8 text".to_owned()
}

fn ended() -> Failure {
    Failure::new(Code::EndOfInput, "stdin ended")
}

fn errno(doing: &str, error: Errno) -> Failure {
    let why = std::io::Error::from_raw_os_error(error.raw());
    Failure::new(Code::Host(IO), format!("{doing}: {why}"))
}
