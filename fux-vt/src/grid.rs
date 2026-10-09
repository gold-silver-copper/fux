use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::ops::Range;
use std::sync::Arc;

use crate::compact::{BLANK, Compact, Line, Text};
use crate::geometry::{Size, Span};
use crate::history::{Arriving, History, trimmed};
use crate::link::Links;
use crate::style::Styles;
use crate::{Attributes, CellRef, Error, Row, RowId};

/// The cursor, and what DECSC saves with it. Only the grid's movements set
/// its row and column, each keeping them on the grid: a glyph that reaches
/// the last column leaves the cursor there with `pending_wrap` set.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Cursor {
    row: u16,
    col: u16,
    /// DEC STD 070's Last Column Flag (Appendix D.6.1): a glyph went into
    /// the last column, so the next one, with autowrap on, first moves to
    /// the start of the next line.
    pub pending_wrap: bool,
    /// Origin mode (DECOM).
    pub origin: bool,
}

impl Cursor {
    pub fn row(&self) -> u16 {
        self.row
    }
    pub fn col(&self) -> u16 {
        self.col
    }
    /// Its row and column.
    pub fn at(&self) -> (u16, u16) {
        (self.row, self.col)
    }
    /// This cursor at `row` and `col`, or as near as a grid of `size` has.
    fn placed(self, (row, col): (u16, u16), size: Size) -> Self {
        Self {
            row: row.min(size.lines().last()),
            col: col.min(size.columns().last()),
            ..self
        }
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
/// Inlined into its callers: without, scrolling counts 5% more
/// instructions (vt/scrolling, fux-bench).
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
    /// The row's width: for a row of the screen, always the grid's; it goes
    /// with a row into history, which keeps rows of the widths they had.
    width: u16,
    wrapped: bool,
    /// How far into the row a cell may differ from a blank in the default
    /// attributes: every cell from here to `width` is one, so recycling the
    /// slot clears only the cells before it, and a row scrolled into
    /// history is looked at no further.
    used: u16,
    /// Whether the row's links are its slot's array in `Grid::linked`. An
    /// array for a slot whose row is not linked is a recycled row's: stale,
    /// read by no one, and still counted until links are freed
    /// (`free_links`).
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
    size: Size,
    pub history_limit: usize,
    pub cursor: Cursor,
    /// The cursor DECSC saved, which DECRC restores.
    pub saved: Cursor,
    /// The top and bottom margins (DECSTBM): the scrolling region.
    pub lines: Span,
    /// The left and right margins (DECSLRM, with DECLRMM set), set with
    /// `set_columns` alone, which keeps `lr`.
    columns: Span,
    /// Whether `columns` is narrower than the screen: one test for every
    /// operation the margins bound, which without them does as it did.
    lr: bool,
    /// Whether its screen's cells are yet to be made (`Grid::unmade`):
    /// every row is blank, and `make` makes them.
    unmade: bool,
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
    pub fn check_size(size: Size, history: usize) -> Result<(), Error> {
        let retained = history
            .checked_add(usize::from(size.rows()))
            .ok_or(Error::Capacity)?;
        if retained > MAX_ROWS
            || retained
                .checked_mul(usize::from(size.cols()))
                .is_none_or(|n| n > MAX_CELLS)
        {
            return Err(Error::Capacity);
        }
        Ok(())
    }

    pub fn new(
        size: Size,
        history_limit: usize,
        next: &mut u64,
        version: u64,
    ) -> Result<Self, Error> {
        Self::made(size, history_limit, next, version, false)
    }

    /// `new`, but with no storage for the screen's cells until `make`
    /// makes it: the rows, their identities and versions, the cursor and
    /// the margins are all `new`'s. Blank rows read as blank, and resizing
    /// and clearing keep it unmade (`resized`, `clear`); anything else is
    /// for a made grid. The alternate screen is made so, as most programs
    /// never show it.
    pub fn unmade(size: Size, next: &mut u64, version: u64) -> Result<Self, Error> {
        Self::made(size, 0, next, version, true)
    }

    /// Gives an unmade grid (`unmade`) its screen's cells, blank, as `new`
    /// would have made them; nothing if it has them. On failure, it stays
    /// as it was.
    pub fn make(&mut self) -> Result<(), Error> {
        if !self.unmade {
            return Ok(());
        }
        let size = self.size.cells();
        self.cells
            .try_reserve_exact(size)
            .map_err(|_| Error::Capacity)?;
        self.cells.resize(size, BLANK);
        self.unmade = false;
        Ok(())
    }

    fn made(
        size: Size,
        history_limit: usize,
        next: &mut u64,
        version: u64,
        unmade: bool,
    ) -> Result<Self, Error> {
        Self::check_size(size, history_limit)?;
        let mut grid = Self {
            unmade,
            ..Self::bare(size, history_limit)
        };
        grid.reserve_screen()?;
        for _ in 0..size.rows() {
            let id = next_id(next)?;
            grid.push_screen_row(
                Meta::new(id, version, size.cols(), false, 0),
                &[],
                None,
                None,
            );
        }
        Ok(grid)
    }

    /// A grid of `size` with no row yet, nor storage for one.
    fn bare(size: Size, history_limit: usize) -> Self {
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
            size,
            history_limit,
            cursor: Cursor::default(),
            saved: Cursor::default(),
            lines: size.lines(),
            columns: size.columns(),
            lr: false,
            unmade: false,
        }
    }

