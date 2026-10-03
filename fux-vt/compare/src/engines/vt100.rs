//! The vt100 crate, fux-vt's ancestor, read into a snapshot. It is a speed
//! baseline and a comparison on request, not a voter: fux-vt inherited its
//! choices, which is what the vote is meant to catch.
//!
//! vt100 is fed bytes (`Parser::process`) and keeps 100 000 rows of
//! scrollback here, whatever fux-vt keeps.
//!
//! Read: every cell of `Screen::cell` (contents, `is_wide`,
//! `is_wide_continuation`, colours, bold, dim, italic, underline,
//! inverse); `Screen::row_wrapped`; the cursor (`cursor_position`, whose
//! column is one past the last while a wrap is pending, and
//! `hide_cursor`); the alternate screen, application cursor, application
//! keypad and bracketed paste; the title, from the `set_window_title`
//! callback; and history, by scrolling the view back one row at a time
//! (`Screen::set_scrollback`) and reading its top row, then back to the
//! live screen.
//!
//! What vt100 does not keep, or does not tell: an underline colour, blink,
//! hidden, strikeout (it ignores SGR 5, 8 and 9), autowrap (it always
//! wraps), origin mode (kept, but with no accessor), focus reporting, the
//! kitty keyboard flags, and replies (it answers nothing).
//!
//! vt100 panics on two inputs, at a `u16` subtraction that overflows:
//! when a line wraps on a one-row screen (`grid.rs`, `col_wrap`), and on
//! a wide glyph on a one-column screen (`screen.rs`, `text`). With
//! overflow checks, as this harness builds, both panic; without them, the
//! first still panics, at an `unwrap` just after. The harness catches the
//! panic, and vt100 abstains for the rest of the case
//! (`replay --engines vt100,ghostty --size 1x1 'ab'`, `--size 2x1
//! '\u{754c}'`).
//!
//! Its quirks, beside Ghostty; fux-vt inherited most of vt100's other
//! choices. Each replays with `fux-vt-compare replay --engines
//! vt100,ghostty`:
//!
//! - It never reflows (`--size 2x4 'abcdef' resize:2x8`).
//! - SGR 58 is unknown, so its colour's arguments read as SGRs of their
//!   own: `58;2;4;5;6` sets dim and underline (`'\e[58;2;4;5;6mX'`).
//! - Bold and dim are one intensity, so the later of SGR 1 and 2 wins, as
//!   in fux-vt; Ghostty keeps both (`'\e[1;2mX'`).
//! - A glyph's width is its first char's: VS16 does not widen an emoji
//!   (`--size 1x2 '\u{2764}\u{fe0f}'`).
//! - HVP (`CSI f`), SCOSC and SCORC (`CSI s`, `CSI u`) are unknown
//!   (`--size 1x2 '\e[1;2f'`, `--size 1x4 'a\e[sb\e[uc'`).
//! - RI on the top row of the screen, above the scroll region, scrolls the
//!   region down (`--size 3x1 'l\r\ni\e[2;3r\e[H\eM'`).
//! - Invalid UTF-8, and a printed U+FFFD, leave no glyph: vt100 drops U+FFFD
//!   and the C1 range, as fux-vt does (`'a\xffb'`).
//! - A title with a `;` in it is dropped: vt100 takes OSC 0 and 2 only
//!   with exactly one parameter (`'\e]2;x\x07\e]2;a;b\x07'`).
use crate::engine::{Can, Engine, Kind, Setup, always};
use crate::snapshot::{Cell, Color, Line, Snapshot, Style, Width};

/// Scrollback vt100 keeps, in rows: far more than a case writes.
const SCROLLBACK: usize = 100_000;

pub const KIND: Kind = Kind {
    name: "vt100",
    about: "the vt100 crate, fux-vt's ancestor: a speed baseline, not a voter",
    can: Can {
        underline_color: false,
        blink: false,
        hidden: false,
        strikeout: false,
        autowrap: false,
        origin: false,
        focus_reporting: false,
        kitty_keyboard_flags: false,
        reports: false,
        synchronized_output: false,
        in_band_resize: false,
        ..Can::ALL
    },
    panel: false,
    in_process: true,
    available: always,
    make,
};

