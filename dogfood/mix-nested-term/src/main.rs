//! mix-nested-term: a full-screen app with a bordered box that runs a
//! program on a PTY and shows its fux-vt screen inside, keys forwarded as
//! the program asked for them, the box's size given to the PTY, and a
//! status line below the box.
use std::io::Write;
use std::os::fd::AsFd;
use std::process::ExitCode;
use std::time::Instant;

use fux_vt::keys::decode::{Decoder, Input, Timing};
use fux_vt::keys::{KeyPress, encode};
use fux_vt::{Attributes, CellRef, Mode, Options, Parser, RowId, Screen, Sink};
use fuxix::Errno;
use fuxix::poll::{Events, Poll, PollFd};
use fuxix::process::{Dispositions, Pid, Signal, Status};
use fuxix::{pty, signal, terminal};

const USAGE: &str = "usage: mix-nested-term [--quit-key KEY] [--] [COMMAND ARGS...]
  Runs COMMAND (default $SHELL) in a box. KEY (default C-]) quits.";

/// The program's replies (DA, DSR, ...), to send back on the PTY.
#[derive(Default)]
struct Replies(Vec<u8>);

impl Sink for Replies {
    fn reply(&mut self, bytes: &[u8]) {
        self.0.extend_from_slice(bytes);
    }
}

/// Where the box is: the outer terminal's size and the inner screen's.
#[derive(Clone, Copy, PartialEq)]
struct Layout {
    rows: u16,
    cols: u16,
}

impl Layout {
    /// Borders take a row above and below and a column either side; the
    /// status line the last row.
    fn inner(self) -> Option<(u16, u16)> {
        let rows = self.rows.checked_sub(3).filter(|&r| r > 0)?;
        let cols = self.cols.checked_sub(2).filter(|&c| c > 0)?;
        Some((rows, cols))
    }
}

/// What the outer terminal shows of the box, to repaint only what changed.
struct View {
    layout: Layout,
    painted: Vec<Option<(RowId, u64)>>,
    status: String,
    whole: bool,
}

struct App {
    argv: Vec<String>,
    quit: KeyPress,
    parser: Parser,
    master: pty::Master,
    pid: Pid,
    exited: Option<Status>,
    to_child: Vec<u8>,
    view: View,
}

fn outer_layout() -> Layout {
    match terminal::window_size(std::io::stdout()) {
        Some((size, _)) => Layout {
            rows: size.rows.get(),
            cols: size.cols.get(),
        },
        None => Layout { rows: 24, cols: 80 },
    }
}

fn parse_args() -> Result<(Vec<String>, KeyPress), String> {
    let mut quit = "C-]".parse::<KeyPress>().map_err(|e| e.to_string())?;
    let mut argv = Vec::new();
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--quit-key" => {
                let name = it.next().ok_or("--quit-key needs a key, e.g. C-q")?;
                quit = name.parse().map_err(|e| format!("--quit-key {name}: {e}"))?;
            }
            "-h" | "--help" => return Err(USAGE.into()),
            "--" => {
                argv.extend(it.by_ref());
            }
            _ if argv.is_empty() && arg.starts_with('-') => return Err(format!("unknown option {arg}\n{USAGE}")),
            _ => {
                argv.push(arg);
                argv.extend(it.by_ref());
            }
        }
    }
    if argv.is_empty() {
        argv.push(std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into()));
    }
    Ok((argv, quit))
}

fn sizes(layout: Layout) -> Result<(fux_vt::Size, terminal::Size), String> {
    let (rows, cols) = layout.inner().ok_or("the terminal is too small for the box")?;
    let vt = fux_vt::Size::new(rows, cols).map_err(|e| e.to_string())?;
    let pty = terminal::Size::new(rows, cols).ok_or("the terminal is too small for the box")?;
    Ok((vt, pty))
}

