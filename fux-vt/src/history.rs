//! A grid's history: the rows scrolled off the top of its screen, oldest
//! first, as many as fit its budget of bytes.
//!
//! A row is kept as the cells it uses: those before its last cell that is
//! not blank in the default attributes, which is all most rows need of
//! their width, in blocks of cells that rows fill one after another and
//! leave, oldest first, as they go; a block left empty is kept for the next
//! to fill. The cells past a row's own are blank, as they were. With each
//! row go its identity and version, its width, whether it is soft-wrapped
//! and whether a prompt starts on it; and, for the rows that have any, its
//! text (`compact::Text`) and its links, kept beside the blocks by the
//! row's number in the order rows came.
//!
//! A row scrolled off is the commonest thing a terminal does, so taking it
//! in is a few loads and stores, its cells going after the newest block's;
//! whatever else (a block to start or leave, text, links) is out of line.
//! The grid trims it to its budget of bytes once a row is in (`trim`): each
//! row's record (`Kept`), the cells it keeps and its text count.

use std::collections::{HashMap, VecDeque};
use std::hash::{BuildHasherDefault, Hasher};
use std::ops::Range;

use crate::compact::{BLANK, Compact, NO_TEXT, Text};
use crate::grid::{MAX_CELLS, MAX_ROWS};
use crate::link::RowLinks;
use crate::{Error, RowId};

/// The cells a block holds, unless one row needs more. Small, as a block
/// partly filled (the newest), partly left (the oldest) and kept for the
/// next (the spare) cost what they hold; so many rows to one that a row
/// left at a block's end costs little.
const BLOCK: usize = 1 << 12;
/// The most blocks: their numbers are 16 bits. More than a grid can fill
/// (`grid::MAX_CELLS`): a block made is half full, or holds a row half its
/// size or more.
const BLOCKS: usize = 1 << 16;

/// A map by a row's number in the order rows came.
type ByRow<T> = HashMap<u64, T, BuildHasherDefault<SlotHasher>>;

/// Hashes a row's number, a small number, by one multiplication
/// (Fibonacci hashing), rather than by SipHash, which the default hasher
/// spends on every lookup: it is no input a program chooses.
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
    fn write_u64(&mut self, n: u64) {
        self.0 = (self.0 ^ n).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    }
}

/// A row of history: what it is, and where its cells are.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Kept {
    pub(crate) id: RowId,
    pub(crate) version: u64,
    /// The rest, in one word: the number of the block its cells are in (16
    /// bits, the blocks made before it, wrapping), where in the block they
    /// start (12: a row in a block with others starts before `BLOCK`), how
    /// many cells it keeps and how many it has, the rest blank (16 each),
    /// and its flags (4).
    place: u64,
}

const _: () = assert!(std::mem::size_of::<Kept>() == 24);
const _: () = assert!(BLOCK <= 1 << 12);

/// `Kept`'s flags.
const WRAPPED: u8 = 1;
const PROMPT: u8 = 2;
const LINKED: u8 = 4;
const TEXT: u8 = 8;

impl Kept {
    #[inline]
    fn new(
        id: RowId,
        version: u64,
        (block, start): (u16, u16),
        len: u16,
        width: u16,
        flags: u8,
    ) -> Self {
        Self {
            id,
            version,
            place: u64::from(block)
                | u64::from(start & 0xfff) << 16
                | u64::from(len) << 28
                | u64::from(width) << 44
                | u64::from(flags & 0xf) << 60,
        }
    }
    #[inline]
    fn block(&self) -> u16 {
        u16::try_from(self.place & 0xffff).unwrap_or(0)
    }
    fn start(&self) -> usize {
        usize::try_from((self.place >> 16) & 0xfff).unwrap_or(0)
    }
    #[inline]
    pub(crate) fn len(&self) -> usize {
        usize::try_from((self.place >> 28) & 0xffff).unwrap_or(0)
    }
    pub(crate) fn width(&self) -> u16 {
        u16::try_from((self.place >> 44) & 0xffff).unwrap_or(0)
    }
    #[inline]
    fn flags(&self) -> u8 {
        u8::try_from(self.place >> 60).unwrap_or(0)
    }
    fn set(&mut self, flag: u8, on: bool) {
        let bit = u64::from(flag) << 60;
        if on {
            self.place |= bit;
        } else {
            self.place &= !bit;
        }
    }
    pub(crate) fn wrapped(&self) -> bool {
        self.flags() & WRAPPED != 0
    }
    pub(crate) fn prompt(&self) -> bool {
        self.flags() & PROMPT != 0
    }
}

