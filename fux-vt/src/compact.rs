//! The one way cells are stored, eight bytes each: a cell's text and
//! halves, with its style's number (`style.rs`) beside them. A screen's
//! rows and a host's [`Cells`](crate::Cells) store cells so, and readers
//! never see one: a [`CellRef`] reads it with its row's text, and the
//! grid's styles or the `Cells`' attributes.
//!
//! A cell holds up to four bytes of text, one character, inline. A longer
//! cluster goes in its row's text ([`Text`]): those of 5 to 17 bytes, the
//! short ones, in a part of their own that always has room, and the long
//! ones in a part bounded at 32 bytes a cell and one cluster more.

use crate::CellRef;
use crate::cell::{ASCII, CLUSTER_CAPACITY, floor};
use crate::style::Styles;

/// A stored cell. Inline, `text` is the cell's text, zeros after it (a
/// cell's text has no NUL, which is never printed). Spilled, it is where
/// the text is in its row's [`Text`]: an offset of 24 bits, little-endian,
/// then a length, of 5 to 17 bytes in the short clusters and more in the
/// long ones. `word` is the style's number (`style.rs`, 28 bits) and the
/// flags; in a `Cells`, whose attributes are beside its cells, the style
/// is 0. All zeros is a blank cell in the default attributes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Compact {
    text: [u8; 4],
    word: u32,
}

const _: () = assert!(size_of::<Compact>() == 8);

/// A blank cell in the default attributes: what a row is past the cells it
/// keeps.
pub(crate) const BLANK: Compact = Compact {
    text: [0; 4],
    word: 0,
};

/// The text of a row that keeps none.
pub(crate) static NO_TEXT: Text = Text {
    long: Vec::new(),
    short: Vec::new(),
};

/// The style's number, in `word`.
const STYLE: u32 = (1 << 28) - 1;
/// The second half of a wide glyph; and its first. Their bits, shifted
/// down 22, are [`CellRef`]'s.
const CONTINUATION: u32 = 1 << 28;
const WIDE: u32 = 1 << 29;
const HALVES: u32 = WIDE | CONTINUATION;
/// The text is in the row's text.
const SPILLED: u32 = 1 << 30;
/// The glyph is protected from selective erase (DECSCA 1, or SPA): the
/// word's last bit, so a protected cell costs nothing more. Only a glyph
/// printed while the pen protects has it; a blank, and a wide glyph's
/// second half, never do (its first half's says).
pub(crate) const PROTECTED: u32 = 1 << 31;
/// What a cell keeps of its word when its text changes: all but where
/// the text is.
const KEPT: u32 = STYLE | HALVES | PROTECTED;
const _: () = assert!(HALVES >> 22 == CellRef::HALVES as u32);
const _: () = assert!(SHORT == 17 && SPILLED == 1 << 30);

/// The longest cluster a short one is. A long cluster the row has no room
/// for is cut to this.
const SHORT: usize = 17;
/// The bytes a row's short text first takes room for.
const FIRST_SHORT: usize = 256;

