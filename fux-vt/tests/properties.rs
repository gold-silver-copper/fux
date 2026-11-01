//! Properties checked against independent models on random input:
//! printed text is segmented into cells as UAX #29 has it; reflow keeps
//! every line's text; `Cells` operations keep their contracts; the kitty
//! keyboard flag stacks behave as plain stacks.

use fux_vt::{CellRef, Cells, Feature, Options, Parser, Size};
#[path = "corpus/graphemes.rs"]
mod graphemes;
#[path = "corpus/models.rs"]
mod models;
use models::{cells_of, printable};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

#[expect(dead_code, reason = "each test uses different generator helpers")]
#[path = "../src/test_rng.rs"]
mod test_rng;
use test_rng::Rng;

#[path = "corpus/feed.rs"]
#[expect(dead_code, reason = "each test binary uses some of the helpers")]
mod feed;
use feed::{Feed, Look, cells_text};

// Printing and grapheme clusters.

/// GB9 joins each modifier; GB999 breaks before the vowel jamo. The
/// README's zero-width tailoring attaches the jamo to the same cell, whose
/// kept text must stay a prefix after a modifier did not fit (125 + 4).
#[test]
fn a_full_cell_drops_zero_width_clusters_after_gb999() -> Result {
    let kept: String = std::iter::once('x')
        .chain(std::iter::repeat_n('\u{1f3fb}', 31))
        .collect();
    let text = format!("{kept}\u{1f3fb}\u{1161}a");
    let expected = vec![(kept, true), ("a".to_owned(), false)];
    assert_eq!(cells_of(&text), expected);
    let mut parser = Parser::new(Size::new(1, 6)?, 0)?;
    for byte in text.bytes() {
        parser.process(&[byte])?;
    }
    let screen = parser.screen();
    let actual: Vec<_> = screen
        .rows()
        .next()
        .ok_or("row")?
        .cells()
        .filter(|c| !c.contents().is_empty() && !c.is_wide_continuation())
        .map(|c| (c.contents().to_owned(), c.is_wide()))
        .collect();
    assert_eq!(actual, expected);
    assert_eq!(screen.cursor_position(), (0, 3));
    Ok(())
}

/// GB9b joins the Prepend run, but the zero-width Prepend ends the cell
/// under the README's tailoring even when it or an earlier Prepend was
/// dropped. What follows it starts a new cell.
#[test]
fn a_dropped_zero_width_prepend_still_ends_the_cell() -> Result {
    let kept: String = std::iter::repeat_n('\u{600}', 64).collect();
    for extra in ["", "\u{600}"] {
        let text = format!("{kept}{extra}\u{605}A");
        let expected = vec![(kept.clone(), true), ("A".to_owned(), false)];
        let mut parser = Parser::new(Size::new(1, 6)?, 0)?;
        parser.process(text.as_bytes())?;
        let screen = parser.screen();
        let actual: Vec<_> = screen
            .rows()
            .next()
            .ok_or("row")?
            .cells()
            .filter(|c| !c.contents().is_empty() && !c.is_wide_continuation())
            .map(|c| (c.contents().to_owned(), c.is_wide()))
            .collect();
        assert_eq!(actual, expected, "{text:?}");
        assert_eq!(screen.cursor_position(), (0, 3));
        assert_eq!(cells_of(&text), expected, "{text:?}");
    }
    Ok(())
}

