//! The workloads: fixed work, each in this process and repeated to a fixed
//! size, so that a run of one is the same instructions every time.
//!
//! - `vt/NAME`: fux-vt alone, set up as fux sets up a pane
//!   (`fux::pane::OPTIONS`), fed in 4 KiB pieces as `compare`'s `bench`
//!   feeds it: the synthetic workloads on a 50×200 screen, each recording
//!   at its own size (`vt/corpus:NAME`), and every recording in turn
//!   (`vt/corpus`).
//! - `vt/vtebench:NAME`, `vt/kitty:NAME`, `vt/avt:NAME`,
//!   `vt/foot:alt-random-writes`, `vt/yes:crlf`, `vt/yes:lf`,
//!   `vt/sixel:frames`: fux-vt alone on each of the census's speed
//!   payloads (`census`), 8 MiB at 50×200, each alone.
//! - `pane/NAME`: the same through `fux::pane::Pane::output`, 16 KiB at a
//!   time, as fux's server gives a pane what it reads.
//! - `paint/corpus`: a session (`fux::session::Session`, without
//!   processes) whose client shows one pane at each recording's size,
//!   given every recording; after each read, the client's screen composed
//!   (`render::compose_into`) and painted (`render::paint_into`) as the
//!   server does. `paint/split`: the same into two panes side by side.
//! - `session/keystroke`: a key typed at a client whose pane shows a full
//!   screen, as the server takes it: decoded and given to the pane
//!   (`Session::input`), the pane's echo read (`Session::output`), then the
//!   client painted (`before_paint`, `compose_into`, `paint_into`): the
//!   server's work for one keystroke, over and over.
//! - `decode/legacy`, `decode/kitty`: fux's decoder of a client's
//!   terminal (`fux::decode::Decoder`) on the keys typed in the recordings,
//!   as a legacy terminal sends them and as a kitty-protocol terminal does
//!   with the flags fux asks of it (5).
//! - `encode/legacy`, `encode/kitty`: those keys encoded for a pane
//!   (`fux::encode::key_bytes`), in legacy mode and in every kitty flag.
//! - `wait/text`: waits for a text that never comes (`fux::wait::Waiting`),
//!   8 armed one after another, on four panes of a session, each asked for
//!   its answer and when it is due
//!   after each 64 KiB tick (in 16 KiB reads) of the recordings: the server's
//!   look at each output while a wait is armed.
//! - `ready/probe`: what a pane's program is blocked in
//!   (`fuxix::process::activity`, which `fux::ready` asks while a wait
//!   is armed), of a sleeping child, over and over.
//! - `script/compile`: fuxth's checker and compiler on command lines an
//!   agent sends (`fux::script::compile`), over and over: what every
//!   `fux WORDS` does before it runs.
//!
//! Each workload has a baseline: the same run without the part measured.
//! For most that is making the bytes; for `paint/*` it is also feeding the
//! session, so the difference is composition and paint alone.
use crate::census;
use crate::corpus::{self, Recording};
use crate::synthetic;
use std::hint::black_box;
use std::os::unix::process::CommandExt;
use std::path::Path;

/// fux-vt is fed this much at a time, as `compare`'s `bench` feeds it.
const CHUNK: usize = 4096;
/// fux's server reads up to this much of a pane's output a tick
/// (`PANE_READ`, src/server.rs) ...
const PANE_READ: usize = 64 * 1024;
/// ... and gives it to the pane this much at a time, its read buffer
/// (`read_pane`).
const PANE_PIECE: usize = 16 * 1024;
/// Each synthetic workload's size.
const SYNTHETIC: usize = 16 << 20;
/// Each recording alone, over and over to this size.
const RECORDING: usize = 4 << 20;
/// Every recording in turn, over and over to this size.
const CORPUS: usize = 4 << 20;
/// The keys typed in the recordings, over and over to this many bytes.
const KEYS: usize = 1 << 20;
/// Keystrokes through a session, this many.
const KEYSTROKES: usize = 20_000;
/// Keys encoded, over and over to this many.
const STROKES: usize = 1 << 20;
/// Bytes read into the panes of `wait/text`, over and over to this many.
const WAITED: usize = 4 << 20;
/// Activity probes, this many.
const PROBES: usize = 2_000;
/// Command lines compiled, this many.
const COMPILES: usize = 2_000;
/// Command lines an agent sends, as `docs/agents.md` shows them.
const LINES: [&str; 4] = [
    "opts str app name str './app --fast' command run",
    "%2 str y text str Enter key send-keys",
    "%2 str 'listening on' wait-text wait",
    "define twice dup add end list 1 2 3 end each twice end len drop %2 all-history capture",
];
/// The bytes of a pane's history fux keeps by default.
const HISTORY: usize = 8 << 20;
/// The kitty flags fux pushes on a client's terminal (`outer`):
/// disambiguate and alternate keys.
const KITTY_CLIENT: u8 = 5;
/// Every kitty flag a pane's program can ask for.
const KITTY_ALL: u8 = 31;

