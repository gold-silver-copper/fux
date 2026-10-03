//! xterm itself, under Xvfb, read by printing its screen.
//!
//! # Driven
//!
//! Each harness process starts one Xvfb (`-noreset`), which picks a free
//! display (`-displayfd`), and a shell beside it that stops it within a
//! second of the harness exiting. Before each xterm starts, the harness
//! checks that the display's socket is still there: an Xvfb of another
//! harness process that had the same display number can remove it as it
//! exits, and then a new Xvfb is started. Each terminal is an xterm of its
//! own, a child of the harness, run with `-geometry COLSxROWS`, an empty
//! title (`-T ''`), no utmp entry (`-ut`) and the pane program (see
//! `pane.rs`) as its command; one that exits before its pane program runs
//! (it could not open the display) is started again, up to three times.
//! Resources it is given:
//!
//! - `locale: false`, `utf8: 2`, `utf8Title: true`: UTF-8 always. Xlib
//!   here knows no UTF-8 locale, and left to the locale xterm reads bytes
//!   as Latin-1.
//! - `precompose: false`: a base and its combining marks are kept as
//!   written, not composed into one character (`e\u{301}` stays two
//!   characters, not `é`).
//! - `printerCommand: cat >> FILE`, `printAttributes: 2` (SGR with
//!   colours), `printerNewLine: false` (a soft-wrapped row ends with CR
//!   alone), `printerFormFeed: true` (FF ends a print, so the harness knows
//!   it has all of it), `printerAutoClose: true`, `printRawChars: true`
//!   (U+FFFD prints as itself, not as `#`, and the second half of a wide
//!   character as U+FFFF, so widths are xterm's own).
//! - `allowWindowOps: true` and `disallowedWindowOps` empty: resizing and
//!   reading the title (`CSI 21 t`) by escape sequence.
//! - `saveLines: 100000`; `boldColors: false`, so bold does not brighten
//!   colours 0–7.
//!
//! A resize is `CSI 8 ; rows ; cols t`, then `CSI 18 t` until xterm reports
//! the new size. Dropping the engine kills its xterm.
//!
//! # Read
//!
//! - Cells and history: `CSI ? 11 i`, media copy of all pages: the saved
//!   lines and the screen, or on the alternate screen that screen alone.
//!   History read on the primary screen is kept, and given again while the
//!   alternate screen is shown (the primary's history does not change
//!   underneath it, except by a resize). Rows are decoded by
//!   `pane::Reader` (see `pane::Glyphs::Xterm`). Each row ends with CR, then
//!   LF unless it is soft-wrapped; the last row has no LF either way, and
//!   reads as not soft-wrapped. (`CSI ? 10 i` would print only the
//!   scrolling region.)
//! - Pending scrolls are flushed first (`CSI ? 4 h`, `CSI ? 4 l`: smooth
//!   scrolling on and off): with jump scrolling, xterm moves rows into its
//!   saved lines only when it flushes, and a print before that has no
//!   history.
//! - Cursor: a cursor position report; in origin mode it counts from the
//!   top margin, which DECRQSS (`DCS $ q r ST`) tells.
//! - Modes: DECRQM for 1, 6, 7, 25, 66, 1004, 2004, and 1049, 1047 and 47
//!   for the alternate screen.
//! - Title: `CSI 21 t`.
//! - Pending wrap cannot be read: a cursor report gives the last column
//!   either way.
//! - xterm has no underline colour, and no kitty keyboard protocol.
//! - A row prints up to its last drawn cell, so the style of the blank
//!   cells after it cannot be read: the background of cells erased at the
//!   end of a row is lost (`'\e[44m\e[K'`). Those cells' text is compared,
//!   and their style is not (`Line::unread_from`).
//! - The print gives an SGR before a cell with attributes and none before
//!   a plain cell after it, which reads as plain (see `pane::Glyphs::Xterm`).
//! - Pending scrolls are also flushed before EL 1 and ED 1, whose end of a
//!   pending wrap otherwise depends on whether the cursor's row waits on a
//!   jump scroll (see `flushed_before_clearing_left`).
//! - The harness's own queries between steps are output to xterm, which
//!   fux-vt never sees. xterm abstains from a case where they matter: a
//!   step that begins with REP before printing (after the queries xterm
//!   has nothing to repeat), and a read after output that ends partway
//!   through a UTF-8 character (the queries would end it, as U+FFFD).
//! - xterm abstains from a case once its output holds SGR 58: xterm has no
//!   underline colour, and reads the colour's parameters as attributes
//!   (`58;5;9` is blink and strikeout), so what it shows says nothing of
//!   the rest of the SGR.
//!
//! # Quirks
//!
//! xterm's own choices, read right (each is
//! `fux-vt-compare replay --engines xterm,ghostty ...`; Ghostty agrees with
//! fux-vt in all but `4:0`, where xterm does):
//!
//! - SGR 58 is not one of its sequences: `58;5;9` reads as 58, then blink
//!   and strikeout (`--size 1x3 '\e[4;58;5;9mX'`), and `4:0` does not end
//!   underline (`--size 1x3 '\e[4m\e[4:0mX'`). (SGR 21 is a double
//!   underline, as in Ghostty: `--size 1x1 '\e[21mx'`.)
//! - DECSTR resets the pen, cursor visibility, autowrap and the keypad, as
//!   DEC specifies (`--size 2x5 'ab\e[1m\e[!pX'`,
//!   `--size 1x1 '\e[?25l' '\e[!p'`).
//! - An empty title sets the default, `xterm` (`--size 1x1 '\e]2;\x07'`).
//! - No reflow on resize (`--size 1x1 --history 10000 'ab' resize:3x10`).
//! - ZWJ and VS16 are not kept, so emoji sequences are cells of their own
//!   (`--size 1x8 '👨\u{200d}👩❤\u{fe0f}'`); a skin tone and a second
//!   regional indicator are cells of their own.
use crate::engine::{Can, Engine, Kind, Setup};
use crate::engines::pane::{self, Glyphs, Pane, Reader};
use crate::snapshot::{Line, Snapshot};
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

