use std::collections::VecDeque;
use std::ops::Range;
use std::sync::Arc;

use crate::compact::{BLANK, Compact, Line, Text};
use crate::geometry::{Size, Span};
use crate::history::{Arriving, History, PROMPT, RENDITION_SHIFT, WRAPPED, trimmed};
use crate::link::{Held, Hyperlink, Links, RowLinks};
use crate::screen::Saved;
use crate::style::Styles;
use crate::{Attributes, CellRef, Error, LineRendition, Row, RowId, reserve};

/// The cursor, and what DECSC saves with it. Only the grid's movements set
/// its row and column, each keeping them on the grid: a glyph that reaches
/// the last column leaves the cursor there with `pending_wrap` set.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Cursor {
    row: u16,
    col: u16,
    /// A glyph went into the last column, and the cursor waits past it:
    /// with autowrap on, DEC STD 070's Last Column Flag (Appendix D.6.1),
    /// so the next glyph first moves to the start of the next line; with
    /// it off, the next glyph overwrites the last column, and the flag
    /// (`Screen::pending_wrap`) is not set. A mark joins the glyph either
    /// way. DECAWM's changes, DECSTR and DECSC treat it so.
    pub pending_wrap: bool,
    /// `pending_wrap` came back with DECRC: the Last Column Flag, which
    /// setting autowrap keeps (DEC STD 070, Appendix D.6.1), even if it
    /// was off. A glyph's own flag is never restored. It means something
    /// only while `pending_wrap` is set: what clears that (CR, a cursor
    /// move, `wrap`, a mode change) may leave this, and the flag is read
    /// only through both (`Screen::last_column_flag`). Only `advance_within`
    /// (which clears this) and DECRC (which sets this to the flag it
    /// restores) set `pending_wrap`.
    pub restored: bool,
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

/// Takes the row in `slot`, of `width` columns, into `history`, if it has
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
    (texts, links): (&[Text], Option<&[RowLinks]>),
    (slot, width): (usize, u16),
) -> Result<bool, Error> {
    let Some(m) = meta.get_mut(slot) else {
        return Ok(false);
    };
    if texts.get(slot).is_some_and(|text| !text.is_empty())
        || links.is_some_and(|links| links.get(slot).is_some_and(|l| !l.is_empty()))
    {
        return Ok(false);
    }
    // Past its `used` mark the row is blank. A slot's cells are within
    // `cells`, so these do not wrap.
    let start = slot.wrapping_mul(usize::from(width));
    let row = cells
        .get_mut(start..start.wrapping_add(usize::from(m.used)))
        .unwrap_or_default();
    let len = trimmed(row).len();
    // The blank tail scanned; history's push counts the cells it keeps.
    work!(cells, row.len().saturating_sub(len));
    let kept = row.get_mut(..len).unwrap_or_default();
    history.push(m.arriving(kept, width))?;
    // Blanked here, while they are at hand, not when the slot is taken;
    // a row of one cell, as a short line's often is, without a call.
    match kept {
        [cell] => *cell = BLANK,
        _ => kept.fill(BLANK),
    }
    m.used = 0;
    Ok(true)
}

/// `Grid::narrow`'s bits.
const LR: u8 = 1;
const DOUBLED: u8 = 2;

/// The most cells a screen holds, and its history (512 MiB at 8 bytes a
/// cell), whatever history's budget.
pub(crate) const MAX_CELLS: usize = 64 * 1024 * 1024;
/// The most rows history holds, whatever its budget.
pub(crate) const MAX_ROWS: usize = 1_048_576;

/// A row of the screen, in its slot: what it is, besides its cells.
#[derive(Clone, Copy, Debug)]
struct Meta {
    id: RowId,
    version: u64,
    /// How far into the row a cell may differ from a blank in the default
    /// attributes: every cell from here to the grid's width is one, so
    /// recycling the slot clears only the cells before it, and a row
    /// scrolled into history is looked at no further.
    used: u16,
    /// Whether the row is soft-wrapped (`WRAPPED`) and whether a prompt
    /// starts on it (`PROMPT`, OSC 133 ; A), the bits history keeps them
    /// in, and its line rendition above them (`RENDITION`): one byte, which
    /// a row taken into history hands on as it is.
    flags: u8,
}

// `flags` fits where the struct had padding: a row costs no more for it.
const _: () = assert!(size_of::<Meta>() == 24);

impl Meta {
    /// What the row brings into history: `cells`, its cells before its
    /// blank tail, of `width`.
    fn arriving(self, cells: &[Compact], width: u16) -> Arriving<'_> {
        Arriving {
            id: self.id,
            version: self.version,
            flags: self.flags,
            cells,
            width,
        }
    }
    fn new(id: RowId, version: u64, wrapped: bool, used: u16) -> Self {
        Self {
            id,
            version,
            used,
            flags: if wrapped { WRAPPED } else { 0 },
        }
    }
    fn wrapped(&self) -> bool {
        self.flags & WRAPPED != 0
    }
    fn prompt(&self) -> bool {
        self.flags & PROMPT != 0
    }
    fn rendition(&self) -> LineRendition {
        LineRendition::from_bits(self.flags >> RENDITION_SHIFT)
    }
    /// The row with `flag` on or off.
    fn set(&mut self, flag: u8, on: bool) {
        self.flags = (self.flags & !flag) | if on { flag } else { 0 };
    }
    /// The row with rendition `rendition`, its other flags kept.
    fn with_rendition(mut self, rendition: LineRendition) -> Self {
        self.flags = (self.flags & (WRAPPED | PROMPT)) | rendition.bits() << RENDITION_SHIFT;
        self
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
    /// Puts `slot` under the last row.
    fn push_back(&mut self, slot: usize) {
        if self.top != 0 {
            spend!(slots, self.slots.len());
            if let Some(slots) = self.slots.get_mut(..) {
                slots.rotate_left(self.top);
            }
            self.top = 0;
        }
        self.slots.push(slot);
    }
    /// Turns the slots of rows `low` to `high` by `by`: to the left (each
    /// `by` rows up, the first `by` going to the end), or to the right.
    /// Once, whatever `by`: rows `low` to `high`, and, if they cross the
    /// ring's end, every row first, to bring its top to the start.
    fn rotate(&mut self, (low, high): (usize, usize), by: usize, left: bool) {
        let len = high.wrapping_sub(low).wrapping_add(1);
        if high < low || high >= self.slots.len() || by > len {
            return;
        }
        let (front, _) = self.as_mut_slices();
        if low < front.len() && high >= front.len() {
            // Across the ring's end: its top to the start, as `push_back`.
            work!(slots, self.slots.len());
            if let Some(slots) = self.slots.get_mut(..) {
                slots.rotate_left(self.top);
            }
            self.top = 0;
        }
        let (front, back) = self.as_mut_slices();
        let split = front.len();
        let run = match (low.checked_sub(split), high.checked_sub(split)) {
            (Some(low), Some(high)) => back.get_mut(low..=high),
            _ => front.get_mut(low..=high),
        };
        if let Some(run) = run {
            work!(slots, run.len());
            if left {
                run.rotate_left(by);
            } else {
                run.rotate_right(by);
            }
        }
    }
    /// The top row's slot becomes the bottom row's.
    #[inline]
    fn turn(&mut self) {
        let top = self.top.wrapping_add(1);
        self.top = if top >= self.slots.len() { 0 } else { top };
    }
    fn as_mut_slices(&mut self) -> (&mut [usize], &mut [usize]) {
        let top = self.top.min(self.slots.len());
        match self.slots.split_at_mut_checked(top) {
            Some((before, from)) => (from, before),
            None => (&mut [], &mut []),
        }
    }
    /// The rows' slots, top to bottom.
    fn iter(&self) -> impl Iterator<Item = &usize> {
        (0..self.len()).filter_map(|row| self.get(row))
    }
}

closed! {
    /// What ED and EL erase: from the cursor to the end, from the start to
    /// the cursor, or all.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum Erase {
        /// Its parameter.
        pub(crate) fn number(self) -> u16;
        /// The cursor's cell and what follows.
        Below => 0,
        /// What comes before, and the cursor's cell.
        Above => 1,
        /// All of it.
        All => 2,
    }
}

/// The most flag sets a kitty keyboard stack holds; a push onto a full stack
/// drops the oldest, as kitty does, so a runaway program cannot grow it.
const KEYBOARD_STACK_LIMIT: usize = 32;

/// One screen's kitty keyboard protocol flag stack. The primary and
/// alternate screens each keep one, so a program that pushes flags on the
/// alternate screen and exits without popping them leaves the shell's alone.
#[derive(Clone, Debug, Default)]
pub(crate) struct KeyboardStack(VecDeque<u8>);

