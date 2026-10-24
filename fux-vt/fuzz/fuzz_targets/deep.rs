#![no_main]
//! The deep target (`THREATS.md` section 6): large state
//! and every host call. Up to 511 by 511 cells and 8 MiB of history, every
//! `Feature` and pictures drawn, fed up to 1 MiB of output a run by
//! `process_until_stop` in 4 KiB reads, as fux's pane reads, with each
//! call's work held to the frozen ceiling U (4.4); a second parser fed a
//! byte at a time, its row versions checked after every byte while it
//! holds few enough rows; and between the outputs the host's calls:
//! resizes, history budgets, palettes, pictures
//! listed and encoded, the picture budget, windows read. fux-vt's
//! invariant checker runs after every operation, on both, and the two must
//! agree.
//!
//! Input: six header bytes, then operations.
//! - `h0`, `h1`: 1 + 2 × h rows and columns;
//! - `h2`: the history budget, `BUDGETS[h2 % 5]`;
//! - `h3`, `h4`: bit i of `h3 | h4 << 8` turns `Feature::ALL[i]` on;
//!   `h4 & 0x40` an identity;
//! - `h5`: the host's cell for pictures (`cell`), which draw sixel.
//!
//! Operations, by their first byte:
//! - `ff r c`: resize to 1 + 2r by 1 + 2c;
//! - `fe b`: history's budget to `BUDGETS[b % 5]`;
//! - `fd`: clear history;
//! - `f9 k`: the host's palette, entry k % 16 known;
//! - `f6`: the host reads: the pictures, each encoded whole and in part,
//!   held to `encode`'s bound (B08), both screens' rows, the colours, how
//!   many rows went;
//! - `f5 c`: the host's cell for pictures;
//! - `f4 b`: the picture budget, `PICTURES[b % 4]`;
//! - `f3` and eight bytes: a window's text, as in `terminal`;
//! - `f2 n k` and k + 1 bytes: those bytes as output, (n + 1) × 16 times;
//! - `f1 n` and n bytes: a character of `tests/corpus/graphemes.rs` for
//!   each;
//! - any other byte `k`: the next k + 1 bytes as output.
use fux_vt::{Feature, Graphics, Identity, Options, Parser, Rgb, Size, Work};
use libfuzzer_sys::fuzz_target;
mod common;
use common::{
    Record, Rows, features_reached, marks_sound, pick, reached, until_stops, versions_follow,
};
#[path = "../../tests/corpus/graphemes.rs"]
mod graphemes;
#[path = "../../tests/corpus/invariants.rs"]
mod invariants;
#[cfg(feature = "checks")]
#[path = "../../tests/corpus/known.rs"]
mod known;

/// History budgets: none, a few rows, some blocks, many, and fux's.
const BUDGETS: [usize; 5] = [0, 1 << 10, 64 << 10, 1 << 20, 8 << 20];

/// Picture budgets: none, one small picture, a large one, fux's.
const PICTURES: [usize; 4] = [0, 64 << 10, 36 << 20, fux_vt::PICTURE_BYTES];

/// The most output fed in one run, so a run takes seconds.
const MOST: usize = 1 << 20;

/// The most bytes the byte-at-a-time parser is fed in one run, and the
/// most rows it may hold, for its versions to be checked after each byte.
const SPLIT: usize = 64 << 10;
const SPLIT_ROWS: usize = 2048;

/// The most cells the byte-at-a-time checks hash in one run: at 2,048
/// rows of 81 cells, 64 KiB of bytes hashed 10^10 cells, a minute without
/// a sanitizer, past `-timeout`. About a second's worth.
const SPLIT_CELLS: usize = 1 << 27;

/// The most operations a run makes, and the most host reads (`f6`) among
/// them: each checks screens of up to 261,121 cells and 8 MiB of history,
/// and a read encodes every picture shown, so a run stays within seconds.
const OPS: usize = 128;
const READS: usize = 4;

/// The work `sixel::encode` may do a crop pixel (`THREATS.md` B08).
const C_ENC: u64 = 3;

/// Whether every item of `items` is equal.
pub fn alike<T: PartialEq>(items: &[T]) -> bool {
    items.iter().all(|item| items.first() == Some(item))
}

/// A size of 1 + 2r by 1 + 2c.
fn size(r: u8, c: u8) -> Option<Size> {
    let side = |n: u8| u16::from(n).saturating_mul(2).saturating_add(1);
    Size::new(side(r), side(c)).ok()
}

/// A host cell of 1 to 16 by 1 to 29 pixels, from one byte.
fn graphics(c: u8) -> Graphics {
    let width = u16::from(c & 0x0f).saturating_add(1);
    let height = u16::from(c >> 4).saturating_mul(2).saturating_add(1);
    Graphics {
        cell: Some((width, height)),
        sixel: true,
    }
}

