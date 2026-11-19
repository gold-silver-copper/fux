//! The holder: the one small process that keeps a pane. It owns the pty and
//! the emulator, publishes what the terminal shows as files in the pane's
//! directory (its working directory), and takes bytes and commands from two
//! FIFOs. It handles no signals: killed, it is gone, and the kernel hangs up
//! its program.

use crate::sys::{self, Winsize};
use crate::term::Term;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind, Read, Write};
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
/// An idle holder looks this often whether its directory is still there.
const LOOK_EVERY: i32 = 2000;

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
    pictures: Vec<u32>,
    command: String,
    /// `running`, `hungup`, `exited CODE` or `killed SIGNAL`: in `status` and on line 1 of the screens.
    state: String,
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

/// Replaces a file in one step, so a reader sees the old one or the new one.
pub fn replace(name: &str, content: &[u8]) -> io::Result<()> {
    let new = format!("{name}.new");
    fs::write(&new, content)?;
    fs::rename(&new, name)
}

/// Keeps the pane whose directory is the working directory until its program ends.
pub fn hold(command: &[OsString], cwd: &OsString) -> Result<(), String> {
    let lock = File::open(".").map_err(|e| format!("cannot open the pane's directory: {e}"))?;
    // A `fux3 wait` may hold the lock for an instant: try for a little while.
    let locked = (0..20).any(|i| {
        std::thread::sleep(Duration::from_millis(5 * u64::from(i > 0)));
        lock.try_lock().is_ok()
    });
    if !locked {
        return Err("a holder already keeps this pane".into());
    }
    let (input, _input_writer) = fifo("in").map_err(|e| format!("cannot make in: {e}"))?;
    let (ctl, _ctl_writer) = fifo("ctl").map_err(|e| format!("cannot make ctl: {e}"))?;
    // The first size is COLUMNS x LINES when the starter set them (the viewer
    // does, for key scripts), and 80x24 otherwise. `resize` changes it later.
    let number = |name: &str, or: u16| {
        std::env::var(name).ok().and_then(|n| n.parse().ok()).filter(|n| (1..=1000).contains(n)).unwrap_or(or)
    };
    let (cols, rows) = (number("COLUMNS", 80), number("LINES", 24));
    let size = Winsize { cols, rows, xpixel: cols * 10, ypixel: rows * 20 };
    let (master, slave) = sys::pty(size).map_err(|e| format!("cannot open a pty: {e}"))?;
    let stdio = || slave.try_clone().map(Stdio::from).map_err(|e| format!("cannot copy the pty: {e}"));
    let Some((program, args)) = command.split_first() else {
        return Err("no program to run".into());
    };
    let pane = std::env::current_dir().map_err(|e| format!("no working directory: {e}"))?;
    let mut child = Command::new(program);
    child
        .args(args)
        .current_dir(cwd)
        .env("TERM", TERM)
        .env("FUX3C_PANE", pane)
        .env_remove("COLUMNS")
        .env_remove("LINES");
    child.stdin(stdio()?).stdout(stdio()?).stderr(stdio()?);
    sys::new_session(&mut child, true);
    let mut child = child.spawn().map_err(|e| format!("cannot start {}: {e}", program.to_string_lossy()))?;
    let pid = child.id();
    // The program's end arrives as a line on a pipe, so the loop below waits
    // on it like on everything else.
    let (mut ended, mut end_writer) = io::pipe().map_err(|e| format!("cannot make a pipe: {e}"))?;
    std::thread::spawn(move || {
        let state = match child.wait() {
            Ok(status) => match (status.code(), status.signal()) {
                (Some(code), _) => format!("exited {code}"),
                (_, signal) => format!("killed {}", signal.unwrap_or(0)),
            },
            Err(error) => format!("lost {error}"),
        };
        let _ = end_writer.write_all(state.as_bytes());
    });
    let command = command.iter().map(|a| a.to_string_lossy()).collect::<Vec<_>>().join(" ");
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
        logged: 0,
    };
    let _ = fs::remove_file("history.1");
    File::create("history").map_err(|e| format!("cannot make history: {e}"))?;
    holder.publish().map_err(|e| format!("cannot write the screen: {e}"))?;
    holder.status().map_err(|e| format!("cannot write status: {e}"))?;

    let mut buffer = vec![0u8; 64 << 10];
    let (mut dirty, mut published, mut budget) = (false, Instant::now(), READ_BUDGET);
    loop {
        let since = published.elapsed();
        if since >= PUBLISH_EVERY {
            budget = READ_BUDGET;
            if dirty {
                match holder.publish() {
                    Err(e) if e.kind() == ErrorKind::NotFound => {
                        return Err("the pane's directory is gone".into());
                    }
                    Err(e) => holder.log(&format!("cannot write the screen: {e}")),
                    Ok(()) => {}
                }
                (dirty, published) = (false, Instant::now());
            }
        }
        let master = holder.master.as_ref().map(|m| m.as_raw_fd());
        let mut wanted = vec![(ended.as_raw_fd(), sys::POLLIN), (ctl.as_raw_fd(), sys::POLLIN)];
        if let Some(master) = master {
            let events =
                if budget > 0 { sys::POLLIN } else { 0 } | if holder.input.is_empty() { 0 } else { sys::POLLOUT };
            wanted.push((master, events));
            wanted.push((input.as_raw_fd(), if holder.input.len() < INPUT_MAX { sys::POLLIN } else { 0 }));
        }
        let timeout =
            if dirty || budget == 0 { PUBLISH_EVERY.saturating_sub(since).as_millis() as i32 + 1 } else { LOOK_EVERY };
        let events = sys::wait(&wanted, timeout).map_err(|e| format!("cannot wait: {e}"))?;
        let ready = |i: usize| events.get(i).is_some_and(|e| *e != 0);
        if events.iter().all(|e| *e == 0) && !dirty && fs::symlink_metadata("in").is_err() {
            return Err("the pane's directory is gone".into());
        }
        if ready(1) {
            holder.commands(&ctl, &mut buffer);
            dirty = true;
        }
        if ready(2) {
            let n = holder.read_program(&mut buffer);
            budget = budget.saturating_sub(n);
            dirty |= n > 0;
            holder.write_program();
        }
        if ready(3) && holder.master.is_some() {
            let room = INPUT_MAX.saturating_sub(holder.input.len()).min(buffer.len());
            if let Some(Ok(n)) = buffer.get_mut(..room).map(|b| (&input).read(b)) {
                holder.input.extend_from_slice(buffer.get(..n).unwrap_or(&[]));
            }
            holder.write_program();
        }
        if ready(0) {
            let mut state = String::new();
            let _ = ended.read_to_string(&mut state);
            // What the program wrote last is still in the pty: read it, up to a bound.
            let mut left = 4 << 20;
            while left > 0 {
                match holder.read_program(&mut buffer) {
                    0 => break,
                    n => left -= n.min(left),
                }
            }
            (holder.master, holder.state) = (None, state);
            let _ = holder.publish();
            let _ = holder.status();
            let _ = fs::remove_file("in");
            let _ = fs::remove_file("ctl");
            return Ok(());
        }
    }
}

