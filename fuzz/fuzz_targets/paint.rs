#![no_main]
//! Input: two bytes for the size (1–40 rows by 1–120 columns; ff ff is one
//! row of u16::MAX columns), a flags byte, the cursors, then four-byte cell
//! writes. Bit 2 of the flags gives the new grid its own size, in the next two
//! bytes, so that the paint is a full one.
use fux::render::{Grid, paint};
use fux_vt::{Attributes, Cell, Color, Parser};
use libfuzzer_sys::fuzz_target;

/// A glyph: its text, whether it is wide. `None` is a cell never written.
const GLYPHS: [Option<(&str, bool)>; 8] = [
    Some(("a", false)),
    Some(("Z", false)),
    Some((" ", false)),
    Some(("~", false)),
    Some(("é", false)),
    Some(("e\u{301}", false)),
    Some(("界", true)),
    None,
];

/// What fux paints: fux-vt keeps bold and dim apart (SGR 1 and 2 replace one
/// another), and fux never sets both, so no set here does.
fn attributes(index: u8) -> Attributes {
    let plain = Attributes::default();
    match index % 10 {
        0 => plain,
        1 => plain.with_bold(true),
        2 => plain.with_dim(true),
        3 => plain.with_italic(true).with_underline(true),
        4 => plain.with_inverse(true),
        5 => Attributes::new(Color::Idx(1), Color::Default),
        6 => Attributes::new(Color::Idx(9), Color::Idx(4)).with_bold(true),
        7 => Attributes::new(Color::Idx(200), Color::Idx(17)),
        8 => Attributes::new(Color::Rgb(1, 2, 3), Color::Default),
        _ => Attributes::new(Color::Default, Color::Rgb(250, 128, 0))
            .with_dim(true)
            .with_inverse(true),
    }
}

struct Bytes<'a>(&'a [u8]);
impl Bytes<'_> {
    fn next(&mut self) -> u8 {
        let Some((&b, rest)) = self.0.split_first() else {
            return 0;
        };
        self.0 = rest;
        b
    }
    fn size(&mut self) -> (u16, u16) {
        let (r, c) = (self.next(), self.next());
        if (r, c) == (0xff, 0xff) {
            (1, u16::MAX)
        } else {
            (1 + u16::from(r % 40), 1 + u16::from(c % 120))
        }
    }
    fn at(&mut self, rows: u16, cols: u16) -> (u16, u16) {
        let y = u16::from(self.next()) % rows;
        let x = u16::from_be_bytes([self.next(), self.next()]) % cols;
        (y, x)
    }
}

fn index(grid: &Grid, y: u16, x: u16) -> usize {
    usize::from(y) * usize::from(grid.cols) + usize::from(x)
}

/// A glyph written at (y, x), a wide one with its second half.
fn write(grid: &mut Grid, y: u16, x: u16, kind: u8) {
    let glyph = GLYPHS[usize::from(kind) % GLYPHS.len()];
    let attrs = attributes(kind / 8);
    let at = index(grid, y, x);
    match glyph {
        None => grid.cells[at] = Cell::default(),
        Some((text, wide)) => {
            grid.cells[at] = Cell::new(text, wide, attrs).unwrap_or_default();
            if wide && x + 1 < grid.cols {
                grid.cells[at + 1] = Cell::wide_continuation();
            }
        }
    }
}

/// Wide glyphs whole, as a terminal keeps them: a second half without its
/// first, or a first half without its second except in the last column,
/// becomes a blank.
fn repair(grid: &mut Grid) {
    for y in 0..grid.rows {
        for x in 0..grid.cols {
            let at = index(grid, y, x);
            let cell = grid.cells[at];
            let last = x + 1 == grid.cols;
            let broken = (cell.is_wide() && !last && !grid.cells[at + 1].is_wide_continuation())
                || (cell.is_wide_continuation() && (x == 0 || !grid.cells[at - 1].is_wide()));
            if broken {
                grid.cells[at] = Cell::default();
            }
        }
    }
}

fn grid(rows: u16, cols: u16) -> Grid {
    Grid::new(rows, cols)
}

fn text(cell: &Cell) -> &str {
    if cell.has_contents() {
        cell.contents()
    } else {
        " "
    }
}

fuzz_target!(|data: &[u8]| {
    let mut bytes = Bytes(data);
    let (rows, cols) = bytes.size();
    let flags = bytes.next();
    let (new_rows, new_cols) = if flags & 4 != 0 {
        bytes.size()
    } else {
        (rows, cols)
    };
    let mut old = grid(rows, cols);
    old.cursor = (flags & 1 != 0).then(|| bytes.at(rows, cols));
    let new_cursor = (flags & 2 != 0).then(|| bytes.at(new_rows, new_cols));
    // The new grid starts as the old one, so that the paint is a diff, unless
    // it has a size of its own.
    let mut writes = Vec::new();
    while !bytes.0.is_empty() {
        let op = bytes.next();
        let (which, kind) = (op & 0x80 != 0, op & 0x7f);
        writes.push((which, kind, bytes.next(), bytes.next(), bytes.next()));
    }
    for &(which, kind, y, xh, xl) in &writes {
        if !which {
            let (y, x) = (u16::from(y) % rows, u16::from_be_bytes([xh, xl]) % cols);
            write(&mut old, y, x, kind);
        }
    }
    repair(&mut old);
    let mut new = if (new_rows, new_cols) == (rows, cols) {
        old.clone()
    } else {
        grid(new_rows, new_cols)
    };
    new.cursor = new_cursor;
    for &(which, kind, y, xh, xl) in &writes {
        if which {
            let (y, x) = (
                u16::from(y) % new_rows,
                u16::from_be_bytes([xh, xl]) % new_cols,
            );
            write(&mut new, y, x, kind);
        }
    }
    repair(&mut new);

    // The client's terminal: `old` as painted from nothing, then the paint
    // that turns it into `new`.
    let Ok(mut terminal) = Parser::new(new_rows, new_cols, 0) else {
        return;
    };
    assert!(terminal.process(&paint(None, &old)).is_ok());
    assert!(terminal.process(&paint(Some(&old), &new)).is_ok());
    let screen = terminal.screen();
    for y in 0..new_rows {
        for x in 0..new_cols {
            let mut want = new.cells[index(&new, y, x)];
            // A wide glyph cannot fit the last column: it is painted blank.
            if want.is_wide() && x + 1 == new_cols {
                want = Cell::new(" ", false, want.attributes()).unwrap_or_default();
            }
            let got = screen.cell(y, x).copied().unwrap_or_default();
            assert_eq!(
                (got.is_wide(), got.is_wide_continuation()),
                (want.is_wide(), want.is_wide_continuation()),
                "width at {y},{x}"
            );
            if !want.is_wide_continuation() {
                assert_eq!(text(&got), text(&want), "text at {y},{x}");
                assert_eq!(got.attributes(), want.attributes(), "attributes at {y},{x}");
            }
        }
    }
    match new.cursor {
        Some(at) => {
            assert!(!screen.hide_cursor());
            assert_eq!(screen.cursor_position(), at);
        }
        None => assert!(screen.hide_cursor()),
    }
});
