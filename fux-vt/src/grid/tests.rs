use super::*;

/// Up, the departing rows going into history.
const UP: Scroll = Scroll::Up { history: true };

/// Heap bytes; a Vec's allocation cannot exceed `isize::MAX`, so no product
/// saturates.
fn heap(grid: &Grid) -> [usize; 3] {
    [
        grid.cells.capacity().saturating_mul(size_of::<Cell>()),
        grid.meta.capacity().saturating_mul(size_of::<Meta>()),
        grid.order.capacity().saturating_mul(size_of::<usize>()),
    ]
}
#[test]
fn measured_storage_plateau_and_transactional_resize_peak_include_metadata() -> Result<(), Error> {
    let mut next = 0;
    let mut primary = Grid::new(24, 80, 10_000, &mut next, 0)?;
    let alternate = Grid::new(24, 80, 0, &mut next, 0)?;
    let initial_primary = heap(&primary);
    let initial_alternate = heap(&alternate);
    for version in 1..=10_000 {
        primary.scroll((0, 23), 1, UP, Attributes::default(), &mut next, version)?;
    }
    let plateau = heap(&primary);
    for version in 10_001..=20_000 {
        primary.scroll((0, 23), 1, UP, Attributes::default(), &mut next, version)?;
    }
    assert_eq!(heap(&primary), plateau);
    assert_eq!(primary.history_len(), 10_000);
    let resized_primary = primary.resized(60, 120, &mut next, 20_001)?;
    let resized_alternate = alternate.resized(60, 120, &mut next, 20_001)?;
    let new_primary = heap(&resized_primary);
    let new_alternate = heap(&resized_alternate);
    // Screen::resize constructs BOTH replacements before assigning either.
    // These are actual reserved vector capacities, not just live cell counts.
    let old_heap = plateau.iter().sum::<usize>() + initial_alternate.iter().sum::<usize>();
    let new_heap = new_primary.iter().sum::<usize>() + new_alternate.iter().sum::<usize>();
    let screen_bytes = std::mem::size_of::<crate::Screen>();
    let peak_reserved = old_heap + new_heap + screen_bytes + 2 * std::mem::size_of::<Grid>();
    assert_eq!(std::mem::size_of::<Cell>(), 32);
    println!(
        "MEMORY-BOUNDS {{\"components\":[\"cells\",\"row_metadata\",\"slot_order\"],\"initial_primary\":{initial_primary:?},\"initial_alternate\":{initial_alternate:?},\"plateau_primary\":{plateau:?},\"resized_primary\":{new_primary:?},\"resized_alternate\":{new_alternate:?},\"screen_object_bytes\":{screen_bytes},\"steady_reserved_bytes\":{},\"resize_peak_reserved_bytes\":{peak_reserved},\"scrolls\":20000}}",
        old_heap + screen_bytes
    );
    Ok(())
}

// Narrowing a pane whose rows all stay live must not keep the old width as the
// storage stride: that multiplied every later copy of the grid by the widest
// width it ever had, and doubled the smoke's 200-pane `scale` run.
#[test]
fn narrowing_live_rows_uses_the_new_width_as_stride() -> Result<(), Error> {
    let mut next = 0;
    let wide = Grid::new(24, 400, 100, &mut next, 0)?;
    let narrow = wide.resized(23, 10, &mut next, 1)?;
    assert_eq!(narrow.history_len(), 0);
    assert_eq!(narrow.stride, 10);
    Ok(())
}

/// `move_row` is `remove(from)` then `insert(to)`, for every pair of rows,
/// in a deque that is one slice and in one that has wrapped round into two;
/// and it moves nothing for an index out of range.
#[test]
fn moving_a_row_is_a_removal_then_an_insertion() -> Result<(), Error> {
    let mut next = 0;
    let contiguous = Grid::new(5, 1, 0, &mut next, 0)?;
    let mut wrapped = Grid::new(5, 1, 4, &mut next, 0)?;
    for version in 1..=11 {
        wrapped.scroll((0, 4), 1, UP, Attributes::default(), &mut next, version)?;
    }
    let (front, back) = wrapped.order.as_slices();
    assert!(!front.is_empty() && !back.is_empty(), "the deque wraps");
    moves_are_removals_then_insertions(&contiguous);
    moves_are_removals_then_insertions(&wrapped);
    Ok(())
}

fn moves_are_removals_then_insertions(grid: &Grid) {
    let rows: Vec<usize> = grid.order.iter().copied().collect();
    let len = rows.len();
    for from in 0..len {
        for to in 0..len {
            let mut moved = grid.clone();
            assert_eq!(moved.move_row(from, to), rows.get(from).copied());
            let rest = rows.iter().enumerate().filter(|(i, _)| *i != from);
            let mut expected: Vec<usize> = rest.clone().take(to).map(|(_, r)| *r).collect();
            expected.extend(rows.get(from));
            expected.extend(rest.skip(to).map(|(_, r)| *r));
            assert_eq!(
                moved.order.iter().copied().collect::<Vec<_>>(),
                expected,
                "{from} to {to}"
            );
        }
    }
    let mut untouched = grid.clone();
    assert_eq!(untouched.move_row(1, len), None);
    assert_eq!(untouched.move_row(len, 1), None);
    assert_eq!(untouched.order, grid.order);
}

/// What a reader sees of a row: its identity, version, wrap and prompt
/// flags, and each cell as stored, with its text and its link's number.
pub(crate) type Seen = (RowId, u64, bool, bool, Vec<(Cell, String, u16)>);

impl Grid {
    /// `erase` as it was before the `used` mark bounded it: every cell of
    /// the span compared, then every cell written, with the halves of wide
    /// glyphs it splits; the oracle `erase` is checked against.
    pub(crate) fn erase_reference(
        &mut self,
        row: u16,
        start: u16,
        end: u16,
        attributes: Attributes,
        version: u64,
    ) {
        let (cols, last) = (self.cols.get(), self.cols.last());
        let mut clears_edge = end >= cols;
        let written = if attributes == Attributes::default() {
            0
        } else {
            end
        };
        self.mutate_row(row, version, written, |cells| {
            let span = usize::from(start)..usize::from(end.min(cols));
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

    /// What a reader sees of every retained row, in order.
    pub(crate) fn seen(&self) -> Vec<Seen> {
        (0..self.retained_len())
            .filter_map(|i| self.row_at(i))
            .map(|row| {
                let cells = row
                    .cells()
                    .enumerate()
                    .map(|(col, c)| {
                        let link = row.links.and_then(|l| l.get(col)).copied().unwrap_or(0);
                        (*c.stored(), c.contents().to_owned(), link)
                    })
                    .collect();
                (row.id, row.version, row.wrapped, row.prompt, cells)
            })
            .collect()
    }
}
