//! Composing the screen: cells in rectangles first, escape sequences only
//! from the finished result, so nothing a pane printed can reach the outer
//! terminal as a sequence.
use crate::layout::{Axis, Border};
use crate::panes::Screen;

/// The largest screen composed; a bigger terminal is left blank past it.
pub const MAX_COLS: u16 = 512;
pub const MAX_ROWS: u16 = 256;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Cell {
    text: String,
    sgr: String,
    /// The second column of the wide glyph to its left.
    tail: bool,
}

impl Cell {
    /// A cell: its glyph, its pen as SGR parameters after a reset
    /// (`;1;31`), and whether it is a wide glyph's second column. Anything
    /// that is no glyph or no SGR parameter is dropped here, so nothing a
    /// pane holds can reach the terminal as a sequence.
    pub fn new(text: &str, sgr: &str, tail: bool) -> Cell {
        Cell {
            text: clean_glyph(text).to_owned(),
            sgr: clean_sgr(sgr).to_owned(),
            tail,
        }
    }
}

/// A rectangle of the screen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tile {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

/// A screen's worth of cells.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Frame {
    cols: u16,
    rows: u16,
    cells: Vec<Cell>,
}

fn clean_sgr(sgr: &str) -> &str {
    let good = sgr
        .bytes()
        .all(|b| b.is_ascii_digit() || b == b';' || b == b':');
    if good { sgr } else { "" }
}

fn clean_glyph(text: &str) -> &str {
    if text.is_empty() || text.chars().any(char::is_control) {
        " "
    } else {
        text
    }
}

impl Frame {
    pub fn new(cols: u16, rows: u16) -> Frame {
        let (cols, rows) = (cols.min(MAX_COLS), rows.min(MAX_ROWS));
        let count = usize::from(cols).saturating_mul(rows.into());
        Frame {
            cols,
            rows,
            cells: vec![Cell::default(); count],
        }
    }

    fn at(&mut self, x: u16, y: u16) -> Option<&mut Cell> {
        if x >= self.cols || y >= self.rows {
            return None;
        }
        let index = usize::from(y)
            .saturating_mul(self.cols.into())
            .saturating_add(x.into());
        self.cells.get_mut(index)
    }

    /// Writes one cell; a wide glyph it splits loses its other half too
    /// (blanked), as the terminal would erase it.
    fn put(&mut self, x: u16, y: u16, text: &str, sgr: &str, tail: bool) {
        let blank = |cell: &mut Cell| {
            cell.text.clear();
            cell.text.push(' ');
            cell.tail = false;
        };
        let was_tail = self.at(x, y).is_some_and(|c| c.tail);
        if was_tail
            && !tail
            && let Some(left) = x.checked_sub(1).and_then(|l| self.at(l, y))
        {
            blank(left);
        }
        if let Some(right) = x.checked_add(1).and_then(|r| self.at(r, y))
            && right.tail
        {
            blank(right);
        }
        if let Some(cell) = self.at(x, y) {
            cell.text.clear();
            cell.text.push_str(text);
            cell.sgr.clear();
            cell.sgr.push_str(sgr);
            cell.tail = tail;
        }
    }

    /// Writes `text` from `x`, at most `w` columns of it.
    pub fn label(&mut self, x: u16, y: u16, w: u16, text: &str, sgr: &str) {
        let mut column = 0u16;
        for (glyph, width) in fux_vt::clusters(text) {
            if width == 0 || glyph.chars().any(char::is_control) {
                continue;
            }
            if column.saturating_add(width) > w {
                break;
            }
            self.put(x.saturating_add(column), y, glyph, sgr, false);
            if width == 2 {
                self.put(x.saturating_add(column).saturating_add(1), y, "", sgr, true);
            }
            column = column.saturating_add(width);
        }
    }

    /// Fills a row from `x`, `w` columns, with `text`.
    pub fn fill(&mut self, x: u16, y: u16, w: u16, text: &str, sgr: &str) {
        for column in 0..w {
            self.put(x.saturating_add(column), y, text, sgr, false);
        }
    }

    /// Draws a pane's screen into `tile`: top-left, clipped, a taller
    /// screen scrolled to keep the cursor in view. A wide glyph the edge
    /// would cut is left blank. Where the cursor lands, if it is shown there.
    pub fn pane(&mut self, tile: Tile, screen: &Screen) -> Option<(u16, u16)> {
        let head = &screen.head;
        let first = first_row(screen, tile);
        for (offset, row) in screen
            .rows
            .iter()
            .skip(first.into())
            .take(tile.h.into())
            .enumerate()
        {
            let y = tile
                .y
                .saturating_add(u16::try_from(offset).unwrap_or(u16::MAX));
            for (column, cell) in row.iter().enumerate() {
                let Ok(column) = u16::try_from(column) else {
                    break;
                };
                let wide = row
                    .get(usize::from(column).saturating_add(1))
                    .is_some_and(|next| next.tail);
                let width = if wide { 2 } else { 1 };
                if column.saturating_add(width) > tile.w {
                    break;
                }
                if let Some(at) = self.at(tile.x.saturating_add(column), y) {
                    at.clone_from(cell);
                }
            }
        }
        let row = head.cursor_row.checked_sub(first).filter(|r| *r < tile.h)?;
        (head.cursor_shown && head.cursor_col < tile.w).then(|| {
            (
                tile.x.saturating_add(head.cursor_col),
                tile.y.saturating_add(row),
            )
        })
    }