pub const KIND: Kind = Kind {
    name: "xterm",
    about: "xterm itself, under Xvfb, read by printing its screen",
    can: Can {
        pending_wrap: false,
        underline_color: false,
        kitty_keyboard_flags: false,
        synchronized_output: false,
        in_band_resize: false,
        link_uri: false,
        link_group: false,
        prompt: false,
        ..Can::ALL
    },
    panel: false,
    in_process: false,
    available,
    make,
};

/// The continuation bytes a UTF-8 character still owes after `bytes`,
/// given `owed` before them.
fn owed_after(mut owed: u8, bytes: &[u8]) -> u8 {
    for &b in bytes {
        owed = match b {
            0x80..=0xbf if owed > 0 => owed.saturating_sub(1),
            0xc2..=0xdf => 1,
            0xe0..=0xef => 2,
            0xf0..=0xf4 => 3,
            _ => 0,
        };
    }
    owed
}

/// Whether `bytes` hold a REP (`CSI Pn b`) before any graphic character:
/// one xterm takes as following the harness's own queries, after which it
/// has nothing to repeat, where fux-vt, which never sees them, repeats the
/// character printed before them (`'ab' '\e[3b'` as two steps, against
/// `'ab\e[3b'` as one).
fn repeats_before_printing(bytes: &[u8]) -> bool {
    let mut i = 0usize;
    while let Some(&b) = bytes.get(i) {
        i = i.saturating_add(1);
        match b {
            0x1b => match bytes.get(i) {
                Some(b'[') => {
                    let body = bytes.get(i.saturating_add(1)..).unwrap_or_default();
                    let len = body
                        .iter()
                        .position(|b| !(0x20..=0x3f).contains(b))
                        .unwrap_or(body.len());
                    let intermediates = body
                        .get(..len)
                        .unwrap_or_default()
                        .iter()
                        .any(|b| (0x20..=0x2f).contains(b));
                    if body.get(len) == Some(&b'b') && !intermediates {
                        return true;
                    }
                    i = i.saturating_add(len).saturating_add(2);
                }
                Some(b']' | b'P' | b'_' | b'^' | b'X') => {
                    let end = bytes
                        .get(i..)
                        .unwrap_or_default()
                        .iter()
                        .position(|&b| b == 0x07 || b == 0x9c || b == b'\\')
                        .unwrap_or(bytes.len());
                    i = i.saturating_add(end).saturating_add(1);
                }
                _ => i = i.saturating_add(1),
            },
            0x00..=0x1f | 0x7f => {}
            _ => return false,
        }
    }
    false
}

