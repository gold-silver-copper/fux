//! The viewer: one more client. It draws a workspace's panes from their
//! `screen.styled` files as the `layout` file places them, sends what is
//! typed to the focused pane's `in`, and runs a script for a key that has
//! one. It keeps nothing the files do not say, except which workspace it
//! shows.

use crate::layout::{self, Layout, Rect};
use crate::sys::{self, Winsize};
use crate::term::{
    BOLD, Cell, Color, DIM, KEYS, LINKS_LIST_MAX, MORE, PICTURE_MAX, SIZE_MAX, Style, Term, base64_encode,
};
use crate::width::Joiner;
use crate::{is_name, keys};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, AsRawFd};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// The most key scripts running at once; one more is refused with a message.
const SCRIPTS_MAX: usize = 32;
const LINE_MAX: usize = 4096;
/// The most keys a menu lists.
const MENU_MAX: usize = 200;
/// More than the largest screen a holder writes: 500 by 500 cells, each with its own colours.
const STYLED_MAX: u64 = 32 << 20;
const TYPED_MAX: usize = 64 << 10;
const SCRIPT_WAIT: std::time::Duration = std::time::Duration::from_secs(2);
/// How long the start of an escape sequence waits for the rest of it: after that an ESC by
/// itself is the Escape key.
const ESCAPE_WAIT: Duration = Duration::from_millis(4);
/// For how long after a key went to a pane its echo is looked for every millisecond.
const ECHO_WAIT: Duration = Duration::from_millis(40);

/// A key of a menu: its name, what it does, and the heading it is listed under (or none).
type Entry = (String, String, String);

/// What tells one writing of a file from the next: its inode and its time.
type Stamp = Option<(u64, i64, i64)>;

type Drawn = (PathBuf, usize, usize, usize, usize, u32);

/// What a frame is drawn from, other than the terminal's size: the workspace's folder, its
/// layout and what is wrong with it, the message, what the terminal is like and whether its
/// window has the keys, each tile's screen file (as `Pane::seen`) and whether its holder lives,
/// and the focused pane's `clipboard` file.
type From = (PathBuf, String, Option<String>, String, (bool, bool, u8, bool, (u16, u16)), Vec<(Seen, bool)>, Stamp);
/// A screen file's inode, time and length.
type Seen = Option<(u64, i64, i64, u64)>;

/// What the viewer knows of a pane: its last screen, read from the file.
struct Pane {
    /// The screen's file when it was last read.
    seen: Seen,
    term: Term,
    /// The size the pane says it has (`size`), if it says one.
    size: (usize, usize),
    pid: u32,
    state: String,
    /// The addresses of the links on the screen: a cell's `link` less one is its place here.
    addresses: Vec<String>,
    /// Pictures on the screen: file, row, column, columns, rows.
    pictures: Vec<(String, usize, usize, usize, usize)>,
    /// The size last asked for (and the cell's, in pixels), and of which holder: asked once, not fought over.
    asked: (u32, usize, usize, (u16, u16)),
    /// The title its program set (the `title` file), without control characters.
    title: String,
}

/// Where the press of a key went: to the viewer itself (and the script it ran, if it ran one),
/// or to a pane (its directory and its program).
#[derive(Clone)]
enum Went {
    Viewer(Option<PathBuf>),
    Pane(PathBuf, u32),
}

struct View {
    session: PathBuf,
    workspace: String,
    /// The workspace shown before this one: this viewer's own memory, in no file.
    before: Option<String>,
    keys: PathBuf,
    tty: File,
    /// What goes to the terminal is written by a thread of its own, so that a terminal that
    /// reads slowly holds up the frames and not the keys: the way to it (until the viewer
    /// leaves), and how much of what was sent there it has not written yet.
    out: Option<mpsc::Sender<String>>,
    unwritten: Arc<AtomicUsize>,
    size: Winsize,
    /// What the terminal shows now, to send only what changes.
    front: Vec<Cell>,
    /// What the last frame was drawn from, if no menu was open.
    from: Option<From>,
    panes: HashMap<String, Pane>,
    focus: Option<String>,
    /// Where each pane's screen was drawn last, for the mouse.
    tiles: Vec<(String, Rect)>,
    /// A mouse button is down: for the focused program (true), or in a click that only moved the focus (false).
    held: Option<bool>,
    /// Keys wait for the next look at the layout.
    pause: bool,
    /// The open menu: its directory and its entries.
    menu: Option<(PathBuf, Vec<Entry>)>,
    message: String,
    /// Whether the terminal said it draws sixel pictures.
    sixel: bool,
    /// The kitty keyboard flags the terminal takes, of those fux passes on.
    keyboard: u8,
    /// The addresses of the links drawn: a cell's `link` less one is its place here. And each
    /// address's number, and how many of them the last frame drew.
    links: Vec<String>,
    numbers: HashMap<String, u16>,
    live: usize,
    /// The keys that are down, as far as the terminal tells, and where the press of each went.
    down: Vec<((u32, u8), Went)>,
    /// The terminal joins characters into clusters as fux does (its mode 2027), and whether
    /// this viewer was the one to turn that on.
    joins: bool,
    joined: bool,
    /// The pane that was told it has the keys (it asked to be told: mode 1004).
    told_focus: Option<PathBuf>,
    /// The pane this viewer itself runs in, if it runs in one.
    own: std::ffi::OsString,
    /// Each pane's `notice` as it was last looked at: the file's stamp, and whether it has one
    /// this viewer has not shown on a focused pane yet. And when all were last looked at.
    notices: HashMap<String, (Stamp, bool)>,
    looked: Option<Instant>,
    /// While the terminal is still being asked what it is like: since when, and the colours it
    /// has told of.
    colours: Option<(Instant, String)>,
    /// What this viewer wrote into the session's `colours` and `keyboard` files last.
    written: Vec<(&'static str, String)>,
    /// The pictures on the terminal now: file, column, row, columns, rows, and whose holder's.
    drawn: Vec<Drawn>,
    cursor: String,
    /// What the terminal was last asked to report (the mouse, its window's focus).
    modes: String,
    /// Whether the terminal's window has the keys, as far as it told.
    window: bool,
    /// The pane that had the focus at the last look, and its `clipboard` file as it was then.
    copied: Option<(PathBuf, Stamp)>,
    /// The title this viewer gave its own terminal, if any.
    title: Option<String>,
    carry: Vec<u8>,
    typed: Vec<u8>,
    /// When a key last went to a pane: its echo is looked for often for a moment after.
    keyed: Option<Instant>,
    pasting: bool,
    scripts: Vec<Child>,
    /// The script the keys wait for: its pid and when it started.
    waiting: Option<(u32, Instant)>,
    ctl_path: PathBuf,
    /// A writer of our own on the viewer's FIFO, so that a script leaving is never its end.
    _ctl_writer: File,
    ctl_line: Vec<u8>,
    quit: bool,
}

/// Puts the terminal back when the viewer leaves, however it leaves short of
/// being killed.
struct Restore(File, sys::Termios);
impl Drop for Restore {
    fn drop(&mut self) {
        let _ = self.0.write_all(b"\x1b]8;;\x1b\\\x1b[?2004l\x1b[?1004l\x1b[?9l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?1l\x1b>\x1b[0 q\x1b[m\x1b[?25h\x1b[?7h\x1b[<u\x1b[?1049l\x1b[23;2t");
        let _ = sys::restore_mode(&self.0, &self.1);
    }
}

pub fn view(session: PathBuf, workspace: String, keys: PathBuf) -> Result<(), String> {
    let tty = io::stdin().as_fd().try_clone_to_owned().map(File::from).map_err(|e| format!("standard input: {e}"))?;
    let mut size = sys::winsize(&tty).map_err(|_| "the viewer needs a terminal on its standard input")?;
    // What is made is the owner's alone; what is there already is left as it is.
    let made = fs::DirBuilder::new().mode(0o700).recursive(true).create(session.join(&workspace));
    made.map_err(|e| format!("{}: {e}", session.display()))?;
    if !keys.exists() {
        for (name, script) in keys::DEFAULT {
            let path = keys.join(name);
            let made = path.parent().map_or(Ok(()), fs::create_dir_all).and_then(|()| fs::write(&path, script));
            let made = made.and_then(|()| {
                fs::set_permissions(&path, fs::Permissions::from_mode(if name.contains("/.") { 0o644 } else { 0o755 }))
            });
            made.map_err(|e| format!("{}: {e}", path.display()))?;
        }
    }
    let ctl_path = session.join(format!(".view.{}", std::process::id()));
    // (One of that name is a dead viewer's, whose number this process has now.)
    let _ = fs::remove_file(&ctl_path);
    sys::make_fifo(&ctl_path).map_err(|e| format!("{}: {e}", ctl_path.display()))?;
    let open = |write: bool| OpenOptions::new().read(!write).write(write).custom_flags(sys::O_NONBLOCK).open(&ctl_path);
    let ctl = open(false).map_err(|e| format!("{}: {e}", ctl_path.display()))?;
    let ctl_writer = open(true).map_err(|e| format!("{}: {e}", ctl_path.display()))?;
    let saved = sys::raw_mode(&tty).map_err(|e| format!("cannot set the terminal: {e}"))?;
    let restore = Restore(tty.try_clone().map_err(|e| e.to_string())?, saved);
    size.rows = 0; // the first frame sees a new size and draws everything
    let (out, queue) = mpsc::channel::<String>();
    let unwritten = Arc::new(AtomicUsize::new(0));
    // (The writer says so on a pipe when it has written, or cannot: the loop waits on that as on
    // the keys, and looks no more often for a terminal that reads slowly.)
    let (wrote, mut done) = io::pipe().map_err(|e| format!("cannot make a pipe: {e}"))?;
    let writer = {
        let (mut tty, unwritten) = (tty.try_clone().map_err(|e| e.to_string())?, unwritten.clone());
        std::thread::Builder::new().spawn(move || {
            let written = queue.into_iter().try_for_each(|text| {
                tty.write_all(text.as_bytes())?;
                unwritten.fetch_sub(1, Ordering::Release);
                done.write_all(b"w")
            });
            let _ = done.write_all(b"w");
            written
        })
    };
    let writer = writer.map_err(|e| format!("cannot start a thread: {e}"))?;
    let mut view = View {
        session,
        workspace,
        before: None,
        keys,
        tty,
        out: Some(out),
        unwritten,
        size,
        front: Vec::new(),
        from: None,
        panes: HashMap::new(),
        focus: None,
        tiles: Vec::new(),
        held: None,
        pause: false,
        menu: None,
        message: String::new(),
        sixel: false,
        keyboard: 0,
        links: Vec::new(),
        numbers: HashMap::new(),
        live: 0,
        down: Vec::new(),
        joins: false,
        joined: false,
        told_focus: None,
        own: std::env::var_os(name!("PANE")).unwrap_or_default(),
        notices: HashMap::new(),
        looked: None,
        colours: Some((Instant::now(), String::new())),
        written: Vec::new(),
        drawn: Vec::new(),
        cursor: String::new(),
        modes: String::new(),
        window: true,
        title: None,
        copied: None,
        carry: Vec::new(),
        typed: Vec::new(),
        keyed: None,
        pasting: false,
        scripts: Vec::new(),
        waiting: None,
        ctl_path,
        _ctl_writer: ctl_writer,
        ctl_line: Vec::new(),
        quit: false,
    };
    // The alternate screen, pastes marked, and "what are you" (for sixel). On the way the
    // terminal is asked whether it takes the kitty keyboard protocol: flags are set with the old
    // ones kept (to give back at the end), read back, and put to none until a pane asks.
    // The terminal is asked once what its colours are (the two it draws with and its palette),
    // and last of all what it is: its answer to that ends the wait for the others.
    // (First a link is closed: whatever ran before may have left one open, and no cell here is part of it.)
    const START: &str = "\x1b]8;;\x1b\\\x1b[22;2t\x1b[?1049h\x1b[?2004h\x1b[?7l\x1b[>31u\x1b[?u\x1b[=0u\x1b[?2027$p\x1b]10;?\x1b\\\x1b]11;?\x1b\\";
    let asked: String = (0..=255).map(|n| format!("\x1b]4;{n};?\x1b\\")).collect();
    view.write(START.to_string() + &asked + "\x1b[c");
    let result = view.run(&ctl, &wrote, &writer);
    // The keys leave with the viewer: a key that is down in a pane is let go there.
    view.leave(None);
    // The pane that was told it has the keys is told that it has them no more.
    view.tell(None);
    if view.joined {
        view.write("\x1b[?2027l".into());
    }
    // All of it is written before the terminal is put back; what could not be is why the viewer stopped.
    view.out = None;
    let result = result.and(writer.join().unwrap_or(Ok(())));
    drop(restore);
    let _ = fs::remove_file(&view.ctl_path);
    result.map_err(|e| format!("the viewer stopped: {e}"))
}

impl View {
    /// Hands the terminal's writer something to write.
    fn write(&mut self, text: String) {
        if let Some(out) = &self.out {
            self.unwritten.fetch_add(1, Ordering::Release);
            let _ = out.send(text);
        }
    }

