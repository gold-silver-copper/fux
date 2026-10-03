//! `transparency`: a program inside fux should look exactly as it does
//! with no fux in between.
//!
//! For each corpus recording, the program's output goes two ways, to two
//! terminals of one kind (the reference engine, Ghostty unless `--engines`
//! says otherwise):
//!
//! 1. **Directly**: to a terminal of the recording's size.
//! 2. **Through fux**: fux as a library, as its server runs it. A
//!    [`Session`] with one workspace, one tab and one pane, and one client
//!    one row taller than the recording, so that the pane, above the bar,
//!    is exactly the recording's size (`Session::pane_area`; the placement
//!    is checked). The output goes into the pane as the server reads it
//!    (`Session::output`, at most 16 KiB a read), the pane's replies are
//!    taken as its program would read them, and the client is painted as
//!    the server paints it (see [`Through::paint`]). The client's terminal
//!    gets what `fux attach` writes to it ([`fux::client::ENTER`]), what the
//!    server sends it outside paints (its queries), and every paint.
//!
//! Then the pane's rectangle of the client's terminal is compared with the
//! direct terminal, field by field (`snapshot::differences`), on what the
//! reference engine can tell (its `Can`) and a screen shows ([`SHOWN`]).
//! Both sides are read by the same engine, so a difference is never the
//! engines splitting on fux's paint: it is a fux bug, the harness's, or
//! fux-vt reading the program's own bytes otherwise than the engine does,
//! where fux paints what fux-vt has. Those are recorded ([`KNOWN`]), each
//! with its reason and the differences it covers.
//!
//! **Where it compares.** After each recording step, and inside a step at
//! each frame boundary of synchronized output (2026): before each BSU and
//! after each ESU. `--chunk N` adds a point every N bytes. At each point
//! the client is painted, and compared unless the pane holds a frame (from
//! BSU to ESU fux shows the screen as it was before the frame, by design);
//! a held frame is compared at its end. At the end of the recording a
//! frame still held is released, as the server does once its timeout
//! passes, and painted.
//!
//! **Other multiplexers** ([`multiplexers`]): the same comparison through a
//! real tmux and zellij, each a server of its own; their differences are
//! scored, not failed.
use crate::corpus::{self, Recording};
use crate::engine::{Can, ENGINES, Engine, Kind, Setup};
use crate::engines::pane;
use crate::snapshot::{self, Diff, Field, Line, Snapshot};
use fux::command::ClientId;
use fux::layout::{PaneId, Placement, Rect};
use fux::render::{self, Grid};
use fux::session::{Outgoing, Session};
use std::fmt::Write as _;
use std::fs::File;
use std::io::Read;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// What a screen shows, the fields compared: each cell (text, width, style,
/// link), the rows where a prompt starts, and the cursor where it shows.
///
/// Left out, as nothing on the screen shows them, and a multiplexer keeps
/// them for the pane rather than passing them on:
/// - soft-wrap flags and a pending wrap: fux places every run it paints,
///   with autowrap off;
/// - the modes but cursor visibility: they say how keys and the mouse are
///   read, which fux does for the pane, and what fux's own client asks of
///   its terminal;
/// - the title, which fux shows in its bar;
/// - reports, which go to the program, not to the screen;
/// - history: fux keeps the pane's own, and paints only the screen.
///
/// And where the cursor is, when neither side shows it (see [`compare`]).
pub const SHOWN: Can = Can {
    widths: true,
    wrapped: false,
    pending_wrap: false,
    fg: true,
    bg: true,
    underline_color: true,
    bold: true,
    dim: true,
    italic: true,
    underline: true,
    blink: true,
    inverse: true,
    hidden: true,
    strikeout: true,
    cursor: true,
    cursor_visible: true,
    autowrap: false,
    origin: false,
    alternate: false,
    application_cursor: false,
    application_keypad: false,
    bracketed_paste: false,
    focus_reporting: false,
    kitty_keyboard_flags: false,
    synchronized_output: false,
    in_band_resize: false,
    link_uri: true,
    link_group: true,
    prompt: true,
    title: false,
    reports: false,
    history: false,
};

/// The most the server reads from a pane at once (its read buffer).
const READ: usize = 16 * 1024;

/// Begin and end synchronized update.
const BSU: &[u8] = b"\x1b[?2026h";
const ESU: &[u8] = b"\x1b[?2026l";

/// Where the cursor of a cropped screen is when it is outside the crop.
const OUTSIDE: (u16, u16) = (u16::MAX, u16::MAX);

/// `rect` of a snapshot, as a snapshot of its own: its rows and cells, the
/// cursor relative to it ([`OUTSIDE`] if it is not in it), and the rest as
/// it was.
pub fn crop(s: &Snapshot, rect: Rect) -> Snapshot {
    let (x, y) = (usize::from(rect.x), usize::from(rect.y));
    let screen = s
        .screen
        .iter()
        .skip(y)
        .take(usize::from(rect.h))
        .map(|line| Line {
            cells: line
                .cells
                .iter()
                .skip(x)
                .take(usize::from(rect.w))
                .cloned()
                .collect(),
            wrapped: line.wrapped,
            prompt: line.prompt,
            unread_from: line.unread_from.map(|from| from.saturating_sub(x)),
        })
        .collect();
    let (cy, cx) = s.cursor;
    let inside = cy >= rect.y
        && cx >= rect.x
        && cy.saturating_sub(rect.y) < rect.h
        && cx.saturating_sub(rect.x) < rect.w;
    let cursor = if inside {
        (cy.saturating_sub(rect.y), cx.saturating_sub(rect.x))
    } else {
        OUTSIDE
    };
    Snapshot {
        rows: rect.h,
        cols: rect.w,
        cursor,
        screen,
        ..s.clone()
    }
}

/// The differences between the screen shown directly and through a
/// multiplexer (already cropped to its pane), on what `can` tells and the
/// screen shows. Where neither side shows the cursor, where it is shows
/// nowhere, and is not compared.
pub fn compare(direct: &Snapshot, through: &Snapshot, can: &Can) -> Vec<Diff> {
    let direct = direct.masked(can).masked(&SHOWN);
    let mut through = through.masked(can).masked(&SHOWN);
    if !direct.cursor_visible && !through.cursor_visible {
        through.cursor = direct.cursor;
    }
    snapshot::differences(&direct, &through)
}

