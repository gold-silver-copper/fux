//! Properties checked against independent models on random input:
//! printed text is segmented into cells as UAX #29 has it; reflow keeps
//! every line's text; `Cells` behaves as a plain list of cells; the kitty
//! keyboard flag stacks behave as plain stacks.

use fux_vt::{Attributes, Cell, Cells, Color, Options, Parser};
use std::collections::VecDeque;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
#[path = "corpus/graphemes.rs"]
mod graphemes;

type Result = std::result::Result<(), Box<dyn std::error::Error>>;

/// A small deterministic generator (splitmix64), so a failure names its case.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut x = self.0;
        x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        x ^ (x >> 31)
    }
    /// Below `n`, or 0 for an `n` of 0.
    fn below(&mut self, n: usize) -> usize {
        let n = u64::try_from(n).unwrap_or(u64::MAX);
        let value = self.next().checked_rem(n).unwrap_or(0);
        usize::try_from(value).unwrap_or(0)
    }
    /// From 1 to `n`.
    fn upto(&mut self, n: usize) -> usize {
        self.below(n).saturating_add(1)
    }
    fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }
    fn pick<T: Copy>(&mut self, items: &[T]) -> Option<T> {
        items.get(self.below(items.len())).copied()
    }
}

// Printing and grapheme clusters.

/// Whether `c` continues any cluster: Extend, ZWJ or SpacingMark.
fn extends(c: char) -> bool {
    format!("a{c}").graphemes(true).count() == 1
}

/// Whether `c` is Prepend: it joins a letter after it.
fn prepend(c: char) -> bool {
    format!("{c}a").graphemes(true).count() == 1
}

/// A character fux-vt prints as a glyph or joins to one: not a control, and
/// if it takes no columns, one that continues a cluster (others have no
/// cell to go in, and are attached to whatever is before them). U+17D8, the
/// one character unicode-width makes three columns wide, is left to its own
/// test (`a_three_column_character_*`).
fn printable(c: char) -> bool {
    !c.is_control()
        && c != '\u{FFFD}'
        && c.width().is_some_and(|w| w <= 2)
        && (c.width() != Some(0) || extends(c))
}

fn character(r: &mut Rng, first: bool) -> char {
    loop {
        let c = if r.chance(85) {
            r.pick(graphemes::CHARACTERS).unwrap_or('a')
        } else {
            let p = u32::try_from(r.below(0x3_0000)).unwrap_or(0x61);
            char::from_u32(p).unwrap_or('a')
        };
        if printable(c) && (!first || c.width().is_some_and(|w| w > 0)) {
            return c;
        }
    }
}

/// The clusters fux-vt makes of `text`: UAX #29's, broken after a Prepend
/// character unless what follows extends it.
fn clusters(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for grapheme in text.graphemes(true) {
        let mut current = String::new();
        let mut previous = None;
        for c in grapheme.chars() {
            if previous.is_some_and(prepend) && !extends(c) {
                out.push(std::mem::take(&mut current));
            }
            current.push(c);
            previous = Some(c);
        }
        out.push(current);
    }
    out
}

/// Adds `c` to `text` if the result fits in `Cell::CLUSTER_CAPACITY` bytes.
fn append(text: &mut String, c: char) -> bool {
    let fits = text
        .len()
        .checked_add(c.len_utf8())
        .is_some_and(|n| n <= Cell::CLUSTER_CAPACITY);
    if fits {
        text.push(c);
    }
    fits
}

