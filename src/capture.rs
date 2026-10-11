//! What `capture-pane` and `capture-client` print: a screen's text, trimmed,
//! sliced and joined as asked, with its colours and attributes and its
//! pane's state if asked.
use crate::command::CaptureView;
use crate::id::ClientId;
use crate::json::Json;
use crate::pane::Pane;
use fux_vt::{Attributes, Color, Mode, MouseProtocolMode, Rgb};
use std::time::Instant;

/// A captured line's cells, each its glyph (the second cell of a wide one
/// empty, a blank one a space) and its attributes.
type Cells<'a> = Vec<(&'a str, Attributes)>;

/// A pane's screen, and `history` rows before it, as `view` asks.
pub fn pane(pane: &Pane, history: Option<usize>, view: CaptureView, json: bool) -> String {
    let screen = pane.screen();
    // History rows above the screen, oldest first, then the screen itself.
    let top = (screen.window().row(0)).map(|top| top.up(history.unwrap_or(0)));
    let rows = std::iter::successors(top, fux_vt::Row::below).enumerate();
    let in_rows = |i: &usize| {
        (view.rows).is_none_or(|(start, end)| (usize::from(start)..=usize::from(end)).contains(i))
    };
    let mut lines: Vec<Cells<'_>> = Vec::new();
    // A soft-wrapped row goes on in the next, joined with `-J`.
    let mut wraps = false;
    for (_, row) in rows.filter(|(i, _)| in_rows(i)) {
        let cells = row.cells().map(|cell| {
            let text = if cell.is_wide_continuation() {
                ""
            } else {
                crate::render::shown(cell)
            };
            (text, cell.attributes())
        });
        let cells: Cells<'_> = match view.cols {
            Some((start, end)) => {
                let (start, end) = (usize::from(start), usize::from(end));
                cells
                    .skip(start)
                    .take(end.saturating_sub(start).saturating_add(1))
                    .collect()
            }
            None => cells.collect(),
        };
        match lines.last_mut() {
            Some(last) if wraps => last.extend(cells),
            _ => lines.push(cells),
        }
        wraps = view.join && row.wrapped();
    }
    let (rows, cols) = screen.size().into();
    let cursor = Some(screen.cursor_position());
    let state = json.then(|| state(pane, Instant::now()));
    print(
        ("pane", pane.id.to_string()),
        (rows, cols),
        cursor,
        (lines, view),
        state,
    )
}

/// What a client's terminal shows, row by row, as `capture-client` prints
/// it.
pub fn client(client: ClientId, grid: &crate::render::Grid, json: bool) -> String {
    let lines = (0..grid.rows).map(|y| grid.row_text(y)).collect::<Vec<_>>();
    let lines = lines
        .iter()
        .map(|line| vec![(line.as_str(), Attributes::default())])
        .collect();
    let view = CaptureView {
        full: true,
        ..CaptureView::default()
    };
    print(
        ("client", client.to_string()),
        (grid.rows, grid.cols),
        grid.cursor,
        (lines, view),
        json.then(Vec::new),
    )
}

/// What a pane's program is doing, for `--json`: its foreground program,
/// the status it exited with, how long since it last wrote, and the modes
/// that say how it takes input.
pub fn state(pane: &Pane, now: Instant) -> Vec<(&'static str, Json<'static>)> {
    let screen = pane.screen();
    let program = pane
        .process
        .child()
        .map(|child| pane.process.job().unwrap_or(child.leader.pid()))
        .and_then(fuxix::process::name);
    let idle = now.saturating_duration_since(pane.wrote_at).as_millis();
    let exited = pane.process.exit_status();
    vec![
        ("program", program.map_or(Json::Null, Json::str)),
        (
            "exited",
            exited.map_or(Json::Null, |s| Json::Number(i64::from(s))),
        ),
        (
            "idle_ms",
            Json::Number(i64::try_from(idle).unwrap_or(i64::MAX)),
        ),
        ("alt_screen", Json::Bool(screen.mode(Mode::AlternateScreen))),
        (
            "mouse",
            Json::Bool(screen.mouse_protocol_mode() != MouseProtocolMode::None),
        ),
        (
            "bracketed_paste",
            Json::Bool(screen.mode(Mode::BracketedPaste)),
        ),
    ]
}

