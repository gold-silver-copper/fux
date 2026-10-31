//! What `capture` reads, of a pane or a client: a screen's text, trimmed,
//! sliced and joined as asked, with its colours and attributes and its
//! pane's state, as data (`Captured`) that the client renders as text or
//! as JSON.
use crate::pane::{Pane, Process, Runs};
use fux_vt::{Attributes, CommandMark, Mode, MouseProtocolMode, Row, Screen};
use fux_wire::answer::{Captured, Line, Of, Run, highlighted};
use fux_wire::command::{CaptureView, Source};
use fux_wire::id::ClientId;
use fux_wire::json::Json;
use fux_wire::wire::Memory;

/// A pane's screen, and `history` rows before it, as `view` asks, read
/// once with its version and state; with `copy_mode`, that says a client
/// is in copy mode on it (`state`).
pub fn pane(
    pane: &Pane,
    run: fux_wire::command::Run,
    history: Option<usize>,
    view: CaptureView,
    copy_mode: bool,
) -> Result<Captured, fux_wire::code::Error> {
    let screen = pane.screen();
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
    let selection = selected_rows(read, view);
    let read = rows_of(screen, history, view)
        .0
        .skip(selection.start)
        .take(selection.len());
    let mut state = state(pane, copy_mode)?;
    // The history rows kept above the first row read: history it was not
    // asked for (rows a range leaves out are not counted).
    state.push(("above", above.map_or(Json::Null, Json::count)));
    let of = Of::Pane {
        version: pane.version(run),
        wrap_pending: cursor.is_some() && screen.pending_wrap(),
        state: state.into_iter().map(|(k, v)| (k.to_owned(), v)).collect(),
    };
    Ok(build(read, of, (screen.size().into(), cursor), view))
}

fn build<'a>(
    read: impl Iterator<Item = (Row<'a>, std::ops::Range<usize>)> + Clone,
    of: Of,
    (size, cursor): ((u16, u16), Option<(u16, u16)>),
    view: CaptureView,
) -> Captured {
    // A row starts a line unless the row before it continues on it.
    let joined = move |row: &Row<'_>| !view.wrapped && row.continues();
    let count = read
        .clone()
        .scan(false, |into, (row, _)| {
            Some(!std::mem::replace(into, joined(&row)))
        })
        .filter(|starts| *starts)
        .count();
    let mut built = Builder::new(Vec::with_capacity(count));
    let (mut open, mut into) = (false, false);
    for (row, span) in read {
        if open && !into {
            built.finish();
        }
        (open, into) = (true, joined(&row));
        for (text, style) in cells_of(row, span, view) {
            built.add(text, style, view);
        }
    }
    if open {
        built.finish();
    }
    for _ in 0..if view.full { built.empty } else { 0 } {
        built.lines.push(Line::default());
    }
    let (lines, styles) = (built.lines, built.table.styles);
    captured(of, size, cursor, lines, styles, view)
}

/// A capture being built: its lines, the styles they name and its empty
/// lines held back (dropped at the end unless `full`); and the line being read, in room reused from line to line:
/// its text, its runs with their styles, and what trimming keeps of it,
/// if any: its bytes, its runs, and its last run as kept.
struct Builder {
    lines: Vec<Line>,
    table: Table,
    empty: usize,
    text: String,
    runs: Vec<(Attributes, Run)>,
    kept: Option<(usize, usize, Run)>,
}

impl Builder {
    fn new(lines: Vec<Line>) -> Self {
        Builder {
            lines,
            table: Table::default(),
            empty: 0,
            text: String::new(),
            runs: Vec::new(),
            kept: None,
        }
    }

    /// Add a cell's text in `style` to the line, in a new run if the style
    /// changes and `view` shows styles or marks. Trimming keeps all but
    /// the trailing blanks that neither marks nor styles show, alike in
    /// SGR and JSON runs. Plain text trims them all; full keeps them all.
    /// Only whole blank cells count: a cluster may contain a space as part
    /// of its text (Unicode Prepend).
    #[inline]
    fn add(&mut self, text: &str, style: Attributes, view: CaptureView) {
        self.text.push_str(text);
        if view.styles || view.marks {
            if self.runs.last().is_none_or(|(last, _)| *last != style) {
                self.runs.push((style, Run::default()));
            }
            if let Some((_, run)) = self.runs.last_mut() {
                let more = u32::try_from(text.len()).unwrap_or(u32::MAX);
                run.bytes = run.bytes.saturating_add(more);
                run.cells = run.cells.saturating_add(1);
            }
        }
        let blank = !view.full
            && text == " "
            && !(view.marks && highlighted(&style) || view.styles && painted(&style));
        if !blank {
            let last = self.runs.last().map_or(Run::default(), |(_, run)| *run);
            self.kept = Some((self.text.len(), self.runs.len(), last));
        }
    }

