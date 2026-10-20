//! Speed: every engine fed the same workloads, as a program's output
//! arrives, 4 KiB at a time, on a 50×200 screen with 10000 rows of
//! history. The workloads are modelled on alacritty's vtebench
//! (github.com/alacritty/vtebench, `benchmarks/`). An engine in this
//! process is timed on parsing and applying alone; an engine behind a
//! process of its own also pays for the pipe to it, so its figure is
//! end to end, and marked so.
//!
//! The corpus (`corpus/`) gives workloads of real traffic beside these:
//! each recording's bytes, replayed as fast as they parse at the size they
//! were recorded at, over and over to make up the same amount, and all of
//! them in turn (`corpus`, which a run without names includes; `bench
//! corpus` gives every recording alone too).
//!
//! The census's speed payloads (vtebench's, kitty's, avt's, foot's, `y\r\n`,
//! `y\n` and sixel frames, [`census`]) are workloads too, each alone at
//! 50x200 (`bench census` names them all). Workloads are made one at a time,
//! and the timer encloses the feed alone: never a snapshot.
//!
//! Each run gives four tables, never ranked together: equal budget (10,000
//! rows asked, the rows each engine held read back) and equal retention (no
//! history, read back as 0, or the cell is marked and not ranked), each in
//! process and out of process apart. fux-vt is also run with print recording
//! on, as fux runs it (`fux-vt+prints`). After the timer each cell records
//! the contract (rows held, reply bytes, prints recorded) and the time of one
//! read (`snapshot(0)`), a feed-plus-read figure for engines that defer work.
//! Every heading and the JSON carry the fairness record: the input's
//! SHA-256, the chunk size, the setup, reflow and recording, that no read is
//! timed, in or out of process, the CPU set and the repeats.
//!
//! `NAME@N` is workload NAME fed N bytes a call instead of [`CHUNK`]
//! (`bench ascii@1 ascii@64 ascii@16384 ascii@65536`).
use crate::engine::{ENGINES, Kind, SUBJECT, Setup};
use std::time::{Duration, Instant};

/// The synthetic workloads: fux-bench's file, compiled here too, so both
/// crates time the same bytes from one source.
#[path = "../../../bench/src/synthetic.rs"]
pub mod synthetic;
pub use synthetic::{COLS, GENERATORS as WORKLOADS, ROWS};
pub const CHUNK: usize = 4096;
const RUNS: usize = 3;

/// A workload made: its name, what it is, the screen it runs on, and its
/// bytes.
pub struct Load {
    pub name: String,
    pub about: String,
    pub rows: u16,
    pub cols: u16,
    pub bytes: Vec<u8>,
    /// Where in `bytes` the terminal is resized, and to what: a recording's
    /// resizes, and each recording's own size in the corpus all in turn.
    pub resizes: Vec<(usize, u16, u16)>,
    /// The bytes an engine is given a call: [`CHUNK`], or the name's `@N`.
    pub chunk: usize,
}

/// A part of a load's bytes, and the size, rows and columns, the terminal
/// is resized to before it, if it is.
pub type Part<'a> = (Option<(u16, u16)>, &'a [u8]);

impl Load {
    /// Its bytes in parts, each with the size the terminal is resized to
    /// before it, if it is.
    pub fn parts(&self) -> Vec<Part<'_>> {
        let mut out = Vec::new();
        let (mut from, mut size) = (0usize, None);
        for &(at, rows, cols) in &self.resizes {
            out.push((size, self.bytes.get(from..at).unwrap_or_default()));
            (from, size) = (at, Some((rows, cols)));
        }
        out.push((size, self.bytes.get(from..).unwrap_or_default()));
        out
    }

    /// Feeds it to `engine` [`Load::chunk`] bytes at a time, resized where
    /// it is.
    pub fn feed(&self, engine: &mut dyn crate::engine::Engine) -> Result<(), String> {
        for (size, bytes) in self.parts() {
            if let Some((rows, cols)) = size {
                engine.resize(rows, cols)?;
            }
            engine.feed(bytes, self.chunk)?;
        }
        Ok(())
    }
}

/// No pass of a recording starts once this many resizes are made, as
/// fux-bench's `RESIZES_AT_MOST`: each reflows the history the passes
/// before left, so a short recording that resizes, over and over to its
/// size, would be resizes alone.
const RESIZES_AT_MOST: usize = 256;