/// Where a step's output is compared: before each BSU, after each ESU,
/// every `chunk` bytes, and at its end; ascending, each once.
pub fn points(bytes: &[u8], chunk: Option<usize>) -> Vec<usize> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while let Some(start) = bytes
        .get(at..)
        .and_then(|rest| rest.iter().position(|&b| b == 0x1b))
        .and_then(|i| i.checked_add(at))
    {
        let rest = bytes.get(start..).unwrap_or_default();
        if rest.starts_with(BSU) {
            out.push(start);
        } else if rest.starts_with(ESU) {
            out.push(start.saturating_add(ESU.len()));
        }
        at = start.saturating_add(1);
    }
    if let Some(chunk) = chunk.filter(|&c| c > 0) {
        let mut next = chunk;
        while next < bytes.len() {
            out.push(next);
            next = next.saturating_add(chunk);
        }
    }
    out.push(bytes.len());
    out.retain(|&p| p > 0 || bytes.is_empty());
    out.sort_unstable();
    out.dedup();
    out
}

/// A terminal of `kind` and size.
fn terminal(kind: &Kind, rows: u16, cols: u16) -> Result<Box<dyn Engine>, String> {
    (kind.make)(&Setup {
        rows,
        cols,
        history: 0,
        reflow: false,
    })
}

/// fux, as its server runs one client showing one pane, and the client's
/// terminal.
pub struct Through {
    session: Session,
    client: ClientId,
    pane: PaneId,
    /// Where the pane is on the client's screen.
    pub rect: Rect,
    /// What the client's terminal shows, once `painted`, and the grid the
    /// next paint is composed into: the server's `shown` and `spare`.
    shown: Grid,
    spare: Grid,
    painted: bool,
    placement: Placement,
    buffer: Vec<u8>,
    terminal: Box<dyn Engine>,
    pub paints: usize,
    pub painted_bytes: usize,
}

impl Through {
    /// A session whose one pane is `rows` by `cols`, shown to one client
    /// whose terminal is a `kind`, painted once, as the server paints a
    /// client that has just attached.
    pub fn new(kind: &Kind, rows: u16, cols: u16) -> Result<Through, String> {
        let mut session = Session::new(
            fux::config::Config::default(),
            PathBuf::from("/nonexistent/fux-vt-compare.sock"),
            false,
        );
        session.start().map_err(|e| format!("fux: {e}"))?;
        let client_rows = rows.checked_add(1).ok_or("too many rows for the bar")?;
        let client = session
            .attach(client_rows, cols, None)
            .map_err(|e| format!("fux: {e}"))?;
        let mut placement = Placement::default();
        let view = session.views.get(&client).ok_or("fux: no view")?;
        session.placement_into(view, &mut placement);
        let (pane, rect) = match placement.panes.as_slice() {
            [(pane, rect)] => (*pane, *rect),
            _ => return Err("fux: not one pane on the client's screen".into()),
        };
        if (rect.h, rect.w) != (rows, cols) {
            return Err(format!(
                "fux: the pane is {}x{}, not {rows}x{cols}",
                rect.h, rect.w
            ));
        }
        let mut terminal = terminal(kind, client_rows, cols)?;
        terminal.process(fux::client::ENTER.as_bytes())?;
        let mut through = Through {
            session,
            client,
            pane,
            rect,
            shown: Grid::new(0, 0),
            spare: Grid::new(0, 0),
            painted: false,
            placement,
            buffer: Vec::new(),
            terminal,
            paints: 0,
            painted_bytes: 0,
        };
        through.paint()?;
        Ok(through)
    }

    /// The client's terminal resized to show a pane of `rows` by `cols`, as
    /// a user resizes the window: the session resizes the client, and so
    /// the pane, the terminal takes the new size, and the next paint is the
    /// server's.
    pub fn resize(&mut self, rows: u16, cols: u16) -> Result<(), String> {
        let client_rows = rows.checked_add(1).ok_or("too many rows for the bar")?;
        self.session.resize(self.client, client_rows, cols);
        self.terminal.resize(client_rows, cols)?;
        let view = self.session.views.get(&self.client).ok_or("fux: no view")?;
        self.session.placement_into(view, &mut self.placement);
        self.rect = match self.placement.panes.as_slice() {
            [(_, rect)] => *rect,
            _ => return Err("fux: not one pane on the client's screen".into()),
        };
        if (self.rect.h, self.rect.w) != (rows, cols) {
            return Err(format!(
                "fux: the pane is {}x{} after a resize, not {rows}x{cols}",
                self.rect.h, self.rect.w
            ));
        }
        Ok(())
    }

    /// The program's output, read into the pane as the server reads it;
    /// its replies are read by the program.
    pub fn output(&mut self, bytes: &[u8]) {
        let mut rest = bytes;
        while !rest.is_empty() {
            let (now, later) = rest
                .split_at_checked(READ.min(rest.len()))
                .unwrap_or((rest, &[]));
            self.session.output(self.pane, now);
            rest = later;
        }
        for pane in self.session.panes.values_mut() {
            pane.input.drain_all();
        }
    }

    /// Whether the pane holds a frame of synchronized output.
    pub fn held(&self) -> bool {
        self.session
            .panes
            .get(&self.pane)
            .is_some_and(|p| p.frame_deadline().is_some())
    }

    /// Releases a held frame, as the server does once its timeout passes.
    pub fn release(&mut self) {
        if let Some(due) = self.session.next_frame_release() {
            self.session.release_frames(due);
        }
    }

    /// One turn of the server's loop for this client: settle, send what is
    /// waiting outside paints, and paint if the client's view is dirty.
    /// The paint is the server's (`Server::paint`): composed into the spare
    /// grid; nothing sent if it is the screen the client shows; else the
    /// difference from the shown grid (all of it on the first paint), and
    /// the grids swap. Paints are not spaced 16 ms apart, as the server
    /// spaces them: one is made at every point compared.
    pub fn paint(&mut self) -> Result<(), String> {
        self.session.settle_if_needed();
        for outgoing in std::mem::take(&mut self.session.outbox) {
            match outgoing {
                Outgoing::Bytes(client, bytes) if client == self.client => {
                    self.terminal.process(&bytes)?;
                }
                Outgoing::Exit(client, reason) if client == self.client => {
                    return Err(format!("fux detached the client: {reason}"));
                }
                Outgoing::Shutdown(reason) => return Err(format!("fux stopped: {reason}")),
                Outgoing::Bytes(..) | Outgoing::Exit(..) => {}
            }
        }
        let dirty = self
            .session
            .views
            .get(&self.client)
            .is_some_and(|v| v.dirty);
        if !dirty
            || !render::compose_into(
                &self.session,
                self.client,
                &mut self.spare,
                &mut self.placement,
            )
        {
            return Ok(());
        }
        if !(self.painted && self.spare == self.shown) {
            self.buffer.clear();
            let shown = self.painted.then_some(&self.shown);
            render::paint_into(shown, &self.spare, &mut self.buffer);
            self.terminal.process(&self.buffer)?;
            self.paints = self.paints.saturating_add(1);
            self.painted_bytes = self.painted_bytes.saturating_add(self.buffer.len());
            std::mem::swap(&mut self.shown, &mut self.spare);
            self.painted = true;
        }
        if let Some(view) = self.session.views.get_mut(&self.client) {
            view.dirty = false;
        }
        Ok(())
    }

