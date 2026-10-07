use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::num::NonZeroU16;
use std::ops::Range;
use std::sync::Arc;

use crate::compact::{BLANK, Compact, Line, NO_TEXT, Text};
use crate::history::{Arriving, History, trimmed};
use crate::link::Links;
use crate::style::Styles;
use crate::{Attributes, CellRef, Error, Row, RowId};

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

/// How many styles of the table a grid keeps at hand (`Grid::recent`).
const RECENT: usize = 32;

/// Where attributes are kept among the styles at hand: a hash of them.
#[inline]
fn recent(attributes: Attributes) -> usize {
    let (a, b) = attributes.bits();
    let hash = (a ^ b.rotate_left(17)).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    // The top five bits, one of 32.
    usize::try_from(hash >> 59).unwrap_or(0)
}

/// Takes the row in `slot`, of `cols` columns, into `history`, if it has
/// neither text nor links: its cells before its blank tail, which are
/// blanked, as the row the slot takes next needs them, and what it is.
/// Whether it did. Given the grid's parts, which it borrows apart.
#[inline(always)]
fn take_row(
    history: &mut History,
    cells: &mut [Compact],
    meta: &mut [Meta],
    texts: &[Text],
    (slot, cols): (usize, usize),
) -> Result<bool, Error> {
    let Some(m) = meta.get_mut(slot) else {
        return Ok(false);
    };
    if m.linked || texts.get(slot).is_some_and(|text| !text.is_empty()) {
        return Ok(false);
    }
    // Past its `used` mark the row is blank. A slot's cells are within
    // `cells`, so these do not wrap.
    let start = slot.wrapping_mul(cols);
    let row = cells
        .get_mut(start..start.wrapping_add(usize::from(m.used)))
        .unwrap_or_default();
    let len = trimmed(row).len();
    let kept = row.get_mut(..len).unwrap_or_default();
    history.push(Arriving {
        id: m.id,
        version: m.version,
        wrapped: m.wrapped,
        prompt: m.prompt,
        cells: kept,
        width: m.width,
    })?;
    // Blanked here, while they are at hand, not when the slot is taken;
    // a row of one cell, as a short line's often is, without a call.
    match kept {
        [cell] => *cell = BLANK,
        _ => kept.fill(BLANK),
    }
    m.used = 0;
    Ok(true)
}

/// Maximum addressable retained cells per buffer (512 MiB at 8 bytes a cell).
/// Storage is committed only for live/retained rows, not empty history slots.
pub(crate) const MAX_CELLS: usize = 64 * 1024 * 1024;
pub(crate) const MAX_ROWS: usize = 1_048_576;

/// A row of the screen, in its slot: what it is, besides its cells.
#[derive(Clone, Copy, Debug)]
struct Meta {
    id: RowId,
    version: u64,
    width: u16,
    wrapped: bool,
    /// How far into the row a cell may differ from a blank in the default
    /// attributes: every cell from here to `width` is one, so recycling the
    /// slot clears only the cells before it, and a row scrolled into
    /// history is looked at no further.
    used: u16,
    /// Whether the row has an array of links in `Grid::linked`.
    linked: bool,
    /// Whether a prompt starts on the row (OSC 133 ; A).
    prompt: bool,
}

// `linked` and `prompt` fit where the struct had padding: a row costs
// no more for them.
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

/// The slot of each row of the screen, top to bottom: a ring of the slots,
/// the top row's at `top`. Scrolling the whole screen turns it by one, the
/// top row's slot becoming the bottom row's, and moves no slot.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Order {
    slots: Vec<usize>,
    top: usize,
}

impl Order {
    fn len(&self) -> usize {
        self.slots.len()
    }
    #[cfg(test)]
    fn capacity(&self) -> usize {
        self.slots.capacity()
    }
    fn try_reserve_exact(&mut self, more: usize) -> Result<(), Error> {
        self.slots
            .try_reserve_exact(more)
            .map_err(|_| Error::Capacity)
    }
    /// Where row `row` is in `slots`.
    #[inline]
    fn at(&self, row: usize) -> Option<usize> {
        if row >= self.slots.len() {
            return None;
        }
        // Both are below the length, so this does not wrap.
        let at = self.top.wrapping_add(row);
        Some(at.checked_sub(self.slots.len()).unwrap_or(at))
    }
    #[inline]
    fn get(&self, row: usize) -> Option<&usize> {
        self.slots.get(self.at(row)?)
    }
    fn get_mut(&mut self, row: usize) -> Option<&mut usize> {
        let at = self.at(row)?;
        self.slots.get_mut(at)
    }
    #[inline]
    fn front(&self) -> Option<&usize> {
        self.get(0)
    }
    /// Puts `slot` under the last row.
    fn push_back(&mut self, slot: usize) {
        if self.top != 0 {
            if let Some(slots) = self.slots.get_mut(..) {
                slots.rotate_left(self.top);
            }
            self.top = 0;
        }
        self.slots.push(slot);
    }
    /// The top row's slot becomes the bottom row's.
    #[inline]
    fn turn(&mut self) {
        let top = self.top.wrapping_add(1);
        self.top = if top >= self.slots.len() { 0 } else { top };
    }
    /// The rows, top to bottom, in two runs.
    fn as_slices(&self) -> (&[usize], &[usize]) {
        let top = self.top.min(self.slots.len());
        match self.slots.split_at_checked(top) {
            Some((before, from)) => (from, before),
            None => (&[], &[]),
        }
    }
    fn as_mut_slices(&mut self) -> (&mut [usize], &mut [usize]) {
        let top = self.top.min(self.slots.len());
        match self.slots.split_at_mut_checked(top) {
            Some((before, from)) => (from, before),
            None => (&mut [], &mut []),
        }
    }
    fn iter(&self) -> impl Iterator<Item = &usize> {
        let (front, back) = self.as_slices();
        front.iter().chain(back)
    }
}

/// A grid: its screen, rows of `cols` cells in slots, `order` saying which
/// slot each row is in; and its history (`history.rs`), the rows scrolled
/// off the screen's top, each kept as the cells it uses. A row's place
/// among the retained rows counts history first, oldest first, then the
/// screen from its top.
#[derive(Clone, Debug)]
pub(crate) struct Grid {
    cells: Vec<Compact>,
    meta: Vec<Meta>,
    /// Each slot's text too long for its cells to hold inline.
    texts: Vec<Text>,
    /// The links the cells point to.
    pub links: Links,
    /// The attributes of the cells' styles: shared with the grid a resize
    /// makes from this one, until either adds a style.
    styles: Arc<Styles>,
    /// Styles of the table found lately, each with its attributes, by a
    /// hash of them (`recent`): a program's colours, which it changes
    /// between, are found again without a search of the table. Empty, as
    /// whenever styles are numbered anew, each holds the default attributes,
    /// which are no style of the table.
    recent: [(Attributes, u32); RECENT],
    /// Changed whenever a style's number may have changed: a sweep, the
    /// grid started again. A number got with one epoch is a style's while
    /// the epoch lasts, and a resize, which keeps the numbers, keeps it.
    epoch: u64,
    /// The link of each cell of the slots whose `Meta::linked` is set, by
    /// slot: only rows a link was printed in have one (see `link.rs`).
    linked: Linked,
    /// The slot of each row of the screen, top to bottom.
    order: Order,
    history: History,
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
    /// The left and right margins (DECSLRM, with DECLRMM set), zero-based
    /// and inclusive: the screen's first and last columns unless a program
    /// set them. Set with `set_columns` alone, which keeps `lr`.
    pub left: u16,
    pub right: u16,
    /// Whether `left` and `right` are narrower than the screen: one test
    /// for every operation they bound, which without them does as it did.
    lr: bool,
    /// Whether its screen's cells are yet to be made (`Grid::unmade`):
    /// every row is blank, and `make` makes them.
    unmade: bool,
}

