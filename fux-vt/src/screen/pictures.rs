//! Pictures a program draws with sixel, placed on the screen: each takes the
//! cells it covers, which hold [`PLACEHOLDER`] while it is there. Text or
//! an erase over any of them replaces the placeholder, as a terminal's
//! text replaces the pixels below it, and the picture is gone; scrolling
//! takes the cells, and the picture, along.

use super::Screen;
use crate::RowId;
use crate::sixel::Picture;
use std::sync::Arc;

/// What a picture's cells hold while it is there: the character kitty's
/// graphics protocol keeps for cells a picture covers, one cell wide.
/// Printed as a blank.
pub const PLACEHOLDER: &str = "\u{10EEEE}";

/// The most pictures kept, on both screens together; a new one past it
/// lets one go, those no longer shown first (`Screen::victim`).
const MAX_PICTURES: usize = 64;

/// The most bytes of pixels a terminal's pictures hold together by
/// default ([`crate::Parser::set_picture_bytes`]), however large their data
/// says they are: those kept on either screen, the one being drawn, and its
/// old pixels while it grows (`THREATS.md` B01). Past it, pictures no
/// longer shown go first, then the oldest. It holds the largest picture,
/// 4096 by 2160 (`sixel::MAX_SIDE`), twice.
pub const PICTURE_BYTES: usize = 96 << 20;

/// What the host's terminal draws, for the pictures a program asks about
/// and draws: the size of a cell in pixels, if known, and whether it shows
/// sixel graphics. With no cell size known, a picture has no cells to be
/// laid out in: no picture is placed, and `CSI 14 t` and `CSI 16 t` are
/// not answered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Graphics {
    /// A cell's size in pixels.
    pub cell: Option<CellSize>,
    /// Whether it shows sixel graphics.
    pub sixel: bool,
}

/// The size of a cell in pixels, as the host's terminal says it: at least
/// a sixel's band (`LEAST`), so that placing a picture costs a bounded
/// number of cells (`THREATS.md` B05). A smaller one is no size a picture
/// can be laid out in, and is refused, not replaced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellSize {
    width: u16,
    height: u16,
}

impl CellSize {
    /// The smallest cell, width and height in pixels: a sixel band, 2 by 6.
    pub const LEAST: (u16, u16) = (2, 6);
    /// A cell `width` by `height` pixels, unless smaller than a sixel band.
    pub fn new(width: u16, height: u16) -> Option<Self> {
        holds_a_band(width, height).then_some(Self { width, height })
    }
    /// Its width in pixels.
    pub fn width(self) -> u16 {
        self.width
    }
    /// Its height in pixels.
    pub fn height(self) -> u16 {
        self.height
    }
}

/// Whether a cell `width` by `height` pixels holds a sixel band
/// (`CellSize::LEAST`): a function of its own, which the mutation guards
/// name (cargo-mutants leaves `new` unmutated).
fn holds_a_band(width: u16, height: u16) -> bool {
    width >= CellSize::LEAST.0 && height >= CellSize::LEAST.1
}

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
    /// Its first rows that are gone, scrolled off the top of a region or
    /// a screen that keeps no history (DEC STD 070-9, 11.2.4: what scrolls
    /// off is gone, the rest stays), noted by the scroll that took them
    /// (`crop_pictures`), whatever margins are set later.
    cropped: u16,
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
    /// Its first rows that are gone: scrolled off the top of a screen or
    /// a region that keeps no history (DEC STD 070-9, 11.2.4), the rest
    /// shown below them. Above the screen's top (`row` negative) or on
    /// it, above a region's top margin, those rows hold none of it.
    pub cropped: u16,
    /// The width and height in pixels of the cells it was laid out in.
    pub cell: (u16, u16),
}

impl Screen {
    /// A cell's size in pixels, the host's (`Parser::set_host_graphics`),
    /// if it said.
    pub fn cell_size(&self) -> Option<CellSize> {
        self.graphics.cell
    }

    /// Whether the host draws sixel, as DA1 says: only with a cell size.
    pub(crate) fn sixel(&self) -> bool {
        self.graphics.sixel
    }

