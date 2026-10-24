//! The workloads: fixed work, each in this process and repeated to a fixed
//! size, so that a run of one is the same instructions every time.
//!
//! - `vt/NAME`: fux-vt alone, set up as fux sets up a pane
//!   (`fux::pane::OPTIONS`), fed in 4 KiB pieces as `compare`'s `bench`
//!   feeds it: the synthetic workloads on a 50×200 screen, each recording
//!   at its own size (`vt/corpus:NAME`), and every recording in turn
//!   (`vt/corpus`). `vt/ascii@N` and `vt/corpus@N`: the same fed N bytes
//!   a call, for each N of [`CHUNKS`] (any N runs by name).
//! - `vt/resize-history`: a pane's parser, its history full at 50×200,
//!   resized to 80 columns and back, [`RESIZES`] times: every row of
//!   history reflowed, both ways. `vt/resize-193`, `:no-reflow`,
//!   `:no-history` and `:short-rows`: to 193 columns and back, as
//!   [`RESIZINGS`] sets each up.
//! - `vt/direct-colours`, `vt/links`: a moment each, as
//!   [`MOMENTS`] says: what is set up is the baseline's too, and what
//!   follows it, 4 KiB a call, is measured.
//! - `vt/vtebench:NAME`, `vt/kitty:NAME`, `vt/avt:NAME`,
//!   `vt/foot:alt-random-writes`, `vt/yes:crlf`, `vt/yes:lf`,
//!   `vt/sixel:frames`: fux-vt alone on each of the census's speed
//!   payloads (`census`), 8 MiB at 50×200, each alone.
//! - `encode/sixel`: a 1920×1080 picture in 256 colours (one frame of
//!   `vt/sixel:256`) encoded as sixel whole, [`ENCODES`] times, as fux's
//!   renderer encodes a picture it shows (`fux_vt::sixel::encode`); the
//!   baseline decodes it alone.
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
//! - `wait/version`: waits for a version that never changes (`fux::wait::Waiting`),
//!   8 armed one after another, on four panes of a session, each asked for
//!   its answer and when it is due
//!   after each 64 KiB tick (in 16 KiB reads) of the recordings: the server's
//!   look at each output while a wait is armed.
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
use std::path::Path;

/// fux-vt is fed this much at a time, as `compare`'s `bench` feeds it.
const CHUNK: usize = 4096;
/// The other call sizes `vt/ascii@N` and `vt/corpus@N` are listed at.
const CHUNKS: [usize; 4] = [1, 64, 16 << 10, 64 << 10];
/// `vt/resize-history`'s resizes to 80 columns and back.
const RESIZES: usize = 4;
/// `encode/sixel`'s encodings of its picture.
const ENCODES: usize = 8;
/// The columns `vt/resize-history` resizes to.
const NARROW: u16 = 80;

/// A resize workload: its name, the columns it resizes to and back, with
/// reflow or not, with a pane's history or none, and its history filled
/// with `vt/ascii`'s lines or with short ones (`y`).
struct Resizing {
    name: &'static str,
    narrow: u16,
    reflow: bool,
    history: bool,
    short: bool,
}

const RESIZINGS: [Resizing; 5] = [
    Resizing {
        name: "vt/resize-history",
        narrow: NARROW,
        reflow: true,
        history: true,
        short: false,
    },
    Resizing {
        name: "vt/resize-193",
        narrow: 193,
        reflow: true,
        history: true,
        short: false,
    },
    Resizing {
        name: "vt/resize-193:no-reflow",
        narrow: 193,
        reflow: false,
        history: true,
        short: false,
    },
    Resizing {
        name: "vt/resize-193:no-history",
        narrow: 193,
        reflow: true,
        history: false,
        short: false,
    },
    Resizing {
        name: "vt/resize-193:short-rows",
        narrow: 193,
        reflow: true,
        history: true,
        short: true,
    },
];

/// A moment's workload: its name, what it is, what sets it up and what
/// is measured, each made for a divisor of its size (the tests' shrink).
type Moment = (
    &'static str,
    &'static str,
    fn(usize) -> Vec<u8>,
    fn(usize) -> Vec<u8>,
);

