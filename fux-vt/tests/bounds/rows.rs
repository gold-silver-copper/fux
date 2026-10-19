//! The bound rows of `threats.md` section 2, B01 to B45: each a probe of
//! its reproducer at a scale and a chunking, held to the row's bound
//! (section 2), work constants (4.2) and memory terms (3.1, 3.2), with a
//! lower bound on each counter it names (4.3). A row whose code or API is
//! still to come says what it waits on, and fails.

use std::time::Instant;

use fux_vt::keys::decode::{Decoder, Input, PASTE_LIMIT};
use fux_vt::{
    Feature, Graphics, Identity, Mode, Options, Parser, Size, Work, bytes::ByteQueue,
    sixel::Picture,
};

use super::Heap;
use super::probe::{
    Bound, CHUNKS, PICTURE_BYTES, Probe, Scale, Setup, pieces, prints, repeat, steady,
};

/// What a row's probe found: nothing, or how it broke its bound.
pub type Found = Result<(), String>;

/// A row's probe at one scale and chunking.
pub type Row = fn(Scale, usize) -> Found;

/// The most pixels a picture may have (`sixel.rs`, `MAX_PIXELS`).
const MAX_PIXELS: u64 = 4096 * 2160;

/// Feeds `setup` `input`, held to `bound` for `copies` copies, and then
/// `after`'s checks.
fn fed(
    setup: &Setup,
    bound: Bound,
    chunk: usize,
    copies: usize,
    input: &[u8],
    after: impl FnOnce(&Probe) -> Found,
) -> Found {
    let mut probe = Probe::new(setup, bound, chunk, copies)?;
    probe.feed(input)?;
    after(&probe)?;
    probe.finish()
}

/// No more work than the ceiling a call, and the heap within `heap`.
fn heap(heap: usize) -> Bound {
    Bound { work: &[], heap }
}

/// The bytes of `text`, for a row's input.
fn b(text: &str) -> Vec<u8> {
    text.as_bytes().to_vec()
}

// B01: pictures kept, in bytes: at most P = 96 MiB a parser, the one
// being drawn and its growth included.
pub fn b01(scale: Scale, chunk: usize) -> Found {
    let setup = Setup::defaults();
    let one = repeat(b"\x1bPq\"1;1;4096;2160\x1b\\", 64);
    let input = repeat(&one, scale.times());
    // First with a host cell as large as the largest picture: each takes
    // one cell, so nothing scrolls or prints, and the heap is the
    // pictures' and little more.
    let bound = heap(PICTURE_BYTES.saturating_add(1 << 20));
    let mut probe = Probe::new(&setup, bound, chunk, scale.times())?;
    probe.parser.set_host_graphics(Graphics {
        cell: Some((4096, 2160)),
        sixel: true,
    });
    probe.feed(&input)?;
    probe.finish()?;
    let bound = heap(steady(&setup).saturating_add(PICTURE_BYTES));
    fed(&setup, bound, chunk, scale.times(), &input, |_| Ok(()))
}

// B02: at most 64 placements a screen.
pub fn b02(scale: Scale, chunk: usize) -> Found {
    let setup = Setup::defaults();
    let one = repeat(b"\x1bPq@\x1b\\", 65);
    let bound = heap(steady(&setup).saturating_add(PICTURE_BYTES));
    fed(
        &setup,
        bound,
        chunk,
        scale.times(),
        &repeat(&one, scale.times()),
        |p| {
            let shown = p.parser.screen().pictures().count();
            (shown <= 64)
                .then_some(())
                .ok_or(format!("{shown} pictures shown"))
        },
    )
}

// B03: the decoder's buffer and its growth: pixels at most 4096 a byte,
// and growth copying or zeroing at most twice a picture's final buffer.
pub fn b03(scale: Scale, chunk: usize) -> Found {
    let setup = Setup::defaults();
    const BOUND: Bound = Bound {
        work: &[("pixels", 4096, 0, 2 * MAX_PIXELS)],
        heap: 0,
    };
    let bound = Bound {
        heap: steady(&setup).saturating_add(PICTURE_BYTES),
        ..BOUND
    };
    // A picture that grows one column at a time, 340 bands down.
    let mut rightward = b("\x1bPq");
    rightward.extend(repeat(b"~-", 340));
    for n in 1..=2000 {
        rightward.extend(format!("$!{n}?~").bytes());
    }
    rightward.extend(b"\x1b\\");
    // Painted: a column of 12 pixels (6 rows at 2:1) a band, then 2,000.
    let painted = 340 * 12 + 2000 * 12;
    fed(
        &setup,
        bound,
        chunk,
        scale.times(),
        &repeat(&rightward, scale.times()),
        |p| {
            p.at_least(
                "pixels",
                u64::try_from(scale.times())
                    .unwrap_or(0)
                    .saturating_mul(painted),
            )
        },
    )?;
    // The width ladder: a raster widening by one pixel each, before any
    // sixel, which at BASE grew the picture each time. Raster attributes
    // grow nothing now (they are kept as numbers until the picture ends),
    // so the ladders paint no pixel and have no work to reach: they guard
    // that a raster grows nothing, by the bound.
    let mut widths = b("\x1bPq");
    for w in 1..=4096 {
        widths.extend(format!("\"1;1;{w};2160").bytes());
    }
    widths.extend(b"\x1b\\");
    fed(
        &setup,
        bound,
        chunk,
        scale.times(),
        &repeat(&widths, scale.times()),
        |_| Ok(()),
    )?;
    // The raster ladder: growing one row at a time.
    let mut heights = b("\x1bPq");
    for h in 1..=2160 {
        heights.extend(format!("\"1;1;4096;{h}").bytes());
    }
    heights.extend(b"\x1b\\");
    fed(
        &setup,
        bound,
        chunk,
        scale.times(),
        &repeat(&heights, scale.times()),
        |_| Ok(()),
    )
}

// B04: sixel decoding paints at most 4,096 pixels a byte, at any aspect.
pub fn b04(scale: Scale, chunk: usize) -> Found {
    let setup = Setup::defaults();
    const BOUND: Bound = Bound {
        work: &[("pixels", 4096, 0, 2 * MAX_PIXELS)],
        heap: 0,
    };
    let bound = Bound {
        heap: steady(&setup).saturating_add(PICTURE_BYTES),
        ..BOUND
    };
    let mut one = b("\x1bPq\"360;1");
    one.extend(repeat(b"!4096~$", 600));
    one.extend(b"\x1b\\");
    // Each `!4096~` paints 4,096 columns of six rows, at grid resolution.
    let painted = u64::try_from(scale.times())
        .unwrap_or(0)
        .saturating_mul(600 * 4096 * 6);
    fed(
        &setup,
        bound,
        chunk,
        scale.times(),
        &repeat(&one, scale.times()),
        |p| p.at_least("pixels", painted),
    )
}

// B05: a declared extent with nothing drawn allocates and paints nothing,
// and its placement is a row fill: each of its rows (⌈2160 ÷ the cell's
// height⌉) filled across the 200 columns, and taken into history by a line
// feed at B18's 200 + 3 cells (decision 11): at a 10×20 cell 108 × 403 ÷
// 19 bytes, at most 2,291 cells a byte, and at the smallest, 2×6, 360 ×
// 403 ÷ 19, at most 7,636 (P5); with P2 1 and 2 as with 0. Portrait,
// 2160 by 4096, ⌈4096 ÷ the cell's height⌉ rows: 205 × 403 ÷ 19, at most
// 4,349, and at 2×6 683 × 403 ÷ 19, at most 14,487 (decision 830).
pub fn b05(scale: Scale, chunk: usize) -> Found {
    let setup = Setup::defaults();
    for (picture, (cell, per_byte, rows)) in [
        ("\x1bPq\"1;1;4096;2160\x1b\\", ((10, 20), 2291, 108)),
        ("\x1bPq\"1;1;4096;2160\x1b\\", ((2, 6), 7636, 360)),
        ("\x1bP0;1q\"1;1;4096;2160\x1b\\", ((10, 20), 2291, 108)),
        ("\x1bP0;2q\"1;1;4096;2160\x1b\\", ((10, 20), 2291, 108)),
        // Portrait: ⌈4,096 ÷ 20⌉ = 205 rows, or 683 at 2×6, each filled
        // across 200 columns and taken into history at 203 (decision 830).
        ("\x1bPq\"1;1;2160;4096\x1b\\", ((10, 20), 4349, 205)),
        ("\x1bPq\"1;1;2160;4096\x1b\\", ((2, 6), 14_487, 683)),
    ] {
        let input = repeat(&repeat(picture.as_bytes(), 215), scale.times());
        let bound = Bound {
            work: &[("pixels", 0, 0, 0)],
            heap: steady(&setup),
        };
        let mut probe = Probe::new(&setup, bound, chunk, scale.times())?;
        probe.parser.set_host_graphics(Graphics {
            cell: Some(cell),
            sixel: true,
        });
        probe.feed(&input)?;
        let most = u64::try_from(input.len())
            .unwrap_or(0)
            .saturating_mul(per_byte);
        if probe.counted("cells")? > most {
            return Err(format!(
                "{} cells past {most} at a {cell:?} cell ({})",
                probe.counted("cells")?,
                picture.escape_debug()
            ));
        }
        // Each picture covers 200 columns of its rows.
        let placed = u64::try_from(scale.times())
            .unwrap_or(0)
            .saturating_mul(215 * 200)
            .saturating_mul(rows);
        probe.at_least("cells", placed)?;
        probe.finish()?;
    }
    Ok(())
}

// B06: a parser cloned mid-picture copies the picture being drawn: the
// heap's peak at most twice B03's.
pub fn b06(scale: Scale, chunk: usize) -> Found {
    let setup = Setup::defaults();
    let bound = heap(steady(&setup).saturating_add(2 * 53_084_160));
    let mut probe = Probe::new(&setup, bound, chunk, 1)?;
    probe.feed(b"\x1bPq\"1;1;4096;2160~")?;
    for _ in 0..scale.times() {
        let clone = probe.host(0, |p| p.clone())?;
        drop(std::hint::black_box(clone));
    }
    probe.feed(b"\x1b\\")?;
    // A picture the host keeps (its `Arc`) outlives its eviction, outside
    // the parser's budget: the parser counts none, and its pixels live on.
    let kept = (probe.parser.screen().pictures().next()).map(|shown| shown.picture.clone());
    let kept = kept.ok_or("no picture shown")?;
    probe.host(0, |p| p.set_picture_bytes(0))?;
    if probe.parser.screen().pictures().count() != 0 {
        return Err("a lowered budget kept the picture".into());
    }
    let held = probe.live();
    drop(kept);
    if probe.live() >= held {
        return Err(format!(
            "{held} bytes live, the host's picture not among them"
        ));
    }
    probe.finish()
}