    /// A grid with no row yet that takes this one's place: the same styles,
    /// and their numbers, which the rows it is given keep; unmade if this
    /// one is.
    fn successor(&self, size: Size) -> Self {
        Self {
            styles: Arc::clone(&self.styles),
            recent: self.recent,
            epoch: self.epoch,
            unmade: self.unmade,
            ..Self::bare(size, self.history_limit)
        }
    }

    /// Room for the screen's rows, exactly; their cells' only once made.
    fn reserve_screen(&mut self) -> Result<(), Error> {
        // Within the limits: `check_size` came first.
        let rows = usize::from(self.size.rows());
        let cells = if self.unmade { 0 } else { self.size.cells() };
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
        let cols = usize::from(self.size.cols());
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

    pub fn size(&self) -> Size {
        self.size
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
        let start = slot.checked_mul(usize::from(self.size.cols()))?;
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
    /// The row in slot `slot`, row `row` of the screen.
    fn slot_row(&self, slot: usize, row: usize) -> Option<Row<'_>> {
        let m = self.meta.get(slot)?;
        let links = if m.linked {
            self.linked.get(&slot).map(|links| &**links)
        } else {
            None
        };
        Some(Row {
            grid: self,
            index: self.history.len().checked_add(row)?,
            id: m.id,
            version: m.version,
            wrapped: m.wrapped,
            prompt: m.prompt,
            cells: self.slice(slot),
            width: usize::from(m.width),
            text: self.texts.get(slot)?,
            links,
            styles: &self.styles,
        })
    }
    pub fn row_at(&self, index: usize) -> Option<Row<'_>> {
        let Some(row) = index.checked_sub(self.history.len()) else {
            let found = self.history.get(index)?;
            return Some(Row {
                grid: self,
                index,
                id: found.kept.id,
                version: found.kept.version,
                wrapped: found.kept.wrapped(),
                prompt: found.kept.prompt(),
                cells: found.cells,
                width: usize::from(found.kept.width()),
                text: found.text,
                links: found.links,
                styles: &self.styles,
            });
        };
        self.slot_row(self.screen_slot(row)?, row)
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
        self.slot_row(self.slot(row)?, usize::from(row))
    }
    #[inline]
    fn slot(&self, row: u16) -> Option<usize> {
        if row >= self.size.rows() {
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

    /// `table_style` for attributes not found lately, which are then. Out
    /// of line, so that `table_style`, which finds a pen used lately in a
    /// few compares, stays small where it is inlined.
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

    /// Forgets every link: RIS. No row is linked after, so a row given a
    /// link again makes its array anew (`set_link`).
    pub fn reset_links(&mut self) {
        for m in &mut self.meta {
            m.linked = false;
        }
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
        self.links.free_unused();
        if self.links.used_within_half() {
            return;
        }
        for index in 0..self.history.len() {
            if self.links.used_within_half() {
                break;
            }
            self.history.unlink(index, version, &mut self.links);
        }
        self.links.free_unused();
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
        let (cols, last) = (self.size.cols(), self.size.columns().last());
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
                text.release();
            }
            self.unlink(slot);
        }
    }

