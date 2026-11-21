//! The holder: the one small process that keeps a pane. It owns the pty and
//! the emulator, publishes what the terminal shows as files in the pane's
//! directory (its working directory), and takes bytes and commands from two
//! FIFOs. It handles no signals: killed, it is gone, and the kernel hangs up
//! its program.

use crate::sys::{self, Winsize};
use crate::term::{Out, SIZE_MAX, Term};
use crate::{plain, replace};
use std::collections::{BTreeSet, HashMap};
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind, PipeReader, Read, Seek, Write};
use std::os::fd::AsFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The terminal a pane claims to be: see the README for why this is honest.
pub const TERM: &str = "xterm-256color";
/// While the program keeps writing (never a millisecond without output), the screen files are
/// replaced at most once in this long (40 times a second).
const PUBLISH_EVERY: Duration = Duration::from_millis(25);
/// What a program wrote before it went quiet is shown at once, but not sooner than this after
/// the last time (125 times a second, for a program that writes a little every few milliseconds).
const PUBLISH_QUIET: Duration = Duration::from_millis(8);
/// A program has gone quiet when it wrote nothing for this long.
const QUIET: Duration = Duration::from_millis(1);
/// The most output read from the program between two publishes; past it the
/// program waits, as it would on any slow terminal.
const READ_BUDGET: usize = 512 << 10;
/// The most typed bytes and answers waiting for the program; past it `in` is
/// not read, so its writers wait.
const INPUT_MAX: usize = 64 << 10;
/// `history` becomes `history.1` when it is longer than this, or `history.styled` is (and that
/// `history.styled.1` with it: the two have the same lines).
const HISTORY_MAX: u64 = 4 << 20;
const CTL_LINE_MAX: usize = 1280;
const LOG_MAX: usize = 64 << 10;
/// The longest a program may hold its screen back while it draws (mode 2026).
const DRAWING_MAX: Duration = Duration::from_millis(500);

/// A holder looks this often whether its directory is still there.
const LOOK_EVERY: Duration = Duration::from_secs(2);

/// What tells a file from the one read before: when it was written, its length, its number.
type Stamp = (std::time::SystemTime, u64, u64);

