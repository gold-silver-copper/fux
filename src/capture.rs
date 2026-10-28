//! What `capture` and `capture-client` read: a screen's text, trimmed,
//! sliced and joined as asked, with its colours and attributes and its
//! pane's state, as data (`Captured`) that the client renders as text or
//! as JSON.
use crate::command::{CaptureView, Run, Source, Version};
use crate::id::ClientId;
use crate::json::Json;
use crate::pane::{Pane, Process, Runs};
use crate::wire::{wire, wire_enum};
use fux_vt::{
    Attributes, Blink, Color, CommandMark, Mode, MouseProtocolMode, Rgb, Row, Screen,
    UnderlineStyle,
};
use std::time::Instant;

/// Synthesized output work: at most two cells per admitted wire byte.
pub(crate) const OUTPUT_CELLS: usize = 2 * crate::wire::MAX_REPLY;

/// Borrowed rows and their selected columns, with no boxed iterator state.
struct ReadRows<'a> {
    rows: std::iter::Take<std::iter::Skip<fux_vt::Rows<'a>>>,
    at: usize,
    count: usize,
    from: usize,
    to: usize,
    ended: bool,
}
impl<'a> ReadRows<'a> {
    fn new(
        rows: fux_vt::Rows<'a>,
        skip: usize,
        count: usize,
        from: usize,
        to: usize,
        ended: bool,
    ) -> Self {
        let rows = rows.skip(skip).take(count);
        let count = rows.len();
        Self {
            rows,
            at: 0,
            count,
            from,
            to,
            ended,
        }
    }
}
impl<'a> Iterator for ReadRows<'a> {
    type Item = (Row<'a>, std::ops::Range<usize>);
    fn next(&mut self) -> Option<Self::Item> {
        for row in self.rows.by_ref() {
            let i = self.at;
            self.at = self.at.saturating_add(1);
            let from = if i == 0 { self.from } else { 0 };
            let to = if self.at == self.count && self.ended {
                if i == 0 && self.from >= self.to {
                    0
                } else {
                    self.to
                }
            } else {
                usize::MAX
            };
            if from < to {
                return Some((row, from..to));
            }
        }
        None
    }
}

/// A run while it is built and trimmed: its count stays exact until the
/// surviving cells are converted to the wire's `Cut`.
struct LineRun {
    text: String,
    cells: usize,
    style: Attributes,
}

wire! {
    /// A capture, read once: what it read, its screen's size and cursor
    /// (its row and column, if it shows on the screen read), and its
    /// lines, trimmed as its view asked, each a run of cells for each
    /// change of style; and whether its text shows styles (SGR) and marks.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Captured {
        pub of: Of,
        pub screen_rows: u16,
        pub screen_cols: u16,
        pub screen_cursor: Option<(u16, u16)>,
        pub lines: Vec<Vec<Cut>>,
        pub styles: bool,
        pub marks: bool,
    }
}

wire! {
    /// What a capture read: a pane, at its version, read with it, its
    /// pending wrap and its state (`state`); or a client's terminal.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum Of {
        Pane {
            version: Version,
            /// A glyph went into the last column: the next one goes on the
            /// next line, though the cursor shows on this one's last cell.
            wrap_pending: bool,
            state: Vec<(String, Json)>,
        },
        Client(client: ClientId),
    }
}

wire! {
    /// A run of a line's cells of one style: their glyphs, and how many
    /// cells they fill.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Cut {
        pub text: String,
        pub cells: u32,
        pub style: Attributes,
    }
}

wire_enum!(Color { Default, Idx(n), Rgb(rgb) });
wire_enum!(Blink { None, Slow, Rapid });

/// The style's colours, its flags and its underline's style and blink, as
/// the vocabulary of `Attributes` gives and takes them.
impl crate::wire::Wire for Attributes {
    fn put(&self, out: &mut impl crate::wire::Sink) {
        let a = *self;
        [a.foreground(), a.background(), a.underline_color()]
            .iter()
            .for_each(|c| c.put(out));
        [
            a.bold(),
            a.dim(),
            a.italic(),
            a.inverse(),
            a.hidden(),
            a.strikeout(),
        ]
        .iter()
        .for_each(|f| f.put(out));
        a.underline_style().number().put(out);
        a.blink().put(out);
    }
    fn get(r: &mut crate::wire::Reader<'_>) -> Result<Self, crate::wire::Error> {
        let [fg, bg, under] = [Color::get(r)?, Color::get(r)?, Color::get(r)?];
        let mut flag = || bool::get(r);
        let a = Attributes::new(fg, bg)
            .with_underline_color(under)
            .with_bold(flag()?)
            .with_dim(flag()?)
            .with_italic(flag()?)
            .with_inverse(flag()?)
            .with_hidden(flag()?)
            .with_strikeout(flag()?);
        let style = UnderlineStyle::from_number(u16::get(r)?);
        let style = style.ok_or(crate::wire::Error::Bad("underline style"))?;
        Ok(a.with_underline_style(style).with_blink(Blink::get(r)?))
    }
}

/// A pane's screen, and `history` rows before it, as `view` asks, read
/// once with its version and state; with `copy_mode`, that says a client
/// is in copy mode on it (`state`).
pub fn pane(
    pane: &Pane,
    run: Run,
    history: Option<usize>,
    view: CaptureView,
    copy_mode: bool,
) -> Result<Captured, crate::session::Error> {
    let limits = Limits::new(pane.history_bytes(), pane.screen().size().into())?;
    pane_with(pane, run, history, view, copy_mode, limits)
}

fn pane_with(
    pane: &Pane,
    run: Run,
    history: Option<usize>,
    view: CaptureView,
    copy_mode: bool,
    limits: Limits,
) -> Result<Captured, crate::session::Error> {
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
    let selection = selected_rows(read, view, limits)?;
    let read = selection.read(rows_of(screen, history, view).0);
    let mut budget = crate::wire::Budget::new();
    let mut state = state(pane, Instant::now(), copy_mode, &mut budget)?;
    // The history rows kept above the first row read: history it was not
    // asked for (rows a range leaves out are not counted).
    state.push(("above", above.map_or(Json::Null, Json::count)));
    let of = Of::Pane {
        version: pane.version(run),
        wrap_pending: cursor.is_some() && screen.pending_wrap(),
        state: budget.fields(state)?,
    };
    build(read, of, (screen.size().into(), cursor), view, &mut budget)
}

