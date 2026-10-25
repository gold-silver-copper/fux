//! The safety reproducers as frozen seeds (`THREATS.md` section 2): each
//! defined here once, written as a fuzz input (in the `terminal` and
//! `keys` fuzz targets' formats under `fuzz/seeds/`; a `terminal` seed
//! past that target's 4 KiB runs in `deep`'s, if one of its runs holds
//! it), and replayed to check that it reaches the limit it is for. The
//! seed exercises the limit; whether fux-vt stays within it is the bounds
//! suite's row (`tests/bounds.rs`), and `bounds/known-failing.txt` names
//! the rows that fail.
//!
//! A row whose reproducer is a host call (a clone, a picture built or
//! encoded, a size, a refused allocation, an encoder) has its seed's host
//! calls in `reaches`, and in `deep`'s format where that target makes the
//! call. A row whose bound no defect ever broke (kept by a guard) says so,
//! "a guard: no defect", beside its seed.
//!
//! `cargo test -p fux-vt --features checks --test seeds -- --ignored
//! write_the_seeds` writes the fuzz inputs from these definitions.

use std::path::{Path, PathBuf};

use fux_vt::keys::decode::{Decoder, Input};
use fux_vt::sixel::Picture;
use fux_vt::{Event, Feature, Options, Parser, Sink, Size, Work};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

/// The empty rows 8 MiB of history holds: 682 of its ring's chunks of 512
/// rows (12,288 bytes each, `THREATS.md` B17), the newest filling and the
/// oldest let go once all its rows have gone.
const FULL: std::ops::RangeInclusive<usize> = 681 * 512 + 1..=682 * 512;

/// What a seed is fed after: nothing, or history filled first.
#[derive(Clone, Copy)]
enum Prefix {
    None,
    /// 349,600 line feeds: 8 MiB of empty history rows, as many as its
    /// ring's chunks of 512 hold (`FULL`).
    EmptyHistory,
    /// This many full rows of `x`, as wide as the screen.
    FullRows(usize),
    /// 2,000 links of 2,083-byte URIs, 4 MiB of links held (B16).
    HeldLinks,
    /// An `x`, then this many of the allocations fux-vt makes room for
    /// refused (`refuse_next`); a call refused is no failure of the replay.
    Refused(u32),
}

/// Which fuzz target's format the seed is also written in.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Fuzz {
    None,
    Terminal,
    Keys,
    /// `deep`'s format, with these host operations after the output (the
    /// bytes in deep's operations), for a reproducer of host calls.
    Deep(&'static [u8]),
}

