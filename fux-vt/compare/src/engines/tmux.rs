//! tmux, a server of its own, read with `capture-pane`.
//!
//! # Driven
//!
//! Each harness process has one tmux server, on the socket `tmux` in the
//! process's directory (`tmux -S`; see `pane.rs`), not `-L`, whose socket
//! would be left in `/tmp/tmux-<uid>/`: tmux does not remove its socket
//! when it exits, so the harness does, with its last session. It is
//! started with no configuration (`-f /dev/null`). Each terminal is a
//! session of its own in it, whose one
//! pane runs the pane program (see `pane.rs`) at exactly the case's size:
//! the status line is off, `window-size` is manual, and history is 100000
//! rows (`history-limit`, set before the session is made, as it applies to
//! new panes only). Its title starts empty (`select-pane -T ''`). A resize
//! is `resize-window`, which resizes and reflows the pane at once. Dropping
//! the engine kills its session, and the server exits with its last
//! session (`exit-empty`). If the harness dies, its pane programs see the
//! end of their input and exit, and so the sessions and the server.
//!
//! # Read
//!
//! - Cells: `capture-pane -p -e -N -F`, decoded by `pane::Reader` (see
//!   `pane::Glyphs::Tmux` for how cells and widths are worked out). `-F`
//!   prints each row's flags, `W` for a soft-wrapped row. `-N` prints a
//!   row's cells up to its allocated size, and the rest are blank.
//! - History: the same capture from `-S -N` (the rows before the screen),
//!   or, on the alternate screen, which has no history, a second capture
//!   with `-a` (the primary screen and its history).
//! - Tabs: a tab across blank cells is kept as one cell that prints as
//!   `\t`, spanning to the next tab stop as they are now
//!   (`#{pane_tabs}`), or to the last column. A tab written before tab
//!   stops were changed spans what it spanned then, and reads wrong.
//! - Cursor, modes and title: `display -p` with `cursor_x`, `cursor_y`,
//!   `cursor_flag`, `alternate_on`, `origin_flag`, `wrap_flag`,
//!   `keypad_cursor_flag`, `keypad_flag`, `bracket_paste_flag` and
//!   `pane_title`. tmux keeps a cursor waiting to wrap one past the last
//!   column: that is `pending_wrap`.
//! - Focus reporting has no format: it is asked of the terminal, by DECRQM
//!   (`CSI ? 1004 $ p`), through the pane.
//! - Kitty keyboard flags: tmux does not implement the protocol, and keeps
//!   none.
//!
//! # Quirks
//!
//! tmux's own choices, where Ghostty agrees with fux-vt (each is
//! `fux-vt-compare replay --engines tmux,ghostty ...`):
//!
//! - A cursor position report while a wrap is pending gives the column one
//!   past the last, where tmux keeps the cursor (`--size 1x3 'abc\e[6n'`
//!   reports `CSI 1;4R`).
//! - With autowrap off, a glyph in the last column leaves no wrap pending
//!   (`--size 1x3 '\e[?7labcdef'`); in a one-column screen the glyphs after
//!   the first are dropped (`--size 1x1 '\e[?7l78'`).
//! - Restoring a cursor saved while a wrap was pending leaves none pending
//!   (`--size 1x5 'abcde\e7\e8'`).
//! - An APC string sets the title, as in screen (`--size 1x1 '\e_abc\e\\'`).
//! - A glyph that wraps after LF moved the pending cursor down marks the row
//!   it wrapped from, which LF left blank, as soft-wrapped
//!   (`--size 2x5 'abcde\nX'`).
//! - CHT (`CSI I`) is not implemented (`--size 1x20 '\e[2IX\e[ZY'`), nor
//!   mode 1048 (`--size 2x5 'ab\e[?1048h\e[?1047hX\e[?1047l\e[?1048lY'`).
//! - A consonant after a virama starts a cell of its own
//!   (`--size 1x5 'क\u{94d}षZ'`).
//! - The primary screen is not reflowed while the alternate screen is
//!   shown (the named case `reflow-moves-the-saved-cursor`).
use crate::engine::{Can, Engine, Kind, Setup};
use crate::engines::pane::{self, Glyphs, Pane, Reader};
use crate::snapshot::{Line, Snapshot};
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

pub const KIND: Kind = Kind {
    name: "tmux",
    about: "tmux, a server of its own, read with capture-pane",
    can: Can {
        kitty_keyboard_flags: false,
        synchronized_output: false,
        in_band_resize: false,
        ..Can::ALL
    },
    panel: false,
    in_process: false,
    available,
    make,
};

/// History tmux keeps, in rows: far more than a case can fill.
const HISTORY: &str = "100000";

/// How long a new session may take to start its pane program.
const START: Duration = Duration::from_secs(10);