const MOMENTS: [Moment; 2] = [
    (
        "vt/direct-colours",
        "4 MiB of cells each in a 24-bit colour none had before: the style table's inserts \
         and its sweeps",
        |_| Vec::new(),
        |shrink| direct_colours((4_usize << 20).checked_div(shrink).unwrap_or(1)),
    ),
    (
        "vt/links",
        "OSC 8 hyperlinks each to a URI of its own, past the 4 MiB of held links at which \
         the link sweep sorts every run",
        |_| Vec::new(),
        |shrink| links((6_usize << 20).checked_div(shrink).unwrap_or(1)),
    ),
];

/// Cells each in a new 24-bit foreground, to `size` bytes.
fn direct_colours(size: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(size);
    let mut n = 0u32;
    while out.len() < size {
        let [_, r, g, b] = n.to_be_bytes();
        out.extend_from_slice(format!("\x1b[38;2;{r};{g};{b}mx").as_bytes());
        n = n.wrapping_add(1);
    }
    out
}

/// Words each linked to a URI of its own, to `size` bytes.
fn links(size: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(size);
    let mut n = 0usize;
    while out.len() < size {
        let link =
            format!("\x1b]8;;https://example.com/a/longer/path/{n}\x1b\\word\x1b]8;;\x1b\\ ");
        out.extend_from_slice(link.as_bytes());
        n = n.saturating_add(1);
    }
    out
}
/// The `vt/ascii` bytes that fill its history: a pane's 8 MiB holds some
/// 6,000 of its rows, a quarter of these.
const FILL: usize = 4 << 20;
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
/// No pass of a recording starts once this many resizes are made: each
/// reflows the history the passes before left, so a short recording that
/// resizes, over and over to its size, would be resizes alone (zsh-resize,
/// 1,533 bytes and 3 resizes, took 30 s to 4 MiB).
const RESIZES_AT_MOST: usize = 256;
/// The keys typed in the recordings, over and over to this many bytes.
const KEYS: usize = 1 << 20;
/// Keystrokes through a session, this many.
const KEYSTROKES: usize = 20_000;
/// Keys encoded, over and over to this many.
const STROKES: usize = 1 << 20;
/// Bytes read into the panes of `wait/version`, over and over to this many.
const WAITED: usize = 4 << 20;
/// Command lines compiled, this many.
const COMPILES: usize = 2_000;
/// Command lines an agent sends, as `docs/agents.md` shows them.
const LINES: [&str; 4] = [
    "opts str app name str './app --fast' command run",
    "%2 str y text str Enter key send-keys",
    "%2 str 0.57.0.3.0 changed wait",
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
    for name in ["ascii", "corpus"] {
        for n in CHUNKS {
            out.push(spec(
                format!("vt/{name}@{n}"),
                format!("vt/{name} fed {n} bytes a call"),
            ));
        }
    }
    out.push(spec(
        "encode/sixel".into(),
        format!("a 1920x1080 256-colour picture encoded as sixel, {ENCODES} times"),
    ));
    for r in &RESIZINGS {
        let history = match (r.history, r.short) {
            (false, _) => "no history".to_owned(),
            (true, false) => format!("a full history ({} MiB)", HISTORY >> 20),
            (true, true) => format!("a full history ({} MiB) of short rows", HISTORY >> 20),
        };
        let reflow = if r.reflow { "" } else { ", reflow off" };
        out.push(spec(
            r.name.into(),
            format!(
                "{history} at 50x200 resized to {} columns and back, {RESIZES} times{reflow}",
                r.narrow
            ),
        ));
    }
    for (name, about, _, _) in MOMENTS {
        out.push(spec(name.into(), about.into()));
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
            "wait/version",
            "a wait on four panes asked after each read of every recording, 4 MiB",
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
/// gives the code measured. A workload that is no stream of bytes says
/// `NO_STREAM`.
pub fn fed(name: &str) -> String {
    if RESIZINGS.iter().any(|r| r.name == name) {
        format!("{NO_STREAM}: fux-vt::Parser::resize, its history filled 4096 bytes a call")
    } else if name.starts_with("vt/") {
        let chunk = chunked(name).map_or(CHUNK, |(_, n)| n);
        format!("fux-vt::Parser::process_with, {chunk} bytes a call")
    } else if name == "encode/sixel" {
        format!("{NO_STREAM}: fux_vt::sixel::encode of the whole picture, {ENCODES} times")
    } else if name.starts_with("pane/") {
        "fux::pane::Pane::output, 16384 bytes a call".into()
    } else if name.starts_with("paint/") || name == "wait/version" {
        "fux::session::Session::output, 16384 bytes a call, 65536 a tick".into()
    } else {
        NO_STREAM.into()
    }
}

/// What [`fed`] says of a workload that is no stream of bytes.
pub const NO_STREAM: &str = "not a stream of bytes";

/// A workload's name without its `@N`, and the N bytes a call it names
/// (else [`CHUNK`]); an `@` with no N of at least 1 is an error.
fn chunked(name: &str) -> Result<(&str, usize), String> {
    match name.rsplit_once('@') {
        None => Ok((name, CHUNK)),
        Some((base, n)) => match n.parse::<usize>() {
            Ok(n) if n > 0 => Ok((base, n)),
            _ => Err(format!("{name:?}: @N takes bytes a call, at least 1")),
        },
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
    if let Some(r) = RESIZINGS.iter().find(|r| r.name == name) {
        let history = if r.history { size(HISTORY) } else { 0 };
        return resize_history(r, baseline, size(FILL), history);
    }
    if let Some((_, _, setup, measured)) = MOMENTS.iter().find(|(n, _, _, _)| *n == name) {
        return moment(&setup(shrink), &measured(shrink), baseline);
    }
    if name == "encode/sixel" {
        return encode_sixel(
            payloads,
            baseline,
            ENCODES.checked_div(shrink).unwrap_or(1).max(1),
        );
    }
    let (name, chunk) = chunked(name)?;
    if chunk != CHUNK && !name.starts_with("vt/") {
        return Err(format!("{name:?}: only a vt/ workload takes @N"));
    }
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
        return vt(&[load], baseline, chunk);
    }
    let recordings = corpus::recordings(dir)?;
    if let Some(rest) = name.strip_prefix("vt/") {
        let loads = loads(rest, &recordings, size(RECORDING), shrink)?;
        return vt(&loads, baseline, chunk);
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
        "wait/version" => wait(&recordings, baseline, size(WAITED)),
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
        // Each at the size it was written at, its resizes kept.
        let segments: Vec<_> = recordings.iter().flat_map(Recording::segments).collect();
        return Ok(passes(&segments, size(CORPUS)));
    }
    if let Some(one) = name.strip_prefix("corpus:")
        && each > 0
        && let Some(r) = recordings.iter().find(|r| r.name == one)
    {
        // Over and over, whole, its resizes with it, to `each` bytes.
        return Ok(passes(&r.segments(), each));
    }
    Err(format!("no workload {name:?}; `list` lists them"))
}

/// `segments` (a size and the bytes written at it) over and over, whole,
/// until there are `total` bytes, or [`RESIZES_AT_MOST`] resizes.
fn passes(segments: &[(u16, u16, Vec<u8>)], total: usize) -> Vec<Load> {
    let recorded: usize = segments.iter().map(|(_, _, b)| b.len()).sum();
    let (mut out, mut made, mut resizes) = (Vec::<Load>::new(), 0usize, 0usize);
    while recorded > 0 && made < total && resizes < RESIZES_AT_MOST {
        for (rows, cols, bytes) in segments {
            if out
                .last()
                .is_some_and(|l| (l.rows, l.cols) != (*rows, *cols))
            {
                resizes = resizes.saturating_add(1);
            }
            made = made.saturating_add(bytes.len());
            out.push(Load {
                rows: *rows,
                cols: *cols,
                bytes: bytes.clone(),
            });
        }
    }
    out
}

/// Replies and events, kept from being optimized away.
struct Sink;

impl fux_vt::Sink for Sink {
    fn reply(&mut self, bytes: &[u8]) {
        black_box(bytes);
    }
}

/// What a pane is given of `bytes` (a tick's read, or a whole load), a
/// call at a time, as fux's server gives it: [`PANE_PIECE`] bytes a call.
fn given(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    pieces(bytes, PANE_PIECE)
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

/// The parser fed `loads`, each at its size, `chunk` bytes a call. A size
/// or a parser that can't be made fails the run: it would measure
/// nothing. What the parser refuses as it reads (an allocation past its
/// limits) is the workload's, as fux's pane takes it, and goes on.
fn vt(loads: &[Load], baseline: bool, chunk: usize) -> Result<Done, String> {
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
        for piece in pieces(&load.bytes, chunk) {
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

/// A pane's parser.
fn pane_parser(history: usize, options: fux_vt::Options) -> Result<fux_vt::Parser, String> {
    let size = fux_vt::Size::new(synthetic::ROWS, synthetic::COLS).map_err(|e| e.to_string())?;
    fux_vt::Parser::with_options(size, history, options)
        .map_err(|e| format!("a parser of {size:?}: {e}"))
}

/// A moment: a pane's parser fed `setup`, then `measured` unless
/// `baseline`, both 4 KiB a call. Its units are the bytes measured.
fn moment(setup: &[u8], measured: &[u8], baseline: bool) -> Result<Done, String> {
    let mut parser = pane_parser(HISTORY, fux::pane::OPTIONS)?;
    for piece in pieces(setup, CHUNK) {
        let _ = parser.process_with(piece, &mut Sink);
    }
    if !baseline {
        for piece in pieces(measured, CHUNK) {
            let _ = parser.process_with(black_box(piece), &mut Sink);
        }
    }
    black_box(parser.screen().cursor_position());
    Ok(Done {
        units: measured.len().max(1),
        frames: 0,
        painted: 0,
    })
}

/// A pane's parser at 50x200 with `history` bytes of history, given
/// `fill` bytes of `vt/ascii` (or of `y` lines, if `short`), 4 KiB a
/// call: enough to fill its history.
fn full_history(
    fill: usize,
    history: usize,
    options: fux_vt::Options,
    short: bool,
) -> Result<fux_vt::Parser, String> {
    let mut parser = pane_parser(history, options)?;
    let bytes = match short {
        true => repeated(b"y\r\n", fill),
        false => synthetic::make("ascii", fill).ok_or("no ascii workload")?,
    };
    for piece in pieces(&bytes, CHUNK) {
        let _ = parser.process_with(piece, &mut Sink);
    }
    Ok(parser)
}

/// The picture `encode/sixel` encodes: one frame of `vt/sixel:256`,
/// decoded by a pane's parser.
fn sixel_picture(payloads: &Path) -> Result<std::sync::Arc<fux_vt::sixel::Picture>, String> {
    let frame = census::make("sixel:256", payloads, 1).ok_or("no sixel:256 payload")??;
    let size = fux_vt::Size::new(census::ROWS, census::COLS).map_err(|e| e.to_string())?;
    let mut parser = fux_vt::Parser::with_options(size, HISTORY, fux::pane::OPTIONS)
        .map_err(|e| format!("a parser of {size:?}: {e}"))?;
    parser.process(&frame).map_err(|e| e.to_string())?;
    let shown = parser
        .screen()
        .pictures()
        .next()
        .map(|p| std::sync::Arc::clone(p.picture));
    shown.ok_or_else(|| "sixel:256 showed no picture".to_owned())
}

/// `encode/sixel`: its picture encoded whole `times` times; the baseline
/// only decodes it. Its units are the bytes encoded.
fn encode_sixel(payloads: &Path, baseline: bool, times: usize) -> Result<Done, String> {
    let picture = sixel_picture(payloads)?;
    let mut units = 0usize;
    if !baseline {
        let mut out = Vec::new();
        for _ in 0..times {
            out.clear();
            let whole = (0, 0, picture.width(), picture.height());
            fux_vt::sixel::encode(black_box(&picture), whole, &mut out);
            units = units.saturating_add(black_box(&out).len());
        }
    }
    Ok(Done {
        units: units.max(1),
        frames: 0,
        painted: 0,
    })
}

/// `vt/resize-history`: a full history (`full_history`) resized to
/// [`NARROW`] columns and back, [`RESIZES`] times, each reflowing every
/// row; the baseline fills it and resizes nothing.
fn resize_history(
    r: &Resizing,
    baseline: bool,
    fill: usize,
    history: usize,
) -> Result<Done, String> {
    let options = match r.reflow {
        true => fux::pane::OPTIONS,
        false => (fux_vt::Feature::ALL.into_iter())
            .filter(|&f| f != fux_vt::Feature::Reflow && fux::pane::OPTIONS.has(f))
            .collect::<fux_vt::Options>()
            .with_identity(fux::pane::OPTIONS.identity()),
    };
    let mut parser = full_history(fill, history, options, r.short)?;
    let wide = parser.screen().size();
    let narrow = fux_vt::Size::new(wide.rows(), r.narrow).map_err(|e| e.to_string())?;
    if !baseline {
        for _ in 0..RESIZES {
            for size in [narrow, wide] {
                parser
                    .resize(black_box(size))
                    .map_err(|e| format!("resize to {size:?}: {e}"))?;
            }
        }
    }
    black_box(parser.screen().history_len());
    Ok(Done {
        units: RESIZES.saturating_mul(2),
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
        for piece in given(&load.bytes) {
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
            argv: Vec::new(),
            typed: None,
            cwd: None,
            env: fux::command::Env::default(),
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
                    for piece in given(read) {
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

/// A session of `panes` panes in all, none running a process
/// (`Session::new(.., false)`): their output is what the workload feeds.
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
            argv: Vec::new(),
            typed: None,
            cwd: None,
            env: fux::command::Env::default(),
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
        let shown: Vec<&fux::pane::Pane> = panes.iter().filter_map(|p| s.panes.get(p)).collect();
        fux::wait::Waiting::new(&shown, None, now, std::time::Duration::from_secs(3600))
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
            for piece in given(read) {
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
    /// bytes whole.
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
    /// never counted as having fed its bytes.
    #[test]
    fn a_vt_workload_that_cannot_be_set_up_fails() {
        let load = super::Load {
            rows: 0,
            cols: 80,
            bytes: b"x".to_vec(),
        };
        assert!(super::vt(&[load], false, super::CHUNK).is_err());
    }

    /// A short recording that resizes is repeated only until it has
    /// resized [`super::RESIZES_AT_MOST`] times: zsh-resize to 4 MiB was
    /// some 8,000 resizes, each reflowing a history, 30 s a run.
    #[test]
    fn a_recording_that_resizes_is_repeated_to_a_bound() -> Result<(), String> {
        let recordings = crate::corpus::recordings(&dir())?;
        let loads = super::loads("corpus:zsh-resize", &recordings, super::RECORDING, 1)?;
        let sizes: Vec<_> = loads.iter().map(|l| (l.rows, l.cols)).collect();
        let resizes = sizes
            .iter()
            .zip(sizes.iter().skip(1))
            .filter(|(a, b)| a != b)
            .count();
        assert!(
            resizes > 0 && resizes <= super::RESIZES_AT_MOST + 4,
            "{resizes}"
        );
        Ok(())
    }

    /// `encode/sixel` encodes the whole 1920x1080 picture `vt/sixel:256`
    /// decodes.
    #[test]
    fn the_sixel_picture_encoded_is_the_whole_frame() -> Result<(), String> {
        let picture = super::sixel_picture(&payloads())?;
        assert_eq!((picture.width(), picture.height()), (1920, 1080));
        let done = super::encode_sixel(&payloads(), false, 1)?;
        assert!(done.units > 1000, "{}", done.units);
        Ok(())
    }

    /// `@N` names the bytes a call, and is refused where it means nothing.
    #[test]
    fn a_workload_says_how_many_bytes_a_call_it_is_fed() {
        assert!(super::fed("vt/ascii@1").contains(" 1 bytes a call"));
        assert!(super::fed("vt/corpus").contains(" 4096 bytes a call"));
        assert!(super::fed("vt/resize-history").starts_with(super::NO_STREAM));
        for wrong in ["vt/ascii@0", "vt/ascii@x", "pane/ascii@64"] {
            assert!(super::run_shrunk(wrong, &dir(), &payloads(), false, 64).is_err());
        }
    }

    /// `vt/resize-history` resizes a history that is full: twice the fill
    /// keeps about as many rows, the budget dropping the oldest.
    #[test]
    fn the_history_resized_is_full() -> Result<(), String> {
        let (fill, history) = (super::FILL / 16, super::HISTORY / 16);
        let pane = fux::pane::OPTIONS;
        let rows = super::full_history(fill, history, pane, false)?
            .screen()
            .history_len();
        let more = super::full_history(fill * 2, history, pane, false)?
            .screen()
            .history_len();
        assert!(
            rows > 300 && rows.abs_diff(more) <= rows / 20,
            "{rows} {more}"
        );
        Ok(())
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

    /// A pane is given what one of fux's server's reads brings, 16 KiB
    /// (src/server.rs, read_pane's buffer), not a tick's 64 KiB: a tick is
    /// four calls, and the fairness record says so.
    #[test]
    fn a_pane_is_given_what_one_server_read_brings() {
        let tick = vec![b'x'; super::PANE_READ];
        let calls: Vec<usize> = super::given(&tick).map(<[u8]>::len).collect();
        assert_eq!(calls, [16384; 4]);
        for name in ["pane/ascii", "paint/corpus", "wait/version"] {
            assert!(super::fed(name).contains("16384 bytes a call"), "{name}");
        }
    }
}
