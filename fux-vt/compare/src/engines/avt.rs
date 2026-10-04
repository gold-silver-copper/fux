//! asciinema's virtual terminal, avt, read into a snapshot.
//!
//! avt is fed text, not bytes (`Vt::feed_str`). The adapter decodes the
//! bytes itself: an incomplete UTF-8 sequence at the end of one `process`
//! call is kept and finished by the next, and every invalid sequence is
//! one U+FFFD, as `String::from_utf8_lossy` makes it. That decoding is the
//! adapter's, not avt's.
//!
//! avt keeps 100 000 rows of scrollback here, whatever fux-vt keeps.
//!
//! Read: every cell of `Vt::view` (its char, its width of 1, 2 or 0 for
//! the second half of a wide glyph, and its pen); the cursor (`Vt::cursor`,
//! whose column is one past the last while a wrap is pending, and whose
//! `visible` is mode 25); `Vt::cursor_key_app_mode`; and history from
//! `Vt::lines`, the rows before the view. avt keeps no public wrap flag
//! or mode accessor, so two more come from its own serialisations:
//!
//! - A row's wrap flag is whether its `Debug` form, the row's text with
//!   `⏎` appended when it wraps, differs from that of its text alone.
//! - Origin mode, autowrap and the alternate screen are the last of
//!   `CSI ? 6`, `7` and `1047` `h` or `l` in `Vt::dump`, the sequence avt
//!   writes to recreate its state, which ends by setting each mode that
//!   differs from its default.
//!
//! What avt does not keep at all: an underline colour, hidden (SGR 8),
//! the application keypad, bracketed paste, focus reporting, the kitty
//! keyboard flags, a title (it parses OSC and drops it), and replies (it
//! answers nothing).
//!
//! Its quirks: where it differs from fux-vt and Ghostty both, or from
//! Ghostty where fux-vt has an inherited choice of its own. Each replays
//! with `fux-vt-compare replay --engines avt,ghostty`:
//!
//! - With autowrap off, a glyph in the last column leaves no wrap pending:
//!   avt keeps the cursor on it (`--size 1x1 '\e[?7li'`).
//! - Saving the cursor drops a pending wrap: avt saves the last column
//!   instead (`--size 1x5 'abcde\e7\e8'`). And DECSC saves autowrap, which
//!   DECRC restores (`'\e[?7l\e7\e[?7h\e8'`).
//! - DECSTR shows the cursor and resets the pen, as DEC's soft reset does
//!   (`'\e[?25l\e[!p'`, `--size 2x5 'ab\e[1m\e[!pX'`).
//! - SGR 58 is unknown, so its colour's arguments read as SGRs of their
//!   own: `58;5;9` sets blink and strikeout (`'\e[58;5;9mX'`). SGR 6 is not
//!   blink (`'\e[6mX'`), and SGR 21 is normal intensity (`'\e[1;21mX'`).
//! - Bold and dim are one intensity, so the later of SGR 1 and 2 wins, as
//!   in fux-vt; Ghostty keeps both (`'\e[1;2mX'`).
//! - The blanks it makes, erasing, inserting, deleting or scrolling, take
//!   the whole pen, where xterm's and fux-vt's take its colours alone
//!   (`--size 2x3 'abc\r\ndef\e[2;41m\n'`), so it does not vote on a
//!   blank's attributes (`engine::Blanks`).
//! - A cell holds one char: a combining mark takes a cell of its own
//!   (`'e\u{301}'`), and an emoji modifier replaces the emoji before it
//!   (`--size 1x2 '\u{1f44d}\u{1f3fd}'`).
//! - A wide glyph on a one-column screen marks the row wrapped and moves
//!   the cursor down, printing nothing (`--size 2x1 '\u{754c}'`).
//! - CHT 0 moves one tab stop, as CHT 1 does (`--size 1x5 '\e[0I'`).
//! - DECSTBM with a bottom past the screen is ignored, not clamped
//!   (`--size 5x1 '\e[4;99r\e[?6h'`).
//! - When a scroll region above the bottom of the screen scrolls, the row
//!   that wrapped onto its last row loses its wrap flag (`--size 6x1
//!   '\e[1;5r\e[3Habcd'`).
//! - Shrinking the screen drops the rows below the cursor first, even the
//!   rest of the cursor's own wrapped line (`--size 3x2 --history 10000
//!   'abcde\e[A' resize:2x2`).
//! - A printed space with the default pen is a blank to avt, so a reflow
//!   drops trailing spaces, and the wraps they made (`--size 1x3 --history
//!   10000 --newline-before-resize 'a  ' resize:2x2`).
//! - UTF-8-encoded C1 controls are controls, since avt is fed chars: U+009B
//!   is a CSI (`--size 1x5 'a\xc2\x9b31mb'`).
use crate::engine::{Blanks, Can, Engine, Kind, Setup, always};
use crate::snapshot::{Cell, Color, Line, Snapshot, Style, Width};

/// Scrollback avt keeps, in rows: far more than a case writes.
const SCROLLBACK: usize = 100_000;

pub const KIND: Kind = Kind {
    name: "avt",
    about: "asciinema's virtual terminal, avt",
    can: Can {
        underline_color: false,
        hidden: false,
        application_keypad: false,
        bracketed_paste: false,
        focus_reporting: false,
        kitty_keyboard_flags: false,
        title: false,
        reports: false,
        synchronized_output: false,
        in_band_resize: false,
        link_uri: false,
        link_group: false,
        prompt: false,
        ..Can::ALL
    },
    blanks: Blanks {
        attributes: false,
        ..Blanks::XTERM
    },
    panel: true,
    in_process: true,
    available: always,
    make,
};