impl Compact {
    /// A blank cell in style `style`.
    #[inline]
    pub(crate) fn blank(style: u32) -> Self {
        Self {
            text: [0; 4],
            word: style & STYLE,
        }
    }
    /// The glyph `c`, `width` columns wide, in style `style`, protected
    /// if `style` has [`PROTECTED`] too.
    #[inline]
    pub(crate) fn glyph(c: char, width: usize, style: u32) -> Self {
        // A char is at most four UTF-8 bytes; the rest stay zeros.
        let mut text = [0; 4];
        c.encode_utf8(&mut text);
        Self {
            text,
            word: (style & (STYLE | PROTECTED)) | if width == 2 { WIDE } else { 0 },
        }
    }
    /// The ASCII character `byte` in style `style`, protected if `style`
    /// has [`PROTECTED`] too.
    #[inline]
    pub(crate) fn ascii(byte: u8, style: u32) -> Self {
        Self {
            text: [byte, 0, 0, 0],
            word: style & (STYLE | PROTECTED),
        }
    }
    /// Cells `cells` set to the glyphs of `run`, ASCII, in style `style`, as
    /// [`Compact::ascii`] makes each: each cell one 64-bit value, the glyph's
    /// byte low and the style high, which a run of them stores as fast as
    /// the machine moves words.
    pub(crate) fn fill_ascii(cells: &mut [Self], run: &[u8], style: u32) {
        let high = u64::from(style & (STYLE | PROTECTED)) << 32;
        for (cell, &byte) in cells.iter_mut().zip(run) {
            let bits = high | u64::from(byte);
            // Exact: the low half is the byte, the high half the word.
            *cell = Self {
                text: u32::try_from(bits & 0xffff_ffff).unwrap_or(0).to_le_bytes(),
                word: u32::try_from(bits >> 32).unwrap_or(0),
            };
        }
    }
    /// Whether any of `cells` is either half of a wide glyph: their words
    /// or'd together, which a run of them does without a branch a cell.
    pub(crate) fn any_halves(cells: &[Self]) -> bool {
        Self::any_of(cells, HALVES)
    }
    /// Whether any of `cells` keeps its text in its row's (`Text`).
    pub(crate) fn any_spilled(cells: &[Self]) -> bool {
        Self::any_of(cells, SPILLED)
    }
    /// The bytes of text `cells` keep in their row's (`Text`), as their
    /// locators say: a sum without a branch a cell.
    pub(crate) fn spilled_bytes(cells: &[Self]) -> usize {
        let bytes = (cells.iter()).fold(0u32, |bytes, cell| {
            let (_, len, spilled) = cell.shape();
            bytes.wrapping_add(len & 0u32.wrapping_sub(spilled))
        });
        usize::try_from(bytes).unwrap_or(usize::MAX)
    }
    /// The cell in words, for passes of word arithmetic over runs of
    /// cells: where its text starts, its length, as `locator` reads them
    /// for a spilled cell, and 1 if it is spilled.
    #[inline(always)]
    fn shape(&self) -> (u32, u32, u32) {
        let text = u32::from_le_bytes(self.text);
        (text & 0xff_ffff, text >> 24, (self.word >> 30) & 1)
    }
    /// Whether any of `cells` is protected or either half of a wide glyph.
    pub(crate) fn any_protected_or_halves(cells: &[Self]) -> bool {
        Self::any_of(cells, PROTECTED | HALVES)
    }
    /// Whether any of `cells` has a flag of `flags` in its word.
    fn any_of(cells: &[Self], flags: u32) -> bool {
        cells.iter().fold(0, |any, cell| any | cell.word) & flags != 0
    }
    /// DECSERA's and DECERA's erase of `cells`, which hold no half of a
    /// wide glyph and, for DECSERA (`all` none), no protected glyph: each
    /// glyph a space in its own style, or `all`'s, the empty cells left
    /// empty (`Grid::erase_to_spaces`); whether one changed. A run at once.
    pub(crate) fn space_all(cells: &mut [Self], all: Option<u32>) -> bool {
        let mut changed = false;
        for cell in cells.iter_mut() {
            let occupied = cell.has_contents();
            let space = Self::ascii(b' ', all.unwrap_or(cell.style()));
            if occupied {
                *cell = space;
            }
            changed |= occupied;
        }
        changed
    }
    /// A blank cell in style 0, the halves `halves` says ([`CellRef`]'s).
    #[inline]
    pub(crate) fn shaped(halves: u8) -> Self {
        Self {
            text: [0; 4],
            word: (u32::from(halves) << 22) & HALVES,
        }
    }
    /// The second half of a wide glyph: no text, the default style.
    pub(crate) fn continuation() -> Self {
        Self {
            text: [0; 4],
            word: CONTINUATION,
        }
    }
    /// The cell's style.
    #[inline]
    pub(crate) fn style(&self) -> u32 {
        self.word & STYLE
    }
    /// Gives the cell style `style`, keeping the rest.
    #[inline]
    pub(crate) fn set_style(&mut self, style: u32) {
        self.word = (self.word & !STYLE) | (style & STYLE);
    }
    /// The cell with no text and neither half, in its style.
    #[inline]
    pub(crate) fn blanked(&self) -> Self {
        Self::blank(self.style())
    }
    /// Whether the cell holds text: neither blank nor the second half of a
    /// wide glyph.
    #[inline]
    pub(crate) fn has_contents(&self) -> bool {
        let [first, ..] = self.text;
        first != 0 || self.is_spilled()
    }
    /// Whether the cell holds a glyph two columns wide.
    #[inline]
    pub(crate) fn is_wide(&self) -> bool {
        self.word & WIDE != 0
    }
    /// Whether the cell is the second half of the wide glyph before it.
    #[inline]
    pub(crate) fn is_wide_continuation(&self) -> bool {
        self.word & CONTINUATION != 0
    }
    /// Marks the cell as the leading half of a wide glyph.
    pub(crate) fn widen(&mut self) {
        self.word |= WIDE;
    }
    /// Whether the cell's text is in its row's text.
    #[inline]
    pub(crate) fn is_spilled(&self) -> bool {
        self.word & SPILLED != 0
    }
    /// Whether the glyph is protected from selective erase (DECSCA, SPA).
    #[inline]
    pub(crate) fn is_protected(&self) -> bool {
        self.word & PROTECTED != 0
    }
    /// Whether the cell, of a row whose text is `text`, holds what `other`
    /// holds, its text being `held`: the same text, halves, style and
    /// protection.
    #[cfg(test)]
    pub(crate) fn holds(&self, text: &Text, other: &Self, held: &str) -> bool {
        if !self.is_spilled() && !other.is_spilled() {
            return self == other;
        }
        self.word == other.word && text.of(self) == held
    }
    /// Whether the cell is `blank(style)`: no text, neither half of a wide
    /// glyph, and that style.
    #[inline]
    pub(crate) fn is_blank(&self, style: u32) -> bool {
        *self == Self::blank(style)
    }
    /// Whether the cell is exactly what `ascii(byte, style)` makes.
    #[inline]
    pub(crate) fn is_ascii(&self, byte: u8, style: u32) -> bool {
        *self == Self::ascii(byte, style)
    }
    /// The halves, as [`CellRef`]'s.
    #[inline]
    fn halves(&self) -> u8 {
        let [halves, ..] = ((self.word & HALVES) >> 22).to_le_bytes();
        halves
    }
    /// The inline text; empty for a spilled cell.
    #[inline]
    fn inline(&self) -> &str {
        if self.is_spilled() {
            return "";
        }
        let word = u32::from_le_bytes(self.text);
        // One ASCII byte, as most cells hold, or none: its text without
        // validating it, which readers that walk every cell (copy, search)
        // would pay on each.
        if word < 0x80 {
            let [at, ..] = self.text;
            let at = usize::from(at);
            let len = usize::from(at != 0);
            return ASCII.get(at..at.saturating_add(len)).unwrap_or("");
        }
        str::from_utf8(self.inline_bytes()).unwrap_or("")
    }
    /// The inline text's bytes, unread as UTF-8; none for a spilled cell.
    #[inline]
    fn inline_bytes(&self) -> &[u8] {
        if self.is_spilled() {
            return &[];
        }
        // The bytes up to the last that is not zero: text has none inside.
        let word = u32::from_le_bytes(self.text);
        let len = 32u32.saturating_sub(word.leading_zeros()).div_ceil(8);
        usize::try_from(len)
            .ok()
            .and_then(|len| self.text.get(..len))
            .unwrap_or_default()
    }
    /// The cell with `c` after its inline text, if both fit inline: its
    /// text's bytes and `c`'s as one little-endian word. A cell with no
    /// text has none to follow (`Line::append` gives it a space first).
    #[inline]
    fn inline_with(self, c: char) -> Option<Self> {
        // Full already, as a character of four bytes leaves it.
        let [.., last] = self.text;
        if last != 0 || self.is_spilled() || !self.has_contents() {
            return None;
        }
        let word = u32::from_le_bytes(self.text);
        let used = 32u32.saturating_sub(word.leading_zeros()).div_ceil(8);
        let mut encoded = [0; 4];
        let length = u32::try_from(c.encode_utf8(&mut encoded).len()).ok()?;
        if used.checked_add(length)? > 4 {
            return None;
        }
        let added = u32::from_le_bytes(encoded).checked_shl(used.checked_mul(8)?)?;
        Some(Self {
            text: (word | added).to_le_bytes(),
            word: self.word & KEPT,
        })
    }
    /// The inline text as one little-endian word, zeros after it; `None`
    /// for a spilled cell: what a DECRQCRA checksum decodes without
    /// reading it as UTF-8 (`screen.rs`, `checksum_of`).
    #[inline]
    pub(crate) fn inline_word(&self) -> Option<u32> {
        (!self.is_spilled()).then(|| u32::from_le_bytes(self.text))
    }
    /// The first character of the inline text; none for a spilled cell.
    #[inline]
    pub(crate) fn first(&self) -> Option<char> {
        self.inline().chars().next()
    }
    /// Where a spilled cell's text is in its row's text, and how long it
    /// is.
    #[inline]
    fn locator(&self) -> Option<(usize, usize)> {
        if !self.is_spilled() {
            return None;
        }
        let [a, b, c, len] = self.text;
        let start = usize::try_from(u32::from_le_bytes([a, b, c, 0])).ok()?;
        Some((start, usize::from(len)))
    }
    /// Where a spilled cell's text starts in its part of its row's text.
    #[inline]
    fn start(&self) -> usize {
        let [a, b, c, _] = self.text;
        usize::try_from(u32::from_le_bytes([a, b, c, 0])).unwrap_or(usize::MAX)
    }
    /// Where a long cluster's text is in its row's long text (`long`), or
    /// a short one's in its short text.
    fn spilled(&self, long: bool) -> Option<std::ops::Range<usize>> {
        let (start, len) = self.locator().filter(|(_, len)| (*len > SHORT) == long)?;
        Some(start..start.checked_add(len)?)
    }
    /// The cell, keeping its halves and style, holding `text` inline, if
    /// it fits: what `Line::set` stores, without the row's text.
    #[inline]
    pub(crate) fn holding(self, text: &[u8]) -> Option<Self> {
        let mut bytes = [0; 4];
        match (text, &mut bytes) {
            // One byte, as most text is: stored without a copy's call.
            ([byte], [first, ..]) => *first = *byte,
            (text, bytes) => crate::copy_from(bytes.get_mut(..text.len())?, text)?,
        }
        Some(Self {
            text: bytes,
            word: self.word & KEPT,
        })
    }
    /// The cell, keeping its halves and style, locating `len` bytes of its
    /// row's text from `start`; blank if `start` takes more than 24 bits.
    fn with_spilled(self, start: u32, len: u8) -> Self {
        match start.to_le_bytes() {
            [a, b, c, 0] => Self {
                text: [a, b, c, len],
                word: (self.word & KEPT) | SPILLED,
            },
            _ => self.blanked(),
        }
    }
    /// The cell as a reader sees it, its text in `text` if it is not
    /// inline, its attributes in `styles`.
    #[inline]
    pub(crate) fn read<'a>(&'a self, text: &'a Text, styles: &Styles) -> CellRef<'a> {
        self.read_as(text, styles.get(self.style()))
    }
    /// `read`, the cell's style's attributes being `attributes`.
    #[inline]
    pub(crate) fn read_as<'a>(
        &'a self,
        text: &'a Text,
        attributes: crate::Attributes,
    ) -> CellRef<'a> {
        CellRef::of(text.of(self), attributes, self.halves())
    }
}

