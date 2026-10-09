//! A row's version changes on each edit that changes the row, and on no
//! other: after every `process`, a row whose cells or wrap flag differ has a
//! greater version and is among the dirty rows; and input whose every edit
//! leaves its row as it was changes no version at all.
use fux_vt::{Attributes, Blink, CellRef, Cells, Color, Parser, RowId, Size};
use std::collections::HashMap;
use std::fmt::Write;

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

/// A small deterministic generator, so a failure names its case.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next().checked_rem(n).unwrap_or(0)
    }
    /// A screen of up to 12 rows by 32 columns.
    fn size(&mut self) -> Result<Size> {
        Ok(Size::new(
            u16::try_from(self.below(12).saturating_add(1))?,
            u16::try_from(self.below(32).saturating_add(1))?,
        )?)
    }
    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        let n = u64::try_from(items.len()).unwrap_or(1);
        usize::try_from(self.below(n))
            .ok()
            .and_then(|i| items.get(i))
            .copied()
            .unwrap_or("")
    }
}

/// A piece of output: text, wide glyphs and combining marks, erases of every
/// kind, SGR, cursor moves, scroll regions, insert and delete, modes, the
/// alternate screen and resets.
fn piece(r: &mut Rng) -> String {
    let n = r.below(12);
    match r.below(20) {
        // A mark alone, on whatever glyph is before the cursor.
        18 => "\u{301}".to_owned(),
        19 => format!("\x1b[{};{}H{}", r.below(14), r.below(34), '\u{302}'),
        0..=3 => r
            .pick(&[
                "hello ", "abc", "界界 ", "x\u{301}", "\r\n", "\t", "\x08", "world",
            ])
            .to_owned(),
        4 => format!("\x1b[{};{}H", r.below(14), r.below(34)),
        5 => format!("\x1b[{n}{}", r.pick(&["A", "B", "C", "D", "G", "d"])),
        6 => format!("\x1b[{}{}", r.below(3), r.pick(&["K", "J"])),
        7 => format!("\x1b[{n}X"),
        8 => format!("\x1b[{n}{}", r.pick(&["@", "P", "L", "M", "S", "T"])),
        9 => format!("\x1b[{};{}r", r.below(10), r.below(14)),
        10 => format!(
            "\x1b[{};{}m",
            r.pick(&["0", "1", "2", "7", "32", "44"]),
            r.below(50)
        ),
        11 => r
            .pick(&[
                "\x1b[?1049h",
                "\x1b[?1049l",
                "\x1b[?7l",
                "\x1b[?7h",
                "\x1b[?6h",
                "\x1b[?6l",
            ])
            .to_owned(),
        12 => "\x1bc".to_owned(),
        13 => "\x1bM".to_owned(),
        _ => (0..r.below(4)).fold(String::new(), |mut out, i| {
            let _ = write!(out, "line {i} of output\r\n");
            out
        }),
    }
}

/// Every retained row by identity: its version, wrap flag and cells.
fn retained(parser: &Parser) -> HashMap<RowId, (u64, bool, Cells)> {
    let screen = parser.screen();
    let retained = screen
        .history_len()
        .saturating_add(usize::from(screen.size().rows()));
    (0..retained)
        .filter_map(|i| screen.row_from_bottom(i))
        .map(|row| {
            (
                row.id(),
                (row.version(), row.wrapped(), row.cells().collect()),
            )
        })
        .collect()
}

/// The SGR that sets `attributes`' underline style, kitty's `4:n`.
fn underline(attributes: Attributes) -> &'static str {
    match attributes.underline_style().number() {
        2 => "4:2",
        3 => "4:3",
        4 => "4:4",
        5 => "4:5",
        _ => "4",
    }
}

