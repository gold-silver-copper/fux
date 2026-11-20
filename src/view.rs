//! The viewer: one more client. It draws a workspace's panes from their
//! `screen.styled` files as the `layout` file places them, sends what is
//! typed to the focused pane's `in`, and runs a script for a key that has
//! one. It keeps nothing the files do not say, except which workspace it
//! shows.

use crate::layout::{self, Layout, Rect};
use crate::sys::{self, Winsize};
use crate::term::{BOLD, Cell, Color, DIM, PICTURE_MAX, SIZE_MAX, Style, Term};
use crate::width::width;
use crate::{is_name, keys};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, AsRawFd};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Instant;

/// The most key scripts running at once; one more is refused with a message.
const SCRIPTS_MAX: usize = 32;
const LINE_MAX: usize = 4096;
/// The most keys a menu lists.
const MENU_MAX: usize = 200;
/// More than the largest screen a holder writes: 500 by 500 cells, each with its own colours.
const STYLED_MAX: u64 = 32 << 20;
const TYPED_MAX: usize = 64 << 10;
const SCRIPT_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// A key of a menu: its name, what it does, and the heading it is listed under (or none).
type Entry = (String, String, String);

type Drawn = (PathBuf, usize, usize, usize, usize, u32);

/// What the viewer knows of a pane: its last screen, read from the file.
struct Pane {
    /// The file's inode, time and length when it was last read.
    seen: Option<(u64, i64, i64, u64)>,
    term: Term,
    pid: u32,
    state: String,
    /// Pictures on the screen: file, row, column, columns, rows.
    pictures: Vec<(String, usize, usize, usize, usize)>,
    /// The size last asked for (and the cell's, in pixels), and of which holder: asked once, not fought over.
    asked: (u32, usize, usize, (u16, u16)),
}

struct View {
    session: PathBuf,
    workspace: String,
    /// The workspace shown before this one: this viewer's own memory, in no file.
    before: Option<String>,
    keys: PathBuf,
    tty: File,
    size: Winsize,
    /// What the terminal shows now, to send only what changes.
    front: Vec<Cell>,
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
    /// The pictures on the terminal now: file, column, row, columns, rows, and whose holder's.
    drawn: Vec<Drawn>,
    cursor: String,
    carry: Vec<u8>,
    typed: Vec<u8>,
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
        let _ = self.0.write_all(b"\x1b[?2004l\x1b[?9l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?1l\x1b>\x1b[0 q\x1b[m\x1b[?25h\x1b[?1049l");
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
    sys::make_fifo(&ctl_path).map_err(|e| format!("{}: {e}", ctl_path.display()))?;
    let open = |write: bool| OpenOptions::new().read(!write).write(write).custom_flags(sys::O_NONBLOCK).open(&ctl_path);
    let ctl = open(false).map_err(|e| format!("{}: {e}", ctl_path.display()))?;
    let ctl_writer = open(true).map_err(|e| format!("{}: {e}", ctl_path.display()))?;
    let saved = sys::raw_mode(&tty).map_err(|e| format!("cannot set the terminal: {e}"))?;
    let restore = Restore(tty.try_clone().map_err(|e| e.to_string())?, saved);
    size.rows = 0; // the first frame sees a new size and draws everything
    let mut view = View {
        session,
        workspace,
        before: None,
        keys,
        tty,
        size,
        front: Vec::new(),
        panes: HashMap::new(),
        focus: None,
        tiles: Vec::new(),
        held: None,
        pause: false,
        menu: None,
        message: String::new(),
        sixel: false,
        drawn: Vec::new(),
        cursor: String::new(),
        carry: Vec::new(),
        typed: Vec::new(),
        pasting: false,
        scripts: Vec::new(),
        waiting: None,
        ctl_path,
        _ctl_writer: ctl_writer,
        ctl_line: Vec::new(),
        quit: false,
    };
    // The alternate screen, pastes marked, and "what are you" (for sixel).
    let result = view.tty.write_all(b"\x1b[?1049h\x1b[?2004h\x1b[c").and_then(|()| view.run(&ctl));
    drop(restore);
    let _ = fs::remove_file(&view.ctl_path);
    result.map_err(|e| format!("the viewer stopped: {e}"))
}