/// Flushes pending scrolls (`CSI ? 4 h`, `CSI ? 4 l`) before each EL 1 and
/// ED 1 (`CSI 1 K`, `CSI 1 J`, and their `?` forms). xterm's `ClearLeft`
/// ends a pending wrap only while the cursor's row is not waiting for a
/// jump scroll to be drawn (`AddToVisible`, xterm 411 `util.c`); otherwise
/// the wrap stays, so which bytes arrive together would decide it
/// (`replay --engines xterm,ghostty --size 2x4 'abcdefghijkl\e[1KX'`).
/// The flush itself keeps a pending wrap, in every engine.
fn flushed_before_clearing_left(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut rest = bytes;
    while let Some(start) = rest.iter().position(|&b| b == 0x1b) {
        let (before, from) = rest.split_at_checked(start).unwrap_or((rest, &[]));
        out.extend_from_slice(before);
        let body = from.get(2..).unwrap_or_default();
        let len = body
            .iter()
            .position(|b| !(0x30..=0x3f).contains(b))
            .unwrap_or(body.len());
        let params = body.get(..len).unwrap_or_default();
        let clears_left = from.get(1) == Some(&b'[')
            && matches!(body.get(len), Some(b'J' | b'K'))
            && params.strip_prefix(b"?").unwrap_or(params) == b"1";
        if clears_left {
            out.extend_from_slice(b"\x1b[?4h\x1b[?4l");
        }
        out.push(0x1b);
        rest = from.get(1..).unwrap_or_default();
    }
    out.extend_from_slice(rest);
    out
}

/// Whether `bytes` hold an SGR (`CSI … m`) with a parameter 58, in either
/// form (`58;5;9`, `58:5:9`), not counting a colour's own parameters
/// (`38;5;58`).
fn sets_underline_colour(bytes: &[u8]) -> bool {
    let mut rest = bytes;
    while let Some(start) = rest.iter().position(|&b| b == 0x1b) {
        let after = rest.get(start.saturating_add(1)..).unwrap_or_default();
        let Some(body) = after.strip_prefix(b"[") else {
            rest = after;
            continue;
        };
        let len = body
            .iter()
            .position(|b| !(0x30..=0x3f).contains(b))
            .unwrap_or(body.len());
        let (params, after) = body.split_at_checked(len).unwrap_or((body, &[]));
        if after.first() == Some(&b'm') && sgr_has_58(params) {
            return true;
        }
        rest = after;
    }
    false
}

fn sgr_has_58(params: &[u8]) -> bool {
    // Each group's code, and whether it holds its own parameters (`:`).
    let codes: Vec<(Option<u16>, bool)> = params
        .split(|&b| b == b';')
        .map(|group| {
            let code = group.split(|&b| b == b':').next().unwrap_or_default();
            let code = std::str::from_utf8(code).ok().and_then(|c| c.parse().ok());
            (code, group.contains(&b':'))
        })
        .collect();
    let mut i = 0usize;
    while let Some(&code) = codes.get(i) {
        i = i.saturating_add(1);
        match code {
            (Some(58), _) => return true,
            (Some(38 | 48), false) => match codes.get(i) {
                Some((Some(5), _)) => i = i.saturating_add(2),
                Some((Some(2), _)) => i = i.saturating_add(4),
                _ => {}
            },
            _ => {}
        }
    }
    false
}

