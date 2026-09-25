use std::collections::VecDeque;
use std::num::NonZeroU16;
use std::ops::Range;

use crate::{Attributes, Cell, Error, Row, RowId};

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

/// Maximum addressable retained cells per buffer (2 GiB at 32 bytes/cell).
/// Storage is committed only for live/retained rows, not empty history slots.
pub(crate) const MAX_CELLS: usize = 64 * 1024 * 1024;
pub(crate) const MAX_ROWS: usize = 1_048_576;

#[derive(Clone, Copy, Debug)]
struct Meta {
    id: RowId,
    version: u64,
    width: u16,
    wrapped: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct Grid {
    cells: Vec<Cell>,
    meta: Vec<Meta>,
    order: VecDeque<usize>,
    stride: usize,
    pub rows: Extent,
    pub cols: Extent,
    pub history_limit: usize,
    pub cursor: (u16, u16),
    pub saved_cursor: (u16, u16),
    pub origin: bool,
    pub saved_origin: bool,
    pub top: u16,
    pub bottom: u16,
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
            order: VecDeque::new(),
            stride: usize::from(cols.get()),
            rows,
            cols,
            history_limit,
            cursor: (0, 0),
            saved_cursor: (0, 0),
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
        self.meta.push(Meta {
            id,
            version,
            width: self.cols.get(),
            wrapped: false,
        });
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
    pub fn row_at(&self, index: usize) -> Option<Row<'_>> {
        let slot = *self.order.get(index)?;
        let m = self.meta.get(slot)?;
        Some(Row {
            id: m.id,
            version: m.version,
            wrapped: m.wrapped,
            cells: self.slice(slot),
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
    pub fn cell(&self, row: u16, col: u16) -> Option<&Cell> {
        self.live_row(row)?.cells.get(usize::from(col))
    }
    pub fn mutate_row(&mut self, row: u16, version: u64, f: impl FnOnce(&mut [Cell])) {
        if let Some(slot) = self.slot(row) {
            if let Some(m) = self.meta.get_mut(slot) {
                m.version = version;
            }
            f(self.slice_mut(slot));
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
        self.mutate_row(row, version, |cells| {
            for col in usize::from(start)..usize::from(end.min(cols)) {
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
                        *cell = Cell::blank(attributes);
                    }
                }
            }
        });
        if clears_edge {
            self.wrap(row, false, version);
        }
    }

    pub fn edit_cells(&mut self, count: u16, insert: bool, version: u64) {
        let (row, col) = self.cursor;
        // At most the cells from the cursor to the edge.
        let count = usize::from(count.min(self.cols.get().saturating_sub(col)));
        if count == 0 {
            return;
        }
        self.mutate_row(row, version, |cells| {
            let col = usize::from(col);
            let len = cells.len();
            // Where the cells shifted out start, and so where blanks go.
            let (Some(shifted), Some(blank)) = (
                if insert {
                    len.checked_sub(count)
                } else {
                    col.checked_add(count)
                },
                if insert {
                    col.checked_add(count).map(|end| col..end)
                } else {
                    len.checked_sub(count).map(|start| start..len)
                },
            ) else {
                return;
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
            if let Some(tail) = cells.get_mut(col..) {
                if insert {
                    tail.rotate_right(count);
                } else {
                    tail.rotate_left(count);
                }
            }
            if let Some(cells) = cells.get_mut(blank) {
                cells.fill(Cell::default());
            }
            repair_wide(cells);
        });
        self.wrap(row, false, version);
    }

    fn recycle(&mut self, slot: usize, id: RowId, version: u64) {
        if let Some(m) = self.meta.get_mut(slot) {
            *m = Meta {
                id,
                version,
                width: self.cols.get(),
                wrapped: false,
            };
        }
        self.slice_mut(slot).fill(Cell::default());
    }

    /// Moves slots, not cells. Only whole-screen upward scrolling enters history.
    pub fn scroll(
        &mut self,
        (top, bottom): (u16, u16),
        count: u16,
        up: bool,
        history: bool,
        next: &mut u64,
        version: u64,
    ) -> Result<(), Error> {
        if bottom >= self.rows.get() {
            return Ok(());
        }
        let Some(height) = bottom.checked_sub(top).and_then(|h| h.checked_add(1)) else {
            return Ok(());
        };
        let count = count.min(height);
        for _ in 0..count {
            if up && history && top == 0 && bottom == self.rows.last() && self.history_limit > 0 {
                if self.history_len() < self.history_limit {
                    let slot = self.allocate(next, version)?;
                    self.order.push_back(slot);
                } else {
                    let id = next_id(next)?;
                    if let Some(slot) = self.order.pop_front() {
                        self.recycle(slot, id, version);
                        self.order.push_back(slot);
                    }
                }
            } else {
                let id = next_id(next)?;
                let (from, to) = if up { (top, bottom) } else { (bottom, top) };
                let (Some(from), Some(to)) = (self.index(from), self.index(to)) else {
                    return Ok(());
                };
                if let Some(slot) = self.order.remove(from) {
                    self.recycle(slot, id, version);
                    self.order.insert(to, slot);
                }
                if !up {
                    self.wrap(bottom, false, version);
                }
            }
        }
        Ok(())
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
            order: VecDeque::new(),
            stride,
            rows,
            cols,
            history_limit: self.history_limit,
            cursor: (shifted(self.cursor.0), self.cursor.1.min(cols.last())),
            saved_cursor: (
                shifted(self.saved_cursor.0),
                self.saved_cursor.1.min(cols.last()),
            ),
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
            replacement.meta.push(Meta {
                id,
                version: if is_history {
                    old.map_or(version, |r| r.version)
                } else {
                    version
                },
                width,
                wrapped,
            });
            replacement.order.push_back(p);
            if let Some(old) = old {
                let len = old.cells.len().min(usize::from(width));
                if let (Some(dst), Some(src)) = (
                    start
                        .checked_add(len)
                        .and_then(|end| replacement.cells.get_mut(start..end)),
                    old.cells.get(..len),
                ) {
                    dst.copy_from_slice(src);
                }
                repair_wide(replacement.slice_mut(p));
            }
        }
        Ok(replacement)
    }

    pub fn clear(&mut self, next: &mut u64, version: u64) -> Result<(), Error> {
        *self = Self::new(
            self.rows.get(),
            self.cols.get(),
            self.history_limit,
            next,
            version,
        )?;
        Ok(())
    }

    pub fn in_region(&self) -> bool {
        (self.top..=self.bottom).contains(&self.cursor.0)
    }
    pub fn position(&mut self, row: u16, col: u16) {
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