// B07: `Picture::new` checks its size against its pixels, so `encode`'s
// work is at most the crop's pixels within the picture.
pub fn b07(scale: Scale, _: usize) -> Found {
    let made = |w: u32, h: u32, len: usize| Picture::new(w, h, vec![0; len]).is_some();
    let checks = [
        (
            made(u32::MAX, u32::MAX, 0),
            false,
            "u32::MAX square, no pixels",
        ),
        (made(5, 3, 59), false, "w × h × 4 − 1"),
        (made(5, 3, 60), true, "w × h × 4"),
        (made(5, 3, 61), false, "w × h × 4 + 1"),
        (made(4096, 1, 4096 * 4), true, "4096 wide"),
        (made(4097, 1, 4097 * 4), false, "4097 wide"),
        (made(1, 4096, 4096 * 4), true, "4096 tall"),
        (made(1, 4097, 4097 * 4), false, "4097 tall"),
    ];
    for (made, want, what) in checks {
        if made != want {
            return Err(format!("{what}: made {made}"));
        }
    }
    // The largest crop over a small picture encodes in a bounded time.
    let picture = Picture::new(4, 4, vec![255; 64]).ok_or("4 by 4")?;
    let start = Instant::now();
    let mut out = Vec::new();
    for _ in 0..scale.times() {
        out.clear();
        fux_vt::sixel::encode(&picture, (0, 0, u32::MAX, u32::MAX), &mut out);
    }
    let took = start.elapsed();
    (took.as_secs() < 1 && !out.is_empty())
        .then_some(())
        .ok_or(format!("{took:?} to encode a 4 by 4 picture"))
}

// B08: `encode`: at most 12 bytes a crop pixel and 6 a colour a band,
// besides its colours' definitions, and at most C_ENC units of work a crop
// pixel, however many colours a band has.
pub fn b08(scale: Scale, _: usize) -> Found {
    let (width, height) = (2000u32, 1000u32);
    let mut rgba = Vec::new();
    for y in 0..height {
        for x in 0..width {
            let [grey, ..] = ((x ^ y) & 255).to_le_bytes();
            rgba.extend([grey, grey, grey, 255]);
        }
    }
    let picture = Picture::new(width, height, rgba).ok_or("2000 by 1000")?;
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    let bands = u64::from(height.div_ceil(6));
    let most = pixels
        .saturating_mul(12)
        .saturating_add(bands.saturating_mul(256 * 6))
        .saturating_add(256 * 20)
        .saturating_add(64);
    let mut out = Vec::new();
    for _ in 0..scale.times() {
        out.clear();
        let before = Work::now();
        fux_vt::sixel::encode(&picture, (0, 0, width, height), &mut out);
        encoded(pixels, Work::now().since(before))?;
        let len = u64::try_from(out.len()).unwrap_or(u64::MAX);
        if len > most {
            return Err(format!("{len} bytes encoded, past {most}"));
        }
    }
    if out.is_empty() {
        return Err("nothing encoded".to_owned());
    }
    // 256 colours a band, each only in the band's first and last columns:
    // the work is the pixels', not each colour's across the width.
    let mut rgba = Vec::new();
    for y in 0..height {
        for x in 0..width {
            let ends = x < 128 || x >= width.saturating_sub(128);
            let [grey, ..] = (x % 128)
                .saturating_mul(2)
                .saturating_add(y % 2)
                .to_le_bytes();
            rgba.extend(if ends {
                [grey, grey, grey, 255]
            } else {
                [0; 4]
            });
        }
    }
    let picture = Picture::new(width, height, rgba).ok_or("2000 by 1000")?;
    let before = Work::now();
    fux_vt::sixel::encode(&picture, (0, 0, width, height), &mut out);
    encoded(pixels, Work::now().since(before))
}

/// The work `encode` may do a crop pixel (`threats.md` B08): it reads each
/// pixel twice, for its colours and for its band, and sets at most one
/// sixel of it.
const C_ENC: u64 = 3;

/// Whether `done`, an `encode` of `pixels` crop pixels, kept to C_ENC a
/// pixel, and counted no less than it read.
fn encoded(pixels: u64, done: Work) -> Found {
    let most = pixels.saturating_mul(C_ENC);
    if done.pixels > most || done.pixels < pixels {
        return Err(format!(
            "encode did {} units for {pixels} pixels, past {most}, or fewer than its pixels",
            done.pixels
        ));
    }
    Ok(())
}

/// `\n` enough times to fill 8 MiB of history with empty rows: 349,525.
fn empty_history() -> Vec<u8> {
    repeat(b"\n", 349_600)
}

// B09: listing pictures doesn't scan history: rows at most 128 × (⌈log2
// (history rows + 1)⌉ + rows) a call, and the cells pictures cover.
pub fn b09(scale: Scale, chunk: usize) -> Found {
    let setup = Setup::defaults();
    let bound = heap(steady(&setup).saturating_add(PICTURE_BYTES));
    let (history, pictures) = (empty_history(), repeat(b"\x1bPq@\x1b\\", 64));
    let mut probe = Probe::new(&setup, bound, chunk.max(4096), 1)?;
    probe.feed(&history)?;
    probe.feed(&pictures)?;
    let history = u64::try_from(probe.parser.screen().history_len()).unwrap_or(u64::MAX);
    let log = u64::from(history.saturating_add(1).ilog2().saturating_add(1));
    let per_call = 128u64
        .saturating_mul(log.saturating_add(50))
        .saturating_add(64);
    let rows: &'static [(&'static str, u64, u64, u64)] =
        Box::leak(Box::new([("rows", 0, per_call, 0)]));
    probe.restart(
        Bound {
            work: rows,
            ..bound
        },
        1,
    );
    for _ in 0..scale.times() {
        let shown = probe.host(0, |p| p.screen().pictures().count())?;
        if shown == 0 {
            return Err("no picture shown".into());
        }
    }
    probe.finish()
}

// B10: `Prints` keeps at most max(4 B a cell, 16 KiB) of text and 4,096
// runs between takes: 171,072 bytes at the defaults.
pub fn b10(scale: Scale, chunk: usize) -> Found {
    let setup = Setup::defaults();
    let bound = heap(171_072usize.saturating_add(history_peak(setup.history)));
    let input = repeat(b"X\x1b[65535b", scale.times());
    let mut probe = Probe::new(&setup, bound, chunk, scale.times())?;
    probe.feed(&input)?;
    probe.at_least("prints", 40_000)?;
    probe.finish()?;
    // What `Prints` kept, untaken: at most 4 bytes a cell of text, and
    // 171,072 bytes in all.
    let prints = probe.parser.take_prints();
    let text = prints
        .iter()
        .map(|p| p.text.len())
        .fold(0, usize::saturating_add);
    let most = setup.cells().saturating_mul(4).max(16 << 10);
    if text > most || prints.heap() > 171_072 {
        return Err(format!(
            "Prints kept {text} bytes of text, {} bytes in all, between takes",
            prints.heap()
        ));
    }
    Ok(())
}

/// SGRs that give each cell a direct background of its own, `count` of
/// them from colour `from`, a cell each.
fn colours(from: u32, count: u32) -> Vec<u8> {
    let mut out = Vec::new();
    for i in from..from.saturating_add(count) {
        let [r, g, bl, _] = i.saturating_add(1).to_le_bytes();
        out.extend(format!("\x1b[48;2;{r};{g};{bl}mx").bytes());
    }
    out
}

/// The history term of the formula (`threats.md` 3.1, B17): 1.1 × H +
/// 64 KiB.
fn history_term(history: usize) -> usize {
    history
        .saturating_add(history / 10)
        .saturating_add(64 << 10)
}

/// The history term and its transients while processing (`threats.md`
/// 3.2: a ring or map growing or shrinking, at most half of it more).
fn history_peak(history: usize) -> usize {
    let term = history_term(history);
    term.saturating_add(term / 2)
}

// B11: the style table's bytes count in history's budget: the heap within
// the formula, less the links, which this input opens none of.
pub fn b11(scale: Scale, chunk: usize) -> Found {
    let setup = Setup::defaults().history(1 << 20);
    let runs = usize::from(setup.rows).saturating_mul(2 * 256 * 48);
    let links = runs.saturating_add(2 * (4 << 20));
    let bound = heap(steady(&setup).saturating_sub(links));
    let count = u32::try_from(scale.times())
        .unwrap_or(0)
        .saturating_mul(150_000);
    let input = colours(0, count);
    fed(&setup, bound, chunk.max(16), scale.times(), &input, |p| {
        p.at_least("inserts", u64::from(count))?;
        p.at_least("lookups", u64::from(count))
    })
}

/// rust-opus's family of styles whose attributes hash alike at BASE
/// (`collide.py`, `cell.rs`'s `Hash`): `count` SGRs, an `x` each, 100 a
/// line.
fn colliding(count: usize) -> Vec<u8> {
    const M: u64 = u64::MAX;
    let rotl = |x: u64, n: u32| x.rotate_left(n) & M;
    let pk = |t: u64, r: u64, g: u64, bl: u64| t | r << 8 | g << 16 | bl << 24;
    let fg = pk(2, 10, 20, 30);
    let mut x = None;
    let mut out = Vec::new();
    let mut made = 0usize;
    'all: for ug in (0..256u64).filter(|ug| ug.is_multiple_of(2)) {
        for ub in 0..256u64 {
            if made == count {
                break 'all;
            }
            let rb = rotl(pk(2, 0, ug, ub), 23);
            let x = *x.get_or_insert((fg | pk(2, 0, 0, 0) << 32) ^ rb);
            let bg = (x ^ rb) >> 32;
            out.extend(
                format!(
                    "\x1b[38;2;10;20;30;48;2;{};{};{};58;2;0;{ug};{ub}mx",
                    (bg >> 8) & 255,
                    (bg >> 16) & 255,
                    bg >> 24
                )
                .bytes(),
            );
            made = made.saturating_add(1);
            if made.is_multiple_of(100) {
                out.extend(b"\r\n");
            }
        }
    }
    out
}

