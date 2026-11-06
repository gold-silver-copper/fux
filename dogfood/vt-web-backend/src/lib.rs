//! A web terminal's backend core: program output goes through fux-vt, and
//! after each chunk come JSON lines a browser paints from, one object each:
//!
//! - `{"reply":"…"}`: bytes to send back to the program (a cursor report).
//! - `{"title":"…"}`, `{"bell":true}`: what the program asked the window for.
//! - `{"seq":N,"size":[R,C],"scroll":K,"rows":[…],"cursor":{…}}`: an update.
//!   `size` comes when the size changed (the browser clears to blank),
//!   `scroll` when the rows moved up `K` rows (down for a negative `K`),
//!   then `rows` lists only the rows that differ from what the browser holds:
//!   `{"y":Y,"runs":[{"t":"text","w":cells,…style}]}`, cells past the last
//!   run blank. A run's style is only what differs from the default:
//!   `fg`/`bg`/`ulc` (an index 0–255 or `"#rrggbb"`), `b` bold, `d` dim,
//!   `i` italic, `inv` inverse, `hid` hidden, `s` strikeout, `ul` and
//!   `blink` (their style), `link` (its URI). `cursor` is
//!   `{"y":Y,"x":X,"visible":bool}`.
//!
//! Inside a synchronized frame (`CSI ? 2026 h`) no update is sent until it
//! ends, or until [`Backend::flush`] says it has waited long enough.
use fux_vt::{
    Attributes, CellRef, Color, Event, Feature, Mark, Mode, Options, Parser, Row, RowId, Sink, Size,
};
use std::fmt::Write;

/// A failure: a size with no cells, or one the terminal may not hold.
pub type Error = Box<dyn std::error::Error + Send + Sync>;

/// A terminal whose screen a browser mirrors.
pub struct Backend {
    parser: Parser,
    /// When the browser's copy was last brought up to date.
    mark: Option<Mark>,
    /// The identity and version of each row the browser holds, top first.
    sent: Vec<(RowId, u64)>,
    /// The size the browser has, as rows and columns.
    size: (u16, u16),
    cursor: Option<(u16, u16, bool)>,
    seq: u64,
}

/// What the program asked of the window while a chunk was read.
#[derive(Default)]
struct Asked {
    replies: Vec<u8>,
    title: Option<String>,
    bell: bool,
}

impl Sink for Asked {
    fn reply(&mut self, bytes: &[u8]) {
        self.replies.extend_from_slice(bytes);
    }

    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Title(kept) => {
                self.title = Some(String::from_utf8_lossy(kept.bytes).into_owned())
            }
            Event::Bell => self.bell = true,
            _ => {}
        }
    }
}

impl Backend {
    /// A terminal of `rows` by `cols` keeping `history_bytes` of scrollback.
    pub fn new(rows: u16, cols: u16, history_bytes: usize) -> Result<Backend, Error> {
        let options = Options::new()
            .with(Feature::Events)
            .with(Feature::Hyperlinks)
            .with(Feature::Reflow);
        let parser = Parser::with_options(Size::new(rows, cols)?, history_bytes, options)?;
        Ok(Backend {
            parser,
            mark: None,
            sent: Vec::new(),
            size: (0, 0),
            cursor: None,
            seq: 0,
        })
    }

    /// The terminal, to read more of it than the updates carry.
    pub fn parser(&self) -> &Parser {
        &self.parser
    }

    /// Reads a chunk of the program's output and appends the JSON lines it
    /// gives to `out`, each ending in a newline.
    pub fn feed(&mut self, bytes: &[u8], out: &mut String) -> Result<(), Error> {
        let mut asked = Asked::default();
        // The update below looks at the mode, so a frame's start or end
        // needs no stop.
        let _ = self.parser.process_all(bytes, &mut asked);
        if !asked.replies.is_empty() {
            out.push_str("{\"reply\":");
            string(out, &String::from_utf8_lossy(&asked.replies));
            out.push_str("}\n");
        }
        if let Some(title) = asked.title {
            out.push_str("{\"title\":");
            string(out, &title);
            out.push_str("}\n");
        }
        if asked.bell {
            out.push_str("{\"bell\":true}\n");
        }
        if !self.parser.screen().mode(Mode::SynchronizedOutput) {
            self.update(out);
        }
        Ok(())
    }

