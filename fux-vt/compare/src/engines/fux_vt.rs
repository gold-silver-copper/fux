//! fux-vt, the subject, read into a snapshot.
use crate::engine::{Engine, Setup};
use crate::snapshot::{self, Cell, Color, Device, Line, Snapshot, Style, Width};
use fux_vt::{Blink, CellRef, Event, Feature, Identity, Mode, Options, Parser, Sink};

/// How the parser is set up: as ratty sets it up (reflow, an identity), with
/// events on so titles can be compared; or, with `reflow` off, as fux does.
pub fn options(reflow: bool) -> Options {
    // As fux's panes are set up (`fux::pane::OPTIONS`), with ratty's
    // reflow and identity in its setup.
    fux::pane::OPTIONS
        .set(Feature::Reflow, reflow)
        .with_identity(Some(if reflow {
            Identity {
                name: "fux-vt-ghostty",
                version: "0.0.0",
            }
        } else {
            fux::pane::IDENTITY
        }))
}

#[derive(Default)]
struct Heard {
    replies: Vec<u8>,
    title: Option<String>,
}

impl Sink for Heard {
    fn reply(&mut self, bytes: &[u8]) {
        self.replies.extend_from_slice(bytes);
    }
    fn event(&mut self, event: Event<'_>) {
        if let Event::Title(title) = event {
            self.title = Some(String::from_utf8_lossy(title).into_owned());
        }
    }
}

pub struct Vt {
    parser: Parser,
    heard: Heard,
}

fn color(c: fux_vt::Color) -> Color {
    match c {
        fux_vt::Color::Idx(n) => Color::Idx(n),
        fux_vt::Color::Rgb(fux_vt::Rgb { r, g, b }) => Color::Rgb(r, g, b),
        fux_vt::Color::Default => Color::Default,
    }
}

fn cell(c: &CellRef<'_>) -> Cell {
    let style = Style {
        fg: color(c.fgcolor()),
        bg: color(c.bgcolor()),
        underline_color: color(c.underline_color()),
        bold: c.bold(),
        dim: c.dim(),
        italic: c.italic(),
        underline: c.underline(),
        blink: c.blink() != Blink::None,
        inverse: c.inverse(),
        hidden: c.hidden(),
        strikeout: c.strikeout(),
    };
    let width = if c.is_wide_continuation() {
        Width::Tail
    } else if c.is_wide() {
        Width::Wide
    } else {
        Width::Narrow
    };
    let text = if c.has_contents() { c.contents() } else { "" };
    Cell::new(text, width, style)
}

/// The bytes of history's budget beside its rows, with any history, for
/// what else it holds: the oldest and newest blocks partly filled, the
/// ring's last chunk, the maps' buckets. Enough at every size tried, rows
/// 1 to 30,000 at 1 to 6000 columns.
const HISTORY_ROOM: usize = 1 << 16;

/// The cells a block of fux-vt's history holds (history.rs, `BLOCK`). A
/// row's cells are never split between blocks, so a block holds as many
/// whole rows as fit, and the cells past the last are spent; a row wider
/// than a block has one of its own.
const BLOCK_CELLS: usize = 1 << 12;

/// History's budget for `rows` full rows of `cols`: 24 bytes a row, and 8
/// a cell of the blocks the rows fill (a row's share of a block, which is
/// more than its own cells where `cols` does not divide [`BLOCK_CELLS`]);
/// and, with any history, [`HISTORY_ROOM`] beside them: so fux-vt keeps at
/// least the rows of plain cells (styles count in the budget too,
/// `fux-vt/THREATS.md` B11), and a row lost is seen. The adapter's and
/// `bench`'s fux-vt+prints'.
pub fn history_bytes(rows: usize, cols: u16) -> usize {
    if rows == 0 {
        return 0;
    }
    let cols = usize::from(cols).max(1);
    let block = BLOCK_CELLS.max(cols);
    let blocks = rows.div_ceil(BLOCK_CELLS.checked_div(cols).unwrap_or(1).max(1));
    (rows.saturating_mul(24))
        .saturating_add(blocks.saturating_mul(block).saturating_mul(8))
        .saturating_add(HISTORY_ROOM)
}

pub fn make(setup: &Setup) -> Result<Box<dyn Engine>, String> {
    let size = crate::vt_size(setup.rows, setup.cols)?;
    let history = history_bytes(setup.history, setup.cols);
    let parser = Parser::with_options(size, history, options(setup.reflow))
        .map_err(|e| format!("fux-vt: {e}"))?;
    Ok(Box::new(Vt {
        parser,
        heard: Heard::default(),
    }))
}

impl Engine for Vt {
    fn history_len(&mut self) -> Option<usize> {
        Some(self.parser.screen().history_len())
    }

    fn process(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.parser
            .process_with(bytes, &mut self.heard)
            .map_err(|e| format!("fux-vt: {e}"))
    }

    fn resize(&mut self, rows: u16, cols: u16) -> Result<(), String> {
        self.parser
            .resize(crate::vt_size(rows, cols)?)
            .map_err(|e| format!("fux-vt: {e}"))
    }

