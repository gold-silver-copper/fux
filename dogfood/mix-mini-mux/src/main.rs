//! mix-mini-mux: a minimal two-pane terminal multiplexer. Two programs run
//! in PTYs (fuxix), side by side or stacked, each emulated by a fux-vt
//! screen and painted into the real terminal, with a status line below.
//!
//! ```text
//! mix-mini-mux [--stack] [--prefix LETTER] [CMD1 [CMD2]]
//! ```
//! Each CMD is run by `$SHELL -c` (default: `$SHELL` itself). Keys go to the
//! focused pane, encoded as its program asked (cursor modes, kitty
//! protocol). Ctrl-<prefix> (default Ctrl-a), then `o` or Tab switches
//! focus, `q` quits, and the prefix again sends it. It ends once both
//! programs have, and leaves the terminal as it found it.

use fux_vt::keys::decode::{Decoder, Input, Timing};
use fux_vt::keys::{Key, KeyPress, Modifiers, encode};
use fux_vt::{Attributes, Event, Feature, Mark, Mode, Options, Parser, Sink};
use fuxix::Errno;
use fuxix::poll::{Events, Poll, PollFd};
use fuxix::process::{Dispositions, Job, Signal, Status};
use fuxix::pty::Master;
use fuxix::signal::Pipe;
use fuxix::terminal::Raw;
use std::io::Write;
use std::time::Instant;

/// History each pane keeps; the mux shows none of it, so a little.
const HISTORY_BYTES: usize = 1 << 20;
/// The most read from one pane in a turn, so a flood keeps keys flowing.
const READ_PER_TURN: usize = 1 << 18;

/// Where a pane sits on the real terminal, 0-based.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Place {
    top: u16,
    left: u16,
    size: fux_vt::Size,
}

/// The two panes' places and the separator's, for a terminal of `rows` by
/// `cols` (the last row is the status line), or why it is too small.
fn layout(rows: u16, cols: u16, stacked: bool) -> Result<[Place; 2], String> {
    let small = || format!("a {rows}x{cols} terminal is too small for two panes");
    let usable = rows.checked_sub(1).filter(|r| *r > 0).ok_or_else(small)?;
    let size = |r, c| fux_vt::Size::new(r, c).map_err(|_| small());
    if stacked {
        let first = usable.checked_sub(1).map(|r| r / 2).ok_or_else(small)?;
        let second = usable.saturating_sub(first).saturating_sub(1);
        Ok([
            Place { top: 0, left: 0, size: size(first, cols)? },
            Place { top: first.saturating_add(1), left: 0, size: size(second, cols)? },
        ])
    } else {
        let first = cols.checked_sub(1).map(|c| c / 2).ok_or_else(small)?;
        let second = cols.saturating_sub(first).saturating_sub(1);
        Ok([
            Place { top: 0, left: 0, size: size(usable, first)? },
            Place { top: 0, left: first.saturating_add(1), size: size(usable, second)? },
        ])
    }
}

/// What a pane's program asks beyond its screen: replies, a title, bells.
struct PaneSink<'a> {
    replies: &'a mut Vec<u8>,
    title: &'a mut String,
    bells: &'a mut u32,
}

impl Sink for PaneSink<'_> {
    fn reply(&mut self, bytes: &[u8]) {
        self.replies.extend_from_slice(bytes);
    }
    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Title(kept) => *self.title = String::from_utf8_lossy(kept.bytes).into_owned(),
            Event::Bell => *self.bells = self.bells.saturating_add(1),
            _ => {}
        }
    }
}

struct Pane {
    master: Master,
    job: Job,
    parser: Parser,
    place: Place,
    /// Painted up to here.
    mark: Mark,
    /// Bytes for the program not yet written: keys and replies.
    pending: Vec<u8>,
    title: String,
    bells: u32,
    /// How it ended, once it has.
    ended: Option<Status>,
    /// The master said its slave is closed: nothing more to read.
    closed: bool,
}