fn make(setup: &Setup) -> Result<Box<dyn Engine>, String> {
    let vt = avt::Vt::builder()
        .size(usize::from(setup.cols), usize::from(setup.rows))
        .scrollback_limit(SCROLLBACK)
        .build();
    Ok(Box::new(Avt {
        vt,
        tail: Vec::new(),
    }))
}

pub struct Avt {
    vt: avt::Vt,
    /// The start of a UTF-8 sequence the last call ended in.
    tail: Vec<u8>,
}

fn color(c: Option<avt::Color>) -> Color {
    match c {
        None => Color::Default,
        Some(avt::Color::Indexed(n)) => Color::Idx(n),
        Some(avt::Color::RGB(rgb)) => Color::Rgb(rgb.r, rgb.g, rgb.b),
    }
}

fn cell(c: &avt::Cell) -> Cell {
    let pen = c.pen();
    let style = Style {
        fg: color(pen.foreground()),
        bg: color(pen.background()),
        underline_color: Color::Default,
        bold: pen.is_bold(),
        dim: pen.is_faint(),
        italic: pen.is_italic(),
        underline: pen.is_underline(),
        blink: pen.is_blink(),
        inverse: pen.is_inverse(),
        hidden: false,
        strikeout: pen.is_strikethrough(),
    };
    let width = match c.width() {
        0 => Width::Tail,
        2 => Width::Wide,
        _ => Width::Narrow,
    };
    Cell::new(&c.char().to_string(), width, style)
}

/// A row, `cols` cells wide.
fn line(l: &avt::Line, cols: usize) -> Line {
    let mut cells: Vec<Cell> = l.cells().iter().take(cols).map(cell).collect();
    cells.resize(cols, Cell::new("", Width::Narrow, Style::default()));
    Line {
        unread_from: None,
        prompt: false,
        cells,
        wrapped: format!("{l:?}") != format!("{:?}", l.text()),
    }
}

/// Whether `mode` is set at the end of avt's dump, or `default` if the
/// dump never names it.
fn mode(dump: &str, mode: &str, default: bool) -> bool {
    let set = dump.rfind(&format!("\x1b[?{mode}h"));
    let reset = dump.rfind(&format!("\x1b[?{mode}l"));
    match (set, reset) {
        (None, None) => default,
        (Some(_), None) => true,
        (None, Some(_)) => false,
        (Some(s), Some(r)) => s > r,
    }
}

impl Engine for Avt {
    fn process(&mut self, bytes: &[u8]) -> Result<(), String> {
        if self.tail.is_empty()
            && let Ok(text) = std::str::from_utf8(bytes)
        {
            self.vt.feed_str(text);
            return Ok(());
        }
        let mut input = std::mem::take(&mut self.tail);
        input.extend_from_slice(bytes);
        let mut text = String::with_capacity(input.len());
        let mut chunks = input.utf8_chunks().peekable();
        while let Some(chunk) = chunks.next() {
            text.push_str(chunk.valid());
            let bad = chunk.invalid();
            if bad.is_empty() {
                continue;
            }
            let unfinished = chunks.peek().is_none()
                && std::str::from_utf8(bad).is_err_and(|e| e.error_len().is_none());
            if unfinished {
                self.tail = bad.to_vec();
            } else {
                text.push('\u{fffd}');
            }
        }
        self.vt.feed_str(&text);
        Ok(())
    }

    fn resize(&mut self, rows: u16, cols: u16) -> Result<(), String> {
        self.vt.resize(usize::from(cols), usize::from(rows));
        Ok(())
    }

    fn snapshot(&mut self, history_rows: usize) -> Result<Snapshot, String> {
        let (cols, rows) = self.vt.size();
        let size = |n: usize| u16::try_from(n).map_err(|e| format!("avt: size: {e}"));
        let cursor = self.vt.cursor();
        // A cursor waiting to wrap is kept one past the last column.
        let pending_wrap = cursor.col >= cols;
        let screen = self.vt.view().map(|l| line(l, cols)).collect();
        let kept = self.vt.lines().count().saturating_sub(rows);
        let history = self
            .vt
            .lines()
            .take(kept)
            .skip(kept.saturating_sub(history_rows))
            .map(|l| {
                let line = line(l, cols);
                (line.text(), line.wrapped)
            })
            .collect();
        let dump = self.vt.dump();
        Ok(Snapshot {
            rows: size(rows)?,
            cols: size(cols)?,
            cursor: (
                size(cursor.row)?,
                size(cursor.col.min(cols.saturating_sub(1)))?,
            ),
            pending_wrap,
            cursor_visible: cursor.visible,
            autowrap: mode(&dump, "7", true),
            origin: mode(&dump, "6", false),
            alternate: mode(&dump, "1047", false),
            application_cursor: self.vt.cursor_key_app_mode(),
            application_keypad: false,
            bracketed_paste: false,
            synchronized_output: false,
            in_band_resize: false,
            focus_reporting: false,
            kitty_keyboard_flags: 0,
            title: String::new(),
            reports: Vec::new(),
            screen,
            history,
        })
    }
}