/// The text of a row's cells that is not inline: the long clusters, and
/// the short ones, of 5 to 17 bytes, each kind one after another in its own
/// part. Overwritten clusters leave their text behind until the row runs
/// out of room for more and is compacted.
#[derive(Clone, Debug, Default)]
pub(crate) struct Text {
    long: Vec<u8>,
    short: Vec<u8>,
}

impl Text {
    /// The text of `cell`, a cell of this row.
    #[inline]
    pub(crate) fn of<'a>(&'a self, cell: &'a Compact) -> &'a str {
        let Some((start, len)) = cell.locator() else {
            return cell.inline();
        };
        let held = if len > SHORT { &self.long } else { &self.short };
        start
            .checked_add(len)
            .and_then(|end| held.get(start..end))
            .and_then(|s| str::from_utf8(s).ok())
            .unwrap_or("")
    }
    /// `of`'s bytes, unread as UTF-8: what an append copies.
    #[inline]
    pub(crate) fn bytes_of<'a>(&'a self, cell: &'a Compact) -> &'a [u8] {
        let Some((start, len)) = cell.locator() else {
            return cell.inline_bytes();
        };
        let held = if len > SHORT { &self.long } else { &self.short };
        start
            .checked_add(len)
            .and_then(|end| held.get(start..end))
            .unwrap_or_default()
    }
    /// The bytes an append to `cell` follows: its text, or a space for an
    /// empty cell.
    #[inline]
    fn followed<'a>(&'a self, cell: &'a Compact) -> &'a [u8] {
        if cell.has_contents() {
            self.bytes_of(cell)
        } else {
            b" "
        }
    }
    /// Bytes of long clusters kept, live or left behind: the row's
    /// `text_len`, at most `long_limit` of its cells. The short clusters are
    /// not counted, so a `Text` with none of these may still hold text
    /// (`is_empty`).
    pub(crate) fn long_len(&self) -> usize {
        self.long.len()
    }
    /// The bytes of text kept, long clusters and short.
    pub(crate) fn bytes(&self) -> usize {
        self.long.len().saturating_add(self.short.len())
    }
    /// Whether the row keeps no text at all.
    pub(crate) fn is_empty(&self) -> bool {
        self.long.len() == 0 && self.short.is_empty()
    }
    /// Forgets all the text, releasing its memory.
    pub(crate) fn release(&mut self) {
        for part in [&mut self.long, &mut self.short] {
            if part.capacity() != 0 {
                *part = Vec::new();
            }
        }
    }
    /// Forgets all the text, keeping its memory for the row's next: a slot
    /// of the screen, which rows pass through.
    pub(crate) fn empty(&mut self) {
        self.long.clear();
        self.short.clear();
    }
    /// A copy, in no more memory than it needs: what history keeps.
    pub(crate) fn exact(&self) -> Text {
        spend!(text, self.bytes());
        Text {
            long: self.long.as_slice().to_vec(),
            short: self.short.as_slice().to_vec(),
        }
    }
    /// The most bytes of long clusters `cells` cells keep: 32 a cell, about
    /// as much as the cells themselves, and one whole cluster more, so that
    /// even a one-column row holds its longest.
    pub(crate) fn long_limit(cells: usize) -> usize {
        cells
            .saturating_mul(32)
            .saturating_add(CLUSTER_CAPACITY)
            .min(usize::try_from(u32::MAX).unwrap_or(usize::MAX))
    }
    /// The most bytes of short clusters `cells` cells keep: one each. A
    /// cluster grown in place (`Line::append`) may pass it by its growth,
    /// until the next store compacts.
    fn short_limit(cells: usize) -> usize {
        cells.saturating_mul(SHORT)
    }
    /// The long clusters' part (`long`), or the short ones'.
    fn part(&self, long: bool) -> &Vec<u8> {
        if long { &self.long } else { &self.short }
    }
    fn part_mut(&mut self, long: bool) -> &mut Vec<u8> {
        if long {
            &mut self.long
        } else {
            &mut self.short
        }
    }
}