impl Pane {
    fn start(command: &[String], place: Place) -> Result<Pane, String> {
        let pty_size = fuxix::terminal::Size::from(place.size.nonzero());
        let _ = fuxix::pty::load(std::path::Path::new("no-pinned-conpty"));
        let (master, slave) = fuxix::pty::open(pty_size).map_err(|e| format!("opening a PTY: {e}"))?;
        let job = slave
            .spawn(command, |c| { c.env("TERM", "xterm-256color"); }, Dispositions::Default)
            .map_err(|e| format!("starting {command:?}: {e}"))?;
        let options = Options::new().with(Feature::Events);
        let parser = Parser::with_options(place.size, HISTORY_BYTES, options)
            .map_err(|e| format!("making a screen: {e:?}"))?;
        let mark = parser.screen().mark();
        let title = command.last().cloned().unwrap_or_default();
        Ok(Pane { master, job, parser, place, mark, pending: Vec::new(), title, bells: 0, ended: None, closed: false })
    }

    /// Reads what the program wrote, up to `limit`, onto the screen.
    fn read(&mut self, limit: usize) -> Result<(), String> {
        let mut buffer = vec![0u8; 1 << 16];
        let mut taken = 0usize;
        while taken < limit {
            match fuxix::io::read(&self.master, &mut buffer) {
                Ok(0) => {
                    self.closed = true;
                    break;
                }
                Ok(n) => {
                    taken = taken.saturating_add(n);
                    let mut sink = PaneSink { replies: &mut self.pending, title: &mut self.title, bells: &mut self.bells };
                    let _refused = self.parser.process_all(buffer.get(..n).unwrap_or_default(), &mut sink);
                }
                Err(Errno::AGAIN) => break,
                Err(Errno::INTR) => {}
                // Linux: the last holder of the slave closed it.
                Err(Errno::IO) => {
                    self.closed = true;
                    break;
                }
                Err(e) => return Err(format!("reading pane {}: {e}", self.job.leader())),
            }
        }
        Ok(())
    }

    fn flush(&mut self) {
        match fuxix::io::write_what_fits(&self.master, &self.pending) {
            Ok(n) => drop(self.pending.drain(..n.min(self.pending.len()))),
            Err(Errno::AGAIN | Errno::INTR) => {}
            // The program is gone; what it was sent goes nowhere.
            Err(_) => self.pending.clear(),
        }
    }

    fn resize(&mut self, place: Place) -> Result<(), String> {
        self.place = place;
        self.parser.resize(place.size).map_err(|e| format!("resizing a screen: {e:?}"))?;
        let pty_size = fuxix::terminal::Size::from(place.size.nonzero());
        fuxix::terminal::set_window_size(&self.master, pty_size, None).map_err(|e| format!("TIOCSWINSZ: {e}"))
    }

    /// Paints the rows changed since the last paint, or all of them.
    fn paint(&mut self, out: &mut Vec<u8>, all: bool) {
        let screen = self.parser.screen();
        let all = all || screen.full_refresh_since(self.mark);
        let Place { top, left, size } = self.place;
        for (r, row) in screen.window().iter() {
            if !all && !row.changed_since(self.mark) {
                continue;
            }
            let _ = write!(out, "\x1b[{};{}H\x1b[0m", top.saturating_add(r).saturating_add(1), left.saturating_add(1));
            let mut pen = Attributes::default();
            let mut col = 0u16;
            while col < size.cols() {
                let cell = row.cell(usize::from(col));
                let attributes = cell.map(|c| c.attributes()).unwrap_or_default();
                attributes.write_sgr_change(pen, out, false);
                pen = attributes;
                let (text, width) = match cell {
                    Some(c) if c.is_wide() && col.saturating_add(1) < size.cols() => (c.shown(), 2),
                    // A wide glyph the pane's edge cuts would spill past it.
                    Some(c) if !c.is_wide() && !c.is_wide_continuation() => (c.shown(), 1),
                    _ => (" ", 1),
                };
                out.extend_from_slice(text.as_bytes());
                col = col.saturating_add(width);
            }
            out.extend_from_slice(b"\x1b[0m");
        }
        self.mark = screen.mark();
    }
}

