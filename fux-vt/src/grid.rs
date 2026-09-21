use std::collections::VecDeque;

use crate::{Attributes, Cell, Error, Row, RowId};

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
    pub rows: u16,
    pub cols: u16,
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
    pub fn check_size(rows: u16, cols: u16, history: usize) -> Result<(), Error> {
        if rows == 0 || cols == 0 {
            return Err(Error::ZeroSize);
        }
        let retained = history
            .checked_add(usize::from(rows))
            .ok_or(Error::Capacity)?;
        if retained > MAX_ROWS
            || retained
                .checked_mul(usize::from(cols))
                .is_none_or(|n| n > MAX_CELLS)
        {
            return Err(Error::Capacity);
        }
        Ok(())
    }

    pub fn new(
        rows: u16,
        cols: u16,
        history_limit: usize,
        next: &mut u64,
        version: u64,
    ) -> Result<Self, Error> {
        Self::check_size(rows, cols, history_limit)?;
        let mut grid = Self {
            cells: Vec::new(),
            meta: Vec::new(),
            order: VecDeque::new(),
            stride: usize::from(cols),
            rows,
            cols,
            history_limit,
            cursor: (0, 0),
            saved_cursor: (0, 0),
            origin: false,
            saved_origin: false,
            top: 0,
            bottom: rows - 1,
        };
        grid.reserve_rows(usize::from(rows))?;
        for _ in 0..rows {
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
        if slot == self.meta.capacity() || self.cells.len() + self.stride > self.cells.capacity() {
            let maximum = self.history_limit + usize::from(self.rows);
            let capacity = (slot + 1).saturating_mul(2).min(maximum);
            self.reserve_rows(capacity)?;
        }
        let id = next_id(next)?;
        self.cells
            .resize(self.cells.len() + self.stride, Cell::default());
        self.meta.push(Meta {
            id,
            version,
            width: self.cols,
            wrapped: false,
        });
        Ok(slot)
    }

    pub fn history_len(&self) -> usize {
        self.order.len().saturating_sub(usize::from(self.rows))
    }
    pub fn retained_len(&self) -> usize {
        self.order.len()
    }
    pub fn storage_cells(&self) -> usize {
        self.cells.capacity()
    }

    fn slice(&self, slot: usize) -> &[Cell] {
        let width = self.meta.get(slot).map_or(0, |m| usize::from(m.width));
        let start = slot * self.stride;
        self.cells.get(start..start + width).unwrap_or(&[])
    }
    fn slice_mut(&mut self, slot: usize) -> &mut [Cell] {
        let width = self.meta.get(slot).map_or(0, |m| usize::from(m.width));
        let start = slot * self.stride;
        self.cells.get_mut(start..start + width).unwrap_or(&mut [])
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
    pub fn live_row(&self, row: u16) -> Option<Row<'_>> {
        if row >= self.rows {
            return None;
        }
        self.row_at(self.history_len() + usize::from(row))
    }
    fn slot(&self, row: u16) -> Option<usize> {
        if row >= self.rows {
            return None;
        }
        self.order
            .get(self.history_len() + usize::from(row))
            .copied()
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
        let cols = self.cols;
        let mut clears_edge = end >= cols;
        self.mutate_row(row, version, |cells| {
            for col in usize::from(start)..usize::from(end.min(cols)) {
                if let Some(cell) = cells.get(col).copied() {
                    if cell.is_wide() {
                        if let Some(other) = cells.get_mut(col + 1) {
                            *other = Cell::blank(other.attributes);
                        }
                        clears_edge |= col + 2 == usize::from(cols);
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
        let count = usize::from(count.min(self.cols.saturating_sub(col)));
        if count == 0 {
            return;
        }
        self.mutate_row(row, version, |cells| {
            let col = usize::from(col);
            // Clear a wide glyph straddling either edit boundary before shifting.
            for boundary in [
                col,
                if insert {
                    cells.len() - count
                } else {
                    col + count
                },
            ] {
                if cells.get(boundary).is_some_and(Cell::is_wide_continuation) {
                    if let Some(c) = boundary.checked_sub(1).and_then(|i| cells.get_mut(i)) {
                        *c = Cell::blank(c.attributes);
                    }
                    if let Some(c) = cells.get_mut(boundary) {
                        *c = Cell::blank(c.attributes);
                    }
                }
            }
            let len = cells.len();
            if let Some(tail) = cells.get_mut(col..) {
                if insert {
                    tail.rotate_right(count);
                } else {
                    tail.rotate_left(count);
                }
            }
            let blank = if insert {
                col..col + count
            } else {
                len - count..len
            };
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
                width: self.cols,
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
        if top > bottom || bottom >= self.rows {
            return Ok(());
        }
        let count = count.min(bottom - top + 1);
        for _ in 0..count {
            if up && history && top == 0 && bottom == self.rows - 1 && self.history_limit > 0 {
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
                let offset = self.history_len();
                let from = offset + usize::from(if up { top } else { bottom });
                let to = offset + usize::from(if up { bottom } else { top });
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
        Self::check_size(rows, cols, self.history_limit)?;
        let history = self.history_len();
        let stride = (0..history)
            .filter_map(|i| self.row_at(i))
            .map(|r| r.cells.len())
            .max()
            .unwrap_or(0)
            .max(usize::from(cols));
        let mut replacement = Self {
            cells: Vec::new(),
            meta: Vec::new(),
            order: VecDeque::new(),
            stride,
            rows,
            cols,
            history_limit: self.history_limit,
            cursor: (self.cursor.0.min(rows - 1), self.cursor.1.min(cols - 1)),
            saved_cursor: (
                self.saved_cursor.0.min(rows - 1),
                self.saved_cursor.1.min(cols - 1),
            ),
            origin: self.origin,
            saved_origin: self.saved_origin,
            top: self.top,
            bottom: if self.bottom == self.rows - 1 {
                rows - 1
            } else {
                self.bottom.min(rows - 1)
            },
        };
        if replacement.top > replacement.bottom {
            replacement.top = 0;
        }
        replacement.reserve_rows(history + usize::from(rows))?;
        for i in 0..history + usize::from(rows) {
            let old = self
                .row_at(i)
                .filter(|_| i < history + usize::from(self.rows.min(rows)));
            let id = match old {
                Some(r) => r.id,
                None => next_id(next)?,
            };
            let width = if i < history {
                old.map_or(cols, |r| r.cells.len() as u16)
            } else {
                cols
            };
            let start = replacement.cells.len();
            replacement.cells.resize(start + stride, Cell::default());
            let wrapped = old.is_some_and(|r| r.wrapped) && i < history;
            replacement.meta.push(Meta {
                id,
                version: if i < history {
                    old.map_or(version, |r| r.version)
                } else {
                    version
                },
                width,
                wrapped,
            });
            replacement.order.push_back(i);
            if let Some(old) = old {
                let len = old.cells.len().min(usize::from(width));
                if let (Some(dst), Some(src)) = (
                    replacement.cells.get_mut(start..start + len),
                    old.cells.get(..len),
                ) {
                    dst.copy_from_slice(src);
                }
                repair_wide(replacement.slice_mut(i));
            }
        }
        Ok(replacement)
    }

    pub fn clear(&mut self, next: &mut u64, version: u64) -> Result<(), Error> {
        *self = Self::new(self.rows, self.cols, self.history_limit, next, version)?;
        Ok(())
    }

    /// Temporary diagnostic reproducing the oracle's line edits outside margins.
    #[cfg(feature = "differential")]
    pub fn oracle_edit_lines(
        &mut self,
        count: u16,
        insert: bool,
        next: &mut u64,
        version: u64,
    ) -> Result<(), Error> {
        let row = self.cursor.0;
        let count = if insert {
            count
        } else {
            count.min(self.rows - row)
        };
        for _ in 0..count {
            if !insert && row == self.bottom + 1 {
                continue;
            }
            let (from, to) = if insert {
                (self.bottom, row)
            } else if row <= self.bottom {
                (row, self.bottom)
            } else {
                (row - 1, self.bottom + 1)
            };
            let id = next_id(next)?;
            let offset = self.history_len();
            if let Some(slot) = self.order.remove(offset + usize::from(from)) {
                self.recycle(slot, id, version);
                self.order.insert(offset + usize::from(to), slot);
            }
            if insert {
                self.wrap(self.bottom, false, version);
            }
        }
        Ok(())
    }

    pub fn in_region(&self) -> bool {
        (self.top..=self.bottom).contains(&self.cursor.0)
    }
    pub fn position(&mut self, row: u16, col: u16) {
        self.cursor = if self.origin {
            (
                row.saturating_add(self.top).min(self.bottom).max(self.top),
                col.min(self.cols - 1),
            )
        } else {
            (row.min(self.rows - 1), col.min(self.cols - 1))
        };
    }
}

pub(crate) fn repair_wide(cells: &mut [Cell]) {
    for i in 0..cells.len() {
        let invalid = cells.get(i).is_some_and(|c| {
            c.is_wide() && !cells.get(i + 1).is_some_and(Cell::is_wide_continuation)
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
