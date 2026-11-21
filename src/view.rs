//! The viewer: one more client. It draws a workspace's panes from the files of
//! the screen each shows (`shown/screen.styled`) as the `layout` file places them, sends what is
//! typed to the focused pane's `in`, and runs a script for a key that has
//! one. It keeps nothing the files do not say, except which workspace it
//! shows.

use crate::layout::{self, Layout, Rect};
use crate::sys::{self, Winsize};
use crate::term::{
    BOLD, Cell, Color, DIM, KEYS, LINKS_LIST_MAX, MORE, PICTURE_MAX, SIZE_MAX, Screen, Style, Term, base64_encode,
};
use crate::width::Joiner;
use crate::{Stamp, is_name, keys, sixel, stamp, text};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::num::NonZeroU32;
use std::os::fd::AsFd;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
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

/// The most bytes of pictures in one frame, and so in a viewer's memory.
const FRAME_PICTURES_MAX: usize = 4 * PICTURE_MAX;

/// Which part of which picture: the file, as it was written, and the pixels its tile has for
/// it, counted in this terminal's cells: how many rows of its pixels are above the tile, and
/// how many pixels wide and high the tile is from the picture's corner on.
type Part = (PathBuf, Stamp, [usize; 3]);

/// What is drawn of a picture: the sixel, and how many pixels wide and high it is; or nothing.
type Sixel = Option<(String, usize, usize)>;

/// A picture on the terminal: its part, and the column, row, columns and rows of what is drawn
/// of it.
type Drawn = (Part, usize, usize, usize, usize);

/// What is drawn of a picture in the pixels its tile has for it, and how many pixels wide and
/// high that is: the picture as it is, unread, if its pixels are square and all of them are in
/// the tile, and else the part of it that is, cut out of it. Nothing, if the file is not one
/// whole sixel picture or none of it is in the tile; or why it is not drawn, in a few words.
pub fn part((path, _, [top, wide, high]): &Part) -> Result<Sixel, &'static str> {
    if *wide == 0 || *high == 0 {
        return Err("the terminal does not say how many pixels a cell has");
    }
    let (bytes, Ok(_)) = crate::read(sys::open(path, false), PICTURE_MAX as u64 + 9) else { return Ok(None) };
    if bytes.len() > PICTURE_MAX + 8 {
        return Err("its file is over 4 MiB");
    }
    // Only one whole sixel picture is passed on: nothing else in the file can reach the terminal.
    let inside = bytes.strip_prefix(b"\x1bP").and_then(|rest| rest.strip_suffix(b"\x1b\\"));
    let inside = inside.filter(|inside| Term::is_sixel(inside));
    let Some(inside) = inside else { return Ok(None) };
    let (w, h, square) = sixel::size(inside);
    let (wide, high) = (w.min(*wide), h.saturating_sub(*top).min(*high));
    let sixel = match square && (*top, wide, high) == (0, w, h) {
        true => String::from_utf8_lossy(inside).into_owned(),
        false => sixel::cut(inside, [0, *top, wide, high], PICTURE_MAX)?,
    };
    Ok((wide > 0 && high > 0 && !sixel.is_empty()).then_some((sixel, wide, high)))
}

/// What a frame is drawn from, other than the terminal's size and the workspace (another one
/// forgets it): the layout and what is wrong with it, the message, what the terminal is like and
/// whether its window has the keys, each tile's `generation` file (as `Pane::seen`: a holder writes
/// it anew after any file of its pane, last in each round of writing) and whether its holder lives,
/// the focused pane's `clipboard` file, and each picture's file.
type From = (String, Option<String>, String, (bool, bool, u8, bool, (u16, u16)), Vec<(Stamp, bool)>, Stamp, Vec<Stamp>);

/// What the viewer knows of a pane: its last screen, read from the file.
#[derive(Default)]
struct Pane {
    /// The pane's `generation` file when its files were last read (none: they never were).
    seen: Option<Stamp>,
    /// That file as the last look found it, and whether its number was odd: read again when it is another.
    looked: (Stamp, bool),
    screen: Screen,
    /// The size the pane says it has (`size`), if it says one.
    size: (usize, usize),
    pid: u32,
    state: String,
    /// The addresses of the links on the screen: a cell's `link` less one is its place here.
    addresses: Vec<String>,
    /// Pictures on the screen: file, row, column, and how many rows of its pixels are above the screen.
    pictures: Vec<(String, [usize; 3])>,
    /// The size last asked for (and the cell's, in pixels), and of which holder: asked once, not fought over.
    asked: (u32, usize, usize, (u16, u16)),
    /// The title its program set (the `title` file), without control characters.
    title: String,
    /// The colour its program gave the cursor, if any (the fifth word of `cursor`).
    cursor_colour: Option<[u8; 3]>,
}

/// Where the press of a key went, if its repeats or its release have somewhere to go: to a
/// script the viewer ran for it, or to a pane (its directory and its program).
#[derive(Clone)]
enum Went {
    Script(PathBuf),
    Pane(PathBuf, NonZeroU32),
}

struct View {
    session: PathBuf,
    workspace: String,
    keys: PathBuf,
    tty: File,
    /// What goes to the terminal is written by a thread of its own, so that a terminal that
    /// reads slowly holds up the frames and not the keys: the way to it, and how much of what
    /// was sent there it has not said it has written yet.
    out: mpsc::Sender<String>,
    unwritten: usize,
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
    /// The addresses of the links of the last frame: a cell's `link` in `front` less one is its
    /// place here. Each frame makes its own, of its tiles' panes' addresses in turn.
    links: Vec<String>,
    /// The keys that are down, as far as the terminal tells, and where the press of each went:
    /// those whose press went to a script or a pane. Any other is the viewer's own to its end.
    down: Vec<((u32, u8), Went)>,
    /// If the terminal joins characters into clusters as fux does (its mode 2027): whether
    /// this viewer was the one to turn that on.
    joins: Option<bool>,
    /// The pane that was told it has the keys (it asked to be told: mode 1004).
    told_focus: Option<PathBuf>,
    /// Each pane's `notice` as it was last looked at: the file's stamp, and whether it has one
    /// this viewer has not shown on a focused pane yet. And when all were last looked at.
    notices: HashMap<String, (Stamp, bool)>,
    looked: Option<Instant>,
    /// While the terminal is still being asked what it is like: since when, and the colours it
    /// has told of.
    colours: Option<(Instant, String)>,
    /// The pictures on the terminal now.
    drawn: Vec<Drawn>,
    /// What the last frame had of each picture of its tiles, by its part: the sixel to draw (the
    /// picture's own if all of it is in the tile, else what was cut out of it) and how many
    /// pixels wide and high that is; or nothing, if nothing of it is drawn.
    parts: Vec<(Part, Sixel)>,
    cursor: String,
    /// The colour this viewer gave its terminal's cursor, if any: the focused pane's own.
    cursor_colour: Option<[u8; 3]>,
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
    pasting: Option<bool>,
    /// The key scripts that still run, each with when it started while the keys wait for it (two seconds).
    scripts: Vec<(Child, Option<Instant>)>,
    ctl_path: PathBuf,
    ctl_line: Vec<u8>,
}

