//! fux-vt's standalone types, beside the parser: `Cells`, the run of cells
//! fux composes frames in, given random edits of every kind; `CellRef::new`;
//! `continues_cluster`; and fux-vt's constants and limits. Compared after
//! every edit.
use crate::case::{Difference, same, same_list};
use crate::model::{Blink, Color, Row, Style, Underline};
use crate::rng::Rng;
use crate::{base, graphemes, work};

/// fux-vt's types that stand alone, on one side.
pub trait Standalone {
    type Run: Clone;
    /// Each constant and limit, by name.
    fn constants() -> Vec<(&'static str, String)>;
    fn continues_cluster(cluster: &str, c: char) -> bool;
    /// `Cells::new(len)`.
    fn run(len: usize) -> Self::Run;
    /// An edit, and what it returned.
    fn apply(run: &mut Self::Run, edit: &Edit) -> String;
    /// The run read into a row: its length, text and cells.
    fn read(run: &Self::Run, out: &mut Row);
}

/// An edit of a `Cells`.
#[derive(Clone, Debug)]
pub enum Edit {
    /// `set` with a cell `CellRef::new` makes.
    Set {
        i: usize,
        text: String,
        wide: bool,
        style: Style,
    },
    /// `set` with `CellRef::wide_continuation`.
    Continuation { i: usize },
    /// `set_attributes`.
    SetAttributes { i: usize, style: Style },
    /// `fill` with a cell `CellRef::new` makes.
    Fill {
        start: usize,
        end: usize,
        text: String,
        wide: bool,
        style: Style,
    },
    /// `resize` with a cell `CellRef::new` makes.
    Resize {
        len: usize,
        text: String,
        wide: bool,
        style: Style,
    },
    /// `set` from a cell of the run as it was.
    Copy { i: usize, from: usize },
    /// The run collected from its own cells (`FromIterator`), compared
    /// with itself (`PartialEq`), and taking its place.
    Collect,
}

fn color(r: &mut Rng) -> Color {
    match r.below(4) {
        0 => Color::Default,
        1 => Color::Idx(u8::try_from(r.below(256)).unwrap_or(0)),
        2 => Color::Idx(u8::try_from(r.below(16)).unwrap_or(0)),
        _ => Color::Rgb(
            u8::try_from(r.below(256)).unwrap_or(0),
            u8::try_from(r.below(256)).unwrap_or(0),
            u8::try_from(r.below(256)).unwrap_or(0),
        ),
    }
}

/// A random style, now and then the default.
pub fn style(r: &mut Rng) -> Style {
    if r.chance(30) {
        return default_style();
    }
    Style {
        fg: color(r),
        bg: color(r),
        underline_color: color(r),
        bold: r.chance(30),
        dim: r.chance(30),
        italic: r.chance(30),
        underline: r.chance(30),
        underline_style: match r.below(6) {
            0 => Underline::None,
            1 => Underline::Single,
            2 => Underline::Double,
            3 => Underline::Curly,
            4 => Underline::Dotted,
            _ => Underline::Dashed,
        },
        inverse: r.chance(30),
        blink: match r.below(3) {
            0 => Blink::None,
            1 => Blink::Slow,
            _ => Blink::Rapid,
        },
        hidden: r.chance(20),
        strikeout: r.chance(20),
    }
}

/// The default style.
pub fn default_style() -> Style {
    Style {
        fg: Color::Default,
        bg: Color::Default,
        underline_color: Color::Default,
        bold: false,
        dim: false,
        italic: false,
        underline: false,
        underline_style: Underline::None,
        inverse: false,
        blink: Blink::None,
        hidden: false,
        strikeout: false,
    }
}

/// Text for a cell: empty, a character, a short cluster, or one longer
/// than a cell holds inline or than a cluster may be.
fn text(r: &mut Rng) -> String {
    let count = match r.below(6) {
        0 => 0,
        1 | 2 => 1,
        3 => r.below(4).saturating_add(2),
        4 => r.below(30).saturating_add(5),
        _ => r.below(60).saturating_add(40),
    };
    (0..count)
        .filter_map(|_| r.pick(graphemes::CHARACTERS).copied())
        .collect()
}

fn edit(r: &mut Rng, len: usize) -> Edit {
    let at = |r: &mut Rng| r.below(len.saturating_add(3));
    match r.below(9) {
        0..=2 => Edit::Set {
            i: at(r),
            text: text(r),
            wide: r.chance(30),
            style: style(r),
        },
        3 => Edit::Continuation { i: at(r) },
        4 => Edit::SetAttributes {
            i: at(r),
            style: style(r),
        },
        5 => {
            let (x, y) = (at(r), at(r));
            Edit::Fill {
                start: x.min(y),
                end: x.max(y),
                text: text(r),
                wide: r.chance(20),
                style: style(r),
            }
        }
        6 => Edit::Resize {
            len: r.below(40),
            text: text(r),
            wide: r.chance(10),
            style: style(r),
        },
        7 => Edit::Copy {
            i: at(r),
            from: at(r),
        },
        _ => Edit::Collect,
    }
}

/// The constants, then `continues_cluster` on random clusters, then runs
/// of random edits: `runs` runs. How many edits were compared.
pub fn check(seed: u64, runs: usize) -> Result<usize, Difference> {
    compare::<base::Types, work::Types>(&mut Rng::new(seed), runs)
}

fn compare<A: Standalone, B: Standalone>(r: &mut Rng, runs: usize) -> Result<usize, Difference> {
    same_list(None, "constants", &A::constants(), &B::constants())?;
    for _ in 0..runs.saturating_mul(20) {
        let cluster = text(r);
        let Some(&c) = r.pick(graphemes::CHARACTERS) else {
            continue;
        };
        same(
            None,
            &format!("continues_cluster({cluster:?}, {c:?})"),
            &A::continues_cluster(&cluster, c),
            &B::continues_cluster(&cluster, c),
        )?;
    }
    let mut edits = 0usize;
    let (mut ra, mut rb) = (Row::default(), Row::default());
    for run in 0..runs {
        let len = r.below(30);
        let (mut a, mut b) = (A::run(len), B::run(len));
        let mut log: Vec<String> = vec![format!("Cells::new({len})")];
        for _ in 0..r.below(40).saturating_add(1) {
            A::read(&a, &mut ra);
            let e = edit(r, ra.len);
            log.push(format!("{e:?}"));
            let context = || format!("run {run}, after\n    {}", log.join("\n    "));
            same(
                None,
                &context(),
                &A::apply(&mut a, &e),
                &B::apply(&mut b, &e),
            )?;
            A::read(&a, &mut ra);
            B::read(&b, &mut rb);
            same(None, &context(), &ra, &rb)?;
            edits = edits.saturating_add(1);
        }
    }
    Ok(edits)
}
