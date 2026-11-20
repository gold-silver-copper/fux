//! The holder: the one small process that keeps a pane. It owns the pty and
//! the emulator, publishes what the terminal shows as files in the pane's
//! directory (its working directory), and takes bytes and commands from two
//! FIFOs. It handles no signals: killed, it is gone, and the kernel hangs up
//! its program.

use crate::sys::{self, Winsize};
use crate::term::{SIZE_MAX, Term};
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind, PipeReader, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt};
use std::os::unix::process::ExitStatusExt;
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
const CTL_LINE_MAX: usize = 256;
const LOG_MAX: usize = 64 << 10;
/// A holder looks this often whether its directory is still there.
const LOOK_EVERY: Duration = Duration::from_secs(2);
const GONE: &str = "the pane's directory is gone";

struct Holder {
    term: Term,
    master: Option<File>,
    /// Kept open so that output a program wrote just before it ended is not
    /// thrown away (macOS drops it when the last slave closes).
    _slave: File,
    pid: u32,
    version: u64,
    input: Vec<u8>,
    ctl_line: Vec<u8>,
    /// The numbers of the picture files written and not yet removed.
    pictures: Vec<u32>,
    command: String,
    /// `running`, `hungup`, `exited CODE` or `killed SIGNAL`: in `status` and on line 1 of the screens.
    state: String,
    /// The state `status` holds: it is written again, after the screens, when the state has changed.
    told: String,
    logged: usize,
}

