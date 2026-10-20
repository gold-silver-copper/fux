//! `Cells` against a plain list of cells.
//!
//! The input is structured (`arbitrary`): a run's length and edits of every
//! kind to it -- texts inline, spilled and past its budget, copies from
//! another run, attribute changes, fills and resizes. After each, every
//! cell must hold what the model says, the run's text must stay within its
//! budget, and collecting it must copy it exactly.
#![no_main]
use fux_vt::Cells;
use libfuzzer_sys::arbitrary::{self, Arbitrary};
use libfuzzer_sys::fuzz_target;
#[path = "../../tests/corpus/models.rs"]
mod models;
use models::{CellsOp, Model};

#[derive(Arbitrary, Debug)]
enum Op {
    SetText {
        i: u8,
        text: u8,
        wide: bool,
        style: u8,
    },
    SetFrom {
        i: u8,
        j: u8,
        text: u8,
        wide: bool,
        style: u8,
    },
    SetAttributes {
        i: u8,
        style: u8,
    },
    Fill {
        a: u8,
        b: u8,
        continuation: bool,
        style: u8,
    },
    Resize {
        len: u8,
    },
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
    // refused, by `new` and by `resize`, with nothing changed: the largest
    // `usize` too, which overflowed at BASE.
    if let Some(huge) = HUGE
        .get(usize::from(len.saturating_sub(0xfc)))
        .filter(|_| len >= 0xfc)
    {
        assert!(Cells::new(*huge).is_err(), "{huge} cells made");
        if let Ok(mut small) = Cells::new(3) {
            let blank = fux_vt::CellRef::new(" ", false, fux_vt::Attributes::default());
            assert!(small.resize(*huge, blank).is_err(), "{huge} cells made");
            assert_eq!(small.len(), 3);
        }
    }
    // Small runs, so their text budget runs out.
    let len = usize::from(len % 9);
    let (Ok(mut cells), Ok(mut other)) = (Cells::new(len), Cells::new(4)) else {
        return;
    };
    let mut model = vec![Model::blank(); len];
    for op in ops.iter().take(200) {
        let op = match op {
            Op::SetText {
                i,
                text: t,
                wide,
                style: s,
            } => CellsOp::SetText {
                i: usize::from(*i % 10),
                text: text(t),
                wide: *wide,
                attributes: style(s),
            },
            Op::SetFrom {
                i,
                j,
                text: t,
                wide,
                style: s,
            } => CellsOp::SetFrom {
                i: usize::from(*i % 10),
                j: usize::from(*j % 4),
                text: text(t),
                wide: *wide,
                attributes: style(s),
            },
            Op::SetAttributes { i, style: s } => CellsOp::SetAttributes {
                i: usize::from(*i % 10),
                attributes: style(s),
            },
            Op::Fill {
                a,
                b,
                continuation,
                style: s,
            } => CellsOp::Fill {
                a: usize::from(*a % 11),
                b: usize::from(*b % 11),
                continuation: *continuation,
                attributes: style(s),
            },
            Op::Resize { len } => CellsOp::Resize {
                len: usize::from(*len % 9),
            },
        };
        models::apply(&mut cells, &mut other, &mut model, &op);
        models::check(&cells, &model);
    }
});
