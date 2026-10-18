//! The safety reproducers as frozen seeds (HAR-28, `threats.md` section 2's
//! "Seeds"): each defined here once, written as a file (`seeds/*.bin`, and
//! in the `terminal` and `keys` fuzz targets' formats under
//! `fuzz/seeds/`), and replayed to check that it reaches the limit it is
//! for. That holds at BASE and after the fix alike: the seed exercises the
//! limit. Whether fux-vt stays within it is the bounds suite's row
//! (`tests/bounds.rs`), which fails at BASE for every seed here and is on
//! its known-failing list.
//!
//! `cargo test -p fux-vt --features checks --test seeds -- --ignored
//! write_the_seeds` writes the files from these definitions; a test fails
//! when a file and its definition differ.

use std::path::{Path, PathBuf};

use fux_vt::keys::decode::{Decoder, Input};
use fux_vt::{Event, Feature, Options, Parser, Sink, Size, Work};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

/// What a seed is fed after: nothing, or history filled first.
#[derive(Clone, Copy)]
enum Prefix {
    None,
    /// 349,600 line feeds: 8 MiB of empty history rows.
    EmptyHistory,
    /// This many full rows of `x`, as wide as the screen.
    FullRows(usize),
}

/// Which fuzz target's format the seed is also written in.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Fuzz {
    None,
    Terminal,
    Keys,
}

/// What a replay of a seed did: the parser after it, its work, the
/// replies and the events it sent, or the key decoder's inputs.
struct Replayed {
    parser: Parser,
    work: Work,
    events: usize,
    inputs: Vec<Input>,
}

/// A frozen seed: its file's name, the bound row it reproduces, the
/// screen it is fed to (rows, columns, history bytes), what comes first,
/// its bytes, what reaching its limit is, and its fuzz target.
struct Seed {
    name: &'static str,
    row: &'static str,
    size: (u16, u16, usize),
    prefix: Prefix,
    bytes: fn() -> Vec<u8>,
    reaches: fn(&Replayed) -> std::result::Result<(), String>,
    fuzz: Fuzz,
}

/// `bytes` `times` times over.
fn repeat(bytes: &[u8], times: usize) -> Vec<u8> {
    std::iter::repeat_n(bytes, times)
        .flatten()
        .copied()
        .collect()
}

/// `e` and `marks` U+0301s.
fn cluster(marks: usize) -> Vec<u8> {
    let marks: String = std::iter::repeat_n("\u{301}", marks).collect();
    [b"e".as_slice(), marks.as_bytes()].concat()
}

/// `Err(what)` unless `holds`.
fn ensure(holds: bool, what: impl FnOnce() -> String) -> std::result::Result<(), String> {
    if holds { Ok(()) } else { Err(what()) }
}

/// `Ok` if the screen's text holds `text`.
fn text_shows(r: &Replayed, text: &str) -> std::result::Result<(), String> {
    let shown: String = r
        .parser
        .screen()
        .main_rows()
        .map(|row| row.cells_text())
        .collect();
    ensure(shown.contains(text), || {
        format!("{text:?} not on the screen")
    })
}

/// The widest picture shown, and the tallest.
fn shown(r: &Replayed) -> (u32, u32, usize) {
    let pictures: Vec<_> = r.parser.screen().pictures().collect();
    let width = pictures
        .iter()
        .map(|p| p.picture.width())
        .max()
        .unwrap_or(0);
    let height = pictures
        .iter()
        .map(|p| p.picture.height())
        .max()
        .unwrap_or(0);
    (width, height, pictures.len())
}