fn build<'a>(
    read: impl Iterator<Item = (Row<'a>, std::ops::Range<usize>)>,
    of: Of,
    (size, cursor): ((u16, u16), Option<(u16, u16)>),
    view: CaptureView,
    budget: &mut crate::wire::Budget,
) -> Result<Captured, crate::session::Error> {
    let mut lines: Vec<Vec<LineRun>> = Vec::new();
    let mut wraps = false;
    let mut blanks = 0usize;
    use crate::wire::{Sink, Wire};
    let mut wire = crate::wire::Writer::counting(crate::wire::MAX_REPLY.saturating_sub(64));
    of.put(&mut wire);
    wire.check_ref().map_err(|_| too_large())?;
    for (row, span) in read {
        let source_start = span.start;
        let source_end = columns(row, span.clone(), CaptureView { cols: None, ..view }).end;
        let span = columns(row, span, view);
        let count = span.len();
        let mut lead = if span.start > source_start {
            row.cell(span.start.saturating_sub(1))
                .map_or(Attributes::default(), |cell| cell.attributes())
        } else {
            Attributes::default()
        };
        let mut cells = row
            .cells()
            .skip(span.start)
            .take(source_end.saturating_sub(span.start))
            .map(move |cell| {
                if cell.is_wide_continuation() {
                    ("", lead)
                } else {
                    lead = cell.attributes();
                    (cell.shown(), lead)
                }
            })
            .peekable();
        if !wraps || lines.is_empty() {
            trim_line(&mut lines, blanks, budget)?;
            blanks = 0;
            0u32.put(&mut wire);
            wire.check_ref().map_err(|_| too_large())?;
            budget.reserve(&mut lines, 1).map_err(|_| too_large())?;
            lines.push(Vec::new());
        }
        if let Some(line) = lines.last_mut() {
            for col in 0..count {
                let Some((mut text, style)) = cells.next() else {
                    break;
                };
                // A wide glyph cut in two shows a blank in the half taken.
                if view.cols.is_some()
                    && ((col == 0 && text.is_empty())
                        || (col.saturating_add(1) == count
                            && cells.peek().is_some_and(|(t, _)| t.is_empty())))
                {
                    text = " ";
                }
                // Trim trailing blanks unless highlighted with marks or
                // painted with styles, alike in SGR and JSON runs. Plain
                // text trims them all; full keeps them all. Only whole
                // blank cells count: a cluster may contain a space as
                // part of its text (Unicode Prepend).
                blanks = if !view.full
                    && text == " "
                    && !(view.marks && highlighted(&style) || view.styles && painted(&style))
                {
                    blanks.saturating_add(1)
                } else {
                    0
                };
                let style = if view.styles || view.marks {
                    style
                } else {
                    Attributes::default()
                };
                if !line.last().is_some_and(|last| last.style == style) {
                    0u32.put(&mut wire);
                    0u32.put(&mut wire);
                    style.put(&mut wire);
                    wire.check_ref().map_err(|_| too_large())?;
                    budget.reserve(line, 1).map_err(|_| too_large())?;
                    line.push(LineRun {
                        text: String::new(),
                        cells: 0,
                        style,
                    });
                }
                if let Some(last) = line.last_mut() {
                    wire.extend_from_slice(text.as_bytes());
                    wire.check_ref().map_err(|_| too_large())?;
                    budget.text(&mut last.text, text).map_err(|_| too_large())?;
                    last.cells = last.cells.saturating_add(1);
                }
            }
        }
        wraps = !view.wrapped && row.continues();
    }
    trim_line(&mut lines, blanks, budget)?;
    captured(of, size, cursor, lines, view, budget)
}

/// Select physical rows, widening a joined range to the logical lines
/// at its ends without reading the rows after those lines.
struct Selection {
    start: usize,
    count: usize,
}
impl Selection {
    fn read<'a>(
        &self,
        read: impl Iterator<Item = (Row<'a>, std::ops::Range<usize>)>,
    ) -> impl Iterator<Item = (Row<'a>, std::ops::Range<usize>)> {
        read.skip(self.start).take(self.count)
    }
}

fn selected_rows<'a>(
    read: impl Iterator<Item = (Row<'a>, std::ops::Range<usize>)>,
    view: CaptureView,
    limits: Limits,
) -> Result<Selection, crate::session::Error> {
    let (first, last) = view
        .rows
        .map_or((0, usize::MAX), |(a, b)| (usize::from(a), usize::from(b)));
    if view.wrapped && first > last {
        return Ok(Selection { start: 0, count: 0 });
    }
    let skip = if view.wrapped { first } else { 0 };
    let read = read.skip(skip);
    let mut cells = 0usize;
    let mut line_cells = 0usize;
    let (mut start, mut count, mut line_start) = (skip, 0usize, 0usize);
    for (index, (row, span)) in read.enumerate() {
        // The VT bounds retained rows; this scan allocates no row vector.
        let work = columns(row, span, view).len();
        if view.wrapped {
            cells = cells.checked_add(work).ok_or_else(too_large)?;
        } else {
            line_cells = line_cells.checked_add(work).ok_or_else(too_large)?;
            if index >= first {
                cells = line_cells;
            }
        }
        if cells > limits.cells {
            return Err(too_large());
        }
        if !view.wrapped && index <= first {
            start = line_start;
        }
        let end = index.checked_add(skip).ok_or_else(too_large)?;
        count = end.saturating_add(1).saturating_sub(start);
        if end >= last && (view.wrapped || !row.continues()) {
            break;
        }
        if !row.continues() {
            line_start = index.saturating_add(1);
            if index < first {
                line_cells = 0;
            }
        }
    }
    if !view.wrapped && first >= start.saturating_add(count) {
        count = 0;
    }
    Ok(Selection { start, count })
}