fn character(r: &mut Rng) -> char {
    loop {
        let c = if r.chance(85) {
            r.pick(graphemes::CHARACTERS).unwrap_or('a')
        } else {
            let p = u32::try_from(r.below(0x3_0000)).unwrap_or(0x61);
            char::from_u32(p).unwrap_or('a')
        };
        if printable(c) {
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
        let text: String = (0..r.upto(40)).map(|_| character(&mut r)).collect();
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
        let mut parser = Parser::new(Size::new(2, cols)?, 0)?;
        let mut rest = input.as_slice();
        while !rest.is_empty() {
            let (piece, tail) = rest
                .split_at_checked(r.upto(7).min(rest.len()))
                .unwrap_or((rest, &[]));
            parser.process(piece)?;
            rest = tail;
        }
        let screen = parser.screen();
        let row = screen.rows().nth_back(1).ok_or("row")?;
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
            row.cell(col).is_some_and(|c| c.contents().is_empty()),
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
/// column, as in xterm 411 and glibc (THREATS.md B32: a cell holds 1 or 2):
/// over text, it leaves none of it stale, and in a region of 2 columns
/// it is printed, the cursor 1 on.
#[test]
fn the_three_column_character_takes_one_column() -> Result {
    let mut parser = Parser::new(Size::new(2, 10)?, 0)?;
    parser.process("a\u{17D8}b".as_bytes())?;
    let screen = parser.screen();
    let cell = |col| screen.cell(0, col).map(|c| (c.contents(), c.is_wide()));
    assert_eq!(cell(1), Some(("\u{17D8}", false)));
    assert_eq!(cell(2), Some(("b", false)));
    assert_eq!(screen.cursor_position(), (0, 3));
    let mut parser = Parser::new(Size::new(1, 10)?, 0)?;
    parser.process("XYZW\r\u{17D8}".as_bytes())?;
    assert_eq!(
        parser.screen().window().row(0).map(|r| cells_text(&r)),
        Some("\u{17D8}YZW      ".into())
    );
    assert_eq!(parser.screen().cursor_position(), (0, 1));
    let mut parser = Parser::new(Size::new(1, 10)?, 0)?;
    parser.process("\x1b[?69h\x1b[1;2s\u{17D8}x".as_bytes())?;
    assert_eq!(
        parser.screen().window().row(0).map(|r| cells_text(&r)),
        Some("\u{17D8}x        ".into())
    );
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
    let rows: Vec<_> = parser.screen().rows().collect();
    let mut out = vec![String::new()];
    for (i, row) in rows.iter().enumerate() {
        let next_wide = rows
            .get(i.saturating_add(1))
            .and_then(|next| next.cell(0))
            .is_some_and(|c| c.is_wide());
        let last = row.len().saturating_sub(1);
        let mut line = out.pop().unwrap_or_default();
        for (col, cell) in row.cells().enumerate() {
            let spacer = row.continues() && col == last && next_wide && cell.contents().is_empty();
            if cell.is_wide_continuation() || spacer {
                continue;
            }
            line.push_str(if !cell.contents().is_empty() {
                cell.contents()
            } else {
                " "
            });
        }
        out.push(line);
        if !row.continues() {
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
    let options = Options::new().with(Feature::Reflow);
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
        let mut parser = Parser::with_options(Size::new(rows, cols)?, 1 << 20, options)?;
        parser.process(text.as_bytes())?;
        let before = lines(&parser);
        let cursor = parser.screen().cursor_position();
        let narrow = u16::try_from(
            r.below(usize::from(cols).saturating_sub(2))
                .saturating_add(2),
        )?;
        let taller = u16::try_from(r.below(4))?;
        parser.resize(Size::new(rows.saturating_add(taller), narrow)?)?;
        assert_eq!(
            lines(&parser),
            before,
            "case {case}: {text:?} at {narrow} columns"
        );
        parser.resize(Size::new(rows, cols)?)?;
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

fn repeated(c: char, n: usize) -> String {
    std::iter::repeat_n(c, n).collect()
}

#[test]
fn cells_set_and_borrowed_copy_keep_text_shape_and_report_truncation() -> Result {
    let cases = [
        ("".into(), "".into()),
        ("a".into(), "a".into()),
        ("界".into(), "界".into()),
        ("e\u{301}".into(), "e\u{301}".into()),
        (repeated('a', 17), repeated('a', 17)),
        (repeated('a', 18), repeated('a', 18)),
        (repeated('a', 128), repeated('a', 128)),
        (repeated('a', 129), repeated('a', 128)),
        (repeated('界', 43), repeated('界', 42)),
    ];
    for (text, kept) in cases {
        for attributes in models::STYLES {
            for wide in [false, true] {
                let mut cells = Cells::new(2)?;
                let mut other = Cells::new(4)?;
                let source = CellRef::new(&text, wide, attributes);
                let whole = cells.set(0, source);
                models::stored(&cells, 0, source, whole);
                assert_eq!(whole, text == kept);
                assert_eq!(cells.get(0).ok_or("cell")?.contents(), kept);
                other.set(0, CellRef::new("different locator", false, attributes));
                other.set(3, source);
                let borrowed = other.get(3).ok_or("source")?;
                cells.fill(0..2, CellRef::default());
                assert!(cells.set(0, borrowed));
                assert!(!cells.set(2, borrowed));
                assert_eq!(cells.get(0), Some(borrowed));
            }
        }
    }
    let [_, attributes, other_attributes] = models::STYLES;
    let mut cells = Cells::new(2)?;
    let text = repeated('界', 42);
    assert!(cells.set(0, CellRef::new(&text, true, attributes)));
    let source = CellRef::new(&text, true, other_attributes);
    assert!(!cells.set(1, source));
    models::stored(&cells, 1, source, false);
    assert_eq!(cells.get(1).ok_or("partial")?.contents(), repeated('界', 5));
    // Copied cell by cell, the cut text with them, they are the same.
    let mut copy = Cells::new(cells.len())?;
    for (i, cell) in cells.range(0..cells.len()).enumerate() {
        copy.set(i, cell);
    }
    assert_eq!(cells, copy);

    // Live long text admits exactly the row limit after compaction; a
    // cluster one byte past it is cut, with or without overwritten text.
    // The limit: 32 bytes a cell, and one longest cluster more (`Cells`).
    let limit: usize = 2 * 32 + 128;
    let over = repeated('c', limit.saturating_sub(127));
    for overwritten in [false, true] {
        let mut cells = Cells::new(2)?;
        if overwritten {
            for c in ['b', 'a'] {
                let text = repeated(c, limit.saturating_sub(128));
                assert!(cells.set(0, CellRef::new(&text, false, attributes)));
            }
        }
        assert!(cells.set(1, CellRef::new(&repeated('a', 128), false, attributes)));
        assert!(!cells.set(0, CellRef::new(&over, false, attributes)));
        assert_eq!(cells.get(0).ok_or("cut")?.contents(), repeated('c', 17));
    }
    Ok(())
}

#[test]
fn cell_attributes_preserve_text_shape_and_other_cells() -> Result {
    let [_, attributes, _] = models::STYLES;
    let mut cells = Cells::new(4)?;
    for (i, text) in ["a", "e\u{301}", &repeated('x', 128)]
        .into_iter()
        .enumerate()
    {
        assert!(cells.set(i, CellRef::new(text, true, attributes)));
    }
    cells.set(3, CellRef::wide_continuation());
    for i in 0..cells.len() {
        for attributes in models::STYLES {
            let before = cells.clone();
            cells.set_attributes(i, attributes);
            models::unchanged(&cells, &before, i..i.saturating_add(1));
            models::restyled(&cells, &before, i, attributes);
        }
    }
    let before = cells.clone();
    cells.set_attributes(cells.len(), attributes);
    assert_eq!(cells, before);
    Ok(())
}

#[test]
fn cell_fill_clips_ranges_and_preserves_outside_cells() -> Result {
    let [_, attributes, other_attributes] = models::STYLES;
    for (start, end) in [(0, 0), (0, 4), (1, 3), (2, 8), (4, 8), (3, 1)] {
        for fill in [
            CellRef::new("z", true, attributes),
            CellRef::wide_continuation(),
        ] {
            let mut cells = Cells::new(4)?;
            cells.fill(0..4, CellRef::new("old", false, other_attributes));
            let before = cells.clone();
            cells.fill(start..end, fill);
            models::unchanged(&cells, &before, start..end);
            assert!(cells.range(start..end).all(|got| got == fill));
        }
    }
    Ok(())
}

// The kitty keyboard protocol.

/// Random pushes, pops, sets, screen switches and resets agree with two
/// plain stacks, one a screen, of at most 32 flag sets.
#[test]
fn kitty_keyboard_flags_agree_with_two_plain_stacks() -> Result {
    let options = Options::new().with(Feature::KittyKeyboard);
    let mut r = Rng(0x0c17_7700_0000_0004);
    for case in 0..300 {
        let mut parser = Parser::with_options(Size::new(4, 10)?, 0, options)?;
        let mut primary: Vec<u8> = Vec::new();
        let mut secondary: Vec<u8> = Vec::new();
        let mut alternate = false;
        let mut log = String::new();
        for _ in 0..80 {
            let n = r.below(40);
            // A flags number's bits past the spec's five mean nothing, and
            // report event types (2) isn't kept: fux sends no repeats or
            // releases, and the spec's "Detection of support" has a
            // terminal report only the flags it honours.
            let flags = u8::try_from(n & 0b1_1101).unwrap_or(u8::MAX);
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