/// A workload's name and what it is.
pub struct Spec {
    pub name: String,
    pub about: String,
}

/// Every workload, for the recordings in `recordings`.
pub fn list(recordings: &[Recording]) -> Vec<Spec> {
    let spec = |name: String, about: String| Spec { name, about };
    let mut out = Vec::new();
    for layer in ["vt", "pane"] {
        for (name, about, _) in synthetic::GENERATORS {
            out.push(spec(
                format!("{layer}/{name}"),
                format!("{about}, {} MiB at 50x200", SYNTHETIC >> 20),
            ));
        }
        out.push(spec(
            format!("{layer}/corpus"),
            format!(
                "every recording in turn, each at its size, {} MiB",
                CORPUS >> 20
            ),
        ));
    }
    for r in recordings {
        out.push(spec(
            format!("vt/corpus:{}", r.name),
            format!(
                "{} bytes recorded at {}x{}, over and over to {} MiB",
                r.bytes().len(),
                r.rows,
                r.cols,
                RECORDING >> 20
            ),
        ));
    }
    for p in census::PAYLOADS {
        out.push(spec(
            format!("vt/{}", p.name),
            format!(
                "{}, {} MiB at {}x{}",
                p.about,
                census::SIZE >> 20,
                census::ROWS,
                census::COLS
            ),
        ));
    }
    out.push(spec(
        "paint/corpus".into(),
        format!(
            "compose and paint after each read of every recording, {} MiB, one pane",
            CORPUS >> 20
        ),
    ));
    out.push(spec(
        "paint/split".into(),
        format!(
            "the same into two panes side by side, {} MiB each",
            CORPUS >> 20
        ),
    ));
    out.push(spec(
        "session/keystroke".into(),
        format!(
            "{KEYSTROKES} keys typed at a client, each echoed by its pane and painted, at 40x120"
        ),
    ));
    for (name, about) in [
        (
            "decode/legacy",
            "the recordings' keys, as a legacy terminal sends them",
        ),
        (
            "decode/kitty",
            "the same keys as a kitty-protocol terminal sends them",
        ),
        (
            "encode/legacy",
            "the same encoded for a pane in legacy mode",
        ),
        (
            "encode/kitty",
            "the same encoded for a pane with every kitty flag",
        ),
        (
            "wait/text",
            "a wait on four panes asked after each read of every recording, 4 MiB",
        ),
        (
            "ready/probe",
            "what a sleeping child is blocked in, 2000 times: the user part (instructions:u), most of a probe being the kernel's",
        ),
        (
            "script/compile",
            "fuxth's checker and compiler on four agent command lines, 2000 times",
        ),
    ] {
        out.push(spec(name.into(), about.into()));
    }
    out
}

/// How a workload is fed, for its fairness record: the bytes a call
/// gives the code measured.
pub fn fed(name: &str) -> &'static str {
    if name.starts_with("vt/") {
        "fux-vt::Parser::process_with, 4096 bytes a call"
    } else if name.starts_with("pane/") {
        "fux::pane::Pane::output, 16384 bytes a call"
    } else if name.starts_with("paint/") || name == "wait/text" {
        "fux::session::Session::output, 16384 bytes a call, 65536 a tick"
    } else {
        "not a stream of bytes"
    }
}

/// What a run did, to print: it also keeps the work from being optimized
/// away.
pub struct Done {
    /// Bytes (or keys) the measured part went through.
    pub units: usize,
    /// For a paint workload: frames painted and bytes sent to the client.
    pub frames: usize,
    pub painted: usize,
}

/// Runs the named workload once, or its baseline, on the recordings in
/// `dir`.
pub fn run(name: &str, dir: &Path, payloads: &Path, baseline: bool) -> Result<Done, String> {
    run_shrunk(name, dir, payloads, baseline, 1)
}