// B12: style inserts expected O(1) under chosen collisions: the family
// that hashed alike at BASE, 8 Ki, 16 Ki and 32 Ki of it, spread over the
// table's places by its keyed hash, at most 64 styles to a place (the
// checker's `styles.spread`, which `finish` runs), and each new style an
// insert. Deterministic: no wall time.
pub fn b12(scale: Scale, chunk: usize) -> Found {
    let setup = Setup::defaults();
    let sizes: &[usize] = match scale {
        Scale::Once => &[8 << 10],
        Scale::Ten => &[16 << 10],
        Scale::Hundred => &[32 << 10],
    };
    for &count in sizes {
        let input = colliding(count);
        let mut probe = Probe::new(&setup, heap(steady(&setup)), chunk.max(4096), 1)?;
        probe.feed(&input)?;
        probe.at_least("inserts", u64::try_from(count).unwrap_or(0))?;
        probe.finish()?;
    }
    Ok(())
}

// B13: the style sweep: at most 7 cells visited a byte, amortized.
pub fn b13(scale: Scale, chunk: usize) -> Found {
    let setup = Setup::defaults().history(1 << 20);
    const BOUND: Bound = Bound {
        work: &[("styles", 7, 0, 0)],
        heap: 0,
    };
    let bound = Bound {
        heap: steady(&setup),
        ..BOUND
    };
    let rows = repeat(&[[b'x'; 200].as_slice(), b"\r\n"].concat(), 700);
    let count = u32::try_from(scale.times())
        .unwrap_or(0)
        .saturating_mul(20_000);
    let mut input = b("\x1b[H");
    for i in 0..count {
        let [r, g, bl, _] = i.saturating_add(1).to_le_bytes();
        input.extend(format!("\x1b[48;2;{r};{g};{bl}mx\x1b[D").bytes());
    }
    let mut probe = Probe::new(&setup, bound, chunk.max(16), 1)?;
    probe.feed(&rows)?;
    probe.restart(bound, scale.times());
    probe.feed(&input)?;
    // A sweep came, and it read every cell: the screen's and history's.
    let cells = u64::try_from(setup.cells())
        .unwrap_or(0)
        .saturating_add(700 * 200);
    probe.at_least("styles", cells)?;
    probe.finish()
}

/// A link of a `uri_len`-byte URI with `id`, over one `x`, then closed.
fn link(id: &str, uri_len: usize) -> Vec<u8> {
    let mut out = format!("\x1b]8;{id};").into_bytes();
    out.extend(repeat(b"u", uri_len));
    out.extend(b"\x07x\x1b]8;;\x07");
    out
}

// B14: hyperlinks held: at most 4 MiB a grid, as real heap.
pub fn b14(scale: Scale, chunk: usize) -> Found {
    // A screen large enough to keep every link on it, with no history.
    let setup = Setup::defaults().sized(300, 200).history(0);
    let runs = usize::from(setup.rows).saturating_mul(256 * 48);
    let bound = heap((4usize << 20).saturating_add(runs).saturating_add(171_072));
    let long = repeat(&link("", 2083), 2000);
    fed(
        &setup,
        bound,
        chunk.max(16),
        scale.times(),
        &repeat(&long, scale.times()),
        |_| Ok(()),
    )?;
    let mut short = Vec::new();
    for i in 0..60_000usize.saturating_mul(scale.times()).min(60_000) {
        short.extend(link(&format!("id={i}"), 1));
    }
    fed(&setup, bound, chunk.max(16), scale.times(), &short, |_| {
        Ok(())
    })
}

// B15: link runs that history keeps count in its budget.
pub fn b15(scale: Scale, chunk: usize) -> Found {
    // History large enough that its rows' runs are most of what is held.
    let setup = Setup::defaults().history(1 << 20);
    let bound = heap(
        history_peak(setup.history)
            .saturating_add(usize::from(setup.rows).saturating_mul(2 * 256 * 48))
            .saturating_add(171_072),
    );
    let mut row = Vec::new();
    for _ in 0..100 {
        row.extend(b"\x1b]8;id=a;u\x07x\x1b]8;id=b;u\x07x");
    }
    row.extend(b"\r\n");
    let input = repeat(&row, scale.times().saturating_mul(1000));
    // Once fed, history holds its term, its rows' runs included, beside
    // the screen's runs (two grids of 50 rows of at most 256 runs, room
    // for twice that) and `Prints`.
    let most = history_term(setup.history)
        .saturating_add(usize::from(setup.rows).saturating_mul(2 * 256 * 48))
        .saturating_add(prints(&setup));
    fed(&setup, bound, chunk.max(16), scale.times(), &input, |p| {
        (p.live() <= most)
            .then_some(())
            .ok_or(format!("{} bytes held, past {most}", p.live()))
    })
}

// B16: making room for links costs at most 256 steps a byte, amortized.
pub fn b16(scale: Scale, chunk: usize) -> Found {
    let setup = Setup::defaults().history(0);
    const BOUND: Bound = Bound {
        work: &[("links", 256, 0, 0)],
        heap: 0,
    };
    let bound = Bound {
        heap: steady(&setup),
        ..BOUND
    };
    let mut input = repeat(&link("", 2083), 2000);
    input.extend(repeat(
        b"\x1b]8;;x\x07y\x1b]8;;\x07",
        scale.times().saturating_mul(1000),
    ));
    fed(&setup, bound, chunk.max(16), scale.times(), &input, |p| {
        p.at_least("links", 2000)
    })?;
    // The sweep's transient is its links' and their rows', however many
    // rows history holds besides (FA-SCR-H2): a full history of empty
    // rows, then the links above.
    let setup = Setup::defaults();
    let mut probe = Probe::new(&setup, heap(usize::MAX), chunk.max(16), 1)?;
    probe.feed(&repeat(b"\n", 400_000))?;
    let (transient, before) = (Heap::begin(), probe.counted("links")?);
    probe.feed(&input)?;
    if probe.counted("links")? <= before {
        return Err("no link sweep".into());
    }
    // The links' own memory, 4 MiB and 64 bytes each, and their runs.
    let most = (4 << 20) + (64 + 24 + 16) * 3000 + (64 << 10);
    match transient.peak() {
        peak if peak <= most => Ok(()),
        peak => Err(format!(
            "links and their sweep held {peak} bytes, past {most}"
        )),
    }
}

// B17: history takes at most 1.1 × its budget + 64 KiB, blocks and maps
// included.
pub fn b17(scale: Scale, chunk: usize) -> Found {
    // Rows of 2,049 cells: two never share a block. History holds at most
    // its term once processed, and its transients on the way, beside what
    // `Prints` holds at this size.
    let wide = Setup::defaults().sized(50, 2049).history(1 << 20);
    let bound = heap(history_peak(wide.history).saturating_add(prints(&wide)));
    let row = [[b'x'; 2049].as_slice(), b"\r\n"].concat();
    let rows = 130usize.saturating_mul(scale.times());
    let most = history_term(wide.history).saturating_add(prints(&wide));
    fed(
        &wide,
        bound,
        chunk.max(16),
        scale.times(),
        &repeat(&row, rows),
        |p| {
            p.at_least("rows", u64::try_from(rows.saturating_sub(50)).unwrap_or(0))?;
            (p.live() <= most)
                .then_some(())
                .ok_or(format!("history holds {} bytes, past {most}", p.live()))
        },
    )?;
    // 349,525 empty rows, then full ones: the ring keeps its room.
    let setup = Setup::defaults();
    let bound = heap(history_peak(setup.history).saturating_add(171_072));
    let mut input = empty_history();
    let full = [[b'x'; 200].as_slice(), b"\r\n"].concat();
    input.extend(repeat(&full, 6000));
    fed(
        &setup,
        bound,
        chunk.max(4096),
        1,
        // At 100x as at 10x: 3.6 M rows a time already turn the ring and
        // its maps over 10 times; a hundred would take minutes for no
        // other state.
        &repeat(&input, scale.times().min(10)),
        |_| Ok(()),
    )
}

// B18: a line feed into history costs at most cols + 3 cells and 2 rows,
// amortized.
pub fn b18(scale: Scale, chunk: usize) -> Found {
    let setup = Setup::defaults();
    const BOUND: Bound = Bound {
        work: &[("cells", 203, 0, 0), ("rows", 2, 0, 0)],
        heap: 0,
    };
    let bound = Bound {
        heap: steady(&setup),
        ..BOUND
    };
    let mut input = repeat(&[[b'x'; 200].as_slice(), b"\r\n"].concat(), 50);
    let feeds = 100_000usize.saturating_mul(scale.times());
    input.extend(repeat(b"\n", feeds));
    fed(&setup, bound, chunk.max(16), scale.times(), &input, |p| {
        p.at_least("rows", u64::try_from(feeds.saturating_sub(50)).unwrap_or(0))
    })
}

// B19: finding where a frame ends reads each byte at most twice:
// `scanned` at most twice the bytes held and 16 a call (decision 823).
pub fn b19(scale: Scale, chunk: usize) -> Found {
    let setup = Setup::defaults();
    const BOUND: Bound = Bound {
        work: &[("scanned", 2, 16, 0)],
        heap: 0,
    };
    let bound = Bound {
        heap: (8usize << 20).saturating_add(steady(&setup)),
        ..BOUND
    };
    let n = (64usize << 10).saturating_mul(scale.times());
    let digits = repeat(b"1", n);
    // The host's frame buffer, its own: room made before the probe.
    let mut held = Vec::with_capacity(n.saturating_add(2));
    held.extend_from_slice(b"\x1b[");
    let mut probe = Probe::new(&setup, bound, chunk, 1)?;
    let begun = probe.host(0, |p| p.process_until_stop(b"\x1b[?2026h", &mut Sinkless))?;
    if !matches!(begun, Ok(Some((_, fux_vt::Stop::Begun)))) {
        return Err(format!("no frame begun: {begun:?}"));
    }
    let mut scan = probe.parser.frame_scan();
    for piece in super::probe::pieces(&digits, chunk) {
        held.extend_from_slice(piece);
        let end = probe.host(piece.len(), |_| scan.end(&held))?;
        if end.is_some() {
            return Err("a frame ended in its digits".into());
        }
    }
    let report = scan.check(&held);
    if !report.is_clean() {
        return Err(report.to_string());
    }
    probe.at_least("scanned", u64::try_from(n).unwrap_or(0))?;
    probe.finish()?;
    // A long DECRST that ends the frame, its parameter's leading zeros read
    // as they come and once (FA-CODEX-5): found at its final byte.
    let mut probe = Probe::new(&setup, bound, chunk, 1)?;
    let _ = probe.host(0, |p| p.process_until_stop(b"\x1b[?2026h", &mut Sinkless))?;
    let decrst = [b("\x1b[?"), repeat(b"0", n), b("2026l")].concat();
    let mut held = Vec::with_capacity(decrst.len());
    let mut scan = probe.parser.frame_scan();
    let mut found = None;
    for piece in super::probe::pieces(&decrst, chunk) {
        held.extend_from_slice(piece);
        found = found.or(probe.host(piece.len(), |_| scan.end(&held))?);
    }
    if found != Some(decrst.len()) {
        return Err(format!("a long DECRST ended the frame at {found:?}"));
    }
    probe.finish()
}

