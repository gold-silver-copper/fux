//! The viewer: one more client. It draws a workspace's panes from their
//! `screen.styled` files as the `layout` file places them, sends what is
//! typed to the focused pane's `in`, and runs a script for a key that has
//! one. It keeps nothing the files do not say, except which workspace it
//! shows.

use crate::layout::{self, Layout, Rect};
use crate::sys::{self, Winsize};
use crate::term::{BOLD, Cell, PICTURE_MAX, REVERSE, SIZE_MAX, Style, Term};
use crate::width::width;
use crate::{is_name, keys};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, AsRawFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
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
    /// `in`, open for writing while a holder reads it.
    input: Option<File>,
}

struct View {
    session: PathBuf,
    workspace: String,
    keys: PathBuf,
    tty: File,
    size: Winsize,
    /// What the terminal shows now, to send only what changes.
    front: Vec<Cell>,
    panes: HashMap<String, Pane>,
    focus: Option<String>,
    /// The open menu: its directory and its entries.
    menu: Option<(PathBuf, Vec<String>)>,
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
        let _ = self.0.write_all(b"\x1b[?2004l\x1b[?1l\x1b>\x1b[0 q\x1b[m\x1b[?25h\x1b[?1049l");
        let _ = sys::restore_mode(&self.0, &self.1);
    }
}

