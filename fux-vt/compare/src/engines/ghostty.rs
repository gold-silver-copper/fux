//! Ghostty's terminal core (libghostty-vt), read into a snapshot.
use crate::engine::{Blanks, Can, Engine, Kind, Setup, always};
use crate::snapshot::{self, Cell, Color, Line, Snapshot, Style, Width};
use libghostty_vt::screen::{CellContentTag, CellWide, GridRef, RowSemanticPrompt};
use libghostty_vt::style::{StyleColor, Underline};
use libghostty_vt::terminal::{Mode, Point, PointCoordinate};
use libghostty_vt::{Terminal, TerminalOptions};
use std::cell::RefCell;
use std::rc::Rc;

/// History Ghostty keeps, in bytes: far more than fux-vt's row limits, so
/// the rows compared are always still there. Ghostty keeps it even when
/// fux-vt keeps none (and none is compared): without scrollback, Ghostty
/// leaves a stale soft-wrap flag on the row it recycles when it scrolls
/// (`replay --size 1x3 abcd`).
const HISTORY_BYTES: usize = 64 << 20;

pub struct Ghostty {
    terminal: Terminal<'static, 'static>,
    replies: Rc<RefCell<Vec<u8>>>,
}

fn err(what: &str) -> impl Fn(libghostty_vt::Error) -> String + '_ {
    move |e| format!("ghostty: {what}: {e}")
}

fn color(c: StyleColor) -> Color {
    match c {
        StyleColor::None => Color::Default,
        StyleColor::Palette(index) => Color::Idx(index.0),
        StyleColor::Rgb(rgb) => Color::Rgb(rgb.r, rgb.g, rgb.b),
    }
}

fn text(at: &GridRef<'_>) -> Result<String, String> {
    let mut buf = vec!['\0'; 16];
    let len = match at.graphemes(&mut buf) {
        Ok(len) => len,
        Err(libghostty_vt::Error::OutOfSpace { required }) => {
            buf = vec!['\0'; required.max(256)];
            at.graphemes(&mut buf).map_err(err("graphemes"))?
        }
        Err(e) => return Err(err("graphemes")(e)),
    };
    Ok(buf.iter().take(len).collect())
}

/// The URI of the cell's hyperlink, if it has one. Ghostty's C API gives
/// no link's identity, so which cells share one is not read.
fn link(at: &GridRef<'_>) -> Result<Option<(String, String)>, String> {
    let mut buf = vec![0u8; 256];
    let len = match at.hyperlink_uri(&mut buf) {
        Ok(len) => len,
        Err(libghostty_vt::Error::OutOfSpace { required }) => {
            buf = vec![0u8; required];
            at.hyperlink_uri(&mut buf).map_err(err("hyperlink"))?
        }
        Err(e) => return Err(err("hyperlink")(e)),
    };
    let uri = buf.get(..len).ok_or("ghostty: hyperlink: too long")?;
    Ok((len > 0).then(|| (String::from_utf8_lossy(uri).into_owned(), String::new())))
}

fn cell(at: &GridRef<'_>) -> Result<Cell, String> {
    let raw = at.cell().map_err(err("cell"))?;
    let width = match raw.wide().map_err(err("wide"))? {
        CellWide::Narrow | CellWide::SpacerHead => Width::Narrow,
        CellWide::Wide => Width::Wide,
        CellWide::SpacerTail => Width::Tail,
    };
    let s = at.style().map_err(err("style"))?;
    let mut style = Style {
        fg: color(s.fg_color),
        bg: color(s.bg_color),
        underline_color: color(s.underline_color),
        bold: s.bold,
        dim: s.faint,
        italic: s.italic,
        underline: s.underline != Underline::None,
        blink: s.blink,
        inverse: s.inverse,
        hidden: s.invisible,
        strikeout: s.strikethrough,
    };
    // A blank cell erased with a background colour keeps the colour in
    // the cell, not in a style.
    match raw.content_tag().map_err(err("content"))? {
        CellContentTag::BgColorPalette => {
            style.bg = Color::Idx(raw.bg_color_palette().map_err(err("bg"))?.0);
        }
        CellContentTag::BgColorRgb => {
            let rgb = raw.bg_color_rgb().map_err(err("bg"))?;
            style.bg = Color::Rgb(rgb.r, rgb.g, rgb.b);
        }
        CellContentTag::Codepoint | CellContentTag::CodepointGrapheme => {}
    }
    let text = if raw.has_text().map_err(err("text"))? {
        text(at)?
    } else {
        String::new()
    };
    Ok(Cell::new(&text, width, style).linked(link(at)?))
}

pub const KIND: Kind = Kind {
    name: "ghostty",
    about: "Ghostty's terminal core, libghostty-vt, built from source by Zig",
    can: Can {
        link_group: false,
        ..Can::ALL
    },
    blanks: Blanks {
        fg: false,
        ..Blanks::XTERM
    },
    panel: true,
    in_process: true,
    available: always,
    make,
};