    /// What the host's terminal draws, as [`crate::Parser::set_host_graphics`]
    /// says.
    pub(crate) fn set_graphics(&mut self, graphics: Graphics) {
        self.graphics = graphics;
    }

    /// Places `picture` at the cursor, in the host's cells (none without
    /// a cell size, which nothing can stand in for): its cells take the placeholder, a
    /// row at a time down from the cursor's (scrolling at the bottom, as a
    /// terminal scrolls a sixel picture), clipped at the right edge; the
    /// cursor ends on the row its last band ends in, in its first column. A picture of this screen
    /// it covers goes, and so does one this screen no longer shows (text
    /// or an erase over it, its rows gone from history), whose pixels
    /// nothing can show again. Says whether it was placed: not without a
    /// cell size, nor when it lost bands as it was placed.
    pub(crate) fn place_picture(&mut self, picture: Arc<Picture>) -> bool {
        let Some(cell) = self.graphics.cell else {
            return false;
        };
        if self.pictures.is_empty() {
            // Nothing to follow: its rows are followed from here.
            self.primary
                .followed()
                .set(Some(self.primary.history_numbers().1));
        }
        let cell = (cell.width(), cell.height());
        self.track_pictures();
        let cells = |pixels: u32, per: u16| {
            u16::try_from(pixels.div_ceil(u32::from(per)))
                .unwrap_or(u16::MAX)
                .max(1)
        };
        let (cols, mut rows) = (
            cells(picture.width(), cell.0),
            cells(picture.height(), cell.1),
        );
        // DECSDM set: at the top left, clipped at the bottom, scrolling
        // nothing, the cursor put back where it was (ctlseqs; xterm 411's
        // `graphics_sixel.c`).
        let display = self
            .mode(crate::Mode::SixelDisplay)
            .then(|| self.grid().cursor);
        if display.is_some() {
            let g = self.grid_mut();
            (g.cursor.pending_wrap, g.cursor.restored) = (false, false);
            g.set_row(0);
            g.set_col(0);
            rows = rows.min(self.size().rows());
        }
        let col = self.grid().cursor.col();
        // Clipped at the right margin, or the screen's edge past it (DEC STD
        // 070-9, 11.2.2: sixels "printed past the right margin are not
        // printed"), as the line a glyph printed there would end at.
        let across = cols.min(self.grid().line_end().saturating_sub(col));
        let Some(top) = self.cursor_mark().map(|mark| mark.row) else {
            return false;
        };
        // Each row's cells take the placeholder as a fill, not as text: no
        // pen, mode, cluster or print reaches them (DEC STD 070-9, 11.1).
        self.break_cluster();
        let placeholder = PLACEHOLDER.chars().next().unwrap_or(' ');
        let span = (col, col.saturating_add(across));
        // A line feed on the bottom margin between left and right margins
        // scrolls their cells, not the rows (`Grid::scroll_columns`); one
        // outside them does nothing. Either way the rows keep their
        // identities while the bands move or stack.
        let (mut lifted, mut stacked) = (false, false);
        // Each band's row filled, and the line feed before it copying a
        // row into history at the bottom: a picture's few bytes ask for
        // its rows' cells, which the budget counts here (B05, decision
        // 950), the cells' own counts aside.
        let cols = usize::from(self.size().cols());
        crate::budget::spend(
            crate::budget::units(
                usize::from(rows).saturating_mul(cols.saturating_add(usize::from(across))),
            ),
            crate::budget::weight::cells,
        );
        for row in 0..rows {
            if row > 0 && display.is_some() {
                let g = self.grid_mut();
                g.set_row(g.cursor.row().saturating_add(1));
            } else if row > 0 {
                let g = self.grid();
                let at_bottom = g.lr() && g.cursor.row() == g.lines.last();
                lifted |= at_bottom;
                stacked |= at_bottom && !g.in_columns();
                self.linefeed();
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
        // The screen row of its last band, and that row.
        let mut last = Some(self.grid().cursor.row());
        let mut bottom = self.cursor_mark().map_or(top, |mark| mark.row);
        // At most 8 rows below it: a new line goes down as many rows as the
        // aspect ratio makes six pixel rows, so a few bytes could otherwise
        // ask for any number of line feeds (B05). Each that scrolls the
        // cells between left and right margins lifts its bands with them.
        if let Some(cursor) = display {
            self.grid_mut().cursor = cursor;
        } else {
            for _ in 0..down.saturating_sub(rows.saturating_sub(1)).min(8) {
                let g = self.grid();
                if g.lr() && g.in_columns() && g.cursor.row() == g.lines.last() {
                    lifted = true;
                    last = last.and_then(|last| last.checked_sub(1));
                    let row = last.and_then(|last| self.grid().live_row(last));
                    bottom = row.map_or(bottom, |row| row.id());
                }
                self.linefeed();
            }
            self.grid_mut().set_col(col);
        }
        // Moved with the cells, the picture's rows are the last `rows`
        // down to its last band's, if they lie inside the region; else it
        // lost bands, and is not placed.
        let top = match lifted {
            false => Some(top),
            true => {
                let g = self.grid();
                let first = last.and_then(|last| last.checked_sub(rows.saturating_sub(1)));
                first
                    .filter(|first| !stacked && *first >= g.lines.first())
                    .and_then(|first| g.live_row(first))
                    .map(|row| row.id())
            }
        };
        let alternate = self.alternate_active;
        let Some(top) = top else {
            return false;
        };
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
            cropped: 0,
            id: self.picture_ids,
        });
        // Its rows, and others', may have gone into history.
        self.track_pictures();
        // Its own line feeds scrolled the rows they took off the region
        // before it was kept for `crop_pictures` to crop.
        let region = self.grid().lines;
        if !self.keeps_rows(region)
            && let Some(lost) = (self.pictures.back()).and_then(|p| self.lost_off(p, region))
            && let Some(p) = self.pictures.back_mut()
        {
            p.cropped = lost;
        }
        // Pictures it now covers, wholly or not, are gone: their cells hold
        // its placeholder, not theirs.
        let placed = self.pictures.len().saturating_sub(1);
        let mine = (self.pictures.back()).and_then(|p| Some(self.shown(p)?.area()));
        let covered = |s: Shown<'_>| mine.is_some_and(|mine| overlaps(s.area(), mine));
        let gone = |p: &Placement, screen: &Screen| {
            p.alternate == alternate && screen.shown(p).is_none_or(covered)
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
        true
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

    /// Whether `bytes` more fit in the budget beside the pictures kept; if
    /// `evict`, letting pictures go, [`Self::victim`] first, until they do.
    /// False, with none let go, if they don't with none kept.
    pub(crate) fn make_room(&mut self, bytes: usize, evict: bool) -> bool {
        if !evict || bytes > self.picture_budget {
            return !self.over_budget(bytes);
        }
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
        let kept = self.pictures.len();
        // The one being drawn past it alone goes (the parser's), and the
        // pictures kept still fit the budget without it.
        let fits = self.make_room(drawing, true) || {
            let _ = self.make_room(0, true);
            false
        };
        // A picture let go changed what the screen shows: so do its counts.
        if self.pictures.len() != kept {
            self.version = self.version.saturating_add(1);
            self.revise(1);
        }
        fits
    }

    /// The pictures the screen shows: those of the screen in use whose
    /// every cell holds the placeholder, but the rows cropped off the top
    /// of a screen or region that keeps no history (`shown`).
    pub fn pictures(&self) -> impl Iterator<Item = Shown<'_>> {
        self.track_pictures();
        self.pictures
            .iter()
            .filter(|p| p.alternate == self.alternate_active)
            .filter_map(|p| self.shown(p))
    }

    /// Follows the pictures whose top rows have gone into the main screen's
    /// history since last followed, so that each is found there by its
    /// number, not by a search of history (`THREATS.md` B09): the rows
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
    /// found by `top_index`, or, that row gone (scrolled off a screen or a
    /// region that keeps no history, as a picture taller than the rows
    /// below the cursor scrolls its own top off), its rows from its bottom
    /// row up, those gone cropped (DEC STD 070-9, 11.2.4: what scrolls off
    /// is gone, the rest stays): those above the oldest row kept, or above
    /// the top margin, which the region's scrolls took; and the cells read
    /// of its rows changed since it was last found whole (`THREATS.md`
    /// B09), and, its top gone, of its first row left, which a row edit
    /// rather than a scroll may have made another's.
    fn shown<'a>(&'a self, placement: &'a Placement) -> Option<Shown<'a>> {
        let grid = self.grid();
        let last = usize::from(placement.rows.saturating_sub(1));
        let (first, cropped) = match self.top_index(placement) {
            Some(first) => (i64::try_from(first).ok()?, None),
            None => {
                let bottom = i64::try_from(grid.screen_index_of(placement.bottom)?).ok()?;
                let first = bottom.checked_sub(i64::try_from(last).ok()?)?;
                // The rows its scrolls took, and any above the oldest row
                // kept.
                let kept = first
                    .saturating_add(i64::from(placement.cropped))
                    .max(first.max(0));
                (
                    first,
                    Some(usize::try_from(kept.saturating_sub(first)).ok()?),
                )
            }
        };
        let at = |offset: usize| {
            let index = first.checked_add(i64::try_from(offset).ok()?)?;
            grid.row_at(usize::try_from(index).ok()?)
        };
        let width = self.size().cols();
        let across = placement.cols.min(width.saturating_sub(placement.col));
        let since = match at(last) {
            Some(row) if row.id() == placement.bottom => Some(placement.whole_at),
            Some(_) | None => None,
        };
        let skip = cropped.unwrap_or(0);
        for offset in skip..usize::from(placement.rows) {
            // Gone past the bottom: not shown.
            let row = at(offset)?;
            let unsure = cropped.is_some() && offset == skip;
            if !unsure && since.is_some_and(|since| row.version() <= since) {
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
        let history = i64::try_from(grid.history_len()).ok()?;
        Some(Shown {
            id: placement.id,
            picture: &placement.picture,
            row: first.saturating_sub(history),
            col: placement.col,
            cols: across,
            rows: placement.rows,
            cropped: u16::try_from(skip).ok()?,
            cell: placement.cell,
        })
    }

    /// Notes, after a scroll up of `region` that kept none of the rows it
    /// took, the rows each picture of the screen in use whose top row it
    /// took lost off the region's top: those of its rows above the top
    /// margin now, which were the region's and are others' (a picture
    /// that went on above the region, its top row there, was cut by the
    /// scroll and is not shown after).
    #[cold]
    #[inline(never)]
    pub(crate) fn crop_pictures(
        &mut self,
        region: crate::geometry::Span,
        direction: crate::grid::Scroll,
    ) {
        use crate::grid::Scroll;
        let kept = direction == (Scroll::Up { history: true }) && self.keeps_rows(region);
        if kept || direction == Scroll::Down {
            return;
        }
        let lost: Vec<Option<u16>> = (self.pictures.iter())
            .map(|p| self.lost_off(p, region))
            .collect();
        for (p, lost) in self.pictures.iter_mut().zip(lost) {
            p.cropped = p.cropped.max(lost.unwrap_or(0));
        }
    }

    /// Whether a scroll up of `region` that may keep its rows takes them
    /// into history (`Grid::scroll`), not off the screen.
    fn keeps_rows(&self, region: crate::geometry::Span) -> bool {
        region.first() == 0 && self.grid().history_bytes() > 0
    }

    /// The rows of `p`, its top row gone, above `region`'s top margin:
    /// those a scroll up of the region that kept none took off it.
    fn lost_off(&self, p: &Placement, region: crate::geometry::Span) -> Option<u16> {
        if p.alternate != self.alternate_active || self.top_index(p).is_some() {
            return None;
        }
        let history = i64::try_from(self.grid().history_len()).ok()?;
        let at = i64::try_from(self.grid().screen_index_of(p.bottom)?).ok()?;
        let first = at
            .saturating_sub(i64::from(p.rows.saturating_sub(1)))
            .saturating_sub(history);
        let above = i64::from(region.first()).saturating_sub(first);
        Some(u16::try_from(above.clamp(0, i64::from(p.rows))).unwrap_or(p.rows))
    }

    /// The pixel size of the text area, for a program's `CSI 14 t`.
    pub(crate) fn pixel_size(&self) -> Option<(u32, u32)> {
        let cell = self.graphics.cell?;
        let (rows, cols) = self.size().into();
        Some((
            u32::from(rows).saturating_mul(u32::from(cell.height())),
            u32::from(cols).saturating_mul(u32::from(cell.width())),
        ))
    }
}

/// The pictures' part of the invariant checker (THREATS.md section 6).
#[cfg(feature = "checks")]
impl Screen {
    /// 30: at most [`MAX_PICTURES`] on both screens together, their
    /// numbers rising.
    pub(super) fn check_picture_count(&self) -> Result<(), String> {
        let kept = self.pictures.len();
        crate::check::ensure(kept <= MAX_PICTURES, || format!("{kept} pictures kept"))?;
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
    /// holding the placeholder, on the screen in use, but those cropped off
    /// the top of a screen or region that kept no history (DEC STD 070-9,
    /// 11.2.4).
    pub(super) fn check_pictures_shown(&self) -> Result<(), String> {
        let grid = self.grid();
        // What is cropped is what the scrolls noted on the placement, and
        // what lies above the screen's top: no row is cropped by a margin
        // set since.
        for (p, shown) in (self.pictures.iter())
            .filter(|p| p.alternate == self.alternate_active)
            .filter_map(|p| Some((p, self.shown(p)?)))
        {
            // Above the oldest row kept: `row` counts from the screen's top,
            // history's rows above it.
            let history = i64::try_from(grid.history_len()).unwrap_or(i64::MAX);
            let above = shown.row.saturating_add(history).saturating_neg().max(0);
            let above = u16::try_from(above).unwrap_or(u16::MAX);
            let noted = match self.top_index(p) {
                Some(_) => 0,
                None => p.cropped.max(above.min(p.rows)),
            };
            crate::check::ensure(shown.cropped == noted, || {
                format!(
                    "picture {} shown with {} rows cropped, {noted} noted",
                    shown.id, shown.cropped
                )
            })?;
        }
        for shown in self.pictures() {
            let top = i64::try_from(grid.history_len())
                .unwrap_or(i64::MAX)
                .saturating_add(shown.row);
            let across = shown.cols.min(self.size().cols().saturating_sub(shown.col));
            for offset in i64::from(shown.cropped)..i64::from(shown.rows) {
                let index = top.saturating_add(offset);
                let row = usize::try_from(index).ok().and_then(|i| grid.row_at(i));
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

impl Shown<'_> {
    /// The cells it covers: its top row left, first column, columns and
    /// rows left, its cropped rows being others'.
    fn area(&self) -> (i64, u16, u16, u16) {
        let row = self.row.saturating_add(i64::from(self.cropped));
        let rows = self.rows.saturating_sub(self.cropped);
        (row, self.col, self.cols, rows)
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

/// The pictures' checks, each shown pictures with their one fault.
#[cfg(all(test, feature = "checks"))]
mod faults {
    use super::*;
    use crate::{Error, Size};

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
            cell: (10, 20),
            alternate: false,
            whole_at: 0,
            cropped: 0,
            id,
        })
    }

    /// A placement found whole at the newest version, whose cell holds no
    /// placeholder: `shown` reads none of its rows, and check 32 reads
    /// them all (guard mutants, `check_pictures_shown`).
    #[test]
    fn a_picture_shown_without_its_cells_is_a_fault() -> Result<(), Error> {
        let size = Size::new(3, 6).map_err(|_| Error::Capacity)?;
        let mut screen = Screen::new(size, 0)?;
        let top = screen.primary.live_row(0).map_or(RowId(0), |r| r.id());
        assert!(screen.check_pictures_shown().is_ok());
        let stale = placed(1, 4, top).map(|p| Placement {
            whole_at: u64::MAX,
            ..p
        });
        screen.pictures.extend(stale);
        assert_eq!(screen.pictures().count(), 1);
        assert!(screen.check_pictures_shown().is_err());
        Ok(())
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
        // 33 on each screen: 66 in all.
        let mut both = screen.clone();
        for id in 0..66 {
            let mut picture = placed(id, 4, top);
            if let Some(p) = picture.as_mut() {
                p.alternate = id % 2 == 1;
            }
            both.pictures.extend(picture);
        }
        assert!(both.check_picture_count().is_err(), "33 pictures a screen");
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