    fn run(&mut self, ctl: &File, wrote: &io::PipeReader, writer: &JoinHandle<io::Result<()>>) -> io::Result<()> {
        let mut buffer = vec![0u8; 16 << 10];
        // When something last happened, and when the terminal last sent anything.
        let (mut active, mut heard) = (Instant::now(), Instant::now());
        // (The writer ends only when the terminal cannot be written to.)
        while !self.quit && !writer.is_finished() {
            // A key's script runs before the keys typed after it, as a command
            // in a shell does: they wait until it ends (and what it printed is
            // read, and what it changed is drawn). One that takes longer than
            // two seconds is left running and the keys go on.
            self.scripts.retain_mut(|script| !matches!(script.try_wait(), Ok(Some(_))));
            if let Some((pid, since)) = self.waiting {
                let running = self.scripts.iter().any(|script| script.id() == pid);
                if running && since.elapsed() > SCRIPT_WAIT {
                    self.message = "a key script is still running: keys no longer wait for it".into();
                }
                if !running || since.elapsed() > SCRIPT_WAIT {
                    self.waiting = None;
                }
            }
            let n = (&*ctl).read(&mut buffer).unwrap_or(0);
            for &byte in buffer.get(..n).unwrap_or(&[]) {
                if byte != b'\n' {
                    // One byte past the bound marks the line as too long; the rest of it is not kept.
                    if self.ctl_line.len() <= LINE_MAX {
                        self.ctl_line.push(byte);
                    }
                } else if self.ctl_line.len() > LINE_MAX {
                    self.ctl_line.clear();
                    self.message = "a line over 4096 bytes from a key script was dropped".into();
                } else {
                    let line = String::from_utf8_lossy(&std::mem::take(&mut self.ctl_line)).into_owned();
                    self.command(line.trim());
                }
            }
            if self.frame()? || n > 0 {
                active = Instant::now();
            }
            // Files are looked at often just after something happened and seldom at rest.
            let timeout = match active.elapsed().as_millis() {
                _ if self.waiting.is_some() => 2,
                // (A key's echo is a millisecond or two away.)
                _ if self.keyed.is_some_and(|at| at.elapsed() < ECHO_WAIT) => 1,
                0..300 => 4,
                300..3000 => 16,
                _ => 50,
            };
            // (The start of an escape sequence is looked at again when its wait is over.)
            let rest = ESCAPE_WAIT.saturating_sub(heard.elapsed());
            let timeout = if self.carry.is_empty() || rest.is_zero() { timeout } else { rest.as_millis() as i32 + 1 };
            // Typed bytes are not read while 64 KiB of them wait for a script.
            let typing = if self.carry.len() < TYPED_MAX { sys::POLLIN } else { 0 };
            let wanted =
                [(self.tty.as_raw_fd(), typing), (ctl.as_raw_fd(), sys::POLLIN), (wrote.as_raw_fd(), sys::POLLIN)];
            let events = sys::wait(&wanted, timeout)?;
            if events.first().is_some_and(|e| *e != 0) {
                let n = (&self.tty).read(&mut buffer)?;
                if n == 0 {
                    return Ok(());
                }
                self.carry.extend_from_slice(buffer.get(..n).unwrap_or(&[]));
                (active, heard) = (Instant::now(), Instant::now());
            }
            // (What the writer said is taken: the frame that waited for it is next.)
            if events.get(2).is_some_and(|e| *e != 0) {
                let _ = (&*wrote).read(&mut buffer)?;
                active = Instant::now();
            }
            // The terminal is given a second to say what it is like.
            // The terminal is given a second to say what it is like before that is written down
            // for the session, and ten (a slow line) before a string from it is no answer any more.
            let asked = self.colours.as_ref().map_or(Duration::ZERO, |(since, _)| since.elapsed());
            if asked > Duration::from_secs(1) {
                self.told(asked > Duration::from_secs(10));
            }
            if self.waiting.is_none() && !self.carry.is_empty() {
                // When the terminal sent nothing more for a moment, a lone ESC is the Escape key.
                self.keys(heard.elapsed() >= ESCAPE_WAIT);
            }
        }
        Ok(())
    }

    /// Another workspace is shown: what was read of the panes is forgotten, and with it what the
    /// last frame was drawn from, so the next one is drawn (and the panes read) whatever it finds.
    fn forget(&mut self) {
        self.panes.clear();
        self.from = None;
    }

    /// A line a script printed: a command of the viewer's, or else a message to show.
    fn command(&mut self, line: &str) {
        match line.split_once(' ').unwrap_or((line, "")) {
            ("quit", "") => self.quit = true,
            // `workspace -` is the workspace shown before this one.
            ("workspace", name) if is_name(self.before.as_deref().filter(|_| name == "-").unwrap_or(name)) => {
                let name = self.before.clone().filter(|_| name == "-").unwrap_or(name.into());
                if !self.session.join(&name).is_dir() {
                    self.message = format!("no workspace {name}");
                } else if name != self.workspace {
                    self.before = Some(std::mem::replace(&mut self.workspace, name));
                    self.forget();
                    self.looked = None;
                }
            }
            ("workspace", name) => self.message = format!("no workspace {name}"),
            _ => self.message = line.chars().filter(|c| !c.is_control()).take(200).collect(),
        }
    }

    // ---- keys ------------------------------------------------------------