    /// The pane's rectangle of the client's terminal.
    pub fn snapshot(&mut self) -> Result<Snapshot, String> {
        Ok(crop(&self.terminal.snapshot(0)?, self.rect))
    }
}

/// A difference recorded, with its reason: where the reference engine
/// reads the program's own bytes otherwise than fux-vt does (the corpus
/// shows it too), so that no paint can make the two sides agree. Only the
/// differences `covers` takes are expected; any other still fails.
pub struct Known {
    pub recordings: Recordings,
    pub engines: &'static [&'static str],
    pub why: &'static str,
    /// Whether this is the difference: the difference, then the direct
    /// screen and the screen through the multiplexer.
    pub covers: fn(&Diff, &Snapshot, &Snapshot) -> bool,
}

/// The recordings a recorded difference is expected in.
pub enum Recordings {
    /// Any: what it covers is narrow enough to be expected anywhere.
    Every,
    Only(&'static [&'static str]),
}

impl Recordings {
    fn contains(&self, recording: &str) -> bool {
        match self {
            Recordings::Every => true,
            Recordings::Only(names) => names.contains(&recording),
        }
    }
}

/// The differences recorded.
pub const KNOWN: &[Known] = &[
    Known {
        recordings: Recordings::Every,
        engines: &["ghostty", "alacritty"],
        why: "a cell erased while a foreground is set (ECH, EL, ED, ICH, DCH, IL, DL, a \
            scroll's new row: tmux pads its status line with CSI 100 X in black on green, \
            neovim, htop, mc, ncdu, ranger and tig clear in their own colours) keeps the \
            pen's foreground in fux-vt, as in xterm (`corpus` agrees beside xterm; fux-vt's \
            README, CSI J / K and CSI @ P X); Ghostty's and alacritty's keep only the \
            background. fux paints the cells as fux-vt has them, so the blanks have that \
            foreground through fux and the default directly; a blank's foreground is not \
            drawn. Only a foreground on a cell blank on both sides is covered. `replay \
            --engines ghostty,alacritty --size 2x10 '\\e[30m\\e[42mab\\e[3Xcd'` shows it.",
        covers: blank_foreground,
    },
    Known {
        recordings: Recordings::Only(&["delta-diff"]),
        engines: &["alacritty", "avt", "wezterm"],
        why: "delta draws its wrap marker in the last column, then sends EL 0 with the wrap \
            pending: xterm, Ghostty, libvterm and fux-vt erase the marker; alacritty, avt and \
            wezterm keep it (`corpus` shows it in its marks). fux paints the row as fux-vt \
            has it, without the marker.",
        covers: erased_at_the_last_column,
    },
];

/// The cell a difference's key names, `cell (Y,X) ...`.
fn cell_of(key: &str) -> Option<(usize, usize)> {
    let (y, rest) = key.strip_prefix("cell (")?.split_once(',')?;
    let (x, _) = rest.split_once(')')?;
    Some((y.parse().ok()?, x.parse().ok()?))
}

fn cell_at(s: &Snapshot, (y, x): (usize, usize)) -> Option<&snapshot::Cell> {
    s.screen.get(y)?.cells.get(x)
}

/// A foreground that differs on a cell blank on both sides.
fn blank_foreground(d: &Diff, direct: &Snapshot, through: &Snapshot) -> bool {
    d.field == Field::Fg
        && cell_of(&d.key).is_some_and(|at| {
            [direct, through]
                .iter()
                .all(|s| cell_at(s, at).is_some_and(|c| c.text.is_empty()))
        })
}

/// Any difference in a cell of the last column that is blank through the
/// multiplexer.
fn erased_at_the_last_column(d: &Diff, direct: &Snapshot, through: &Snapshot) -> bool {
    cell_of(&d.key).is_some_and(|(y, x)| {
        x.checked_add(1) == Some(usize::from(direct.cols))
            && cell_at(through, (y, x)).is_some_and(|c| c.text.is_empty())
    })
}

/// The recorded difference that covers every one of `differences`, if one
/// does.
fn known(
    recording: &str,
    engine: &str,
    differences: &[Diff],
    direct: &Snapshot,
    through: &Snapshot,
) -> Option<&'static Known> {
    KNOWN.iter().find(|k| {
        k.recordings.contains(recording)
            && k.engines.contains(&engine)
            && differences.iter().all(|d| (k.covers)(d, direct, through))
    })
}

/// A point where the two sides differ.
struct First {
    step: usize,
    /// Bytes into the step, and into the recording.
    offset: usize,
    at: usize,
    differences: Vec<Diff>,
    direct: Snapshot,
    through: Snapshot,
}

/// How one recording went through fux beside one engine.
struct Outcome {
    points: usize,
    /// Points not compared: the pane held a frame there.
    held: usize,
    /// Points that differ as recorded, and otherwise.
    recorded: usize,
    differing: usize,
    /// Steps whose end was compared, and of them those identical.
    step_ends: usize,
    step_ends_same: usize,
    /// The first point that differs otherwise than recorded.
    first: Option<First>,
    /// The recorded difference seen, if one was.
    known: Option<&'static Known>,
    paints: usize,
    painted_bytes: usize,
}

fn through_fux(
    kind: &Kind,
    recording: &Recording,
    chunk: Option<usize>,
) -> Result<Outcome, String> {
    let (rows, cols) = (recording.rows, recording.cols);
    let mut direct = terminal(kind, rows, cols)?;
    let mut through = Through::new(kind, rows, cols)?;
    let mut outcome = Outcome {
        points: 0,
        held: 0,
        recorded: 0,
        differing: 0,
        step_ends: 0,
        step_ends_same: 0,
        first: None,
        known: None,
        paints: 0,
        painted_bytes: 0,
    };
    let mut at = 0usize;
    let steps = recording.steps.len();
    for (step, (_, bytes)) in recording.steps.iter().enumerate() {
        // A resize is made on both sides and painted, and not judged until
        // the program has answered it, as the corpus judges it: right after
        // it each side shows its own resize policy (see corpus.rs).
        if let Some((rows, cols)) = recording.resize_at(step) {
            direct.resize(rows, cols)?;
            through.resize(rows, cols)?;
            through.paint()?;
        }
        let mut from = 0usize;
        for point in points(bytes, chunk) {
            let piece = bytes.get(from..point).unwrap_or_default();
            direct.process(piece)?;
            through.output(piece);
            at = at.saturating_add(piece.len());
            from = point;
            let last = step.saturating_add(1) == steps && point == bytes.len();
            if last && through.held() {
                through.release();
            }
            through.paint()?;
            if through.held() {
                outcome.held = outcome.held.saturating_add(1);
                continue;
            }
            let (d, t) = (direct.snapshot(0)?, through.snapshot()?);
            outcome.points = outcome.points.saturating_add(1);
            let differences = compare(&d, &t, &kind.can);
            if point == bytes.len() {
                outcome.step_ends = outcome.step_ends.saturating_add(1);
                if differences.is_empty() {
                    outcome.step_ends_same = outcome.step_ends_same.saturating_add(1);
                }
            }
            if differences.is_empty() {
                continue;
            }
            if let Some(k) = known(&recording.name, kind.name, &differences, &d, &t) {
                outcome.recorded = outcome.recorded.saturating_add(1);
                outcome.known = Some(k);
                continue;
            }
            outcome.differing = outcome.differing.saturating_add(1);
            if outcome.first.is_none() {
                outcome.first = Some(First {
                    step,
                    offset: point,
                    at,
                    differences,
                    direct: d,
                    through: t,
                });
            }
        }
    }
    outcome.paints = through.paints;
    outcome.painted_bytes = through.painted_bytes;
    Ok(outcome)
}

