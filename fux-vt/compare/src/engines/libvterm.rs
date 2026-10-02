//! libvterm 0.3.3, the engine of Neovim's and Vim's terminals, compiled
//! from its release source (`build.rs`, with `run.sh` fetching it) and
//! read into a snapshot through a small C shim, `libvterm_shim.c`, which
//! gives the Rust side plain structs of ints instead of libvterm's
//! bitfields.
//!
//! How it is driven: one `VTerm` in UTF-8 mode with its screen layer, the
//! alternate screen enabled, reflow on (as Neovim sets it; libvterm reflows
//! whatever `Setup::reflow` says), damage merged as Neovim merges it, and a
//! hard reset. Output goes in through `vterm_input_write`, resizes through
//! `vterm_set_size`. Replies are collected by an output callback: the
//! buffered `vterm_output_read` the C API also offers is deprecated, and
//! silently drops a reply that does not fit its 4 KiB.
//!
//! How it is read:
//!
//! - Cells through `vterm_screen_get_cell`. A cell whose first codepoint is
//!   `-1` is the second half of a wide glyph; an erased cell holds none. A
//!   cell holds a base character and at most five combining ones. A
//!   codepoint that is not a `char` (libvterm decodes five- and six-byte
//!   UTF-8) reads as U+FFFD.
//! - Colours carry a "default" flag (`VTERM_COLOR_DEFAULT_FG` or `_BG`)
//!   over an RGB value; flagged, a colour is `Color::Default`. SGR 30–37
//!   and 90–97 are palette indices 0–15.
//! - `get_cell` folds DECSCNM (reverse video for the whole screen) into
//!   every cell's reverse attribute; the shim folds it back out, as the
//!   cell's own attribute is what is compared.
//! - libvterm keeps soft wrap on the row that continues, not on the row
//!   that wraps: row y is soft-wrapped when row y+1 is a continuation
//!   (`vterm_state_get_lineinfo`).
//! - The cursor from `vterm_state_get_cursorpos`; while a wrap is pending
//!   it is already in the last column.
//! - Cursor visibility, the alternate screen and the title from the
//!   screen's `settermprop` callback. A title comes in fragments, which
//!   the shim joins.
//! - The public API has no getter for the modes, nor for the pending wrap
//!   (DECRQM answers for some modes, but not 66, and asking means writing
//!   to the terminal). The shim reads them from `VTermState` through
//!   `vterm_internal.h`, the header of the very source it is compiled
//!   with: `at_phantom`, and `mode.autowrap`, `origin`, `cursor`
//!   (DECCKM), `keypad`, `bracketpaste` and `report_focus`.
//! - History: the screen's `sb_pushline` and `sb_popline` callbacks keep
//!   up to [`HISTORY_ROWS`] pushed rows in the shim (without their
//!   trailing plain blanks), and give rows back when a screen grows
//!   (libvterm reflows only with `sb_popline` set). `sb_clear` (ED 3)
//!   empties it. libvterm says nothing of a pushed row's soft wrap, and
//!   moves its continuation flags before it scrolls, so the shim stands
//!   between the state and the screen (`putglyph`, `erase`, `scrollrect`)
//!   and keeps the flags from before each scroll.
//!
//! What it cannot tell: dim (no SGR 2), underline colour (no SGR 58), and
//! kitty keyboard flags (no kitty keyboard protocol).
//!
//! Where libvterm would crash or hang, the shim steps in; each step is
//! shown by a `fux-vt-compare replay --engines libvterm,ghostty` with these
//! arguments:
//!
//! - A resize that loses the cursor (on the last row of a wrapped line too
//!   tall for the new screen) aborts the process ("screen_resize failed to
//!   update cursor position"). `build.rs` turns that `abort` into a
//!   return, after which libvterm puts the cursor at (0, 0), and the
//!   line's other rows are gone: `--size 2x4 abcdefgh resize:2x1`.
//! - A wide glyph whose second half would be past the row (in a
//!   one-column terminal, or repeated by REP in the last column) is written
//!   through a null pointer. The shim drops it: `--size 2x1 '中'`,
//!   `--size 2x4 'a中\e[b'`. In one column libvterm also moves to the next
//!   line before dropping it.
//! - REP with no glyph printed yet steps the cursor by a width of 0
//!   forever. The shim ignores it: `--size 1x4 '\e[b'`.
//! - A resize when the top row continues a line that scrolled off reads
//!   before the screen's buffer, looking for the line's start. The shim
//!   makes the top row the start of its line first, so the wrap from
//!   history into the screen is lost:
//!   `--history 10 --size 2x3 abcdefg resize:4x3`.
//! - DECRC after a resize puts the cursor back where DECSC saved it,
//!   unchecked, past a smaller screen. The shim keeps the saved cursor on
//!   the screen: `--size 3x3 '\e[3;3H\e7' resize:2x2 '\e8X'`.
//! - Not caught: DECDWL in a one-column terminal makes rows of width 0,
//!   and the cursor goes to column -1, which the adapter reports as an
//!   error: `--size 1x1 '\e#6\e[I'`. No generator writes DECDWL.
//!
//! Its quirks and choices, where Ghostty agrees with fux-vt and libvterm
//! does not, with the same command:
//!
//! - A scroll by the whole height of its region erases the region and
//!   pushes nothing to history, so a one-row terminal keeps no history:
//!   `--history 10 --size 1x2 abcd`, `--history 10 --size 2x3
//!   'a\r\nb\e[2S'`. Lines that DL takes off the top row do go to history:
//!   `--history 10 --size 2x3 'x\r\ny\r\na\r\nb\e[H\e[M'`.
//! - A resize in the alternate screen pushes the primary screen's spare
//!   rows to history as read from the alternate screen: `--history 10
//!   --size 3x4 'ab\r\ncd\r\nef\e[?1049hXY' resize:2x4 '\e[?1049l'`.
//! - Reflow joins only rows on the screen. Rows pulled back from history
//!   keep the width they had, cut to the new one, and are not joined to
//!   each other or to the screen: `--history 10000 --size 4x6
//!   'abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGH' resize:3x4 resize:6x9`.
//! - Overwriting the first half of a wide glyph leaves its second half,
//!   so the new glyph reads as wide: `--size 1x4 '中\ra'`; overwriting the
//!   second half leaves the first, now narrow: `--size 1x4 '中\x08b'`.
//! - With autowrap off, a wide glyph that does not fit wraps anyway:
//!   `--size 2x3 '\e[?7lab中'`; and a glyph in the last column leaves no
//!   wrap pending, so it is overwritten after autowrap is back on:
//!   `--size 2x3 '\e[?7labc\e[?7hd'`.
//! - A pending wrap is cancelled only by a cursor move that moves it: in a
//!   one-column terminal CR and BS leave it pending: `--size 2x1 'a\rX'`.
//! - Scrolling a region that ends above a continuation row leaves that
//!   row continuing the region's new last row: `--size 3x2
//!   'abcde\e[1;2r\e[2;1H\n'`.
//! - DECRC with nothing saved, and leaving mode 1048 or 1049 before
//!   entering it, hide the cursor (the saved visibility starts as off):
//!   `--size 1x1 '\e8'`.
//! - RIS or DECSTR in the alternate screen leaves it shown, though the
//!   state says the primary screen is: `--size 2x4 '\e[?1049hx\ec'`.
//! - Mode 47 is not supported (1047 and 1049 are): `--size 2x5
//!   'ab\e[?47hX'`.
//! - `CSI s` is always DECSLRM, which homes the cursor, never SCOSC; `CSI
//!   u` is not supported: `--size 1x4 'ab\e[s'`.
//! - DECSTBM accepts a region of one line (`--size 3x1
//!   'x\e[1;1r\e[1H\eM\eM'`) and takes a bottom of 0 as 0, an invalid
//!   region, not as the last line (`--size 4x2 '\e[2;0r\e[?6h'`).
//! - DECSTR resets the pen, DECCKM, DECKPAM and DECTCEM, as DEC's
//!   specification has it, and bracketed paste and focus reporting too:
//!   `--size 2x5 'ab\e[1m\e[!pX'`, `--size 1x1
//!   '\e[?1h\e=\e[?2004h\e[?1004h\e[!p'`.
//! - SGR 6 (rapid blink) is not blink: `--size 1x4 '\e[6mA'`. SGR 58 is
//!   unknown, so its arguments are read as attributes of their own
//!   (`5` blink, `9` strikeout): `--size 1x3 '\e[58;5;9mX'`. In the colon
//!   form of direct colour, the colour-space slot is read as red:
//!   `--size 1x3 '\e[38:2::255:0:0mX'`.
//! - An erased cell takes the pen's foreground as well as its background,
//!   on a line scrolled in too: `--size 1x2 'a\e[30m  '`.
//! - ESC in a string not followed by `\` ends the string and is dropped,
//!   and what follows is printed: `--size 1x8 '\e]2;he\e]2;llo\x07'`.
//! - A C1 control written as UTF-8 is a glyph of width -1, which moves the
//!   cursor left: `--size 1x4 'a\xc2\x85b'`.
use crate::engine::{Can, Engine, Kind, Setup, always};
use crate::snapshot::{self, Cell, Color, Line, Snapshot, Style, Width};