/// What a replay of a seed did: the parser after it, its work, the
/// replies and the events it sent, or the key decoder's inputs.
struct Replayed {
    parser: Parser,
    work: Work,
    events: usize,
    replies: Vec<Vec<u8>>,
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
            // as numbers, so it reaches the widest picture.
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
                    FULL.contains(&s.history_len()) && s.pictures().count() >= 1,
                    || format!("{} history rows", s.history_len()),
                )
            },
            fuzz: Fuzz::None,
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
            // Past the payload limit: read to its end, one event, which
            // says it was cut (`Kept::cut`).
            reaches: |r| ensure(r.events == 1, || format!("{} events", r.events)),
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
            name: "b24-held-escape",
            row: "B24",
            size: defaults,
            prefix: Prefix::None,
            // An ESC that ends the first 4 KiB read, held, then reads far
            // longer than any sequence.
            bytes: || {
                [
                    repeat(b"b", 4095).as_slice(),
                    b"\x1b",
                    &repeat(b"a", 64 << 10),
                ]
                .concat()
            },
            // Every b, Alt-a, and every other a typed.
            reaches: |r| {
                let keys = r
                    .inputs
                    .iter()
                    .filter(|i| matches!(i, Input::Key(_)))
                    .count();
                ensure(keys == 4095 + (64 << 10), || format!("{keys} keys typed"))
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
            fuzz: Fuzz::Terminal,
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
            fuzz: Fuzz::Terminal,
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
                ensure(found && FULL.contains(&s.history_len()), || {
                    "not found".into()
                })
            },
            fuzz: Fuzz::None,
        },
        Seed {
            name: "b02-65-placements",
            row: "B02",
            size: defaults,
            prefix: Prefix::None,
            bytes: || {
                let mut out = Vec::new();
                // Rows 1 and 3, every other column.
                let cells = [1u16, 3].into_iter().flat_map(|row| {
                    (0..50u16).map(move |c| (row, c.saturating_mul(2).saturating_add(1)))
                });
                for (row, col) in cells.take(65) {
                    out.extend(format!("\x1b[{row};{col}H\x1bPq@\x1b\\").bytes());
                }
                out
            },
            // 65 placements, each in a cell of its own: the 65th lets the
            // oldest go, 64 shown. Once
            // past its bound at 10x: placing checked every kept picture
            // through history.
            reaches: |r| {
                let (_, _, count) = shown(r);
                ensure(count == 64, || format!("{count} of 65 shown"))
            },
            fuzz: Fuzz::Terminal,
        },
        Seed {
            name: "b06-clone-mid-picture",
            row: "B06",
            size: defaults,
            prefix: Prefix::None,
            bytes: || b"\x1bPq\"1;1;4096;2160~".to_vec(),
            // A parser cloned mid-picture draws the same picture as the
            // original; a host's copy of a picture outlives its eviction,
            // which a lowered budget makes. A guard: no defect.
            reaches: |r| {
                let (mut a, mut b) = (r.parser.clone(), r.parser.clone());
                for p in [&mut a, &mut b] {
                    p.process(b"\x1b\\").map_err(|e| e.to_string())?;
                }
                let size = |p: &Parser| {
                    let shown = p.screen().pictures().next();
                    shown.map(|s| (s.picture.width(), s.picture.height()))
                };
                ensure(
                    size(&a) == Some((4096, 2160)) && size(&a) == size(&b),
                    || format!("pictures {:?} and {:?}", size(&a), size(&b)),
                )?;
                let kept = a.screen().pictures().next().map(|s| s.picture.clone());
                a.set_picture_bytes(0);
                ensure(
                    a.screen().pictures().count() == 0 && kept.is_some_and(|p| p.width() == 4096),
                    || "the budget kept the picture, or the host's copy went".into(),
                )
            },
            // Drawn, encoded by the host, let go by a budget of none, read
            // again (deep's `01 ESC \`, `f6`, `f4 00`, `f6`).
            fuzz: Fuzz::Deep(&[0x01, 0x1b, b'\\', 0xf6, 0xf4, 0x00, 0xf6]),
        },
        Seed {
            name: "b07-picture-sizes",
            row: "B07",
            size: defaults,
            prefix: Prefix::None,
            bytes: Vec::new,
            // `Picture::new` takes exactly w × h × 4 bytes and sides up to
            // 4,096; an empty u32::MAX square, whose crop `encode` once
            // never finished, is refused. Host calls only: `deep`
            // makes no picture of its own.
            reaches: |_| {
                let made = |w: u32, h: u32, len: usize| Picture::new(w, h, vec![0; len]).is_some();
                let all = [
                    !made(u32::MAX, u32::MAX, 0),
                    !made(5, 3, 59),
                    made(5, 3, 60),
                    !made(5, 3, 61),
                    made(4096, 1, 4096 * 4),
                    !made(4097, 1, 4097 * 4),
                ];
                ensure(all.iter().all(|ok| *ok), || format!("{all:?}"))
            },
            fuzz: Fuzz::None,
        },
        Seed {
            name: "b08-encode-colours",
            row: "B08",
            size: defaults,
            prefix: Prefix::None,
            bytes: || {
                // 256 colours, a column each, two bands: a band of 256
                // colours.
                let mut out = b"\x1bPq".to_vec();
                for c in 0..256u32 {
                    let [g, ..] = c.to_le_bytes();
                    out.extend(format!("#{c};2;{};{};{}", g % 101, (g / 3) % 101, 100).bytes());
                }
                for _ in 0..2 {
                    for c in 0..256u32 {
                        out.extend(format!("#{c}~").bytes());
                    }
                    out.extend(b"-");
                }
                out.extend(b"\x1b\\");
                out
            },
            // Encoded whole within its bound: 12 bytes a pixel and 6 a
            // colour a band, besides the colours' definitions, and C_ENC
            // = 3 units of work a pixel. A guard: no defect.
            reaches: |r| {
                let shown = r.parser.screen().pictures().next().ok_or("no picture")?;
                let (w, h) = (shown.picture.width(), shown.picture.height());
                let mut out = Vec::new();
                let before = Work::now();
                fux_vt::sixel::encode(shown.picture, (0, 0, w, h), &mut out);
                let done = Work::now().since(before);
                let pixels = u64::from(w).saturating_mul(u64::from(h));
                let bands = u64::from(h.div_ceil(6));
                let most = (pixels.saturating_mul(12))
                    .saturating_add(bands.saturating_mul(256 * 6))
                    .saturating_add(256 * 20 + 64);
                let len = u64::try_from(out.len()).unwrap_or(u64::MAX);
                ensure(
                    w >= 256 && len > 0 && len <= most && done.pixels <= pixels.saturating_mul(3),
                    || format!("{w}x{h}: {len} bytes, {} units", done.pixels),
                )
            },
            fuzz: Fuzz::Deep(&[0xf6]),
        },
        Seed {
            name: "b13-style-sweep",
            row: "B13",
            size: defaults,
            prefix: Prefix::None,
            bytes: || {
                let mut out = b"\x1b[H".to_vec();
                for i in 1..=5000u32 {
                    let [r, g, b, _] = i.to_le_bytes();
                    out.extend(format!("\x1b[48;2;{r};{g};{b}mx\x1b[D").bytes());
                }
                out
            },
            // Past the table's floor with one cell live: a sweep came. Once
            // past its heap bound at 10x.
            reaches: |r| {
                ensure(r.work.inserts > 4096 && r.work.styles > 0, || {
                    format!("{} styles, {} swept", r.work.inserts, r.work.styles)
                })
            },
            fuzz: Fuzz::Terminal,
        },
        Seed {
            name: "b16-links-past-the-held",
            row: "B16",
            size: (50, 200, 0),
            prefix: Prefix::HeldLinks,
            bytes: || repeat(b"\x1b]8;;x\x07y\x1b]8;;\x07", 1000),
            // With 4 MiB of links held, new links make room by a sweep.
            // A guard: no defect. The prefix is no fuzz input.
            reaches: |r| {
                ensure(r.work.links >= 1000, || {
                    format!("{} link steps", r.work.links)
                })
            },
            fuzz: Fuzz::None,
        },
        Seed {
            name: "b18-line-feeds-into-history",
            row: "B18",
            size: defaults,
            prefix: Prefix::None,
            bytes: || repeat(b"\n", 5_000),
            // Each line feed at the bottom a row into history. A guard: no
            // defect.
            reaches: |r| ensure(r.work.rows >= 4_950, || format!("{} rows", r.work.rows)),
            fuzz: Fuzz::Terminal,
        },
        Seed {
            name: "b22-many-parameters",
            row: "B22",
            size: defaults,
            prefix: Prefix::None,
            bytes: || {
                let mut out = b"\x1b[".to_vec();
                out.extend(repeat(b"1;", 1000));
                out.extend(b"m");
                out
            },
            // Past the parameters kept, the sequence still carried out
            // with those it kept (DEC STD 070: extra parameters are
            // ignored): bold. A guard: no defect.
            reaches: |r| {
                ensure(r.parser.screen().attributes().bold(), || {
                    "the SGR was not carried out".into()
                })
            },
            fuzz: Fuzz::Terminal,
        },
        Seed {
            name: "b26-single-keys",
            row: "B26",
            size: defaults,
            prefix: Prefix::None,
            bytes: || repeat(b"a", 64 << 10),
            // A key an input byte, every one. A guard: no defect.
            reaches: |r| {
                let keys = r
                    .inputs
                    .iter()
                    .filter(|i| matches!(i, Input::Key(_)))
                    .count();
                ensure(keys == 64 << 10, || format!("{keys} keys typed"))
            },
            fuzz: Fuzz::Keys,
        },
        Seed {
            name: "b27-kitty-stack",
            row: "B27",
            size: defaults,
            prefix: Prefix::None,
            bytes: || {
                let mut out = b"\x1b[>1u".to_vec();
                out.extend(repeat(b"\x1b[>2u", 32));
                out.extend(b"\x1b[<32u");
                out
            },
            // 33 pushes into a stack of 32 evict the oldest, so 32 pops
            // empty it and reset the flags (the kitty keyboard protocol).
            // A guard: no defect.
            reaches: |r| {
                let flags = r.parser.screen().kitty_keyboard_flags();
                ensure(flags == 0, || format!("flags {flags} after the pops"))
            },
            fuzz: Fuzz::Terminal,
        },
        Seed {
            name: "b28-setting-replies",
            row: "B28",
            size: defaults,
            prefix: Prefix::None,
            bytes: || {
                let mut out = b"\x1b[1;2;3;4:3;5;7;8;9;38;2;1;2;3;48;2;4;5;6;58;2;7;8;9m".to_vec();
                out.extend(repeat(b"\x1bP$qm\x1b\\", 585));
                out
            },
            // Every request answered with the longest pen, each answer
            // within 16 bytes a byte that asked. A guard: no
            // defect.
            reaches: |r| {
                let longest = r.replies.iter().map(Vec::len).max().unwrap_or(0);
                ensure(
                    r.replies.len() == 585 && longest > 20 && longest <= 7 * 16,
                    || format!("{} replies, the longest {longest}", r.replies.len()),
                )
            },
            fuzz: Fuzz::Terminal,
        },
        Seed {
            name: "b30-long-cluster",
            row: "B30",
            size: defaults,
            prefix: Prefix::None,
            bytes: || cluster(64),
            // A cell keeps 128 bytes at most: e and 63 of the 64 marks,
            // 127 bytes. A guard: no defect.
            reaches: |r| {
                let cell = r.parser.screen().cell(0, 0);
                let held = cell.map_or(0, |c| c.contents().len());
                ensure(held == 127, || format!("a cluster of {held} bytes"))
            },
            fuzz: Fuzz::Terminal,
        },
        Seed {
            name: "b38-largest-screen",
            row: "B38",
            size: defaults,
            prefix: Prefix::None,
            bytes: Vec::new,
            // 64 Mi cells fit, one row more does not, and a 65535 by 65535
            // parser is refused. Host calls only: `deep`'s sides are at
            // most 511. A guard: no defect.
            reaches: |_| {
                let fits = |rows, cols| Size::new(rows, cols).is_ok_and(fux_vt::fits);
                let refused =
                    Size::new(u16::MAX, u16::MAX).is_ok_and(|s| Parser::new(s, 0).is_err());
                ensure(fits(8192, 8192) && !fits(8192, 8193) && refused, || {
                    "a size past 64 Mi cells was taken".into()
                })
            },
            fuzz: Fuzz::None,
        },
        Seed {
            name: "b39-cells-past-the-most",
            row: "B39",
            size: defaults,
            prefix: Prefix::None,
            bytes: Vec::new,
            // `Cells::new` and `resize` past 64 Mi cells are refused, and
            // `resize` leaves the cells as they were; `Cells::new(usize::
            // MAX)` once panicked. Host calls only.
            reaches: |_| {
                let past = [(64 << 20) + 1, usize::MAX];
                let refused = past.iter().all(|&len| {
                    let new = fux_vt::Cells::new(len).is_err();
                    let resized = fux_vt::Cells::new(4).is_ok_and(|mut cells| {
                        cells.resize(len, fux_vt::CellRef::default()).is_err() && cells.len() == 4
                    });
                    new && resized
                });
                ensure(refused, || "Cells past 64 Mi made".into())
            },
            fuzz: Fuzz::None,
        },
        Seed {
            name: "b41-push-with-clear",
            row: "B41",
            size: defaults,
            prefix: Prefix::None,
            bytes: Vec::new,
            // A `push_with` closure that clears the queue under what was
            // taken, then a push: the queue holds the push. It once
            // held nothing. Host calls only.
            reaches: |_| {
                let mut queue = fux_vt::bytes::ByteQueue::default();
                queue.push(b"abcdef");
                queue.take(2);
                queue.push_with(Vec::clear);
                queue.push(b"xy");
                ensure(queue.as_slice() == b"xy", || {
                    format!("the queue holds {:?}", queue.as_slice())
                })
            },
            fuzz: Fuzz::None,
        },
        Seed {
            name: "b42-tab-stops",
            row: "B42",
            size: defaults,
            prefix: Prefix::None,
            bytes: || {
                let mut out = b"\x1b[H".to_vec();
                out.extend(repeat(b"\x1bH\x1b[C", 200));
                out.extend(b"\r\tX");
                out
            },
            // A tab stop at every column: a tab from the first goes to
            // the second. A guard: no defect.
            reaches: |r| {
                let cell = r.parser.screen().cell(0, 1);
                ensure(cell.is_some_and(|c| c.contents() == "X"), || {
                    "the tab went past the second column".into()
                })
            },
            fuzz: Fuzz::Terminal,
        },
        Seed {
            name: "b43-notifications",
            row: "B43",
            size: defaults,
            prefix: Prefix::None,
            bytes: || repeat(b"\x1b]9;hello\x07", 400),
            // Each notification an event, none kept. A guard: no
            // defect.
            reaches: |r| ensure(r.events == 400, || format!("{} events", r.events)),
            fuzz: Fuzz::Terminal,
        },
        Seed {
            name: "b44-refused-mark",
            row: "B44",
            size: (1, 10, 1 << 20),
            prefix: Prefix::Refused(1),
            bytes: || b"\x1b]133;A\x07hello".to_vec(),
            // A prompt mark's fresh line scrolls into history, whose first
            // block is refused: one refusal, and the text after it shown.
            // `hello` once went into the OSC. `deep` refuses nothing.
            reaches: |r| {
                text_shows(r, "hello")?;
                let refused = r.parser.refused();
                ensure(refused == 1, || format!("{refused} refusals"))
            },
            fuzz: Fuzz::None,
        },
        Seed {
            name: "b45-longest-key",
            row: "B45",
            size: defaults,
            prefix: Prefix::None,
            bytes: Vec::new,
            // The longest keys: a kitty report of the largest code points
            // with every modifier bit, and an SGR mouse report at the
            // largest position, each within 64 bytes, a maximum
            // once stated nowhere. Host calls only.
            reaches: |_| {
                use fux_vt::keys::encode::{KeyMode, Report, key_bytes};
                use fux_vt::keys::mouse::{MouseAction, MouseButton, MouseEvent, mouse_bytes};
                use fux_vt::keys::{Key, KeyPress, Keystroke, Kitty, Modifiers};
                use fux_vt::{MouseProtocolEncoding, MouseProtocolMode};
                let mods = Modifiers {
                    shift: true,
                    alt: true,
                    ctrl: true,
                };
                let stroke = Keystroke {
                    press: KeyPress::new(Key::Char('\u{10ffff}'), mods),
                    kitty: Some(Kitty {
                        code: Some(0x10_ffff),
                        shifted: Some(0x10_ffff),
                        base: Some(0x10_ffff),
                        mods: u8::MAX,
                    }),
                };
                let mode = KeyMode::Kitty {
                    alternate: true,
                    report: Report::AllKeysWithText,
                    backarrow: false,
                };
                let (mut key, mut mouse) = (Vec::new(), Vec::new());
                key_bytes(stroke, mode, &mut key);
                let event = MouseEvent {
                    action: MouseAction::Motion(Some(MouseButton::ALL[0])),
                    mods,
                    row: u16::MAX,
                    col: u16::MAX,
                };
                let (any, sgr) = (MouseProtocolMode::AnyMotion, MouseProtocolEncoding::Sgr);
                mouse_bytes(event, any, sgr, &mut mouse);
                let within = |out: &Vec<u8>| (1..=64).contains(&out.len());
                ensure(within(&key) && within(&mouse), || {
                    format!("{} and {} bytes", key.len(), mouse.len())
                })
            },
            fuzz: Fuzz::None,
        },
    ]
}