/// [`run`], every size divided by `shrink`: for the tests, in a debug
/// build.
fn run_shrunk(
    name: &str,
    dir: &Path,
    payloads: &Path,
    baseline: bool,
    shrink: usize,
) -> Result<Done, String> {
    let size = |n: usize| n.checked_div(shrink).unwrap_or(n);
    // A census payload needs no recordings: reading them is most of a
    // light workload's baseline, and of its noise.
    if let Some(bytes) = name
        .strip_prefix("vt/")
        .and_then(|rest| census::make(rest, payloads, size(census::SIZE)))
    {
        let load = Load {
            rows: census::ROWS,
            cols: census::COLS,
            bytes: bytes?,
        };
        return vt(&[load], baseline);
    }
    let recordings = corpus::recordings(dir)?;
    if let Some(rest) = name.strip_prefix("vt/") {
        let loads = loads(rest, &recordings, size(RECORDING), shrink)?;
        return vt(&loads, baseline);
    }
    if let Some(rest) = name.strip_prefix("pane/") {
        let loads = loads(rest, &recordings, 0, shrink)?;
        return pane(&loads, baseline);
    }
    match name {
        "paint/corpus" => paint(&recordings, false, baseline, size(CORPUS)),
        "paint/split" => paint(&recordings, true, baseline, size(CORPUS)),
        "session/keystroke" => keystroke(&recordings, baseline, size(KEYSTROKES)),
        "decode/legacy" => decode(&keys(&recordings, None), baseline, size(KEYS)),
        "decode/kitty" => decode(
            &keys(&recordings, Some(kitty_mode(KITTY_CLIENT)?)),
            baseline,
            size(KEYS),
        ),
        "encode/legacy" => encode(
            &recordings,
            fux::encode::KeyMode::Legacy {
                application: false,
                keypad: false,
                other_keys: None,
                backarrow: false,
            },
            baseline,
            size(STROKES),
        ),
        "encode/kitty" => encode(&recordings, kitty_mode(KITTY_ALL)?, baseline, size(STROKES)),
        "wait/text" => wait(&recordings, baseline, size(WAITED)),
        "ready/probe" => ready(baseline, size(PROBES)),
        "script/compile" => compile(baseline, size(COMPILES)),
        _ => Err(format!("no workload {name:?}; `list` lists them")),
    }
}

/// Bytes for a screen of `rows` by `cols`.
struct Load {
    rows: u16,
    cols: u16,
    bytes: Vec<u8>,
}

/// `bytes` over and over, whole, until there are at least `total`.
fn repeated(bytes: &[u8], total: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(total.saturating_add(bytes.len()));
    while !bytes.is_empty() && out.len() < total {
        out.extend_from_slice(bytes);
    }
    out
}

/// The loads of a `vt/` or `pane/` workload: a synthetic one, every
/// recording in turn, or one recording (if `each` is not 0, to that size),
/// the first two divided by `shrink`.
fn loads(
    name: &str,
    recordings: &[Recording],
    each: usize,
    shrink: usize,
) -> Result<Vec<Load>, String> {
    let size = |n: usize| n.checked_div(shrink).unwrap_or(n);
    if let Some(bytes) = synthetic::make(name, size(SYNTHETIC)) {
        return Ok(vec![Load {
            rows: synthetic::ROWS,
            cols: synthetic::COLS,
            bytes,
        }]);
    }
    if name == "corpus" {
        let total: usize = recordings.iter().map(|r| r.bytes().len()).sum();
        let mut out = Vec::new();
        let mut made = 0usize;
        while total > 0 && made < size(CORPUS) {
            // Each at the size it was written at, its resizes kept.
            for (rows, cols, bytes) in recordings.iter().flat_map(Recording::segments) {
                made = made.saturating_add(bytes.len());
                out.push(Load { rows, cols, bytes });
            }
        }
        return Ok(out);
    }
    if let Some(one) = name.strip_prefix("corpus:")
        && each > 0
        && let Some(r) = recordings.iter().find(|r| r.name == one)
    {
        // Over and over, whole, its resizes with it, to `each` bytes.
        let segments = r.segments();
        let recorded: usize = segments.iter().map(|(_, _, b)| b.len()).sum();
        let (mut out, mut made) = (Vec::new(), 0usize);
        while made < each && recorded > 0 {
            for (rows, cols, bytes) in &segments {
                made = made.saturating_add(bytes.len());
                out.push(Load {
                    rows: *rows,
                    cols: *cols,
                    bytes: bytes.clone(),
                });
            }
        }
        return Ok(out);
    }
    Err(format!("no workload {name:?}; `list` lists them"))
}

