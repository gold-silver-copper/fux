use std::collections::{HashMap, VecDeque};
use std::hash::{BuildHasherDefault, Hasher};
use std::num::NonZeroU16;
use std::ops::Range;
use std::sync::Arc;

use crate::cell::{Line, Spill};
use crate::link::Links;
use crate::{Attributes, Cell, CellRef, Error, Row, RowId};

/// A grid's number of rows or columns. Never zero, so a grid always has a
/// last row and a last column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Extent(NonZeroU16);

impl Extent {
    pub fn new(n: u16) -> Result<Self, Error> {
        NonZeroU16::new(n).map(Self).ok_or(Error::ZeroSize)
    }
    pub fn get(self) -> u16 {
        self.0.get()
    }
    /// The last row or column.
    pub fn last(self) -> u16 {
        // Exact: an extent is at least one.
        self.0.get().saturating_sub(1)
    }
}

/// Maximum addressable retained cells per buffer (2.5 GiB at 40 bytes/cell).
/// Storage is committed only for live/retained rows, not empty history slots.
pub(crate) const MAX_CELLS: usize = 64 * 1024 * 1024;
pub(crate) const MAX_ROWS: usize = 1_048_576;

#[derive(Clone, Copy, Debug)]
struct Meta {
    id: RowId,
    version: u64,
    width: u16,
    wrapped: bool,
    /// How far into the row a cell may differ from `Cell::default()`:
    /// every cell from here to `width` is blank, so recycling the slot
    /// clears only the cells before it. A short line scrolled away costs a
    /// few cells, not a row of cold memory.
    used: u16,
    /// Whether the row has an array of links in `Grid::linked`.
    linked: bool,
    /// Whether a prompt starts on the row (OSC 133 ; A).
    prompt: bool,
}

// The two flags fit where the struct had padding: a row without links costs
// no more than it did before links.
const _: () = assert!(std::mem::size_of::<Meta>() == 24);

impl Meta {
    fn new(id: RowId, version: u64, width: u16, wrapped: bool, used: u16) -> Self {
        Self {
            id,
            version,
            width,
            wrapped,
            used,
            linked: false,
            prompt: false,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Grid {
    cells: Vec<Cell>,
    meta: Vec<Meta>,
    /// Each slot's text too long for its cells to hold inline.
    spill: Vec<Spill>,
    /// The links the cells point to.
    pub links: Links,
    /// The link of each cell of the slots whose `Meta::linked` is set, by
    /// slot: only rows a link was printed in have one (see `link.rs`).
    linked: Linked,
    order: VecDeque<usize>,
    stride: usize,
    pub rows: Extent,
    pub cols: Extent,
    pub history_limit: usize,
    /// Always on the grid: a glyph that reaches the last column leaves the
    /// cursor there with `pending_wrap` set.
    pub cursor: (u16, u16),
    /// DEC STD 070's Last Column Flag (Appendix D.6.1): a glyph went into
    /// the last column, so the next one, with autowrap on, first moves to
    /// the start of the next line.
    pub pending_wrap: bool,
    pub saved_cursor: (u16, u16),
    /// The flag DECSC saved with the cursor, which DECRC restores.
    pub saved_pending_wrap: bool,
    pub origin: bool,
    pub saved_origin: bool,
    pub top: u16,
    pub bottom: u16,
}

/// A grid's cursor and what goes with it (`Grid::clone_cursor`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct CursorState {
    cursor: (u16, u16),
    pending_wrap: bool,
    origin: bool,
    margins: (u16, u16),
    saved: ((u16, u16), bool, bool),
}

/// Which way a scroll moves rows: up, the rows leaving the top going into
/// history if `history` (and the region is the whole screen), or down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Scroll {
    Up { history: bool },
    Down,
}

pub(crate) fn next_id(next: &mut u64) -> Result<RowId, Error> {
    let id = *next;
    *next = next.checked_add(1).ok_or(Error::IdentityExhausted)?;
    Ok(RowId(id))
}

impl Grid {
    pub fn check_size(rows: u16, cols: u16, history: usize) -> Result<(Extent, Extent), Error> {
        let (rows, cols) = (Extent::new(rows)?, Extent::new(cols)?);
        let retained = history
            .checked_add(usize::from(rows.get()))
            .ok_or(Error::Capacity)?;
        if retained > MAX_ROWS
            || retained
                .checked_mul(usize::from(cols.get()))
                .is_none_or(|n| n > MAX_CELLS)
        {
            return Err(Error::Capacity);
        }
        Ok((rows, cols))
    }

    pub fn new(
        rows: u16,
        cols: u16,
        history_limit: usize,
        next: &mut u64,
        version: u64,
    ) -> Result<Self, Error> {
        let (rows, cols) = Self::check_size(rows, cols, history_limit)?;
        let mut grid = Self {
            cells: Vec::new(),
            meta: Vec::new(),
            spill: Vec::new(),
            links: Links::default(),
            linked: Linked::default(),
            order: VecDeque::new(),
            stride: usize::from(cols.get()),
            rows,
            cols,
            history_limit,
            cursor: (0, 0),
            pending_wrap: false,
            saved_cursor: (0, 0),
            saved_pending_wrap: false,
            origin: false,
            saved_origin: false,
            top: 0,
            bottom: rows.last(),
        };
        grid.reserve_rows(usize::from(rows.get()))?;
        for _ in 0..rows.get() {
            let slot = grid.allocate(next, version)?;
            grid.order.push_back(slot);
        }
        Ok(grid)
    }

    fn reserve_rows(&mut self, count: usize) -> Result<(), Error> {
        let size = count.checked_mul(self.stride).ok_or(Error::Capacity)?;
        if size > MAX_CELLS {
            return Err(Error::Capacity);
        }
        self.cells
            .try_reserve_exact(size.saturating_sub(self.cells.len()))
            .map_err(|_| Error::Capacity)?;
        self.meta
            .try_reserve_exact(count.saturating_sub(self.meta.len()))
            .map_err(|_| Error::Capacity)?;
        self.spill
            .try_reserve_exact(count.saturating_sub(self.spill.len()))
            .map_err(|_| Error::Capacity)?;
        self.order
            .try_reserve_exact(count.saturating_sub(self.order.len()))
            .map_err(|_| Error::Capacity)?;
        Ok(())
    }

