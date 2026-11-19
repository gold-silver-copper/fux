//! Composing the screen: cells in rectangles first, escape sequences only
//! from the finished result, so nothing a pane printed can reach the outer
//! terminal as a sequence.
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
        // Writing over half of a wide glyph loses the other half on a
        // terminal: it is blanked here too, so this frame is what the
        // terminal shows and a later frame redraws the glyph whole.
        if !tail {
            let was_tail = self.at(x, y).is_some_and(|cell| cell.tail);
            if was_tail && let Some(head) = x.checked_sub(1).and_then(|h| self.at(h, y)) {
                head.text.clear();
                head.text.push(' ');
            }
            if let Some(next) = x.checked_add(1).and_then(|n| self.at(n, y))
                && next.tail
            {
                next.tail = false;
                next.text.clear();
                next.text.push(' ');
            }
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
                // A wide glyph is drawn once, two columns wide; on a double
                // line the two columns after it are room.
                let tail = wide && extra == 1;
                let text = if tail { "" } else { " " };
                self.put(at.saturating_add(extra), y, text, sgr, tail);
            }
            column = column.saturating_add(width);
        }
    }

    /// Draws a pane into `tile`: top-left, a taller screen scrolled to keep
    /// the cursor in view. Where the cursor lands, if it is shown there.
    pub fn pane(&mut self, tile: Tile, rows: &[Row], meta: &Meta) -> Option<(u16, u16)> {
        let height = tile.h;
        let top = tile.y;
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
            // Where the terminal's cursor is now: unknown after a glyph
            // that took no cell, so the next cell moves there itself.
            at = match fux_vt::str_width(text) {
                0 => None,
                1 => Some((x.saturating_add(1), y)),
                _ => Some((x.saturating_add(2), y)),
            };
        }
        out
    }
}

/// The borders' colour, and that of the borders around the focused pane.
pub const BORDER: &str = ";38;5;240";
pub const FOCUSED_BORDER: &str = ";32";

/// One cell of a border: where it is, and whether its line runs up and
/// down (between panes side by side) or across.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Edge {
    pub x: u16,
    pub y: u16,
    pub upright: bool,
}

impl Frame {
    /// Draws border cells as lines, each joined to the border cells beside
    /// it; those touching `focus` (its edges and corners) in green.
    pub fn borders(&mut self, edges: &[Edge], focus: Option<Tile>) {
        let set: std::collections::HashSet<(u16, u16)> =
            edges.iter().map(|edge| (edge.x, edge.y)).collect();
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
        for edge in edges {
            let (x, y) = (edge.x, edge.y);
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
                _ if edge.upright => "\u{2502}",
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

    /// D6: on a double-width line a wide glyph is still drawn.
    #[test]
    fn a_wide_glyph_shows_on_a_double_line() {
        let row = Row {
            rendition: 1,
            ..wide_row()
        };
        let mut frame = Frame::new(12, 1);
        frame.row(0, 0, 12, &row);
        let out = String::from_utf8_lossy(&frame.diff(&Frame::new(12, 1))).into_owned();
        let after = out.split('\u{4e16}').nth(1).unwrap_or("missing");
        assert!(!after.starts_with('\x1b'), "drawn over: {out:?}");
        assert!(out.contains('\u{4e16}'), "{out:?}");
    }

    /// What a terminal shows of row 0 after `bytes`, from a blank screen.
    fn shown(screens: &[&[u8]], cols: u16) -> Result<String, Box<dyn std::error::Error>> {
        let size = fux_vt::Size::new(1, cols)?;
        let mut term = fux_vt::Parser::with_options(size, 0, fux_vt::Options::new())?;
        for bytes in screens {
            term.process_all(bytes, &mut ());
        }
        let window = term.screen().window();
        Ok(window
            .iter()
            .map(|(_, row)| row.text())
            .collect::<Vec<_>>()
            .join(""))
    }

    /// A panel covering half of a wide glyph, then gone: the terminal shows
    /// each frame as composed, with no half glyph and nothing left behind.
    #[test]
    fn covering_half_a_wide_glyph_leaves_nothing_behind() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut glyph = Frame::new(6, 1);
        glyph.label(0, 0, 6, "a\u{4e16}bc", "");
        let mut covered = glyph.clone();
        covered.label(2, 0, 4, "XYZ", ";7");
        let blank = Frame::new(6, 1);
        let first = glyph.diff(&blank);
        let cover = covered.diff(&glyph);
        let uncover = glyph.diff(&covered);
        assert_eq!(shown(&[&first, &cover], 6)?.trim_end(), "a XYZ");
        assert_eq!(
            shown(&[&first, &cover, &uncover], 6)?.trim_end(),
            "a\u{4e16}bc"
        );
        // And a glyph written over the first half of another.
        let mut over = glyph.clone();
        over.label(1, 0, 1, "Q", "");
        let both = [&first[..], &over.diff(&glyph)[..]];
        assert_eq!(shown(&both, 6)?.trim_end(), "aQ bc");
        Ok(())
    }

    #[test]
    fn a_wide_glyph_cut_by_the_edge_is_blank() {
        let mut frame = Frame::new(4, 1);
        frame.row(0, 0, 2, &wide_row());
        let out = String::from_utf8_lossy(&frame.diff(&Frame::new(4, 1))).into_owned();
        assert!(out.contains('a') && !out.contains('\u{4e16}'), "{out:?}");
    }
}
