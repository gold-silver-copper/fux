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
//! The grid trims it to its budget of bytes once a row is in (`trim`): what
//! it holds counts, as the memory it takes (`THREATS.md` B17): the ring of
//! rows' records (`Kept`) and every block as their capacity, the rows'
//! text, and the maps' buckets. While it holds more than its budget the
//! oldest rows go, a run of them at a time (`pop_run`). The ring's room
//! comes and goes a chunk at a time (`Ring`), a block's as its last row
//! goes, and a map gives its room back once it has room for more than four
//! times its entries and 64.

use std::collections::{HashMap, VecDeque};

mod ring;
use ring::Ring;
mod spans;
use spans::Spans;
use std::hash::{BuildHasherDefault, Hasher};
use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::compact::{BLANK, Compact, NO_TEXT, Text};
use crate::grid::{MAX_CELLS, MAX_ROWS};
use crate::link::{KeptLinks, Runs};
use crate::{Error, LineRendition, RowId};

/// The cells a block holds, unless one row needs more. Small, as a block
/// partly filled (the newest) or partly left (the oldest) costs what it
/// holds; so many rows to one that a row left at a block's end costs
/// little.
const BLOCK: usize = 1 << 12;
/// The most blocks: their numbers are 16 bits. More than a grid can fill
/// (`grid::MAX_CELLS`): a block made is half full, or holds a row half its
/// size or more.
const BLOCKS: usize = 1 << 16;

/// The bytes `History::texts` and `History::linked` take for each row of
/// their capacity: a bucket is the row's number, its value and the map's
/// control byte (57 and 41 bytes on 64-bit targets), and a map has 8
/// buckets for 7 of capacity (66 and 47 bytes a row).
const TEXT_BUCKET: usize = (size_of::<(u64, Text)>() + 1) * 8 / 7 + 1;
const LINK_BUCKET: usize = (size_of::<(u64, KeptLinks)>() + 1) * 8 / 7 + 1;
/// The bytes `History::renditions` takes for each row of its capacity.
const RENDITION: usize = size_of::<(u64, LineRendition)>();

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

const _: () = assert!(size_of::<Kept>() == 24);

/// The bytes of budget for each row of the ring's chunks: a chunk holds a
/// sixteenth of the most rows the budget holds (`History::shift_for`).
const CHUNKS_PER_BUDGET: usize = size_of::<Kept>() * 16;
const _: () = assert!(BLOCK <= 1 << 12);

/// `Kept`'s flags, the first two the screen's rows keep too (`Meta`).
pub(crate) const WRAPPED: u8 = 1;
pub(crate) const PROMPT: u8 = 2;
/// Where a screen row's flags keep its rendition (`LineRendition::bits`),
/// above `WRAPPED` and `PROMPT`; history keeps it apart (`renditions`).
pub(crate) const RENDITION_SHIFT: u8 = 2;
const LINKED: u8 = 4;
const TEXT: u8 = 8;
/// A screen row's flag, past its rendition's bits: soft-wrapped, as xterm
/// keeps it, but its line ends with it, the row below erased whole since
/// (`Row::continues`). History keeps it apart (`ended`).
pub(crate) const ENDS: u8 = 16;

impl Kept {
    /// What `row` is kept as, its cells at `start` in block `block`.
    #[inline]
    fn new(row: &Arriving<'_>, (block, start): (u16, u16)) -> Self {
        let flags = row.flags & (WRAPPED | PROMPT);
        let len = u16::try_from(row.cells.len()).unwrap_or(u16::MAX);
        Self {
            id: row.id,
            version: row.version,
            place: u64::from(block)
                | u64::from(start & 0xfff) << 16
                | u64::from(len) << 28
                | u64::from(row.width) << 44
                | u64::from(flags & 0xf) << 60,
        }
    }
    #[inline]
    fn block(&self) -> u16 {
        u16::try_from(self.place & 0xffff).unwrap_or(0)
    }
    #[inline]
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
}

/// The most room a map of history's holds for `len` entries before it
/// gives some back (`History::maps_changed`).
fn most_room(len: usize) -> usize {
    len.saturating_mul(4).saturating_add(64)
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
    /// Whether it is soft-wrapped (`WRAPPED`) and whether a prompt starts
    /// on it (`PROMPT`), and its rendition in the bits above them, as the
    /// screen's row kept them.
    pub(crate) flags: u8,
    /// Its cells before its blank tail (`trimmed`), of `width`.
    pub(crate) cells: &'a [Compact],
    pub(crate) width: u16,
}

/// A row of history as a reader finds it.
pub(crate) struct Found<'a> {
    pub(crate) kept: &'a Kept,
    /// Its flags as a screen row's (`WRAPPED`, `PROMPT`, its rendition
    /// from `RENDITION_SHIFT`).
    pub(crate) flags: u8,
    pub(crate) cells: &'a [Compact],
    pub(crate) text: &'a Text,
    pub(crate) links: Option<Runs<'a>>,
}

/// An `Option<u64>` noted through `&self`, while it is read
/// (`Screen::track_pictures`, from `Screen::pictures`): kept in an atomic,
/// so that a `Screen`, which is `Sync`, read from several threads at once
/// notes it alike from each. `None` is kept as `u64::MAX`, which is no
/// row's number.
#[derive(Debug)]
pub(crate) struct Noted(AtomicU64);