/// A family of styles whose attributes hashed alike unkeyed: `count`
/// SGRs, an `x` each, 100 a line.
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

/// Counts the events a parser sends, and keeps its replies.
#[derive(Default)]
struct Events(usize, Vec<Vec<u8>>);
impl Sink for Events {
    fn event(&mut self, _: Event<'_>) {
        self.0 = self.0.saturating_add(1);
    }
    fn reply(&mut self, bytes: &[u8]) {
        self.1.push(bytes.to_vec());
    }
}

/// A link of a 2,083-byte URI over one `x`, then closed: 2,000 of them
/// hold 4 MiB of links (B16).
fn held_link() -> Vec<u8> {
    let mut out = b"\x1b]8;;".to_vec();
    out.extend(repeat(b"u", 2083));
    out.extend(b"\x07x\x1b]8;;\x07");
    out
}

/// `bytes` in pieces of 4 KiB, as fux feeds a pane.
fn pieces(mut rest: &[u8]) -> impl Iterator<Item = &[u8]> {
    std::iter::from_fn(move || {
        let (piece, after) = rest.split_at_checked(4096).unwrap_or((rest, &[]));
        rest = after;
        (!piece.is_empty()).then_some(piece)
    })
}

/// Replays `seed` on its screen, every feature on and the host's cells 10
/// by 20 pixels (pictures are placed only in a cell size the host gave), in
/// 4 KiB pieces after its prefix; a key seed through the key decoder, in
/// 4 KiB reads too.
fn replay(seed: &Seed) -> Result<Replayed> {
    let (rows, cols, history) = seed.size;
    let options: Options = Feature::ALL.into_iter().collect();
    let mut parser = Parser::with_options(Size::new(rows, cols)?, history, options)?;
    parser.set_host_graphics(fux_vt::Graphics {
        cell: fux_vt::CellSize::new(10, 20),
        sixel: true,
    });
    let prefix = match seed.prefix {
        Prefix::None => Vec::new(),
        Prefix::EmptyHistory => repeat(b"\n", 349_600),
        Prefix::FullRows(n) => {
            let row = [repeat(b"x", usize::from(cols)).as_slice(), b"\r\n"].concat();
            repeat(&row, n)
        }
        Prefix::HeldLinks => repeat(&held_link(), 2000),
        Prefix::Refused(_) => b"x".to_vec(),
    };
    for piece in pieces(&prefix) {
        parser.process(piece)?;
    }
    if let Prefix::Refused(n) = seed.prefix {
        fux_vt::refuse_next(n);
    }
    let bytes = (seed.bytes)();
    let (before, mut events, mut inputs) = (Work::now(), Events::default(), Vec::new());
    if seed.fuzz == Fuzz::Keys {
        let mut decoder = Decoder::default();
        for piece in pieces(&bytes) {
            decoder.bytes(piece, &mut inputs);
        }
        decoder.timeout(&mut inputs);
    } else {
        for piece in pieces(&bytes) {
            let fed = parser.process_with(piece, &mut events);
            if !matches!(seed.prefix, Prefix::Refused(_)) {
                fed?;
            }
        }
    }
    fux_vt::refuse_next(0);
    Ok(Replayed {
        parser,
        work: Work::now().since(before),
        events: events.0,
        replies: events.1,
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

/// The most bytes of input the `terminal` target is run with (`-max_len`,
/// fuzz.yml); a longer seed goes to `deep`, whose is 64 KiB.
const TERMINAL_MAX: usize = 4096;

/// The `deep` fuzz target's input for `output` at `size` (rows, columns,
/// history bytes): a header for the nearest screen it makes (1 + 2h a
/// side) and the history budget at least `size`'s, every option on; then `output` as its operations: a run of a pattern of up to
/// 256 bytes repeated 16 times or more as `f2 n k`, the rest as plain
/// output of up to 241 bytes an operation. `None` past what one run takes:
/// 128 operations, 1 MiB of output, 64 KiB of input (fuzz/README.md).
fn deep_input((rows, cols, history): (u16, u16, usize), output: &[u8]) -> Option<Vec<u8>> {
    const BUDGETS: [usize; 5] = [0, 1 << 10, 64 << 10, 1 << 20, 8 << 20];
    let half = |n: u16| u8::try_from(n.saturating_sub(1) / 2).ok();
    let budget = BUDGETS.iter().position(|b| *b >= history).unwrap_or(4);
    let mut out = vec![
        half(rows)?,
        half(cols)?,
        u8::try_from(budget).ok()?,
        0xff,
        0xff,
        0,
    ];
    let (mut rest, mut ops) = (output, 0usize);
    while !rest.is_empty() {
        ops = ops.saturating_add(1);
        // The pattern at the start repeated most, if 16 times or more.
        let repeats = |p: usize| {
            let unit = rest.get(..p)?;
            let copy =
                |i: &usize| rest.get(i.saturating_mul(p)..i.saturating_add(1).saturating_mul(p));
            let n = (0..).take_while(|i| copy(i) == Some(unit)).count();
            (n >= 16).then_some((p, n))
        };
        let most = 256.min(rest.len() / 16);
        let pattern = (1..=most)
            .filter_map(repeats)
            .max_by_key(|&(p, n)| p.saturating_mul(n));
        if let Some((p, n)) = pattern {
            let times = (n / 16).min(256);
            let (n, k) = (times.saturating_sub(1), p.saturating_sub(1));
            out.extend([0xf2, u8::try_from(n).ok()?, u8::try_from(k).ok()?]);
            out.extend_from_slice(rest.get(..p)?);
            rest = rest.get(p.saturating_mul(times).saturating_mul(16)..)?;
        } else {
            let (piece, after) = rest.split_at_checked(241).unwrap_or((rest, &[]));
            out.push(u8::try_from(piece.len().saturating_sub(1)).ok()?);
            out.extend_from_slice(piece);
            rest = after;
        }
    }
    (ops <= 128 && output.len() <= 1 << 20 && out.len() <= 64 << 10).then_some(out)
}

/// The output a `deep` input feeds after its header, as the target reads
/// its operations: for checking that a seed's file holds its bytes.
fn deep_output(input: &[u8]) -> Vec<u8> {
    let (mut rest, mut out) = (input.get(6..).unwrap_or_default(), Vec::new());
    while let Some((&op, tail)) = rest.split_first() {
        rest = tail;
        let (once, times, taken) = match op {
            0xf2 => {
                let n = usize::from(rest.first().copied().unwrap_or(0));
                let k = usize::from(rest.get(1).copied().unwrap_or(0));
                let end = k.saturating_add(3);
                (
                    rest.get(2..end),
                    n.saturating_add(1).saturating_mul(16),
                    end,
                )
            }
            k => {
                let end = usize::from(k).saturating_add(1);
                (rest.get(..end), 1, end)
            }
        };
        out.extend(repeat(once.unwrap_or_default(), times));
        rest = rest.get(taken..).unwrap_or_default();
    }
    out
}

/// The files a seed is written as: its fuzz target's input, a `terminal`
/// seed too long for that target's runs in `deep`'s, if one of its runs
/// takes it.
fn files(seed: &Seed) -> Vec<(PathBuf, Vec<u8>)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let bytes = (seed.bytes)();
    let mut files = Vec::new();
    match seed.fuzz {
        Fuzz::None => {}
        Fuzz::Terminal => {
            let input = terminal_input(&bytes);
            if input.len() <= TERMINAL_MAX {
                files.push((root.join("fuzz/seeds/terminal").join(seed.name), input));
            } else if let Some(input) = deep_input(seed.size, &bytes) {
                files.push((root.join("fuzz/seeds/deep").join(seed.name), input));
            }
        }
        Fuzz::Deep(host) => {
            if let Some(mut input) = deep_input(seed.size, &bytes) {
                input.extend_from_slice(host);
                files.push((root.join("fuzz/seeds/deep").join(seed.name), input));
            }
        }
        // The keys target's first byte is the piece size (0, each read
        // whole), its second the options (none); a byte 0xff is `ff ff`.
        Fuzz::Keys => files.push((
            root.join("fuzz/seeds/keys").join(seed.name),
            [0u8, 0]
                .into_iter()
                .chain(bytes.iter().flat_map(|&b| match b {
                    0xff => vec![0xff, 0xff],
                    b => vec![b],
                }))
                .collect(),
        )),
    }
    files
}

/// A seed in `deep`'s format feeds its bytes, whole: its file is the
/// reproducer that reaches its limit (`every_seed_reaches_its_limit`).
#[test]
fn deep_seeds_feed_their_bytes() {
    for seed in seeds() {
        for (path, input) in files(&seed) {
            if path.parent().and_then(Path::file_name) == Some("deep".as_ref()) {
                let host = match seed.fuzz {
                    Fuzz::Deep(host) => host.len(),
                    Fuzz::None | Fuzz::Terminal | Fuzz::Keys => 0,
                };
                let output = input.get(..input.len().saturating_sub(host));
                assert_eq!(
                    deep_output(output.unwrap_or_default()),
                    (seed.bytes)(),
                    "{}",
                    seed.name
                );
            }
        }
    }
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

/// Every seed names a row of `THREATS.md` 2, once.
#[test]
fn every_seed_names_its_row_once() {
    let seeds = seeds();
    let mut names: Vec<&str> = seeds.iter().map(|s| s.name).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), seeds.len());
    for seed in &seeds {
        assert!(seed.row.starts_with('B') && seed.name.starts_with(&seed.row.to_lowercase()));
    }
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
