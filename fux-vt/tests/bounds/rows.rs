//! The bound rows of `threats.md` section 2, B01 to B45: each a probe of
//! its reproducer at a scale and a chunking, held to the row's bound
//! (section 2), work constants (4.2) and memory terms (3.1, 3.2), with a
//! lower bound on each counter it names (4.3). A row whose code or API is
//! still to come says what it waits on, and fails.

use std::time::Instant;

use fux_vt::keys::decode::{Decoder, Input, PASTE_LIMIT};
use fux_vt::{Graphics, Mode, Parser, Size, Work, bytes::ByteQueue, sixel::Picture};

use super::Heap;
use super::probe::{Bound, CHUNKS, PICTURE_BYTES, Probe, Scale, Setup, repeat, steady};

/// What a row's probe found: nothing, or how it broke its bound.
pub type Found = Result<(), String>;

/// A row's probe at one scale and chunking.
pub type Row = fn(Scale, usize) -> Found;

/// The most pixels a picture may have (`sixel.rs`, `MAX_PIXELS`).
const MAX_PIXELS: u64 = 4096 * 2160;

/// A row whose code or API is still to come: it fails, saying what it
/// waits on.
fn waits(on: &str) -> Found {
    Err(format!("waits on {on}"))
}

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
    // sixel, so each one grows the picture.
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
// and its placement is a row fill: cells at most 2,201 a byte at a 10×20
// cell.
pub fn b05(scale: Scale, chunk: usize) -> Found {
    let setup = Setup::defaults();
    const BOUND: Bound = Bound {
        work: &[("cells", 2201, 0, 0), ("pixels", 0, 0, 0)],
        heap: 0,
    };
    let bound = Bound {
        heap: steady(&setup).saturating_add(PICTURE_BYTES),
        ..BOUND
    };
    let one = repeat(b"\x1bPq\"1;1;4096;2160\x1b\\", 215);
    let input = repeat(&one, scale.times());
    let mut probe = Probe::new(&setup, bound, chunk, scale.times())?;
    probe.parser.set_host_graphics(Graphics {
        cell: Some((10, 20)),
        sixel: true,
    });
    probe.feed(&input)?;
    // Each picture covers 200 columns of 108 rows.
    let placed = u64::try_from(scale.times())
        .unwrap_or(0)
        .saturating_mul(215 * 200 * 108);
    probe.at_least("cells", placed)?;
    probe.finish()
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
    probe.finish()
}

// B07: `Picture::new` checks its size against its pixels.
pub fn b07(_: Scale, _: usize) -> Found {
    waits("Picture::new (SIX-5): a picture built with sides past its pixels is not checked")
}

// B08: `encode`: at most 12 bytes a crop pixel and 6 a colour a band,
// besides its colours' definitions.
pub fn b08(scale: Scale, _: usize) -> Found {
    let (width, height) = (2000u32, 1000u32);
    let mut rgba = Vec::new();
    for y in 0..height {
        for x in 0..width {
            let [grey, ..] = ((x ^ y) & 255).to_le_bytes();
            rgba.extend([grey, grey, grey, 255]);
        }
    }
    let picture = Picture {
        width,
        height,
        rgba,
    };
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
        fux_vt::sixel::encode(&picture, (0, 0, width, height), &mut out);
        let len = u64::try_from(out.len()).unwrap_or(u64::MAX);
        if len > most {
            return Err(format!("{len} bytes encoded, past {most}"));
        }
    }
    (!out.is_empty())
        .then_some(())
        .ok_or("nothing encoded".to_owned())
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

/// The screens' share of the style tables (`threats.md` B11), both grids.
fn styles_term(setup: &Setup) -> usize {
    let entries = setup
        .cells()
        .saturating_mul(2)
        .next_power_of_two()
        .max(4096);
    entries
        .saturating_mul(43)
        .saturating_add(16)
        .saturating_mul(2)
}