/// What puts the terminal back when the viewer leaves, however it leaves short of being killed;
/// its mode is put back after it. (The cursor's shape and colour are put to the terminal's own,
/// not to what another program may have made them before.)
const LEAVING: &[u8] = b"\x1b]8;;\x1b\\\x1b[?2004l\x1b[?1004l\x1b[?9l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?1l\x1b>\x1b[0 q\x1b]112\x1b\\\x1b[m\x1b[?25h\x1b[?7h\x1b[<u\x1b[?1049l\x1b[23;2t";

pub fn view(session: PathBuf, workspace: String, keys: PathBuf) -> Result<(), String> {
    let tty = io::stdin().as_fd().try_clone_to_owned().map(File::from).map_err(|e| format!("standard input: {e}"))?;
    let size = sys::winsize(&tty).map_err(|_| "the viewer needs a terminal on its standard input")?;
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
    // (One of that name is a dead viewer's, whose number this process has now: it goes.)
    let ctl = sys::fifo(&ctl_path).map_err(|e| format!("{}: {e}", ctl_path.display()))?;
    let restore = sys::raw_mode(tty.try_clone().map_err(|e| e.to_string())?, LEAVING);
    let restore = restore.map_err(|e| format!("cannot set the terminal: {e}"))?;
    let (out, queue) = mpsc::channel::<String>();
    // (The writer says so on a pipe when it has written, or cannot: the loop waits on that as on
    // the keys, and looks no more often for a terminal that reads slowly.)
    let (wrote, mut done) = io::pipe().map_err(|e| format!("cannot make a pipe: {e}"))?;
    let writer = {
        let mut tty = tty.try_clone().map_err(|e| e.to_string())?;
        std::thread::Builder::new().spawn(move || {
            queue.into_iter().try_for_each(|text| {
                tty.write_all(text.as_bytes())?;
                done.write_all(b"w")
            })
        })
    };
    let writer = writer.map_err(|e| format!("cannot start a thread: {e}"))?;
    let mut view = View {
        session,
        workspace,
        keys,
        tty,
        out,
        unwritten: 0,
        size: Winsize { cols: size.cols.min(SIZE_MAX as u16), rows: size.rows.min(SIZE_MAX as u16), ..size },
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
        parts: Vec::new(),
        keyboard: 0,
        links: Vec::new(),
        down: Vec::new(),
        joins: None,
        told_focus: None,
        notices: HashMap::new(),
        looked: None,
        colours: Some((Instant::now(), String::new())),
        drawn: Vec::new(),
        cursor: String::new(),
        cursor_colour: None,
        modes: String::new(),
        window: true,
        title: None,
        copied: None,
        carry: Vec::new(),
        typed: Vec::new(),
        keyed: None,
        pasting: None,
        scripts: Vec::new(),
        ctl_path,
        ctl_line: Vec::new(),
    };
    // The alternate screen, pastes marked, and "what are you" (for sixel). On the way the
    // terminal is asked whether it takes the kitty keyboard protocol: flags are set with the old
    // ones kept (to give back at the end), read back, and put to none until a pane asks.
    // The terminal is asked once what its colours are (the two it draws with, its cursor's and its palette),
    // and last of all what it is: its answer to that ends the wait for the others.
    // (First a link is closed: whatever ran before may have left one open, and no cell here is part of it.)
    const START: &str = "\x1b]8;;\x1b\\\x1b[22;2t\x1b[?1049h\x1b[2J\x1b[?2004h\x1b[?7l\x1b[>31u\x1b[?u\x1b[=0u\x1b[?2027$p\x1b]10;?\x1b\\\x1b]11;?\x1b\\\x1b]12;?\x1b\\";
    let asked: String = (0..=255).map(|n| format!("\x1b]4;{n};?\x1b\\")).collect();
    view.write(START.to_string() + &asked + "\x1b[c");
    let result = view.run(&ctl, &wrote);
    // The keys leave with the viewer: a key that is down in a pane is let go there.
    view.leave(None);
    // The pane that was told it has the keys is told that it has them no more.
    view.tell(None);
    if view.joins == Some(true) {
        view.write("\x1b[?2027l".into());
    }
    // All of it is written before the terminal is put back; what could not be is why the viewer stopped.
    drop(view.out);
    let result = result.and(writer.join().unwrap_or(Ok(())));
    drop(restore);
    let _ = fs::remove_file(&view.ctl_path);
    result.map_err(|e| format!("the viewer stopped: {e}"))
}

impl View {
    /// Hands the terminal's writer something to write.
    fn write(&mut self, text: String) {
        self.unwritten += 1;
        let _ = self.out.send(text);
    }

