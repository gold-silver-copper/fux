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

/// Rows selected for a capture, each with the columns its source takes.
type ReadRows<'a> = Box<dyn Iterator<Item = (Row<'a>, std::ops::Range<usize>)> + 'a>;

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
    history_bytes: usize,
) -> Result<Captured, crate::session::Error> {
    let screen = pane.screen();
    let mut lines: Vec<Vec<LineRun>> = Vec::new();
    // A soft-wrapped row goes on in the next, joined unless `wrapped`.
    let mut wraps = false;
    let mut blanks = 0usize;
    let limits = Limits::new(history_bytes, screen.size().into())?;
    if view.source == Source::LastOutput && screen.rows().len() > limits.rows.min(limits.cells) {
        return Err(too_large());
    }
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
    let read = || selection.read(rows_of(screen, history, view).0);
    let mut budget = crate::wire::Budget::new();
    let mut state = state(pane, Instant::now(), copy_mode, &mut budget)?;
    // The history rows kept above the first row read: history it was not
    // asked for (rows a range leaves out are not counted).
    state.push(("above", above.map_or(Json::Null, Json::count)));
    let of = Of::Pane {
        version: pane.version(run),
        wrap_pending: cursor.is_some() && screen.pending_wrap(),
        state: state.into_iter().map(|(k, v)| (k.to_owned(), v)).collect(),
    };
    admit(read(), view, &of)?;
    for (row, span) in read() {
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
        // Columns are relative to the span (last-output may start mid-row).
        let (start, count) = view.cols.map_or((0, usize::MAX), |(start, end)| {
            (
                usize::from(start),
                usize::from(end)
                    .saturating_sub(usize::from(start))
                    .saturating_add(1),
            )
        });
        let mut cells = cells.skip(start).peekable();
        if !wraps || lines.is_empty() {
            trim_line(&mut lines, blanks);
            blanks = 0;
            lines.try_reserve(1).map_err(|_| too_large())?;
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
                    line.try_reserve(1).map_err(|_| too_large())?;
                    line.push(LineRun {
                        text: String::new(),
                        cells: 0,
                        style,
                    });
                }
                if let Some(last) = line.last_mut() {
                    last.text.try_reserve(text.len()).map_err(|_| too_large())?;
                    last.text.push_str(text);
                    last.cells = last.cells.saturating_add(1);
                }
            }
        }
        wraps = !view.wrapped && row.continues();
    }
    trim_line(&mut lines, blanks);
    Ok(captured(of, screen.size().into(), cursor, lines, view))
}

/// Select physical rows, widening a joined range to the logical lines
/// at its ends without reading the rows after those lines.
struct Selection {
    start: usize,
    count: usize,
}
impl Selection {
    fn read<'a>(&self, read: ReadRows<'a>) -> ReadRows<'a> {
        Box::new(read.skip(self.start).take(self.count))
    }
}

fn selected_rows(
    mut read: ReadRows<'_>,
    view: CaptureView,
    limits: Limits,
) -> Result<Selection, crate::session::Error> {
    let (first, last) = view
        .rows
        .map_or((0, usize::MAX), |(a, b)| (usize::from(a), usize::from(b)));
    let skip = if view.wrapped { first } else { 0 };
    if skip > 0 {
        read = Box::new(read.skip(skip));
    }
    let mut cells = limits.cells;
    let (mut start, mut count, mut line_start) = (skip, 0usize, 0usize);
    for (index, (row, _)) in read.enumerate() {
        // Bound the metadata scan too, including the prefix needed to find
        // a joined range's first whole logical line. No row vector is made.
        cells = cells.checked_sub(row.len().max(1)).ok_or_else(too_large)?;
        if index >= limits.rows {
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
        }
    }
    if !view.wrapped && first >= start.saturating_add(count) {
        count = 0;
    }
    Ok(Selection { start, count })
}

