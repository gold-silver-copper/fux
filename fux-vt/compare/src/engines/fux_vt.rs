//! fux-vt, the subject, read into a snapshot.
use crate::engine::{Engine, Setup};
use crate::snapshot::{self, Cell, Color, Line, Snapshot, Style, Width};
use fux_vt::{Blink, CellRef, Event, Identity, Options, Parser, Sink};

/// How the parser is set up: as ratty sets it up (reflow, an identity, the
/// kitty keyboard protocol), with events on so titles can be compared; or,
/// with `reflow` off, as fux does.
pub fn options(reflow: bool) -> Options {
    Options {
        events: true,
        extended_replies: false,
        kitty_keyboard: reflow,
        reflow,
        identity: reflow.then_some(Identity {
            name: "fux-vt-ghostty",
            version: "0.0.0",
        }),
    }
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
        fux_vt::Color::Default => Color::Default,
        fux_vt::Color::Idx(n) => Color::Idx(n),
        fux_vt::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
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

pub fn make(setup: &Setup) -> Result<Box<dyn Engine>, String> {
    let parser = Parser::with_options(setup.rows, setup.cols, setup.history, options(setup.reflow))
        .map_err(|e| format!("fux-vt: {e}"))?;
    Ok(Box::new(Vt {
        parser,
        heard: Heard::default(),
    }))
}

impl Engine for Vt {
    fn process(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.parser
            .process_with(bytes, &mut self.heard)
            .map_err(|e| format!("fux-vt: {e}"))
    }

    fn resize(&mut self, rows: u16, cols: u16) -> Result<(), String> {
        self.parser
            .resize(rows, cols)
            .map_err(|e| format!("fux-vt: {e}"))
    }

    /// Its whole history, whatever is asked: the other engines are asked
    /// for as many rows as it keeps.
    fn snapshot(&mut self, _: usize) -> Result<Snapshot, String> {
        let s = self.parser.screen();
        let (rows, cols) = s.size();
        let cursor = s.cursor_position();
        let pending_wrap = s.pending_wrap();
        let screen = (0..rows)
            .map(|y| Line {
                cells: (0..cols)
                    .map(|x| {
                        s.cell(y, x).map_or_else(
                            || Cell::new("", Width::Narrow, Style::default()),
                            |c| cell(&c),
                        )
                    })
                    .collect(),
                wrapped: s.row_wrapped(y),
            })
            .collect();
        let history = (0..s.history_len())
            .rev()
            .filter_map(|back| {
                let row = s.row_from_bottom(usize::from(rows).checked_add(back)?)?;
                let line = Line {
                    cells: row.cells().map(|c| cell(&c)).collect(),
                    wrapped: row.wrapped,
                };
                Some((line.text(), row.wrapped))
            })
            .collect();
        Ok(Snapshot {
            rows,
            cols,
            cursor,
            pending_wrap,
            cursor_visible: !s.hide_cursor(),
            autowrap: s.autowrap(),
            origin: s.origin_mode(),
            alternate: s.alternate_screen(),
            application_cursor: s.application_cursor(),
            application_keypad: s.application_keypad(),
            bracketed_paste: s.bracketed_paste(),
            focus_reporting: s.focus_reporting(),
            kitty_keyboard_flags: s.kitty_keyboard_flags(),
            title: self.heard.title.clone().unwrap_or_default(),
            reports: snapshot::reports(&self.heard.replies),
            screen,
            history,
        })
    }
}
