//! WezTerm's terminal model, wezterm-term, read into a snapshot.
//!
//! Tested at wezterm commit `cab25161054c` (the revision `Cargo.lock`
//! pins; `Cargo.toml` names no `rev`).
//!
//! How it is driven: `Terminal::new` with a configuration of its own
//! (100000 rows of scrollback whatever fux-vt keeps, the kitty keyboard
//! protocol on, and Unicode version 14, wezterm's newest width rules,
//! as mode 2027 is on in Ghostty), then `advance_bytes` and `resize`.
//! Replies go to the writer through a thread wezterm starts, so before
//! each snapshot a marker is pasted (`send_paste`) and waited for: every
//! reply sent before it has then arrived. The marker, and the paste
//! brackets around it, are taken out of the replies.
//!
//! How it is read: the visible lines and the history above them from the
//! active screen (`phys_row`, `lines_in_phys_range`), each cell from
//! `visible_cells` (a cell of width 2 is wide, the one after it its tail),
//! each row's wrap flag from `last_cell_was_wrapped`, and the modes from
//! their getters. The title is the last one an `Alert::WindowTitleChanged`
//! names (OSC 0 and 2), as fux-vt reports titles: `get_title` prefers an
//! OSC 1 icon name and starts as "wezterm". The kitty keyboard flags are
//! read from the encoding's `Debug` form, as their type is not exported.
//! Direct colours are stored as `n / 255.0` and read back exactly.
//!
//! What it cannot tell: pending wrap. wezterm keeps it in a private
//! `wrap_next`, with the cursor on the last glyph printed.
//!
//! Where it differs from fux-vt and Ghostty, by its own choice or defect
//! (each a `replay --engines wezterm,ghostty` command):
//!
//! - A wide glyph that reaches the end of a row leaves the cursor on its
//!   first column, not the last: `--size 1x4 'ab界'`.
//! - A wide glyph is printed in the last column, cut in half, rather than
//!   wrapped first: `--size 1x5 '界界界'`, `--size 1x1 '\e[999C界\r'`.
//! - CUP, CHA and HPA can put the cursor one past the last column (kept
//!   for reflow); a glyph printed there is off the screen, and HTS there
//!   panics in wezterm-term (`TabStop::set_tab_stop`), which this engine
//!   reports as an error: `--size 1x1 '\e[2;99f-'`, `--size 1x3
//!   '\e[99;99f\e[2D'`, `--size 1x3 '\e[99G\eH'`. Such a cursor is read
//!   in the last column.
//! - A glyph printed in the last column with autowrap off sets no pending
//!   wrap, so after `CSI ? 7 h` the next glyph overwrites it: `--size 2x1
//!   '\e[?7lc\e[?7h#'`.
//! - A row's wrap flag is an attribute of its last cell, so writing over
//!   that cell clears it: `--size 3x2 'abc\e[1;2HZ'`. SD keeps the flag on
//!   a row whose continuation it pushed off the screen: `--size 2x1
//!   'lo\e[1T'`.
//! - Erased and scrolled-in blanks take the whole pen but underline,
//!   overline and strikeout (foreground, underline colour, bold, dim,
//!   italic, blink, inverse, hidden), not only the background: `--size 1x2
//!   'da\e[41;49;2mo'`.
//! - In origin mode a line feed or a wrap moves the cursor down by one plus
//!   the top margin: `--size 6x3 '\e[2;6r\e[?6h\nX'`.
//! - A CSI sequence whose last parameter is empty is ignored: `--size 3x1
//!   '\e[2;r\e[?6h\e[0;7H'`, `--size 1x1 '\e[=1;u'`.
//! - DECSTR also turns autowrap on, DECCKM, DECKPAM and SGR off, and leaves
//!   the alternate screen, as VT510 describes it: `--size 2x5
//!   'ab\e[1m\e[!pX'`, `--size 2x4 '\e[?7l\e[!p'`, `--size 2x4
//!   '\e[?1049h\e[!p'`.
//! - `CSI ? 1049 l` outside the alternate screen does not restore the
//!   cursor (`--size 1x2 'd\e[?1049l'`); `CSI ? 1049 h` inside it does
//!   nothing (`--size 1x1 '\e[?1049hx\e[?1049h'`).
//! - REP repeats the cell left of the cursor, whatever was printed, and
//!   wraps by itself, even a wide glyph into the last column: `--size 1x3
//!   '\e[2b'`, `--size 1x3 '界\e[1b'`.
//! - A truncated UTF-8 sequence becomes U+FFFD and takes the byte that cut
//!   it short with it: `--size 1x8 '\xc3xy'` shows `�y`.
//! - Raw C1 bytes are controls (0x9b is CSI, 0x90 DCS): `--size 1x8
//!   '1\x9bPq4'`.
//! - Graphemes are joined within one write only, and a zero-width grapheme
//!   on its own is dropped, so a combining mark in a later write is lost:
//!   `--size 2x6 'e' '\u{301}x'`.
//! - A keycap sequence is narrow: `--size 2x6 '1\u{fe0f}\u{20e3}x'`.
//! - Reflow puts a cursor in column 0 of a continuation row at the end of
//!   the row before: `--size 1x1 --history 10000 --newline-before-resize
//!   'llo ' resize:2x3`.
use crate::engine::{Can, Engine, Kind, Setup, always};
use crate::snapshot::{self, Cell, Color, Line, Snapshot, Style, Width};
use std::any::Any;
use std::io;
use std::panic::{self, AssertUnwindSafe};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Duration;
use wezterm_term::color::{ColorAttribute, ColorPalette, SrgbaTuple};
use wezterm_term::{
    Alert, AlertHandler, Blink, CellAttributes, CursorPosition, Intensity, LATEST_UNICODE_VERSION,
    Terminal, TerminalConfiguration, TerminalSize, Underline, UnicodeVersion,
};