/// Admission uses today's history setting here alone. Two visited cells
/// per history byte admit the measured default-history blank workload
/// (115,673 rows at 80 columns), plus the live screen. Large histories
/// stop at 16 Mi cells, the next binary power above that measurement,
/// and still meet the reply's independent byte and allocation caps.
#[derive(Clone, Copy)]
struct Limits {
    cells: usize,
    rows: usize,
}
impl Limits {
    fn new(history_bytes: usize, (rows, cols): (u16, u16)) -> Result<Self, crate::session::Error> {
        let live = usize::from(rows)
            .checked_mul(usize::from(cols))
            .ok_or_else(too_large)?;
        let cells = history_bytes
            .checked_mul(2)
            .and_then(|n| n.checked_add(live))
            .ok_or_else(too_large)?
            .min(16 << 20);
        Ok(Self {
            cells,
            rows: crate::wire::MAX_DECODED
                .checked_div(std::mem::size_of::<Vec<Cut>>())
                .ok_or_else(too_large)?
                / 4,
        })
    }
}

fn too_large() -> crate::session::Error {
    crate::session::fail(
        crate::code::Raise::TooLong,
        "capture exceeds its cell, byte or allocation limit; request fewer history rows or a narrower wrapped range",
    )
}

/// Count the untrimmed payload and retained run slots before creating any
/// text or runs. Trimming can only reduce this conservative admission.
fn admit(rows: ReadRows<'_>, view: CaptureView, of: &Of) -> Result<(), crate::session::Error> {
    use crate::wire::{MAX_DECODED, MAX_REPLY, Sink, Wire, Writer};
    let mut wire = Writer::counting(MAX_REPLY.saturating_sub(64));
    of.put(&mut wire);
    let mut heap = MAX_DECODED / 2;
    for (row, span) in rows {
        heap = heap
            .checked_sub(std::mem::size_of::<Vec<Cut>>())
            .ok_or_else(too_large)?;
        0u32.put(&mut wire);
        let spacer = usize::from(!view.wrapped && row.ends_in_spacer());
        let end = row.len().saturating_sub(spacer).min(span.end);
        let from = span.start.min(end);
        let (start, count) = view.cols.map_or((0, usize::MAX), |(a, b)| {
            (
                usize::from(a),
                usize::from(b)
                    .saturating_sub(usize::from(a))
                    .saturating_add(1),
            )
        });
        let mut previous = None;
        let mut lead = Attributes::default();
        for cell in row.cells().take(end).skip(from).skip(start).take(count) {
            let text = if cell.is_wide_continuation() {
                " "
            } else {
                lead = cell.attributes();
                cell.shown()
            };
            let style = if view.styles || view.marks {
                lead
            } else {
                Attributes::default()
            };
            if previous != Some(style) {
                0u32.put(&mut wire); // text length
                0u32.put(&mut wire); // cells
                style.put(&mut wire);
                heap = heap
                    .checked_sub(std::mem::size_of::<LineRun>())
                    .ok_or_else(too_large)?;
                previous = Some(style);
            }
            wire.extend_from_slice(text.as_bytes());
            heap = heap.checked_sub(text.len()).ok_or_else(too_large)?;
            if wire.full() {
                return Err(too_large());
            }
        }
    }
    wire.check().map_err(|_| too_large())
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
    let read = Box::new(rows.skip(skip).map(|row| (row, 0..usize::MAX)));
    (read, Some(skip))
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
        return Box::new(std::iter::empty());
    };
    let Some(last) = screen.row_index(end.row) else {
        return Box::new(std::iter::empty());
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
    let rows = screen
        .rows()
        .skip(first)
        .take(last.saturating_sub(first).saturating_add(1));
    let count = rows.len();
    Box::new(rows.enumerate().filter_map(move |(i, row)| {
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
    }))
}

