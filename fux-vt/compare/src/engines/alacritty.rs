//! Alacritty's terminal core, alacritty_terminal, read into a snapshot.
//!
//! Driven: a `Term` fed through vte's `ansi::Processor`, with 100000 rows
//! of history whatever fux-vt keeps, and the kitty keyboard protocol on.
//! Replies (`Event::PtyWrite`) and titles (`Event::Title`, `ResetTitle`)
//! come through the event listener, which the `Term` owns, so it writes
//! them where the adapter can read them.
//!
//! Read: `grid()` by `Line` and `Column`, the active screen only. A cell's
//! text is its char and the zero-width chars in its `extra`; its width is
//! `WIDE_CHAR` (wide) or `WIDE_CHAR_SPACER` (tail); a row's soft-wrap flag
//! is `WRAPLINE` on its last cell. History is the rows above `Line(0)`,
//! down to `-history_size()`. Pending wrap is the cursor's
//! `input_needs_wrap`; the modes are `TermMode` bits.
//!
//! Normalized here, beside the rules every engine follows:
//!
//! - A tab leaves a `'\t'` in the blank cell it starts from, for copying
//!   text out; it reads as a blank.
//! - `LEADING_WIDE_CHAR_SPACER`, the spacer at the end of a row where a wide
//!   glyph that did not fit would have started, reads as a narrow blank.
//! - Synchronized updates are applied at once. vte's processor holds back
//!   everything after `CSI ? 2026 h` until `CSI ? 2026 l`, or until
//!   Alacritty's event loop sees its 150 ms timer run out; there is no event
//!   loop here, and the harness reads state, not frames. The processor's
//!   timer never runs.
//!
//! What it cannot tell: blink. Alacritty keeps no blink attribute; SGR 5
//! and 6 are dropped.
//!
//! A wide glyph on a one-column terminal panics alacritty_terminal: with
//! autowrap on, `input` writes the glyph in column 0 and then its tail in
//! column 1, which is not there (`replay --size 2x1 '\u{754c}'`). On one
//! column only, the adapter feeds a byte at a time, catches that panic and
//! finishes what `input` would have done (see `process_one_column`): the
//! glyph stays, wide with no tail, and the cursor waits on it to wrap.
//! fux-vt and Ghostty print nothing there.
//!
//! Where it differs from fux-vt while Ghostty agrees with fux-vt (replays
//! are `fux-vt-compare replay --engines alacritty,ghostty ...`):
//!
//! - `CSI = flags ; mode u` changes the active kitty keyboard flags but not
//!   the entry on top of the stack, so a later pop restores what was pushed
//!   before, or 0 (`--size 1x1 '\e[=4;1u\e[>3u\e[<u'`: 0, not 4).
//! - DECSTBM is checked (top below bottom) before it is clamped to the
//!   screen. A region that starts below the last row is kept, as an empty
//!   region at the bottom, where scrolling does nothing and origin mode
//!   homes to the last row (`--size 2x1 '\e[3;4r\e[?6h\e[5G'`,
//!   `--size 1x1 --history 50 '\e[3;10r\e[S'`); one that clamps to a single
//!   row is kept too (`--size 2x1 '\e[2;5r\e[99H\eM'`).
//! - Resetting DECOM (`CSI ? 6 l`) leaves the cursor where it is
//!   (`--size 1x3 '\e[2C\e[?6l'`).
//! - In origin mode, moves that keep or step the row (CUU, CUD, CNL, CPL,
//!   VPR, CHA, HPR) add the region's top a second time
//!   (`--size 4x3 '\e[2;4r\e[?6h\e[1B'`: row 3, not 2).
//! - CUU, CUD, CNL and CPL stop at the screen's edge, not at the scrolling
//!   region's margins (`--size 4x3 '\e[1;3r\e[99B'`,
//!   `--size 4x3 '\e[2;4r\e[4H\e[99A'`).
//! - A parameter of 0 moves by one in HPR, VPR and CHT
//!   (`--size 1x8 '\e[0a'`, `--size 1x8 '\e[0I'`, `--size 2x1 '\e[0e'`).
//! - A row's soft-wrap flag lives on its last cell, so printing over that
//!   cell clears it (`--size 4x3 'abcd\e[Hxyz'`).
//! - Only mode 1049 switches screens: 47 and 1047 are ignored
//!   (`--size 2x5 'ab\e[?47hX'`), and `CSI ? 1049 l` on the main screen
//!   restores no cursor (`--size 4x3 'ab\e[?1049l'`).
//! - No grapheme clusters: each code point is measured alone, so VS16 does
//!   not widen (`--size 1x8 '\u{2764}\u{fe0f}x'`), a ZWJ sequence is
//!   several glyphs (`--size 1x8 '\u{1f468}\u{200d}\u{1f469}x'`) and a
//!   regional-indicator pair two narrow cells
//!   (`--size 1x8 '\u{1f1fa}\u{1f1f8}x'`). A zero-width char with no glyph
//!   before it joins the blank cell under the cursor
//!   (`--size 1x3 '\u{301}'`).
//! - A reflow to fewer columns keeps the cursor on its screen row and
//!   pushes the rows the reflow adds above it into history, even with blank
//!   rows below (`--size 3x6 --history 100 'abcde\r\n$ ' resize:3x3`).
//! - Backspace in column 0 does nothing, so on one column it leaves a wrap
//!   pending (`--size 1x1 'a\x08'`).
//!
//! Where it agrees with fux-vt and Ghostty does not: EL 0, ECH and ICH keep
//! a pending wrap, EL 0 then erasing nothing (`--size 1x5 'abcde\e[K'`);
//! IL and DL leave the column alone (`--size 3x5 'ab\e[LX'`); erasing a row
//! clears its wrap flag (`--size 4x3 'abcd\e[H\e[2K'`). Raw bytes 0x80–0x9F
//! are taken as C1 controls (vte), where Ghostty prints U+FFFD
//! (`--size 1x4 'a\x90b'`).
use crate::engine::{Can, Engine, Kind, Setup, always};
use crate::snapshot::{self, Cell, Color, Line, Snapshot, Style, Width};
use alacritty_terminal::Term;
use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, Row};
use alacritty_terminal::index;
use alacritty_terminal::term::cell::{self, Flags};
use alacritty_terminal::term::{Config, TermMode};
use alacritty_terminal::vte::ansi::{self, NamedColor, Processor, Timeout};
use std::cell::RefCell;
use std::panic::{self, AssertUnwindSafe};
use std::rc::Rc;
use std::time::Duration;

