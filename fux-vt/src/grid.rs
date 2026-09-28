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
    /// Edits a live row's cells with `f`, which says whether it changed any
    /// of them; only then does the row take `version`. An edit that leaves
    /// the row as it was leaves its version alone.
    pub fn mutate_row(&mut self, row: u16, version: u64, f: impl FnOnce(&mut [Cell]) -> bool) {
        if let Some(slot) = self.slot(row)
            && f(self.slice_mut(slot))
            && let Some(m) = self.meta.get_mut(slot)
        {
            m.version = version;
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
            // The tail rotates `count` cells right to insert, left to delete.
            // `rotate_*` panics past the end of what it turns: the clamp is
            // the reason it is allowed here (clippy.toml), and costs nothing,
            // as `count` is at most the cells from the cursor to the edge.
            if let Some(tail) = cells.get_mut(col..) {
                let count = count.min(tail.len());
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
            // Inserting and deleting always count as a change.
            true
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
                if let Some(slot) = self.move_row(from, to) {
                    self.recycle(slot, id, version);
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
        let cursor = self
            .history_len()
            .checked_add(usize::from(self.cursor.0))
            .map(|row| (row, usize::from(self.cursor.1)));
        let layout = self.reflow(usize::from(cols.get()), cursor, &mut Layout)?;
        // Blank lines below the cursor are dropped before any line scrolls
        // into history: a mostly empty screen keeps its text on screen.
        let screen = usize::from(rows.get());
        let (cursor_row, cursor_col) = layout.cursor;
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
        let mut replacement = Self {
            cells: Vec::new(),
            meta: Vec::new(),
            order: VecDeque::new(),
            stride: usize::from(cols.get()),
            rows,
            cols,
            history_limit: self.history_limit,
            // The cursor's row is on screen: `live_top` is at most its row,
            // and the screen reaches past it.
            cursor: (
                u16::try_from(cursor_row.saturating_sub(live_top))
                    .map_or(rows.last(), |row| row.min(rows.last())),
                u16::try_from(cursor_col).map_or(cols.get(), |col| col.min(cols.get())),
            ),
            saved_cursor: (
                self.saved_cursor.0.min(rows.last()),
                self.saved_cursor.1.min(cols.last()),
            ),
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
        let mut copy = Copy {
            grid: &mut replacement,
            rows: base..end,
            next,
            version,
        };
        self.reflow(usize::from(cols.get()), cursor, &mut copy)?;
        // Blank rows under the last line, if the lines do not fill the screen.
        while replacement.order.len() < keep_total {
            let slot = replacement.meta.len();
            replacement.meta.push(Meta {
                id: next_id(next)?,
                version,
                width: cols.get(),
                wrapped: false,
            });
            replacement.order.push_back(slot);
        }
        Ok(replacement)
    }

    /// Lays every retained row out again at `width` columns, one logical
    /// line after another, giving each cell and each finished row to
    /// `target`. `cursor` is the retained row and column of the cursor.
    fn reflow(
        &self,
        width: usize,
        cursor: Option<(usize, usize)>,
        target: &mut impl Reflow,
    ) -> Result<Reflowed, Error> {
        let retained = self.retained_len();
        let blank = Cell::default();
        let mut out = Reflowed {
            rows: 0,
            cursor: (0, 0),
            trailing_blank: 0,
        };
        let mut found = false;
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
            // Its length without the blank tail, and where the cursor is in it.
            let mut length = 0usize;
            let mut offset = None;
            for (i, row) in (start..=end).zip(line.clone()) {
                if let Some((row_index, col)) = cursor
                    && row_index == i
                {
                    offset = length.checked_add(col);
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
            let mut placed = false;
            let ids = line.clone().map(|r| r.id);
            let mut ids = ids.fuse();
            for (n, cell) in line.flat_map(|r| r.cells.iter()).take(length).enumerate() {
                if width < 2 && (cell.is_wide() || cell.is_wide_continuation()) {
                    // A wide glyph cannot be drawn in one column.
                    continue;
                }
                let full = used >= width;
                let pad = !full && cell.is_wide() && used.saturating_add(1) == width;
                if full || pad {
                    if pad {
                        target.cell(out.rows, used, blank);
                    }
                    target.row(out.rows, true, ids.next())?;
                    out.rows = out.rows.saturating_add(1);
                    used = 0;
                }
                if offset == Some(n) {
                    out.cursor = (out.rows, used);
                    placed = true;
                }
                target.cell(out.rows, used, *cell);
                used = used.saturating_add(1);
            }
            if let Some(offset) = offset
                && !placed
            {
                // At or past the end of the line's text: as far past it on
                // the last row, at most waiting to wrap after the last column.
                let past = offset.saturating_sub(length);
                out.cursor = (out.rows, used.saturating_add(past).min(width));
                placed = true;
            }
            found |= placed;
            target.row(out.rows, false, ids.next())?;
            out.rows = out.rows.saturating_add(1);
            out.trailing_blank = if length == 0 {
                out.trailing_blank
                    .saturating_add(out.rows.saturating_sub(first))
            } else {
                0
            };
            start = end.saturating_add(1);
        }
        if !found {
            out.cursor = (out.rows.saturating_sub(1), 0);
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
            *self = Self::new(
                self.rows.get(),
                self.cols.get(),
                self.history_limit,
                next,
                version,
            )?;
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
        self.saved_cursor = (0, 0);
        self.origin = false;
        self.saved_origin = false;
        self.top = 0;
        self.bottom = self.rows.last();
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

/// Where a reflow's rows go: `Layout` only counts them; `Copy` writes the
/// ones kept into the replacement grid.
trait Reflow {
    /// `cell` is at `col` of reflowed row `row`.
    fn cell(&mut self, row: usize, col: usize, cell: Cell);
    /// Reflowed row `row` is finished; `wrapped` if its line goes on, and
    /// the identity of its line's row in the same place before, if any.
    fn row(&mut self, row: usize, wrapped: bool, id: Option<RowId>) -> Result<(), Error>;
}

/// The shape of a reflow: how many rows, where the cursor is, and how many
/// rows at the end hold blank lines.
struct Reflowed {
    rows: usize,
    cursor: (usize, usize),
    trailing_blank: usize,
}

struct Layout;
impl Reflow for Layout {
    fn cell(&mut self, _: usize, _: usize, _: Cell) {}
    fn row(&mut self, _: usize, _: bool, _: Option<RowId>) -> Result<(), Error> {
        Ok(())
    }
}

/// Writes reflowed rows `rows` into `grid`, whose cells are allocated and
/// blank, one slot a row in order.
struct Copy<'a> {
    grid: &'a mut Grid,
    rows: Range<usize>,
    next: &'a mut u64,
    version: u64,
}
impl Reflow for Copy<'_> {
    fn cell(&mut self, row: usize, col: usize, cell: Cell) {
        if self.rows.contains(&row)
            && let Some(slot) = row.checked_sub(self.rows.start)
            && let Some(at) = slot
                .checked_mul(self.grid.stride)
                .and_then(|start| start.checked_add(col))
            && col < self.grid.stride
            && let Some(target) = self.grid.cells.get_mut(at)
        {
            *target = cell;
        }
    }
    fn row(&mut self, row: usize, wrapped: bool, id: Option<RowId>) -> Result<(), Error> {
        if !self.rows.contains(&row) {
            return Ok(());
        }
        let id = match id {
            Some(id) => id,
            None => next_id(self.next)?,
        };
        let slot = self.grid.meta.len();
        self.grid.meta.push(Meta {
            id,
            version: self.version,
            width: self.grid.cols.get(),
            wrapped,
        });
        self.grid.order.push_back(slot);
        repair_wide(self.grid.slice_mut(slot));
        Ok(())
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