/// Rows of history kept: far more than a case can write, so the rows
/// compared are always still there.
const HISTORY_ROWS: usize = 100_000;

pub const KIND: Kind = Kind {
    name: "libvterm",
    about: "libvterm, the engine of Neovim's and Vim's terminals, compiled from its release source",
    can: Can {
        dim: false,
        underline_color: false,
        kitty_keyboard_flags: false,
        ..Can::ALL
    },
    panel: true,
    in_process: true,
    available: always,
    make,
};

fn make(setup: &Setup) -> Result<Box<dyn Engine>, String> {
    Ok(Box::new(Libvterm {
        term: ffi::Term::new(setup.rows, setup.cols, HISTORY_ROWS)?,
    }))
}

pub struct Libvterm {
    term: ffi::Term,
}

fn color(c: ffi::RawColor) -> Color {
    match c.kind {
        1 => Color::Idx(c.index),
        2 => Color::Rgb(c.red, c.green, c.blue),
        _ => Color::Default,
    }
}

fn cell(raw: &ffi::RawCell) -> Cell {
    let width = match raw.width {
        0 => Width::Tail,
        2 => Width::Wide,
        _ => Width::Narrow,
    };
    let count = usize::try_from(raw.count).unwrap_or(0);
    let text: String = raw
        .chars
        .iter()
        .take(count)
        .map(|&c| char::from_u32(c).unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect();
    let style = Style {
        fg: color(raw.fg),
        bg: color(raw.bg),
        underline_color: Color::Default,
        bold: raw.bold != 0,
        dim: false,
        italic: raw.italic != 0,
        underline: raw.underline != 0,
        blink: raw.blink != 0,
        inverse: raw.reverse != 0,
        hidden: raw.conceal != 0,
        strikeout: raw.strike != 0,
    };
    Cell::new(&text, width, style)
}

fn blank() -> Cell {
    Cell::new("", Width::Narrow, Style::default())
}

fn int(what: &str, n: i32) -> Result<u16, String> {
    // The caller names the engine.
    u16::try_from(n).map_err(|_| format!("{what} {n} is out of range"))
}

impl Engine for Libvterm {
    fn process(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.term.write(bytes);
        Ok(())
    }

    fn resize(&mut self, rows: u16, cols: u16) -> Result<(), String> {
        self.term.resize(rows, cols);
        Ok(())
    }

    fn snapshot(&mut self, history_rows: usize) -> Result<Snapshot, String> {
        let t = &self.term;
        let s = t.state();
        let rows = int("rows", s.rows)?;
        let cols = int("columns", s.cols)?;
        let screen = (0..rows)
            .map(|y| Line {
                unread_from: None,
                cells: (0..cols)
                    .map(|x| t.cell(y, x).map_or_else(blank, |c| cell(&c)))
                    .collect(),
                wrapped: t.row_wrapped(y),
            })
            .collect();
        let kept = t.history_len();
        let history = (kept.saturating_sub(history_rows)..kept)
            .map(|i| {
                let line = Line {
                    unread_from: None,
                    cells: (0..t.history_cols(i))
                        .filter_map(|x| t.history_cell(i, x).map(|c| cell(&c)))
                        .collect(),
                    wrapped: t.history_wrapped(i),
                };
                (line.text(), line.wrapped)
            })
            .collect();
        Ok(Snapshot {
            rows,
            cols,
            cursor: (
                int("cursor row", s.cursor_row)?,
                int("cursor column", s.cursor_col)?,
            ),
            pending_wrap: s.pending_wrap != 0,
            cursor_visible: s.cursor_visible != 0,
            autowrap: s.autowrap != 0,
            origin: s.origin != 0,
            alternate: s.alternate != 0,
            application_cursor: s.application_cursor != 0,
            application_keypad: s.application_keypad != 0,
            bracketed_paste: s.bracketed_paste != 0,
            focus_reporting: s.focus_reporting != 0,
            kitty_keyboard_flags: 0,
            title: String::from_utf8_lossy(t.title()).into_owned(),
            reports: snapshot::reports(t.replies()),
            screen,
            history,
        })
    }
}

/// The shim's interface, and a safe owner of one terminal.
#[expect(
    unsafe_code,
    reason = "libvterm is C: its shim is called through FFI, and only here"
)]
mod ffi {
    use std::ffi::{c_char, c_int};
    use std::marker::{PhantomData, PhantomPinned};
    use std::ptr::NonNull;