/// Text taken from one row's to another's a cell at a time, joined while
/// each cell's follows the last's on both sides, to be compared and copied
/// as one run: `len` bytes from `from` in one row's part `long` to `to` in
/// the other's.
#[derive(Clone, Copy, Default)]
struct Run {
    long: bool,
    from: usize,
    to: usize,
    len: usize,
}

impl Run {
    /// Whether a cell's text, from `from` to `to` in part `long`, follows
    /// the run's on both sides.
    #[inline]
    fn follows(&self, long: bool, from: usize, to: usize) -> bool {
        self.long == long
            && self.from.saturating_add(self.len) == from
            && self.to.saturating_add(self.len) == to
    }
    /// Writes the run's bytes in `from` over those in `to` where they
    /// differ: whether they did; none if either side is not there.
    fn copy(&self, from: &Text, to: &mut Text) -> Option<bool> {
        let source = (from.part(self.long)).get(self.from..self.from.saturating_add(self.len))?;
        let target = (to.part_mut(self.long)).get_mut(self.to..self.to.saturating_add(self.len))?;
        work!(text, source.len());
        Some(source != target && crate::copy_from(target, source).is_some())
    }
}

/// A part of a row's text that `Line::write` appends clusters to from
/// another row's: its `limit`, the `end` of the other's, the bytes `from`
/// to `to` of the other's not yet appended, and the bytes it will hold
/// once they are (`held`).
#[derive(Clone, Copy)]
struct Part {
    limit: usize,
    end: usize,
    from: usize,
    to: usize,
    held: usize,
}

impl Part {
    fn new(limit: usize, end: usize, held: usize) -> Self {
        Self {
            limit,
            end,
            from: 0,
            to: 0,
            held,
        }
    }
    /// Takes the `len` bytes of a cluster from `start` of `from` into the
    /// run, if they fit, as `store_short` and `store_long` would store
    /// them without compacting (a text of four bytes or fewer they would
    /// keep inline): where they will be. The run so far goes into `to`
    /// first if they do not follow it.
    #[inline(always)]
    fn take(&mut self, start: usize, len: usize, from: &[u8], to: &mut Vec<u8>) -> Option<u32> {
        let end = start.saturating_add(len);
        let held = self.held.saturating_add(len);
        if len <= 4 || end > self.end || held > self.limit {
            return None;
        }
        let first = u32::try_from(self.held).ok()?;
        if self.to != start {
            self.flush(from, to);
            self.from = start;
        }
        (self.to, self.held) = (end, held);
        Some(first)
    }
    /// Appends the run's bytes of `from` to `to`.
    fn flush(&mut self, from: &[u8], to: &mut Vec<u8>) {
        if let Some(bytes) = from.get(self.from..self.to) {
            work!(text, bytes.len());
            to.extend_from_slice(bytes);
        }
        (self.from, self.to) = (0, 0);
    }
}

/// A part of a row's text, the long clusters' (`true`) or the short ones',
/// and bytes of it.
type Place = (bool, std::ops::Range<usize>);

/// All ones for 1, none for 0.
#[inline(always)]
fn mask(bit: u32) -> u32 {
    0u32.wrapping_sub(bit)
}

/// Where the text of the clusters among `cells` is in their row's, if
/// each is of more than four bytes, all in one part, each's text right
/// after the one's before it, and none more than a cell from the one
/// before it, as wide glyphs or glyphs between clusters leave them: that
/// part (`long`) and those bytes. A pass of word arithmetic over each
/// cell and the two before it, with no branch a cell.
fn run_of(cells: &[Compact]) -> Option<Place> {
    let first = cells.iter().position(Compact::is_spilled)?;
    let last = cells.iter().rposition(Compact::is_spilled)?;
    let ((start, len, _), (at, end, _)) = (cells.get(first)?.shape(), cells.get(last)?.shape());
    let long = len > 17;
    let mut odd = 0;
    for cell in cells {
        let (_, len, spilled) = cell.shape();
        odd |= mask(spilled) & (u32::from(len <= 4) | u32::from((len > 17) != long));
    }
    let bytes = usize::try_from(start).ok()?..usize::try_from(at.wrapping_add(end)).ok()?;
    let cells = cells.get(first..=last)?;
    // Every cell a cluster, as most often: each's text follows the one's
    // before it.
    if cells.iter().fold(SPILLED, |all, cell| all & cell.word) != 0 {
        for (one, cell) in cells.iter().zip(cells.iter().skip(1)) {
            let ((at, _, _), (one, len, _)) = (cell.shape(), one.shape());
            odd |= at ^ one.wrapping_add(len);
        }
        return (odd == 0).then_some((long, bytes));
    }
    // Else from the first on, each cluster's text follows the one's a cell
    // or two before it, which there is.
    let (head, ones, twos) = shifted(cells);
    if let Some([one, cell]) = cells
        .get(..2)
        .and_then(|two| <[Compact; 2]>::try_from(two).ok())
    {
        let ((at, _, spilled), (one, len, _)) = (cell.shape(), one.shape());
        odd |= mask(spilled) & (at ^ one.wrapping_add(len));
    }
    for ((cell, one), two) in head.iter().zip(ones).zip(twos) {
        let ((at, _, spilled), (one, one_len, after_one), (two, two_len, after_two)) =
            (cell.shape(), one.shape(), two.shape());
        let before = mask(after_one);
        let after = (one.wrapping_add(one_len) & before) | (two.wrapping_add(two_len) & !before);
        odd |= mask(spilled) & ((at ^ after) | ((after_one | after_two) ^ 1));
    }
    (odd == 0).then_some((long, bytes))
}