/// A capture as it is printed: its lines, trimmed unless `view.full`, in
/// SGR with `view.styles`; or with `--json` (`state` given) an object
/// naming what was captured, with its size, cursor, lines, their style runs
/// with `view.styles`, and the state.
fn print(
    (key, id): (&'static str, String),
    (rows, cols): (u16, u16),
    cursor: Option<(u16, u16)>,
    (mut lines, view): (Vec<Cells<'_>>, CaptureView),
    state: Option<Vec<(&'static str, Json<'static>)>>,
) -> String {
    if !view.full {
        for line in &mut lines {
            while line.last().is_some_and(|(text, _)| *text == " ") {
                line.pop();
            }
        }
        while lines.last().is_some_and(Vec::is_empty) {
            lines.pop();
        }
    }
    let text = |line: &Cells<'_>| line.iter().map(|(text, _)| *text).collect::<String>();
    let mut out = match state {
        Some(state) => {
            let number = |n: u16| Json::Number(i64::from(n));
            let cursor =
                cursor.map_or(Json::Null, |(y, x)| Json::Array(vec![number(y), number(x)]));
            let mut fields = vec![
                (key, Json::str(id)),
                ("rows", number(rows)),
                ("cols", number(cols)),
                ("cursor", cursor),
                (
                    "lines",
                    Json::Array(lines.iter().map(|l| Json::str(text(l))).collect()),
                ),
            ];
            if view.styles {
                fields.push(("styles", Json::Array(lines.iter().map(runs).collect())));
            }
            fields.extend(state);
            Json::Object(fields).render()
        }
        None if view.styles => {
            let styled: Vec<String> = lines.iter().map(sgr_line).collect();
            styled.join("\n")
        }
        None => lines.iter().map(text).collect::<Vec<_>>().join("\n"),
    };
    out.push('\n');
    out
}

/// A line in SGR: each change of attributes set from nothing, and reset at
/// its end.
fn sgr_line(line: &Cells<'_>) -> String {
    let mut out = Vec::new();
    let mut pen = Attributes::default();
    for (text, attributes) in line {
        if *attributes != pen {
            crate::render::sgr(&mut out, *attributes, true);
            pen = *attributes;
        }
        out.extend_from_slice(text.as_bytes());
    }
    if pen != Attributes::default() {
        out.extend_from_slice(b"\x1b[0m");
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A line's runs of cells with attributes other than the default: where
/// each starts, from 0, how many cells, and what it has.
fn runs(line: &Cells<'_>) -> Json<'static> {
    let mut runs = Vec::new();
    let mut start = 0usize;
    for group in line.chunk_by(|(_, a), (_, b)| a == b) {
        let len = group.len();
        if let Some((_, attributes)) = group.first().filter(|(_, a)| *a != Attributes::default()) {
            let count = |n: usize| Json::Number(i64::try_from(n).unwrap_or(i64::MAX));
            let mut fields = vec![("col", count(start)), ("len", count(len))];
            fields.extend(described(*attributes));
            runs.push(Json::Object(fields));
        }
        start = start.saturating_add(len);
    }
    Json::Array(runs)
}

/// Attributes as `--json` names them: the colours other than the default,
/// and the attributes set.
fn described(a: Attributes) -> Vec<(&'static str, Json<'static>)> {
    let colour = |c: Color| match c {
        Color::Idx(n) => Some(Json::Number(i64::from(n))),
        Color::Rgb(Rgb { r, g, b }) => Some(Json::str(format!("#{r:02x}{g:02x}{b:02x}"))),
        // A kind of colour fux-vt does not know yet is told as the default.
        Color::Default | _ => None,
    };
    let mut fields = Vec::new();
    fields.extend(colour(a.foreground()).map(|c| ("fg", c)));
    fields.extend(colour(a.background()).map(|c| ("bg", c)));
    let flags = [
        ("bold", a.bold()),
        ("dim", a.dim()),
        ("italic", a.italic()),
        ("underline", a.underline()),
        ("blink", a.blink() != fux_vt::Blink::None),
        ("inverse", a.inverse()),
        ("hidden", a.hidden()),
        ("strikeout", a.strikeout()),
    ];
    fields.extend(
        (flags.into_iter()).filter_map(|(name, set)| set.then_some((name, Json::Bool(true)))),
    );
    fields
}

/// A row's text, wide glyphs whole and trailing blanks trimmed.
pub fn row_text(row: fux_vt::Row<'_>) -> String {
    let row = row.cells().filter(|c| !c.is_wide_continuation());
    let line: String = row.map(crate::render::shown).collect();
    line.trim_end_matches(' ').to_owned()
}