    /// The shim's `FvcTerm`, opaque.
    #[repr(C)]
    pub struct Raw {
        _data: [u8; 0],
        _marker: PhantomData<(*mut u8, PhantomPinned)>,
    }

    /// `FvcColor`: kind 0 the default, 1 a palette index, 2 direct colour.
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub struct RawColor {
        pub kind: i32,
        pub index: u8,
        pub red: u8,
        pub green: u8,
        pub blue: u8,
    }

    /// `FvcCell`: width 1 or 2, or 0 for the second half of a wide glyph.
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub struct RawCell {
        pub chars: [u32; 6],
        pub count: i32,
        pub width: i32,
        pub bold: i32,
        pub underline: i32,
        pub italic: i32,
        pub blink: i32,
        pub reverse: i32,
        pub conceal: i32,
        pub strike: i32,
        pub fg: RawColor,
        pub bg: RawColor,
    }

    /// `FvcState`.
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub struct RawState {
        pub rows: i32,
        pub cols: i32,
        pub cursor_row: i32,
        pub cursor_col: i32,
        pub pending_wrap: i32,
        pub cursor_visible: i32,
        pub autowrap: i32,
        pub origin: i32,
        pub alternate: i32,
        pub application_cursor: i32,
        pub application_keypad: i32,
        pub bracketed_paste: i32,
        pub focus_reporting: i32,
    }

