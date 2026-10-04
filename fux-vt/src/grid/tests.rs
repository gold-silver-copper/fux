use super::*;

/// Up, the departing rows going into history.
const UP: Scroll = Scroll::Up { history: true };

/// Heap bytes; a Vec's allocation cannot exceed `isize::MAX`, so no product
/// saturates.
fn heap(grid: &Grid) -> [usize; 3] {
    [
        grid.cells.capacity().saturating_mul(size_of::<Compact>()),
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
        primary.scroll((0, 23), 1, UP, 0, &mut next, version)?;
    }
    let plateau = heap(&primary);
    for version in 10_001..=20_000 {
        primary.scroll((0, 23), 1, UP, 0, &mut next, version)?;
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
    assert_eq!(std::mem::size_of::<Compact>(), 8);
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
        wrapped.scroll((0, 4), 1, UP, 0, &mut next, version)?;
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
/// flags, and each cell as stored, with its attributes, its text and its
/// link's number.
pub(crate) type Seen = (
    RowId,
    u64,
    bool,
    bool,
    Vec<(Compact, Attributes, String, u16)>,
);

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
        let style = self.style(attributes);
        self.mutate_row(row, version, written, |cells| {
            let span = usize::from(start)..usize::from(end.min(cols));
            let blank = Compact::blank(style);
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
                            *other = other.blanked();
                        }
                        clears_edge |= next == Some(usize::from(last));
                    } else if cell.is_wide_continuation()
                        && let Some(other) = col.checked_sub(1).and_then(|i| cells.get_mut(i))
                    {
                        *other = other.blanked();
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
                    .cells
                    .iter()
                    .enumerate()
                    .map(|(col, c)| {
                        let link = row.links.and_then(|l| l.get(col)).copied().unwrap_or(0);
                        let read = c.read(row.spill, row.styles);
                        (*c, read.attributes(), read.contents().to_owned(), link)
                    })
                    .collect();
                (row.id, row.version, row.wrapped, row.prompt, cells)
            })
            .collect()
    }
}

/// A small deterministic generator (splitmix64), so a failure names its case.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut x = self.0;
        x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        x ^ (x >> 31)
    }
    /// Below `n`, or 0 for an `n` of 0.
    fn below(&mut self, n: usize) -> usize {
        let n = u64::try_from(n).unwrap_or(u64::MAX);
        let value = self.next().checked_rem(n).unwrap_or(0);
        usize::try_from(value).unwrap_or(0)
    }
    /// Below `n`, as a `u16`.
    fn small(&mut self, n: u16) -> u16 {
        u16::try_from(self.below(usize::from(n))).unwrap_or(0)
    }
    fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }
}

/// Output leaving what a reflow keeps or moves: text, wide glyphs, clusters
/// too long to hold inline, soft wraps, blank and coloured tails, erased
/// rows, links, prompts, the saved cursor and scrolls.
const PIECES: &[&[u8]] = &[
    b"abc",
    b"hello world ",
    b"xxxxxxxxxxxxxxxxxxxxxxx",
    "\u{4e2d}".as_bytes(),
    "a\u{4e2d}\u{6587}\u{5b57}b".as_bytes(),
    "\u{1f600}".as_bytes(),
    "e\u{301}\u{302}\u{303}\u{304}\u{305}\u{306}\u{307}\u{308}\u{309}".as_bytes(),
    b"\r\n",
    b"\r\n",
    b"\n",
    b"\r",
    b"\x1b[K",
    b"\x1b[1K",
    b"\x1b[2K",
    b"\x1b[J",
    b"\x1b[2J",
    b"\x1b[44m\x1b[K\x1b[0m",
    b"\x1b[41mred\x1b[0m",
    b"\x1b[3;5H",
    b"\x1b[99;99H",
    b"\x1b[2;1H",
    b"\x1b[A",
    b"\x1b7",
    b"\x1b8",
    b"\x1b]8;;https://a\x1b\\link\x1b]8;;\x1b\\",
    b"\x1b]8;id=1;https://b\x1b\\\xe4\xb8\xad \x1b]8;;\x1b\\",
    b"\x1b]133;A\x07",
    b"\x1b[S",
    b"\x1b[T",
    b"\x1b[2@",
    b"\x1b[2P",
    b"\x1b[2;3r",
    b"\x1b[r",
    b"\x1b[?1049h",
    b"\x1b[?1049l",
];