    /// Takes the keys out of what was typed. With `whole`, nothing more came
    /// for a moment, so a lone ESC is the Escape key.
    fn keys(&mut self, whole: bool) {
        let bytes = std::mem::take(&mut self.carry);
        // The keys are for the pane the layout says has them now, which a command may have
        // changed since it was last drawn.
        self.focus = Layout::load(&self.session.join(&self.workspace)).1.tab().map(|tab| tab.focus.clone());
        // (And a key still down in the pane that had them is let go there, before what comes
        // of it now is looked at.)
        let has = self.focus.as_ref().map(|name| self.session.join(&self.workspace).join(name));
        self.leave(has.as_deref());
        // (Whether its program wants a paste marked is read now, as who it is is for a key: a frame
        // may be a while behind, when the terminal reads slowly.)
        let wants_paste = has.as_deref().is_some_and(|pane| asks(pane).paste);
        let mut at = 0;
        while let Some(rest) = bytes.get(at..).filter(|rest| !rest.is_empty()) {
            if self.pasting {
                // Pasted text is never keys: it goes to the pane as it is, up to the end mark.
                const END: &[u8] = b"\x1b[201~";
                let end = rest.windows(END.len()).position(|window| window == END);
                // The start of an end mark at the very end is kept until the rest of it comes.
                let held = (1..END.len()).rev().find(|&k| END.get(..k).is_some_and(|part| rest.ends_with(part)));
                let text = end.unwrap_or(rest.len() - held.unwrap_or(0));
                self.typed.extend_from_slice(rest.get(..text).unwrap_or(&[]));
                at += text;
                if end.is_none() {
                    break;
                }
                self.pasting = false;
                self.typed.extend_from_slice(if wants_paste { END } else { &[] });
                at += END.len();
                continue;
            }
            // While the terminal is being asked what it is like, its answers may come in pieces:
            // nothing is taken for a key until it is whole, and a string is an answer.
            let asking = self.colours.is_some();
            let Some((length, name)) = keys::next(rest, whole && !asking, asking) else { break };
            let raw = rest.get(..length).unwrap_or(&[]);
            at += length;
            match name.as_str() {
                "paste-start" => {
                    self.pasting = true;
                    self.typed.extend_from_slice(if wants_paste { raw } else { &[] });
                }
                name if name.starts_with("mouse:") => self.mouse(name.trim_start_matches("mouse:")),
                name if name.starts_with("terminal:") => {
                    self.sixel = name.split([':', '?', ';']).any(|n| n == "4");
                    // It was asked last: the colours the terminal told of before it are all it
                    // will tell. They go into the session's `colours` file, from which every
                    // pane's program is answered; the viewer that wrote last is believed.
                    self.told(true);
                }
                name if name.starts_with("answer:") => self.colour(name.trim_start_matches("answer:")),
                // The terminal takes the kitty keyboard protocol, and these of the flags it was asked for.
                name if name.starts_with("keyboard:") => {
                    self.keyboard = name.trim_start_matches("keyboard:").parse::<u8>().unwrap_or(0) & KEYS;
                }
                // The terminal's answer about joining characters into clusters (mode 2027): set (1,
                // or 3 for good) or not set but known (2), which is then turned on.
                // The window's own focus: for the pane that has the keys, if it asked to be told.
                name if name.starts_with("focus:") => {
                    // (To the pane that was told it has the keys, whichever the layout names by now.)
                    self.window = name == "focus:I";
                    if let Some(pane) = self.told_focus.clone() {
                        self.send();
                        let input = OpenOptions::new().write(true).custom_flags(sys::O_NONBLOCK).open(pane.join("in"));
                        let _ = input.and_then(|mut input| input.write(raw));
                    }
                }
                name if name.starts_with("mode:2027;") && !self.joins => {
                    let state = name.trim_start_matches("mode:2027;").parse::<u8>().unwrap_or(0);
                    (self.joins, self.joined) = (matches!(state, 1..=3), state == 2);
                    if self.joined {
                        self.write("\x1b[?2027h".into());
                    }
                }
                name if name.starts_with("mode:") => {}
                name => self.key(name, raw),
            }
            if self.waiting.is_some() || std::mem::take(&mut self.pause) {
                break;
            }
        }
        self.carry = bytes.get(at..).unwrap_or(&[]).to_vec();
        self.send();
    }

    /// The terminal's answer about a colour (`11;rgb:1c1c/1c1c/1c1c`, `4;1;rgb:cc/00/00`): a
    /// line for the `colours` file, if it was asked and the answer is one.
    fn colour(&mut self, answer: &str) {
        let Some((which, value)) = answer.split_once(";rgb:") else { return };
        let name = match which.split_once(';') {
            None if which == "10" => "foreground".to_string(),
            None if which == "11" => "background".to_string(),
            Some(("4", index)) if index.parse::<u8>().is_ok_and(|n| n.to_string() == index) => index.to_string(),
            _ => return,
        };
        // Each part is one to four hex digits of however many bits: its first eight are kept.
        let part = |part: &str| {
            let full = (1u32 << (4 * part.len().clamp(1, 4))) - 1;
            let hex = (1..=4).contains(&part.len()) && part.bytes().all(|b| b.is_ascii_hexdigit());
            u32::from_str_radix(part, 16).ok().filter(|_| hex).map(|value| value * 255 / full)
        };
        // Three parts, each a number, or it is no answer; and no more is kept than was asked.
        let parts: Vec<Option<u32>> = value.split('/').map(part).collect();
        if let (Some((_, told)), [Some(r), Some(g), Some(b)]) = (&mut self.colours, parts.as_slice())
            && told.len() < 8 << 10
        {
            told.push_str(&format!("{name} #{r:02x}{g:02x}{b:02x}\n"));
        }
    }

    /// The terminal has said all it will of what it is like (it answered the last question, or
    /// a second went by): the colours it told of go into the session's `colours` file, from
    /// which every pane's program is answered. If it told of none, the file is left as it is.
    /// The file is written whole under a name of this viewer's own and then given its name, so
    /// of two viewers the last to write is believed.
    fn told(&mut self, last: bool) {
        let Some((since, told)) = self.colours.take() else { return };
        // And which keyboard flags it takes (none, if it did not say): a program in a pane that
        // asks is told no more than that.
        let files = [("colours", told.clone()), ("keyboard", format!("{}\n", self.keyboard))];
        for (name, text) in files.into_iter().filter(|(name, text)| *name == "keyboard" || !text.is_empty()) {
            // (Only what is new since it was written last.)
            if self.written.iter().any(|(file, was)| *file == name && *was == text) {
                continue;
            }
            let own = self.session.join(format!(".{name}.{}", std::process::id()));
            let written = fs::write(&own, &text).and_then(|()| fs::rename(&own, self.session.join(name)));
            if let Err(e) = written {
                let _ = fs::remove_file(&own);
                self.message = format!("cannot write {}: {e}", self.session.join(name).display());
            }
            self.written.retain(|(file, _)| *file != name);
            self.written.push((name, text));
        }
        // Until its last answer has come, the terminal is still being listened to.
        self.colours = (!last).then_some((since, told));
    }

    /// Tells the pane that has the keys so, if it asked to be told (mode 1004), and the one
    /// that was told before that it has them no more.
    fn tell(&mut self, now: Option<PathBuf>) {
        if self.told_focus != now {
            // (The one that was told is told no more only if it still asks: a program that has
            // turned the mode off, or has ended, left a shell there that never asked.)
            // (While the window itself is without the keys, the one was told so already and the
            // other is told when the window has them again.)
            let told = self.told_focus.take().filter(|pane| self.window && asks(pane).focus);
            for (pane, report) in [(told, "\x1b[O"), (now.clone().filter(|_| self.window), "\x1b[I")] {
                let input =
                    pane.map(|pane| OpenOptions::new().write(true).custom_flags(sys::O_NONBLOCK).open(pane.join("in")));
                let _ = input.map(|input| input.and_then(|mut input| input.write_all(report.as_bytes())));
            }
            self.told_focus = now;
        }
    }

    /// Looks at the `notice` of every pane of the workspace, four times a second: a new one
    /// is passed on to this terminal once (the bell as a bell, words as a notice), said in the
    /// corner, and remembered until its pane has the keys in the tab shown.
    fn notices(&mut self, layout: &Layout, directory: &Path) {
        // What the pane with the keys had to say has been seen.
        let key = |name: &str| directory.join(name).to_string_lossy().into_owned();
        if let Some(seen) = self.focus.as_ref().and_then(|focus| self.notices.get_mut(&key(focus))) {
            seen.1 = false;
        }
        if self.looked.is_some_and(|at| at.elapsed() < Duration::from_millis(250)) {
            return;
        }
        self.looked = Some(Instant::now());
        if self.notices.len() > 4000 {
            self.notices.clear();
        }
        // (A workspace looked at for the first time: what is there is old, and is not rung. When
        // that was is kept, as a file's time is.)
        // (A file's time may be some milliseconds behind the clock's.)
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        let now = now.saturating_sub(Duration::from_millis(20));
        let started = (0, now.as_secs() as i64, i64::from(now.subsec_nanos()));
        let first = !self.notices.contains_key(&key(""));
        let since = self.notices.entry(key("")).or_insert((Some(started), false)).0;
        for name in layout.tabs.iter().flat_map(|tab| tab.panes()) {
            let file = directory.join(name).join("notice");
            let stamp: Stamp =
                fs::metadata(&file).ok().filter(|m| m.is_file()).map(|m| (m.ino(), m.mtime(), m.mtime_nsec()));
            let known = self.notices.get(&key(name)).copied();
            if known.is_some_and(|(was, _)| was == stamp) {
                continue;
            }
            // (One that came with its pane is old too, if it was written before the workspace
            // was first looked at: a pane moved here. A new pane's first is new.)
            let after = stamp.zip(since).is_some_and(|(stamp, since)| (stamp.1, stamp.2) > (since.1, since.2));
            let new = stamp.is_some() && !first && (known.is_some() || after);
            self.notices.insert(key(name), (stamp, stamp.is_some() && self.focus.as_deref() != Some(name)));
            if new {
                let mut text = Vec::new();
                let file = OpenOptions::new().read(true).custom_flags(sys::O_NONBLOCK).open(&file);
                let _ = file.map(|file| file.take(1200).read_to_end(&mut text));
                let text = String::from_utf8_lossy(&text);
                let words = text.lines().next().unwrap_or_default().split_once(' ').map_or("", |(_, words)| words);
                let words: String = words.chars().filter(|c| !c.is_control()).take(200).collect();
                // (With the pane's name before them, the words cannot be taken for one of the
                // numbered commands some terminals read after `9;`.)
                let rung = if words.is_empty() { "\x07".to_string() } else { format!("\x1b]9;{name}: {words}\x1b\\") };
                self.write(rung);
                self.message = if words.is_empty() { format!("{name}: bell") } else { format!("{name}: {words}") };
            }
        }
    }