/// `segments` (a size and the bytes written at it) over and over, whole,
/// until there are at least `total` bytes, or [`RESIZES_AT_MOST`]
/// resizes: the bytes, and where the size changes (`Load::resizes`), the
/// first segment's size the load's own.
fn sized(segments: &[(u16, u16, Vec<u8>)], total: usize) -> (Vec<u8>, Vec<(usize, u16, u16)>) {
    let (mut bytes, mut resizes) = (Vec::new(), Vec::new());
    let mut size = segments.first().map(|(r, c, _)| (*r, *c));
    let recorded: usize = segments.iter().map(|(_, _, b)| b.len()).sum();
    while recorded > 0 && bytes.len() < total && resizes.len() < RESIZES_AT_MOST {
        for (rows, cols, part) in segments {
            if size != Some((*rows, *cols)) {
                resizes.push((bytes.len(), *rows, *cols));
                size = Some((*rows, *cols));
            }
            bytes.extend_from_slice(part);
        }
    }
    (bytes, resizes)
}

/// `bytes` over and over, whole, until there are at least `total`.
#[cfg(test)]
pub fn repeated(bytes: &[u8], total: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(total.saturating_add(bytes.len()));
    while !bytes.is_empty() && out.len() < total {
        out.extend_from_slice(bytes);
    }
    out
}

/// The census's speed payloads, made as `fux-bench` makes its `vt/`
/// workloads of them: one file both crates compile.
#[path = "../../../bench/src/census.rs"]
pub mod census;

