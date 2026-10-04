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

/// The corpus's workloads: every recording alone, and all of them in
/// turn, each `total` bytes or a little more. Each alone runs at the size
/// it was recorded at, without its resizes; all of them in turn run at the
/// first's size (40x120, as most are).
fn corpus_loads(total: usize) -> Result<Vec<Load>, String> {
    let recordings = crate::corpus::recordings(&[])?;
    let mut each = Vec::new();
    let mut all = Vec::new();
    for r in &recordings {
        let bytes = r.bytes();
        all.extend_from_slice(&bytes);
        each.push(Load {
            name: format!("corpus:{}", r.name),
            about: format!("{}, {} bytes recorded", r.version, bytes.len()),
            rows: r.rows,
            cols: r.cols,
            bytes: repeated(&bytes, total),
        });
    }
    let together = recordings.first().map(|first| Load {
        name: "corpus".into(),
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
    Ok(together.into_iter().chain(each).collect())
}

/// The best of [`RUNS`] times to feed `bytes` to a fresh engine.
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

/// Times fux-vt and `engines` on the named workloads, each `mb` MiB, and
/// prints MB/s as a table. With none named: the synthetic workloads and
/// the corpus all together; `corpus` names every corpus workload, each
/// recording alone too.
pub fn run(engines: &[usize], names: &[String], mb: usize) -> Result<bool, String> {
    let bytes = mb.saturating_mul(1 << 20);
    let synthetic = WORKLOADS.iter().map(|(name, about, make)| Load {
        name: (*name).to_owned(),
        about: (*about).to_owned(),
        rows: ROWS,
        cols: COLS,
        bytes: make(&mut Rng::new(1), bytes),
    });
    let mut every: Vec<Load> = synthetic.collect();
    every.extend(corpus_loads(bytes)?);
    let known = every
        .iter()
        .map(|l| l.name.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    for n in names {
        if !every
            .iter()
            .any(|l| &l.name == n || (n == "corpus" && l.name.starts_with("corpus")))
        {
            return Err(format!("no workload {n:?}; there are: {known}"));
        }
    }
    let chosen: Vec<&Load> = every
        .iter()
        .filter(|l| {
            if names.is_empty() {
                // By default the corpus all together, so a run stays a few
                // minutes; `corpus` adds each recording alone.
                !l.name.starts_with("corpus:")
            } else {
                names
                    .iter()
                    .any(|n| *n == l.name || (n == "corpus" && l.name.starts_with("corpus")))
            }
        })
        .collect();
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
    for load in &chosen {
        let name = &load.name;
        print!("{name:<22}");
        for kind in &kinds {
            let cell = match time(kind, load) {
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
    }
    println!();
    for load in &chosen {
        println!("{:<22} {}", load.name, load.about);
    }
    Ok(true)
}
