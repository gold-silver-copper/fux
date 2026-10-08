//! The workloads: fixed work, each in this process and repeated to a fixed
//! size, so that a run of one is the same instructions every time.
//!
//! - `vt/NAME`: fux-vt alone, set up as fux sets up a pane
//!   (`fux::pane::OPTIONS`), fed in 4 KiB pieces as `compare`'s `bench`
//!   feeds it: the synthetic workloads on a 50×200 screen, each recording
//!   at its own size (`vt/corpus:NAME`), and every recording in turn
//!   (`vt/corpus`).
//! - `pane/NAME`: the same through `fux::pane::Pane::output`, in the 64 KiB
//!   reads fux's server makes of a pane.
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
//!
//! Each workload has a baseline: the same run without the part measured.
//! For most that is making the bytes; for `paint/*` it is also feeding the
//! session, so the difference is composition and paint alone.
use crate::corpus::{self, Recording};
use crate::synthetic;
use std::hint::black_box;
use std::path::Path;

/// fux-vt is fed this much at a time, as `compare`'s `bench` feeds it.
const CHUNK: usize = 4096;
/// fux's server reads this much of a pane's output at a time (`PANE_READ`).
const PANE_READ: usize = 64 * 1024;
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
/// What fux keeps of a pane's history by default.
const HISTORY: usize = 10_000;
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
            "the same keys encoded for a pane in legacy mode",
        ),
        (
            "encode/kitty",
            "the same keys encoded for a pane with every kitty flag",
        ),
    ] {
        out.push(spec(name.into(), about.into()));
    }
    out
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
pub fn run(name: &str, dir: &Path, baseline: bool) -> Result<Done, String> {
    run_shrunk(name, dir, baseline, 1)
}

