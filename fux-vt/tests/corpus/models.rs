//! Independent models of what fux-vt should do, shared by the property
//! tests and the fuzz targets: how printed text is segmented into cells
//! (UAX #29 by unicode-segmentation, widths by unicode-width as fux-vt's
//! written rule tailors it), and what a
//! run of `Cells` holds after edits.
#![allow(dead_code, reason = "each user takes the models it needs")]

use fux_vt::{Attributes, CLUSTER_CAPACITY, CellRef, Cells, Color};

/// The longest a short cluster is: what a long one `Cells` has no room
/// for is cut to.
const SHORT: usize = 17;
use std::collections::VecDeque;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// The columns `c` takes, by fux-vt's written width rule (its README, "A
/// glyph", B32): unicode-width's, but a cell holds at most 2, so U+17D8,
/// the one character unicode-width makes 3 columns wide, takes 1, as in
/// xterm 411 and glibc.
pub fn width(c: char) -> Option<usize> {
    c.width().map(|w| if w > 2 { 1 } else { w })
}

/// The columns `text` takes by the same rule: unicode-width's, each
/// character it makes wider than 2 counted at 1.
fn text_width(text: &str) -> usize {
    let over = text.chars().filter(|c| c.width().is_some_and(|w| w > 2));
    let extra = over
        .map(|c| c.width().unwrap_or(1).saturating_sub(1))
        .sum::<usize>();
    text.width().saturating_sub(extra)
}

/// A character fux-vt prints as a glyph or joins to one: any but a
/// control, which is carried out. One of no width that joins no cluster is
/// printable too: `cells_of` places it by the README's written rule.
pub fn printable(c: char) -> bool {
    !c.is_control() && width(c).is_some()
}

/// The clusters fux-vt makes of `text`: UAX #29's.
pub fn clusters(text: &str) -> Vec<String> {
    text.graphemes(true).map(str::to_owned).collect()
}

/// Adds `c` to `text` if the result fits in `CLUSTER_CAPACITY` bytes.
pub fn append(text: &mut String, c: char) -> bool {
    let fits = text
        .len()
        .checked_add(c.len_utf8())
        .is_some_and(|n| n <= CLUSTER_CAPACITY);
    if fits {
        text.push(c);
    }
    fits
}

/// The cells `text` printed on one line makes, as (text, wide): one a
/// cluster, keeping its characters until one does not fit in
/// `CLUSTER_CAPACITY` bytes, wide once a kept prefix is two columns
/// wide. A cluster that starts with a character taking no columns (a
/// format control, a zero-width Prepend character, a mark or a lone vowel
/// or trailing jamo after a break) has no cell of its own, as the README's
/// departure from UAX #29 says ("A character of no width that joins no
/// cluster"): it joins the cell before, with what UAX #29 joins to it, the
/// cell widening if they make it two columns, but for a zero-width Prepend
/// character, which joins it alone, what follows segmented afresh; at the
/// line's start its characters of no width are dropped, and the rest
/// starts afresh.
pub fn cells_of(text: &str) -> Vec<(String, bool)> {
    let mut cells: Vec<(String, bool)> = Vec::new();
    let mut queue: VecDeque<String> = clusters(text).into();
    while let Some(cluster) = queue.pop_front() {
        let mut chars = cluster.chars();
        let Some(first) = chars.next() else { continue };
        // Prepend: UAX #29 joins it to a letter after it (GB9b).
        let prepend = width(first) == Some(0) && clusters(&format!("{first}a")).len() == 1;
        if prepend && let Some((text, wide)) = cells.last_mut() {
            if append(text, first) {
                *wide |= text_width(text) >= 2;
            }
            for next in clusters(chars.as_str()).into_iter().rev() {
                queue.push_front(next);
            }
            continue;
        }
        if width(first) == Some(0) {
            if let Some((text, wide)) = cells.last_mut() {
                for c in cluster.chars() {
                    if !append(text, c) {
                        break;
                    }
                    *wide |= text_width(text) >= 2;
                }
            } else {
                let rest: String = cluster
                    .chars()
                    .skip_while(|c| width(*c) == Some(0))
                    .collect();
                for next in clusters(&rest).into_iter().rev() {
                    queue.push_front(next);
                }
            }
            continue;
        }
        let mut text = String::from(first);
        let mut wide = width(first) == Some(2);
        // A start of the cluster: once a character is dropped, the rest go.
        for c in chars {
            if !append(&mut text, c) {
                break;
            }
            wide |= text_width(&text) >= 2;
        }
        cells.push((text, wide));
    }
    cells
}

/// A cell as the model keeps it.
#[derive(Clone, Debug, PartialEq)]
pub struct Model {
    pub text: String,
    pub wide: bool,
    pub continuation: bool,
    pub attributes: Attributes,
}

impl Model {
    pub fn blank() -> Self {
        Self {
            text: String::new(),
            wide: false,
            continuation: false,
            attributes: Attributes::default(),
        }
    }
}

/// The longest start of `text` of at most `max` bytes, in whole chars.
pub fn floor(text: &str, max: usize) -> String {
    let mut out = String::new();
    for c in text.chars() {
        if out.len().saturating_add(c.len_utf8()) > max {
            break;
        }
        out.push(c);
    }
    out
}

/// The text `Cells` keeps of `text` stored at `i`, given the others:
/// `CLUSTER_CAPACITY` bytes of it at most, in whole chars; and if what the
/// others keep beyond their cells leaves no room, what fits inline.
pub fn kept(model: &[Model], i: usize, text: &str) -> String {
    let text = floor(text, CLUSTER_CAPACITY);
    if text.len() <= SHORT {
        return text;
    }
    let others = model
        .iter()
        .enumerate()
        .filter(|(j, m)| *j != i && m.text.len() > SHORT)
        .fold(0usize, |sum, (_, m)| sum.saturating_add(m.text.len()));
    if others.saturating_add(text.len()) <= Cells::text_limit(model.len()) {
        text
    } else {
        floor(&text, SHORT)
    }
}