/// How long an xterm may take to start its pane program, and a print or a
/// resize to arrive.
const START: Duration = Duration::from_secs(20);
const ARRIVE: Duration = Duration::from_secs(10);

/// Everything a snapshot asks, ending with the print.
const QUERY: &[u8] = b"\x1b[?4h\x1b[?4l\x1b[6n\
\x1b[?1$p\x1b[?6$p\x1b[?7$p\x1b[?25$p\x1b[?66$p\x1b[?1004$p\x1b[?2004$p\
\x1b[?1049$p\x1b[?1047$p\x1b[?47$p\x1b[21t\x1bP$qr\x1b\\\x1b[?11i";

fn available() -> Result<(), String> {
    pane::on_path("xterm")?;
    pane::on_path("Xvfb")
}

/// This process's Xvfb, the shell that stops it once this process has
/// gone, and its display.
struct Xvfb {
    server: Child,
    watch: Child,
    display: u32,
}

static XVFB: Mutex<Option<Xvfb>> = Mutex::new(None);

/// The display of this process's Xvfb, started if it is not running, or
/// started again if its socket has gone (the Xvfb of another process that
/// had the same display can remove it as it exits).
pub fn display() -> Result<u32, String> {
    let mut xvfb = XVFB.lock().map_err(|_| "the Xvfb handle is poisoned")?;
    let running = xvfb.as_mut().and_then(|x| {
        let socket = PathBuf::from(format!("/tmp/.X11-unix/X{}", x.display));
        (matches!(x.server.try_wait(), Ok(None)) && socket.exists()).then_some(x.display)
    });
    let n = match running {
        Some(n) => n,
        None => {
            if let Some(old) = xvfb.as_mut() {
                stop(&mut old.server);
                // The watch sees Xvfb gone within a second.
                let _ = old.watch.wait();
            }
            let started = start_xvfb()?;
            let n = started.display;
            *xvfb = Some(started);
            n
        }
    };
    drop(xvfb);
    Ok(n)
}

/// Stops a child with SIGTERM, so it cleans up after itself, and reaps it.
fn stop(child: &mut Child) {
    if matches!(child.try_wait(), Ok(None)) {
        let _ = Command::new("kill")
            .arg(child.id().to_string())
            .stderr(Stdio::null())
            .status();
        let _ = child.wait();
    }
}

