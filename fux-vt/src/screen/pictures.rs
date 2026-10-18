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

/// The most bytes of pixels a terminal's pictures hold together by
/// default ([`crate::Parser::set_picture_bytes`]), however large their data
/// says they are: those kept on either screen, the one being drawn, and its
/// old pixels while it grows (`threats.md` B01). Past it, pictures no
/// longer shown go first, then the oldest. It holds the largest picture,
/// 4096 by 2160 (`sixel::MAX_SIDE`), twice.
pub const PICTURE_BYTES: usize = 96 << 20;

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

    /// Places `picture` at the cursor: its cells take the placeholder, a
    /// row at a time down from the cursor's (scrolling at the bottom, as a
    /// terminal scrolls a sixel picture), clipped at the right edge; the
    /// cursor ends below it, in its first column. A picture of this screen
    /// it covers goes, and so does one this screen no longer shows (text
    /// or an erase over it, its rows gone from history), whose pixels
    /// nothing can show again.
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
            for c in std::iter::repeat_n(PLACEHOLDER, usize::from(across)).flat_map(str::chars) {
                self.print(c)?;
            }
        }
        self.linefeed()?;
        self.grid_mut().set_col(col);
        let alternate = self.alternate_active;
        // Pictures it now covers, wholly or not, are gone: their cells hold
        // its placeholder, not theirs.
        let gone = |p: &Placement, screen: &Screen| {
            p.alternate == alternate
                && screen.shown(p).is_none_or(|s| {
                    let mine = (screen.top_row(top), col, cols, rows);
                    overlaps((s.row, s.col, s.cols, s.rows), mine)
                })
        };
        let keep: Vec<bool> = self.pictures.iter().map(|p| !gone(p, self)).collect();
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
        // Its pixels had room as it grew: only the count can be past.
        while self.pictures.len() > MAX_PICTURES || self.over_budget(0) {
            let Some(victim) = self.victim() else { break };
            self.pictures.remove(victim);
        }
        Ok(())
    }

    /// The bytes of pixels the screen's pictures hold, on either grid.
    pub(crate) fn picture_bytes(&self) -> usize {
        (self.pictures.iter())
            .map(|p| p.picture.rgba.capacity())
            .fold(0, usize::saturating_add)
    }

    /// Whether the pictures kept and `extra` bytes more are past the budget.
    fn over_budget(&self, extra: usize) -> bool {
        self.picture_bytes().saturating_add(extra) > self.picture_budget
    }

    /// The picture to let go first: the oldest no longer shown (on the
    /// other screen, covered, or its rows gone), else the oldest.
    fn victim(&self) -> Option<usize> {
        let hidden = (self.pictures.iter())
            .position(|p| p.alternate != self.alternate_active || self.shown(p).is_none());
        hidden.or_else(|| (!self.pictures.is_empty()).then_some(0))
    }

    /// Lets pictures go, [`Self::victim`] first, until `bytes` more fit in
    /// the budget beside them; false if they don't with none kept.
    pub(crate) fn make_room(&mut self, bytes: usize) -> bool {
        while self.over_budget(bytes) {
            let Some(victim) = self.victim() else {
                return false;
            };
            self.pictures.remove(victim);
        }
        true
    }

    /// Sets the bytes the pictures may hold, the one being drawn's
    /// `drawing` included, and lets pictures go to fit; false if `drawing`
    /// alone is past it.
    pub(crate) fn set_picture_bytes(&mut self, bytes: usize, drawing: usize) -> bool {
        self.picture_budget = bytes;
        self.make_room(drawing)
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

/// The pictures' part of the invariant checker (threats.md section 6).
#[cfg(feature = "checks")]
impl Screen {
    /// 30: at most [`MAX_PICTURES`] on each screen, their numbers rising.
    pub(super) fn check_picture_count(&self) -> Result<(), String> {
        for alternate in [false, true] {
            let on = self
                .pictures
                .iter()
                .filter(|p| p.alternate == alternate)
                .count();
            crate::check::ensure(on <= MAX_PICTURES, || format!("{on} pictures on a screen"))?;
        }
        let rising =
            (self.pictures.iter().zip(self.pictures.iter().skip(1))).all(|(a, b)| a.id < b.id);
        crate::check::ensure(rising, || "picture numbers out of order".into())
    }

    /// 31: the pictures kept, and `drawing` bytes of the one being drawn,
    /// hold at most the budget.
    pub(crate) fn check_picture_bytes(&self, drawing: usize) -> Result<(), String> {
        let bytes = self.picture_bytes().saturating_add(drawing);
        crate::check::ensure(bytes <= self.picture_budget, || {
            format!("{bytes} bytes of pictures, {} at most", self.picture_budget)
        })
    }

    /// 32: each picture shown has its rows, every cell it covers there
    /// holding the placeholder, on the screen in use.
    pub(super) fn check_pictures_shown(&self) -> Result<(), String> {
        let grid = self.grid();
        for shown in self.pictures() {
            let top = i64::try_from(grid.history_len())
                .unwrap_or(i64::MAX)
                .saturating_add(shown.row);
            let across = shown.cols.min(self.size().cols().saturating_sub(shown.col));
            for offset in 0..i64::from(shown.rows) {
                let row = usize::try_from(top.saturating_add(offset))
                    .ok()
                    .and_then(|i| grid.row_at(i));
                let whole = row.is_some_and(|row| {
                    (shown.col..shown.col.saturating_add(across)).all(|c| {
                        row.cell(usize::from(c))
                            .is_some_and(|cell| cell.contents() == PLACEHOLDER)
                    })
                });
                crate::check::ensure(whole, || {
                    format!("picture {} shown without its cells", shown.id)
                })?;
            }
        }
        Ok(())
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

/// The pictures' checks, each shown pictures with their one fault (HAR-26).
#[cfg(all(test, feature = "checks"))]
mod faults {
    use super::*;
    use crate::Size;

    fn placed(id: u64, rgba: Vec<u8>, top: RowId) -> Placement {
        Placement {
            picture: Arc::new(Picture {
                width: 1,
                height: 1,
                rgba,
            }),
            top,
            col: 0,
            cols: 1,
            rows: 1,
            cell: CELL,
            alternate: false,
            id,
        }
    }

    #[test]
    fn too_many_pictures_out_of_order_or_too_large_are_faults() -> Result<(), Error> {
        let size = Size::new(3, 6).map_err(|_| Error::Capacity)?;
        let screen = Screen::new(size, 0)?;
        let top = screen.primary.live_row(0).map_or(RowId(0), |r| r.id());
        assert!(screen.check_picture_count().is_ok());
        let mut many = screen.clone();
        for id in 0..65 {
            many.pictures.push_back(placed(id, vec![0; 4], top));
        }
        assert!(many.check_picture_count().is_err(), "65 pictures");
        let mut backwards = screen.clone();
        backwards.pictures.push_back(placed(2, vec![0; 4], top));
        backwards.pictures.push_back(placed(1, vec![0; 4], top));
        assert!(backwards.check_picture_count().is_err(), "out of order");
        let mut large = screen.clone();
        large.pictures.push_back(placed(
            1,
            Vec::with_capacity(PICTURE_BYTES.saturating_add(1)),
            top,
        ));
        assert!(large.check_picture_bytes(0).is_err(), "past the bytes");
        let mut small = screen;
        small.picture_budget = 0;
        assert!(small.check_picture_bytes(0).is_ok());
        assert!(small.check_picture_bytes(1).is_err(), "the drawing past it");
        Ok(())
    }
}
