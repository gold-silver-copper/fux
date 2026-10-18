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
use crate::engine::{ENGINES, Kind, SUBJECT, Setup};
use crate::rng::Rng;
use std::fmt::Write;
use std::time::{Duration, Instant};

pub const ROWS: u16 = 50;
pub const COLS: u16 = 200;
pub const CHUNK: usize = 4096;
const RUNS: usize = 3;

/// A workload: its name, what it is, and how to make `bytes` of it.
pub type Workload = (&'static str, &'static str, fn(&mut Rng, usize) -> Vec<u8>);

pub const WORKLOADS: &[Workload] = &[
    ("ascii", "lines of plain ASCII text, scrolling", ascii),
    (
        "dense-cells",
        "full screens where every cell has its own 256-colour foreground and background",
        dense_cells,
    ),
    (
        "medium-cells",
        "full screens of words, an SGR change every few cells",
        medium_cells,
    ),
    (
        "cursor-motion",
        "a glyph at a random position, over and over",
        cursor_motion,
    ),
    (
        "scrolling",
        "short lines, scrolling the whole screen",
        scrolling,
    ),
    (
        "scroll-region",
        "lines scrolling inside a region of half the screen",
        scroll_region,
    ),
    (
        "unicode",
        "CJK, accented text, combining marks and emoji, scrolling",
        unicode,
    ),
];

fn ascii(r: &mut Rng, bytes: usize) -> Vec<u8> {
    let words = [
        "lorem ",
        "ipsum ",
        "dolor ",
        "sit ",
        "amet, ",
        "consectetur ",
        "adipiscing ",
        "elit ",
    ];
    let mut out = Vec::with_capacity(bytes);
    while out.len() < bytes {
        let mut line = 0usize;
        while line < 150 {
            let word = r.pick(&words).copied().unwrap_or("x ");
            out.extend_from_slice(word.as_bytes());
            line = line.saturating_add(word.len());
        }
        out.extend_from_slice(b"\r\n");
    }
    out
}

fn dense_cells(r: &mut Rng, bytes: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes);
    while out.len() < bytes {
        dense_screen(r, &mut out, ROWS, COLS);
    }
    out
}

/// A screen of `rows` x `cols` where every cell has its own 256-colour
/// foreground and background: one screen of `dense-cells`.
pub fn dense_screen(r: &mut Rng, out: &mut Vec<u8>, rows: u16, cols: u16) {
    out.extend_from_slice(b"\x1b[H");
    for _ in 0..usize::from(rows).saturating_mul(usize::from(cols)) {
        let glyph = char::from(b'A'.saturating_add(u8::try_from(r.below(26)).unwrap_or(0)));
        let _ = write!(
            Text(out),
            "\x1b[38;5;{};48;5;{}m{glyph}",
            r.below(256),
            r.below(256)
        );
    }
}

fn medium_cells(r: &mut Rng, bytes: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes);
    while out.len() < bytes {
        medium_screen(r, &mut out, ROWS, COLS);
    }
    out
}

/// A screen of `rows` x `cols` of words, an SGR change every few cells:
/// one screen of `medium-cells`.
pub fn medium_screen(r: &mut Rng, out: &mut Vec<u8>, rows: u16, cols: u16) {
    let words = [
        "the ", "quick ", "brown ", "fox ", "jumps ", "over ", "lazy ", "dog ",
    ];
    let sgr = [
        "\x1b[1m",
        "\x1b[0m",
        "\x1b[31m",
        "\x1b[4m",
        "\x1b[7m",
        "\x1b[38;2;10;200;30m",
        "\x1b[44m",
        "\x1b[m",
    ];
    out.extend_from_slice(b"\x1b[H");
    for _ in 0..usize::from(rows)
        .saturating_mul(usize::from(cols))
        .checked_div(6)
        .unwrap_or(1)
    {
        out.extend_from_slice(r.pick(&sgr).copied().unwrap_or("").as_bytes());
        out.extend_from_slice(r.pick(&words).copied().unwrap_or("").as_bytes());
    }
}