/// The real terminal, raw and on its alternate screen until dropped.
struct Terminal {
    _raw: Raw<std::io::Stdin>,
}

const ENTER: &[u8] = b"\x1b[?1049h\x1b[?2004h\x1b[H\x1b[2J";
const LEAVE: &[u8] = b"\x1b[?2026l\x1b[0m\x1b[?2004l\x1b[?25h\x1b[?1049l";

impl Terminal {
    fn enter() -> Result<Terminal, String> {
        let raw = Raw::enter(std::io::stdin()).map_err(|e| format!("stdin is no terminal: {e}"))?;
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = std::io::stdout().write_all(LEAVE);
            previous(info);
        }));
        std::io::stdout().write_all(ENTER).map_err(|e| e.to_string())?;
        Ok(Terminal { _raw: raw })
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let mut out = std::io::stdout();
        let _ = out.write_all(LEAVE).and_then(|()| out.flush());
    }
}

fn terminal_size() -> Result<(u16, u16), String> {
    let (size, _) = fuxix::terminal::stdin_size().ok_or("stdout is no terminal")?;
    Ok((size.rows.get(), size.cols.get()))
}

struct Mux {
    panes: [Pane; 2],
    focus: usize,
    stacked: bool,
    rows: u16,
    cols: u16,
    prefix: KeyPress,
    prefixed: bool,
    bells: u32,
    quit: bool,
}

impl Mux {
    fn focused(&mut self) -> &mut Pane {
        let [a, b] = &mut self.panes;
        if self.focus == 0 { a } else { b }
    }

    fn input(&mut self, input: Input) {
        match input {
            Input::Key(stroke) if self.prefixed => {
                self.prefixed = false;
                match stroke.press.key {
                    Key::Char('o') | Key::Tab => self.focus ^= 1,
                    Key::Char('q') => self.quit = true,
                    _ if stroke.press == self.prefix => {
                        let pane = self.focused();
                        let mode = pane.parser.screen().key_mode();
                        encode::key_bytes(stroke, mode, &mut pane.pending);
                    }
                    _ => {}
                }
            }
            Input::Key(stroke) if stroke.press == self.prefix => self.prefixed = true,
            Input::Key(stroke) => {
                let pane = self.focused();
                let mode = pane.parser.screen().key_mode();
                encode::key_bytes(stroke, mode, &mut pane.pending);
            }
            Input::Paste(text) => {
                let pane = self.focused();
                let bracketed = pane.parser.screen().mode(Mode::BracketedPaste);
                encode::paste(&text, bracketed, &mut pane.pending);
            }
            _ => {}
        }
    }

    fn relayout(&mut self) -> Result<(), String> {
        let (rows, cols) = terminal_size()?;
        let places = layout(rows, cols, self.stacked)?;
        (self.rows, self.cols) = (rows, cols);
        for (pane, place) in self.panes.iter_mut().zip(places) {
            pane.resize(place)?;
        }
        Ok(())
    }