/// What a difference's report shows: the step, its keys, where in the
/// output, the differences, and both screens.
fn report(recording: &Recording, kind: &Kind, first: &First, through: &str) -> String {
    let mut out = String::new();
    let keys = recording
        .steps
        .get(first.step)
        .map_or("", |(k, _)| k.as_str());
    let keys = if first.step == 0 { "(start)" } else { keys };
    let _ = writeln!(
        out,
        "  first at step {} of {} (keys '{keys}'), {} bytes into it ({} into the recording), beside {}",
        first.step,
        recording.steps.len(),
        first.offset,
        first.at,
        kind.name
    );
    for diff in first.differences.iter().take(8) {
        let _ = writeln!(
            out,
            "    {}: directly {}, through {through} {}",
            diff.key, diff.fux, diff.other
        );
    }
    if first.differences.len() > 8 {
        let _ = writeln!(out, "    ... ({} in all)", first.differences.len());
    }
    let shown = snapshot::side_by_side(&first.direct, &first.through, through);
    for (i, line) in shown.lines().enumerate() {
        if i == 0 {
            let width = usize::from(first.direct.cols.max(first.through.cols)).max(6);
            let _ = writeln!(out, "      {:<width$}   through {through}", "directly");
        } else {
            let _ = writeln!(out, "    {line}");
        }
    }
    out
}

/// The options `transparency` takes after its name.
struct Options {
    engines: Option<String>,
    chunk: Option<usize>,
    multiplexers: bool,
    json: Option<PathBuf>,
    /// With `--size`, the words are steps of output, not recordings.
    size: Option<(u16, u16)>,
    words: Vec<String>,
}

fn options(argv: &[String]) -> Result<Options, String> {
    let mut out = Options {
        engines: None,
        chunk: None,
        multiplexers: false,
        json: None,
        size: None,
        words: Vec::new(),
    };
    let mut words = argv.iter();
    while let Some(word) = words.next() {
        let mut value = |name: &str| {
            words
                .next()
                .cloned()
                .ok_or(format!("transparency: {name} needs a value"))
        };
        match word.as_str() {
            "--engines" => out.engines = Some(value("--engines")?),
            "--chunk" => {
                let text = value("--chunk")?;
                out.chunk = Some(text.parse().map_err(|e| format!("--chunk: {e}"))?);
            }
            "--json" => out.json = Some(PathBuf::from(value("--json")?)),
            "--size" => out.size = Some(crate::dimensions(&value("--size")?)?),
            "--multiplexers" => out.multiplexers = true,
            other if other.starts_with("--") => {
                return Err(format!("transparency: unknown option {other}"));
            }
            word => out.words.push(word.to_owned()),
        }
    }
    Ok(out)
}

/// The recordings the options name: those in the corpus (all, if none is
/// named), or with `--size`, one made of the steps given, each output
/// written as `replay` takes it.
fn chosen(options: &Options) -> Result<Vec<Recording>, String> {
    let Some((rows, cols)) = options.size else {
        return corpus::recordings(&options.words);
    };
    let steps = options
        .words
        .iter()
        .map(|word| crate::escape::unescape(word).map(|bytes| (String::new(), bytes)))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(vec![Recording {
        name: "replay".into(),
        program: "replay".into(),
        version: String::new(),
        rows,
        cols,
        steps,
        resizes: Vec::new(),
    }])
}

/// The engines `--engines` names (default Ghostty): they must run in this
/// process.
fn kinds(list: Option<&str>) -> Result<Vec<&'static Kind>, String> {
    let chosen = crate::engines(list.unwrap_or("ghostty"))?;
    chosen
        .iter()
        .map(|&i| {
            let kind = ENGINES.get(i).ok_or("no such engine")?;
            if kind.in_process {
                Ok(kind)
            } else {
                Err(format!(
                    "transparency: {} does not run in this process",
                    kind.name
                ))
            }
        })
        .collect()
}