    /// One key: a script if the key's file is one, a menu if it is a
    /// directory, and otherwise the key goes to the pane.
    fn key(&mut self, name: &str, raw: &[u8]) {
        // (A key comes from a window that has the keys, whatever it last told of its focus.)
        self.window = true;
        // A program may have asked to be told when keys are let go and held (the kitty
        // protocol's flag 2), and the terminal then tells the viewer too. A key belongs, until it
        // is let go, to whoever got its press, and only a press makes it someone's: a repeat or a
        // release with no press goes nowhere. The viewer's own (the prefix, a key of a menu): its
        // repeats and its release go nowhere. A pane's: they go to that pane, whether a menu is
        // open or not. While a menu is open, a modifier key by itself is nothing.
        let event = keys::event(raw);
        // (A sign may be told by its own number when pressed and by its key's when let go.)
        let same = |key: &(u32, u8)| event.is_some_and(|(id, sign, ..)| *key == id || Some(*key) == sign);
        if let Some((_, _, kind, modifier)) = event {
            let went = self.down.iter().find(|(key, _)| same(key)).map(|(_, went)| went.clone());
            if kind != 2 {
                self.down.retain(|(key, _)| !same(key));
            }
            match (kind, went) {
                (1, _) => {}
                (2 | 3, Some(Went::Pane(directory, pid))) => return self.pass(&directory, pid, raw),
                // Held, a key does again what its press did, there and only there: the script it
                // ran in a menu that stays open (to resize, say), while that menu is the one open.
                (2, Some(Went::Viewer(Some(script)))) => {
                    let menu = self.menu.as_ref().map(|(directory, _)| directory.as_path());
                    if menu.is_some() && menu == script.parent() && script.with_file_name(".repeat").exists() {
                        self.send();
                        self.script(&script);
                    }
                    return;
                }
                // (And what is no press, repeat or release is nothing.)
                _ => return,
            }
            if modifier && self.menu.is_some() {
                return;
            }
        }
        self.message.clear();
        let menu = self.menu.take();
        let file = menu.as_ref().map_or(&self.keys, |(directory, _)| directory).join(name);
        let went = match fs::metadata(&file) {
            Ok(kind) if !name.is_empty() && kind.is_dir() => {
                self.menu = Some((file.clone(), menu_entries(&file)));
                Went::Viewer(None)
            }
            Ok(kind) if !name.is_empty() && kind.is_file() => {
                self.send();
                self.script(&file);
                // A menu with a file `.repeat` in it stays for the next key (to resize, say), until a key it has not.
                self.menu = menu.filter(|(directory, _)| directory.join(".repeat").exists());
                Went::Viewer(Some(file))
            }
            _ if menu.is_none() => {
                self.typed.extend_from_slice(raw);
                self.keyed = Some(Instant::now());
                // (Its release is the pane's only if the program there asks to be told of
                // releases now: a key typed to one that does not is typed, and no more.)
                let directory = self.session.join(&self.workspace).join(self.focus.as_deref().unwrap_or_default());
                match program(&directory) {
                    (pid, true) if pid != 0 => Went::Pane(directory, pid),
                    _ => Went::Viewer(None),
                }
            }
            // A key that a menu which stays open does not have ends it; if it is the key that
            // led there (the prefix), its own menu is open again, as when it is pressed anew.
            _ => {
                let top = self.keys.join(name);
                if !name.is_empty()
                    && menu.is_some_and(|(was, _)| was != top && was.starts_with(&top) && was.join(".repeat").exists())
                {
                    self.menu = Some((top.clone(), menu_entries(&top)));
                }
                Went::Viewer(None)
            }
        };
        // Which keys are down is remembered for their releases: the last 64.
        if let Some((id, ..)) = event {
            self.down.retain(|(key, _)| !same(key));
            let extra = (self.down.len() + 1).saturating_sub(64);
            self.down.drain(..extra);
            self.down.push((id, went));
        }
    }

    /// A key's repeat or release goes to the pane that got its press, after what was typed before
    /// it, if that pane's program is still the one that got it and still asks to be told of keys
    /// let go. Who a pane is, is read when it matters: nothing the viewer remembers of it is asked.
    fn pass(&mut self, directory: &Path, pid: u32, raw: &[u8]) {
        if pid != 0 && program(directory) == (pid, true) {
            self.send();
            let input = OpenOptions::new().write(true).custom_flags(sys::O_NONBLOCK).open(directory.join("in"));
            let _ = input.and_then(|mut input| input.write(raw));
        }
    }

    /// The keys are with the pane `has` now, or with none: a key that is down in another pane is
    /// let go there, so that a press is followed by its release (the terminal, asked for the next
    /// pane's flags or left to itself, may never tell of it, or tell of it in another form), and
    /// what the terminal still tells of that key reaches no pane.
    fn leave(&mut self, has: Option<&Path>) {
        for at in 0..self.down.len() {
            if let Some(((code, last), Went::Pane(directory, pid))) = self.down.get(at).cloned()
                && Some(directory.as_path()) != has
            {
                self.pass(&directory, pid, format!("\x1b[{code};1:3{}", char::from(last)).as_bytes());
                self.down.get_mut(at).into_iter().for_each(|(_, went)| *went = Went::Viewer(None));
            }
        }
    }

    /// A mouse report from the terminal. It goes to the focused pane's
    /// program, counted from its own tile's corner and in the form it asked
    /// for. A press on another tile gives that one the keys, and nothing more.
    fn mouse(&mut self, report: &str) {
        let release = report.ends_with('m');
        let mut numbers = report.trim_end_matches(['M', 'm']).split(';').filter_map(|n| n.parse::<usize>().ok());
        let (Some(button), Some(x), Some(y)) = (numbers.next(), numbers.next(), numbers.next()) else { return };
        // A press: not a move (32), not the wheel (64), a button down; Shift, Alt and Ctrl (4, 8, 16) aside.
        let press = !release && button & 0x60 == 0 && button & 3 != 3;
        if self.held == Some(false) {
            // The rest of a click that only moved the focus is nobody's.
            self.held = if release { None } else { self.held };
            return;
        }
        let inside = |r: &Rect| (r.x + 1..=r.x + r.w).contains(&x) && (r.y + 1..=r.y + r.h).contains(&y);
        let over = self.tiles.iter().find(|(_, body)| inside(body)).map(|(name, _)| name.clone());
        if self.held.is_none() && over != self.focus {
            if let Some(name) = over.filter(|_| press) {
                let _ = layout::command(&self.session.join(&self.workspace), &["pane", &name]);
                // Keys typed after the click are for the pane it chose: they wait for the next look at the layout.
                (self.held, self.pause) = (Some(false), true);
            }
            return;
        }
        // (Where the tile is, is as it was drawn; what its program asks to be told is read now.)
        let focused = self.tiles.iter().find(|(name, _)| self.focus.as_ref() == Some(name));
        let Some((term, body)) =
            focused.map(|(name, body)| (asks(&self.session.join(&self.workspace).join(name)), *body))
        else {
            return;
        };
        // A button held stays that program's until it is let go: a drag out of the tile is reported at its edge.
        self.held = if press {
            Some(true)
        } else if release {
            None
        } else {
            self.held
        };
        let (x, y) = (x.clamp(body.x + 1, body.x + body.w) - body.x, y.clamp(body.y + 1, body.y + body.h) - body.y);
        // The oldest kind of report (9) knows only presses of the three buttons.
        if term.mouse == 0 || (term.mouse == 9 && !press) {
            return;
        }
        if term.mouse_sgr {
            let end = if release { 'm' } else { 'M' };
            self.typed.extend_from_slice(format!("\x1b[<{button};{x};{y}{end}").as_bytes());
        } else if let Ok(button) = u8::try_from(button) {
            // The old form: a release is button 3 with the modifiers, and a place past 223 is sent as 0.
            let old = |n: usize| u8::try_from(n).ok().and_then(|n| n.checked_add(32)).unwrap_or(0);
            let button = if release { 3 | (button & 0x1c) } else { button };
            self.typed.extend_from_slice(&[0x1b, b'[', b'M', button.saturating_add(32), old(x), old(y)]);
        }
    }

    fn script(&mut self, file: &Path) {
        if self.scripts.len() >= SCRIPTS_MAX {
            self.message = format!("{SCRIPTS_MAX} key scripts are still running: {} was not started", file.display());
            return;
        }
        let pane = self.focus.as_ref().map(|focus| self.session.join(&self.workspace).join(focus));
        let mut path =
            std::env::current_exe().ok().and_then(|exe| Some(exe.parent()?.as_os_str().to_owned())).unwrap_or_default();
        path.push(":");
        path.push(std::env::var_os("PATH").unwrap_or_default());
        // A writer of the script's own, which waits when the FIFO is full (ours never waits).
        let output = || OpenOptions::new().write(true).open(&self.ctl_path).map(Stdio::from);
        let (Ok(out), Ok(err)) = (output(), output()) else { return };
        let mut script = Command::new(file);
        script.env(name!("DIR"), &self.session).env(name!("WS"), &self.workspace).env(name!("KEYS"), &self.keys);
        script.env(name!("PANE"), pane.unwrap_or_default()).env(name!("VIEW"), &self.ctl_path).env("PATH", path);
        script.env(name!("SELF"), &self.own);
        // A pane a script starts begins at the size of the whole view, right for a new tab. The
        // size a tile would have if the focused pane were split is given too, side by side
        // (ROW) and one above the other (COL): a script that splits passes it on to `fux start`,
        // and its program is never drawn at one size and then at another.
        script.env("COLUMNS", self.size.cols.to_string()).env("LINES", self.size.rows.to_string());
        let area = Rect { x: 0, y: 0, w: usize::from(self.size.cols), h: usize::from(self.size.rows) };
        let layout = Layout::load(&self.session.join(&self.workspace)).1;
        for (name, way) in [(name!("ROW"), "row"), (name!("COL"), "col")] {
            // A pane no folder can be named after is split off in a copy of the layout.
            let mut split = layout.clone();
            split.tabs.push(layout::Tab { root: layout::Node::Pane("\0".into()), focus: "\0".into() });
            // Beside the pane the script is told of, as the split keys split: the same pane for both.
            let words: &[&str] = match &self.focus {
                Some(focus) => &["split", way, "\0", "beside", focus],
                None => &["split", way, "\0"],
            };
            let tile = split.edit(words).ok().and_then(|()| {
                split.tab()?.tiles(area).into_iter().find(|(pane, _)| pane == "\0").map(|(_, tile)| tile)
            });
            let tile = tile.unwrap_or(area);
            script.env(name, format!("{}x{}", tile.w, tile.h));
        }
        match script.stdin(Stdio::null()).stdout(out).stderr(err).spawn() {
            Ok(child) => {
                self.waiting = Some((child.id(), Instant::now()));
                self.scripts.push(child);
            }
            Err(e) => self.message = format!("{}: {e}", file.file_name().unwrap_or_default().to_string_lossy()),
        }
    }