    /// Resizes the terminal, and appends the update that follows.
    pub fn resize(&mut self, rows: u16, cols: u16, out: &mut String) -> Result<(), Error> {
        self.parser.resize(Size::new(rows, cols)?)?;
        self.update(out);
        Ok(())
    }

    /// Appends an update even inside a synchronized frame: for a server
    /// whose wait for the frame's end ran out.
    pub fn flush(&mut self, out: &mut String) {
        self.update(out);
    }

    /// Appends an update of what changed since the last, if anything did.
    fn update(&mut self, out: &mut String) {
        let screen = self.parser.screen();
        let window = screen.window();
        let size = screen.size();
        let now: Vec<(RowId, u64)> = window
            .iter()
            .map(|(_, r)| (r.id(), r.version()))
            .collect();
        let resized = self.size != (size.rows(), size.cols()) || self.mark.is_none();
        let full = resized || self.mark.is_none_or(|mark| screen.full_refresh_since(mark));
        // After a scroll the rows are the browser's, moved: it moves them too.
        let scroll = if full && !resized {
            scrolled(&self.sent, &now)
        } else {
            0
        };
        let held = shifted(&self.sent, scroll);
        let mut rows = String::new();
        for (&(id, version), (y, row)) in now.iter().zip(window.iter()) {
            let changed = match self.mark {
                Some(mark) if !full => row.changed_since(mark),
                _ => resized || held.get(usize::from(y)) != Some(&Some((id, version))),
            };
            if changed {
                if !rows.is_empty() {
                    rows.push(',');
                }
                let _ = write!(rows, "{{\"y\":{y},");
                row_json(&row, &mut rows);
                rows.push('}');
            }
        }
        let (y, x) = screen.cursor_position();
        let cursor = (y, x, screen.mode(Mode::ShowCursor));
        let moved = self.cursor != Some(cursor);
        if rows.is_empty() && !moved && !resized && scroll == 0 {
            self.mark = Some(screen.mark());
            return;
        }
        self.seq = self.seq.saturating_add(1);
        let _ = write!(out, "{{\"seq\":{}", self.seq);
        if resized {
            let _ = write!(out, ",\"size\":[{},{}]", size.rows(), size.cols());
        }
        if scroll != 0 {
            let _ = write!(out, ",\"scroll\":{scroll}");
        }
        let _ = writeln!(
            out,
            ",\"rows\":[{rows}],\"cursor\":{{\"y\":{y},\"x\":{x},\"visible\":{}}}}}",
            cursor.2
        );
        self.mark = Some(screen.mark());
        self.sent = now;
        self.size = (size.rows(), size.cols());
        self.cursor = Some(cursor);
    }
}

/// How many rows `now` is `sent` moved up (negative: down), if it is that
/// with at least one row in common; else 0.
fn scrolled(sent: &[(RowId, u64)], now: &[(RowId, u64)]) -> i32 {
    let same = |a: &[(RowId, u64)], b: &[(RowId, u64)]| a.iter().zip(b).all(|(a, b)| a.0 == b.0);
    let (Some(first_now), Some(first_sent)) = (now.first(), sent.first()) else {
        return 0;
    };
    if let Some(up) = sent.iter().position(|r| r.0 == first_now.0)
        && up > 0
        && same(sent.get(up..).unwrap_or_default(), now)
    {
        return i32::try_from(up).unwrap_or(0);
    }
    if let Some(down) = now.iter().position(|r| r.0 == first_sent.0)
        && down > 0
        && same(sent, now.get(down..).unwrap_or_default())
    {
        return i32::try_from(down).map_or(0, |down| -down);
    }
    0
}