/// Replies and events, kept from being optimized away.
struct Sink;

impl fux_vt::Sink for Sink {
    fn reply(&mut self, bytes: &[u8]) {
        black_box(bytes);
    }
}

/// `bytes` in pieces of `size`, the last one shorter.
fn pieces(bytes: &[u8], size: usize) -> impl Iterator<Item = &[u8]> {
    let mut rest = bytes;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let (piece, tail) = rest.split_at_checked(size).unwrap_or((rest, &[]));
        rest = tail;
        Some(piece)
    })
}

/// The parser fed `loads`, each at its size. A size or a parser that
/// can't be made fails the run: it would measure nothing. What the parser
/// refuses as it reads (an allocation past its limits) is the workload's,
/// as fux's pane takes it, and goes on.
fn vt(loads: &[Load], baseline: bool) -> Result<Done, String> {
    let units = loads.iter().map(|l| l.bytes.len()).sum();
    if baseline {
        black_box(loads);
        return Ok(Done {
            units,
            frames: 0,
            painted: 0,
        });
    }
    let mut parser: Option<fux_vt::Parser> = None;
    for load in loads {
        let size = fux_vt::Size::new(load.rows, load.cols)
            .map_err(|e| format!("{}x{}: {e}", load.rows, load.cols))?;
        let p = match &mut parser {
            Some(p) => {
                p.resize(size)
                    .map_err(|e| format!("resize to {size:?}: {e}"))?;
                p
            }
            None => parser.insert(
                fux_vt::Parser::with_options(size, HISTORY, fux::pane::OPTIONS)
                    .map_err(|e| format!("a parser of {size:?}: {e}"))?,
            ),
        };
        for piece in pieces(&load.bytes, CHUNK) {
            let _ = p.process_with(black_box(piece), &mut Sink);
        }
    }
    black_box(parser.as_ref().map(|p| p.screen().cursor_position()));
    Ok(Done {
        units,
        frames: 0,
        painted: 0,
    })
}

fn pane(loads: &[Load], baseline: bool) -> Result<Done, String> {
    let units = loads.iter().map(|l| l.bytes.len()).sum();
    let first = loads.first().ok_or("no bytes")?;
    let mut pane = fux::pane::Pane::new(
        "%1".parse::<fux::id::PaneId>().map_err(|e| e.to_string())?,
        "bench".into(),
        fux::pane::Runs::Shell,
        fux::pane::size(first.rows, first.cols),
        HISTORY,
    )
    .map_err(|e| e.to_string())?;
    if baseline {
        black_box(loads);
        black_box(&pane);
        return Ok(Done {
            units,
            frames: 0,
            painted: 0,
        });
    }
    for load in loads {
        pane.resize(fux::pane::size(load.rows, load.cols));
        for piece in pieces(&load.bytes, PANE_PIECE) {
            pane.output(black_box(piece));
            // What a call's budget left is read on the pane's next turns.
            while pane.has_unread() {
                pane.read_unread();
            }
        }
    }
    black_box(pane.screen().cursor_position());
    Ok(Done {
        units,
        frames: 0,
        painted: 0,
    })
}