/// History Alacritty keeps, in rows: far more than fux-vt's row limits.
const HISTORY_ROWS: usize = 100_000;

pub const KIND: Kind = Kind {
    name: "alacritty",
    about: "Alacritty's terminal core, alacritty_terminal",
    can: Can {
        blink: false,
        synchronized_output: false,
        in_band_resize: false,
        prompt: false,
        ..Can::ALL
    },
    panel: true,
    in_process: true,
    available: always,
    make,
};

fn make(setup: &Setup) -> Result<Box<dyn Engine>, String> {
    Ok(Box::new(Alacritty::new(setup.rows, setup.cols)?))
}

/// What the terminal says through its listener.
#[derive(Default)]
struct Heard {
    replies: Vec<u8>,
    title: String,
}

#[derive(Clone)]
struct Listener(Rc<RefCell<Heard>>);

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        let mut heard = self.0.borrow_mut();
        if let Event::PtyWrite(text) = event {
            heard.replies.extend_from_slice(text.as_bytes());
        } else if let Event::Title(title) = event {
            heard.title = title;
        } else if let Event::ResetTitle = event {
            heard.title.clear();
        }
    }
}

/// A synchronized-update timer that never runs, so nothing is held back.
#[derive(Default)]
struct Immediate;

impl Timeout for Immediate {
    fn set_timeout(&mut self, _: Duration) {}
    fn clear_timeout(&mut self) {}
    fn pending_timeout(&self) -> bool {
        false
    }
}

/// A terminal size, as `Term::new` and `Term::resize` take one.
struct Size {
    rows: usize,
    cols: usize,
}