/// `run_of` for `cells` and the cells `olds` they go over, each a cluster
/// as long as the one it goes over: each's part and bytes, and whether a
/// cell's word differs from the one's it goes over.
fn runs_over(cells: &[Compact], olds: &[Compact]) -> Option<(Place, Place, bool)> {
    let (first, last) = (cells.first()?, cells.last()?);
    let (old_first, old_last) = (olds.first()?, olds.last()?);
    let shapes = |cell: &Compact, old: &Compact| (cell.shape(), old.shape(), cell.word ^ old.word);
    let ((start, len, spilled), (at, held, kept), word) = shapes(first, old_first);
    let (mut odd, mut words) = ((spilled & kept) ^ 1 | (len ^ held), word);
    let pairs = (cells.iter().zip(olds)).zip(cells.iter().skip(1).zip(olds.iter().skip(1)));
    for ((a, x), (b, y)) in pairs {
        let (((start, len, _), (at, _, _), _), ((next, len_b, spilled), (to, held, kept), word)) =
            (shapes(a, x), shapes(b, y));
        odd |= (spilled & kept) ^ 1 | (len_b ^ held);
        odd |= (start.wrapping_add(len) ^ next) | (at.wrapping_add(len) ^ to);
        odd |= u32::from(len > 17) ^ u32::from(len_b > 17);
        words |= word;
    }
    let ((end, len_last, _), (to, _, _), _) = shapes(last, old_last);
    let bytes = |start: u32, end: u32| {
        Some(usize::try_from(start).ok()?..usize::try_from(end.wrapping_add(len_last)).ok()?)
    };
    let (source, target) = (bytes(start, end)?, bytes(at, to)?);
    (odd == 0).then_some(((len > 17, source), (len > 17, target), words != 0))
}

/// `cells` from the third on, from the second on, and all.
fn shifted(cells: &[Compact]) -> (&[Compact], &[Compact], &[Compact]) {
    let from = |n: usize| cells.get(n..).unwrap_or_default();
    (from(2), from(1), cells)
}

/// The text's part of the invariant checker (threats.md section 6, 13).
#[cfg(feature = "checks")]
impl Text {
    /// Whether the text of a row of `width` cells, `cells` those kept, is
    /// within its limits, and each cell's text is whole where its locator
    /// says, of at most [`CLUSTER_CAPACITY`] bytes.
    pub(crate) fn check(&self, cells: &[Compact], width: usize) -> Result<(), String> {
        let (long, short) = (Self::long_limit(width), Self::short_limit(width));
        crate::check::ensure(
            self.long.len() <= long && self.short.len() <= short.saturating_add(CLUSTER_CAPACITY),
            || {
                format!(
                    "{} long and {} short bytes, limits {long} and {short}",
                    self.long.len(),
                    self.short.len()
                )
            },
        )?;
        for (i, cell) in cells.iter().enumerate() {
            let Some((start, len)) = cell.locator() else {
                continue;
            };
            let held = if len > SHORT { &self.long } else { &self.short };
            let text = start
                .checked_add(len)
                .and_then(|end| held.get(start..end))
                .and_then(|bytes| str::from_utf8(bytes).ok());
            crate::check::ensure(
                text.is_some_and(|t| !t.is_empty() && t.len() <= CLUSTER_CAPACITY),
                || format!("column {i}: text at {start} of {len} bytes is not there"),
            )?;
        }
        Ok(())
    }
}

impl AsRef<Compact> for Compact {
    fn as_ref(&self) -> &Compact {
        self
    }
}
impl AsMut<Compact> for Compact {
    fn as_mut(&mut self) -> &mut Compact {
        self
    }
}

/// A row's cells with their text, to write into: a grid's cells, or a
/// host's with their attributes beside each (`Cells`).
pub(crate) struct Line<'a, C = Compact> {
    pub(crate) cells: &'a mut [C],
    pub(crate) text: &'a mut Text,
}