/// A sink that keeps nothing.
struct Sinkless;
impl fux_vt::Sink for Sinkless {}

// B20: the OSC buffer holds at most 65,537 bytes.
pub fn b20(scale: Scale, chunk: usize) -> Found {
    let setup = Setup::defaults();
    let mut one = b("\x1b]52;c;");
    one.extend(repeat(b"A", 65_537));
    one.extend(b"\x07");
    let bound = heap(65_537 + (16 << 10));
    fed(
        &setup,
        bound,
        chunk,
        scale.times(),
        &repeat(&one, scale.times()),
        |_| Ok(()),
    )
}

// B21: strings fux-vt doesn't keep cost nothing to hold, and a byte each.
pub fn b21(scale: Scale, chunk: usize) -> Found {
    let setup = Setup::defaults();
    const BOUND: Bound = Bound {
        work: &[("bytes", 1, 0, 0)],
        heap: 4 << 10,
    };
    let mut one = b("\x1b_");
    one.extend(repeat(b"a", 64 << 10));
    one.extend(b"\x1b\\");
    fed(
        &setup,
        BOUND,
        chunk,
        scale.times(),
        &repeat(&one, scale.times()),
        |_| Ok(()),
    )?;
    // Without `Feature::Pictures`, sixel is a DCS fux-vt ignores (REB-8):
    // 64 KiB of sixel data cost what any ignored string does.
    let plain = Setup {
        options: (Feature::ALL.into_iter())
            .filter(|f| *f != Feature::Pictures)
            .collect(),
        ..setup
    };
    let mut sixel = b("\x1bPq#1;2;100;0;0");
    sixel.extend(repeat(b"!4096~", (64 << 10) / 6));
    sixel.extend(b"\x1b\\");
    fed(
        &plain,
        BOUND,
        chunk,
        scale.times(),
        &repeat(&sixel, scale.times()),
        |p| {
            let shown = p.parser.screen().pictures().count();
            (shown == 0)
                .then_some(())
                .ok_or(format!("{shown} pictures shown"))
        },
    )
}

// B22: a sequence keeps at most 32 parameters, on no heap.
pub fn b22(scale: Scale, chunk: usize) -> Found {
    let setup = Setup::defaults();
    let mut one = b("\x1b[");
    one.extend(repeat(b"1;", 1000));
    one.extend(b"m");
    fed(
        &setup,
        heap(4 << 10),
        chunk,
        scale.times(),
        &repeat(&one, scale.times()),
        |_| Ok(()),
    )
}

/// What the key decoder made of an input: how many inputs, how many of
/// them keys, and the inputs themselves if they were kept.
struct Decoded {
    inputs: usize,
    keys: usize,
    kept: Vec<Input>,
}

/// Takes the decoder's output `out` as a host does: counted, and kept in
/// `kept` if `keep`, else dropped; `room` the most the two held.
fn take(
    out: &mut Vec<Input>,
    kept: &mut Vec<Input>,
    keep: bool,
    room: &mut usize,
    made: &mut Decoded,
) {
    *room = (*room).max(out.capacity().saturating_add(kept.capacity()));
    made.inputs = made.inputs.saturating_add(out.len());
    let keys = out.iter().filter(|i| matches!(i, Input::Key(_))).count();
    made.keys = made.keys.saturating_add(keys);
    if keep {
        kept.append(out);
    } else {
        out.clear();
    }
}

/// The key decoder fed `input` at `chunk` bytes a read, its output taken
/// after each read as a host takes it (and kept if `keep`), its work held
/// to 2 a byte and `per_read` a read, and what it holds to `most` bytes
/// beyond its output's own (`threats.md` B23 to B26).
fn decoded(
    input: &[u8],
    chunk: usize,
    per_read: u64,
    most: usize,
    keep: bool,
) -> Result<Decoded, String> {
    decoded_by(Decoder::default(), input, chunk, per_read, most, keep)
}

/// `decoded`, by `decoder`.
fn decoded_by(
    mut decoder: Decoder,
    input: &[u8],
    chunk: usize,
    per_read: u64,
    most: usize,
    keep: bool,
) -> Result<Decoded, String> {
    let heap = Heap::begin();
    let (mut out, mut kept, mut room) = (Vec::new(), Vec::new(), 0usize);
    let mut made = Decoded {
        inputs: 0,
        keys: 0,
        kept: Vec::new(),
    };
    let (before, mut fed, mut reads) = (Work::now(), 0u64, 0u64);
    for piece in super::probe::pieces(input, chunk) {
        decoder.bytes(piece, &mut out);
        take(&mut out, &mut kept, keep, &mut room, &mut made);
        fed = fed.saturating_add(u64::try_from(piece.len()).unwrap_or(u64::MAX));
        reads = reads.saturating_add(1);
        let counted = Work::now().since(before).keys;
        let bound = fed
            .saturating_mul(2)
            .saturating_add(per_read.saturating_mul(reads));
        if counted > bound {
            return Err(format!(
                "keys {counted} past {bound} after {fed} bytes in {reads} reads"
            ));
        }
        let report = decoder.check();
        if !report.is_clean() {
            return Err(report.to_string());
        }
        // The output is the host's, with its growth: twice its room.
        let own = room.saturating_mul(size_of::<Input>()).saturating_mul(2);
        let held = heap.peak().saturating_sub(own);
        if held > most {
            return Err(format!("the decoder held {held} bytes, past {most}"));
        }
    }
    decoder.timeout(&mut out);
    take(&mut out, &mut kept, keep, &mut room, &mut made);
    if Work::now().since(before).keys < fed {
        return Err("the decoder counted fewer bytes than it was given".into());
    }
    made.kept = kept;
    Ok(made)
}

// B23: a paste keeps at most its limit + 5 bytes; one read is never copied
// whole.
pub fn b23(scale: Scale, chunk: usize) -> Found {
    let input = repeat(
        b"\x1b[200~\x1b[201~",
        5461usize.saturating_mul(scale.times()),
    );
    let most = (PASTE_LIMIT + 5).saturating_add(3 * PASTE_LIMIT);
    decoded(&input, chunk, 258, most, false)?;
    // A paste at its limit, then an ESC that may begin its end marker: its
    // room stays within the limit + 5, as the checker reads it, however
    // the bytes are split (FA-CODEX-4).
    let full = [b("\x1b[200~"), repeat(b"a", PASTE_LIMIT), b("\x1b")].concat();
    decoded(&full, chunk, 258, most, false)?;
    // At the limit and one past it (KEY-5): delivered, then refused whole.
    for (len, delivered) in [(PASTE_LIMIT, true), (PASTE_LIMIT + 1, false)] {
        let mut one = b("\x1b[200~");
        one.extend(repeat(b"p", len));
        one.extend(b("\x1b[201~"));
        let (mut decoder, mut out) = (Decoder::default(), Vec::new());
        let (mut pasted, mut refused) = (0usize, 0usize);
        for piece in super::probe::pieces(&repeat(&one, scale.times()), chunk) {
            decoder.bytes(piece, &mut out);
            for input in std::mem::take(&mut out) {
                match input {
                    Input::Paste(p) if p.len() == len => pasted = pasted.saturating_add(1),
                    Input::PasteTooLong => refused = refused.saturating_add(1),
                    Input::Paste(_)
                    | Input::Key(_)
                    | Input::FocusIn
                    | Input::FocusOut
                    | Input::Mouse(_)
                    | Input::Reply(_) => {}
                }
            }
        }
        let want = scale.times();
        if (pasted, refused) != if delivered { (want, 0) } else { (0, want) } {
            return Err(format!(
                "a paste of {len}: {pasted} delivered, {refused} refused"
            ));
        }
    }
    Ok(())
}

// B24: the key decoder holds at most 258 bytes of an unfinished sequence.
pub fn b24(scale: Scale, chunk: usize) -> Found {
    let mut input = b("\x1b");
    // 16 MiB at 100x, not 100: the held ESC decides at the first read, and
    // what follows is plain keys, the same at any length.
    let a = (1usize << 20).saturating_mul(scale.times()).min(16 << 20);
    input.extend(repeat(b"a", a));
    decoded(&input, chunk, 258, 4 << 10, false)?;
    // An ESC that ends a read, held, then reads far longer than any
    // sequence (KEY-3): the held ESC takes only what decides it.
    let held = [repeat(b"b", 4095).as_slice(), b"\x1b", &repeat(b"a", a)].concat();
    decoded(&held, 4096, 258, 4 << 10, false)?;
    // And when what decides it leaves another ESC held, the 259th byte of
    // the read (lane B's finding): the rest of a 64 KiB read is decided as
    // far again, never held or copied whole. Nothing is typed, so that
    // the decoder's own heap shows: dropped OSC answers, each ESC ending a
    // read's first 259 bytes.
    let k: usize = 64 << 10;
    let again = [
        b("\x1b]11;"),
        repeat(b"1", k.saturating_sub(6)),
        b("\x1b\\\x1b]11;"),
        repeat(b"1", 252),
        b("\x1b]11;"),
        repeat(b"1", k.saturating_sub(259 + 4)),
    ]
    .concat();
    decoded(&again, k, 258, 1 << 10, false)?;
    // The same with nothing typed, so that the decoder's own heap shows:
    // a dropped OSC answer, the ESC that ends the read, then an OSC answer
    // too long to keep over many reads, held nowhere.
    let quiet = [
        b("\x1b]11;"),
        repeat(b"1", 4089),
        b("\x07\x1b]11;"),
        repeat(b"1", a),
    ]
    .concat();
    decoded(&quiet, 4096, 258, 1 << 10, false)?;
    // Answers held while they are expected (FA-KEY-4): a DCS answer at
    // its limit; an OSC answer one past it, its last byte an ESC that may
    // begin ST; and a `CSI ?` answer at its limit after an Escape (FA-KEY-1).
    // A read scans and copies what is held: 2 × 258 a read (decision 821).
    let expecting = || {
        let mut decoder = Decoder::default();
        decoder.expect(Instant::now());
        decoder
    };
    let mut answer = b("\x1bP1+r");
    answer.extend(repeat(b"6", 258 - 7));
    answer.extend(b"\x1b\\");
    for decoder in [Decoder::default(), expecting()] {
        decoded_by(
            decoder,
            &repeat(&answer, scale.times()),
            1,
            516,
            4 << 10,
            false,
        )?;
    }
    let osc = [b("\x1b]1"), repeat(b"a", 255), b("\x1b")].concat();
    let csi = [b("\x1b\x1b[?"), repeat(b"1", 255)].concat();
    // A key CSI past its window after an Escape (FA-RE-B-4).
    let key = [b("\x1b\x1b["), repeat(b"1", 256)].concat();
    for input in [osc, csi, key] {
        decoded_by(expecting(), &input, chunk, 516, 4 << 10, false)?;
    }
    Ok(())
}