impl App {
    fn start(argv: Vec<String>, quit: KeyPress) -> Result<App, String> {
        let layout = outer_layout();
        let (vt, size) = sizes(layout)?;
        let parser = Parser::with_options(vt, 4 << 20, Options::default()).map_err(|e| e.to_string())?;
        let (master, slave) = pty::open(size).map_err(|e| e.to_string())?;
        let (_child, pid) = slave
            .spawn(&argv, |c| {
                c.env("TERM", "xterm-256color");
            }, Dispositions::Default)
            .map_err(|e| format!("{}: {e}", argv.first().map_or("", String::as_str)))?;
        Ok(App {
            argv,
            quit,
            parser,
            master,
            pid,
            exited: None,
            to_child: Vec::new(),
            view: View { layout, painted: Vec::new(), status: String::new(), whole: true },
        })
    }

    /// Reads what the program wrote; `false` once the PTY is closed.
    fn read_child(&mut self) -> bool {
        let mut buf = [0u8; 65536];
        loop {
            match fuxix::io::read(&self.master, &mut buf) {
                Ok(0) | Err(Errno::IO) => return false,
                Ok(n) => {
                    let mut replies = Replies::default();
                    let _refused = self.parser.process_all(buf.get(..n).unwrap_or_default(), &mut replies);
                    self.to_child.extend_from_slice(&replies.0);
                }
                Err(Errno::AGAIN) => return true,
                Err(Errno::INTR) => {}
                Err(_) => return false,
            }
        }
    }

    /// Sends what is queued for the program, as much as the PTY takes.
    fn write_child(&mut self) {
        match fuxix::io::write_what_fits(&self.master, &self.to_child) {
            Ok(n) => drop(self.to_child.drain(..n.min(self.to_child.len()))),
            // The program is gone: what it was sent no longer matters.
            Err(_) => self.to_child.clear(),
        }
    }

    /// Handles one decoded input; `false` to quit.
    fn input(&mut self, input: Input) -> bool {
        let screen = self.parser.screen();
        match input {
            Input::Key(_) if self.exited.is_some() => return false,
            Input::Key(stroke) if stroke.press == self.quit => return false,
            Input::Key(stroke) => encode::key_bytes(stroke, screen.key_mode(), &mut self.to_child),
            Input::Paste(text) => encode::paste(&text, screen.mode(Mode::BracketedPaste), &mut self.to_child),
            Input::FocusIn | Input::FocusOut => {
                let report = screen.focus_report(input == Input::FocusIn);
                self.to_child.extend_from_slice(report.unwrap_or_default());
            }
            // The box asks for no mouse reports, and no answers.
            Input::Mouse(_) | Input::PasteTooLong | Input::Reply(_) => {}
        }
        true
    }

    /// The outer terminal changed size: the box, the PTY and the screen
    /// follow.
    fn resize(&mut self) -> Result<(), String> {
        let layout = outer_layout();
        if layout == self.view.layout {
            return Ok(());
        }
        let (vt, size) = sizes(layout)?;
        self.parser.resize(vt).map_err(|e| e.to_string())?;
        terminal::set_window_size(&self.master, size, None).map_err(|e| format!("TIOCSWINSZ: {e}"))?;
        self.view.layout = layout;
        self.view.whole = true;
        Ok(())
    }

    fn status_text(&self) -> String {
        let screen = self.parser.screen();
        let size = screen.size();
        let title: String = String::from_utf8_lossy(screen.title().bytes).chars().filter(|c| !c.is_control()).collect();
        let state = match self.exited {
            None => format!("{} | quit: {}", self.pid.as_raw(), key_name(self.quit)),
            Some(Status::Exited(code)) => format!("exited {code} | any key quits"),
            Some(Status::Signalled { signal, .. }) => {
                let name = Signal::from_raw(signal).map_or("?", Signal::name);
                format!("killed by SIG{name} | any key quits")
            }
        };
        // What changes first, as the line is cut at the terminal's width.
        format!(" {}x{} | {state} | {title} | {}", size.rows(), size.cols(), self.argv.join(" "))
    }
}