impl KeyboardStack {
    pub(crate) fn top(&self) -> u8 {
        self.0.back().copied().unwrap_or(0)
    }
    pub(crate) fn push(&mut self, flags: u8) {
        if self.0.len() >= KEYBOARD_STACK_LIMIT {
            self.0.pop_front();
        }
        self.0.push_back(flags);
    }
    pub(crate) fn pop(&mut self, count: u16) {
        self.0
            .truncate(self.0.len().saturating_sub(usize::from(count)));
    }
    /// `CSI = flags ; mode u`: mode 1 (the default) replaces the top's
    /// flags, 2 adds to them and 3 removes from them; an empty stack's are 0.
    pub(crate) fn set(&mut self, flags: u8, mode: u16) {
        let top = self.top();
        self.pop(1);
        self.push(match mode {
            2 => top | flags,
            3 => top & !flags,
            _ => flags,
        });
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
    /// Each slot's cells' links: none for most, which costs no memory.
    linked: Vec<RowLinks>,
    /// The cells' links that have an `id`, and what the links cost.
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
    /// The slot of each row of the screen, top to bottom.
    order: Order,
    history: History,
    size: Size,
    pub cursor: Cursor,
    /// What DECSC saved, which DECRC restores.
    pub saved: Saved,
    /// The top and bottom margins (DECSTBM): the scrolling region.
    pub lines: Span,
    /// The left and right margins (DECSLRM, with DECLRMM set), set with
    /// `set_columns` alone, which keeps `lr`.
    columns: Span,
    /// What may end a line before the screen's edge, a bit each: `LR`,
    /// `columns` is narrower than the screen; `DOUBLED`, a row of the
    /// screen may be half as long (`LineRendition`), set when one is made
    /// so and cleared when every row is single width again. One test for
    /// every operation either bounds, which without them does as it did.
    narrow: u8,
    /// Whether its screen's cells are yet to be made (`Grid::unmade`):
    /// every row is blank, and `make` makes them.
    unmade: bool,
    /// The kitty keyboard flags the program on this screen pushed.
    pub keyboard: KeyboardStack,
}

/// Which way a scroll moves rows: up, the rows leaving the top going into
/// history if `history` (and the region is the whole screen), or down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Scroll {
    Up { history: bool },
    Down,
}

/// The identity `next` holds, and the next one: 64 bits counted up by one
/// do not run out, so none is ever reused.
pub(crate) fn next_id(next: &mut u64) -> RowId {
    let id = *next;
    *next = next.wrapping_add(1);
    RowId(id)
}

impl Grid {
    /// The most bytes its history holds.
    pub(crate) fn history_bytes(&self) -> usize {
        self.history.budget()
    }

    /// Lets every row of history go, its budget kept.
    pub(crate) fn clear_history(&mut self) {
        let budget = self.history.budget();
        self.history.set_budget(0);
        self.history.set_budget(budget);
    }

    pub(crate) fn set_history_bytes(&mut self, bytes: usize) {
        self.history.set_budget(bytes);
    }

    /// Whether a screen of `size` is more than a grid holds.
    pub fn check_size(size: Size) -> Result<(), Error> {
        (size.cells() <= MAX_CELLS)
            .then_some(())
            .ok_or(Error::Capacity)
    }

    pub fn new(
        size: Size,
        history_bytes: usize,
        next: &mut u64,
        version: u64,
    ) -> Result<Self, Error> {
        Self::made(size, history_bytes, next, version, false)
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
        spend!(cells, size);
        reserve(&mut self.cells, size)?;
        self.cells.resize(size, BLANK);
        self.unmade = false;
        Ok(())
    }

    fn made(
        size: Size,
        history_bytes: usize,
        next: &mut u64,
        version: u64,
        unmade: bool,
    ) -> Result<Self, Error> {
        Self::check_size(size)?;
        let mut grid = Self {
            unmade,
            ..Self::bare(size, history_bytes)
        };
        grid.reserve_screen()?;
        for _ in 0..size.rows() {
            let id = next_id(next);
            grid.push_screen_row(Meta::new(id, version, false, 0), &[], None, None);
        }
        Ok(grid)
    }

    /// A grid of `size` with no row yet, nor storage for one.
    fn bare(size: Size, history_bytes: usize) -> Self {
        Self {
            cells: Vec::new(),
            meta: Vec::new(),
            texts: Vec::new(),
            linked: Vec::new(),
            links: Links::default(),
            styles: Arc::default(),
            recent: [(Attributes::default(), 0); RECENT],
            order: Order::default(),
            history: History::new(history_bytes),
            size,
            cursor: Cursor::default(),
            saved: Saved::default(),
            lines: size.lines(),
            columns: size.columns(),
            narrow: 0,
            unmade: false,
            keyboard: KeyboardStack::default(),
        }
    }

    /// A grid with no row yet that takes this one's place: the same styles,
    /// and their numbers, which the rows it is given keep; unmade if this
    /// one is.
    fn successor(&self, size: Size) -> Self {
        Self {
            styles: Arc::clone(&self.styles),
            recent: self.recent,
            unmade: self.unmade,
            ..Self::bare(size, self.history_bytes())
        }
    }

    /// Room for the screen's rows, exactly; their cells' only once made.
    fn reserve_screen(&mut self) -> Result<(), Error> {
        // Within the limits: `check_size` came first.
        let rows = usize::from(self.size.rows());
        let cells = if self.unmade { 0 } else { self.size.cells() };
        reserve(&mut self.cells, cells)?;
        reserve(&mut self.meta, rows)?;
        reserve(&mut self.texts, rows)?;
        reserve(&mut self.linked, rows)?;
        self.order
            .try_reserve_exact(rows.saturating_sub(self.order.len()))?;
        Ok(())
    }

    /// Keeps a row laid out whole in `cells`, as wide as they are, as a
    /// resize makes a grid anew: into history, with its text and links, if
    /// `history`; else on the screen. Past `written` its cells are blank.
    fn keep_row(
        &mut self,
        meta: Meta,
        (cells, written): (&[Compact], usize),
        (text, links): (Option<Text>, Option<RowLinks>),
        history: bool,
    ) -> Result<(), Error> {
        if !history {
            self.push_screen_row(meta, cells, text, links);
            return Ok(());
        }
        let width = u16::try_from(cells.len()).map_err(|_| Error::Capacity)?;
        let kept = trimmed(cells.get(..written).unwrap_or(cells));
        self.history.push(meta.arriving(kept, width))?;
        // A row with neither text nor links, as most are, attaches none.
        let text = text.filter(|text| !text.is_empty());
        if text.is_some() || links.is_some() {
            self.history.attach(text.map(|text| text.exact()), links);
        }
        self.history.trim();
        Ok(())
    }

    /// Puts a row under the screen's rows, in a slot of its own: `meta`,
    /// `cells` from its first (the rest blank), its text and its links.
    /// Room was made for it (`reserve_screen`).
    fn push_screen_row(
        &mut self,
        meta: Meta,
        cells: &[Compact],
        text: Option<Text>,
        links: Option<RowLinks>,
    ) {
        let slot = self.meta.len();
        let cols = usize::from(self.size.cols());
        spend!(rows, 1);
        // An unmade grid's rows are blank, and have no cells to put.
        if !self.unmade {
            spend!(cells, cols);
            let start = self.cells.len();
            self.cells
                .extend_from_slice(cells.get(..cells.len().min(cols)).unwrap_or_default());
            self.cells.resize(start.saturating_add(cols), BLANK);
        }
        if meta.rendition().is_double() {
            self.narrow |= DOUBLED;
        }
        self.meta.push(meta);
        self.texts.push(text.unwrap_or_default());
        self.linked.push(links.unwrap_or_default());
        self.order.push_back(slot);
    }