// B25: an answer longer than any real one is dropped whole, however it
// arrives.
pub fn b25(scale: Scale, chunk: usize) -> Found {
    let mut one = b("\x1b[?");
    one.extend(repeat(b"1", 300));
    one.extend(b"c");
    let input = repeat(&one, scale.times());
    let whole = decoded(&input, input.len(), 258, 4 << 10, true)?;
    let split = decoded(&input, chunk, 258, 4 << 10, true)?;
    if whole.kept != split.kept {
        return Err(format!(
            "decoded as {} inputs whole, {} in pieces of {chunk}",
            whole.inputs, split.inputs
        ));
    }
    if split.keys != 0 {
        return Err(format!("{} keys typed from an answer", split.keys));
    }
    // Answers past their limit with no end yet, in pieces: held no
    // longer than 258 bytes (KEY-8's guards on each limit).
    for head in ["\x1b]11;", "\x1b[?"] {
        let unended = [b(head), repeat(b"1", 300)].concat();
        decoded(&repeat(&unended, scale.times()), chunk, 258, 4 << 10, false)?;
    }
    // Each window one past its limit (KEY-5), counted as the decoder counts
    // it, after the opening (FA-KEY-5): an OSC answer of 129 bytes with its
    // BEL, a DCS of 257 with its ST's ESC while answers are expected, a key
    // CSI of 33 parameter bytes, a `CSI ?` of 257, each then `x`: decoded
    // alike whole and in pieces, and of it only the `x` typed. `n` counts
    // the opening too.
    let past = |head: &str, n: usize, end: &str| {
        let mut one = b(head);
        one.extend(repeat(b"1", n.saturating_sub(head.len())));
        one.extend(b(end));
        one.extend(b"x");
        repeat(&one, scale.times())
    };
    for (input, expect) in [
        (past("\x1b]11;", 2 + 128, "\x07"), false),
        (past("\x1bP1+r", 2 + 256, "\x1b\\"), true),
        (past("\x1b[", 2 + 33, "u"), false),
        (past("\x1b[?", 2 + 257, "c"), false),
    ] {
        let decode = |size: usize| {
            let mut decoder = Decoder::default();
            if expect {
                decoder.expect(Instant::now());
            }
            let mut out = Vec::new();
            for piece in super::probe::pieces(&input, size) {
                decoder.bytes(piece, &mut out);
            }
            decoder.timeout(&mut out);
            out
        };
        let whole = decode(input.len());
        let typed = whole.iter().filter(|i| matches!(i, Input::Key(_))).count();
        if typed != scale.times() || decode(chunk) != whole {
            return Err(format!(
                "{}: {typed} keys typed whole, or decoded otherwise in pieces of {chunk}",
                input.get(..8).unwrap_or_default().escape_ascii()
            ));
        }
    }
    Ok(())
}

// B26: each input byte makes at most one `Input`.
pub fn b26(scale: Scale, chunk: usize) -> Found {
    let input = repeat(b"a", (64usize << 10).saturating_mul(scale.times()));
    let made = decoded(&input, chunk, 258, 4 << 10, false)?;
    (made.inputs <= input.len()).then_some(()).ok_or(format!(
        "{} inputs of {} bytes",
        made.inputs,
        input.len()
    ))
}

// B27: the kitty keyboard stack holds 32 entries.
pub fn b27(scale: Scale, chunk: usize) -> Found {
    let setup = Setup::defaults();
    let input = repeat(b"\x1b[>1u", 33usize.saturating_mul(scale.times()));
    fed(&setup, heap(4 << 10), chunk, scale.times(), &input, |_| {
        Ok(())
    })
}

// B28: replies are at most 16 bytes a byte that asked.
pub fn b28(scale: Scale, chunk: usize) -> Found {
    let setup = Setup::defaults();
    const BOUND: Bound = Bound {
        work: &[("replies", 16, 0, 0)],
        heap: 64 << 10,
    };
    let mut input = b("\x1b[1;2;3;4:3;5;7;8;9;38;2;1;2;3;48;2;4;5;6;58;2;7;8;9m");
    input.extend(repeat(
        b"\x1bP$qm\x1b\\",
        585usize.saturating_mul(scale.times()),
    ));
    fed(&setup, BOUND, chunk, scale.times(), &input, |p| {
        p.at_least("replies", 585 * 20)
    })?;
    // Each kind of reply against the shortest request for it, with what
    // makes its answer longest set up first: the most digits a position
    // has (a screen of 8192 columns, the cursor at its end), an identity
    // of `Identity::MAX_LEN` bytes, every rendition and three direct
    // colours: at most 16 bytes a byte that asked (SCR-12).
    let half: String = std::iter::repeat_n('n', Identity::MAX_LEN / 2).collect();
    let name: &'static str = Box::leak(half.clone().into_boxed_str());
    let version: &'static str = Box::leak(half.into_boxed_str());
    let options = Options::from_iter(Feature::ALL).with_identity(Some(Identity { name, version }));
    let wide = Setup {
        options,
        ..Setup::defaults().sized(3, 8192)
    };
    let before =
        b("\x1b[3;8192H\x1b[1;2;3;4:3;5;7;8;9;38;2;111;222;233;48;2;144;255;166;58;2;177;188;199m");
    let asks: [&[u8]; 18] = [
        b"\x1b[5n",
        b"\x1b[6n",
        b"\x1b[?6n",
        b"\x1b[c",
        b"\x1b[>c",
        b"\x1b[=c",
        b"\x1b[>q",
        b"\x1b[?2026$p",
        b"\x1b[4$p",
        b"\x1b]4;255;?\x07",
        b"\x1b]10;?\x07",
        b"\x1b]12;?\x07",
        b"\x1b[18t",
        b"\x1b[14t",
        b"\x1b[1;1;1;1;1;1*y",
        b"\x1bP$qm\x1b\\",
        b"\x1bP$qr\x1b\\",
        b"\x1b[?996n",
    ];
    let mut answered = 0usize;
    for ask in asks {
        let mut probe = Probe::new(&wide, heap(steady(&wide)), chunk, 1)?;
        probe.feed(&before)?;
        let set_up = probe.counted("replies")?;
        probe.feed(ask)?;
        let replied = probe.counted("replies")?.saturating_sub(set_up);
        let most = u64::try_from(ask.len()).unwrap_or(0).saturating_mul(16);
        if replied > most {
            return Err(format!("{ask:?}: {replied} bytes of reply"));
        }
        answered = answered.saturating_add(usize::from(replied > 0));
    }
    // Those a host's colours, pixels or a colour scheme answer go
    // unanswered here: the 13 others answer.
    (answered >= 13)
        .then_some(())
        .ok_or(format!("{answered} of the requests answered"))
}

// B29: a prompt mark costs a fixed number of row reads: at most 9 a byte.
pub fn b29(scale: Scale, chunk: usize) -> Found {
    let setup = Setup::defaults();
    const BOUND: Bound = Bound {
        work: &[("rows", 9, 0, 0)],
        heap: 0,
    };
    let bound = Bound {
        heap: steady(&setup),
        ..BOUND
    };
    let feeds = repeat(b"\n", 400_000);
    let marks = repeat(
        b"\x1b]133;C;late\x07",
        290usize.saturating_mul(scale.times()),
    );
    let mut probe = Probe::new(&setup, bound, 4096, 1)?;
    probe.feed(&feeds)?;
    probe.restart(bound, scale.times());
    for piece in super::probe::pieces(&marks, chunk) {
        probe.feed(piece)?;
    }
    // Each late mark reads a row of history at least, looking for where
    // its command began.
    let marks = u64::try_from(290usize.saturating_mul(scale.times())).unwrap_or(0);
    probe.at_least("rows", marks)?;
    probe.finish()
}

/// `e` and `marks` U+0301s: a cluster of 1 + 2 × `marks` bytes.
fn cluster(marks: usize) -> Vec<u8> {
    let marks: String = std::iter::repeat_n("\u{301}", marks).collect();
    [b"e".as_slice(), marks.as_bytes()].concat()
}

// B30: a cell keeps at most 128 bytes of text; the join costs at most 256
// text bytes a byte.
pub fn b30(scale: Scale, chunk: usize) -> Found {
    let setup = Setup::defaults();
    const BOUND: Bound = Bound {
        work: &[("text", 256, 0, 0)],
        heap: 0,
    };
    let bound = Bound {
        heap: steady(&setup),
        ..BOUND
    };
    let input = repeat(&cluster(64), scale.times());
    fed(&setup, bound, chunk, scale.times(), &input, |p| {
        p.at_least("text", 128)
    })
}

// B31: a zero-width character after a full row costs at most 257 text
// bytes (TXT-2's reproducer).
pub fn b31(scale: Scale, chunk: usize) -> Found {
    let setup = Setup::defaults().sized(1, 200).history(0);
    const BOUND: Bound = Bound {
        work: &[("text", 257, 0, 0)],
        heap: 0,
    };
    let bound = Bound {
        heap: steady(&setup),
        ..BOUND
    };
    let mut prefix = repeat(&cluster(50), 64);
    prefix.extend(cluster(22));
    prefix.extend(cluster(8));
    let marks: String =
        std::iter::repeat_n("\u{200b}", 1000usize.saturating_mul(scale.times())).collect();
    let mut probe = Probe::new(&setup, bound, chunk, 1)?;
    probe.feed(&prefix)?;
    probe.restart(bound, scale.times());
    probe.feed(marks.as_bytes())?;
    // Each ZWSP's three bytes, at least, go into the cell's text.
    let marks = u64::try_from(3000usize.saturating_mul(scale.times())).unwrap_or(0);
    probe.at_least("text", marks)?;
    probe.finish()
}

// B32: no glyph is wider than 2 cells: U+17D8 leaves the cursor at most
// 2 columns on.
pub fn b32(scale: Scale, chunk: usize) -> Found {
    let setup = Setup::defaults().sized(1, 10).history(0);
    let mut probe = Probe::new(&setup, heap(64 << 10), chunk, scale.times())?;
    probe.feed(b"XYZW")?;
    for _ in 0..scale.times() {
        probe.feed("\r\u{17d8}".as_bytes())?;
        let (_, col) = probe.parser.screen().cursor_position();
        if col > 2 {
            return Err(format!("U+17D8 moved the cursor {col} columns"));
        }
    }
    probe.finish()
}