/// The parsers fed alike, and what the run has fed.
struct Run {
    whole: Parser,
    split: Parser,
    fed: usize,
    split_fed: usize,
    hashed: usize,
}

impl Run {
    /// Feeds `bytes` to both: `whole` in reads of 4 KiB by
    /// `process_until_stop`, each call within the ceiling; `split` a byte at
    /// a time, its versions checked after each, until it has had `SPLIT`
    /// bytes, and the two compared after; then neither is fed again.
    fn feed(&mut self, bytes: &[u8]) {
        let bytes = bytes
            .get(..MOST.saturating_sub(self.fed).min(bytes.len()))
            .unwrap_or_default();
        self.fed = self.fed.saturating_add(bytes.len());
        let (mut a, mut b) = (Record::default(), Record::default());
        let (at, mark) = (Rows::of(&self.whole), self.whole.screen().mark());
        let mut last = Work::now();
        let mut rest = bytes;
        while !rest.is_empty() {
            let (piece, after) = rest.split_at_checked(4096).unwrap_or((rest, &[]));
            rest = after;
            until_stops(&mut self.whole, piece, &mut a, |_| {
                let now = Work::now();
                let done = now.since(last);
                last = now;
                assert!(
                    done.cost() <= Work::CEILING,
                    "a call did {} units, past U: {done:?}",
                    done.cost() / 20
                );
            });
        }
        marks_sound(&at, mark, &self.whole, &Rows::of(&self.whole));
        if self.split_fed.saturating_add(bytes.len()) > SPLIT {
            self.split_fed = usize::MAX;
            return;
        }
        self.split_fed = self.split_fed.saturating_add(bytes.len());
        // The rows after a byte are those before the next.
        let mut kept = None;
        for byte in bytes {
            let screen = self.split.screen();
            let cells = (screen.rows().len()).saturating_mul(usize::from(screen.size().cols()));
            let few = screen.rows().len() <= SPLIT_ROWS
                && self.hashed.saturating_add(cells) <= SPLIT_CELLS;
            if few {
                self.hashed = self.hashed.saturating_add(cells);
            }
            let before = few.then(|| kept.take().unwrap_or_else(|| Rows::of(&self.split)));
            assert!(
                (self.split)
                    .process_with(std::slice::from_ref(byte), &mut b)
                    .is_ok()
            );
            if let Some(before) = before {
                let after = Rows::of(&self.split);
                versions_follow(&before, &after);
                kept = Some(after);
            }
        }
        assert_eq!(a, b);
    }

    /// Both parsers, while `split` is still fed.
    fn both(&mut self) -> Vec<&mut Parser> {
        if self.split_fed == usize::MAX {
            vec![&mut self.whole]
        } else {
            vec![&mut self.whole, &mut self.split]
        }
    }
}

/// What the host reads of `p` (`f6`): every picture shown, encoded whole
/// and in its lower right quarter, held to `encode`'s work and output
/// (B08); both screens' rows; the colours; the rows gone.
fn host_reads(p: &Parser) {
    let screen = p.screen();
    for shown in screen.pictures() {
        let (width, height) = (shown.picture.width(), shown.picture.height());
        for crop in [
            (0, 0, width, height),
            (width / 2, height / 2, width, height),
        ] {
            let mut out = Vec::new();
            let before = Work::now();
            fux_vt::sixel::encode(shown.picture, crop, &mut out);
            let done = Work::now().since(before);
            let (w, h) = (
                u64::from(crop.2.min(width.saturating_sub(crop.0))),
                u64::from(crop.3.min(height.saturating_sub(crop.1))),
            );
            let pixels = w.saturating_mul(h);
            assert!(done.pixels <= pixels.saturating_mul(C_ENC), "{done:?}");
            let bands = h.div_ceil(6);
            let most = pixels
                .saturating_mul(12)
                .saturating_add(bands.saturating_mul(256 * 6))
                .saturating_add(256 * 20 + 64);
            let len = u64::try_from(out.len()).unwrap_or(u64::MAX);
            assert!(len <= most, "{len} bytes encoded for {pixels} pixels");
        }
    }
    let rows = screen
        .main_rows()
        .len()
        .saturating_add(screen.alternate_rows().len());
    assert!(rows >= usize::from(screen.size().rows()));
    for index in 0..=u8::MAX {
        let _ = screen.palette_color(index);
    }
    for number in 10..=19 {
        let _ = screen.dynamic_color(number);
    }
}