/// Where the census's vendored payload data is.
fn payloads() -> std::path::PathBuf {
    census::data(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

/// Every workload's name and what it is, without making any: the
/// synthetic ones, the census's payloads, `corpus` (every recording in
/// turn) and `corpus:NAME` (one recording alone).
pub fn names() -> Result<Vec<(String, String)>, String> {
    let mut out: Vec<(String, String)> = WORKLOADS
        .iter()
        .map(|(name, about, _)| ((*name).to_owned(), (*about).to_owned()))
        .collect();
    out.extend(census::PAYLOADS.iter().map(|p| {
        (
            p.name.to_owned(),
            format!("{}, at {}x{}", p.about, census::ROWS, census::COLS),
        )
    }));
    let recordings = crate::corpus::recordings(&[])?;
    if let Some(first) = recordings.first() {
        out.push((
            "corpus".into(),
            format!("every recording in turn, at {}x{}", first.rows, first.cols),
        ));
    }
    out.extend(recordings.iter().map(|r| {
        (
            format!("corpus:{}", r.name),
            format!("{}, recorded", r.version),
        )
    }));
    Ok(out)
}

/// The named workload, `total` bytes of it or a little more, alone: a
/// synthetic one at 50x200, a census payload at its size, `corpus`
/// (every recording in turn, each at its own size, with its resizes, as
/// fux-bench feeds it) or `corpus:NAME` (one recording at its size, with
/// its resizes).
pub fn load(name: &str, total: usize) -> Result<Load, String> {
    let (base, chunk) = chunked(name)?;
    let mut load = unchunked(base, total)?;
    if chunk != CHUNK {
        load.about = format!("{}, {chunk} bytes a call", load.about);
    }
    load.name = name.to_owned();
    load.chunk = chunk;
    Ok(load)
}

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

/// The bytes a call the named workload is fed.
pub fn chunk_of(name: &str) -> usize {
    chunked(name).map_or(CHUNK, |(_, n)| n)
}

// The workloads made on this thread, for the tests: a run makes the ones
// it times and no other.
#[cfg(test)]
thread_local! {
    static MADE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// [`load`] of a name without `@N`.
fn unchunked(name: &str, total: usize) -> Result<Load, String> {
    #[cfg(test)]
    MADE.with(|made| made.set(made.get().saturating_add(1)));
    if let Some(((_, about, _), bytes)) =
        (WORKLOADS.iter().find(|(n, _, _)| *n == name)).zip(synthetic::make(name, total))
    {
        return Ok(Load {
            name: name.to_owned(),
            about: (*about).to_owned(),
            rows: ROWS,
            cols: COLS,
            bytes,
            resizes: Vec::new(),
            chunk: CHUNK,
        });
    }
    if let Some(bytes) = census::make(name, &payloads(), total) {
        return Ok(Load {
            name: name.to_owned(),
            about: census::PAYLOADS
                .iter()
                .find(|p| p.name == name)
                .map_or_else(String::new, |p| {
                    format!("{} (the census fed {} MiB)", p.about, census::SIZE >> 20)
                }),
            rows: census::ROWS,
            cols: census::COLS,
            bytes: bytes?,
            resizes: Vec::new(),
            chunk: CHUNK,
        });
    }
    let recordings = crate::corpus::recordings(&[])?;
    if name == "corpus" {
        let first = recordings.first().ok_or("the corpus is empty")?;
        let segments: Vec<_> = recordings
            .iter()
            .flat_map(crate::corpus::Recording::segments)
            .collect();
        let recorded: usize = segments.iter().map(|(_, _, b)| b.len()).sum();
        let (bytes, resizes) = sized(&segments, total);
        return Ok(Load {
            name: name.to_owned(),
            about: format!(
                "every recording in turn, {recorded} bytes recorded, each at its size with its \
                 resizes, from {}x{}",
                first.rows, first.cols
            ),
            rows: first.rows,
            cols: first.cols,
            bytes,
            resizes,
            chunk: CHUNK,
        });
    }
    let one = name
        .strip_prefix("corpus:")
        .and_then(|n| recordings.iter().find(|r| r.name == n))
        .ok_or(format!("no workload {name:?}"))?;
    let recorded = one.bytes().len();
    let (bytes, resizes) = sized(&one.segments(), total);
    Ok(Load {
        name: name.to_owned(),
        about: format!("{}, {recorded} bytes recorded", one.version),
        rows: one.rows,
        cols: one.cols,
        bytes,
        resizes,
        chunk: CHUNK,
    })
}

/// The workloads `names` asks for, by name: with none, the synthetic ones
/// and the corpus all together; `corpus` names every recording, all
/// together and each alone; `census` every census payload; `NAME@N` that
/// one fed N bytes a call, after the others.
pub fn chosen(names: &[String]) -> Result<Vec<String>, String> {
    let (chunked, names): (Vec<String>, Vec<String>) =
        names.iter().cloned().partition(|n| n.contains('@'));
    for n in &chunked {
        let (base, _) = self::chunked(n)?;
        if !self::names()?.iter().any(|(name, _)| name == base) {
            return Err(format!("no workload {base:?} (in {n:?})"));
        }
    }
    if names.is_empty() && !chunked.is_empty() {
        return Ok(chunked);
    }
    let every = self::names()?;
    let picks = |n: &str, name: &str| {
        n == name
            || (n == "corpus" && name.starts_with("corpus"))
            || (n == "census" && census::PAYLOADS.iter().any(|p| p.name == name))
    };
    for n in &names {
        if !every.iter().any(|(name, _)| picks(n, name)) {
            let known: Vec<&str> = every.iter().map(|(name, _)| name.as_str()).collect();
            return Err(format!(
                "no workload {n:?}; there are: census corpus {}",
                known.join(" ")
            ));
        }
    }
    Ok(every
        .into_iter()
        .map(|(name, _)| name)
        .filter(|name| {
            if names.is_empty() {
                WORKLOADS.iter().any(|(n, _, _)| n == name) || name == "corpus"
            } else {
                names.iter().any(|n| picks(n, name))
            }
        })
        .chain(chunked)
        .collect())
}

/// The history the engines are set up to keep, for one table: never
/// ranked together.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Retention {
    /// Equal budget: 10,000 rows asked. fux-vt gets the bytes for 10,000
    /// full rows, which hold more short rows; the others keep what their
    /// own setups keep (100,000 rows, Ghostty 64 MiB). The rows each held
    /// are read back.
    Budget,
    /// Equal retention: no history, every engine holding the screen alone,
    /// checked by reading back the rows each held (0). Only engines whose
    /// history this harness sets.
    None,
}

impl Retention {
    pub const ALL: [Retention; 2] = [Retention::Budget, Retention::None];

    pub fn name(self) -> &'static str {
        match self {
            Retention::Budget => "equal-budget",
            Retention::None => "equal-retention",
        }
    }

    pub fn parse(name: &str) -> Option<Retention> {
        Retention::ALL.into_iter().find(|r| r.name() == name)
    }

    pub fn about(self) -> &'static str {
        match self {
            Retention::Budget => {
                "10,000 rows asked: fux-vt the bytes for 10,000 full rows, the others their \
                 own setups (100,000 rows, Ghostty 64 MiB); rows held read back"
            }
            Retention::None => {
                "no history: every engine holds the screen alone, read back as 0 rows"
            }
        }
    }

    /// `kind` made for this retention, or `None` if this harness cannot set
    /// its history.
    pub fn make(
        self,
        kind: &Kind,
        setup: &Setup,
    ) -> Option<Result<Box<dyn crate::engine::Engine>, String>> {
        use crate::engines;
        if self == Retention::Budget {
            return Some((kind.make)(setup));
        }
        let none = Setup {
            history: 0,
            ..*setup
        };
        Some(match kind.name {
            "fux-vt" => (kind.make)(&none),
            "ghostty" => engines::ghostty::with_scrollback(&none, 0),
            "alacritty" => engines::alacritty::with_history(&none, 0),
            "libvterm" => engines::libvterm::with_history(&none, 0),
            "avt" => engines::avt::with_history(&none, 0),
            "wezterm" => engines::wezterm::with_history(&none, 0),
            "vt100" => engines::vt100::with_history(&none, 0),
            _ => return None,
        })
    }
}