/// The SGR that sets exactly `attributes`.
fn sgr(attributes: Attributes) -> String {
    let mut out = String::from("\x1b[0");
    for (on, code) in [
        (attributes.bold(), "1"),
        (attributes.dim(), "2"),
        (attributes.italic(), "3"),
        (attributes.underline(), underline(attributes)),
        (attributes.inverse(), "7"),
        (attributes.blink() == Blink::Slow, "5"),
        (attributes.blink() == Blink::Rapid, "6"),
        (attributes.hidden(), "8"),
        (attributes.strikeout(), "9"),
    ] {
        if on {
            out.push(';');
            out.push_str(code);
        }
    }
    for (color, base) in [
        (attributes.foreground(), 38),
        (attributes.background(), 48),
        (attributes.underline_color(), 58),
    ] {
        let _ = match color {
            Color::Idx(i) => write!(out, ";{base};5;{i}"),
            Color::Rgb(r, g, b) => write!(out, ";{base};2;{r};{g};{b}"),
            Color::Default | _ => Ok(()),
        };
    }
    out.push('m');
    out
}

/// Input whose every edit leaves its row as it was: each live row's glyphs
/// written again where they are, with their own attributes, stepping over
/// blanks, wide glyphs' second halves and cells with combining marks (which
/// take two edits to write), and a tail already blank in one style erased
/// again, unless the row is soft-wrapped: erasing to its end would end the
/// wrap. A wide glyph that does not fit wraps from a row whose last column
/// is blank.
fn no_op(parser: &Parser) -> String {
    let screen = parser.screen();
    let (rows, cols) = screen.size().into();
    // Positions are absolute, whatever the program set.
    let mut out = String::from("\x1b[?6l");
    for y in 0..rows {
        let Some(row) =
            screen.row_from_bottom(usize::from(rows.saturating_sub(y).saturating_sub(1)))
        else {
            continue;
        };
        let _ = write!(out, "\x1b[{};1H", u32::from(y).saturating_add(1));
        let cells: Vec<CellRef<'_>> = row.cells().collect();
        // Where the blank tail starts, if it is one blank in one style.
        let tail = cells
            .iter()
            .rposition(|c| c.has_contents() || c.is_wide_continuation())
            .map_or(0, |i| i.saturating_add(1));
        let rest = cells.get(tail..).unwrap_or_default();
        let uniform = rest
            .first()
            .filter(|first| rest.iter().all(|c| c == *first))
            .copied();
        let mut x = 0usize;
        for cell in cells.get(..tail).unwrap_or_default() {
            let text = cell.contents();
            if cell.is_wide_continuation() {
                continue;
            }
            let width = if cell.is_wide() { 2 } else { 1 };
            x = x.saturating_add(width);
            if !cell.has_contents() || text.chars().nth(1).is_some() {
                // Stepped over, not written: to the column after it.
                let _ = write!(out, "\x1b[{}G", x.saturating_add(1));
            } else {
                out.push_str(&sgr(cell.attributes()));
                out.push_str(text);
            }
        }
        if let Some(blank) = uniform
            && tail < usize::from(cols)
            && !row.wrapped()
        {
            let _ = write!(out, "\x1b[{}G", tail.saturating_add(1));
            out.push_str(&sgr(blank.attributes()));
            out.push_str("\x1b[K");
        }
    }
    out
}

#[test]
fn a_version_changes_with_its_row_and_only_then() -> Result {
    let mut r = Rng(0x7e85_10a5_0000_0001);
    let (mut calls, mut changed, mut quiet) = (0u64, 0u64, 0u64);
    for case in 0..1_500u64 {
        let size = r.size()?;
        let history = usize::try_from(r.below(20))?;
        let mut parser = Parser::new(size, history)?;
        for step in 0..r.below(30) {
            // Some output of every kind, then the same screen written again.
            let bytes = if step % 3 == 2 {
                no_op(&parser)
            } else {
                let pieces = if r.below(3) == 0 { 1 } else { 1 + r.below(6) };
                (0..pieces).map(|_| piece(&mut r)).collect()
            };
            let before = retained(&parser);
            let mark = parser.screen().mark();
            parser.process(bytes.as_bytes())?;
            let after = retained(&parser);
            let screen = parser.screen();
            let dirty: Vec<RowId> = screen.dirty_rows_since(mark).map(|r| r.id()).collect();
            let full = screen.full_refresh_since(mark);
            for (id, (version, wrapped, cells)) in &after {
                let Some((was, was_wrapped, was_cells)) = before.get(id) else {
                    continue;
                };
                if wrapped != was_wrapped || cells != was_cells {
                    assert!(
                        version > was,
                        "case {case}: {bytes:?} changed {id:?} without a new version"
                    );
                    assert!(
                        full || dirty.contains(id),
                        "case {case}: {bytes:?} changed {id:?}, not dirty"
                    );
                    changed = changed.saturating_add(1);
                } else if step % 3 == 2 {
                    assert_eq!(
                        version, was,
                        "case {case}: {bytes:?} left {id:?} as it was, with a new version"
                    );
                }
            }
            if step % 3 == 2 {
                assert!(
                    dirty.is_empty() && !full,
                    "case {case}: {bytes:?} made rows dirty"
                );
                quiet = quiet.saturating_add(1);
            }
            calls = calls.saturating_add(1);
        }
    }
    assert!(
        calls > 10_000 && changed > 10_000 && quiet > 3_000,
        "{calls} {changed} {quiet}"
    );
    Ok(())
}