fn save(path: &Path, value: &serde_json::Value) -> Result<(), String> {
    let mut text = serde_json::to_string_pretty(value).map_err(|e| format!("json: {e}"))?;
    text.push('\n');
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

/// The command that shows a recording's comparison again.
fn replay(options: &Options, kind: &Kind, recording: &Recording) -> String {
    let mut out = format!("fux-vt/compare/run.sh transparency --engines {}", kind.name);
    if let Some(chunk) = options.chunk {
        let _ = write!(out, " --chunk {chunk}");
    }
    match options.size {
        Some((rows, cols)) => {
            let _ = write!(out, " --size {rows}x{cols}");
            for (_, bytes) in &recording.steps {
                let _ = write!(out, " '{}'", crate::escape::escape(bytes));
            }
        }
        None => {
            let _ = write!(out, " {}", recording.name);
        }
    }
    out
}

/// `transparency [--engines LIST] [--chunk N] [--json FILE] [NAME...]`, or
/// `--size RxC STEP...` for output given, or with `--multiplexers`, tmux's
/// and zellij's scores. False if any recording differs through fux
/// otherwise than recorded ([`KNOWN`]).
pub fn run(argv: &[String]) -> Result<bool, String> {
    let options = options(argv)?;
    if options.multiplexers {
        return multiplexers(&options);
    }
    let kinds = kinds(options.engines.as_deref())?;
    let recordings = chosen(&options)?;
    let started = Instant::now();
    let mut ok = true;
    let mut results = Vec::new();
    for kind in &kinds {
        println!(
            "transparency: each recording directly and through fux, read by {}",
            kind.name
        );
        let (mut same, mut recorded, mut points, mut differing) = (0usize, 0usize, 0usize, 0usize);
        for recording in &recordings {
            let outcome = crate::case::guarded(|| through_fux(kind, recording, options.chunk))?;
            points = points.saturating_add(outcome.points);
            differing = differing.saturating_add(outcome.differing);
            let label = format!(
                "{} ({} steps, {} bytes): {} points compared, {} in held frames; {} paints, {} bytes",
                recording.name,
                recording.steps.len(),
                recording.bytes().len(),
                outcome.points,
                outcome.held,
                outcome.paints,
                outcome.painted_bytes
            );
            let mut first_json = serde_json::Value::Null;
            match (&outcome.first, outcome.known) {
                (None, None) => {
                    same = same.saturating_add(1);
                    println!("ok       {label}");
                }
                (None, Some(known)) => {
                    recorded = recorded.saturating_add(1);
                    println!(
                        "differs  {label}; {} differ as recorded: {}",
                        outcome.recorded, known.why
                    );
                }
                (Some(first), _) => {
                    ok = false;
                    println!("DIFFERS  {label}; {} differ", outcome.differing);
                    print!("{}", report(recording, kind, first, "fux"));
                    println!("  replay: {}", replay(&options, kind, recording));
                    first_json = serde_json::json!({
                        "step": first.step,
                        "offset": first.offset,
                        "at": first.at,
                        "differences": first
                            .differences
                            .iter()
                            .map(|d| serde_json::json!({
                                "key": d.key, "directly": d.fux, "through": d.other
                            }))
                            .collect::<Vec<_>>(),
                    });
                }
            }
            results.push(serde_json::json!({
                "engine": kind.name,
                "recording": recording.name,
                "steps": recording.steps.len(),
                "bytes": recording.bytes().len(),
                "points": outcome.points,
                "held": outcome.held,
                "points_differing_as_recorded": outcome.recorded,
                "points_differing": outcome.differing,
                "step_ends": outcome.step_ends,
                "step_ends_identical": outcome.step_ends_same,
                "identical": outcome.first.is_none() && outcome.known.is_none(),
                "recorded": outcome.known.map(|k| k.why),
                "paints": outcome.paints,
                "painted_bytes": outcome.painted_bytes,
                "first_difference": first_json,
            }));
        }
        println!(
            "{same} of {} recordings look the same through fux, beside {}; {recorded} differ as recorded, {} otherwise ({points} points compared, {differing} differ otherwise than recorded)",
            recordings.len(),
            kind.name,
            recordings
                .len()
                .saturating_sub(same)
                .saturating_sub(recorded),
        );
    }
    let seconds = started.elapsed().as_secs_f64();
    println!("({seconds:.1}s)");
    if let Some(path) = &options.json {
        save(
            path,
            &serde_json::json!({
                "check": "transparency",
                "multiplexer": "fux",
                "version": fux::pane::IDENTITY.version,
                "chunk": options.chunk,
                "seconds": seconds,
                "ok": ok,
                "results": results,
            }),
        )?;
    }
    Ok(ok)
}

// ------------------------------------------------------------ multiplexers

/// How long a multiplexer's client must stay quiet after a step for its
/// screen to be taken as drawn, and the longest a step may take.
const QUIET: Duration = Duration::from_millis(200);
const STEP_LIMIT: Duration = Duration::from_secs(10);
/// How long a multiplexer may take to start its pane's program.
const START: Duration = Duration::from_secs(15);

/// A multiplexer compared with fux: a real server of its own, a client on
/// a PTY, the recording replayed into its one pane.
#[derive(Clone, Copy)]
enum Mux {
    /// tmux with no configuration (`-f /dev/null`): its status line under
    /// the pane.
    Tmux,
    /// zellij with a configuration and layout of its own: one pane, no
    /// frames, no bars.
    Zellij,
}

impl Mux {
    fn name(self) -> &'static str {
        match self {
            Mux::Tmux => "tmux",
            Mux::Zellij => "zellij",
        }
    }

    /// The installed version, as it says it.
    fn version(self) -> String {
        let flag = match self {
            Mux::Tmux => "-V",
            Mux::Zellij => "--version",
        };
        Command::new(self.name())
            .arg(flag)
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
            .unwrap_or_default()
    }

    /// The client's rows for a pane of `rows`, and where the pane is: tmux's
    /// status line takes a row under it.
    fn screen(self, rows: u16, cols: u16) -> Result<(u16, Rect), String> {
        let client_rows = match self {
            Mux::Tmux => rows.checked_add(1).ok_or("too many rows")?,
            Mux::Zellij => rows,
        };
        Ok((
            client_rows,
            Rect {
                x: 0,
                y: 0,
                w: cols,
                h: rows,
            },
        ))
    }
}

/// zellij's configuration: no pane frames, no tips, release notes or
/// session saving, no mouse.
const ZELLIJ_CONFIG: &str = "\
pane_frames false
simplified_ui true
show_startup_tips false
show_release_notes false
session_serialization false
mouse_mode false
";

/// A multiplexer's client on a PTY, what it writes read by a thread here
/// into a terminal of the reference engine's kind.
struct Client {
    child: std::process::Child,
    from: mpsc::Receiver<Vec<u8>>,
    terminal: Box<dyn Engine>,
    /// The PTY's master, kept open while the client runs.
    _master: OwnedFd,
    /// The server's files: tmux's socket, zellij's directories.
    dir: PathBuf,
    mux: Mux,
    env: Vec<(String, String)>,
}