/// A grid's cursor and what goes with it (`Grid::clone_cursor`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct CursorState {
    cursor: (u16, u16),
    pending_wrap: bool,
    origin: bool,
    margins: (u16, u16, u16, u16),
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
        Self::made(rows, cols, history_limit, next, version, false)
    }

    /// `new`, but with no storage for the screen's cells until `make`
    /// makes it: the rows, their identities and versions, the cursor and
    /// the margins are all `new`'s. Blank rows read as blank, and resizing
    /// and clearing keep it unmade (`resized`, `clear`); anything else is
    /// for a made grid. The alternate screen is made so, as most programs
    /// never show it.
    pub fn unmade(rows: u16, cols: u16, next: &mut u64, version: u64) -> Result<Self, Error> {
        Self::made(rows, cols, 0, next, version, true)
    }

    /// Gives an unmade grid (`unmade`) its screen's cells, blank, as `new`
    /// would have made them; nothing if it has them. On failure, it stays
    /// as it was.
    pub fn make(&mut self) -> Result<(), Error> {
        if !self.unmade {
            return Ok(());
        }
        let size = usize::from(self.rows.get())
            .checked_mul(usize::from(self.cols.get()))
            .ok_or(Error::Capacity)?;
        self.cells
            .try_reserve_exact(size)
            .map_err(|_| Error::Capacity)?;
        self.cells.resize(size, BLANK);
        self.unmade = false;
        Ok(())
    }

    fn made(
        rows: u16,
        cols: u16,
        history_limit: usize,
        next: &mut u64,
        version: u64,
        unmade: bool,
    ) -> Result<Self, Error> {
        let (rows, cols) = Self::check_size(rows, cols, history_limit)?;
        let mut grid = Self {
            unmade,
            ..Self::bare(rows, cols, history_limit)
        };
        grid.reserve_screen()?;
        for _ in 0..rows.get() {
            let id = next_id(next)?;
            grid.push_screen_row(
                Meta::new(id, version, cols.get(), false, 0),
                &[],
                None,
                None,
            );
        }
        Ok(grid)
    }

    /// A grid of `rows` by `cols` with no row yet, nor storage for one.
    fn bare(rows: Extent, cols: Extent, history_limit: usize) -> Self {
        Self {
            cells: Vec::new(),
            meta: Vec::new(),
            texts: Vec::new(),
            links: Links::default(),
            styles: Arc::default(),
            recent: [(Attributes::default(), 0); RECENT],
            epoch: 0,
            linked: Linked::default(),
            order: Order::default(),
            history: History::new(history_limit),
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
            left: 0,
            right: cols.last(),
            lr: false,
            unmade: false,
        }
    }

    /// A grid with no row yet that takes this one's place: the same styles,
    /// and their numbers, which the rows it is given keep; unmade if this
    /// one is.
    fn successor(&self, rows: Extent, cols: Extent) -> Self {
        Self {
            styles: Arc::clone(&self.styles),
            recent: self.recent,
            epoch: self.epoch,
            unmade: self.unmade,
            ..Self::bare(rows, cols, self.history_limit)
        }
    }

    /// Room for the screen's rows, exactly; their cells' only once made.
    fn reserve_screen(&mut self) -> Result<(), Error> {
        let rows = usize::from(self.rows.get());
        let size = rows
            .checked_mul(usize::from(self.cols.get()))
            .ok_or(Error::Capacity)?;
        if size > MAX_CELLS {
            return Err(Error::Capacity);
        }
        let cells = if self.unmade { 0 } else { size };
        self.cells
            .try_reserve_exact(cells.saturating_sub(self.cells.len()))
            .map_err(|_| Error::Capacity)?;
        self.meta
            .try_reserve_exact(rows.saturating_sub(self.meta.len()))
            .map_err(|_| Error::Capacity)?;
        self.texts
            .try_reserve_exact(rows.saturating_sub(self.texts.len()))
            .map_err(|_| Error::Capacity)?;
        self.order
            .try_reserve_exact(rows.saturating_sub(self.order.len()))?;
        Ok(())
    }

    /// Puts a row under the screen's rows, in a slot of its own: `meta`,
    /// `cells` from its first (the rest blank), its text and its links.
    /// Room was made for it (`reserve_screen`).
    fn push_screen_row(
        &mut self,
        mut meta: Meta,
        cells: &[Compact],
        text: Option<Text>,
        links: Option<Box<[u16]>>,
    ) {
        let slot = self.meta.len();
        let cols = usize::from(self.cols.get());
        // An unmade grid's rows are blank, and have no cells to put.
        if !self.unmade {
            let start = self.cells.len();
            self.cells
                .extend_from_slice(cells.get(..cells.len().min(cols)).unwrap_or_default());
            self.cells.resize(start.saturating_add(cols), BLANK);
        }
        meta.linked = links.is_some();
        if let Some(links) = links {
            self.linked.insert(slot, links);
        }
        self.meta.push(meta);
        self.texts.push(text.unwrap_or_default());
        self.order.push_back(slot);
    }

    pub fn history_len(&self) -> usize {
        self.history.len()
    }
    /// The slot of the screen's row `row`, counted from 0 at the top.
    fn screen_slot(&self, row: usize) -> Option<usize> {
        self.order.get(row).copied()
    }
    pub fn retained_len(&self) -> usize {
        self.history.len().saturating_add(self.order.len())
    }
    /// The cells retained: the screen's, and those its history's rows
    /// keep.
    pub fn storage_cells(&self) -> usize {
        self.cells
            .capacity()
            .saturating_add(self.history.kept_cells())
    }

    /// Where a slot's cells are.
    fn cells_of(&self, slot: usize) -> Option<Range<usize>> {
        let width = usize::from(self.meta.get(slot)?.width);
        let start = slot.checked_mul(usize::from(self.cols.get()))?;
        Some(start..start.checked_add(width)?)
    }
    fn slice(&self, slot: usize) -> &[Compact] {
        self.cells_of(slot)
            .and_then(|cells| self.cells.get(cells))
            .unwrap_or(&[])
    }
    fn slice_mut(&mut self, slot: usize) -> &mut [Compact] {
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
            text: self.texts.get_mut(slot)?,
        })
    }
    /// The row in slot `slot`.
    fn slot_row(&self, slot: usize) -> Option<Row<'_>> {
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
            width: usize::from(m.width),
            text: self.texts.get(slot)?,
            links,
            table: &self.links,
            styles: &self.styles,
        })
    }
    pub fn row_at(&self, index: usize) -> Option<Row<'_>> {
        let Some(row) = index.checked_sub(self.history.len()) else {
            let found = self.history.get(index)?;
            return Some(Row {
                id: found.kept.id,
                version: found.kept.version,
                wrapped: found.kept.wrapped(),
                prompt: found.kept.prompt(),
                cells: found.cells,
                width: usize::from(found.kept.width()),
                text: found.text,
                links: found.links,
                table: &self.links,
                styles: &self.styles,
            });
        };
        self.slot_row(self.screen_slot(row)?)
    }
    pub fn row_by_id(&self, id: RowId) -> Option<Row<'_>> {
        self.index_of(id).and_then(|index| self.row_at(index))
    }
    pub fn index_of(&self, id: RowId) -> Option<usize> {
        self.history.position(id).or_else(|| {
            self.order
                .iter()
                .position(|slot| self.meta.get(*slot).is_some_and(|m| m.id == id))
                .and_then(|row| row.checked_add(self.history.len()))
        })
    }
    pub fn live_row(&self, row: u16) -> Option<Row<'_>> {
        self.slot_row(self.slot(row)?)
    }
    #[inline]
    fn slot(&self, row: u16) -> Option<usize> {
        if row >= self.rows.get() {
            return None;
        }
        self.order.get(usize::from(row)).copied()
    }
    /// A live row's cell, found without making its `Row`: printing asks
    /// for cells on its way, and a `Row` carries the row's links.
    pub fn cell(&self, row: u16, col: u16) -> Option<CellRef<'_>> {
        let slot = self.slot(row)?;
        let cell = self.slice(slot).get(usize::from(col))?;
        Some(cell.read(self.texts.get(slot)?, &self.styles))
    }
    /// A live row's cells as stored.
    pub fn live_cells(&self, row: u16) -> &[Compact] {
        self.slot(row).map_or(&[], |slot| self.slice(slot))
    }
    /// A live row's cell as stored, for what its halves and whether it
    /// has text say, which need neither its text nor its attributes.
    #[inline]
    pub fn stored(&self, row: u16, col: u16) -> Option<&Compact> {
        self.slice(self.slot(row)?).get(usize::from(col))
    }
    /// Edits a live row's cells with `f`, which says whether it changed any
    /// of them; only then does the row take `version`. An edit that leaves
    /// the row as it was leaves its version alone. Every cell `f` makes
    /// other than a blank in the default style is before `end`, which the
    /// row's `used` mark is raised to.
    pub fn mutate_row(
        &mut self,
        row: u16,
        version: u64,
        end: u16,
        f: impl FnOnce(&mut [Compact]) -> bool,
    ) {
        if let Some(slot) = self.slot(row)
            && f(self.slice_mut(slot))
            && let Some(m) = self.meta.get_mut(slot)
        {
            m.version = version;
            m.used = m.used.max(end.min(m.width));
        }
    }
    /// Writes the ASCII `run` from column `col` of live row `row`, in style
    /// `style`, as `mutate_row` writes, taking `version` only if a cell
    /// changed; unless a cell there is half of a wide glyph, which the
    /// general path repairs, when nothing is written and `false` returned.
    #[inline]
    pub fn write_ascii(
        &mut self,
        row: u16,
        col: u16,
        run: &[u8],
        style: u32,
        version: u64,
    ) -> bool {
        let Some(slot) = self.slot(row) else {
            return false;
        };
        let start = usize::from(col);
        let Some(dst) = start
            .checked_add(run.len())
            .and_then(|end| self.slice_mut(slot).get_mut(start..end))
        else {
            return false;
        };
        // One glyph, as at a cursor moved to it: the cell alone, without
        // the set-up the loops below take for a run.
        if let ([cell], [byte]) = (&mut *dst, run) {
            if cell.is_wide() || cell.is_wide_continuation() {
                return false;
            }
            if cell.is_ascii(*byte, style) {
                return true;
            }
            *cell = Compact::ascii(*byte, style);
            if let Some(m) = self.meta.get_mut(slot) {
                let end = u16::try_from(start.saturating_add(1)).unwrap_or(m.width);
                m.version = version;
                m.used = m.used.max(end.min(m.width));
            }
            return true;
        }
        // A short run, a word between colours, cell by cell; a long one by the loops that move words,
        // whose set-up a short one would not repay.
        let halves = if run.len() < LONG_RUN {
            dst.iter().any(|c| c.is_wide() || c.is_wide_continuation())
        } else {
            Compact::any_halves(dst)
        };
        if halves {
            return false;
        }
        // Already these very cells, as a redraw finds them: the row is as
        // it was. New text differs at the first cell.
        let first = dst.first().zip(run.first());
        if first.is_some_and(|(c, b)| c.is_ascii(*b, style)) && unchanged(dst, run, style) {
            return true;
        }
        if run.len() < LONG_RUN {
            for (cell, byte) in dst.iter_mut().zip(run) {
                *cell = Compact::ascii(*byte, style);
            }
        } else {
            Compact::fill_ascii(dst, run, style);
        }
        if let Some(m) = self.meta.get_mut(slot) {
            let end = u16::try_from(start.saturating_add(run.len())).unwrap_or(m.width);
            m.version = version;
            m.used = m.used.max(end.min(m.width));
        }
        true
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

    /// The number of the style with `attributes`, which is added to the
    /// table if they need it and it has none. Adding a style may sweep the
    /// styles, renumbering every cell's: a number got before this call is
    /// not to be used after it.
    pub fn style(&mut self, attributes: Attributes) -> u32 {
        match attributes.inline_style() {
            Some(style) => style,
            None => self.table_style(attributes),
        }
    }

    /// The number of the style with `attributes`, which are no number of
    /// their own (`Attributes::inline_style`): the table's, found among
    /// those found lately if it is there.
    #[inline]
    pub fn table_style(&mut self, attributes: Attributes) -> u32 {
        let at = recent(attributes);
        if let Some(&(seen, id)) = self.recent.get(at)
            && seen == attributes
        {
            return id;
        }
        self.find_table_style(attributes, at)
    }

    /// `table_style` for attributes not found lately, which are then.
    #[inline(never)]
    fn find_table_style(&mut self, attributes: Attributes, at: usize) -> u32 {
        let id = match self.styles.find(attributes) {
            Some(id) => id,
            None => {
                let cells = self.cells.len().saturating_add(self.history.kept_cells());
                if self.styles.wants_sweep(cells) {
                    self.sweep();
                }
                // After a sweep there is room: the styles in use are at
                // most the cells, far fewer than the table holds.
                Arc::make_mut(&mut self.styles)
                    .insert(attributes)
                    .unwrap_or(0)
            }
        };
        if let Some(slot) = self.recent.get_mut(at) {
            *slot = (attributes, id);
        }
        id
    }

    /// Keeps only the styles cells have, numbered anew, and gives every
    /// cell its style's new number (`style.rs`).
    #[cold]
    #[inline(never)]
    pub(crate) fn sweep(&mut self) {
        let mut used = vec![false; self.styles.len()];
        let mut mark = |cell: &Compact| {
            if let Some(mark) = Styles::place(cell.style()).and_then(|i| used.get_mut(i)) {
                *mark = true;
            }
        };
        self.cells.iter().for_each(&mut mark);
        self.history.each_cell(mark);
        let renumber = Arc::make_mut(&mut self.styles).retain(&used);
        let mut renumbered = |cell: &mut Compact| {
            if let Some(&new) = Styles::place(cell.style()).and_then(|i| renumber.get(i)) {
                cell.set_style(new);
            }
        };
        self.cells.iter_mut().for_each(&mut renumbered);
        self.history.each_cell_mut(renumbered);
        self.recent = [(Attributes::default(), 0); RECENT];
        self.epoch = self.epoch.wrapping_add(1);
    }

    /// The styles' epoch (`Grid::epoch`).
    #[inline]
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// How many styles the grid's table holds, in use or not.
    #[cfg(test)]
    pub(crate) fn style_count(&self) -> usize {
        self.styles.len()
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

    /// The links of every row, history's and the screen's, and of slots
    /// recycled since, which no row has.
    fn every_link(&self) -> impl Iterator<Item = &[u16]> {
        self.history
            .links()
            .chain(self.linked.values().map(|row| &**row))
    }

    /// The cells the grid counts for each link, and those a count of its
    /// rows' links finds; and whether every number a row has is a link's.
    #[cfg(test)]
    pub fn link_counts(&self) -> (Vec<u32>, Vec<u32>, bool) {
        let mut fresh = self.links.clone();
        fresh.recount(self.every_link());
        let held = self
            .every_link()
            .flat_map(|row| row.iter())
            .all(|n| *n == 0 || self.links.get(*n).is_some());
        (self.links.counts(), fresh.counts(), held)
    }

    /// Forgets every link: RIS.
    pub fn reset_links(&mut self) {
        self.linked.clear();
        self.history.reset_links();
        self.links = Links::default();
    }

    /// Takes `links` as the grid's links, counting their cells: those of a
    /// grid that this one replaces in a resize, which copied its rows.
    pub fn adopt_links(&mut self, mut links: Links) {
        links.recount(self.every_link());
        self.links = links;
    }

    /// The number of the link of `uri` and `id`, held as a new link with
    /// `key` if it is not held yet. With no room for it, room is made
    /// (`free_links`), at most once in a while after that fails; `None` if
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
            self.free_links(version);
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
    fn free_links(&mut self, version: u64) {
        // The links of slots recycled since, which no row has now; a row
        // leaving history let its links go as it left.
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
        for index in 0..self.history.len() {
            if self.links.used_within_half() {
                break;
            }
            self.history.unlink(index, version, &mut self.links);
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
    /// Blanks columns `start` to `end` of live row `row` in `attributes`,
    /// with the other half of a wide glyph the span splits. The row takes
    /// `version` only if a cell changed.
    ///
    /// The cells from the row's `used` mark on are blank in the default
    /// attributes already, so an erase in those (what programs mostly
    /// send: tmux sends an EL for nearly every line it draws) looks at the
    /// cells before the mark alone, and one that reaches the mark brings
    /// the mark back to `start`: a row erased once costs nothing to erase
    /// again, and nothing to recycle.
    pub fn erase(&mut self, row: u16, start: u16, end: u16, style: u32, version: u64) {
        let (cols, last) = (self.cols.get(), self.cols.last());
        let mut clears_edge = end >= cols;
        let Some(slot) = self.slot(row) else {
            return;
        };
        let Some(&Meta { used, width, .. }) = self.meta.get(slot) else {
            return;
        };
        let plain = style == 0;
        let span_end = usize::from(end.min(cols));
        let first = usize::from(start);
        // The cells that may not be blank in `attributes` yet: in the
        // default attributes, none past the mark.
        let reach = if plain {
            span_end.min(usize::from(used))
        } else {
            span_end
        };
        let blank = Compact::blank(style);
        let cells = self.slice_mut(slot);
        // Already blank in this style up to the first that is not, as an
        // erased tail is: the row is as it was, and a blank is never half
        // a wide glyph, so there is nothing to repair either.
        let differs = cells
            .get(first..reach)
            .and_then(|run| run.iter().position(|c| !c.is_blank(style)));
        let changed = differs.is_some();
        if let Some(differs) = differs {
            // A wide glyph inside the run goes with it; one across either
            // end loses its other half too, which keeps its attributes. A
            // half is always next to its other half (`repair_wide`).
            if let Some(cell) = cells.get(first)
                && !cell.is_wide()
                && cell.is_wide_continuation()
                && let Some(other) = first.checked_sub(1).and_then(|i| cells.get_mut(i))
            {
                *other = other.blanked();
            }
            if let Some(at) = reach.checked_sub(1)
                && cells.get(at).is_some_and(Compact::is_wide)
            {
                if let Some(other) = cells.get_mut(reach) {
                    *other = other.blanked();
                }
                // The glyph's second half is in the last column.
                clears_edge |= reach == usize::from(last);
            }
            if let Some(run) = cells
                .get_mut(first..reach)
                .and_then(|run| run.get_mut(differs..))
            {
                run.fill(blank);
            }
        }
        if let Some(m) = self.meta.get_mut(slot) {
            if changed {
                m.version = version;
                if !plain {
                    m.used = m.used.max(end.min(width));
                }
            }
            if plain && span_end >= usize::from(m.used) {
                // Every cell from `start` on is blank now.
                m.used = m.used.min(start);
            }
            // As `wrap` ends the row's soft wrap.
            if clears_edge && m.wrapped {
                m.wrapped = false;
                m.version = version;
            }
        }
        // A whole row erased keeps no text, and no links: its blank cells
        // would never read them.
        if start == 0 && end >= cols {
            if let Some(text) = self.texts.get_mut(slot) {
                text.clear();
            }
            self.unlink(slot);
        }
    }

    /// `erase`, leaving protected glyphs (DECSCA, SPA) as they are: each
    /// run of unprotected cells between them is erased, as xterm's
    /// `ClearInLine2` erases around them. A wide glyph's second half is
    /// protected if its first half is. Whether there was a protected glyph
    /// in the span.
    #[inline(never)]
    pub fn erase_unprotected(
        &mut self,
        row: u16,
        start: u16,
        end: u16,
        style: u32,
        version: u64,
    ) -> bool {
        let end = end.min(self.cols.get());
        let cells = self.live_cells(row);
        let protected = |at: usize| {
            cells.get(at).is_some_and(|c| {
                c.is_protected()
                    || c.is_wide_continuation()
                        && at
                            .checked_sub(1)
                            .and_then(|i| cells.get(i))
                            .is_some_and(Compact::is_protected)
            })
        };
        let mut runs = Vec::new();
        let mut from = start;
        let mut found = false;
        for col in start..end {
            if protected(usize::from(col)) {
                found = true;
                if from < col {
                    runs.push((from, col));
                }
                from = col.saturating_add(1);
            }
        }
        if from < end {
            runs.push((from, end));
        }
        for (from, to) in runs {
            self.erase(row, from, to, style, version);
        }
        found
    }

    /// ICH and DCH, at the cursor, which they leave where it is; they end
    /// a pending wrap (DEC STD 070, Appendix D.6.1). The cells they bring
    /// in are blank in `blank`. DCH ends the row's soft wrap; ICH, and the
    /// insertion IRM makes, keep it, as xterm does. With left and right
    /// margins they edit up to the right margin, and outside them they do
    /// nothing at all, the pending wrap staying too, as in xterm
    /// (`InsertChar`, `DeleteChar`; DEC STD 070, 5.4.3).
    pub fn edit_cells(&mut self, count: u16, insert: bool, blank: u32, version: u64) {
        if self.lr && !self.in_columns() {
            return;
        }
        self.pending_wrap = false;
        let (row, col) = self.cursor;
        self.edit_row(row, col, count, insert, blank, version);
    }

    /// ICH or DCH of `count` cells at column `col` of live row `row`, up
    /// to the right margin: `edit_cells` at the cursor, and DECIC and DECDC
    /// on each row of the scrolling region. `col` is between the margins.
    pub fn edit_row(
        &mut self,
        row: u16,
        col: u16,
        count: u16,
        insert: bool,
        blank: u32,
        version: u64,
    ) {
        // The cells the edit moves end at the right margin, which is the
        // last column without margins.
        let end = self.right.saturating_add(1).min(self.cols.get());
        // At most the cells from the column to the margin.
        let count = usize::from(count.min(end.saturating_sub(col)));
        if count == 0 {
            return;
        }
        let cols = self.cols.get();
        let end = usize::from(end);
        let blank = Compact::blank(blank);
        self.mutate_row(row, version, cols, |row_cells| {
            let col = usize::from(col);
            // A wide glyph across the right margin loses both halves: the
            // edit moves one and not the other.
            if row_cells
                .get(end)
                .is_some_and(Compact::is_wide_continuation)
            {
                for at in [end.saturating_sub(1), end] {
                    if let Some(c) = row_cells.get_mut(at) {
                        *c = c.blanked();
                    }
                }
            }
            let Some(cells) = row_cells.get_mut(..end) else {
                return false;
            };
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
                if cells
                    .get(boundary)
                    .is_some_and(Compact::is_wide_continuation)
                {
                    if let Some(c) = boundary.checked_sub(1).and_then(|i| cells.get_mut(i)) {
                        *c = c.blanked();
                    }
                    if let Some(c) = cells.get_mut(boundary) {
                        *c = c.blanked();
                    }
                }
            }
            shift(cells, col, count, insert, blank);
            repair_wide(row_cells);
            // Inserting and deleting always count as a change.
            true
        });
        // The cells' links move with them.
        if let Some(slot) = self.slot(row)
            && let Some(row_links) = self.linked.get_mut(&slot)
            && let Some(links) = row_links.get_mut(..end)
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
    /// `free_links` to let go (`Meta::linked` says they are no row's): a
    /// scroll costs what it did before links.
    #[inline]
    fn recycle(&mut self, slot: usize, id: RowId, version: u64) {
        let mut used = usize::from(self.cols.get());
        if let Some(m) = self.meta.get_mut(slot) {
            used = usize::from(m.used);
            *m = Meta::new(id, version, self.cols.get(), false, 0);
        }
        // A row taken into history leaves its slot blank (`take_row`).
        if used > 0 {
            let cells = self.slice_mut(slot);
            let used = used.min(cells.len());
            if let Some(cells) = cells.get_mut(..used) {
                cells.fill(BLANK);
            }
        }
        if let Some(text) = self.texts.get_mut(slot) {
            text.empty();
        }
    }

    /// Gives the blank cells of a row brought in style `blank`, the pen's
    /// colours. They are blanked first in the default style, all zeros,
    /// which compiles to a memset, much faster than storing any other cell;
    /// this goes over them again only for another pen: its callers call it
    /// only for a `blank` that is not 0. Kept out of line so the two are
    /// never fused into one slower loop.
    #[inline(never)]
    fn colour(&mut self, slot: usize, blank: u32) {
        for cell in self.slice_mut(slot) {
            cell.set_style(blank);
        }
        if let Some(m) = self.meta.get_mut(slot) {
            m.used = m.width;
        }
    }

    /// Moves slots, not cells. Only whole-screen upward scrolling enters
    /// history. The rows brought in are blank in `blank`.
    #[inline]
    pub fn scroll(
        &mut self,
        (top, bottom): (u16, u16),
        count: u16,
        direction: Scroll,
        blank: u32,
        next: &mut u64,
        version: u64,
    ) -> Result<(), Error> {
        // A line feed at the bottom of the screen, the commonest scroll,
        // goes straight to history.
        if direction == (Scroll::Up { history: true })
            && top == 0
            && bottom == self.rows.last()
            && self.history_limit > 0
        {
            for _ in 0..count.min(self.rows.get()) {
                self.scroll_into_history(blank, next, version)?;
            }
            return Ok(());
        }
        self.scroll_region((top, bottom), count, direction, blank, next, version)
    }

    /// A scroll inside left and right margins (DEC STD 070, 5.4.3; xterm's
    /// `scrollInMargins`): the cells between the margins of rows `top` to
    /// `bottom` move `count` rows up or down, and those the move leaves
    /// are blank in `blank`. Rows do not move, so each keeps its identity,
    /// its soft wrap and its prompt mark, as xterm keeps a row's flags;
    /// their cells change, with their text and links, and so their
    /// versions. A wide glyph across either margin, in any row of the
    /// region, loses both halves first, as in xterm. Nothing goes into
    /// history.
    pub fn scroll_columns(
        &mut self,
        (top, bottom): (u16, u16),
        count: u16,
        up: bool,
        blank: u32,
        version: u64,
    ) {
        if bottom >= self.rows.get() {
            return;
        }
        let Some(height) = bottom.checked_sub(top).and_then(|h| h.checked_add(1)) else {
            return;
        };
        let count = count.min(height);
        if count == 0 {
            return;
        }
        let (left, right) = (usize::from(self.left), usize::from(self.right));
        let end = right.saturating_add(1);
        for y in top..=bottom {
            self.mutate_row(y, version, 0, |cells| {
                let mut changed = false;
                for edge in [left, end] {
                    if edge > 0 && cells.get(edge).is_some_and(Compact::is_wide_continuation) {
                        for at in [edge.saturating_sub(1), edge] {
                            if let Some(c) = cells.get_mut(at) {
                                *c = c.blanked();
                            }
                        }
                        changed = true;
                    }
                }
                changed
            });
        }
        let span = left..end;
        // Up, each row takes the cells of the row `count` below it, from
        // the top; down, of the row `count` above it, from the bottom. The
        // rows left at the far end are blanked.
        let moved = height.saturating_sub(count);
        for i in 0..moved {
            let (to, from) = if up {
                (
                    top.saturating_add(i),
                    top.saturating_add(i).saturating_add(count),
                )
            } else {
                let to = bottom.saturating_sub(i);
                (to, to.saturating_sub(count))
            };
            self.copy_span(from, to, span.clone(), version);
        }
        for i in 0..count {
            let y = if up {
                bottom.saturating_sub(i)
            } else {
                top.saturating_add(i)
            };
            self.blank_span(y, span.clone(), blank, version);
        }
    }

    /// Copies the cells `span` of live row `from` into live row `to`, with
    /// their text and links; `to` takes `version` if a cell changed.
    fn copy_span(&mut self, from: u16, to: u16, span: Range<usize>, version: u64) {
        let (Some(source), Some(target)) = (self.slot(from), self.slot(to)) else {
            return;
        };
        let cells: Vec<Compact> = self
            .slice(source)
            .get(span.clone())
            .unwrap_or_default()
            .to_vec();
        let text = self.texts.get(source).map(Text::exact).unwrap_or_default();
        let links: Option<Vec<u16>> = if self.meta.get(source).is_some_and(|m| m.linked) {
            self.linked
                .get(&source)
                .and_then(|row| row.get(span.clone()))
                .map(<[u16]>::to_vec)
        } else {
            None
        };
        let mut changed = false;
        if let Some(mut line) = self.line(target) {
            for (i, cell) in cells.iter().enumerate() {
                let at = span.start.saturating_add(i);
                let Some(&old) = line.cells.get(at) else {
                    continue;
                };
                if old.same_as(line.text, cell, &text) {
                    continue;
                }
                changed = true;
                if cell.is_spilled() {
                    line.set(at, *cell, text.of(cell));
                } else if let Some(slot) = line.cells.get_mut(at) {
                    *slot = *cell;
                }
            }
        }
        if changed && let Some(m) = self.meta.get_mut(target) {
            m.version = version;
            let end = u16::try_from(span.end).unwrap_or(m.width);
            m.used = m.used.max(end.min(m.width));
        }
        // The links, a cell at a time: each takes its source's, or none.
        let linked = links.is_some() || self.meta.get(target).is_some_and(|m| m.linked);
        if linked {
            for (i, at) in span.enumerate() {
                let link = links.as_ref().and_then(|l| l.get(i)).copied().unwrap_or(0);
                self.set_link(to, at..at.saturating_add(1), link, version);
            }
        }
    }

    /// Blanks the cells `span` of live row `row` in style `blank`, with no
    /// links, keeping the row's soft wrap; it takes `version` if a cell
    /// changed.
    fn blank_span(&mut self, row: u16, span: Range<usize>, blank: u32, version: u64) {
        let cell = Compact::blank(blank);
        let end = u16::try_from(span.end).unwrap_or(u16::MAX);
        self.mutate_row(row, version, end, |cells| {
            let Some(run) = cells.get_mut(span.clone()) else {
                return false;
            };
            if run.iter().all(|c| *c == cell) {
                return false;
            }
            run.fill(cell);
            true
        });
        self.set_link(row, span, 0, version);
    }

    /// `scroll` within the margins, or down, or without history.
    #[inline]
    fn scroll_region(
        &mut self,
        (top, bottom): (u16, u16),
        count: u16,
        direction: Scroll,
        blank: u32,
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
            let id = next_id(next)?;
            let (from, to) = if up { (top, bottom) } else { (bottom, top) };
            if let Some(slot) = self.move_row(usize::from(from), usize::from(to)) {
                self.recycle(slot, id, version);
                if blank != 0 {
                    self.colour(slot, blank);
                }
            }
            if !up {
                self.wrap(bottom, false, version);
            }
        }
        Ok(())
    }

    /// Moves the screen's top row into history, the oldest row there going
    /// if there are more than the limit, and puts a new blank row under
    /// the screen's last, in style `blank`, with a new identity, in the top
    /// row's slot. Nothing moves if there is no identity or no room in
    /// history.
    #[inline]
    fn scroll_into_history(
        &mut self,
        blank: u32,
        next: &mut u64,
        version: u64,
    ) -> Result<(), Error> {
        // An identity for the new row, taken once nothing can fail.
        let after = next.checked_add(1).ok_or(Error::IdentityExhausted)?;
        let Some(&slot) = self.order.front() else {
            return Ok(());
        };
        let taken = take_row(
            &mut self.history,
            self.cells.as_mut_slice(),
            self.meta.as_mut_slice(),
            self.texts.as_slice(),
            (slot, usize::from(self.cols.get())),
        )?;
        if !taken {
            self.take_row_with_extras(slot)?;
        }
        if self.history.len() > self.history_limit {
            self.history.pop(&mut self.links);
        }
        let id = RowId(*next);
        *next = after;
        if taken {
            // Its cells are blank, and it has neither text nor links: it
            // is a new row once it has a new identity.
            if let Some(m) = self.meta.get_mut(slot) {
                *m = Meta::new(id, version, self.cols.get(), false, 0);
            }
        } else {
            self.recycle(slot, id, version);
        }
        if blank != 0 {
            self.colour(slot, blank);
        }
        self.order.turn();
        Ok(())
    }

    /// `take_row` for a row with text or links: they go with it.
    #[cold]
    #[inline(never)]
    fn take_row_with_extras(&mut self, slot: usize) -> Result<(), Error> {
        let Some(&m) = self.meta.get(slot) else {
            return Ok(());
        };
        let start = slot.saturating_mul(usize::from(self.cols.get()));
        let used = start.saturating_add(usize::from(m.used.min(m.width)));
        let cells = trimmed(self.cells.get(start..used).unwrap_or_default());
        self.history.push(Arriving {
            id: m.id,
            version: m.version,
            wrapped: m.wrapped,
            prompt: m.prompt,
            cells,
            width: m.width,
        })?;
        self.retire_extras(slot, m.linked);
        Ok(())
    }

    /// The text and links of the row in `slot`, which history has just
    /// taken, go with it: a copy of the text, and the links themselves.
    #[cold]
    #[inline(never)]
    fn retire_extras(&mut self, slot: usize, linked: bool) {
        let text = self
            .texts
            .get(slot)
            .filter(|text| !text.is_empty())
            .map(Text::exact);
        let links = if linked {
            self.linked.remove(&slot)
        } else {
            None
        };
        if let Some(m) = self.meta.get_mut(slot) {
            m.linked = false;
        }
        self.history.attach(text, links);
    }

    /// Moves the screen's row at `from` to `to`, the rows between closing
    /// up behind it: a removal then an insertion, one row at a time. Its
    /// slot, or `None`, with nothing moved, if either is off the screen.
    fn move_row(&mut self, from: usize, to: usize) -> Option<usize> {
        let slot = *self.order.get(from)?;
        if to >= self.order.len() {
            return None;
        }
        // The order is a ring, read as two slices from its top. Usually the
        // rows from `from` to `to` lie in one of them: there the move shifts
        // that run by one.
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
            // The rest of the run moves over by one, and the row goes in at
            // the end it moved to: one copy, not the general rotation's. The
            // ranges are the run's own, so `copy_within` cannot panic: the
            // one call outside `bytes::copy_within` (clippy.toml).
            let end = run.len().saturating_sub(1);
            if from < to {
                run.copy_within(1.., 0);
                *run.last_mut()? = slot;
            } else {
                run.copy_within(..end, 1);
                *run.first_mut()? = slot;
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

    /// This grid at `rows` by `cols` without reflow: each row keeps its
    /// cells, cut or padded to the new width (`reflowed` rewraps them). The
    /// new storage is built before anything changes, so a failed allocation
    /// leaves this grid as it was.
    pub fn resized(
        &self,
        rows: u16,
        cols: u16,
        next: &mut u64,
        version: u64,
    ) -> Result<Self, Error> {
        let (rows, cols) = Self::check_size(rows, cols, self.history_limit)?;
        let history = self.history_len();
        // Rows are placed around the cursor, so that the line it is on stays
        // in view (finding 017 of the Bevy version, docs/breaks-audit.md). A shrink drops rows below the cursor first, and only
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
        let mut replacement = Self {
            // A pending wrap is dropped: the cursor goes one past where it
            // waited, as far as the new width allows.
            cursor: (shifted(self.cursor.0), self.next_column().min(cols.last())),
            saved_cursor: (
                shifted(self.saved_cursor.0),
                past(self.saved_cursor.1, self.saved_pending_wrap).min(cols.last()),
            ),
            origin: self.origin,
            saved_origin: self.saved_origin,
            top: self.top,
            bottom: if self.bottom == self.rows.last() {
                rows.last()
            } else {
                self.bottom.min(rows.last())
            },
            ..self.successor(rows, cols)
        };
        if replacement.top > replacement.bottom {
            replacement.top = 0;
        }
        replacement.reserve_screen()?;
        let end = base.checked_add(keep_total).ok_or(Error::Capacity)?;
        // Each row is laid out whole in `row`, as wide as it is, then kept.
        let mut row = Vec::new();
        for (p, source) in (base..end).enumerate() {
            let old = self.row_at(source).filter(|_| source < old_retained);
            let is_history = p < new_history;
            let id = match old {
                Some(r) => r.id,
                None => next_id(next)?,
            };
            let width = match old {
                // A row is never wider than the u16 grid it was made in.
                Some(r) if is_history => u16::try_from(r.width).map_err(|_| Error::Capacity)?,
                Some(_) | None => cols.get(),
            };
            row.clear();
            row.resize(usize::from(width), BLANK);
            let wrapped = old.is_some_and(|r| r.wrapped) && is_history;
            let row_version = if is_history {
                old.map_or(version, |r| r.version)
            } else {
                version
            };
            // As much of the old row's links as fits, as of its cells.
            let links = old.and_then(|r| r.links).map(|links| {
                let mut kept = vec![0; usize::from(width)];
                let len = kept.len().min(links.len());
                if let (Some(dst), Some(src)) = (kept.get_mut(..len), links.get(..len)) {
                    crate::copy_from(dst, src);
                }
                kept.into_boxed_slice()
            });
            let mut text = None;
            if let Some(old) = old {
                // As much of the old row as fits, over the new one's start:
                // both runs are `len` long.
                let len = old.cells.len().min(usize::from(width));
                if let (Some(dst), Some(src)) = (row.get_mut(..len), old.cells.get(..len)) {
                    crate::copy_from(dst, src);
                }
                repair_wide(&mut row);
                // The row's text comes along, stored again within the
                // budget of the row's new width.
                text = Some(Line::rebuilt(&mut row, old.text));
            }
            let prompt = old.is_some_and(|r| r.prompt);
            if is_history {
                let cells = trimmed(&row);
                replacement.history.push(Arriving {
                    id,
                    version: row_version,
                    wrapped,
                    prompt,
                    cells,
                    width,
                })?;
                replacement
                    .history
                    .attach(text.map(|text| text.exact()), links);
            } else {
                let mut meta = Meta::new(id, row_version, width, wrapped, width);
                meta.prompt = prompt;
                replacement.push_screen_row(meta, &row, text, links);
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
        self.reflowed_by(rows, cols, next, version, Lines::Changed)
    }

    /// `reflowed`, laying out cell by cell the lines `lines` says.
    fn reflowed_by(
        &self,
        rows: u16,
        cols: u16,
        next: &mut u64,
        version: u64,
        lines: Lines,
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
        let layout = self.reflow(usize::from(cols.get()), marks, &mut Layout, lines)?;
        // Blank lines below the cursor are dropped before any line scrolls
        // into history: a mostly empty screen keeps its text on screen.
        let screen = usize::from(rows.get());
        let [(cursor_row, cursor_col), (saved_row, saved_col)] = layout.marks;
        // Only as many as the screen's own lines are past the new height:
        // rows in history stay there, and a resize never brings one back
        // above the cursor to fill the place of dropped blank lines.
        let drop = layout
            .trailing_blank
            .min(
                layout
                    .rows
                    .saturating_sub(layout.screen_line)
                    .saturating_sub(screen),
            )
            .min(layout.rows.saturating_sub(cursor_row.saturating_add(1)));
        let total = layout.rows.saturating_sub(drop);
        let live_top = total.saturating_sub(screen).min(cursor_row);
        let base = live_top.saturating_sub(self.history_limit);
        let end = total.min(live_top.checked_add(screen).ok_or(Error::Capacity)?);
        // One past the last column is the last column, waiting to wrap.
        let column = |col: usize| u16::try_from(col).map_or(cols.get(), |col| col.min(cols.get()));
        let (cursor_col, saved_col) = (column(cursor_col), column(saved_col));
        // A row on the screen, the first if above it, the last if below.
        let row = |row: usize| {
            u16::try_from(row.saturating_sub(live_top))
                .map_or(rows.last(), |row| row.min(rows.last()))
        };
        let mut replacement = Self {
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
            ..self.successor(rows, cols)
        };
        replacement.reserve_screen()?;
        let mut copy = Fill {
            grid: &mut replacement,
            rows: base..end,
            screen: live_top,
            next,
            version,
            cells: vec![BLANK; usize::from(cols.get())],
            written: 0,
            text: Text::default(),
            links: None,
        };
        self.reflow(usize::from(cols.get()), marks, &mut copy, lines)?;
        // Blank rows under the last line, if the lines do not fill the screen.
        while replacement.order.len() < screen {
            let meta = Meta::new(next_id(next)?, version, cols.get(), false, 0);
            replacement.push_screen_row(meta, &[], None, None);
        }
        Ok(replacement)
    }

    /// Lays every retained row out again at `width` columns, one logical
    /// line after another, giving each run of cells and each finished row
    /// to `target`. `marks` are retained rows and columns, the cursor's and
    /// the saved cursor's, each laid out as the cursor is. A line is laid
    /// out a run of cells at a time (`lay_out_runs`); at a width under two,
    /// or for every line if `lines` is `Lines::All`, a cell at a time
    /// (`lay_out_cells`), which says what the runs must come to.
    fn reflow(
        &self,
        width: usize,
        marks: [Option<(usize, usize)>; 2],
        target: &mut impl Reflow,
        lines: Lines,
    ) -> Result<Reflowed, Error> {
        let retained = self.retained_len();
        let mut pass = Pass {
            width,
            marks,
            out: Reflowed {
                rows: 0,
                marks: [(0, 0); 2],
                trailing_blank: 0,
                screen_line: 0,
            },
            found: [false; 2],
            target,
        };
        let mut start = 0;
        while start < retained {
            // A line runs through its wrapped rows to the row that ends it.
            let mut end = start;
            while end.checked_add(1).is_some_and(|next| next < retained) && self.wrapped_at(end) {
                end = end.saturating_add(1);
            }
            if (start..=end).contains(&self.history_len()) {
                pass.out.screen_line = pass.out.rows;
            }
            if lines == Lines::Changed && width >= 2 {
                self.lay_out_runs(start, end, &mut pass)?;
            } else {
                self.lay_out_cells(start, end, &mut pass)?;
            }
            start = end.saturating_add(1);
        }
        let mut out = pass.out;
        for (mark, found) in out.marks.iter_mut().zip(pass.found) {
            if !found {
                *mark = (out.rows.saturating_sub(1), 0);
            }
        }
        Ok(out)
    }

    /// Lays out the line of rows `start` through `end` a cell at a time.
    fn lay_out_cells(
        &self,
        start: usize,
        end: usize,
        pass: &mut Pass<'_, impl Reflow>,
    ) -> Result<(), Error> {
        let Pass {
            width,
            marks,
            out,
            found,
            target,
        } = pass;
        let width = *width;
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
            for (offset, mark) in offsets.iter_mut().zip(*marks) {
                if let Some((row_index, col)) = mark
                    && row_index == i
                {
                    *offset = length.checked_add(col);
                }
            }
            if i != end
                && row.padded().next_back().is_some_and(|c| c.same(&BLANK))
                && self
                    .row_at(i.saturating_add(1))
                    .and_then(|next| next.padded().next())
                    .is_some_and(Compact::is_wide)
                && let Some(at) = length
                    .checked_add(row.width)
                    .and_then(|end| end.checked_sub(1))
            {
                spacers.push(at);
            }
            let used = if i == end {
                row.padded()
                    .rposition(|c| !c.same(&BLANK))
                    .map_or(0, |last| last.saturating_add(1))
            } else {
                row.width
            };
            length = length.saturating_add(used);
            if i == end && used == 0 {
                // The tail runs back over earlier rows' blanks too.
                length = self.line_length(start, end);
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
            let (links, text) = (r.links, r.text);
            r.padded().enumerate().map(move |(i, cell)| {
                let link = links.and_then(|l| l.get(i)).copied().unwrap_or(0);
                (cell, text, link)
            })
        });
        for (n, (cell, text, link)) in cells.take(length).enumerate() {
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
                    target.cell(out.rows, used, &BLANK, &NO_TEXT, 0);
                }
                target.row(out.rows, used, true, ids.next(), prompt)?;
                out.rows = out.rows.saturating_add(1);
                used = 0;
                prompt = false;
            }
            for ((offset, mark), placed) in offsets.iter().zip(&mut out.marks).zip(&mut placed) {
                if *offset == Some(n) {
                    *mark = (out.rows, used);
                    *placed = true;
                }
            }
            prompt |= pending;
            pending = false;
            target.cell(out.rows, used, cell, text, link);
            used = used.saturating_add(1);
        }
        // A prompt starting past the line's text starts on its last row.
        prompt |= pending || prompts.iter().any(|at| *at >= length);
        for (((offset, mark), placed), found) in offsets
            .iter()
            .zip(&mut out.marks)
            .zip(&mut placed)
            .zip(found.iter_mut())
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
        target.row(out.rows, used, false, ids.next(), prompt)?;
        out.rows = out.rows.saturating_add(1);
        out.trailing_blank = if length == 0 {
            out.trailing_blank
                .saturating_add(out.rows.saturating_sub(first))
        } else {
            0
        };
        Ok(())
    }

    /// Lays out the line of rows `start` through `end` as `lay_out_cells`
    /// does, `width` being at least two, but a run of cells at a time: as
    /// many of a row's as fit in the row being laid out, short of a wide
    /// glyph that would end in its last column, or a spacer, which no cell
    /// is laid out from. So a line costs its rows and the rows it is laid
    /// out in, not its cells; one whose rows stay as they are (every line
    /// but a few, when only the rows change) costs a run a row. The line's
    /// length comes from its rows' widths and `used` marks.
    fn lay_out_runs(
        &self,
        start: usize,
        end: usize,
        pass: &mut Pass<'_, impl Reflow>,
    ) -> Result<(), Error> {
        let length = self.text_length(start, end);
        let mut line = Run {
            used: 0,
            next: start,
            end,
            prompt: false,
            pending: false,
            offsets: [None; 2],
            placed: [false; 2],
        };
        let first = pass.out.rows;
        let mut base = 0usize;
        for i in start..=end {
            let Some(row) = self.row_at(i) else {
                continue;
            };
            let len = row.width;
            // The row the row's first cell goes to is where the prompt
            // starts after, or the line's last, if no cell goes after it.
            line.pending |= row.prompt;
            for (offset, mark) in line.offsets.iter_mut().zip(pass.marks) {
                if let Some((row_index, col)) = mark
                    && row_index == i
                {
                    *offset = base.checked_add(col);
                }
            }
            let mut taken = len.min(length.saturating_sub(base));
            if i != end && self.spacer(i) {
                // A cursor on a spacer goes with the glyph after it.
                let spacer = base.saturating_add(len).saturating_sub(1);
                for offset in &mut line.offsets {
                    if *offset == Some(spacer) {
                        *offset = spacer.checked_add(1);
                    }
                }
                taken = taken.min(len.saturating_sub(1));
            }
            self.lay_out_run(row, base, taken, &mut line, pass)?;
            base = base.saturating_add(len);
        }
        line.prompt |= line.pending;
        let out = &mut pass.out;
        for (((offset, mark), placed), found) in line
            .offsets
            .iter()
            .zip(&mut out.marks)
            .zip(&mut line.placed)
            .zip(pass.found.iter_mut())
        {
            if let Some(offset) = offset
                && !*placed
            {
                // At or past the end of the line's text: as far past it
                // on the last row, at most waiting to wrap after the
                // last column.
                let past = offset.saturating_sub(length);
                *mark = (out.rows, line.used.saturating_add(past).min(pass.width));
                *placed = true;
            }
            *found |= *placed;
        }
        let id = self.id_at(line.next, end);
        pass.target
            .row(out.rows, line.used, false, id, line.prompt)?;
        out.rows = out.rows.saturating_add(1);
        out.trailing_blank = if length == 0 {
            out.trailing_blank
                .saturating_add(out.rows.saturating_sub(first))
        } else {
            0
        };
        Ok(())
    }

    /// Lays out the first `taken` cells of `row`, which starts `base` cells
    /// into its line, after the line's cells laid out so far.
    fn lay_out_run(
        &self,
        row: Row<'_>,
        base: usize,
        taken: usize,
        line: &mut Run,
        pass: &mut Pass<'_, impl Reflow>,
    ) -> Result<(), Error> {
        let width = pass.width;
        let mut at = 0usize;
        while at < taken {
            if line.used >= width {
                self.end_row(line, pass)?;
            }
            let room = width.saturating_sub(line.used);
            let take = room.min(taken.saturating_sub(at));
            let next = at.saturating_add(take);
            // A wide glyph ending the run in the last column goes to the
            // next row, a blank left in its place.
            let last = next.saturating_sub(1);
            let padded = take == room && row.stored(last).is_some_and(Compact::is_wide);
            if padded {
                line.place(row, base, at..last, pass);
                pass.target
                    .cell(pass.out.rows, line.used, &BLANK, &NO_TEXT, 0);
                self.end_row(line, pass)?;
                line.place(row, base, last..next, pass);
            } else {
                line.place(row, base, at..next, pass);
            }
            at = next;
        }
        Ok(())
    }

    /// Ends the row being laid out, its line going on in the next.
    fn end_row(&self, line: &mut Run, pass: &mut Pass<'_, impl Reflow>) -> Result<(), Error> {
        let id = self.id_at(line.next, line.end);
        line.next = line.next.saturating_add(1);
        pass.target
            .row(pass.out.rows, line.used, true, id, line.prompt)?;
        pass.out.rows = pass.out.rows.saturating_add(1);
        line.used = 0;
        line.prompt = false;
        Ok(())
    }

    /// The length of the line from row `start` through `end` without its
    /// blank tail, which can run back over several rows: as `line_length`
    /// finds it, from the rows' widths and their cells back from their
    /// `used` marks.
    fn text_length(&self, start: usize, end: usize) -> usize {
        let mut length = (start..=end).fold(0usize, |sum, i| sum.saturating_add(self.width_at(i)));
        for i in (start..=end).rev() {
            length = length.saturating_sub(self.width_at(i));
            let text = self.text_end(i);
            if text > 0 {
                return length.saturating_add(text);
            }
        }
        0
    }

    /// Whether retained row `index`, soft-wrapped, ends in a spacer: a
    /// blank that a wide glyph starting the next row did not fit in.
    fn spacer(&self, index: usize) -> bool {
        let last = self.width_at(index).checked_sub(1);
        last.and_then(|col| self.cell_at(index, col))
            .is_some_and(|c| c.is_blank(0))
            && self
                .cell_at(index.saturating_add(1), 0)
                .is_some_and(|c| c.is_wide())
    }

    /// The identity of retained row `index`, if it is at most `end`: the
    /// line's row whose place a row laid out takes.
    fn id_at(&self, index: usize, end: usize) -> Option<RowId> {
        if index > end {
            return None;
        }
        self.row_at(index).map(|row| row.id)
    }

    /// Whether retained row `index` is soft-wrapped.
    fn wrapped_at(&self, index: usize) -> bool {
        match index.checked_sub(self.history.len()) {
            None => self.history.kept(index).is_some_and(|kept| kept.wrapped()),
            Some(row) => self
                .screen_slot(row)
                .and_then(|slot| self.meta.get(slot))
                .is_some_and(|m| m.wrapped),
        }
    }

    /// Retained row `index`'s width: how many cells it has.
    fn width_at(&self, index: usize) -> usize {
        match index.checked_sub(self.history.len()) {
            None => self
                .history
                .kept(index)
                .map_or(0, |kept| usize::from(kept.width())),
            Some(row) => self
                .screen_slot(row)
                .and_then(|slot| self.meta.get(slot))
                .map_or(0, |m| usize::from(m.width)),
        }
    }

    /// Retained row `index`'s cell `col`, if it has one.
    fn cell_at(&self, index: usize, col: usize) -> Option<Compact> {
        if col >= self.width_at(index) {
            return None;
        }
        Some(match index.checked_sub(self.history.len()) {
            None => self.history.cell(index, col),
            Some(row) => self
                .screen_slot(row)
                .and_then(|slot| self.slice(slot).get(col))
                .copied()
                .unwrap_or(BLANK),
        })
    }

    /// How many of retained row `index`'s cells come before its blank
    /// tail: looked for back from its `used` mark, past which every cell
    /// is blank. A history row keeps none of its blank tail.
    fn text_end(&self, index: usize) -> usize {
        let Some(row) = index.checked_sub(self.history.len()) else {
            return self.history.kept(index).map_or(0, |kept| kept.len());
        };
        let Some(slot) = self.screen_slot(row) else {
            return 0;
        };
        let used = self.meta.get(slot).map_or(0, |m| usize::from(m.used));
        let cells = self.slice(slot);
        cells
            .get(..used.min(cells.len()))
            .unwrap_or_default()
            .iter()
            .rposition(|c| !c.is_blank(0))
            .map_or(0, |last| last.saturating_add(1))
    }

    /// How many cells of the line from row `start` through `end` come before
    /// its blank tail, which can run back over several rows.
    fn line_length(&self, start: usize, end: usize) -> usize {
        let mut length = 0usize;
        let mut kept = 0usize;
        for row in (start..=end).filter_map(|i| self.row_at(i)) {
            for cell in row.padded() {
                length = length.saturating_add(1);
                if !cell.same(&BLANK) {
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
            let mut grid = Self::made(
                self.rows.get(),
                self.cols.get(),
                self.history_limit,
                next,
                version,
                self.unmade,
            )?;
            grid.adopt_links(std::mem::take(&mut self.links));
            grid.epoch = self.epoch.wrapping_add(1);
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
    /// `new` would make no smaller (none for an unmade grid's cells).
    pub fn recyclable(&self) -> bool {
        let rows = usize::from(self.rows.get());
        let cols = usize::from(self.cols.get());
        self.history.is_bare()
            && self.order.len() == rows
            && self.meta.len() == rows
            && self.cells.capacity()
                == if self.unmade {
                    0
                } else {
                    rows.saturating_mul(cols)
                }
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
        // No cell has a style but the default now.
        self.styles = Arc::default();
        self.recent = [(Attributes::default(), 0); RECENT];
        self.epoch = self.epoch.wrapping_add(1);
        self.cursor = (0, 0);
        self.pending_wrap = false;
        self.saved_cursor = (0, 0);
        self.saved_pending_wrap = false;
        self.origin = false;
        self.saved_origin = false;
        self.top = 0;
        self.bottom = self.rows.last();
        self.set_columns(0, self.cols.last());
        Ok(())
    }

    /// The cursor and what goes with it: pending wrap, origin mode, the
    /// margins and the saved cursor.
    pub fn clone_cursor(&self) -> CursorState {
        CursorState {
            cursor: self.cursor,
            pending_wrap: self.pending_wrap,
            origin: self.origin,
            margins: (self.top, self.bottom, self.left, self.right),
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
        let (top, bottom, left, right) = state.margins;
        (self.top, self.bottom) = (top, bottom);
        self.set_columns(left, right);
        (
            self.saved_cursor,
            self.saved_pending_wrap,
            self.saved_origin,
        ) = state.saved;
    }
    /// Whether every slot's cells from `used` to its width are blank, as
    /// recycling relies on; an unmade grid's are all blank.
    #[cfg(test)]
    pub fn blank_past_used(&self) -> bool {
        self.unmade
            || (0..self.meta.len()).all(|slot| {
                let used = self.meta.get(slot).map_or(0, |m| usize::from(m.used));
                self.slice(slot)
                    .get(used..)
                    .is_some_and(|tail| tail.iter().all(|c| *c == BLANK))
            })
    }
    pub fn in_region(&self) -> bool {
        (self.top..=self.bottom).contains(&self.cursor.0)
    }
    /// Whether left and right margins narrower than the screen are set
    /// (DECSLRM): every operation they bound looks here first, so without
    /// them each costs this test and no more.
    #[inline]
    pub fn lr(&self) -> bool {
        self.lr
    }
    /// Sets the left and right margins, `left` before `right`, both on the
    /// screen.
    pub fn set_columns(&mut self, left: u16, right: u16) {
        self.left = left;
        self.right = right;
        self.lr = left != 0 || right != self.cols.last();
    }
    /// Whether the cursor is between the left and right margins.
    #[inline]
    pub fn in_columns(&self) -> bool {
        (self.left..=self.right).contains(&self.cursor.1)
    }
    /// One past the last column a glyph printed now may take: the right
    /// margin's, unless the cursor is past it, when the margin is no bound
    /// (DEC STD 070, 5.4.3; xterm's `dotext`). The screen's without margins.
    #[inline]
    pub fn line_end(&self) -> u16 {
        if self.lr {
            return self.line_end_in_margins();
        }
        self.cols.get()
    }
    /// `line_end` with left and right margins. Out of line, as every margin
    /// case is, so that without margins each costs one test.
    #[cold]
    #[inline(never)]
    fn line_end_in_margins(&self) -> u16 {
        if self.cursor.1 <= self.right {
            self.right.saturating_add(1)
        } else {
            self.cols.get()
        }
    }
    /// Where the next glyph goes, before any wrap: the cursor's column, or
    /// one past the last column while a wrap is pending.
    pub fn next_column(&self) -> u16 {
        past(self.cursor.1, self.pending_wrap)
    }
    /// Puts the cursor in column `col` of its row, from where it is; one
    /// past the end of its line (`line_end`) is the line's last column with
    /// a wrap pending.
    #[inline]
    pub fn advance_to(&mut self, col: u16) {
        let end = self.line_end();
        self.advance_within(col, end);
    }
    /// `advance_to`, the line ending at `end`, as `line_end` found it.
    #[inline]
    pub fn advance_within(&mut self, col: u16, end: u16) {
        self.pending_wrap = col >= end;
        self.cursor.1 = col.min(end.saturating_sub(1));
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
    /// The column CR moves the cursor to: the left margin, unless the
    /// cursor is left of it outside origin mode, when it is the first
    /// (xterm's `CarriageReturn`; DEC STD 070 leaves CR at the margin).
    #[inline]
    pub fn carriage_column(&self) -> u16 {
        if self.lr {
            return self.carriage_in_margins();
        }
        0
    }
    #[cold]
    #[inline(never)]
    fn carriage_in_margins(&self) -> u16 {
        if self.origin || self.cursor.1 >= self.left {
            self.left
        } else {
            0
        }
    }
    /// CUP: moves the cursor, within the margins in origin mode, where
    /// lines count from the top margin and columns from the left. Like
    /// every cursor movement, it ends a pending wrap.
    #[inline]
    pub fn position(&mut self, row: u16, col: u16) {
        self.pending_wrap = false;
        // Without margins, the left one is the first column and the right
        // the last: outside origin mode the margins are no bound.
        self.cursor = if self.origin {
            (
                row.saturating_add(self.top).min(self.bottom).max(self.top),
                col.saturating_add(self.left).min(self.right),
            )
        } else {
            (row.min(self.rows.last()), col.min(self.cols.last()))
        };
    }
    /// BS without reverse wraparound: back a column, stopping at the left
    /// margin unless the cursor is already left of it (xterm's
    /// `CursorBack`).
    #[inline]
    pub fn back(&mut self) {
        if self.lr && self.cursor.1 == self.left {
            return;
        }
        self.cursor.1 = self.cursor.1.saturating_sub(1);
    }
    /// The column CUF, or HPR (`absolute`, outside origin mode), moves the
    /// cursor `n` columns right to: no further than the right margin, for
    /// CUF while the cursor is not past it, for HPR in origin mode
    /// (xterm's `CursorForward` and `CASE_HPR`), else the last column.
    #[inline]
    pub fn forward(&self, n: u16, absolute: bool) -> u16 {
        let col = self.cursor.1.saturating_add(n);
        if self.lr {
            return self.forward_in_margins(col, absolute);
        }
        col.min(self.cols.last())
    }
    #[cold]
    #[inline(never)]
    fn forward_in_margins(&self, col: u16, absolute: bool) -> u16 {
        let bounded = if absolute {
            self.origin
        } else {
            self.cursor.1 <= self.right
        };
        col.min(if bounded {
            self.right
        } else {
            self.cols.last()
        })
    }
    /// The column CUB moves the cursor `n` columns left to: no further than
    /// the left margin, unless the cursor is left of it already (xterm's
    /// `CursorBack`).
    #[inline]
    pub fn backward(&self, n: u16) -> u16 {
        let col = self.cursor.1.saturating_sub(n);
        if self.lr && self.cursor.1 >= self.left {
            return col.max(self.left);
        }
        col
    }
    /// The column CHA and HPA move the cursor to, `col` counting from the
    /// left margin in origin mode, no further than the right one.
    #[inline]
    pub fn column(&self, col: u16) -> u16 {
        if self.lr && self.origin {
            return col.saturating_add(self.left).min(self.right);
        }
        col.min(self.cols.last())
    }
    /// Moves the cursor to line `line` as CUP does, keeping its column,
    /// which origin mode keeps within the right margin (VPA and VPR, as
    /// xterm addresses them).
    #[inline]
    pub fn position_line(&mut self, line: u16) {
        let col = self.cursor.1;
        self.position(line, 0);
        self.cursor.1 = if self.origin {
            col.min(self.right)
        } else {
            col
        };
    }
}

/// The column one past `col` if a wrap is pending there, else `col`.
fn past(col: u16, pending_wrap: bool) -> u16 {
    col.saturating_add(u16::from(pending_wrap))
}

/// Where a reflow's rows go: `Layout` only counts them; `Fill` writes the
/// ones kept into the replacement grid.
trait Reflow {
    /// `cell`, of a row whose text is in `text`, with link `link`, is at
    /// `col` of reflowed row `row`.
    fn cell(&mut self, row: usize, col: usize, cell: &Compact, text: &Text, link: u16);
    /// `cells`, of a row whose text is in `text`, with links `links` (as
    /// many as it has, the rest none), are at `col` on of reflowed row
    /// `row`, where they fit; the row's cells past them that the run
    /// covers, which it does not keep, are blank.
    fn run(&mut self, row: usize, col: usize, cells: &[Compact], text: &Text, links: &[u16]);
    /// Reflowed row `row` is finished, its cells from `used` on blank;
    /// `wrapped` if its line goes on, the identity of its line's row in the
    /// same place before, if any, and whether a prompt starts on it.
    fn row(
        &mut self,
        row: usize,
        used: usize,
        wrapped: bool,
        id: Option<RowId>,
        prompt: bool,
    ) -> Result<(), Error>;
}

/// How a reflow lays out lines: a run of cells at a time where it can, or
/// every line a cell at a time, as the oracle the runs are checked against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lines {
    Changed,
    #[cfg(test)]
    All,
}

/// A reflow under way: the width it lays out at, the marks it places, what
/// it has laid out, which marks it has found, and where the rows go.
struct Pass<'a, T> {
    width: usize,
    marks: [Option<(usize, usize)>; 2],
    out: Reflowed,
    found: [bool; 2],
    target: &'a mut T,
}

/// A line being laid out a run at a time (`Grid::lay_out_runs`).
#[derive(Debug)]
struct Run {
    /// The cells laid out in the row being laid out.
    used: usize,
    /// The line's row whose identity the next row laid out takes, and its
    /// last row.
    next: usize,
    end: usize,
    /// Whether a prompt starts on the row being laid out, or on the row
    /// the next cell placed goes to.
    prompt: bool,
    pending: bool,
    /// Where the marks are in the line, and whether they have been placed.
    offsets: [Option<usize>; 2],
    placed: [bool; 2],
}

impl Run {
    /// Lays out cells `cells` of `row`, which starts `base` cells into the
    /// line, in the row being laid out, after its cells so far: they fit.
    fn place(
        &mut self,
        row: Row<'_>,
        base: usize,
        cells: Range<usize>,
        pass: &mut Pass<'_, impl Reflow>,
    ) {
        if cells.is_empty() || cells.end > row.width {
            return;
        }
        // The cells the row keeps of the run; the rest are blank.
        let kept = row.cells.len();
        let run = row
            .cells
            .get(cells.start.min(kept)..cells.end.min(kept))
            .unwrap_or_default();
        let start = base.saturating_add(cells.start);
        let end = base.saturating_add(cells.end);
        for ((offset, mark), placed) in self
            .offsets
            .iter()
            .zip(&mut pass.out.marks)
            .zip(&mut self.placed)
        {
            if let Some(offset) = *offset
                && (start..end).contains(&offset)
            {
                *mark = (
                    pass.out.rows,
                    self.used.saturating_add(offset.saturating_sub(start)),
                );
                *placed = true;
            }
        }
        self.prompt |= self.pending;
        self.pending = false;
        let links = row.links.map_or(&[][..], |links| {
            let len = links.len();
            links
                .get(cells.start.min(len)..cells.end.min(len))
                .unwrap_or_default()
        });
        pass.target
            .run(pass.out.rows, self.used, run, row.text, links);
        self.used = self.used.saturating_add(cells.len());
    }
}

/// The shape of a reflow: how many rows, where the cursor and the saved
/// cursor are, and how many rows at the end hold blank lines.
struct Reflowed {
    rows: usize,
    marks: [(usize, usize); 2],
    trailing_blank: usize,
    /// The laid-out row the screen's line begins on: the line holding the
    /// screen's first row before.
    screen_line: usize,
}

struct Layout;
impl Reflow for Layout {
    fn cell(&mut self, _: usize, _: usize, _: &Compact, _: &Text, _: u16) {}
    fn run(&mut self, _: usize, _: usize, _: &[Compact], _: &Text, _: &[u16]) {}
    fn row(&mut self, _: usize, _: usize, _: bool, _: Option<RowId>, _: bool) -> Result<(), Error> {
        Ok(())
    }
}

/// Writes reflowed rows `rows` into `grid`, which has none yet: each row is
/// laid out in `cells`, `text` and `links`, then goes into the grid's
/// history if it is above `screen`, else onto its screen. A cluster held in
/// its old row's text is stored again in its new row's.
struct Fill<'a> {
    grid: &'a mut Grid,
    rows: Range<usize>,
    screen: usize,
    next: &'a mut u64,
    version: u64,
    cells: Vec<Compact>,
    /// How far into `cells` the row being laid out was written: past it,
    /// they are blank.
    written: usize,
    text: Text,
    links: Option<Box<[u16]>>,
}
impl Fill<'_> {
    /// The links of the row being laid out, none at first.
    fn links(&mut self) -> &mut [u16] {
        let width = self.cells.len();
        self.links
            .get_or_insert_with(|| vec![0; width].into_boxed_slice())
    }
}
impl Reflow for Fill<'_> {
    fn cell(&mut self, row: usize, col: usize, cell: &Compact, text: &Text, link: u16) {
        if !self.rows.contains(&row) || col >= self.cells.len() {
            return;
        }
        self.written = self.written.max(col.saturating_add(1));
        if link != 0
            && let Some(at) = self.links().get_mut(col)
        {
            *at = link;
        }
        let stored = *cell;
        if !stored.is_spilled() {
            if let Some(target) = self.cells.get_mut(col) {
                *target = stored;
            }
            return;
        }
        Line {
            cells: &mut self.cells,
            text: &mut self.text,
        }
        .set(col, stored, text.of(cell));
    }
    fn run(&mut self, row: usize, col: usize, cells: &[Compact], text: &Text, links: &[u16]) {
        if !self.rows.contains(&row) {
            return;
        }
        let Some(dst) = col
            .checked_add(cells.len())
            .and_then(|end| self.cells.get_mut(col..end))
        else {
            return;
        };
        self.written = self.written.max(col.saturating_add(cells.len()));
        crate::copy_from(dst, cells);
        // The clusters held in the row's text are stored again, left to
        // right, as placing the cells one at a time stores them: until
        // then, a cell locates nothing in the new row's text.
        if !text.is_empty() && cells.iter().any(Compact::is_spilled) {
            for (cell, src) in dst.iter_mut().zip(cells) {
                if src.is_spilled() {
                    *cell = src.blanked();
                }
            }
            let mut line = Line {
                cells: &mut self.cells,
                text: &mut self.text,
            };
            for (i, cell) in cells.iter().enumerate() {
                if cell.is_spilled() {
                    line.set(col.saturating_add(i), *cell, text.of(cell));
                }
            }
        }
        if links.iter().any(|link| *link != 0)
            && let Some(dst) = col
                .checked_add(links.len())
                .and_then(|end| self.links().get_mut(col..end))
        {
            crate::copy_from(dst, links);
        }
    }
    fn row(
        &mut self,
        row: usize,
        used: usize,
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
        let cols = self.grid.cols.get();
        // The row's `used` mark is where its cells laid out end, so the
        // next reflow finds its text, and recycling clears it, from there.
        let used = u16::try_from(used).map_or(cols, |used| used.min(cols));
        // Past the mark, every cell is blank: no half of a wide glyph.
        if let Some(cells) = self.cells.get_mut(..usize::from(used)) {
            repair_wide(cells);
        }
        let text = std::mem::take(&mut self.text);
        let links = self.links.take();
        let written = std::mem::take(&mut self.written);
        if row < self.screen {
            let cells = trimmed(self.cells.get(..written).unwrap_or_default());
            self.grid.history.push(Arriving {
                id,
                version: self.version,
                wrapped,
                prompt,
                cells,
                width: cols,
            })?;
            if !text.is_empty() || links.is_some() {
                self.grid.history.attach(Some(text.exact()), links);
            }
        } else {
            let mut meta = Meta::new(id, self.version, cols, wrapped, used);
            meta.prompt = prompt;
            self.grid
                .push_screen_row(meta, &self.cells, Some(text), links);
        }
        if let Some(cells) = self.cells.get_mut(..written) {
            cells.fill(BLANK);
        }
        Ok(())
    }
}

/// The rows' links, by slot.
pub(crate) type Linked = HashMap<usize, Box<[u16]>, BuildHasherDefault<SlotHasher>>;

/// Hashes a slot, or a row's number, a small number, by one multiplication
/// (Fibonacci hashing), rather than by SipHash, which the default hasher
/// spends on every lookup: neither is input a program chooses.
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
        self.write_u64(n);
    }
    fn write_u64(&mut self, n: u64) {
        self.0 = (self.0 ^ n).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    }
}

/// Drops slot `slot`'s links, which its cells no longer have. Out of line,
/// so that the paths that recycle and erase rows, which only call it for a
/// row with links, carry none of it.
#[cold]
#[inline(never)]
fn forget(linked: &mut Linked, links: &mut Links, slot: usize) {
    if let Some(row) = linked.remove(&slot) {
        links.release_all(&row);
    }
}

/// The run of ASCII from which `write_ascii` writes a word a cell.
const LONG_RUN: usize = 8;

/// Whether `cells` are already the ASCII `run` in style `style`. Kept out
/// of line, so that the write that usually follows compiles as if it were
/// not there.
#[inline(never)]
fn unchanged(cells: &[Compact], run: &[u8], style: u32) -> bool {
    cells.iter().zip(run).all(|(c, b)| c.is_ascii(*b, style))
}

/// ICH and DCH on a row's cells, or on their links: what is at and after
/// `col` turns `count` places right to insert, left to delete, and the
/// places it leaves are `blank`.
fn shift<T: Copy>(cells: &mut [T], col: usize, count: usize, insert: bool, blank: T) {
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

pub(crate) fn repair_wide(cells: &mut [Compact]) {
    for i in 0..cells.len() {
        let invalid = cells.get(i).is_some_and(|c| {
            c.is_wide()
                && !i
                    .checked_add(1)
                    .and_then(|j| cells.get(j))
                    .is_some_and(Compact::is_wide_continuation)
                || c.is_wide_continuation()
                    && !i
                        .checked_sub(1)
                        .and_then(|j| cells.get(j))
                        .is_some_and(Compact::is_wide)
        });
        if invalid && let Some(cell) = cells.get_mut(i) {
            *cell = cell.blanked();
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;