/// Stored history work follows its byte budget; live cells follow the
/// screen. Synthesized blanks explicitly kept by full or joined views
/// have at most two cells per output byte. All terms are admitted here.
#[derive(Clone, Copy)]
struct Limits {
    cells: usize,
}
impl Limits {
    fn new(history_bytes: usize, (rows, cols): (u16, u16)) -> Result<Self, crate::session::Error> {
        let live = usize::from(rows)
            .checked_mul(usize::from(cols))
            .ok_or_else(too_large)?;
        let cells = history_bytes
            .checked_mul(2)
            .and_then(|n| n.checked_add(live))
            .and_then(|n| n.checked_add(OUTPUT_CELLS))
            .ok_or_else(too_large)?;
        Ok(Self { cells })
    }
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
            usize::from(b)
                .saturating_sub(usize::from(a))
                .saturating_add(1),
        )
    });
    let first = span.start.saturating_add(skip).min(end);
    first..first.saturating_add(count).min(end)
}

fn too_large() -> crate::session::Error {
    crate::session::fail(
        crate::code::Raise::TooLong,
        "capture exceeds its cell, byte or allocation limit; request fewer history rows or a narrower wrapped range",
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
) -> (ReadRows<'_>, Option<usize>) {
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
    (
        ReadRows::new(rows, skip, usize::MAX, 0, usize::MAX, false),
        Some(skip),
    )
}

/// The last command's output between its marks, a range of each row's
/// columns: while one runs, its output so far, to the cursor.
fn last_output(screen: &Screen) -> ReadRows<'_> {
    let commands = screen.commands();
    let (row, col) = screen.cursor_position();
    let cursor = (screen.window().row(row)).map(|r| CommandMark { row: r.id(), col });
    let running = commands.output_start.zip(cursor);
    let ended = running.is_none();
    let Some((start, end)) = running.or(commands.last_output) else {
        return ReadRows::new(screen.rows(), 0, 0, 0, 0, true);
    };
    let Some(last) = screen.row_index(end.row) else {
        return ReadRows::new(screen.rows(), 0, 0, 0, 0, true);
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
    ReadRows::new(
        screen.rows(),
        first,
        last.saturating_sub(first).saturating_add(1),
        usize::from(start.col),
        usize::from(end.col),
        ended,
    )
}

/// What a client's terminal shows, row by row.
pub fn client(
    client: ClientId,
    grid: &crate::render::Grid,
) -> Result<Captured, crate::session::Error> {
    use crate::wire::{Sink, Wire};
    let mut budget = crate::wire::Budget::new();
    let mut wire = crate::wire::Writer::counting(crate::wire::MAX_REPLY.saturating_sub(64));
    let of = Of::Client(client);
    of.put(&mut wire);
    let mut lines = budget.vector(usize::from(grid.rows))?;
    for y in 0..grid.rows {
        0u32.put(&mut wire);
        0u32.put(&mut wire);
        0u32.put(&mut wire);
        Attributes::default().put(&mut wire);
        wire.check_ref().map_err(|_| too_large())?;
        let mut text = String::new();
        for cell in grid.row(y).filter(|cell| !cell.is_wide_continuation()) {
            wire.extend_from_slice(cell.shown().as_bytes());
            wire.check_ref().map_err(|_| too_large())?;
            budget
                .text(&mut text, cell.shown())
                .map_err(|_| too_large())?;
        }
        while text.chars().next_back().is_some_and(char::is_whitespace) {
            text.pop();
        }
        let mut line = budget.vector(1)?;
        line.push(LineRun {
            text,
            cells: 1,
            style: Attributes::default(),
        });
        lines.push(line);
    }
    let view = CaptureView {
        full: true,
        ..CaptureView::default()
    };
    captured(
        Of::Client(client),
        (grid.rows, grid.cols),
        grid.cursor,
        lines,
        view,
        &mut budget,
    )
}

/// What is known of a pane's program, for `json`: its foreground program,
/// the status it exited with, how long since it last wrote, and the modes
/// that say how it takes input; whether a client is in copy mode on it,
/// and the cell its program is told, by its tab's host
/// (`Session::host_panes`).
pub(crate) fn state(
    pane: &Pane,
    now: Instant,
    copy_mode: bool,
    budget: &mut crate::wire::Budget,
) -> Result<Vec<(&'static str, Json)>, crate::session::Error> {
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
        .map(|pid| fuxix::process::name(pid, budget))
        .transpose()?
        .flatten();
    let (launch_argv, argv) = arguments(&pane.runs, budget, |budget| {
        leader
            .map(|pid| fuxix::process::argv(pid, budget))
            .transpose()
            .map(Option::flatten)
    })?;
    let cwd = match leader
        .map(|pid| fuxix::process::cwd(pid, budget))
        .transpose()?
        .flatten()
    {
        Some(path) => {
            let text = fuxix::Allocation::lossy(budget, path.as_os_str().as_encoded_bytes())?;
            let capacity = path.capacity();
            drop(path);
            budget.release(capacity)?;
            Json::str(text)
        }
        None => Json::Null,
    };
    let idle = now.saturating_duration_since(pane.wrote_at).as_millis();
    let last_status = screen.commands().last_status;

    let (exited, signal) = match pane.process().exit_status() {
        Some(crate::process::Ended::Exited(code)) => (Json::int(code), Json::Null),
        Some(crate::process::Ended::Signalled(signal)) => (Json::Null, Json::int(signal)),
        Some(crate::process::Ended::Lost) | None => (Json::Null, Json::Null),
    };
    let hung_up = matches!(pane.process(), Process::HungUp(_));
    let mouse = screen.mouse_protocol_mode();
    let bracketed_paste = screen.mode(Mode::BracketedPaste);
    let mut pictures = Vec::new();
    for shown in screen.pictures() {
        budget.reserve(&mut pictures, 1)?;
        pictures.push(picture(shown, budget)?);
    }
    let pictures = Json::Array(pictures);
    let notification = pane.alerts.last.as_ref();
    // The fields whose value the program sent more of than is kept.
    let cut = [
        ("title", pane.title().cut()),
        ("notification", notification.is_some_and(|n| n.cut())),
        ("output", pane.left.output == crate::pane::Output::Cut),
    ];
    let truncated = cut.into_iter().filter(|(_, cut)| *cut);
    let mut cut_fields = budget.vector(3)?;
    for (field, _) in truncated {
        cut_fields.push(Json::str(budget.string(field)?));
    }
    let losses = pane.lost();
    let lost = budget.object([
        ("input", Json::uint(losses.input)),
        ("replies", Json::uint(losses.replies)),
        ("copies_cut", Json::uint(losses.copies_cut)),
        ("copies_not_text", Json::uint(losses.copies_not_text)),
        ("pictures", Json::uint(losses.pictures)),
        ("memory", Json::uint(losses.memory)),
    ])?;
    let cell = match cell {
        Some(cell) => budget.object([
            ("width", Json::int(cell.width())),
            ("height", Json::int(cell.height())),
        ])?,
        None => Json::Null,
    };
    let read_error = pane.read_error().map(|e| e.message(budget)).transpose()?;
    let values = [
        ("title", Json::str(budget.string(pane.title().as_str())?)),
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
        // Past u64's milliseconds no monotonic clock reaches: unknown.
        (
            "idle_ms",
            u64::try_from(idle).map_or(Json::Null, Json::Uint),
        ),
        ("alt_screen", Json::Bool(screen.mode(Mode::AlternateScreen))),
        ("mouse", Json::Bool(mouse != MouseProtocolMode::None)),
        // What it asked for: whether a move without a button is reported.
        (
            "mouse_mode",
            match mouse {
                MouseProtocolMode::None => Json::Null,
                MouseProtocolMode::Press => Json::str(budget.string("press")?),
                MouseProtocolMode::PressRelease => Json::str(budget.string("press_release")?),
                MouseProtocolMode::Highlight => Json::str(budget.string("highlight")?),
                MouseProtocolMode::ButtonMotion => Json::str(budget.string("button_motion")?),
                MouseProtocolMode::AnyMotion => Json::str(budget.string("any_motion")?),
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
            budget.optional_text(notification.map(|n| n.as_str()))?,
        ),
        ("truncated", Json::Array(cut_fields)),
        ("input_unread", Json::count(pane.left.input)),
        ("cwd", cwd),
        ("argv", argv),
    ];
    let mut fields = budget.vector(
        values
            .len()
            .checked_add(1)
            .ok_or_else(crate::wire::Budget::exceeded)?,
    )?;
    fields.extend(values);
    Ok(fields)
}

fn arguments(
    runs: &Runs,
    budget: &mut crate::wire::Budget,
    observe: impl FnOnce(
        &mut crate::wire::Budget,
    ) -> Result<Option<Vec<std::ffi::OsString>>, crate::session::Error>,
) -> Result<(Json, Json), crate::session::Error> {
    let argv = match observe(budget)? {
        Some(words) => {
            let bytes = words.iter().try_fold(
                words
                    .capacity()
                    .checked_mul(std::mem::size_of::<std::ffi::OsString>())
                    .ok_or_else(crate::wire::Budget::exceeded)?,
                |n, word| {
                    n.checked_add(word.capacity())
                        .ok_or_else(crate::wire::Budget::exceeded)
                },
            )?;
            let mut values = budget.vector(words.len())?;
            for word in &words {
                values.push(Json::str(fuxix::Allocation::lossy(
                    budget,
                    word.as_encoded_bytes(),
                )?));
            }
            drop(words);
            budget.release(bytes)?;
            Json::Array(values)
        }
        None => Json::Null,
    };
    let program = &runs.launch().argv;
    let mut values = budget.vector(program.len())?;
    for word in program.iter() {
        values.push(Json::str(budget.string(word)?));
    }
    let launch = Json::Array(values);
    Ok((launch, argv))
}

/// Where a picture is, for `json`: its top row (negative once scrolled
/// above the screen), first column, the cells it covers, its first rows
/// that are gone (`cropped`: scrolled off a region's top or a screen's
/// that keeps no history, the rest shown below them), and its size in
/// pixels.
fn picture(
    shown: fux_vt::Shown<'_>,
    budget: &mut crate::wire::Budget,
) -> Result<Json, crate::session::Error> {
    budget.object([
        ("row", Json::Int(shown.row)),
        ("col", Json::int(shown.col)),
        ("rows", Json::int(shown.rows)),
        ("cropped", Json::int(shown.cropped)),
        ("cols", Json::int(shown.cols)),
        ("width", Json::int(shown.picture.width())),
        ("height", Json::int(shown.picture.height())),
    ])
}

/// Remove the last line's unshown trailing blank cells, counted while
/// reading them rather than inferred from the run's cluster text.
fn trim_line(
    lines: &mut [Vec<LineRun>],
    blanks: usize,
    budget: &mut crate::wire::Budget,
) -> Result<(), crate::session::Error> {
    if let Some(line) = lines.last_mut() {
        for _ in 0..blanks {
            if let Some(last) = line.last_mut() {
                last.text.pop();
                last.cells = last.cells.saturating_sub(1);
                if last.cells == 0 {
                    budget.release(last.text.capacity())?;
                    line.pop();
                }
            }
        }
    }
    Ok(())
}

/// A capture of `lines`, its empty last lines dropped unless `full`.
fn captured(
    of: Of,
    (screen_rows, screen_cols): (u16, u16),
    screen_cursor: Option<(u16, u16)>,
    mut lines: Vec<Vec<LineRun>>,
    view: CaptureView,
    budget: &mut crate::wire::Budget,
) -> Result<Captured, crate::session::Error> {
    if !view.full {
        while lines.last().is_some_and(Vec::is_empty) {
            if let Some(line) = lines.pop() {
                budget.release(
                    line.capacity()
                        .checked_mul(std::mem::size_of::<LineRun>())
                        .ok_or_else(too_large)?,
                )?;
            }
        }
    }
    let old_capacity = lines
        .capacity()
        .checked_mul(std::mem::size_of::<Vec<LineRun>>())
        .ok_or_else(too_large)?;
    let mut captured = budget.vector(lines.len()).map_err(|_| too_large())?;
    for line in lines {
        let old = line
            .capacity()
            .checked_mul(std::mem::size_of::<LineRun>())
            .ok_or_else(too_large)?;
        let mut cuts = budget.vector(line.len()).map_err(|_| too_large())?;
        for run in line {
            cuts.push(Cut {
                text: run.text,
                cells: u32::try_from(run.cells).unwrap_or(u32::MAX),
                style: run.style,
            });
        }
        budget.release(old)?;
        captured.push(cuts);
    }
    budget.release(old_capacity)?;
    budget.slots::<Captured>(1)?;
    Ok(Captured {
        of,
        screen_rows,
        screen_cols,
        screen_cursor,
        lines: captured,
        styles: view.styles,
        marks: view.marks,
    })
}

/// Highlighted: reverse video, or a background colour.
fn highlighted(a: &Attributes) -> bool {
    a.inverse() || a.background() != Color::Default
}

/// A blank's style shows on the screen: a background colour, reverse
/// video, a line under or through it. One bold or coloured in front shows
/// none.
fn painted(a: &Attributes) -> bool {
    highlighted(a) || a.underline() || a.strikeout()
}

impl Captured {
    /// Charge all retained input before converting it on the client.
    pub(crate) fn charge(
        &self,
        budget: &mut crate::wire::Budget,
    ) -> Result<(), crate::session::Error> {
        budget.slots::<Vec<Cut>>(self.lines.capacity())?;
        for line in &self.lines {
            budget.slots::<Cut>(line.capacity())?;
            for cut in line {
                budget.charge(cut.text.capacity())?;
            }
        }
        if let Of::Pane { state, .. } = &self.of {
            budget.slots::<(String, Json)>(state.capacity())?;
            for (key, value) in state {
                budget.charge(key.capacity())?;
                value.charge(budget)?;
            }
        }
        Ok(())
    }

    /// As printed, admitted beside the retained capture before allocation.
    pub fn text(&self) -> Result<String, crate::session::Error> {
        let mut budget = crate::wire::Budget::new();
        budget.slots::<Captured>(1)?;
        self.text_with(&mut budget)
    }

    pub(crate) fn text_with(
        &self,
        budget: &mut crate::wire::Budget,
    ) -> Result<String, crate::session::Error> {
        self.charge(budget)?;
        budget.charge(self.text_scratch())?;
        let text = crate::json::render(budget, |out| self.write_text(out))?;
        budget.text_value(&text)?;
        Ok(text)
    }

    pub(crate) fn text_scratch(&self) -> usize {
        if self.styles { 128 } else { 0 }
    }

    pub(crate) fn write_text(&self, out: &mut dyn std::fmt::Write) -> std::fmt::Result {
        // All flags and three RGB colours fit in the declared scratch space.
        let mut sgr = Vec::new();
        sgr.try_reserve_exact(self.text_scratch())
            .map_err(|_| std::fmt::Error)?;
        for (i, line) in self.lines.iter().enumerate() {
            if i > 0 {
                out.write_char('\n')?;
            }
            write_line(out, line, self.marks, self.styles, &mut sgr)?;
        }
        Ok(())
    }

    /// JSON directly from the capture: no second graph of owned run objects.
    pub(crate) fn write_json(&self, out: &mut dyn std::fmt::Write) -> std::fmt::Result {
        match &self.of {
            Of::Pane { version, .. } => write!(out, "{{\"pane\":\"{}\"", version.pane)?,
            Of::Client(client) => write!(out, "{{\"client\":\"{client}\"")?,
        }
        write!(
            out,
            ",\"screen_rows\":{},\"screen_cols\":{},\"screen_cursor\":",
            self.screen_rows, self.screen_cols
        )?;
        match self.screen_cursor {
            Some((y, x)) => write!(out, "[{y},{x}]")?,
            None => out.write_str("null")?,
        }
        out.write_str(",\"lines\":[")?;
        struct Escaped<'a>(&'a mut dyn std::fmt::Write);
        impl std::fmt::Write for Escaped<'_> {
            fn write_str(&mut self, text: &str) -> std::fmt::Result {
                crate::json::escaped(self.0, text)
            }
        }
        for (i, line) in self.lines.iter().enumerate() {
            if i > 0 {
                out.write_char(',')?;
            }
            out.write_char('"')?;
            write_line(&mut Escaped(out), line, self.marks, false, &mut Vec::new())?;
            out.write_char('"')?;
        }
        out.write_char(']')?;
        if let Of::Pane { wrap_pending, .. } = &self.of {
            write!(out, ",\"wrap_pending\":{wrap_pending}")?;
        }
        if self.styles {
            out.write_str(",\"styles\":[")?;
            for (i, line) in self.lines.iter().enumerate() {
                if i > 0 {
                    out.write_char(',')?;
                }
                out.write_char('[')?;
                let mut start = 0u32;
                let mut first = true;
                for cut in line {
                    if cut.style != Attributes::default() {
                        if !first {
                            out.write_char(',')?;
                        }
                        first = false;
                        write!(out, "{{\"col\":{start},\"len\":{}", cut.cells)?;
                        describe(out, cut.style)?;
                        out.write_char('}')?;
                    }
                    start = start.saturating_add(cut.cells);
                }
                out.write_char(']')?;
            }
            out.write_char(']')?;
        }
        if let Of::Pane { version, state, .. } = &self.of {
            write!(out, ",\"version\":\"{version}\"")?;
            for (key, value) in state {
                out.write_char(',')?;
                crate::json::write_str(out, key)?;
                out.write_char(':')?;
                value.write(out)?;
            }
        }
        out.write_char('}')
    }
}