/// Every recording in turn to the client of a session, as the server would
/// give it, composed and painted after each read unless `baseline`.
/// `session/keystroke`: a session whose client shows one 40x120 pane full
/// of a recording's last screen; then `keys` keys typed at it, each given
/// to the pane, the pane's echo of it read, and the client painted, as the
/// server does for a keystroke. The baseline types nothing and paints
/// nothing: the run is the server's whole work for the keys.
fn keystroke(recordings: &[Recording], baseline: bool, keys: usize) -> Result<Done, String> {
    let config = fux::config::Config {
        history_bytes: HISTORY,
        ..fux::config::Config::default()
    };
    let mut s = fux::session::Session::new(config, "/nonexistent/fux.sock".into(), false);
    s.start().map_err(|e| e.to_string())?;
    let c = s
        .attach_as(fux::view::ClientKind::Terminal, 41, 120, None)
        .map_err(|e| e.to_string())?;
    let pane = s.panes.keys().next().copied().ok_or("no pane")?;
    // A full screen, as a pane at work shows: the recordings' output.
    for r in recordings.iter().take(8) {
        for (_, output) in &r.steps {
            s.output(pane, output);
            unread(&mut s);
        }
    }
    s.hold_copies();
    let mut shown = fux::render::Grid::new(0, 0);
    let mut spare = fux::render::Grid::new(0, 0);
    let mut placement = fux::layout::Placement::default();
    let mut buffer = Vec::new();
    let mut have = false;
    let (mut painted, mut frames) = (0usize, 0usize);
    let mut echo = [0u8; 4];
    for i in 0..keys {
        if baseline {
            black_box(i);
            continue;
        }
        let key = b'a'.saturating_add(u8::try_from(i % 26).unwrap_or(0));
        // As the client sends a key its bindings leave for the pane.
        let press = fux::keys::KeyPress::char(char::from(key));
        s.acts(c, vec![fux::act::Act::Key(press.into())]);
        // What the PTY would carry to the program, and its echo back.
        let typed = s
            .panes
            .get_mut(&pane)
            .map(|p| p.input.drain_all())
            .unwrap_or_default();
        black_box(&typed);
        let glyph = char::from(key).encode_utf8(&mut echo);
        s.output(pane, glyph.as_bytes());
        s.hold_copies();
        buffer.clear();
        buffer.extend_from_slice(&s.before_paint(c));
        if let Some(view) = s.views.get_mut(&c) {
            view.dirty = false;
        }
        let Some(view) = s.views.get(&c) else {
            continue;
        };
        fux::render::compose_view(&s, view, &mut spare, &mut placement);
        // Every key changes the screen: painted without asking whether it
        // did, as `paint_into` finds what changed.
        fux::render::paint_into(&shown, have, &spare, &mut buffer);
        painted = painted.saturating_add(buffer.len());
        frames = frames.saturating_add(1);
        black_box(&buffer);
        std::mem::swap(&mut shown, &mut spare);
        have = true;
    }
    Ok(Done {
        units: keys,
        frames,
        painted,
    })
}

fn paint(
    recordings: &[Recording],
    split: bool,
    baseline: bool,
    total: usize,
) -> Result<Done, String> {
    let config = fux::config::Config {
        history_bytes: HISTORY,
        ..fux::config::Config::default()
    };
    let mut s = fux::session::Session::new(config, "/nonexistent/fux.sock".into(), false);
    s.start().map_err(|e| e.to_string())?;
    let first = recordings.first().ok_or("no recordings")?;
    // The bar takes a row: the pane is the recording's size.
    let c = s
        .attach_as(
            fux::view::ClientKind::Terminal,
            first.rows.saturating_add(1),
            first.cols,
            None,
        )
        .map_err(|e| e.to_string())?;
    if split {
        let split = fux::command::Command::Split {
            axis: fux::layout::Axis::Horizontal,
            target: *s.panes.keys().next().ok_or("no pane")?,
            cmd: Vec::new(),
            cwd: None,
            env: fux::command::Env::default(),
            run: false,
            id: None,
        };
        s.run_command(
            &split,
            &fux::session::Origin::Cli(fux::session::Caller::default()),
        );
    }
    let panes: Vec<_> = s.panes.keys().copied().collect();
    let mut shown = fux::render::Grid::new(0, 0);
    let mut spare = fux::render::Grid::new(0, 0);
    let mut placement = fux::layout::Placement::default();
    let mut buffer = Vec::new();
    let (mut painted, mut frames, mut units, mut have) = (0usize, 0usize, 0usize, false);
    while units < total {
        for r in recordings {
            s.resize(c, r.rows.saturating_add(1), r.cols);
            for (_, output) in &r.steps {
                for read in pieces(output, PANE_READ) {
                    for piece in pieces(read, PANE_PIECE) {
                        for &id in &panes {
                            s.output(id, piece);
                        }
                        unread(&mut s);
                    }
                    units = units.saturating_add(read.len());
                    s.hold_copies();
                    let dirty = s.views.get(&c).is_some_and(|v| v.dirty);
                    if let Some(view) = s.views.get_mut(&c) {
                        view.dirty = false;
                    }
                    if baseline || !dirty {
                        continue;
                    }
                    let Some(view) = s.views.get(&c) else {
                        continue;
                    };
                    fux::render::compose_view(&s, view, &mut spare, &mut placement);
                    if have && spare == shown {
                        continue;
                    }
                    buffer.clear();
                    fux::render::paint_into(&shown, have, &spare, &mut buffer);
                    painted = painted.saturating_add(buffer.len());
                    frames = frames.saturating_add(1);
                    black_box(&buffer);
                    std::mem::swap(&mut shown, &mut spare);
                    have = true;
                }
            }
        }
    }
    Ok(Done {
        units,
        frames,
        painted,
    })
}