/// One engine on one workload: the median of [`RUNS`] feeds, each into a
/// fresh engine, in rounds that take every engine in turn (so a machine
/// that slows or speeds up meanwhile moves them all alike), timed around
/// the feed alone; then, outside the timer,
/// what the engine kept (the contract): the rows of history it holds and
/// the reply bytes it wrote; and the time of one read after the feed
/// (`snapshot(0)`), for engines that defer work until they are read.
struct Measured {
    /// Bytes fed.
    fed: usize,
    feed: Duration,
    read: Duration,
    rows_held: Option<usize>,
    reply_bytes: usize,
    /// fux-vt with print recording: the runs it recorded.
    prints: Option<usize>,
}

fn measure(kind: &Kind, load: &Load, retention: Retention) -> Option<Result<Measured, String>> {
    let setup = Setup {
        rows: load.rows,
        cols: load.cols,
        history: 10_000,
        reflow: true,
    };
    // No setup for this retention: unavailable; one that fails to be
    // made: an error, said.
    if let Err(e) = retention.make(kind, &setup)? {
        return Some(Err(format!("made: {e}")));
    }
    let bytes = load.bytes.as_slice();
    Some(crate::case::guarded(|| {
        let mut engine = retention.make(kind, &setup).ok_or("no setup")??;
        let started = Instant::now();
        load.feed(engine.as_mut())?;
        let feed = started.elapsed();
        let started = Instant::now();
        std::hint::black_box(engine.snapshot(0)?);
        let read = started.elapsed();
        Ok(Measured {
            fed: bytes.len(),
            feed,
            read,
            rows_held: engine.history_len(),
            reply_bytes: engine.replies()?.len(),
            prints: None,
        })
    }))
}

/// The run of median feed time of a cell's rounds: none if the engine
/// has no setup here, the first error if a round failed.
fn median(mut runs: Vec<Option<Result<Measured, String>>>) -> Option<Result<Measured, String>> {
    if runs.iter().any(Option::is_none) {
        return None;
    }
    if let Some(Some(Err(e))) = runs.iter().find(|r| matches!(r, Some(Err(_)))) {
        return Some(Err(e.clone()));
    }
    runs.sort_by_key(|r| r.as_ref().and_then(|m| m.as_ref().ok()).map(|m| m.feed));
    let middle = runs.len() / 2;
    runs.into_iter().nth(middle).flatten()
}

/// fux-vt as fux runs it, recording what is printed (`record_prints`) and
/// taking the record after each chunk, as a pane does after each read.
fn measure_prints(load: &Load, retention: Retention) -> Result<Measured, String> {
    let size = crate::vt_size(load.rows, load.cols)?;
    let rows = match retention {
        Retention::Budget => 10_000usize,
        Retention::None => 0,
    };
    // As the adapter sizes it.
    let history = crate::engines::fux_vt::history_bytes(rows, load.cols);
    {
        let mut parser =
            fux_vt::Parser::with_options(size, history, crate::engines::fux_vt::options(true))
                .map_err(|e| format!("fux-vt: {e}"))?;
        parser.record_prints(true);
        let mut recorded = 0usize;
        let started = Instant::now();
        for (size, part) in load.parts() {
            if let Some((rows, cols)) = size {
                parser
                    .resize(crate::vt_size(rows, cols)?)
                    .map_err(|e| format!("fux-vt: {e}"))?;
            }
            let mut rest = part;
            while !rest.is_empty() {
                let (now, later) = rest
                    .split_at_checked(load.chunk.min(rest.len()))
                    .unwrap_or((rest, &[]));
                parser
                    .process_with_replies(now, |r| {
                        std::hint::black_box(r);
                    })
                    .map_err(|e| format!("fux-vt: {e}"))?;
                let prints = parser.take_prints();
                recorded = recorded.saturating_add(std::hint::black_box(prints).iter().count());
                rest = later;
            }
        }
        let feed = started.elapsed();
        Ok(Measured {
            fed: load.bytes.len(),
            feed,
            read: Duration::ZERO,
            rows_held: Some(parser.screen().history_len()),
            reply_bytes: 0,
            prints: Some(recorded),
        })
    }
}

/// The programs an input's SHA-256 is asked of, in turn.
const HASHERS: [(&str, &[&str]); 2] = [("sha256sum", &[]), ("shasum", &["-a", "256"])];

/// The input's SHA-256, for the fairness record, by the first of
/// `HASHERS` that gives one. An error if none is there or none gives one.
pub fn sha256(bytes: &[u8]) -> Result<String, String> {
    sha256_by(bytes, &HASHERS)
}