    /// Its whole history, whatever is asked: the other engines are asked
    /// for as many rows as it keeps.
    fn snapshot(&mut self, _: usize) -> Result<Snapshot, String> {
        let s = self.parser.screen();
        let (rows, cols) = s.size().into();
        let cursor = s.cursor_position();
        let pending_wrap = s.pending_wrap();
        let screen = (0..rows)
            .map(|y| Line {
                unread_from: None,
                prompt: s.starts_prompt(y),
                cells: (0..cols)
                    .map(|x| {
                        let link = s
                            .link(y, x)
                            .map(|l| (l.uri().to_owned(), l.key().to_string()));
                        s.cell(y, x)
                            .map_or_else(
                                || Cell::new("", Width::Narrow, Style::default()),
                                |c| cell(&c),
                            )
                            .linked(link)
                    })
                    .collect(),
                wrapped: s.row_wrapped(y),
            })
            .collect();
        let history = s
            .rows()
            .take(s.history_len())
            .map(|row| {
                let line = Line {
                    unread_from: None,
                    prompt: false,
                    cells: row.cells().map(|c| cell(&c)).collect(),
                    wrapped: row.wrapped(),
                };
                (line.text(), row.wrapped())
            })
            .collect();
        Ok(Snapshot {
            rows,
            cols,
            cursor,
            pending_wrap,
            cursor_visible: s.mode(Mode::ShowCursor),
            autowrap: s.mode(Mode::Autowrap),
            origin: s.mode(Mode::Origin),
            alternate: s.mode(Mode::AlternateScreen),
            application_cursor: s.mode(Mode::ApplicationCursor),
            application_keypad: s.mode(Mode::ApplicationKeypad),
            bracketed_paste: s.mode(Mode::BracketedPaste),
            synchronized_output: s.mode(Mode::SynchronizedOutput),
            in_band_resize: s.mode(Mode::InBandResize),
            focus_reporting: s.mode(Mode::FocusReporting),
            kitty_keyboard_flags: s.kitty_keyboard_flags(),
            title: self.heard.title.clone().unwrap_or_default(),
            reports: snapshot::reports(&self.heard.replies),
            screen,
            history,
            device: device(&self.parser),
        })
    }

    fn replies(&mut self) -> Result<Vec<u8>, String> {
        Ok(self.heard.replies.clone())
    }
}

/// The device's state fux-vt's API shows, and what only its answers to
/// input do, read on copies: the tab stops by HT from the first column,
/// and the protected cells by what a selective erase of the whole screen
/// leaves (DECSED, `CSI ? 2 J`): those holding text.
fn device(parser: &Parser) -> Device {
    struct Deaf;
    impl fux_vt::Sink for Deaf {}
    let s = parser.screen();
    let (rows, cols) = s.size().into();
    let renditions = (0..rows)
        .map(|y| match s.line_rendition(y) {
            fux_vt::LineRendition::Single => 0,
            fux_vt::LineRendition::DoubleWidth => 1,
            fux_vt::LineRendition::DoubleHeightTop => 2,
            fux_vt::LineRendition::DoubleHeightBottom => 3,
        })
        .collect();
    let mut tabs = parser.clone();
    let mut tab_stops = Vec::new();
    // Margins and origin mode off first, as the stops are the screen's,
    // and the row made single width, whose end would stop the tabs.
    let _ = tabs.process_with(b"\x1b[?6l\x1b[?69l\x1b[r\x1b#5\r", &mut Deaf);
    for _ in 0..cols {
        let before = tabs.screen().cursor_position().1;
        let _ = tabs.process_with(b"\t", &mut Deaf);
        let at = tabs.screen().cursor_position().1;
        if at <= before || at.saturating_add(1) >= cols {
            break;
        }
        tab_stops.push(at);
    }
    let mut erased = parser.clone();
    let _ = erased.process_with(b"\x1b[?2J", &mut Deaf);
    let e = erased.screen();
    let protected = (0..rows)
        .flat_map(|y| (0..cols).map(move |x| (y, x)))
        .filter(|&(y, x)| e.cell(y, x).is_some_and(|c| !c.contents().is_empty()))
        .collect();
    Device {
        // Ps 0 and 1 are one shape, a blinking block, as 1; the terminal's
        // own, which fux-vt doesn't know, as 7 (DECSCUSR 7 restores it),
        // compared with no one.
        cursor_shape: match s.cursor_shape().number() {
            0 => 1,
            n => n,
        },
        tab_stops,
        renditions,
        protected,
    }
}

#[cfg(test)]
mod tests {
    use crate::engine::Setup;

    /// The adapter's history keeps at least the rows asked of full rows of
    /// plain cells, at widths that do and don't divide a block, as the
    /// cases and the bench read it ("history rows kept"): 10,000 rows at
    /// 200 columns, the bench's census size, kept 9,791 when a row was
    /// counted at its own cells alone.
    #[test]
    fn history_keeps_the_rows_asked() -> Result<(), String> {
        for (rows, cols) in [
            (100, 80),
            (1000, 1000),
            (10_000, 80),
            (10_000, 200),
            (10_000, 4097),
        ] {
            let mut engine = super::make(&Setup {
                rows: 10,
                cols,
                history: rows,
                reflow: true,
            })?;
            let mut line = vec![b'x'; usize::from(cols)];
            line.extend_from_slice(b"\r\n");
            for _ in 0..rows.saturating_add(100) {
                engine.feed(&line, 4096)?;
            }
            let held = engine.history_len().unwrap_or(0);
            assert!(held >= rows, "{held} of {rows} rows kept at {cols} columns");
        }
        Ok(())
    }
}