/// The key mode of a pane whose program pushed the kitty `flags`.
fn kitty_mode(flags: u8) -> Result<fux::encode::KeyMode, String> {
    let size = fux_vt::Size::new(1, 1).map_err(|e| e.to_string())?;
    let mut parser =
        fux_vt::Parser::with_options(size, 0, fux::pane::OPTIONS).map_err(|e| e.to_string())?;
    parser
        .process(format!("\x1b[>{flags}u").as_bytes())
        .map_err(|e| e.to_string())?;
    Ok(parser.screen().key_mode())
}

/// The keys of every step of every recording, as a legacy terminal sends
/// them, or as a kitty-protocol terminal in `kitty` mode does.
fn keys(recordings: &[Recording], kitty: Option<fux::encode::KeyMode>) -> Vec<Vec<u8>> {
    let legacy = recordings
        .iter()
        .flat_map(|r| r.steps.iter().map(|(keys, _)| keys.clone()))
        .filter(|keys| !keys.is_empty());
    let Some(mode) = kitty else {
        return legacy.collect();
    };
    legacy
        .map(|keys| {
            let mut out = Vec::new();
            for stroke in strokes(&keys) {
                fux::encode::key_bytes(stroke, mode, &mut out);
            }
            out
        })
        .collect()
}

/// The keystrokes fux's decoder reads in `keys`, its Escape deadline passing
/// at the end.
fn strokes(keys: &[u8]) -> Vec<fux::keys::Keystroke> {
    let mut decoder = fux::decode::Decoder::default();
    let mut inputs = Vec::new();
    decoder.bytes(keys, &mut inputs);
    decoder.timeout(&mut inputs);
    inputs
        .into_iter()
        .filter_map(|input| {
            if let fux::decode::Input::Key(stroke) = input {
                Some(stroke)
            } else {
                None
            }
        })
        .collect()
}

fn decode(steps: &[Vec<u8>], baseline: bool, total: usize) -> Result<Done, String> {
    if steps.iter().all(Vec::is_empty) {
        return Err("no keys in the recordings".into());
    }
    let mut units = 0usize;
    let mut inputs = Vec::new();
    let mut decoder = fux::decode::Decoder::default();
    while units < total {
        for keys in steps {
            units = units.saturating_add(keys.len());
            if baseline {
                black_box(keys);
                continue;
            }
            // Each step's keys arrive as one read, and the Escape deadline
            // passes after them, as they were typed with pauses.
            decoder.bytes(black_box(keys), &mut inputs);
            decoder.timeout(&mut inputs);
            black_box(&inputs);
            inputs.clear();
        }
    }
    Ok(Done {
        units,
        frames: 0,
        painted: 0,
    })
}

fn encode(
    recordings: &[Recording],
    mode: fux::encode::KeyMode,
    baseline: bool,
    total: usize,
) -> Result<Done, String> {
    let all: Vec<fux::keys::Keystroke> = keys(recordings, None)
        .iter()
        .flat_map(|keys| strokes(keys))
        .collect();
    if all.is_empty() {
        return Err("no keys in the recordings".into());
    }
    let mut units = 0usize;
    let mut out = Vec::new();
    while units < total {
        for &stroke in &all {
            units = units.saturating_add(1);
            if baseline {
                black_box(stroke);
                continue;
            }
            fux::encode::key_bytes(black_box(stroke), mode, &mut out);
            black_box(&out);
            out.clear();
        }
    }
    Ok(Done {
        units,
        frames: 0,
        painted: 0,
    })
}

/// A session whose shell pane is real, with `panes` panes in all.
fn session(panes: usize) -> Result<fux::session::Session, String> {
    let config = fux::config::Config {
        history_bytes: HISTORY,
        ..fux::config::Config::default()
    };
    let mut s = fux::session::Session::new(config, "/nonexistent/fux.sock".into(), false);
    s.start().map_err(|e| e.to_string())?;
    for _ in 1..panes {
        let split = fux::command::Command::Split {
            axis: fux::layout::Axis::Horizontal,
            target: *s.panes.keys().next().ok_or("no pane")?,
            cmd: Vec::new(),
            cwd: None,
            env: fux::command::Env::default(),
            run: false,
            id: None,
        };
        s.run_command(
            &split,
            &fux::session::Origin::Cli(fux::session::Caller::default()),
        );
    }
    Ok(s)
}