impl<C: AsRef<Compact> + AsMut<Compact>> Line<'_, C> {
    /// Adds `c` to the cluster in cell `i`; an empty cell first takes a
    /// space for `c` to follow. Whether it was kept: a cluster at
    /// [`CLUSTER_CAPACITY`], or a row out of room, drops it.
    pub(crate) fn append(&mut self, i: usize, c: char) -> bool {
        let Some(&cell) = self.cells.get(i).map(AsRef::as_ref) else {
            return false;
        };
        let Some(end) = (self.text.followed(&cell))
            .len()
            .checked_add(c.len_utf8())
            .filter(|end| *end <= CLUSTER_CAPACITY)
        else {
            return false;
        };
        // The cluster is read and joined again whole.
        work!(text, end);
        // Still inline: the cell holds it all, its text and `c` as one
        // word, with no copy's call.
        if let Some(joined) = cell.inline_with(c)
            && let Some(slot) = self.cells.get_mut(i).map(AsMut::as_mut)
        {
            *slot = joined;
            return true;
        }
        let mut encoded = [0; 4];
        let encoded = c.encode_utf8(&mut encoded).as_bytes();
        let long = end > SHORT;
        let limit = Text::long_limit(self.cells.len());
        // In place, when the cluster is the last text of its length the
        // row keeps. A short cluster is never out of room.
        let (store, room) = if long {
            (&mut self.text.long, limit)
        } else {
            (&mut self.text.short, usize::MAX)
        };
        if let Some(range) = cell.spilled(long)
            && range.end == store.len()
            && store.len().saturating_add(encoded.len()) <= room
            && let (Ok(start), Ok(len)) = (u32::try_from(range.start), u8::try_from(end))
        {
            push_utf8(store, encoded);
            if let Some(slot) = self.cells.get_mut(i).map(AsMut::as_mut) {
                *slot = cell.with_spilled(start, len);
            }
            return true;
        }
        // Else joined in a buffer as long as a cluster of its kind is:
        // whole UTF-8 then a character's encoding is whole UTF-8.
        let current = self.text.followed(&cell);
        if !long {
            let mut buffer = [0u8; SHORT];
            let Some(joined) = join(&mut buffer, current, encoded) else {
                return false;
            };
            self.store_short(i, cell, joined);
            return true;
        }
        let mut buffer = [0u8; CLUSTER_CAPACITY];
        let Some(joined) = join(&mut buffer, current, encoded) else {
            return false;
        };
        // Unlike `set`, the cell keeps what it has if the row has no room,
        // even compacted: an append never loses text.
        self.store_long(i, cell, joined, false)
    }

    /// Sets cell `i` to `cell`'s halves and style holding `text`: inline
    /// if it fits, else in the row's text. A cluster longer than
    /// [`CLUSTER_CAPACITY`] is cut there; a long one the row has no
    /// room for, even compacted, is cut to a short one.
    /// Whether the text was kept whole.
    pub(crate) fn set(&mut self, i: usize, cell: Compact, text: &str) -> bool {
        let kept = floor(text, CLUSTER_CAPACITY);
        self.store(i, cell, kept) && kept.len() == text.len()
    }

    fn store(&mut self, i: usize, cell: Compact, text: &str) -> bool {
        if i >= self.cells.len() {
            return false;
        }
        if text.len() <= SHORT {
            self.store_short(i, cell, text.as_bytes());
            return true;
        }
        if self.store_long(i, cell, text.as_bytes(), true) {
            return true;
        }
        self.store_short(i, cell, floor(text, SHORT).as_bytes());
        false
    }

    /// Sets cell `i` to `cell`'s halves and style holding `text`, a long
    /// cluster, in the row's long text, compacting it first if it is out
    /// of room. If `replace`, the text the cell had goes before that: it
    /// is being replaced. Whether the row had room.
    fn store_long(&mut self, i: usize, cell: Compact, text: &[u8], replace: bool) -> bool {
        let limit = Text::long_limit(self.cells.len());
        if self.text.long.len().saturating_add(text.len()) > limit {
            if replace && let Some(slot) = self.cells.get_mut(i).map(AsMut::as_mut) {
                *slot = slot.blanked();
            }
            // Compacted, the text is what the cells hold, end to end: if
            // that leaves no room either, compacting would copy it all for
            // nothing (threats.md B31).
            if self.live_long().saturating_add(text.len()) <= limit {
                self.compact(true, 0..0);
            }
        }
        let long = &mut self.text.long;
        let (Ok(start), Ok(len)) = (u32::try_from(long.len()), u8::try_from(text.len())) else {
            return false;
        };
        let room = long.len().saturating_add(text.len()) <= limit;
        match self.cells.get_mut(i).map(AsMut::as_mut) {
            Some(slot) if room => {
                work!(text, text.len());
                long.extend_from_slice(text);
                // Compaction may have moved the text the cell had; its
                // halves and style are `cell`'s.
                *slot = cell.with_spilled(start, len);
                true
            }
            _ => false,
        }
    }

    /// Sets cell `i` to `cell`'s halves and style holding `text`, whole
    /// UTF-8 of at most [`SHORT`] bytes: inline, or with
    /// the row's short text, which always has room for it once compacted.
    fn store_short(&mut self, i: usize, cell: Compact, text: &[u8]) {
        if let Some(inline) = cell.holding(text) {
            if let Some(slot) = self.cells.get_mut(i).map(AsMut::as_mut) {
                *slot = inline;
            }
            return;
        }
        let limit = Text::short_limit(self.cells.len());
        if self.text.short.len().saturating_add(text.len()) > limit {
            // The text the cell had goes: it is being replaced.
            if let Some(slot) = self.cells.get_mut(i).map(AsMut::as_mut) {
                *slot = slot.blanked();
            }
            self.compact(false, 0..0);
        }
        let short = &mut self.text.short;
        if let (Ok(start), Ok(len), Some(slot)) = (
            u32::try_from(short.len()),
            u8::try_from(text.len()),
            self.cells.get_mut(i).map(AsMut::as_mut),
        ) {
            if short.capacity() == 0 {
                // A row with one short cluster usually has more: room for
                // a few at once, rather than growing a few bytes at a time.
                short.reserve(FIRST_SHORT.min(limit));
            }
            work!(text, text.len());
            short.extend_from_slice(text);
            *slot = cell.with_spilled(start, len);
        }
    }

    /// The bytes of long text the cells hold: what `compact(true)` keeps.
    fn live_long(&self) -> usize {
        work!(cells, self.cells.len());
        (self.cells.iter())
            .filter_map(|cell| cell.as_ref().spilled(true))
            .map(|range| range.len())
            .fold(0, usize::saturating_add)
    }

    /// Whether cell `i` holds text of `len` bytes in the row's text, which
    /// a cluster as long can take over where it lies (`replace`): with
    /// `replace`, the copy a cell at a time that the tests hold
    /// `Line::copy`'s runs to.
    #[cfg(test)]
    pub(crate) fn replaces(&self, i: usize, len: usize) -> bool {
        (self.cells.get(i))
            .and_then(|c| c.as_ref().locator())
            .is_some_and(|(_, held)| held == len)
    }
    /// Sets cell `i` to `cell`'s halves and style holding `text`, written
    /// over the text the cell holds, as long (`replaces`): no room taken,
    /// so no compaction, as a copy of clusters over clusters of their size
    /// would otherwise need (B35).
    #[cfg(test)]
    pub(crate) fn replace(&mut self, i: usize, cell: Compact, text: &str) {
        let Some((start, len)) = (self.cells.get(i)).and_then(|c| c.as_ref().locator()) else {
            return;
        };
        let store = if len > SHORT {
            &mut self.text.long
        } else {
            &mut self.text.short
        };
        let held = start
            .checked_add(len)
            .and_then(|end| store.get_mut(start..end));
        if let (Some(held), Ok(at), Ok(n)) = (held, u32::try_from(start), u8::try_from(len))
            && held.len() == text.len()
            && let Some(slot) = self.cells.get_mut(i).map(AsMut::as_mut)
        {
            work!(text, text.len());
            held.copy_from_slice(text.as_bytes());
            *slot = cell.with_spilled(at, n);
        }
    }
    /// Blanks cells `span` and keeps only the text the others hold, for
    /// the span to be written cell by cell: what is written then is all the
    /// row's text holds that is dead, and no write compacts the row again
    /// (DECCRA, B35).
    pub(crate) fn clear_for(&mut self, span: std::ops::Range<usize>) {
        let cells = self.cells.get_mut(span.clone()).unwrap_or_default();
        for cell in cells.iter_mut().map(AsMut::as_mut) {
            *cell = cell.blanked();
        }
        let _ = self.clear_outside(span);
    }
    /// `clear_for` but for blanking the span, whose cells are left as they
    /// were, for the caller to write or blank them all: the text the other
    /// cells hold kept, and the row's text as it was given back, where the
    /// span's cells find theirs.
    pub(crate) fn clear_outside(&mut self, span: std::ops::Range<usize>) -> Text {
        let mut old = Text::default();
        for long in [true, false] {
            if !self.text.part(long).is_empty() {
                *old.part_mut(long) = self.compact(long, span.clone());
            }
        }
        old
    }

    /// Keeps only the text of long clusters (`long`), or of short ones, of
    /// the cells outside `skip`, relocating their cells; the part as it
    /// was.
    fn compact(&mut self, long: bool, skip: std::ops::Range<usize>) -> Vec<u8> {
        let store = if long {
            &mut self.text.long
        } else {
            &mut self.text.short
        };
        let old = std::mem::take(store);
        let (head, rest) = (self.cells)
            .split_at_mut_checked(skip.start.min(self.cells.len()))
            .unwrap_or_default();
        let tail = rest.get_mut(skip.len()..).unwrap_or_default();
        for cell in head.iter_mut().chain(tail).map(AsMut::as_mut) {
            let Some(range) = cell.spilled(long) else {
                continue;
            };
            // The range's length is the cell's locator byte.
            let [.., len] = cell.text;
            let (Some(text), Ok(start)) = (old.get(range), u32::try_from(store.len())) else {
                *cell = cell.blanked();
                continue;
            };
            store.extend_from_slice(text);
            *cell = cell.with_spilled(start, len);
        }
        // What it copied: the text the cells hold.
        spend!(text, store.len());
        old
    }

    /// The text of `cells`, whose text is in `old`, stored again within
    /// their budget, left to right; `cells` are relocated into it.
    pub(crate) fn rebuilt(cells: &mut [C], old: &Text) -> Text {
        let mut text = Text::default();
        if old.is_empty() {
            return text;
        }
        let spilled: Vec<(usize, Compact)> = (cells.iter().map(|c| *c.as_ref()).enumerate())
            .filter(|(_, cell)| cell.is_spilled())
            .collect();
        let mut line = Line {
            cells,
            text: &mut text,
        };
        line.store_again(spilled.into_iter(), old);
        text
    }

    /// Stores the text of the cells `spilled` names again, left to right,
    /// each by its place in the line and as it was, its text in `old`.
    /// Every one is blanked first: until stored again, none may locate
    /// text in this store, which a compaction would misread.
    pub(crate) fn store_again(
        &mut self,
        spilled: impl Iterator<Item = (usize, Compact)> + Clone,
        old: &Text,
    ) {
        for (i, cell) in spilled.clone() {
            if let Some(stored) = self.cells.get_mut(i) {
                *stored.as_mut() = cell.blanked();
            }
        }
        for (i, cell) in spilled {
            self.set(i, cell, old.of(&cell));
        }
    }

    /// Cell `i`'s text.
    pub(crate) fn text(&self, i: usize) -> &str {
        self.cells
            .get(i)
            .map_or("", |cell| self.text.of(cell.as_ref()))
    }
}

