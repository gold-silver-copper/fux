//! Native `Cells` operations keep their contracts: exact or reported
//! partial text, copied shapes and styles, untouched cells outside edits,
//! clipped fills, blank resets and exact copies.
//! Structured input includes tiny runs, spilling and huge-size refusals.
#![no_main]
use fux_vt::{CellRef, Cells};
use libfuzzer_sys::arbitrary::{self, Arbitrary};
use libfuzzer_sys::fuzz_target;
#[path = "../../tests/corpus/models.rs"]
mod models;

#[derive(Arbitrary, Debug)]
enum Op {
    SetText(u8, u8, bool, u8),
    SetFrom(u8, u8, u8, bool, u8),
    SetAttributes(u8, u8),
    Fill(u8, u8, bool, u8),
    Reset(u8),
}

/// Lengths past the most cells a run holds: one past it, a large one, the
/// largest an allocation could ask for, and the largest `usize`.
const HUGE: [usize; 4] = [(64 << 20) + 1, 1 << 40, isize::MAX as usize, usize::MAX];

fuzz_target!(|input: (u8, Vec<Op>)| {
    let (len, ops) = input;
    let texts = models::texts();
    let text = |t: &u8| {
        (texts.get(usize::from(*t).checked_rem(texts.len()).unwrap_or(0)))
            .cloned()
            .unwrap_or_default()
    };
    let styles = models::STYLES;
    let style = |s: &u8| {
        (styles.get(usize::from(*s).checked_rem(styles.len()).unwrap_or(0)))
            .copied()
            .unwrap_or_default()
    };
    // A length past what `Cells` holds (64 Mi cells, `THREATS.md` B39) is
    // refused, by `new` and by `reset`, with nothing changed: the largest
    // `usize` too, which overflowed at BASE.
    if let Some(huge) = HUGE
        .get(usize::from(len.saturating_sub(0xfc)))
        .filter(|_| len >= 0xfc)
    {
        assert!(Cells::new(*huge).is_err(), "{huge} cells made");
        if let Ok(mut small) = Cells::new(3) {
            let blank = CellRef::new(
                "retained",
                true,
                models::STYLES.first().copied().unwrap_or_default(),
            );
            small.set(1, blank);
            let before = small.clone();
            assert!(small.reset(*huge).is_err(), "{huge} cells made");
            assert_eq!(small, before);
        }
    }
    // Small runs, so their text budget runs out.
    let len = usize::from(len % 9);
    let (Ok(mut cells), Ok(mut other)) = (Cells::new(len), Cells::new(4)) else {
        return;
    };
    for op in ops.iter().take(200) {
        let before = cells.clone();
        let range = match op {
            Op::SetText(i, t, wide, s) => {
                let i = usize::from(*i % 10);
                let text = text(t);
                let source = CellRef::new(&text, *wide, style(s));
                let whole = cells.set(i, source);
                models::stored(&cells, i, source, whole);
                Some(i..i.saturating_add(1))
            }
            Op::SetFrom(i, j, t, wide, s) => {
                let (i, j) = (usize::from(*i % 10), usize::from(*j % 4));
                other.set(j, CellRef::new(&text(t), *wide, style(s)));
                if let Some(source) = other.get(j) {
                    let whole = cells.set(i, source);
                    models::stored(&cells, i, source, whole);
                }
                Some(i..i.saturating_add(1))
            }
            Op::SetAttributes(i, s) => {
                let i = usize::from(*i % 10);
                cells.set_attributes(i, style(s));
                models::restyled(&cells, &before, i, style(s));
                Some(i..i.saturating_add(1))
            }
            Op::Fill(a, b, continuation, s) => {
                let (a, b) = (usize::from(*a % 11), usize::from(*b % 11));
                let range = a.min(b)..a.max(b);
                let fill = if *continuation {
                    CellRef::wide_continuation()
                } else {
                    CellRef::new("z", false, style(s))
                };
                cells.fill(range.clone(), fill);
                assert!(cells.range(range.clone()).all(|got| got == fill));
                Some(range)
            }
            Op::Reset(len) => {
                let len = usize::from(*len % 9);
                assert!(cells.reset(len).is_ok());
                assert_eq!(cells.len(), len);
                assert!(cells.range(0..len).all(|got| got == CellRef::default()));
                None
            }
        };
        if let Some(range) = range {
            assert_eq!(cells.len(), before.len());
            models::unchanged(&cells, &before, range);
        }
        // Copied cell by cell, with their text, they are the same.
        assert!(Cells::new(cells.len()).is_ok_and(|mut copy| {
            for (i, cell) in cells.range(0..cells.len()).enumerate() {
                copy.set(i, cell);
            }
            copy == cells
        }));
    }
});