impl Noted {
    /// None noted.
    pub(crate) const fn none() -> Self {
        Self(AtomicU64::new(u64::MAX))
    }
    pub(crate) fn get(&self) -> Option<u64> {
        Some(self.0.load(Ordering::Relaxed)).filter(|n| *n != u64::MAX)
    }
    pub(crate) fn set(&self, n: Option<u64>) {
        self.0.store(n.unwrap_or(u64::MAX), Ordering::Relaxed);
    }
}

/// None noted.
impl Default for Noted {
    fn default() -> Self {
        Self::none()
    }
}

impl Clone for Noted {
    fn clone(&self) -> Self {
        Self(AtomicU64::new(self.0.load(Ordering::Relaxed)))
    }
}

#[derive(Debug, Default)]
pub(crate) struct History {
    /// The rows, oldest first.
    rows: Ring,
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
    /// The cells the blocks have room for, `older`'s and the newest's.
    room_cells: usize,
    /// The bytes the ring, the blocks and the maps take, as their
    /// capacity: what the rows hold but their text (`recount`); and the
    /// maps' share of it.
    held: usize,
    maps: usize,
    /// The bytes the rows' links' runs take (`THREATS.md` B15).
    runs: usize,
    texts: ByRow<Text>,
    linked: ByRow<KeptLinks>,
    /// The room each map's table was made with, its buckets as allocated:
    /// `HashMap::capacity` counts out the entries removed since, whose
    /// buckets the table keeps, so it is noted when the table
    /// grows or is made anew, and read for what the maps hold.
    texts_room: usize,
    linked_room: usize,
    /// The rows that are not single width, by number, oldest first: most
    /// histories have none, and a row's record has no bit to spare.
    renditions: VecDeque<(u64, LineRendition)>,
    /// The rows whose line ends with them though soft-wrapped (`ENDS`), by
    /// number, oldest first: as rare.
    ended: VecDeque<u64>,
    /// The rows by identity (`position`): its first `spans::FREE_SPANS`
    /// spans at no cost to the budget, so that what a history keeps does
    /// not change with how its rows' identities run; the rest counted in
    /// it, `spans::SPAN_BYTES` each (`THREATS.md` B40).
    spans: Spans,
    /// The cells the rows keep, for `storage_cells`.
    cells: usize,
    /// The bytes of the rows' text.
    text: usize,
    /// The most bytes the rows hold (`bytes`) once trimmed.
    budget: usize,
    /// Bytes counted in the budget beside the rows': the grid's style
    /// table's beyond the screen's share (`THREATS.md` B11).
    charged: usize,
    /// What the rows may hold: the budget less what is charged.
    room: usize,
    /// The newest version of a row let go past the budget.
    pub(crate) let_go: u64,
    /// The number of the row to come next when the screen's pictures
    /// were last followed into history (`Screen::track_pictures`).
    followed: Noted,
}

/// A copy keeps each block's room, as rows fill the newest within it, and
/// counts again what the ring and the maps it made take, as `Ring`'s copy
/// does for the ring: a derived copy kept the original's counts beside
/// storage of other sizes.
impl Clone for History {
    fn clone(&self) -> Self {
        let block = |b: &Vec<Compact>| {
            let mut copy = Vec::with_capacity(b.capacity());
            copy.extend_from_slice(b);
            copy
        };
        let mut copy = Self {
            rows: self.rows.clone(),
            first_row: self.first_row,
            older: self.older.iter().map(block).collect(),
            newest: block(&self.newest),
            first_block: self.first_block,
            newest_block: self.newest_block,
            room_cells: self.room_cells,
            held: 0,
            maps: 0,
            runs: self.runs,
            texts: self.texts.clone(),
            linked: self.linked.clone(),
            texts_room: self.texts_room,
            linked_room: self.linked_room,
            renditions: self.renditions.clone(),
            ended: self.ended.clone(),
            spans: self.spans.clone(),
            cells: self.cells,
            text: self.text,
            budget: self.budget,
            charged: self.charged,
            room: self.room,
            let_go: self.let_go,
            followed: self.followed.clone(),
        };
        copy.recount();
        copy
    }
}