fn cursor_motion(r: &mut Rng, bytes: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes);
    while out.len() < bytes {
        let (row, col) = (
            r.below(usize::from(ROWS)).saturating_add(1),
            r.below(usize::from(COLS)).saturating_add(1),
        );
        let _ = write!(Text(&mut out), "\x1b[{row};{col}H*");
    }
    out
}

fn scrolling(_: &mut Rng, bytes: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes);
    while out.len() < bytes {
        out.extend_from_slice(b"y\r\n");
    }
    out
}

fn scroll_region(_: &mut Rng, bytes: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes);
    let _ = write!(
        Text(&mut out),
        "\x1b[{};{}r\x1b[{}H",
        ROWS / 4,
        ROWS / 4 * 3,
        ROWS / 4 * 3
    );
    while out.len() < bytes {
        out.extend_from_slice(b"line in a region\r\n");
    }
    out.extend_from_slice(b"\x1b[r");
    out
}

fn unicode(r: &mut Rng, bytes: usize) -> Vec<u8> {
    let pieces = [
        "界", "全角", "é", "ñandú ", "e\u{301}", "👍", "👍🏽", "🇺🇸", "Ж", "ष्", "a", " ",
    ];
    let mut out = Vec::with_capacity(bytes);
    while out.len() < bytes {
        for _ in 0..60 {
            out.extend_from_slice(r.pick(&pieces).copied().unwrap_or("").as_bytes());
        }
        out.extend_from_slice(b"\r\n");
    }
    out
}

/// `write!` into bytes.
struct Text<'a>(&'a mut Vec<u8>);

impl std::fmt::Write for Text<'_> {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        self.0.extend_from_slice(s.as_bytes());
        Ok(())
    }
}

/// A workload made: its name, what it is, the screen it runs on, and its
/// bytes.
pub struct Load {
    pub name: String,
    pub about: String,
    pub rows: u16,
    pub cols: u16,
    pub bytes: Vec<u8>,
}

/// `bytes` over and over, whole, until there are at least `total`.
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
/// (every recording in turn, at the first's size, 40x120 as most are,
/// without resizes) or `corpus:NAME` (one recording at its size, without
/// its resizes).
pub fn load(name: &str, total: usize) -> Result<Load, String> {
    if let Some((_, about, make)) = WORKLOADS.iter().find(|(n, _, _)| *n == name) {
        return Ok(Load {
            name: name.to_owned(),
            about: (*about).to_owned(),
            rows: ROWS,
            cols: COLS,
            bytes: make(&mut Rng::new(1), total),
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
        });
    }
    let recordings = crate::corpus::recordings(&[])?;
    if name == "corpus" {
        let first = recordings.first().ok_or("the corpus is empty")?;
        let all: Vec<u8> = recordings.iter().flat_map(|r| r.bytes()).collect();
        return Ok(Load {
            name: name.to_owned(),
            about: format!(
                "every recording in turn, {} bytes recorded, at {}x{}",
                all.len(),
                first.rows,
                first.cols
            ),
            rows: first.rows,
            cols: first.cols,
            bytes: repeated(&all, total),
        });
    }
    let one = name
        .strip_prefix("corpus:")
        .and_then(|n| recordings.iter().find(|r| r.name == n))
        .ok_or(format!("no workload {name:?}"))?;
    let bytes = one.bytes();
    Ok(Load {
        name: name.to_owned(),
        about: format!("{}, {} bytes recorded", one.version, bytes.len()),
        rows: one.rows,
        cols: one.cols,
        bytes: repeated(&bytes, total),
    })
}