fn make(setup: &Setup) -> Result<Box<dyn Engine>, String> {
    Ok(Box::new(Vt100 {
        parser: vt100::Parser::new_with_callbacks(
            setup.rows,
            setup.cols,
            SCROLLBACK,
            Heard::default(),
        ),
    }))
}

#[derive(Default)]
struct Heard {
    title: Option<String>,
}

impl vt100::Callbacks for Heard {
    fn set_window_title(&mut self, _: &mut vt100::Screen, title: &[u8]) {
        self.title = Some(String::from_utf8_lossy(title).into_owned());
    }
}

pub struct Vt100 {
    parser: vt100::Parser<Heard>,
}

fn color(c: vt100::Color) -> Color {
    match c {
        vt100::Color::Default => Color::Default,
        vt100::Color::Idx(n) => Color::Idx(n),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

fn cell(c: &vt100::Cell) -> Cell {
    let style = Style {
        fg: color(c.fgcolor()),
        bg: color(c.bgcolor()),
        underline_color: Color::Default,
        bold: c.bold(),
        dim: c.dim(),
        italic: c.italic(),
        underline: c.underline(),
        blink: false,
        inverse: c.inverse(),
        hidden: false,
        strikeout: false,
    };
    let width = if c.is_wide_continuation() {
        Width::Tail
    } else if c.is_wide() {
        Width::Wide
    } else {
        Width::Narrow
    };
    Cell::new(c.contents(), width, style)
}

/// Row `y` of the view.
fn line(s: &vt100::Screen, y: u16, cols: u16) -> Line {
    Line {
        unread_from: None,
        cells: (0..cols)
            .map(|x| {
                s.cell(y, x)
                    .map_or_else(|| Cell::new("", Width::Narrow, Style::default()), cell)
            })
            .collect(),
        wrapped: s.row_wrapped(y),
    }
}

/// Everything compared, read from vt100's screen.
fn read(parser: &mut vt100::Parser<Heard>, history_rows: usize) -> Snapshot {
    let title = parser.callbacks().title.clone().unwrap_or_default();
    let s = parser.screen_mut();
    let (rows, cols) = s.size();
    let (row, col) = s.cursor_position();
    let screen = (0..rows).map(|y| line(s, y, cols)).collect();
    // The view scrolled back `back` rows has history row `kept - back` at
    // its top.
    s.set_scrollback(usize::MAX);
    let kept = s.scrollback();
    let mut history = Vec::new();
    for back in (1..=kept.min(history_rows)).rev() {
        s.set_scrollback(back);
        let l = line(s, 0, cols);
        history.push((l.text(), l.wrapped));
    }
    s.set_scrollback(0);
    Snapshot {
        rows,
        cols,
        // A cursor waiting to wrap is kept one past the last column.
        cursor: (row, col.min(cols.saturating_sub(1))),
        pending_wrap: col >= cols,
        cursor_visible: !s.hide_cursor(),
        autowrap: false,
        origin: false,
        alternate: s.alternate_screen(),
        application_cursor: s.application_cursor(),
        application_keypad: s.application_keypad(),
        bracketed_paste: s.bracketed_paste(),
        synchronized_output: false,
        in_band_resize: false,
        focus_reporting: false,
        kitty_keyboard_flags: 0,
        title,
        reports: Vec::new(),
        screen,
        history,
    }
}

impl Engine for Vt100 {
    fn process(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.parser.process(bytes);
        Ok(())
    }

    fn resize(&mut self, rows: u16, cols: u16) -> Result<(), String> {
        self.parser.screen_mut().set_size(rows, cols);
        Ok(())
    }

    fn snapshot(&mut self, history_rows: usize) -> Result<Snapshot, String> {
        Ok(read(&mut self.parser, history_rows))
    }
}
