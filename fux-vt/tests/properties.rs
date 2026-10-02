//! Properties checked against independent models on random input:
//! printed text is segmented into cells as UAX #29 has it; reflow keeps
//! every line's text; `Cells` behaves as a plain list of cells; the kitty
//! keyboard flag stacks behave as plain stacks.

use fux_vt::{Cells, Options, Parser};
use unicode_width::UnicodeWidthChar;
#[path = "corpus/graphemes.rs"]
mod graphemes;
#[path = "corpus/models.rs"]
mod models;
use models::{CellsOp, Model, cells_of, printable};

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
            let spacer = row.wrapped() && col == last && next_wide && !cell.has_contents();
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
        if !row.wrapped() {
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
    let options = Options::new().with_reflow(true);
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

/// Random edits to `Cells`, of every kind, on runs small enough that their
/// text budget runs out, agree with a plain list of cells after each.
#[test]
fn cells_agree_with_a_plain_list_of_cells() -> Result {
    let texts = models::texts();
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
            let attributes = r.pick(&models::STYLES).unwrap_or_default();
            let op = match r.below(6) {
                0 | 1 => CellsOp::SetText {
                    i,
                    text,
                    wide,
                    attributes,
                },
                2 => CellsOp::SetFrom {
                    i,
                    j: r.below(4),
                    text,
                    wide,
                    attributes,
                },
                3 => CellsOp::SetAttributes { i, attributes },
                4 => CellsOp::Fill {
                    a: r.below(len.saturating_add(2)),
                    b: r.below(len.saturating_add(2)),
                    continuation: r.chance(50),
                    attributes,
                },
                _ => CellsOp::Resize { len: r.below(8) },
            };
            models::apply(&mut cells, &mut other, &mut model, &op);
            models::check(&cells, &model);
            edits = edits.saturating_add(1);
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
    let options = Options::new().with_kitty_keyboard(true);
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
