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
/// A full history's storage, metadata included, stops growing: the
/// screen's as much after 20,000 scrolls as after 10,000, and history's
/// within its budget. A resize builds the new screen and history before
/// it lets the old go (`THREATS.md` B37): the new history within its
/// budget and a tenth more and 64 KiB (3.2's resize term), the new screens'
/// cells exactly their size, so the peak is the old and those together.
#[test]
fn storage_with_history_full_stops_growing_and_a_resize_holds_its_bound() -> Result<(), Error> {
    const BUDGET: usize = 10_000 * 24;
    let mut next = 0;
    let mut primary = Grid::new(Size::of(24, 80), BUDGET, &mut next, 0)?;
    let alternate = Grid::new(Size::of(24, 80), 0, &mut next, 0)?;
    for version in 1..=10_000 {
        primary.scroll(Size::of(24, 80).lines(), 1, UP, 0, &mut next, version);
    }
    let plateau = heap(&primary);
    for version in 10_001..=20_000 {
        primary.scroll(Size::of(24, 80).lines(), 1, UP, 0, &mut next, version);
    }
    // The screen's storage as it was, and history's within its budget.
    let now = heap(&primary);
    assert_eq!(now.get(..3), plateau.get(..3));
    assert!(now.iter().zip(plateau).skip(3).all(|(h, _)| *h <= BUDGET));
    // 240,000 bytes hold 19 of the ring's chunks of 512 empty rows (12,288
    // bytes each, THREATS.md B17): the newest of them filling, and the
    // oldest let go only once all its rows have gone.
    assert!((18 * 512 + 1..=19 * 512).contains(&primary.history_len()));
    let resized_primary = primary.resized(Size::of(60, 120), &mut next, 20_001)?;
    let resized_alternate = alternate.resized(Size::of(60, 120), &mut next, 20_001)?;
    let [cells, _, _, history] = heap(&resized_primary);
    assert!(
        history <= BUDGET + BUDGET / 10 + (64 << 10),
        "{history} bytes of new history"
    );
    assert_eq!(cells, 60 * 120 * size_of::<Compact>());
    assert_eq!(heap(&resized_alternate).first(), Some(&cells));
    Ok(())
}

