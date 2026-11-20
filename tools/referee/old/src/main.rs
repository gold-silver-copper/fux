//! The old emulator as a witness: reads a case's steps on standard input
//! (`size COLS ROWS`, then `feed HEX` and `resize COLS ROWS` lines), and
//! prints `cursor ROW COL` and the screen's rows, each after a `|`.
use fux_vt::{Feature, Options, Parser, Size};
use std::io::Read;

fn main() {
    let mut steps = String::new();
    std::io::stdin().read_to_string(&mut steps).expect("the steps");
    let mut parser: Option<Parser> = None;
    for line in steps.lines() {
        let (kind, rest) = line.split_once(' ').unwrap_or((line, ""));
        let numbers: Vec<u16> = rest.split(' ').filter_map(|n| n.parse().ok()).collect();
        match (kind, numbers.as_slice(), &mut parser) {
            ("size", [cols, rows], _) => {
                let size = Size::new(*rows, *cols).expect("a size");
                parser = Some(Parser::with_options(size, 1 << 20, Options::new().with(Feature::Reflow)).expect("a parser"));
            }
            ("resize", [cols, rows], Some(parser)) => parser.resize(Size::new(*rows, *cols).expect("a size")).expect("a resize"),
            ("feed", _, Some(parser)) => {
                let bytes: Vec<u8> = (0..rest.len() / 2).filter_map(|i| u8::from_str_radix(rest.get(2 * i..2 * i + 2)?, 16).ok()).collect();
                parser.process_all(&bytes, &mut ());
            }
            _ => {}
        }
    }
    let parser = parser.expect("a size line first");
    let screen = parser.screen();
    let (row, col) = screen.cursor_position();
    println!("cursor {row} {col}");
    let rows: Vec<String> = screen.rows().map(|row| row.text()).collect();
    let shown = rows.len().saturating_sub(usize::from(screen.size().rows()));
    rows.iter().skip(shown).for_each(|row| println!("|{row}"));
}