/// History WezTerm keeps, in rows: far more than a case can fill.
const HISTORY_ROWS: usize = 100_000;

/// What is pasted to learn that every earlier reply has arrived.
const SYNC: &str = "\u{1}fux-vt-compare sync\u{1}";
const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

/// How long a sync may take before the engine is said to have failed.
const SYNC_WAIT: Duration = Duration::from_secs(10);

#[derive(Debug)]
struct Config;

impl TerminalConfiguration for Config {
    fn scrollback_size(&self) -> usize {
        HISTORY_ROWS
    }

    fn color_palette(&self) -> ColorPalette {
        ColorPalette::default()
    }

    fn enable_kitty_keyboard(&self) -> bool {
        true
    }

    fn unicode_version(&self) -> UnicodeVersion {
        LATEST_UNICODE_VERSION
    }
}

#[derive(Default)]
struct Heard {
    bytes: Vec<u8>,
    syncs: u64,
}

/// The replies, as WezTerm's writer thread hands them over.
#[derive(Default)]
struct Replies {
    heard: Mutex<Heard>,
    arrived: Condvar,
}

struct Writer(Arc<Replies>);

impl io::Write for Writer {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut heard = self.0.heard.lock().unwrap_or_else(PoisonError::into_inner);
        heard.bytes.extend_from_slice(buf);
        let sync = SYNC.as_bytes();
        while let Some(at) = (0..heard.bytes.len())
            .find(|&i| heard.bytes.get(i..i.saturating_add(sync.len())) == Some(sync))
        {
            let mut start = at;
            let mut end = at.saturating_add(sync.len());
            if let Some(before) = at.checked_sub(PASTE_START.len())
                && heard.bytes.get(before..at) == Some(PASTE_START)
            {
                start = before;
            }
            let after = end.saturating_add(PASTE_END.len());
            if heard.bytes.get(end..after) == Some(PASTE_END) {
                end = after;
            }
            let tail = heard.bytes.get(end..).unwrap_or_default().to_vec();
            heard.bytes.truncate(start);
            heard.bytes.extend_from_slice(&tail);
            heard.syncs = heard.syncs.saturating_add(1);
        }
        drop(heard);
        self.0.arrived.notify_all();
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// The last window title set, as fux-vt reports titles.
struct Titles(Arc<Mutex<Option<String>>>);

impl AlertHandler for Titles {
    fn alert(&mut self, alert: Alert) {
        if let Alert::WindowTitleChanged(title) = alert {
            *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(title);
        }
    }
}

pub struct Wezterm {
    terminal: Terminal,
    replies: Arc<Replies>,
    title: Arc<Mutex<Option<String>>>,
    syncs: u64,
    /// Why it stopped: wezterm-term panicked, and is not asked again.
    broken: Option<String>,
}

/// A channel of a direct colour, stored as `n as f32 / 255.0`, back as `n`.
fn channel(v: f32) -> u8 {
    (0..=u8::MAX)
        .find(|&n| (f32::from(n) / 255.0).to_bits() == v.to_bits())
        .unwrap_or(0)
}

fn color(c: ColorAttribute) -> Color {
    match c {
        ColorAttribute::Default => Color::Default,
        ColorAttribute::PaletteIndex(index) => Color::Idx(index),
        ColorAttribute::TrueColorWithDefaultFallback(SrgbaTuple(r, g, b, _))
        | ColorAttribute::TrueColorWithPaletteFallback(SrgbaTuple(r, g, b, _), _) => {
            Color::Rgb(channel(r), channel(g), channel(b))
        }
    }
}

fn style(a: &CellAttributes) -> Style {
    Style {
        fg: color(a.foreground()),
        bg: color(a.background()),
        underline_color: color(a.underline_color()),
        bold: a.intensity() == Intensity::Bold,
        dim: a.intensity() == Intensity::Half,
        italic: a.italic(),
        underline: a.underline() != Underline::None,
        blink: a.blink() != Blink::None,
        inverse: a.reverse(),
        hidden: a.invisible(),
        strikeout: a.strikethrough(),
    }
}

fn line(row: &wezterm_term::Line, cols: usize) -> Line {
    let mut cells = vec![Cell::new("", Width::Narrow, Style::default()); cols];
    for c in row.visible_cells() {
        let x = c.cell_index();
        let wide = c.width() > 1;
        if let Some(slot) = cells.get_mut(x) {
            let width = if wide { Width::Wide } else { Width::Narrow };
            *slot = Cell::new(c.str(), width, style(c.attrs()));
        }
        if wide && let Some(slot) = cells.get_mut(x.saturating_add(1)) {
            *slot = Cell::new("", Width::Tail, Style::default());
        }
    }
    Line {
        unread_from: None,
        cells,
        wrapped: row.last_cell_was_wrapped(),
    }
}

/// The kitty keyboard flags, read from the encoding's `Debug` form: the
/// flags' type is not exported.
fn kitty_flags(t: &Terminal) -> Result<u8, String> {
    let encoding = format!("{:?}", t.get_keyboard_encoding());
    let Some(inner) = encoding
        .strip_prefix("Kitty(")
        .and_then(|rest| rest.strip_suffix(')'))
    else {
        return Ok(0);
    };
    let mut bits = 0u8;
    for name in inner.split(" | ") {
        bits |= match name {
            "DISAMBIGUATE_ESCAPE_CODES" => 1,
            "REPORT_EVENT_TYPES" => 2,
            "REPORT_ALTERNATE_KEYS" => 4,
            "REPORT_ALL_KEYS_AS_ESCAPE_CODES" => 8,
            "REPORT_ASSOCIATED_TEXT" => 16,
            "NONE" | "(empty)" => 0,
            other => other
                .strip_prefix("0x")
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
                .ok_or_else(|| format!("wezterm: kitty flags: {encoding}"))?,
        };
    }
    Ok(bits)
}

fn size(rows: u16, cols: u16) -> TerminalSize {
    TerminalSize {
        rows: usize::from(rows),
        cols: usize::from(cols),
        ..TerminalSize::default()
    }
}

pub const KIND: Kind = Kind {
    name: "wezterm",
    about: "WezTerm's terminal model, wezterm-term, from git",
    can: Can {
        pending_wrap: false,
        ..Can::ALL
    },
    panel: true,
    in_process: true,
    available: always,
    make,
};

fn make(setup: &Setup) -> Result<Box<dyn Engine>, String> {
    let replies = Arc::new(Replies::default());
    let title = Arc::new(Mutex::new(None));
    let mut terminal = Terminal::new(
        size(setup.rows, setup.cols),
        Arc::new(Config),
        "fux-vt-compare",
        "0.0.0",
        Box::new(Writer(Arc::clone(&replies))),
    );
    terminal.set_notification_handler(Box::new(Titles(Arc::clone(&title))));
    Ok(Box::new(Wezterm {
        terminal,
        replies,
        title,
        syncs: 0,
        broken: None,
    }))
}

fn message(payload: &(dyn Any + Send)) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("?")
}

