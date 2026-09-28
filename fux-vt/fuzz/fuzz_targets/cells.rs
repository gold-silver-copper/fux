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

fuzz_target!(|input: (u8, Vec<Op>)| {
    let (len, ops) = input;
    let texts = models::texts();
    let text = |t: &u8| texts[usize::from(*t) % texts.len()].clone();
    let style = |s: &u8| models::STYLES[usize::from(*s) % models::STYLES.len()];
    // Small runs, so their text budget runs out.
    let len = usize::from(len % 9);
    let mut cells = Cells::new(len);
    let mut other = Cells::new(4);
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
