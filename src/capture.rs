//! What `capture` and `capture-client` print: a screen's text, trimmed,
//! sliced and joined as asked, with its colours and attributes and its
//! pane's state if asked.
use crate::command::{CaptureView, Source};
use crate::id::ClientId;
use crate::json::Json;
use crate::pane::Pane;
use fux_vt::{Attributes, Color, Mode, MouseProtocolMode, Rgb};
use std::time::Instant;

/// A captured line's cells, each its glyph (the second cell of a wide one
/// empty, a blank one a space) and its attributes.
type Cells<'a> = Vec<(&'a str, Attributes)>;

/// A capture as text, and as JSON if it was asked for too: read once.
type Read = (String, Option<String>);

/// A pane's screen, and `history` rows before it, as `view` asks, with
/// `json` its JSON too; with `copy_mode`, that says a client is in copy
/// mode on it.
pub fn pane(
    pane: &Pane,
    history: Option<usize>,
    view: CaptureView,
    json: bool,
    copy_mode: bool,
) -> Read {
    let screen = pane.screen();
    let in_rows = |i: &usize| {
        (view.rows).is_none_or(|(start, end)| (usize::from(start)..=usize::from(end)).contains(i))
    };
    let mut lines: Vec<Cells<'_>> = Vec::new();
    // A soft-wrapped row goes on in the next, joined with `joined`.
    let mut wraps = false;
    let read = rows_of(screen, history, view);
    // The cursor's row and column, if it shows on the screen captured: the
    // last output is read from the screen shown, whatever else is asked.
    let alt = screen.mode(Mode::AlternateScreen);
    let live = match view.source {
        Source::Shown | Source::LastOutput => true,
        Source::Main => !alt,
        Source::Alternate => alt,
    };
    let (cursor_row, cursor_col) = screen.cursor_position();
    let cursor_id = (live && screen.mode(Mode::ShowCursor))
        .then(|| screen.window().row(cursor_row).map(|row| row.id()))
        .flatten();
    // Where it is among the lines captured, found as they are made.
    let mut cursor = None;
    // Whether the row before ended in a blank no glyph was written to: the
    // cell a wide glyph left, not fitting at the row's end.
    let mut padded = false;
    // Joined, a line some of whose rows `rows` takes is taken whole: the
    // line each row is part of, and the lines the range touches.
    let line_of: Vec<usize> = read
        .iter()
        .scan((0usize, false), |(line, wrapped), (row, _)| {
            if !*wrapped {
                *line = line.saturating_add(1);
            }
            *wrapped = view.join && row.wrapped();
            Some(*line)
        })
        .collect();
    let touched: std::collections::HashSet<usize> = (line_of.iter().enumerate())
        .filter(|(i, _)| in_rows(i))
        .map(|(_, line)| *line)
        .collect();
    let taken = |i: &usize| {
        in_rows(i) || (view.join && line_of.get(*i).is_some_and(|line| touched.contains(line)))
    };
    for (_, (row, span)) in read.into_iter().enumerate().filter(|(i, _)| taken(i)) {
        // A wide glyph's second cell is drawn as its first: the same style,
        // so a run of it is not broken in two.
        let mut lead = Attributes::default();
        let cells = row
            .cells()
            .enumerate()
            .filter(|(col, _)| span.contains(col))
            .map(move |(_, cell)| {
                if cell.is_wide_continuation() {
                    ("", lead)
                } else {
                    lead = cell.attributes();
                    (crate::render::shown(cell), lead)
                }
            });
        let cells: Cells<'_> = match view.cols {
            Some((start, end)) => {
                let (start, end) = (usize::from(start), usize::from(end));
                let len = end.saturating_sub(start).saturating_add(1);
                // With the cell after them, to tell a wide glyph cut there.
                let mut cut: Cells<'_> = cells.skip(start).take(len.saturating_add(1)).collect();
                let next = (cut.len() > len).then(|| cut.pop()).flatten();
                // A wide glyph cut in two shows as a blank in the half taken,
                // so each column is one cell.
                if next.is_some_and(|(text, _)| text.is_empty())
                    && let Some(last) = cut.last_mut()
                {
                    last.0 = " ";
                }
                if let Some(first) = cut.first_mut().filter(|(text, _)| text.is_empty()) {
                    first.0 = " ";
                }
                cut
            }
            None => cells.collect(),
        };
        // The cursor's cell among these, if it is on this row and in its
        // columns taken.
        let at = (Some(row.id()) == cursor_id)
            .then(|| {
                let col = usize::from(cursor_col);
                let col = col
                    .checked_sub(span.start)
                    .filter(|_| span.contains(&col))?;
                match view.cols {
                    Some((start, end)) if col <= usize::from(end) => {
                        col.checked_sub(usize::from(start))
                    }
                    Some(_) => None,
                    None => Some(col),
                }
            })
            .flatten();
        let count = lines.len();
        match lines.last_mut() {
            Some(last) if wraps => {
                // The blank a wide glyph left at the row's end, not fitting
                // there, is no part of the line.
                if padded && cells.get(1).is_some_and(|(text, _)| text.is_empty()) {
                    last.pop();
                }
                if let Some(col) = at {
                    cursor = Some((count.saturating_sub(1), last.len().saturating_add(col)));
                }
                last.extend(cells);
            }
            _ => {
                if let Some(col) = at {
                    cursor = Some((count, col));
                }
                lines.push(cells);
            }
        }
        wraps = view.join && row.wrapped();
        padded = ends_padded(&row);
    }
    let (rows, cols) = screen.size().into();
    let cursor =
        cursor.and_then(|(row, col)| Some((u16::try_from(row).ok()?, u16::try_from(col).ok()?)));
    let state = json.then(|| state(pane, Instant::now(), copy_mode));
    print(
        ("pane", pane.id.to_string()),
        (rows, cols),
        cursor,
        (lines, view, screen_background(screen)),
        state,
    )
}

