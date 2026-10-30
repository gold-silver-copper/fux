//! Independent models of what fux-vt should do, shared by the property
//! tests and the fuzz targets: how printed text is segmented into cells
//! (UAX #29 by unicode-segmentation, widths by unicode-width as fux-vt's
//! written rule tailors it), plus cell-operation fixtures and contracts.
#![allow(dead_code, reason = "each user takes the models it needs")]

use fux_vt::{Attributes, CLUSTER_CAPACITY, CellRef, Cells, Color};

use std::collections::VecDeque;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// The spacing marks unicode-width makes 0 and fux-vt's rule 1, the
/// canonical decompositions of two whose second is one, `(first, second)
/// -> composed`, and every spacing mark (`data/spacing-marks.txt`, from
/// UnicodeData.txt).
type Marks = (
    std::collections::HashSet<char>,
    std::collections::HashMap<(char, char), char>,
    std::collections::HashSet<char>,
);

fn marks() -> &'static Marks {
    static MARKS: std::sync::OnceLock<Marks> = std::sync::OnceLock::new();
    MARKS.get_or_init(|| {
        let hex = |h: &str| u32::from_str_radix(h, 16).ok().and_then(char::from_u32);
        let mut marks = Marks::default();
        let text = include_str!("../data/spacing-marks.txt");
        for line in text.lines().filter(|line| !line.starts_with('#')) {
            let fields: Vec<&str> = line.split_whitespace().collect();
            match fields[..] {
                [code, "widened"] => {
                    marks.0.extend(hex(code));
                    marks.2.extend(hex(code));
                }
                [code] => marks.2.extend(hex(code)),
                [whole, "=", first, second] => {
                    if let (Some(w), Some(f), Some(s)) = (hex(whole), hex(first), hex(second)) {
                        marks.1.insert((f, s), w);
                    }
                }
                _ => {}
            }
        }
        marks
    })
}

/// The columns `c` takes, by fux-vt's written width rule (its README, "A
/// glyph", B32): unicode-width's, but a cell holds at most 2, so U+17D8,
/// the one character unicode-width makes 3 columns wide, takes 1, as in
/// xterm 411 and glibc; and a spacing mark of combining class 0 it makes
/// 0 takes 1, as in wcwidth 0.9.2.
pub fn width(c: char) -> Option<usize> {
    if marks().0.contains(&c) {
        return Some(1);
    }
    c.width().map(|w| if w > 2 { 1 } else { w })
}

/// The Khmer letters unicode-width 0.2.2 subjoins after a coeng (its
/// `KHMER_COENG_ELIGIBLE_LETTER`).
fn khmer_coeng_eligible(c: char) -> bool {
    matches!(
        c,
        '\u{1780}'..='\u{1782}'
            | '\u{1784}'..='\u{1787}'
            | '\u{1789}'..='\u{178c}'
            | '\u{178e}'..='\u{1793}'
            | '\u{1795}'..='\u{1798}'
            | '\u{179b}'..='\u{179d}'
            | '\u{17a0}'
            | '\u{17a2}'
            | '\u{17a7}'
            | '\u{17ab}'
            | '\u{17ac}'
            | '\u{17af}'
    )
}