    pub fn size(&self) -> Size {
        self.size
    }
    /// The attributes of the cells' styles.
    pub fn styles(&self) -> &Styles {
        &self.styles
    }
    pub fn history_len(&self) -> usize {
        self.history.len()
    }
    /// The cursor's row, counted from the oldest row history keeps.
    pub(crate) fn cursor_index(&self) -> Option<usize> {
        self.history_len()
            .checked_add(usize::from(self.cursor.row()))
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
        let cols = usize::from(self.size.cols());
        let start = slot.checked_mul(cols)?;
        Some(start..start.checked_add(cols)?)
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
        Some(Row {
            grid: self,
            // Exact: rows are far fewer than a usize holds.
            index: self.history.len().saturating_add(row),
            id: m.id,
            version: m.version,
            flags: m.flags,
            cells: self.slice(slot),
            width: usize::from(self.size.cols()),
            text: self.texts.get(slot)?,
            links: self.linked.get(slot).filter(|links| !links.is_empty()),
        })
    }
    pub fn row_at(&self, index: usize) -> Option<Row<'_>> {
        work!(rows, 1);
        let Some(row) = index.checked_sub(self.history.len()) else {
            let found = self.history.get(index)?;
            return Some(Row {
                grid: self,
                index,
                id: found.kept.id,
                version: found.kept.version,
                flags: found.flags,
                cells: found.cells,
                width: usize::from(found.kept.width()),
                text: found.text,
                links: found.links,
            });
        };
        self.slot_row(*self.order.get(row)?, row)
    }
    pub fn row_by_id(&self, id: RowId) -> Option<Row<'_>> {
        self.index_of(id).and_then(|index| self.row_at(index))
    }
    /// Where the row with identity `id` is, counted from the oldest row
    /// history keeps: the screen's rows are looked through first, then
    /// history's.
    pub fn index_of(&self, id: RowId) -> Option<usize> {
        self.screen_index_of(id)
            .or_else(|| self.history.position(id))
    }
    /// Where the row with identity `id` is, as `index_of` says, if it is
    /// one of the screen's rows: at most its rows read.
    pub(crate) fn screen_index_of(&self, id: RowId) -> Option<usize> {
        let row =
            (self.order.iter()).position(|slot| self.meta.get(*slot).is_some_and(|m| m.id == id));
        spend!(
            rows,
            row.map_or(self.order.len(), |row| row.saturating_add(1))
        );
        row.and_then(|row| row.checked_add(self.history.len()))
    }
    /// Where the screen's pictures were last followed into history to
    /// (`Screen::track_pictures`).
    pub(crate) fn followed(&self) -> &crate::history::Noted {
        self.history.followed()
    }
    /// The numbers, in the order rows came into history, of the oldest row
    /// it keeps and of the next to come.
    pub(crate) fn history_numbers(&self) -> (u64, u64) {
        self.history.numbers()
    }
    /// Where the history row numbered `number` in the order rows came is,
    /// if it is kept and is row `id`: one row read.
    pub(crate) fn index_of_number(&self, number: u64, id: RowId) -> Option<usize> {
        work!(rows, 1);
        let index = self.history.index_of_number(number)?;
        (self.history.kept(index)?.id == id).then_some(index)
    }
    /// The identity of the history row numbered `number` in the order rows
    /// came, if it is kept.
    pub(crate) fn id_of_number(&self, number: u64) -> Option<RowId> {
        let index = self.history.index_of_number(number)?;
        Some(self.history.kept(index)?.id)
    }
    /// Whether the screen's row `row` is soft-wrapped, read without making
    /// its `Row`: printing asks on its way.
    pub fn live_wrapped(&self, row: u16) -> bool {
        self.slot(row)
            .and_then(|slot| self.meta.get(slot))
            .is_some_and(Meta::wrapped)
    }
    pub fn live_row(&self, row: u16) -> Option<Row<'_>> {
        self.slot_row(self.slot(row)?, usize::from(row))
    }
    /// The slot of the screen's row `row`, counted from 0 at the top. The
    /// order holds the screen's rows, exactly, once the grid is made.
    #[inline]
    fn slot(&self, row: u16) -> Option<usize> {
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
        {
            self.changed(slot, end.into(), version);
        }
    }
    /// The row in `slot` changed: it takes `version`, and its `used` mark
    /// is raised to `end`, or the grid's width if that is less.
    #[inline]
    fn changed(&mut self, slot: usize, end: usize, version: u64) {
        let end = u16::try_from(end).unwrap_or(u16::MAX).min(self.size.cols());
        if let Some(m) = self.meta.get_mut(slot) {
            m.version = version;
            m.used = m.used.max(end);
        }
    }
    /// Writes the ASCII `run` from column `col` of live row `row`, in style
    /// `style`, as `mutate_row` writes, taking `version` only if a cell
    /// changed; unless a cell there is half of a wide glyph, which the
    /// general path repairs, when it writes nothing and says how many
    /// bytes come before the first such cell, which it could write.
    #[inline]
    pub fn write_ascii(
        &mut self,
        row: u16,
        col: u16,
        run: &[u8],
        style: u32,
        version: u64,
    ) -> Written {
        let Some(slot) = self.slot(row) else {
            return Written::Before(0);
        };
        work!(cells, run.len());
        let start = usize::from(col);
        let Some(dst) = start
            .checked_add(run.len())
            .and_then(|end| self.slice_mut(slot).get_mut(start..end))
        else {
            return Written::Before(0);
        };
        // One glyph, as at a cursor moved to it: the cell alone, without
        // the set-up the loops below take for a run.
        if let ([cell], [byte]) = (&mut *dst, run) {
            if cell.is_wide() || cell.is_wide_continuation() {
                return Written::Before(0);
            }
            if cell.is_ascii(*byte, style) {
                return Written::Same;
            }
            *cell = Compact::ascii(*byte, style);
            self.changed(slot, start.saturating_add(1), version);
            return Written::All;
        }
        // A short run, a word between colours, cell by cell; a long one by
        // the loops that move words, whose set-up a short one would not
        // repay.
        let halves = if run.len() < LONG_RUN {
            dst.iter().any(|c| c.is_wide() || c.is_wide_continuation())
        } else {
            Compact::any_halves(dst)
        };
        if halves {
            return Written::Before(first_half(dst));
        }
        // Already these very cells, as a redraw finds them: the row is as
        // it was. New text differs at the first cell.
        let first = dst.first().zip(run.first());
        if first.is_some_and(|(c, b)| c.is_ascii(*b, style)) && unchanged(dst, run, style) {
            return Written::Same;
        }
        if run.len() < LONG_RUN {
            for (cell, byte) in dst.iter_mut().zip(run) {
                *cell = Compact::ascii(*byte, style);
            }
        } else {
            Compact::fill_ascii(dst, run, style);
        }
        self.changed(slot, start.saturating_add(run.len()), version);
        Written::All
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
        {
            self.changed(slot, end.into(), version);
        }
    }
    /// Gives the cells `span` of live row `row` the open link `pen`, or
    /// none, as printing them does. The grid makes its link of `pen` the
    /// first time; with no room for it, the cells have none. Out of line,
    /// so that printing, which calls it for every glyph once a link has
    /// been opened, makes one call of it.
    #[inline(never)]
    pub(crate) fn print_link(
        &mut self,
        row: u16,
        span: Range<usize>,
        pen: Option<&Hyperlink>,
        version: u64,
    ) {
        let link = match pen {
            None => None,
            Some(pen) => {
                if let Held::Pending = self.links.open {
                    self.open(pen, version);
                }
                match &self.links.open {
                    Held::At(link) => Some(link),
                    Held::Pending | Held::Refused => None,
                }
            }
        };
        let slot = self
            .slot(row)
            .filter(|_| span.end <= usize::from(self.size.cols()));
        set_link(&mut self.meta, &mut self.linked, slot, span, link, version);
    }
    /// Makes the grid's link of `pen`, the link the program opened: out of
    /// line, as printing makes it once.
    #[cold]
    #[inline(never)]
    fn open(&mut self, pen: &Hyperlink, version: u64) {
        let link = self.intern(pen, version);
        self.links.open = link.map_or(Held::Refused, Held::At);
    }
    /// Gives the cells `span` of live row `row` link `link`, or none.
    fn set_link(&mut self, row: u16, span: Range<usize>, link: Option<&Hyperlink>, version: u64) {
        let slot = self
            .slot(row)
            .filter(|_| span.end <= usize::from(self.size.cols()));
        set_link(&mut self.meta, &mut self.linked, slot, span, link, version);
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
        work!(lookups, 1);
        let id = match self.styles.find(attributes) {
            Some(id) => id,
            None => {
                work!(inserts, 1);
                let cells = self.cells.len().saturating_add(self.history.kept_cells());
                let mut swept = self.styles.wants_sweep(cells);
                if swept {
                    self.sweep();
                }
                // Its bytes change only as its room grows, or a sweep:
                // history makes room for them first, and if it can't, the
                // styles only it held go too.
                let grows = self.styles.full();
                if grows && !self.make_style_room() {
                    self.sweep();
                    swept = true;
                }
                // After a sweep there is room: the styles in use are at
                // most the cells, far fewer than the table holds.
                let id = Arc::make_mut(&mut self.styles)
                    .insert(attributes)
                    .unwrap_or(0);
                if grows || swept {
                    let share = crate::style::screen_share(self.cells.len());
                    self.history
                        .charge(self.styles.bytes().saturating_sub(share));
                }
                id
            }
        };
        if let Some(slot) = self.recent.get_mut(at) {
            *slot = (attributes, id);
        }
        id
    }

    /// Charges the style table's bytes, one more style in it, beyond the
    /// screen's share to history's budget, the oldest rows going to fit
    /// (`threats.md` B11); whether they fit, with no rows if need be.
    fn make_style_room(&mut self) -> bool {
        let share = crate::style::screen_share(self.cells.len());
        let after = self.styles.bytes_after_insert().saturating_sub(share);
        self.history.charge(after);
        !(after > self.history.budget())
    }

    /// Keeps only the styles cells have, numbered anew, and gives every
    /// cell its style's new number (`style.rs`).
    #[cold]
    #[inline(never)]
    pub(crate) fn sweep(&mut self) {
        // Every cell is read twice: to mark its style, then to renumber it.
        let cells = self.cells.len().saturating_add(self.history.kept_cells());
        spend!(styles, cells.saturating_mul(2));
        let mut used = vec![false; self.styles.len()];
        let mut mark = |cell: &Compact| {
            if let Some(mark) = Styles::place(cell.style()).and_then(|i| used.get_mut(i)) {
                *mark = true;
            }
        };
        self.cells.iter().for_each(&mut mark);
        self.history.each_cell_mut(|cell| mark(cell));
        let renumber = Arc::make_mut(&mut self.styles).retain(&used);
        let mut renumbered = |cell: &mut Compact| {
            if let Some(&new) = Styles::place(cell.style()).and_then(|i| renumber.get(i)) {
                cell.set_style(new);
            }
        };
        self.cells.iter_mut().for_each(&mut renumbered);
        self.history.each_cell_mut(renumbered);
        self.recent = [(Attributes::default(), 0); RECENT];
    }

    /// How many styles the grid's table holds, in use or not.
    #[cfg(test)]
    pub(crate) fn style_count(&self) -> usize {
        self.styles.len()
    }

    /// Forgets every link: RIS.
    pub fn reset_links(&mut self) {
        self.linked.iter_mut().for_each(RowLinks::clear);
        self.history.reset_links();
        self.links = Links::default();
    }

    /// The grid's link of the one `opened`, made if no row has it. With
    /// no room for it, room is made (`make_room`), at most once in a while
    /// after that fails; `None` if there is still none.
    pub fn intern(&mut self, opened: &Hyperlink, version: u64) -> Option<Hyperlink> {
        let (uri, id) = (&opened.uri, opened.id.as_ref());
        if let Some(id) = id
            && let Some(link) = self.links.find(uri, id)
        {
            return Some(link.clone());
        }
        if let Some(link) = self.links.insert(opened) {
            return Some(link);
        }
        if !self.links.may_make_room() {
            return None;
        }
        self.make_room(version);
        let link = self.links.insert(opened);
        if link.is_none() {
            self.links.stall();
        }
        link
    }

    /// Makes room for links: the rows' links are counted again
    /// (`Links::sweep`); if they cost more than half the bound, the oldest
    /// history rows lose theirs, taking `version`, until the rest cost at
    /// most half. History loses links before text, and the screen's rows
    /// never do.
    fn make_room(&mut self, version: u64) {
        // Oldest first, as links open: the screen's rows top to bottom.
        let linked = &self.linked;
        let history = self
            .history
            .links()
            .map(|(index, links)| (Some(index), links));
        let screen = self
            .order
            .iter()
            .filter_map(|slot| Some((None, linked.get(*slot)?)));
        let lose = self.links.sweep(history.chain(screen), self.history.len());
        for index in 0..lose {
            self.history.unlink(index, version);
        }
    }

    /// The newest version of a history row let go past the limit.
    pub(crate) fn let_go(&self) -> u64 {
        self.history.let_go
    }
    /// Marks live row `row` as where a prompt starts (OSC 133 ; A), or
    /// not: ED erased it. A change of mark gives the row `version`
    /// (`threats.md` P12): a host redraws what shows the marks.
    pub fn set_prompt(&mut self, row: u16, prompt: bool, version: u64) {
        if let Some(slot) = self.slot(row)
            && let Some(m) = self.meta.get_mut(slot)
            && m.prompt() != prompt
        {
            m.version = version;
            m.set(PROMPT, prompt);
        }
    }

    pub fn wrap(&mut self, row: u16, wrapped: bool, version: u64) {
        if let Some(slot) = self.slot(row)
            && let Some(m) = self.meta.get_mut(slot)
            && m.wrapped() != wrapped
        {
            m.version = version;
            m.set(WRAPPED, wrapped);
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
        let Some(&Meta { used, .. }) = self.meta.get(slot) else {
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
        work!(cells, reach.saturating_sub(first));
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
                && let Some(other) = cells.get_mut(first.wrapping_sub(1))
            {
                *other = other.blanked();
            }
            if cells
                .get(reach.wrapping_sub(1))
                .is_some_and(Compact::is_wide)
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
                    m.used = m.used.max(end.min(cols));
                }
            }
            if plain && span_end >= usize::from(m.used) {
                // Every cell from `start` on is blank now.
                m.used = m.used.min(start);
            }
            // As `wrap` ends the row's soft wrap.
            if clears_edge && m.wrapped() {
                m.set(WRAPPED, false);
                m.version = version;
            }
        }
        // A whole row erased keeps no text, and no links: its blank cells
        // would never read them.
        if start == 0 && end >= cols {
            if let Some(text) = self.texts.get_mut(slot) {
                text.release();
            }
            if !self.links.none()
                && let Some(rows) = self.size.lines().starting_at(row)
            {
                self.unlink_rows(rows, 1, false);
            }
        }
    }

    /// ED (`display`) or EL of `part`, from the cursor, ending a pending
    /// wrap: `erase` erases each row's span. A row ED erases whole is no
    /// prompt's, as in Ghostty; EL, and ED's part of the cursor's row,
    /// leave the mark (a shell redrawing its prompt erases from it).
    #[inline]
    pub fn erase_in(
        &mut self,
        display: bool,
        part: Erase,
        version: u64,
        mut erase: impl FnMut(&mut Self, u16, u16, u16),
    ) {
        self.cursor.pending_wrap = false;
        let ((row, col), cols) = (self.cursor.at(), self.size.cols());
        if display {
            // ED spends the screen's cells (`budget`); EL, a row's at
            // most, nothing.
            crate::budget::spend(
                crate::budget::units(self.size.cells()),
                crate::budget::weight::cells,
            );
            for y in 0..self.size.rows() {
                let whole = match part {
                    Erase::Below => y > row,
                    Erase::Above => y < row,
                    Erase::All => true,
                };
                if whole {
                    erase(self, y, 0, cols);
                    self.set_prompt(y, false, version);
                    if self.doubled() {
                        self.set_rendition(y, LineRendition::Single, version);
                    }
                }
            }
        }
        let (start, end) = match part {
            Erase::Below => (col, cols),
            Erase::Above => (0, col.saturating_add(1).min(cols)),
            Erase::All => (0, cols),
        };
        erase(self, row, start, end);
        // ED makes a line it erases whole single width (DEC STD 070, ED,
        // note 1, and its algorithm); EL leaves it as it is.
        if display && self.doubled() && start == 0 && end >= self.row_end(row) {
            self.set_rendition(row, LineRendition::Single, version);
            // Every row is single width again after ED 2.
            if part == Erase::All {
                self.narrow &= !DOUBLED;
            }
        }
    }

    /// `erase`, leaving the guarded glyphs (SPA's) as they are. Each run of
    /// other cells between them is erased, as xterm's `ClearInLine2` erases
    /// around them. A wide glyph's second half is guarded if its first half
    /// is. Out of line, as protection is rare.
    #[inline(never)]
    pub fn erase_unprotected(&mut self, row: u16, start: u16, end: u16, style: u32, version: u64) {
        let end = end.min(self.size.cols());
        let (cells, styles) = (self.live_cells(row), &self.styles);
        let kept = |c: &Compact| styles.guarded(c.style());
        let protected = |at: usize| {
            cells.get(at).is_some_and(|c| {
                kept(c)
                    || c.is_wide_continuation() && cells.get(at.wrapping_sub(1)).is_some_and(kept)
            })
        };
        work!(cells, end.saturating_sub(start));
        let mut runs = Vec::new();
        let mut from = start;
        for col in start..end {
            if protected(usize::from(col)) {
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
    }

    /// DECFRA: the columns `start` to `end` of live row `row` filled with
    /// `c`, a glyph one column wide, in `style`, with no link: erased as
    /// `erase` erases them, which blanks a wide glyph they cut, then each
    /// taking `c`.
    pub fn fill(&mut self, row: u16, (start, end): (u16, u16), c: char, style: u32, version: u64) {
        self.erase(row, start, end, style, version);
        let span = usize::from(start)..usize::from(end);
        work!(cells, span.len());
        let glyph = Compact::glyph(c, 1, style);
        self.mutate_row(row, version, end, |cells| {
            cells
                .get_mut(span.clone())
                .map(|run| run.fill(glyph))
                .is_some()
        });
        self.set_link(row, span, None, version);
    }

    /// DECSERA, DECSED and DECSEL: each glyph DECSCA does not protect in
    /// the columns `start` to `end` of live row `row` made a space, keeping
    /// its attributes; empty cells stay empty (DEC STD 070, each of them).
    /// With `all`, DECERA: every glyph, protected or not, a space in that
    /// style, "clearing all character attributes", links too. A wide
    /// glyph's second half goes with its first, and one the span cuts is
    /// blanked whole.
    pub fn erase_to_spaces(
        &mut self,
        row: u16,
        (start, end): (u16, u16),
        all: Option<u32>,
        version: u64,
    ) {
        let span = usize::from(start)..usize::from(end);
        work!(cells, span.len());
        self.mutate_row(row, version, end, |cells| {
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

    /// DECCRA: the cells `columns` of rows `lines`, text and attributes,
    /// copied to the rows and columns from `to`, as far as the screen goes
    /// (DEC STD 070, DECCRA), all read before any is written, so the two
    /// may overlap. A wide glyph the copy cuts is blanked whole. The cells
    /// copied to have no link.
    pub fn copy_rectangle(&mut self, lines: Span, columns: Span, to: (u16, u16), version: u64) {
        let span = usize::from(columns.first())..usize::from(columns.end());
        let source: Vec<(Vec<Compact>, Text)> = lines
            .each()
            .map(|y| {
                let text = self.slot(y).and_then(|slot| self.texts.get(slot));
                let cells = self.live_cells(y).get(span.clone()).unwrap_or_default();
                let mut cells = cells.to_vec();
                // Each cell read, then written; and the row's text cloned.
                spend!(cells, cells.len().saturating_mul(2));
                spend!(text, text.map_or(0, Text::bytes));
                // The half of a glyph the source's edge cuts.
                if let Some(cut) = cells.first_mut().filter(|c| c.is_wide_continuation()) {
                    *cut = cut.blanked();
                }
                if let Some(cut) = cells.last_mut().filter(|c| c.is_wide()) {
                    *cut = cut.blanked();
                }
                (cells, text.cloned().unwrap_or_default())
            })
            .collect();
        let (row, col) = to;
        let end = col.saturating_add(columns.len()).min(self.size.cols());
        for (y, (cells, text)) in (row..self.size.rows()).zip(source) {
            self.mutate_line(y, version, end, |line| {
                let span = usize::from(col)..usize::from(end);
                line.clear_for(span.clone());
                for (at, cell) in span.zip(&cells) {
                    line.set(at, *cell, text.of(cell));
                }
                repair_wide(line.cells);
                true
            });
            self.set_link(y, usize::from(col)..usize::from(end), None, version);
        }
    }

    /// ICH and DCH, at the cursor, which they leave where it is; they end
    /// a pending wrap (DEC STD 070, Appendix D.6.1). The cells they bring
    /// in are blank in `blank`. DCH ends the row's soft wrap; ICH, and the
    /// insertion IRM makes, keep it, as xterm does. With left and right
    /// margins they edit up to the right margin, and outside them they do
    /// nothing at all, the pending wrap staying too, as in xterm
    /// (`InsertChar`, `DeleteChar`; DEC STD 070, 5.4.3).
    pub fn edit_cells(&mut self, count: u16, insert: bool, blank: u32, version: u64) {
        if self.lr() && !self.in_columns() {
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
        // last column without margins, or at the row's end, on a row half
        // as long (ICH and DCH, their algorithms).
        let end = self.columns.end().min(self.row_end(row));
        // At most the cells from the column to the margin.
        let count = usize::from(count.min(end.saturating_sub(col)));
        if count == 0 {
            return;
        }
        let cols = self.size.cols();
        let end = usize::from(end);
        work!(cells, end.saturating_sub(usize::from(col)));
        let blank = Compact::blank(blank);
        self.mutate_row(row, version, cols, |row_cells| {
            let col = usize::from(col);
            // A wide glyph across the right margin loses both halves: the
            // edit moves one and not the other.
            split_wide(row_cells, end);
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
                split_wide(cells, boundary);
            }
            shift(cells, col, count, insert, blank);
            repair_wide(row_cells);
            // Inserting and deleting always count as a change.
            true
        });
        // The cells' links move with them.
        if let Some(slot) = self.slot(row)
            && let Some(links) = self.linked.get_mut(slot)
            && !links.is_empty()
        {
            links.shift(usize::from(col), count, insert, end);
        }
        if !insert {
            self.wrap(row, false, version);
        }
    }

    /// Gives a slot a new row: `id`, unwrapped, every cell blank, with no
    /// text. Only the cells that may not be blank already are cleared. Its
    /// links are its callers' to drop, out of the way of a scroll.
    #[inline]
    fn recycle(&mut self, slot: usize, id: RowId, version: u64) {
        let mut used = usize::from(self.size.cols());
        if let Some(m) = self.meta.get_mut(slot) {
            used = usize::from(m.used);
            *m = Meta::new(id, version, false, 0);
        }
        work!(cells, used);
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
    /// Drops the links of the `count` rows at the top of `region`, or at
    /// its bottom if `bottom`. Out of line, and called only if the grid
    /// holds links: most hold none, and a scroll pays a test.
    #[cold]
    #[inline(never)]
    fn unlink_rows(&mut self, region: Span, count: u16, bottom: bool) {
        let (top, end) = (region.first(), region.last().saturating_add(1));
        let rows = if bottom {
            end.saturating_sub(count)..end
        } else {
            top..top.saturating_add(count)
        };
        for row in rows {
            if let Some(links) = self.slot(row).and_then(|slot| self.linked.get_mut(slot)) {
                links.clear();
            }
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
        work!(cells, self.size.cols());
        for cell in self.slice_mut(slot) {
            cell.set_style(blank);
        }
        if let Some(m) = self.meta.get_mut(slot) {
            m.used = self.size.cols();
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
            && self.history_bytes() > 0
        {
            let count = count.min(self.size.rows());
            for done in 0..count {
                if self.scroll_into_history(blank, next, version).is_err() {
                    let rest = count.saturating_sub(done);
                    self.scroll_past_history(rest, blank, next, version);
                    break;
                }
            }
            return Ok(());
        }
        self.scroll_region(region, count, direction, blank, next, version);
        Ok(())
    }

    /// No room in history for the rows a scroll of the screen takes into
    /// it: these `count` go as with no history (threats.md 1.3, SCR-10),
    /// counted. Out of line, as no scroll but a refused one comes here.
    #[cold]
    #[inline(never)]
    fn scroll_past_history(&mut self, count: u16, blank: u32, next: &mut u64, version: u64) {
        crate::degraded();
        if !self.links.none() {
            self.unlink_rows(self.size.lines(), count, false);
        }
        // Each top row goes, its slot a new blank row under the screen's
        // last, as `scroll_into_history` makes one.
        for _ in 0..count {
            let Some(&slot) = self.order.get(0) else {
                return;
            };
            let id = next_id(next);
            self.recycle(slot, id, version);
            if blank != 0 {
                self.colour(slot, blank);
            }
            self.order.turn();
        }
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
        spend!(cells, usize::from(height).saturating_mul(2));
        for y in top..=bottom {
            self.mutate_row(y, version, 0, |cells| {
                let mut changed = false;
                for edge in [left, end] {
                    changed |= edge > 0 && split_wide(cells, edge);
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
        spend!(cells, cells.len().saturating_mul(2));
        let text = self.texts.get(source).map(Text::exact).unwrap_or_default();
        let links = self.linked.get(source).filter(|l| !l.is_empty()).cloned();
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
        if changed {
            self.changed(target, span.end, version);
        }
        // The links, a cell at a time: each takes its source's, or none.
        let linked = links.is_some() || self.linked.get(target).is_some_and(|l| !l.is_empty());
        if linked {
            for at in span {
                let link = links.as_ref().and_then(|l| l.get(at));
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
        spend!(cells, span.len());
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
        self.set_link(row, span, None, version);
    }

    /// `scroll` within the margins, or down, or without history.
    ///
    /// A soft wrap is the row's own, as xterm keeps it (soft wrap is
    /// xterm's notion; no standard has one): it moves with the row, the
    /// rows brought in have none, and the row above the region keeps its
    /// own (`replay --engines xterm --size 2x1 '\e[999Cab\e[L'`, and
    /// `--size 6x1 '~!@#\e[3;5r\e[10H\e[2T'` scrolling down). Scrolling
    /// up, a line wrapping at the bottom margin goes on in the row brought
    /// in (`Screen::wrap` marks it before the scroll).
    #[inline]
    fn scroll_region(
        &mut self,
        region: Span,
        count: u16,
        direction: Scroll,
        blank: u32,
        next: &mut u64,
        version: u64,
    ) {
        let up = direction != Scroll::Down;
        let (top, bottom) = (region.first(), region.last());
        let count = count.min(region.len());
        if count == 0 {
            return;
        }
        if count == 1 {
            let id = next_id(next);
            let (from, to) = if up { (top, bottom) } else { (bottom, top) };
            if let Some(slot) = self.move_row(usize::from(from), usize::from(to)) {
                self.recycle(slot, id, version);
                if blank != 0 {
                    self.colour(slot, blank);
                }
            }
        } else {
            // The region's slots turned once, by `count` (threats.md B36),
            // then the rows brought in made anew in the order moving them
            // one at a time would: up, from the first brought in, at the
            // bottom less `count`; down, from the last, at the top.
            let (low, high) = (usize::from(top), usize::from(bottom));
            self.order.rotate((low, high), usize::from(count), up);
            for k in 0..count {
                let row = if up {
                    bottom
                        .saturating_add(1)
                        .saturating_sub(count)
                        .saturating_add(k)
                } else {
                    top.saturating_add(count)
                        .saturating_sub(1)
                        .saturating_sub(k)
                };
                let id = next_id(next);
                if let Some(slot) = self.slot(row) {
                    self.recycle(slot, id, version);
                    if blank != 0 {
                        self.colour(slot, blank);
                    }
                }
            }
        }
        // The rows brought in, in the slots of those that left, have none
        // of their links.
        if !self.links.none() {
            self.unlink_rows(region, count, up);
        }
    }

    /// Moves the screen's top row into history, the oldest row there going
    /// if there are more than the limit, and puts a new blank row under
    /// the screen's last, in style `blank`, with a new identity, in the top
    /// row's slot. Nothing moves if there is no room in history.
    #[inline]
    fn scroll_into_history(
        &mut self,
        blank: u32,
        next: &mut u64,
        version: u64,
    ) -> Result<(), Error> {
        let Some(&slot) = self.order.get(0) else {
            return Ok(());
        };
        let taken = take_row(
            &mut self.history,
            self.cells.as_mut_slice(),
            self.meta.as_mut_slice(),
            // A grid that holds no links has no row to look for them in.
            (&self.texts, (!self.links.none()).then_some(&self.linked)),
            (slot, self.size.cols()),
        )?;
        if !taken {
            self.take_row_with_extras(slot)?;
        }
        self.history.trim();
        let id = next_id(next);
        if taken {
            // Its cells are blank, and it has neither text nor links: it
            // is a new row once it has a new identity.
            if let Some(m) = self.meta.get_mut(slot) {
                *m = Meta::new(id, version, false, 0);
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
        let used = start.saturating_add(usize::from(m.used));
        let cells = trimmed(self.cells.get(start..used).unwrap_or_default());
        // The blank tail scanned; history's push counts the cells it keeps.
        work!(cells, usize::from(m.used).saturating_sub(cells.len()));
        self.history.push(m.arriving(cells, self.size.cols()))?;
        // The row's text and links, which history has just taken, go with
        // it: a copy of the text, and the links themselves.
        let text = self.texts.get(slot).map(Text::exact);
        let links = (self.linked.get_mut(slot).map(std::mem::take)).filter(|l| !l.is_empty());
        self.history.attach(text, links);
        Ok(())
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
        work!(slots, high.saturating_sub(low).saturating_add(1));
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
        Self::check_size(size)?;
        let (rows, cols) = (size.rows(), size.cols());
        let history = self.history_len();
        // Rows are placed around the cursor, so that the line it is on stays
        // in view (docs/breaks-audit.md, 017: a resized pane lost its bottom
        // line). A shrink drops rows below the cursor first, and only then
        // scrolls rows above it into history: a screen with its content at
        // the top keeps it, and a full screen keeps its bottom line. A grow
        // pulls rows back from history above, as xterm does, and pads the rest
        // with blank rows below. History is trimmed to its budget, oldest
        // first.
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
        let end = live_top
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
            saved: Saved {
                cursor: shifted(self.saved.cursor),
                ..self.saved
            },
            ..self.successor(size)
        };
        replacement.reserve_screen()?;
        // Each row is laid out whole in `row`, as wide as it is, then kept.
        let mut row = Vec::new();
        let same_width = self.size.cols() == cols;
        for source in 0..end {
            let old = self.row_at(source).filter(|_| source < old_retained);
            let is_history = source < live_top;
            let id = old.map_or_else(|| next_id(next), |r| r.id);
            let width = match old {
                // A row is never wider than the u16 grid it was made in.
                Some(r) if is_history => u16::try_from(r.width).map_err(|_| Error::Capacity)?,
                Some(_) | None => cols,
            };
            row.clear();
            row.resize(usize::from(width), BLANK);
            work!(cells, width);
            // A row cut or padded to a new width no longer continues on
            // the next; at the same width it does, as it did.
            let wrapped = old.is_some_and(|r| r.wrapped()) && (is_history || same_width);
            let row_version = if is_history {
                old.map_or(version, |r| r.version)
            } else {
                version
            };
            // As much of the old row's links as fits, as of its cells.
            let links = old
                .and_then(|r| r.links)
                .map(|links| links.clipped(usize::from(width)));
            let mut text = None;
            if let Some(old) = old {
                // As much of the old row as fits, over the new one's start:
                // both runs are `len` long.
                let len = old.cells.len().min(usize::from(width));
                if let (Some(dst), Some(src)) = (row.get_mut(..len), old.cells.get(..len)) {
                    crate::copy_from(dst, src);
                }
                // A row half as long keeps its rendition, and loses what
                // is past its end at the new width, as DECDWL loses it.
                if old.rendition().is_double()
                    && let Some(past) = row.get_mut(usize::from((width / 2).max(1))..)
                {
                    past.fill(BLANK);
                }
                repair_wide(&mut row);
                // The row's text comes along, stored again within the
                // budget of the row's new width.
                text = Some(Line::rebuilt(&mut row, old.text));
            }
            let mut meta = Meta::new(id, row_version, wrapped, width)
                .with_rendition(old.map_or(LineRendition::Single, |r| r.rendition()));
            meta.set(PROMPT, old.is_some_and(|r| r.starts_prompt()));
            replacement.keep_row(meta, (&row, row.len()), (text, links), is_history)?;
        }
        replacement.fit_cursor();
        Ok(replacement)
    }

    /// Starts the grid again, as `new` makes it. A grid that holds only its
    /// live rows, in storage made for them, keeps that storage: its rows are
    /// blanked and given new identities, top to bottom, as `new` gives them.
    pub fn clear(&mut self, next: &mut u64, version: u64) -> Result<(), Error> {
        if !self.recyclable() {
            // The links' table is kept, which counts the link the program
            // has open (`Screen::link_cells`), and finds it by its `id`.
            match Self::made(self.size, self.history_bytes(), next, version, self.unmade) {
                Ok(mut grid) => {
                    grid.links = std::mem::take(&mut self.links);
                    *self = grid;
                    return Ok(());
                }
                // No room for a grid made anew: history goes, and the rows
                // the grid holds are blanked in place (threats.md 1.3,
                // SCR-10), counted.
                Err(refused) => {
                    if !self.holds_its_rows() {
                        return Err(refused);
                    }
                    self.history = History::new(self.history_bytes());
                    crate::degraded();
                }
            }
        }
        self.renew(next, version);
        Ok(())
    }

    /// Whether the grid's storage holds a slot for each of its live rows,
    /// each as wide as the grid (none for an unmade grid's cells), as
    /// `renew` blanks them.
    fn holds_its_rows(&self) -> bool {
        let rows = usize::from(self.size.rows());
        let cells = if self.unmade { 0 } else { self.size.cells() };
        self.order.len() == rows && self.meta.len() == rows && self.cells.len() >= cells
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
    /// top to bottom; the cursors, origin, margins and keyboard stack are
    /// reset.
    fn renew(&mut self, next: &mut u64, version: u64) {
        for index in 0..self.order.len() {
            let id = next_id(next);
            if let Some(&slot) = self.order.get(index) {
                self.recycle(slot, id, version);
            }
        }
        self.linked.iter_mut().for_each(RowLinks::clear);
        // No cell has a style but the default now.
        self.styles = Arc::default();
        self.recent = [(Attributes::default(), 0); RECENT];
        self.cursor = Cursor::default();
        self.saved = Saved::default();
        self.keyboard = KeyboardStack::default();
        self.narrow &= !DOUBLED;
        self.reset_margins();
    }
    /// Whether every slot's cells from `used` to its width are blank, as
    /// recycling relies on; an unmade grid's are all blank.
    #[cfg(any(test, feature = "checks"))]
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
        self.narrow & LR != 0
    }
    /// Whether a row of the screen may be half as long.
    #[inline]
    pub(crate) fn doubled(&self) -> bool {
        self.narrow & DOUBLED != 0
    }
    /// The left and right margins.
    pub fn columns(&self) -> Span {
        self.columns
    }
    /// Sets the left and right margins.
    pub fn set_columns(&mut self, columns: Span) {
        self.columns = columns;
        self.narrow = (self.narrow & !LR)
            | if columns != self.size.columns() {
                LR
            } else {
                0
            };
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
    /// On a row half as long, its end bounds it too (`row_end`).
    #[inline]
    pub fn line_end(&self) -> u16 {
        if self.narrow() {
            return self.line_end_in_margins();
        }
        self.size.cols()
    }
    /// `line_end` with left and right margins, or a row half as long. Out
    /// of line, as every margin case is, so that without margins each
    /// costs one test.
    #[cold]
    #[inline(never)]
    fn line_end_in_margins(&self) -> u16 {
        let end = if self.lr() && self.cursor.col <= self.columns.last() {
            self.columns.end()
        } else {
            self.size.cols()
        };
        end.min(self.row_end(self.cursor.row))
    }
    /// Whether a line may end before the screen's edge: left and right
    /// margins are set, or a row may be half as long.
    #[inline]
    pub fn narrow(&self) -> bool {
        self.narrow != 0
    }
    /// The rendition of the screen's row `row` (DECDWL, DECDHL).
    pub fn rendition(&self, row: u16) -> LineRendition {
        self.slot(row)
            .and_then(|slot| self.meta.get(slot))
            .map_or(LineRendition::Single, Meta::rendition)
    }
    /// One past the last column of the screen's row `row`: the screen's
    /// width, or on a row that is not single width half of it, rounded
    /// down (DEC STD 070, 5.14.1, `end_of_line`), and at least one column.
    #[inline]
    pub fn row_end(&self, row: u16) -> u16 {
        if !self.doubled() {
            return self.size.cols();
        }
        self.halved_end(row)
    }
    #[cold]
    #[inline(never)]
    fn halved_end(&self, row: u16) -> u16 {
        let cols = self.size.cols();
        if self.rendition(row).is_double() {
            (cols / 2).max(1)
        } else {
            cols
        }
    }
    /// Keeps the cursor short of its row's end: a cursor moved onto or
    /// along a row half as long stops at its last column (DEC STD 070,
    /// 5.6.1; IND, LF, CUU, CUD, CUF, CUP and HT, their algorithms).
    #[inline]
    pub(crate) fn fit_cursor(&mut self) {
        if self.doubled() {
            self.fit_to_row();
        }
    }
    #[cold]
    #[inline(never)]
    fn fit_to_row(&mut self) {
        let last = self.halved_end(self.cursor.row).saturating_sub(1);
        self.cursor.col = self.cursor.col.min(last);
    }
    /// Sets the rendition of the screen's row `row` (DECSWL, DECDWL,
    /// DECDHL). A row made half as long loses its cells past its new end,
    /// and the cursor on it goes no further than its last column (DECDWL
    /// and DECDHL, their notes and algorithms); one made single width
    /// keeps every cell it has (DECSWL, note 2).
    pub fn set_rendition(&mut self, row: u16, rendition: LineRendition, version: u64) {
        let Some(m) = self.slot(row).and_then(|slot| self.meta.get_mut(slot)) else {
            return;
        };
        if m.rendition() == rendition {
            return;
        }
        *m = m.with_rendition(rendition);
        m.version = version;
        if rendition.is_double() {
            self.narrow |= DOUBLED;
            let (end, cols) = (self.halved_end(row), self.size.cols());
            self.erase(row, end, cols, 0, version);
            if self.cursor.row == row {
                self.fit_to_row();
            }
        }
    }
    /// Makes every row of the screen single width, as setting DECLRMM does
    /// (DEC STD 070, DECLRMM, 5-29).
    pub fn single_width(&mut self, version: u64) {
        if !self.doubled() {
            return;
        }
        for row in 0..self.size.rows() {
            self.set_rendition(row, LineRendition::Single, version);
        }
        self.narrow &= !DOUBLED;
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
        (self.cursor.pending_wrap, self.cursor.restored) = (col >= end, false);
        self.cursor.col = col.min(end.saturating_sub(1));
    }
    /// Moves the cursor to row `row`, the last if past it.
    #[inline]
    pub fn set_row(&mut self, row: u16) {
        self.cursor.row = row.min(self.size.lines().last());
        self.fit_cursor();
    }
    /// Moves the cursor to column `col`, the last if past it.
    #[inline]
    /// A column past the end of a row half as long is its callers' to
    /// bring back (`fit_cursor`): most move left, or to a column on it.
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
        self.cursor.col = if self.lr() {
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
    /// Always inlined: as a call, CUP counts 1.5% more instructions
    /// (vt/cursor-motion, fux-bench).
    #[inline(always)]
    pub fn position(&mut self, row: u16, col: u16) {
        self.cursor.pending_wrap = false;
        let (lines, columns) = self.addressed();
        (self.cursor.row, self.cursor.col) = (lines.nth(row), columns.nth(col));
        self.fit_cursor();
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
    /// CUF, or HPR (`absolute`, outside origin mode): the cursor `n`
    /// columns right, no further than the right margin, for CUF while the
    /// cursor is not past it, for HPR in origin mode (xterm's
    /// `CursorForward` and `CASE_HPR`), else the last column.
    #[inline]
    pub fn forward(&mut self, n: u16, absolute: bool) {
        let col = self.cursor.col.saturating_add(n);
        self.cursor.col = if self.narrow() {
            self.forward_in_margins(col, absolute)
        } else {
            col.min(self.size.columns().last())
        };
    }
    /// `forward` with left and right margins, or on a row half as long,
    /// whose end bounds it too.
    #[cold]
    #[inline(never)]
    fn forward_in_margins(&self, col: u16, absolute: bool) -> u16 {
        let end = self.row_end(self.cursor.row).saturating_sub(1);
        if !self.lr() {
            return col.min(end);
        }
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
        .min(end)
    }
    /// CUB: the cursor `n` columns left, no further than the left margin,
    /// unless it is left of it already (xterm's `CursorBack`).
    #[inline]
    pub fn backward(&mut self, n: u16) {
        let col = self.cursor.col.saturating_sub(n);
        let left = self.columns.first();
        self.cursor.col = if self.lr() && self.cursor.col >= left {
            col.max(left)
        } else {
            col
        };
    }
    /// CHA and HPA: the cursor to column `col`, counting from the left
    /// margin in origin mode, no further than the right one.
    #[inline]
    pub fn column(&mut self, col: u16) {
        self.cursor.col = if self.narrow() {
            self.column_in_margins(col)
        } else {
            col.min(self.size.columns().last())
        };
    }
    /// `column` with left and right margins, from the left one in origin
    /// mode, or on a row half as long, whose end bounds it.
    #[cold]
    #[inline(never)]
    fn column_in_margins(&self, col: u16) -> u16 {
        let col = if self.lr() && self.cursor.origin {
            self.columns.nth(col)
        } else {
            col.min(self.size.columns().last())
        };
        col.min(self.row_end(self.cursor.row).saturating_sub(1))
    }
    /// Moves the cursor to line `line` as CUP does, keeping its column,
    /// which origin mode keeps within the right margin (VPA and VPR, as
    /// xterm addresses them).
    #[inline]
    pub fn position_line(&mut self, line: u16) {
        let col = self.cursor.col;
        let (lines, columns) = self.addressed();
        self.cursor.pending_wrap = false;
        (self.cursor.row, self.cursor.col) = (lines.nth(line), col.min(columns.last()));
        self.fit_cursor();
    }
}

/// The column one past `col` if a wrap is pending there, else `col`.
fn past(col: u16, pending_wrap: bool) -> u16 {
    col.saturating_add(u16::from(pending_wrap))
}

/// Gives the cells `span` of the row in slot `slot` link `link`, or none:
/// the row takes `version` if a cell's link changed. Given the grid's
/// parts, which it borrows apart.
#[inline(always)]
fn set_link(
    meta: &mut [Meta],
    links: &mut [RowLinks],
    slot: Option<usize>,
    span: Range<usize>,
    link: Option<&Hyperlink>,
    version: u64,
) {
    // Printing with no link open, into a row with none, as most printing
    // after a link was seen is, changes nothing: one test.
    if let Some(slot) = slot
        && let Some(links) = links.get_mut(slot)
        && (link.is_some() || !links.is_empty())
        && links.set(span, link)
        && let Some(m) = meta.get_mut(slot)
    {
        m.version = version;
    }
}

/// What `write_ascii` made of a run.
#[must_use]
#[derive(Clone, Copy, Debug)]
pub(crate) enum Written {
    /// It wrote all of it.
    All,
    /// The cells were already the run: nothing changed.
    Same,
    /// It wrote none of it, as it meets half of a wide glyph, which the
    /// general path repairs, after this many bytes: those it would write.
    Before(usize),
}

/// How many of `cells` come before the first that is half of a wide
/// glyph. Out of line: a run seldom meets one.
#[cold]
#[inline(never)]
fn first_half(cells: &[Compact]) -> usize {
    cells
        .iter()
        .position(|c| c.is_wide() || c.is_wide_continuation())
        .unwrap_or(cells.len())
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

/// ICH and DCH on a row's cells: what is at and after
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

/// If cell `at` is the second half of a wide glyph, blanks it and the
/// first half before it; whether it did.
fn split_wide(cells: &mut [Compact], at: usize) -> bool {
    let Some(pair) = cells.get_mut(at.saturating_sub(1)..=at) else {
        return false;
    };
    if !pair.last().is_some_and(Compact::is_wide_continuation) {
        return false;
    }
    for cell in pair {
        *cell = cell.blanked();
    }
    true
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

/// The grid's part of the invariant checker (threats.md section 6).
#[cfg(feature = "checks")]
impl Grid {
    /// Runs every check of the grid, its history, styles and links, the
    /// grid named `which` and its screen at `version`, into `report`.
    pub(crate) fn check(&self, which: &str, version: u64, report: &mut crate::Checked) {
        report.record("grid.order", which, self.check_order());
        report.record("grid.slots", which, self.check_slots());
        report.record("grid.blank_past_used", which, self.check_blank_past_used());
        report.record("grid.wide_pairs", which, self.check_wide_pairs());
        report.record("grid.text", which, self.check_text());
        report.record("grid.style_numbers", which, self.check_style_numbers());
        report.record("grid.recent", which, self.check_recent());
        report.record("styles.table", which, self.styles.check());
        let keyed = |attributes: &Attributes| {
            std::hash::BuildHasher::hash_one(&crate::style::Keyed, attributes)
        };
        report.record("styles.spread", which, self.styles.check_spread(keyed));
        report.record("grid.versions", which, self.check_versions(version));
        report.record("grid.cursor", which, self.check_cursor());
        report.record("grid.margins", which, self.check_margins());
        report.record("grid.keyboard", which, self.check_keyboard());
        report.record("links.runs", which, self.check_link_runs());
        report.record("links.held", which, self.check_links_held());
        self.history.check(which, report);
    }

    /// Every row of the grid, screen and history, as the checker reads it:
    /// its cells kept, its width, its text and its links. History's are
    /// read without the work counters seeing them.
    fn checked_rows(
        &self,
    ) -> impl Iterator<Item = (&[Compact], usize, &Text, Option<&RowLinks>)> + '_ {
        let history = (0..self.history.len()).filter_map(|i| {
            let found = self.history.get(i)?;
            let width = usize::from(found.kept.width());
            Some((found.cells, width, found.text, found.links))
        });
        let cols = usize::from(self.size.cols());
        let screen = (self.order.iter()).filter_map(move |&slot| {
            let links = self.linked.get(slot).filter(|l| !l.is_empty());
            Some((self.slice(slot), cols, self.texts.get(slot)?, links))
        });
        history.chain(screen)
    }

    /// 7: the order is a permutation of the slots, one a row.
    fn check_order(&self) -> Result<(), String> {
        let rows = usize::from(self.size.rows());
        crate::check::ensure(self.order.len() == rows, || {
            format!("{} rows ordered, {rows} rows", self.order.len())
        })?;
        crate::check::ensure(self.order.top < rows.max(1), || {
            format!("the order's top {} past {rows} rows", self.order.top)
        })?;
        let mut seen = vec![false; self.meta.len()];
        for &slot in &self.order.slots {
            match seen.get_mut(slot) {
                Some(seen) if !*seen => *seen = true,
                Some(_) => return Err(format!("slot {slot} ordered twice")),
                None => return Err(format!("slot {slot} past {} slots", self.meta.len())),
            }
        }
        crate::check::ensure(seen.iter().all(|s| *s), || "a slot no row has".into())
    }

    /// 8 and 10: one record, text and links a slot; the screen's cells, a
    /// row of the grid's width a slot, once made.
    fn check_slots(&self) -> Result<(), String> {
        let slots = self.meta.len();
        crate::check::ensure(
            self.texts.len() == slots && self.linked.len() == slots,
            || {
                format!(
                    "{slots} slots, {} texts, {} links",
                    self.texts.len(),
                    self.linked.len()
                )
            },
        )?;
        let cells = if self.unmade {
            0
        } else {
            slots.saturating_mul(usize::from(self.size.cols()))
        };
        crate::check::ensure(self.cells.len() == cells, || {
            format!("{} cells for {slots} slots", self.cells.len())
        })
    }

    /// 9: every cell past a slot's `used` mark is blank.
    fn check_blank_past_used(&self) -> Result<(), String> {
        crate::check::ensure(self.blank_past_used(), || {
            "a cell past a row's used mark is not blank".into()
        })
    }

    /// 11: a wide glyph's halves are side by side, on the screen and in
    /// history.
    fn check_wide_pairs(&self) -> Result<(), String> {
        for (row, (cells, width, _, _)) in self.checked_rows().enumerate() {
            let cell = |i: usize| cells.get(i).copied().unwrap_or(BLANK);
            for i in 0..width {
                let here = cell(i);
                let paired = if here.is_wide() {
                    i.checked_add(1)
                        .filter(|j| *j < width)
                        .is_some_and(|j| cell(j).is_wide_continuation())
                } else if here.is_wide_continuation() {
                    i.checked_sub(1).is_some_and(|j| cell(j).is_wide())
                } else {
                    true
                };
                if !paired {
                    return Err(format!("row {row}, column {i}: half a wide glyph"));
                }
            }
        }
        Ok(())
    }

    /// 13: each row's text within its limits, and every cell's text where
    /// its locator says.
    fn check_text(&self) -> Result<(), String> {
        for (row, (cells, width, text, _)) in self.checked_rows().enumerate() {
            text.check(cells, width)
                .map_err(|e| format!("row {row}: {e}"))?;
        }
        Ok(())
    }

    /// 14: every style of the table a cell has, on the screen and in
    /// history, is in the grid's table.
    fn check_style_numbers(&self) -> Result<(), String> {
        let len = self.styles.len();
        for (row, (cells, _, _, _)) in self.checked_rows().enumerate() {
            for (i, cell) in cells.iter().enumerate() {
                if Styles::place(cell.style()).is_some_and(|place| place >= len) {
                    return Err(format!(
                        "row {row}, column {i}: style {:#x} past {len} styles",
                        cell.style()
                    ));
                }
            }
        }
        Ok(())
    }

    /// The styles found lately are the table's, with their attributes.
    fn check_recent(&self) -> Result<(), String> {
        for (at, &(attributes, style)) in self.recent.iter().enumerate() {
            if attributes == Attributes::default() {
                continue;
            }
            crate::check::ensure(
                self.styles.get(style) == attributes.with_guard(false),
                || format!("style {style:#x} found lately at {at} has other attributes"),
            )?;
        }
        Ok(())
    }

    /// 20: every row's version is at most the screen's, `version`.
    fn check_versions(&self, version: u64) -> Result<(), String> {
        let screen = self.meta.iter().map(|m| m.version);
        let history =
            (0..self.history.len()).filter_map(|i| self.history.kept(i).map(|k| k.version));
        match history.chain(screen).find(|v| *v > version) {
            Some(v) => Err(format!("a row's version {v} past the screen's {version}")),
            None => Ok(()),
        }
    }

    /// 25: the cursor and the saved cursor are on the grid.
    fn check_cursor(&self) -> Result<(), String> {
        let (rows, cols) = (self.size.rows(), self.size.cols());
        for (name, cursor) in [("cursor", self.cursor), ("saved cursor", self.saved.cursor)] {
            crate::check::ensure(cursor.row < rows && cursor.col < cols, || {
                format!("{name} at {:?} off {rows}x{cols}", cursor.at())
            })?;
        }
        Ok(())
    }

    /// 26: the margins are inside the grid, each the whole of it or two
    /// lines or columns apart at least, and `lr` says whether the left and
    /// right ones are narrower than the grid.
    fn check_margins(&self) -> Result<(), String> {
        let (rows, cols) = (self.size.rows(), self.size.cols());
        let inside = |span: Span, n: u16| span.first() <= span.last() && span.last() < n;
        crate::check::ensure(
            inside(self.lines, rows) && inside(self.columns, cols),
            || {
                format!(
                    "margins {:?} {:?} off {rows}x{cols}",
                    self.lines, self.columns
                )
            },
        )?;
        let set = |span: Span, whole: Span| span == whole || span.first() < span.last();
        crate::check::ensure(
            set(self.lines, self.size.lines()) && set(self.columns, self.size.columns()),
            || {
                format!(
                    "margins {:?} {:?} one line or column",
                    self.lines, self.columns
                )
            },
        )?;
        crate::check::ensure(self.lr() == (self.columns != self.size.columns()), || {
            format!("lr {} with columns {:?}", self.lr(), self.columns)
        })?;
        // A row half as long is never missed: its end bounds the cursor
        // and printing only while `doubled` says one may be.
        let halved = (0..rows).any(|row| self.rendition(row).is_double());
        crate::check::ensure(self.doubled() || !halved, || {
            "a row is half as long, unmarked".into()
        })
    }

    /// 36: the kitty keyboard stack holds at most its limit.
    fn check_keyboard(&self) -> Result<(), String> {
        crate::check::ensure(self.keyboard.0.len() <= KEYBOARD_STACK_LIMIT, || {
            format!("{} kitty keyboard flags held", self.keyboard.0.len())
        })
    }

    /// 24: each row's link runs inside its width, in order and apart.
    fn check_link_runs(&self) -> Result<(), String> {
        for (row, (_, width, _, links)) in self.checked_rows().enumerate() {
            if let Some(links) = links {
                links.check(width).map_err(|e| format!("row {row}: {e}"))?;
            }
        }
        Ok(())
    }

    /// 23 and 24: what the links held cost covers the links the rows have,
    /// within the bound, and each with an `id` is found by it.
    fn check_links_held(&self) -> Result<(), String> {
        let links = self.checked_rows().filter_map(|(_, _, _, links)| links);
        self.links.check(links)
    }
}

/// The grid's checks, each shown a grid with its one fault (HAR-26): a
/// check that stops checking fails here.
#[cfg(all(test, feature = "checks"))]
mod faults {
    use super::*;

    /// A grid of 3 rows of 4, with a few rows of history.
    fn grid() -> Result<Grid, Error> {
        let size = Size::new(3, 4).map_err(|_| Error::Capacity)?;
        let mut next = 1;
        let mut grid = Grid::new(size, 1 << 12, &mut next, 1)?;
        for _ in 0..5 {
            let _ = grid.write_ascii(0, 0, b"ab", 0, 1);
            grid.scroll(
                size.lines(),
                1,
                Scroll::Up { history: true },
                0,
                &mut next,
                1,
            )?;
        }
        Ok(grid)
    }

    /// The checks that found a fault in `grid`.
    fn faulted(grid: &Grid) -> Vec<&'static str> {
        let mut report = crate::Checked::default();
        grid.check("grid", 1, &mut report);
        report.faults().iter().map(|f| f.check).collect()
    }

    /// Whether `grid` is clean, and `fault` makes it fail `check` alone.
    fn shows(check: &str, fault: impl FnOnce(&mut Grid)) -> Result<(), Error> {
        let mut grid = grid()?;
        assert_eq!(faulted(&grid), Vec::<&str>::new());
        fault(&mut grid);
        let found = faulted(&grid);
        assert!(found.contains(&check), "{check}: {found:?}");
        Ok(())
    }

    #[test]
    fn each_check_names_its_fault() -> Result<(), Error> {
        shows("grid.order", |g| {
            if let [first, second, ..] = g.order.slots.as_mut_slice() {
                *first = *second;
            }
        })?;
        shows("grid.slots", |g| {
            g.texts.pop();
        })?;
        shows("grid.blank_past_used", |g| {
            if let Some(cell) = g.cells.get_mut(1) {
                *cell = Compact::ascii(b'x', 0);
            }
            g.meta.iter_mut().for_each(|m| m.used = 0);
        })?;
        shows("grid.wide_pairs", |g| {
            if let Some(cell) = g.cells.get_mut(0) {
                *cell = Compact::glyph('一', 2, 0);
            }
            g.meta.iter_mut().for_each(|m| m.used = 4);
        })?;
        shows("grid.text", |g| {
            let long = "e\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}";
            g.mutate_line(0, 1, 1, |line| line.set(0, Compact::glyph('e', 1, 0), long));
            if let Some(&slot) = g.order.get(0)
                && let Some(text) = g.texts.get_mut(slot)
            {
                *text = Text::default();
            }
        })?;
        shows("grid.style_numbers", |g| {
            if let Some(cell) = g.cells.get_mut(0) {
                cell.set_style(crate::style::TABLE | 5);
            }
            g.meta.iter_mut().for_each(|m| m.used = 4);
        })?;
        shows("grid.recent", |g| {
            let bold = Attributes::default().with_bold(true);
            if let Some(slot) = g.recent.get_mut(0) {
                *slot = (bold, crate::style::TABLE | 3);
            }
        })?;
        shows("grid.versions", |g| {
            g.meta.iter_mut().for_each(|m| m.version = 2);
        })?;
        shows("grid.cursor", |g| g.cursor.row = 3)?;
        shows("grid.margins", |g| g.narrow = LR)?;
        shows("grid.margins", |g| {
            g.set_rendition(0, LineRendition::DoubleWidth, 1);
            g.narrow = 0;
        })?;
        shows("grid.keyboard", |g| g.keyboard.0.extend([1; 33]))?;
        Ok(())
    }
}

/// What a fault test needs of a grid it breaks on purpose.
#[cfg(all(test, feature = "checks"))]
impl Grid {
    /// Gives live row `row` identity `id`, another row's maybe.
    pub(crate) fn give_id(&mut self, row: u16, id: RowId) {
        if let Some(m) = self.slot(row).and_then(|slot| self.meta.get_mut(slot)) {
            m.id = id;
        }
    }
}