/// `sha256` by the first of `programs` that exits 0 with 64 hex digits:
/// one that is missing or fails is passed over for the next.
fn sha256_by(bytes: &[u8], programs: &[(&str, &[&str])]) -> Result<String, String> {
    use std::io::Write as _;
    let mut why = Vec::new();
    for &(program, args) in programs {
        let child = std::process::Command::new(program)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn();
        let mut child = match child {
            Ok(child) => child,
            Err(e) => {
                why.push(format!("{program}: {e}"));
                continue;
            }
        };
        // A program that exits before reading it all fails below.
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(bytes);
        }
        let out = match child.wait_with_output() {
            Ok(out) => out,
            Err(e) => {
                why.push(format!("{program}: {e}"));
                continue;
            }
        };
        let text = String::from_utf8_lossy(&out.stdout);
        let hash = text.split_whitespace().next().unwrap_or_default();
        if out.status.success() && hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Ok(hash.to_owned());
        }
        why.push(format!("{program}: no hash ({})", out.status));
    }
    Err(format!(
        "sha256: none of the programs gave one: {}",
        why.join("; ")
    ))
}

/// The CPUs this process may run on.
pub fn cpu_set() -> String {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("Cpus_allowed_list:"))
                .map(|l| l.trim().to_owned())
        })
        .unwrap_or_else(|| "unknown".into())
}

/// A table's columns: fux-vt with print recording, then each engine.
enum Column<'a> {
    Prints,
    Engine(&'a Kind),
}

impl Column<'_> {
    fn name(&self) -> &'static str {
        match self {
            Column::Prints => "fux-vt+prints",
            Column::Engine(k) => k.name,
        }
    }
}

/// Times fux-vt and `engines` on the named workloads (see [`chosen`]),
/// each `mb` MiB, made one at a time, and prints MB/s in four tables:
/// equal budget and equal retention, each in process and out of process
/// apart, never ranked together. Each heading carries the fairness record,
/// and the JSON (to `json`, if given) every measure.
pub fn run(engines: &[usize], names: &[String], mb: usize) -> Result<bool, String> {
    run_to(engines, names, mb, None)
}

pub fn run_to(
    engines: &[usize],
    names: &[String],
    mb: usize,
    json: Option<&str>,
) -> Result<bool, String> {
    let bytes = mb.saturating_mul(1 << 20);
    let chosen = chosen(names)?;
    let kinds: Vec<&Kind> = std::iter::once(&SUBJECT)
        .chain(engines.iter().filter_map(|&i| ENGINES.get(i)))
        .collect();
    let cpus = cpu_set();
    let tables: Vec<(Retention, bool)> = Retention::ALL
        .into_iter()
        .flat_map(|r| [(r, true), (r, false)])
        .collect();
    let mut results: Vec<Vec<(String, Cells)>> = tables.iter().map(|_| Vec::new()).collect();
    let mut inputs = Vec::new();
    for name in &chosen {
        // Made one at a time, and dropped after: all of them at once took
        // gigabytes.
        let load = load(name, bytes)?;
        inputs.push(serde_json::json!({
            "name": load.name, "about": load.about, "bytes": load.bytes.len(),
            "sha256": sha256(&load.bytes)?, "rows": load.rows, "cols": load.cols,
            "chunk_bytes": load.chunk,
        }));
        for ((retention, in_process), out) in tables.iter().zip(&mut results) {
            let mut columns: Vec<Column> = Vec::new();
            if *in_process {
                columns.push(Column::Prints);
            }
            columns.extend(
                kinds
                    .iter()
                    .filter(|k| k.in_process == *in_process)
                    .map(|k| Column::Engine(k)),
            );
            // Rounds, each taking every column in turn.
            let mut runs: Vec<Vec<_>> = columns.iter().map(|_| Vec::new()).collect();
            for _ in 0..RUNS {
                for (c, column_runs) in columns.iter().zip(&mut runs) {
                    column_runs.push(match c {
                        Column::Prints => Some(measure_prints(&load, *retention)),
                        Column::Engine(k) => measure(k, &load, *retention),
                    });
                }
            }
            let cells: Cells = (columns.iter().zip(runs))
                .map(|(c, column_runs)| (c.name().to_owned(), median(column_runs)))
                .collect();
            out.push((name.clone(), cells));
        }
    }
    report(&tables, &results, &inputs, mb, &cpus, json)
}

/// Each table's cells, by workload: (column, measured).
type Cells = Vec<(String, Option<Result<Measured, String>>)>;

