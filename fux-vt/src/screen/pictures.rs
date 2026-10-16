//! Pictures a program draws with sixel, placed on the screen: each takes the
//! cells it covers, which hold [`PLACEHOLDER`] while it is there. Text or
//! an erase over any of them replaces the placeholder, as a terminal's
//! text replaces the pixels below it, and the picture is gone; scrolling
//! takes the cells, and the picture, along.

use super::Screen;
use crate::Error;
use crate::RowId;
use crate::sixel::Picture;
use std::sync::Arc;

/// What a picture's cells hold while it is there: the character kitty's
/// graphics protocol keeps for cells a picture covers, one cell wide.
/// Printed as a blank.
pub const PLACEHOLDER: &str = "\u{10EEEE}";

/// The most pictures a screen keeps; a new one past it lets the oldest go.
const MAX_PICTURES: usize = 64;

/// What the host's terminal draws, for the pictures a program asks about
/// and draws: the size of a cell in pixels, if known, and whether it shows
/// sixel graphics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Graphics {
    /// A cell's width and height in pixels.
    pub cell: Option<(u16, u16)>,
    /// Whether it shows sixel graphics.
    pub sixel: bool,
}

/// The cell a picture is laid out in when the host does not say: a common
/// terminal font's.
const CELL: (u16, u16) = (10, 20);

/// A picture placed on a grid.
#[derive(Clone, Debug)]
pub(crate) struct Placement {
    picture: Arc<Picture>,
    /// Its top row, which it moves with, and its first column.
    top: RowId,
    col: u16,
    /// The cells it covers, and the cell size it was laid out in.
    cols: u16,
    rows: u16,
    cell: (u16, u16),
    /// On the alternate screen.
    alternate: bool,
    /// Its number, never reused, so a host can tell it from another.
    id: u64,
}

/// A picture as the screen shows it now: its top row on the screen, which
/// is above the screen's top (negative) once that has scrolled away, its
/// first column, the cells it covers, and its pixel size per cell.
#[derive(Clone, Copy, Debug)]
pub struct Shown<'a> {
    /// Its number, never another picture's.
    pub id: u64,
    /// Its pixels.
    pub picture: &'a Arc<Picture>,
    /// Its top row, from the screen's top.
    pub row: i64,
    /// Its first column.
    pub col: u16,
    /// The columns it covers, some maybe past the right edge.
    pub cols: u16,
    /// The rows it covers.
    pub rows: u16,
    /// The width and height in pixels of the cells it was laid out in.
    pub cell: (u16, u16),
}

impl Screen {
    /// What the host's terminal draws, as [`crate::Parser::set_host_graphics`]
    /// says.
    pub(crate) fn set_graphics(&mut self, graphics: Graphics) {
        self.graphics = graphics;
    }

    pub(crate) fn graphics(&self) -> Graphics {
        self.graphics
    }