impl View {
    fn run(&mut self, ctl: &File) -> io::Result<()> {
        let mut buffer = vec![0u8; 16 << 10];
        let mut active = Instant::now();
        while !self.quit {
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
                0..300 => 4,
                300..3000 => 16,
                _ => 50,
            };
            // Typed bytes are not read while 64 KiB of them wait for a script.
            let typing = if self.carry.len() < TYPED_MAX { sys::POLLIN } else { 0 };
            let wanted = [(self.tty.as_raw_fd(), typing), (ctl.as_raw_fd(), sys::POLLIN)];
            let events = sys::wait(&wanted, timeout)?;
            if events.first().is_some_and(|e| *e != 0) {
                let n = (&self.tty).read(&mut buffer)?;
                if n == 0 {
                    return Ok(());
                }
                self.carry.extend_from_slice(buffer.get(..n).unwrap_or(&[]));
                active = Instant::now();
            }
            if self.waiting.is_none() && !self.carry.is_empty() {
                // When nothing at all happened for a moment, a lone ESC is the Escape key.
                self.keys(events.iter().all(|e| *e == 0));
            }
        }
        Ok(())
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
                    self.panes.clear();
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
        let wants_paste = self.focus.as_ref().and_then(|f| self.panes.get(f)).is_some_and(|pane| pane.term.paste);
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
            let Some((length, name)) = keys::next(rest, whole) else { break };
            let raw = rest.get(..length).unwrap_or(&[]);
            at += length;
            match name.as_str() {
                "paste-start" => {
                    self.pasting = true;
                    self.typed.extend_from_slice(if wants_paste { raw } else { &[] });
                }
                name if name.starts_with("mouse:") => self.mouse(name.trim_start_matches("mouse:")),
                name if name.starts_with("terminal:") => self.sixel = name.split([':', '?', ';']).any(|n| n == "4"),
                name => self.key(name, raw),
            }
            if self.waiting.is_some() || std::mem::take(&mut self.pause) {
                break;
            }
        }
        self.carry = bytes.get(at..).unwrap_or(&[]).to_vec();
        self.send();
    }

    /// One key: a script if the key's file is one, a menu if it is a
    /// directory, and otherwise the key goes to the pane.
    fn key(&mut self, name: &str, raw: &[u8]) {
        self.message.clear();
        let menu = self.menu.take();
        let file = menu.as_ref().map_or(&self.keys, |(directory, _)| directory).join(name);
        match fs::metadata(&file) {
            Ok(kind) if !name.is_empty() && kind.is_dir() => self.menu = Some((file.clone(), menu_entries(&file))),
            Ok(kind) if !name.is_empty() && kind.is_file() => {
                self.send();
                self.script(&file);
                // A menu with a file `.repeat` in it stays for the next key (to resize, say), until a key it has not.
                self.menu = menu.filter(|(directory, _)| directory.join(".repeat").exists());
            }
            _ if menu.is_none() => self.typed.extend_from_slice(raw),
            _ => {}
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
        let focused = self.tiles.iter().find(|(name, _)| self.focus.as_ref() == Some(name));
        let Some((term, body)) = focused.and_then(|(name, body)| Some((&self.panes.get(name)?.term, *body))) else {
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
        // A pane a script starts begins at the size of the whole view, right for a new tab.
        script.env("COLUMNS", self.size.cols.to_string()).env("LINES", self.size.rows.to_string());
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
            Err(e) => self.message = format!("{name}: {e}: what you typed was dropped"),
        }
    }

    // ---- drawing ---------------------------------------------------------

    /// Looks at the files and draws what changed. Says whether anything did.
    fn frame(&mut self) -> io::Result<bool> {
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
                self.panes.clear();
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
        let colour = |fg: u8, bg: Option<u8>, flags: u16| Style {
            fg: Color::Indexed(fg),
            bg: bg.map_or(Color::Default, Color::Indexed),
            flags,
        };
        // What no tile covers is a line: marked here, drawn once the tiles are in place.
        let line = Cell { ch: '\u{2}', ..Cell::blank(Style::default()) };
        let mut back = vec![if tiles.is_empty() { Cell::blank(Style::default()) } else { line }; w * h];
        self.focus = tab.map(|tab| tab.focus.clone());
        self.panes.retain(|name, _| layout.tabs.iter().any(|tab| tab.panes().contains(&name.as_str())));
        self.tiles = tiles.clone();
        let (mut cursor, mut pictures): (String, Vec<Drawn>) = (String::new(), Vec::new());
        let mut modes = String::from("\x1b[?9l\x1b[?1000l\x1b[?1002l\x1b[?1003l");
        for (name, body) in &tiles {
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
                if (pane.term.cols, pane.term.rows) != (body.w, body.h) || !cell.is_empty() {
                    let _ = ctl
                        .and_then(|mut ctl| ctl.write_all(format!("resize {} {}{cell}\n", body.w, body.h).as_bytes()));
                }
            }
            for y in 0..body.h {
                for x in 0..body.w {
                    let mut cell = pane.term.cell_at(y, x).copied().unwrap_or(Cell::blank(Style::default()));
                    // Half a wide character at the tile's edge is drawn as a blank.
                    let cut = x + 1 == body.w && pane.term.cell_at(y, x + 1).is_some_and(|next| next.ch == '\0');
                    cell.ch = if cut { ' ' } else { cell.ch };
                    if let Some(slot) = back.get_mut((body.y + y) * w + body.x + x) {
                        *slot = cell;
                    }
                }
            }
            // A pane whose program ended, or whose holder is gone, says so in its last row's corner.
            let live = pane.state == "running"
                && OpenOptions::new()
                    .read(true)
                    .custom_flags(sys::O_NONBLOCK)
                    .open(&pane_directory)
                    .is_ok_and(|dir| matches!(dir.try_lock_shared(), Err(fs::TryLockError::WouldBlock)));
            let state = match (live, pane.state.as_str()) {
                (true, "running") => String::new(),
                (false, "running") => " lost ".into(),
                (_, "") => " no pane here ".into(),
                (_, state) => format!(" {state} "),
            };
            let state = cells(&state, colour(11, Some(236), 0));
            let room = state.len().min(body.w);
            put(&mut back, w, body.x + body.w - room, body.y + body.h - 1, room, &state);
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
            }
            for (file, row, col, cols, rows) in &pane.pictures {
                if self.sixel && file.ends_with(".sixel") && row + rows <= body.h && col + cols <= body.w {
                    pictures.push((pane_directory.join(file), body.x + col, body.y + row, *cols, *rows, pane.pid));
                }
            }
        }
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
            *cell = Cell { ch, marks: ['\0'; 2], style: colour(if near { 2 } else { 240 }, None, 0) };
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
                cells(&format!(" {} ", tab.focus), if i == layout.current { colour(0, Some(2), BOLD) } else { bar })
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
        cursor.push_str(&modes);
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
        for (i, (new, old)) in back.iter().zip(self.front.iter_mut()).enumerate() {
            if new == old {
                continue;
            }
            *old = *new;
            let (x, y) = (i % w.max(1), i / w.max(1));
            redraw |= pictures.iter().any(|p| (p.1..p.1 + p.3).contains(&x) && (p.2..p.2 + p.4).contains(&y));
            if new.ch == '\0' {
                continue;
            }
            if at != Some(i) {
                out.push_str(&format!("\x1b[{};{}H", y + 1, x + 1));
            }
            if style != Some(new.style) {
                new.style.sgr(&mut out);
                style = Some(new.style);
            }
            new.push_text(&mut out);
            // After anything but plain ASCII, the terminal's idea of the column may not be ours.
            at = new.ch.is_ascii().then_some(i + 1);
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
        if out.is_empty() && cursor == self.cursor {
            return Ok(false);
        }
        self.cursor = cursor;
        self.tty.write_all(format!("\x1b[?2026h\x1b[?25l{out}\x1b[m{}\x1b[?2026l", self.cursor).as_bytes())?;
        Ok(true)
    }
}