// B33: REP costs at most 14,564 cell writes a byte; a REP on a 2×10
// screen with 1 KiB of history at most 720.
pub fn b33(scale: Scale, chunk: usize) -> Found {
    let setup = Setup::defaults();
    const BOUND: Bound = Bound {
        work: &[("cells", 14_564, 0, 0)],
        heap: 0,
    };
    let bound = Bound {
        heap: steady(&setup),
        ..BOUND
    };
    let input = repeat(b"X\x1b[65535b", 455usize.saturating_mul(scale.times()));
    fed(&setup, bound, chunk, scale.times(), &input, |p| {
        p.at_least("cells", 10_000)
    })?;
    let small = Setup::defaults().sized(2, 10).history(1 << 10);
    // The heap: 64 KiB, and what Prints may keep at 2×10 (B10: 16 KiB of
    // text and 4,096 runs of 32 bytes).
    const SMALL: Bound = Bound {
        work: &[("cells", 0, 0, 720)],
        heap: (64 << 10) + (16 << 10) + 4096 * 32,
    };
    let copies = scale.times();
    fed(
        &small,
        SMALL,
        chunk,
        copies,
        &repeat(b"X\x1b[65535b", copies),
        |p| p.at_least("cells", 20),
    )
}

// B34: non-ASCII REP costs at most 23,832 cell writes a byte (23,831 as
// stated, measured at the fix: decisions.md 25).
pub fn b34(scale: Scale, chunk: usize) -> Found {
    let setup = Setup::defaults();
    const BOUND: Bound = Bound {
        work: &[("cells", 23_832, 0, 0)],
        heap: 0,
    };
    let bound = Bound {
        heap: steady(&setup),
        ..BOUND
    };
    let input = repeat(
        "一\x1b[65535b".as_bytes(),
        372usize.saturating_mul(scale.times()),
    );
    fed(&setup, bound, chunk, scale.times(), &input, |p| {
        p.at_least("cells", 20_000)
    })
}

// B34a: REP of a glyph that joins itself skips the copies that change
// nothing: at most 12 text bytes and 12 cells a byte, and the filling of
// the cluster once, each join reading it whole: at most 128 × 43 ÷ 2 =
// 2,752 text bytes (decisions.md 25).
pub fn b34a(scale: Scale, chunk: usize) -> Found {
    let setup = Setup::defaults();
    const BOUND: Bound = Bound {
        work: &[("text", 12, 0, 2752), ("cells", 12, 0, 0)],
        heap: 0,
    };
    let bound = Bound {
        heap: steady(&setup),
        ..BOUND
    };
    let input = repeat(
        "\u{1100}\x1b[65535b".as_bytes(),
        372usize.saturating_mul(scale.times()),
    );
    // The cluster filled once: 42 joins of the 3-byte jamo, each reading
    // the cluster whole, 3 + 6 + … + 126 = 2,709 text bytes.
    fed(&setup, bound, chunk, scale.times(), &input, |p| {
        p.at_least("text", 2709)
    })
}

/// The screen-wide functions of B35, each a sequence repeated to fill
/// 4 KiB, at a screen of `rows` by `cols`.
fn screen_wide(rows: u16, cols: u16) -> Vec<(&'static str, Vec<u8>)> {
    let rect = format!("1;1;{rows};{cols}");
    vec![
        ("DECRQCRA", format!("\x1b[1;1;{rect}*y").into_bytes()),
        ("DECCRA", b("\x1b[$v")),
        ("DECFRA", format!("\x1b[88;{rect}$x").into_bytes()),
        ("DECERA", format!("\x1b[{rect}$z").into_bytes()),
        ("DECSERA", format!("\x1b[{rect}${{").into_bytes()),
        ("DECCARA", format!("\x1b[{rect};1$r").into_bytes()),
        ("DECRARA", format!("\x1b[{rect};1$t").into_bytes()),
        ("DECALN", b("\x1b#8")),
        ("ED 2", b("x\x1b[2J")),
        ("1049", b("\x1b[?1049hx\x1b[?1049l")),
        ("DECIC", b("\x1b['}")),
        ("DECDC", b("\x1b['~")),
        ("SL", b("\x1b[ @")),
        ("SR", b("\x1b[ A")),
        // Scrolls inside left and right margins move cells, not rows:
        // an LF at the bottom margin, and SU.
        (
            "DECLRMM LF",
            format!(
                "\x1b[?69h\x1b[1;{}s\x1b[{rows};2H\n",
                cols.saturating_sub(1)
            )
            .into_bytes(),
        ),
        (
            "DECLRMM SU",
            format!("\x1b[?69h\x1b[1;{}s\x1b[S", cols.saturating_sub(1)).into_bytes(),
        ),
    ]
}

// B35: functions that touch the whole screen cost at most half a screen
// of cells a byte, and DECCRA's text R × (long + short limit) ÷ 4 a byte;
// at fux's larger sizes, each call within U.
pub fn b35(scale: Scale, chunk: usize) -> Found {
    let sizes: &[(u16, u16)] = match scale {
        Scale::Once => &[(50, 200)],
        Scale::Ten => &[(50, 200), (500, 500)],
        Scale::Hundred => &[(50, 200), (500, 500), (4095, 4096)],
    };
    for &(rows, cols) in sizes {
        let setup = Setup::defaults().sized(rows, cols);
        let half = u64::try_from(setup.cells() / 2).unwrap_or(u64::MAX);
        let c = u64::from(cols);
        let text =
            u64::from(rows).saturating_mul(c.saturating_mul(32 + 17).saturating_add(128)) / 4;
        let work: &'static [(&'static str, u64, u64, u64)] =
            Box::leak(Box::new([("cells", half, 0, 0), ("text", text, 0, 0)]));
        let bound = Bound {
            work,
            heap: steady(&setup).saturating_mul(2),
        };
        // A screen of text, its cells holding clusters, to copy and edit:
        // whole rows of 17-byte clusters, which fill a row's short text,
        // their base letters differing from row to row so that a copy
        // changes every cell.
        let mut prefix = Vec::new();
        for y in 0..rows.min(200) {
            let mut each = cluster(8);
            if let Some(base) = each.first_mut() {
                *base = if y % 2 == 0 { b'e' } else { b'a' };
            }
            prefix.extend(repeat(&each, usize::from(cols.min(500))));
            prefix.extend(b"\r\n");
        }
        // At fux's largest pane the row checks each call's ceiling, U: one
        // 4 KiB call of each function, in one piece. Its work a byte is
        // B35's at 50x200 and 500x500 already, where the input is
        // repeated; repeated here, the work the bound allows (N / 2 cells
        // a byte, 8.4 M) takes hours (decisions.md 45).
        let largest = rows == 4095;
        if largest && chunk != 4096 {
            continue;
        }
        for (name, sequence) in screen_wide(rows, cols) {
            // 100x as 10x below fux's largest pane: the work a byte is the
            // same at every count, and a hundred 4 KiB calls of each
            // function at 500x500 take an hour.
            let times = if largest { 1 } else { scale.times().min(10) };
            let count = (4096usize.checked_div(sequence.len()).unwrap_or(0)).saturating_mul(times);
            let input = repeat(&sequence, count);
            let mut probe = Probe::new(&setup, bound, chunk.max(16), 1)?;
            probe.feed(&prefix)?;
            probe.restart(bound, 1);
            probe
                .feed(&input)
                .map_err(|e| format!("{name} at {rows}x{cols}: {e}"))?;
            // Each function touches every cell the screen of clusters
            // holds at least once: what reaching its limit is (a
            // rectangle's default is the page; ED 2 erases what is
            // written, once).
            let filled = usize::from(rows.min(200)).saturating_mul(usize::from(cols.min(500)));
            let cells = u64::try_from(filled).unwrap_or(0);
            probe
                .at_least("cells", cells)
                .map_err(|e| format!("{name} at {rows}x{cols}: {e}"))?;
            probe
                .finish()
                .map_err(|e| format!("{name} at {rows}x{cols}: {e}"))?;
        }
        // Scrolls inside left and right margins, set up once: each copies
        // the region's span of every row, with its clusters' text, and the
        // wrap of text printed at the bottom margin makes one too, which
        // no sequence does (decision 831). A line feed, one byte: at most
        // 2 x the screen's cells and R rows of span text.
        let n = u64::try_from(setup.cells()).unwrap_or(u64::MAX);
        let r = u64::from(rows);
        let wide = format!("\x1b[?69h\x1b[1;{}s\x1b[{rows};2H", cols.saturating_sub(1));
        let narrow = format!("\x1b[?69h\x1b[1;2s\x1b[{rows};1H");
        let times = if largest { 1 } else { scale.times().min(10) };
        // The span's text, at most 49 bytes a cell (32 long, 17 short): the
        // whole width less a column, or the narrow margins' 2 cells, one
        // scroll for every 2 glyphs printed.
        let span_text = c.saturating_sub(1).saturating_mul(49).saturating_add(128);
        for (name, setup_bytes, each, text) in [
            ("bare LF", &wide, &b"\n"[..], r.saturating_mul(span_text)),
            ("bare SU", &wide, b"\x1b[S", r.saturating_mul(span_text)),
            ("wrapping text", &narrow, b"x", r.saturating_mul(49 + 128)),
        ] {
            let work: &'static [(&'static str, u64, u64, u64)] = Box::leak(Box::new([
                ("cells", n.saturating_mul(2), 0, 0),
                ("text", text, 0, 0),
            ]));
            let margins = Bound { work, ..bound };
            let input = repeat(
                each,
                4096usize
                    .checked_div(each.len())
                    .unwrap_or(0)
                    .saturating_mul(times),
            );
            let mut probe = Probe::new(&setup, margins, chunk.max(16), 1)?;
            probe.feed(&prefix)?;
            probe.feed(setup_bytes.as_bytes())?;
            probe.restart(margins, 1);
            probe
                .feed(&input)
                .map_err(|e| format!("{name} at {rows}x{cols}: {e}"))?;
            probe
                .finish()
                .map_err(|e| format!("{name} at {rows}x{cols}: {e}"))?;
        }
    }
    Ok(())
}