    /// One frame: the panes' changes, the frame, the status line, the cursor.
    fn paint(&mut self, all: bool) -> std::io::Result<()> {
        let mut out = b"\x1b[?2026h\x1b[?25l".to_vec();
        if all {
            out.extend_from_slice(b"\x1b[0m\x1b[2J");
            let [a, b] = &self.panes;
            if self.stacked {
                let _ = write!(out, "\x1b[{};1H{}", b.place.top, "─".repeat(usize::from(self.cols)));
            } else {
                for row in 1..self.rows {
                    let _ = write!(out, "\x1b[{row};{}H│", a.place.size.cols().saturating_add(1));
                }
            }
        }
        for pane in &mut self.panes {
            pane.paint(&mut out, all);
            self.bells = self.bells.saturating_add(std::mem::take(&mut pane.bells));
        }
        let _ = write!(out, "\x1b[{};1H\x1b[0m\x1b[K", self.rows);
        // Each part of the status line while it fits: a wrap would scroll.
        let mut room = usize::from(self.cols);
        let key = match self.prefix.key { Key::Char(c) => c, _ => 'a' };
        let help = format!(" C-{key} o: switch  C-{key} q: quit");
        let parts = self.panes.iter().enumerate().map(|(n, pane)| {
            let state = match pane.ended {
                Some(Status::Exited(code)) => format!(" [exited {code}]"),
                None => String::new(),
            };
            let title: String = pane.title.chars().filter(|c| !c.is_control()).take(24).collect();
            (n == self.focus, format!(" {}: {title}{state} ", n.saturating_add(1)))
        });
        // The help is kept; the panes' titles take what room is left.
        let help_width = fux_vt::str_width(&help);
        room = room.saturating_sub(help_width);
        for (focused, text) in parts {
            let width = fux_vt::str_width(&text).saturating_add(1);
            let Some(left) = room.checked_sub(width) else { break };
            room = left;
            let style = if focused { "\x1b[7m" } else { "" };
            let _ = write!(out, "{style}{text}\x1b[0m ");
        }
        if help_width <= usize::from(self.cols) {
            out.extend_from_slice(help.as_bytes());
        }
        if self.bells > 0 {
            out.push(b'\x07');
            self.bells = 0;
        }
        let pane = self.focused();
        let screen = pane.parser.screen();
        let (row, col) = screen.cursor_position();
        let (top, left) = (pane.place.top, pane.place.left);
        let _ = write!(out, "\x1b[{};{}H", top.saturating_add(row).saturating_add(1), left.saturating_add(col).saturating_add(1));
        if screen.mode(Mode::ShowCursor) && pane.ended.is_none() {
            out.extend_from_slice(b"\x1b[?25h");
        }
        out.extend_from_slice(b"\x1b[?2026l");
        let mut stdout = std::io::stdout().lock();
        stdout.write_all(&out)?;
        stdout.flush()
    }

    /// Notes each pane's program that has ended, reading the last it wrote.
    fn reap(&mut self) -> Result<(), String> {
        for pane in &mut self.panes {
            if pane.ended.is_some() {
                continue;
            }
            if let Some(status) = pane.job.ended().map_err(|e| format!("waitid: {e}"))? {
                pane.read(usize::MAX)?;
                pane.closed = true;
                pane.ended = Some(status);
            }
        }
        Ok(())
    }
}

fn run() -> Result<(), String> {
    let mut stacked = false;
    let mut prefix = 'a';
    let mut commands = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--stack" => stacked = true,
            "--prefix" => {
                let letter = args.next().unwrap_or_default();
                let mut chars = letter.chars();
                prefix = match (chars.next(), chars.next()) {
                    (Some(c), None) if c.is_ascii_lowercase() => c,
                    _ => return Err("--prefix takes one letter, a to z".into()),
                };
            }
            "-h" | "--help" => return Err("usage: mix-mini-mux [--stack] [--prefix LETTER] [CMD1 [CMD2]]".into()),
            _ if commands.len() < 2 => commands.push(arg),
            _ => return Err(format!("unexpected argument {arg}")),
        }
    }
    let shell = std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".into());
    let argv = |n: usize| match commands.get(n) {
        Some(command) => vec![shell.clone(), "/c".into(), command.clone()],
        None => vec![shell.clone()],
    };
    let (rows, cols) = terminal_size()?;
    let [first, second] = layout(rows, cols, stacked)?;
    // Caught before the programs start, which begin with every signal default.
    let signals = Pipe::new(&[Signal::Winch, Signal::Chld, Signal::Term, Signal::Hup, Signal::Int])
        .map_err(|e| format!("signal pipe: {e}"))?;
    let panes = [Pane::start(&argv(0), first)?, Pane::start(&argv(1), second)?];
    let ctrl = Modifiers { ctrl: true, ..Modifiers::NONE };
    let mut mux = Mux { panes, focus: 0, stacked, rows, cols, prefix: KeyPress::new(Key::Char(prefix), ctrl), prefixed: false, bells: 0, quit: false };
    let terminal = Terminal::enter()?;
    let result = event_loop(&mut mux, &signals);
    drop(terminal);
    for pane in &mux.panes {
        if pane.ended.is_none() {
            let _ = fuxix::pty::hang_up(&pane.master, &pane.job);
        }
    }
    result
}