impl Holder {
    /// A line in `log` (the holder's standard error), which stops at 64 KiB.
    fn log(&mut self, line: &str) {
        if self.logged < LOG_MAX {
            self.logged += line.len() + 1;
            eprintln!("{line}");
            if self.logged >= LOG_MAX {
                eprintln!("the log is full: nothing more is written here");
            }
        }
    }

    fn status(&self) -> io::Result<()> {
        replace("status", format!("pid {}\nstate {}\ncommand {}\n", self.pid, self.state, self.command).as_bytes())
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
        n
    }

    fn write_program(&mut self) {
        if let Some(Ok(n)) = self.master.as_ref().map(|mut m| m.write(&self.input)) {
            self.input.drain(..n.min(self.input.len()));
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
            Some("kill") => {
                // Closing the master is the hangup. What the program and its
                // children do about it is theirs to decide.
                if self.master.take().is_some() {
                    self.state = "hungup".into();
                    self.status().map_err(|e| e.to_string())?;
                }
                Ok(())
            }
            Some("resize") => {
                let mut number = |low: u16, high: u16| {
                    words.next().and_then(|w| w.parse::<u16>().ok()).filter(|n| (low..=high).contains(n))
                };
                let (Some(cols), Some(rows)) = (number(1, 1000), number(1, 1000)) else {
                    return Err(
                        "resize takes COLS ROWS, each 1 to 1000, and may take a cell's WIDTH HEIGHT in pixels".into()
                    );
                };
                let cell = (
                    number(1, 100).unwrap_or(self.term.cell.0 as u16),
                    number(1, 100).unwrap_or(self.term.cell.1 as u16),
                );
                let Some(master) = &self.master else {
                    return Err("the program was hung up".into());
                };
                let size =
                    Winsize { cols, rows, xpixel: cols.saturating_mul(cell.0), ypixel: rows.saturating_mul(cell.1) };
                sys::set_winsize(master, size).map_err(|e| e.to_string())?;
                self.term.cell = (usize::from(cell.0), usize::from(cell.1));
                self.term.resize(usize::from(cols), usize::from(rows));
                Ok(())
            }
            _ => Err("not a command (kill, resize COLS ROWS)".into()),
        }
    }

    /// Writes what changed: history first, then pictures, then both screens,
    /// so nothing a screen names is missing and no line is in neither file.
    fn publish(&mut self) -> io::Result<()> {
        if !self.term.history.is_empty() {
            if fs::metadata("history").is_ok_and(|m| m.len() > HISTORY_MAX) {
                fs::rename("history", "history.1")?;
            }
            let mut history = OpenOptions::new().append(true).create(true).open("history")?;
            history.write_all(self.term.history.as_bytes())?;
            self.term.history.clear();
        }
        let mut pictures = String::new();
        for picture in &mut self.term.pictures {
            let name = format!("pic-{}.sixel", picture.id);
            if let Some(data) = picture.data.take() {
                fs::write(&name, data)?;
                self.pictures.push(picture.id);
            }
            pictures
                .push_str(&format!(" pic={name}@{},{}+{}x{}", picture.row, picture.col, picture.cols, picture.rows));
        }
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
        // A picture no screen names any more has no reader left.
        self.pictures.retain(|id| {
            let kept = self.term.pictures.iter().any(|p| p.id == *id);
            if !kept {
                let _ = fs::remove_file(format!("pic-{id}.sixel"));
            }
            kept
        });
        Ok(())
    }
}
