use super::reflow::Lines;
use super::*;
use crate::test_rng::Rng;

/// Up, the departing rows going into history.
const UP: Scroll = Scroll::Up { history: true };

/// Heap bytes; a Vec's allocation cannot exceed `isize::MAX`, so no product
/// saturates.
fn heap(grid: &Grid) -> [usize; 4] {
    [
        grid.cells.capacity().saturating_mul(size_of::<Compact>()),
        grid.meta.capacity().saturating_mul(size_of::<Meta>()),
        grid.order.capacity().saturating_mul(size_of::<usize>()),
        grid.history.heap(),
    ]
}
/// A full history's storage, metadata included, stops growing: as much
/// after 20,000 scrolls as after 10,000. The peak a resize reaches, with
/// both screens' replacements built before either is assigned, is printed
/// (`MEMORY-BOUNDS`), not asserted.
#[test]
fn storage_with_history_full_stops_growing_and_the_resize_peak_is_printed() -> Result<(), Error> {
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
        "MEMORY-BOUNDS {{\"components\":[\"cells\",\"row_metadata\",\"slot_order\",\"history\"],\"initial_primary\":{initial_primary:?},\"initial_alternate\":{initial_alternate:?},\"plateau_primary\":{plateau:?},\"resized_primary\":{new_primary:?},\"resized_alternate\":{new_alternate:?},\"screen_object_bytes\":{screen_bytes},\"steady_reserved_bytes\":{},\"resize_peak_reserved_bytes\":{peak_reserved},\"scrolls\":20000}}",
        old_heap + screen_bytes
    );
    Ok(())
}

// Narrowing a pane whose rows all stay live must not keep the old width in
// its storage: that multiplied every later copy of the grid by the widest
// width it ever had, and doubled the smoke's 200-pane `scale` run.
#[test]
fn narrowing_live_rows_uses_the_new_width() -> Result<(), Error> {
    let mut next = 0;
    let wide = Grid::new(24, 400, 100, &mut next, 0)?;
    let narrow = wide.resized(23, 10, &mut next, 1)?;
    assert_eq!(narrow.history_len(), 0);
    assert_eq!(narrow.cells.capacity(), 23 * 10);
    assert_eq!(narrow.storage_cells(), 23 * 10);
    Ok(())
}

/// `move_row` is `remove(from)` then `insert(to)`, for every pair of rows,
/// in an order that is one slice and in one that has wrapped round into two;
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
    assert!(!front.is_empty() && !back.is_empty(), "the order wraps");
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
            if let Some(text) = self.texts.get_mut(slot) {
                text.clear();
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
                        let read = c.read(row.text, row.styles);
                        (*c, read.attributes(), read.contents().to_owned(), link)
                    })
                    .collect();
                (row.id, row.version, row.wrapped, row.prompt, cells)
            })
            .collect()
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
    // Rows of the screen are poked; some go into history after.
    let rows = usize::from(grid.rows.get());
    for _ in 0..r.below(5) {
        let Some(&slot) = grid.order.get(r.below(rows)) else {
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
    if grid.history_limit > 0 && r.chance(50) {
        let mut next = max_id(&grid).saturating_add(1);
        let last = grid.rows.last();
        grid.scroll((0, last), r.small(grid.rows.get()), UP, 0, &mut next, 999)?;
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

/// The greatest identity a retained row of `grid` has.
fn max_id(grid: &Grid) -> u64 {
    (0..grid.retained_len())
        .filter_map(|i| grid.row_at(i))
        .map(|row| row.id.0)
        .max()
        .unwrap_or(0)
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
            (g.rows, g.cols, g.history_len(), g.storage_cells()),
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
            .filter(|i| g.row_at(*i).is_some_and(|r| r.wrapped))
            .count()
    };
    let (mut wrapped_in, mut wrapped_out) = (0usize, 0usize);
    for case in 0..3_000 {
        let grid = random_grid(&mut r)?;
        let next = max_id(&grid).saturating_add(1);
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

/// A live row's cells as a reader reads them: text, halves, attributes
/// and link.
fn read_row(grid: &Grid, y: u16) -> Vec<(String, bool, bool, Attributes, Option<String>)> {
    let Some(row) = grid.live_row(y) else {
        return Vec::new();
    };
    (0..row.width)
        .filter_map(|x| {
            let cell = row.cell(x)?;
            let link = row.link(x).map(|l| l.uri().to_owned());
            Some((
                cell.contents().to_owned(),
                cell.is_wide(),
                cell.is_wide_continuation(),
                cell.attributes(),
                link,
            ))
        })
        .collect()
}

/// Scrolling between left and right margins as wide as the screen
/// (`scroll_columns`, which copies cells, their text and their links from
/// row to row) leaves every row reading as scrolling the rows themselves
/// (`scroll_region`, which moves them) does: over random grids, with wide
/// glyphs, halves alone, clusters kept in a row's text and links, every
/// region, count and way, blanks in the default style or another.
#[test]
fn scrolling_between_full_margins_is_scrolling_rows() -> Result<(), Error> {
    let mut r = Rng(0x0ef1_0000_0000_0069);
    let coloured = Attributes::new(crate::Color::Idx(1), crate::Color::Idx(4))
        .inline_style()
        .unwrap_or(0);
    let mut moved = 0usize;
    for case in 0..3_000 {
        let grid = random_grid(&mut r)?;
        let rows = grid.rows.get();
        let top = r.small(rows);
        let bottom = top.saturating_add(r.small(rows.saturating_sub(top)));
        let count = r.small(rows.saturating_add(2)).saturating_add(1);
        let up = r.chance(50);
        let blank = if r.chance(30) { coloured } else { 0 };
        let (mut by_rows, mut by_cells) = (grid.clone(), grid.clone());
        let mut next = max_id(&grid).saturating_add(1);
        let direction = if up {
            Scroll::Up { history: false }
        } else {
            Scroll::Down
        };
        by_rows.scroll_region((top, bottom), count, direction, blank, &mut next, 1_000)?;
        by_cells.scroll_columns((top, bottom), count, up, blank, 1_000);
        for y in 0..rows {
            let (a, b) = (read_row(&by_rows, y), read_row(&by_cells, y));
            assert_eq!(
                a, b,
                "case {case}: {top}..={bottom} by {count}, up {up}, row {y}"
            );
            moved = moved.saturating_add(usize::from(a != read_row(&grid, y)));
        }
        assert!(by_cells.blank_past_used(), "case {case}: used");
    }
    assert!(moved > 3_000, "{moved} rows changed");
    Ok(())
}

/// After `reset_links` (RIS), a row that had links is given them again as
/// any row is: `Meta::linked` is cleared with the arrays it named, so a
/// link printed there makes the row's array anew.
#[test]
fn a_row_given_links_after_reset_links_keeps_them() -> Result<(), Error> {
    let mut next = 0;
    let mut grid = Grid::new(2, 5, 0, &mut next, 0)?;
    let uri: Arc<str> = Arc::from("https://example.com");
    let link = grid.intern(&uri, None, 1, 1).ok_or(Error::Capacity)?;
    grid.set_link(0, 0..2, link, 1);
    grid.reset_links();
    let link = grid.intern(&uri, None, 1, 2).ok_or(Error::Capacity)?;
    grid.set_link(0, 0..2, link, 2);
    let slot = grid.slot(0).ok_or(Error::Capacity)?;
    let links = grid.linked.get(&slot).map(|links| links.to_vec());
    assert_eq!(links, Some(vec![link, link, 0, 0, 0]));
    Ok(())
}
