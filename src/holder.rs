//! The holder: the one small process that keeps a pane. It owns the pty and
//! the emulator, publishes what the terminal shows as files in the pane's
//! directory (its working directory), and takes bytes and commands from two
//! FIFOs. It handles no signals: killed, it is gone, and the kernel hangs up
//! its program.

use crate::sys::{self, Winsize};
use crate::term::{Out, SIZE_MAX, Term};
use crate::{plain, replace};
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind, PipeReader, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The terminal a pane claims to be: see the README for why this is honest.
pub const TERM: &str = "xterm-256color";
/// The screen files are replaced at most once in this long (40 times a second).
const PUBLISH_EVERY: Duration = Duration::from_millis(25);
/// The most output read from the program between two publishes; past it the
/// program waits, as it would on any slow terminal.
const READ_BUDGET: usize = 512 << 10;
/// The most typed bytes and answers waiting for the program; past it `in` is
/// not read, so its writers wait.
const INPUT_MAX: usize = 64 << 10;
/// `history` becomes `history.1` when it is longer than this.
const HISTORY_MAX: u64 = 4 << 20;
const CTL_LINE_MAX: usize = 1280;
const LOG_MAX: usize = 64 << 10;
/// The longest a program may hold its screen back while it draws (mode 2026).
const DRAWING_MAX: Duration = Duration::from_millis(500);

/// A holder looks this often whether its directory is still there.
const LOOK_EVERY: Duration = Duration::from_secs(2);
const GONE: &str = "the pane's directory is gone";

/// What tells a file from the one read before: when it was written, its length, its number.
type Stamp = (std::time::SystemTime, u64, u64);