struct Holder {
    term: Term,
    master: Option<File>,
    /// Kept open so that output a program wrote just before it ended is not
    /// thrown away (macOS drops it when the last slave closes).
    _slave: File,
    /// The facts as they were written last, each in its file: one is written again only when it changes.
    facts: HashMap<&'static str, Option<Vec<u8>>>,
    /// What the program said last of its title, clipboard, directory and what to notice: facts
    /// as the others, each in its file, or (the directory of another machine) no file.
    said: Vec<(&'static str, Option<Vec<u8>>)>,
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
    pictures: BTreeSet<u32>,
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
    /// `history.styled` could not be begun anew yet (see `split`): nothing is written to it.
    styled_lost: bool,
    logged: usize,
}

/// A FIFO of ours: made if missing, opened to read without waiting, and with
/// a writer of our own so that a writer leaving is never the end of the file.
fn fifo(name: &str) -> io::Result<(File, File)> {
    if !fs::symlink_metadata(name).is_ok_and(|m| m.file_type().is_fifo()) {
        let _ = fs::remove_file(name);
        sys::make_fifo(name.as_ref())?;
    }
    let reader = sys::open(name, false)?;
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
        let kept = [name!("DIR"), name!("KEYS"), name!("EDIT")];
        let ours =
            |n: &OsString| n.as_encoded_bytes().starts_with(name!("").as_bytes()) && !kept.iter().any(|k| n == k);
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
        child
            .env("COLORTERM", "truecolor")
            .env("TERM_PROGRAM", crate::TERM_PROGRAM)
            .env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
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
        facts: HashMap::new(),
        said: Vec::new(),
        // Who holds the pane, the program's own number, and what it runs: a fact as the others
        // are, written once (it never changes), with them and before the first screen.
        status: format!("holder {}\npid {pid}\ncommand {command}\n", std::process::id()),
        failing: false,
        noticed: 0,
        drawing: None,
        syncs: 0,
        input: Vec::new(),
        ctl_line: Vec::new(),
        pictures: BTreeSet::new(),
        state: "running".into(),
        pending: Out::default(),
        session,
        told: (None, None),
        history_open: false,
        styled_lost: false,
        logged: 0,
    };
    holder.term.told().name = format!("{} {}", crate::TERM_PROGRAM, env!("CARGO_PKG_VERSION"));
    for name in odd {
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
        // What an earlier holder of this pane left goes before this one's first
        // screen is there: its title and clipboard, its history and its pictures.
        for old in ["title", "clipboard", "cwd", "notice"] {
            let _ = fs::remove_file(old);
        }
        for history in ["history", "history.styled"] {
            let _ = fs::remove_file(format!("{history}.1"));
            File::create(history).map_err(|e| format!("cannot make {history}: {e}"))?;
        }
        for old in fs::read_dir(".").into_iter().flatten().flatten().map(|entry| entry.file_name()) {
            let number = old.to_str().and_then(|name| name.strip_prefix("pic-")?.strip_suffix(".sixel"));
            if number.is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())) {
                let _ = fs::remove_file(old);
            }
        }
        self.publish().map_err(|e| format!("cannot write the screen: {e}"))?;

        let mut buffer = vec![0u8; 64 << 10];
        let (mut dirty, mut published, mut looked, mut budget) = (false, Instant::now(), Instant::now(), READ_BUDGET);
        // When the program last wrote anything, and when it was last given its read budget.
        let (mut heard, mut filled) = (Instant::now(), Instant::now());
        loop {
            let since = published.elapsed();
            // The program has gone quiet: nothing more from it for a millisecond (whatever else
            // the holder was woken for meanwhile), and not because it is not being read.
            let quiet = budget > 0 && heard.elapsed() >= QUIET;
            // A program in the middle of drawing (mode 2026) is not shown until it says it is done,
            // or half a second has gone by: one that never says so cannot freeze its pane.
            // (The half second runs from the first time it said so with no end since.)
            let again = std::mem::replace(&mut self.syncs, self.term.screen().syncs) != self.term.screen().syncs;
            self.drawing = match self.drawing {
                _ if !self.term.screen().sync => None,
                Some(since) if !again => Some(since),
                _ => Some(Instant::now()),
            };
            let left = self.drawing.map_or(Duration::ZERO, |since| DRAWING_MAX.saturating_sub(since.elapsed()));
            let drawing = !left.is_zero() && self.master.is_some();
            let every = if quiet && !self.failing { PUBLISH_QUIET } else { PUBLISH_EVERY };
            // The budget is for what is read between two screens; while a program in the middle
            // of drawing is not shown, for what is read in the time of one.
            if drawing && filled.elapsed() >= PUBLISH_EVERY {
                (budget, filled) = (READ_BUDGET, Instant::now());
            }
            if since >= every && dirty && !drawing {
                // What could not be written is tried again in the next round (and said once).
                let written = self.publish();
                match &written {
                    // (A file that is not there means the pane is gone only if its `in` is.)
                    Err(e) if e.kind() == ErrorKind::NotFound && fs::symlink_metadata("in").is_err() => {
                        return Ok(());
                    }
                    Err(e) if !self.failing => self.log(&format!("cannot write the pane's files: {e}")),
                    _ => {}
                }
                self.failing = written.is_err();
                (dirty, published) = (self.failing, Instant::now());
                (budget, filled) = (READ_BUDGET, published);
            }
            // The directory is the pane: when it is gone, so is this.
            if looked.elapsed() >= LOOK_EVERY {
                looked = Instant::now();
                if fs::symlink_metadata("in").is_err() {
                    return Ok(());
                }
            }
            let master = self.master.as_ref();
            let read = if budget > 0 { sys::POLLIN } else { 0 };
            let write = if self.input.is_empty() { 0 } else { sys::POLLOUT };
            let wanted = [
                (Some(ended.as_fd()), sys::POLLIN),
                (Some(ctl.as_fd()), sys::POLLIN),
                (master.map(AsFd::as_fd), read | write),
                (master.map(|_| input.as_fd()), if self.input.len() < INPUT_MAX { sys::POLLIN } else { 0 }),
            ];
            let look = LOOK_EVERY.saturating_sub(looked.elapsed());
            let wait = match dirty || budget == 0 {
                // (While it draws there is nothing to write out, only more of it to read.)
                true if drawing && budget > 0 => left.min(look),
                true if drawing => PUBLISH_EVERY.saturating_sub(filled.elapsed()),
                // (Has it more to say? A millisecond without it tells.)
                true if budget > 0 && !quiet => QUIET.saturating_sub(heard.elapsed()).min(every.saturating_sub(since)),
                true => every.saturating_sub(since),
                false => look,
            };
            // (To the millisecond above, and one at least: a wait that ends early only comes round again.)
            let wait = (wait.as_micros().div_ceil(1000) as i32).max(1);
            let [ended_now, commanded, program, typed] =
                sys::wait(wanted, wait).map_err(|e| format!("cannot wait: {e}"))?;
            if commanded != 0 {
                self.commands(ctl, &mut buffer);
                dirty = true;
            }
            if program & !sys::POLLOUT != 0 && budget > 0 {
                let n = self.read_program(buffer.get_mut(..budget.min(64 << 10)).unwrap_or(&mut []));
                budget -= n.min(budget);
                dirty |= n > 0;
                heard = if n > 0 { Instant::now() } else { heard };
            }
            if typed != 0 && self.master.is_some() {
                let room = INPUT_MAX.saturating_sub(self.input.len()).min(buffer.len());
                if let Some(Ok(n)) = buffer.get_mut(..room).map(|b| (&*input).read(b)) {
                    self.input.extend_from_slice(buffer.get(..n).unwrap_or(&[]));
                }
            }
            let typed = self.master.as_ref().filter(|_| !self.input.is_empty()).map(|mut m| m.write(&self.input));
            if let Some(Ok(n)) = typed {
                self.input.drain(..n.min(self.input.len()));
            }
            if ended_now != 0 {
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
                // The last screen is tried for two seconds, so that who waits for the pane's end
                // finds all of it; what cannot be written by then is said in the log.
                let tried = Instant::now();
                while let Err(e) = self.publish() {
                    let gone = e.kind() == ErrorKind::NotFound && fs::symlink_metadata("in").is_err();
                    if tried.elapsed() > Duration::from_secs(2) || gone {
                        self.log(&format!("cannot write the last screen (the program {}): {e}", self.state));
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(20));
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
                let text = stamp.map_or_else(String::new, |_| crate::text(&file, 16 << 10));
                *known = stamp;
                match name {
                    "colours" => self.term.told().colours = text,
                    _ => self.term.told().keyboard = text.trim().parse().unwrap_or(0),
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
        let files = [
            ("history", &mut self.pending.history, out.unhistory),
            ("history.styled", &mut self.pending.styled, out.unstyled),
        ];
        let lost = self.styled_lost;
        let taken = files.map(|(name, waiting, back)| {
            let keep = waiting.len().saturating_sub(back);
            let written = (back - (waiting.len() - keep)) as u64;
            if waiting.is_char_boundary(keep) {
                waiting.truncate(keep);
            }
            let cut = |file: File| file.set_len(file.metadata()?.len().saturating_sub(written));
            let cut = OpenOptions::new().write(true).open(name).and_then(cut);
            (written > 0 && !(lost && name == "history.styled")).then_some(cut.map_err(|e| (name, e)))
        });
        if taken.iter().any(Option::is_some) {
            self.history_open = Holder::open_end();
        }
        for (name, e) in taken.into_iter().flatten().filter_map(Result::err) {
            self.log(&format!("{name} keeps bytes that are on the screen again: {e}"));
            self.split();
        }
        self.pending.styled.push_str(&out.styled);
        self.pending.history.push_str(&out.history);
        self.pending.pictures.append(&mut out.pictures);
        self.pending.title = out.title.or(self.pending.title.take());
        self.pending.clipboard = out.clipboard.or(self.pending.clipboard.take());
        self.pending.cwd = out.cwd.or(self.pending.cwd.take());
        self.pending.notice = out.notice.or(self.pending.notice.take());
        self.pending.notices += out.notices;
        // (Of what waits to be written, the longer text is the one that counts.)
        if self.pending.history.len().max(self.pending.styled.len()) > READ_BUDGET {
            self.history();
        }
    }

    /// Whether `history` ends in the middle of a line: what its last byte says.
    fn open_end() -> bool {
        let mut last = *b"\n";
        let end = File::open("history").and_then(|mut file| {
            file.seek(io::SeekFrom::End(-1))?;
            file.read_exact(&mut last)
        });
        end.is_ok() && last != *b"\n"
    }

    /// Something could not be done to one of the two history files, so they no longer have the
    /// same lines. `history` keeps what it has; `history.styled` is begun anew, empty and with
    /// no older file, so that it never has the colours of other lines: from here on it has
    /// `history`'s lines again. (While it cannot be emptied it is not written, and this is tried
    /// again before the next lines.) The rows a resize sent to history stay there: how many of
    /// their bytes are in the files is no longer known.
    fn split(&mut self) {
        let gone = fs::remove_file("history.styled.1").or_else(|e| match e.kind() {
            ErrorKind::NotFound => Ok(()),
            _ => Err(e),
        });
        let anew = gone.and_then(|()| File::create("history.styled"));
        if let (Err(e), false) = (&anew, self.styled_lost) {
            self.log(&format!("history.styled cannot be begun anew, and is not written until it can: {e}"));
        }
        self.styled_lost = anew.is_err();
        self.term.keep_lent();
        self.history_open = Holder::open_end();
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
                let old = self.term.told().cell;
                let old = (old.0 as u16, old.1 as u16);
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
                    self.term.keep_pictures(|_| false);
                    self.term.told().cell = (usize::from(cell.0), usize::from(cell.1));
                }
                self.term.resize(usize::from(cols), usize::from(rows));
                self.collect();
                Ok(())
            }
            _ => Err("not a command (kill, resize COLS ROWS, notify TEXT)".into()),
        }
    }

    /// Appends the lines that scrolled off to `history`, and the same lines with their looks to
    /// `history.styled`. Lines that cannot be written are lost, and the log says so: they are
    /// not kept to try again, and `history.styled` is begun anew (see `split`).
    fn history(&mut self) {
        let (lines, styled) = (std::mem::take(&mut self.pending.history), std::mem::take(&mut self.pending.styled));
        if self.styled_lost {
            self.split();
        }
        let files = [("history", &lines), ("history.styled", &styled)];
        let files = files.get(..if self.styled_lost { 1 } else { 2 }).unwrap_or_default();
        // A file past its size is put aside where a line ends, so that no line is in two files:
        // before these lines if the file ends with a whole one, and else after the first line
        // end among them (a line that never ends is cut when the file is twice its size). Both
        // files are put aside when either is past its size, at the same line; and neither while
        // their last rows are a resize's to take back, which it takes from these files.
        let size = files.iter().filter_map(|(name, _)| fs::metadata(name).ok()).map(|m| m.len()).max().unwrap_or(0);
        let whole = match (size > HISTORY_MAX && !self.term.lent(), self.history_open) {
            (false, _) => None,
            (true, false) => Some(true),
            (true, true) => lines.contains('\n').then_some(false).or((size > 2 * HISTORY_MAX).then_some(true)),
        };
        self.history_open = !lines.ends_with('\n');
        let mut split = false;
        for (name, text) in files {
            let cut = whole.map(|whole| if whole { 0 } else { text.find('\n').map_or(text.len(), |end| end + 1) });
            let (old, new) = text.split_at_checked(cut.unwrap_or(text.len())).unwrap_or((text, ""));
            let append =
                |text: &str| OpenOptions::new().append(true).create(true).open(name)?.write_all(text.as_bytes());
            let aside = |()| fs::rename(name, format!("{name}.1")).and_then(|()| append(new));
            if let Err(e) = append(old).and_then(|()| if cut.is_some() { aside(()) } else { Ok(()) }) {
                self.log(&format!("{name} was lost (some or all of {} bytes): {e}", text.len()));
                split = true;
            }
        }
        if split {
            self.split();
        }
    }

    /// Writes what changed: history first, then pictures, the title and the
    /// clipboard, then both screens; so nothing a screen names is missing and
    /// no line is in neither file.
    fn publish(&mut self) -> io::Result<()> {
        if !self.pending.history.is_empty() || !self.pending.styled.is_empty() {
            self.history();
        }
        for (id, data) in std::mem::take(&mut self.pending.pictures) {
            // One that left the screen before it was written has no reader. One that
            // cannot be written is not on the screen either: it goes, and the log says why.
            let name = format!("pic-{id}.sixel");
            if !self.term.has_picture(id) {
                continue;
            }
            self.pictures.insert(id);
            if let Err(e) = fs::write(&name, data) {
                let _ = fs::remove_file(&name);
                self.term.keep_pictures(|kept| kept != id);
                self.log(&format!("a picture was dropped, as its file could not be written: {e}"));
            }
        }
        // Every fact has a file of its own, written when it changes, before the screens. A reader
        // may find a fact one publish ahead of the text it read: it keeps the cursor inside the
        // rows it has.
        // (A picture's rows that went off the top are counted still: its first row is then above row 0.)
        let placed = |p: &crate::term::Picture| {
            let row = if p.off > 0 { format!("-{}", p.off) } else { p.row.to_string() };
            format!("pic-{}.sixel {row} {} {} {}\n", p.id, p.col, p.cols, p.rows + p.off)
        };
        let words = self.term.words();
        let (term, screen) = (&self.term, self.term.screen());
        let shown = if screen.cursor_shown { "shown" } else { "hidden" };
        // (The cursor's colour, if the program set one: a fifth word.)
        let colour = term.cursor_colour().map_or(String::new(), |[r, g, b]| format!(" #{r:02x}{g:02x}{b:02x}"));
        // While a program has the alternate screen, the main screen it covers has two files, as
        // the screens are written; they are there before `modes` says `alt`, and gone before it
        // no longer does. Which of its rows go on is a third line of `lines` meanwhile.
        let (main, looks, under) = match term.covered() {
            Some((plain, styled, wraps)) => (Some(plain.into_bytes()), Some(styled.into_bytes()), wraps + "\n"),
            None => (None, None, String::new()),
        };
        let covered = [("screen.main", main), ("screen.main.styled", looks)];
        let texts = [
            // How the screen's rows are lines of the pane's text: the number of the first row's
            // line, then the rows that go on from the row above.
            ("lines", format!("{}\n{}\n{under}", term.top(), term.wraps())),
            ("marks", term.marks()),
            ("links", term.links()),
            ("pictures", screen.pictures.iter().map(placed).collect()),
            ("size", format!("{} {}\n", screen.cols, screen.rows)),
            ("cursor", format!("{} {} {shown} {}{colour}\n", screen.row, screen.col, screen.cursor_shape)),
            ("modes", format!("{}\n", words)),
            ("state", format!("{}\n", self.state)),
            ("status", self.status.clone()),
        ];
        let facts = covered.into_iter().chain(texts.into_iter().map(|(name, text)| (name, Some(text.into_bytes()))));
        // What the program said: the title is one line; the clipboard is the bytes it set, and
        // set again is written again (what is on the terminal's clipboard may be another's by now).
        let mut said = Vec::new();
        said.extend(self.pending.title.take().map(|title| ("title", Some(format!("{title}\n").into_bytes()))));
        if let Some(bytes) = self.pending.clipboard.take() {
            self.facts.remove("clipboard");
            said.push(("clipboard", Some(bytes)));
        }
        // The directory the program says it is in: kept if it is one of this machine (no host
        // named, or this one's name or address), and forgotten if it is another machine's (a
        // shell at the far end of ssh): a path of there means nothing here.
        if let Some((host, path)) = self.pending.cwd.take() {
            // (The file is the program's to write too: what it says is written, whatever was written last.)
            self.facts.remove("cwd");
            let same = |name: &str| name.eq_ignore_ascii_case(&host);
            let here = host.is_empty()
                || ["localhost", "127.0.0.1", "[::1]"].into_iter().any(same)
                || sys::host().is_some_and(|name| same(&name));
            match (here, path.starts_with('/')) {
                (true, true) => said.push(("cwd", Some(format!("{path}\n").into_bytes()))),
                (true, false) => {}
                (false, _) => said.push(("cwd", None)),
            }
        }
        // What the program wanted noticed last: a number that grows with each one (so that the
        // same words twice are two), then the words; the number alone is the bell.
        if let Some(text) = self.pending.notice.take() {
            self.noticed = self.noticed.wrapping_add(std::mem::take(&mut self.pending.notices).max(1));
            let line = format!("{} {text}", self.noticed).trim_end().to_string() + "\n";
            said.push(("notice", Some(line.into_bytes())));
        }
        for (name, bytes) in said {
            self.said.retain(|(fact, _)| *fact != name);
            self.said.push((name, bytes));
        }
        // (One that cannot be written fails the round, which is tried again: the newest of each.)
        for (name, bytes) in facts.chain(self.said.iter().cloned()) {
            if self.facts.get(name) != Some(&bytes) {
                match &bytes {
                    Some(bytes) => replace(name, bytes)?,
                    None => fs::remove_file(name)
                        .or_else(|e| if e.kind() == ErrorKind::NotFound { Ok(()) } else { Err(e) })?,
                }
                self.facts.insert(name, bytes);
            }
        }
        // The screens are text and nothing else, written last of all, `screen.styled` at the very
        // end: who watches a pane watches that one file, and finds every fact already there.
        let term = &self.term;
        replace("screen", term.plain().as_bytes())?;
        replace("screen.styled", term.styled().as_bytes())?;
        // A picture neither screen has any more (the one shown, the one behind) has no reader left.
        self.pictures.retain(|id| {
            let kept = self.term.has_picture(*id);
            if !kept {
                let _ = fs::remove_file(format!("pic-{id}.sixel"));
            }
            kept
        });
        Ok(())
    }
}