/// The columns `text` takes by the same rule: unicode-width's, each
/// character it makes wider than 2 counted at 1, without its rule that a
/// Khmer coeng takes a column back before a letter it subjoins (the coeng
/// takes none, its letter one),
/// each spacing mark it makes 0 and the rule 1 one more; but each
/// character's alone in place of the columns of the text composed (each
/// pair of a character and such a mark that composes canonically made
/// one, from the start), where such a mark right after another spacing
/// mark takes none.
fn text_width(text: &str) -> usize {
    let over = text.chars().filter(|c| c.width().is_some_and(|w| w > 2));
    let extra = over
        .map(|c| c.width().unwrap_or(1).saturating_sub(1))
        .sum::<usize>();
    let (widened, pairs, spacing) = marks();
    let alone = |c: char| width(c).unwrap_or(1);
    let counted = |c: char, before: Option<char>| {
        if widened.contains(&c) && before.is_some_and(|b| spacing.contains(&b)) {
            0
        } else {
            alone(c)
        }
    };
    // unicode-width's columns, the widened marks' one each, then the rule's
    // for the composed text in place of each character's alone.
    // unicode-width takes a column back for each coeng before a letter it
    // subjoins (its KHMER_COENG_ELIGIBLE_LETTER); the rule does not.
    let subjoined = text
        .chars()
        .zip(text.chars().skip(1))
        .filter(|&(c, next)| c == '\u{17d2}' && khmer_coeng_eligible(next))
        .count();
    let base = (text.width().saturating_sub(extra))
        .saturating_add(subjoined)
        .saturating_add(text.chars().filter(|c| widened.contains(c)).count());
    let summed = text.chars().map(alone).fold(0usize, usize::saturating_add);
    let mut rule = 0usize;
    let mut chars = text.chars();
    if let Some(first) = chars.next() {
        let (mut before, mut pending) = (None, first);
        for c in chars {
            match pairs.get(&(pending, c)) {
                Some(&whole) => pending = whole,
                None => {
                    rule = rule.saturating_add(counted(pending, before));
                    (before, pending) = (Some(pending), c);
                }
            }
        }
        rule = rule.saturating_add(counted(pending, before));
    }
    base.saturating_add(rule).saturating_sub(summed)
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

/// Adds `c` to `text` if the cell has dropped no character and the result
/// fits in `CLUSTER_CAPACITY` bytes. Once full, it keeps only its prefix,
/// even across the cluster boundaries the zero-width tailoring joins.
fn append(text: &mut String, c: char, full: &mut bool) -> bool {
    let fits = !*full
        && text
            .len()
            .checked_add(c.len_utf8())
            .is_some_and(|n| n <= CLUSTER_CAPACITY);
    *full = !fits;
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
    let mut full = false;
    let mut queue: VecDeque<String> = clusters(text).into();
    while let Some(cluster) = queue.pop_front() {
        let mut chars = cluster.chars();
        let Some(first) = chars.next() else { continue };
        if zero_prepend(first)
            && let Some((text, wide)) = cells.last_mut()
        {
            if append(text, first, &mut full) {
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
                    if !append(text, c, &mut full) {
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
        full = false;
        // Once a character is dropped, append drops the rest. After a
        // Prepend character of no width, kept or dropped (inside a cluster
        // begun by one of a column, U+0600), what follows is segmented
        // afresh, as after one that begins it.
        while let Some(c) = chars.next() {
            if append(&mut text, c, &mut full) {
                wide |= text_width(&text) >= 2;
            }
            if zero_prepend(c) {
                for next in clusters(chars.as_str()).into_iter().rev() {
                    queue.push_front(next);
                }
                break;
            }
        }
        cells.push((text, wide));
    }
    cells
}

/// Whether `c` is a Prepend character of no width, which joins no cluster
/// after it (README, "A character of no width that joins no cluster"):
/// UAX #29 joins it to a letter after it (GB9b).
fn zero_prepend(c: char) -> bool {
    width(c) == Some(0) && clusters(&format!("{c}a")).len() == 1
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

/// A store reports exact text or a maximal UTF-8 prefix, keeping shape
/// and attributes. Missing destinations refuse.
pub fn stored(cells: &Cells, i: usize, source: CellRef<'_>, whole: bool) {
    let Some(got) = cells.get(i) else {
        assert!(i >= cells.len());
        assert!(!whole);
        return;
    };
    let text = got.contents();
    assert!(source.contents().starts_with(text));
    assert_eq!(whole, text == source.contents());
    assert!(text.len() <= CLUSTER_CAPACITY);
    if !whole {
        let limit = if text.len() <= 17 {
            17
        } else {
            CLUSTER_CAPACITY
        };
        let next = source
            .contents()
            .get(text.len()..)
            .and_then(|s| s.chars().next());
        assert!(next.is_some_and(|c| text.len().saturating_add(c.len_utf8()) > limit));
    }
    assert_eq!(got.is_wide(), source.is_wide());
    assert_eq!(got.is_wide_continuation(), source.is_wide_continuation());
    assert_eq!(got.attributes(), source.attributes());
}

/// Restyling keeps a cell's text and halves and sets its attributes.
pub fn restyled(cells: &Cells, before: &Cells, i: usize, attributes: Attributes) {
    let (Some(old), Some(got)) = (before.get(i), cells.get(i)) else {
        assert!(i >= before.len() && i >= cells.len());
        return;
    };
    assert_eq!(got.contents(), old.contents());
    assert_eq!(
        (got.is_wide(), got.is_wide_continuation()),
        (old.is_wide(), old.is_wide_continuation())
    );
    assert_eq!(got.attributes(), attributes);
}

/// An edit leaves every cell outside its range untouched.
pub fn unchanged(cells: &Cells, before: &Cells, range: std::ops::Range<usize>) {
    assert!(cells.range(0..range.start).eq(before.range(0..range.start)));
    assert!(
        cells
            .range(range.end..usize::MAX)
            .eq(before.range(range.end..usize::MAX))
    );
}
