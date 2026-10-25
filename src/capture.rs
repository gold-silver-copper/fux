//! What `capture` and `capture-client` print: a screen's text, trimmed,
//! sliced and joined as asked, with its colours and attributes and its
//! pane's state if asked.
use crate::command::{CaptureView, Run, Source};
use crate::id::ClientId;
use crate::json::Json;
use crate::pane::{Pane, Process, Runs};
use fux_vt::{Attributes, Color, CommandMark, Mode, MouseProtocolMode, Rgb, Row, Screen};
use std::time::Instant;

/// A captured line's cells, each its glyph (the second cell of a wide one
/// empty, a blank one a space) and its attributes.
type Cells<'a> = Vec<(&'a str, Attributes)>;

/// A capture as text, and as JSON, read once (`json` gives the second).
type Read = (String, String);

/// A pane's screen, and `history` rows before it, as `view` asks, and its
/// JSON (which `json` gives, read once with the text); with `copy_mode`,
/// that says a client is in copy mode on it.
pub fn pane(
    pane: &Pane,
    run: Run,
    history: Option<usize>,
    view: CaptureView,
    copy_mode: bool,
) -> Read {
    let screen = pane.screen();
    let mut lines: Vec<Cells<'_>> = Vec::new();
    // A soft-wrapped row goes on in the next, joined unless `wrapped`.
    let mut wraps = false;
    let (read, above) = rows_of(screen, history, view);
    // The cursor's row and column, if it shows on the screen captured: the
    // last output is read from the screen shown, whatever else is asked.
    let alt = screen.mode(Mode::AlternateScreen);
    let live = match view.source {
        Source::Shown | Source::LastOutput => true,
        Source::Main => !alt,
        Source::Alternate => alt,
    };
    // Where it is on the screen, as a mouse event and a state line count.
    let cursor = (live && screen.mode(Mode::ShowCursor)).then(|| screen.cursor_position());
    // Joined, a line some of whose rows `rows` takes is taken whole: the
    // range widened to the whole lines at its ends.
    let continues = |i: usize| !view.wrapped && read.get(i).is_some_and(|(row, _)| row.continues());
    let taken = view.rows.map(|(start, end)| {
        let (mut first, mut last) = (usize::from(start), usize::from(end));
        while let Some(before) = first.checked_sub(1)
            && continues(before)
        {
            first = before;
        }
        while continues(last) {
            last = last.saturating_add(1);
        }
        first..=last
    });
    let taken = |i: &usize| taken.as_ref().is_none_or(|rows| rows.contains(i));
    for (_, (row, span)) in read.into_iter().enumerate().filter(|(i, _)| taken(i)) {
        // A wide glyph's second cell is drawn as its first: the same style,
        // so a run of it is not broken in two.
        let mut lead = Attributes::default();
        // Joined, the spacer a wide glyph left at the row's end, not fitting
        // there, is no part of the line (`Row::ends_in_spacer`).
        let spacer = usize::from(!view.wrapped && row.ends_in_spacer());
        let cells = row
            .cells()
            .take(row.len().saturating_sub(spacer))
            .enumerate()
            .filter(|(col, _)| span.contains(col))
            .map(move |(_, cell)| {
                if cell.is_wide_continuation() {
                    ("", lead)
                } else {
                    lead = cell.attributes();
                    (cell.shown(), lead)
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
        match lines.last_mut() {
            Some(last) if wraps => last.extend(cells),
            _ => lines.push(cells),
        }
        wraps = !view.wrapped && row.continues();
    }
    let (rows, cols) = screen.size().into();
    let mut state = state(pane, run, Instant::now(), copy_mode);
    // The history rows kept above the first row read: history it was not
    // asked for (rows a range leaves out are not counted).
    state.push(("above", above.map_or(Json::Null, Json::int)));
    // A glyph went into the last column: the next one goes on the next
    // line, though the cursor shows on this one's last cell.
    let pending = cursor.is_some() && screen.pending_wrap();
    print(
        ("pane", pane.id.to_string()),
        (rows, cols),
        (cursor, Some(pending)),
        (lines, view),
        state,
    )
}

/// The rows a capture reads, each with the columns of it it reads: the
/// screen and `history` rows above it (of the main screen with
/// `main-screen`), and how many rows are kept above them; or the last
/// command's output.
fn rows_of(
    screen: &Screen,
    history: Option<usize>,
    view: CaptureView,
) -> (Vec<(Row<'_>, std::ops::Range<usize>)>, Option<usize>) {
    let rows = match view.source {
        Source::Shown => screen.rows(),
        Source::Main => screen.main_rows(),
        Source::Alternate => screen.alternate_rows(),
        Source::LastOutput => return (last_output(screen), None),
    };
    let shown = usize::from(screen.size().rows());
    let keep = shown.saturating_add(history.unwrap_or(0));
    // Only the rows kept are read: history past them is skipped unread.
    let skip = rows.len().saturating_sub(keep);
    let read = rows.skip(skip).map(|row| (row, 0..usize::MAX)).collect();
    (read, Some(skip))
}

/// The last command's output between its marks, a range of each row's
/// columns: while one runs, its output so far, to the cursor.
fn last_output(screen: &Screen) -> Vec<(Row<'_>, std::ops::Range<usize>)> {
    let commands = screen.commands();
    let (row, col) = screen.cursor_position();
    let cursor = (screen.window().row(row)).map(|r| CommandMark { row: r.id(), col });
    let running = commands.output_start.zip(cursor);
    let ended = running.is_none();
    let Some((start, end)) = running.or(commands.last_output) else {
        return Vec::new();
    };
    let Some(last) = screen.row_index(end.row) else {
        return Vec::new();
    };
    // A start gone (history's limit cut it, or the screen was cleared or
    // switched) or below the end: from what is kept above the end, the
    // screen's top if the start was cleared.
    let left = CommandMark { col: 0, ..start };
    let (first, start) = match screen.row_index(start.row) {
        // An ended command's end left of its start on one row (its last
        // write a `\r`): the row up to the end. A running one's cursor may
        // go back so too, its row then whole.
        Some(first) if first == last && ended && start.col > end.col => (first, left),
        Some(first) if first <= last => (first, start),
        Some(_) => (screen.history_len().min(last), left),
        None => (0, left),
    };
    let rows: Vec<Row<'_>> = screen
        .rows()
        .skip(first)
        .take(last.saturating_sub(first).saturating_add(1))
        .collect();
    let count = rows.len();
    rows.into_iter()
        .enumerate()
        .filter_map(|(i, row)| {
            let from = if i == 0 { usize::from(start.col) } else { 0 };
            // An ended command's output ends at its end mark; a running
            // one's last row is whole, what it wrote right of the cursor
            // too.
            let to = match (i.saturating_add(1) == count && ended, end.col) {
                (true, col) if i == 0 && start.col >= col => 0,
                (true, col) => usize::from(col),
                (false, _) => usize::MAX,
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
        (grid.cursor, None),
        (lines, view),
        Vec::new(),
    )
}

/// What is known of a pane's program, for `json`: its foreground program,
/// the status it exited with, how long since it last wrote, and the modes
/// that say how it takes input.
pub fn state(
    pane: &Pane,
    run: Run,
    now: Instant,
    copy_mode: bool,
) -> Vec<(&'static str, Json<'static>)> {
    let screen = pane.screen();
    // What runs in the foreground: a job the shell started, else the
    // program itself; nothing, where its terminal can't tell.
    let job = pane.process.job();
    let foreground = (pane.process.child()).and_then(|child| match job {
        Ok(job) => Some(job.unwrap_or(child.leader.pid())),
        Err(_) => None,
    });
    let program = foreground.and_then(fuxix::process::name);
    let idle = now.saturating_duration_since(pane.wrote_at).as_millis();
    let last_status = screen.commands().last_status;
    let argv = match (&pane.runs, job) {
        (_, Ok(Some(job))) => fuxix::process::argv(job),
        (Runs::Program(launch), Ok(None)) => Some(launch.argv.clone()),
        (Runs::Shell, Ok(None)) => foreground.and_then(fuxix::process::argv),
        (_, Err(_)) => None,
    };
    let (exited, signal) = match pane.process.exit_status() {
        Some(crate::process::Ended::Exited(code)) => (Json::int(code), Json::Null),
        Some(crate::process::Ended::Signalled(signal)) => (Json::Null, Json::int(signal)),
        Some(crate::process::Ended::Lost) | None => (Json::Null, Json::Null),
    };
    let paused = pane.paused();
    let hung_up = matches!(pane.process, Process::HungUp(_));
    let mouse = screen.mouse_protocol_mode();
    let bracketed_paste = screen.mode(Mode::BracketedPaste);
    let pictures = Json::Array(screen.pictures().map(picture).collect());
    let notification = pane.alerts.last.clone();
    vec![
        ("version", Json::str(pane.version(run).to_string())),
        ("program", program.map_or(Json::Null, Json::str)),
        ("paused", Json::Bool(paused)),
        // Its program closed its terminal and runs on: nothing reads keys
        // there (the terminal hung up; Linux says so).
        ("hung_up", Json::Bool(hung_up)),
        ("exited", exited),
        ("signal", signal),
        ("idle_ms", Json::int(idle)),
        ("alt_screen", Json::Bool(screen.mode(Mode::AlternateScreen))),
        ("mouse", Json::Bool(mouse != MouseProtocolMode::None)),
        // What it asked for: whether a move without a button is reported.
        (
            "mouse_mode",
            match mouse {
                MouseProtocolMode::None => Json::Null,
                MouseProtocolMode::Press => Json::str("press"),
                MouseProtocolMode::PressRelease => Json::str("press_release"),
                MouseProtocolMode::Highlight => Json::str("highlight"),
                MouseProtocolMode::ButtonMotion => Json::str("button_motion"),
                MouseProtocolMode::AnyMotion => Json::str("any_motion"),
            },
        ),
        ("bracketed_paste", Json::Bool(bracketed_paste)),
        ("pictures", pictures),
        (
            "cell",
            screen.cell_size().map_or(Json::Null, |cell| {
                Json::Object(vec![
                    ("width", Json::int(cell.width())),
                    ("height", Json::int(cell.height())),
                ])
            }),
        ),
        ("cursor_visible", Json::Bool(screen.mode(Mode::ShowCursor))),
        ("history_lines", Json::int(screen.history_len())),
        ("copy_mode", Json::Bool(copy_mode)),
        ("command_running", Json::Bool(pane.command_running())),
        ("last_status", last_status.map_or(Json::Null, Json::int)),
        ("bells", Json::int(pane.alerts.rung)),
        ("notification", notification.map_or(Json::Null, Json::str)),
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
/// above the screen), first column, the cells it covers, its first rows
/// that are gone (`cropped`: scrolled off a region's top or a screen's
/// that keeps no history, the rest shown below them), and its size in
/// pixels.
fn picture(shown: fux_vt::Shown<'_>) -> Json<'static> {
    Json::Object(vec![
        ("row", Json::Number(shown.row)),
        ("col", Json::int(shown.col)),
        ("rows", Json::int(shown.rows)),
        ("cropped", Json::int(shown.cropped)),
        ("cols", Json::int(shown.cols)),
        ("width", Json::int(shown.picture.width())),
        ("height", Json::int(shown.picture.height())),
    ])
}

/// A capture as it is printed: its lines, trimmed unless `view.full`, in
/// SGR with `view.styles`; and with `state` given, as `json` prints it,
/// an object naming what was captured, with its size, cursor, lines, their
/// style runs with `view.styles`, and the state.
fn print(
    (key, id): (&'static str, String),
    (rows, cols): (u16, u16),
    (cursor, pending): (Option<(u16, u16)>, Option<bool>),
    (mut lines, view): (Vec<Cells<'_>>, CaptureView),
    state: Vec<(&'static str, Json<'static>)>,
) -> Read {
    // Highlighted: reverse video, or a background colour.
    let highlighted = |a: &Attributes| a.inverse() || a.background() != Color::Default;
    // A blank's style shows on the screen: a background colour, reverse
    // video, a line under or through it. One bold or coloured in front
    // shows none.
    let painted = |a: &Attributes| highlighted(a) || a.underline() || a.strikeout();
    // Blanks at a line's end are trimmed, unless they show: highlighted
    // with `marks`, painted with `styles` (alike as SGR and in JSON's lines
    // and runs, a painted surface kept whole); plain text trims them all
    // (blanks, however painted, are no text). `view.full` keeps them all.
    let trim = |lines: &mut Vec<Cells<'_>>| {
        if view.full {
            return;
        }
        let shows = |a: &Attributes| (view.marks && highlighted(a)) || (view.styles && painted(a));
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
    trim(&mut lines);
    let shown: Vec<String> = lines.iter().map(|l| text(l, view.styles)).collect();
    let mut out = shown.join("\n");
    out.push('\n');
    // JSON's lines are the text's, trimmed alike.
    // The screen's own row and column, as the state line says them,
    // whatever blank rows are trimmed from the lines.
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
    fields.extend(pending.map(|p| ("wrap_pending", Json::Bool(p))));
    if view.styles {
        fields.push(("styles", Json::Array(lines.iter().map(runs).collect())));
    }
    fields.extend(state);
    (out, format!("{}\n", Json::Object(fields).render()))
}

/// A line, in SGR with `styles`: each change of attributes set from
/// nothing, and reset at its end. With `marks`, what is highlighted
/// (reverse video, or a background colour) is between `⟦` and `⟧`, drawn
/// outside the SGR.
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
        Color::Default => None,
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

#[cfg(test)]
mod tests {
    use crate::command::CaptureView;
    use crate::id::PaneId;
    use crate::pane::{Pane, Runs};

    /// A soft-wrapped row's last cell is the spacer a wide glyph left, no
    /// part of the line, only while nothing was painted there: a blank EL
    /// erased in a colour is a cell the program painted, and shows, so a
    /// joined capture keeps it, as a copy does (`Row::copied`).
    #[test]
    fn a_painted_blank_before_a_wide_glyph_wrapped_on_is_part_of_the_line()
    -> Result<(), Box<dyn std::error::Error>> {
        let join = CaptureView::default();
        let captured = |output: &str| -> Result<String, Box<dyn std::error::Error>> {
            let size = fux_vt::Size::new(3, 5)?;
            let mut pane = Pane::new(PaneId::of(1), "sh".into(), Runs::Shell, size, 0)?;
            pane.output(output.as_bytes());
            Ok(super::pane(
                &pane,
                crate::command::Run::now().ok_or("a clock")?,
                None,
                join,
                false,
            )
            .0)
        };
        // Cols 3 and 4 erased, 世 at col 4 does not fit and wraps on.
        let first = |output| {
            Ok::<_, Box<dyn std::error::Error>>(captured(output)?.lines().next().map(str::to_owned))
        };
        assert_eq!(first("abc\x1b[K\x1b[5G世")?.as_deref(), Some("abc 世"));
        assert_eq!(
            first("abc\x1b[41m\x1b[K\x1b[m\x1b[5G世")?.as_deref(),
            Some("abc  世")
        );
        Ok(())
    }

    /// Rows that start or end inside a wrapped line take it whole, joined;
    /// `wrapped`, they take the rows asked alone.
    #[test]
    fn rows_inside_a_wrapped_line_take_it_whole() -> Result<(), Box<dyn std::error::Error>> {
        let size = fux_vt::Size::new(6, 5)?;
        let mut pane = Pane::new(PaneId::of(1), "sh".into(), Runs::Shell, size, 0)?;
        pane.output(b"top\r\nabcdefghijklm\r\nend");
        let run = crate::command::Run::now().ok_or("a clock")?;
        let capture = |rows, wrapped| {
            let view = CaptureView {
                rows: Some(rows),
                wrapped,
                ..CaptureView::default()
            };
            super::pane(&pane, run, None, view, false).0
        };
        assert_eq!(capture((2, 2), false), "abcdefghijklm\n");
        assert_eq!(capture((0, 1), false), "top\nabcdefghijklm\n");
        assert_eq!(capture((3, 4), false), "abcdefghijklm\nend\n");
        assert_eq!(capture((2, 2), true), "fghij\n");
        Ok(())
    }

    /// Columns cut from a joined line leave the spacer out only when they
    /// take its column: cut short of it, the last cell taken is text.
    #[test]
    fn columns_cut_short_of_the_spacer_keep_their_last_cell()
    -> Result<(), Box<dyn std::error::Error>> {
        let view = CaptureView {
            cols: Some((0, 2)),
            ..CaptureView::default()
        };
        let size = fux_vt::Size::new(3, 5)?;
        let mut pane = Pane::new(PaneId::of(1), "sh".into(), Runs::Shell, size, 0)?;
        // 世 at col 4 does not fit and wraps on, leaving the spacer there.
        pane.output("abcd\x1b[5G世".as_bytes());
        let text = super::pane(
            &pane,
            crate::command::Run::now().ok_or("a clock")?,
            None,
            view,
            false,
        )
        .0;
        assert_eq!(text.lines().next(), Some("abc世"));
        Ok(())
    }

    /// An ended command's output is what lies between its marks: a prompt
    /// drawn after its end mark on the same row is not part of it. A
    /// running one's is all of its rows so far.
    #[test]
    fn last_output_ends_at_the_end_mark() -> Result<(), Box<dyn std::error::Error>> {
        let view = CaptureView {
            source: crate::command::Source::LastOutput,
            ..CaptureView::default()
        };
        let size = fux_vt::Size::new(3, 20)?;
        let mut pane = Pane::new(PaneId::of(1), "sh".into(), Runs::Shell, size, 0)?;
        pane.output(b"$ \x1b]133;C\x07hello\x1b]133;D;0\x07PROMPT> ");
        let text = super::pane(
            &pane,
            crate::command::Run::now().ok_or("a clock")?,
            None,
            view,
            false,
        )
        .0;
        assert_eq!(text, "hello\n");
        // A running command's row is whole, its cursor at the start of it
        // too (a progress line it rewrites).
        pane.output(b"\r\n$ \x1b]133;C\x07step 1\r");
        let text = super::pane(
            &pane,
            crate::command::Run::now().ok_or("a clock")?,
            None,
            view,
            false,
        )
        .0;
        assert_eq!(text, "step 1\n");
        Ok(())
    }
}
