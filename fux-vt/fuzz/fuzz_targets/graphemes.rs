//! Printed text against the model of how it is segmented into cells.
//!
//! The input is structured (`arbitrary`): lines of characters, each picked
//! from `tests/corpus/graphemes.rs` or any scalar value; where SGR goes
//! between them; the sizes of the pieces the output arrives in; and a
//! width to reflow to and back. Every printed row must hold exactly the
//! cells the model makes of its line -- one a cluster as UAX #29 has it,
//! as wide as unicode-width lays it out -- with the cursor after the last,
//! however the output is cut up; and again after reflowing narrower and
//! back.
#![no_main]
use fux_vt::{Options, Parser, Row};
use libfuzzer_sys::arbitrary::{self, Arbitrary};
use libfuzzer_sys::fuzz_target;
use unicode_width::UnicodeWidthChar;
#[path = "../../tests/corpus/graphemes.rs"]
mod graphemes;
#[path = "../../tests/corpus/models.rs"]
mod models;

#[derive(Arbitrary, Debug)]
enum Pick {
    Table(u8),
    Scalar(u32),
}

#[derive(Arbitrary, Debug)]
struct Input {
    lines: Vec<Vec<(Pick, bool)>>,
    pieces: Vec<u8>,
    reflow_to: Option<u8>,
}

impl Pick {
    fn char(&self) -> Option<char> {
        match self {
            Pick::Table(i) => {
                let table = graphemes::CHARACTERS;
                table.get(usize::from(*i) % table.len()).copied()
            }
            Pick::Scalar(p) => char::from_u32(*p % 0x11_0000),
        }
    }
}

/// Asserts row `row` holds `expected`, and nothing after it.
fn row_is(row: Row<'_>, expected: &[(String, bool)], line: &str) {
    let mut col = 0usize;
    for (text, wide) in expected {
        let cell = row.cell(col);
        assert_eq!(
            cell.map(|c| (c.contents(), c.is_wide())),
            Some((text.as_str(), *wide)),
            "{line:?} at column {col}"
        );
        if *wide {
            assert!(row.cell(col + 1).is_some_and(|c| c.is_wide_continuation()));
        }
        col += if *wide { 2 } else { 1 };
    }
    assert!(row.cells().skip(col).all(|c| !c.has_contents()), "{line:?}");
}

fuzz_target!(|input: Input| {
    // Lines of printable characters, the first taking columns: what the
    // model describes. At most 8 lines of 48 characters.
    let lines: Vec<String> = input
        .lines
        .iter()
        .take(8)
        .map(|picks| {
            let mut line = String::new();
            for (pick, _) in picks.iter().take(48) {
                if let Some(c) = pick.char().filter(|c| models::printable(*c))
                    && (!line.is_empty() || c.width().is_some_and(|w| w > 0))
                {
                    line.push(c);
                }
            }
            line
        })
        .collect();
    let expected: Vec<Vec<(String, bool)>> = lines.iter().map(|l| models::cells_of(l)).collect();
    let widest = expected
        .iter()
        .zip(&lines)
        .map(|(cells, line)| {
            let columns: usize = cells
                .iter()
                .map(|(_, wide)| if *wide { 2 } else { 1 })
                .sum();
            columns.max(line.len())
        })
        .max()
        .unwrap_or(0);
    let cols = u16::try_from(widest + 4).unwrap_or(u16::MAX);
    let rows = u16::try_from(lines.len() + 1).unwrap_or(1);

    // The output: lines apart by CR LF, SGR where the input says.
    let mut output = Vec::new();
    let mut bold = input.lines.iter().flatten().map(|(_, b)| *b);
    for (n, line) in lines.iter().enumerate() {
        if n > 0 {
            output.extend_from_slice(b"\r\n");
        }
        for c in line.chars() {
            if bold.next().unwrap_or(false) {
                output.extend_from_slice(b"\x1b[1m");
            }
            let mut buffer = [0; 4];
            output.extend_from_slice(c.encode_utf8(&mut buffer).as_bytes());
        }
    }

    let options = Options::new().with_reflow(true);
    let Ok(mut parser) = Parser::with_options(rows, cols, 1_000, options) else {
        return;
    };
    let mut rest = output.as_slice();
    let mut sizes = input.pieces.iter().cycle();
    while !rest.is_empty() {
        let size = sizes
            .next()
            .map_or(rest.len(), |s| usize::from(*s % 16) + 1);
        let (piece, tail) = rest.split_at_checked(size).unwrap_or((rest, &[]));
        assert!(parser.process(piece).is_ok());
        rest = tail;
    }
    let check = |parser: &Parser| {
        let screen = parser.screen();
        for (y, (cells, line)) in expected.iter().zip(&lines).enumerate() {
            let row = screen.row_from_bottom(lines.len() - y);
            row_is(row.unwrap_or_else(|| panic!("row {y}")), cells, line);
        }
    };
    check(&parser);
    let last = expected.last().map_or(0, |cells| {
        cells
            .iter()
            .map(|(_, wide)| if *wide { 2 } else { 1 })
            .sum()
    });
    if !lines.is_empty() {
        // A line that fills the row leaves the cursor in the last column,
        // waiting to wrap: one past it.
        let screen = parser.screen();
        let at = usize::from(screen.cursor_position().1)
            .saturating_add(usize::from(screen.pending_wrap()));
        assert_eq!(at, last);
    }

    // Narrower and back: every line as it was, the cursor on it.
    if let Some(width) = input.reflow_to {
        let cursor = (
            parser.screen().cursor_position(),
            parser.screen().pending_wrap(),
        );
        let narrow = u16::from(width).clamp(2, cols);
        assert!(parser.resize(rows, narrow).is_ok());
        assert!(parser.resize(rows, cols).is_ok());
        check(&parser);
        let after = (
            parser.screen().cursor_position(),
            parser.screen().pending_wrap(),
        );
        assert_eq!(after, cursor);
    }
});