/// Prints [`run_to`]'s tables of `results` (and writes their JSON to
/// `json`, if given): whether every measure was made, a failed one failing
/// the run after its table and JSON.
fn report(
    tables: &[(Retention, bool)],
    results: &[Vec<(String, Cells)>],
    inputs: &[serde_json::Value],
    mb: usize,
    cpus: &str,
    json: Option<&str>,
) -> Result<bool, String> {
    let mut doc_tables = Vec::new();
    let mut errors = 0usize;
    for ((retention, in_process), rows) in tables.iter().zip(results) {
        let Some((_, first)) = rows.first() else {
            continue;
        };
        if first.is_empty() {
            continue;
        }
        let place = if *in_process {
            "in process"
        } else {
            "own process, end to end"
        };
        println!(
            "\n{} ({}), {place}: MB/s, the median of {RUNS} rounds taking the engines in turn, the timer around the feed alone \
             (no read timed); {CHUNK}-byte chunks (NAME@N: N); {mb} MiB per workload; {ROWS}x{COLS} \
             (recordings at their size); fux-vt reflow on, print recording off but in \
             fux-vt+prints; CPUs {cpus}; ! = the contract failed (not ranked)",
            retention.name(),
            retention.about()
        );
        print!("{:<34}", "");
        for (column, _) in first {
            print!(" {column:>13}");
        }
        println!();
        let mut doc_rows = Vec::new();
        for (name, cells) in rows {
            print!("{name:<34}");
            let mut doc_cells = serde_json::Map::new();
            for (column, cell) in cells {
                let text = match cell {
                    None => "unavailable".to_owned(),
                    Some(Err(e)) => {
                        eprintln!("{column}: {name}: {e}");
                        errors = errors.saturating_add(1);
                        "error".to_owned()
                    }
                    Some(Ok(m)) => {
                        let total = (m.fed as f64).max(1.0);
                        let mbs = total / m.feed.as_secs_f64().max(1e-9) / 1e6;
                        // Equal retention: every engine must hold no rows.
                        let kept =
                            *retention == Retention::None && m.rows_held.is_some_and(|r| r > 0);
                        doc_cells.insert(
                            column.clone(),
                            serde_json::json!({
                                "mb_per_s": mbs,
                                "feed_seconds": m.feed.as_secs_f64(),
                                "read_seconds": m.read.as_secs_f64(),
                                "feed_and_read_mb_per_s": total / m.feed.saturating_add(m.read).as_secs_f64().max(1e-9) / 1e6,
                                "rows_held": m.rows_held,
                                "reply_bytes": m.reply_bytes,
                                "prints_recorded": m.prints,
                                "contract_failed": kept,
                            }),
                        );
                        format!("{mbs:.1}{}", if kept { "!" } else { "" })
                    }
                };
                print!(" {text:>13}");
            }
            println!();
            doc_rows.push(serde_json::json!({ "name": name, "engines": doc_cells }));
        }
        // The rows each engine held, read back, and a read after the feed.
        println!(
            "rows held after the feed (read back), and MB/s of the feed plus one read (snapshot(0); fux-vt's adapter reads its whole history):"
        );
        for (name, cells) in rows {
            print!("{name:<34}");
            for (_, cell) in cells {
                let text = match cell {
                    Some(Ok(m)) => format!(
                        "{}/{:.0}",
                        m.rows_held
                            .map_or_else(|| "?".to_owned(), |r| r.to_string()),
                        (m.fed as f64)
                            / m.feed.saturating_add(m.read).as_secs_f64().max(1e-9)
                            / 1e6
                    ),
                    _ => "-".to_owned(),
                };
                print!(" {text:>13}");
            }
            println!();
        }
        doc_tables.push(serde_json::json!({
            "retention": retention.name(),
            "setup": retention.about(),
            "in_process": in_process,
            "fairness": {
                "chunk_bytes": CHUNK,
                "mb": mb,
                "read_timed": false,
                "reflow": "fux-vt on; the others as they are",
                "print_recording": "off, but fux-vt+prints",
                "cpus": cpus,
                "repeats": RUNS,
                "statistic": "the median feed of the rounds (repeats), each taking every engine in turn",
                "kitty_images": "ignored-protocol throughput: fux-vt keeps no kitty images",
                "sixel": "ignored-protocol throughput for an engine without sixel",
            },
            "workloads": doc_rows,
        }));
    }
    println!();
    for input in inputs {
        println!(
            "{:<34} {} sha256 {}",
            input
                .get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(""),
            input
                .get("about")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(""),
            input
                .get("sha256")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
        );
    }
    if let Some(path) = json {
        let doc = serde_json::json!({
            "kind": "fux-vt-compare bench",
            "inputs": inputs,
            "tables": doc_tables,
        });
        let text = serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())?;
        std::fs::write(path, text).map_err(|e| format!("{path}: {e}"))?;
        eprintln!("wrote {path}");
    }
    // A measure that failed fails the run, after its table and JSON.
    if errors > 0 {
        eprintln!("bench: {errors} measures failed (error above)");
    }
    Ok(errors == 0)
}