/// The seeds, one a reproducer.
fn seeds() -> Vec<Seed> {
    let defaults = (50, 200, 8 << 20);
    vec![
        Seed {
            name: "b01-raster-pictures",
            row: "B01",
            size: defaults,
            prefix: Prefix::None,
            bytes: || repeat(b"\x1bPq\"1;1;4096;2160\x1b\\", 64),
            // More asked for than the budget holds: some went.
            reaches: |r| {
                let (_, _, count) = shown(r);
                ensure((1..64).contains(&count), || format!("{count} of 64 shown"))
            },
            fuzz: Fuzz::Terminal,
        },
        Seed {
            name: "b03-rightward",
            row: "B03",
            size: defaults,
            prefix: Prefix::None,
            bytes: || {
                let mut out = b"\x1bPq".to_vec();
                out.extend(repeat(b"~-", 340));
                for n in 1..=2000 {
                    out.extend(format!("$!{n}?~").bytes());
                }
                out.extend(b"\x1b\\");
                out
            },
            // A picture grown a column at a time to 2,001.
            reaches: |r| {
                let (width, _, _) = shown(r);
                ensure(width == 2001, || format!("a picture {width} wide"))
            },
            fuzz: Fuzz::Terminal,
        },
        Seed {
            name: "b03-width-ladder",
            row: "B03",
            size: defaults,
            prefix: Prefix::None,
            bytes: || {
                let mut out = b"\x1bPq".to_vec();
                for w in 1..=4096 {
                    out.extend(format!("\"1;1;{w};2160").bytes());
                }
                out.extend(b"\x1b\\");
                out
            },
            // An extent one pixel wider each raster, to the widest: kept
            // as numbers since SIX-3, so it reaches the widest picture.
            reaches: |r| {
                let (width, height, _) = shown(r);
                ensure((width, height) == (4096, 2160), || {
                    format!("a picture {width}x{height}")
                })
            },
            fuzz: Fuzz::Terminal,
        },
        Seed {
            name: "b04-aspect-repeat",
            row: "B04",
            size: defaults,
            prefix: Prefix::None,
            bytes: || {
                let mut out = b"\x1bPq\"360;1".to_vec();
                out.extend(repeat(b"!4096~$", 600));
                out.extend(b"\x1b\\");
                out
            },
            // Repeats of 4,096 at 360:1: the widest picture, 2,160 tall.
            reaches: |r| {
                let (width, height, _) = shown(r);
                ensure(width == 4096 && height == 2160, || {
                    format!("{width}x{height}")
                })
            },
            fuzz: Fuzz::Terminal,
        },
        Seed {
            name: "b04-sixel-sub",
            row: "B04",
            size: defaults,
            prefix: Prefix::None,
            bytes: || b"\x1bPq#1;2;100;0;0~\x1a~\x1b\\X".to_vec(),
            // A SUB inside sixel read, and the text after the picture.
            reaches: |r| text_shows(r, "X"),
            fuzz: Fuzz::Terminal,
        },
        Seed {
            name: "b04-sixel-can",
            row: "B04",
            size: defaults,
            prefix: Prefix::None,
            bytes: || b"\x1bPq#1;2;100;0;0~\x18X".to_vec(),
            // A CAN inside sixel read, and the text after it.
            reaches: |r| text_shows(r, "X"),
            fuzz: Fuzz::Terminal,
        },
        Seed {
            name: "b05-extents",
            row: "B05",
            size: defaults,
            prefix: Prefix::None,
            bytes: || repeat(b"\x1bPq\"1;1;4096;2160\x1b\\", 215),
            // Pictures of nothing drawn, placed: their cells written.
            reaches: |r| {
                let (_, _, count) = shown(r);
                ensure(count >= 1 && r.work.cells >= 200 * 108, || {
                    format!("{count} shown, {} cells", r.work.cells)
                })
            },
            fuzz: Fuzz::Terminal,
        },
        Seed {
            name: "b09-pictures-over-history",
            row: "B09",
            size: defaults,
            prefix: Prefix::EmptyHistory,
            bytes: || repeat(b"\x1bPq@\x1b\\", 64),
            // Pictures listed with a full history behind them.
            reaches: |r| {
                let s = r.parser.screen();
                ensure(
                    s.history_len() > 349_000 && s.pictures().count() >= 1,
                    || format!("{} history rows", s.history_len()),
                )
            },
            fuzz: Fuzz::None,
        },
        Seed {
            name: "b10-prints-rep",
            row: "B10",
            size: defaults,
            prefix: Prefix::None,
            bytes: || b"X\x1b[65535b".to_vec(),
            // Recorded up to the cap, 4 bytes a cell, at least.
            reaches: |r| {
                ensure(r.work.prints >= 39_000, || {
                    format!("{} recorded", r.work.prints)
                })
            },
            fuzz: Fuzz::Terminal,
        },
        Seed {
            name: "b11-every-cell-a-colour",
            row: "B11",
            size: (2, 10, 1 << 10),
            prefix: Prefix::None,
            bytes: || {
                let mut out = Vec::new();
                for i in 1..=5000u32 {
                    let [r, g, b, _] = i.to_le_bytes();
                    out.extend(format!("\x1b[48;2;{r};{g};{b}mx").bytes());
                }
                out
            },
            // Past the table's floor of 4,096 styles.
            reaches: |r| {
                ensure(r.work.inserts > 4096, || {
                    format!("{} styles", r.work.inserts)
                })
            },
            fuzz: Fuzz::Terminal,
        },
        Seed {
            name: "b12-colliding-styles",
            row: "B12",
            size: defaults,
            prefix: Prefix::None,
            bytes: || colliding(2000),
            // Each a style of its own in the table.
            reaches: |r| {
                ensure(r.work.inserts >= 2000, || {
                    format!("{} styles", r.work.inserts)
                })
            },
            fuzz: Fuzz::None,
        },
        Seed {
            name: "b14-short-id-links",
            row: "B14",
            size: (300, 200, 0),
            prefix: Prefix::None,
            bytes: || {
                let mut out = Vec::new();
                for i in 0..4000 {
                    out.extend(format!("\x1b]8;id={i};u\x07x\x1b]8;;\x07").bytes());
                }
                out
            },
            // Every link a cell's.
            reaches: |r| {
                let row = r.parser.screen().window().row(0);
                ensure(row.is_some_and(|row| row.link(0).is_some()), || {
                    "no link".into()
                })
            },
            fuzz: Fuzz::None,
        },
        Seed {
            name: "b15-link-runs-in-history",
            row: "B15",
            size: (50, 200, 64 << 10),
            prefix: Prefix::None,
            bytes: || {
                let mut row = repeat(b"\x1b]8;id=a;u\x07x\x1b]8;id=b;u\x07x", 100);
                row.extend(b"\r\n");
                repeat(&row, 60)
            },
            // History keeps rows with their links.
            reaches: |r| {
                let linked = r.parser.screen().rows().take(5).any(|row| row.has_links());
                ensure(linked, || "no history row with links".into())
            },
            fuzz: Fuzz::None,
        },
        Seed {
            name: "b17-wide-rows",
            row: "B17",
            size: (50, 2049, 64 << 10),
            prefix: Prefix::None,
            bytes: || repeat(&[[b'x'; 2049].as_slice(), b"\r\n"].concat(), 55),
            // Rows of 2,049 cells in history.
            reaches: |r| {
                let s = r.parser.screen();
                let wide = s.rows().next().is_some_and(|row| row.len() == 2049);
                ensure(s.history_len() >= 2 && wide, || {
                    format!("{} rows", s.history_len())
                })
            },
            fuzz: Fuzz::None,
        },
        Seed {
            name: "b17-empty-then-full-rows",
            row: "B17",
            size: defaults,
            prefix: Prefix::EmptyHistory,
            bytes: || repeat(&[[b'x'; 200].as_slice(), b"\r\n"].concat(), 600),
            // Full rows after the empty ones, history at its budget.
            reaches: |r| {
                let s = r.parser.screen();
                let newest = s.history_len().checked_sub(1).and_then(|i| s.rows().nth(i));
                let full = newest
                    .is_some_and(|row| row.cells_text().bytes().eq(std::iter::repeat_n(b'x', 200)));
                ensure(full && s.history_len() > 5000, || {
                    format!("{} rows", s.history_len())
                })
            },
            fuzz: Fuzz::None,
        },
        Seed {
            name: "b19-unfinished-sequence",
            row: "B19",
            size: defaults,
            prefix: Prefix::None,
            bytes: || {
                let mut out = b"\x1b[?2026h\x1b[".to_vec();
                out.extend(repeat(b"1", 64 << 10));
                out
            },
            // Read as a frame: it begins, and does not end in its digits.
            reaches: |r| {
                ensure(
                    r.parser.screen().mode(fux_vt::Mode::SynchronizedOutput),
                    || "no frame begun".into(),
                )
            },
            fuzz: Fuzz::Terminal,
        },
        Seed {
            name: "b20-long-osc",
            row: "B20",
            size: defaults,
            prefix: Prefix::None,
            bytes: || {
                let mut out = b"\x1b]52;c;".to_vec();
                out.extend(repeat(b"A", 65_537));
                out.extend(b"\x07");
                out
            },
            // Past the payload limit: read to its end, no event.
            reaches: |r| ensure(r.events == 0, || format!("{} events", r.events)),
            fuzz: Fuzz::Terminal,
        },
        Seed {
            name: "b21-long-apc",
            row: "B21",
            size: defaults,
            prefix: Prefix::None,
            bytes: || {
                let mut out = b"\x1b_".to_vec();
                out.extend(repeat(b"a", 64 << 10));
                out.extend(b"\x1b\\X");
                out
            },
            // An APC over many reads, held nowhere, and the text after it.
            reaches: |r| text_shows(r, "X"),
            fuzz: Fuzz::Terminal,
        },
        Seed {
            name: "b23-short-pastes",
            row: "B23",
            size: defaults,
            prefix: Prefix::None,
            bytes: || repeat(b"\x1b[200~\x1b[201~", 5461),
            // Every paste delivered.
            reaches: |r| {
                let pastes = r
                    .inputs
                    .iter()
                    .filter(|i| matches!(i, Input::Paste(_)))
                    .count();
                ensure(pastes == 5461, || format!("{pastes} pastes"))
            },
            fuzz: Fuzz::Keys,
        },
        Seed {
            name: "b25-long-csi-answer",
            row: "B25",
            size: defaults,
            prefix: Prefix::None,
            bytes: || {
                let mut out = b"\x1b[?".to_vec();
                out.extend(repeat(b"1", 300));
                out.extend(b"c");
                out
            },
            // Past every answer's window: no key typed of it, whole.
            reaches: |r| {
                let keys = r
                    .inputs
                    .iter()
                    .filter(|i| matches!(i, Input::Key(_)))
                    .count();
                ensure(keys == 0, || format!("{keys} keys typed"))
            },
            fuzz: Fuzz::Keys,
        },
        Seed {
            name: "b29-late-command-marks",
            row: "B29",
            size: defaults,
            prefix: Prefix::EmptyHistory,
            bytes: || repeat(b"\x1b]133;C;late\x07", 290),
            // Late marks with a full history behind them.
            reaches: |r| {
                let s = r.parser.screen();
                ensure(s.history_len() == 349_525, || {
                    format!("{} rows", s.history_len())
                })
            },
            fuzz: Fuzz::None,
        },
        Seed {
            name: "b31-marks-after-a-full-row",
            row: "B31",
            size: (1, 200, 0),
            prefix: Prefix::None,
            bytes: || {
                let mut out = repeat(&cluster(50), 64);
                out.extend(cluster(22));
                out.extend(cluster(8));
                out.extend(repeat("\u{200b}".as_bytes(), 1000));
                out
            },
            // The row's long text at its limit, within 20 bytes.
            reaches: |r| {
                let row = r.parser.screen().window().row(0);
                let held = row.map_or(0, |row| row.text_len());
                ensure(held >= 6_500, || format!("{held} bytes of text"))
            },
            fuzz: Fuzz::None,
        },
        Seed {
            name: "b32-u17d8",
            row: "B32",
            size: (1, 10, 0),
            prefix: Prefix::None,
            bytes: || "XYZW\r\u{17d8}".as_bytes().to_vec(),
            // The character printed in the first cell.
            reaches: |r| {
                let cell = r.parser.screen().cell(0, 0);
                ensure(cell.is_some_and(|c| c.contents() == "\u{17d8}"), || {
                    "U+17D8 not printed".into()
                })
            },
            fuzz: Fuzz::Terminal,
        },
        Seed {
            name: "b33-rep-small-screen",
            row: "B33",
            size: (2, 10, 1 << 10),
            prefix: Prefix::None,
            bytes: || b"X\x1b[65535b".to_vec(),
            // Copies past the screen and history: history full.
            reaches: |r| {
                let rows = r.parser.screen().history_len();
                ensure(rows >= 9, || format!("{rows} history rows"))
            },
            fuzz: Fuzz::Terminal,
        },
        Seed {
            name: "b34-wide-rep",
            row: "B34",
            size: defaults,
            prefix: Prefix::None,
            bytes: || repeat("一\x1b[65535b".as_bytes(), 372),
            // Every copy of a wide glyph printed or skipped.
            reaches: |r| {
                ensure(r.work.cells >= 65_535 * 2, || {
                    format!("{} cells", r.work.cells)
                })
            },
            fuzz: Fuzz::Terminal,
        },
        Seed {
            name: "b34a-self-joining-rep",
            row: "B34a",
            size: defaults,
            prefix: Prefix::None,
            bytes: || repeat("\u{1100}\x1b[65535b".as_bytes(), 372),
            // The jamo's cluster full: 42 of them, 126 bytes.
            reaches: |r| {
                let cell = r.parser.screen().cell(0, 0);
                let held = cell.map_or(0, |c| c.contents().len());
                ensure(held >= 126, || format!("a cluster of {held} bytes"))
            },
            fuzz: Fuzz::Terminal,
        },
        Seed {
            name: "b35-deccra-over-clusters",
            row: "B35",
            size: defaults,
            prefix: Prefix::None,
            bytes: || {
                let mut out = Vec::new();
                for _ in 0..10 {
                    out.extend(repeat(&cluster(8), 200));
                    out.extend(b"\r\n");
                }
                out.extend(repeat(b"\x1b[$v", 256));
                out
            },
            // The clusters copied over themselves, there still.
            reaches: |r| {
                let cell = r.parser.screen().cell(0, 0);
                ensure(cell.is_some_and(|c| c.contents().len() == 17), || {
                    "the cluster went".into()
                })
            },
            fuzz: Fuzz::None,
        },
        Seed {
            name: "b35-decaln",
            row: "B35",
            size: defaults,
            prefix: Prefix::None,
            bytes: || repeat(b"x\x1b#8", 1024),
            // Every cell E.
            reaches: |r| {
                let s = r.parser.screen();
                let all = (0..50)
                    .all(|y| (0..200).all(|x| s.cell(y, x).is_some_and(|c| c.contents() == "E")));
                ensure(all, || "a cell not E".into())
            },
            fuzz: Fuzz::Terminal,
        },
        Seed {
            name: "b36-insert-lines",
            row: "B36",
            size: (4095, 1, 0),
            prefix: Prefix::None,
            bytes: || b"\x1b[4095L".to_vec(),
            // The region's rows moved.
            reaches: |r| ensure(r.work.slots >= 4095, || format!("{} slots", r.work.slots)),
            fuzz: Fuzz::Terminal,
        },
        Seed {
            name: "b37-rejected-resize",
            row: "B37",
            size: defaults,
            prefix: Prefix::FullRows(6000),
            bytes: || b"\x1b[?2026h".to_vec(),
            // A resize past the limit, with history full and a frame begun:
            // refused.
            reaches: |r| {
                let mut parser = r.parser.clone();
                let refused =
                    Size::new(u16::MAX, u16::MAX).is_ok_and(|s| parser.resize(s).is_err());
                ensure(refused, || "the resize was taken".into())
            },
            fuzz: Fuzz::None,
        },
        Seed {
            name: "b40-lookup-over-history",
            row: "B40",
            size: defaults,
            prefix: Prefix::EmptyHistory,
            bytes: || b"x".to_vec(),
            // The screen's top row found by its identity past a full history.
            reaches: |r| {
                let s = r.parser.screen();
                let top = s.window().row(0).map(|row| row.id());
                let found = top.is_some_and(|id| s.row_by_id(id).is_some());
                ensure(found && s.history_len() == 349_525, || "not found".into())
            },
            fuzz: Fuzz::None,
        },
    ]
}