/// Waits armed one after another on the same session, each for this many
/// of the recordings' bytes. A wait's own maps are hashed with keys of
/// their own, so its work differs from one wait to the next; many make a
/// run's count steady (one alone spread over 1% from run to run).
const WAITS: usize = 8;

fn wait(recordings: &[Recording], baseline: bool, total: usize) -> Result<Done, String> {
    let mut s = session(4)?;
    let panes: Vec<_> = s.panes.keys().copied().collect();
    let now = std::time::Instant::now();
    let arm = |s: &fux::session::Session| {
        let wait = fux::command::Wait {
            until: vec![fux::command::Until::Text("a text never written".into())],
            timeout: Some(std::time::Duration::from_secs(3600)),
            print: true,
            marks: false,
        };
        let shown: Vec<&fux::pane::Pane> = panes.iter().filter_map(|p| s.panes.get(p)).collect();
        fux::wait::Waiting::new(
            &shown,
            wait,
            fux::wait::Follows::Action,
            now,
            std::time::Duration::from_secs(3600),
        )
    };
    let all: Vec<u8> = recordings.iter().flat_map(Recording::bytes).collect();
    let bytes = repeated(&all, total);
    let each = bytes.len().div_ceil(WAITS).max(1);
    let mut units = 0usize;
    let mut n = 0usize;
    for part in pieces(&bytes, each) {
        let mut waiting = arm(&s);
        for read in pieces(part, PANE_READ) {
            let pane = panes
                .get(n.checked_rem(panes.len()).unwrap_or(0))
                .copied()
                .ok_or("no pane")?;
            n = n.saturating_add(1);
            for piece in pieces(read, PANE_PIECE) {
                s.output(pane, piece);
            }
            unread(&mut s);
            units = units.saturating_add(read.len());
            if baseline {
                continue;
            }
            black_box(waiting.answer(&s, now));
            black_box(waiting.due());
        }
    }
    Ok(Done {
        units,
        frames: 0,
        painted: 0,
    })
}