impl Wezterm {
    /// Runs `f` on the terminal, a panic in it an error, and every call
    /// after it too: the panic may have left the model half changed.
    fn guarded<T>(&mut self, what: &str, f: impl FnOnce(&mut Terminal) -> T) -> Result<T, String> {
        if let Some(why) = &self.broken {
            return Err(why.clone());
        }
        let terminal = &mut self.terminal;
        panic::catch_unwind(AssertUnwindSafe(|| f(terminal))).map_err(|payload| {
            let why = format!("wezterm: panicked in {what}: {}", message(payload.as_ref()));
            self.broken = Some(why.clone());
            why
        })
    }

    /// Every reply so far: replies go through a writer thread, so a paste
    /// is sent after them and waited for.
    fn replies(&mut self) -> Result<Vec<String>, String> {
        self.terminal
            .send_paste(SYNC)
            .map_err(|e| format!("wezterm: sync: {e}"))?;
        self.syncs = self.syncs.saturating_add(1);
        let want = self.syncs;
        let (heard, wait) = self
            .replies
            .arrived
            .wait_timeout_while(
                self.replies
                    .heard
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner),
                SYNC_WAIT,
                |h| h.syncs < want,
            )
            .unwrap_or_else(PoisonError::into_inner);
        let reports = snapshot::reports(&heard.bytes);
        drop(heard);
        if wait.timed_out() {
            return Err("wezterm: replies did not arrive".into());
        }
        Ok(reports)
    }
}