/// rust-opus's family of styles whose attributes hash alike at BASE
/// (`collide.py`): `count` SGRs, an `x` each, 100 a line.
fn colliding(count: usize) -> Vec<u8> {
    let pk = |t: u64, r: u64, g: u64, b: u64| t | r << 8 | g << 16 | b << 24;
    let fg = pk(2, 10, 20, 30);
    let (mut x, mut out, mut made) = (None, Vec::new(), 0usize);
    'all: for ug in (0..256u64).filter(|ug| ug.is_multiple_of(2)) {
        for ub in 0..256u64 {
            if made == count {
                break 'all;
            }
            let rb = pk(2, 0, ug, ub).rotate_left(23);
            let x = *x.get_or_insert((fg | pk(2, 0, 0, 0) << 32) ^ rb);
            let bg = (x ^ rb) >> 32;
            let (r, g, b) = ((bg >> 8) & 255, (bg >> 16) & 255, bg >> 24);
            out.extend(format!("\x1b[38;2;10;20;30;48;2;{r};{g};{b};58;2;0;{ug};{ub}mx").bytes());
            made = made.saturating_add(1);
            if made.is_multiple_of(100) {
                out.extend(b"\r\n");
            }
        }
    }
    out
}

/// Counts the events a parser sends.
#[derive(Default)]
struct Events(usize);
impl Sink for Events {
    fn event(&mut self, _: Event<'_>) {
        self.0 = self.0.saturating_add(1);
    }
}