    fn allocate(&mut self, next: &mut u64, version: u64) -> Result<usize, Error> {
        let slot = self.meta.len();
        let end = self
            .cells
            .len()
            .checked_add(self.stride)
            .ok_or(Error::Capacity)?;
        if slot == self.meta.capacity() || end > self.cells.capacity() {
            let maximum = self
                .history_limit
                .checked_add(usize::from(self.rows.get()))
                .ok_or(Error::Capacity)?;
            // Doubling, up to what the grid can ever retain.
            let capacity = slot
                .checked_add(1)
                .ok_or(Error::Capacity)?
                .saturating_mul(2)
                .min(maximum);
            self.reserve_rows(capacity)?;
        }
        let id = next_id(next)?;
        self.cells.resize(end, Cell::default());
        self.meta
            .push(Meta::new(id, version, self.cols.get(), false, 0));
        self.spill.push(Spill::default());
        Ok(slot)
    }

    pub fn history_len(&self) -> usize {
        // A grid always retains its live rows.
        self.order
            .len()
            .saturating_sub(usize::from(self.rows.get()))
    }
    pub fn retained_len(&self) -> usize {
        self.order.len()
    }
    pub fn storage_cells(&self) -> usize {
        self.cells.capacity()
    }

    /// Where a slot's cells are: `width` cells from the slot's stride.
    fn cells_of(&self, slot: usize) -> Option<Range<usize>> {
        let width = usize::from(self.meta.get(slot)?.width);
        let start = slot.checked_mul(self.stride)?;
        Some(start..start.checked_add(width)?)
    }
    fn slice(&self, slot: usize) -> &[Cell] {
        self.cells_of(slot)
            .and_then(|cells| self.cells.get(cells))
            .unwrap_or(&[])
    }
    fn slice_mut(&mut self, slot: usize) -> &mut [Cell] {
        match self.cells_of(slot) {
            Some(cells) => self.cells.get_mut(cells).unwrap_or(&mut []),
            None => &mut [],
        }
    }
    /// A slot's cells and text, to write into.
    fn line(&mut self, slot: usize) -> Option<Line<'_>> {
        let cells = self.cells_of(slot)?;
        Some(Line {
            cells: self.cells.get_mut(cells)?,
            spill: self.spill.get_mut(slot)?,
        })
    }
    pub fn row_at(&self, index: usize) -> Option<Row<'_>> {
        let slot = *self.order.get(index)?;
        let m = self.meta.get(slot)?;
        let links = if m.linked {
            self.linked.get(&slot).map(|links| &**links)
        } else {
            None
        };
        Some(Row {
            id: m.id,
            version: m.version,
            wrapped: m.wrapped,
            prompt: m.prompt,
            cells: self.slice(slot),
            spill: self.spill.get(slot)?,
            links,
            table: &self.links,
        })
    }
    pub fn row_by_id(&self, id: RowId) -> Option<Row<'_>> {
        self.order
            .iter()
            .position(|slot| self.meta.get(*slot).is_some_and(|m| m.id == id))
            .and_then(|index| self.row_at(index))
    }
    pub fn index_of(&self, id: RowId) -> Option<usize> {
        self.order
            .iter()
            .position(|slot| self.meta.get(*slot).is_some_and(|m| m.id == id))
    }
    /// The retained index of a live row.
    fn index(&self, row: u16) -> Option<usize> {
        if row >= self.rows.get() {
            return None;
        }
        self.history_len().checked_add(usize::from(row))
    }
    pub fn live_row(&self, row: u16) -> Option<Row<'_>> {
        self.row_at(self.index(row)?)
    }
    fn slot(&self, row: u16) -> Option<usize> {
        self.order.get(self.index(row)?).copied()
    }
    /// A live row's cell, found without making its `Row`: printing asks
    /// for cells on its way, and a `Row` carries the row's links.
    pub fn cell(&self, row: u16, col: u16) -> Option<CellRef<'_>> {
        let slot = self.slot(row)?;
        let cell = self.slice(slot).get(usize::from(col))?;
        Some(CellRef::new(cell, self.spill.get(slot)?))
    }
    /// A live row's cells, as `cell` finds them.
    pub fn live_cells(&self, row: u16) -> &[Cell] {
        self.slot(row).map_or(&[], |slot| self.slice(slot))
    }
    /// Edits a live row's cells with `f`, which says whether it changed any
    /// of them; only then does the row take `version`. An edit that leaves
    /// the row as it was leaves its version alone. A blank cell `f` makes
    /// anything else is before `end`.
    pub fn mutate_row(
        &mut self,
        row: u16,
        version: u64,
        end: u16,
        f: impl FnOnce(&mut [Cell]) -> bool,
    ) {
        if let Some(slot) = self.slot(row)
            && f(self.slice_mut(slot))
            && let Some(m) = self.meta.get_mut(slot)
        {
            m.version = version;
            m.used = m.used.max(end.min(m.width));
        }
    }
    /// `mutate_row` with the row's text too, for edits that store clusters.
    pub fn mutate_line(
        &mut self,
        row: u16,
        version: u64,
        end: u16,
        f: impl FnOnce(&mut Line<'_>) -> bool,
    ) {
        if let Some(slot) = self.slot(row)
            && let Some(mut line) = self.line(slot)
            && f(&mut line)
            && let Some(m) = self.meta.get_mut(slot)
        {
            m.version = version;
            m.used = m.used.max(end.min(m.width));
        }
    }
    /// Gives the cells `span` of live row `row` link `link`, 0 for none, as
    /// printing them does. A row gets its array of links only when given a
    /// link; it takes `version` if a cell's link changed.
    pub fn set_link(&mut self, row: u16, span: Range<usize>, link: u16, version: u64) {
        let Some(slot) = self.slot(row) else {
            return;
        };
        let Some(m) = self.meta.get_mut(slot) else {
            return;
        };
        if !m.linked {
            if link == 0 {
                return;
            }
            let none = vec![0; usize::from(m.width)];
            // The links of the row the slot held before, which recycling it
            // left, are let go now.
            if let Some(old) = self.linked.insert(slot, none.into_boxed_slice()) {
                self.links.release_all(&old);
            }
            m.linked = true;
        }
        if let Some(run) = self.linked.get_mut(&slot).and_then(|l| l.get_mut(span))
            && run.iter().any(|n| *n != link)
        {
            self.links.release_all(run);
            self.links
                .hold(link, u32::try_from(run.len()).unwrap_or(u32::MAX));
            run.fill(link);
            m.version = version;
        }
    }

    /// Forgets slot `slot`'s links; whether it had any.
    fn unlink(&mut self, slot: usize) -> bool {
        match self.meta.get_mut(slot) {
            Some(m) if m.linked => {
                m.linked = false;
                forget(&mut self.linked, &mut self.links, slot);
                true
            }
            Some(_) | None => false,
        }
    }

    /// The cells the grid counts for each link, and those a count of its
    /// rows' links finds; and whether every number a row has is a link's.
    #[cfg(test)]
    pub fn link_counts(&self) -> (Vec<u32>, Vec<u32>, bool) {
        let mut fresh = self.links.clone();
        fresh.recount(self.linked.values().map(|row| &**row));
        let held = self
            .linked
            .values()
            .flat_map(|row| row.iter())
            .all(|n| *n == 0 || self.links.get(*n).is_some());
        (self.links.counts(), fresh.counts(), held)
    }

    /// Forgets every link: RIS.
    pub fn reset_links(&mut self) {
        self.linked.clear();
        self.links = Links::default();
    }

    /// Takes `links` as the grid's links, counting their cells: those of a
    /// grid that this one replaces in a resize, which copied its rows.
    pub fn adopt_links(&mut self, links: Links) {
        self.links = links;
        self.links.recount(self.linked.values().map(|row| &**row));
    }

    /// The number of the link of `uri` and `id`, held as a new link with
    /// `key` if it is not held yet. With no room for it, room is made
    /// (`make_room`), at most once in a while after that fails; `None` if
    /// there is still none.
    pub fn intern(
        &mut self,
        uri: &Arc<str>,
        id: Option<&Arc<str>>,
        key: u64,
        version: u64,
    ) -> Option<u16> {
        if let Some(id) = id
            && let Some(n) = self.links.find(uri, id)
        {
            return Some(n);
        }
        let id_text = id.map(|id| &**id);
        if !self.links.fits(uri, id_text) {
            if !self.links.may_make_room() {
                return None;
            }
            self.make_room(version);
            if !self.links.fits(uri, id_text) {
                self.links.stall();
                return None;
            }
        }
        self.links.insert(uri, id, key)
    }

    /// Frees the links no row has. If more than half the bounds are still
    /// in use, the oldest history rows lose their links, taking `version`,
    /// until the links rows have are at most half, and the links only they
    /// had are freed: history loses links before text, and the screen's
    /// rows never do. The links are counted (`link.rs`), so this walks the
    /// links and the history rows that lose theirs, never every cell.
    fn make_room(&mut self, version: u64) {
        // The links of rows recycled since, which no row has now.
        let left: Vec<usize> = self
            .linked
            .keys()
            .filter(|slot| !self.meta.get(**slot).is_some_and(|m| m.linked))
            .copied()
            .collect();
        for slot in left {
            forget(&mut self.linked, &mut self.links, slot);
        }
        self.links.free_unused(&[]);
        if self.links.used_within_half() {
            return;
        }
        for index in 0..self.history_len() {
            if self.links.used_within_half() {
                break;
            }
            if let Some(&slot) = self.order.get(index)
                && self.unlink(slot)
                && let Some(m) = self.meta.get_mut(slot)
            {
                m.version = version;
            }
        }
        self.links.free_unused(&[]);
    }

    /// Marks live row `row` as where a prompt starts (OSC 133 ; A).
    pub fn mark_prompt(&mut self, row: u16) {
        if let Some(slot) = self.slot(row)
            && let Some(m) = self.meta.get_mut(slot)
        {
            m.prompt = true;
        }
    }

    /// Unmarks live row `row` as where a prompt starts: ED erased it.
    pub fn clear_prompt(&mut self, row: u16) {
        if let Some(slot) = self.slot(row)
            && let Some(m) = self.meta.get_mut(slot)
        {
            m.prompt = false;
        }
    }

    pub fn wrap(&mut self, row: u16, wrapped: bool, version: u64) {
        if let Some(slot) = self.slot(row)
            && let Some(m) = self.meta.get_mut(slot)
            && m.wrapped != wrapped
        {
            m.version = version;
            m.wrapped = wrapped;
        }
    }
    pub fn erase(&mut self, row: u16, start: u16, end: u16, attributes: Attributes, version: u64) {
        let (cols, last) = (self.cols.get(), self.cols.last());
        let mut clears_edge = end >= cols;
        // Blanks in the default attributes are what a recycled row holds;
        // others count as written.
        let written = if attributes == Attributes::default() {
            0
        } else {
            end
        };
        self.mutate_row(row, version, written, |cells| {
            let span = usize::from(start)..usize::from(end.min(cols));
            // Already blank in this style, as erasing an erased tail finds
            // it: the row is as it was. A blank is never half a wide glyph,
            // so there is nothing to repair either.
            let blank = Cell::blank(attributes);
            if cells
                .get(span.clone())
                .is_none_or(|run| run.iter().all(|c| c.same(&blank)))
            {
                return false;
            }
            for col in span {
                if let Some(cell) = cells.get(col).copied() {
                    if cell.is_wide() {
                        let next = col.checked_add(1);
                        if let Some(other) = next.and_then(|i| cells.get_mut(i)) {
                            *other = Cell::blank(other.attributes);
                        }
                        // The glyph's second half is in the last column.
                        clears_edge |= next == Some(usize::from(last));
                    } else if cell.is_wide_continuation()
                        && let Some(other) = col.checked_sub(1).and_then(|i| cells.get_mut(i))
                    {
                        *other = Cell::blank(other.attributes);
                    }
                    if let Some(cell) = cells.get_mut(col) {
                        *cell = blank;
                    }
                }
            }
            true
        });
        if clears_edge {
            self.wrap(row, false, version);
        }
        // A whole row erased keeps no text, and no links: its blank cells
        // would never read them.
        if start == 0
            && end >= cols
            && let Some(slot) = self.slot(row)
        {
            if let Some(spill) = self.spill.get_mut(slot) {
                spill.clear();
            }
            self.unlink(slot);
        }
    }

    /// ICH and DCH, at the cursor, which they leave where it is; they end
    /// a pending wrap (DEC STD 070, Appendix D.6.1). The cells they bring
    /// in are blank in `blank`. DCH ends the row's soft wrap; ICH, and the
    /// insertion IRM makes, keep it, as xterm does.
    pub fn edit_cells(&mut self, count: u16, insert: bool, blank: Attributes, version: u64) {
        self.pending_wrap = false;
        let (row, col) = self.cursor;
        // At most the cells from the cursor to the edge.
        let count = usize::from(count.min(self.cols.get().saturating_sub(col)));
        if count == 0 {
            return;
        }
        let cols = self.cols.get();
        self.mutate_row(row, version, cols, |cells| {
            let col = usize::from(col);
            let len = cells.len();
            // Where the cells shifted out start.
            let shifted = if insert {
                len.checked_sub(count)
            } else {
                col.checked_add(count)
            };
            let Some(shifted) = shifted.filter(|_| count <= len) else {
                // Nothing was edited.
                return false;
            };
            // Clear a wide glyph straddling either edit boundary before shifting.
            for boundary in [col, shifted] {
                if cells.get(boundary).is_some_and(Cell::is_wide_continuation) {
                    if let Some(c) = boundary.checked_sub(1).and_then(|i| cells.get_mut(i)) {
                        *c = Cell::blank(c.attributes);
                    }
                    if let Some(c) = cells.get_mut(boundary) {
                        *c = Cell::blank(c.attributes);
                    }
                }
            }
            shift(cells, col, count, insert, Cell::blank(blank));
            repair_wide(cells);
            // Inserting and deleting always count as a change.
            true
        });
        // The cells' links move with them.
        if let Some(slot) = self.slot(row)
            && let Some(links) = self.linked.get_mut(&slot)
        {
            // The cells shifted out of the row lose their links: the last
            // `count` to insert, those from the cursor to delete.
            let col = usize::from(col);
            let gone = if insert {
                links.len().saturating_sub(count)..links.len()
            } else {
                col..col.saturating_add(count)
            };
            self.links.release_all(links.get(gone).unwrap_or_default());
            shift(links, col, count, insert, 0);
        }
        if !insert {
            self.wrap(row, false, version);
        }
    }

    /// Gives a slot a new row: `id`, unwrapped, every cell blank. Only the
    /// cells that may not be blank already are cleared. The old row's
    /// links, if it had any, are left in `linked` for `set_link` or
    /// `make_room` to let go (`Meta::linked` says they are no row's): a
    /// scroll costs what it did before links.
    #[inline]
    fn recycle(&mut self, slot: usize, id: RowId, version: u64) {
        let mut used = usize::from(self.cols.get());
        if let Some(m) = self.meta.get_mut(slot) {
            used = usize::from(m.used);
            *m = Meta::new(id, version, self.cols.get(), false, 0);
        }
        let cells = self.slice_mut(slot);
        let used = used.min(cells.len());
        if let Some(cells) = cells.get_mut(..used) {
            cells.fill(Cell::default());
        }
        if let Some(spill) = self.spill.get_mut(slot) {
            spill.clear();
        }
    }

    /// Gives the blank cells of a row brought in the attributes `blank`,
    /// the pen's colours. They are blanked first in the default attributes,
    /// all zeros, which compiles to a memset, much faster than storing any
    /// other cell; this goes over them again only for another pen, and is
    /// kept out of line so the two are never fused into one slower loop.
    #[inline(never)]
    fn colour(&mut self, slot: usize, blank: Attributes) {
        if blank != Attributes::default() {
            for cell in self.slice_mut(slot) {
                cell.attributes = blank;
            }
            if let Some(m) = self.meta.get_mut(slot) {
                m.used = m.width;
            }
        }
    }

    /// Moves slots, not cells. Only whole-screen upward scrolling enters
    /// history. The rows brought in are blank in `blank`.
    pub fn scroll(
        &mut self,
        (top, bottom): (u16, u16),
        count: u16,
        direction: Scroll,
        blank: Attributes,
        next: &mut u64,
        version: u64,
    ) -> Result<(), Error> {
        let up = direction != Scroll::Down;
        if bottom >= self.rows.get() {
            return Ok(());
        }
        let Some(height) = bottom.checked_sub(top).and_then(|h| h.checked_add(1)) else {
            return Ok(());
        };
        let count = count.min(height);
        for _ in 0..count {
            if direction == (Scroll::Up { history: true })
                && top == 0
                && bottom == self.rows.last()
                && self.history_limit > 0
            {
                if self.history_len() < self.history_limit {
                    let slot = self.allocate(next, version)?;
                    self.colour(slot, blank);
                    self.order.push_back(slot);
                } else {
                    let id = next_id(next)?;
                    if let Some(slot) = self.order.pop_front() {
                        self.recycle(slot, id, version);
                        self.colour(slot, blank);
                        self.order.push_back(slot);
                    }
                }
            } else {
                let id = next_id(next)?;
                let (from, to) = if up { (top, bottom) } else { (bottom, top) };
                let (Some(from), Some(to)) = (self.index(from), self.index(to)) else {
                    return Ok(());
                };
                if let Some(slot) = self.move_row(from, to) {
                    self.recycle(slot, id, version);
                    self.colour(slot, blank);
                }
                if !up {
                    self.wrap(bottom, false, version);
                }
            }
        }
        Ok(())
    }

    /// Moves the retained row at `from` to `to`, the rows between closing up
    /// behind it: a removal then an insertion, one row at a time. Its slot,
    /// or `None`, with nothing moved, if either index is out of range.
    fn move_row(&mut self, from: usize, to: usize) -> Option<usize> {
        let slot = *self.order.get(from)?;
        if to >= self.order.len() {
            return None;
        }
        // Usually the rows from `from` to `to` lie in one of the deque's two
        // slices: there the move turns that run by one.
        let (low, high) = (from.min(to), from.max(to));
        let (front, back) = self.order.as_mut_slices();
        let split = front.len();
        let run = if high < split {
            front.get_mut(low..=high)
        } else {
            match (low.checked_sub(split), high.checked_sub(split)) {
                (Some(low), Some(high)) => back.get_mut(low..=high),
                _ => None,
            }
        };
        if let Some(run) = run {
            // The run holds `from` and `to`, so at least one row: a turn by
            // one never passes its end.
            if from < to {
                run.rotate_left(1);
            } else {
                run.rotate_right(1);
            }
            return Some(slot);
        }
        // Across the two slices, one row at a time.
        let mut at = from;
        while at != to {
            let next = if at < to {
                at.checked_add(1)?
            } else {
                at.checked_sub(1)?
            };
            let row = *self.order.get(next)?;
            *self.order.get_mut(at)? = row;
            at = next;
        }
        *self.order.get_mut(to)? = slot;
        Some(slot)
    }

    /// Build replacement storage first, so allocation failure leaves this grid unchanged.
    pub fn resized(
        &self,
        rows: u16,
        cols: u16,
        next: &mut u64,
        version: u64,
    ) -> Result<Self, Error> {
        let (rows, cols) = Self::check_size(rows, cols, self.history_limit)?;
        let history = self.history_len();
        // Reflow around the cursor so the line it is on stays visible (hunt 8
        // finding 017). A shrink drops rows below the cursor first, and only
        // then scrolls rows above it into history: a screen with its content at
        // the top keeps it, and a full screen keeps its bottom line. A grow
        // pulls rows back from history above, as xterm does, and pads the rest
        // with blank rows below. history_limit bounds history, oldest first.
        let old_retained = self.retained_len();
        let live_top = match rows.get().checked_sub(self.rows.get()) {
            // A grow takes back as many history rows as there are.
            Some(grown) if grown > 0 => history.saturating_sub(usize::from(grown)),
            // A shrink scrolls up only the rows from the top through the
            // cursor's that no longer fit.
            Some(_) | None => {
                let through_cursor = usize::from(self.cursor.0)
                    .checked_add(1)
                    .ok_or(Error::Capacity)?;
                history
                    .checked_add(through_cursor.saturating_sub(usize::from(rows.get())))
                    .ok_or(Error::Capacity)?
            }
        };
        let new_history = live_top.min(self.history_limit);
        // Rows past the history limit are dropped, oldest first.
        let base = live_top.saturating_sub(self.history_limit);
        let keep_total = new_history
            .checked_add(usize::from(rows.get()))
            .ok_or(Error::Capacity)?;
        // Both cursors move with the rows they sit on, and stop at the last.
        let shifted = |row: u16| {
            let index = history.checked_add(usize::from(row));
            let row = index.map_or(usize::MAX, |i| i.saturating_sub(live_top));
            u16::try_from(row).map_or(rows.last(), |row| row.min(rows.last()))
        };
        // History rows keep their old width, so the stride covers exactly the
        // rows that become history -- including live rows a shrink scrolls up,
        // which old history alone would under-size. Live rows take `cols`; a
        // wider stride would carry a narrowed pane's old width forever.
        let stride = (base..live_top)
            .filter_map(|i| self.row_at(i))
            .map(|r| r.cells.len())
            .max()
            .unwrap_or(0)
            .max(usize::from(cols.get()));
        let mut replacement = Self {
            cells: Vec::new(),
            meta: Vec::new(),
            spill: Vec::new(),
            links: Links::default(),
            linked: Linked::default(),
            order: VecDeque::new(),
            stride,
            rows,
            cols,
            history_limit: self.history_limit,
            // A pending wrap is dropped: the cursor goes one past where it
            // waited, as far as the new width allows.
            cursor: (shifted(self.cursor.0), self.next_column().min(cols.last())),
            pending_wrap: false,
            saved_cursor: (
                shifted(self.saved_cursor.0),
                past(self.saved_cursor.1, self.saved_pending_wrap).min(cols.last()),
            ),
            saved_pending_wrap: false,
            origin: self.origin,
            saved_origin: self.saved_origin,
            top: self.top,
            bottom: if self.bottom == self.rows.last() {
                rows.last()
            } else {
                self.bottom.min(rows.last())
            },
        };
        if replacement.top > replacement.bottom {
            replacement.top = 0;
        }
        replacement.reserve_rows(keep_total)?;
        let end = base.checked_add(keep_total).ok_or(Error::Capacity)?;
        for (p, source) in (base..end).enumerate() {
            let old = self.row_at(source).filter(|_| source < old_retained);
            let is_history = p < new_history;
            let id = match old {
                Some(r) => r.id,
                None => next_id(next)?,
            };
            let width = match old {
                // A row is never wider than the u16 grid it was made in.
                Some(r) if is_history => {
                    u16::try_from(r.cells.len()).map_err(|_| Error::Capacity)?
                }
                Some(_) | None => cols.get(),
            };
            let start = replacement.cells.len();
            let row_end = start.checked_add(stride).ok_or(Error::Capacity)?;
            replacement.cells.resize(row_end, Cell::default());
            let wrapped = old.is_some_and(|r| r.wrapped) && is_history;
            let row_version = if is_history {
                old.map_or(version, |r| r.version)
            } else {
                version
            };
            let mut meta = Meta::new(id, row_version, width, wrapped, width);
            meta.prompt = old.is_some_and(|r| r.prompt);
            // As much of the old row's links as fits, as of its cells.
            if let Some(links) = old.and_then(|r| r.links) {
                let mut kept = vec![0; usize::from(width)];
                let len = kept.len().min(links.len());
                if let (Some(dst), Some(src)) = (kept.get_mut(..len), links.get(..len)) {
                    crate::copy_from(dst, src);
                }
                replacement.linked.insert(p, kept.into_boxed_slice());
                meta.linked = true;
            }
            replacement.meta.push(meta);
            replacement.spill.push(Spill::default());
            replacement.order.push_back(p);
            if let Some(old) = old {
                // As much of the old row as fits, over the new one's start:
                // both runs are `len` long.
                let len = old.cells.len().min(usize::from(width));
                let dst = start
                    .checked_add(len)
                    .and_then(|end| replacement.cells.get_mut(start..end));
                if let (Some(dst), Some(src)) = (dst, old.cells.get(..len)) {
                    crate::copy_from(dst, src);
                }
                repair_wide(replacement.slice_mut(p));
                // The row's text comes along, stored again within the
                // budget of the row's new width.
                let text = Line::rebuilt(replacement.slice_mut(p), old.spill);
                if let Some(spill) = replacement.spill.get_mut(p) {
                    *spill = text;
                }
            }
        }
        Ok(replacement)
    }

    /// The primary screen resized with reflow: every logical line, a run of
    /// soft-wrapped rows and the row that ends it, history included, is
    /// re-wrapped at `cols`, without splitting wide glyphs. The cursor stays
    /// on its character. When there are more rows than fit, blank lines
    /// below the cursor go first, then lines scroll into history, oldest
    /// dropped first past the history limit; rows below the screen once
    /// the cursor's row is at its top are dropped. The scroll region is
    /// reset, as xterm does on resize. Like `resized`, it builds replacement
    /// storage first, so allocation failure leaves this grid unchanged.
    ///
    /// Two walks over the rows, one to lay them out and one to copy them,
    /// so no line is ever gathered in memory of its own. The k-th row of a
    /// reflowed line keeps the identity of the line's k-th row before, if it
    /// had one; every row takes `version`.
    pub fn reflowed(
        &self,
        rows: u16,
        cols: u16,
        next: &mut u64,
        version: u64,
    ) -> Result<Self, Error> {
        let (rows, cols) = Self::check_size(rows, cols, self.history_limit)?;
        // The cursor and the saved cursor go with their characters; one
        // waiting to wrap is laid out one past its glyph.
        let at = |(row, col): (u16, u16), pending: bool| {
            self.history_len()
                .checked_add(usize::from(row))
                .map(|row| (row, usize::from(past(col, pending))))
        };
        let marks = [
            at(self.cursor, self.pending_wrap),
            at(self.saved_cursor, self.saved_pending_wrap),
        ];
        let layout = self.reflow(usize::from(cols.get()), marks, &mut Layout)?;
        // Blank lines below the cursor are dropped before any line scrolls
        // into history: a mostly empty screen keeps its text on screen.
        let screen = usize::from(rows.get());
        let [(cursor_row, cursor_col), (saved_row, saved_col)] = layout.marks;
        let drop = layout
            .trailing_blank
            .min(layout.rows.saturating_sub(screen))
            .min(layout.rows.saturating_sub(cursor_row.saturating_add(1)));
        let total = layout.rows.saturating_sub(drop);
        let live_top = total.saturating_sub(screen).min(cursor_row);
        let base = live_top.saturating_sub(self.history_limit);
        let end = total.min(live_top.checked_add(screen).ok_or(Error::Capacity)?);
        let keep_total = live_top
            .saturating_sub(base)
            .checked_add(screen)
            .ok_or(Error::Capacity)?;
        // One past the last column is the last column, waiting to wrap.
        let column = |col: usize| u16::try_from(col).map_or(cols.get(), |col| col.min(cols.get()));
        let (cursor_col, saved_col) = (column(cursor_col), column(saved_col));
        // A row on the screen, the first if above it, the last if below.
        let row = |row: usize| {
            u16::try_from(row.saturating_sub(live_top))
                .map_or(rows.last(), |row| row.min(rows.last()))
        };
        let mut replacement = Self {
            cells: Vec::new(),
            meta: Vec::new(),
            spill: Vec::new(),
            links: Links::default(),
            linked: Linked::default(),
            order: VecDeque::new(),
            stride: usize::from(cols.get()),
            rows,
            cols,
            history_limit: self.history_limit,
            // The cursor's row is on screen: `live_top` is at most its row,
            // and the screen reaches past it.
            cursor: (row(cursor_row), cursor_col.min(cols.last())),
            pending_wrap: cursor_col >= cols.get(),
            // The saved cursor moves with its character as the cursor
            // does, so DECRC (as 1049 leaves the alternate screen) finds it.
            saved_cursor: (row(saved_row), saved_col.min(cols.last())),
            saved_pending_wrap: saved_col >= cols.get(),
            origin: self.origin,
            saved_origin: self.saved_origin,
            top: 0,
            bottom: rows.last(),
        };
        replacement.reserve_rows(keep_total)?;
        let size = keep_total
            .checked_mul(replacement.stride)
            .ok_or(Error::Capacity)?;
        replacement.cells.resize(size, Cell::default());
        replacement.spill.resize_with(keep_total, Spill::default);
        let mut copy = Copy {
            grid: &mut replacement,
            rows: base..end,
            next,
            version,
        };
        self.reflow(usize::from(cols.get()), marks, &mut copy)?;
        // Blank rows under the last line, if the lines do not fill the screen.
        while replacement.order.len() < keep_total {
            let slot = replacement.meta.len();
            replacement
                .meta
                .push(Meta::new(next_id(next)?, version, cols.get(), false, 0));
            replacement.order.push_back(slot);
        }
        Ok(replacement)
    }

    /// Lays every retained row out again at `width` columns, one logical
    /// line after another, giving each cell and each finished row to
    /// `target`. `marks` are retained rows and columns, the cursor's and
    /// the saved cursor's, each laid out as the cursor is.
    fn reflow(
        &self,
        width: usize,
        marks: [Option<(usize, usize)>; 2],
        target: &mut impl Reflow,
    ) -> Result<Reflowed, Error> {
        let retained = self.retained_len();
        let blank = Cell::default();
        let no_text = Spill::default();
        let pad = CellRef::new(&blank, &no_text);
        let mut out = Reflowed {
            rows: 0,
            marks: [(0, 0); 2],
            trailing_blank: 0,
        };
        let mut found = [false; 2];
        let mut start = 0;
        while start < retained {
            // A line runs through its wrapped rows to the row that ends it.
            let mut end = start;
            while end.checked_add(1).is_some_and(|next| next < retained)
                && self.row_at(end).is_some_and(|r| r.wrapped)
            {
                end = end.saturating_add(1);
            }
            let line = (start..=end).filter_map(|i| self.row_at(i));
            // Its length without the blank tail, where the cursor is in it,
            // and the spacers in it: the blank a reflow left at the end of a
            // row when a wide glyph did not fit, which is no part of the text.
            let mut length = 0usize;
            let mut offsets = [None; 2];
            let mut spacers = Vec::new();
            // Where each row a prompt starts on begins in the line: the row
            // its first cell goes to is where the prompt starts after.
            let mut prompts = Vec::new();
            for (i, row) in (start..=end).zip(line.clone()) {
                if row.prompt {
                    prompts.push(length);
                }
                for (offset, mark) in offsets.iter_mut().zip(marks) {
                    if let Some((row_index, col)) = mark
                        && row_index == i
                    {
                        *offset = length.checked_add(col);
                    }
                }
                if i != end
                    && row.cells.last().is_some_and(|c| c.same(&blank))
                    && self
                        .row_at(i.saturating_add(1))
                        .and_then(|next| next.cells.first())
                        .is_some_and(Cell::is_wide)
                    && let Some(at) = length
                        .checked_add(row.cells.len())
                        .and_then(|end| end.checked_sub(1))
                {
                    spacers.push(at);
                }
                let used = if i == end {
                    row.cells
                        .iter()
                        .rposition(|c| !c.same(&blank))
                        .map_or(0, |last| last.saturating_add(1))
                } else {
                    row.cells.len()
                };
                length = length.saturating_add(used);
                if i == end && used == 0 {
                    // The tail runs back over earlier rows' blanks too.
                    length = self.trimmed(start, end);
                }
            }
            let first = out.rows;
            let mut used = 0usize;
            let mut placed = [false; 2];
            let ids = line.clone().map(|r| r.id);
            let mut ids = ids.fuse();
            // A prompt starts on the row being laid out, or on the row the
            // next cell placed goes to.
            let (mut prompt, mut pending) = (false, false);
            let cells = line.flat_map(|r| {
                let links = r.links;
                r.cells().enumerate().map(move |(i, cell)| {
                    let link = links.and_then(|l| l.get(i)).copied().unwrap_or(0);
                    (cell, link)
                })
            });
            for (n, (cell, link)) in cells.take(length).enumerate() {
                pending |= prompts.contains(&n);
                if spacers.contains(&n) {
                    // A cursor on a spacer goes with the glyph after it.
                    for offset in &mut offsets {
                        if *offset == Some(n) {
                            *offset = n.checked_add(1);
                        }
                    }
                    continue;
                }
                if width < 2 && (cell.is_wide() || cell.is_wide_continuation()) {
                    // A wide glyph cannot be drawn in one column.
                    continue;
                }
                let full = used >= width;
                let padded = !full && cell.is_wide() && used.saturating_add(1) == width;
                if full || padded {
                    if padded {
                        target.cell(out.rows, used, pad, 0);
                    }
                    target.row(out.rows, true, ids.next(), prompt)?;
                    out.rows = out.rows.saturating_add(1);
                    used = 0;
                    prompt = false;
                }
                for ((offset, mark), placed) in offsets.iter().zip(&mut out.marks).zip(&mut placed)
                {
                    if *offset == Some(n) {
                        *mark = (out.rows, used);
                        *placed = true;
                    }
                }
                prompt |= pending;
                pending = false;
                target.cell(out.rows, used, cell, link);
                used = used.saturating_add(1);
            }
            // A prompt starting past the line's text starts on its last row.
            prompt |= pending || prompts.iter().any(|at| *at >= length);
            for (((offset, mark), placed), found) in offsets
                .iter()
                .zip(&mut out.marks)
                .zip(&mut placed)
                .zip(&mut found)
            {
                if let Some(offset) = offset
                    && !*placed
                {
                    // At or past the end of the line's text: as far past it
                    // on the last row, at most waiting to wrap after the
                    // last column.
                    let past = offset.saturating_sub(length);
                    *mark = (out.rows, used.saturating_add(past).min(width));
                    *placed = true;
                }
                *found |= *placed;
            }
            target.row(out.rows, false, ids.next(), prompt)?;
            out.rows = out.rows.saturating_add(1);
            out.trailing_blank = if length == 0 {
                out.trailing_blank
                    .saturating_add(out.rows.saturating_sub(first))
            } else {
                0
            };
            start = end.saturating_add(1);
        }
        for (mark, found) in out.marks.iter_mut().zip(found) {
            if !found {
                *mark = (out.rows.saturating_sub(1), 0);
            }
        }
        Ok(out)
    }

    /// How many cells of the line from row `start` through `end` come before
    /// its blank tail, which can run back over several rows.
    fn trimmed(&self, start: usize, end: usize) -> usize {
        let blank = Cell::default();
        let mut length = 0usize;
        let mut kept = 0usize;
        for row in (start..=end).filter_map(|i| self.row_at(i)) {
            for cell in row.cells {
                length = length.saturating_add(1);
                if !cell.same(&blank) {
                    kept = length;
                }
            }
        }
        kept
    }

    /// Starts the grid again, as `new` makes it. A grid that holds only its
    /// live rows, in storage made for them, keeps that storage: its rows are
    /// blanked and given new identities, top to bottom, as `new` gives them.
    pub fn clear(&mut self, next: &mut u64, version: u64) -> Result<(), Error> {
        if !self.recyclable() {
            // The links are kept, as a link the program has open keeps its
            // number (`Screen::pen_link`).
            let mut grid = Self::new(
                self.rows.get(),
                self.cols.get(),
                self.history_limit,
                next,
                version,
            )?;
            grid.adopt_links(std::mem::take(&mut self.links));
            *self = grid;
            return Ok(());
        }
        // `new` would run out of identities partway, having taken those
        // before; the rows are left as they were.
        if next.checked_add(u64::from(self.rows.get())).is_none() {
            *next = u64::MAX;
            return Err(Error::IdentityExhausted);
        }
        self.renew(next, version)
    }

    /// Whether `clear` can start the grid again in the storage it has: it
    /// holds its live rows alone, each as wide as the grid, in storage that
    /// `new` would make no smaller.
    pub fn recyclable(&self) -> bool {
        let rows = usize::from(self.rows.get());
        let cols = usize::from(self.cols.get());
        self.order.len() == rows
            && self.meta.len() == rows
            && self.stride == cols
            && self.cells.capacity() == rows.saturating_mul(cols)
    }

    /// Blanks every row of a recyclable grid and gives each a new identity,
    /// top to bottom; the cursors, origin and margins are reset. `next` must
    /// have identities enough.
    fn renew(&mut self, next: &mut u64, version: u64) -> Result<(), Error> {
        for index in 0..self.order.len() {
            let id = next_id(next)?;
            if let Some(&slot) = self.order.get(index) {
                self.recycle(slot, id, version);
            }
        }
        self.cursor = (0, 0);
        self.pending_wrap = false;
        self.saved_cursor = (0, 0);
        self.saved_pending_wrap = false;
        self.origin = false;
        self.saved_origin = false;
        self.top = 0;
        self.bottom = self.rows.last();
        Ok(())
    }

    /// The cursor and what goes with it: pending wrap, origin mode, the
    /// margins and the saved cursor.
    pub fn clone_cursor(&self) -> CursorState {
        CursorState {
            cursor: self.cursor,
            pending_wrap: self.pending_wrap,
            origin: self.origin,
            margins: (self.top, self.bottom),
            saved: (
                self.saved_cursor,
                self.saved_pending_wrap,
                self.saved_origin,
            ),
        }
    }
    /// Puts back what `clone_cursor` took, on a grid of the same size.
    pub fn set_cursor(&mut self, state: CursorState) {
        self.cursor = state.cursor;
        self.pending_wrap = state.pending_wrap;
        self.origin = state.origin;
        (self.top, self.bottom) = state.margins;
        (
            self.saved_cursor,
            self.saved_pending_wrap,
            self.saved_origin,
        ) = state.saved;
    }
    /// Whether every slot's cells from `used` to its width are blank, as
    /// recycling relies on.
    #[cfg(test)]
    pub fn blank_past_used(&self) -> bool {
        (0..self.meta.len()).all(|slot| {
            let used = self.meta.get(slot).map_or(0, |m| usize::from(m.used));
            self.slice(slot)
                .get(used..)
                .is_some_and(|tail| tail.iter().all(|c| *c == Cell::default()))
        })
    }
    pub fn in_region(&self) -> bool {
        (self.top..=self.bottom).contains(&self.cursor.0)
    }
    /// Where the next glyph goes, before any wrap: the cursor's column, or
    /// one past the last column while a wrap is pending.
    pub fn next_column(&self) -> u16 {
        past(self.cursor.1, self.pending_wrap)
    }
    /// Puts the cursor in column `col` of its row; one past the last
    /// column is the last column with a wrap pending.
    pub fn advance_to(&mut self, col: u16) {
        self.pending_wrap = col >= self.cols.get();
        self.cursor.1 = col.min(self.cols.last());
    }
    /// The cursor's line as CUP addresses it: from the top margin in
    /// origin mode.
    pub fn cursor_line(&self) -> u16 {
        if self.origin {
            self.cursor.0.saturating_sub(self.top)
        } else {
            self.cursor.0
        }
    }
    /// CUP: moves the cursor, within the margins in origin mode. Like every
    /// cursor movement, it ends a pending wrap.
    pub fn position(&mut self, row: u16, col: u16) {
        self.pending_wrap = false;
        self.cursor = if self.origin {
            (
                row.saturating_add(self.top).min(self.bottom).max(self.top),
                col.min(self.cols.last()),
            )
        } else {
            (row.min(self.rows.last()), col.min(self.cols.last()))
        };
    }
}