fn key_name(key: KeyPress) -> String {
    let mut name = String::new();
    if key.mods.ctrl {
        name.push_str("C-");
    }
    if key.mods.alt {
        name.push_str("M-");
    }
    match key.key {
        fux_vt::keys::Key::Char(c) => name.push(c),
        other => name.push_str(&format!("{other:?}")),
    }
    name
}

/// `text` cut or padded to exactly `cols` columns.
fn fit(text: &str, cols: u16) -> String {
    let mut out = String::new();
    let mut used = 0u16;
    for c in text.chars() {
        let w = fux_vt::char_width(c).and_then(|w| u16::try_from(w).ok()).unwrap_or(0);
        if used + w > cols {
            break;
        }
        used += w;
        out.push(c);
    }
    out.extend(std::iter::repeat_n(' ', usize::from(cols - used)));
    out
}

/// Appends what repaints the box from the last paint: the borders and every
/// row on a new size, then each row whose identity or version changed,
/// the status line if it changed, and the cursor.
fn paint(view: &mut View, screen: &Screen, status: String, exited: bool, out: &mut Vec<u8>) {
    let Layout { rows, cols } = view.layout;
    let Some((inner_rows, inner_cols)) = view.layout.inner() else { return };
    out.extend_from_slice(b"\x1b[?2026h\x1b[?25l");
    if view.whole {
        out.extend_from_slice(b"\x1b[0m\x1b[H\x1b[2J");
        let bar = "─".repeat(usize::from(inner_cols));
        out.extend_from_slice(format!("\x1b[1;1H┌{bar}┐").as_bytes());
        for y in 0..inner_rows {
            out.extend_from_slice(format!("\x1b[{};1H│\x1b[{};{}H│", y + 2, y + 2, cols).as_bytes());
        }
        out.extend_from_slice(format!("\x1b[{};1H└{bar}┘", inner_rows + 2).as_bytes());
        view.painted = vec![None; usize::from(inner_rows)];
        view.status.clear();
        view.whole = false;
    }
    let window = screen.window();
    for (y, row) in window.iter() {
        let now = Some((row.id(), row.version()));
        let Some(painted) = view.painted.get_mut(usize::from(y)) else { continue };
        if *painted == now {
            continue;
        }
        *painted = now;
        // A double-width row can't be doubled inside the box (DECDWL is a
        // whole outer line's): its half is drawn single width.
        out.extend_from_slice(format!("\x1b[{};2H", y + 2).as_bytes());
        let mut pen = Attributes::default();
        out.extend_from_slice(b"\x1b[0m");
        let mut x = 0u16;
        while x < inner_cols {
            let blank = CellRef::new("", false, Attributes::default());
            let cell = window.cell(y, x).unwrap_or(blank);
            if cell.is_wide_continuation() {
                x += 1;
                continue;
            }
            cell.attributes().write_sgr_change(pen, out, true);
            pen = cell.attributes();
            out.extend_from_slice(cell.shown().as_bytes());
            x += if cell.is_wide() { 2 } else { 1 };
        }
        out.extend_from_slice(b"\x1b[0m");
    }
    if status != view.status {
        out.extend_from_slice(format!("\x1b[{rows};1H\x1b[0;7m{}\x1b[0m", fit(&status, cols)).as_bytes());
        view.status = status;
    }
    let (cy, cx) = screen.cursor_position();
    out.extend_from_slice(format!("\x1b[{};{}H", cy + 2, cx + 2).as_bytes());
    if screen.mode(Mode::ShowCursor) && !exited {
        out.extend_from_slice(b"\x1b[?25h");
    }
    out.extend_from_slice(b"\x1b[?2026l");
}