struct Holder {
    term: Term,
    master: Option<File>,
    /// Kept open so that output a program wrote just before it ended is not
    /// thrown away (macOS drops it when the last slave closes).
    _slave: File,
    pid: u32,
    /// The facts as they were written last, each in its file: one is written again only when it changes.
    facts: Vec<(&'static str, String)>,
    /// What the `status` file says.
    status: String,
    /// The last round of writing failed.
    failing: bool,
    /// How many notices there have been: the number in the `notice` file.
    noticed: u32,
    /// Since when the program has been in the middle of drawing (mode 2026).
    drawing: Option<Instant>,
    syncs: u32,
    input: Vec<u8>,
    ctl_line: Vec<u8>,
    /// The numbers of the picture files written and not yet removed.
    pictures: Vec<u32>,
    /// `running`, `hungup`, `exited CODE` or `killed SIGNAL`: the `state` file.
    state: String,
    /// What the emulator produced and the next publish writes: history, pictures, a title, a clipboard.
    pending: Out,
    /// The session's folder, where the files are that say what the terminal outside is like, and
    /// which `colours` and `keyboard` were read last.
    session: PathBuf,
    told: (Option<Stamp>, Option<Stamp>),
    /// The `history` file ends in the middle of a line.
    history_open: bool,
    /// The title in the `title` file.
    title: String,
    logged: usize,
}

/// A FIFO of ours: made if missing, opened to read without waiting, and with
/// a writer of our own so that a writer leaving is never the end of the file.
fn fifo(name: &str) -> io::Result<(File, File)> {
    if !fs::symlink_metadata(name).is_ok_and(|m| m.file_type().is_fifo()) {
        let _ = fs::remove_file(name);
        sys::make_fifo(name.as_ref())?;
    }
    let reader = OpenOptions::new().read(true).custom_flags(sys::O_NONBLOCK).open(name)?;
    let writer = OpenOptions::new().write(true).open(name)?;
    Ok((reader, writer))
}

/// Keeps the pane whose directory is the working directory until its program
/// ends. `fux start` locked the directory and handed the lock over as this
/// process's standard input: it is held for exactly as long as the holder lives.
pub fn hold(command: &[OsString], cwd: &OsString) -> Result<(), String> {
    // The first size is COLUMNS x LINES when the starter set them (the viewer
    // does, for key scripts), and 80x24 otherwise. `resize` changes it later.
    let mut odd = Vec::new();
    let mut number = |name: &'static str, or: u16| match std::env::var_os(name) {
        None => or,
        Some(value) => match value.to_str().and_then(|n| n.parse::<u16>().ok()) {
            Some(n) if (1..=SIZE_MAX).contains(&usize::from(n)) => n,
            _ => {
                odd.push(name);
                or
            }
        },
    };
    let (cols, rows) = (number("COLUMNS", 80), number("LINES", 24));
    let size = Winsize { cols, rows, xpixel: cols * 10, ypixel: rows * 20 };
    let (master, slave) = sys::pty(size).map_err(|e| format!("cannot open a pty: {e}"))?;
    let Some((program, args)) = command.split_first() else {
        return Err("no program to run".into());
    };
    // `fux start` says under what name the pane's directory was started.
    let pane = std::env::var_os(name!("PANE")).ok_or("hold is started by `fux start`")?;
    // (The session is two folders up from the pane, by the name the pane was started under.)
    let session = Path::new(&pane).ancestors().nth(2).unwrap_or(Path::new("")).to_path_buf();
    let mut child = {
        let stdio = || slave.try_clone().map(Stdio::from).map_err(|e| format!("cannot copy the pty: {e}"));
        let mut child = Command::new(program);
        // The program is told what terminal it is in and which pane. Of this
        // program's own names it gets the session's and the keys' if they
        // were set, and no other (a key script's workspace is not this
        // pane's); and no size but the terminal's own.
        let (dir, keys) = (name!("DIR"), name!("KEYS"));
        let ours = |n: &OsString| n.as_encoded_bytes().starts_with(name!("").as_bytes()) && n != dir && n != keys;
        for name in std::env::vars_os().map(|(name, _)| name).filter(ours) {
            child.env_remove(name);
        }
        child
            .args(args)
            .current_dir(cwd)
            .env("TERM", TERM)
            .env(name!("PANE"), pane)
            .env_remove("COLUMNS")
            .env_remove("LINES");
        child.env("TERM_PROGRAM", crate::TERM_PROGRAM).env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
        child.stdin(stdio()?).stdout(stdio()?).stderr(stdio()?);
        sys::new_session(&mut child, true);
        child.spawn().map_err(|e| format!("cannot start {}: {e}", program.to_string_lossy()))?
    };
    let pid = child.id();
    // The program's end arrives as a line on a pipe, so the loop waits on it
    // like on everything else.
    let (mut ended, mut end_writer) = io::pipe().map_err(|e| format!("cannot make a pipe: {e}"))?;
    let waiter = std::thread::Builder::new().spawn(move || {
        let state = match child.wait() {
            Ok(status) => match (status.code(), status.signal()) {
                (Some(code), _) => format!("exited {code}"),
                (_, signal) => format!("killed {}", signal.unwrap_or(0)),
            },
            Err(error) => format!("lost {error}"),
        };
        let _ = end_writer.write_all(state.as_bytes());
    });
    waiter.map_err(|e| format!("cannot start a thread: {e}"))?;
    // The command as `status` shows it: without control characters, and its first 4,096 characters.
    let command = plain(&command.iter().map(|a| a.to_string_lossy()).collect::<Vec<_>>().join(" "));
    let command: String = command.chars().take(4096).collect();
    let mut holder = Holder {
        term: Term::new(usize::from(cols), usize::from(rows)),
        master: Some(master),
        _slave: slave,
        pid,
        facts: Vec::new(),
        status: String::new(),
        failing: false,
        noticed: 0,
        drawing: None,
        syncs: 0,
        input: Vec::new(),
        ctl_line: Vec::new(),
        pictures: Vec::new(),
        state: "running".into(),
        pending: Out::default(),
        session,
        told: (None, None),
        history_open: false,
        title: String::new(),
        logged: 0,
    };
    for name in odd {
        holder.log(&format!("{name} is not a number from 1 to {SIZE_MAX}: the pane starts at {cols}x{rows}"));
    }
    // The FIFOs are there for as long as this function runs, and not after.
    let fifos = fifo("in").and_then(|input| Ok((input, fifo("ctl")?)));
    let result = fifos.map_err(|e| format!("cannot make the FIFOs: {e}"));
    let result =
        result.and_then(|((input, _in_writer), (ctl, _ctl_writer))| holder.run(&input, &ctl, &mut ended, &command));
    let _ = fs::remove_file("in");
    let _ = fs::remove_file("ctl");
    // A directory that is gone is how a pane is closed: no failure, and nothing to say.
    result.or_else(|why| if why == GONE { Ok(()) } else { Err(why) })
}