/// What `display -p` is asked: the cursor, the modes, the tab stops (after
/// a `T`, so an empty list is still a field), and the title, last, as it
/// may hold spaces.
const FORMAT: &str = "#{cursor_x} #{cursor_y} #{pane_width} #{pane_height} #{cursor_flag} \
#{alternate_on} #{origin_flag} #{wrap_flag} #{keypad_cursor_flag} #{keypad_flag} \
#{bracket_paste_flag} T#{pane_tabs} #{pane_title}";

fn available() -> Result<(), String> {
    pane::on_path("tmux")
}

/// The sessions this process has open: the server exits with the last.
static SESSIONS: AtomicUsize = AtomicUsize::new(0);

/// This process's server socket, in its directory.
fn socket() -> Result<PathBuf, String> {
    Ok(pane::dir()?.join("tmux"))
}

/// Runs a tmux command (`;` separating several) on this process's server,
/// and gives what it printed.
fn tmux(args: &[&str]) -> Result<Vec<u8>, String> {
    let out = Command::new("tmux")
        .arg("-S")
        .arg(socket()?)
        .args(["-f", "/dev/null"])
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("tmux: {e}"))?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(format!(
            "tmux {}: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

pub struct Tmux {
    pane: Pane,
    session: String,
    rows: u16,
    cols: u16,
    killed: bool,
}

fn make(setup: &Setup) -> Result<Box<dyn Engine>, String> {
    let (pane, program) = Pane::new()?;
    let session = format!("t{}", pane::serial());
    let (rows, cols) = (setup.rows.to_string(), setup.cols.to_string());
    let mut tmux_engine = Tmux {
        pane,
        session,
        rows: setup.rows,
        cols: setup.cols,
        killed: true,
    };
    let target = tmux_engine.session.clone();
    // From here the session may exist, and dropping the engine kills it.
    SESSIONS.fetch_add(1, Ordering::SeqCst);
    tmux_engine.killed = false;
    tmux(&[
        "start-server",
        ";",
        "set",
        "-g",
        "history-limit",
        HISTORY,
        ";",
        "set",
        "-g",
        "status",
        "off",
        ";",
        "set",
        "-g",
        "window-size",
        "manual",
        ";",
        "new-session",
        "-d",
        "-s",
        &target,
        "-x",
        &cols,
        "-y",
        &rows,
        &program,
        ";",
        "resize-window",
        "-t",
        &target,
        "-x",
        &cols,
        "-y",
        &rows,
        ";",
        "select-pane",
        "-t",
        &target,
        "-T",
        "",
    ])?;
    if !tmux_engine.pane.connect(START)? {
        return Err(format!(
            "tmux: the pane program did not start within {START:?}"
        ));
    }
    tmux_engine.pane.output(b"")?;
    let shown = tmux_engine.info()?;
    if (shown.rows, shown.cols) != (setup.rows, setup.cols) {
        return Err(format!(
            "tmux: the pane is {}x{}, not {}x{}",
            shown.rows, shown.cols, setup.rows, setup.cols
        ));
    }
    Ok(Box::new(tmux_engine))
}

/// What `display -p` tells.
struct Info {
    cursor: (u16, u16),
    pending_wrap: bool,
    rows: u16,
    cols: u16,
    cursor_visible: bool,
    alternate: bool,
    origin: bool,
    autowrap: bool,
    application_cursor: bool,
    application_keypad: bool,
    bracketed_paste: bool,
    tabs: Vec<usize>,
    title: String,
}

impl Tmux {
    fn info(&self) -> Result<Info, String> {
        let out = tmux(&["display", "-p", "-t", &self.session, FORMAT])?;
        let text = String::from_utf8_lossy(&out);
        let text = text.strip_suffix('\n').unwrap_or(&text);
        let fields: Vec<&str> = text.splitn(13, ' ').collect();
        let bad = || format!("tmux: cannot read {text:?}");
        let number = |i: usize| -> Result<u16, String> {
            fields.get(i).and_then(|f| f.parse().ok()).ok_or_else(bad)
        };
        let flag = |i: usize| fields.get(i) == Some(&"1");
        let (x, y, cols, rows) = (number(0)?, number(1)?, number(2)?, number(3)?);
        let tabs = fields
            .get(11)
            .and_then(|f| f.strip_prefix('T'))
            .ok_or_else(bad)?
            .split(',')
            .filter_map(|t| t.parse().ok())
            .collect();
        Ok(Info {
            cursor: (y, x.min(cols.saturating_sub(1))),
            pending_wrap: x >= cols,
            rows,
            cols,
            cursor_visible: flag(4),
            alternate: flag(5),
            origin: flag(6),
            autowrap: flag(7),
            application_cursor: flag(8),
            application_keypad: flag(9),
            bracketed_paste: flag(10),
            tabs,
            title: fields.get(12).copied().unwrap_or_default().to_owned(),
        })
    }

    /// The rows of a capture, each with its wrap flag.
    fn capture(&self, extra: &[&str], from: usize) -> Result<Vec<(bool, Vec<u8>)>, String> {
        let start = format!("-{from}");
        let mut args = vec!["capture-pane", "-p", "-N", "-F"];
        args.extend_from_slice(extra);
        args.extend_from_slice(&["-S", &start, "-E", "-", "-t", &self.session]);
        let out = tmux(&args)?;
        let body = out.strip_suffix(b"\n").unwrap_or(&out);
        Ok(body
            .split(|&b| b == b'\n')
            .map(|line| {
                let at = line.iter().position(|&b| b == b' ').unwrap_or(line.len());
                let (flags, rest) = line.split_at_checked(at).unwrap_or((line, &[]));
                (
                    flags.contains(&b'W'),
                    rest.strip_prefix(b" ").unwrap_or(rest).to_vec(),
                )
            })
            .collect())
    }

    fn read(&mut self, history_rows: usize) -> Result<Snapshot, String> {
        let focus = pane::mode(&self.pane.ask(b"\x1b[?1004$p")?, 1004) == Some(1);
        let info = self.info()?;
        let tabs = info.tabs.clone();
        let last = usize::from(info.cols).saturating_sub(1);
        let tab = move |at: usize| {
            tabs.iter()
                .copied()
                .find(|&stop| stop > at)
                .unwrap_or(last)
                .min(last)
                .saturating_sub(at)
        };
        let rows = usize::from(info.rows);
        let mut reader = Reader::new(Glyphs::Tmux);
        let mut lines: Vec<Line> = Vec::new();
        let mut history = Vec::new();
        let screen_from = if info.alternate { 0 } else { history_rows };
        let captured = self.capture(&["-e"], screen_from)?;
        let first_screen = captured.len().saturating_sub(rows);
        for (i, (wrapped, text)) in captured.iter().enumerate() {
            let cells = reader.row(text, info.cols, &tab);
            let line = Line {
                unread_from: None,
                cells,
                wrapped: *wrapped,
            };
            if i < first_screen {
                history.push((line.text(), line.wrapped));
            } else {
                lines.push(line);
            }
        }
        if info.alternate && history_rows > 0 {
            // The primary screen and its history, without the screen.
            let primary = self.capture(&["-a"], history_rows)?;
            let kept = primary.len().saturating_sub(rows);
            let mut plain = Reader::new(Glyphs::Tmux);
            for (wrapped, text) in primary.iter().take(kept) {
                let line = Line {
                    unread_from: None,
                    cells: plain.row(text, info.cols, &tab),
                    wrapped: *wrapped,
                };
                history.push((line.text(), line.wrapped));
            }
        }
        Ok(Snapshot {
            rows: info.rows,
            cols: info.cols,
            cursor: info.cursor,
            pending_wrap: info.pending_wrap,
            cursor_visible: info.cursor_visible,
            autowrap: info.autowrap,
            origin: info.origin,
            alternate: info.alternate,
            application_cursor: info.application_cursor,
            application_keypad: info.application_keypad,
            bracketed_paste: info.bracketed_paste,
            synchronized_output: false,
            in_band_resize: false,
            focus_reporting: focus,
            kitty_keyboard_flags: 0,
            title: info.title,
            reports: self.pane.reports(),
            screen: lines,
            history,
        })
    }
}

impl Engine for Tmux {
    fn process(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.pane.output(bytes)
    }

    fn resize(&mut self, rows: u16, cols: u16) -> Result<(), String> {
        tmux(&[
            "resize-window",
            "-t",
            &self.session,
            "-x",
            &cols.to_string(),
            "-y",
            &rows.to_string(),
        ])?;
        self.rows = rows;
        self.cols = cols;
        Ok(())
    }

    fn snapshot(&mut self, history_rows: usize) -> Result<Snapshot, String> {
        let snapshot = self.read(history_rows)?;
        if (snapshot.rows, snapshot.cols) != (self.rows, self.cols) {
            return Err(format!(
                "tmux: the pane is {}x{}, not {}x{}",
                snapshot.rows, snapshot.cols, self.rows, self.cols
            ));
        }
        Ok(snapshot)
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

impl Drop for Tmux {
    fn drop(&mut self) {
        if self.killed {
            return;
        }
        let _ = tmux(&["kill-session", "-t", &self.session]);
        // The server exits with its last session, and leaves its socket.
        if SESSIONS.fetch_sub(1, Ordering::SeqCst) == 1
            && let Ok(socket) = socket()
        {
            let _ = fs::remove_file(socket);
            pane::tidy();
        }
    }
}