/// The dirty live rows are the live rows among the dirty rows, with their
/// places on the screen, top to bottom; all of them after a full refresh.
#[test]
fn dirty_live_rows_are_the_live_dirty_rows() -> Result {
    let mut r = Rng(0xd1e7_0000_0000_0003);
    let (mut compared, mut refreshed) = (0u64, 0u64);
    for _ in 0..1_000u64 {
        let size = r.size()?;
        let history = usize::try_from(r.below(40))?;
        let mut parser = Parser::new(size, history)?;
        for _ in 0..r.below(20) {
            let mark = parser.screen().mark();
            let bytes: String = (0..1 + r.below(4)).map(|_| piece(&mut r)).collect();
            parser.process(bytes.as_bytes())?;
            if r.below(8) == 0 {
                parser.resize(r.size()?)?;
            }
            let screen = parser.screen();
            let height = screen.size().rows();
            // The live row at `y` is `height - 1 - y` rows from the bottom.
            let live: HashMap<RowId, u16> = (0..height)
                .filter_map(|y| {
                    let from_bottom = usize::from(height.saturating_sub(y).saturating_sub(1));
                    screen.row_from_bottom(from_bottom).map(|row| (row.id(), y))
                })
                .collect();
            let expected: Vec<(u16, RowId)> = screen
                .dirty_rows_since(mark)
                .filter_map(|row| live.get(&row.id()).map(|y| (*y, row.id())))
                .collect();
            let got: Vec<(u16, RowId)> = screen
                .dirty_live_rows_since(mark)
                .map(|(y, row)| (y, row.id()))
                .collect();
            assert_eq!(got, expected, "{bytes:?}");
            // Top to bottom.
            assert!(got.iter().zip(got.iter().skip(1)).all(|(a, b)| a.0 < b.0));
            if screen.full_refresh_since(mark) {
                assert_eq!(got.len(), usize::from(height), "{bytes:?}");
                refreshed = refreshed.saturating_add(1);
            }
            compared = compared.saturating_add(1);
        }
    }
    assert!(
        compared > 5_000 && refreshed > 500,
        "{compared} {refreshed}"
    );
    Ok(())
}

#[test]
fn the_common_redraw_changes_no_version() -> Result {
    let mut parser = Parser::new(Size::new(5, 20)?, 10)?;
    let redraw = "\x1b[H\x1b[1;1Hone\x1b[K\x1b[2;1H\x1b[1m界 two\x1b[0m\x1b[K\x1b[3;1Hthree\x1b[K";
    parser.process(redraw.as_bytes())?;
    let versions = |p: &Parser| -> Vec<u64> {
        (0..5)
            .filter_map(|i| p.screen().row_from_bottom(i).map(|r| r.version()))
            .collect()
    };
    let before = versions(&parser);
    let mark = parser.screen().mark();
    parser.process(redraw.as_bytes())?;
    assert_eq!(versions(&parser), before);
    assert_eq!(parser.screen().dirty_rows_since(mark).count(), 0);
    // Output was still processed.
    assert!(parser.screen().changed_since(mark));
    // A real edit is still a change.
    parser.process(b"\x1b[3;1HthreE")?;
    assert_eq!(parser.screen().dirty_rows_since(mark).count(), 1);
    Ok(())
}