/// The cells `text` printed on one line makes, as (text, wide): one a
/// cluster, keeping its characters until one does not fit in
/// `Cell::CLUSTER_CAPACITY` bytes, wide once a kept prefix is two columns
/// wide. A cluster that starts with characters taking no columns (after a
/// Control-class format character, which UAX #29 breaks after) has no cell
/// for them: they join the cell before, as marks always have, without
/// widening it, and the rest of it starts afresh.
fn cells_of(text: &str) -> Vec<(String, bool)> {
    let mut cells: Vec<(String, bool)> = Vec::new();
    let mut queue: VecDeque<String> = clusters(text).into();
    while let Some(cluster) = queue.pop_front() {
        let mut chars = cluster.chars();
        let Some(first) = chars.next() else { continue };
        if first.width() == Some(0) {
            let leading: String = cluster
                .chars()
                .take_while(|c| c.width() == Some(0))
                .collect();
            if let Some((text, _)) = cells.last_mut() {
                for c in leading.chars() {
                    append(text, c);
                }
            }
            let rest: String = cluster.chars().skip(leading.chars().count()).collect();
            for next in clusters(&rest).into_iter().rev() {
                queue.push_front(next);
            }
            continue;
        }
        let mut text = String::from(first);
        let mut wide = first.width() == Some(2);
        // A start of the cluster: once a character is dropped, the rest go.
        for c in chars {
            if !append(&mut text, c) {
                break;
            }
            wide |= text.width() >= 2;
        }
        cells.push((text, wide));
    }
    cells
}

/// Random text printed on one line, in random pieces (splitting UTF-8) with
/// SGR between characters, gives one cell to each cluster fux-vt makes,
/// holding all of it that fits, as wide as unicode-width lays it out, and
/// leaves the cursor after the last.
#[test]
fn printed_text_is_segmented_as_the_model_says() -> Result {
    let mut r = Rng(0x6ea9_5e61_0000_0001);
    let mut checked = 0usize;
    for case in 0..20_000 {
        let text: String = (0..r.upto(40)).map(|i| character(&mut r, i == 0)).collect();
        let expected = cells_of(&text);
        let columns = expected.iter().fold(0usize, |sum, (_, wide)| {
            sum.saturating_add(if *wide { 2 } else { 1 })
        });
        // Wide enough for every cluster, and for all their text to spill.
        let cols = u16::try_from(columns.max(text.len()).saturating_add(4))?;
        let mut input = Vec::new();
        for c in text.chars() {
            if r.chance(10) {
                input.extend_from_slice(b"\x1b[1m");
            }
            let mut buffer = [0; 4];
            input.extend_from_slice(c.encode_utf8(&mut buffer).as_bytes());
        }
        let mut parser = Parser::new(2, cols, 0)?;
        let mut rest = input.as_slice();
        while !rest.is_empty() {
            let (piece, tail) = rest
                .split_at_checked(r.upto(7).min(rest.len()))
                .unwrap_or((rest, &[]));
            parser.process(piece)?;
            rest = tail;
        }
        let screen = parser.screen();
        let row = screen.row_from_bottom(1).ok_or("row")?;
        let mut col = 0usize;
        for (text_expected, wide) in &expected {
            let cell = row.cell(col).ok_or("cell")?;
            assert_eq!(
                (cell.contents(), cell.is_wide()),
                (text_expected.as_str(), *wide),
                "case {case}: {text:?} at column {col}: {:?}",
                row.cells().map(|c| c.contents()).collect::<Vec<_>>()
            );
            if *wide {
                let second = row.cell(col.saturating_add(1)).ok_or("second half")?;
                assert!(second.is_wide_continuation(), "case {case}: {text:?}");
            }
            col = col.saturating_add(if *wide { 2 } else { 1 });
        }
        assert!(
            row.cell(col).is_some_and(|c| !c.has_contents()),
            "case {case}: {text:?}"
        );
        assert_eq!(
            usize::from(screen.cursor_position().1),
            col,
            "case {case}: {text:?}"
        );
        checked = checked.saturating_add(expected.len());
    }
    assert!(checked > 200_000, "{checked}");
    Ok(())
}