pub fn view(session: PathBuf, workspace: String, keys: PathBuf) -> Result<(), String> {
    let tty = io::stdin().as_fd().try_clone_to_owned().map(File::from).map_err(|e| format!("standard input: {e}"))?;
    let mut size = sys::winsize(&tty).map_err(|_| "the viewer needs a terminal on its standard input")?;
    fs::create_dir_all(session.join(&workspace)).map_err(|e| format!("{}: {e}", session.display()))?;
    if !keys.exists() {
        for (name, script) in keys::DEFAULT {
            let path = keys.join(name);
            let made = path.parent().map_or(Ok(()), fs::create_dir_all).and_then(|()| fs::write(&path, script));
            let made = made.and_then(|()| fs::set_permissions(&path, fs::Permissions::from_mode(0o755)));
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
        keys,
        tty,
        size,
        front: Vec::new(),
        panes: HashMap::new(),
        focus: None,
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
            let mut wanted = vec![(self.tty.as_raw_fd(), typing), (ctl.as_raw_fd(), sys::POLLIN)];
            let writers: Vec<(String, i32)> = self
                .panes
                .iter()
                .filter_map(|(name, pane)| Some((name.clone(), pane.input.as_ref()?.as_raw_fd())))
                .collect();
            wanted.extend(writers.iter().map(|(_, fd)| (*fd, 0)));
            let events = sys::wait(&wanted, timeout)?;
            // A writer with an event has lost its reader: the holder is gone.
            for ((name, _), _) in writers.iter().zip(events.iter().skip(2)).filter(|(_, event)| **event != 0) {
                self.panes.remove(name);
                active = Instant::now();
            }
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
            ("workspace", name) if is_name(name) && self.session.join(name).is_dir() => {
                self.workspace = name.into();
                self.panes.clear();
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
                name if name.starts_with("terminal:") => self.sixel = name.split([':', '?', ';']).any(|n| n == "4"),
                name => self.key(name, raw),
            }
            if self.waiting.is_some() {
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
            }
            _ if menu.is_none() => self.typed.extend_from_slice(raw),
            _ => {}
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
        script.env("FUX3C_DIR", &self.session).env("FUX3C_WS", &self.workspace).env("FUX3C_KEYS", &self.keys);
        script.env("FUX3C_PANE", pane.unwrap_or_default()).env("FUX3C_VIEW", &self.ctl_path).env("PATH", path);
        // A pane a script starts begins at the size of the whole view, right for a new tab.
        script.env("COLUMNS", self.size.cols.to_string()).env("LINES", self.size.rows.saturating_sub(1).to_string());
        match script.stdin(Stdio::null()).stdout(out).stderr(err).spawn() {
            Ok(child) => {
                self.waiting = Some((child.id(), Instant::now()));
                self.scripts.push(child);
            }
            Err(e) => self.message = format!("{}: {e}", file.display()),
        }
    }

    /// Writes what was typed to the focused pane's `in`, without waiting.
    fn send(&mut self) {
        let typed = std::mem::take(&mut self.typed);
        let Some((name, pane)) = self.focus.as_ref().and_then(|f| Some((f, self.panes.get_mut(f)?))) else { return };
        if typed.is_empty() {
            return;
        }
        match pane.input.as_ref().map(|mut input| input.write(&typed)) {
            Some(Ok(n)) if n == typed.len() => {}
            Some(Ok(_)) | Some(Err(_)) => self.message = format!("{name} is not reading: what you typed was dropped"),
            None => self.message = format!("{name} has ended: nothing reads what you type"),
        }
    }

    // ---- drawing ---------------------------------------------------------

    /// Looks at the files and draws what changed. Says whether anything did.
    fn frame(&mut self) -> io::Result<bool> {
        let mut size = sys::winsize(&self.tty)?;
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
        let directory = self.session.join(&self.workspace);
        let (text, mut layout, mut wrong) = Layout::load(&directory);
        // The file says what is shown: when panes came or went, it is written
        // again, so that an edit is always an edit of what is on the screen.
        if wrong.is_none() && text != layout.print() {
            wrong = layout::command(&directory, &[]).err();
            layout = Layout::load(&directory).1;
        }
        let tab = layout.tab();
        let tiles = tab.map(|tab| tab.tiles(Rect { x: 0, y: 0, w, h: h.saturating_sub(1) })).unwrap_or_default();
        // What no tile covers is the line between the parts of a row.
        let line = Cell { ch: if tiles.is_empty() { ' ' } else { '│' }, ..Cell::blank(Style::default()) };
        let mut back = vec![line; w * h];
        self.focus = tab.map(|tab| tab.focus.clone());
        self.panes.retain(|name, _| layout.tabs.iter().any(|tab| tab.panes().contains(&name.as_str())));
        let (mut cursor, mut pictures): (String, Vec<Drawn>) = (String::new(), Vec::new());
        let cell_size = (size.xpixel / size.cols.max(1), size.ypixel / size.rows.max(1));
        for (name, rect) in &tiles {
            let pane = self.panes.entry(name.clone()).or_insert_with(Pane::new);
            let pane_directory = directory.join(name);
            pane.read(&pane_directory);
            let focused = self.focus.as_ref() == Some(name);
            let state = match (pane.input.is_some(), pane.state.as_str()) {
                (true, "running") => String::new(),
                (false, "running") => " [lost]".into(),
                (_, "") => " [no pane here]".into(),
                (_, state) => format!(" [{state}]"),
            };
            // A title when there is more than one tile, or something to say.
            let titled = usize::from(tiles.len() > 1 || !state.is_empty());
            let body = Rect { y: rect.y + titled, h: rect.h.saturating_sub(titled), ..*rect };
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
            if titled == 1 {
                let style = Style { flags: if focused { REVERSE | BOLD } else { 0 }, ..Style::default() };
                let fill = if focused { " " } else { "─" };
                put(
                    &mut back,
                    w,
                    rect.x,
                    rect.y,
                    rect.w,
                    &cells(&format!("{fill} {name}{state} {}", fill.repeat(rect.w)), style),
                );
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
            if focused && pane.term.cursor_shown && pane.term.row < body.h && pane.term.col < body.w {
                let (row, col) = (body.y + pane.term.row + 1, body.x + pane.term.col + 1);
                cursor = format!("\x1b[{row};{col}H\x1b[{} q\x1b[?25h", pane.term.cursor_shape);
            }
            // The outer terminal sends cursor and keypad keys the way the focused program asked for.
            if focused {
                let (cursor_keys, keypad) = (pane.term.app_cursor, pane.term.app_keypad);
                cursor.push_str(if cursor_keys { "\x1b[?1h" } else { "\x1b[?1l" });
                cursor.push_str(if keypad { "\x1b=" } else { "\x1b>" });
            }
            for (file, row, col, cols, rows) in &pane.pictures {
                if self.sixel && file.ends_with(".sixel") && row + rows <= body.h && col + cols <= body.w {
                    pictures.push((pane_directory.join(file), body.x + col, body.y + row, *cols, *rows, pane.pid));
                }
            }
        }
        // The last line: the workspace, then what there is to say (what is
        // wrong with the layout file first), then the tabs: as many as fit,
        // the one shown among them.
        let (bold, plain) = (Style { flags: REVERSE | BOLD, ..Style::default() }, Style::default());
        let mut bar = cells(&format!(" {} ", self.workspace), bold);
        let hint = format!("no panes in {}: keys are scripts in {}", self.workspace, self.keys.display());
        let note =
            wrong.as_ref().unwrap_or(if tiles.is_empty() && self.message.is_empty() { &hint } else { &self.message });
        if !note.is_empty() {
            bar.extend(cells(&format!(" {note} "), plain).into_iter().take(w * 2 / 3));
            bar.extend(cells("│", plain));
        }
        let labels = layout.tabs.iter().enumerate().map(|(i, tab)| {
            let style = Style { flags: if i == layout.current { REVERSE } else { 0 }, ..plain };
            cells(&format!(" {}:{} ", i + 1, tab.focus), style)
        });
        let labels: Vec<Vec<Cell>> = labels.collect();
        let fits = |first: usize| {
            let shown = labels.iter().take(layout.current + 1).skip(first);
            bar.len() + shown.map(Vec::len).sum::<usize>() <= w
        };
        let first = (0..=layout.current).find(|first| fits(*first)).unwrap_or(layout.current);
        bar.extend(labels.iter().skip(first).flatten());
        put(&mut back, w, 0, h.saturating_sub(1), w, &cells(&" ".repeat(w), plain));
        put(&mut back, w, 0, h.saturating_sub(1), w, &bar);
        if let Some((directory, entries)) = &self.menu {
            // The menu, above the last line: its keys in as many columns as it takes to show them all.
            let room = h.saturating_sub(2).max(1);
            let across = entries.len().div_ceil(room).clamp(1, (w / 16).max(1));
            let down = entries.len().div_ceil(across).min(room);
            let more = entries.len().saturating_sub(across * down);
            let more = if more > 0 { format!(" ({more} more do not fit)") } else { String::new() };
            let name = directory.strip_prefix(&self.keys).unwrap_or(directory).display();
            let title = format!(" {name}: a key runs its script, any other key closes this{more}");
            let top = h.saturating_sub(down + 2);
            for y in top..h.saturating_sub(1) {
                put(&mut back, w, 0, y, w, &cells(&" ".repeat(w), bold));
            }
            put(&mut back, w, 0, top, w, &cells(&title, bold));
            for (i, entry) in entries.iter().enumerate().take(across * down) {
                let (x, width) = ((i / down) * (w / across), w / across);
                put(&mut back, w, x, top + 1 + i % down, width.saturating_sub(1), &cells(&format!(" {entry}"), bold));
            }
            cursor = String::new();
        }
        if cursor.is_empty() {
            cursor.push_str("\x1b[?25l");
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
                if let Some(inside) = inside.filter(|inside| inside.iter().all(|b| (0x20..=0x7e).contains(b))) {
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
            input: None,
        }
    }

    /// Reads the pane's screen if its file changed, and opens `in` if a holder reads it.
    fn read(&mut self, directory: &Path) {
        if self.input.is_none() {
            self.input = OpenOptions::new().write(true).custom_flags(sys::O_NONBLOCK).open(directory.join("in")).ok();
        }
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
        let text = String::from_utf8_lossy(&bytes);
        let mut lines = text.lines();
        let mut cursor = (0, 0);
        self.pictures.clear();
        self.state.clear();
        self.term = Term::new(1, 1);
        self.term.cursor_shown = false;
        // Line 1 says what the rows cannot: `key=value` words.
        for (key, value) in lines.next().unwrap_or_default().split(' ').filter_map(|word| word.split_once('=')) {
            let numbers: Vec<usize> = value.split(['x', ',', '@', '+']).filter_map(|n| n.parse().ok()).collect();
            match (key, numbers.as_slice()) {
                ("pid", [pid]) => self.pid = *pid as u32,
                ("size", [cols, rows]) => self.term = Term::new(*cols, *rows),
                ("cursor", [row, col]) => cursor = (*row, *col),
                ("shape", [shape]) => self.term.cursor_shape = (*shape).min(6) as u8,
                ("state", _) => self.state = value.replace(':', " "),
                ("modes", _) => {
                    let has = |mode: &str| value.split(',').any(|m| m == mode);
                    self.term.cursor_shown = has("cursor");
                    (self.term.app_cursor, self.term.app_keypad, self.term.paste) =
                        (has("appcursor"), has("keypad"), has("paste"));
                }
                // A picture is kept only if it lies on a screen of the largest size there is.
                ("pic", [row, col, cols, rows]) if numbers.iter().all(|n| *n <= SIZE_MAX) => {
                    let name = value.split('@').next().unwrap_or_default();
                    if is_name(name) {
                        self.pictures.push((name.into(), *row, *col, *cols, *rows));
                    }
                }
                _ => {}
            }
        }
        for (row, line) in lines.enumerate().take(self.term.rows) {
            self.term.feed(format!("\x1b[{};1H", row + 1).as_bytes());
            self.term.feed(line.as_bytes());
        }
        (self.term.row, self.term.col) = cursor;
    }
}

/// The entries of a menu: each key in the directory and, from the second
/// line of its script, what it does. At most 200.
fn menu_entries(directory: &Path) -> Vec<String> {
    let names = fs::read_dir(directory).into_iter().flatten().flatten();
    let names = names.filter_map(|entry| entry.file_name().into_string().ok()).filter(|name| !name.starts_with('.'));
    let mut names: Vec<String> = names.take(MENU_MAX + 1).collect();
    let more = names.len() > MENU_MAX;
    names.truncate(MENU_MAX);
    names.sort();
    let described = names.iter().map(|name| {
        let path = directory.join(name);
        if path.is_dir() {
            return format!("{name}  …");
        }
        let mut head = Vec::new();
        let _ = File::open(&path).map(|file| file.take(400).read_to_end(&mut head));
        let head = String::from_utf8_lossy(&head);
        format!("{name}  {}", head.lines().nth(1).and_then(|line| line.strip_prefix("# ")).unwrap_or_default())
    });
    described.chain(more.then(|| "…  more keys than a menu lists".to_string())).collect()
}