fn event_loop(mux: &mut Mux, signals: &Pipe) -> Result<(), String> {
    let stdin = std::io::stdin();
    let mut decoder = Decoder::with_paste_limit(Timing::new(25, 1000), 1 << 20);
    let mut inputs = Vec::new();
    let mut poll = Poll::default();
    let mut buffer = vec![0u8; 4096];
    let mut all = true;
    while !mux.quit && mux.panes.iter().any(|p| p.ended.is_none()) {
        mux.paint(all).map_err(|e| format!("writing the terminal: {e}"))?;
        all = false;
        let timeout = decoder.deadline().map(|d| d.saturating_duration_since(Instant::now()));
        let [a, b] = &mux.panes;
        let wants = |p: &Pane| if p.pending.is_empty() { Events::IN } else { Events::IN | Events::OUT };
        let mut list = poll.list();
        let keys = list.push(PollFd::new(&stdin, Events::IN));
        let caught = list.push(PollFd::new(signals.reader(), Events::IN));
        // A closed master is left out: asked for nothing, it still wakes
        // the poll with its hang-up, at once, every turn.
        let first = (!a.closed).then(|| list.push(PollFd::new(&a.master, wants(a))));
        let second = (!b.closed).then(|| list.push(PollFd::new(&b.master, wants(b))));
        match list.poll(timeout) {
            Ok(_) | Err(Errno::INTR) => {}
            Err(e) => return Err(format!("poll: {e}")),
        }
        let events = |entry: Option<_>| entry.map_or(Events::NONE, |e| list.events(e));
        let ready = [list.events(keys), list.events(caught), events(first), events(second)];
        // The signal pipe is drained below whatever it said.
        let [keys, _, first, second] = ready;
        while let Some(signal) = signals.next() {
            match signal {
                Signal::Winch => {
                    mux.relayout()?;
                    all = true;
                }
                Signal::Chld => mux.reap()?,
                _ => mux.quit = true,
            }
        }
        for (pane, events) in mux.panes.iter_mut().zip([first, second]) {
            if events.intersects(Events::IN | Events::HUP | Events::ERR) {
                pane.read(READ_PER_TURN)?;
            }
        }
        if keys.intersects(Events::IN | Events::HUP) {
            match fuxix::io::read(&stdin, &mut buffer) {
                Ok(0) => mux.quit = true,
                Ok(n) => {
                    decoder.bytes(buffer.get(..n).unwrap_or_default(), &mut inputs);
                    decoder.mark(Instant::now());
                }
                Err(Errno::AGAIN | Errno::INTR) => {}
                Err(e) => return Err(format!("reading keys: {e}")),
            }
        }
        if decoder.deadline().is_some_and(|d| d <= Instant::now()) {
            decoder.timeout(&mut inputs);
        }
        for input in inputs.drain(..) {
            mux.input(input);
        }
        for pane in &mut mux.panes {
            if !pane.closed && !pane.pending.is_empty() {
                pane.flush();
            }
        }
    }
    Ok(())
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("mix-mini-mux: {message}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "tests")]
mod tests {
    use super::*;

    #[test]
    fn layouts_split_the_terminal_around_a_separator_and_the_status_line() {
        let [a, b] = layout(24, 81, false).unwrap();
        assert_eq!((a.top, a.left, a.size.rows(), a.size.cols()), (0, 0, 23, 40));
        assert_eq!((b.top, b.left, b.size.rows(), b.size.cols()), (0, 41, 23, 40));
        let [a, b] = layout(24, 80, true).unwrap();
        assert_eq!((a.top, a.size.rows(), b.top, b.size.rows()), (0, 11, 12, 11));
        assert!(layout(2, 80, true).is_err());
        assert!(layout(24, 2, false).is_err());
    }
}