impl Holder {
    fn run(&mut self, input: &File, ctl: &File, ended: &mut PipeReader, command: &str) -> Result<(), String> {
        // Who holds the pane, the program's own number, and what it runs: a fact as the others
        // are, written once (it never changes), with them and before the first screen.
        self.status = format!("holder {}\npid {}\ncommand {command}\n", std::process::id(), self.pid);
        // What an earlier holder of this pane left goes before this one's first
        // screen is there: its title and clipboard, its history and its pictures.
        for old in ["title", "clipboard", "cwd", "notice"] {
            let _ = fs::remove_file(old);
        }
        let _ = fs::remove_file("history.1");
        File::create("history").map_err(|e| format!("cannot make history: {e}"))?;
        for old in fs::read_dir(".").into_iter().flatten().flatten().map(|entry| entry.file_name()) {
            let number = old.to_str().and_then(|name| name.strip_prefix("pic-")?.strip_suffix(".sixel"));
            if number.is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())) {
                let _ = fs::remove_file(old);
            }
        }
        self.publish().map_err(|e| format!("cannot write the screen: {e}"))?;

        let mut buffer = vec![0u8; 64 << 10];
        let (mut dirty, mut published, mut looked, mut budget) = (false, Instant::now(), Instant::now(), READ_BUDGET);
        loop {
            let since = published.elapsed();
            // A program in the middle of drawing (mode 2026) is not shown until it says it is done,
            // or half a second has gone by: one that never says so cannot freeze its pane.
            // (Each time it says so anew, the half second starts anew.)
            let again = std::mem::replace(&mut self.syncs, self.term.syncs) != self.term.syncs;
            self.drawing = match self.drawing {
                _ if !self.term.sync => None,
                Some(since) if !again => Some(since),
                _ => Some(Instant::now()),
            };
            let left = self.drawing.map_or(Duration::ZERO, |since| DRAWING_MAX.saturating_sub(since.elapsed()));
            let drawing = !left.is_zero() && self.master.is_some();
            if since >= PUBLISH_EVERY {
                budget = READ_BUDGET;
                if dirty && !drawing {
                    // What could not be written is tried again in the next round (and said once).
                    let written = self.publish();
                    match &written {
                        Err(e) if e.kind() == ErrorKind::NotFound => return Err(GONE.into()),
                        Err(e) if !self.failing => self.log(&format!("cannot write the pane's files: {e}")),
                        _ => {}
                    }
                    self.failing = written.is_err();
                    (dirty, published) = (self.failing, Instant::now());
                }
            }
            // The directory is the pane: when it is gone, so is this.
            if looked.elapsed() >= LOOK_EVERY {
                looked = Instant::now();
                if fs::symlink_metadata("in").is_err() {
                    return Err(GONE.into());
                }
            }
            let mut wanted = vec![(ended.as_raw_fd(), sys::POLLIN), (ctl.as_raw_fd(), sys::POLLIN)];
            if let Some(master) = &self.master {
                let read = if budget > 0 { sys::POLLIN } else { 0 };
                let write = if self.input.is_empty() { 0 } else { sys::POLLOUT };
                wanted.push((master.as_raw_fd(), read | write));
                wanted.push((input.as_raw_fd(), if self.input.len() < INPUT_MAX { sys::POLLIN } else { 0 }));
            }
            let look = LOOK_EVERY.saturating_sub(looked.elapsed());
            let wait = match dirty || budget == 0 {
                true if drawing => left.max(PUBLISH_EVERY.saturating_sub(since)).min(look),
                true => PUBLISH_EVERY.saturating_sub(since),
                false => look,
            };
            let events = sys::wait(&wanted, wait.as_millis() as i32 + 1).map_err(|e| format!("cannot wait: {e}"))?;
            let event = |i: usize| events.get(i).copied().unwrap_or(0);
            if event(1) != 0 {
                self.commands(ctl, &mut buffer);
                dirty = true;
            }
            if event(2) & !sys::POLLOUT != 0 && budget > 0 {
                let n = self.read_program(buffer.get_mut(..budget.min(64 << 10)).unwrap_or(&mut []));
                budget -= n.min(budget);
                dirty |= n > 0;
            }
            if event(3) != 0 && self.master.is_some() {
                let room = INPUT_MAX.saturating_sub(self.input.len()).min(buffer.len());
                if let Some(Ok(n)) = buffer.get_mut(..room).map(|b| (&*input).read(b)) {
                    self.input.extend_from_slice(buffer.get(..n).unwrap_or(&[]));
                }
            }
            let typed = self.master.as_ref().filter(|_| !self.input.is_empty()).map(|mut m| m.write(&self.input));
            if let Some(Ok(n)) = typed {
                self.input.drain(..n.min(self.input.len()));
            }
            if event(0) != 0 {
                let mut state = String::new();
                let _ = ended.read_to_string(&mut state);
                // What the program wrote last is still in the pty: read it, up to 4 MiB.
                let mut left = 4 << 20;
                while left > 0 {
                    match self.read_program(&mut buffer) {
                        0 => break,
                        n => left -= n.min(left),
                    }
                }
                (self.master, self.state) = (None, state);
                if let Err(e) = self.publish() {
                    self.log(&format!("cannot write the last screen (the program {}): {e}", self.state));
                }
                return Ok(());
            }
        }
    }

    /// A line in `log` (the holder's standard error). The file never passes
    /// 64 KiB: its last line says that it is full. Control characters in what
    /// is quoted are written as `?`, so `cat log` is as safe as the screens.
    fn log(&mut self, line: &str) {
        const FULL: &str = "the log is full: nothing more is written here";
        let line = plain(line);
        // Only what was written counts, so a write that fails does not close the log.
        if self.logged + line.len() + 1 + FULL.len() < LOG_MAX {
            if writeln!(io::stderr(), "{line}").is_ok() {
                self.logged += line.len() + 1;
            }
        } else if self.logged < LOG_MAX && writeln!(io::stderr(), "{FULL}").is_ok() {
            self.logged = LOG_MAX;
        }
    }

    /// Reads what the terminal outside is like, if it has changed. It is in two files of the
    /// session, if a viewer (or a person) wrote them: `colours`, and `keyboard` (the flags of
    /// the kitty protocol that terminal takes, a number). 16 KiB of each at most.
    fn told(&mut self) {
        for (name, known) in [("colours", &mut self.told.0), ("keyboard", &mut self.told.1)] {
            // (A file, not something that only has a file's name; told apart from the one read
            // last by when it was written, how long it is and which file it is.)
            let file = self.session.join(name);
            let stamp = fs::metadata(&file).ok().filter(|file| file.is_file());
            let stamp = stamp.and_then(|file| Some((file.modified().ok()?, file.len(), file.ino())));
            if stamp != *known {
                let mut text = Vec::new();
                if stamp.is_some() {
                    // (Opened without waiting, should the name have become a FIFO's meanwhile.)
                    let file = OpenOptions::new().read(true).custom_flags(sys::O_NONBLOCK).open(&file);
                    let _ = file.map(|file| file.take(16 << 10).read_to_end(&mut text));
                }
                let text = String::from_utf8_lossy(&text).into_owned();
                *known = stamp;
                match name {
                    "colours" => self.term.colours = text,
                    _ => self.term.keyboard = text.trim().parse().unwrap_or(0),
                }
            }
        }
    }

    /// Feeds the emulator what the program wrote; says how many bytes.
    fn read_program(&mut self, buffer: &mut [u8]) -> usize {
        let Some(n) = self.master.as_ref().and_then(|mut m| m.read(buffer).ok()) else {
            return 0;
        };
        // What the colours are is in the session's `colours` file, two folders up, if a viewer
        // (or a person) wrote one: read again when it has changed, 16 KiB of it at most.
        self.told();
        self.term.feed(buffer.get(..n).unwrap_or(&[]));
        self.collect();
        n
    }

    /// Takes what the emulator produced: answers go to the program and
    /// refusals to the log now; the rest waits for the next publish.
    fn collect(&mut self) {
        let mut out = self.term.take();
        if self.input.len() + out.reply.len() <= INPUT_MAX {
            self.input.extend_from_slice(&out.reply);
        } else {
            out.refused.push("an answer to the program was dropped: 64 KiB were waiting for it");
        }
        out.refused.into_iter().for_each(|refused| self.log(refused));
        // A resize took rows back from history's end: from what waits to be written, then from the file.
        let waiting = self.pending.history.len().saturating_sub(out.unhistory);
        let written = (out.unhistory - (self.pending.history.len() - waiting)) as u64;
        if self.pending.history.is_char_boundary(waiting) {
            self.pending.history.truncate(waiting);
        }
        if written > 0 {
            let cut = OpenOptions::new().write(true).open("history");
            let cut = cut.and_then(|file| file.set_len(file.metadata()?.len().saturating_sub(written)));
            if let Err(e) = cut {
                self.log(&format!("history keeps {written} bytes that are on the screen again: {e}"));
            }
            self.history_open = false;
        }
        self.pending.history.push_str(&out.history);
        self.pending.pictures.append(&mut out.pictures);
        self.pending.title = out.title.or(self.pending.title.take());
        self.pending.clipboard = out.clipboard.or(self.pending.clipboard.take());
        self.pending.cwd = out.cwd.or(self.pending.cwd.take());
        self.pending.notice = out.notice.or(self.pending.notice.take());
        self.pending.notices += out.notices;
        if self.pending.history.len() > READ_BUDGET {
            self.history();
        }
    }

    fn commands(&mut self, mut ctl: &File, buffer: &mut [u8]) {
        let n = ctl.read(buffer).unwrap_or(0);
        for &byte in buffer.get(..n).unwrap_or(&[]) {
            if byte != b'\n' {
                self.ctl_line.push(byte);
                continue;
            }
            let line = String::from_utf8_lossy(&std::mem::take(&mut self.ctl_line)).into_owned();
            if line.len() > CTL_LINE_MAX {
                self.log("ctl: a line over 1280 bytes was dropped");
            } else if let Err(why) = self.command(&line) {
                self.log(&format!("ctl: {line}: {why}"));
            }
        }
        self.ctl_line.truncate(CTL_LINE_MAX + 1);
    }

    fn command(&mut self, line: &str) -> Result<(), String> {
        let mut words = line.split_whitespace();
        match words.next() {
            // Anyone may have something noticed: the words after `notify`, as a program's own notice.
            Some("notify") => {
                let text = line.trim_start().strip_prefix("notify").unwrap_or_default().trim();
                let text: String = text.chars().filter(|c| !c.is_control()).take(1024).collect();
                (self.pending.notice, self.pending.notices) = (Some(text), self.pending.notices + 1);
                Ok(())
            }
            Some("kill") if words.next().is_none() => {
                // Closing the master is the hangup. What the program and its
                // children do about it is theirs to decide.
                if self.master.take().is_some() {
                    self.state = "hungup".into();
                }
                Ok(())
            }
            Some("resize") => {
                let numbers: Vec<u16> = words.map(|word| word.parse().unwrap_or(0)).collect();
                let (cols, rows, cell) = match numbers.as_slice() {
                    [cols @ 1..=500, rows @ 1..=500] => (*cols, *rows, None),
                    [cols @ 1..=500, rows @ 1..=500, w @ 1..=100, h @ 1..=100] => (*cols, *rows, Some((*w, *h))),
                    _ => return Err("resize takes COLS ROWS, each 1 to 500, then perhaps a cell's WIDTH HEIGHT in pixels, each 1 to 100".into()),
                };
                let old = (self.term.cell.0 as u16, self.term.cell.1 as u16);
                let cell = cell.unwrap_or(old);
                if self.master.is_none() {
                    return Err("the program was hung up".into());
                }
                // What the program has written so far, it wrote for the old size:
                // it is drawn at that size first (up to 1 MiB of it).
                let mut buffer = vec![0u8; 64 << 10];
                for _ in 0..16 {
                    if self.read_program(&mut buffer) == 0 {
                        break;
                    }
                }
                let Some(master) = &self.master else {
                    return Err("the program was hung up".into());
                };
                let size = Winsize { cols, rows, xpixel: cols * cell.0, ypixel: rows * cell.1 };
                sys::set_winsize(master, size).map_err(|e| e.to_string())?;
                if cell != old {
                    // Pictures were laid out in cells of the old size.
                    self.term.pictures.clear();
                    self.term.other_pictures.clear();
                    self.term.cell = (usize::from(cell.0), usize::from(cell.1));
                }
                self.term.resize(usize::from(cols), usize::from(rows));
                self.collect();
                Ok(())
            }
            _ => Err("not a command (kill, resize COLS ROWS, notify TEXT)".into()),
        }
    }

    /// Appends the lines that scrolled off to `history`. Lines that cannot be
    /// written are lost, and the log says so: they are not kept to try again.
    fn history(&mut self) {
        let lines = std::mem::take(&mut self.pending.history);
        // A file is put aside where a line ends, so that no line is in two files (one that
        // never ends is cut when the file is twice its size).
        let rotated = match fs::metadata("history") {
            Ok(m) if m.len() > HISTORY_MAX && (!self.history_open || m.len() > 2 * HISTORY_MAX) => {
                fs::rename("history", "history.1")
            }
            _ => Ok(()),
        };
        self.history_open = !lines.ends_with('\n');
        let file = rotated.and_then(|()| OpenOptions::new().append(true).create(true).open("history"));
        if let Err(e) = file.and_then(|mut file| file.write_all(lines.as_bytes())) {
            self.log(&format!("history was lost (some or all of {} bytes): {e}", lines.len()));
        }
    }

    /// Writes what changed: history first, then pictures, the title and the
    /// clipboard, then both screens; so nothing a screen names is missing and
    /// no line is in neither file.
    fn publish(&mut self) -> io::Result<()> {
        if !self.pending.history.is_empty() {
            self.history();
        }
        for (id, data) in std::mem::take(&mut self.pending.pictures) {
            // One that left the screen before it was written has no reader. One that
            // cannot be written is not on the screen either: it goes, and the log says why.
            let name = format!("pic-{id}.sixel");
            if !self.term.pictures.iter().chain(&self.term.other_pictures).any(|p| p.id == id) {
                continue;
            }
            self.pictures.push(id);
            if let Err(e) = fs::write(&name, data) {
                let _ = fs::remove_file(&name);
                self.term.pictures.retain(|p| p.id != id);
                self.term.other_pictures.retain(|p| p.id != id);
                self.log(&format!("a picture was dropped, as its file could not be written: {e}"));
            }
        }
        // Every fact has a file of its own, written when it changes, before the screens. A reader
        // may find a fact one publish ahead of the text it read: it keeps the cursor inside the
        // rows it has.
        let placed =
            |p: &crate::term::Picture| format!("pic-{}.sixel {} {} {} {}\n", p.id, p.row, p.col, p.cols, p.rows);
        let term = &mut self.term;
        let shown = if term.cursor_shown { "shown" } else { "hidden" };
        let facts = [
            ("top", format!("{}\n", term.top())),
            ("wraps", format!("{}\n", term.wraps())),
            ("pictures", term.pictures.iter().map(placed).collect()),
            ("size", format!("{} {}\n", term.cols, term.rows)),
            ("cursor", format!("{} {} {shown} {}\n", term.row, term.col, term.cursor_shape)),
            ("modes", format!("{}\n", term.words())),
            ("state", format!("{}\n", self.state)),
            ("status", self.status.clone()),
        ];
        for (name, text) in facts {
            if !self.facts.iter().any(|(fact, was)| *fact == name && *was == text) {
                replace(name, text.as_bytes())?;
                self.facts.retain(|(fact, _)| *fact != name);
                self.facts.push((name, text));
            }
        }
        // The title is one line, rewritten only when it changes; the clipboard is the bytes the program set.
        if let Some(title) = self.pending.title.take().filter(|title| *title != self.title) {
            match replace("title", format!("{title}\n").as_bytes()) {
                Ok(()) => self.title = title,
                Err(e) => self.log(&format!("the title could not be written: {e}")),
            }
        }
        if let Some(Err(e)) = self.pending.clipboard.take().map(|bytes| replace("clipboard", &bytes)) {
            self.log(&format!("the clipboard could not be written: {e}"));
        }
        // The directory the program says it is in: kept if it is one of this machine (no host
        // named, or this one's name), and forgotten if it is another machine's (a shell at the
        // far end of ssh): a path of there means nothing here.
        if let Some((host, path)) = self.pending.cwd.take() {
            let same = |name: &str| name.eq_ignore_ascii_case(&host);
            let here = host.is_empty() || same("localhost") || sys::host().is_some_and(|name| same(&name));
            let written = match (here, path.starts_with('/')) {
                (true, true) => replace("cwd", format!("{path}\n").as_bytes()),
                (true, false) => Ok(()),
                (false, _) => fs::remove_file("cwd"),
            };
            if let Err(e) = written.or_else(|e| if e.kind() == ErrorKind::NotFound { Ok(()) } else { Err(e) }) {
                self.log(&format!("the directory could not be written: {e}"));
            }
        }
        // What the program wanted noticed last: a number that grows with each one (so that the
        // same words twice are two), then the words; the number alone is the bell.
        if let Some(text) = self.pending.notice.take() {
            self.noticed = self.noticed.wrapping_add(std::mem::take(&mut self.pending.notices).max(1));
            let line = format!("{} {text}", self.noticed).trim_end().to_string() + "\n";
            if let Err(e) = replace("notice", line.as_bytes()) {
                self.log(&format!("a notice could not be written: {e}"));
            }
        }
        // The screens are text and nothing else, written last of all, `screen.styled` at the very
        // end: who watches a pane watches that one file, and finds every fact already there.
        let term = &self.term;
        replace("screen", term.plain().as_bytes())?;
        replace("screen.styled", term.styled().as_bytes())?;
        // A picture neither screen has any more (the one shown, the one behind) has no reader left.
        self.pictures.sort_unstable();
        self.pictures.dedup();
        self.pictures.retain(|id| {
            let kept = self.term.pictures.iter().chain(&self.term.other_pictures).any(|p| p.id == *id);
            if !kept {
                let _ = fs::remove_file(format!("pic-{id}.sixel"));
            }
            kept
        });
        Ok(())
    }
}