    /// Writes what was typed to the focused pane's `in`, without waiting.
    fn send(&mut self) {
        let typed = std::mem::take(&mut self.typed);
        let Some(name) = self.focus.as_ref().filter(|_| !typed.is_empty()) else { return };
        // Resolve the name again: a live pane can have moved since the last frame.
        let path = self.session.join(&self.workspace).join(name).join("in");
        let input = OpenOptions::new().write(true).custom_flags(sys::O_NONBLOCK).open(path);
        match input.and_then(|mut input| input.write(&typed)) {
            Ok(n) if n == typed.len() => {}
            Ok(n) => self.message = format!("{name}: only {n} of {} typed bytes were queued", typed.len()),
            // A FIFO nobody reads (6, on Linux and macOS alike), or none at all: the pane has ended.
            Err(e) if e.raw_os_error() == Some(6) || e.kind() == io::ErrorKind::NotFound => {
                self.message = format!("{name} has ended: nothing reads what you type");
            }
            Err(e) => self.message = format!("{name}: {e}: what you typed was dropped"),
        }
    }

    // ---- drawing ---------------------------------------------------------

    /// Looks at the files and draws what changed. Says whether anything did.
    fn frame(&mut self) -> io::Result<bool> {
        // One frame at a time: while the terminal is still reading the last, the next one waits
        // (and is then of what the panes show by then), and the keys do not.
        if self.unwritten.load(Ordering::Acquire) > 0 {
            return Ok(false);
        }
        let mut size = sys::winsize(&self.tty)?;
        let cell_size = (size.xpixel / size.cols.max(1), size.ypixel / size.rows.max(1));
        // A terminal larger than the largest pane is drawn in its top left corner.
        (size.cols, size.rows) = (size.cols.min(SIZE_MAX as u16), size.rows.min(SIZE_MAX as u16));
        let (w, h) = (usize::from(size.cols), usize::from(size.rows));
        let mut out = String::new();
        if size != self.size {
            self.size = size;
            self.front = vec![Cell { ch: '\u{1}', ..Cell::blank(Style::default()) }; w * h];
            self.drawn.clear();
            out.push_str("\x1b[m\x1b[2J");
        }
        // A workspace that is gone gives way to the one shown before it, if
        // that is still there, and else to `main`, if that is.
        if !self.session.join(&self.workspace).is_dir() {
            let other = [self.before.take().unwrap_or_default(), "main".into()];
            if let Some(other) = other.into_iter().find(|name| is_name(name) && self.session.join(name).is_dir()) {
                self.message = format!("workspace {} is gone", std::mem::replace(&mut self.workspace, other));
                self.forget();
            }
        }
        let directory = self.session.join(&self.workspace);
        let (text, mut layout, mut wrong) = Layout::load(&directory);
        // The file says what is shown: when panes came or went, it is written
        // again, so that an edit is always an edit of what is on the screen.
        if wrong.is_none() && text != layout.print() {
            wrong = layout::command(&directory, &[]).err();
            layout = Layout::load(&directory).1;
        }
        let tab = layout.tab();
        // Panes get every cell but the lines between them: no titles, and no row for a bar.
        let tiles = tab.map(|tab| tab.tiles(Rect { x: 0, y: 0, w, h })).unwrap_or_default();
        self.focus = tab.map(|tab| tab.focus.clone());
        self.notices(&layout, &directory);
        // A frame drawn from what the last one was drawn from is the last one: nothing is put
        // together and nothing compared. (A menu is drawn from more than this: with one open,
        // every frame is.)
        let stamp = |file: PathBuf| fs::metadata(file).ok();
        let panes: Vec<(Seen, bool)> = tiles
            .iter()
            .map(|(name, _)| (directory.join(name), directory.join(name).join("screen.styled")))
            .map(|(pane, screen)| {
                let held = OpenOptions::new()
                    .read(true)
                    .custom_flags(sys::O_NONBLOCK)
                    .open(pane)
                    .is_ok_and(|dir| matches!(dir.try_lock_shared(), Err(fs::TryLockError::WouldBlock)));
                (stamp(screen).map(|m| (m.ino(), m.mtime(), m.mtime_nsec(), m.len())), held)
            })
            .collect();
        let held: Vec<bool> = panes.iter().map(|(_, held)| *held).collect();
        let clipboard = self.focus.as_ref().and_then(|name| stamp(directory.join(name).join("clipboard")));
        // (By when the file last changed in any way: one that could not be read may be readable now.)
        let clipboard = clipboard.map(|m| (m.ino(), m.ctime(), m.ctime_nsec()));
        let terminal = (self.sixel, self.joins, self.keyboard, self.window, cell_size);
        let from = (directory.clone(), layout.print(), wrong.clone(), self.message.clone(), terminal, panes, clipboard);
        let from = self.menu.is_none().then_some(from);
        if out.is_empty() && from.is_some() && from == self.from {
            return Ok(false);
        }
        self.from = from;
        let colour = |fg: u8, bg: Option<u8>, flags: u16| Style {
            fg: Color::Indexed(fg),
            bg: bg.map_or(Color::Default, Color::Indexed),
            ul: Color::Default,
            flags,
        };
        // What no tile covers is a line: marked here, drawn once the tiles are in place.
        let line = Cell { ch: '\u{2}', ..Cell::blank(Style::default()) };
        let mut back = vec![if tiles.is_empty() { Cell::blank(Style::default()) } else { line }; w * h];
        // (The table of links' addresses keeps only those of the cells on the terminal, numbered
        // anew, when it has grown large and most of it is of links that are drawn no more, or
        // when this frame's could be more than it has numbers left for.)
        let most = usize::from(u16::MAX).saturating_sub(1000 * tiles.len());
        if self.links.len() > 4000 && (self.links.len() > 2 * self.live || self.links.len() > most) {
            let mut old = std::mem::take(&mut self.links);
            let mut moved = vec![0u16; old.len()];
            for cell in &mut self.front {
                let at = usize::from(cell.link).wrapping_sub(1);
                cell.link = match (moved.get_mut(at), old.get_mut(at)) {
                    (Some(number), Some(address)) => {
                        if *number == 0 {
                            self.links.push(std::mem::take(address));
                            *number = self.links.len() as u16;
                        }
                        *number
                    }
                    _ => 0,
                };
            }
            self.numbers =
                self.links.iter().zip(1..=u16::MAX).map(|(address, number)| (address.clone(), number)).collect();
        }
        self.panes.retain(|name, _| layout.tabs.iter().any(|tab| tab.panes().contains(&name.as_str())));
        self.tiles = tiles.clone();
        let (mut cursor, mut pictures): (String, Vec<Drawn>) = (String::new(), Vec::new());
        let mut live = 0;
        let mut title = None;
        let mut modes = String::from("\x1b[?9l\x1b[?1000l\x1b[?1002l\x1b[?1003l");
        let mut wants_focus = None;
        let mut keys = 0;
        for ((name, body), held) in tiles.iter().zip(held) {
            let pane = self.panes.entry(name.clone()).or_insert_with(Pane::new);
            let pane_directory = directory.join(name);
            pane.read(&pane_directory);
            let focused = self.focus.as_ref() == Some(name);
            if body.w == 0 || body.h == 0 {
                continue;
            }
            // Each holder is asked once to take the tile's size, and told the size of a cell in pixels.
            let asked = (pane.pid, body.w, body.h, cell_size);
            if pane.asked != asked {
                pane.asked = asked;
                let ctl = OpenOptions::new().write(true).custom_flags(sys::O_NONBLOCK).open(pane_directory.join("ctl"));
                let cell = if cell_size.0 > 0 {
                    format!(" {} {}", cell_size.0.min(100), cell_size.1.clamp(1, 100))
                } else {
                    String::new()
                };
                if pane.size != (body.w, body.h) || !cell.is_empty() {
                    let _ = ctl
                        .and_then(|mut ctl| ctl.write_all(format!("resize {} {}{cell}\n", body.w, body.h).as_bytes()));
                }
            }
            let mut mine: Vec<Option<u16>> = vec![None; pane.addresses.len()];
            for y in 0..body.h {
                for x in 0..body.w {
                    let mut cell = pane.term.cell_at(y, x).copied().unwrap_or(Cell::blank(Style::default()));
                    // Half a wide character at the tile's edge is drawn as a blank.
                    let cut = x + 1 == body.w && pane.term.cell_at(y, x + 1).is_some_and(|next| next.ch == '\0');
                    cell = if cut { Cell::blank(cell.style) } else { cell };
                    // A character your terminal may draw wider than fux counts it (it may not join what
                    // fux joins) would there reach past the tile's edge: a `?` stands in each of its columns.
                    let first = x - usize::from(cell.ch == '\0' && x > 0);
                    // (A terminal that says it joins characters as fux does draws it as fux counts it.)
                    if let Some(whole) = pane.term.cell_at(y, first).filter(|c| c.spill() > 0 && !self.joins)
                        && first + 1 + usize::from(whole.wide()) + whole.spill() > body.w
                    {
                        cell = Cell { ch: '?', more: ['\0'; MORE], ..cell };
                    }
                    // A link's number is its address's place in the viewer's own table (found once
                    // for each of the pane's addresses; past 65,535 addresses on one screen, no link).
                    cell.link = match usize::from(cell.link).checked_sub(1) {
                        Some(at) if mine.get(at).is_some_and(Option::is_some) => {
                            mine.get(at).copied().flatten().unwrap_or(0)
                        }
                        Some(at) => {
                            let address = pane.addresses.get(at);
                            let number = address.and_then(|address| {
                                let next = u16::try_from(self.links.len() + 1).ok();
                                let number = self.numbers.get(address).copied().or(next)?;
                                if usize::from(number) > self.links.len() {
                                    self.links.push(address.clone());
                                    self.numbers.insert(address.clone(), number);
                                }
                                Some(number)
                            });
                            // (Out of numbers: the next frame is put together too, and by then the
                            // table has given up those of the links this one draws no more.)
                            if address.is_some() && number.is_none() {
                                self.from = None;
                            }
                            mine.get_mut(at).into_iter().for_each(|slot| *slot = Some(number.unwrap_or(0)));
                            live += usize::from(number.is_some());
                            number.unwrap_or(0)
                        }
                        None => 0,
                    };
                    if let Some(slot) = back.get_mut((body.y + y) * w + body.x + x) {
                        *slot = cell;
                    }
                }
            }
            // A pane whose program ended, or whose holder is gone, says so in its last row's corner.
            let live = pane.state == "running" && held;
            let state = match (live, pane.state.as_str()) {
                (true, "running") => String::new(),
                (false, "running") => " lost ".into(),
                (_, "") => " no pane here ".into(),
                (_, state) => format!(" {state} "),
            };
            let state = cells(&state, colour(11, Some(236), 0));
            let room = state.len().min(body.w);
            put(&mut back, w, body.x + body.w - room, body.y + body.h - 1, room, &state);
            // What the focused pane's program copies goes to this terminal's clipboard,
            // once: when its `clipboard` file is another than at the last look, the pane
            // having had the focus then too. Coming to a pane only notes what is there.
            if focused {
                let file =
                    OpenOptions::new().read(true).custom_flags(sys::O_NONBLOCK).open(pane_directory.join("clipboard"));
                let stamp: Stamp = file
                    .as_ref()
                    .ok()
                    .and_then(|file| file.metadata().ok())
                    .map(|m| (m.ino(), m.mtime(), m.mtime_nsec()));
                let seen = self.copied.replace((pane_directory.clone(), stamp));
                if let (Ok(file), Some((before, old))) = (file, seen)
                    && before == pane_directory
                    && old != stamp
                {
                    let mut bytes = Vec::new();
                    let _ = file.take(1 << 20).read_to_end(&mut bytes);
                    out.push_str(&format!("\x1b]52;c;{}\x07", base64_encode(&bytes)));
                }
            }
            if focused && !pane.title.is_empty() {
                title = Some(pane.title.clone());
            }
            if focused && pane.term.cursor_shown && pane.term.row < body.h && pane.term.col < body.w {
                let (row, col) = (body.y + pane.term.row + 1, body.x + pane.term.col + 1);
                cursor = format!("\x1b[{row};{col}H\x1b[{} q\x1b[?25h", pane.term.cursor_shape);
            }
            // The outer terminal sends cursor and keypad keys the way the focused program asked for.
            if focused {
                let (cursor_keys, keypad) = (pane.term.app_cursor, pane.term.app_keypad);
                cursor.push_str(if cursor_keys { "\x1b[?1h" } else { "\x1b[?1l" });
                cursor.push_str(if keypad { "\x1b=" } else { "\x1b>" });
                // And reports the mouse when, and as far as, a living program asked (always in the SGR form).
                if pane.term.mouse != 0 && live {
                    modes.push_str(&format!("\x1b[?{}h\x1b[?1006h", pane.term.mouse));
                }
                // And is told when this terminal's window gains and loses the keys, if it asked.
                if pane.term.focus && live {
                    wants_focus = Some(pane_directory.clone());
                }
                // And tells keys as that program asked (the kitty protocol), if the terminal can.
                if live {
                    keys = pane.term.keys() & self.keyboard;
                }
            }
            for (file, row, col, cols, rows) in &pane.pictures {
                if self.sixel && file.ends_with(".sixel") && row + rows <= body.h && col + cols <= body.w {
                    pictures.push((pane_directory.join(file), body.x + col, body.y + row, *cols, *rows, pane.pid));
                }
            }
        }
        self.live = live;
        // The lines between panes: `│` and `─`, a tee or a cross where they
        // meet, and green where they touch the focused pane.
        let focus = tiles.iter().find(|(name, _)| self.focus.as_ref() == Some(name)).map(|(_, body)| *body);
        let lines: Vec<bool> = back.iter().map(|cell| cell.ch == '\u{2}').collect();
        let is_line = |x: Option<usize>, y: Option<usize>| match (x, y) {
            (Some(x), Some(y)) if x < w => lines.get(y * w + x) == Some(&true),
            _ => false,
        };
        for (i, cell) in back.iter_mut().enumerate().filter(|(i, _)| lines.get(*i) == Some(&true)) {
            let (x, y) = (i % w, i / w);
            let (up, down) = (is_line(Some(x), y.checked_sub(1)), is_line(Some(x), Some(y + 1)));
            let (left, right) = (is_line(x.checked_sub(1), Some(y)), is_line(Some(x + 1), Some(y)));
            let ch = match (up || down, left || right) {
                (true, true) if !left => '├',
                (true, true) if !right => '┤',
                (true, true) if !up => '┬',
                (true, true) if !down => '┴',
                (true, true) => '┼',
                (false, true) => '─',
                (true, false) => '│',
                // A line one cell long: upright if panes are beside it.
                (false, false) if x > 0 && x + 1 < w => '│',
                (false, false) => '─',
            };
            let near = focus.is_some_and(|f| x + 1 >= f.x && x <= f.x + f.w && y + 1 >= f.y && y <= f.y + f.h);
            *cell = Cell { ch, more: ['\0'; MORE], style: colour(if near { 2 } else { 240 }, None, 0), link: 0 };
        }
        // A notice sits in the bottom right corner until the next key: what is
        // wrong with the layout file, what a script said, or that there are no panes.
        let hint = format!("no panes in {}: keys are scripts in {}", self.workspace, self.keys.display());
        let (note, style) = match &wrong {
            Some(wrong) => (wrong, colour(9, Some(236), 0)),
            None if tiles.is_empty() && self.message.is_empty() => (&hint, colour(11, Some(236), 0)),
            None => (&self.message, colour(11, Some(236), 0)),
        };
        let last = h.saturating_sub(1);
        if !note.is_empty() && self.menu.is_none() {
            let note = cells(&format!(" {note} "), style);
            let room = note.len().min(w.saturating_sub(1));
            put(&mut back, w, w.saturating_sub(room + 1), last, room, &note);
        }
        if let Some((directory, entries)) = &self.menu {
            // While a prefix is in use: the bar over the last row (the
            // workspace, its tabs, the keys typed so far), and above it, in
            // the corner, the keys that can follow.
            let (bar, panel) = (colour(250, Some(236), 0), colour(255, Some(238), 0));
            let typed: Vec<String> = directory
                .strip_prefix(&self.keys)
                .into_iter()
                .flat_map(|path| path.iter())
                .map(|key| key.to_string_lossy().into_owned())
                .collect();
            let typed = typed.join(" ");
            let mut left = cells(&format!(" {} ", self.workspace), colour(250, Some(236), BOLD));
            let labels = layout.tabs.iter().enumerate().map(|(i, tab)| {
                // (A tab with a notice not seen yet has a mark.)
                let workspace = self.session.join(&self.workspace);
                let noted = |pane: &&str| self.notices.get(workspace.join(pane).to_string_lossy().as_ref());
                let unseen = tab.panes().iter().any(|pane| noted(pane).is_some_and(|notice| notice.1));
                let mark = if unseen { "!" } else { "" };
                cells(
                    &format!(" {}{mark} ", tab.focus),
                    if i == layout.current { colour(0, Some(2), BOLD) } else { bar },
                )
            });
            let labels: Vec<Vec<Cell>> = labels.collect();
            // On the right: what there is to say, or else the keys typed so far.
            let right = if note.is_empty() {
                cells(&format!("{typed} …"), colour(0, Some(11), BOLD))
            } else {
                cells(&format!(" {note} "), style)
            };
            let right: Vec<Cell> = right.into_iter().take(w * 3 / 4).collect();
            put(&mut back, w, 0, last, w, &cells(&" ".repeat(w), bar));
            // As many tabs as fit, the one shown among them.
            let room = w.saturating_sub(right.len() + 2);
            let fits = |first: usize| {
                left.len() + labels.iter().take(layout.current + 1).skip(first).map(Vec::len).sum::<usize>() <= room
            };
            let first = (0..=layout.current).find(|first| fits(*first)).unwrap_or(layout.current);
            left.extend(labels.iter().skip(first).flatten());
            put(&mut back, w, 0, last, room, &left);
            put(&mut back, w, w.saturating_sub(right.len() + 1), last, right.len(), &right);
            // The keys, under their headings; what does not fit above the bar is counted, not shown.
            let key_width = entries.iter().map(|(key, ..)| key.chars().count()).max().unwrap_or(0);
            let mut lines = vec![(typed, BOLD)];
            for (i, (key, about, group)) in entries.iter().enumerate() {
                if !group.is_empty()
                    && i.checked_sub(1).and_then(|before| entries.get(before)).is_none_or(|before| before.2 != *group)
                {
                    lines.push((group.clone(), BOLD));
                }
                lines.push((format!("{key:>key_width$}  {about}"), 0));
            }
            // A box as wide as its longest line, in cells, and a cell more on each
            // side. A menu taller than the room above the bar goes on in a
            // second box to the right of the first, and so on; what still does
            // not fit is counted (lines, headings among them), not shown.
            let lines: Vec<Vec<Cell>> = lines
                .iter()
                .map(|(text, flags)| cells(&format!(" {text}"), Style { flags: *flags, ..panel }))
                .collect();
            let widths = |boxes: &[&[Vec<Cell>]]| {
                boxes.iter().map(|part| part.iter().map(Vec::len).max().unwrap_or(0) + 1).sum::<usize>()
            };
            let mut boxes: Vec<&[Vec<Cell>]> = lines.chunks(last.max(1)).collect();
            let all = boxes.len();
            while boxes.len() > 1 && widths(&boxes) > w {
                boxes.pop();
            }
            // The last line shown gives its place to the count, so it is counted too.
            let hidden: usize = lines.chunks(last.max(1)).skip(boxes.len()).map(<[_]>::len).sum::<usize>() + 1;
            let more = cells(&format!(" ▼ {hidden} more"), Style { flags: DIM, ..panel });
            let (height, mut right) = (boxes.first().map_or(0, |part| part.len()), w);
            for (n, part) in boxes.iter().enumerate().rev() {
                let width = (part.iter().map(Vec::len).max().unwrap_or(0) + 1).min(right);
                right -= width;
                for i in 0..height {
                    let y = last.saturating_sub(height) + i;
                    let hides = n + 1 == boxes.len() && boxes.len() < all && i + 1 == height;
                    let line = if hides { Some(&more) } else { part.get(i) };
                    put(&mut back, w, right, y, width, &cells(&" ".repeat(width), panel));
                    put(&mut back, w, right, y, width, line.map_or(&[], |line| line));
                }
            }
            cursor = String::new();
        }
        if cursor.is_empty() {
            cursor.push_str("\x1b[?25l");
        }
        modes.push_str(if wants_focus.is_some() { "\x1b[?1004h" } else { "\x1b[?1004l" });
        self.tell(wants_focus);
        self.leave(self.focus.as_ref().map(|name| directory.join(name)).as_deref());
        if self.keyboard != 0 {
            cursor.push_str(&format!("\x1b[={keys}u"));
        }
        // A menu is drawn over the panes, pictures and all: none is drawn while one is open.
        if self.menu.is_some() {
            pictures.clear();
        }

        // Pictures are pixels over cells: when one comes, goes or is written
        // over, the cells under the old ones are sent again and all are redrawn.
        let mut redraw = pictures != self.drawn;
        if redraw {
            for (_, x, y, cols, rows, _) in &self.drawn {
                for cell in (0..*rows).flat_map(|r| (0..*cols).map(move |c| (y + r) * w + x + c)) {
                    self.front.get_mut(cell).into_iter().for_each(|cell| cell.ch = '\u{1}');
                }
            }
        }
        let mut style = None;
        let mut at = None;
        let mut pen = 0;
        // The cells from here to `again` are sent whether they changed or not: a character before
        // them may have been drawn over them.
        let mut again = 0;
        // What a terminal would join to the character in the cell before it (half a flag to the
        // other half, anything to U+0600) is kept apart from it by U+200B, as in `screen.styled`:
        // a move of the cursor to where it is parts nothing. So every cell is its own character.
        let mut joiner = Joiner::default();
        for (i, (new, old)) in back.iter().zip(self.front.iter_mut()).enumerate() {
            let (x, y) = (i % w.max(1), i / w.max(1));
            (again, joiner) = if x == 0 { (0, Joiner::default()) } else { (again, joiner) };
            let apart = new.ch != '\0' && joiner.joins(new.ch);
            if apart {
                joiner = Joiner::default();
                joiner.joins(new.ch);
            }
            new.more.iter().take_while(|&&c| c != '\0').for_each(|&c| _ = joiner.joins(c));
            if new == old && i >= again {
                continue;
            }
            *old = *new;
            redraw |= pictures.iter().any(|p| (p.1..p.1 + p.3).contains(&x) && (p.2..p.2 + p.4).contains(&y));
            if new.ch == '\0' {
                continue;
            }
            let place = format!("\x1b[{};{}H", y + 1, x + 1);
            // Anything but plain ASCII is placed by its row and column, and so is what follows it:
            // your terminal's idea of a character's columns, or of what joins what, may not be fux's.
            let ascii = new.ch.is_ascii() && new.more.first() == Some(&'\0');
            // (The terminal is asked not to wrap, so that what it draws too wide at the screen's
            // edge stays on its row: every row starts with its place.)
            if at != Some(i) || !ascii || x == 0 {
                // (A link is closed before the cursor moves: none is open over cells not drawn.)
                if std::mem::take(&mut pen) != 0 {
                    out.push_str("\x1b]8;;\x1b\\");
                }
                out.push_str(&place);
            }
            if style != Some(new.style) {
                new.style.sgr(&mut out);
                style = Some(new.style);
            }
            if apart {
                out.push('\u{200b}');
            }
            let link = |out: &mut String, number: u16| {
                let address = usize::from(number).checked_sub(1).and_then(|at| self.links.get(at));
                out.push_str(&format!("\x1b]8;;{}\x1b\\", address.map_or("", String::as_str)));
            };
            // A wide character that may come out narrower (one of several parts, an emoji) is
            // drawn over two blanks, of its link as it is (closed again before the cursor goes
            // back over them); the cells one may come out over are sent after it.
            if new.wide() && !new.plain() {
                if new.link != pen {
                    link(&mut out, new.link);
                }
                out.push_str("  ");
                if new.link != 0 {
                    link(&mut out, 0);
                }
                pen = 0;
                out.push_str(&place);
            }
            // A cell that is part of a link is drawn inside it (OSC 8): the link is opened before
            // the first such cell and closed before the next cell that is not part of it, so
            // none reaches past its tile or over a line between tiles.
            if new.link != pen {
                link(&mut out, new.link);
                pen = new.link;
            }
            new.push_text(&mut out);
            again = again.max(i + 1 + usize::from(new.wide()) + new.spill());
            at = ascii.then_some(i + 1);
        }
        if pen != 0 {
            out.push_str("\x1b]8;;\x1b\\");
        }
        if redraw {
            for (file, x, y, ..) in &pictures {
                // Opened without waiting (should the name be a FIFO), and read up to a picture's bound.
                let mut bytes = Vec::new();
                let file = OpenOptions::new().read(true).custom_flags(sys::O_NONBLOCK).open(file);
                let _ = file.map(|file| file.take(PICTURE_MAX as u64 + 8).read_to_end(&mut bytes));
                let inside = bytes.strip_prefix(b"\x1bP").and_then(|rest| rest.strip_suffix(b"\x1b\\"));
                // Only one whole sixel picture is passed on: nothing else in the file can reach the terminal.
                let sixel =
                    |inside: &&[u8]| inside.iter().find(|b| !(b.is_ascii_digit() || **b == b';')) == Some(&b'q');
                if let Some(inside) =
                    inside.filter(|inside| inside.iter().all(|b| (0x20..=0x7e).contains(b))).filter(sixel)
                {
                    out.push_str(&format!("\x1b[{};{}H\x1bP{}\x1b\\", y + 1, x + 1, String::from_utf8_lossy(inside)));
                }
            }
            self.drawn = pictures;
        }
        // This terminal's title is the focused pane's; with none, it is what it was before the viewer.
        if title != self.title {
            out.push_str(&title.as_ref().map_or("\x1b[23;2t\x1b[22;2t".into(), |title| format!("\x1b]2;{title}\x07")));
            self.title = title;
        }
        if out.is_empty() && cursor == self.cursor && modes == self.modes {
            return Ok(false);
        }
        // (What the terminal is to report is said when it changes, not with every frame: a
        // terminal may answer the asking itself.)
        let set = if modes == self.modes { "" } else { modes.as_str() };
        self.write(format!("\x1b[?2026h\x1b[?25l{out}\x1b[m{cursor}{set}\x1b[?2026l"));
        (self.cursor, self.modes) = (cursor, modes);
        Ok(true)
    }
}

/// Text of the viewer's own (a title, the last line, a menu) as cells: a
/// control character becomes `?`, and the rest is cut into characters as a
/// pane's text is, by the emulator. So nothing from a file's name or a
/// script's words reaches the terminal as anything but text.
fn cells(text: &str, style: Style) -> Vec<Cell> {
    let mut bytes = String::new();
    style.sgr(&mut bytes);
    let text: Vec<char> = text.chars().take(SIZE_MAX / 2).collect();
    bytes.extend(text.iter().map(|&c| if c.is_control() { '?' } else { c }));
    // One row with room for all of it and a column to spare, so that nothing wraps.
    let mut row = Term::new(2 * text.len() + 1, 1);
    row.feed(bytes.as_bytes());
    (0..row.col).filter_map(|x| row.cell_at(0, x).copied()).collect()
}

/// Puts cells on row `y` from column `x`, at most `room` of them and none
/// past the screen's width `w`; a wide character that would be cut is a blank.
fn put(back: &mut [Cell], w: usize, x: usize, y: usize, room: usize, cells: &[Cell]) {
    let room = room.min(w.saturating_sub(x));
    // What is written over the right half of a wide character takes its left half with it.
    if room > 0
        && x > 0
        && back.get(y * w + x).is_some_and(|under| under.ch == '\0')
        && let Some(left) = back.get_mut(y * w + x - 1)
    {
        *left = Cell::blank(left.style);
    }
    for (i, cell) in cells.iter().enumerate().take(room) {
        let cut = i + 1 == room && cells.get(i + 1).is_some_and(|next| next.ch == '\0');
        if let Some(slot) = back.get_mut(y * w + x + i) {
            *slot = if cut { Cell::blank(cell.style) } else { *cell };
        }
    }
}

/// The first 256 bytes of one of a pane's small files, as it is now.
fn small(directory: &Path, name: &str) -> String {
    let mut bytes = Vec::new();
    let file = OpenOptions::new().read(true).custom_flags(sys::O_NONBLOCK).open(directory.join(name));
    let _ = file.map(|file| file.take(256).read_to_end(&mut bytes));
    String::from_utf8_lossy(&bytes).into_owned()
}

/// How a pane's program asks for its input (pastes marked, the mouse, the focus, keys), as its
/// `modes` file says now: what is typed is sent by this, not by what was last drawn.
fn asks(directory: &Path) -> Term {
    let mut term = Term::new(1, 1);
    term.set_words(&small(directory, "modes"));
    term
}

/// Who a pane's program is (the pid in `status`) and whether it asks to be told of keys let go
/// and held (flag 2 of the `keys` word in `modes`), as the files say now.
fn program(directory: &Path) -> (u32, bool) {
    let pid = small(directory, "status").lines().find_map(|line| line.strip_prefix("pid ")?.parse().ok()).unwrap_or(0);
    (pid, asks(directory).keys() & 2 != 0)
}

impl Pane {
    fn new() -> Pane {
        Pane {
            seen: None,
            term: Term::new(1, 1),
            size: (0, 0),
            pid: 0,
            state: String::new(),
            pictures: Vec::new(),
            addresses: Vec::new(),
            asked: (0, 0, 0, (0, 0)),
            title: String::new(),
        }
    }