/// [`run`], every size divided by `shrink`: for the tests, in a debug
/// build.
fn run_shrunk(name: &str, dir: &Path, baseline: bool, shrink: usize) -> Result<Done, String> {
    let size = |n: usize| n.checked_div(shrink).unwrap_or(n);
    let recordings = corpus::recordings(dir)?;
    if let Some(rest) = name.strip_prefix("vt/") {
        let loads = loads(rest, &recordings, size(RECORDING), shrink)?;
        return Ok(vt(&loads, baseline));
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
        "decode/kitty" => decode(&keys(&recordings, Some(KITTY_CLIENT)), baseline, size(KEYS)),
        "encode/legacy" => encode(
            &recordings,
            fux::encode::KeyMode::legacy(false),
            baseline,
            size(STROKES),
        ),
        "encode/kitty" => encode(
            &recordings,
            fux::encode::KeyMode {
                application: false,
                kitty: KITTY_ALL,
                other_keys: None,
            },
            baseline,
            size(STROKES),
        ),
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
            for r in recordings {
                let bytes = r.bytes();
                made = made.saturating_add(bytes.len());
                out.push(Load {
                    rows: r.rows,
                    cols: r.cols,
                    bytes,
                });
            }
        }
        return Ok(out);
    }
    if let Some(one) = name.strip_prefix("corpus:")
        && each > 0
        && let Some(r) = recordings.iter().find(|r| r.name == one)
    {
        return Ok(vec![Load {
            rows: r.rows,
            cols: r.cols,
            bytes: repeated(&r.bytes(), each),
        }]);
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

fn vt(loads: &[Load], baseline: bool) -> Done {
    let units = loads.iter().map(|l| l.bytes.len()).sum();
    if baseline {
        black_box(loads);
        return Done {
            units,
            frames: 0,
            painted: 0,
        };
    }
    let mut parser: Option<fux_vt::Parser> = None;
    for load in loads {
        match &mut parser {
            Some(p) => {
                let _ = p.resize(load.rows, load.cols);
            }
            None => {
                parser =
                    fux_vt::Parser::with_options(load.rows, load.cols, HISTORY, fux::pane::OPTIONS)
                        .ok();
            }
        }
        if let Some(p) = &mut parser {
            for piece in pieces(&load.bytes, CHUNK) {
                let _ = p.process_with(black_box(piece), &mut Sink);
            }
        }
    }
    black_box(parser.as_ref().map(|p| p.screen().cursor_position()));
    Done {
        units,
        frames: 0,
        painted: 0,
    }
}

fn pane(loads: &[Load], baseline: bool) -> Result<Done, String> {
    let units = loads.iter().map(|l| l.bytes.len()).sum();
    let first = loads.first().ok_or("no bytes")?;
    let mut pane = fux::pane::Pane::new(
        fux::command::parse_pane("%1").map_err(|e| e.to_string())?,
        "bench".into(),
        "/bin/sh".into(),
        first.rows,
        first.cols,
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
        pane.resize(load.rows, load.cols);
        for piece in pieces(&load.bytes, PANE_READ) {
            black_box(pane.output(black_box(piece)));
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
        history_lines: HISTORY,
        ..fux::config::Config::default()
    };
    let mut s = fux::session::Session::new(config, "/nonexistent/fux.sock".into(), false);
    s.start().map_err(|e| e.to_string())?;
    let c = s.attach(41, 120, None).map_err(|e| e.to_string())?;
    let pane = s.panes.keys().next().copied().ok_or("no pane")?;
    // A full screen, as a pane at work shows: the recordings' output.
    for r in recordings.iter().take(8) {
        for (_, output) in &r.steps {
            s.output(pane, output);
        }
    }
    s.settle_if_needed();
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
        s.input(c, &[key]);
        // What the PTY would carry to the program, and its echo back.
        let typed = s
            .panes
            .get_mut(&pane)
            .map(|p| p.input.drain_all())
            .unwrap_or_default();
        black_box(&typed);
        let glyph = char::from(key).encode_utf8(&mut echo);
        s.output(pane, glyph.as_bytes());
        s.settle_if_needed();
        buffer.clear();
        buffer.extend_from_slice(&s.before_paint(c));
        if let Some(view) = s.views.get_mut(&c) {
            view.dirty = false;
        }
        if !fux::render::compose_into(&s, c, &mut spare, &mut placement) {
            continue;
        }
        // Every key changes the screen: painted without asking whether it
        // did, as `paint_into` finds what changed.
        fux::render::paint_into(have.then_some(&shown), &spare, &mut buffer);
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
        history_lines: HISTORY,
        ..fux::config::Config::default()
    };
    let mut s = fux::session::Session::new(config, "/nonexistent/fux.sock".into(), false);
    s.start().map_err(|e| e.to_string())?;
    let first = recordings.first().ok_or("no recordings")?;
    // The bar takes a row: the pane is the recording's size.
    let c = s
        .attach(first.rows.saturating_add(1), first.cols, None)
        .map_err(|e| e.to_string())?;
    if split {
        let argv = ["split".to_owned(), "-h".to_owned()];
        s.run(&argv, &fux::session::Ctx::client(c));
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
                for piece in pieces(output, PANE_READ) {
                    for &id in &panes {
                        s.output(id, piece);
                    }
                    units = units.saturating_add(piece.len());
                    s.settle_if_needed();
                    let dirty = s.views.get(&c).is_some_and(|v| v.dirty);
                    if let Some(view) = s.views.get_mut(&c) {
                        view.dirty = false;
                    }
                    if baseline || !dirty {
                        continue;
                    }
                    if !fux::render::compose_into(&s, c, &mut spare, &mut placement) {
                        continue;
                    }
                    if have && spare == shown {
                        continue;
                    }
                    buffer.clear();
                    fux::render::paint_into(have.then_some(&shown), &spare, &mut buffer);
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

/// The keys of every step of every recording, as a legacy terminal sends
/// them, or as a kitty-protocol terminal with `kitty` flags does.
fn keys(recordings: &[Recording], kitty: Option<u8>) -> Vec<Vec<u8>> {
    let legacy = recordings
        .iter()
        .flat_map(|r| r.steps.iter().map(|(keys, _)| keys.clone()))
        .filter(|keys| !keys.is_empty());
    let Some(flags) = kitty else {
        return legacy.collect();
    };
    let mode = fux::encode::KeyMode {
        application: false,
        kitty: flags,
        other_keys: None,
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

#[cfg(test)]
mod tests {
    use std::path::Path;

    fn dir() -> std::path::PathBuf {
        crate::corpus::dir(&Path::new(env!("CARGO_MANIFEST_DIR")).join(".."))
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
                let done = super::run_shrunk(&spec.name, &dir(), baseline, 64);
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
        let done = super::run_shrunk("paint/corpus", &dir(), false, 16);
        assert!(done.is_ok_and(|d| d.frames > 100 && d.painted > d.frames));
        let base = super::run_shrunk("paint/corpus", &dir(), true, 16);
        assert!(base.is_ok_and(|d| d.frames == 0));
    }

    #[test]
    fn an_unknown_workload_is_an_error() {
        assert!(super::run("vt/nothing", &dir(), false).is_err());
        assert!(super::run("nothing", &dir(), false).is_err());
    }
}