// B36: a scroll inside a region moves at most twice its rows' slots and
// its cells, its count clamped: N / 4 cells and 2R / 3 slots a byte.
pub fn b36(scale: Scale, chunk: usize) -> Found {
    let rows: u16 = 4095;
    let setup = Setup::defaults().sized(rows, 1).history(0);
    const BOUND: Bound = Bound {
        work: &[("cells", 4095 / 4, 0, 0), ("slots", 2 * 4095 / 3, 0, 0)],
        heap: 0,
    };
    let bound = Bound {
        heap: steady(&setup),
        ..BOUND
    };
    let input = repeat(b"\x1b[4095L", scale.times());
    fed(&setup, bound, chunk, scale.times(), &input, |p| {
        p.at_least("slots", u64::from(rows))
    })?;
    // A region across the ring's end (FA-SIZE-1): a line feed at the
    // bottom, with history, turns the ring by one, so the last two rows
    // cross its end; each IL of both there moves at most twice their
    // slots.
    let setup = Setup::defaults().sized(rows, 1);
    let times = scale.times().saturating_mul(100);
    let crossing = Bound {
        work: &[("slots", 0, 0, 4)],
        heap: steady(&setup),
    };
    let mut probe = Probe::new(&setup, heap(steady(&setup)), chunk, times)?;
    probe.feed(b"\x1b[4095;1H\n\x1b[4094;4095r")?;
    probe.restart(crossing, times);
    probe.feed(&repeat(b"\x1b[4094;1H\x1b[2L", times))?;
    probe.at_least("slots", 2)?;
    probe.finish()
}

// B37: a resize needs up to twice history's memory while it runs, and a
// rejected one changes nothing, synchronized output included.
pub fn b37(scale: Scale, chunk: usize) -> Found {
    let setup = Setup::defaults();
    let bound = heap(steady(&setup).saturating_add(12_700_000));
    let rows = repeat(&[[b'x'; 200].as_slice(), b"\r\n"].concat(), 6000);
    let mut probe = Probe::new(&setup, bound, chunk.max(4096), 1)?;
    probe.feed(&rows)?;
    let history = probe.parser.screen().history_len();
    let cells = history.saturating_mul(200).saturating_add(setup.cells());
    // Reflow copies each cell out, pushes it into the new grid and clears
    // its row: 3 a cell, and a row read and kept a row (decision 11).
    let most = u64::try_from(cells.saturating_add(history).saturating_mul(3)).unwrap_or(u64::MAX);
    let per_call: &'static [(&'static str, u64, u64, u64)] =
        Box::leak(Box::new([("rows", 0, most, 0), ("cells", 0, most, 0)]));
    probe.restart(
        Bound {
            work: per_call,
            ..bound
        },
        1,
    );
    for _ in 0..scale.times() {
        for cols in [199, 200] {
            let size = Size::new(50, cols).map_err(|e| e.to_string())?;
            probe
                .host(0, |p| p.resize(size))?
                .map_err(|e| e.to_string())?;
        }
    }
    probe.feed(b"\x1b[?2026h")?;
    let huge = Size::new(u16::MAX, u16::MAX).map_err(|e| e.to_string())?;
    if probe.host(0, |p| p.resize(huge))?.is_ok() {
        return Err("a 65535x65535 resize was taken".into());
    }
    if !probe.parser.screen().mode(Mode::SynchronizedOutput) {
        return Err("a rejected resize ended synchronized output".into());
    }
    probe.finish()
}

// B38: a screen holds at most 64 Mi cells.
pub fn b38(_: Scale, _: usize) -> Found {
    let size = |rows, cols| Size::new(rows, cols).map_err(|e| e.to_string());
    let checks = [
        (size(8191, 8192)?, true),
        (size(8192, 8192)?, true),
        (size(8192, 8193)?, false),
        (size(u16::MAX, u16::MAX)?, false),
    ];
    for (size, fits) in checks {
        if fux_vt::fits(size) != fits {
            return Err(format!("{size:?} fits: {}", !fits));
        }
    }
    match Parser::new(size(u16::MAX, u16::MAX)?, 0) {
        Ok(_) => Err("a 65535x65535 parser was made".into()),
        Err(_) => Ok(()),
    }
}

// B39: `Cells` holds at most 64 Mi cells, and refuses more.
pub fn b39(scale: Scale, _: usize) -> Found {
    const MAX_CELLS: usize = 64 << 20;
    // Past the most: refused, by `new` and by `resize`, which leaves the
    // cells as they were. (At the most, 512 MiB, only at 100x.)
    for len in [MAX_CELLS.saturating_add(1), usize::MAX] {
        let made = std::panic::catch_unwind(|| fux_vt::Cells::new(len).map(|c| c.len()));
        match made {
            Ok(Err(fux_vt::Error::Capacity)) => {}
            Ok(other) => return Err(format!("Cells::new({len}) gave {other:?}")),
            Err(_) => return Err(format!("Cells::new({len}) panics (SCR-11)")),
        }
        let mut cells = fux_vt::Cells::new(4).map_err(|e| e.to_string())?;
        let grown = std::panic::catch_unwind(move || {
            let grown = cells.resize(len, fux_vt::CellRef::default());
            (grown, cells.len())
        });
        match grown {
            Ok((Err(fux_vt::Error::Capacity), 4)) => {}
            Ok(other) => return Err(format!("resize to {len} gave {other:?}")),
            Err(_) => return Err(format!("resize to {len} panics (SCR-11)")),
        }
    }
    let most = if scale == Scale::Hundred {
        MAX_CELLS
    } else {
        1 << 10
    };
    let made = fux_vt::Cells::new(most).map_err(|e| e.to_string())?;
    (made.len() == most)
        .then_some(())
        .ok_or(format!("Cells::new({most}) made {}", made.len()))
}

// B40: finding a row by its identity costs a binary search: rows at most
// rows + ⌈log2(history rows + 1)⌉ a call.
pub fn b40(scale: Scale, chunk: usize) -> Found {
    let setup = Setup::defaults();
    let bound = heap(steady(&setup));
    let feeds = empty_history();
    let mut probe = Probe::new(&setup, bound, chunk.max(4096), 1)?;
    probe.feed(&feeds)?;
    let history = u64::try_from(probe.parser.screen().history_len()).unwrap_or(u64::MAX);
    let per_call =
        50u64.saturating_add(u64::from(history.saturating_add(1).ilog2()).saturating_add(1));
    let rows: &'static [(&'static str, u64, u64, u64)] =
        Box::leak(Box::new([("rows", 0, per_call, 0)]));
    probe.restart(
        Bound {
            work: rows,
            ..bound
        },
        1,
    );
    // The screen's top row, and history's newest, just above it.
    let screen = probe.parser.screen();
    let top = screen.window().row(0).map(|r| r.id());
    let above = screen
        .rows()
        .rev()
        .nth(usize::from(setup.rows))
        .map(|r| r.id());
    let (Some(top), Some(above)) = (top, above) else {
        return Err("no top row, or none above it".into());
    };
    for _ in 0..10usize.saturating_mul(scale.times()) {
        for id in [top, above] {
            let found = probe.host(0, |p| p.screen().row_by_id(id).is_some())?;
            if !found {
                return Err(format!("row {id:?} was not found"));
            }
        }
    }
    probe.at_least("rows", 1)?;
    probe.finish()
}

// B41: `push_with`'s closure may only add bytes: one that takes them
// away leaves a queue that still takes what is pushed next.
pub fn b41(scale: Scale, _: usize) -> Found {
    for _ in 0..scale.times() {
        let mut queue = ByteQueue::default();
        // Less taken than is left: the bytes stay where they are, and the
        // closure clears them from under what was taken.
        queue.push(b"abcdef");
        queue.take(2);
        queue.push_with(Vec::clear);
        queue.push(b"xy");
        if queue.as_slice() != b"xy" {
            return Err(format!(
                "after a closure that cleared it, the queue holds {:?}",
                queue.as_slice()
            ));
        }
    }
    Ok(())
}

// B42: small fixed state: tab stops at most a byte a column.
pub fn b42(scale: Scale, chunk: usize) -> Found {
    let setup = Setup::defaults();
    let input = repeat(b"\x1bH\x1b[C", 200usize.saturating_mul(scale.times()));
    fed(&setup, heap(4 << 10), chunk, scale.times(), &input, |_| {
        Ok(())
    })
}

// B43: events go straight to the host: fux-vt keeps none.
pub fn b43(scale: Scale, chunk: usize) -> Found {
    let setup = Setup::defaults();
    let input = repeat(b"\x1b]9;hello\x07", 400usize.saturating_mul(scale.times()));
    fed(
        &setup,
        heap(65_537 + (4 << 10)),
        chunk,
        scale.times(),
        &input,
        |_| Ok(()),
    )
}

// B44: a refused allocation loses only what it was for, and every later
// byte is read (PAR-3), each site by its degrade (SCR-10).
pub fn b44(_: Scale, chunk: usize) -> Found {
    // A prompt mark's fresh line, and a picture's placement, scroll the
    // row into history, whose first block is refused. At BASE the parser
    // stayed in the OSC (`hello` went into it) or in sixel with no
    // decoder (`hello` was swallowed). The picture's graphics new lines
    // take the cursor down a row, past the 20-pixel cell its one band
    // fits in (SIX-12: the cursor goes where the sixel motion went).
    for string in [
        &b"\x1b]133;A\x07"[..],
        b"\x1b]133;A\x1b\\",
        b"\x1bPq#1~~---\x1b\\",
    ] {
        let mut parser = refusing(1)?;
        let failed = (pieces(&[string, b"hello"].concat(), chunk))
            .filter(|piece| parser.process(piece).is_err())
            .count();
        fux_vt::refuse_next(0);
        let text: String = parser
            .screen()
            .main_rows()
            .map(|r| r.cells_text())
            .collect();
        let shown = string.escape_ascii();
        if !text.contains("hello") {
            return Err(format!(
                "{shown}: the text after the refusal was lost: {text:?}"
            ));
        }
        if (failed, parser.refused()) != (1, 1) {
            return Err(format!(
                "{shown}: {failed} calls failed and {} refusals were counted, not 1 and 1",
                parser.refused()
            ));
        }
        let report = parser.check();
        if !report.is_clean() {
            return Err(format!("{shown}: the checker found: {report}"));
        }
    }
    // Two refusals are counted as two: the alternate screen's grid too.
    let mut parser = refusing(2)?;
    let _ = parser.process(b"\x1b]133;A\x07");
    let _ = parser.process(b"\x1b[?1049h");
    fux_vt::refuse_next(0);
    match parser.refused() {
        2 => {}
        n => return Err(format!("two refusals counted as {n}")),
    }
    modes_past_a_refusal()?;
    degrades(chunk)
}