#[cfg(test)]
mod tests {
    use super::Retention;
    use crate::engine::{ENGINES, SUBJECT, Setup};

    /// A cell is its rounds' median feed, none if a round had no setup,
    /// and the error if one failed.
    #[test]
    fn a_cell_is_the_median_of_its_rounds() {
        let run = |ms: u64| {
            Some(Ok(super::Measured {
                fed: 1,
                feed: std::time::Duration::from_millis(ms),
                read: std::time::Duration::ZERO,
                rows_held: None,
                reply_bytes: 0,
                prints: None,
            }))
        };
        let feed =
            |m: Option<Result<super::Measured, String>>| m.and_then(Result::ok).map(|m| m.feed);
        let middle = super::median(vec![run(9), run(3), run(5)]);
        assert_eq!(feed(middle), Some(std::time::Duration::from_millis(5)));
        assert!(super::median(vec![run(1), None, run(2)]).is_none());
        let failed = super::median(vec![run(1), Some(Err("x".into())), run(2)]);
        assert!(matches!(failed, Some(Err(e)) if e == "x"));
    }

    /// `NAME@N` is NAME fed N bytes a call, chosen after the others; a bad
    /// N or an unknown NAME is refused.
    #[test]
    fn a_workload_at_a_chunk_size_is_fed_that_many_bytes_a_call() -> Result<(), String> {
        let names = ["ascii".to_owned(), "ascii@1".to_owned()];
        assert_eq!(super::chosen(&names)?, names);
        let load = super::load("ascii@64", 1 << 10)?;
        assert_eq!((load.name.as_str(), load.chunk), ("ascii@64", 64));
        assert_eq!(super::load("ascii", 1 << 10)?.chunk, super::CHUNK);
        for wrong in ["ascii@0", "ascii@x", "nothing@64"] {
            assert!(super::chosen(&[wrong.to_owned()]).is_err(), "{wrong}");
        }
        Ok(())
    }

    /// A short recording that resizes is repeated only until it has
    /// resized [`super::RESIZES_AT_MOST`] times, as fux-bench repeats it.
    #[test]
    fn a_recording_that_resizes_is_repeated_to_a_bound() -> Result<(), String> {
        let load = super::load("corpus:zsh-resize", 4 << 20)?;
        let resizes = load.resizes.len();
        assert!(
            resizes > 0 && resizes <= super::RESIZES_AT_MOST + 4,
            "{resizes}"
        );
        Ok(())
    }

    /// A recording that resizes is fed at each size it was written at, as
    /// fux-bench feeds it: zsh-resize's parts are its size, then each
    /// resize's, its bytes whole; the corpus all in turn resizes to each
    /// recording's own size (HAR-78).
    #[test]
    fn a_recording_is_fed_at_each_size_it_was_written_at() -> Result<(), String> {
        let recordings = crate::corpus::recordings(&[])?;
        let zsh = recordings
            .iter()
            .find(|r| r.name == "zsh-resize")
            .ok_or("no zsh-resize")?;
        let load = super::load("corpus:zsh-resize", zsh.bytes().len())?;
        let sizes: Vec<(u16, u16)> = std::iter::once((zsh.rows, zsh.cols))
            .chain(zsh.resizes.iter().map(|&(_, r, c)| (r, c)))
            .collect();
        assert!(sizes.len() > 1, "zsh-resize resizes");
        let parts = load.parts();
        let made: Vec<(u16, u16)> = (parts.iter())
            .scan((load.rows, load.cols), |at, (size, _)| {
                *at = size.unwrap_or(*at);
                Some(*at)
            })
            .collect();
        assert_eq!(made, sizes);
        let fed: Vec<u8> = parts.iter().flat_map(|(_, b)| b.iter().copied()).collect();
        assert_eq!(fed, zsh.bytes());
        let all = super::load("corpus", 1)?;
        let differ = recordings
            .iter()
            .filter(|r| (r.rows, r.cols) != (all.rows, all.cols))
            .count();
        assert!(
            all.resizes.len() >= differ,
            "{} resizes, {differ} recordings of another size",
            all.resizes.len()
        );
        Ok(())
    }