    /// ED (`display`) or EL in `mode`, 0 to 2, from the cursor, ending a
    /// pending wrap: `erase` erases each row's span, and says whether it
    /// found something it left; whether any did. A row ED erases whole is
    /// no prompt's, as in Ghostty; EL, and ED's part of the cursor's row,
    /// leave the mark (a shell redrawing its prompt erases from it).
    #[inline]
    pub fn erase_in(
        &mut self,
        display: bool,
        mode: u16,
        mut erase: impl FnMut(&mut Self, u16, u16, u16) -> bool,
    ) -> bool {
        self.cursor.pending_wrap = false;
        let ((row, col), cols) = (self.cursor.at(), self.size.cols());
        let mut found = false;
        if display {
            for y in 0..self.size.rows() {
                if (mode == 0 && y > row) || (mode == 1 && y < row) || mode == 2 {
                    found |= erase(self, y, 0, cols);
                    self.clear_prompt(y);
                }
            }
        }
        let (start, end) = match mode {
            0 => (col, cols),
            1 => (0, col.saturating_add(1).min(cols)),
            _ => (0, cols),
        };
        erase(self, row, start, end) | found
    }

    /// `erase`, leaving protected glyphs (DECSCA, SPA) as they are: each
    /// run of unprotected cells between them is erased, as xterm's
    /// `ClearInLine2` erases around them. A wide glyph's second half is
    /// protected if its first half is. Whether there was a protected glyph
    /// in the span. Out of line, as protection is rare.
    #[inline(never)]
    pub fn erase_unprotected(
        &mut self,
        row: u16,
        start: u16,
        end: u16,
        style: u32,
        version: u64,
    ) -> bool {
        let end = end.min(self.size.cols());
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
        self.cursor.pending_wrap = false;
        let (row, col) = self.cursor.at();
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
        let end = self.columns.end();
        // At most the cells from the column to the margin.
        let count = usize::from(count.min(end.saturating_sub(col)));
        if count == 0 {
            return;
        }
        let cols = self.size.cols();
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
        let mut used = usize::from(self.size.cols());
        if let Some(m) = self.meta.get_mut(slot) {
            used = usize::from(m.used);
            *m = Meta::new(id, version, self.size.cols(), false, 0);
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
        region: Span,
        count: u16,
        direction: Scroll,
        blank: u32,
        next: &mut u64,
        version: u64,
    ) -> Result<(), Error> {
        // A line feed at the bottom of the screen, the commonest scroll,
        // goes straight to history.
        if direction == (Scroll::Up { history: true })
            && region == self.size.lines()
            && self.history_limit > 0
        {
            for _ in 0..count.min(self.size.rows()) {
                self.scroll_into_history(blank, next, version)?;
            }
            return Ok(());
        }
        self.scroll_region(region, count, direction, blank, next, version)
    }

    /// A scroll inside left and right margins (DEC STD 070, 5.4.3; xterm's
    /// `scrollInMargins`): the cells between the margins of rows `top` to
    /// `region` move `count` rows up or down, and those the move leaves
    /// are blank in `blank`. Rows do not move, so each keeps its identity,
    /// its soft wrap and its prompt mark, as xterm keeps a row's flags;
    /// their cells change, with their text and links, and so their
    /// versions. A wide glyph across either margin, in any row of the
    /// region, loses both halves first, as in xterm. Nothing goes into
    /// history.
    pub fn scroll_columns(&mut self, region: Span, count: u16, up: bool, blank: u32, version: u64) {
        let (top, bottom, height) = (region.first(), region.last(), region.len());
        let count = count.min(height);
        if count == 0 {
            return;
        }
        let left = usize::from(self.columns.first());
        let end = usize::from(self.columns.end());
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
        region: Span,
        count: u16,
        direction: Scroll,
        blank: u32,
        next: &mut u64,
        version: u64,
    ) -> Result<(), Error> {
        let up = direction != Scroll::Down;
        let (top, bottom) = (region.first(), region.last());
        for _ in 0..count.min(region.len()) {
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
            (slot, usize::from(self.size.cols())),
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
                *m = Meta::new(id, version, self.size.cols(), false, 0);
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
        let start = slot.saturating_mul(usize::from(self.size.cols()));
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
    pub fn resized(&self, size: Size, next: &mut u64, version: u64) -> Result<Self, Error> {
        Self::check_size(size, self.history_limit)?;
        let (rows, cols) = (size.rows(), size.cols());
        let history = self.history_len();
        // Rows are placed around the cursor, so that the line it is on stays
        // in view (docs/breaks-audit.md, 017: a resized pane lost its bottom
        // line). A shrink drops rows below the cursor first, and only then
        // scrolls rows above it into history: a screen with its content at
        // the top keeps it, and a full screen keeps its bottom line. A grow
        // pulls rows back from history above, as xterm does, and pads the rest
        // with blank rows below. history_limit bounds history, oldest first.
        let old_retained = self.retained_len();
        let live_top = match rows.checked_sub(self.size.rows()) {
            // A grow takes back as many history rows as there are.
            Some(grown) if grown > 0 => history.saturating_sub(usize::from(grown)),
            // A shrink scrolls up only the rows from the top through the
            // cursor's that no longer fit.
            Some(_) | None => {
                let through_cursor = usize::from(self.cursor.row)
                    .checked_add(1)
                    .ok_or(Error::Capacity)?;
                history
                    .checked_add(through_cursor.saturating_sub(usize::from(rows)))
                    .ok_or(Error::Capacity)?
            }
        };
        let new_history = live_top.min(self.history_limit);
        // Rows past the history limit are dropped, oldest first.
        let base = live_top.saturating_sub(self.history_limit);
        let keep_total = new_history
            .checked_add(usize::from(rows))
            .ok_or(Error::Capacity)?;
        // Both cursors move with the rows they sit on, and stop at the last.
        // They keep their columns, as far as the new width allows, and a
        // wrap they wait on, as xterm keeps it at any width (DEC STD 070,
        // Appendix D.6.1: a resize is no movement that ends it). The scroll
        // region is reset, as xterm does, and as `reflowed` does.
        let shifted = |cursor: Cursor| {
            let index = history.checked_add(usize::from(cursor.row));
            let row = index.map_or(usize::MAX, |i| i.saturating_sub(live_top));
            let row = u16::try_from(row).unwrap_or(u16::MAX);
            cursor.placed((row, cursor.col), size)
        };
        let mut replacement = Self {
            cursor: shifted(self.cursor),
            saved: shifted(self.saved),
            ..self.successor(size)
        };
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
                Some(_) | None => cols,
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

    /// Starts the grid again, as `new` makes it. A grid that holds only its
    /// live rows, in storage made for them, keeps that storage: its rows are
    /// blanked and given new identities, top to bottom, as `new` gives them.
    pub fn clear(&mut self, next: &mut u64, version: u64) -> Result<(), Error> {
        if !self.recyclable() {
            // The links are kept, as a link the program has open keeps its
            // number (`Screen::pen_link`).
            let mut grid = Self::made(self.size, self.history_limit, next, version, self.unmade)?;
            grid.adopt_links(std::mem::take(&mut self.links));
            grid.epoch = self.epoch.wrapping_add(1);
            *self = grid;
            return Ok(());
        }
        // `new` would run out of identities partway, having taken those
        // before; the rows are left as they were.
        if next.checked_add(u64::from(self.size.rows())).is_none() {
            *next = u64::MAX;
            return Err(Error::IdentityExhausted);
        }
        self.renew(next, version)
    }

    /// Whether `clear` can start the grid again in the storage it has: it
    /// holds its live rows alone, each as wide as the grid, in storage that
    /// `new` would make no smaller (none for an unmade grid's cells).
    pub fn recyclable(&self) -> bool {
        let rows = usize::from(self.size.rows());
        let cells = if self.unmade { 0 } else { self.size.cells() };
        self.history.is_bare()
            && self.order.len() == rows
            && self.meta.len() == rows
            && self.cells.capacity() == cells
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
        self.cursor = Cursor::default();
        self.saved = Cursor::default();
        self.reset_margins();
        Ok(())
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
    /// Whether left and right margins narrower than the screen are set
    /// (DECSLRM): every operation they bound looks here first, so without
    /// them each costs this test and no more.
    #[inline]
    pub fn lr(&self) -> bool {
        self.lr
    }
    /// The left and right margins.
    pub fn columns(&self) -> Span {
        self.columns
    }
    /// Sets the left and right margins.
    pub fn set_columns(&mut self, columns: Span) {
        self.columns = columns;
        self.lr = columns != self.size.columns();
    }
    /// The margins at the screen's edges, as resets put them.
    pub fn reset_margins(&mut self) {
        self.lines = self.size.lines();
        self.set_columns(self.size.columns());
    }
    /// Whether the cursor is between the left and right margins.
    #[inline]
    pub fn in_columns(&self) -> bool {
        self.columns.contains(self.cursor.col)
    }
    /// One past the last column a glyph printed now may take: the right
    /// margin's, unless the cursor is past it, when the margin is no bound
    /// (DEC STD 070, 5.4.3; xterm's `dotext`). The screen's without margins.
    #[inline]
    pub fn line_end(&self) -> u16 {
        if self.lr {
            return self.line_end_in_margins();
        }
        self.size.cols()
    }
    /// `line_end` with left and right margins. Out of line, as every margin
    /// case is, so that without margins each costs one test.
    #[cold]
    #[inline(never)]
    fn line_end_in_margins(&self) -> u16 {
        if self.cursor.col <= self.columns.last() {
            self.columns.end()
        } else {
            self.size.cols()
        }
    }
    /// Where the next glyph goes, before any wrap: the cursor's column, or
    /// one past the last column while a wrap is pending.
    pub fn next_column(&self) -> u16 {
        past(self.cursor.col, self.cursor.pending_wrap)
    }
    /// Puts the cursor in column `col` of its row, from where it is; one
    /// past the end of its line (`line_end`) is the line's last column with
    /// a wrap pending.
    #[inline]
    pub fn advance_to(&mut self, col: u16) {
        let end = self.line_end();
        self.advance_within(col, end);
    }
    /// `advance_to`, the line ending at `end`, as `line_end` found it: at
    /// most the screen's width, so the cursor stays on it.
    #[inline]
    pub fn advance_within(&mut self, col: u16, end: u16) {
        self.cursor.pending_wrap = col >= end;
        self.cursor.col = col.min(end.saturating_sub(1));
    }
    /// Moves the cursor to row `row`, the last if past it.
    #[inline]
    pub fn set_row(&mut self, row: u16) {
        self.cursor.row = row.min(self.size.lines().last());
    }
    /// Moves the cursor to column `col`, the last if past it.
    #[inline]
    pub fn set_col(&mut self, col: u16) {
        self.cursor.col = col.min(self.size.columns().last());
    }
    /// The cursor's line as CUP addresses it: from the top margin in
    /// origin mode.
    pub fn cursor_line(&self) -> u16 {
        self.cursor.row.saturating_sub(self.addressed().0.first())
    }
    /// CR: the cursor to the left margin, unless it is left of it outside
    /// origin mode, when to the first column (xterm's `CarriageReturn`; DEC
    /// STD 070 leaves CR at the margin).
    #[inline]
    pub fn carriage_return(&mut self) {
        self.cursor.col = if self.lr {
            self.carriage_in_margins()
        } else {
            0
        };
    }
    #[cold]
    #[inline(never)]
    fn carriage_in_margins(&self) -> u16 {
        if self.cursor.origin || self.cursor.col >= self.columns.first() {
            self.columns.first()
        } else {
            0
        }
    }
    /// CUP: moves the cursor, within the margins in origin mode, where
    /// lines count from the top margin and columns from the left. Like
    /// every cursor movement, it ends a pending wrap.
    #[inline]
    pub fn position(&mut self, row: u16, col: u16) {
        self.cursor.pending_wrap = false;
        let (lines, columns) = self.addressed();
        (self.cursor.row, self.cursor.col) = (lines.nth(row), columns.nth(col));
    }
    /// The rows and columns CUP addresses: between the margins in origin
    /// mode, else the whole screen, as the margins are no bound outside it.
    #[inline]
    pub fn addressed(&self) -> (Span, Span) {
        if self.cursor.origin {
            (self.lines, self.columns)
        } else {
            (self.size.lines(), self.size.columns())
        }
    }
    /// BS without reverse wraparound: back a column, stopping at the left
    /// margin unless the cursor is already left of it (xterm's
    /// `CursorBack`).
    #[inline]
    pub fn back(&mut self) {
        if self.lr && self.cursor.col == self.columns.first() {
            return;
        }
        self.cursor.col = self.cursor.col.saturating_sub(1);
    }
    /// CUF, or HPR (`absolute`, outside origin mode): the cursor `n`
    /// columns right, no further than the right margin, for CUF while the
    /// cursor is not past it, for HPR in origin mode (xterm's
    /// `CursorForward` and `CASE_HPR`), else the last column.
    #[inline]
    pub fn forward(&mut self, n: u16, absolute: bool) {
        let col = self.cursor.col.saturating_add(n);
        self.cursor.col = if self.lr {
            self.forward_in_margins(col, absolute)
        } else {
            col.min(self.size.columns().last())
        };
    }
    #[cold]
    #[inline(never)]
    fn forward_in_margins(&self, col: u16, absolute: bool) -> u16 {
        let bounded = if absolute {
            self.cursor.origin
        } else {
            self.cursor.col <= self.columns.last()
        };
        col.min(if bounded {
            self.columns.last()
        } else {
            self.size.columns().last()
        })
    }
    /// CUB: the cursor `n` columns left, no further than the left margin,
    /// unless it is left of it already (xterm's `CursorBack`).
    #[inline]
    pub fn backward(&mut self, n: u16) {
        let col = self.cursor.col.saturating_sub(n);
        let left = self.columns.first();
        self.cursor.col = if self.lr && self.cursor.col >= left {
            col.max(left)
        } else {
            col
        };
    }
    /// CHA and HPA: the cursor to column `col`, counting from the left
    /// margin in origin mode, no further than the right one.
    #[inline]
    pub fn column(&mut self, col: u16) {
        self.cursor.col = if self.lr && self.cursor.origin {
            self.columns.nth(col)
        } else {
            col.min(self.size.columns().last())
        };
    }
    /// Moves the cursor to line `line` as CUP does, keeping its column,
    /// which origin mode keeps within the right margin (VPA and VPR, as
    /// xterm addresses them).
    #[inline]
    pub fn position_line(&mut self, line: u16) {
        let col = self.cursor.col;
        self.position(line, 0);
        self.cursor.col = col.min(self.addressed().1.last());
    }
}

/// The column one past `col` if a wrap is pending there, else `col`.
fn past(col: u16, pending_wrap: bool) -> u16 {
    col.saturating_add(u16::from(pending_wrap))
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

mod reflow;
#[cfg(test)]
pub(crate) mod tests;