/// The cells of a row before its blank tail: those history keeps.
#[inline]
pub(crate) fn trimmed(cells: &[Compact]) -> &[Compact] {
    let len = cells
        .iter()
        .rposition(|cell| *cell != BLANK)
        .map_or(0, |last| last.saturating_add(1));
    cells.get(..len).unwrap_or_default()
}

/// What a row brings into history, besides its text and links
/// (`History::attach`).
#[derive(Clone, Copy)]
pub(crate) struct Arriving<'a> {
    pub(crate) id: RowId,
    pub(crate) version: u64,
    pub(crate) wrapped: bool,
    pub(crate) prompt: bool,
    /// Its cells before its blank tail (`trimmed`), of `width`.
    pub(crate) cells: &'a [Compact],
    pub(crate) width: u16,
}

/// A row of history as a reader finds it.
pub(crate) struct Found<'a> {
    pub(crate) kept: &'a Kept,
    pub(crate) cells: &'a [Compact],
    pub(crate) text: &'a Text,
    pub(crate) links: Option<&'a RowLinks>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct History {
    /// The rows, oldest first.
    rows: VecDeque<Kept>,
    /// The number of the rows come before the oldest.
    first_row: u64,
    /// The blocks before the newest, oldest first, and the newest, which
    /// rows fill; none yet if it holds no memory.
    older: VecDeque<Vec<Compact>>,
    newest: Vec<Compact>,
    /// The number of `older`'s first block (wrapping); the newest's,
    /// `newest_block`, is the number after `older`'s last.
    first_block: u16,
    newest_block: u16,
    /// A block the rows left, kept for the next to fill.
    spare: Vec<Compact>,
    texts: ByRow<Text>,
    linked: ByRow<RowLinks>,
    /// The cells the rows keep, for `storage_cells`.
    cells: usize,
    /// The bytes of the rows' text.
    text: usize,
    /// The most bytes the rows hold (`bytes`) once trimmed.
    budget: usize,
    /// The newest version of a row let go past the budget.
    pub(crate) let_go: u64,
}