// Narrowing a pane whose rows all stay live must not keep the old width in
// its storage: that multiplied every later copy of the grid by the widest
// width it ever had, and doubled the smoke's 200-pane `scale` run.
#[test]
fn narrowing_live_rows_uses_the_new_width() -> Result<(), Error> {
    let mut next = 0;
    let wide = Grid::new(Size::of(24, 400), 100, &mut next, 0)?;
    let narrow = wide.resized(Size::of(23, 10), &mut next, 1)?;
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
    let contiguous = Grid::new(Size::of(5, 1), 0, &mut next, 0)?;
    let mut wrapped = Grid::new(Size::of(5, 1), 4, &mut next, 0)?;
    for version in 1..=11 {
        wrapped.scroll(Size::of(5, 1).lines(), 1, UP, 0, &mut next, version);
    }
    let order = &wrapped.order;
    assert!((1..order.len()).contains(&order.top), "the order wraps");
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
/// link.
pub(crate) type Seen = (
    RowId,
    u64,
    bool,
    bool,
    Vec<(Compact, Attributes, String, Option<Hyperlink>)>,
);

impl Grid {
    /// Every link a row of the grid has, its history's and its screen's.
    pub(crate) fn held_links(&self) -> impl Iterator<Item = &Hyperlink> {
        let history = (0..self.history_len()).filter_map(|index| self.history.get(index));
        history
            .filter_map(|row| row.links)
            .chain(self.linked.iter().map(RowLinks::runs))
            .flat_map(Runs::links)
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
                        let link = row.links.and_then(|l| l.get(col)).cloned();
                        let read = c.read(row.text, row.grid.styles());
                        (*c, read.attributes(), read.contents().to_owned(), link)
                    })
                    .collect();
                (
                    row.id,
                    row.version,
                    row.wrapped(),
                    row.starts_prompt(),
                    cells,
                )
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
        .set(crate::Feature::Reflow, r.chance(70))
        .with(crate::Feature::Hyperlinks)
        .with(crate::Feature::PromptMarks);
    let (rows, cols) = (r.small(7).saturating_add(1), r.small(12).saturating_add(1));
    // Up to 30 rows of history, at 64 bytes a row.
    let history = r.below(30).saturating_mul(64);
    let mut p = crate::Parser::with_options(Size::of(rows, cols), history, options)?;
    for _ in 0..r.below(80) {
        match r.below(16) {
            0 => p.resize(Size::of(
                r.small(7).saturating_add(1),
                r.small(12).saturating_add(1),
            ))?,
            // A line to the last column, waiting to wrap.
            1 => {
                let cols = p.screen().size().cols();
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
    let (rows, cols) = (usize::from(grid.size.rows()), grid.size.cols());
    let width = usize::from(cols);
    for _ in 0..r.below(5) {
        let Some(&slot) = grid.order.get(r.below(rows)) else {
            continue;
        };
        let Some(m) = grid.meta.get_mut(slot) else {
            continue;
        };
        let col = r.below(width);
        match r.below(5) {
            0 => {
                let wrapped = m.wrapped();
                m.set(WRAPPED, !wrapped);
            }
            1 => m.set(PROMPT, true),
            2 => {
                m.used = cols;
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
                let link = grid.links.insert(&Hyperlink::new(0, "poked", None));
                if let Some(links) = grid.linked.get_mut(slot) {
                    links.set(col..col.saturating_add(1), link.as_ref());
                }
            }
            _ => m.used = u16::try_from(r.below(width.saturating_add(1))).unwrap_or(cols),
        }
    }
    if !grid.blank_past_used() {
        // A `used` mark poked below a cell with text: put back.
        for m in &mut grid.meta {
            m.used = cols;
        }
    }
    if grid.history_bytes() > 0 && r.chance(50) {
        let mut next = max_id(&grid).saturating_add(1);
        let (lines, rows) = (grid.size.lines(), grid.size.rows());
        grid.scroll(lines, r.small(rows), UP, 0, &mut next, 999);
    }
    let (rows, cols) = grid.size.into();
    for cursor in [&mut grid.cursor, &mut grid.saved.cursor] {
        if r.chance(50) {
            (cursor.row, cursor.col) = (r.small(rows), r.small(cols));
            cursor.pending_wrap = r.chance(30);
        }
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
        let rows = grid.size.rows();
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
        let region = Span::of(top, bottom);
        by_rows.scroll_region(region, count, direction, blank, &mut next, 1_000);
        by_cells.scroll_columns(region, count, up, blank, 1_000);
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

/// Turning rows that cross the ring's end turns those rows and no others,
/// as turning them where the ring starts at its top does, each way and
/// by every count.
#[test]
fn a_rotation_across_the_rings_end_turns_only_its_rows() {
    for top in 0..6 {
        for (low, high) in [(0, 5), (1, 4), (2, 5), (3, 4), (0, 0)] {
            for by in 0..=high - low + 1 {
                for left in [true, false] {
                    let mut order = Order {
                        slots: (0..6).collect(),
                        top,
                    };
                    let mut want: Vec<usize> = order.iter().copied().collect();
                    if let Some(run) = want.get_mut(low..=high) {
                        if left {
                            run.rotate_left(by);
                        } else {
                            run.rotate_right(by);
                        }
                    }
                    order.rotate((low, high), by, left);
                    let got: Vec<usize> = order.iter().copied().collect();
                    assert_eq!(
                        got, want,
                        "top {top}, rows {low}..={high}, by {by}, left {left}"
                    );
                }
            }
        }
    }
}

/// DECCRA's and a margin scroll's copies of a row's cells as they were
/// made a cell at a time (`copy_cells` and `copy_span_cells` at 808da566),
/// which the copies of whole runs must equal, to the byte.
mod per_cell {
    use super::*;

    pub(super) fn copy_cells(
        grid: &mut Grid,
        from: u16,
        y: u16,
        span: Range<usize>,
        to: Range<u16>,
        version: u64,
    ) {
        let text = grid.slot(from).and_then(|slot| grid.texts.get(slot));
        let text = text.cloned().unwrap_or_default();
        let mut cells = grid.live_cells(from).get(span).unwrap_or_default().to_vec();
        if let Some(cut) = cells.first_mut().filter(|c| c.is_wide_continuation()) {
            *cut = cut.blanked();
        }
        if let Some(cut) = cells.last_mut().filter(|c| c.is_wide()) {
            *cut = cut.blanked();
        }
        grid.mutate_line(y, version, to.end, |line| {
            let span = usize::from(to.start)..usize::from(to.end);
            line.clear_for(span.clone());
            for (at, cell) in span.zip(&cells) {
                if cell.is_spilled() {
                    line.set(at, *cell, text.of(cell));
                } else if let Some(slot) = line.cells.get_mut(at) {
                    *slot = *cell;
                }
            }
            repair_wide(line.cells);
            true
        });
    }

    pub(super) fn copy_span_cells(
        grid: &mut Grid,
        source: usize,
        target: usize,
        span: Range<usize>,
        version: u64,
    ) {
        let cells = grid
            .slice(source)
            .get(span.clone())
            .unwrap_or_default()
            .to_vec();
        let source = grid.texts.get(source).cloned().unwrap_or_default();
        let texts: Vec<String> = (cells.iter())
            .map(|cell| match cell.is_spilled() {
                true => source.of(cell).to_owned(),
                false => String::new(),
            })
            .collect();
        let text_of = |i: usize| texts.get(i).map_or("", String::as_str);
        let mut changed = false;
        if let Some(mut line) = grid.line(target) {
            let olds = line.cells.get(span.clone()).unwrap_or_default();
            changed = (cells.iter().zip(olds).enumerate())
                .any(|(i, (cell, old))| !old.holds(line.text, cell, text_of(i)));
            let in_place = (cells.iter().enumerate())
                .zip(span.clone())
                .all(|((i, cell), at)| !cell.is_spilled() || line.replaces(at, text_of(i).len()));
            if changed && !in_place {
                line.clear_for(span.clone());
            }
            for (i, (at, cell)) in span.clone().zip(&cells).enumerate().filter(|_| changed) {
                if !cell.is_spilled() {
                    if let Some(slot) = line.cells.get_mut(at) {
                        *slot = *cell;
                    }
                } else if in_place {
                    line.replace(at, *cell, text_of(i));
                } else {
                    line.set(at, *cell, text_of(i));
                }
            }
        }
        if changed {
            grid.changed(target, span.end, version);
        }
    }
}

/// A primary screen's grid as output leaves it at 9 rows of up to 40
/// columns, its rows full of clusters as long as each other or not, some
/// in history's way: clusters of every length a row's text keeps, short
/// and long, wide ones and wide glyphs, rows written alike, overwritten
/// and erased, links, a picture, lines of double width, and scrolls
/// between left and right margins.
fn cluster_grid(r: &mut Rng) -> Result<Grid, Error> {
    let options: crate::Options = crate::Feature::ALL.into_iter().collect();
    let cols = r.small(39).saturating_add(2);
    let mut p = crate::Parser::with_options(Size::of(9, cols), 0, options)?;
    let cluster = |r: &mut Rng| -> String {
        let base = r.pick(&['e', 'e', '\u{4e2d}', 'x']).unwrap_or('e');
        let marks = r.pick(&[1, 2, 7, 8, 8, 9, 20, 63]).unwrap_or(8);
        std::iter::once(base)
            .chain(std::iter::repeat_n('\u{301}', marks))
            .collect()
    };
    for _ in 0..r.below(40) {
        let piece = match r.below(12) {
            // A row written alike, cell after cell: one cluster again.
            0..=2 => {
                let one = cluster(r);
                let row = r.small(9).saturating_add(1);
                format!("\x1b[{row};1H{}", std::iter::repeat_n(one.as_str(), usize::from(cols)).collect::<String>())
            }
            3 | 4 => (0..r.below(30)).map(|_| cluster(r)).collect(),
            5 => format!("\x1b[{};{}H", r.small(9).saturating_add(1), r.small(cols).saturating_add(1)),
            6 => "\x1b]8;;https://a\x1b\\link \u{4e2d}e\u{301}\u{302}\u{303}\u{304}\u{305}\x1b]8;;\x1b\\".to_owned(),
            7 => r.pick(&["\x1b#6", "\x1b#3", "\x1b#5", "\x1b[2K", "\x1b[3X", "\x1b[2P", "\x1b[2@"]).unwrap_or("").to_owned(),
            8 => "\x1bPq#0;2;0;0;0#0~~~~-~~\x1b\\".to_owned(),
            9 => format!("\x1b[?69h\x1b[{};{}s\x1b[9;{}H\n\x1b[S\x1b[T\x1b[?69l", r.small(4).saturating_add(1), r.small(4).saturating_add(5), r.small(4).saturating_add(2)),
            10 => "\x1b[41m".to_owned(),
            _ => "\x1b[m\r\n".to_owned(),
        };
        p.process(piece.as_bytes())?;
    }
    Ok(p.screen().primary_grid().clone())
}

/// The copies of whole runs of cells, DECCRA's (`copy_cells`) and a
/// margin scroll's (`copy_span_cells`), leave each grid as the copies a
/// cell at a time did, its cells, text, versions and links to the byte:
/// over random spans of random rows of grids full of clusters, the row
/// over itself and over another.
#[test]
fn copies_of_runs_are_the_copies_a_cell_at_a_time() -> Result<(), Error> {
    let mut r = Rng(0x0ac5_9a25_0000_0001);
    let mut fast = 0usize;
    for case in 0..4_000 {
        let grid = cluster_grid(&mut r)?;
        let (rows, cols) = grid.size.into();
        for _ in 0..4 {
            let (from, y) = (r.small(rows), r.small(rows));
            let y = if r.chance(30) { from } else { y };
            let start = if r.chance(40) {
                0
            } else {
                usize::from(r.small(cols))
            };
            let end = if r.chance(40) {
                usize::from(cols)
            } else {
                start.saturating_add(
                    r.below(usize::from(cols).saturating_sub(start))
                        .saturating_add(1),
                )
            };
            let span = start..end;
            let (mut runs, mut cells) = (grid.clone(), grid.clone());
            let what = format!("case {case}: {from} to {y}, {span:?}");
            if r.chance(50) {
                let col = if r.chance(50) { 0 } else { r.small(cols) };
                let stop = col
                    .saturating_add(u16::try_from(span.len()).unwrap_or(0))
                    .min(cols);
                let stop = stop.min(grid.row_end(y)).max(col);
                runs.copy_cells(from, y, span.clone(), col..stop, 7);
                per_cell::copy_cells(&mut cells, from, y, span, col..stop, 7);
            } else if from != y
                && let (Some(source), Some(target)) = (grid.slot(from), grid.slot(y))
            {
                runs.copy_span_cells(source, target, span.clone(), 7);
                per_cell::copy_span_cells(&mut cells, source, target, span, 7);
            }
            let (a, b) = (format!("{runs:?}"), format!("{cells:?}"));
            assert!(a == b, "{what}\n runs: {a}\n cells: {b}");
            fast = fast.saturating_add(usize::from(a != format!("{grid:?}")));
        }
    }
    assert!(fast > 4_000, "{fast} copies changed their grid");
    Ok(())
}

thread_local! {
    /// Whether `Grid::restyle` takes the cell-by-cell way it had before it
    /// went a run at a time (`restyle_per_cell`), to check the two agree.
    pub(crate) static PER_CELL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

impl Grid {
    /// Fresh styles put in the table while the next new one would neither sweep
    /// it nor find it out of room, so that the next does.
    pub(crate) fn fill_styles_to_sweep(&mut self) {
        let cells = self.cells.len().saturating_add(self.history.kept_cells());
        let share = crate::style::screen_share(self.cells.len());
        for n in 0..u32::MAX {
            let room = self.styles.bytes_after_insert().saturating_sub(share);
            if self.styles.wants_sweep(cells) || self.styles.full() && room > self.history.budget()
            {
                return;
            }
            let [_, r, g, b] = n.to_be_bytes();
            let colour = crate::Color::Rgb(crate::Rgb { r, g, b });
            self.style(Attributes::default().with_underline_color(colour));
        }
    }
    /// `Grid::erase_to_spaces` as it was: a cell at a time unless the
    /// span holds no half of a wide glyph, the row's halves then repaired.
    pub(crate) fn erase_to_spaces_per_cell(
        &mut self,
        row: u16,
        (start, end): (u16, u16),
        all: Option<u32>,
        version: u64,
    ) {
        let span = usize::from(start)..usize::from(end);
        work!(cells, span.len());
        self.mutate_row(row, version, end, |cells| {
            // A span with no half of a wide glyph, and none protected that
            // DECSERA would keep, as most are: erased a run at once.
            if let Some(run) = cells.get_mut(span.clone()) {
                let plain = match all {
                    Some(_) => !Compact::any_halves(run),
                    None => !Compact::any_halves(run) && !run.iter().any(Compact::is_protected),
                };
                if plain {
                    let mut changed = false;
                    for cell in run.iter_mut() {
                        let occupied = cell.has_contents();
                        if occupied {
                            *cell = Compact::ascii(b' ', all.unwrap_or(cell.style()));
                        }
                        changed |= occupied;
                    }
                    return changed;
                }
            }
            // Whether the glyph before is protected.
            let before = span.start.checked_sub(1).and_then(|i| cells.get(i));
            let before = before.filter(|_| all.is_none());
            let (mut kept, mut changed) = (before.is_some_and(Compact::is_protected), false);
            for cell in cells.get_mut(span).into_iter().flatten() {
                if cell.is_wide_continuation() {
                    if !kept {
                        *cell = cell.blanked();
                        changed = true;
                    }
                    continue;
                }
                kept = all.is_none() && cell.is_protected();
                if cell.has_contents() && !kept {
                    *cell = Compact::ascii(b' ', all.unwrap_or(cell.style()));
                    changed = true;
                }
            }
            repair_wide(cells);
            changed
        });
        if all.is_some() {
            self.set_link(row, usize::from(start)..usize::from(end), None, version);
        }
    }

    /// `Grid::restyle` as it was: `restyle` asked of each cell it changes,
    /// the cell read when its turn comes.
    pub(crate) fn restyle_per_cell(
        &mut self,
        row: u16,
        (start, end): (u16, u16),
        fill: bool,
        version: u64,
        restyle: &mut impl FnMut(&mut Self, u32) -> u32,
    ) {
        let Some(slot) = self.slot(row) else {
            return;
        };
        let end = end.min(self.size.cols());
        work!(cells, end.saturating_sub(start));
        let mut changed = false;
        for col in usize::from(start)..usize::from(end) {
            let Some(&cell) = self.slice(slot).get(col) else {
                break;
            };
            let occupied = cell.has_contents() || cell.is_wide_continuation();
            if !occupied && !fill {
                continue;
            }
            let style = restyle(self, cell.style());
            let Some(at) = self.slice_mut(slot).get_mut(col) else {
                break;
            };
            let new = if occupied {
                let mut new = *at;
                new.set_style(style);
                new
            } else {
                Compact::ascii(b' ', style)
            };
            if new != *at {
                *at = new;
                changed = true;
            }
        }
        if changed {
            self.changed(slot, usize::from(end), version);
        }
    }
}