/// `bytes` in pieces of 4 KiB, as fux feeds a pane.
fn pieces(mut rest: &[u8]) -> impl Iterator<Item = &[u8]> {
    std::iter::from_fn(move || {
        let (piece, after) = rest.split_at_checked(4096).unwrap_or((rest, &[]));
        rest = after;
        (!piece.is_empty()).then_some(piece)
    })
}

/// Replays `seed` on its screen, every feature on and prints recorded, in
/// 4 KiB pieces after its prefix; a key seed through the key decoder.
fn replay(seed: &Seed) -> Result<Replayed> {
    let (rows, cols, history) = seed.size;
    let options: Options = Feature::ALL.into_iter().collect();
    let mut parser = Parser::with_options(Size::new(rows, cols)?, history, options)?;
    parser.record_prints(true);
    let prefix = match seed.prefix {
        Prefix::None => Vec::new(),
        Prefix::EmptyHistory => repeat(b"\n", 349_600),
        Prefix::FullRows(n) => {
            let row = [repeat(b"x", usize::from(cols)).as_slice(), b"\r\n"].concat();
            repeat(&row, n)
        }
    };
    for piece in pieces(&prefix) {
        parser.process(piece)?;
    }
    let bytes = (seed.bytes)();
    let (before, mut events, mut inputs) = (Work::now(), Events::default(), Vec::new());
    if seed.fuzz == Fuzz::Keys {
        let mut decoder = Decoder::default();
        decoder.bytes(&bytes, &mut inputs);
        decoder.timeout(&mut inputs);
    } else {
        for piece in pieces(&bytes) {
            parser.process_with(piece, &mut events)?;
        }
    }
    Ok(Replayed {
        parser,
        work: Work::now().since(before),
        events: events.0,
        inputs,
    })
}