/// The column one past `col` if a wrap is pending there, else `col`.
fn past(col: u16, pending_wrap: bool) -> u16 {
    col.saturating_add(u16::from(pending_wrap))
}

/// Where a reflow's rows go: `Layout` only counts them; `Copy` writes the
/// ones kept into the replacement grid.
trait Reflow {
    /// `cell`, with link `link`, is at `col` of reflowed row `row`.
    fn cell(&mut self, row: usize, col: usize, cell: CellRef<'_>, link: u16);
    /// Reflowed row `row` is finished; `wrapped` if its line goes on, the
    /// identity of its line's row in the same place before, if any, and
    /// whether a prompt starts on it.
    fn row(
        &mut self,
        row: usize,
        wrapped: bool,
        id: Option<RowId>,
        prompt: bool,
    ) -> Result<(), Error>;
}

/// The shape of a reflow: how many rows, where the cursor and the saved
/// cursor are, and how many rows at the end hold blank lines.
struct Reflowed {
    rows: usize,
    marks: [(usize, usize); 2],
    trailing_blank: usize,
}

struct Layout;
impl Reflow for Layout {
    fn cell(&mut self, _: usize, _: usize, _: CellRef<'_>, _: u16) {}
    fn row(&mut self, _: usize, _: bool, _: Option<RowId>, _: bool) -> Result<(), Error> {
        Ok(())
    }
}

/// Writes reflowed rows `rows` into `grid`, whose cells and texts are
/// allocated and blank, one slot a row in order. A cluster held in its old
/// row's text is stored again in its new row's.
struct Copy<'a> {
    grid: &'a mut Grid,
    rows: Range<usize>,
    next: &'a mut u64,
    version: u64,
}
impl Reflow for Copy<'_> {
    fn cell(&mut self, row: usize, col: usize, cell: CellRef<'_>, link: u16) {
        let stride = self.grid.stride;
        if !self.rows.contains(&row) || col >= stride {
            return;
        }
        let Some(slot) = row.checked_sub(self.rows.start) else {
            return;
        };
        let Some(start) = slot.checked_mul(stride) else {
            return;
        };
        if link != 0
            && let Some(at) = self
                .grid
                .linked
                .entry(slot)
                .or_insert_with(|| vec![0; stride].into_boxed_slice())
                .get_mut(col)
        {
            *at = link;
        }
        let stored = *cell.stored();
        if !stored.is_spilled() {
            if let Some(target) = start
                .checked_add(col)
                .and_then(|at| self.grid.cells.get_mut(at))
            {
                *target = stored;
            }
            return;
        }
        let cells = start
            .checked_add(stride)
            .and_then(|end| self.grid.cells.get_mut(start..end));
        if let (Some(cells), Some(spill)) = (cells, self.grid.spill.get_mut(slot)) {
            Line { cells, spill }.set(col, stored, cell.contents());
        }
    }
    fn row(
        &mut self,
        row: usize,
        wrapped: bool,
        id: Option<RowId>,
        prompt: bool,
    ) -> Result<(), Error> {
        if !self.rows.contains(&row) {
            return Ok(());
        }
        let id = match id {
            Some(id) => id,
            None => next_id(self.next)?,
        };
        let slot = self.grid.meta.len();
        let cols = self.grid.cols.get();
        let mut meta = Meta::new(id, self.version, cols, wrapped, cols);
        meta.prompt = prompt;
        meta.linked = self.grid.linked.contains_key(&slot);
        self.grid.meta.push(meta);
        self.grid.order.push_back(slot);
        repair_wide(self.grid.slice_mut(slot));
        Ok(())
    }
}