/// U+17D8 KHMER SIGN BEYYAL, three columns wide by unicode-width, takes one
/// narrow cell and the two after it blank: what follows lands where
/// unicode-width lays it out, as it did in 0.1.5.
#[test]
fn a_three_column_character_takes_a_cell_and_two_blanks() -> Result {
    let mut parser = Parser::new(2, 10, 0)?;
    parser.process("a\u{17D8}b".as_bytes())?;
    let screen = parser.screen();
    let cell = |col| screen.cell(0, col).map(|c| (c.contents(), c.is_wide()));
    assert_eq!(cell(1), Some(("\u{17D8}", false)));
    assert_eq!(cell(2), Some(("", false)));
    assert_eq!(cell(3), Some(("", false)));
    assert_eq!(cell(4), Some(("b", false)));
    assert_eq!(screen.cursor_position(), (0, 5));
    Ok(())
}

// Reflow.

/// Pieces of a line: words, wide glyphs, marks, and now and then a cluster
/// too long for a cell to hold inline.
const PIECES: &[&str] = &[
    "a",
    "hello",
    " ",
    "world ",
    "界",
    "界界",
    "e\u{301}",
    "x",
    "0123456789",
    "ｱ",
    "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}",
    "\u{1F3F4}\u{E0067}\u{E0062}\u{E0073}\u{E0063}\u{E0074}\u{E007F}",
    "\u{1F44D}\u{1F3FD}",
];

/// Every line of the primary screen and its history, soft-wrapped rows
/// joined, as text: a blank a reflow left before a wide glyph it moved to
/// the next row is no part of it, nor are blanks ending a line, nor blank
/// lines at the end.
fn lines(parser: &Parser) -> Vec<String> {
    let screen = parser.screen();
    let retained = screen
        .history_len()
        .saturating_add(usize::from(screen.size().0));
    let rows: Vec<_> = (0..retained)
        .rev()
        .filter_map(|offset| screen.row_from_bottom(offset))
        .collect();
    let mut out = vec![String::new()];
    for (i, row) in rows.iter().enumerate() {
        let next_wide = rows
            .get(i.saturating_add(1))
            .and_then(|next| next.cell(0))
            .is_some_and(|c| c.is_wide());
        let last = row.len().saturating_sub(1);
        let mut line = out.pop().unwrap_or_default();
        for (col, cell) in row.cells().enumerate() {
            let spacer = row.wrapped && col == last && next_wide && !cell.has_contents();
            if cell.is_wide_continuation() || spacer {
                continue;
            }
            line.push_str(if cell.has_contents() {
                cell.contents()
            } else {
                " "
            });
        }
        out.push(line);
        if !row.wrapped {
            out.push(String::new());
        }
    }
    let mut out: Vec<String> = out.iter().map(|l| l.trim_end().to_owned()).collect();
    while out.last().is_some_and(String::is_empty) {
        out.pop();
    }
    out
}

/// Reflowing to a narrower width and back keeps every line's text, and the
/// cursor where it was; so does reflowing to the narrower width alone.
#[test]
fn reflow_narrower_and_back_keeps_every_line() -> Result {
    let options = Options {
        reflow: true,
        ..Options::default()
    };
    let mut r = Rng(0x0ef1_0000_0000_0002);
    for case in 0..4_000 {
        let rows = u16::try_from(r.below(10).saturating_add(3))?;
        let cols = u16::try_from(r.below(40).saturating_add(8))?;
        let mut text = String::new();
        for line in 0..r.upto(12) {
            if line > 0 {
                text.push_str("\r\n");
            }
            for _ in 0..r.below(12) {
                text.push_str(r.pick(PIECES).unwrap_or("a"));
            }
        }
        let mut parser = Parser::with_options(rows, cols, 1_000, options)?;
        parser.process(text.as_bytes())?;
        let before = lines(&parser);
        let cursor = parser.screen().cursor_position();
        let narrow = u16::try_from(
            r.below(usize::from(cols).saturating_sub(2))
                .saturating_add(2),
        )?;
        let taller = u16::try_from(r.below(4))?;
        parser.resize(rows.saturating_add(taller), narrow)?;
        assert_eq!(
            lines(&parser),
            before,
            "case {case}: {text:?} at {narrow} columns"
        );
        parser.resize(rows, cols)?;
        assert_eq!(
            lines(&parser),
            before,
            "case {case}: {text:?} back from {narrow}"
        );
        assert_eq!(
            parser.screen().cursor_position(),
            cursor,
            "case {case}: {text:?}"
        );
    }
    Ok(())
}