/// Whether `row` ends in a blank no glyph was written to: the cell a wide
/// glyph left, not fitting at the row's end. Of a line wrapped on with that
/// glyph, it is no part.
pub(crate) fn ends_padded(row: &fux_vt::Row<'_>) -> bool {
    (row.cells().last()).is_some_and(|c| c.contents().is_empty() && !c.is_wide_continuation())
}

/// The screen's own background, against which a highlight shows: the
/// terminal's default if a quarter of the screen has it, else the colour
/// most of the screen's cells have (a program that paints its whole
/// screen). Read over the whole screen, whatever part is captured.
fn screen_background(screen: &fux_vt::Screen) -> fux_vt::Color {
    let mut counts: Vec<(fux_vt::Color, usize)> = Vec::new();
    let mut total = 0usize;
    let rows = screen.rows().skip(screen.history_len());
    for cell in rows.flat_map(|row| row.cells()) {
        let colour = cell.attributes().background();
        total = total.saturating_add(1);
        match counts.iter_mut().find(|(c, _)| *c == colour) {
            Some((_, n)) => *n = n.saturating_add(1),
            None => counts.push((colour, 1)),
        }
    }
    let default = fux_vt::Color::default();
    let plain = (counts.iter())
        .find(|(c, _)| *c == default)
        .map_or(0, |(_, n)| *n);
    if plain.saturating_mul(4) >= total {
        return default;
    }
    (counts.into_iter())
        .max_by_key(|(_, n)| *n)
        .map_or(default, |(c, _)| c)
}

