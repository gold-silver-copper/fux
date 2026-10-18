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
    /// Its top row, which it moves with, and its first column; and its
    /// bottom row.
    top: RowId,
    bottom: RowId,
    /// The top row's number in the order rows came into the main screen's
    /// history, once it has (`Screen::track_pictures`).
    number: crate::history::Noted,
    col: u16,
    /// The cells it covers, the columns those it was placed in (clipped
    /// at the margin or edge it met), and the cell size it was laid out in.
    cols: u16,
    rows: u16,
    cell: (u16, u16),
    /// On the alternate screen.
    alternate: bool,
    /// The version it was last found whole at: a row of it no newer holds
    /// the placeholder still, as a row's version changes with its cells,
    /// while its bottom row is where it was. Scrolls keep the order of the
    /// rows they don't make anew, so no older row is among its rows then.
    whole_at: u64,
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
    /// The columns its cells hold: those it was placed in, clipped at the
    /// right margin or the screen's edge it met then, and at the edge now.
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
    /// cursor ends on the row its last band ends in, in its first column. A picture of this screen
    /// it covers goes, and so does one this screen no longer shows (text
    /// or an erase over it, its rows gone from history), whose pixels
    /// nothing can show again.
    pub(crate) fn place_picture(&mut self, picture: Arc<Picture>) -> Result<(), Error> {
        if self.pictures.is_empty() {
            // Nothing to follow: its rows are followed from here.
            self.primary
                .followed()
                .set(Some(self.primary.history_numbers().1));
        }
        self.track_pictures();
        let cell = self.graphics.cell.unwrap_or(CELL);
        let cells = |pixels: u32, per: u16| {
            let per = u32::from(per.max(1));
            u16::try_from(pixels.div_ceil(per))
                .unwrap_or(u16::MAX)
                .max(1)
        };
        let (cols, rows) = (
            cells(picture.width(), cell.0),
            cells(picture.height(), cell.1),
        );
        let col = self.grid().cursor.col();
        // Clipped at the right margin, or the screen's edge past it (DEC STD
        // 070-9, 11.2.2: sixels "printed past the right margin are not
        // printed"), as the line a glyph printed there would end at.
        let across = cols.min(self.grid().line_end().saturating_sub(col));
        let Some(top) = self.cursor_mark().map(|mark| mark.row) else {
            return Ok(());
        };
        // Each row's cells take the placeholder as a fill, not as text: no
        // pen, mode, cluster or print reaches them (DEC STD 070-9, 11.1).
        self.break_cluster();
        let placeholder = PLACEHOLDER.chars().next().unwrap_or(' ');
        let span = (col, col.saturating_add(across));
        for row in 0..rows {
            if row > 0 {
                self.linefeed()?;
            }
            self.grid_mut().set_col(col);
            self.grid_mut().cursor.pending_wrap = false;
            self.with_grid(|g, _, v| {
                let y = g.cursor.row();
                let wrapped = g.live_wrapped(y);
                g.fill(y, span, placeholder, 0, v);
                g.wrap(y, wrapped, v);
            });
        }
        let bottom = self.cursor_mark().map_or(top, |mark| mark.row);
        // "The ANSI text vertical position is updated to reflect the
        // vertical motion commands executed" (DEC STD 070-9, 11.3): the
        // cursor goes down to the row the last band ends in when that is
        // below the picture, a graphics new line having come last, as
        // encoders end; else it stays on the picture's last row, where
        // xterm 411 puts it, as "conforming software will not assume any
        // exact alignment of the ANSI text vertical position" (11.3) and
        // xterm decides what the standard leaves. Its column is where it
        // was.
        let down = picture.ended().checked_div(u32::from(cell.1)).unwrap_or(0);
        let down = u16::try_from(down).unwrap_or(u16::MAX);
        for _ in 0..down.saturating_sub(rows.saturating_sub(1)).min(8) {
            self.linefeed()?;
        }
        self.grid_mut().set_col(col);
        let alternate = self.alternate_active;
        self.picture_ids = self.picture_ids.wrapping_add(1);
        self.pictures.push_back(Placement {
            picture,
            top,
            bottom,
            number: crate::history::Noted::none(),
            col,
            cols: across,
            rows,
            cell,
            alternate,
            whole_at: self.version,
            id: self.picture_ids,
        });
        // Its rows, and others', may have gone into history.
        self.track_pictures();
        // Pictures it now covers, wholly or not, are gone: their cells hold
        // its placeholder, not theirs.
        let placed = self.pictures.len().saturating_sub(1);
        let top_row = |p: &Placement| self.row_of(self.top_index(p));
        let mine = (self.pictures.back()).map(|p| (top_row(p), col, across, rows));
        let gone = |p: &Placement, screen: &Screen| {
            p.alternate == alternate
                && screen.shown(p).is_none_or(|s| {
                    mine.is_some_and(|mine| overlaps((s.row, s.col, s.cols, s.rows), mine))
                })
        };
        let keep: Vec<bool> = (self.pictures.iter().enumerate())
            .map(|(at, p)| at >= placed || !gone(p, self))
            .collect();
        let mut keep = keep.into_iter();
        self.pictures.retain(|_| keep.next().unwrap_or(true));
        // Those of this screen kept are whole now, and this one is: what
        // changes them from here on takes a newer version.
        let whole_at = self.version;
        (self.pictures.iter_mut())
            .filter(|p| p.alternate == alternate)
            .for_each(|p| p.whole_at = whole_at);
        self.begin();
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
            .map(|p| p.picture.bytes())
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
        self.track_pictures();
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
        self.track_pictures();
        self.pictures
            .iter()
            .filter(|p| p.alternate == self.alternate_active)
            .filter_map(|p| self.shown(p))
    }

    /// Follows the pictures whose top rows have gone into the main screen's
    /// history since last followed, so that each is found there by its
    /// number, not by a search of history (`threats.md` B09): the rows
    /// come since are read once, each looked for among those tops, with
    /// nothing to do if none came or no picture's top is still to follow.
    /// Rows go into history only from the screen's top and stay in the
    /// order they came, so a top not on the screen is there or gone.
    pub(crate) fn track_pictures(&self) {
        let (first, next) = self.primary.history_numbers();
        let tracked = self.primary.followed().get().unwrap_or(first);
        if tracked == next || self.pictures.is_empty() {
            return;
        }
        let from = tracked.max(first);
        self.primary.followed().set(Some(next));
        let mut wanted: Vec<(RowId, usize)> = (self.pictures.iter().enumerate())
            .filter(|(_, p)| !p.alternate && p.number.get().is_none())
            .map(|(at, p)| (p.top, at))
            .collect();
        if wanted.is_empty() {
            return;
        }
        wanted.sort_unstable();
        for number in from..next {
            work!(rows, 1);
            let Some(id) = self.primary.id_of_number(number) else {
                continue;
            };
            if let Ok(found) = wanted.binary_search_by_key(&id, |(top, _)| *top)
                && let Some(&(_, at)) = wanted.get(found)
                && let Some(placement) = self.pictures.get(at)
            {
                placement.number.set(Some(number));
            }
        }
    }

    /// Follows the pictures into a history made anew (a resize): each is
    /// looked for in all of it once.
    pub(crate) fn retrack_pictures(&mut self) {
        self.pictures.iter().for_each(|p| p.number.set(None));
        self.primary
            .followed()
            .set(Some(self.primary.history_numbers().0));
        self.track_pictures();
    }

    /// Where `placement`'s top row is, counted from the oldest row history
    /// keeps: by its number in the main screen's history, if followed
    /// there, else among the screen's rows.
    fn top_index(&self, placement: &Placement) -> Option<usize> {
        let grid = self.grid();
        let followed = (placement.number.get())
            .filter(|_| !placement.alternate)
            .and_then(|number| grid.index_of_number(number, placement.top));
        followed.or_else(|| grid.screen_index_of(placement.top))
    }

    /// `placement` as the screen shows it, if it is whole: its top row
    /// found by `top_index`, and the cells read of its rows changed since
    /// it was last found whole (`threats.md` B09).
    fn shown<'a>(&'a self, placement: &'a Placement) -> Option<Shown<'a>> {
        let grid = self.grid();
        let first = self.top_index(placement)?;
        let width = self.size().cols();
        let across = placement.cols.min(width.saturating_sub(placement.col));
        let last = usize::from(placement.rows.saturating_sub(1));
        let bottom = first.checked_add(last).and_then(|i| grid.row_at(i));
        let since = match bottom {
            Some(row) if row.id() == placement.bottom => Some(placement.whole_at),
            Some(_) | None => None,
        };
        for offset in 0..usize::from(placement.rows) {
            let Some(row) = first.checked_add(offset).and_then(|i| grid.row_at(i)) else {
                continue;
            };
            if since.is_some_and(|since| row.version() <= since) {
                continue;
            }
            work!(cells, across);
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
            row: self.row_of(Some(first)),
            col: placement.col,
            cols: across,
            rows: placement.rows,
            cell: placement.cell,
        })
    }

    /// Where the row at `index`, counted from the oldest history keeps, is
    /// from the screen's top: negative in history, the least there is if
    /// it is gone.
    fn row_of(&self, index: Option<usize>) -> i64 {
        let top = i64::try_from(self.grid().history_len()).unwrap_or(i64::MAX);
        let index = index.and_then(|i| i64::try_from(i).ok());
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

    /// A placement of one pixel, its bytes in room for `room`.
    fn placed(id: u64, room: usize, top: RowId) -> Option<Placement> {
        let mut rgba = Vec::with_capacity(room);
        rgba.extend([0; 4]);
        Some(Placement {
            picture: Arc::new(Picture::new(1, 1, rgba)?),
            top,
            bottom: top,
            number: crate::history::Noted::none(),
            col: 0,
            cols: 1,
            rows: 1,
            cell: CELL,
            alternate: false,
            whole_at: 0,
            id,
        })
    }

    #[test]
    fn too_many_pictures_out_of_order_or_too_large_are_faults() -> Result<(), Error> {
        let size = Size::new(3, 6).map_err(|_| Error::Capacity)?;
        let screen = Screen::new(size, 0)?;
        let top = screen.primary.live_row(0).map_or(RowId(0), |r| r.id());
        assert!(screen.check_picture_count().is_ok());
        let mut many = screen.clone();
        for id in 0..65 {
            many.pictures.extend(placed(id, 4, top));
        }
        assert!(many.check_picture_count().is_err(), "65 pictures");
        let mut backwards = screen.clone();
        backwards.pictures.extend(placed(2, 4, top));
        backwards.pictures.extend(placed(1, 4, top));
        assert!(backwards.check_picture_count().is_err(), "out of order");
        let mut large = screen.clone();
        large
            .pictures
            .extend(placed(1, PICTURE_BYTES.saturating_add(1), top));
        assert!(large.check_picture_bytes(0).is_err(), "past the bytes");
        let mut small = screen;
        small.picture_budget = 0;
        assert!(small.check_picture_bytes(0).is_ok());
        assert!(small.check_picture_bytes(1).is_err(), "the drawing past it");
        Ok(())
    }
}