// Cells.

/// A cell as the model keeps it.
#[derive(Clone, Debug, PartialEq)]
struct Model {
    text: String,
    wide: bool,
    continuation: bool,
    attributes: Attributes,
}

impl Model {
    fn blank() -> Self {
        Self {
            text: String::new(),
            wide: false,
            continuation: false,
            attributes: Attributes::default(),
        }
    }
}

/// The longest start of `text` of at most `max` bytes, in whole chars.
fn floor(text: &str, max: usize) -> String {
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
fn kept(model: &[Model], i: usize, text: &str) -> String {
    let text = floor(text, Cell::CLUSTER_CAPACITY);
    if text.len() <= Cell::INLINE_CAPACITY {
        return text;
    }
    let others = model
        .iter()
        .enumerate()
        .filter(|(j, m)| *j != i && m.text.len() > Cell::INLINE_CAPACITY)
        .fold(0usize, |sum, (_, m)| sum.saturating_add(m.text.len()));
    if others.saturating_add(text.len()) <= Cells::text_limit(model.len()) {
        text
    } else {
        floor(&text, Cell::INLINE_CAPACITY)
    }
}

/// Sets model cell `i`, if there is one.
fn put(model: &mut [Model], i: usize, cell: Model) {
    if let Some(slot) = model.get_mut(i) {
        *slot = cell;
    }
}

/// Random edits to `Cells`, of every kind, on runs small enough that their
/// text budget runs out, agree with a plain list of cells after each.
#[test]
fn cells_agree_with_a_plain_list_of_cells() -> Result {
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
    let styles = [
        Attributes::default(),
        Attributes::new(Color::Idx(1), Color::Rgb(1, 2, 3)).with_bold(true),
        Attributes::default().with_underline_color(Color::Idx(9)),
    ];
    let mut r = Rng(0xce11_5000_0000_0003);
    let mut edits = 0usize;
    for _ in 0..1_500 {
        let len = r.upto(6);
        let mut cells = Cells::new(len);
        let mut model = vec![Model::blank(); len];
        let mut other = Cells::new(4);
        for _ in 0..60 {
            let i = r.below(model.len().saturating_add(1));
            let text = texts.get(r.below(texts.len())).cloned().unwrap_or_default();
            let wide = r.chance(30);
            let attributes = r.pick(&styles).unwrap_or_default();
            match r.below(6) {
                0 | 1 => {
                    let whole = cells.set_text(i, &text, wide, attributes);
                    if i < model.len() {
                        let keep = kept(&model, i, &text);
                        assert_eq!(whole, keep == text, "{text:?}");
                        let cell = Model {
                            text: keep,
                            wide,
                            continuation: false,
                            attributes,
                        };
                        put(&mut model, i, cell);
                    }
                }
                2 => {
                    // From another run, wherever it keeps its text.
                    let j = r.below(4);
                    other.set_text(j, &text, wide, attributes);
                    if let Some(from) = other.get(j) {
                        let whole = cells.set(i, from);
                        if i < model.len() {
                            let keep = kept(&model, i, from.contents());
                            assert_eq!(whole, keep == from.contents());
                            let cell = Model {
                                text: keep,
                                wide: from.is_wide(),
                                continuation: false,
                                attributes: from.attributes(),
                            };
                            put(&mut model, i, cell);
                        }
                    }
                }
                3 => {
                    cells.set_attributes(i, attributes);
                    if let Some(m) = model.get_mut(i) {
                        m.attributes = attributes;
                    }
                }
                4 => {
                    let (a, b) = (
                        r.below(len.saturating_add(2)),
                        r.below(len.saturating_add(2)),
                    );
                    let (a, b) = (a.min(b), a.max(b));
                    let continuation = r.chance(50);
                    let cell = if continuation {
                        Cell::wide_continuation()
                    } else {
                        Cell::new("z", false, attributes).unwrap_or_default()
                    };
                    cells.fill(a..b, cell);
                    for m in model.iter_mut().take(b).skip(a) {
                        *m = if continuation {
                            Model {
                                continuation: true,
                                ..Model::blank()
                            }
                        } else {
                            Model {
                                text: "z".into(),
                                wide: false,
                                continuation: false,
                                attributes,
                            }
                        };
                    }
                }
                _ => {
                    let new = r.below(8);
                    cells.resize(new, Cell::default());
                    model.resize(new, Model::blank());
                    // A shorter run keeps a shorter run's budget: cells whose
                    // text no longer fits, left to right, keep what fits
                    // inline.
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
            edits = edits.saturating_add(1);
            assert_eq!(cells.len(), model.len());
            for (i, m) in model.iter().enumerate() {
                let cell = cells.get(i).ok_or("cell")?;
                let got = Model {
                    text: cell.contents().to_owned(),
                    wide: cell.is_wide(),
                    continuation: cell.is_wide_continuation(),
                    attributes: cell.attributes(),
                };
                assert_eq!(&got, m, "cell {i} of {model:?}");
            }
            assert!(cells.text_len() <= Cells::text_limit(cells.len()));
            let copy: Cells = cells.iter().collect();
            assert_eq!(copy, cells);
        }
    }
    assert!(edits > 80_000, "{edits}");
    Ok(())
}

// The kitty keyboard protocol.

/// Random pushes, pops, sets, screen switches and resets agree with two
/// plain stacks, one a screen, of at most 32 flag sets.
#[test]
fn kitty_keyboard_flags_agree_with_two_plain_stacks() -> Result {
    let options = Options {
        kitty_keyboard: true,
        ..Options::default()
    };
    let mut r = Rng(0x0c17_7700_0000_0004);
    for case in 0..300 {
        let mut parser = Parser::with_options(4, 10, 0, options)?;
        let mut primary: Vec<u8> = Vec::new();
        let mut secondary: Vec<u8> = Vec::new();
        let mut alternate = false;
        let mut log = String::new();
        for _ in 0..80 {
            let n = r.below(40);
            let flags = u8::try_from(n).unwrap_or(u8::MAX);
            let stack = if alternate {
                &mut secondary
            } else {
                &mut primary
            };
            let sequence = match r.below(7) {
                0 => {
                    if stack.len() == 32 {
                        // The oldest goes.
                        *stack = stack.iter().skip(1).copied().collect();
                    }
                    stack.push(flags);
                    format!("\x1b[>{n}u")
                }
                1 => {
                    let count = r.below(4);
                    let keep = stack.len().saturating_sub(count.max(1));
                    stack.truncate(keep);
                    if count == 0 {
                        "\x1b[<u".to_owned()
                    } else {
                        format!("\x1b[<{count}u")
                    }
                }
                2 => {
                    let mode = r.below(4);
                    if stack.is_empty() {
                        stack.push(0);
                    }
                    if let Some(top) = stack.last_mut() {
                        *top = match mode {
                            2 => *top | flags,
                            3 => *top & !flags,
                            _ => flags,
                        };
                    }
                    format!("\x1b[={n};{mode}u")
                }
                3 => {
                    alternate = !alternate;
                    if alternate {
                        "\x1b[?1049h".into()
                    } else {
                        "\x1b[?1049l".into()
                    }
                }
                4 if r.chance(10) => {
                    primary.clear();
                    secondary.clear();
                    alternate = false;
                    "\x1bc".into()
                }
                _ => "\x1b[?u".into(),
            };
            log.push_str(&sequence);
            parser.process(sequence.as_bytes())?;
            let stack = if alternate { &secondary } else { &primary };
            let expected = stack.last().copied().unwrap_or(0);
            assert_eq!(
                parser.screen().kitty_keyboard_flags(),
                expected,
                "case {case}: {log:?}"
            );
        }
    }
    Ok(())
}