/// Text of the viewer's own (a title, the last line, a menu) as cells: a
/// control character becomes `?`, a wide character takes its two cells, and
/// one that takes none is left out. So nothing from a file's name or a
/// script's words reaches the terminal as anything but text.
fn cells(text: &str, style: Style) -> Vec<Cell> {
    let mut cells = Vec::new();
    for ch in text.chars().map(|c| if c.is_control() { '?' } else { c }) {
        let wide = match width(ch) {
            0 => continue,
            cells => cells == 2,
        };
        cells.push(Cell { ch, marks: ['\0'; 2], style });
        cells.extend(wide.then_some(Cell { ch: '\0', marks: ['\0'; 2], style }));
    }
    cells
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
        left.ch = ' ';
    }
    for (i, cell) in cells.iter().enumerate().take(room) {
        let cut = i + 1 == room && cells.get(i + 1).is_some_and(|next| next.ch == '\0');
        if let Some(slot) = back.get_mut(y * w + x + i) {
            *slot = if cut { Cell { ch: ' ', ..*cell } } else { *cell };
        }
    }
}

impl Pane {
    fn new() -> Pane {
        Pane {
            seen: None,
            term: Term::new(1, 1),
            pid: 0,
            state: String::new(),
            pictures: Vec::new(),
            asked: (0, 0, 0, (0, 0)),
        }
    }

    /// Reads the pane's screen if its file changed.
    fn read(&mut self, directory: &Path) {
        let file = directory.join("screen.styled");
        let seen = fs::metadata(&file).ok().map(|m| (m.ino(), m.mtime(), m.mtime_nsec(), m.len()));
        if seen == self.seen {
            return;
        }
        self.seen = seen;
        // Opened without waiting (should the name be a FIFO), and read up to the largest screen there is.
        let mut bytes = Vec::new();
        let file = OpenOptions::new().read(true).custom_flags(sys::O_NONBLOCK).open(&file);
        let _ = file.map(|file| file.take(STYLED_MAX).read_to_end(&mut bytes));
        self.pictures.clear();
        self.state.clear();
        let (pid, state, pictures) = (&mut self.pid, &mut self.state, &mut self.pictures);
        self.term = Term::from_screen(&String::from_utf8_lossy(&bytes), |key, value| {
            let numbers: Vec<usize> = value.split(['@', ',', '+', 'x']).filter_map(|n| n.parse().ok()).collect();
            match (key, numbers.as_slice()) {
                ("pid", [n]) => *pid = *n as u32,
                ("state", _) => *state = value.replace(':', " "),
                // A picture is kept only if it lies on a screen of the largest size there is.
                ("pic", [row, col, cols, rows]) if numbers.iter().all(|n| *n <= SIZE_MAX) => {
                    let name = value.split('@').next().unwrap_or_default();
                    if is_name(name) {
                        pictures.push((name.into(), *row, *col, *cols, *rows));
                    }
                }
                _ => {}
            }
        });
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