impl History {
    pub(crate) fn new(budget: usize) -> Self {
        Self {
            rows: Ring::new(Self::shift_for(budget)),
            budget,
            room: budget,
            ..Self::default()
        }
    }
    /// The ring's chunks for `budget`: a sixteenth of the most rows it
    /// holds, as a power of two, from 1 to 512 rows (12 KiB), so that
    /// what a chunk holds unused is little of the budget.
    fn shift_for(budget: usize) -> u32 {
        let rows = (budget.checked_div(CHUNKS_PER_BUDGET))
            .unwrap_or(0)
            .clamp(1, 512);
        rows.ilog2()
    }
    pub(crate) fn len(&self) -> usize {
        self.rows.len()
    }
    /// Where the screen's pictures were last followed to.
    pub(crate) fn followed(&self) -> &Noted {
        &self.followed
    }
    pub(crate) fn budget(&self) -> usize {
        self.budget
    }
    /// The bytes history holds: its ring, blocks and maps as their
    /// capacity, and the rows' text; and the bytes charged to it.
    #[cfg(any(test, feature = "checks"))]
    pub(crate) fn bytes(&self) -> usize {
        self.rows_bytes().saturating_add(self.charged)
    }
    /// The bytes the rows hold: the ring, blocks and maps as their
    /// capacity, and the rows' text.
    #[inline(always)]
    fn rows_bytes(&self) -> usize {
        self.held.saturating_add(self.text)
    }
    /// Counts again what the ring, the blocks and the maps take: after any
    /// of them is made, grows, shrinks or goes.
    fn recount(&mut self) {
        self.maps = self.maps_bytes();
        self.held = (self.rows.capacity().saturating_mul(size_of::<Kept>()))
            .saturating_add(self.room_cells.saturating_mul(size_of::<Compact>()))
            .saturating_add(self.maps)
            .saturating_add(self.runs);
    }
    /// The bytes the maps' buckets take.
    fn maps_bytes(&self) -> usize {
        (self.texts_room.saturating_mul(TEXT_BUCKET))
            .saturating_add(self.linked_room.saturating_mul(LINK_BUCKET))
            .saturating_add(self.renditions.capacity().saturating_mul(RENDITION))
            .saturating_add(self.ended.capacity().saturating_mul(size_of::<u64>()))
            .saturating_add(self.spans.charged())
    }
    /// The cells a block holds: a block's worth (`BLOCK`), or for a small
    /// budget an eighth of it, so that a block is never most of what
    /// history may hold.
    fn block_cells(&self) -> usize {
        (self.budget / 64).clamp(1, BLOCK)
    }
    /// The bytes charged beside the rows (`charge`).
    #[cfg(feature = "checks")]
    pub(crate) fn charged(&self) -> usize {
        self.charged
    }
    /// Counts `bytes` in the budget beside the rows, in place of what was
    /// charged before, and lets the oldest rows go to fit.
    pub(crate) fn charge(&mut self, bytes: usize) {
        self.charged = bytes;
        self.room = self.budget.saturating_sub(bytes);
        self.trim();
    }
    /// Lets the oldest rows go while history holds more than the budget
    /// (`make_room` keeps it to the rows and cells a grid may hold,
    /// `grid::MAX_ROWS` and `MAX_CELLS`). What it holds changes only out
    /// of line, so a row arriving usually lets none go.
    #[inline]
    pub(crate) fn trim(&mut self) {
        if self.over() {
            self.trim_over();
        }
    }
    /// Whether history holds more than it may.
    #[inline(always)]
    fn over(&self) -> bool {
        self.rows_bytes() > self.room
    }
    /// `trim` when history holds more than it may: the oldest rows go
    /// until it doesn't; with none left, it holds nothing.
    #[cold]
    #[inline(never)]
    fn trim_over(&mut self) {
        while !self.rows.is_empty() && self.over() {
            self.pop_run();
        }
        if self.rows.is_empty() && self.over() {
            self.release();
        }
    }
    /// Lets all its storage go, holding no row.
    fn release(&mut self) {
        self.runs = 0;
        self.rows = Ring::new(Self::shift_for(self.budget));
        self.older.clear();
        self.newest = Vec::new();
        self.newest_block = self.first_block;
        self.room_cells = 0;
        self.texts = ByRow::default();
        self.linked = ByRow::default();
        (self.texts_room, self.linked_room) = (0, 0);
        self.renditions = VecDeque::new();
        self.ended = VecDeque::new();
        self.spans = Spans::default();
        self.recount();
    }
    /// Holds at most `budget` bytes from now on, letting the oldest rows
    /// go at once if it holds more: the rows that fit are kept anew, in
    /// storage made for the new budget, with their numbers.
    pub(crate) fn set_budget(&mut self, budget: usize) {
        let mut fresh = Self::new(budget);
        (fresh.first_row, fresh.let_go) = (self.first_row, self.let_go);
        fresh.followed = self.followed.clone();
        fresh.charge(self.charged);
        fresh.spans.stage();
        let took = fresh.take(self);
        fresh.spans.finish();
        if took.is_ok() {
            *self = fresh;
        } else {
            // No memory for it: the rows go from where they are.
            self.budget = budget;
            self.room = budget.saturating_sub(self.charged);
            self.trim();
        }
    }
    /// Takes in `old`'s rows, oldest first, letting the oldest go as it
    /// fills; fails only if there is no memory for them.
    fn take(&mut self, old: &Self) -> Result<(), Error> {
        for index in 0..old.len() {
            let Some(found) = old.get(index) else {
                continue;
            };
            let kept = found.kept;
            self.push(Arriving {
                id: kept.id,
                version: kept.version,
                flags: found.flags,
                cells: found.cells,
                width: kept.width(),
            })?;
            let text = (!found.text.is_empty()).then(|| found.text.exact());
            if text.is_some() || found.links.is_some() {
                self.attach(text, found.links.and_then(KeptLinks::of));
            }
            self.trim();
        }
        Ok(())
    }
    /// The cells the rows keep.
    pub(crate) fn kept_cells(&self) -> usize {
        self.cells
    }
    /// Whether nothing is held: no row, and no storage made for one.
    pub(crate) fn is_bare(&self) -> bool {
        self.rows.capacity() == 0 && self.older.is_empty() && self.newest.capacity() == 0
    }
    pub(crate) fn kept(&self, index: usize) -> Option<&Kept> {
        self.rows.get(index)
    }
    /// The number of row `index` in the order rows came.
    fn number(&self, index: usize) -> Option<u64> {
        self.first_row.checked_add(u64::try_from(index).ok()?)
    }
    /// The numbers, in the order rows came, of the oldest row kept and of
    /// the next to come.
    pub(crate) fn numbers(&self) -> (u64, u64) {
        let next = self.number(self.rows.len()).unwrap_or(u64::MAX);
        (self.first_row, next)
    }
    /// Where the row numbered `number` in the order rows came is, oldest
    /// first, if it is kept.
    pub(crate) fn index_of_number(&self, number: u64) -> Option<usize> {
        let index = usize::try_from(number.checked_sub(self.first_row)?).ok()?;
        (index < self.rows.len()).then_some(index)
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
    #[inline(always)]
    pub(crate) fn get(&self, index: usize) -> Option<Found<'_>> {
        let kept = self.kept(index)?;
        let cells = if kept.len() == 0 {
            &[][..]
        } else {
            self.block(kept.block())?.get(Self::cells_of(kept)?)?
        };
        let number = self.number(index)?;
        // Looked up only for a row its flags say has some.
        let text = (kept.flags() & TEXT != 0).then(|| self.texts.get(&number));
        let text = text.flatten().unwrap_or(&NO_TEXT);
        let links = (kept.flags() & LINKED != 0).then(|| self.linked.get(&number));
        let links = links.flatten().map(KeptLinks::runs);
        let mut flags = kept.flags() & (WRAPPED | PROMPT);
        if !self.renditions.is_empty() {
            flags |= self.rendition_of(number).bits() << RENDITION_SHIFT;
        }
        if !self.ended.is_empty() && self.ended.binary_search(&number).is_ok() {
            flags |= ENDS;
        }
        Some(Found {
            kept,
            flags,
            cells,
            text,
            links,
        })
    }
    /// Where the row with identity `id` is, oldest first: found by its
    /// span (`Spans`), one row read (`THREATS.md` B40).
    pub(crate) fn position(&self, id: RowId) -> Option<usize> {
        let index = self.index_of_number(self.spans.number_of(id.0)?)?;
        spend!(rows, 1);
        (self.rows.get(index)?.id == id).then_some(index)
    }
    /// The links of each row that has any, with its index, oldest first.
    pub(crate) fn links(&self) -> impl Iterator<Item = (usize, Runs<'_>)> {
        // The link sweep walks every row for those with links.
        spend!(links, self.rows.len());
        let linked = self.rows.iter().enumerate();
        linked
            .filter(|(_, kept)| kept.flags() & LINKED != 0)
            .filter_map(|(index, _)| Some((index, self.linked.get(&self.number(index)?)?.runs())))
    }
    /// Calls `f` with every cell the rows keep (not those of rows gone,
    /// which a block keeps until every row in it has gone): to mark the
    /// styles they use, and renumber them.
    pub(crate) fn each_cell_mut(&mut self, mut f: impl FnMut(&mut Compact)) {
        let (older, newest) = (&mut self.older, &mut self.newest);
        for kept in self.rows.iter() {
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

    /// Room for a row of `len` cells, if there is not: a chunk more in
    /// `rows`; and its cells, after the newest block's or in a block of its
    /// own: the newest block grows, doubling, until it holds a block
    /// (`block_cells`); else a block starts, of a block's cells, or just the
    /// row's for a row longer than half a block, or for the first block as
    /// much as its first row needs. Fails only if there is no memory for
    /// it. Out of line: a row arriving in history usually fits where the
    /// last one ended.
    #[cold]
    #[inline(never)]
    fn make_room(&mut self, len: usize) -> Result<(), Error> {
        let block = self.block_cells();
        if !self.rows.has_room() {
            // The oldest go first, so that with the chunk made it holds no
            // more rows than a grid may (`MAX_ROWS`).
            let most = MAX_ROWS.saturating_sub(self.rows.chunk());
            while self.rows.len() > most {
                self.pop();
            }
            self.rows.grow()?;
            self.recount();
        }
        if self.fits(len) {
            return Ok(());
        }
        // No more blocks than their numbers count, nor cells than a grid
        // may hold: the oldest rows go to make room.
        let most = MAX_CELLS.saturating_sub(len.max(block));
        while !self.rows.is_empty()
            && (self.older.len() >= BLOCKS.saturating_sub(2) || self.room_cells > most)
        {
            self.pop();
        }
        let end = self.newest.len().saturating_add(len);
        if self.newest.capacity() > 0 && end <= block {
            let before = self.newest.capacity();
            let capacity = before.saturating_mul(2).clamp(end, block);
            crate::reserve(&mut self.newest, capacity)?;
            self.room_cells = (self.room_cells)
                .saturating_add(self.newest.capacity())
                .saturating_sub(before);
            self.recount();
            return Ok(());
        }
        let first = self.newest.capacity() == 0 && self.older.is_empty();
        let capacity = if first || len > block / 2 { len } else { block };
        if self.older.len() >= BLOCKS.saturating_sub(2) {
            return Err(Error::Capacity);
        }
        let mut cells = Vec::new();
        crate::reserve(&mut cells, capacity)?;
        self.start_block(cells);
        Ok(())
    }

    /// Takes in `row` after the rows there are: its cells are those before
    /// its blank tail (`trimmed`). Fails, with nothing changed, only if
    /// there is no memory for it. Inlined: it is most of what scrolling a
    /// row into history does (`Grid::scroll_into_history`).
    #[inline(always)]
    pub(crate) fn push(&mut self, row: Arriving<'_>) -> Result<(), Error> {
        let len = row.cells.len();
        if !self.rows.has_room() || !self.fits(len) {
            self.make_room(len)?;
        }
        work!(rows, 1);
        work!(cells, len);
        let start = u16::try_from(self.newest.len()).unwrap_or(0);
        // A row of one cell, as a short line's often is, is stored, not
        // copied by a call.
        match row.cells {
            [cell] => self.newest.push(*cell),
            cells => self.newest.extend_from_slice(cells),
        }
        self.cells = self.cells.saturating_add(len);
        // A row whose identity follows the last one's goes on with its
        // span; any other begins one.
        if self.spans.went_on(row.id.0) {
            self.begin_span(row.id);
        }
        self.rows
            .push_back(Kept::new(&row, (self.newest_block, start)));
        if (row.flags >> RENDITION_SHIFT) & 3 != 0 {
            self.keep_rendition(LineRendition::from_bits(row.flags >> RENDITION_SHIFT));
        }
        if row.flags & ENDS != 0 {
            let _ = self.keep_ended();
        }
        Ok(())
    }

    /// Rows are about to be pushed in from another history (a resize):
    /// the spans they begin are found once it is done (`finish_spans`).
    pub(crate) fn stage_spans(&mut self) {
        self.spans.stage();
    }
    /// The rows a resize pushed are all in: their spans are found now.
    pub(crate) fn finish_spans(&mut self) {
        self.spans.finish();
    }

    /// Notes that the row coming next, `id`, begins a span of identities.
    /// Out of line, as rows come one identity after another as a screen
    /// scrolls them in.
    #[cold]
    #[inline(never)]
    fn begin_span(&mut self, id: RowId) {
        let number = self.number(self.rows.len()).unwrap_or(u64::MAX);
        let before = self.spans.charged();
        self.spans.begin(id.0, number, self.first_row);
        // What the spans cost changes alone: counted again by the change,
        // as `recount` would count it.
        let after = self.spans.charged();
        self.maps = self.maps.saturating_sub(before).saturating_add(after);
        self.held = self.held.saturating_sub(before).saturating_add(after);
    }

    /// Notes the rendition of the row pushed last, which is not single
    /// width. Out of line, as most rows are.
    #[cold]
    #[inline(never)]
    fn keep_rendition(&mut self, rendition: LineRendition) {
        let Some(number) = self.rows.len().checked_sub(1).and_then(|i| self.number(i)) else {
            return;
        };
        // No memory for it: the row is kept single width.
        if self.renditions.try_reserve(1).is_ok() {
            self.renditions.push_back((number, rendition));
            self.recount();
        }
    }
    /// Notes that the row pushed last, soft-wrapped, ends its line (`ENDS`).
    #[cold]
    #[inline(never)]
    fn keep_ended(&mut self) -> bool {
        let Some(number) = self.rows.len().checked_sub(1).and_then(|i| self.number(i)) else {
            return false;
        };
        // No memory for it: the line goes on, as xterm's would.
        let new = self.ended.back() != Some(&number) && self.ended.try_reserve(1).is_ok();
        if new {
            self.ended.push_back(number);
            self.recount();
        }
        new
    }
    /// Whether a row kept is not single width.
    pub(crate) fn has_renditions(&self) -> bool {
        !self.renditions.is_empty()
    }
    /// The rendition of row `number`, single width unless noted.
    #[cold]
    #[inline(never)]
    fn rendition_of(&self, number: u64) -> LineRendition {
        let found = self.renditions.binary_search_by_key(&number, |&(n, _)| n);
        found
            .ok()
            .and_then(|i| self.renditions.get(i))
            .map_or(LineRendition::Single, |&(_, r)| r)
    }
    /// Lets the renditions of the rows let go go with them.
    #[cold]
    #[inline(never)]
    fn drop_renditions(&mut self) {
        while self
            .renditions
            .front()
            .is_some_and(|&(n, _)| n < self.first_row)
        {
            self.renditions.pop_front();
        }
        while self.ended.front().is_some_and(|&n| n < self.first_row) {
            self.ended.pop_front();
        }
        self.maps_changed();
    }

    /// Lets the oldest rows go up to the first whose going may change what
    /// history holds, and it: one with text or links, the last with cells
    /// in its block, one in a later block, or the last of its chunk. As
    /// `pop` a row at a time, in a loop that reads each row once.
    fn pop_run(&mut self) {
        let first_block = self.first_block;
        let first_end = self.older.front().map(Vec::len);
        let (mut n, mut let_go, mut cells, mut leave) = (0usize, self.let_go, 0usize, None);
        for kept in self.rows.front_run() {
            if kept.flags() & (TEXT | LINKED) != 0 {
                break;
            }
            n = n.saturating_add(1);
            let_go = let_go.max(kept.version);
            cells = cells.saturating_add(kept.len());
            // A row with no cells is in no block: its number is the newest's
            // when it came.
            if kept.len() == 0 {
                continue;
            }
            if kept.block() != first_block {
                leave = Some(kept.block());
                break;
            }
            if first_end == Some(kept.start().saturating_add(kept.len())) {
                leave = Some(first_block.wrapping_add(1));
                break;
            }
        }
        if n == 0 {
            self.pop();
            return;
        }
        work!(rows, n);
        self.first_row = self
            .first_row
            .wrapping_add(u64::try_from(n).unwrap_or(u64::MAX));
        self.let_go = let_go;
        self.cells = self.cells.saturating_sub(cells);
        if !self.renditions.is_empty() || !self.ended.is_empty() {
            self.drop_renditions();
        }
        let freed = self.rows.advance(n);
        match leave {
            Some(block) => self.leave_blocks(block),
            None if freed => self.recount(),
            None => {}
        }
    }

    /// Lets the oldest row go, and its links; and the blocks no row's
    /// cells are in now: before its own, or its own too if its cells were
    /// the last there.
    #[inline]
    fn pop(&mut self) {
        let Some((gone, freed)) = self.rows.pop_front() else {
            return;
        };
        work!(rows, 1);
        let number = self.first_row;
        self.first_row = self.first_row.wrapping_add(1);
        self.let_go = self.let_go.max(gone.version);
        self.cells = self.cells.saturating_sub(gone.len());
        if gone.flags() & (TEXT | LINKED) != 0 {
            self.forget(number, gone.flags());
        }
        if !self.renditions.is_empty() || !self.ended.is_empty() {
            self.drop_renditions();
        }
        let ended = (gone.len() > 0)
            .then(|| gone.start().saturating_add(gone.len()))
            .is_some_and(|end| {
                gone.block() == self.first_block
                    && self.older.front().is_some_and(|first| first.len() == end)
            });
        if ended {
            self.leave_blocks(gone.block().wrapping_add(1));
        } else if gone.len() > 0 && gone.block() != self.first_block {
            self.leave_blocks(gone.block());
        } else if freed {
            // Its chunk of the ring went with it.
            self.recount();
        }
    }

    /// Starts `block`, empty, after the newest, if there is one; it has
    /// room for the row to come (`make_room`).
    fn start_block(&mut self, block: Vec<Compact>) {
        self.room_cells = self.room_cells.saturating_add(block.capacity());
        // The newest goes before it, if it was a block; if not, the new
        // block takes its number, which rows with no cells have.
        let last = std::mem::replace(&mut self.newest, block);
        if last.capacity() > 0 {
            self.older.push_back(last);
            self.newest_block = self.newest_block.wrapping_add(1);
        }
        self.recount();
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
        if flags & LINKED != 0
            && let Some(links) = self.linked.remove(&number)
        {
            self.runs = self.runs.saturating_sub(links.heap());
            self.maps_changed();
            self.recount();
            return;
        }
        self.maps_changed();
    }

    /// After a map's entries changed: each gives back its room when it
    /// holds room for more than four times its entries and 64, so that a
    /// map that grows by doubling as rows come and go doesn't shrink back
    /// at once (`THREATS.md` B17); and what history holds is counted again.
    fn maps_changed(&mut self) {
        let shrink = |capacity: usize, len: usize| capacity > most_room(len);
        if shrink(self.texts_room, self.texts.len()) {
            self.texts.shrink_to(self.texts.len());
            self.texts_room = self.texts.capacity();
        }
        if shrink(self.linked_room, self.linked.len()) {
            self.linked.shrink_to(self.linked.len());
            self.linked_room = self.linked.capacity();
        }
        if shrink(self.renditions.capacity(), self.renditions.len()) {
            self.renditions.shrink_to(self.renditions.len());
        }
        if shrink(self.ended.capacity(), self.ended.len()) {
            self.ended.shrink_to(self.ended.len());
        }
        if self.maps_bytes() != self.maps {
            self.recount();
        }
    }

    /// Lets the blocks before block `block` go: no row is in them, as
    /// rows go oldest first.
    #[cold]
    #[inline(never)]
    fn leave_blocks(&mut self, block: u16) {
        let left = usize::from(block.wrapping_sub(self.first_block));
        // A block before the first is none to leave.
        if left > self.older.len() {
            return;
        }
        for _ in 0..left {
            if let Some(block) = self.older.pop_front() {
                self.first_block = self.first_block.wrapping_add(1);
                self.room_cells = self.room_cells.saturating_sub(block.capacity());
            }
        }
        self.recount();
    }

    /// Gives the row pushed last its text, if it keeps any, and its links,
    /// if it has any.
    #[cold]
    #[inline(never)]
    pub(crate) fn attach(&mut self, text: Option<Text>, links: Option<KeptLinks>) {
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
            self.texts_room = self.texts_room.max(self.texts.capacity());
            kept.set(TEXT, true);
        }
        if let Some(links) = links {
            self.runs = self.runs.saturating_add(links.heap());
            self.linked.insert(number, links);
            self.linked_room = self.linked_room.max(self.linked.capacity());
            kept.set(LINKED, true);
            self.recount();
        } else if self.maps_bytes() != self.maps {
            // Inserting only grows a map.
            self.recount();
        }
    }

    /// Whether row `index`'s line goes on in the row after: soft-wrapped,
    /// and not noted ended (`ENDS`).
    pub(crate) fn continues(&self, index: usize) -> bool {
        let wrapped = self.kept(index).is_some_and(Kept::wrapped);
        wrapped
            && (self.ended.is_empty()
                || self
                    .number(index)
                    .is_none_or(|n| self.ended.binary_search(&n).is_err()))
    }
    /// The newest row's line ends with it, which then takes `version`: the
    /// screen's top row, that it went on in, was erased whole. It stays
    /// soft-wrapped, as xterm keeps it (`ENDS`).
    pub(crate) fn end_newest_line(&mut self, version: u64) {
        if self
            .rows
            .len()
            .checked_sub(1)
            .and_then(|i| self.rows.get(i))
            .is_some_and(Kept::wrapped)
            && self.keep_ended()
            && let Some(kept) = self.rows.back_mut()
        {
            kept.version = version;
            // Its note counts in the budget.
            self.trim();
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
            if let Some(links) = self.linked.remove(&number) {
                self.runs = self.runs.saturating_sub(links.heap());
            }
            self.maps_changed();
            self.recount();
        }
    }

    /// The heap bytes held, counted from the storage itself: what `bytes`
    /// counts less what is charged.
    #[cfg(any(test, feature = "checks"))]
    pub(crate) fn heap(&self) -> usize {
        let cell = std::mem::size_of::<Compact>();
        self.older
            .iter()
            .chain([&self.newest])
            .map(|block| block.capacity().saturating_mul(cell))
            .sum::<usize>()
            .saturating_add(
                self.rows
                    .capacity()
                    .saturating_mul(std::mem::size_of::<Kept>()),
            )
            .saturating_add(self.maps_bytes())
            .saturating_add(self.linked.values().map(KeptLinks::heap).sum::<usize>())
            .saturating_add(self.text)
    }

    /// Forgets every row's links: RIS.
    pub(crate) fn reset_links(&mut self) {
        self.linked.clear();
        self.runs = 0;
        for kept in self.rows.iter_mut() {
            kept.set(LINKED, false);
        }
        self.maps_changed();
        self.recount();
    }
}

/// History's part of the invariant checker (THREATS.md section 6).
#[cfg(feature = "checks")]
impl History {
    /// Runs every check of history, of the grid named `which`, into
    /// `report`.
    pub(crate) fn check(&self, which: &str, report: &mut crate::Checked) {
        report.record("history.counts", which, self.check_counts());
        report.record("history.blocks", which, self.check_blocks());
        report.record("history.flags", which, self.check_flags());
        report.record("history.budget", which, self.check_budget());
        report.record("history.capacities", which, self.check_capacities());
    }

