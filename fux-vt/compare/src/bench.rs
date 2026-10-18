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

/// The best of [`RUNS`] times to feed `bytes` to a fresh engine: the timer
/// encloses the feed alone, never a read of the screen.
fn time(kind: &Kind, load: &Load) -> Result<Duration, String> {
    let setup = Setup {
        rows: load.rows,
        cols: load.cols,
        history: 10_000,
        reflow: true,
    };
    let bytes = load.bytes.as_slice();
    let mut best = Duration::MAX;
    for _ in 0..RUNS {
        let took = crate::case::guarded(|| {
            let mut engine = (kind.make)(&setup)?;
            let started = Instant::now();
            engine.feed(bytes, CHUNK)?;
            Ok(started.elapsed())
        })?;
        best = best.min(took);
    }
    Ok(best)
}

/// Times fux-vt and `engines` on the named workloads (see [`chosen`]),
/// each `mb` MiB, made one at a time, and prints MB/s as a table.
pub fn run(engines: &[usize], names: &[String], mb: usize) -> Result<bool, String> {
    let bytes = mb.saturating_mul(1 << 20);
    let chosen = chosen(names)?;
    let kinds: Vec<&Kind> = std::iter::once(&SUBJECT)
        .chain(engines.iter().filter_map(|&i| ENGINES.get(i)))
        .collect();
    println!(
        "MB/s, best of {RUNS}, {CHUNK}-byte chunks, {mb} MiB per workload, {ROWS}x{COLS} (the corpus at its own size); * = own process, end to end"
    );
    print!("{:<22}", "");
    for kind in &kinds {
        let mark = if kind.in_process { "" } else { "*" };
        print!(" {:>10}", format!("{}{mark}", kind.name));
    }
    println!();
    let mut abouts = Vec::new();
    for name in &chosen {
        // Made one at a time, and dropped after: all of them at once took
        // gigabytes.
        let load = load(name, bytes)?;
        print!("{name:<22}");
        for kind in &kinds {
            let cell = match time(kind, &load) {
                Ok(took) => {
                    let mbs = (load.bytes.len() as f64) / took.as_secs_f64().max(1e-9) / 1e6;
                    format!("{mbs:.1}")
                }
                Err(e) => {
                    eprintln!("{}: {name}: {e}", kind.name);
                    "error".to_owned()
                }
            };
            print!(" {cell:>10}");
        }
        println!();
        abouts.push((load.name, load.about));
    }
    println!();
    for (name, about) in &abouts {
        println!("{name:<22} {about}");
    }
    Ok(true)
}