/// What a client's terminal shows, row by row.
pub fn client(
    client: ClientId,
    grid: &crate::render::Grid,
) -> Result<Captured, crate::session::Error> {
    client_room(grid.rows, grid.cols)?;
    let mut bytes = crate::wire::MAX_REPLY.saturating_sub(1 << 20);
    for row in 0..grid.rows {
        bytes = bytes.checked_sub(32).ok_or_else(too_large)?;
        for cell in grid.row(row) {
            bytes = bytes
                .checked_sub(cell.shown().len())
                .ok_or_else(too_large)?;
        }
    }
    let lines = (0..grid.rows)
        .map(|y| {
            vec![LineRun {
                text: grid.row_text(y),
                cells: 1,
                style: Attributes::default(),
            }]
        })
        .collect();
    let view = CaptureView {
        full: true,
        ..CaptureView::default()
    };
    Ok(captured(
        Of::Client(client),
        (grid.rows, grid.cols),
        grid.cursor,
        lines,
        view,
    ))
}

/// Admit the composed grid before its cells are allocated or painted.
pub(crate) fn client_room(rows: u16, cols: u16) -> Result<(), crate::session::Error> {
    let cells = usize::from(rows)
        .checked_mul(usize::from(cols))
        .ok_or_else(too_large)?;
    if cells
        > crate::wire::MAX_REPLY
            .checked_div(std::mem::size_of::<LineRun>())
            .ok_or_else(too_large)?
    {
        return Err(too_large());
    }
    Ok(())
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
    budget.slots::<(String, Json)>(64)?;
    budget.charge(pane.title().as_str().len())?;
    let screen = pane.screen();
    let cell = screen.graphics().cell();
    // What runs in the foreground: a job the shell started, else the
    // program itself; nothing, where its terminal can't tell.
    let job = pane.process().job();
    let foreground = (pane.process().child()).and_then(|child| match job {
        Ok(job) => Some(job.unwrap_or(child.leader.pid())),
        Err(_) => None,
    });
    let program = foreground.and_then(fuxix::process::name);
    let idle = now.saturating_duration_since(pane.wrote_at).as_millis();
    let last_status = screen.commands().last_status;
    let argv = match (&pane.runs, job) {
        (_, Ok(Some(job))) => fuxix::process::argv(job, crate::wire::MAX_REPLY / 4)
            .map_err(|_| crate::wire::Budget::exceeded())?,
        (Runs::Explicit(launch), Ok(None)) => {
            budget.slots::<std::ffi::OsString>(launch.argv.len())?;
            for word in &launch.argv {
                budget.charge(word.len())?;
            }
            Some(launch.argv.iter().map(Into::into).collect())
        }
        (Runs::DefaultShell, Ok(None)) => foreground
            .map(|pid| fuxix::process::argv(pid, crate::wire::MAX_REPLY / 4))
            .transpose()
            .map_err(|_| crate::wire::Budget::exceeded())?
            .flatten(),
        (_, Err(_)) => None,
    };
    if let Some(argv) = &argv {
        budget.slots::<Json>(argv.len())?;
        for word in argv {
            budget.charge(
                word.len()
                    .checked_mul(3)
                    .ok_or_else(crate::wire::Budget::exceeded)?,
            )?;
        }
    }
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
        budget.slots::<(String, Json)>(7)?;
        budget.slots::<Json>(1)?;
        pictures
            .try_reserve(1)
            .map_err(|_| crate::wire::Budget::exceeded())?;
        pictures.push(picture(shown));
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
    let truncated = truncated.map(|(field, _)| Json::str(field)).collect();
    Ok(vec![
        ("title", Json::str(pane.title().as_str().to_owned())),
        ("program", program.map_or(Json::Null, Json::str)),
        // Its program closed its terminal and runs on: nothing reads keys
        // there (the terminal hung up; Linux says so).
        ("hung_up", Json::Bool(hung_up)),
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
                MouseProtocolMode::Press => Json::str("press"),
                MouseProtocolMode::PressRelease => Json::str("press_release"),
                MouseProtocolMode::Highlight => Json::str("highlight"),
                MouseProtocolMode::ButtonMotion => Json::str("button_motion"),
                MouseProtocolMode::AnyMotion => Json::str("any_motion"),
            },
        ),
        ("bracketed_paste", Json::Bool(bracketed_paste)),
        ("pictures", pictures),
        ("lost", pane.lost().json()),
        (
            "cell",
            cell.map_or(Json::Null, |cell| {
                Json::object(vec![
                    ("width", Json::int(cell.width())),
                    ("height", Json::int(cell.height())),
                ])
            }),
        ),
        ("cursor_visible", Json::Bool(screen.mode(Mode::ShowCursor))),
        ("history_lines", Json::count(screen.history_len())),
        ("copy_mode", Json::Bool(copy_mode)),
        ("command_running", Json::Bool(pane.command_running())),
        ("last_status", last_status.map_or(Json::Null, Json::int)),
        ("bells", Json::uint(pane.alerts.rung)),
        (
            "notification",
            notification.map_or(Json::Null, |n| Json::str(n.as_str().to_owned())),
        ),
        ("truncated", Json::Array(truncated)),
        ("input_unread", Json::count(pane.left.input)),
        (
            "cwd",
            foreground
                .and_then(fuxix::process::cwd)
                .map_or(Json::Null, |p| text(p.as_os_str())),
        ),
        (
            "argv",
            argv.map_or(Json::Null, |a| {
                Json::Array(a.iter().map(|w| text(w)).collect())
            }),
        ),
    ])
}