impl History {
    pub(crate) fn new(budget: usize) -> Self {
        Self {
            budget,
            ..Self::default()
        }
    }
    pub(crate) fn len(&self) -> usize {
        self.rows.len()
    }
    pub(crate) fn budget(&self) -> usize {
        self.budget
    }
    /// The bytes the rows hold: each one's record, its cells and its text.
    pub(crate) fn bytes(&self) -> usize {
        (self.rows.len().saturating_mul(std::mem::size_of::<Kept>()))
            .saturating_add(self.cells.saturating_mul(std::mem::size_of::<Compact>()))
            .saturating_add(self.text)
    }
    /// The most rows the budget holds, and one more, as a row comes in
    /// before the oldest goes.
    fn most_rows(&self) -> usize {
        (self.budget.checked_div(std::mem::size_of::<Kept>()))
            .unwrap_or(0)
            .min(MAX_ROWS)
            .saturating_add(1)
    }
    /// Lets the oldest rows go while they hold more than the budget, or
    /// more rows or cells than a grid may (`grid::MAX_ROWS`, `MAX_CELLS`).
    #[inline]
    pub(crate) fn trim(&mut self) {
        while !self.rows.is_empty()
            && (self.bytes() > self.budget || self.rows.len() > MAX_ROWS || self.cells > MAX_CELLS)
        {
            self.pop();
        }
    }
    /// Holds at most `budget` bytes from now on, letting the oldest rows
    /// go at once if it holds more.
    pub(crate) fn set_budget(&mut self, budget: usize) {
        self.budget = budget;
        self.trim();
        self.rows.shrink_to(self.most_rows());
    }
    /// The cells the rows keep.
    pub(crate) fn kept_cells(&self) -> usize {
        self.cells
    }
    /// Whether nothing is held: no row, and no storage made for one.
    pub(crate) fn is_bare(&self) -> bool {
        self.rows.capacity() == 0
            && self.older.is_empty()
            && self.newest.capacity() == 0
            && self.spare.capacity() == 0
    }
    pub(crate) fn kept(&self, index: usize) -> Option<&Kept> {
        self.rows.get(index)
    }
    /// The number of row `index` in the order rows came.
    fn number(&self, index: usize) -> Option<u64> {
        self.first_row.checked_add(u64::try_from(index).ok()?)
    }
    /// Where the block numbered `block` is, if it is held, given the
    /// first's number and how many are older than the newest: its index in
    /// `older`, or `older`'s length for the newest.
    fn block_at(first: u16, older: usize, block: u16) -> Option<usize> {
        let at = usize::from(block.wrapping_sub(first));
        (at <= older).then_some(at)
    }
    /// The block numbered `block`, if it is held.
    fn block(&self, block: u16) -> Option<&Vec<Compact>> {
        let at = Self::block_at(self.first_block, self.older.len(), block)?;
        Some(self.older.get(at).unwrap_or(&self.newest))
    }
    /// Where row `kept`'s cells are in its block.
    fn cells_of(kept: &Kept) -> Option<Range<usize>> {
        let start = kept.start();
        Some(start..start.checked_add(kept.len())?)
    }
    /// Row `index`, oldest first.
    pub(crate) fn get(&self, index: usize) -> Option<Found<'_>> {
        let kept = self.kept(index)?;
        let cells = if kept.len() == 0 {
            &[][..]
        } else {
            self.block(kept.block())?.get(Self::cells_of(kept)?)?
        };
        let number = self.number(index)?;
        let text = if kept.flags() & TEXT != 0 {
            self.texts.get(&number).unwrap_or(&NO_TEXT)
        } else {
            &NO_TEXT
        };
        let links = if kept.flags() & LINKED != 0 {
            self.linked.get(&number)
        } else {
            None
        };
        Some(Found {
            kept,
            cells,
            text,
            links,
        })
    }
    /// Where the row with identity `id` is, oldest first.
    pub(crate) fn position(&self, id: RowId) -> Option<usize> {
        self.rows.iter().position(|kept| kept.id == id)
    }
    /// The links of each row that has any, with its index, oldest first.
    pub(crate) fn links(&self) -> impl Iterator<Item = (usize, &RowLinks)> {
        let linked = self.rows.iter().enumerate();
        linked
            .filter(|(_, kept)| kept.flags() & LINKED != 0)
            .filter_map(|(index, _)| Some((index, self.linked.get(&self.number(index)?)?)))
    }
    /// Calls `f` with every cell the rows keep (not those of rows gone,
    /// which a block keeps until every row in it has gone): to mark the
    /// styles they use, and renumber them.
    pub(crate) fn each_cell_mut(&mut self, mut f: impl FnMut(&mut Compact)) {
        let (older, newest) = (&mut self.older, &mut self.newest);
        for kept in &self.rows {
            let at = Self::block_at(self.first_block, older.len(), kept.block());
            let block = at.map(|at| older.get_mut(at).unwrap_or(&mut *newest));
            if let Some(cells) =
                Self::cells_of(kept).and_then(|cells| block.and_then(|b| b.get_mut(cells)))
            {
                cells.iter_mut().for_each(&mut f);
            }
        }
    }

    /// Whether `len` cells go after the newest block's: they end within a
    /// block, in the room it has.
    #[inline]
    fn fits(&self, len: usize) -> bool {
        let end = self.newest.len().saturating_add(len);
        len == 0 || end <= BLOCK && end <= self.newest.capacity()
    }

    /// Room for a row of `len` cells, if there is not: a row more in
    /// `rows`, doubling, up to the most the budget holds (`most_rows`);
    /// and its cells, after the newest block's or in a block of its own:
    /// the newest block grows, doubling, until it holds a block; else a
    /// block starts, the spare if it holds the row, or a block, or for the
    /// first block as much as its first row needs. Fails only if there is
    /// no memory for it. Out of line: a row arriving in history usually
    /// fits where the last one ended.
    #[cold]
    #[inline(never)]
    fn make_room(&mut self, len: usize) -> Result<(), Error> {
        if self.rows.len() == self.rows.capacity() {
            let capacity = self
                .rows
                .len()
                .saturating_add(1)
                .saturating_mul(2)
                .min(self.most_rows().max(self.rows.len().saturating_add(1)));
            self.rows
                .try_reserve_exact(capacity.saturating_sub(self.rows.len()))
                .map_err(|_| Error::Capacity)?;
        }
        if self.fits(len) {
            return Ok(());
        }
        let end = self.newest.len().saturating_add(len);
        if self.newest.capacity() > 0 && end <= BLOCK {
            let capacity = self.newest.capacity().saturating_mul(2).clamp(end, BLOCK);
            return crate::reserve(&mut self.newest, capacity);
        }
        if self.older.len() >= BLOCKS.saturating_sub(2) {
            return Err(Error::Capacity);
        }
        if self.spare.capacity() < len {
            let capacity = if self.newest.capacity() == 0 && self.older.is_empty() {
                len
            } else {
                len.max(BLOCK)
            };
            let mut block = Vec::new();
            crate::reserve(&mut block, capacity)?;
            self.spare = block;
        }
        self.start_block();
        Ok(())
    }

    /// Takes in `row` after the rows there are: its cells are those before
    /// its blank tail (`trimmed`). Fails, with nothing changed, only if
    /// there is no memory for it. Inlined: it is most of what scrolling a
    /// row into history does (`Grid::scroll_into_history`).
    #[inline(always)]
    pub(crate) fn push(&mut self, row: Arriving<'_>) -> Result<(), Error> {
        let len = row.cells.len();
        if self.rows.len() == self.rows.capacity() || !self.fits(len) {
            self.make_room(len)?;
        }
        let start = u16::try_from(self.newest.len()).unwrap_or(0);
        // A row of one cell, as a short line's often is, is stored, not
        // copied by a call.
        match row.cells {
            [cell] => self.newest.push(*cell),
            cells => self.newest.extend_from_slice(cells),
        }
        self.keep(row, (self.newest_block, start));
        Ok(())
    }

    /// Notes `row`, its cells at `place`, after the rows there are.
    fn keep(&mut self, row: Arriving<'_>, place: (u16, u16)) {
        let len = row.cells.len();
        let flags = (if row.wrapped { WRAPPED } else { 0 }) | (if row.prompt { PROMPT } else { 0 });
        let len16 = u16::try_from(len).unwrap_or(u16::MAX);
        self.cells = self.cells.saturating_add(len);
        self.rows.push_back(Kept::new(
            row.id,
            row.version,
            place,
            len16,
            row.width,
            flags,
        ));
    }

    /// Lets the oldest row go, and its links; and, once
    /// a row goes from a later block than the oldest, the blocks before
    /// that, which no row is in now.
    #[inline]
    fn pop(&mut self) {
        let Some(gone) = self.rows.pop_front() else {
            return;
        };
        let number = self.first_row;
        self.first_row = self.first_row.wrapping_add(1);
        self.let_go = self.let_go.max(gone.version);
        self.cells = self.cells.saturating_sub(gone.len());
        if gone.flags() & (TEXT | LINKED) != 0 {
            self.forget(number, gone.flags());
        }
        if gone.block() != self.first_block {
            self.leave_blocks(gone.block());
        }
    }

    /// Starts a block, the spare, after the newest, if there is one; the
    /// spare has room for the row to come (`make_room`).
    fn start_block(&mut self) {
        let mut block = std::mem::take(&mut self.spare);
        block.clear();
        // The newest goes before it, if it was a block; if not, the new
        // block takes its number, which rows with no cells have.
        let last = std::mem::replace(&mut self.newest, block);
        if last.capacity() > 0 {
            self.older.push_back(last);
            self.newest_block = self.newest_block.wrapping_add(1);
        }
    }

    /// The text and links of row `number`, which `flags` says it has, let
    /// go.
    #[cold]
    #[inline(never)]
    fn forget(&mut self, number: u64, flags: u8) {
        if flags & TEXT != 0
            && let Some(text) = self.texts.remove(&number)
        {
            self.text = self.text.saturating_sub(text.bytes());
        }
        if flags & LINKED != 0 {
            self.linked.remove(&number);
        }
    }

    /// Lets the blocks before block `block` go, the largest kept as the
    /// spare: a row of that block has gone, and rows go oldest first.
    #[cold]
    #[inline(never)]
    fn leave_blocks(&mut self, block: u16) {
        let left = usize::from(block.wrapping_sub(self.first_block));
        for _ in 0..left.min(self.older.len()) {
            if let Some(block) = self.older.pop_front() {
                self.first_block = self.first_block.wrapping_add(1);
                if block.capacity() > self.spare.capacity() {
                    self.spare = block;
                }
            }
        }
    }

    /// Gives the row pushed last its text, if it keeps any, and its links,
    /// if it has any.
    #[cold]
    #[inline(never)]
    pub(crate) fn attach(&mut self, text: Option<Text>, links: Option<RowLinks>) {
        let last = self.rows.len().checked_sub(1);
        let Some(number) = last.and_then(|last| self.number(last)) else {
            return;
        };
        let Some(kept) = self.rows.back_mut() else {
            return;
        };
        if let Some(text) = text.filter(|text| !text.is_empty()) {
            self.text = self.text.saturating_add(text.bytes());
            self.texts.insert(number, text);
            kept.set(TEXT, true);
        }
        if let Some(links) = links {
            self.linked.insert(number, links);
            kept.set(LINKED, true);
        }
    }

    /// Row `index` loses its links, if it has any, and takes `version`.
    pub(crate) fn unlink(&mut self, index: usize, version: u64) {
        if let Some(number) = self.number(index)
            && let Some(kept) = self.rows.get_mut(index)
            && kept.flags() & LINKED != 0
        {
            kept.set(LINKED, false);
            kept.version = version;
            self.linked.remove(&number);
        }
    }

    /// The heap bytes held, for a test of how storage plateaus.
    #[cfg(test)]
    pub(crate) fn heap(&self) -> usize {
        let cell = std::mem::size_of::<Compact>();
        self.older
            .iter()
            .chain([&self.newest, &self.spare])
            .map(|block| block.capacity().saturating_mul(cell))
            .sum::<usize>()
            .saturating_add(
                self.rows
                    .capacity()
                    .saturating_mul(std::mem::size_of::<Kept>()),
            )
    }

    /// Forgets every row's links: RIS.
    pub(crate) fn reset_links(&mut self) {
        self.linked.clear();
        for kept in &mut self.rows {
            kept.set(LINKED, false);
        }
    }
}

#[cfg(test)]
mod tests;