    /// Reads the pane if its screen's file changed: the holder writes that file last, so every
    /// fact it wrote before is there by then. A fact may still be a moment ahead of the text
    /// (or far off, in a folder someone copied): the rows are the lines that are there, and the
    /// cursor is kept inside them.
    fn read(&mut self, directory: &Path) {
        let seen =
            fs::metadata(directory.join("screen.styled")).ok().map(|m| (m.ino(), m.mtime(), m.mtime_nsec(), m.len()));
        if seen == self.seen {
            return;
        }
        self.seen = seen;
        // Opened without waiting (should the name be a FIFO), and read up to a bound.
        let fact = |name: &str, most: u64| {
            let mut bytes = Vec::new();
            let file = OpenOptions::new().read(true).custom_flags(sys::O_NONBLOCK).open(directory.join(name));
            let _ = file.map(|file| file.take(most).read_to_end(&mut bytes));
            String::from_utf8_lossy(&bytes).into_owned()
        };
        let numbers = |text: &str| {
            text.split_whitespace().map(|n| n.parse::<usize>().unwrap_or(SIZE_MAX + 1)).collect::<Vec<_>>()
        };
        let styled = fact("screen.styled", STYLED_MAX);
        self.size = match numbers(&fact("size", 64)).as_slice() {
            [cols @ 1..=SIZE_MAX, rows @ 1..=SIZE_MAX] => (*cols, *rows),
            _ => (0, 0),
        };
        let cols = if self.size.0 == 0 { SIZE_MAX } else { self.size.0 };
        // (One column more than the pane is said to have, where there is room for one: there a
        // line that is longer ends, and is cut off.)
        self.term = Term::new((cols + 1).min(SIZE_MAX), styled.lines().count().clamp(1, SIZE_MAX));
        self.term.feed(b"\x1b[?7l");
        for (row, line) in styled.lines().enumerate().take(SIZE_MAX) {
            // The rows are text and colours, as the holder writes them: anything else in a
            // file of someone's own making is shown as `?`.
            let mut text = String::with_capacity(line.len());
            let mut rest = line;
            while let Some(at) = rest.find(|c: char| c.is_control()) {
                let (before, after) = rest.split_at(at);
                let colour = after.strip_prefix("\x1b[").and_then(|sgr| {
                    let end = sgr.find(|c: char| !(c.is_ascii_digit() || c == ';' || c == ':'))?;
                    sgr.get(end..)?.starts_with('m').then_some(2 + end + 1)
                });
                text.push_str(before);
                text.push_str(colour.and_then(|length| after.get(..length)).unwrap_or("?"));
                rest = after.get(colour.unwrap_or(1)..).unwrap_or_default();
            }
            text.push_str(rest);
            self.term.feed(format!("\x1b[{};1H\x1b[m{text}", row + 1).as_bytes());
            if cols < self.term.cols {
                self.term.feed(format!("\x1b[{};{}H\x1b[m\x1b[K", row + 1, cols + 1).as_bytes());
            }
        }
        self.term.set_words(&fact("modes", 256));
        let cursor = fact("cursor", 64);
        let mut cursor = cursor.split_whitespace();
        let mut place = || cursor.next().and_then(|n| n.parse::<usize>().ok());
        let at = (place(), place());
        self.term.cursor_shown = cursor.next() == Some("shown") && at.0.is_some_and(|row| row < self.term.rows);
        (self.term.row, self.term.col) = (at.0.unwrap_or(0).min(self.term.rows - 1), at.1.unwrap_or(0).min(cols - 1));
        self.term.cursor_shape =
            cursor.next().and_then(|shape| shape.parse().ok()).filter(|shape| *shape <= 6).unwrap_or(0);
        self.state = fact("state", 64).trim_end().to_string();
        let status = fact("status", 256);
        self.pid = status.lines().find_map(|line| line.strip_prefix("pid ")?.parse().ok()).unwrap_or(0);
        // A picture is kept only if it lies on a screen of the largest size there is.
        self.pictures.clear();
        for line in fact("pictures", 4096).lines() {
            let (name, place) = line.split_once(' ').unwrap_or_default();
            if let [row, col, cols, rows] = numbers(place).as_slice()
                && [row, col, cols, rows].iter().all(|n| **n <= SIZE_MAX)
                && is_name(name)
            {
                self.pictures.push((name.into(), *row, *col, *cols, *rows));
            }
        }
        // The links (`links`: line, cells into it, how many cells, address) are put on the cells.
        // `top` and `wraps` say which line each row is of; a row starts as many cells into its
        // line as the rows of that line above it hold. An address is plain printable text.
        let rows: Vec<usize> = numbers(&fact("wraps", 4096));
        let (mut line, mut base, mut starts) = (numbers(&fact("top", 32)).first().copied().unwrap_or(0), 0, Vec::new());
        for row in 0..self.term.rows {
            (line, base) = if row > 0 && !rows.contains(&row) { (line.saturating_add(1), 0) } else { (line, base) };
            starts.push((line, base));
            base += cols;
        }
        self.addresses.clear();
        // (Whole records only: a list cut by the bound ends before its last line's end. A thousand
        // addresses, as a holder keeps, and no more cells than the screen has.)
        let list = fact("links", LINKS_LIST_MAX as u64);
        let whole = list.rfind('\n').and_then(|end| list.get(..end)).unwrap_or_default();
        let (mut known, mut cells): (HashMap<&str, u16>, usize) = (HashMap::new(), self.term.rows * cols);
        for run in whole.lines() {
            let mut fields = run.splitn(4, ' ');
            let mut number = || fields.next().and_then(|n| n.parse::<usize>().ok());
            let (Some(line), Some(from), Some(length)) = (number(), number(), number()) else { continue };
            let address = fields.next().unwrap_or_default();
            if address.is_empty() || address.len() > 2048 || !address.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
                continue;
            }
            let link = match known.get(address) {
                Some(link) => *link,
                None if self.addresses.len() < 1000 => {
                    self.addresses.push(address.to_string());
                    known.insert(address, self.addresses.len() as u16);
                    self.addresses.len() as u16
                }
                None => continue,
            };
            for (row, (_, base)) in starts.iter().enumerate().filter(|(_, (of, _))| *of == line) {
                for col in (from.max(*base)..(from.saturating_add(length)).min(base + cols)).take(cells) {
                    self.term.set_link(row, col - base, link);
                    cells -= 1;
                }
            }
        }
        let title = fact("title", 1024);
        self.title = title.lines().next().unwrap_or_default().chars().filter(|c| !c.is_control()).take(256).collect();
    }
}