impl Size {
    fn new(rows: u16, cols: u16) -> Result<Size, String> {
        if rows == 0 || cols == 0 {
            return Err(format!("alacritty: no {rows}x{cols} terminal"));
        }
        Ok(Size {
            rows: usize::from(rows),
            cols: usize::from(cols),
        })
    }
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

pub struct Alacritty {
    term: Term<Listener>,
    processor: Processor<Immediate>,
    heard: Rc<RefCell<Heard>>,
}

fn color(c: ansi::Color) -> Color {
    match c {
        ansi::Color::Spec(rgb) => Color::Rgb(rgb.r, rgb.g, rgb.b),
        ansi::Color::Indexed(index) => Color::Idx(index),
        ansi::Color::Named(named) => named_color(named),
    }
}

/// Named colours 0–15 are palette entries; Foreground and Background are
/// the defaults. The rest are a renderer's: SGR never stores them.
fn named_color(named: NamedColor) -> Color {
    let index = match named {
        NamedColor::Black | NamedColor::DimBlack => 0,
        NamedColor::Red | NamedColor::DimRed => 1,
        NamedColor::Green | NamedColor::DimGreen => 2,
        NamedColor::Yellow | NamedColor::DimYellow => 3,
        NamedColor::Blue | NamedColor::DimBlue => 4,
        NamedColor::Magenta | NamedColor::DimMagenta => 5,
        NamedColor::Cyan | NamedColor::DimCyan => 6,
        NamedColor::White | NamedColor::DimWhite => 7,
        NamedColor::BrightBlack => 8,
        NamedColor::BrightRed => 9,
        NamedColor::BrightGreen => 10,
        NamedColor::BrightYellow => 11,
        NamedColor::BrightBlue => 12,
        NamedColor::BrightMagenta => 13,
        NamedColor::BrightCyan => 14,
        NamedColor::BrightWhite => 15,
        NamedColor::Foreground
        | NamedColor::Background
        | NamedColor::Cursor
        | NamedColor::BrightForeground
        | NamedColor::DimForeground => return Color::Default,
    };
    Color::Idx(index)
}

fn cell(c: &cell::Cell) -> Cell {
    let f = c.flags;
    let style = Style {
        fg: color(c.fg),
        bg: color(c.bg),
        underline_color: c.underline_color().map_or(Color::Default, color),
        bold: f.contains(Flags::BOLD),
        dim: f.contains(Flags::DIM),
        italic: f.contains(Flags::ITALIC),
        underline: f.intersects(Flags::ALL_UNDERLINES),
        blink: false,
        inverse: f.contains(Flags::INVERSE),
        hidden: f.contains(Flags::HIDDEN),
        strikeout: f.contains(Flags::STRIKEOUT),
    };
    let width = if f.contains(Flags::WIDE_CHAR_SPACER) {
        Width::Tail
    } else if f.contains(Flags::WIDE_CHAR) {
        Width::Wide
    } else {
        Width::Narrow
    };
    // A link is one by its id and URI (`Hyperlink`'s equality); a link
    // without an id is given one of its own, `<n>_alacritty`.
    let link = c
        .hyperlink()
        .map(|l| (l.uri().to_owned(), format!("{}\n{}", l.id(), l.uri())));
    if f.contains(Flags::LEADING_WIDE_CHAR_SPACER) {
        return Cell::new("", Width::Narrow, style).linked(link);
    }
    let mut text = String::new();
    text.push(if c.c == '\t' { ' ' } else { c.c });
    text.extend(c.zerowidth().unwrap_or_default());
    Cell::new(&text, width, style).linked(link)
}

fn err(what: &str) -> impl Fn(std::num::TryFromIntError) -> String + '_ {
    move |e| format!("alacritty: {what}: {e}")
}

impl Alacritty {
    fn new(rows: u16, cols: u16) -> Result<Alacritty, String> {
        let heard = Rc::new(RefCell::new(Heard::default()));
        let config = Config {
            scrolling_history: HISTORY_ROWS,
            kitty_keyboard: true,
            ..Config::default()
        };
        let term = Term::new(config, &Size::new(rows, cols)?, Listener(Rc::clone(&heard)));
        Ok(Alacritty {
            term,
            processor: Processor::new(),
            heard,
        })
    }

    /// Row `y` of the active screen, or of history above it (negative).
    fn line(&self, y: i32) -> Line {
        let row = &self.term.grid()[index::Line(y)];
        let cells: &[cell::Cell] = &row[..];
        Line {
            unread_from: None,
            prompt: false,
            cells: cells.iter().map(cell).collect(),
            wrapped: cells
                .last()
                .is_some_and(|c| c.flags.contains(Flags::WRAPLINE)),
        }
    }

