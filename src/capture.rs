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

/// A pane's screen, and `history` rows before it, as `view` asks; with
/// `copy_mode`, its `--json` says a client is in copy mode on it.
pub fn pane(
    pane: &Pane,
    history: Option<usize>,
    view: CaptureView,
    json: bool,
    copy_mode: bool,
) -> String {
    let screen = pane.screen();
    let in_rows = |i: &usize| {
        (view.rows).is_none_or(|(start, end)| (usize::from(start)..=usize::from(end)).contains(i))
    };
    let mut lines: Vec<Cells<'_>> = Vec::new();
    // A soft-wrapped row goes on in the next, joined with `-J`.
    let mut wraps = false;
    for (_, (row, span)) in rows_of(screen, history, view)
        .into_iter()
        .enumerate()
        .filter(|(i, _)| in_rows(i))
    {
        let cells = row
            .cells()
            .enumerate()
            .filter(|(col, _)| span.contains(col))
            .map(|(_, cell)| {
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
    let state = json.then(|| state(pane, Instant::now(), copy_mode));
    print(
        ("pane", pane.id.to_string()),
        (rows, cols),
        cursor,
        (lines, view),
        state,
    )
}

/// The rows a capture reads, each with the columns of it it reads: the
/// screen and `history` rows above it (of the main screen with `--main`), or
/// the last command's output between its marks.
fn rows_of(
    screen: &fux_vt::Screen,
    history: Option<usize>,
    view: CaptureView,
) -> Vec<(fux_vt::Row<'_>, std::ops::Range<usize>)> {
    let all = 0..usize::MAX;
    if view.last_output {
        let Some((start, end)) = screen.commands().last_output else {
            return Vec::new();
        };
        let (Some(first), Some(last)) = (screen.row_index(start.row), screen.row_index(end.row))
        else {
            return Vec::new();
        };
        let rows: Vec<fux_vt::Row<'_>> = screen
            .rows()
            .skip(first)
            .take(last.saturating_sub(first).saturating_add(1))
            .collect();
        let count = rows.len();
        return rows
            .into_iter()
            .enumerate()
            .filter_map(|(i, row)| {
                let from = if i == 0 { usize::from(start.col) } else { 0 };
                let to = if i.saturating_add(1) == count {
                    usize::from(end.col)
                } else {
                    usize::MAX
                };
                // The end mark at a line's start ends the output before it.
                (from < to).then_some((row, from..to))
            })
            .collect();
    }
    let rows: Vec<fux_vt::Row<'_>> = if view.main {
        screen.main_rows().collect()
    } else if view.alt {
        screen.alternate_rows().collect()
    } else {
        screen.rows().collect()
    };
    let shown = usize::from(screen.size().rows());
    let keep = shown.saturating_add(history.unwrap_or(0));
    let skip = rows.len().saturating_sub(keep);
    rows.into_iter()
        .skip(skip)
        .map(|row| (row, all.clone()))
        .collect()
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
/// the status it exited with, how long its screen has shown no change, and
/// the modes
/// that say how it takes input.
pub fn state(pane: &Pane, now: Instant, copy_mode: bool) -> Vec<(&'static str, Json<'static>)> {
    let screen = pane.screen();
    let program = pane
        .process
        .child()
        .map(|child| pane.process.job().unwrap_or(child.leader.pid()))
        .and_then(fuxix::process::name);
    let idle = now.saturating_duration_since(pane.changed_at).as_millis();
    let commands = screen.commands();
    // What runs in the foreground: a job the shell started, else the
    // program itself.
    let foreground =
        (pane.process.child()).map(|child| pane.process.job().unwrap_or(child.leader.pid()));
    let argv = match (&pane.runs, pane.process.job()) {
        (_, Some(job)) => fuxix::process::argv(job),
        (crate::pane::Runs::Program(launch), None) => Some(launch.argv.clone()),
        (crate::pane::Runs::Shell, None) => foreground.and_then(fuxix::process::argv),
    };
    let exited = pane.process.exit_status();
    vec![
        ("program", program.map_or(Json::Null, Json::str)),
        (
            "exited",
            match exited {
                Some(crate::process::Ended::Exited(code)) => Json::Number(i64::from(code)),
                _ => Json::Null,
            },
        ),
        (
            "signal",
            match exited {
                Some(crate::process::Ended::Signalled(signal)) => Json::Number(i64::from(signal)),
                _ => Json::Null,
            },
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
        (
            "pictures",
            Json::Array(screen.pictures().map(picture).collect()),
        ),
        ("cursor_visible", Json::Bool(screen.mode(Mode::ShowCursor))),
        ("history_lines", count(screen.history_len())),
        ("copy_mode", Json::Bool(copy_mode)),
        (
            "revision",
            count(usize::try_from(pane.revision).unwrap_or(usize::MAX)),
        ),
        ("running", Json::Bool(commands.running)),
        (
            "last_status",
            commands
                .last_status
                .map_or(Json::Null, |s| Json::Number(i64::from(s))),
        ),
        (
            "bells",
            count(usize::try_from(pane.alerts.bells).unwrap_or(usize::MAX)),
        ),
        (
            "notification",
            pane.alerts.last.clone().map_or(Json::Null, Json::str),
        ),
        (
            "cwd",
            foreground
                .and_then(fuxix::process::cwd)
                .map_or(Json::Null, |p| Json::str(p.to_string_lossy().into_owned())),
        ),
        (
            "argv",
            argv.map_or(Json::Null, |a| {
                Json::Array(a.into_iter().map(Json::str).collect())
            }),
        ),
    ]
}

fn count(n: usize) -> Json<'static> {
    Json::Number(i64::try_from(n).unwrap_or(i64::MAX))
}

/// Where a picture is, for `--json`: its top row (negative once scrolled
/// above the screen), first column, the cells it covers, and its size in
/// pixels.
fn picture(shown: fux_vt::Shown<'_>) -> Json<'static> {
    let number = |n: u32| Json::Number(i64::from(n));
    Json::Object(vec![
        ("row", Json::Number(shown.row)),
        ("col", number(u32::from(shown.col))),
        ("rows", number(u32::from(shown.rows))),
        ("cols", number(u32::from(shown.cols))),
        ("width", number(shown.picture.width)),
        ("height", number(shown.picture.height)),
    ])
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
