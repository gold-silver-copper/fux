//! Speed: every engine fed the same workloads, as a program's output
//! arrives, 4 KiB at a time, on a 50×200 screen with 10000 rows of
//! history. The workloads are modelled on alacritty's vtebench
//! (github.com/alacritty/vtebench, `benchmarks/`). An engine in this
//! process is timed on parsing and applying alone; an engine behind a
//! process of its own also pays for the pipe to it, so its figure is
//! end to end, and marked so.
use crate::engine::{ENGINES, Kind, SUBJECT, Setup};
use crate::rng::Rng;
use std::fmt::Write;
use std::time::{Duration, Instant};

const ROWS: u16 = 50;
const COLS: u16 = 200;
const CHUNK: usize = 4096;
const RUNS: usize = 3;

/// A workload: its name, what it is, and how to make `bytes` of it.
type Workload = (&'static str, &'static str, fn(&mut Rng, usize) -> Vec<u8>);

const WORKLOADS: &[Workload] = &[
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
        out.extend_from_slice(b"\x1b[H");
        for _ in 0..usize::from(ROWS).saturating_mul(usize::from(COLS)) {
            let glyph = char::from(b'A'.saturating_add(u8::try_from(r.below(26)).unwrap_or(0)));
            let _ = write!(
                Text(&mut out),
                "\x1b[38;5;{};48;5;{}m{glyph}",
                r.below(256),
                r.below(256)
            );
        }
    }
    out
}

fn medium_cells(r: &mut Rng, bytes: usize) -> Vec<u8> {
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
    let mut out = Vec::with_capacity(bytes);
    while out.len() < bytes {
        out.extend_from_slice(b"\x1b[H");
        for _ in 0..usize::from(ROWS)
            .saturating_mul(usize::from(COLS))
            .checked_div(6)
            .unwrap_or(1)
        {
            out.extend_from_slice(r.pick(&sgr).copied().unwrap_or("").as_bytes());
            out.extend_from_slice(r.pick(&words).copied().unwrap_or("").as_bytes());
        }
    }
    out
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

/// The best of [`RUNS`] times to feed `bytes` to a fresh engine.
fn time(kind: &Kind, bytes: &[u8]) -> Result<Duration, String> {
    let setup = Setup {
        rows: ROWS,
        cols: COLS,
        history: 10_000,
        reflow: true,
    };
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

/// Times fux-vt and `engines` on the named workloads (all if none), each
/// `mb` MiB, and prints MB/s as a table.
pub fn run(engines: &[usize], names: &[String], mb: usize) -> Result<bool, String> {
    let chosen: Vec<&Workload> = if names.is_empty() {
        WORKLOADS.iter().collect()
    } else {
        names
            .iter()
            .map(|n| {
                WORKLOADS.iter().find(|w| w.0 == n).ok_or(format!(
                    "no workload {n:?}; there are: {}",
                    WORKLOADS.iter().map(|w| w.0).collect::<Vec<_>>().join(" ")
                ))
            })
            .collect::<Result<_, _>>()?
    };
    let kinds: Vec<&Kind> = std::iter::once(&SUBJECT)
        .chain(engines.iter().filter_map(|&i| ENGINES.get(i)))
        .collect();
    let bytes = mb.saturating_mul(1 << 20);
    println!(
        "MB/s, best of {RUNS}, {ROWS}x{COLS}, {CHUNK}-byte chunks, {mb} MiB per workload; * = own process, end to end"
    );
    print!("{:<14}", "");
    for kind in &kinds {
        let mark = if kind.in_process { "" } else { "*" };
        print!(" {:>10}", format!("{}{mark}", kind.name));
    }
    println!();
    for (name, _, make) in chosen {
        let input = make(&mut Rng::new(1), bytes);
        print!("{name:<14}");
        for kind in &kinds {
            let cell = match time(kind, &input) {
                Ok(took) => {
                    let mbs = (input.len() as f64) / took.as_secs_f64().max(1e-9) / 1e6;
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
    for (name, about, _) in WORKLOADS {
        println!("{name:<14} {about}");
    }
    Ok(true)
}