/// The rows the browser holds once it has moved its rows by `scroll`: `None`
/// where a blank row came in.
fn shifted(sent: &[(RowId, u64)], scroll: i32) -> Vec<Option<(RowId, u64)>> {
    let len = sent.len();
    let by = usize::try_from(scroll.unsigned_abs()).unwrap_or(len);
    (0..len)
        .map(|y| {
            let from = if scroll >= 0 {
                y.checked_add(by)
            } else {
                y.checked_sub(by)
            };
            from.and_then(|from| sent.get(from)).copied()
        })
        .collect()
}

/// Appends `"runs":[…]` for `row`: its cells in runs of one style and link,
/// up to its last that is not a default blank.
fn row_json(row: &Row<'_>, out: &mut String) {
    let blank = |cell: &CellRef<'_>, col: usize| {
        cell.contents().is_empty()
            && cell.attributes() == Attributes::default()
            && row.link(col).is_none()
    };
    let cells: Vec<CellRef<'_>> = row.cells().take(row.stored_len()).collect();
    let end = cells
        .iter()
        .enumerate()
        .rposition(|(col, cell)| !blank(cell, col))
        .map_or(0, |last| last.saturating_add(1));
    out.push_str("\"runs\":[");
    let mut run: Option<(Attributes, Option<&str>, String, usize)> = None;
    let mut first = true;
    for (col, cell) in cells.iter().enumerate().take(end) {
        if cell.is_wide_continuation() {
            continue;
        }
        let link = row.link(col).map(|link| link.uri());
        let width = if cell.is_wide() { 2 } else { 1 };
        match &mut run {
            Some((attributes, uri, text, cells))
                if *attributes == cell.attributes() && *uri == link =>
            {
                text.push_str(cell.shown());
                *cells = cells.saturating_add(width);
            }
            _ => {
                if let Some(done) = run.take() {
                    run_json(&done, out, &mut first);
                }
                run = Some((cell.attributes(), link, cell.shown().to_owned(), width));
            }
        }
    }
    if let Some(done) = run {
        run_json(&done, out, &mut first);
    }
    out.push(']');
}

fn run_json(
    (attributes, link, text, cells): &(Attributes, Option<&str>, String, usize),
    out: &mut String,
    first: &mut bool,
) {
    if !std::mem::take(first) {
        out.push(',');
    }
    out.push_str("{\"t\":");
    string(out, text);
    let _ = write!(out, ",\"w\":{cells}");
    for (name, color) in [
        ("fg", attributes.foreground()),
        ("bg", attributes.background()),
        ("ulc", attributes.underline_color()),
    ] {
        match color {
            Color::Default => {}
            Color::Idx(index) => {
                let _ = write!(out, ",\"{name}\":{index}");
            }
            Color::Rgb(rgb) => {
                let _ = write!(
                    out,
                    ",\"{name}\":\"#{:02x}{:02x}{:02x}\"",
                    rgb.r, rgb.g, rgb.b
                );
            }
        }
    }
    for (name, on) in [
        ("b", attributes.bold()),
        ("d", attributes.dim()),
        ("i", attributes.italic()),
        ("inv", attributes.inverse()),
        ("hid", attributes.hidden()),
        ("s", attributes.strikeout()),
    ] {
        if on {
            let _ = write!(out, ",\"{name}\":true");
        }
    }
    if attributes.underline() {
        let _ = write!(
            out,
            ",\"ul\":\"{}\"",
            format!("{:?}", attributes.underline_style()).to_lowercase()
        );
    }
    if attributes.blink() != fux_vt::Blink::None {
        let _ = write!(
            out,
            ",\"blink\":\"{}\"",
            format!("{:?}", attributes.blink()).to_lowercase()
        );
    }
    if let Some(uri) = link {
        out.push_str(",\"link\":");
        string(out, uri);
    }
    out.push('}');
}

/// Appends `text` as a JSON string.
fn string(out: &mut String, text: &str) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if u32::from(c) < 0x20 || c == '\u{7f}' => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}