fn ready(baseline: bool, probes: usize) -> Result<Done, String> {
    // The probe is asked of a child of this process, with no session
    // around it: a sleep in a group of its own.
    let mut child = std::process::Command::new("sleep")
        .arg("3600")
        .process_group(0)
        .spawn()
        .map_err(|e| e.to_string())?;
    let pid = fuxix::process::Pid::of(&child).ok_or("no pid")?;
    // Asleep before the probes, and before the baseline kills it: the
    // count includes the child's own start, which a child killed early
    // had not finished (7.8 million instructions of 15 to 23). The wait,
    // up to 2 s of 1 ms looks, is counted in the run and its baseline
    // alike, so the baseline takes it away; how many looks it takes moves
    // with the machine's load, and so the workload's spread.
    let wchan = format!("/proc/{}/wchan", pid.as_raw());
    for _ in 0..if cfg!(target_os = "linux") { 2000 } else { 0 } {
        if std::fs::read_to_string(&wchan).is_ok_and(|w| w.contains("nanosleep")) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    for _ in 0..probes {
        if baseline {
            black_box(pid);
            continue;
        }
        black_box(fuxix::process::activity(pid, Path::new("/dev/null"), pid));
    }
    child.kill().map_err(|e| e.to_string())?;
    child.wait().map_err(|e| e.to_string())?;
    Ok(Done {
        units: probes,
        frames: 0,
        painted: 0,
    })
}

fn compile(baseline: bool, compiles: usize) -> Result<Done, String> {
    let lines: Vec<Vec<fuxth::token::Token>> = (LINES.iter())
        .map(|line| fuxth::token::source("bench", line).map_err(|e| e.to_string()))
        .collect::<Result<_, _>>()?;
    for n in 0..compiles {
        let tokens = lines
            .get(n.checked_rem(lines.len()).unwrap_or(0))
            .ok_or("no line")?;
        if baseline {
            black_box(tokens);
            continue;
        }
        black_box(fux::script::compile(tokens).map_err(|e| format!("{e:?}"))?);
    }
    Ok(Done {
        units: compiles,
        frames: 0,
        painted: 0,
    })
}

/// Reads what every pane's output left waiting at its call's budget
/// (`Pane::read_unread`), as the server's next turns do, until none waits.
fn unread(s: &mut fux::session::Session) {
    loop {
        let waiting: Vec<_> = (s.timers())
            .filter_map(|(_, timer)| {
                matches!(timer, fux::session::Timer::Unread(_)).then_some(timer)
            })
            .collect();
        if waiting.is_empty() {
            return;
        }
        for timer in waiting {
            s.fire(timer);
        }
    }
}

#[cfg(test)]
mod tests {
    /// A recording that resizes is fed at each size it was written at:
    /// zsh-resize's loads are its size, then each resize's, in order, its
    /// bytes whole (HAR-78).
    #[test]
    fn a_recording_is_fed_at_each_size_it_was_written_at() -> Result<(), String> {
        let recordings = crate::corpus::recordings(&dir())?;
        let zsh = recordings
            .iter()
            .find(|r| r.name == "zsh-resize")
            .ok_or("no zsh-resize")?;
        let json: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir().join("zsh-resize.json")).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        let size = |v: &serde_json::Value| {
            (
                v.get(0).and_then(serde_json::Value::as_u64),
                v.get(1).and_then(serde_json::Value::as_u64),
            )
        };
        let mut sizes = vec![size(json.get("size").ok_or("no size")?)];
        for step in json
            .get("steps")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(resize) = step.get("resize") {
                sizes.push(size(resize));
            }
        }
        assert!(sizes.len() > 1, "zsh-resize resizes");
        let loads = super::loads("corpus:zsh-resize", &recordings, zsh.bytes().len(), 1)?;
        let made: Vec<_> = loads
            .iter()
            .map(|l| (Some(u64::from(l.rows)), Some(u64::from(l.cols))))
            .collect();
        assert_eq!(made, sizes);
        let fed: Vec<u8> = loads.iter().flat_map(|l| l.bytes.iter().copied()).collect();
        assert_eq!(fed, zsh.bytes());
        Ok(())
    }

    /// A vt workload whose parser can't be made (a size of no rows) fails,
    /// never counted as having fed its bytes (HAR-77).
    #[test]
    fn a_vt_workload_that_cannot_be_set_up_fails() {
        let load = super::Load {
            rows: 0,
            cols: 80,
            bytes: b"x".to_vec(),
        };
        assert!(super::vt(&[load], false).is_err());
    }

    use std::path::Path;

    fn dir() -> std::path::PathBuf {
        crate::corpus::dir(&Path::new(env!("CARGO_MANIFEST_DIR")).join(".."))
    }

    fn payloads() -> std::path::PathBuf {
        crate::census::data(&Path::new(env!("CARGO_MANIFEST_DIR")).join(".."))
    }

    /// Every workload listed runs, and its baseline too.
    #[test]
    fn every_workload_runs() {
        let recordings = crate::corpus::recordings(&dir()).unwrap_or_default();
        let specs = super::list(&recordings);
        assert!(specs.len() > 20);
        for spec in specs.iter().filter(|s| {
            // Each recording alone is the same code as the whole corpus.
            !s.name.starts_with("vt/corpus:") || s.name.ends_with(":vim")
        }) {
            for baseline in [true, false] {
                let done = super::run_shrunk(&spec.name, &dir(), &payloads(), baseline, 64);
                assert!(
                    done.as_ref().is_ok_and(|d| d.units > 0),
                    "{} {baseline}: {:?}",
                    spec.name,
                    done.err()
                );
            }
        }
    }

    /// Composing and painting the corpus paints frames, and the baseline
    /// paints none.
    #[test]
    fn the_paint_workload_paints() {
        let done = super::run_shrunk("paint/corpus", &dir(), &payloads(), false, 16);
        assert!(done.is_ok_and(|d| d.frames > 100 && d.painted > d.frames));
        let base = super::run_shrunk("paint/corpus", &dir(), &payloads(), true, 16);
        assert!(base.is_ok_and(|d| d.frames == 0));
    }

    #[test]
    fn an_unknown_workload_is_an_error() {
        assert!(super::run("vt/nothing", &dir(), &payloads(), false).is_err());
        assert!(super::run("nothing", &dir(), &payloads(), false).is_err());
    }
}
