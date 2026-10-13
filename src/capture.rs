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
    let read = rows_of(screen, history, view);
    // The cursor's row and column, if it shows on the screen captured.
    let alt = screen.mode(Mode::AlternateScreen);
    let live = if view.main { !alt } else { !view.alt || alt };
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
        padded = (row.cells().last())
            .is_some_and(|c| c.contents().is_empty() && !c.is_wide_continuation());
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
        (lines, view, fux_vt::Color::default()),
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
        ("history_lines", count(screen.history_len())),
        ("copy_mode", Json::Bool(copy_mode)),
        (
            "revision",
            count(usize::try_from(pane.revision).unwrap_or(usize::MAX)),
        ),
        (
            "command_running",
            Json::Bool(commands.running && pane.process.exit_status().is_none()),
        ),
        (
            "last_status",
            commands
                .last_status
                .map_or(Json::Null, |s| Json::Number(i64::from(s))),
        ),
        (
            "bells",
            count(usize::try_from(pane.alerts.count()).unwrap_or(usize::MAX)),
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
    (mut lines, view, base): (Vec<Cells<'_>>, CaptureView, fux_vt::Color),
    state: Option<Vec<(&'static str, Json<'static>)>>,
) -> String {
    // What a selection is drawn with: reverse video, or a background
    // other than the screen's own.
    let highlighted = |a: &Attributes| a.inverse() || a.background() != base;
    let json = state.is_some();
    if !view.full {
        // Blanks are trimmed, unless they show: highlighted with --marks,
        // styled with --styles (as SGR; JSON's lines read alike either way).
        let shows = |a: &Attributes| {
            (view.marks && highlighted(a)) || (view.styles && !json && *a != Attributes::default())
        };
        for line in &mut lines {
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
    }
    // On a row trimmed away, the cursor is on none printed.
    let cursor = cursor.filter(|(row, _)| usize::from(*row) < lines.len());
    let text = |line: &Cells<'_>| match view.marks {
        true => marked(line, &highlighted),
        false => line.iter().map(|(text, _)| *text).collect::<String>(),
    };
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

/// A line with what is highlighted, how programs draw a selection, between
/// `⟦` and `⟧`: marks no program's output is likely to hold.
fn marked(line: &Cells<'_>, highlighted: &impl Fn(&Attributes) -> bool) -> String {
    let mut out = String::new();
    let mut on = false;
    for (text, attributes) in line {
        if highlighted(attributes) != on {
            on = !on;
            out.push(if on { '⟦' } else { '⟧' });
        }
        out.push_str(text);
    }
    if on {
        out.push('⟧');
    }
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
    row.cells_text().trim_end_matches(' ').to_owned()
}