fn make(setup: &Setup) -> Result<Box<dyn Engine>, String> {
    with_scrollback(setup, HISTORY_BYTES)
}

/// Ghostty keeping at most `bytes` of scrollback (`footprint`): its limit
/// is in bytes, not rows.
pub fn with_scrollback(setup: &Setup, bytes: usize) -> Result<Box<dyn Engine>, String> {
    Ok(Box::new(Ghostty::new(setup.rows, setup.cols, bytes)?))
}

impl Ghostty {
    fn new(rows: u16, cols: u16, max_scrollback: usize) -> Result<Ghostty, String> {
        let mut terminal = Terminal::new(TerminalOptions {
            cols,
            rows,
            max_scrollback,
        })
        .map_err(err("new"))?;
        // fux-vt always measures a grapheme cluster as a whole, as mode
        // 2027 does.
        terminal
            .set_mode(Mode::GRAPHEME_CLUSTER, true)
            .map_err(err("mode 2027"))?;
        let replies = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&replies);
        terminal
            .on_pty_write(move |_, bytes| sink.borrow_mut().extend_from_slice(bytes))
            .map_err(err("pty write"))?;
        Ok(Ghostty { terminal, replies })
    }

    fn line(&self, point: impl Fn(u16) -> Point, cols: u16) -> Result<Line, String> {
        let mut cells = Vec::with_capacity(usize::from(cols));
        let mut wrapped = false;
        let mut prompt = false;
        for x in 0..cols {
            let at = self.terminal.grid_ref(point(x)).map_err(err("grid ref"))?;
            if x == 0 {
                let row = at.row().map_err(err("row"))?;
                wrapped = row.is_wrapped().map_err(err("row"))?;
                // A primary prompt's row: a continuation line's (`k=c`,
                // `k=s`, or a line the prompt wrapped or went on to) is
                // not where a prompt starts.
                prompt = row.semantic_prompt().map_err(err("row"))? == RowSemanticPrompt::Prompt;
            }
            cells.push(cell(&at)?);
        }
        Ok(Line {
            cells,
            wrapped,
            unread_from: None,
            prompt,
        })
    }

    fn read(&self, history_rows: usize) -> Result<Snapshot, String> {
        let t = &self.terminal;
        let rows = t.rows().map_err(err("rows"))?;
        let cols = t.cols().map_err(err("cols"))?;
        let screen = (0..rows)
            .map(|y| {
                self.line(
                    |x| Point::Active(PointCoordinate { x, y: u32::from(y) }),
                    cols,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        let kept = t.scrollback_rows().map_err(err("scrollback"))?;
        let mut history = Vec::new();
        for y in kept.saturating_sub(history_rows)..kept {
            let y = u32::try_from(y).map_err(|e| format!("ghostty: history row: {e}"))?;
            let line = self.line(|x| Point::History(PointCoordinate { x, y }), cols)?;
            history.push((line.text(), line.wrapped));
        }
        let mode = |m: Mode| t.mode(m).map_err(err("mode"));
        Ok(Snapshot {
            rows,
            cols,
            cursor: (
                t.cursor_y().map_err(err("cursor"))?,
                t.cursor_x().map_err(err("cursor"))?,
            ),
            pending_wrap: t.is_cursor_pending_wrap().map_err(err("cursor"))?,
            cursor_visible: t.is_cursor_visible().map_err(err("cursor"))?,
            autowrap: mode(Mode::WRAPAROUND)?,
            origin: mode(Mode::ORIGIN)?,
            alternate: t.active_screen().map_err(err("screen"))?
                == libghostty_vt::screen::Screen::Alternate,
            application_cursor: mode(Mode::DECCKM)?,
            application_keypad: mode(Mode::KEYPAD_KEYS)?,
            bracketed_paste: mode(Mode::BRACKETED_PASTE)?,
            synchronized_output: mode(Mode::SYNC_OUTPUT)?,
            in_band_resize: mode(Mode::IN_BAND_RESIZE)?,
            focus_reporting: mode(Mode::FOCUS_EVENT)?,
            kitty_keyboard_flags: t.kitty_keyboard_flags().map_err(err("kitty flags"))?.bits(),
            title: t.title().map_err(err("title"))?.to_owned(),
            reports: snapshot::reports(&self.replies.borrow()),
            screen,
            history,
        })
    }
}

impl Engine for Ghostty {
    fn history_len(&mut self) -> Option<usize> {
        self.terminal.scrollback_rows().ok()
    }

    fn process(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.terminal.vt_write(bytes);
        Ok(())
    }

    fn resize(&mut self, rows: u16, cols: u16) -> Result<(), String> {
        self.terminal
            .resize(cols, rows, 8, 16)
            .map_err(err("resize"))
    }

    fn snapshot(&mut self, history_rows: usize) -> Result<Snapshot, String> {
        self.read(history_rows)
    }

    fn replies(&self) -> Vec<u8> {
        self.replies.borrow().clone()
    }
}