/// The rows a capture reads, each with the columns of it it reads: the
/// screen and `history` rows above it (of the main screen with `main-screen`), or
/// the last command's output.
fn rows_of(
    screen: &fux_vt::Screen,
    history: Option<usize>,
    view: CaptureView,
) -> Vec<(fux_vt::Row<'_>, std::ops::Range<usize>)> {
    let all = 0..usize::MAX;
    let rows: Vec<fux_vt::Row<'_>> = match view.source {
        Source::Shown => screen.rows().collect(),
        Source::Main => screen.main_rows().collect(),
        Source::Alternate => screen.alternate_rows().collect(),
        Source::LastOutput => return last_output(screen),
    };
    let shown = usize::from(screen.size().rows());
    let keep = shown.saturating_add(history.unwrap_or(0));
    let skip = rows.len().saturating_sub(keep);
    rows.into_iter()
        .skip(skip)
        .map(|row| (row, all.clone()))
        .collect()
}

/// The last command's output between its marks, a range of each row's
/// columns: while one runs, its output so far, to the cursor.
fn last_output(screen: &fux_vt::Screen) -> Vec<(fux_vt::Row<'_>, std::ops::Range<usize>)> {
    let commands = screen.commands();
    let (row, col) = screen.cursor_position();
    let cursor = (screen.window().row(row)).map(|r| fux_vt::CommandMark { row: r.id(), col });
    let running = commands.output_start.zip(cursor);
    let Some((start, end)) = running.or(commands.last_output) else {
        return Vec::new();
    };
    let Some(last) = screen.row_index(end.row) else {
        return Vec::new();
    };
    // A start gone (history's limit cut it, or the screen was cleared or
    // switched) or below the end: from what is kept above the end, the
    // screen's top if the start was cleared.
    let (first, start) = match screen.row_index(start.row) {
        Some(first) if first <= last && (first, start.col) <= (last, end.col) => (first, start),
        Some(_) => (
            screen.history_len().min(last),
            fux_vt::CommandMark { col: 0, ..start },
        ),
        None => (0, fux_vt::CommandMark { col: 0, ..start }),
    };
    let rows: Vec<fux_vt::Row<'_>> = screen
        .rows()
        .skip(first)
        .take(last.saturating_sub(first).saturating_add(1))
        .collect();
    let count = rows.len();
    rows.into_iter()
        .enumerate()
        .filter_map(|(i, row)| {
            let from = if i == 0 { usize::from(start.col) } else { 0 };
            // The end mark at a line's start ends the output before it;
            // past it, the row is the command's whole (the prompt goes on
            // a row of its own), what it wrote right of the cursor too.
            let to = match (i.saturating_add(1) == count, end.col) {
                (true, 0) => 0,
                (true, col) if i == 0 && start.col >= col => 0,
                _ => usize::MAX,
            };
            (from < to).then_some((row, from..to))
        })
        .collect()
}

/// What a client's terminal shows, row by row, as `capture-client` prints
/// it, and as JSON.
pub fn client(client: ClientId, grid: &crate::render::Grid) -> Read {
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
        (lines, view, fux_vt::Color::default()),
        Some(Vec::new()),
    )
}