fn u16_of(n: usize, what: &str) -> Result<u16, String> {
    u16::try_from(n).map_err(|e| format!("wezterm: {what}: {e}"))
}

impl Engine for Wezterm {
    fn process(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.guarded("advance_bytes", |t| t.advance_bytes(bytes))
    }

    fn resize(&mut self, rows: u16, cols: u16) -> Result<(), String> {
        self.guarded("resize", |t| t.resize(size(rows, cols)))
    }

    fn snapshot(&mut self, history_rows: usize) -> Result<Snapshot, String> {
        if let Some(why) = &self.broken {
            return Err(why.clone());
        }
        let reports = self.replies()?;
        let t = &self.terminal;
        let s = t.screen();
        let (rows, cols) = (s.physical_rows, s.physical_cols);
        let top = s.phys_row(0);
        let mut screen: Vec<Line> = s
            .lines_in_phys_range(top..top.saturating_add(rows))
            .iter()
            .map(|row| line(row, cols))
            .collect();
        screen.resize(
            rows,
            Line {
                unread_from: None,
                cells: vec![Cell::new("", Width::Narrow, Style::default()); cols],
                wrapped: false,
            },
        );
        let history = s
            .lines_in_phys_range(top.saturating_sub(history_rows)..top)
            .iter()
            .map(|row| {
                let l = line(row, cols);
                (l.text(), l.wrapped)
            })
            .collect();
        let at = t.cursor_pos();
        let cursor = (
            u16::try_from(at.y).map_err(|e| format!("wezterm: cursor: {e}"))?,
            u16_of(at.x.min(cols.saturating_sub(1)), "cursor")?,
        );
        let title = self
            .title
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .unwrap_or_default();
        Ok(Snapshot {
            rows: u16_of(rows, "rows")?,
            cols: u16_of(cols, "cols")?,
            cursor,
            pending_wrap: false,
            cursor_visible: at.visibility == CursorPosition::default().visibility,
            autowrap: t.dec_auto_wrap_enabled(),
            origin: t.dec_origin_mode_enabled(),
            alternate: t.is_alt_screen_active(),
            application_cursor: t.application_cursor_keys_enabled(),
            application_keypad: t.application_keypad_enabled(),
            bracketed_paste: t.bracketed_paste_enabled(),
            focus_reporting: t.focus_tracking_enabled(),
            kitty_keyboard_flags: kitty_flags(t)?,
            title,
            reports,
            screen,
            history,
        })
    }
}