    /// The bytes that turn `old` into this frame on a terminal.
    pub fn diff(&self, old: &Frame) -> Vec<u8> {
        let same_shape = self.cols == old.cols && self.rows == old.rows;
        let mut out = Vec::new();
        if !same_shape {
            out.extend_from_slice(b"\x1b[0m\x1b[2J");
        }
        let blank = Cell::default();
        let mut pen: Option<&str> = None;
        let mut at: Option<(u16, u16)> = None;
        for (index, cell) in self.cells.iter().enumerate() {
            let before = if same_shape {
                old.cells.get(index)
            } else {
                None
            };
            let unchanged = before.unwrap_or(&blank) == cell;
            if unchanged || cell.tail {
                continue;
            }
            let columns = usize::from(self.cols.max(1));
            let y = u16::try_from(index.checked_div(columns).unwrap_or(0)).unwrap_or(0);
            let x = u16::try_from(index.checked_rem(columns).unwrap_or(0)).unwrap_or(0);
            if at != Some((x, y)) {
                let (row, col) = (y.saturating_add(1), x.saturating_add(1));
                out.extend_from_slice(format!("\x1b[{row};{col}H").as_bytes());
            }
            if pen != Some(cell.sgr.as_str()) {
                out.extend_from_slice(format!("\x1b[0{}m", cell.sgr).as_bytes());
                pen = Some(&cell.sgr);
            }
            let text = if cell.text.is_empty() {
                " "
            } else {
                &cell.text
            };
            out.extend_from_slice(text.as_bytes());
            let wide = fux_vt::str_width(text) >= 2;
            at = Some((x.saturating_add(if wide { 2 } else { 1 }), y));
        }
        out
    }
}

/// The first of a pane's rows shown in `tile`: a taller screen is scrolled
/// to keep the cursor in view.
pub fn first_row(screen: &Screen, tile: Tile) -> u16 {
    let head = &screen.head;
    (head.cursor_row.saturating_add(1).saturating_sub(tile.h)).min(head.rows.saturating_sub(tile.h))
}

/// The borders' colour, and that of the borders around the focused pane.
pub const BORDER: &str = ";38;5;240";
pub const FOCUSED_BORDER: &str = ";32";

impl Frame {
    /// Draws `borders` as lines, each cell joined to the border cells
    /// beside it; those touching `focus` (its edges and corners) in green.
    pub fn borders(&mut self, borders: &[Border], focus: Option<Tile>) {
        let small = |n: u32| u16::try_from(n).unwrap_or(u16::MAX);
        let along = |b: &Border| b.from..b.from.saturating_add(b.length);
        let edges: Vec<(u16, u16, bool)> = (borders.iter())
            .flat_map(|b| {
                along(b).map(move |at| match b.axis {
                    Axis::Columns => (small(b.at), small(at), true),
                    Axis::Rows => (small(at), small(b.at), false),
                })
            })
            .collect();
        let set: std::collections::HashSet<(u16, u16)> =
            edges.iter().map(|(x, y, _)| (*x, *y)).collect();
        let has = |x: Option<u16>, y: Option<u16>| match (x, y) {
            (Some(x), Some(y)) => set.contains(&(x, y)),
            _ => false,
        };
        let ring = |x: u16, y: u16| {
            focus.is_some_and(|tile| {
                let (x, y) = (u32::from(x), u32::from(y));
                let (left, top) = (u32::from(tile.x), u32::from(tile.y));
                let right = left.saturating_add(u32::from(tile.w));
                let bottom = top.saturating_add(u32::from(tile.h));
                x.saturating_add(1) >= left
                    && x <= right
                    && y.saturating_add(1) >= top
                    && y <= bottom
            })
        };
        for &(x, y, upright) in &edges {
            let up = has(Some(x), y.checked_sub(1));
            let down = has(Some(x), y.checked_add(1));
            let left = has(x.checked_sub(1), Some(y));
            let right = has(x.checked_add(1), Some(y));
            let glyph = match (up, down, left, right) {
                (true, true, true, true) => "\u{253c}",
                (true, true, false, true) => "\u{251c}",
                (true, true, true, false) => "\u{2524}",
                (false, true, true, true) => "\u{252c}",
                (true, false, true, true) => "\u{2534}",
                (true, false, false, true) => "\u{2514}",
                (true, false, true, false) => "\u{2518}",
                (false, true, false, true) => "\u{250c}",
                (false, true, true, false) => "\u{2510}",
                _ if upright => "\u{2502}",
                _ => "\u{2500}",
            };
            let sgr = if ring(x, y) { FOCUSED_BORDER } else { BORDER };
            self.put(x, y, glyph, sgr, false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A write over either half of a wide glyph blanks its other half, so
    /// the frame never holds half a glyph the terminal would not show.
    #[test]
    fn a_write_over_half_a_wide_glyph_blanks_the_other_half() {
        for at in [3u16, 4] {
            let mut frame = Frame::new(10, 1);
            frame.label(3, 0, 2, "\u{4e2d}", "");
            frame.put(at, 0, "x", ";31", false);
            let texts: Vec<&str> = (frame.cells.iter())
                .map(|c| c.text.as_str())
                .skip(3)
                .take(2)
                .collect();
            let other = if at == 3 { texts.get(1) } else { texts.first() };
            assert_eq!(other, Some(&" "), "write at {at}: {texts:?}");
            assert!(frame.cells.iter().all(|c| !c.tail), "a tail left at {at}");
        }
    }
}