/// What a pane's program is doing, for `json`: its foreground program,
/// the status it exited with, how long its screen has shown no change, and
/// the modes
/// that say how it takes input.
pub fn state(pane: &Pane, now: Instant, copy_mode: bool) -> Vec<(&'static str, Json<'static>)> {
    let screen = pane.screen();
    // What runs in the foreground: a job the shell started, else the
    // program itself.
    let foreground =
        (pane.process.child()).map(|child| pane.process.job().unwrap_or(child.leader.pid()));
    let program = foreground.and_then(fuxix::process::name);
    let idle = now.saturating_duration_since(pane.changed_at).as_millis();
    let commands = screen.commands();
    let argv = match (&pane.runs, pane.process.job()) {
        (_, Some(job)) => fuxix::process::argv(job),
        (crate::pane::Runs::Program(launch), None) => Some(launch.argv.clone()),
        (crate::pane::Runs::Shell, None) => foreground.and_then(fuxix::process::argv),
    };
    let (exited, signal) = match pane.process.exit_status() {
        Some(crate::process::Ended::Exited(code)) => (Json::int(code), Json::Null),
        Some(crate::process::Ended::Signalled(signal)) => (Json::Null, Json::int(signal)),
        None => (Json::Null, Json::Null),
    };
    // A program a restore brought back, waiting for `respawn`.
    let paused = matches!(
        (&pane.process, &pane.runs),
        (crate::pane::Process::Absent, crate::pane::Runs::Program(_))
    );
    vec![
        ("program", program.map_or(Json::Null, Json::str)),
        // reading, waiting, busy, exited 0, …: whether to look now.
        ("state", Json::str(crate::ready::state(pane, now).word())),
        ("paused", Json::Bool(paused)),
        ("exited", exited),
        ("signal", signal),
        ("idle_ms", Json::int(idle)),
        ("alt_screen", Json::Bool(screen.mode(Mode::AlternateScreen))),
        (
            "mouse",
            Json::Bool(screen.mouse_protocol_mode() != MouseProtocolMode::None),
        ),
        // What it asked for: whether a move without a button is reported.
        (
            "mouse_mode",
            match screen.mouse_protocol_mode() {
                MouseProtocolMode::None => Json::Null,
                MouseProtocolMode::Press => Json::str("press"),
                MouseProtocolMode::PressRelease => Json::str("press_release"),
                MouseProtocolMode::ButtonMotion => Json::str("button_motion"),
                MouseProtocolMode::AnyMotion => Json::str("any_motion"),
                _ => Json::str("other"),
            },
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
        ("history_lines", Json::int(screen.history_len())),
        ("copy_mode", Json::Bool(copy_mode)),
        ("revision", Json::int(pane.revision)),
        (
            "command_running",
            Json::Bool(commands.running() && pane.process.exit_status().is_none()),
        ),
        (
            "last_status",
            commands.last_status.map_or(Json::Null, Json::int),
        ),
        ("bells", Json::int(pane.alerts.count())),
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

/// Where a picture is, for `json`: its top row (negative once scrolled
/// above the screen), first column, the cells it covers, and its size in
/// pixels.
fn picture(shown: fux_vt::Shown<'_>) -> Json<'static> {
    Json::Object(vec![
        ("row", Json::Number(shown.row)),
        ("col", Json::int(shown.col)),
        ("rows", Json::int(shown.rows)),
        ("cols", Json::int(shown.cols)),
        ("width", Json::int(shown.picture.width)),
        ("height", Json::int(shown.picture.height)),
    ])
}

/// A capture as it is printed: its lines, trimmed unless `view.full`, in
/// SGR with `view.styles`; and with `state` given, as `json` prints it,
/// an object naming what was captured, with its size, cursor, lines, their
/// style runs with `view.styles`, and the state.
fn print(
    (key, id): (&'static str, String),
    (rows, cols): (u16, u16),
    cursor: Option<(u16, u16)>,
    (mut lines, view, base): (Vec<Cells<'_>>, CaptureView, fux_vt::Color),
    state: Option<Vec<(&'static str, Json<'static>)>>,
) -> Read {
    // What a selection is drawn with: reverse video, or a background
    // other than the screen's own.
    let highlighted = |a: &Attributes| a.inverse() || a.background() != base;
    // Blanks are trimmed, unless they show: highlighted with `marks`,
    // styled with `styles` (as SGR; JSON's lines read alike either way, so
    // the text's lines, trimmed again, are the JSON's). `view.full` keeps
    // them all.
    let trim = |lines: &mut Vec<Cells<'_>>, styled: bool| {
        if view.full {
            return;
        }
        let shows = |a: &Attributes| {
            (view.marks && highlighted(a)) || (styled && *a != Attributes::default())
        };
        for line in lines.iter_mut() {
            while line
                .last()
                .is_some_and(|(text, a)| *text == " " && !shows(a))
            {
                line.pop();
            }
        }
        while lines.last().is_some_and(Vec::is_empty) {
            lines.pop();
        }
    };
    let marks = view.marks.then_some(&highlighted);
    let text = |line: &Cells<'_>, styles: bool| sgr_line(line, marks, styles);
    trim(&mut lines, view.styles);
    let shown: Vec<String> = lines.iter().map(|l| text(l, view.styles)).collect();
    let mut out = shown.join("\n");
    out.push('\n');
    let json = state.map(|state| {
        trim(&mut lines, false);
        // On a row trimmed away, the cursor is on none printed.
        let cursor = cursor.filter(|(row, _)| usize::from(*row) < lines.len());
        let cursor = cursor.map_or(Json::Null, |(y, x)| {
            Json::Array(vec![Json::int(y), Json::int(x)])
        });
        let mut fields = vec![
            (key, Json::str(id)),
            ("rows", Json::int(rows)),
            ("cols", Json::int(cols)),
            ("cursor", cursor),
            (
                "lines",
                Json::Array(lines.iter().map(|l| Json::str(text(l, false))).collect()),
            ),
        ];
        if view.styles {
            fields.push(("styles", Json::Array(lines.iter().map(runs).collect())));
        }
        fields.extend(state);
        format!("{}\n", Json::Object(fields).render())
    });
    (out, json)
}

/// A line, in SGR with `styles`: each change of attributes set from
/// nothing, and reset at its end. With `marks`, what is highlighted, how
/// programs draw a selection, is between `⟦` and `⟧` (drawn outside the
/// SGR): marks no program's output is likely to hold.
fn sgr_line(
    line: &Cells<'_>,
    marks: Option<&impl Fn(&Attributes) -> bool>,
    styles: bool,
) -> String {
    let mut out = Vec::new();
    let mut pen = Attributes::default();
    let mut on = false;
    for (text, attributes) in line {
        let lit = marks.is_some_and(|highlighted| highlighted(attributes));
        if lit && !on {
            out.extend_from_slice("⟦".as_bytes());
        }
        if styles && *attributes != pen {
            crate::render::sgr(&mut out, *attributes, true);
            pen = *attributes;
        }
        if on && !lit {
            out.extend_from_slice("⟧".as_bytes());
        }
        on = lit;
        out.extend_from_slice(text.as_bytes());
    }
    if pen != Attributes::default() {
        out.extend_from_slice(b"\x1b[0m");
    }
    if on {
        out.extend_from_slice("⟧".as_bytes());
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
            let mut fields = vec![("col", Json::int(start)), ("len", Json::int(len))];
            fields.extend(described(*attributes));
            runs.push(Json::Object(fields));
        }
        start = start.saturating_add(len);
    }
    Json::Array(runs)
}

/// Attributes as `json` names them: the colours other than the default,
/// and the attributes set.
fn described(a: Attributes) -> Vec<(&'static str, Json<'static>)> {
    let colour = |c: Color| match c {
        Color::Idx(n) => Some(Json::int(n)),
        Color::Rgb(Rgb { r, g, b }) => Some(Json::str(format!("#{r:02x}{g:02x}{b:02x}"))),
        // A kind of colour fux-vt does not know yet is told as the default.
        Color::Default | _ => None,
    };
    let mut fields = Vec::new();
    fields.extend(colour(a.foreground()).map(|c| ("fg", c)));
    fields.extend(colour(a.background()).map(|c| ("bg", c)));
    let flags = a.sgr_flags().map(|(_, name)| name);
    let underline = a.underline().then_some("underline");
    let blink = (a.blink() != fux_vt::Blink::None).then_some("blink");
    fields.extend(
        flags
            .chain(underline)
            .chain(blink)
            .map(|name| (name, Json::Bool(true))),
    );
    fields
}

/// A row's text, wide glyphs whole and trailing blanks trimmed.
pub fn row_text(row: fux_vt::Row<'_>) -> String {
    row.cells_text().trim_end_matches(' ').to_owned()
}