    /// A hash is the input's, by whichever of shasum and sha256sum there
    /// is (FIPS 180-2's example "abc"), never a placeholder.
    #[test]
    fn an_input_hashes_to_its_sha256() -> Result<(), String> {
        let abc = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert_eq!(super::sha256(b"abc")?, abc);
        // One that fails, or is missing, is passed over for the next.
        let after = [
            ("false", &[][..]),
            ("no-such-hasher", &[]),
            ("sha256sum", &[]),
        ];
        assert_eq!(super::sha256_by(b"abc", &after)?, abc);
        assert!(super::sha256_by(b"abc", &[("false", &[])]).is_err());
        Ok(())
    }

    /// Equal retention holds what it says: every engine this harness sets
    /// up keeps no history after 300 lines on a 10-row screen, and equal
    /// budget keeps some.
    #[test]
    fn equal_retention_keeps_no_history() -> Result<(), String> {
        let setup = Setup {
            rows: 10,
            cols: 20,
            history: 10_000,
            reflow: true,
        };
        let lines = super::repeated(b"line\r\n", 300 * 6);
        for kind in std::iter::once(&SUBJECT)
            .chain(ENGINES)
            .filter(|k| k.in_process)
        {
            for (retention, keeps) in [(Retention::None, false), (Retention::Budget, true)] {
                let Some(engine) = retention.make(kind, &setup) else {
                    continue;
                };
                let mut engine = engine?;
                engine.feed(&lines, 64)?;
                if let Some(rows) = engine.history_len() {
                    assert_eq!(
                        rows > 0,
                        keeps,
                        "{} {}: {rows}",
                        kind.name,
                        retention.name()
                    );
                }
            }
        }
        Ok(())
    }

    /// A run makes the workloads it times and no other: two named, two
    /// made, where making every one before choosing took gigabytes (HAR-75).
    #[test]
    fn choosing_a_workload_makes_no_other() -> Result<(), String> {
        super::MADE.with(|made| made.set(0));
        let names = ["ascii".to_owned(), "corpus:bash-small".to_owned()];
        // No bytes: what is counted is the workloads made, not their size.
        assert!(super::run_to(&[], &names, 0, None)?);
        assert_eq!(super::MADE.with(std::cell::Cell::get), names.len());
        Ok(())
    }

    /// An engine that can't be made for a table is an error with its
    /// reason, not "unavailable", which says the harness has no setup.
    #[test]
    fn an_engine_that_cannot_be_made_is_an_error_not_unavailable() {
        let load = super::Load {
            name: "no rows".into(),
            about: String::new(),
            rows: 0,
            cols: 80,
            bytes: b"x".to_vec(),
            resizes: Vec::new(),
            chunk: super::CHUNK,
        };
        let made = super::measure(&SUBJECT, &load, Retention::Budget);
        assert!(
            matches!(&made, Some(Err(e)) if e.starts_with("made: ")),
            "{:?}",
            made.map(|m| m.map(|m| m.fed))
        );
    }

    /// A run with a failed measure fails, after its table; one with none
    /// (an engine with no setup is no failure) does not.
    #[test]
    fn a_run_with_an_error_cell_fails() -> Result<(), String> {
        let measured = || super::Measured {
            fed: 1,
            feed: std::time::Duration::from_millis(1),
            read: std::time::Duration::ZERO,
            rows_held: Some(0),
            reply_bytes: 0,
            prints: None,
        };
        let report = |cell: Option<Result<super::Measured, String>>| {
            let cells = vec![
                ("fux-vt+prints".to_owned(), Some(Ok(measured()))),
                ("fux-vt".to_owned(), cell),
            ];
            super::report(
                &[(Retention::Budget, true)],
                &[vec![("ascii".to_owned(), cells)]],
                &[],
                1,
                "0",
                None,
            )
        };
        assert!(!report(Some(Err("x".into())))?);
        assert!(report(Some(Ok(measured())))?);
        assert!(report(None)?);
        Ok(())
    }

    /// fux-vt+prints keeps the history fux-vt's adapter keeps under equal
    /// budget: 20,000 full lines on 10 rows of 10, the same rows held.
    #[test]
    fn fux_vt_and_prints_keep_one_history_budget() -> Result<(), String> {
        let mut line = vec![b'x'; 10];
        line.extend_from_slice(b"\r\n");
        let load = super::Load {
            name: "lines".into(),
            about: String::new(),
            rows: 10,
            cols: 10,
            bytes: super::repeated(&line, 20_000 * line.len()),
            resizes: Vec::new(),
            chunk: super::CHUNK,
        };
        let prints = super::measure_prints(&load, Retention::Budget)?.rows_held;
        let adapter = super::measure(&SUBJECT, &load, Retention::Budget)
            .ok_or("no setup")??
            .rows_held;
        assert_eq!(prints, adapter);
        Ok(())
    }
}