    unsafe extern "C" {
        fn fvc_libvterm_new(rows: c_int, cols: c_int, history_limit: usize) -> *mut Raw;
        fn fvc_libvterm_free(t: *mut Raw);
        fn fvc_libvterm_write(t: *mut Raw, bytes: *const c_char, len: usize);
        fn fvc_libvterm_resize(t: *mut Raw, rows: c_int, cols: c_int);
        fn fvc_libvterm_state(t: *const Raw, out: *mut RawState);
        fn fvc_libvterm_cell(t: *const Raw, row: c_int, col: c_int, out: *mut RawCell) -> c_int;
        fn fvc_libvterm_row_wrapped(t: *const Raw, row: c_int) -> c_int;
        fn fvc_libvterm_history_len(t: *const Raw) -> usize;
        fn fvc_libvterm_history_cols(t: *const Raw, index: usize) -> c_int;
        fn fvc_libvterm_history_wrapped(t: *const Raw, index: usize) -> c_int;
        fn fvc_libvterm_history_cell(
            t: *const Raw,
            index: usize,
            col: c_int,
            out: *mut RawCell,
        ) -> c_int;
        fn fvc_libvterm_title(t: *const Raw, len: *mut usize) -> *const c_char;
        fn fvc_libvterm_replies(t: *const Raw, len: *mut usize) -> *const c_char;
    }

    /// One terminal, owned: freed when dropped. Every call below passes
    /// the pointer `fvc_libvterm_new` returned, which stays valid until
    /// `Drop` frees it, and the shim neither keeps nor frees any pointer
    /// it is given.
    pub struct Term(NonNull<Raw>);