    fn run(&mut self, ctl: &File, wrote: &io::PipeReader) -> io::Result<()> {
        let mut buffer = vec![0u8; 16 << 10];
        // When something last happened, and when the terminal last sent anything.
        let (mut active, mut heard) = (Instant::now(), Instant::now());
        loop {
            // A key's script runs before the keys typed after it, as a command
            // in a shell does: they wait until it ends (and what it printed is
            // read, and what it changed is drawn). One that takes longer than
            // two seconds is left running and the keys go on.
            self.scripts.retain_mut(|(script, _)| !matches!(script.try_wait(), Ok(Some(_))));
            for (_, since) in
                self.scripts.iter_mut().filter(|(_, since)| since.is_some_and(|at| at.elapsed() > SCRIPT_WAIT))
            {
                self.message = "a key script is still running: keys no longer wait for it".into();
                *since = None;
            }
            let n = (&*ctl).read(&mut buffer).unwrap_or(0);
            for line in crate::lines(&mut self.ctl_line, buffer.get(..n).unwrap_or(&[]), LINE_MAX) {
                match line.as_deref().map(str::trim) {
                    None => self.message = format!("a line over {LINE_MAX} bytes from a key script was dropped"),
                    Some("quit") => return Ok(()),
                    Some(line) => self.command(line),
                }
            }
            if self.frame()? || n > 0 {
                active = Instant::now();
            }
            // Files are looked at often just after something happened and seldom at rest.
            let timeout = match active.elapsed().as_millis() {
                _ if self.waiting() => 2,
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
            let wanted = [(self.tty.as_fd(), typing), (ctl.as_fd(), sys::POLLIN), (wrote.as_fd(), sys::POLLIN)];
            let [typed, _, written] = sys::wait(wanted.map(|(file, events)| (Some(file), events)), timeout)?;
            if typed != 0 {
                let n = (&self.tty).read(&mut buffer)?;
                if n == 0 {
                    return Ok(());
                }
                self.carry.extend_from_slice(buffer.get(..n).unwrap_or(&[]));
                (active, heard) = (Instant::now(), Instant::now());
            }
            // (What the writer said is taken: the frame that waited for it is next.)
            if written != 0 {
                // (The pipe's end is the writer's: it ends only when the terminal cannot be written to.)
                let n @ 1.. = (&*wrote).read(&mut buffer)? else { return Ok(()) };
                self.unwritten = self.unwritten.saturating_sub(n);
                active = Instant::now();
            }
            // The terminal is given ten seconds (a slow line) to say what it is like: after
            // that a string from it is no answer any more, and what it said is written down.
            if self.colours.as_ref().is_some_and(|(since, _)| since.elapsed() > Duration::from_secs(10)) {
                self.told();
            }
            // (Keys wait for the first look at a workspace: one with no pane gets its shell there, and they are its.)
            if !self.waiting() && !self.carry.is_empty() && self.looked.is_some() {
                // When the terminal sent nothing more for a moment, a lone ESC is the Escape key.
                self.keys(heard.elapsed() >= ESCAPE_WAIT);
            }
        }
    }

    /// Whether the keys wait: for every script started that neither ended nor ran two seconds.
    fn waiting(&self) -> bool {
        self.scripts.iter().any(|(_, since)| since.is_some())
    }

    /// Another workspace is shown: what was read of the panes is forgotten, and with it what the
    /// last frame was drawn from, so the next one is drawn (and the panes read) whatever it finds,
    /// and where its tiles were, so a click before that frame is not one on a pane of this name there.
    fn forget(&mut self) {
        self.panes.clear();
        (self.from, self.looked, self.tiles) = (None, None, Vec::new());
    }

    /// A line a script printed: a command of the viewer's, or else a message to show.
    fn command(&mut self, line: &str) {
        match line.split_once(' ').unwrap_or((line, "")) {
            // (A name that has no folder is shown too: its layout's reading says that there is none.)
            ("workspace", name) if !is_name(name) => self.message = format!("no workspace {name}"),
            ("workspace", name) if name == self.workspace => {}
            ("workspace", name) => {
                self.workspace = name.into();
                self.forget();
            }
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
        let layout = Layout::load(&self.session.join(&self.workspace)).1;
        self.focus = layout.tab().map(|tab| tab.focus.clone());
        // (And a key still down in the pane that had them is let go there, before what comes
        // of it now is looked at.)
        let has = self.focus.as_ref().map(|name| self.session.join(&self.workspace).join(name));
        self.leave(has.as_deref());
        let mut at = 0;
        while let Some(rest) = bytes.get(at..).filter(|rest| !rest.is_empty()) {
            if let Some(marked) = self.pasting {
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
                self.pasting = None;
                self.typed.extend_from_slice(if marked { END } else { &[] });
                at += END.len();
                continue;
            }
            // While the terminal is being asked what it is like, its answers may come in pieces:
            // nothing is taken for a key until it is whole, and a string is an answer.
            let asking = self.colours.is_some();
            let Some((length, name, event)) = keys::next(rest, whole && !asking, asking) else { break };
            let raw = rest.get(..length).unwrap_or(&[]);
            at += length;
            match name.split_once(':').unwrap_or((&name, "")) {
                ("paste-start", _) => {
                    // (Whether its program wants a paste marked is read when the paste begins, and kept: it ends as it began.)
                    let marked = has.as_deref().is_some_and(|pane| asks(pane).paste);
                    self.pasting = Some(marked);
                    self.typed.extend_from_slice(if marked { raw } else { &[] });
                }
                ("mouse", report) => self.mouse(report),
                ("terminal", told) => {
                    self.sixel = told.split([':', '?', ';']).any(|n| n == "4");
                    // It was asked last: the colours the terminal told of before it are all it
                    // will tell. They go into the session's `colours` file, from which every
                    // pane's program is answered; the viewer that wrote last is believed.
                    self.told();
                }
                ("answer", answer) => self.colour(answer),
                // The terminal takes the kitty keyboard protocol, and these of the flags it was asked for.
                ("keyboard", flags) => self.keyboard = flags.parse::<u8>().unwrap_or(0) & KEYS,
                // The window's own focus: for the pane that has the keys, if it asked to be told.
                ("focus", way) => {
                    // (To the pane that was told it has the keys, whichever the layout names by now, if it still asks.)
                    self.window = way == "I";
                    if let Some(pane) = self.told_focus.clone().filter(|pane| asks(pane).focus) {
                        self.send();
                        let _ = send_to(&pane.join("in"), raw);
                    }
                }
                // The terminal's answer about joining characters into clusters (mode 2027): set (1,
                // or 3 for good) or not set but known (2), which is then turned on.
                ("mode", state) if self.joins.is_none() => {
                    let state = state.strip_prefix("2027;").and_then(|state| state.parse::<u8>().ok()).unwrap_or(0);
                    self.joins = matches!(state, 1..=3).then_some(state == 2);
                    if self.joins == Some(true) {
                        self.write("\x1b[?2027h".into());
                    }
                }
                ("mode", _) => {}
                _ => self.key(&name, raw, event, &layout),
            }
            if self.waiting() || std::mem::take(&mut self.pause) {
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
            None if which == "12" => "cursor".to_string(),
            Some(("4", index)) if index.parse::<u8>().is_ok_and(|n| n.to_string() == index) => index.to_string(),
            _ => return,
        };
        // Three parts of one to four hex digits each, of which the first eight bits are kept,
        // or it is no answer; and no more is kept than was asked.
        if let (Some((_, told)), Some([r, g, b])) = (&mut self.colours, crate::term::colour(&format!("rgb:{value}")))
            && told.len() < 8 << 10
        {
            told.push_str(&format!("{name} #{r:02x}{g:02x}{b:02x}\n"));
        }
    }

    /// The terminal has said all it will of what it is like (it answered the last question, or
    /// ten seconds went by): the colours it told of go into the session's `colours` file, from
    /// which every pane's program is answered (if it told of none, the file is left as it is),
    /// and the keyboard flags it takes (none, if it did not say) into `keyboard`: a program in
    /// a pane that asks is told no more than that. Each is written whole under a name of this
    /// viewer's own and then given its name, so of two viewers the last to write is believed.
    fn told(&mut self) {
        let Some((_, told)) = self.colours.take() else { return };
        let files = [("colours", told), ("keyboard", format!("{}\n", self.keyboard))];
        for (name, text) in files.into_iter().filter(|(name, text)| *name == "keyboard" || !text.is_empty()) {
            let own = self.session.join(format!(".{name}.{}", std::process::id()));
            if let Err(e) = fs::write(&own, &text).and_then(|()| fs::rename(&own, self.session.join(name))) {
                let _ = fs::remove_file(&own);
                self.message = format!("cannot write {}: {e}", self.session.join(name).display());
            }
        }
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
                let _ = pane.map(|pane| send_to(&pane.join("in"), report.as_bytes()));
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
        // (A workspace looked at for the first time: what is there is old, and is not rung. One
        // that appears or changes after that is new, whoever brought it.)
        let first = self.notices.insert(key(""), (None, false)).is_none();
        for name in layout.tabs.iter().flat_map(|tab| tab.panes()) {
            let file = directory.join(name).join("notice");
            let named: Stamp = fs::metadata(&file).ok().filter(|m| m.is_file()).map(stamp);
            if self.notices.get(&key(name)).is_some_and(|(was, _)| *was == named) {
                continue;
            }
            // (Which notice it is is asked of the open file its words are read from; of its name where none opens.)
            let file = sys::open(&file, false);
            let stamp = file.as_ref().map_or(named, |f| f.metadata().ok().filter(|m| m.is_file()).map(stamp));
            self.notices.insert(key(name), (stamp, stamp.is_some() && self.focus.as_deref() != Some(name)));
            if file.is_ok() && stamp.is_some() && !first {
                let text = String::from_utf8_lossy(&crate::read(file, 1200).0).into_owned();
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
    fn key(&mut self, name: &str, raw: &[u8], event: Option<keys::Event>, layout: &Layout) {
        // (A key comes from a window that has the keys, whatever it last told of its focus.)
        self.window = true;
        // A program may have asked to be told when keys are let go and held (the kitty
        // protocol's flag 2), and the terminal then tells the viewer too. A key belongs, until it
        // is let go, to whoever got its press, and only a press makes it someone's: a repeat or a
        // release with no press goes nowhere. The viewer's own (the prefix, a key of a menu): its
        // repeats and its release go nowhere. A pane's: they go to that pane, whether a menu is
        // open or not. While a menu is open, a modifier key by itself is nothing.
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
                (2, Some(Went::Script(script))) => {
                    let menu = self.menu.as_ref().map(|(directory, _)| directory.as_path());
                    if menu.is_some() && menu == script.parent() && script.with_file_name(".repeat").exists() {
                        self.script(&script, layout);
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
                None
            }
            Ok(kind) if !name.is_empty() && kind.is_file() => {
                self.script(&file, layout);
                // A menu with a file `.repeat` in it stays for the next key (to resize, say), until a key it has not.
                self.menu = menu.filter(|(directory, _)| directory.join(".repeat").exists());
                Some(Went::Script(file))
            }
            _ if menu.is_none() => {
                self.typed.extend_from_slice(raw);
                self.keyed = Some(Instant::now());
                // (Its release is the pane's only if it can have one and the program there asks to be
                // told of releases now: a key typed to one that does not is typed, and no more.)
                let directory = self.session.join(&self.workspace).join(self.focus.as_deref().unwrap_or_default());
                event.and_then(|_| program(&directory)).map(|pid| Went::Pane(directory, pid))
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
                None
            }
        };
        // Which keys are down is remembered for their repeats and releases: the last 64.
        if let Some(((id, ..), went)) = event.zip(went) {
            let extra = (self.down.len() + 1).saturating_sub(64);
            self.down.drain(..extra);
            self.down.push((id, went));
        }
    }

    /// A key's repeat or release goes to the pane that got its press, after what was typed before
    /// it, if that pane's program is still the one that got it and still asks to be told of keys
    /// let go. Who a pane is, is read when it matters: nothing the viewer remembers of it is asked.
    fn pass(&mut self, directory: &Path, pid: NonZeroU32, raw: &[u8]) {
        if program(directory) == Some(pid) {
            self.send();
            let _ = send_to(&directory.join("in"), raw);
        }
    }

    /// The keys are with the pane `has` now, or with none: a key that is down in another pane is
    /// let go there, so that a press is followed by its release (the terminal, asked for the next
    /// pane's flags or left to itself, may never tell of it, or tell of it in another form), and
    /// what the terminal still tells of that key reaches no pane.
    fn leave(&mut self, has: Option<&Path>) {
        for ((code, last), went) in std::mem::take(&mut self.down) {
            match went {
                Went::Pane(directory, pid) if Some(directory.as_path()) != has => {
                    self.pass(&directory, pid, format!("\x1b[{code};1:3{}", char::from(last)).as_bytes());
                }
                went => self.down.push(((code, last), went)),
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
        // The rest of a click that only moved the focus is nobody's; a press begins a new click
        // (the release of the last may never have been told of).
        if self.held == Some(false) && !press {
            self.held = self.held.filter(|_| !release);
            return;
        }
        self.held = self.held.filter(|program| *program);
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
        let Some((name, body)) = self.tiles.iter().find(|(name, _)| self.focus.as_ref() == Some(name)) else { return };
        let term = asks(&self.session.join(&self.workspace).join(name));
        // A button held stays that program's until it is let go: a drag out of the tile is reported at its edge.
        if press || release {
            self.held = press.then_some(true);
        }
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

    fn script(&mut self, file: &Path, layout: &Layout) {
        self.send();
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
        // A pane a script starts begins at the size of the whole view, right for a new tab. The
        // size a tile would have if the focused pane were split is given too, side by side
        // (ROW) and one above the other (COL): a script that splits passes it on to `fux start`,
        // and its program is never drawn at one size and then at another.
        // (A terminal that says it has no columns or no rows has told no size, and a script is told
        // none: what it starts is as large as `fux start` makes a pane it is told no size for.)
        let now = sys::winsize(&self.tty).unwrap_or(self.size);
        let told = [("COLUMNS", now.cols.min(SIZE_MAX as u16), 80), ("LINES", now.rows.min(SIZE_MAX as u16), 24)];
        let [w, h] = told.map(|(name, size, or)| {
            let _ = if size > 0 { script.env(name, size.to_string()) } else { script.env_remove(name) };
            usize::from(if size > 0 { size } else { or })
        });
        let area = Rect { x: 0, y: 0, w, h };
        for (name, row) in [(name!("ROW"), true), (name!("COL"), false)] {
            // A pane no folder can be named after is put, in a copy of the tab shown, beside the
            // pane the script is told of, as the split keys split.
            let tile = layout.tab().zip(self.focus.as_ref()).and_then(|(tab, focus)| {
                let mut split = tab.clone();
                split.beside(focus, "\0", row);
                split.tiles(area).into_iter().find(|(pane, _)| pane == "\0").map(|(_, tile)| tile)
            });
            let tile = tile.unwrap_or(area);
            script.env(name, format!("{}x{}", tile.w, tile.h));
        }
        match script.stdin(Stdio::null()).stdout(out).stderr(err).spawn() {
            Ok(child) => self.scripts.push((child, Some(Instant::now()))),
            Err(e) => self.message = format!("{}: {e}", file.file_name().unwrap_or_default().to_string_lossy()),
        }
    }

    /// Writes what was typed to the focused pane's `in`, without waiting.
    fn send(&mut self) {
        let typed = std::mem::take(&mut self.typed);
        let Some(name) = self.focus.as_ref().filter(|_| !typed.is_empty()) else { return };
        // Resolve the name again: a live pane can have moved since the last frame.
        let path = self.session.join(&self.workspace).join(name).join("in");
        match send_to(&path, &typed) {
            Ok(n) if n == typed.len() => {}
            Ok(n) => self.message = format!("{name}: only {n} of {} typed bytes were queued", typed.len()),
            // A FIFO nobody reads, or none at all: the pane has ended.
            Err(e) if sys::no_reader(&e) || e.kind() == io::ErrorKind::NotFound => {
                self.message = format!("{name} has ended: nothing reads what you type");
            }
            Err(e) => self.message = format!("{name}: {e}: what you typed was dropped"),
        }
    }

    // ---- drawing ---------------------------------------------------------

    /// Looks at the files and draws what changed. Says whether anything did.
    fn frame(&mut self) -> io::Result<bool> {
        // One frame at a time: while the terminal is still reading the last, the next one waits
        // (and is then of what the panes show by then), and the keys do not, but for a workspace's first frame.
        if self.unwritten > 0 {
            return Ok(false);
        }
        let mut size = sys::winsize(&self.tty)?;
        // (A cell is 100 pixels at most, here as in a holder: its pictures are laid out in such cells.)
        let cell_size = ((size.xpixel / size.cols.max(1)).min(100), (size.ypixel / size.rows.max(1)).min(100));
        // A terminal larger than the largest pane is drawn in its top left corner.
        (size.cols, size.rows) = (size.cols.min(SIZE_MAX as u16), size.rows.min(SIZE_MAX as u16));
        let (w, h) = (usize::from(size.cols), usize::from(size.rows));
        let mut out = String::new();
        // (The first frame has nothing drawn before it, and draws everything as at a new size.)
        if size != self.size || self.front.len() != w * h {
            self.size = size;
            self.front = vec![Cell { ch: '\u{1}', ..Cell::blank(Style::default()) }; w * h];
            self.drawn.clear();
            out.push_str("\x1b[m\x1b[2J");
        }
        let directory = self.session.join(&self.workspace);
        let (text, mut layout, mut wrong) = Layout::load(&directory);
        // The file says what is shown: when panes came or went, it is written
        // again, so that an edit is always an edit of what is on the screen.
        if wrong.is_none() && text != layout.print() {
            match layout::command(&directory, &[]) {
                Ok(settled) => layout = settled,
                Err(why) => wrong = Some(why),
            }
        }
        let tab = layout.tab();
        // Panes get every cell but the lines between them: no titles, and no row for a bar.
        let tiles = tab.map(|tab| tab.tiles(Rect { x: 0, y: 0, w, h })).unwrap_or_default();
        self.focus = tab.map(|tab| tab.focus.clone());
        // (With no pane to look at, none was looked at last: what it copies meanwhile is old when it is come back to.)
        self.copied = self.copied.take().filter(|_| self.focus.is_some());
        let first_look = self.looked.is_none();
        // (Before its first screen is read, a pane is one blank cell with the cursor shown on it.)
        let unread = || Pane { screen: Screen::read("", 0), ..Pane::default() };
        self.notices(&layout, &directory);
        // A frame drawn from what the last one was drawn from is the last one: nothing is put
        // together and nothing compared. (A menu is drawn from more than this: with one open,
        // every frame is.)
        let panes: Vec<(Stamp, bool)> = tiles
            .iter()
            .map(|(name, _)| {
                // (Whether its holder lives is whether anything reads its `in`. While it is in the
                // middle of writing, a pane is as it was last read.)
                let held = sys::open(directory.join(name).join("in"), true).is_ok();
                let pane = self.panes.entry(name.clone()).or_insert_with(unread);
                pane.looked = generation(&directory.join(name), pane.looked);
                (pane.seen.filter(|_| pane.looked.1 && held).unwrap_or(pane.looked.0), held)
            })
            .collect();
        let clipboard = self.focus.as_ref().and_then(|name| fs::metadata(directory.join(name).join("clipboard")).ok());
        let clipboard = clipboard.map(stamp);
        let terminal = (self.sixel, self.joins.is_some(), self.keyboard, self.window, cell_size);
        let picture = |file: PathBuf| fs::metadata(file).ok().map(stamp);
        let known = tiles.iter().filter_map(|(name, _)| Some((name, self.panes.get(name)?)));
        let known = known.flat_map(|(name, pane)| pane.pictures.iter().map(move |(file, _)| (name, file)));
        let known: Vec<Stamp> = known.map(|(name, file)| picture(directory.join(name).join(file))).collect();
        let message = self.message.clone();
        let from = (layout.print(), wrong.clone(), message, terminal, panes, clipboard, known);
        if out.is_empty() && self.menu.is_none() && self.from.as_ref() == Some(&from) {
            return Ok(false);
        }
        let colour = |fg: u8, bg: Option<u8>, flags: u16| Style {
            fg: Color::Indexed(fg),
            bg: bg.map_or(Color::Default, Color::Indexed),
            ul: Color::Default,
            flags,
        };
        // What no tile covers is a line: marked here, drawn once the tiles are in place.
        let line = Cell { ch: '\u{2}', ..Cell::blank(Style::default()) };
        let mut back = vec![if tiles.is_empty() { Cell::blank(Style::default()) } else { line }; w * h];
        self.panes.retain(|name, _| layout.tabs.iter().any(|tab| tab.panes().contains(&name.as_str())));
        let (mut cursor, mut pictures): (String, Vec<Drawn>) = (String::new(), Vec::new());
        let (mut parts, mut refused, mut bytes) = (Vec::new(), Vec::<String>::new(), 0);
        let mut cursor_colour = None;
        let (mut links, mut numbers): (Vec<String>, HashMap<String, u16>) = (Vec::new(), HashMap::new());
        let mut title = None;
        let mut modes = String::from("\x1b[?9l\x1b[?1000l\x1b[?1002l\x1b[?1003l");
        let mut wants_focus = None;
        let mut keys = 0;
        for ((name, body), &(seen, held)) in tiles.iter().zip(&from.4) {
            let pane = self.panes.entry(name.clone()).or_insert_with(unread);
            let pane_directory = directory.join(name);
            pane.read(&pane_directory, seen);
            let focused = self.focus.as_ref() == Some(name);
            // Each holder is asked once to take the tile's size, and told the size of a cell in pixels.
            let asked = (pane.pid, body.w, body.h, cell_size);
            if pane.asked != asked {
                pane.asked = asked;
                let cell =
                    if cell_size.0 > 0 { format!(" {} {}", cell_size.0, cell_size.1.max(1)) } else { String::new() };
                if pane.size != (body.w, body.h) || !cell.is_empty() {
                    let resize = format!("resize {} {}{cell}\n", body.w, body.h);
                    let _ = send_to(&pane_directory.join("ctl"), resize.as_bytes());
                }
            }
            for y in 0..body.h {
                for x in 0..body.w {
                    let mut cell = pane.screen.cell_at(y, x).copied().unwrap_or(Cell::blank(Style::default()));
                    // Half a wide character at the tile's edge is drawn as a blank.
                    let cut = x + 1 == body.w && pane.screen.cell_at(y, x + 1).is_some_and(|next| next.ch == '\0');
                    cell = if cut { Cell::blank(cell.style) } else { cell };
                    // A character your terminal may draw wider than fux counts it (it may not join what
                    // fux joins) would there reach past the tile's edge: a `?` stands in each of its columns.
                    let first = x - usize::from(cell.ch == '\0' && x > 0);
                    // (A terminal that says it joins characters as fux does draws it as fux counts it.)
                    if let Some(whole) = pane.screen.cell_at(y, first).filter(|c| c.spill() > 0 && self.joins.is_none())
                        && first + 1 + usize::from(whole.wide()) + whole.spill() > body.w
                    {
                        cell = Cell { ch: '?', more: ['\0'; MORE], ..cell };
                    }
                    // A drawn link's number is its address's place in this frame's table, an address
                    // there once (past 65,535 of them, no link).
                    let address = pane.addresses.get(usize::from(cell.link).wrapping_sub(1));
                    cell.link = address.map_or(0, |address| match numbers.get(address) {
                        Some(number) => *number,
                        None => u16::try_from(links.len() + 1).map_or(0, |number| {
                            links.push(address.clone());
                            numbers.insert(address.clone(), number);
                            number
                        }),
                    });
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
            let (room, y) = (state.len().min(body.w), body.y + body.h - 1);
            put(back.get_mut(y * w..(y + 1) * w).unwrap_or_default(), body.x + body.w - room, room, &state);
            // What the focused pane's program copies goes to this terminal's clipboard, once: when its `clipboard`
            // file is another than at the last look, the pane having had the focus then too. Coming to a pane only
            // notes what is there. (The name's stamp says whether to open it. Which copy it is is asked of the open
            // file its bytes are then read from: one that lands between the two would be sent twice.)
            if focused && self.copied.as_ref().is_none_or(|(at, old)| *at != pane_directory || *old != clipboard) {
                let file = sys::open(pane_directory.join("clipboard"), false);
                let stamp: Stamp = file.as_ref().ok().and_then(|file| file.metadata().ok()).map(stamp);
                let seen = self.copied.replace((pane_directory.clone(), stamp));
                if seen.is_some_and(|(before, old)| before == pane_directory && old != stamp)
                    && let (bytes, Ok(_)) = crate::read(file, 1 << 20)
                {
                    out.push_str(&format!("\x1b]52;c;{}\x07", base64_encode(&bytes)));
                }
            }
            if focused {
                title = Some(pane.title.clone()).filter(|title| !title.is_empty());
                if pane.screen.cursor_shown && pane.screen.cursor.row < body.h && pane.screen.cursor.col < body.w {
                    let (row, col) = (body.y + pane.screen.cursor.row + 1, body.x + pane.screen.cursor.col + 1);
                    cursor = format!("\x1b[{row};{col}H\x1b[{} q\x1b[?25h", pane.screen.cursor_shape);
                }
                // The outer terminal sends cursor and keypad keys the way the focused program asked for.
                // (The cursor's colour is a living program's, as the cursor is.)
                cursor_colour = pane.cursor_colour.filter(|_| live);
                let (cursor_keys, keypad) = (pane.screen.app_cursor, pane.screen.app_keypad);
                cursor.push_str(if cursor_keys { "\x1b[?1h" } else { "\x1b[?1l" });
                cursor.push_str(if keypad { "\x1b=" } else { "\x1b>" });
                // And reports the mouse when, and as far as, a living program asked (always in the SGR form).
                // (Releases too for one that asked for presses alone, 9: a click that only moves the focus ends with one.)
                if pane.screen.mouse != 0 && live {
                    modes.push_str(&format!("\x1b[?{}h\x1b[?1006h", pane.screen.mouse.max(1000)));
                } else {
                    // (And a button held when it asks no more is let go: its release is never told of.)
                    self.held = None;
                }
                // And is told when this terminal's window gains and loses the keys, if it asked.
                if pane.screen.focus && live {
                    wants_focus = Some(pane_directory.clone());
                }
                // And tells keys as that program asked (the kitty protocol), if the terminal can.
                if live {
                    keys = pane.screen.keys() & self.keyboard;
                }
            }
            for (file, [row, col, above]) in &pane.pictures {
                // The cells the tile has for it, from its corner on, and what they are in pixels
                // of this terminal's cells: what the pane counted its cells from is not asked.
                // None is drawn on the terminal's last row: a terminal may put the cursor under a
                // picture, and scroll everything to do so.
                let below = body.h.min(h.saturating_sub(body.y + 1)).saturating_sub(*row);
                let (beside, cell) =
                    (body.w.saturating_sub(*col), (usize::from(cell_size.0), usize::from(cell_size.1)));
                if !self.sixel || !file.ends_with(".sixel") || beside == 0 || below == 0 {
                    continue;
                }
                let path = pane_directory.join(file);
                let key: Part = (path.clone(), picture(path), [*above, beside * cell.0, below * cell.1]);
                // A part of this file made before is drawn again as it was made then, and one
                // refused is not looked at again. What is refused is said in the corner, each once.
                let known = self.parts.iter().position(|(known, _)| *known == key);
                let mut part = known.map_or_else(|| part(&key), |known| Ok(self.parts.swap_remove(known).1));
                bytes += part.as_ref().ok().and_then(Option::as_ref).map_or(0, |(sixel, ..)| sixel.len());
                if bytes > FRAME_PICTURES_MAX && part.as_ref().is_ok_and(Option::is_some) {
                    part = Err("the pictures on the terminal are over 16 MiB together");
                }
                if let Err(why) = part {
                    refused.push(format!("{name}: a picture is not drawn: {why}"));
                }
                let part = part.ok().flatten();
                if let Some((_, wide, high)) = &part {
                    let cells = (wide.div_ceil(cell.0.max(1)), high.div_ceil(cell.1.max(1)));
                    pictures.push((key.clone(), body.x + col, body.y + row, cells.0, cells.1));
                }
                parts.push((key, part));
            }
        }
        (self.parts, self.from) = (parts, self.menu.is_none().then_some(from));
        if !refused.is_empty() {
            refused.dedup();
            self.message = refused.join("; ");
        }
        // The lines between panes: `│` and `─`, a tee or a cross where they
        // meet, and green where they touch the focused pane.
        let focus = tiles.iter().find(|(name, _)| self.focus.as_ref() == Some(name)).map(|(_, body)| *body);
        self.tiles = tiles;
        let lines: Vec<bool> = back.iter().map(|cell| cell.ch == '\u{2}').collect();
        let is_line = |i: usize| lines.get(i) == Some(&true);
        for (i, cell) in back.iter_mut().enumerate().filter(|(i, _)| is_line(*i)) {
            let (x, y) = (i % w, i / w);
            let (up, down) = (i >= w && is_line(i - w), is_line(i + w));
            let (left, right) = (x > 0 && is_line(i - 1), x + 1 < w && is_line(i + 1));
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
        // A workspace that has no pane when it is first shown is not left blank: the keys'
        // `.empty` is run (a new shell, by default). One whose last pane ends while it is
        // shown stays empty, and says how to go on.
        // (No pane is what the layout says, not what there was room to draw. The keys typed after
        // a script that chose this workspace wait for `.empty` too: they are its workspace's.)
        if first_look {
            let first = self.keys.join(".empty");
            if layout.tabs.is_empty() && wrong.is_none() && first.is_file() {
                self.script(&first, &layout);
            }
        }
        // (The key is named if it is there: the keys are the user's own.)
        let hint = match self.keys.join("C-b/t/n").is_file() {
            true => format!("no panes in {}: C-b t n starts a shell", self.workspace),
            false => format!("no panes in {}: keys are scripts in {}", self.workspace, self.keys.display()),
        };
        let (note, style) = match &wrong {
            Some(wrong) => (wrong, colour(9, Some(236), 0)),
            None if layout.tabs.is_empty() && self.message.is_empty() => (&hint, colour(11, Some(236), 0)),
            None => (&self.message, colour(11, Some(236), 0)),
        };
        // (The last row, the bar's and a notice's, is lent apart from the rows above it, the keys': what is given
        // one cannot write on the other, nor past a row's end.)
        let (last, mut covered) = (h.saturating_sub(1), None);
        let (above, foot) = back.split_at_mut_checked(last * w).unwrap_or_default();
        if !note.is_empty() && self.menu.is_none() {
            let note = cells(&format!(" {note} "), style);
            let room = note.len().min(w.saturating_sub(1));
            put(foot, w.saturating_sub(room + 1), room, &note);
        }
        if let Some((directory, entries)) = &self.menu {
            // While a prefix is in use: the bar over the last row (the workspace, its tabs, the keys typed so far),
            // and above it, in the corner, the keys that can follow.
            let (bar, panel) = (colour(250, Some(236), 0), colour(255, Some(238), 0));
            let path = directory.strip_prefix(&self.keys).unwrap_or(Path::new(""));
            let typed = path.iter().map(|key| key.to_string_lossy()).collect::<Vec<_>>().join(" ");
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
            let said = right.len().min(w * 3 / 4);
            foot.fill(Cell::blank(bar));
            // As many tabs as fit, the one shown among them.
            let room = w.saturating_sub(said + 2);
            let fits = |first: usize| {
                left.len() + labels.iter().take(layout.current + 1).skip(first).map(Vec::len).sum::<usize>() <= room
            };
            let first = (0..=layout.current).find(|first| fits(*first)).unwrap_or(layout.current);
            left.extend(labels.iter().skip(first).flatten());
            put(foot, 0, room, &left);
            put(foot, w.saturating_sub(said + 1), said, &right);
            // The keys, under their headings; what does not fit above the bar is counted, not shown.
            let key_width = entries.iter().map(|(key, ..)| key.chars().count()).max().unwrap_or(0);
            let (mut lines, mut heading) = (vec![(typed, BOLD)], "");
            for (key, about, group) in entries {
                if std::mem::replace(&mut heading, group) != group && !group.is_empty() {
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
            // (A box is kept, with its width, while there is room for it; the first is cut to the terminal's width.)
            let (mut right, mut boxes) = (w, Vec::new());
            for part in lines.chunks(last.max(1)) {
                let width = (part.iter().map(Vec::len).max().unwrap_or(0) + 1).min(w);
                let Some(rest) = right.checked_sub(width).filter(|_| last > 0) else { break };
                boxes.push((part, width, w - right));
                right = rest;
            }
            // The last line shown gives its place to the count, so is counted too; a count with no room is a mark.
            let over = lines.len().saturating_sub(boxes.len() * last.max(1));
            let more = format!(" ▼ {} more", over + 1);
            let fits = boxes.last().is_some_and(|(_, width, _)| more.chars().count() <= *width);
            let more = cells(if fits { &more } else { "▼" }, Style { flags: DIM, ..panel });
            let height = boxes.first().map_or(0, |(part, ..)| part.len());
            for (n, (part, width, from)) in boxes.iter().enumerate().rev() {
                for (i, row) in above.chunks_exact_mut(w.max(1)).skip(last.saturating_sub(height)).enumerate() {
                    let hides = n + 1 == boxes.len() && over > 0 && i + 1 == height;
                    let line = if hides { Some(&more) } else { part.get(i) };
                    put(row, right + from, *width, &vec![Cell::blank(panel); *width]);
                    put(row, right + from, *width, line.map_or(&[], |line| line));
                }
            }
            (cursor, cursor_colour, covered) = (String::new(), None, Some((right, last.saturating_sub(height))));
        }
        modes.push_str(if wants_focus.is_some() { "\x1b[?1004h" } else { "\x1b[?1004l" });
        self.tell(wants_focus);
        self.leave(self.focus.as_ref().map(|name| directory.join(name)).as_deref());
        if self.keyboard != 0 {
            cursor.push_str(&format!("\x1b[={keys}u"));
        }
        // A menu is drawn over the panes: a picture it covers a cell of is not drawn while it is open.
        pictures.retain(|p| !covered.is_some_and(|(left, top)| p.1 + p.3 > left && p.2 + p.4 > top));

        // Pictures are pixels over cells: when one comes, goes or is written
        // over, the cells under the old ones are sent again and all are redrawn.
        let mut redraw = pictures != self.drawn;
        if redraw {
            for (_, x, y, cols, rows) in &self.drawn {
                for cell in (0..*rows).flat_map(|r| (0..*cols).map(move |c| (y + r) * w + x + c)) {
                    self.front.get_mut(cell).into_iter().for_each(|cell| cell.ch = '\u{1}');
                }
            }
        }
        let mut style = None;
        let mut at = None;
        // The link the terminal has open (OSC 8) is changed here alone, and only to another.
        let mut pen = 0;
        let mut link = |out: &mut String, next: u16| {
            if std::mem::replace(&mut pen, next) != next {
                out.extend(["\x1b]8;;", address(&links, next).unwrap_or_default(), "\x1b\\"]);
            }
        };
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
            // (A link is the same link if its address is: each frame numbers its own.)
            let same = Cell { link: 0, ..*new } == Cell { link: 0, ..*old }
                && address(&links, new.link) == address(&self.links, old.link);
            *old = *new;
            if same && i >= again {
                continue;
            }
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
                link(&mut out, 0);
                out.push_str(&place);
            }
            if style != Some(new.style) {
                new.style.sgr(&mut out);
                style = Some(new.style);
            }
            if apart {
                out.push('\u{200b}');
            }
            // A wide character that may come out narrower (one of several parts, an emoji) is
            // drawn over two blanks, of its link as it is (closed again before the cursor goes
            // back over them); the cells one may come out over are sent after it.
            if new.wide() && !new.plain() {
                link(&mut out, new.link);
                out.push_str("  ");
                link(&mut out, 0);
                out.push_str(&place);
            }
            // A cell that is part of a link is drawn inside it (OSC 8): the link is opened before
            // the first such cell and closed before the next cell that is not part of it, so
            // none reaches past its tile or over a line between tiles.
            link(&mut out, new.link);
            out.extend(new.chars());
            again = again.max(i + 1 + usize::from(new.wide()) + new.spill());
            at = ascii.then_some(i + 1);
        }
        link(&mut out, 0);
        self.links = links;
        if redraw {
            for (key, x, y, ..) in &pictures {
                if let Some((_, Some((sixel, ..)))) = self.parts.iter().find(|(known, _)| known == key) {
                    out.push_str(&format!("\x1b[{};{}H\x1bP{sixel}\x1b\\", y + 1, x + 1));
                }
            }
            self.drawn = pictures;
        }
        // This terminal's title is the focused pane's; with none, it is what it was before the viewer.
        if title != self.title {
            out.push_str(&title.as_ref().map_or("\x1b[23;2t\x1b[22;2t".into(), |title| format!("\x1b]2;{title}\x07")));
            self.title = title;
        }
        // This terminal's cursor has the colour the focused pane's program set (OSC 12) while that
        // pane has the keys and its program lives, and its own again (OSC 112) when not. No other colour
        // of this terminal is ever set: what a pane's program set is in the pane's cells.
        if cursor_colour != self.cursor_colour {
            let own = |[r, g, b]: [u8; 3]| format!("\x1b]12;#{r:02x}{g:02x}{b:02x}\x1b\\");
            out.push_str(&cursor_colour.map_or("\x1b]112\x1b\\".into(), own));
            self.cursor_colour = cursor_colour;
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
    (0..row.screen().cursor.col).filter_map(|x| row.screen().cell_at(0, x).copied()).collect()
}

/// The address of a link of the given number in a frame's table of them.
fn address(table: &[String], number: u16) -> Option<&str> {
    table.get(usize::from(number).checked_sub(1)?).map(String::as_str)
}

/// Puts cells on a row from column `x`, at most `room` of them and none past
/// the row's end, which is all it can reach; a wide character that would be cut is a blank.
fn put(row: &mut [Cell], x: usize, room: usize, cells: &[Cell]) {
    let room = room.min(row.len().saturating_sub(x));
    // What is written over the right half of a wide character takes its left half with it.
    if room > 0
        && row.get(x).is_some_and(|c| c.ch == '\0')
        && let Some(left) = row.get_mut(x.wrapping_sub(1))
    {
        *left = Cell::blank(left.style);
    }
    for (i, (slot, cell)) in row.iter_mut().skip(x).zip(cells).take(room).enumerate() {
        let cut = i + 1 == room && cells.get(i + 1).is_some_and(|next| next.ch == '\0');
        *slot = if cut { Cell::blank(cell.style) } else { *cell };
    }
}

/// Writes to a pane's FIFO (`in`, `ctl`) without waiting.
fn send_to(path: &Path, bytes: &[u8]) -> io::Result<usize> {
    sys::open(path, true)?.write(bytes)
}

/// How a pane's program asks for its input (pastes marked, the mouse, the focus, keys), as its
/// `modes` file says now: what is typed is sent by this, not by what was last drawn.
fn asks(directory: &Path) -> Screen {
    let mut screen = Screen::default();
    screen.set_words(&text(&directory.join("modes"), 256));
    screen
}

/// Who a pane's program is: the pid in its `status`, as the file says now.
fn pid(directory: &Path) -> u32 {
    text(&directory.join("status"), 256).lines().find_map(|line| line.strip_prefix("pid ")?.parse().ok()).unwrap_or(0)
}

/// Who a pane's program is, if it asks to be told of keys let go and held (flag 2 of the `keys`
/// word in `modes`), as the files say now: a pane with no program has none to tell.
fn program(directory: &Path) -> Option<NonZeroU32> {
    NonZeroU32::new(pid(directory)).filter(|_| asks(directory).keys() & 2 != 0)
}

/// A pane's `generation` as a look finds it: which file, and whether its number is odd (its holder is
/// writing), which is read when the file is another than the one `known`.
fn generation(directory: &Path, known: (Stamp, bool)) -> (Stamp, bool) {
    let stamp = fs::metadata(directory.join("generation")).ok().filter(fs::Metadata::is_file).map(stamp);
    let odd = || text(&directory.join("generation"), 32).trim().parse::<u64>().is_ok_and(|number| number % 2 == 1);
    (stamp, if stamp == known.0 { known.1 } else { odd() })
}

impl Pane {
    /// Reads the pane if its `generation` file changed (`seen`: the file as the frame found it; a holder
    /// writes it last). What was read is kept only if it is the same file afterwards: then every file is of
    /// one round. (In a folder made by hand they may disagree: the rows are the lines there are, the cursor inside.)
    fn read(&mut self, directory: &Path, seen: Stamp) {
        if self.seen == Some(seen) {
            return;
        }
        let last = std::mem::replace(self, Pane { seen: Some(seen), asked: self.asked, ..Pane::default() });
        let fact = |name: &str, most: u64| text(&directory.join(name), most);
        let numbers = |text: &str| {
            text.split_whitespace().map(|n| n.parse::<usize>().unwrap_or(SIZE_MAX + 1)).collect::<Vec<_>>()
        };
        let styled = fact("shown/screen.styled", STYLED_MAX);
        self.size = match numbers(&fact("size", 64)).as_slice() {
            [cols @ 1..=SIZE_MAX, rows @ 1..=SIZE_MAX] => (*cols, *rows),
            _ => (0, 0),
        };
        let cols = if self.size.0 == 0 { SIZE_MAX } else { self.size.0 };
        self.screen = Screen::read(&styled, cols);
        self.screen.set_words(&fact("modes", 256));
        let cursor = fact("cursor", 64);
        let mut cursor = cursor.split_whitespace();
        let mut place = || cursor.next().and_then(|n| n.parse::<usize>().ok());
        let at = (place(), place());
        self.screen.cursor_shown = cursor.next() == Some("shown") && at.0.is_some_and(|row| row < self.screen.rows);
        (self.screen.cursor.row, self.screen.cursor.col) =
            (at.0.unwrap_or(0).min(self.screen.rows - 1), at.1.unwrap_or(0).min(cols - 1));
        self.screen.cursor_shape =
            cursor.next().and_then(|shape| shape.parse().ok()).filter(|shape| *shape <= 6).unwrap_or(0);
        self.cursor_colour = cursor.next().filter(|colour| colour.len() == 7).and_then(crate::term::colour);
        self.state = fact("state", 64).trim_end().to_string();
        self.pid = pid(directory);
        // A picture is kept only if it lies on a screen of the largest size there is. A first row
        // below 0 says how many of its rows are above the screen, each as high as the line's last
        // number says (the cells it was laid out in, not this terminal's: a line that does not say is none).
        for line in fact("shown/pictures", 4096).lines() {
            let mut words = line.split_whitespace();
            let Some(name) = words.next().filter(|name| is_name(name)) else { continue };
            let mut cell = || words.next()?.parse::<i16>().ok().filter(|n| usize::from(n.unsigned_abs()) <= SIZE_MAX);
            let (Some(row), Some(col @ 0..)) = (cell(), cell()) else { continue };
            let high = words.nth(3).and_then(|high| high.parse().ok()).filter(|high| (1..=100).contains(high));
            let Some(high) = high.or((row >= 0).then_some(0)) else { continue };
            let above = usize::from(row.min(0).unsigned_abs()) * high;
            self.pictures.push((name.into(), [row.max(0) as usize, col as usize, above]));
        }
        // The links (`links`: line, cells into it, how many cells, address) are put on the cells.
        // `lines` says which line each row is of (the first row's, then the rows that go on); a
        // row starts as many cells into its line as the rows of that line above it hold. An
        // address is plain printable text.
        let lines = fact("shown/lines", 4096);
        let mut lines = lines.lines().map(&numbers);
        let (top, rows) = (lines.next().unwrap_or_default(), lines.next().unwrap_or_default());
        let (mut line, mut base, mut starts) = (top.first().copied().unwrap_or(0), 0, Vec::new());
        for row in 0..self.screen.rows {
            (line, base) = if row > 0 && !rows.contains(&row) { (line.saturating_add(1), 0) } else { (line, base) };
            starts.push((line, base));
            base += cols;
        }
        // (Whole records only: a list cut by the bound ends before its last line's end. A thousand
        // addresses, as a holder keeps, and no more cells than the screen has.)
        let list = fact("shown/links", LINKS_LIST_MAX as u64);
        let whole = list.rfind('\n').and_then(|end| list.get(..end)).unwrap_or_default();
        let (mut known, mut cells): (HashMap<&str, u16>, usize) = (HashMap::new(), self.screen.rows * cols);
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
                    self.screen.set_link(row, col - base, link);
                    cells -= 1;
                }
            }
        }
        let title = fact("title", 1024);
        self.title = title.lines().next().unwrap_or_default().chars().filter(|c| !c.is_control()).take(256).collect();
        if generation(directory, last.looked).0 != seen {
            *self = last;
        }
    }
}

/// The entries of a menu: each key in the directory and, from its script's
/// second line, what it does; a third line `# group: NAME` names the heading
/// it goes under. A key that opens another menu has `…`, and its words and
/// group are in that directory's file `.about`. Sorted by heading, then key;
/// keys with no heading first. At most 200.
fn menu_entries(directory: &Path) -> Vec<Entry> {
    let names = fs::read_dir(directory).into_iter().flatten().flatten();
    let mut names = names.filter_map(|entry| entry.file_name().into_string().ok()).filter(|n| !n.starts_with('.'));
    let describe = |name: String| {
        let path = directory.join(&name);
        // A script's lines after `#!`: `# caption`, `# group: NAME`. A directory's `.about`: `caption`, `# group: NAME`.
        let (file, before, menu) = if path.is_dir() { (path.join(".about"), "\n# ", "…") } else { (path, "", "") };
        let text = format!("{before}{}", text(&file, 1024));
        let mut lines = text.lines().skip(1).map(|line| line.strip_prefix("# ").unwrap_or_default());
        let about = format!("{}{menu}", lines.next().unwrap_or_default());
        (name, about, lines.next().and_then(|line| line.strip_prefix("group: ")).unwrap_or_default().to_string())
    };
    let mut entries: Vec<Entry> = names.by_ref().take(MENU_MAX).map(describe).collect();
    entries.sort_by(|a, b| (&a.2, &a.0).cmp(&(&b.2, &b.0)));
    entries.extend(names.next().map(|_| ("…".to_string(), "more keys than a menu lists".to_string(), String::new())));
    entries
}