    /// 17: the cells and text counted are those the rows keep, and the
    /// storage counted is the storage held.
    fn check_counts(&self) -> Result<(), String> {
        crate::check::ensure(self.rows_bytes() == self.heap(), || {
            format!("{} bytes counted, {} held", self.rows_bytes(), self.heap())
        })?;
        let cells = self
            .rows
            .iter()
            .map(Kept::len)
            .fold(0, usize::saturating_add);
        let text = self
            .texts
            .values()
            .map(Text::bytes)
            .fold(0, usize::saturating_add);
        crate::check::ensure(cells == self.cells && text == self.text, || {
            format!(
                "{} cells and {} bytes of text counted, {cells} and {text} kept",
                self.cells, self.text
            )
        })
    }

    /// 10 and 18: each row's cells are in a block held, within it, and no
    /// more than its width.
    fn check_blocks(&self) -> Result<(), String> {
        for (i, kept) in self.rows.iter().enumerate() {
            crate::check::ensure(kept.len() <= usize::from(kept.width()), || {
                format!("row {i} keeps {} cells of {}", kept.len(), kept.width())
            })?;
            if kept.len() == 0 {
                continue;
            }
            let block = self.block(kept.block());
            let fits =
                Self::cells_of(kept).is_some_and(|c| block.is_some_and(|b| c.end <= b.len()));
            crate::check::ensure(fits, || {
                format!("row {i}'s cells are not in block {}", kept.block())
            })?;
        }
        Ok(())
    }