/// The rows' links, by slot.
pub(crate) type Linked = HashMap<usize, Box<[u16]>, BuildHasherDefault<SlotHasher>>;

/// Hashes a slot, a small number, by one multiplication (Fibonacci
/// hashing), rather than by SipHash, which the default hasher spends on
/// every lookup: a slot is no input a program chooses.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SlotHasher(u64);

impl Hasher for SlotHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.0 = self.0.rotate_left(8) ^ u64::from(byte);
        }
        self.0 = self.0.wrapping_mul(0x9e37_79b9_7f4a_7c15);
    }
    fn write_usize(&mut self, n: usize) {
        let n = u64::try_from(n).unwrap_or(u64::MAX);
        self.0 = (self.0 ^ n).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    }
}

/// Drops slot `slot`'s links, which its cells no longer have. Out of line,
/// so that the paths that recycle and erase rows, which only call it for a
/// row with links, stay as small as they were before links.
#[cold]
#[inline(never)]
fn forget(linked: &mut Linked, links: &mut Links, slot: usize) {
    if let Some(row) = linked.remove(&slot) {
        links.release_all(&row);
    }
}

/// ICH and DCH on a row's cells, or on their links: what is at and after
/// `col` turns `count` places right to insert, left to delete, and the
/// places it leaves are `blank`.
fn shift<T: std::marker::Copy>(cells: &mut [T], col: usize, count: usize, insert: bool, blank: T) {
    let Some(tail) = cells.get_mut(col..) else {
        return;
    };
    // `rotate_*` panics past the end of what it turns: the clamp is the
    // reason it is allowed here (clippy.toml), and costs nothing, as
    // `count` is at most the cells from the cursor to the edge.
    let count = count.min(tail.len());
    let fill = if insert {
        tail.rotate_right(count);
        0..count
    } else {
        tail.rotate_left(count);
        tail.len().saturating_sub(count)..tail.len()
    };
    if let Some(run) = tail.get_mut(fill) {
        run.fill(blank);
    }
}

pub(crate) fn repair_wide(cells: &mut [Cell]) {
    for i in 0..cells.len() {
        let invalid = cells.get(i).is_some_and(|c| {
            c.is_wide()
                && !i
                    .checked_add(1)
                    .and_then(|j| cells.get(j))
                    .is_some_and(Cell::is_wide_continuation)
                || c.is_wide_continuation()
                    && !i
                        .checked_sub(1)
                        .and_then(|j| cells.get(j))
                        .is_some_and(Cell::is_wide)
        });
        if invalid && let Some(cell) = cells.get_mut(i) {
            *cell = Cell::blank(cell.attributes);
        }
    }
}

#[cfg(test)]
mod tests;