    fn read(&self, history_rows: usize) -> Result<Snapshot, String> {
        let grid = self.term.grid();
        let rows = u16::try_from(grid.screen_lines()).map_err(err("rows"))?;
        let cols = u16::try_from(grid.columns()).map_err(err("columns"))?;
        let screen = (0..i32::from(rows)).map(|y| self.line(y)).collect();
        let kept = i32::try_from(grid.history_size().min(history_rows)).map_err(err("history"))?;
        let history = (1..=kept)
            .rev()
            .map(|back| {
                let line = self.line(back.saturating_neg());
                (line.text(), line.wrapped)
            })
            .collect();
        let cursor = &grid.cursor;
        let mode = *self.term.mode();
        let kitty = [
            (TermMode::DISAMBIGUATE_ESC_CODES, 1),
            (TermMode::REPORT_EVENT_TYPES, 2),
            (TermMode::REPORT_ALTERNATE_KEYS, 4),
            (TermMode::REPORT_ALL_KEYS_AS_ESC, 8),
            (TermMode::REPORT_ASSOCIATED_TEXT, 16),
        ]
        .iter()
        .filter(|(bit, _)| mode.contains(*bit))
        .fold(0, |flags, (_, value)| flags | value);
        let heard = self.heard.borrow();
        Ok(Snapshot {
            rows,
            cols,
            cursor: (
                u16::try_from(cursor.point.line.0).map_err(err("cursor"))?,
                u16::try_from(cursor.point.column.0).map_err(err("cursor"))?,
            ),
            pending_wrap: cursor.input_needs_wrap,
            cursor_visible: mode.contains(TermMode::SHOW_CURSOR),
            autowrap: mode.contains(TermMode::LINE_WRAP),
            origin: mode.contains(TermMode::ORIGIN),
            alternate: mode.contains(TermMode::ALT_SCREEN),
            application_cursor: mode.contains(TermMode::APP_CURSOR),
            application_keypad: mode.contains(TermMode::APP_KEYPAD),
            bracketed_paste: mode.contains(TermMode::BRACKETED_PASTE),
            synchronized_output: false,
            in_band_resize: false,
            focus_reporting: mode.contains(TermMode::FOCUS_IN_OUT),
            kitty_keyboard_flags: kitty,
            title: heard.title.clone(),
            reports: snapshot::reports(&heard.replies),
            screen,
            history,
        })
    }
}

/// Runs `f`, catching a panic, with the panic hook silenced: the panic's
/// message comes back as the error.
fn quietly(f: impl FnOnce()) -> Result<(), String> {
    let hook = panic::take_hook();
    panic::set_hook(Box::new(|_| {}));
    let result = panic::catch_unwind(AssertUnwindSafe(f));
    panic::set_hook(hook);
    result.map_err(|payload| {
        payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_owned()))
            .unwrap_or_else(|| "a panic".to_owned())
    })
}

impl Alacritty {
    /// Feeds a one-column terminal a byte at a time, so a wide glyph's
    /// panic stops at the glyph, and repairs what the panic left, as the
    /// rest of `input` would have: the cursor in column 1 (now on the
    /// glyph, waiting to wrap), the template still marking a tail, and the
    /// row counting two cells written, which panics the next time it is
    /// reset. The processor is replaced, as it still holds the glyph's
    /// bytes; it was in the ground state, as a fresh one is.
    fn process_one_column(&mut self, bytes: &[u8]) -> Result<(), String> {
        for byte in bytes {
            let (term, processor) = (&mut self.term, &mut self.processor);
            let Err(why) = quietly(|| processor.advance(term, std::slice::from_ref(byte))) else {
                continue;
            };
            let grid = self.term.grid_mut();
            let last = grid.columns().saturating_sub(1);
            if grid.cursor.point.column.0 <= last {
                return Err(format!("alacritty: {why}"));
            }
            grid.cursor.point.column = index::Column(last);
            grid.cursor.input_needs_wrap = true;
            grid.cursor.template.flags.remove(Flags::WIDE_CHAR_SPACER);
            let line = grid.cursor.point.line;
            let row = &mut grid[line];
            let cells = row[..].to_vec();
            let written = cells.len();
            *row = Row::from_vec(cells, written);
            self.processor = Processor::new();
        }
        Ok(())
    }
}

impl Engine for Alacritty {
    fn process(&mut self, bytes: &[u8]) -> Result<(), String> {
        if self.term.columns() == 1 {
            return self.process_one_column(bytes);
        }
        self.processor.advance(&mut self.term, bytes);
        Ok(())
    }

    fn resize(&mut self, rows: u16, cols: u16) -> Result<(), String> {
        self.term.resize(Size::new(rows, cols)?);
        Ok(())
    }

    fn snapshot(&mut self, history_rows: usize) -> Result<Snapshot, String> {
        self.read(history_rows)
    }

    fn replies(&self) -> Vec<u8> {
        self.heard.borrow().replies.clone()
    }
}