fuzz_target!(|data: &[u8]| {
    let Some((header, mut input)) = data.split_first_chunk::<6>() else {
        return;
    };
    let [r, c, h, f0, f1, cell] = *header;
    let Some(first) = size(r, c) else {
        return;
    };
    let bits = u16::from(f0) | u16::from(f1) << 8;
    let mut options = Options::new().with_identity((f1 & 0x40 != 0).then_some(Identity {
        name: "fuzz",
        version: "1.2.3",
    }));
    for (i, feature) in (0u32..).zip(Feature::ALL) {
        options = options.set(feature, bits.checked_shr(i).is_some_and(|b| b & 1 != 0));
    }
    let budget = pick(&BUDGETS, usize::from(h));
    let Ok(mut whole) = Parser::with_options(first, budget.unwrap_or(0), options) else {
        return;
    };
    whole.set_host_graphics(graphics(cell));
    reached("call set_host_graphics");
    features_reached(&whole);
    let split = whole.clone();
    let mut run = Run {
        whole,
        split,
        fed: 0,
        split_fed: 0,
        hashed: 0,
    };
    let (mut ops, mut reads) = (0usize, 0usize);
    while let Some((&operation, tail)) = input.split_first() {
        input = tail;
        ops = ops.saturating_add(1);
        if ops > OPS {
            break;
        }
        let mut arg = || {
            let (first, rest) = input.split_first().map_or((0, &[][..]), |(f, r)| (*f, r));
            input = rest;
            first
        };
        match operation {
            0xff => {
                reached("call resize");
                let (r, c) = (arg(), arg());
                if let Some(size) = size(r, c) {
                    for p in run.both() {
                        assert!(p.resize(size).is_ok());
                    }
                }
            }
            0xfe => {
                reached("call set_history_bytes");
                let budget = pick(&BUDGETS, usize::from(arg()));
                for p in run.both() {
                    p.set_history_bytes(budget.unwrap_or(0));
                }
            }
            0xfd => {
                reached("call clear_history");
                run.both().into_iter().for_each(Parser::clear_history);
            }
            0xf9 => {
                reached("call set_host_palette");
                let k = arg();
                let mut palette = [None; 16];
                if let Some(entry) = palette.get_mut(usize::from(k % 16)) {
                    *entry = Some(Rgb { r: k, g: !k, b: k });
                }
                run.both()
                    .into_iter()
                    .for_each(|p| p.set_host_palette(palette));
            }
            0xf6 => {
                reached("call host_reads");
                reads = reads.saturating_add(1);
                if reads <= READS {
                    run.both().into_iter().for_each(|p| host_reads(p));
                }
            }
            0xf5 => {
                reached("call set_host_graphics");
                let cell = graphics(arg());
                run.both()
                    .into_iter()
                    .for_each(|p| p.set_host_graphics(cell));
            }
            0xf4 => {
                reached("call set_picture_bytes");
                let bytes = pick(&PICTURES, usize::from(arg()));
                run.both()
                    .into_iter()
                    .for_each(|p| p.set_picture_bytes(bytes.unwrap_or(0)));
            }
            0xf3 => {
                reached("call Window::text");
                let Some((parameters, rest)) = input.split_first_chunk::<8>() else {
                    break;
                };
                input = rest;
                let [offset, _, _, ay, ax, by, bx, cells] = parameters.map(u16::from);
                let (cells, offset) = (usize::from(cells), usize::from(offset));
                let bytes = cells.saturating_mul(4);
                let texts: Vec<_> = (run.both().into_iter())
                    .map(|p| {
                        let window = p.screen().window();
                        let window = window.row(0).map_or(window, |top| top.up(offset).window());
                        window.text((ay, ax), (by, bx), cells, bytes)
                    })
                    .collect();
                assert!(alike(&texts));
                for text in texts.iter().flatten() {
                    assert!(text.len() <= bytes);
                }
            }
            0xf2 => {
                let (n, k) = (usize::from(arg()), usize::from(arg()));
                let length = k.saturating_add(1).min(input.len());
                let (once, rest) = input.split_at_checked(length).unwrap_or((input, &[]));
                input = rest;
                let times = n.saturating_add(1).saturating_mul(16);
                let repeated: Vec<u8> = std::iter::repeat_n(once, times)
                    .flatten()
                    .copied()
                    .collect();
                run.feed(&repeated);
            }
            0xf1 => {
                let count = usize::from(arg());
                let (picks, rest) = input.split_at_checked(count).unwrap_or((input, &[]));
                input = rest;
                let table = graphemes::CHARACTERS;
                let text: String = (picks.iter())
                    .filter_map(|n| pick(table, usize::from(*n)))
                    .collect();
                run.feed(text.as_bytes());
            }
            k => {
                let length = usize::from(k).saturating_add(1).min(input.len());
                let (bytes, rest) = input.split_at_checked(length).unwrap_or((input, &[]));
                input = rest;
                run.feed(bytes);
            }
        }
        let fed = run.split_fed != usize::MAX;
        for p in run.both() {
            invariants::check(p);
        }
        if fed {
            invariants::same_feed(&run.whole, &run.split);
        }
    }
});