/// Starts an Xvfb on a free display, and a shell that stops it once this
/// process has gone.
fn start_xvfb() -> Result<Xvfb, String> {
    let mut server = Command::new("Xvfb")
        .args([
            "-displayfd",
            "1",
            "-nolisten",
            "tcp",
            "-noreset",
            "-screen",
            "0",
            "4096x4096x24",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("Xvfb: {e}"))?;
    let mut line = String::new();
    if let Some(out) = server.stdout.take() {
        let _ = BufReader::new(out).read_line(&mut line);
    }
    let Ok(display) = line.trim().parse() else {
        stop(&mut server);
        return Err("Xvfb did not start".into());
    };
    let script = format!(
        "while kill -0 {0} 2>/dev/null && kill -0 {1} 2>/dev/null; do sleep 1; done; \
         kill {1} 2>/dev/null",
        std::process::id(),
        server.id()
    );
    match Command::new("/bin/sh")
        .args(["-c", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(watch) => Ok(Xvfb {
            server,
            watch,
            display,
        }),
        Err(e) => {
            stop(&mut server);
            Err(format!("Xvfb's watch: {e}"))
        }
    }
}

pub struct Xterm {
    pane: Pane,
    child: Child,
    print: PathBuf,
    rows: u16,
    cols: u16,
    /// History as last read on the primary screen.
    history: Vec<(String, bool)>,
    /// Whether the harness has sent xterm its own queries (a read or a
    /// resize) since the last output.
    asked: bool,
    /// Continuation bytes the last output still owed a UTF-8 character.
    owed: u8,
}

fn make(setup: &Setup) -> Result<Box<dyn Engine>, String> {
    let (pane, program) = Pane::new()?;
    let print = pane::fresh("print")?;
    File::create(&print).map_err(|e| format!("{}: {e}", print.display()))?;
    let mut xterm = Xterm {
        pane,
        child: spawn(setup, &print, &program)?,
        print,
        rows: setup.rows,
        cols: setup.cols,
        history: Vec::new(),
        asked: false,
        owed: 0,
    };
    // An xterm that could not open the display exits: start another, on a
    // new Xvfb if that one has gone.
    let deadline = Instant::now()
        .checked_add(START)
        .ok_or("a deadline out of range")?;
    let mut tries = 1u8;
    while !xterm.pane.connect(Duration::from_millis(100))? {
        if Instant::now() >= deadline {
            return Err(format!(
                "xterm: the pane program did not start within {START:?}"
            ));
        }
        if !matches!(xterm.child.try_wait(), Ok(None)) {
            if tries >= 3 {
                return Err("xterm: exited three times before its pane program ran".into());
            }
            tries = tries.saturating_add(1);
            xterm.child = spawn(setup, &xterm.print, &program)?;
        }
    }
    xterm.pane.output(b"")?;
    if xterm.size()? != (setup.rows, setup.cols) {
        xterm.resize(setup.rows, setup.cols)?;
    }
    Ok(Box::new(xterm))
}

/// Starts an xterm running `program` that prints to `print`.
fn spawn(setup: &Setup, print: &Path, program: &str) -> Result<Child, String> {
    let display = display()?;
    let resources = [
        "XTerm*locale: false".to_owned(),
        "XTerm*utf8: 2".to_owned(),
        "XTerm*utf8Title: true".to_owned(),
        format!(
            "XTerm*printerCommand: cat >> {}",
            pane::quote(&print.to_string_lossy())
        ),
        "XTerm*printAttributes: 2".to_owned(),
        "XTerm*printerNewLine: false".to_owned(),
        "XTerm*printerFormFeed: true".to_owned(),
        "XTerm*printerAutoClose: true".to_owned(),
        "XTerm*printRawChars: true".to_owned(),
        "XTerm*allowWindowOps: true".to_owned(),
        "XTerm*disallowedWindowOps: ".to_owned(),
        "XTerm*allowTitleOps: true".to_owned(),
        "XTerm*saveLines: 100000".to_owned(),
        "XTerm*boldColors: false".to_owned(),
        "XTerm*precompose: false".to_owned(),
    ];
    let mut command = Command::new("xterm");
    command
        .arg("-display")
        .arg(format!(":{display}"))
        .arg("-geometry")
        .arg(format!("{}x{}", setup.cols, setup.rows))
        .args(["-T", "", "-ut"]);
    for resource in &resources {
        command.arg("-xrm").arg(resource);
    }
    command
        .args(["-e", "/bin/sh", "-c", program])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("xterm: {e}"))
}

/// The numbers of a reply `CSI params final` in `bytes`, after `lead`
/// (`ESC [` and any private marker).
fn numbers(bytes: &[u8], lead: &str, last: char) -> Option<Vec<u16>> {
    let text = String::from_utf8_lossy(bytes);
    let mut rest = text.as_ref();
    while let Some((_, after)) = rest.split_once(lead) {
        let end = after
            .find(|c: char| !(c.is_ascii_digit() || c == ';'))
            .unwrap_or(after.len());
        let (params, tail) = after.split_at_checked(end).unwrap_or((after, ""));
        if tail.starts_with(last) {
            return params.split(';').map(|p| p.parse().ok()).collect();
        }
        rest = after;
    }
    None
}

/// The title in a reply to `CSI 21 t`: `OSC l title ST`.
fn title(bytes: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(bytes);
    let (_, rest) = text.split_once("\x1b]l")?;
    let (title, _) = rest.split_once("\x1b\\")?;
    Some(title.to_owned())
}

impl Xterm {
    /// The text area's size, as xterm reports it.
    fn size(&mut self) -> Result<(u16, u16), String> {
        let reply = self.pane.ask(b"\x1b[18t")?;
        match numbers(&reply, "\x1b[", 't').as_deref() {
            Some([8, rows, cols]) => Ok((*rows, *cols)),
            _ => Err(format!("xterm: no size in {reply:?}")),
        }
    }

    /// Waits for the print to end (FF), and gives it.
    fn printed(&self) -> Result<Vec<u8>, String> {
        let deadline = Instant::now()
            .checked_add(ARRIVE)
            .ok_or("a deadline out of range")?;
        let mut pause = Duration::from_micros(200);
        loop {
            let bytes = fs::read(&self.print).map_err(|e| format!("xterm: print: {e}"))?;
            if let Some(end) = bytes.iter().position(|&b| b == b'\x0c') {
                return Ok(bytes.get(..end).unwrap_or_default().to_vec());
            }
            if Instant::now() >= deadline {
                return Err("xterm: the print did not arrive".into());
            }
            thread::sleep(pause);
            pause = pause.saturating_mul(2).min(Duration::from_millis(5));
        }
    }

    fn read(&mut self, history_rows: usize) -> Result<Snapshot, String> {
        File::create(&self.print).map_err(|e| format!("xterm: print: {e}"))?;
        let replies = self.pane.ask(QUERY)?;
        let printed = self.printed()?;
        let set = |m: u16| pane::mode(&replies, m) == Some(1);
        let alternate = set(1049) || set(1047) || set(47);
        let origin = set(6);
        let (row, col) = match numbers(&replies, "\x1b[", 'R').as_deref() {
            Some([row, col]) => (*row, *col),
            _ => return Err(format!("xterm: no cursor report in {replies:?}")),
        };
        let top = if origin {
            match numbers(&replies, "\x1bP1$r", 'r').as_deref() {
                Some([top, _]) => top.saturating_sub(1),
                _ => 0,
            }
        } else {
            0
        };
        // Rows end with CR; a row that is not soft-wrapped, with LF after.
        let mut rows: Vec<(Vec<u8>, bool)> = Vec::new();
        let mut rest = printed.as_slice();
        while let Some(end) = rest.iter().position(|&b| b == b'\r') {
            let (row, after) = rest.split_at_checked(end).unwrap_or((rest, &[]));
            let after = after.get(1..).unwrap_or_default();
            let newline = after.first() == Some(&b'\n');
            rows.push((row.to_vec(), !newline));
            rest = if newline {
                after.get(1..).unwrap_or_default()
            } else {
                after
            };
        }
        // The last row has no LF either way.
        if let Some(last) = rows.last_mut() {
            last.1 = false;
        }
        let screen_rows = usize::from(self.rows);
        let first_screen = rows.len().saturating_sub(screen_rows);
        let mut reader = Reader::new(Glyphs::Xterm);
        let mut screen = Vec::new();
        let mut history = Vec::new();
        for (i, (text, wrapped)) in rows.iter().enumerate() {
            let cells = reader.row(text, self.cols, &|_| 1);
            let line = Line {
                cells,
                wrapped: *wrapped,
                // The print stops at the row's last drawn cell.
                unread_from: Some(reader.printed),
                prompt: false,
            };
            if i < first_screen {
                history.push((line.text(), line.wrapped));
            } else {
                screen.push(line);
            }
        }
        if alternate {
            history = self.history.clone();
        } else {
            self.history = history.clone();
        }
        let skip = history.len().saturating_sub(history_rows);
        Ok(Snapshot {
            rows: u16::try_from(screen.len()).unwrap_or(u16::MAX),
            cols: self.cols,
            cursor: (
                row.saturating_add(top).saturating_sub(1),
                col.saturating_sub(1),
            ),
            pending_wrap: false,
            cursor_visible: set(25),
            autowrap: set(7),
            origin,
            alternate,
            application_cursor: set(1),
            application_keypad: set(66),
            bracketed_paste: set(2004),
            synchronized_output: false,
            in_band_resize: false,
            focus_reporting: set(1004),
            kitty_keyboard_flags: 0,
            title: title(&replies).unwrap_or_default(),
            reports: self.pane.reports(),
            screen,
            history: history.into_iter().skip(skip).collect(),
        })
    }
}

impl Engine for Xterm {
    fn process(&mut self, bytes: &[u8]) -> Result<(), String> {
        if sets_underline_colour(bytes) {
            return Err("xterm has no SGR 58, and reads its colour as attributes".into());
        }
        if self.asked && repeats_before_printing(bytes) {
            return Err("the harness's queries came between a character and REP".into());
        }
        self.asked = false;
        self.owed = owed_after(self.owed, bytes);
        self.pane.output(&flushed_before_clearing_left(bytes))
    }

    fn resize(&mut self, rows: u16, cols: u16) -> Result<(), String> {
        self.asked = true;
        self.pane
            .ask(format!("\x1b[?4h\x1b[?4l\x1b[8;{rows};{cols}t").as_bytes())?;
        let deadline = Instant::now()
            .checked_add(ARRIVE)
            .ok_or("a deadline out of range")?;
        loop {
            let size = self.size()?;
            if size == (rows, cols) {
                break;
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "xterm: asked for {rows}x{cols}, still {}x{}",
                    size.0, size.1
                ));
            }
            thread::sleep(Duration::from_millis(2));
        }
        self.rows = rows;
        self.cols = cols;
        Ok(())
    }

    fn snapshot(&mut self, history_rows: usize) -> Result<Snapshot, String> {
        if self.owed > 0 {
            return Err("the harness's queries would split a UTF-8 character".into());
        }
        self.asked = true;
        self.read(history_rows)
    }

    /// Streams the whole workload, then waits once.
    fn feed(&mut self, bytes: &[u8], chunk: usize) -> Result<(), String> {
        let mut rest = bytes;
        while !rest.is_empty() {
            let (now, later) = rest
                .split_at_checked(chunk.min(rest.len()))
                .unwrap_or((rest, &[]));
            self.pane.write(now)?;
            rest = later;
        }
        self.pane.sync().map(|_| ())
    }
}