    /// 19: a row's text and link flags say whether the maps have it.
    fn check_flags(&self) -> Result<(), String> {
        let (mut texts, mut linked) = (0usize, 0usize);
        for (i, kept) in self.rows.iter().enumerate() {
            let number = self.number(i).unwrap_or(u64::MAX);
            let (text, links) = (kept.flags() & TEXT != 0, kept.flags() & LINKED != 0);
            crate::check::ensure(
                text == self.texts.contains_key(&number)
                    && links == self.linked.contains_key(&number),
                || format!("row {i}'s flags {:#x} disagree with the maps", kept.flags()),
            )?;
            texts = texts.saturating_add(usize::from(text));
            linked = linked.saturating_add(usize::from(links));
        }
        crate::check::ensure(
            texts == self.texts.len() && linked == self.linked.len(),
            || {
                format!(
                    "{} texts and {} links kept for {texts} and {linked} rows",
                    self.texts.len(),
                    self.linked.len()
                )
            },
        )?;
        // The lines noted ended are of rows kept, oldest first, each
        // soft-wrapped.
        let (first, next) = self.numbers();
        let mut after = first;
        for &number in &self.ended {
            let wrapped = number
                .checked_sub(first)
                .and_then(|i| usize::try_from(i).ok())
                .and_then(|i| self.rows.get(i))
                .is_some_and(Kept::wrapped);
            crate::check::ensure((after..next).contains(&number) && wrapped, || {
                format!("a line noted ended at row {number} of {first}..{next}")
            })?;
            after = number.saturating_add(1);
        }
        // The renditions noted are of rows kept, oldest first, and none
        // single width.
        let (first, next) = self.numbers();
        let mut after = first;
        for &(number, rendition) in &self.renditions {
            crate::check::ensure(
                (after..next).contains(&number) && rendition.is_double(),
                || format!("a rendition noted for row {number} of {first}..{next}"),
            )?;
            after = number.saturating_add(1);
        }
        // 20: each row is found by its identity where it is (`position`).
        for (i, kept) in self.rows.iter().enumerate() {
            let found = self.spans.number_of(kept.id.0);
            crate::check::ensure(found == self.number(i), || {
                format!("row {i}, {:?}, is found as number {found:?}", kept.id)
            })?;
        }
        Ok(())
    }