/// What a grid's row does with cells another row's text holds.
impl Line<'_> {
    /// Writes `cells` over the cells `span`, after `clear_outside(span)`,
    /// their text in `from`: as `clear_for` then `write` would.
    pub(crate) fn write_over(
        &mut self,
        span: std::ops::Range<usize>,
        cells: &[Compact],
        from: &Text,
    ) {
        if span.len() == cells.len() && self.write_runs(span.start, cells, from) {
            return;
        }
        let at = span.start;
        let blank = self.cells.get_mut(span).unwrap_or_default();
        for cell in blank {
            *cell = cell.blanked();
        }
        self.write(at, cells, from);
    }
    /// Writes `cells` from cell `at` on, their text in `from`, another
    /// row's: each as `set` writes a cluster and as stored otherwise. The
    /// text of clusters that follow one another in `from` goes in a run at
    /// once while it fits, where `set` would append each.
    pub(crate) fn write(&mut self, at: usize, cells: &[Compact], from: &Text) {
        let width = self.cells.len();
        let short = Part::new(
            Text::short_limit(width),
            from.short.len(),
            self.text.short.len(),
        );
        let long = Part::new(
            Text::long_limit(width),
            from.long.len(),
            self.text.long.len(),
        );
        let mut parts = [short, long];
        for (i, &cell) in (at..).zip(cells) {
            let Some(slot) = self.cells.get_mut(i) else {
                break;
            };
            if !cell.is_spilled() {
                *slot = cell;
                continue;
            }
            let [.., n] = cell.text;
            let (start, len) = (cell.start(), usize::from(n));
            let [short, long] = &mut parts;
            // Each part on its own, so that what each keeps stays in
            // registers.
            let taken = if len > SHORT {
                long.take(start, len, &from.long, &mut self.text.long)
            } else {
                short.take(start, len, &from.short, &mut self.text.short)
            };
            match taken {
                Some(first) => *slot = cell.with_spilled(first, n),
                None => parts = self.set_from(i, cell, from, parts),
            }
        }
        let [mut short, mut long] = parts;
        short.flush(&from.short, &mut self.text.short);
        long.flush(&from.long, &mut self.text.long);
    }
    /// `write` of cells whose clusters' text is one run of a part of `from`
    /// (`run_of`) that fits whole: each cluster's text where those before
    /// it put it, moved by as much, and the run appended at once. Whether
    /// it was so.
    fn write_runs(&mut self, at: usize, cells: &[Compact], from: &Text) -> bool {
        let Some((long, bytes)) = run_of(cells) else {
            return false;
        };
        let width = self.cells.len();
        let limit = if long {
            Text::long_limit(width)
        } else {
            Text::short_limit(width)
        };
        let held = self.text.part(long).len();
        let end = held.saturating_add(bytes.len());
        let (Some(text), Ok(by), Ok(source)) = (
            from.part(long).get(bytes.clone()),
            u32::try_from(held),
            u32::try_from(bytes.start),
        ) else {
            return false;
        };
        let slots = (at.checked_add(cells.len())).and_then(|stop| self.cells.get_mut(at..stop));
        // Every start in 24 bits, as `with_spilled` stores it.
        let Some(slots) = slots.filter(|_| end <= limit && end < 1 << 24) else {
            return false;
        };
        // Each start moves from `bytes`' to `held`'s, its length above it
        // left as it is: no start passes 24 bits.
        let by = by.wrapping_sub(source);
        for (slot, cell) in slots.iter_mut().zip(cells) {
            let (_, _, spilled) = cell.shape();
            let by = by & mask(spilled);
            let text = u32::from_le_bytes(cell.text).wrapping_add(by).to_le_bytes();
            *slot = Compact {
                text,
                word: cell.word,
            };
        }
        work!(text, text.len());
        self.text.part_mut(long).extend_from_slice(text);
        true
    }
    /// `write`'s cluster that does not fit as it is: what is pending goes
    /// in first, then `set` stores it.
    #[cold]
    #[inline(never)]
    fn set_from(&mut self, i: usize, cell: Compact, from: &Text, parts: [Part; 2]) -> [Part; 2] {
        let [mut short, mut long] = parts;
        short.flush(&from.short, &mut self.text.short);
        long.flush(&from.long, &mut self.text.long);
        self.set(i, cell, from.of(&cell));
        (short.held, long.held) = (self.text.short.len(), self.text.long.len());
        [short, long]
    }

    /// Copies `cells`, their text in `from`, another row's, over cells
    /// `at` on, as a margin scroll copies a span: whether a cell changed.
    /// While each cluster goes over one as long, its text is written where
    /// that one's lies, which needs no room, a run of them at once and
    /// only where it differs; at the first that does not, the span is
    /// cleared and written whole (`clear_for`, `write`), as the row may
    /// have no room left.
    pub(crate) fn copy(&mut self, at: usize, cells: &[Compact], from: &Text) -> bool {
        if let Some(changed) = self.copy_runs(at, cells, from) {
            return changed;
        }
        let span = at..at.saturating_add(cells.len());
        let Some(olds) = self.cells.get_mut(span.clone()) else {
            return false;
        };
        let mut changed = false;
        let mut run = Run::default();
        let mut in_place = true;
        for (cell, slot) in cells.iter().zip(olds.iter_mut()) {
            let old = *slot;
            if !cell.is_spilled() {
                if *cell != old {
                    (*slot, changed) = (*cell, true);
                }
                continue;
            }
            let ([.., len], [.., was]) = (cell.text, old.text);
            if !old.is_spilled() || len != was {
                in_place = false;
                break;
            }
            let (source, target, long) = (cell.start(), old.start(), usize::from(len) > SHORT);
            let len = usize::from(len);
            // The cell's halves and style, its text where the old one's is.
            let new = Compact {
                text: old.text,
                word: (cell.word & KEPT) | SPILLED,
            };
            if new != old {
                (*slot, changed) = (new, true);
            }
            if run.follows(long, source, target) {
                run.len = run.len.saturating_add(len);
                continue;
            }
            let Some(differed) = run.copy(from, self.text) else {
                in_place = false;
                break;
            };
            changed |= differed;
            run = Run {
                long,
                from: source,
                to: target,
                len,
            };
        }
        if in_place && let Some(differed) = run.copy(from, self.text) {
            return changed | differed;
        }
        // The cells written so far are written again with the rest.
        let _ = self.clear_outside(span.clone());
        self.write_over(span, cells, from);
        true
    }

    /// `copy` of cells whose clusters' text is one run of a part of `from`,
    /// each over a cluster as long, their text one run of that part of this
    /// row's (`runs_over`): the cells and the runs compared and written at
    /// once. Whether a cell changed, if it was so.
    fn copy_runs(&mut self, at: usize, cells: &[Compact], from: &Text) -> Option<bool> {
        let olds = self.cells.get_mut(at..at.checked_add(cells.len())?)?;
        let ((long, source), (_, target), changes) = runs_over(cells, olds)?;
        let text = from.part(long).get(source)?;
        let held = self.text.part_mut(long).get_mut(target)?;
        // Each cell its cluster's halves and style, its text where the old
        // one's is; and the run over theirs.
        for (old, cell) in olds.iter_mut().zip(cells).filter(|_| changes) {
            *old = match cell.is_spilled() {
                true => Compact {
                    text: old.text,
                    word: cell.word,
                },
                false => *cell,
            };
        }
        work!(text, text.len());
        let differs = text != &held[..];
        if differs {
            let _ = crate::copy_from(held, text);
        }
        Some(changes || differs)
    }
}

/// `bytes`, a character's UTF-8, pushed onto `store` as copies of one to
/// four bytes known in length, with no copy's call.
#[inline]
fn push_utf8(store: &mut Vec<u8>, bytes: &[u8]) {
    match *bytes {
        [a] => store.push(a),
        [a, b] => store.extend_from_slice(&[a, b]),
        [a, b, c] => store.extend_from_slice(&[a, b, c]),
        [a, b, c, d] => store.extend_from_slice(&[a, b, c, d]),
        _ => store.extend_from_slice(bytes),
    }
}

/// `head` then `tail` in `buffer`, if they fit: the bytes it holds.
fn join<'a>(buffer: &'a mut [u8], head: &[u8], tail: &[u8]) -> Option<&'a [u8]> {
    let end = head.len().checked_add(tail.len())?;
    let (first, second) = buffer.get_mut(..end)?.split_at_mut_checked(head.len())?;
    crate::copy_from(first, head)?;
    crate::copy_from(second, tail)?;
    buffer.get(..end)
}

#[cfg(test)]
mod tests;