/// The entries of a menu: each key in the directory and, from its script's
/// second line, what it does; a third line `# group: NAME` names the heading
/// it goes under. A key that opens another menu has `…`, and its words and
/// group are in that directory's file `.about`. Sorted by heading, then key;
/// keys with no heading first. At most 200.
fn menu_entries(directory: &Path) -> Vec<Entry> {
    let names = fs::read_dir(directory).into_iter().flatten().flatten();
    let names = names.filter_map(|entry| entry.file_name().into_string().ok()).filter(|name| !name.starts_with('.'));
    let mut names: Vec<String> = names.take(MENU_MAX + 1).collect();
    let more = names.len() > MENU_MAX;
    names.truncate(MENU_MAX);
    let head = |path: &Path| {
        let mut head = Vec::new();
        let _ = File::open(path).map(|file| file.take(1024).read_to_end(&mut head));
        String::from_utf8_lossy(&head).into_owned()
    };
    let described = names.into_iter().map(|name| {
        let path = directory.join(&name);
        // A script's lines after `#!`: `# caption`, `# group: NAME`. A directory's `.about`: `caption`, `# group: NAME`.
        let (text, menu) =
            if path.is_dir() { (format!("\n# {}", head(&path.join(".about"))), "…") } else { (head(&path), "") };
        let mut lines = text.lines().skip(1).map(|line| line.strip_prefix("# ").unwrap_or_default());
        let about = format!("{}{menu}", lines.next().unwrap_or_default());
        (name, about, lines.next().and_then(|line| line.strip_prefix("group: ")).unwrap_or_default().to_string())
    });
    let mut entries: Vec<Entry> = described.collect();
    entries.sort_by(|a, b| (&a.2, &a.0).cmp(&(&b.2, &b.0)));
    entries.extend(more.then(|| ("…".to_string(), "more keys than a menu lists".to_string(), String::new())));
    entries
}