    /// 21: within the budget, and the most rows and cells a grid holds.
    fn check_budget(&self) -> Result<(), String> {
        crate::check::ensure(
            self.bytes() <= self.budget && self.rows.len() <= MAX_ROWS && self.cells <= MAX_CELLS,
            || {
                format!(
                    "{} bytes of {}, {} rows, {} cells",
                    self.bytes(),
                    self.budget,
                    self.rows.len(),
                    self.cells
                )
            },
        )
    }

    /// 21: the ring holds room for at most two chunks more than it keeps,
    /// and the maps at most four times what they keep and 64 (THREATS.md
    /// B17).
    fn check_capacities(&self) -> Result<(), String> {
        let ring = self
            .rows
            .len()
            .saturating_add(self.rows.chunk().saturating_mul(2));
        crate::check::ensure(self.rows.capacity() < ring, || {
            format!(
                "the ring holds room for {}, keeping {}",
                self.rows.capacity(),
                self.rows.len()
            )
        })?;
        for (name, room, capacity) in [
            ("texts", self.texts_room, self.texts.capacity()),
            ("links", self.linked_room, self.linked.capacity()),
        ] {
            crate::check::ensure(room >= capacity, || {
                format!("the {name}' table counted as {room}, where it has room for {capacity}")
            })?;
        }
        for (name, capacity, len) in [
            ("texts", self.texts_room, self.texts.len()),
            ("links", self.linked_room, self.linked.len()),
            (
                "renditions",
                self.renditions.capacity(),
                self.renditions.len(),
            ),
        ] {
            crate::check::ensure(capacity <= most_room(len), || {
                format!("the {name} holds room for {capacity}, keeping {len}")
            })?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;

/// History's checks, each shown a history with its one fault.
#[cfg(all(test, feature = "checks"))]
mod faults {
    use super::*;

    /// A history of rows, a few cells each, one with text.
    fn history() -> History {
        let mut history = History::new(1 << 12);
        let cells = [Compact::ascii(b'x', 0); 3];
        for id in 0..6u64 {
            let row = Arriving {
                id: RowId(id),
                version: 1,
                flags: 0,
                cells: &cells,
                width: 4,
            };
            assert!(history.push(row).is_ok());
            history.trim();
        }
        history
    }

    fn faulted(history: &History) -> Vec<&'static str> {
        let mut report = crate::Checked::default();
        history.check("history", &mut report);
        report.faults().iter().map(|f| f.check).collect()
    }

    fn shows(check: &str, fault: impl FnOnce(&mut History)) {
        let mut history = history();
        assert_eq!(faulted(&history), Vec::<&str>::new());
        fault(&mut history);
        let found = faulted(&history);
        assert!(found.contains(&check), "{check}: {found:?}");
    }

    #[test]
    fn each_check_names_its_fault() {
        shows("history.counts", |h| h.cells = h.cells.saturating_add(1));
        shows("history.blocks", |h| {
            if let Some(kept) = h.rows.back_mut() {
                kept.place = (kept.place & !0xffff) | 0x7fff;
            }
        });
        shows("history.flags", |h| {
            if let Some(kept) = h.rows.back_mut() {
                kept.set(TEXT, true);
            }
        });
        shows("history.flags", |h| {
            h.renditions
                .push_back((u64::MAX, LineRendition::DoubleWidth));
        });
        shows("history.flags", |h| {
            if let Some(kept) = h.rows.back_mut() {
                kept.id = RowId(1 << 40);
            }
        });
        shows("history.budget", |h| h.budget = 0);
        shows("history.capacities", |h| h.rows.reserve_more(1000));
    }
}