    /// Copy what trimming keeps of the line into exactly its room, then
    /// empty it for the next.
    fn finish(&mut self) {
        if let Some((bytes, runs, last)) = self.kept.take() {
            for _ in 0..std::mem::take(&mut self.empty) {
                self.lines.push(Line::default());
            }
            let mut kept = Line {
                text: self.text.get(..bytes).unwrap_or_default().to_owned(),
                runs: Vec::with_capacity(runs),
            };
            for (style, run) in self.runs.iter().take(runs) {
                let style = self.table.place(*style);
                kept.runs.push(Run { style, ..*run });
            }
            if let Some(run) = kept.runs.last_mut() {
                (run.bytes, run.cells) = (last.bytes, last.cells);
            }
            self.lines.push(kept);
        } else {
            self.empty = self.empty.saturating_add(1);
        }
        self.text.clear();
        self.runs.clear();
    }
}

/// A row's cells in `span`, cut to the columns `view` takes, each its
/// glyph's text and style: a wide glyph's second cell has no text and its
/// first cell's style, and one cut in two shows a blank in the half taken.
fn cells_of<'a>(
    row: Row<'a>,
    span: std::ops::Range<usize>,
    view: CaptureView,
) -> impl Iterator<Item = (&'a str, Attributes)> + 'a {
    let source_start = span.start;
    let source_end = columns(row, span.clone(), CaptureView { cols: None, ..view }).end;
    let span = columns(row, span, view);
    let (first, count) = (span.start, span.len());
    let mut lead = if first > source_start {
        row.cell(first.saturating_sub(1))
            .map_or(Attributes::default(), |cell| cell.attributes())
    } else {
        Attributes::default()
    };
    // The cell past the last one taken, if it is the second half of a
    // wide glyph the last one begins.
    let split = view.cols.is_some()
        && first.saturating_add(count) < source_end
        && row
            .cell(first.saturating_add(count))
            .is_some_and(|cell| cell.is_wide_continuation());
    row.cells()
        .skip(first)
        .take(count)
        .enumerate()
        .map(move |(col, cell)| {
            let text = if cell.is_wide_continuation() {
                ""
            } else {
                lead = cell.attributes();
                cell.shown()
            };
            let cut = view.cols.is_some()
                && ((col == 0 && text.is_empty()) || (col.saturating_add(1) == count && split));
            (if cut { " " } else { text }, lead)
        })
}

/// A capture's styles, in the order first used, and their places, found
/// by fux-vt's keyed hash so that a program can't choose styles that
/// collide: each slot holds a style's place plus one, or 0 for none. At
/// most half the slots are taken, so a search ends at an empty one.
#[derive(Default)]
struct Table {
    styles: Vec<Attributes>,
    slots: Vec<u32>,
}

impl Table {
    /// The place of `style`, added if new, with more slots if a new
    /// style needs them.
    fn place(&mut self, style: Attributes) -> u32 {
        let mut at = self.slot(style);
        if let Some(place) = self.slots.get(at).and_then(|taken| taken.checked_sub(1)) {
            return place;
        }
        if self.styles.len().saturating_add(1).saturating_mul(2) > self.slots.len() {
            let size = self.slots.len().saturating_mul(2).max(16);
            self.slots = vec![0; size];
            for (place, style) in (1..).zip(&self.styles) {
                let at = self.slot(*style);
                if let Some(slot) = self.slots.get_mut(at) {
                    *slot = place;
                }
            }
            at = self.slot(style);
        }
        self.styles.push(style);
        let place = u32::try_from(self.styles.len()).unwrap_or(u32::MAX);
        if let Some(slot) = self.slots.get_mut(at) {
            *slot = place;
        }
        place.saturating_sub(1)
    }

    /// The slot that holds `style`, or the empty one it would take.
    fn slot(&self, style: Attributes) -> usize {
        use std::hash::BuildHasher;
        let mask = self.slots.len().saturating_sub(1);
        // The hash's low bits, which the mask keeps below the slots' count.
        let hash = fux_vt::Keyed.hash_one(style);
        let mut at = usize::try_from(hash & mask as u64).unwrap_or(mask);
        for _ in 0..self.slots.len() {
            let place = self.slots.get(at).and_then(|taken| taken.checked_sub(1));
            if place.is_none_or(|place| self.styles.get(place as usize) == Some(&style)) {
                break;
            }
            at = at.saturating_add(1) & mask;
        }
        at
    }
}

/// Select physical rows, widening a joined range to the logical lines
/// at its ends without reading the rows after those lines.
fn selected_rows<'a>(
    read: impl Iterator<Item = (Row<'a>, std::ops::Range<usize>)>,
    view: CaptureView,
) -> std::ops::Range<usize> {
    let (first, end) = view
        .rows
        .map_or((0, usize::MAX), |(a, b)| (usize::from(a), usize::from(b)));
    if first >= end {
        return 0..0;
    }
    let skip = if view.wrapped { first } else { 0 };
    let read = read.skip(skip);
    let (mut start, mut count, mut line_start) = (skip, 0usize, 0usize);
    for (index, (row, _)) in read.enumerate() {
        if !view.wrapped && index <= first {
            start = line_start;
        }
        let past = index.saturating_add(skip).saturating_add(1);
        count = past.saturating_sub(start);
        if past >= end && (view.wrapped || !row.continues()) {
            break;
        }
        if !row.continues() {
            line_start = index.saturating_add(1);
        }
    }
    if !view.wrapped && first >= start.saturating_add(count) {
        count = 0;
    }
    start..start.saturating_add(count)
}

fn columns(
    row: Row<'_>,
    span: std::ops::Range<usize>,
    view: CaptureView,
) -> std::ops::Range<usize> {
    let retained = if !view.full && (view.wrapped || !row.continues()) {
        row.stored_len()
    } else {
        row.len()
    };
    let spacer = usize::from(!view.wrapped && row.ends_in_spacer());
    let end = row.len().saturating_sub(spacer).min(retained).min(span.end);
    let (skip, count) = view.cols.map_or((0, usize::MAX), |(a, b)| {
        (
            usize::from(a),
            usize::from(b).saturating_sub(usize::from(a)),
        )
    });
    let first = span.start.saturating_add(skip).min(end);
    first..first.saturating_add(count).min(end)
}

/// The rows a capture reads, each with the columns of it it reads: the
/// screen and `history` rows above it (of the main screen with
/// `main-screen`), and how many rows are kept above them; or the last
/// command's output.
fn rows_of(
    screen: &Screen,
    history: Option<usize>,
    view: CaptureView,
) -> (
    impl Iterator<Item = (Row<'_>, std::ops::Range<usize>)> + Clone,
    Option<usize>,
) {
    let rows = match view.source {
        Source::Shown | Source::LastOutput => screen.rows(),
        Source::Main => screen.main_rows(),
        Source::Alternate => screen.alternate_rows(),
    };
    let (range, columns, ended, above) = if view.source == Source::LastOutput {
        let (range, columns, ended) = last_output(screen).unwrap_or((0..0, 0..0, true));
        (range, columns, ended, None)
    } else {
        let keep = usize::from(screen.size().rows()).saturating_add(history.unwrap_or(0));
        let skip = rows.len().saturating_sub(keep);
        (skip..rows.len(), 0..usize::MAX, false, Some(skip))
    };
    let rows = rows.skip(range.start).take(range.len());
    let count = rows.len();
    let read = rows.enumerate().filter_map(move |(i, row)| {
        let from = if i == 0 { columns.start } else { 0 };
        let to = if i.saturating_add(1) == count && ended {
            if i == 0 && from >= columns.end {
                0
            } else {
                columns.end
            }
        } else {
            usize::MAX
        };
        (from < to).then_some((row, from..to))
    });
    (read, above)
}

/// The physical rows and columns between a command's marks, or its cursor
/// while running. A start lost to history eviction begins at what is kept.
fn last_output(screen: &Screen) -> Option<(std::ops::Range<usize>, std::ops::Range<usize>, bool)> {
    let commands = screen.commands();
    let (row, col) = screen.cursor_position();
    let cursor = screen
        .window()
        .row(row)
        .map(|r| CommandMark { row: r.id(), col });
    let running = commands.output_start.zip(cursor);
    let ended = running.is_none();
    let (start, end) = running.or(commands.last_output)?;
    let last = screen.row_index(end.row)?;
    let left = CommandMark { col: 0, ..start };
    let (first, start) = match screen.row_index(start.row) {
        Some(first) if first == last && ended && start.col > end.col => (first, left),
        Some(first) if first <= last => (first, start),
        Some(_) => (screen.history_len().min(last), left),
        None => (0, left),
    };
    Some((
        first..last.saturating_add(1),
        usize::from(start.col)..usize::from(end.col),
        ended,
    ))
}

/// What a client's terminal shows, row by row.
pub fn client(client: ClientId, grid: &crate::render::Grid) -> Captured {
    let of = Of::Client(client);
    let mut lines = Vec::with_capacity(usize::from(grid.rows));
    for y in 0..grid.rows {
        let text: String = grid
            .row(y)
            .filter(|cell| !cell.is_wide_continuation())
            .map(|cell| cell.shown())
            .collect();
        lines.push(Line {
            text: text.trim_end().to_owned(),
            runs: Vec::new(),
        });
    }
    let view = CaptureView {
        full: true,
        ..CaptureView::default()
    };
    let size = (grid.rows, grid.cols);
    captured(of, size, grid.cursor, lines, Vec::new(), view)
}

/// What is known of a pane's program, for `json`: its foreground program,
/// the status it exited with, and the modes
/// that say how it takes input; whether a client is in copy mode on it,
/// and the cell its program is told, by its tab's host
/// (`Session::host_panes`).
pub(crate) fn state(
    pane: &Pane,
    copy_mode: bool,
) -> Result<Vec<(&'static str, Json)>, fux_wire::code::Error> {
    let screen = pane.screen();
    let cell = screen.graphics().cell();
    // The terminal reports a group, whose leader may already have exited.
    // Read that leader alone; a surviving member is not guessed to replace it.
    let group = pane.process().child().and_then(|child| {
        fuxix::terminal::foreground_group(&child.master)
            .ok()
            .flatten()
    });
    let leader = group.filter(|pid| fuxix::process::exists(*pid));
    let program = leader
        .map(|pid| fuxix::process::name(pid, &mut Memory))
        .transpose()?
        .flatten();
    let observed = leader
        .map(|pid| fuxix::process::argv(pid, &mut Memory))
        .transpose()?
        .flatten();
    let (launch_argv, argv) = arguments(&pane.runs, observed);
    let cwd = match leader
        .map(|pid| fuxix::process::cwd_observed(pid, &mut Memory).map(|r| r.ok().flatten()))
        .transpose()?
        .flatten()
    {
        Some(path) => Json::str(path.as_os_str().to_string_lossy()),
        None => Json::Null,
    };
    let last_status = screen.commands().last_status;

    let (exited, signal) = match pane.process().exit_status() {
        Some(fux_wire::answer::Ended::Exited(code)) => (Json::int(code), Json::Null),
        Some(fux_wire::answer::Ended::Signalled(signal)) => (Json::Null, Json::int(signal)),
        Some(fux_wire::answer::Ended::Lost) | None => (Json::Null, Json::Null),
    };
    let hung_up = matches!(pane.process(), Process::HungUp(_));
    let mouse = screen.mouse_protocol_mode();
    let bracketed_paste = screen.mode(Mode::BracketedPaste);
    let pictures = Json::Array(screen.pictures().map(picture).collect());
    let notification = pane.alerts.last.as_ref();
    // The fields whose value the program sent more of than is kept.
    let cut = [
        ("title", pane.title().cut()),
        ("notification", notification.is_some_and(|n| n.cut())),
        ("output", pane.left.output == crate::pane::Output::Cut),
    ];
    let truncated = cut.into_iter().filter(|(_, cut)| *cut);
    let cut_fields = truncated.map(|(field, _)| Json::str(field)).collect();
    let losses = pane.lost();
    let lost = Json::object(vec![
        ("copies_cut", Json::uint(losses.copies_cut)),
        ("copies_not_text", Json::uint(losses.copies_not_text)),
        ("pictures", Json::uint(losses.pictures)),
        ("memory", Json::uint(losses.memory)),
    ]);
    let cell = match cell {
        Some(cell) => Json::object(vec![
            ("width", Json::int(cell.width())),
            ("height", Json::int(cell.height())),
        ]),
        None => Json::Null,
    };
    let read_error = pane
        .read_error()
        .map(|e| e.message(&mut Memory))
        .transpose()?;
    let values = [
        ("title", Json::str(pane.title().as_str())),
        ("program", program.map_or(Json::Null, Json::str)),
        (
            "foreground_group",
            group.map_or(Json::Null, |pid| Json::int(pid.as_raw())),
        ),
        (
            "foreground_leader",
            leader.map_or(Json::Null, |pid| Json::int(pid.as_raw())),
        ),
        ("launch_argv", launch_argv),
        // Its program closed its terminal and runs on: nothing reads keys
        // there (the terminal hung up; Linux says so).
        ("hung_up", Json::Bool(hung_up)),
        ("read_error", read_error.map_or(Json::Null, Json::str)),
        ("exited", exited),
        ("signal", signal),
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
        ("lost", lost),
        ("cell", cell),
        ("cursor_visible", Json::Bool(screen.mode(Mode::ShowCursor))),
        ("history_bytes", Json::count(pane.history_bytes())),
        (
            "program_clipboard",
            Json::Bool(pane.options().program_clipboard),
        ),
        ("reflow", Json::Bool(pane.options().reflow)),
        (
            "alternate_scroll_lines",
            Json::count(usize::from(pane.options().alternate_scroll_lines.get())),
        ),
        (
            "picture_bytes",
            Json::count(pane.options().picture_bytes.get()),
        ),
        ("history_lines", Json::count(screen.history_len())),
        ("copy_mode", Json::Bool(copy_mode)),
        ("command_running", Json::Bool(pane.command_running())),
        ("last_status", last_status.map_or(Json::Null, Json::int)),
        ("bells", Json::uint(pane.alerts.rung)),
        (
            "notification",
            notification.map_or(Json::Null, |n| Json::str(n.as_str())),
        ),
        ("truncated", Json::Array(cut_fields)),
        ("input_unread", Json::count(pane.left.input)),
        ("cwd", cwd),
        ("argv", argv),
    ];
    let mut fields = Vec::with_capacity(values.len().saturating_add(1));
    fields.extend(values);
    Ok(fields)
}

/// The words a pane's program was launched with, and those its foreground
/// program has now, if observed.
fn arguments(runs: &Runs, observed: Option<Vec<std::ffi::OsString>>) -> (Json, Json) {
    let argv = match observed {
        Some(words) => Json::Array(
            (words.iter())
                .map(|word| Json::str(word.to_string_lossy()))
                .collect(),
        ),
        None => Json::Null,
    };
    let launch = Json::Array(runs.launch().argv.iter().map(Json::str).collect());
    (launch, argv)
}

/// Where a picture is, for `json`: its top row (negative once scrolled
/// above the screen), first column, the cells it covers, its first rows
/// that are gone (`cropped`: scrolled off a region's top or a screen's
/// that keeps no history, the rest shown below them), and its size in
/// pixels.
fn picture(shown: fux_vt::Shown<'_>) -> Json {
    Json::object(vec![
        ("row", Json::Int(shown.row)),
        ("col", Json::int(shown.col)),
        ("rows", Json::int(shown.rows)),
        ("cropped", Json::int(shown.cropped)),
        ("cols", Json::int(shown.cols)),
        ("width", Json::int(shown.picture.width())),
        ("height", Json::int(shown.picture.height())),
    ])
}

/// The capture of `lines`, with the styles their runs name.
fn captured(
    of: Of,
    (screen_rows, screen_cols): (u16, u16),
    screen_cursor: Option<(u16, u16)>,
    lines: Vec<Line>,
    styles: Vec<Attributes>,
    view: CaptureView,
) -> Captured {
    Captured {
        of,
        screen_rows,
        screen_cols,
        screen_cursor,
        lines,
        styles,
        styled: view.styles,
        marks: view.marks,
    }
}

/// A blank's style shows on the screen: a background colour, reverse
/// video, a line under or through it. One bold or coloured in front shows
/// none.
fn painted(a: &Attributes) -> bool {
    highlighted(a) || a.underline() || a.strikeout()
}

#[cfg(test)]
mod tests {
    use crate::pane::tests::pane;
    use fux_wire::command::CaptureView;

    /// A capture as printed.
    trait Printed {
        fn text(&self) -> Result<String, std::fmt::Error>;
    }
    impl Printed for fux_wire::answer::Captured {
        fn text(&self) -> Result<String, std::fmt::Error> {
            let mut text = String::new();
            self.write_text(&mut text)?;
            Ok(text)
        }
    }

    /// A program's observed words read as text, a byte no UTF-8 holds
    /// replaced, beside the words it was launched with.
    #[test]
    fn observed_arguments_read_as_text() -> Result<(), Box<dyn std::error::Error>> {
        let raw = b"a\0\xff\0\0";
        let shell = crate::pane::tests::shell()?;
        let observed = fuxix::process::argv_read(raw.as_slice(), &mut fux_wire::wire::Memory)?;
        let (saved, observed) = super::arguments(&shell, observed);
        assert_eq!(observed.render(), "[\"a\",\"\u{fffd}\",\"\"]");
        assert_eq!(saved.render(), "[\"/bin/sh\"]");
        Ok(())
    }

    #[test]
    fn wide_default_history_is_admitted() -> Result<(), Box<dyn std::error::Error>> {
        let history = fux::script::default_state()
            .map_err(|e| e.to_string())?
            .preferences
            .history_bytes
            .get();
        let mut pane = pane(3, 200, history)?;
        let output: Vec<u8> = std::iter::repeat_n(b"x\r\n".as_slice(), 150_000)
            .flatten()
            .copied()
            .collect();
        pane.output(&output);
        while pane.has_unread() {
            pane.read_unread();
        }
        assert!(pane.screen().history_len().saturating_mul(200) > 16 << 20);
        let capture = super::pane(
            &pane,
            fux_wire::command::Run::now().ok_or("clock")?,
            Some(usize::MAX),
            CaptureView::default(),
            false,
        )?;
        assert!(capture.lines.len() > 80_000);
        let full = super::pane(
            &pane,
            fux_wire::command::Run::now().ok_or("clock")?,
            Some(100_000),
            CaptureView {
                full: true,
                ..CaptureView::default()
            },
            false,
        )?;
        assert_eq!(full.lines.len(), 100_003);
        Ok(())
    }

    #[test]
    fn joined_prefix_costs_metadata_only() -> Result<(), Box<dyn std::error::Error>> {
        let mut pane = pane(1, 200, 0)?;
        pane.output(b"x");
        let row = pane.screen().rows().next().ok_or("row")?;
        let read = Box::new(std::iter::repeat_n((row, 0..usize::MAX), 100));
        let selection = super::selected_rows(
            read,
            CaptureView {
                rows: Some((90, 96)),
                ..CaptureView::default()
            },
        );
        assert_eq!((selection.start, selection.len()), (90, 6));
        Ok(())
    }

    /// A soft-wrapped row's last cell is the spacer a wide glyph left, no
    /// part of the line, only while nothing was painted there: a blank EL
    /// erased in a colour is a cell the program painted, and shows, so a
    /// joined capture keeps it, as a copy does (`Row::copied`).
    #[test]
    fn a_painted_blank_before_a_wide_glyph_wrapped_on_is_part_of_the_line()
    -> Result<(), Box<dyn std::error::Error>> {
        let join = CaptureView::default();
        let captured = |output: &str| -> Result<String, Box<dyn std::error::Error>> {
            let mut pane = pane(3, 5, 0)?;
            pane.output(output.as_bytes());
            Ok(super::pane(
                &pane,
                fux_wire::command::Run::now().ok_or("a clock")?,
                None,
                join,
                false,
            )?
            .text()?)
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
        let mut pane = pane(6, 5, 0)?;
        pane.output(b"top\r\nabcdefghijklm\r\nend");
        let run = fux_wire::command::Run::now().ok_or("a clock")?;
        let capture = |rows, wrapped| -> Result<String, Box<dyn std::error::Error>> {
            let view = CaptureView {
                rows: Some(rows),
                wrapped,
                ..CaptureView::default()
            };
            Ok(super::pane(&pane, run, None, view, false)?.text()?)
        };
        assert_eq!(capture((2, 3), false)?, "abcdefghijklm");
        assert_eq!(capture((0, 2), false)?, "top\nabcdefghijklm");
        assert_eq!(capture((3, 5), false)?, "abcdefghijklm\nend");
        assert_eq!(capture((2, 3), true)?, "fghij");
        for wrapped in [false, true] {
            for at in [0, 2, 4, 100] {
                assert_eq!(capture((at, at), wrapped)?, "");
            }
        }
        assert_eq!(capture((0, 1), false)?, "top");
        assert_eq!(capture((1, 4), true)?, "abcde\nfghij\nklm");
        Ok(())
    }

    /// Columns cut from a joined line leave the spacer out only when they
    /// take its column: cut short of it, the last cell taken is text.
    #[test]
    fn columns_cut_short_of_the_spacer_keep_their_last_cell()
    -> Result<(), Box<dyn std::error::Error>> {
        let view = CaptureView {
            cols: Some((0, 3)),
            ..CaptureView::default()
        };
        let mut pane = pane(3, 5, 0)?;
        // 世 at col 4 does not fit and wraps on, leaving the spacer there.
        pane.output("abcd\x1b[5G世".as_bytes());
        let text = super::pane(
            &pane,
            fux_wire::command::Run::now().ok_or("a clock")?,
            None,
            view,
            false,
        )?
        .text()?;
        assert_eq!(text.lines().next(), Some("abc世"));
        Ok(())
    }

    /// Mixed views preserve wide-cell counts and style runs across wraps,
    /// and trim only blanks whose style the requested view cannot show.
    #[test]
    fn mixed_views_keep_cells_runs_and_visible_blanks() -> Result<(), Box<dyn std::error::Error>> {
        let mut pane = pane(5, 5, 0)?;
        pane.output(
            "\x1b[31mabcde世\x1b[m\r\nx\x1b[4m  \x1b[m\r\ny\x1b[41m  \x1b[31;49m  \x1b[m"
                .as_bytes(),
        );
        let run = fux_wire::command::Run::now().ok_or("a clock")?;
        for flags in 0..16 {
            let view = CaptureView {
                styles: flags & 1 != 0,
                marks: flags & 2 != 0,
                full: flags & 4 != 0,
                wrapped: flags & 8 != 0,
                ..CaptureView::default()
            };
            let got = super::pane(&pane, run, None, view, false)?;
            let styled = view.styles || view.marks;
            let actual: Vec<(String, u32)> = got
                .lines
                .iter()
                .map(|line| (line.text.clone(), line.runs.iter().map(|r| r.cells).sum()))
                .collect();
            let mut want = if view.wrapped {
                vec![
                    ("abcde", 5),
                    if view.full { ("世   ", 5) } else { ("世", 2) },
                ]
            } else {
                vec![if view.full {
                    ("abcde世   ", 10)
                } else {
                    ("abcde世", 7)
                }]
            };
            want.push(match (view.full, view.styles) {
                (true, _) => ("x    ", 5),
                (false, true) => ("x  ", 3),
                (false, false) => ("x", 1),
            });
            want.push(match (view.full, view.styles || view.marks) {
                (true, _) => ("y    ", 5),
                (false, true) => ("y  ", 3),
                (false, false) => ("y", 1),
            });
            if view.full {
                want.push(("     ", 5));
            }
            // A plain capture's lines have no runs to count cells in.
            let want: Vec<_> = want
                .into_iter()
                .map(|(text, cells)| (text.to_owned(), if styled { cells } else { 0 }))
                .collect();
            assert_eq!(actual, want, "{view:?}");
            for line in &got.lines {
                let styles: Vec<_> = got.runs(line).collect::<Option<_>>().ok_or("run")?;
                assert!(
                    styles
                        .iter()
                        .zip(styles.iter().skip(1))
                        .all(|(a, b)| a.2 != b.2)
                );
                assert_eq!(styled, !line.runs.is_empty());
            }
        }
        Ok(())
    }

    /// A space joined to a prepend character is glyph text, not a blank
    /// cell that trimming may remove.
    #[test]
    fn a_space_inside_a_cluster_is_not_a_trailing_blank() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut pane = pane(2, 10, 0)?;
        pane.output("x\u{0600}  ".as_bytes());
        let marks = CaptureView {
            marks: true,
            ..CaptureView::default()
        };
        let run = fux_wire::command::Run::now().ok_or("a clock")?;
        let got = super::pane(&pane, run, None, marks, false)?;
        assert_eq!(got.text()?, "x\u{0600} ");
        assert_eq!(
            got.lines
                .first()
                .and_then(|line| line.runs.first())
                .map(|run| run.cells),
            Some(3)
        );
        Ok(())
    }

    /// A capture lists each style once, numbered in the order first used,
    /// past the index's growth; a style only trimmed blanks had is not
    /// listed.
    #[test]
    fn styles_are_listed_once_in_first_use_order() -> Result<(), Box<dyn std::error::Error>> {
        let mut pane = pane(2, 50, 0)?;
        let mut output = String::new();
        for colour in (0..20).chain(0..20) {
            output.push_str(&format!("\x1b[38;5;{colour}mx"));
        }
        output.push_str("\x1b[38;5;20m \x1b[m");
        pane.output(output.as_bytes());
        let marks = CaptureView {
            marks: true,
            ..CaptureView::default()
        };
        let run = fux_wire::command::Run::now().ok_or("a clock")?;
        let got = super::pane(&pane, run, None, marks, false)?;
        let colour = |n| fux_vt::Attributes::new(fux_vt::Color::Idx(n), fux_vt::Color::Default);
        assert_eq!(got.styles, (0..20).map(colour).collect::<Vec<_>>());
        let line = got.lines.first().ok_or("a line")?;
        let places: Vec<u32> = line.runs.iter().map(|run| run.style).collect();
        assert_eq!(places, (0..20).chain(0..20).collect::<Vec<_>>());
        assert_eq!(line.text.len(), 40);
        Ok(())
    }

    /// Selecting part of a joined line reads through that line's end,
    /// without walking the unrelated rows that follow it.
    #[test]
    fn a_joined_range_stops_at_its_last_logical_line() -> Result<(), Box<dyn std::error::Error>> {
        let mut pane = pane(6, 5, 0)?;
        pane.output(b"abcdefghijklm\r\nlater");
        let visits = std::cell::Cell::new(0usize);
        let read = Box::new(pane.screen().rows().map(|row| {
            visits.set(visits.get().saturating_add(1));
            (row, 0..usize::MAX)
        }));
        let selection = super::selected_rows(
            read,
            CaptureView {
                rows: Some((1, 2)),
                ..CaptureView::default()
            },
        );
        assert_eq!(selection.len(), 3);
        assert_eq!(visits.get(), 3);
        Ok(())
    }

    /// A capture goes on the wire and reads back the same, its styles
    /// too; JSON nested past its bound is refused rather than read.
    #[test]
    fn a_capture_reads_back_as_written() -> Result<(), Box<dyn std::error::Error>> {
        use fux_wire::json::Json;
        use fux_wire::wire::{Reader, encode};
        let mut pane = pane(3, 20, 0)?;
        pane.output(b"\x1b[1;4:3;5;38;2;1;2;3;48;5;9;58;5;2mstyled\x1b[m plain");
        let view = CaptureView {
            styles: true,
            ..CaptureView::default()
        };
        let run = fux_wire::command::Run::now().ok_or("a clock")?;
        let body = fux_wire::protocol::Body::Capture(Box::new(super::pane(
            &pane, run, None, view, false,
        )?));
        assert_eq!(
            Reader::new(&encode(&body)).whole::<fux_wire::protocol::Body>()?,
            body
        );
        let deep = (0..40).fold(Json::Null, |inner, _| Json::Array(vec![inner]));
        assert!(Reader::new(&encode(&deep)).whole::<Json>().is_err());
        Ok(())
    }

    /// An ended command's output is what lies between its marks: a prompt
    /// drawn after its end mark on the same row is not part of it. A
    /// running one's is all of its rows so far.
    #[test]
    fn last_output_ends_at_the_end_mark() -> Result<(), Box<dyn std::error::Error>> {
        let view = CaptureView {
            source: fux_wire::command::Source::LastOutput,
            ..CaptureView::default()
        };
        let mut pane = pane(3, 20, 1 << 20)?;
        for _ in 0..10 {
            pane.output(b"old\r\n");
        }
        assert!(pane.screen().history_len() > 0);
        pane.output(b"$ \x1b]133;C\x07hello\x1b]133;D;0\x07PROMPT> ");
        let text = super::pane(
            &pane,
            fux_wire::command::Run::now().ok_or("a clock")?,
            None,
            view,
            false,
        )?
        .text()?;
        assert_eq!(text, "hello");
        // A running command's row is whole, its cursor at the start of it
        // too (a progress line it rewrites).
        pane.output(b"\r\n$ \x1b]133;C\x07step 1\r");
        let text = super::pane(
            &pane,
            fux_wire::command::Run::now().ok_or("a clock")?,
            None,
            view,
            false,
        )?
        .text()?;
        assert_eq!(text, "step 1");
        Ok(())
    }
}