/// Sets model cell `i`, if there is one.
pub fn put(model: &mut [Model], i: usize, cell: Model) {
    if let Some(slot) = model.get_mut(i) {
        *slot = cell;
    }
}

/// Texts `Cells` operations store: inline, spilled, and past what a small
/// run's budget holds.
pub fn texts() -> Vec<String> {
    let mut texts: Vec<String> = [
        "a",
        "",
        "界",
        "e\u{301}",
        "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    for marks in [10, 35, 60, 85] {
        texts.push(
            std::iter::once('e')
                .chain(std::iter::repeat_n('\u{301}', marks))
                .collect(),
        );
    }
    texts
}

/// Styles `Cells` operations set.
pub const STYLES: [Attributes; 3] = [
    Attributes::new(Color::Default, Color::Default),
    Attributes::new(Color::Idx(1), Color::Rgb(fux_vt::Rgb { r: 1, g: 2, b: 3 })).with_bold(true),
    Attributes::new(Color::Default, Color::Default).with_underline_color(Color::Idx(9)),
];

/// An edit to a run of `Cells`. Indices may be past its end.
#[derive(Clone, Debug)]
pub enum CellsOp {
    SetText {
        i: usize,
        text: String,
        wide: bool,
        attributes: Attributes,
    },
    /// `set` from another run, which first takes the text at `j`.
    SetFrom {
        i: usize,
        j: usize,
        text: String,
        wide: bool,
        attributes: Attributes,
    },
    SetAttributes {
        i: usize,
        attributes: Attributes,
    },
    Fill {
        a: usize,
        b: usize,
        continuation: bool,
        attributes: Attributes,
    },
    Resize {
        len: usize,
    },
}

/// Applies `op` to `cells` and to `model`, the plain list of cells it
/// should equal, asserting what each operation reports. `other` is the
/// run `SetFrom` copies from.
pub fn apply(cells: &mut Cells, other: &mut Cells, model: &mut Vec<Model>, op: &CellsOp) {
    match op {
        CellsOp::SetText {
            i,
            text,
            wide,
            attributes,
        } => {
            let whole = cells.set(*i, CellRef::new(text, *wide, *attributes));
            if *i < model.len() {
                let keep = kept(model, *i, text);
                assert_eq!(whole, keep == *text, "{text:?}");
                let cell = Model {
                    text: keep,
                    wide: *wide,
                    continuation: false,
                    attributes: *attributes,
                };
                put(model, *i, cell);
            }
        }
        CellsOp::SetFrom {
            i,
            j,
            text,
            wide,
            attributes,
        } => {
            other.set(*j, CellRef::new(text, *wide, *attributes));
            if let Some(from) = other.get(*j) {
                let whole = cells.set(*i, from);
                if *i < model.len() {
                    let keep = kept(model, *i, from.contents());
                    assert_eq!(whole, keep == from.contents());
                    let cell = Model {
                        text: keep,
                        wide: from.is_wide(),
                        continuation: false,
                        attributes: from.attributes(),
                    };
                    put(model, *i, cell);
                }
            }
        }
        CellsOp::SetAttributes { i, attributes } => {
            cells.set_attributes(*i, *attributes);
            if let Some(m) = model.get_mut(*i) {
                m.attributes = *attributes;
            }
        }
        CellsOp::Fill {
            a,
            b,
            continuation,
            attributes,
        } => {
            let (a, b) = ((*a).min(*b), (*a).max(*b));
            let cell = if *continuation {
                CellRef::wide_continuation()
            } else {
                CellRef::new("z", false, *attributes)
            };
            cells.fill(a..b, cell);
            for m in model.iter_mut().take(b).skip(a) {
                *m = if *continuation {
                    Model {
                        continuation: true,
                        ..Model::blank()
                    }
                } else {
                    Model {
                        text: "z".into(),
                        wide: false,
                        continuation: false,
                        attributes: *attributes,
                    }
                };
            }
        }
        CellsOp::Resize { len } => {
            assert!(
                cells.resize(*len, CellRef::default()).is_ok(),
                "{len} cells"
            );
            model.resize(*len, Model::blank());
            // A shorter run keeps a shorter run's budget: cells whose text
            // no longer fits, left to right, keep what fits inline.
            let whole = model.clone();
            for (i, m) in whole.iter().enumerate() {
                let mut prefix = model.clone();
                for later in prefix.iter_mut().skip(i) {
                    *later = Model::blank();
                }
                if let Some(slot) = model.get_mut(i) {
                    slot.text = kept(&prefix, i, &m.text);
                }
            }
        }
    }
}

/// Asserts `cells` hold what `model` does, within their text budget, and
/// that collecting them copies them exactly.
pub fn check(cells: &Cells, model: &[Model]) {
    assert_eq!(cells.len(), model.len());
    for (i, m) in model.iter().enumerate() {
        let got = cells.get(i).map(|cell| Model {
            text: cell.contents().to_owned(),
            wide: cell.is_wide(),
            continuation: cell.is_wide_continuation(),
            attributes: cell.attributes(),
        });
        assert_eq!(got.as_ref(), Some(m), "cell {i} of {model:?}");
    }
    assert!(cells.text_len() <= Cells::text_limit(cells.len()));
    let copy: Cells = cells.iter().collect();
    assert!(copy == *cells);
}