fn run(argv: Vec<String>, quit: KeyPress) -> Result<Option<Status>, String> {
    let signals = signal::Pipe::new(&[Signal::Winch, Signal::Chld, Signal::Term, Signal::Hup])
        .map_err(|e| format!("signals: {e}"))?;
    let mut app = App::start(argv, quit)?;
    let stdin = std::io::stdin();
    let _raw = terminal::Raw::enter(stdin.as_fd()).map_err(|e| format!("raw mode: {e}"))?;
    let mut stdout = std::io::stdout().lock();
    let write = |stdout: &mut std::io::StdoutLock, bytes: &[u8]| {
        stdout.write_all(bytes).and_then(|()| stdout.flush()).map_err(|e| format!("writing: {e}"))
    };
    write(&mut stdout, b"\x1b[?1049h")?;
    let result = event_loop(&mut app, &signals, &stdin, &mut stdout, &write);
    // Whatever happened, the program goes and the terminal comes back.
    if app.exited.is_none() {
        let _ = fuxix::process::kill_group(app.pid, Signal::Hup);
    }
    let _ = write(&mut stdout, b"\x1b[0m\x1b[?25h\x1b[?1049l");
    result.map(|()| app.exited)
}

type Writer<'a> = dyn Fn(&mut std::io::StdoutLock<'a>, &[u8]) -> Result<(), String>;

fn event_loop<'a>(
    app: &mut App,
    signals: &signal::Pipe,
    stdin: &std::io::Stdin,
    stdout: &mut std::io::StdoutLock<'a>,
    write: &Writer<'a>,
) -> Result<(), String> {
    // fux's own waits: 35 ms for a lone Escape, 5 s for a paste's rest.
    let mut decoder = Decoder::with_paste_limit(Timing::new(35, 5000), 1 << 20);
    let mut inputs = Vec::new();
    let mut poll = Poll::default();
    let mut frame = Vec::new();
    let mut child_open = true;
    loop {
        frame.clear();
        let status = app.status_text();
        paint(&mut app.view, app.parser.screen(), status, app.exited.is_some(), &mut frame);
        write(stdout, &frame)?;
        let timeout = decoder.deadline().map(|d| d.saturating_duration_since(Instant::now()));
        let mut child_events = Events::NONE;
        if child_open {
            child_events |= Events::IN;
        }
        if !app.to_child.is_empty() {
            child_events |= Events::OUT;
        }
        let mut list = poll.list();
        let keys = list.push(PollFd::new(stdin, Events::IN));
        let child = list.push(PollFd::new(&app.master, child_events));
        let sigs = list.push(PollFd::new(signals.reader(), Events::IN));
        match list.poll(timeout) {
            Ok(_) | Err(Errno::INTR) => {}
            Err(e) => return Err(format!("poll: {e}")),
        }
        let (key_in, child_in, sig_in) = (list.events(keys), list.events(child), list.events(sigs));
        if !sig_in.is_empty() {
            while let Some(sig) = signals.next() {
                match sig {
                    Signal::Winch => app.resize()?,
                    Signal::Chld => {
                        if app.exited.is_none()
                            && let Ok(Some(status)) = fuxix::process::reap(app.pid)
                        {
                            // Everything it wrote is readable now.
                            child_open = app.read_child();
                            app.exited = Some(status);
                        }
                    }
                    _ => return Ok(()),
                }
            }
        }
        if child_open && child_in.intersects(Events::IN | Events::HUP | Events::ERR) {
            child_open = app.read_child();
        }
        if child_in.intersects(Events::OUT) || !app.to_child.is_empty() {
            app.write_child();
        }
        if key_in.intersects(Events::IN | Events::HUP) {
            let mut buf = [0u8; 4096];
            match fuxix::io::read(stdin, &mut buf) {
                Ok(0) => return Ok(()),
                Ok(n) => {
                    decoder.bytes(buf.get(..n).unwrap_or_default(), &mut inputs);
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
            if !app.input(input) {
                return Ok(());
            }
        }
        if !app.to_child.is_empty() {
            app.write_child();
        }
    }
}

fn main() -> ExitCode {
    let (argv, quit) = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("mix-nested-term: {e}");
            return ExitCode::from(2);
        }
    };
    match run(argv, quit) {
        Ok(Some(Status::Exited(code))) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Ok(Some(Status::Signalled { signal, .. })) => ExitCode::from(u8::try_from(128 + signal).unwrap_or(1)),
        Ok(None) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("mix-nested-term: {e}");
            ExitCode::FAILURE
        }
    }
}