/// A refusal inside a mode sequence loses only the mode it was for: the
/// alternate screen 1049 asks room for. Synchronized output, before it or
/// after it, is set, and the frame it begins is told (FA-PAR-1).
fn modes_past_a_refusal() -> Found {
    struct Quiet;
    impl fux_vt::Sink for Quiet {}
    for sequence in [&b"\x1b[?2026;1049h"[..], b"\x1b[?1049;2026h"] {
        let shown = sequence.escape_ascii();
        let mut parser = refusing(1)?;
        let stop = parser.process_until_stop(sequence, &mut Quiet);
        fux_vt::refuse_next(0);
        let length = sequence.len();
        if !matches!(stop, Ok(Some((n, fux_vt::Stop::Begun))) if n == length) {
            return Err(format!("{shown}: {stop:?}, not the frame begun"));
        }
        let screen = parser.screen();
        let modes = (
            screen.mode(Mode::SynchronizedOutput),
            screen.mode(Mode::AlternateScreenSaveCursor),
            parser.refused(),
        );
        if modes != (true, false, 1) {
            return Err(format!(
                "{shown}: (2026, 1049, refusals) {modes:?}, not (true, false, 1)"
            ));
        }
    }
    Ok(())
}

/// Each site's degrade (SCR-10), the checker clean after it and the bytes
/// after it read: a row history has no room for goes as with no history,
/// the screen scrolling still; a RIS with no room for grids made anew
/// blanks them in place, history gone, and resets the rest; a refused
/// alternate screen leaves the primary shown, the mode reset.
fn degrades(chunk: usize) -> Found {
    let lines = |parser: &Parser| -> Vec<String> {
        let rows = parser.screen().rows();
        rows.map(|r| r.cells_text().trim_end().to_owned()).collect()
    };
    let fed = |n: u32, before: &[u8], after: &[u8]| -> Result<Parser, String> {
        let options = Feature::ALL.into_iter().collect();
        let size = Size::new(2, 10).map_err(|e| e.to_string())?;
        let mut parser = Parser::with_options(size, 1 << 20, options).map_err(|e| e.to_string())?;
        parser.process(before).map_err(|e| e.to_string())?;
        fux_vt::refuse_next(n);
        for piece in pieces(after, chunk) {
            let _ = parser.process(piece);
        }
        fux_vt::refuse_next(0);
        let report = parser.check();
        match report.is_clean() {
            true => Ok(parser),
            false => Err(format!(
                "{}: the checker found: {report}",
                after.escape_ascii()
            )),
        }
    };
    let want = |parser: &Parser, rows: &[&str], refused: u64, what: &str| -> Result<(), String> {
        let got = lines(parser);
        match (got == rows, parser.refused() == refused) {
            (true, true) => Ok(()),
            _ => Err(format!(
                "{what}: rows {got:?} and {} refusals, not {rows:?} and {refused}",
                parser.refused()
            )),
        }
    };
    // History's first block refused: `a` goes as with no history, and the
    // screen scrolls for `c` (at BASE the line feed was lost, `c` over `b`).
    let parser = fed(1, b"a\r\nb", b"\r\nc")?;
    want(&parser, &["b", "c"], 1, "a row history had no room for")?;
    // A RIS with history held: grids made anew, then made again for the
    // clear, refused; both blanked in place, history gone, and the modes
    // reset, autowrap among them (at BASE the RIS was lost whole).
    let parser = fed(2, b"a\r\nb\r\nc\x1b[?7l", b"\x1bcz")?;
    want(&parser, &["z", ""], 1, "a RIS with no room")?;
    if !parser.screen().mode(fux_vt::Mode::Autowrap) {
        return Err("a RIS with no room left autowrap off".into());
    }
    // The alternate screen refused: the primary stays shown, the mode
    // reset, and what follows is printed there.
    let parser = fed(1, b"a", b"\x1b[?1049hz")?;
    want(&parser, &["az", ""], 1, "a refused alternate screen")?;
    match parser
        .screen()
        .mode(fux_vt::Mode::AlternateScreenSaveCursor)
    {
        false => Ok(()),
        true => Err("a refused alternate screen reads set".into()),
    }
}

/// A parser of 1 row by 10 columns, with history and every feature, sixel
/// shown, an `x` printed, and its next `n` allocations refused.
fn refusing(n: u32) -> Result<Parser, String> {
    let size = Size::new(1, 10).map_err(|e| e.to_string())?;
    let options = Feature::ALL.into_iter().collect();
    let mut parser = Parser::with_options(size, 1 << 20, options).map_err(|e| e.to_string())?;
    parser.set_host_graphics(Graphics {
        cell: Some((10, 20)),
        sixel: true,
    });
    parser.process(b"x").map_err(|e| e.to_string())?;
    fux_vt::refuse_next(n);
    Ok(parser)
}

// B45: each encoded key or mouse event is at most 64 bytes: every key with
// every modifier, a kitty report of the largest code points with every
// modifier bit, in every key mode; every mouse event at the edges of each
// encoding's positions, in every mode and encoding.
pub fn b45(_: Scale, _: usize) -> Found {
    use fux_vt::keys::encode::{KeyMode, OtherKeys, Report, key_bytes};
    use fux_vt::keys::mouse::{MouseAction, MouseButton, MouseEvent, mouse_bytes};
    use fux_vt::keys::{Direction, Function, Key, KeyPress, Keystroke, Kitty, Modifiers};
    use fux_vt::{MouseProtocolEncoding, MouseProtocolMode};
    let all_mods = || {
        (0..8u8).map(|bits| Modifiers {
            shift: bits & 1 != 0,
            alt: bits & 2 != 0,
            ctrl: bits & 4 != 0,
        })
    };
    let mut keys: Vec<Key> = ['a', 'A', ' ', '\u{7f}', '\u{80}', 'é', '一', '\u{10ffff}']
        .into_iter()
        .map(Key::Char)
        .collect();
    keys.extend([
        Key::Enter,
        Key::Tab,
        Key::Escape,
        Key::Backspace,
        Key::Delete,
        Key::Insert,
        Key::Home,
        Key::End,
        Key::PageUp,
        Key::PageDown,
    ]);
    keys.extend(Direction::ALL.map(Key::Arrow));
    keys.extend((1..=Function::LAST).filter_map(Function::new).map(Key::F));
    let mut modes = Vec::new();
    for (application, backarrow) in [(false, false), (false, true), (true, false), (true, true)] {
        for other_keys in [
            None,
            Some(OtherKeys::Usual),
            Some(OtherKeys::All),
            Some(OtherKeys::Unmodified),
        ] {
            modes.push(KeyMode::Legacy {
                application,
                keypad: application,
                other_keys,
                backarrow,
            });
        }
        for report in [
            Report::Disambiguated,
            Report::AllKeys,
            Report::AllKeysWithText,
        ] {
            modes.push(KeyMode::Kitty {
                alternate: application,
                report,
                backarrow,
            });
        }
    }
    let big = Kitty {
        code: Some(0x10_ffff),
        shifted: Some(0x10_ffff),
        base: Some(0x10_ffff),
        mods: u8::MAX,
    };
    let (mut longest, mut out) = (0usize, Vec::new());
    for &key in &keys {
        for mods in all_mods() {
            for kitty in [None, Some(big)] {
                let stroke = Keystroke {
                    press: KeyPress::new(key, mods),
                    kitty,
                };
                for &mode in &modes {
                    out.clear();
                    key_bytes(stroke, mode, &mut out);
                    longest = longest.max(out.len());
                }
            }
        }
    }
    let keyed = longest;
    let mut actions: Vec<MouseAction> = MouseButton::ALL.map(MouseAction::Press).to_vec();
    actions.push(MouseAction::Release(None));
    actions.push(MouseAction::Motion(None));
    actions.extend(MouseButton::ALL.map(|b| MouseAction::Release(Some(b))));
    actions.extend(MouseButton::ALL.map(|b| MouseAction::Motion(Some(b))));
    let edges = [0u16, 222, 223, 2014, 2015, u16::MAX];
    for &action in &actions {
        for mods in all_mods() {
            for &row in &edges {
                for &col in &edges {
                    let event = MouseEvent {
                        action,
                        mods,
                        row,
                        col,
                    };
                    for mode in [
                        MouseProtocolMode::Press,
                        MouseProtocolMode::PressRelease,
                        MouseProtocolMode::ButtonMotion,
                        MouseProtocolMode::AnyMotion,
                    ] {
                        for encoding in [
                            MouseProtocolEncoding::Default,
                            MouseProtocolEncoding::Utf8,
                            MouseProtocolEncoding::Sgr,
                        ] {
                            out.clear();
                            mouse_bytes(event, mode, encoding, &mut out);
                            longest = longest.max(out.len());
                        }
                    }
                }
            }
        }
    }
    if longest > 64 {
        return Err(format!(
            "an encoding of {longest} bytes (keys at most {keyed}), past 64"
        ));
    }
    Ok(())
}

/// Every row, by its name in `threats.md`.
pub const ROWS: &[(&str, Row)] = &[
    ("B01", b01),
    ("B02", b02),
    ("B03", b03),
    ("B04", b04),
    ("B05", b05),
    ("B06", b06),
    ("B07", b07),
    ("B08", b08),
    ("B09", b09),
    ("B10", b10),
    ("B11", b11),
    ("B12", b12),
    ("B13", b13),
    ("B14", b14),
    ("B15", b15),
    ("B16", b16),
    ("B17", b17),
    ("B18", b18),
    ("B19", b19),
    ("B20", b20),
    ("B21", b21),
    ("B22", b22),
    ("B23", b23),
    ("B24", b24),
    ("B25", b25),
    ("B26", b26),
    ("B27", b27),
    ("B28", b28),
    ("B29", b29),
    ("B30", b30),
    ("B31", b31),
    ("B32", b32),
    ("B33", b33),
    ("B34", b34),
    ("B34a", b34a),
    ("B35", b35),
    ("B36", b36),
    ("B37", b37),
    ("B38", b38),
    ("B39", b39),
    ("B40", b40),
    ("B41", b41),
    ("B42", b42),
    ("B43", b43),
    ("B44", b44),
    ("B45", b45),
];

/// Runs row `name` at `scale`, at every chunking, until one finds it past
/// its bound.
pub fn run(row: Row, scale: Scale) -> Found {
    // Every chunking is run, and what each found is told, so that a row
    // known to fail is seen to fail as it is known to at each.
    let found: Vec<String> = (CHUNKS.into_iter())
        .filter_map(|chunk| {
            row(scale, chunk).err().map(|e| match chunk {
                super::probe::WHOLE => format!("whole: {e}"),
                _ => format!("at {chunk} bytes a call: {e}"),
            })
        })
        .collect();
    match found.is_empty() {
        true => Ok(()),
        false => Err(found.join("; ")),
    }
}

/// How many rows `threats.md` has: B01 to B45 and B34a.
pub const COUNT: usize = 46;

const _: () = assert!(ROWS.len() == COUNT);