/// Text for a file a person reads with `cat`: control characters become `?`.
pub fn plain(text: &str) -> String {
    text.chars().map(|c| if c.is_control() { '?' } else { c }).collect()
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

/// Replaces a file in one step, so a reader sees the old one or the new one.
pub fn replace(name: &str, content: &[u8]) -> io::Result<()> {
    let new = format!("{name}.new");
    fs::write(&new, content)?;
    fs::rename(&new, name)
}

/// Keeps the pane whose directory is the working directory until its program
/// ends. `fux3 start` locked the directory and handed the lock over as this
/// process's standard input: it is held for exactly as long as the holder lives.
pub fn hold(command: &[OsString], cwd: &OsString) -> Result<(), String> {
    // The first size is COLUMNS x LINES when the starter set them (the viewer
    // does, for key scripts), and 80x24 otherwise. `resize` changes it later.
    let mut odd = None;
    let mut number = |name: &'static str, or: u16| match std::env::var(name).ok().map(|n| n.parse::<u16>()) {
        Some(Ok(n)) if (1..=SIZE_MAX).contains(&usize::from(n)) => n,
        Some(_) => {
            odd = Some(name);
            or
        }
        None => or,
    };
    let (cols, rows) = (number("COLUMNS", 80), number("LINES", 24));
    let size = Winsize { cols, rows, xpixel: cols * 10, ypixel: rows * 20 };
    let (master, slave) = sys::pty(size).map_err(|e| format!("cannot open a pty: {e}"))?;
    let Some((program, args)) = command.split_first() else {
        return Err("no program to run".into());
    };
    let pane = std::env::current_dir().map_err(|e| format!("no working directory: {e}"))?;
    let mut child = {
        let stdio = || slave.try_clone().map(Stdio::from).map_err(|e| format!("cannot copy the pty: {e}"));
        let mut child = Command::new(program);
        child.args(args).current_dir(cwd).env("TERM", TERM).env("FUX3C_PANE", pane);
        child.env_remove("COLUMNS").env_remove("LINES").stdin(stdio()?).stdout(stdio()?).stderr(stdio()?);
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
    let command = plain(&command.iter().map(|a| a.to_string_lossy()).collect::<Vec<_>>().join(" "));
    let mut holder = Holder {
        term: Term::new(usize::from(cols), usize::from(rows)),
        master: Some(master),
        _slave: slave,
        pid,
        version: 0,
        input: Vec::new(),
        ctl_line: Vec::new(),
        pictures: Vec::new(),
        command,
        state: "running".into(),
        told: String::new(),
        logged: 0,
    };
    if let Some(name) = odd {
        holder.log(&format!("{name} is not a number from 1 to {SIZE_MAX}: the pane starts at {cols}x{rows}"));
    }
    // The FIFOs are there for as long as this function runs, and not after.
    let fifos = fifo("in").and_then(|input| Ok((input, fifo("ctl")?)));
    let result = fifos.map_err(|e| format!("cannot make the FIFOs: {e}"));
    let result = result.and_then(|((input, _in_writer), (ctl, _ctl_writer))| holder.run(&input, &ctl, &mut ended));
    let _ = fs::remove_file("in");
    let _ = fs::remove_file("ctl");
    result
}

impl Holder {
    fn run(&mut self, input: &File, ctl: &File, ended: &mut PipeReader) -> Result<(), String> {
        // What an earlier holder of this pane left is this one's to replace.
        let _ = fs::remove_file("history.1");
        File::create("history").map_err(|e| format!("cannot make history: {e}"))?;
        for old in fs::read_dir(".").into_iter().flatten().flatten().map(|entry| entry.file_name()) {
            if old.to_str().is_some_and(|name| name.starts_with("pic-") && name.ends_with(".sixel")) {
                let _ = fs::remove_file(old);
            }
        }
        self.publish().map_err(|e| format!("cannot write the screen: {e}"))?;

        let mut buffer = vec![0u8; 64 << 10];
        let (mut dirty, mut published, mut looked, mut budget) = (false, Instant::now(), Instant::now(), READ_BUDGET);
        loop {
            let since = published.elapsed();
            if since >= PUBLISH_EVERY {
                budget = READ_BUDGET;
                if dirty {
                    match self.publish() {
                        Err(e) if e.kind() == ErrorKind::NotFound => return Err(GONE.into()),
                        Err(e) => self.log(&format!("cannot write the screen: {e}")),
                        Ok(()) => {}
                    }
                    (dirty, published) = (false, Instant::now());
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
            let wait = if dirty || budget == 0 { PUBLISH_EVERY.saturating_sub(since) } else { LOOK_EVERY };
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
                // The end is in `status` even when the last screen cannot be written.
                if let Err(e) = self.publish().and(self.tell()) {
                    self.log(&format!("cannot write the last screen: {e}"));
                    let _ = self.tell();
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
        if self.logged + line.len() + 1 + FULL.len() < LOG_MAX {
            self.logged += line.len() + 1;
            let _ = writeln!(io::stderr(), "{line}");
        } else if self.logged < LOG_MAX {
            self.logged = LOG_MAX;
            let _ = writeln!(io::stderr(), "{FULL}");
        }
    }

    /// Feeds the emulator what the program wrote; says how many bytes.
    fn read_program(&mut self, buffer: &mut [u8]) -> usize {
        let Some(n) = self.master.as_ref().and_then(|mut m| m.read(buffer).ok()) else {
            return 0;
        };
        self.term.feed(buffer.get(..n).unwrap_or(&[]));
        let reply = std::mem::take(&mut self.term.reply);
        if self.input.len() + reply.len() <= INPUT_MAX {
            self.input.extend_from_slice(&reply);
        } else {
            self.term.refused = Some("an answer to the program was dropped: 64 KiB were waiting for it");
        }
        if let Some(refused) = self.term.refused.take() {
            self.log(refused);
        }
        if self.term.history.len() > READ_BUDGET {
            self.history();
        }
        n
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
                self.log("ctl: a line over 256 bytes was dropped");
            } else if let Err(why) = self.command(&line) {
                self.log(&format!("ctl: {line}: {why}"));
            }
        }
        self.ctl_line.truncate(CTL_LINE_MAX + 1);
    }

    fn command(&mut self, line: &str) -> Result<(), String> {
        let mut words = line.split_whitespace();
        match words.next() {
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
                Ok(())
            }
            _ => Err("not a command (kill, resize COLS ROWS)".into()),
        }
    }

    /// Appends the lines that scrolled off to `history`. Lines that cannot be
    /// written are lost, and the log says so: they are not kept to try again.
    fn history(&mut self) {
        let lines = std::mem::take(&mut self.term.history);
        if fs::metadata("history").is_ok_and(|m| m.len() > HISTORY_MAX) {
            let _ = fs::rename("history", "history.1");
        }
        let file = OpenOptions::new().append(true).create(true).open("history");
        if let Err(e) = file.and_then(|mut file| file.write_all(lines.as_bytes())) {
            self.log(&format!("{} bytes of history were lost: {e}", lines.len()));
        }
    }

    /// Writes what changed: history first, then pictures, then both screens,
    /// then `status` if the state changed; so nothing a screen names is
    /// missing, no line is in neither file, and `status` never runs ahead.
    fn publish(&mut self) -> io::Result<()> {
        if !self.term.history.is_empty() {
            self.history();
        }
        // A picture that cannot be written is not on the screen: it goes, and the log says why.
        let mut failed = None;
        self.term.pictures.retain_mut(|picture| {
            let Some(data) = &picture.data else { return true };
            let name = format!("pic-{}.sixel", picture.id);
            match fs::write(&name, data) {
                Ok(()) => picture.data = None,
                Err(e) => {
                    let _ = fs::remove_file(&name);
                    failed = Some(format!("a picture was dropped, as its file could not be written: {e}"));
                }
            }
            self.pictures.push(picture.id);
            picture.data.is_none()
        });
        if let Some(failed) = failed {
            self.log(&failed);
        }
        let placed =
            |p: &crate::term::Picture| format!(" pic=pic-{}.sixel@{},{}+{}x{}", p.id, p.row, p.col, p.cols, p.rows);
        let pictures: String = self.term.pictures.iter().map(placed).collect();
        self.version += 1;
        let term = &self.term;
        let modes = [
            (term.cursor_shown, "cursor"),
            (term.app_cursor, "appcursor"),
            (term.app_keypad, "keypad"),
            (term.paste, "paste"),
            (term.alt, "alt"),
        ];
        let modes: Vec<&str> = modes.iter().filter(|m| m.0).map(|m| m.1).collect();
        let header = format!(
            "pid={} state={} v={} size={}x{} cursor={},{} shape={} modes={}{pictures}\n",
            self.pid,
            self.state.replace(' ', ":"),
            self.version,
            term.cols,
            term.rows,
            term.row,
            term.col,
            term.cursor_shape,
            if modes.is_empty() { "-".into() } else { modes.join(",") },
        );
        replace("screen", (header.clone() + &term.plain()).as_bytes())?;
        replace("screen.styled", (header + &term.styled()).as_bytes())?;
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
        self.tell()
    }

    /// Writes `status` if the state is not the one it holds.
    fn tell(&mut self) -> io::Result<()> {
        if self.told != self.state {
            let (pid, holder) = (self.pid, std::process::id());
            let status = format!("pid {pid}\nholder {holder}\nstate {}\ncommand {}\n", self.state, self.command);
            replace("status", status.as_bytes())?;
            self.told.clone_from(&self.state);
        }
        Ok(())
    }
}