// B11: the style table's bytes count in history's budget.
pub fn b11(scale: Scale, chunk: usize) -> Found {
    let setup = Setup::defaults().history(1 << 20);
    let terms = [
        styles_term(&setup),
        history_term(setup.history),
        history_term(setup.history) / 2,
        171_072,
    ];
    let bound = heap(terms.into_iter().fold(0, usize::saturating_add));
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
// at twice the size takes at most 2.3 times as long (probabilistic, the
// keyed hash's; the deterministic ceiling is the probes, which std's map
// keeps to itself).
pub fn b12(scale: Scale, chunk: usize) -> Found {
    let setup = Setup::defaults();
    let small = match scale {
        Scale::Once => 8 << 10,
        Scale::Ten | Scale::Hundred => 16 << 10,
    };
    let time = |count: usize| -> Result<f64, String> {
        let mut best = f64::MAX;
        for _ in 0..3 {
            let input = colliding(count);
            let mut probe = Probe::new(&setup, heap(steady(&setup)), chunk.max(4096), 1)?;
            let started = Instant::now();
            probe.feed(&input)?;
            best = best.min(started.elapsed().as_secs_f64());
            probe.at_least("inserts", u64::try_from(count).unwrap_or(0))?;
        }
        Ok(best)
    };
    let (once, twice) = (time(small)?, time(small.saturating_mul(2))?);
    let ratio = twice / once.max(1e-9);
    (ratio <= 2.3).then_some(()).ok_or(format!(
        "twice the colliding styles took {ratio:.2} times as long"
    ))
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
    let setup = Setup::defaults().history(64 << 10);
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
    let input = repeat(&row, scale.times().saturating_mul(100));
    fed(&setup, bound, chunk.max(16), scale.times(), &input, |_| {
        Ok(())
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
    })
}

// B17: history takes at most 1.1 × its budget + 64 KiB, blocks and maps
// included.
pub fn b17(scale: Scale, chunk: usize) -> Found {
    // Rows of 2,049 cells: two never share a block. History holds at most
    // its term once processed, and its transients on the way.
    let wide = Setup::defaults().sized(50, 2049).history(1 << 20);
    let bound = heap(history_peak(wide.history).saturating_add(171_072));
    let row = [[b'x'; 2049].as_slice(), b"\r\n"].concat();
    let rows = 130usize.saturating_mul(scale.times());
    let most = history_term(wide.history).saturating_add(171_072);
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

// B19: finding where a frame ends reads each byte once: `scanned` at most
// the bytes held and 16 a call.
pub fn b19(scale: Scale, chunk: usize) -> Found {
    let setup = Setup::defaults();
    const BOUND: Bound = Bound {
        work: &[("scanned", 1, 16, 0)],
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
    let begun = probe.host(0, |p| p.process_until_frame(b"\x1b[?2026h", &mut Sinkless))?;
    if !matches!(begun, Ok(Some((_, fux_vt::Frame::Begun)))) {
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
    let heap = Heap::begin();
    let mut decoder = Decoder::default();
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
    decoded(&input, chunk, 258, most, false).map(|_| ())
}

// B24: the key decoder holds at most 258 bytes of an unfinished sequence.
pub fn b24(scale: Scale, chunk: usize) -> Found {
    let mut input = b("\x1b");
    let a = (1usize << 20).saturating_mul(scale.times()).min(16 << 20);
    input.extend(repeat(b"a", a));
    decoded(&input, chunk, 258, 4 << 10, false)?;
    let mut answer = b("\x1bP1+r");
    answer.extend(repeat(b"6", 258 - 7));
    answer.extend(b"\x1b\\");
    decoded(&repeat(&answer, scale.times()), 1, 258, 4 << 10, false).map(|_| ())
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
    (split.keys == 0)
        .then_some(())
        .ok_or(format!("{} keys typed from an answer", split.keys))
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
    })
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
    const SMALL: Bound = Bound {
        work: &[("cells", 0, 0, 720)],
        heap: 64 << 10,
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

// B34: non-ASCII REP costs at most 23,831 cell writes a byte.
pub fn b34(scale: Scale, chunk: usize) -> Found {
    let setup = Setup::defaults();
    const BOUND: Bound = Bound {
        work: &[("cells", 23_831, 0, 0)],
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
// nothing: at most 12 text bytes and 12 cells a byte.
pub fn b34a(scale: Scale, chunk: usize) -> Found {
    let setup = Setup::defaults();
    const BOUND: Bound = Bound {
        work: &[("text", 12, 0, 0), ("cells", 12, 0, 0)],
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
    fed(&setup, bound, chunk, scale.times(), &input, |_| Ok(()))
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
        // A screen of text, its cells holding clusters, to copy and edit.
        let mut prefix = Vec::new();
        for _ in 0..rows.min(200) {
            prefix.extend(repeat(&cluster(8), usize::from(cols.min(200))));
            prefix.extend(b"\r\n");
        }
        for (name, sequence) in screen_wide(rows, cols) {
            let count = (4096usize.checked_div(sequence.len()).unwrap_or(0))
                .saturating_mul(scale.times().min(10));
            let input = repeat(&sequence, count);
            let mut probe = Probe::new(&setup, bound, chunk.max(16), 1)?;
            probe.feed(&prefix)?;
            probe.restart(bound, 1);
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
    })
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
    let most = u64::try_from(cells.saturating_add(history).saturating_mul(2)).unwrap_or(u64::MAX);
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
pub fn b39(_: Scale, _: usize) -> Found {
    let made = std::panic::catch_unwind(|| fux_vt::Cells::new(usize::MAX).len());
    match made {
        Ok(_) => Err("Cells::new(usize::MAX) made cells".into()),
        Err(_) => Err("Cells::new(usize::MAX) panics (SCR-11)".into()),
    }
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
    let top = probe.parser.screen().window().row(0).map(|r| r.id());
    let Some(top) = top else {
        return Err("no top row".into());
    };
    for _ in 0..10usize.saturating_mul(scale.times()) {
        let found = probe.host(0, |p| p.screen().row_by_id(top).is_some())?;
        if !found {
            return Err("the top row was not found".into());
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

// B44: a refused allocation loses only what it was for.
pub fn b44(_: Scale, _: usize) -> Found {
    waits("a test-only fail switch in crate::reserve and Parser::refused (SCR-10, PAR-3)")
}

// B45: each encoded key or mouse event is at most 64 bytes.
pub fn b45(_: Scale, _: usize) -> Found {
    waits("each encoder's exact maximum, stated from encode.rs and mouse.rs (KEY-4)")
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
    for chunk in CHUNKS {
        row(scale, chunk).map_err(|e| format!("at {chunk} bytes a call: {e}"))?;
    }
    Ok(())
}

/// How many rows `threats.md` has: B01 to B45 and B34a.
pub const COUNT: usize = 46;

const _: () = assert!(ROWS.len() == COUNT);