/// The workloads `names` asks for, by name: with none, the synthetic ones
/// and the corpus all together; `corpus` names every recording, all
/// together and each alone; `census` every census payload.
pub fn chosen(names: &[String]) -> Result<Vec<String>, String> {
    let every = self::names()?;
    let picks = |n: &str, name: &str| {
        n == name
            || (n == "corpus" && name.starts_with("corpus"))
            || (n == "census" && census::PAYLOADS.iter().any(|p| p.name == name))
    };
    for n in names {
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

/// One engine on one workload: the best of [`RUNS`] feeds, each into a
/// fresh engine, timed around the feed alone; then, outside the timer,
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
    retention.make(kind, &setup)?.ok()?;
    let bytes = load.bytes.as_slice();
    Some(crate::case::guarded(|| {
        let mut best: Option<Measured> = None;
        for _ in 0..RUNS {
            let mut engine = retention.make(kind, &setup).ok_or("no setup")??;
            let started = Instant::now();
            engine.feed(bytes, CHUNK)?;
            let feed = started.elapsed();
            let started = Instant::now();
            std::hint::black_box(engine.snapshot(0)?);
            let read = started.elapsed();
            let run = Measured {
                fed: bytes.len(),
                feed,
                read,
                rows_held: engine.history_len(),
                reply_bytes: engine.replies()?.len(),
                prints: None,
            };
            if best.as_ref().is_none_or(|b| run.feed < b.feed) {
                best = Some(run);
            }
        }
        best.ok_or_else(|| "no run".to_owned())
    }))
}

/// fux-vt as fux runs it, recording what is printed (`record_prints`) and
/// taking the record after each chunk, as a pane does after each read.
fn measure_prints(load: &Load, retention: Retention) -> Result<Measured, String> {
    let size = crate::vt_size(load.rows, load.cols)?;
    let rows = match retention {
        Retention::Budget => 10_000usize,
        Retention::None => 0,
    };
    // As the adapter sizes it: 24 bytes a row and 8 a cell.
    let history = rows.saturating_mul(usize::from(load.cols).saturating_mul(8).saturating_add(24));
    let mut best: Option<Measured> = None;
    for _ in 0..RUNS {
        let mut parser =
            fux_vt::Parser::with_options(size, history, crate::engines::fux_vt::options(true))
                .map_err(|e| format!("fux-vt: {e}"))?;
        parser.record_prints(true);
        let mut recorded = 0usize;
        let started = Instant::now();
        let mut rest = load.bytes.as_slice();
        while !rest.is_empty() {
            let (now, later) = rest
                .split_at_checked(CHUNK.min(rest.len()))
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
        let feed = started.elapsed();
        let run = Measured {
            fed: load.bytes.len(),
            feed,
            read: Duration::ZERO,
            rows_held: Some(parser.screen().history_len()),
            reply_bytes: 0,
            prints: Some(recorded),
        };
        if best.as_ref().is_none_or(|b| run.feed < b.feed) {
            best = Some(run);
        }
    }
    best.ok_or_else(|| "no run".to_owned())
}

/// The input's SHA-256, by `shasum -a 256` (as run.sh checks its
/// downloads), for the fairness record.
pub fn sha256(bytes: &[u8]) -> String {
    use std::io::Write as _;
    let child = std::process::Command::new("shasum")
        .args(["-a", "256"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn();
    let Ok(mut child) = child else {
        return "unknown".into();
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(bytes);
    }
    child.wait_with_output().map_or_else(
        |_| "unknown".into(),
        |out| {
            String::from_utf8_lossy(&out.stdout)
                .split_whitespace()
                .next()
                .unwrap_or("unknown")
                .to_owned()
        },
    )
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
    // Each table's cells, by workload: (column, measured).
    type Cells = Vec<(String, Option<Result<Measured, String>>)>;
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
            "sha256": sha256(&load.bytes), "rows": load.rows, "cols": load.cols,
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
            let cells: Cells = columns
                .iter()
                .map(|c| {
                    let m = match c {
                        Column::Prints => Some(measure_prints(&load, *retention)),
                        Column::Engine(k) => measure(k, &load, *retention),
                    };
                    (c.name().to_owned(), m)
                })
                .collect();
            out.push((name.clone(), cells));
        }
    }
    let mut doc_tables = Vec::new();
    for ((retention, in_process), rows) in tables.iter().zip(&results) {
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
            "\n{} ({}), {place}: MB/s, the best of {RUNS} feeds, the timer around the feed alone \
             (no read timed); {CHUNK}-byte chunks; {mb} MiB per workload; {ROWS}x{COLS} \
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
                "kitty_images": "ignored-protocol throughput: fux-vt keeps no kitty images",
                "sixel": "ignored-protocol throughput for an engine without sixel",
            },
            "workloads": doc_rows,
        }));
    }
    println!();
    for input in &inputs {
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
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::Retention;
    use crate::engine::{ENGINES, SUBJECT, Setup};

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
}