/// A path or an argument as JSON text, which must be UTF-8: each byte
/// that is not shown as U+FFFD, as `docs/json.md` says.
fn text(os: &std::ffi::OsStr) -> Json {
    Json::str(os.to_string_lossy().into_owned())
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

/// Remove the last line's unshown trailing blank cells, counted while
/// reading them rather than inferred from the run's cluster text.
fn trim_line(lines: &mut [Vec<LineRun>], blanks: usize) {
    if let Some(line) = lines.last_mut() {
        for _ in 0..blanks {
            if let Some(last) = line.last_mut() {
                last.text.pop();
                last.cells = last.cells.saturating_sub(1);
                if last.cells == 0 {
                    line.pop();
                }
            }
        }
    }
}

/// A capture of `lines`, its empty last lines dropped unless `full`.
fn captured(
    of: Of,
    (screen_rows, screen_cols): (u16, u16),
    screen_cursor: Option<(u16, u16)>,
    mut lines: Vec<Vec<LineRun>>,
    view: CaptureView,
) -> Captured {
    // Trimming the runs may leave empty lines at the end; full keeps them.
    if !view.full {
        while lines.last().is_some_and(Vec::is_empty) {
            lines.pop();
        }
    }
    let lines = lines
        .into_iter()
        .map(|line| {
            line.into_iter()
                .map(|run| Cut {
                    text: run.text,
                    cells: u32::try_from(run.cells).unwrap_or(u32::MAX),
                    style: run.style,
                })
                .collect()
        })
        .collect();
    Captured {
        of,
        screen_rows,
        screen_cols,
        screen_cursor,
        lines,
        styles: view.styles,
        marks: view.marks,
    }
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
    /// As it is printed: its lines, in SGR with `styles`, what is
    /// highlighted between `⟦` and `⟧` with `marks`.
    pub fn text(&self) -> String {
        let lines: Vec<String> = (self.lines.iter())
            .map(|line| sgr_line(line, self.marks, self.styles))
            .collect();
        lines.join("\n")
    }

    /// As `json` gives it: an object naming what it read, with its
    /// screen's size and cursor, its lines as plain text, their style runs
    /// with `styles`, and a pane's version, pending wrap and state.
    pub fn json(&self) -> Json {
        let mut fields = match &self.of {
            Of::Pane { version, .. } => vec![("pane", Json::str(version.pane.to_string()))],
            Of::Client(client) => vec![("client", Json::str(client.to_string()))],
        };
        let cursor = self.screen_cursor.map_or(Json::Null, |(y, x)| {
            Json::Array(vec![Json::int(y), Json::int(x)])
        });
        let lines = (self.lines.iter()).map(|l| Json::str(sgr_line(l, self.marks, false)));
        fields.extend([
            ("screen_rows", Json::int(self.screen_rows)),
            ("screen_cols", Json::int(self.screen_cols)),
            ("screen_cursor", cursor),
            ("lines", Json::Array(lines.collect())),
        ]);
        if let Of::Pane { wrap_pending, .. } = &self.of {
            fields.push(("wrap_pending", Json::Bool(*wrap_pending)));
        }
        if self.styles {
            fields.push(("styles", Json::Array(self.lines.iter().map(runs).collect())));
        }
        let mut json = Json::object(fields);
        if let (Json::Object(all), Of::Pane { version, state, .. }) = (&mut json, &self.of) {
            all.push(("version".to_owned(), Json::str(version.to_string())));
            all.extend(state.iter().cloned());
        }
        json
    }
}

/// A line, in SGR with `styles`: each change of attributes set from
/// nothing, and reset at its end. With `marks`, what is highlighted
/// (reverse video, or a background colour) is between `⟦` and `⟧`, drawn
/// outside the SGR.
fn sgr_line(line: &[Cut], marks: bool, styles: bool) -> String {
    let mut out = Vec::new();
    let mut pen = Attributes::default();
    let mut on = false;
    for cut in line {
        let lit = marks && highlighted(&cut.style);
        if lit && !on {
            out.extend_from_slice("⟦".as_bytes());
        }
        if styles && cut.style != pen {
            crate::render::sgr(&mut out, cut.style, true);
            pen = cut.style;
        }
        if on && !lit {
            out.extend_from_slice("⟧".as_bytes());
        }
        on = lit;
        out.extend_from_slice(cut.text.as_bytes());
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
fn runs(line: &Vec<Cut>) -> Json {
    let mut runs = Vec::new();
    let mut start = 0u32;
    for cut in line {
        if cut.style != Attributes::default() {
            let mut fields = vec![("col", Json::uint(start)), ("len", Json::uint(cut.cells))];
            fields.extend(described(cut.style));
            runs.push(Json::object(fields));
        }
        start = start.saturating_add(cut.cells);
    }
    Json::Array(runs)
}

/// Attributes as `json` names them: the colours other than the default,
/// and the attributes set.
fn described(a: Attributes) -> Vec<(&'static str, Json)> {
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
    use crate::pane::tests::Lone;
    use crate::pane::{Pane, Runs};

    /// Blanks still cost work, and a single joined row range can expand
    /// beyond the cap. A narrower physical read remains available.
    #[test]
    fn over_budget_capture_refuses_without_clipping() -> Result<(), Box<dyn std::error::Error>> {
        let mut pane = Lone::new(Pane::new(
            PaneId::of(1),
            "sh".into(),
            Runs::DefaultShell,
            fux_vt::Size::new(3, 80)?,
            1 << 20,
        )?);
        pane.output(b"\x1b[41m \x1b[400b");
        for _ in 0..10 {
            pane.output(b"\r\n");
        }
        let run = crate::command::Run::now().ok_or("a clock")?;
        let refusal = super::pane(
            &pane,
            run,
            Some(usize::MAX),
            CaptureView::default(),
            false,
            0,
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
        let read = super::pane(&pane, run, Some(usize::MAX), view, false, 0)?;
        assert_eq!(read.lines.len(), 1);
        assert_eq!(
            read.lines.first().and_then(|l| l.first()).map(|r| r.cells),
            Some(80)
        );
        let limits = super::Limits {
            cells: 80,
            rows: 10,
        };
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
                "sh".into(),
                Runs::DefaultShell,
                size,
                0,
            )?);
            pane.output(output.as_bytes());
            Ok(super::pane(
                &pane,
                crate::command::Run::now().ok_or("a clock")?,
                None,
                join,
                false,
                8 << 20,
            )?
            .text())
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
            "sh".into(),
            Runs::DefaultShell,
            size,
            0,
        )?);
        pane.output(b"top\r\nabcdefghijklm\r\nend");
        let run = crate::command::Run::now().ok_or("a clock")?;
        let capture = |rows, wrapped| -> Result<String, Box<dyn std::error::Error>> {
            let view = CaptureView {
                rows: Some(rows),
                wrapped,
                ..CaptureView::default()
            };
            Ok(super::pane(&pane, run, None, view, false, 8 << 20)?.text())
        };
        assert_eq!(capture((2, 2), false)?, "abcdefghijklm");
        assert_eq!(capture((0, 1), false)?, "top\nabcdefghijklm");
        assert_eq!(capture((3, 4), false)?, "abcdefghijklm\nend");
        assert_eq!(capture((2, 2), true)?, "fghij");
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
            "sh".into(),
            Runs::DefaultShell,
            size,
            0,
        )?);
        // 世 at col 4 does not fit and wraps on, leaving the spacer there.
        pane.output("abcd\x1b[5G世".as_bytes());
        let text = super::pane(
            &pane,
            crate::command::Run::now().ok_or("a clock")?,
            None,
            view,
            false,
            8 << 20,
        )?
        .text();
        assert_eq!(text.lines().next(), Some("abc世"));
        Ok(())
    }

    /// Mixed views preserve wide-cell counts and style runs across wraps,
    /// and trim only blanks whose style the requested view cannot show.
    #[test]
    fn mixed_views_keep_cells_runs_and_visible_blanks() -> Result<(), Box<dyn std::error::Error>> {
        let mut pane = Lone::new(Pane::new(
            PaneId::of(1),
            "sh".into(),
            Runs::DefaultShell,
            fux_vt::Size::new(5, 5)?,
            0,
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
            let got = super::pane(&pane, run, None, view, false, 8 << 20)?;
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
            "sh".into(),
            Runs::DefaultShell,
            fux_vt::Size::new(2, 10)?,
            0,
        )?);
        pane.output("x\u{0600}  ".as_bytes());
        let got = super::pane(
            &pane,
            crate::command::Run::now().ok_or("a clock")?,
            None,
            CaptureView::default(),
            false,
            8 << 20,
        )?;
        assert_eq!(got.text(), "x\u{0600} ");
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
        super::trim_line(&mut lines, 2);
        let got = super::captured(
            super::Of::Client(crate::id::ClientId::of(1)),
            (1, 3),
            None,
            lines,
            CaptureView::default(),
        );
        assert_eq!(got.text(), "x");
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
            "sh".into(),
            Runs::DefaultShell,
            fux_vt::Size::new(6, 5)?,
            0,
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
            "sh".into(),
            Runs::DefaultShell,
            size,
            0,
        )?);
        pane.output(b"\x1b[1;4:3;5;38;2;1;2;3;48;5;9;58;5;2mstyled\x1b[m plain");
        let view = CaptureView {
            styles: true,
            ..CaptureView::default()
        };
        let run = crate::command::Run::now().ok_or("a clock")?;
        let body = crate::protocol::Body::Capture(Box::new(super::pane(
            &pane,
            run,
            None,
            view,
            false,
            8 << 20,
        )?));
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
            "sh".into(),
            Runs::DefaultShell,
            size,
            0,
        )?);
        pane.output(b"$ \x1b]133;C\x07hello\x1b]133;D;0\x07PROMPT> ");
        let text = super::pane(
            &pane,
            crate::command::Run::now().ok_or("a clock")?,
            None,
            view,
            false,
            8 << 20,
        )?
        .text();
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
            8 << 20,
        )?
        .text();
        assert_eq!(text, "step 1");
        Ok(())
    }
}