    /// A byte string `owner`'s shim holds, valid until the terminal is
    /// next written to, resized or freed: borrowing it from `owner` keeps
    /// all three away.
    fn borrowed(owner: &Term, ptr: *const c_char, len: usize) -> &[u8] {
        let _: &Term = owner;
        if ptr.is_null() || len == 0 {
            return &[];
        }
        // SAFETY: the shim returns its own buffer of `len` initialized
        // bytes, non-null (checked above), which lives as long as the
        // borrow of `owner` (see above).
        unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), len) }
    }

    impl Term {
        pub fn new(rows: u16, cols: u16, history: usize) -> Result<Term, String> {
            // SAFETY: plain integers in; the shim returns a new terminal or
            // null.
            let raw = unsafe { fvc_libvterm_new(c_int::from(rows), c_int::from(cols), history) };
            NonNull::new(raw)
                .map(Term)
                .ok_or_else(|| format!("libvterm: cannot make a {rows}x{cols} terminal"))
        }

        pub fn write(&mut self, bytes: &[u8]) {
            // SAFETY: a live terminal (see `Term`), and `bytes` is valid
            // for `len` bytes for the call; libvterm only reads them.
            unsafe { fvc_libvterm_write(self.0.as_ptr(), bytes.as_ptr().cast(), bytes.len()) }
        }

        pub fn resize(&mut self, rows: u16, cols: u16) {
            // SAFETY: a live terminal (see `Term`).
            unsafe { fvc_libvterm_resize(self.0.as_ptr(), c_int::from(rows), c_int::from(cols)) }
        }

        pub fn state(&self) -> RawState {
            let mut out = RawState::default();
            // SAFETY: a live terminal (see `Term`), and `out` is a valid
            // `FvcState` for the shim to fill.
            unsafe { fvc_libvterm_state(self.0.as_ptr(), &raw mut out) };
            out
        }

        pub fn cell(&self, row: u16, col: u16) -> Option<RawCell> {
            let mut out = RawCell::default();
            // SAFETY: a live terminal (see `Term`); `out` is a valid
            // `FvcCell`, and the shim checks the position.
            let found = unsafe {
                fvc_libvterm_cell(
                    self.0.as_ptr(),
                    c_int::from(row),
                    c_int::from(col),
                    &raw mut out,
                )
            };
            (found != 0).then_some(out)
        }

        pub fn row_wrapped(&self, row: u16) -> bool {
            // SAFETY: a live terminal (see `Term`); the shim checks the row.
            unsafe { fvc_libvterm_row_wrapped(self.0.as_ptr(), c_int::from(row)) != 0 }
        }

        pub fn history_len(&self) -> usize {
            // SAFETY: a live terminal (see `Term`).
            unsafe { fvc_libvterm_history_len(self.0.as_ptr()) }
        }

        pub fn history_cols(&self, index: usize) -> c_int {
            // SAFETY: a live terminal (see `Term`); the shim checks the
            // index.
            unsafe { fvc_libvterm_history_cols(self.0.as_ptr(), index) }
        }

        pub fn history_wrapped(&self, index: usize) -> bool {
            // SAFETY: a live terminal (see `Term`); the shim checks the
            // index.
            unsafe { fvc_libvterm_history_wrapped(self.0.as_ptr(), index) != 0 }
        }

        pub fn history_cell(&self, index: usize, col: c_int) -> Option<RawCell> {
            let mut out = RawCell::default();
            // SAFETY: a live terminal (see `Term`); `out` is a valid
            // `FvcCell`, and the shim checks the index and column.
            let found =
                unsafe { fvc_libvterm_history_cell(self.0.as_ptr(), index, col, &raw mut out) };
            (found != 0).then_some(out)
        }

        pub fn title(&self) -> &[u8] {
            let mut len = 0usize;
            // SAFETY: a live terminal (see `Term`); `len` is a valid
            // `size_t` for the shim to fill.
            let ptr = unsafe { fvc_libvterm_title(self.0.as_ptr(), &raw mut len) };
            borrowed(self, ptr, len)
        }

        /// Every reply the terminal has given.
        pub fn replies(&self) -> &[u8] {
            let mut len = 0usize;
            // SAFETY: a live terminal (see `Term`); `len` is a valid
            // `size_t` for the shim to fill.
            let ptr = unsafe { fvc_libvterm_replies(self.0.as_ptr(), &raw mut len) };
            borrowed(self, ptr, len)
        }
    }

    impl Drop for Term {
        fn drop(&mut self) {
            // SAFETY: the terminal `fvc_libvterm_new` returned, freed once,
            // here, and never used again.
            unsafe { fvc_libvterm_free(self.0.as_ptr()) }
        }
    }
}