/// The `terminal` fuzz target's input for `bytes`: a header for its
/// largest screen (16 by 24, 960 bytes of history) with every option it
/// has on, then the bytes as output, 253 at a time.
fn terminal_input(bytes: &[u8]) -> Vec<u8> {
    // Rows 1 + 0xff mod 16, columns 1 + 0xef mod 24, history 0xff mod 16 a
    // 64 bytes; the high bits every option (fuzz/README.md).
    let mut out = vec![0xff, 0xef, 0xff];
    let mut rest = bytes;
    while let Some((piece, after)) = rest
        .split_at_checked(253)
        .or_else(|| (!rest.is_empty()).then_some((rest, &[][..])))
    {
        let [len, ..] = (u32::try_from(piece.len()).unwrap_or(1).saturating_sub(1)).to_le_bytes();
        out.push(len);
        out.extend_from_slice(piece);
        rest = after;
    }
    out
}

/// The files a seed is written as: its bytes, and its fuzz target's input.
fn files(seed: &Seed) -> Vec<(PathBuf, Vec<u8>)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let bytes = (seed.bytes)();
    let mut files = vec![(
        root.join("tests/seeds").join(format!("{}.bin", seed.name)),
        bytes.clone(),
    )];
    match seed.fuzz {
        Fuzz::None => {}
        Fuzz::Terminal => files.push((
            root.join("fuzz/seeds/terminal").join(seed.name),
            terminal_input(&bytes),
        )),
        // The keys target's first byte is the piece size: 0, whole.
        Fuzz::Keys => files.push((
            root.join("fuzz/seeds/keys").join(seed.name),
            [[0u8].as_slice(), &bytes].concat(),
        )),
    }
    files
}

/// Each seed reaches the limit it is for.
#[test]
fn every_seed_reaches_its_limit() -> Result {
    for seed in seeds() {
        let replayed = replay(&seed)?;
        (seed.reaches)(&replayed).map_err(|e| format!("{} ({}): {e}", seed.name, seed.row))?;
    }
    Ok(())
}

/// The files are what the definitions make, and every reproducer has one:
/// names are unique, and each names a row of `threats.md`.
#[test]
fn the_seed_files_are_written_as_defined() -> Result {
    let seeds = seeds();
    let mut names: Vec<&str> = seeds.iter().map(|s| s.name).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), seeds.len());
    for seed in &seeds {
        assert!(seed.row.starts_with('B') && seed.name.starts_with(&seed.row.to_lowercase()));
        for (path, bytes) in files(seed) {
            let written = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            assert!(
                written == bytes,
                "{} differs from its seed: write the seeds",
                path.display()
            );
        }
    }
    Ok(())
}

/// Writes every seed's files.
#[test]
#[ignore = "writes the seed files: run it after changing a seed"]
fn write_the_seeds() -> Result {
    for seed in seeds() {
        for (path, bytes) in files(&seed) {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(&path, bytes)?;
        }
    }
    Ok(())
}