/// A primary screen grid as random output and resizes leave it, history
/// rows of other widths included, then poked at where output seldom goes:
/// wraps set or cleared on any row, the last retained included; wide
/// glyphs in the last column and halves without their other half; links
/// on blank cells; prompts; `used` marks; the cursor and the saved cursor
/// anywhere, waiting to wrap or not.
fn random_grid(r: &mut Rng) -> Result<Grid, Error> {
    let options = crate::Options::new()
        .with_reflow(r.chance(70))
        .with_hyperlinks(true)
        .with_prompt_marks(true);
    let (rows, cols) = (r.small(7).saturating_add(1), r.small(12).saturating_add(1));
    let mut p = crate::Parser::with_options(rows, cols, r.below(30), options)?;
    for _ in 0..r.below(80) {
        match r.below(16) {
            0 => p.resize(r.small(7).saturating_add(1), r.small(12).saturating_add(1))?,
            // A line to the last column, waiting to wrap.
            1 => {
                let cols = p.screen().size().1;
                let fill: Vec<u8> = std::iter::repeat_n(b'y', usize::from(cols)).collect();
                p.process(b"\r")?;
                p.process(&fill)?;
            }
            // Clusters of up to 121 bytes, enough for rows laid out of
            // them to run out of room for their text.
            2 => {
                for _ in 0..r.below(4).saturating_add(1) {
                    let marks = r.below(61);
                    let cluster: String = std::iter::once('a')
                        .chain(std::iter::repeat_n('\u{301}', marks))
                        .collect();
                    p.process(cluster.as_bytes())?;
                }
            }
            _ => {
                let piece = PIECES.get(r.below(PIECES.len())).copied().unwrap_or(b"a");
                p.process(piece)?;
            }
        }
    }
    let mut grid = p.screen().primary_grid().clone();
    let retained = grid.retained_len();
    for _ in 0..r.below(5) {
        let Some(&slot) = grid.order.get(r.below(retained)) else {
            continue;
        };
        let Some(m) = grid.meta.get_mut(slot) else {
            continue;
        };
        let width = usize::from(m.width);
        let col = r.below(width);
        match r.below(5) {
            0 => m.wrapped = !m.wrapped,
            1 => m.prompt = true,
            2 => {
                m.used = m.width;
                let half = match r.below(3) {
                    0 => Compact::glyph('\u{4e2d}', 2, 0),
                    1 => Compact::continuation(),
                    _ => Compact::default(),
                };
                let at = if r.chance(50) {
                    width.saturating_sub(1)
                } else {
                    col
                };
                if let Some(cell) = grid.slice_mut(slot).get_mut(at) {
                    *cell = half;
                }
            }
            3 => {
                m.linked = true;
                let links = grid
                    .linked
                    .entry(slot)
                    .or_insert_with(|| vec![0; width].into_boxed_slice());
                if let Some(link) = links.get_mut(col) {
                    *link = 1;
                }
            }
            _ => m.used = u16::try_from(r.below(width.saturating_add(1))).unwrap_or(m.width),
        }
    }
    if !grid.blank_past_used() {
        // A `used` mark poked below a cell with text: put back.
        for m in &mut grid.meta {
            m.used = m.width;
        }
    }
    let (rows, cols) = (grid.rows.get(), grid.cols.get());
    if r.chance(50) {
        grid.cursor = (r.small(rows), r.small(cols));
        grid.pending_wrap = r.chance(30);
    }
    if r.chance(50) {
        grid.saved_cursor = (r.small(rows), r.small(cols));
        grid.saved_pending_wrap = r.chance(30);
    }
    Ok(grid)
}

/// No reader can tell the two grids apart: each row's identity, version,
/// wrap, prompt, cells with their text and links, and whether it has
/// links at all; the cursor, the saved cursor, each waiting to wrap or
/// not; the margins and the size. And `runs`'s rows are blank past their
/// `used` marks.
fn assert_same(runs: &Grid, cells: &Grid, case: &str) {
    assert!(runs.seen() == cells.seen(), "{case}: rows");
    let linked = |g: &Grid| -> Vec<bool> {
        (0..g.retained_len())
            .map(|i| g.row_at(i).is_some_and(|r| r.links.is_some()))
            .collect()
    };
    assert_eq!(linked(runs), linked(cells), "{case}: linked rows");
    let shape = |g: &Grid| {
        (
            (g.rows, g.cols, g.history_len(), g.stride),
            (g.cursor, g.pending_wrap),
            (g.saved_cursor, g.saved_pending_wrap),
            (g.origin, g.saved_origin, g.top, g.bottom),
        )
    };
    assert_eq!(shape(runs), shape(cells), "{case}: shape");
    assert!(runs.blank_past_used(), "{case}: used");
}

/// A reflow laying out lines a run of cells at a time (`lay_out_runs`)
/// gives exactly what laying them out a cell at a time does: the two agree
/// on every observable, over random grids and sizes, at the same width (the
/// rows alone changing, as a split, zoom or height-only drag does) and at
/// others.
#[test]
fn laying_out_runs_is_laying_out_cells() -> Result<(), Error> {
    let mut r = Rng(0x0ef1_0000_0000_0003);
    let wrapped = |g: &Grid| {
        (0..g.retained_len())
            .filter(|i| g.meta_at(*i).is_some_and(|m| m.wrapped))
            .count()
    };
    let (mut wrapped_in, mut wrapped_out) = (0usize, 0usize);
    for case in 0..3_000 {
        let grid = random_grid(&mut r)?;
        let next = grid
            .meta
            .iter()
            .map(|m| m.id.0)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        wrapped_in = wrapped_in.saturating_add(wrapped(&grid));
        for target in 0..6 {
            let rows = r.small(9).saturating_add(1);
            let cols = if target < 3 {
                grid.cols.get()
            } else {
                r.small(14).saturating_add(1)
            };
            let (mut a, mut b) = (next, next);
            let runs = grid.reflowed(rows, cols, &mut a, 1_000);
            let cells = grid.reflowed_by(rows, cols, &mut b, 1_000, Lines::All);
            let name = format!("case {case} to {rows}x{cols}");
            match (runs, cells) {
                (Ok(runs), Ok(cells)) => {
                    assert_same(&runs, &cells, &name);
                    if cols != grid.cols.get() {
                        wrapped_out = wrapped_out.saturating_add(wrapped(&runs));
                    }
                }
                (runs, cells) => assert_eq!(runs.err(), cells.err(), "{name}"),
            }
            assert_eq!(a, b, "{name}: identities taken");
        }
    }
    // Soft-wrapped lines go in, and lines are wrapped anew coming out.
    assert!(
        wrapped_in > 10_000 && wrapped_out > 10_000,
        "{wrapped_in} soft-wrapped rows in, {wrapped_out} out at other widths"
    );
    Ok(())
}
