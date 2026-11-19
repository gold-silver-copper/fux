//! Composing the screen: cells in rectangles first, escape sequences only
//! from the finished result, so nothing a pane printed can reach the outer
//! terminal as a sequence.
use crate::layout::Axis;
use fux2::wire::{Cell as PaneCell, Meta, Row};

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

    fn put(&mut self, x: u16, y: u16, text: &str, sgr: &str, tail: bool) {
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

    /// Draws one pane's row, clipped to `w` columns from `x`. A wide glyph
    /// the edge would cut is left blank.
    fn row(&mut self, x: u16, y: u16, w: u16, row: &Row) {
        let scale: u16 = if row.rendition == 0 { 1 } else { 2 };
        let mut cells = row
            .spans
            .iter()
            .flat_map(|span| span.cells.iter().map(|cell| (clean_sgr(&span.sgr), cell)))
            .peekable();
        let mut column = 0u16;
        while let Some((sgr, cell)) = cells.next() {
            let PaneCell::Text(text) = cell else {
                continue;
            };
            let wide = matches!(cells.peek(), Some((_, PaneCell::Tail)));
            let width = scale.saturating_mul(if wide { 2 } else { 1 });
            if column.saturating_add(width) > w {
                break;
            }
            let at = x.saturating_add(column);
            self.put(at, y, clean_glyph(text), sgr, false);
            for extra in 1..width {
                let tail = wide && scale == 1;
                let text = if tail { "" } else { " " };
                self.put(at.saturating_add(extra), y, text, sgr, tail);
            }
            column = column.saturating_add(width);
        }
    }

    /// Draws a pane into `tile`, below its title row: top-left, a taller
    /// screen scrolled to keep the cursor in view. Where the cursor lands,
    /// if it is shown there.
    pub fn pane(&mut self, tile: Tile, rows: &[Row], meta: &Meta) -> Option<(u16, u16)> {
        let height = tile.h.saturating_sub(1);
        let top = tile.y.saturating_add(1);
        let first = meta
            .cursor_row
            .saturating_add(1)
            .saturating_sub(height)
            .min(meta.rows.saturating_sub(height));
        for (offset, row) in rows
            .iter()
            .skip(first.into())
            .take(height.into())
            .enumerate()
        {
            let offset = u16::try_from(offset).unwrap_or(u16::MAX);
            self.row(tile.x, top.saturating_add(offset), tile.w, row);
        }
        let row = meta.cursor_row.checked_sub(first).filter(|r| *r < height)?;
        let doubled = rows
            .get(usize::from(meta.cursor_row))
            .is_some_and(|row| row.rendition != 0);
        let scale = if doubled { 2 } else { 1 };
        let column = meta.cursor_col.saturating_mul(scale);
        (meta.cursor_visible && column < tile.w)
            .then(|| (tile.x.saturating_add(column), top.saturating_add(row)))
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

/// `count` tiles in `area`, a column or row of the area between them left
/// for a border. Too small a piece is left out: fewer tiles come back.
pub fn split(area: Tile, count: usize, axis: Axis) -> Vec<Tile> {
    if count == 0 || area.w == 0 || area.h < 2 {
        return Vec::new();
    }
    if count == 1 {
        return vec![area];
    }
    let across = match axis {
        Axis::Rows => false,
        Axis::Columns => true,
        // A cell is about twice as tall as wide.
        Axis::Auto => area.w >= area.h.saturating_mul(2),
    };
    let first_count = count.div_ceil(2);
    let share = |side: u16| {
        let side = usize::from(side);
        let part = side
            .saturating_mul(first_count)
            .checked_div(count)
            .unwrap_or(0);
        u16::try_from(part).unwrap_or(0)
    };
    let (one, two) = if across {
        let w = share(area.w.saturating_sub(1));
        (
            Tile { w, ..area },
            Tile {
                x: area.x.saturating_add(w).saturating_add(1),
                w: area.w.saturating_sub(w).saturating_sub(1),
                ..area
            },
        )
    } else {
        let h = share(area.h);
        (
            Tile { h, ..area },
            Tile {
                y: area.y.saturating_add(h),
                h: area.h.saturating_sub(h),
                ..area
            },
        )
    };
    let mut tiles = split(one, first_count, axis);
    tiles.extend(split(two, count.saturating_sub(first_count), axis));
    tiles
}

#[cfg(test)]
mod tests {
    use super::*;
    use fux2::wire::Span;

    fn wide_row() -> Row {
        Row {
            rendition: 0,
            spans: vec![Span {
                sgr: ";31\x1b[2J".to_owned(),
                cells: vec![
                    PaneCell::Text("a".to_owned()),
                    PaneCell::Text("\u{4e16}".to_owned()),
                    PaneCell::Tail,
                    PaneCell::Text("\x1b".to_owned()),
                ],
            }],
        }
    }

    #[test]
    fn nothing_a_pane_says_becomes_a_sequence() {
        let mut frame = Frame::new(10, 1);
        frame.row(0, 0, 10, &wide_row());
        let out = String::from_utf8_lossy(&frame.diff(&Frame::new(10, 1))).into_owned();
        assert!(!out.contains("2J"), "{out:?}");
        assert_eq!(out.matches('\x1b').count(), 2, "{out:?}");
    }

    #[test]
    fn a_wide_glyph_cut_by_the_edge_is_blank() {
        let mut frame = Frame::new(4, 1);
        frame.row(0, 0, 2, &wide_row());
        let out = String::from_utf8_lossy(&frame.diff(&Frame::new(4, 1))).into_owned();
        assert!(out.contains('a') && !out.contains('\u{4e16}'), "{out:?}");
    }

    #[test]
    fn tiles_do_not_overlap_and_stay_inside() {
        let area = Tile {
            x: 0,
            y: 0,
            w: 80,
            h: 23,
        };
        for count in 1..=9 {
            let tiles = split(area, count, Axis::Auto);
            assert_eq!(tiles.len(), count);
            for (index, tile) in tiles.iter().enumerate() {
                assert!(tile.x.saturating_add(tile.w) <= 80 && tile.y.saturating_add(tile.h) <= 23);
                for other in tiles.iter().skip(index.saturating_add(1)) {
                    let apart = tile.x.saturating_add(tile.w) <= other.x
                        || other.x.saturating_add(other.w) <= tile.x
                        || tile.y.saturating_add(tile.h) <= other.y
                        || other.y.saturating_add(other.h) <= tile.y;
                    assert!(apart, "{tile:?} and {other:?}");
                }
            }
        }
    }
}