/// Write a line without allocating its plain or escaped intermediate text.
fn write_line(
    out: &mut dyn std::fmt::Write,
    line: &[Cut],
    marks: bool,
    styles: bool,
    sgr: &mut Vec<u8>,
) -> std::fmt::Result {
    let mut pen = Attributes::default();
    let mut on = false;
    for cut in line {
        let lit = marks && highlighted(&cut.style);
        if lit && !on {
            out.write_str("⟦")?;
        }
        if styles && cut.style != pen {
            sgr.clear();
            crate::render::sgr(sgr, cut.style, true);
            out.write_str(std::str::from_utf8(sgr).map_err(|_| std::fmt::Error)?)?;
            pen = cut.style;
        }
        if on && !lit {
            out.write_str("⟧")?;
        }
        on = lit;
        out.write_str(&cut.text)?;
    }
    if pen != Attributes::default() {
        out.write_str("\x1b[0m")?;
    }
    if on {
        out.write_str("⟧")?;
    }
    Ok(())
}

/// Attributes in the JSON vocabulary, streamed into the enclosing run.
fn describe(out: &mut dyn std::fmt::Write, a: Attributes) -> std::fmt::Result {
    for (name, colour) in [("fg", a.foreground()), ("bg", a.background())] {
        match colour {
            Color::Default => {}
            Color::Idx(n) => write!(out, ",\"{name}\":{n}")?,
            Color::Rgb(Rgb { r, g, b }) => write!(out, ",\"{name}\":\"#{r:02x}{g:02x}{b:02x}\"")?,
        }
    }
    let flags = a.sgr_flags().map(|(_, name)| name);
    let underline = a.underline().then_some("underline");
    let blink = (a.blink() != fux_vt::Blink::None).then_some("blink");
    for name in flags.chain(underline).chain(blink) {
        write!(out, ",\"{name}\":true")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::command::{CaptureView, Program};
    use crate::id::PaneId;
    use crate::pane::tests::Lone;
    use crate::pane::{Pane, Runs};

    #[test]
    fn argument_observation_peak_stays_within_shared_grant()
    -> Result<(), Box<dyn std::error::Error>> {
        let allocator = fux_vt_peak::ThreadPeak;
        let launch = Runs::Explicit(crate::pane::Launch {
            argv: Program::new(std::iter::repeat_n('l', 6000).collect(), Vec::new())?,
            cwd: "/".into(),
            env: crate::command::EnvPatch::default(),
        });
        let mut peak = 0;
        for (len, byte) in [(2500, b'a'), (5000, b'a'), (2500, 0xff)] {
            let mut raw: Vec<u8> = std::iter::repeat_n(byte, len).collect();
            raw.push(0);
            let baseline = allocator.live();
            allocator.reset_peak();
            let result = super::arguments(
                &launch,
                &mut crate::wire::Budget::with_limit(8192),
                |budget| fuxix::process::argv_read(raw.as_slice(), budget),
            );
            assert!(result.is_err());
            peak = peak.max(allocator.peak().saturating_sub(baseline));
        }
        assert!(
            peak <= 8192,
            "argument observation allocated {peak} bytes under an 8192-byte grant"
        );
        let raw = b"a\0\xff\0\0";
        let shell = crate::pane::tests::shell()?;
        let mut budget = crate::wire::Budget::with_limit(8192);
        let baseline = allocator.live();
        allocator.reset_peak();
        let (saved, observed) = super::arguments(&shell, &mut budget, |budget| {
            fuxix::process::argv_read(raw.as_slice(), budget)
        })?;
        let retained = allocator.live().saturating_sub(baseline);
        assert_eq!(retained, 8192usize.saturating_sub(budget.remaining()));
        assert!(allocator.peak().saturating_sub(baseline) <= 8192);
        assert_eq!(observed.render(), "[\"a\",\"�\",\"\"]");
        assert_eq!(saved.render(), "[\"/bin/sh\"]");
        Ok(())
    }

    #[test]
    fn wide_default_history_is_admitted() -> Result<(), Box<dyn std::error::Error>> {
        let history = crate::script::default_state()
            .map_err(|e| e.to_string())?
            .preferences
            .history_bytes
            .get();
        let mut pane = Lone::new(Pane::new(
            PaneId::of(1),
            None,
            crate::pane::tests::shell()?,
            fux_vt::Size::new(3, 200)?,
            crate::pane::tests::options(history)?,
        )?);
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
            crate::command::Run::now().ok_or("clock")?,
            Some(usize::MAX),
            CaptureView::default(),
            false,
        )?;
        assert!(capture.lines.len() > 80_000);
        let full = super::pane(
            &pane,
            crate::command::Run::now().ok_or("clock")?,
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
    fn last_output_ignores_unselected_history() -> Result<(), Box<dyn std::error::Error>> {
        let mut pane = Lone::new(Pane::new(
            PaneId::of(1),
            None,
            crate::pane::tests::shell()?,
            fux_vt::Size::new(3, 4)?,
            crate::pane::tests::options(1 << 20)?,
        )?);
        for _ in 0..10 {
            pane.output(b"x\r\n");
        }
        pane.output(b"\x1b]133;C\x07hi\x1b]133;D;0\x07");
        let view = CaptureView {
            source: crate::command::Source::LastOutput,
            ..CaptureView::default()
        };
        let capture = super::pane_with(
            &pane,
            crate::command::Run::now().ok_or("clock")?,
            None,
            view,
            false,
            super::Limits { cells: 2 },
        )?;
        assert_eq!(capture.text()?, "hi");
        Ok(())
    }

    #[test]
    fn joined_prefix_costs_metadata_only() -> Result<(), Box<dyn std::error::Error>> {
        let mut pane = Lone::new(Pane::new(
            PaneId::of(1),
            None,
            crate::pane::tests::shell()?,
            fux_vt::Size::new(1, 200)?,
            crate::pane::tests::options(0)?,
        )?);
        pane.output(b"x");
        let row = pane.screen().rows().next().ok_or("row")?;
        let read = Box::new(std::iter::repeat_n((row, 0..usize::MAX), 100));
        let selection = super::selected_rows(
            read,
            CaptureView {
                rows: Some((90, 95)),
                ..CaptureView::default()
            },
            super::Limits { cells: 1200 },
        )?;
        assert_eq!((selection.start, selection.count), (90, 6));
        Ok(())
    }

    #[test]
    fn short_lines_charge_allocated_capacity() -> Result<(), Box<dyn std::error::Error>> {
        let mut pane = Lone::new(Pane::new(
            PaneId::of(1),
            None,
            crate::pane::tests::shell()?,
            fux_vt::Size::new(1, 2)?,
            crate::pane::tests::options(0)?,
        )?);
        pane.output(b"x");
        let row = pane.screen().rows().next().ok_or("row")?;
        let read = std::iter::repeat_n((row, 0..usize::MAX), 100);
        let mut budget = crate::wire::Budget::with_limit(8192);
        if let Ok(captured) = super::build(
            read,
            super::Of::Client(crate::id::ClientId::of(1)),
            ((100, 2), None),
            CaptureView::default(),
            &mut budget,
        ) {
            let mut allocated = captured
                .lines
                .capacity()
                .saturating_mul(std::mem::size_of::<Vec<super::Cut>>());
            for line in &captured.lines {
                allocated = allocated.saturating_add(
                    line.capacity()
                        .saturating_mul(std::mem::size_of::<super::Cut>()),
                );
                for run in line {
                    allocated = allocated.saturating_add(run.text.capacity());
                }
            }
            assert!(
                allocated <= 8192,
                "retained {allocated} bytes for an 8192-byte grant"
            );
        }
        Ok(())
    }

    /// Blanks still cost work, and a single joined row range can expand
    /// beyond the cap. A narrower physical read remains available.
    #[test]
    fn over_budget_capture_refuses_without_clipping() -> Result<(), Box<dyn std::error::Error>> {
        let mut pane = Lone::new(Pane::new(
            PaneId::of(1),
            None,
            crate::pane::tests::shell()?,
            fux_vt::Size::new(3, 80)?,
            crate::pane::tests::options(1 << 20)?,
        )?);
        pane.output(b"\x1b[41m \x1b[400b");
        for _ in 0..10 {
            pane.output(b"\r\n");
        }
        let run = crate::command::Run::now().ok_or("a clock")?;
        let refusal = super::pane_with(
            &pane,
            run,
            Some(usize::MAX),
            CaptureView::default(),
            false,
            super::Limits { cells: 240 },
        );
        assert!(refusal.is_err());
        assert!(
            refusal
                .err()
                .is_some_and(|e| e.to_string().contains("request fewer history"))
        );
        let view = CaptureView {
            wrapped: true,
            rows: Some((0, 0)),
            full: true,
            ..CaptureView::default()
        };
        let read = super::pane(&pane, run, Some(usize::MAX), view, false)?;
        assert_eq!(read.lines.len(), 1);
        assert_eq!(
            read.lines.first().and_then(|l| l.first()).map(|r| r.cells),
            Some(80)
        );
        let limits = super::Limits { cells: 80 };
        let read = Box::new(pane.screen().rows().map(|row| (row, 0..usize::MAX)));
        assert!(
            super::selected_rows(
                read,
                CaptureView {
                    rows: Some((0, 0)),
                    ..CaptureView::default()
                },
                limits
            )
            .is_err()
        );
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
            let size = fux_vt::Size::new(3, 5)?;
            let mut pane = Lone::new(Pane::new(
                PaneId::of(1),
                None,
                crate::pane::tests::shell()?,
                size,
                crate::pane::tests::options(0)?,
            )?);
            pane.output(output.as_bytes());
            Ok(super::pane(
                &pane,
                crate::command::Run::now().ok_or("a clock")?,
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
        let size = fux_vt::Size::new(6, 5)?;
        let mut pane = Lone::new(Pane::new(
            PaneId::of(1),
            None,
            crate::pane::tests::shell()?,
            size,
            crate::pane::tests::options(0)?,
        )?);
        pane.output(b"top\r\nabcdefghijklm\r\nend");
        let run = crate::command::Run::now().ok_or("a clock")?;
        let capture = |rows, wrapped| -> Result<String, Box<dyn std::error::Error>> {
            let view = CaptureView {
                rows: Some(rows),
                wrapped,
                ..CaptureView::default()
            };
            Ok(super::pane(&pane, run, None, view, false)?.text()?)
        };
        assert_eq!(capture((2, 2), false)?, "abcdefghijklm");
        assert_eq!(capture((0, 1), false)?, "top\nabcdefghijklm");
        assert_eq!(capture((3, 4), false)?, "abcdefghijklm\nend");
        assert_eq!(capture((2, 2), true)?, "fghij");
        assert_eq!(capture((4, 3), true)?, "");
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
        let mut pane = Lone::new(Pane::new(
            PaneId::of(1),
            None,
            crate::pane::tests::shell()?,
            size,
            crate::pane::tests::options(0)?,
        )?);
        // 世 at col 4 does not fit and wraps on, leaving the spacer there.
        pane.output("abcd\x1b[5G世".as_bytes());
        let text = super::pane(
            &pane,
            crate::command::Run::now().ok_or("a clock")?,
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
        let mut pane = Lone::new(Pane::new(
            PaneId::of(1),
            None,
            crate::pane::tests::shell()?,
            fux_vt::Size::new(5, 5)?,
            crate::pane::tests::options(0)?,
        )?);
        pane.output(
            "\x1b[31mabcde世\x1b[m\r\nx\x1b[4m  \x1b[m\r\ny\x1b[41m  \x1b[31;49m  \x1b[m"
                .as_bytes(),
        );
        let run = crate::command::Run::now().ok_or("a clock")?;
        for flags in 0..16 {
            let view = CaptureView {
                styles: flags & 1 != 0,
                marks: flags & 2 != 0,
                full: flags & 4 != 0,
                wrapped: flags & 8 != 0,
                ..CaptureView::default()
            };
            let got = super::pane(&pane, run, None, view, false)?;
            let actual: Vec<(String, u32)> = got
                .lines
                .iter()
                .map(|line| {
                    (
                        line.iter().map(|cut| cut.text.as_str()).collect(),
                        line.iter().map(|cut| cut.cells).sum(),
                    )
                })
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
            let want: Vec<_> = want
                .into_iter()
                .map(|(text, cells)| (text.to_owned(), cells))
                .collect();
            assert_eq!(actual, want, "{view:?}");
            for line in &got.lines {
                assert!(
                    line.iter()
                        .zip(line.iter().skip(1))
                        .all(|(a, b)| a.style != b.style)
                );
                if !view.styles && !view.marks {
                    assert!(
                        line.iter()
                            .all(|cut| cut.style == fux_vt::Attributes::default())
                    );
                }
            }
        }
        Ok(())
    }

    /// A space joined to a prepend character is glyph text, not a blank
    /// cell that trimming may remove.
    #[test]
    fn a_space_inside_a_cluster_is_not_a_trailing_blank() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut pane = Lone::new(Pane::new(
            PaneId::of(1),
            None,
            crate::pane::tests::shell()?,
            fux_vt::Size::new(2, 10)?,
            crate::pane::tests::options(0)?,
        )?);
        pane.output("x\u{0600}  ".as_bytes());
        let got = super::pane(
            &pane,
            crate::command::Run::now().ok_or("a clock")?,
            None,
            CaptureView::default(),
            false,
        )?;
        assert_eq!(got.text()?, "x\u{0600} ");
        assert_eq!(
            got.lines
                .first()
                .and_then(|line| line.first())
                .map(|cut| cut.cells),
            Some(3)
        );
        Ok(())
    }

    /// Trimming precedes the wire count's clamp, including when the
    /// private count exceeds u32. A synthetic count avoids a GiB fixture.
    #[test]
    fn cells_are_clamped_only_after_trimming() -> Result<(), Box<dyn std::error::Error>> {
        let max = usize::try_from(u32::MAX)?;
        let mut lines = vec![vec![super::LineRun {
            text: "x  ".into(),
            cells: max.saturating_add(2),
            style: fux_vt::Attributes::default(),
        }]];
        super::trim_line(&mut lines, 2, &mut crate::wire::Budget::new())?;
        let got = super::captured(
            super::Of::Client(crate::id::ClientId::of(1)),
            (1, 3),
            None,
            lines,
            CaptureView::default(),
            &mut crate::wire::Budget::new(),
        )?;
        assert_eq!(got.text()?, "x");
        assert_eq!(
            got.lines
                .first()
                .and_then(|line| line.first())
                .map(|run| run.cells),
            Some(u32::MAX)
        );
        Ok(())
    }

    /// Selecting part of a joined line reads through that line's end,
    /// without walking the unrelated rows that follow it.
    #[test]
    fn a_joined_range_stops_at_its_last_logical_line() -> Result<(), Box<dyn std::error::Error>> {
        let mut pane = Lone::new(Pane::new(
            PaneId::of(1),
            None,
            crate::pane::tests::shell()?,
            fux_vt::Size::new(6, 5)?,
            crate::pane::tests::options(0)?,
        )?);
        pane.output(b"abcdefghijklm\r\nlater");
        let visits = std::cell::Cell::new(0usize);
        let read = Box::new(pane.screen().rows().map(|row| {
            visits.set(visits.get().saturating_add(1));
            (row, 0..usize::MAX)
        }));
        let selection = super::selected_rows(
            read,
            CaptureView {
                rows: Some((1, 1)),
                ..CaptureView::default()
            },
            super::Limits::new(8 << 20, (6, 5))?,
        )?;
        assert_eq!(selection.count, 3);
        assert_eq!(visits.get(), 3);
        Ok(())
    }

    /// A capture goes on the wire and reads back the same, its styles
    /// too; JSON nested past its bound is refused rather than read.
    #[test]
    fn a_capture_reads_back_as_written() -> Result<(), Box<dyn std::error::Error>> {
        use crate::json::Json;
        use crate::wire::{Reader, encode};
        let size = fux_vt::Size::new(3, 20)?;
        let mut pane = Lone::new(Pane::new(
            PaneId::of(1),
            None,
            crate::pane::tests::shell()?,
            size,
            crate::pane::tests::options(0)?,
        )?);
        pane.output(b"\x1b[1;4:3;5;38;2;1;2;3;48;5;9;58;5;2mstyled\x1b[m plain");
        let view = CaptureView {
            styles: true,
            ..CaptureView::default()
        };
        let run = crate::command::Run::now().ok_or("a clock")?;
        let body =
            crate::protocol::Body::Capture(Box::new(super::pane(&pane, run, None, view, false)?));
        assert_eq!(
            Reader::new(&encode(&body).unwrap_or_default()).whole::<crate::protocol::Body>()?,
            body
        );
        let deep = (0..40).fold(Json::Null, |inner, _| Json::Array(vec![inner]));
        assert!(
            Reader::new(&encode(&deep).unwrap_or_default())
                .whole::<Json>()
                .is_err()
        );
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
        let mut pane = Lone::new(Pane::new(
            PaneId::of(1),
            None,
            crate::pane::tests::shell()?,
            size,
            crate::pane::tests::options(0)?,
        )?);
        pane.output(b"$ \x1b]133;C\x07hello\x1b]133;D;0\x07PROMPT> ");
        let text = super::pane(
            &pane,
            crate::command::Run::now().ok_or("a clock")?,
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
            crate::command::Run::now().ok_or("a clock")?,
            None,
            view,
            false,
        )?
        .text()?;
        assert_eq!(text, "step 1");
        Ok(())
    }
}