    /// Places `picture` at the cursor: its cells take the placeholder, a
    /// row at a time down from the cursor's (scrolling at the bottom, as a
    /// terminal scrolls a sixel picture), clipped at the right edge; the
    /// cursor ends below it, in its first column. A picture it covers goes.
    pub(crate) fn place_picture(&mut self, picture: Arc<Picture>) -> Result<(), Error> {
        let cell = self.graphics.cell.unwrap_or(CELL);
        let cells = |pixels: u32, per: u16| {
            let per = u32::from(per.max(1));
            u16::try_from(pixels.div_ceil(per))
                .unwrap_or(u16::MAX)
                .max(1)
        };
        let (cols, rows) = (cells(picture.width, cell.0), cells(picture.height, cell.1));
        let col = self.grid().cursor.col();
        let width = self.size().cols();
        let across = cols.min(width.saturating_sub(col));
        let Some(top) = self.cursor_mark().map(|mark| mark.row) else {
            return Ok(());
        };
        for row in 0..rows {
            if row > 0 {
                self.linefeed()?;
            }
            self.grid_mut().set_col(col);
            self.grid_mut().cursor.pending_wrap = false;
            for _ in 0..across {
                self.print('\u{10EEEE}')?;
            }
        }
        self.linefeed()?;
        self.grid_mut().set_col(col);
        let alternate = self.alternate_active;
        // Pictures it now covers, wholly or not, are gone: their cells hold
        // its placeholder, not theirs.
        let covered = |p: &Placement, screen: &Screen| {
            p.alternate == alternate
                && screen.shown(p).is_some_and(|s| {
                    let mine = (screen.top_row(top), col, cols, rows);
                    overlaps((s.row, s.col, s.cols, s.rows), mine)
                })
        };
        let keep: Vec<bool> = self.pictures.iter().map(|p| !covered(p, self)).collect();
        let mut keep = keep.into_iter();
        self.pictures.retain(|_| keep.next().unwrap_or(true));
        self.picture_ids = self.picture_ids.wrapping_add(1);
        self.pictures.push_back(Placement {
            picture,
            top,
            col,
            cols,
            rows,
            cell,
            alternate,
            id: self.picture_ids,
        });
        if self.pictures.len() > MAX_PICTURES {
            self.pictures.pop_front();
        }
        Ok(())
    }

    /// The pictures the screen shows: on the screen in use, every cell they
    /// cover that the grid still keeps holding the placeholder.
    pub fn pictures(&self) -> impl Iterator<Item = Shown<'_>> {
        self.pictures
            .iter()
            .filter(|p| p.alternate == self.alternate_active)
            .filter_map(|p| self.shown(p))
    }

    /// `placement` as the screen shows it, if it is whole.
    fn shown<'a>(&'a self, placement: &'a Placement) -> Option<Shown<'a>> {
        let grid = self.grid();
        let first = grid.index_of(placement.top)?;
        let width = self.size().cols();
        let across = placement.cols.min(width.saturating_sub(placement.col));
        for offset in 0..usize::from(placement.rows) {
            let Some(row) = first.checked_add(offset).and_then(|i| grid.row_at(i)) else {
                continue;
            };
            let whole = (placement.col..placement.col.saturating_add(across)).all(|c| {
                row.cell(usize::from(c))
                    .is_some_and(|cell| cell.contents() == PLACEHOLDER)
            });
            if !whole {
                return None;
            }
        }
        Some(Shown {
            id: placement.id,
            picture: &placement.picture,
            row: self.top_row(placement.top),
            col: placement.col,
            cols: placement.cols,
            rows: placement.rows,
            cell: placement.cell,
        })
    }

    /// Where row `id` is from the screen's top, negative in history.
    fn top_row(&self, id: RowId) -> i64 {
        let grid = self.grid();
        let index = grid.index_of(id).and_then(|i| i64::try_from(i).ok());
        let top = i64::try_from(grid.history_len()).unwrap_or(i64::MAX);
        index.map_or(i64::MIN, |i| i.saturating_sub(top))
    }

    /// The pixel size of the text area, for a program's `CSI 14 t`.
    pub(crate) fn pixel_size(&self) -> Option<(u32, u32)> {
        let (width, height) = self.graphics.cell?;
        let (rows, cols) = self.size().into();
        Some((
            u32::from(rows).saturating_mul(u32::from(height)),
            u32::from(cols).saturating_mul(u32::from(width)),
        ))
    }
}

/// Whether two areas of cells, each its top row, first column, columns and
/// rows, share a cell.
fn overlaps(
    (row, col, cols, rows): (i64, u16, u16, u16),
    (r, c, cs, rs): (i64, u16, u16, u16),
) -> bool {
    let (end, e) = (
        row.saturating_add(i64::from(rows)),
        r.saturating_add(i64::from(rs)),
    );
    let (right, ri) = (col.saturating_add(cols), c.saturating_add(cs));
    row < e && r < end && col < ri && c < right
}
