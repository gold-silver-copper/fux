//! The holder: the one small process that keeps a pane. It owns the pty and
//! the emulator, publishes what the terminal shows as files in the pane's
//! directory (its working directory), and takes bytes and commands from two
//! FIFOs. It handles no signals: killed, it is gone, and the kernel hangs up
//! its program.

use crate::sys::{self, Winsize};
use crate::term::{Out, SIZE_MAX, TERM, Term};
use crate::{plain, replace};
use std::collections::{BTreeSet, HashMap};
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind, PipeReader, Read, Write};
use std::os::fd::AsFd;
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

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

struct Holder {
    term: Term,
    master: Option<File>,
    /// The facts as they were written last, each in its file: one is written again only when it changes.
    facts: HashMap<&'static str, Option<Vec<u8>>>,
    /// How many rounds of writing had a notice: the number in the `notice` file.
    noticed: u32,
    input: Vec<u8>,
    ctl_line: Vec<u8>,
    /// The numbers of the picture files written and not yet removed.
    pictures: BTreeSet<u32>,
    /// The session's folder, where the files are that say what the terminal outside is like, and
    /// which `colours` and `keyboard` were read last.
    session: PathBuf,
    told: (crate::Stamp, crate::Stamp),
    logged: usize,
}

/// Keeps the pane whose directory is the working directory until its program
/// ends. `fux start` locked the directory and handed the lock over as this
/// process's standard input: it is held for exactly as long as the holder lives.
pub fn hold(args: &[OsString]) -> Result<(), String> {
    let [cwd, program, program_args @ ..] = args else { return Err("hold is started by `fux start`".into()) };
    // The first size is COLUMNS x LINES when the starter set them (the viewer
    // does, for key scripts), and 80x24 otherwise. `resize` changes it later.
    let number = |name: &str, or: u16| {
        let Some(value) = std::env::var_os(name) else { return Ok(or) };
        let size = value.to_str().and_then(|n| n.parse().ok()).filter(|n| (1..=SIZE_MAX).contains(&usize::from(*n)));
        size.ok_or(format!("{name} is not a number from 1 to {SIZE_MAX}"))
    };
    let (cols, rows) = (number("COLUMNS", 80)?, number("LINES", 24)?);
    let size = Winsize { cols, rows, xpixel: cols * 10, ypixel: rows * 20 };
    // The slave stays open until this function ends, so that output a program wrote just before it
    // ended is not thrown away (macOS drops it when the last slave closes).
    let (master, slave) = sys::pty(size).map_err(|e| format!("cannot open a pty: {e}"))?;
    // `fux start` says under what name the pane's directory was started, and its program is told
    // the same. The session is found as by any command in a pane: the one named, or two folders up.
    let session = crate::here().ok_or("hold is started by `fux start`".into()).and_then(|_| crate::session())?;
    // (The `cwd` an earlier holder left goes before the program starts: the file is the program's to write too.)
    let _ = fs::remove_file("cwd");
    let mut child = {
        let stdio = || slave.try_clone().map(Stdio::from).map_err(|e| format!("cannot copy the pty: {e}"));
        let mut child = Command::new(program);
        // The program is told what terminal it is in and which pane, and no size but the terminal's
        // own. What a key script is told of its viewer is not this pane's.
        for name in [name!("WS"), name!("VIEW"), name!("ROW"), name!("COL")] {
            child.env_remove(name);
        }
        child.args(program_args).current_dir(cwd).env("TERM", TERM).env_remove("COLUMNS").env_remove("LINES");
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
    let command = plain(&args.iter().skip(1).map(|a| a.to_string_lossy()).collect::<Vec<_>>().join(" "));
    let command: String = command.chars().take(4096).collect();
    let mut holder = Holder {
        term: Term::new(usize::from(cols), usize::from(rows)),
        master: Some(master),
        facts: HashMap::new(),
        noticed: 0,
        input: Vec::new(),
        ctl_line: Vec::new(),
        pictures: BTreeSet::new(),
        session,
        told: (None, None),
        logged: 0,
    };
    holder.term.told().name = format!("{} {}", crate::TERM_PROGRAM, env!("CARGO_PKG_VERSION"));
    // The FIFOs are there for as long as this function runs, and not after (what an earlier
    // holder left of those names goes).
    let fifos = sys::fifo("in").and_then(|input| Ok((input, sys::fifo("ctl")?)));
    // Who holds the pane, the program's own number, and what it runs: written once, as it
    // never changes, before the first screen.
    let status = format!("holder {}\npid {pid}\ncommand {command}\n", std::process::id());
    let result = fifos.and_then(|fifos| replace("status", status.as_bytes()).map(|()| fifos));
    let result = result.map_err(|e| format!("cannot make the FIFOs or the status: {e}"));
    let result = result.and_then(|(input, ctl)| holder.run(&input, &ctl, &mut ended));
    let _ = fs::remove_file("in");
    let _ = fs::remove_file("ctl");
    result
}

impl Holder {
    fn run(&mut self, input: &File, ctl: &File, ended: &mut PipeReader) -> Result<(), String> {
        // What an earlier holder of this pane left goes before this one's first screen is there: its clipboard,
        // its history, its title (a fact with none) and its program's end, which is not that of the one `status`
        // names now. Not its pictures' files: each stays until a picture of its number is written.
        for old in ["clipboard", "notice", "state"] {
            let _ = fs::remove_file(old);
        }
        for history in ["history", "history.styled"] {
            let _ = fs::remove_file(format!("{history}.1"));
            File::create(history).map_err(|e| format!("cannot make {history}: {e}"))?;
        }
        self.publish(None).map_err(|e| format!("cannot write the screen: {e}"))?;
        // (The pane is up: `fux start` waits for this line.)
        let _ = crate::say(b"");

        let mut buffer = vec![0u8; 64 << 10];
        // Since when there has been something a round of writing has not written.
        let (mut dirty, mut published, mut looked, mut budget) = (None, Instant::now(), Instant::now(), READ_BUDGET);
        // When the program last wrote anything, and when it was last given its read budget.
        let (mut heard, mut filled) = (Instant::now(), Instant::now());
        loop {
            // How long it still is until `long` after a moment: no time, once that has come.
            let now = Instant::now();
            let left = |since: Instant, long: Duration| long.saturating_sub(now.duration_since(since));
            // When the next round of writing is due. A program in the middle of drawing (mode 2026)
            // is not shown until it says it is done, or the oldest of what is not written is half a
            // second old: nothing it wrote waits longer, so one that never says so, or says it again
            // at once, cannot freeze its pane. Any other is shown this long after the last round, or
            // sooner once it has gone quiet: nothing more from it for a millisecond (whatever else
            // the holder was woken for meanwhile), and not because it is not being read.
            let drawing = self.term.screen().sync && self.master.is_some();
            let round = dirty.map(|first: Instant| match budget {
                _ if drawing => left(first, DRAWING_MAX),
                0 => left(published, PUBLISH_EVERY),
                _ => left(published, PUBLISH_EVERY).min(left(published, PUBLISH_QUIET).max(left(heard, QUIET))),
            });
            // The budget is for what is read between two screens; while a program in the middle
            // of drawing is not shown, for what is read in the time of one.
            let refill = (budget == 0).then(|| left(filled, PUBLISH_EVERY));
            let look = left(looked, LOOK_EVERY);
            // What is due is done, one thing at a time; the wait is for the first of them, and none
            // after one was done (what is next is worked out anew). To the millisecond above.
            let wait = round.into_iter().chain(refill).fold(look, Duration::min).as_nanos().div_ceil(1_000_000) as i32;
            if round == Some(Duration::ZERO) {
                // What could not be written waits, or is still changed: the next round writes it,
                // which the program's next output, a command or its end brings; no timer does.
                if let Err(e) = self.publish(None) {
                    self.log(&format!("cannot write the pane's files: {e}"));
                }
                (dirty, published) = (None, Instant::now());
                (budget, filled) = (READ_BUDGET, published);
            } else if refill == Some(Duration::ZERO) {
                (budget, filled) = (READ_BUDGET, now);
            } else if look.is_zero() {
                // The directory is the pane: when it is gone, so is this.
                looked = now;
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
            let [ended_now, commanded, program, typed] =
                sys::wait(wanted, wait).map_err(|e| format!("cannot wait: {e}"))?;
            if commanded != 0 {
                let n = (&*ctl).read(&mut buffer).unwrap_or(0);
                let lines = crate::lines(&mut self.ctl_line, buffer.get(..n).unwrap_or(&[]), CTL_LINE_MAX);
                dirty = dirty.or(lines.first().map(|_| Instant::now()));
                for line in lines {
                    match line {
                        None => self.log(&format!("ctl: a line over {CTL_LINE_MAX} bytes was dropped")),
                        Some(line) => self.command(&line).unwrap_or_else(|e| self.log(&format!("ctl: {line}: {e}"))),
                    }
                }
            }
            if program & !sys::POLLOUT != 0 && budget > 0 && self.read_program(&mut buffer, &mut budget) {
                heard = Instant::now();
                dirty.get_or_insert(heard);
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
                self.drain(4 << 20);
                self.master = None;
                // (What cannot be written of the last screen is said in the log.)
                if let Err(e) = self.publish(Some(&state)) {
                    self.log(&format!("cannot write the last screen (the program {state}): {e}"));
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
            // (A file, not something that only has a file's name.)
            let file = self.session.join(name);
            let stamp = fs::metadata(&file).ok().filter(|file| file.is_file()).map(crate::stamp);
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

    /// Feeds the emulator one read of what the program wrote, of no more bytes than are `left`, and takes
    /// them from that; says whether there were any.
    fn read_program(&mut self, buffer: &mut [u8], left: &mut usize) -> bool {
        let buffer = buffer.get_mut(..(*left).min(buffer.len())).unwrap_or_default();
        let Some(n @ 1..) = self.master.as_ref().and_then(|mut m| m.read(buffer).ok()) else { return false };
        // What the colours are is in the session's `colours` file, two folders up, if a viewer
        // (or a person) wrote one: read again when it has changed, 16 KiB of it at most.
        self.told();
        self.term.feed(buffer.get(..n).unwrap_or(&[]));
        self.collect(READ_BUDGET);
        *left = left.saturating_sub(n);
        true
    }

    /// Reads what the program has written and was not read yet, `most` bytes of it at most.
    fn drain(&mut self, mut most: usize) {
        let mut buffer = vec![0u8; 64 << 10];
        while most > 0 && self.read_program(&mut buffer, &mut most) {}
    }

    /// Takes what the emulator produced: answers go to the program and refusals to the log now,
    /// and history to its files once `over` bytes of it wait. What the program said once (pictures,
    /// a clipboard, a directory, a notice) comes with `over` 0 alone, and is the caller's to write.
    fn collect(&mut self, over: usize) -> Out {
        let mut out = self.term.take(over);
        if self.input.len() + out.reply.len() <= INPUT_MAX {
            self.input.extend_from_slice(&out.reply);
        } else {
            out.refused
                .push("an answer to the program was dropped: as many typed bytes as are kept were waiting for it");
        }
        out.refused.drain(..).for_each(|refused| self.log(refused));
        if !out.history.is_empty() || !out.styled.is_empty() || out.unhistory.max(out.unstyled) > 0 {
            self.history([("history", &out.history, out.unhistory), ("history.styled", &out.styled, out.unstyled)]);
        }
        out
    }

    fn command(&mut self, line: &str) -> Result<(), String> {
        let mut words = line.split_whitespace();
        match words.next() {
            // Anyone may have something noticed: the words after `notify`, as a program's own notice.
            Some("notify") => self.term.notice(line.trim_start().strip_prefix("notify").unwrap_or_default().trim()),
            Some("kill") if words.next().is_none() => {
                // Closing the master is the hangup. What the program and its
                // children do about it is theirs to decide.
                self.master = None;
                return Ok(());
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
                // What the program has written so far, it wrote for the old size:
                // it is drawn at that size first (up to 1 MiB of it).
                self.drain(1 << 20);
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
            }
            _ => return Err("not a command (kill, resize COLS ROWS, notify TEXT)".into()),
        }
        self.collect(READ_BUDGET);
        Ok(())
    }

    /// Appends the lines that scrolled off to `history`, and the same lines with their looks to
    /// `history.styled`. What cannot be written to a file is lost to that file, and the log says
    /// so: it is not kept to try again, and what a resize sent there is no longer taken back.
    /// Before them, the bytes a resize took back from each file's end go from it (the third of each).
    fn history(&mut self, files: [(&str, &str, usize); 2]) {
        for (name, _, back) in &files {
            let cut = |file: File| file.set_len(file.metadata()?.len().saturating_sub(*back as u64));
            let cut = if *back > 0 { OpenOptions::new().write(true).open(name).and_then(cut) } else { Ok(()) };
            let kept = cut.err().map(|e| format!("{name} keeps bytes that are on the screen again: {e}"));
            kept.into_iter().for_each(|kept| self.log(&kept));
        }
        // Once either file is past its size both are put aside, where the last of these lines ends: no line is in
        // two files, and the two have the same lines. (A line that never ends is cut when a file is twice its size.)
        // Neither while their last rows are a resize's to take back, which it takes from these files. One that cannot
        // be put aside begins anew, its lines lost: still past its size, it would have the other put aside every round.
        let size = files.iter().filter_map(|(name, ..)| fs::metadata(name).ok()).map(|m| m.len()).max().unwrap_or(0);
        let aside = size > HISTORY_MAX && !self.term.lent();
        for (name, text, _) in &files {
            let end = text.rfind('\n').map(|end| end + 1).or((size > 2 * HISTORY_MAX).then_some(0));
            let (old, new) = text.split_at_checked(end.filter(|_| aside).unwrap_or(text.len())).unwrap_or((text, ""));
            let append =
                |text: &str| OpenOptions::new().append(true).create(true).open(name)?.write_all(text.as_bytes());
            let turn =
                || fs::rename(name, format!("{name}.1")).or_else(|e| File::create(name).and(Err(e))).and(append(new));
            if let Err(e) = append(old).and_then(|()| if aside && end.is_some() { turn() } else { Ok(()) }) {
                self.log(&format!("{name} was lost (some or all of {} bytes): {e}", text.len()));
                self.term.keep_lent();
            }
        }
    }

    /// Writes what changed: history first, then pictures, the title and the
    /// clipboard, then both screens; so nothing a screen names is missing and
    /// no line is in neither file. `ended` is what became of the program (`exited CODE`,
    /// `killed SIGNAL`), once it has ended: the `state` file, which until then says `running`,
    /// or `hungup` once the pty is closed.
    fn publish(&mut self, ended: Option<&str>) -> io::Result<()> {
        let written = self.write(ended);
        // A picture neither screen has any more has no reader left, after a round that failed too.
        self.pictures.retain(|id| {
            let kept = self.term.has_picture(*id);
            if !kept {
                let _ = fs::remove_file(format!("pic-{id}.sixel"));
            }
            kept
        });
        written
    }

    fn write(&mut self, ended: Option<&str>) -> io::Result<()> {
        let out = self.collect(0);
        for (id, data) in self.term.new_pictures() {
            // (One that cannot be written is not on the screen: it goes, its file below, and the log says why.)
            self.pictures.insert(id);
            if let Err(e) = fs::write(format!("pic-{id}.sixel"), data) {
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
        let (term, screen, at) = (&self.term, self.term.screen(), self.term.screen().cursor);
        let shown = if screen.cursor_shown { "shown" } else { "hidden" };
        // (The cursor's colour, if the program set one: a fifth word.)
        let colour = term.cursor_colour().map_or(String::new(), |[r, g, b]| format!(" #{r:02x}{g:02x}{b:02x}"));
        // While a program has the alternate screen, the main screen it covers has two files, as
        // the screens are written: that they are there is what says so. Which of its rows go on
        // is a third line of `lines` meanwhile. The title is one line: with none, or an empty one, no file.
        let (main, looks, under) = match term.covered() {
            Some((plain, styled, wraps)) => (Some(plain.into_bytes()), Some(styled.into_bytes()), wraps + "\n"),
            None => (None, None, String::new()),
        };
        let title = Some(term.title()).filter(|title| !title.is_empty()).map(|title| format!("{title}\n").into_bytes());
        let covered = [("screen.main", main), ("screen.main.styled", looks), ("title", title)];
        let state = ended.unwrap_or(if self.master.is_some() { "running" } else { "hungup" });
        let texts = [
            // How the screen's rows are lines of the pane's text: the number of the first row's
            // line, then the rows that go on from the row above.
            ("lines", format!("{}\n{}\n{under}", term.top(), term.wraps())),
            ("marks", term.marks()),
            ("links", term.links()),
            ("pictures", screen.pictures.iter().filter(|p| p.row < screen.rows).map(placed).collect()),
            ("size", format!("{} {}\n", screen.cols, screen.rows)),
            ("cursor", format!("{} {} {shown} {}{colour}\n", at.row, at.col, screen.cursor_shape)),
            ("modes", format!("{}\n", words)),
            ("state", format!("{state}\n")),
        ];
        let facts = covered.into_iter().chain(texts.into_iter().map(|(name, text)| (name, Some(text.into_bytes()))));
        let write = |name: &str, bytes: Option<&[u8]>| match bytes {
            Some(bytes) => replace(name, bytes),
            None => fs::remove_file(name).or_else(|e| if e.kind() == ErrorKind::NotFound { Ok(()) } else { Err(e) }),
        };
        // (One that cannot be written fails the round: it is written in a later one, the newest of each.)
        for (name, bytes) in facts {
            if self.facts.get(name) != Some(&bytes) {
                write(name, bytes.as_deref())?;
                self.facts.insert(name, bytes);
            }
        }
        // What the program said once is this round's to write, as history is: what a round that
        // fails did not write of it is lost, and the log has the failure. A clipboard set again is
        // written again (what is on the terminal's clipboard may be another's by now).
        if let Some(bytes) = &out.clipboard {
            replace("clipboard", bytes)?;
        }
        // The directory the program says it is in: kept if it is one of this machine (no host
        // named, or this one's name or address), and forgotten if it is another machine's (a
        // shell at the far end of ssh): a path of there means nothing here.
        // (The file is the program's to write too: what it says is written, whatever was written last.)
        if let Some((host, path)) = &out.cwd {
            let same = |name: &str| name.eq_ignore_ascii_case(host);
            let here = host.is_empty()
                || ["localhost", "127.0.0.1", "[::1]"].into_iter().any(same)
                || sys::host().is_some_and(|name| same(&name));
            match (here, path.starts_with('/')) {
                (true, true) => replace("cwd", format!("{path}\n").as_bytes())?,
                (true, false) => {}
                (false, _) => write("cwd", None)?,
            }
        }
        // What the program wanted noticed last: a number that grows with each round that writes
        // one (so that the same words twice are two), then the words; the number alone is the bell.
        if let Some(text) = &out.notice {
            self.noticed = self.noticed.wrapping_add(1);
            replace("notice", (format!("{} {text}", self.noticed).trim_end().to_string() + "\n").as_bytes())?;
        }
        // The screens are text and nothing else, written last of all, `screen.styled` at the very
        // end: who watches a pane watches that one file, and finds every fact already there.
        replace("screen", self.term.plain().as_bytes())?;
        replace("screen.styled", self.term.styled().as_bytes())
    }
}