impl Client {
    /// Starts `mux` on a new PTY for a pane of `rows` by `cols`, the pane
    /// running the shell line `program`.
    fn start(mux: Mux, kind: &Kind, rows: u16, cols: u16, program: &str) -> Result<Client, String> {
        let dir = pane::fresh(mux.name())?;
        let home = dir.join("home");
        std::fs::create_dir_all(&home).map_err(|e| format!("{}: {e}", home.display()))?;
        let mut env: Vec<(String, String)> = vec![
            // What a modern terminal says it is: Ghostty, kitty, WezTerm
            // and iTerm2 all say COLORTERM=truecolor, from which tmux
            // takes direct colour.
            ("TERM".into(), "xterm-256color".into()),
            ("COLORTERM".into(), "truecolor".into()),
            ("LANG".into(), "en_US.UTF-8".into()),
            ("HOME".into(), home.to_string_lossy().into_owned()),
            (
                "PATH".into(),
                std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into()),
            ),
        ];
        let argv: Vec<String> = match mux {
            Mux::Tmux => vec![
                "tmux".into(),
                "-u".into(),
                "-S".into(),
                dir.join("socket").to_string_lossy().into_owned(),
                "-f".into(),
                "/dev/null".into(),
                "new-session".into(),
                program.into(),
            ],
            Mux::Zellij => {
                // A socket's path is short: zellij adds a directory of its
                // own and the session's name to this one.
                let sockets = PathBuf::from(format!(
                    "/tmp/fux-vt-compare-z{}-{}",
                    std::process::id(),
                    pane::serial()
                ));
                std::fs::create_dir_all(&sockets)
                    .map_err(|e| format!("{}: {e}", sockets.display()))?;
                std::os::unix::fs::symlink(&sockets, dir.join("sockets"))
                    .map_err(|e| format!("{}: {e}", sockets.display()))?;
                env.push((
                    "ZELLIJ_SOCKET_DIR".into(),
                    sockets.to_string_lossy().into_owned(),
                ));
                let config = dir.join("config.kdl");
                std::fs::write(&config, ZELLIJ_CONFIG)
                    .map_err(|e| format!("{}: {e}", config.display()))?;
                let layout = dir.join("layout.kdl");
                let quoted = program.replace('\\', "\\\\").replace('"', "\\\"");
                let text = format!(
                    "layout {{\n    pane command=\"sh\" close_on_exit=true {{\n        args \"-c\" \"{quoted}\"\n    }}\n}}\n"
                );
                std::fs::write(&layout, text).map_err(|e| format!("{}: {e}", layout.display()))?;
                vec![
                    "zellij".into(),
                    "--config".into(),
                    config.to_string_lossy().into_owned(),
                    "--config-dir".into(),
                    dir.join("config").to_string_lossy().into_owned(),
                    "--data-dir".into(),
                    dir.join("data").to_string_lossy().into_owned(),
                    "--session".into(),
                    format!("t{}", pane::serial()),
                    "--new-session-with-layout".into(),
                    layout.to_string_lossy().into_owned(),
                ]
            }
        };
        let (client_rows, _) = mux.screen(rows, cols)?;
        let (master, slave) =
            fuxix::pty::open(client_rows, cols).map_err(|e| format!("opening a PTY: {e}"))?;
        let me = std::env::current_exe().map_err(|e| format!("this program's path: {e}"))?;
        let clone = |fd: &OwnedFd| fd.try_clone().map_err(|e| format!("the PTY: {e}"));
        let mut command = Command::new(me);
        command
            .arg(crate::record::LAUNCH)
            .args(&argv)
            .env_clear()
            .current_dir(&dir)
            .stdin(clone(&slave)?)
            .stdout(clone(&slave)?)
            .stderr(clone(&slave)?);
        for (key, value) in &env {
            command.env(key, value);
        }
        let child = command
            .spawn()
            .map_err(|e| format!("starting {}: {e}", mux.name()))?;
        drop(slave);
        let mut reader = File::from(clone(&master)?);
        let (tx, from) = mpsc::channel();
        std::thread::Builder::new()
            .name("mux-client".into())
            .spawn(move || {
                let mut buf = vec![0u8; 1 << 16];
                while let Ok(n) = reader.read(&mut buf) {
                    if n == 0 || tx.send(buf.get(..n).unwrap_or_default().to_vec()).is_err() {
                        break;
                    }
                }
            })
            .map_err(|e| format!("thread: {e}"))?;
        Ok(Client {
            child,
            from,
            terminal: terminal(kind, client_rows, cols)?,
            _master: master,
            dir,
            mux,
            env,
        })
    }

    /// Reads what the client writes into its terminal until it has been
    /// quiet for `quiet`, or `limit` has passed.
    fn settle(&mut self, quiet: Duration, limit: Duration) -> Result<(), String> {
        let started = Instant::now();
        while started.elapsed() < limit {
            match self.from.recv_timeout(quiet) {
                Ok(bytes) => self.terminal.process(&bytes)?,
                Err(mpsc::RecvTimeoutError::Timeout | mpsc::RecvTimeoutError::Disconnected) => {
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    /// Runs one of the multiplexer's own commands on its server.
    fn command(&self, args: &[&str]) {
        let mut command = Command::new(self.mux.name());
        command
            .env_clear()
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        for (key, value) in &self.env {
            command.env(key, value);
        }
        if let Mux::Tmux = self.mux {
            command.arg("-S").arg(self.dir.join("socket"));
        }
        let _ = command.args(args).status();
    }
}

impl Drop for Client {
    /// Stops the server and the client, and removes the server's files.
    fn drop(&mut self) {
        match self.mux {
            Mux::Tmux => self.command(&["kill-server"]),
            Mux::Zellij => {
                self.command(&["kill-all-sessions", "--yes"]);
                self.command(&["delete-all-sessions", "--yes", "--force"]);
            }
        }
        let deadline = Instant::now().checked_add(Duration::from_secs(2));
        while matches!(self.child.try_wait(), Ok(None))
            && deadline.is_some_and(|d| Instant::now() < d)
        {
            let _ = self.from.recv_timeout(Duration::from_millis(20));
        }
        if let Some(pid) = fuxix::process::Pid::of(&self.child)
            && matches!(self.child.try_wait(), Ok(None))
        {
            let _ = fuxix::process::kill_group(pid, fuxix::process::Signal::Kill);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Ok(target) = std::fs::read_link(self.dir.join("sockets")) {
            let _ = std::fs::remove_dir_all(target);
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// How one recording went through a multiplexer: steps compared at their
/// ends, those identical, and the first that was not.
struct Score {
    steps: usize,
    same: usize,
    first: Option<First>,
}

fn through_mux(mux: Mux, kind: &Kind, recording: &Recording) -> Result<Score, String> {
    let (rows, cols) = (recording.rows, recording.cols);
    let (_, rect) = mux.screen(rows, cols)?;
    let mut direct = terminal(kind, rows, cols)?;
    let (mut replayer, program) = pane::Pane::new()?;
    let mut client = Client::start(mux, kind, rows, cols, &program)?;
    let deadline = Instant::now()
        .checked_add(START)
        .ok_or("a deadline out of range")?;
    loop {
        client.settle(Duration::from_millis(20), Duration::from_millis(100))?;
        if replayer.connect(Duration::from_millis(50))? {
            break;
        }
        if Instant::now() >= deadline {
            return Err(format!("{} did not start its pane", mux.name()));
        }
    }
    client.settle(QUIET, STEP_LIMIT)?;
    let mut score = Score {
        steps: 0,
        same: 0,
        first: None,
    };
    let mut at = 0usize;
    for (step, (_, bytes)) in recording.steps.iter().enumerate() {
        direct.process(bytes)?;
        replayer.output(bytes)?;
        client.settle(QUIET, STEP_LIMIT)?;
        at = at.saturating_add(bytes.len());
        let d = direct.snapshot(0)?;
        let t = crop(&client.terminal.snapshot(0)?, rect);
        let differences = compare(&d, &t, &kind.can);
        score.steps = score.steps.saturating_add(1);
        if differences.is_empty() {
            score.same = score.same.saturating_add(1);
        } else if score.first.is_none() {
            score.first = Some(First {
                step,
                offset: bytes.len(),
                at,
                differences,
                direct: d,
                through: t,
            });
        }
    }
    drop(replayer);
    drop(client);
    Ok(score)
}

/// A share in percent, to one decimal place, as text.
fn percent(part: usize, whole: usize) -> String {
    let tenths = part
        .saturating_mul(1000)
        .checked_div(whole)
        .unwrap_or_default();
    format!(
        "{}.{}",
        tenths.checked_div(10).unwrap_or_default(),
        tenths.checked_rem(10).unwrap_or_default()
    )
}

/// `transparency --multiplexers`: tmux's and zellij's transparency scores
/// (of those installed), beside fux's own at the ends of the same steps. A
/// difference is scored, not failed.
fn multiplexers(options: &Options) -> Result<bool, String> {
    let kinds = kinds(options.engines.as_deref())?;
    // Recordings that resize are left out: a multiplexer's client would
    // have to be resized mid-recording, which this does not yet do.
    let (recordings, resizing): (Vec<Recording>, Vec<Recording>) = chosen(options)?
        .into_iter()
        .partition(|r| r.resizes.is_empty());
    if !resizing.is_empty() {
        println!(
            "({} recordings that resize are left out of the multiplexer scores)",
            resizing.len()
        );
    }
    let mut results = Vec::new();
    for kind in &kinds {
        println!(
            "transparency through each multiplexer, at the end of each step, read by {}",
            kind.name
        );
        // fux's own score, in process, at the same points.
        let started = Instant::now();
        let (mut same, mut steps, mut steps_same) = (0usize, 0usize, 0usize);
        for recording in &recordings {
            let outcome = crate::case::guarded(|| through_fux(kind, recording, None))?;
            steps = steps.saturating_add(outcome.step_ends);
            steps_same = steps_same.saturating_add(outcome.step_ends_same);
            if outcome.step_ends == outcome.step_ends_same {
                same = same.saturating_add(1);
            }
        }
        let seconds = started.elapsed().as_secs_f64();
        println!(
            "fux {} (in process): {same} of {} recordings identical ({}%), {steps_same} of {steps} steps ({}%), {seconds:.1}s",
            fux::pane::IDENTITY.version,
            recordings.len(),
            percent(same, recordings.len()),
            percent(steps_same, steps),
        );
        results.push(serde_json::json!({
            "engine": kind.name,
            "multiplexer": "fux",
            "version": fux::pane::IDENTITY.version,
            "recordings": recordings.len(),
            "recordings_identical": same,
            "steps": steps,
            "steps_identical": steps_same,
            "seconds": seconds,
        }));
        for mux in [Mux::Tmux, Mux::Zellij] {
            if let Err(why) = pane::on_path(mux.name()) {
                println!("{}: skipped ({why})", mux.name());
                results.push(serde_json::json!({
                    "engine": kind.name, "multiplexer": mux.name(), "skipped": why,
                }));
                continue;
            }
            let version = mux.version();
            let started = Instant::now();
            let (mut same, mut steps, mut steps_same) = (0usize, 0usize, 0usize);
            let mut each = Vec::new();
            for recording in &recordings {
                let score = through_mux(mux, kind, recording)?;
                steps = steps.saturating_add(score.steps);
                steps_same = steps_same.saturating_add(score.same);
                let first_json = match &score.first {
                    None => {
                        same = same.saturating_add(1);
                        println!(
                            "  identical  {}: {} of {} steps",
                            recording.name, score.same, score.steps
                        );
                        serde_json::Value::Null
                    }
                    Some(first) => {
                        println!(
                            "  differs    {}: {} of {} steps identical",
                            recording.name, score.same, score.steps
                        );
                        print!("{}", report(recording, kind, first, mux.name()));
                        serde_json::json!({
                            "step": first.step,
                            "differences": first.differences.iter().map(|d| serde_json::json!({
                                "key": d.key, "directly": d.fux, "through": d.other
                            })).collect::<Vec<_>>(),
                        })
                    }
                };
                each.push(serde_json::json!({
                    "recording": recording.name,
                    "steps": score.steps,
                    "steps_identical": score.same,
                    "identical": score.first.is_none(),
                    "first_difference": first_json,
                }));
            }
            let seconds = started.elapsed().as_secs_f64();
            println!(
                "{version}: {same} of {} recordings identical ({}%), {steps_same} of {steps} steps ({}%), {seconds:.1}s",
                recordings.len(),
                percent(same, recordings.len()),
                percent(steps_same, steps),
            );
            results.push(serde_json::json!({
                "engine": kind.name,
                "multiplexer": mux.name(),
                "version": version,
                "recordings": recordings.len(),
                "recordings_identical": same,
                "steps": steps,
                "steps_identical": steps_same,
                "seconds": seconds,
                "each": each,
            }));
        }
    }
    pane::tidy();
    if let Some(path) = &options.json {
        save(
            path,
            &serde_json::json!({
                "check": "transparency-multiplexers",
                "results": results,
            }),
        )?;
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::{Through, compare, crop, points};
    use crate::engine::{ENGINES, Kind};
    use crate::snapshot::{Cell, Diff, Field, Line, Snapshot, Style, Width};
    use fux::layout::Rect;

    fn ghostty() -> Result<&'static Kind, String> {
        crate::engine::find("ghostty")
            .and_then(|i| ENGINES.get(i))
            .ok_or_else(|| "no ghostty".to_owned())
    }

    /// The differences after `bytes` go to a terminal directly and through
    /// fux, each read by Ghostty.
    fn differences(rows: u16, cols: u16, bytes: &[u8]) -> Result<Vec<Diff>, String> {
        let kind = ghostty()?;
        let mut direct = super::terminal(kind, rows, cols)?;
        let mut through = Through::new(kind, rows, cols)?;
        direct.process(bytes)?;
        through.output(bytes);
        through.paint()?;
        Ok(compare(
            &direct.snapshot(0)?,
            &through.snapshot()?,
            &kind.can,
        ))
    }

    #[test]
    fn points_fall_at_frame_boundaries_every_chunk_and_the_end() {
        let bytes = b"ab\x1b[?2026hcd\x1b[?2026lef";
        assert_eq!(points(bytes, None), [2, 20, 22]);
        assert_eq!(points(bytes, Some(5)), [2, 5, 10, 15, 20, 22]);
        assert_eq!(points(b"", None), [0]);
        assert_eq!(points(b"\x1b[?2026h", None), [8]);
    }

    fn line(text: &str) -> Line {
        Line {
            cells: text
                .chars()
                .map(|c| Cell::new(&c.to_string(), Width::Narrow, Style::default()))
                .collect(),
            wrapped: false,
            prompt: false,
            unread_from: None,
        }
    }

    #[test]
    fn a_crop_is_the_rectangle_with_the_cursor_in_it() {
        let screen = Snapshot {
            rows: 3,
            cols: 4,
            cursor: (2, 3),
            cursor_visible: true,
            screen: vec![line("abcd"), line("efgh"), line("ijkl")],
            ..Snapshot::default()
        };
        let rect = |x, y, w, h| Rect { x, y, w, h };
        let inner = crop(&screen, rect(1, 1, 3, 2));
        assert_eq!((inner.rows, inner.cols, inner.cursor), (2, 3, (1, 2)));
        let texts: Vec<String> = inner.screen.iter().map(Line::text).collect();
        assert_eq!(texts, ["fgh", "jkl"]);
        assert_eq!(crop(&screen, rect(0, 0, 4, 2)).cursor, super::OUTSIDE);
    }

    /// Where a hidden cursor is shows nowhere; a cursor that shows on one
    /// side only, or in two places, differs.
    #[test]
    fn the_cursor_is_compared_where_it_shows() -> Result<(), String> {
        let can = ghostty()?.can;
        let at = |cursor, cursor_visible| Snapshot {
            rows: 1,
            cols: 4,
            cursor,
            cursor_visible,
            screen: vec![line("ab")],
            ..Snapshot::default()
        };
        assert_eq!(compare(&at((0, 1), false), &at((0, 3), false), &can), []);
        let fields =
            |a, b| -> Vec<Field> { compare(&a, &b, &can).into_iter().map(|d| d.field).collect() };
        assert_eq!(fields(at((0, 1), true), at((0, 3), true)), [Field::Cursor]);
        assert_eq!(
            fields(at((0, 1), true), at((0, 1), false)),
            [Field::CursorVisible]
        );
        Ok(())
    }

    /// Combining marks on a glyph a column short of the edge stay on it
    /// through fux (less-small): with autowrap off, Ghostty put them on the
    /// last column's cell, where fux had painted a space.
    #[test]
    fn marks_short_of_the_edge_stay_on_their_glyph() -> Result<(), String> {
        let marked = "xxxxxxxx\u{e4}\u{356}\u{32d}\u{308}\u{307}";
        assert_eq!(differences(3, 10, marked.as_bytes())?, []);
        Ok(())
    }

    /// An emoji modifier a program places after its emoji with a cursor
    /// move of its own (micro-small) is joined to the emoji by Ghostty both
    /// ways, and what follows stays where the program put it.
    #[test]
    fn a_modifier_placed_after_its_emoji_moves_nothing() -> Result<(), String> {
        let placed = "\x1b[1;3H\u{1F44D}\x1b[1;5H\u{1F3FD}\x1b[1;7Hx \u{1F469}";
        assert_eq!(differences(2, 12, placed.as_bytes())?, []);
        Ok(())
    }

    /// Text, styles, a wide glyph, a link and the cursor look the same
    /// through fux; and a difference is found where there is one.
    #[test]
    fn a_screen_looks_the_same_through_fux() -> Result<(), String> {
        let shown = b"a\x1b[1;4;38;2;1;2;3mb\x1b[m\xe6\xbc\xa2\x1b]8;;http://x\x1b\\link\x1b]8;;\x1b\\\r\n\x1b[44mz";
        assert_eq!(differences(3, 10, shown)?, []);
        let kind = ghostty()?;
        let mut direct = super::terminal(kind, 3, 10)?;
        let mut through = Through::new(kind, 3, 10)?;
        direct.process(b"a")?;
        through.output(b"b");
        through.paint()?;
        let found = compare(&direct.snapshot(0)?, &through.snapshot()?, &kind.can);
        assert_eq!(
            found.first().map(|d| (d.key.as_str(), d.field)),
            Some(("cell (0,0) text", Field::Text))
        );
        Ok(())
    }

    /// A frame of synchronized output is shown only once it ends, or when
    /// it is released.
    #[test]
    fn a_frame_shows_at_its_end() -> Result<(), String> {
        let kind = ghostty()?;
        let mut through = Through::new(kind, 2, 6)?;
        let text = |through: &mut Through| -> Result<String, String> {
            Ok(through
                .snapshot()?
                .screen
                .first()
                .map(Line::text)
                .unwrap_or_default())
        };
        through.output(b"a\x1b[?2026hb");
        through.paint()?;
        assert!(through.held());
        assert_eq!(text(&mut through)?, "a");
        through.output(b"c\x1b[?2026l");
        through.paint()?;
        assert!(!through.held());
        assert_eq!(text(&mut through)?, "abc");
        through.output(b"\x1b[?2026hd");
        through.release();
        through.paint()?;
        assert_eq!(text(&mut through)?, "abcd");
        Ok(())
    }

    #[test]
    fn a_recorded_difference_covers_only_its_own() -> Result<(), String> {
        assert_eq!(super::cell_of("cell (12,119) text"), Some((12, 119)));
        assert_eq!(super::cell_of("cursor"), None);
        let blank = |text: &str| Snapshot {
            rows: 1,
            cols: 3,
            screen: vec![line(text)],
            ..Snapshot::default()
        };
        let fg = |x: usize| Diff {
            key: format!("cell (0,{x}) fg"),
            field: Field::Fg,
            styled_cell: Some((0, x)),
            fux: "Default".into(),
            other: "Idx(0)".into(),
        };
        let (a, b) = (blank("x  "), blank("x  "));
        assert!(super::blank_foreground(&fg(1), &a, &b));
        assert!(!super::blank_foreground(&fg(0), &a, &b));
        // A blank's foreground is expected in any recording, beside the
        // engines that keep only the background, and with nothing else.
        assert!(super::known("nvim", "ghostty", &[fg(1)], &a, &b).is_some());
        assert!(super::known("nvim", "libvterm", &[fg(1)], &a, &b).is_none());
        assert!(super::known("nvim", "ghostty", &[fg(1), fg(0)], &a, &b).is_none());
        assert!(super::erased_at_the_last_column(&fg(2), &a, &b));
        assert!(!super::erased_at_the_last_column(&fg(1), &a, &b));
        Ok(())
    }

    /// Every recording looks the same through fux as directly, beside
    /// Ghostty, but as recorded.
    #[test]
    fn every_recording_is_transparent() -> Result<(), String> {
        let kind = ghostty()?;
        for recording in &crate::corpus::recordings(&[])? {
            let outcome = super::through_fux(kind, recording, None)?;
            assert!(outcome.points > 0, "{}", recording.name);
            if let Some(first) = &outcome.first {
                let shown: Vec<String> = first.differences.iter().map(|d| d.line("fux")).collect();
                return Err(format!("{} differs: {shown:?}", recording.name));
            }
        }
        Ok(())
    }

    #[test]
    fn shares_are_in_tenths_of_a_percent() {
        assert_eq!(super::percent(1, 3), "33.3");
        assert_eq!(super::percent(2, 2), "100.0");
        assert_eq!(super::percent(0, 0), "0.0");
    }
}