impl Drop for Xterm {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_file(&self.print);
        pane::tidy();
    }
}

#[cfg(test)]
mod tests {
    use super::{
        flushed_before_clearing_left, owed_after, repeats_before_printing, sets_underline_colour,
    };

    #[test]
    fn a_split_character_and_a_leading_rep_are_found() {
        assert_eq!(owed_after(0, b"a\xc3"), 1);
        assert_eq!(owed_after(1, b"\xa9"), 0);
        assert_eq!(owed_after(0, b"\xe2\x94"), 1);
        assert_eq!(owed_after(0, b"\xe2\x94\x80"), 0);
        for (bytes, found) in [
            (&b"\x1b[3b"[..], true),
            (b"\r\x1b[1m\x1b[b", true),
            (b"\x1b]2;t\x07\x1b[2b", true),
            (b"x\x1b[3b", false),
            (b"\x1b[3 b", false),
            (b"\x1b[3m", false),
        ] {
            assert_eq!(repeats_before_printing(bytes), found, "{bytes:?}");
        }
    }

    #[test]
    fn pending_scrolls_are_flushed_before_clearing_left() {
        assert_eq!(
            flushed_before_clearing_left(b"ab\x1b[1Kc\x1b[?1J\x1b[K\x1b[11K\x1b"),
            b"ab\x1b[?4h\x1b[?4l\x1b[1Kc\x1b[?4h\x1b[?4l\x1b[?1J\x1b[K\x1b[11K\x1b"
        );
    }

    #[test]
    fn sgr_58_is_found_in_either_form() {
        for (bytes, found) in [
            (&b"\x1b[4;58;5;9mX"[..], true),
            (b"a\x1b[58:2::1:2:3m", true),
            (b"\x1b[1m\x1b[4;058m", true),
            (b"\x1b[38;5;58m\x1b[59m", false),
            (b"\x1b[38;2;1;58;3m", false),
            (b"\x1b[38:5:9;2;58m", true),
            (b"\x1b[58H", false),
            (b"\x1b[?58m", false),
        ] {
            assert_eq!(sets_underline_colour(bytes), found, "{bytes:?}");
        }
    }
}
