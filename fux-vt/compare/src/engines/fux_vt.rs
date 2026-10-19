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

/// The bytes of history's budget beside its rows, with any history.
const HISTORY_ROOM: usize = 1 << 16;

/// History's budget for `rows` full rows of `cols`: 24 bytes a row and 8
/// a cell, and, with any history, room beside them for what else history's
/// budget holds (its maps' buckets, the style table past the screen's
/// share): so fux-vt keeps at least the rows, and a row lost is seen. The
/// adapter's and `bench`'s fux-vt+prints'.
pub fn history_bytes(rows: usize, cols: u16) -> usize {
    let row = usize::from(cols).saturating_mul(8).saturating_add(24);
    let room = if rows == 0 { 0 } else { HISTORY_ROOM };
    rows.saturating_mul(row).saturating_add(room)
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
/// leaves (DECSED, `CSI ? 2 J`): those holding text (HAR-46).
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
