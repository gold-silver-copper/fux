//! The cell a grid stores, eight bytes: a [`Cell`]'s text and halves, with
//! its style's number (`style.rs`) in place of its attributes. Readers
//! never see it: a [`CellRef`] reads it with its row's text and the grid's
//! styles.
//!
//! A cell holds up to four bytes of text, one character, inline. A longer
//! cluster goes in its row's text ([`Text`]), as a [`Cell`]'s longer than
//! [`Cell::INLINE_CAPACITY`] goes in its row's spill: those of 5 to 17
//! bytes, which a [`Cell`] held inline, in a part of their own, and the
//! longer ones, the clusters a [`Cell`] spilled, in a [`Spill`] that keeps
//! them exactly as a row of [`Cell`]s keeps them, within the same budget,
//! so what is kept and what is cut is what it always was.

use crate::cell::{ASCII, Spill, floor};
use crate::style::Styles;
use crate::{Cell, CellRef};

/// A grid's cell. Inline, `text` is the cell's text, zeros after it (a
/// grid's text has no NUL, which is never printed). Spilled, it is where
/// the text is in its row's [`Text`]: an offset of 24 bits, little-endian,
/// then a length, of 5 to 17 bytes in the short clusters and more in the
/// long ones. `word` is the style's number (`style.rs`, 28 bits) and the
/// flags. All zeros is a blank cell in the default attributes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Compact {
    text: [u8; 4],
    word: u32,
}

const _: () = assert!(std::mem::size_of::<Compact>() == 8);

/// The style's number, in `word`.
const STYLE: u32 = (1 << 28) - 1;
/// The second half of a wide glyph; and its first. Their bits, shifted
/// down 22, are [`Cell::CONTINUATION`] and [`Cell::WIDE`].
const CONTINUATION: u32 = 1 << 28;
const WIDE: u32 = 1 << 29;
const HALVES: u32 = WIDE | CONTINUATION;
/// The text is in the row's text.
const SPILLED: u32 = 1 << 30;
const _: () = assert!(HALVES >> 22 == Cell::HALVES as u32);

/// The longest cluster a short one is: what a [`Cell`] holds inline.
const SHORT: usize = Cell::INLINE_CAPACITY;
/// The longest text a cell holds inline.
const INLINE: usize = 4;
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
    /// The glyph `c`, `width` columns wide, in style `style`.
    #[inline]
    pub(crate) fn glyph(c: char, width: usize, style: u32) -> Self {
        // A char is at most four UTF-8 bytes; the rest stay zeros.
        let mut text = [0; 4];
        c.encode_utf8(&mut text);
        Self {
            text,
            word: (style & STYLE) | if width == 2 { WIDE } else { 0 },
        }
    }
    /// The ASCII character `byte` in style `style`.
    #[inline]
    pub(crate) fn ascii(byte: u8, style: u32) -> Self {
        Self {
            text: [byte, 0, 0, 0],
            word: style & STYLE,
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
    /// Whether the cell equals `other`: every cell is held one way only,
    /// so the same cells are the same bytes. Two spilled cells are the same
    /// if they locate the same text of one row.
    #[inline]
    pub(crate) fn same(&self, other: &Self) -> bool {
        self == other
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
    /// The halves, as [`Cell::WIDE`] and [`Cell::CONTINUATION`].
    #[inline]
    fn halves(&self) -> u8 {
        u8::try_from((self.word & HALVES) >> 22).unwrap_or(0)
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
        // The bytes up to the last that is not zero: text has none inside.
        let len = usize::try_from(32u32.saturating_sub(word.leading_zeros()).div_ceil(8));
        len.ok()
            .and_then(|len| self.text.get(..len))
            .and_then(|s| std::str::from_utf8(s).ok())
            .unwrap_or("")
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
    /// Where a long cluster's text is in its row's long text.
    fn long(&self) -> Option<std::ops::Range<usize>> {
        let (start, len) = self.locator().filter(|(_, len)| *len > SHORT)?;
        Some(start..start.checked_add(len)?)
    }
    /// Where a short cluster's text is in its row's short text.
    fn short(&self) -> Option<std::ops::Range<usize>> {
        let (start, len) = self.locator().filter(|(_, len)| *len <= SHORT)?;
        Some(start..start.checked_add(len)?)
    }
    /// The cell, keeping its halves and style, holding `text` inline: at
    /// most [`INLINE`] bytes, or the cell is left blank.
    fn with_inline(self, text: &[u8]) -> Self {
        let mut bytes = [0; 4];
        let fits = bytes
            .get_mut(..text.len())
            .and_then(|head| crate::copy_from(head, text))
            .is_some();
        Self {
            text: if fits { bytes } else { [0; 4] },
            word: self.word & (STYLE | HALVES),
        }
    }
    /// The cell, keeping its halves and style, locating `len` bytes of its
    /// row's text from `start`; blank if `start` takes more than 24 bits.
    fn with_spilled(self, start: u32, len: u8) -> Self {
        match start.to_le_bytes() {
            [a, b, c, 0] => Self {
                text: [a, b, c, len],
                word: (self.word & (STYLE | HALVES)) | SPILLED,
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
        CellRef::of(
            text.of(self),
            attributes,
            self.halves(),
            self.has_contents(),
        )
    }
}

/// The text of a row's cells that is not inline: the long clusters in a
/// [`Spill`], as a row of [`Cell`]s keeps them, and the short ones, of 5
/// to 17 bytes, one after another. Overwritten clusters leave their text
/// behind until the row runs out of room for more and is compacted.
#[derive(Clone, Debug, Default)]
pub(crate) struct Text {
    long: Spill,
    short: Vec<u8>,
}

impl Text {
    /// The text of `cell`, a cell of this row.
    #[inline]
    pub(crate) fn of<'a>(&'a self, cell: &'a Compact) -> &'a str {
        let Some((start, len)) = cell.locator() else {
            return cell.inline();
        };
        let held = if len > SHORT {
            &self.long.0
        } else {
            &self.short
        };
        start
            .checked_add(len)
            .and_then(|end| held.get(start..end))
            .and_then(|s| std::str::from_utf8(s).ok())
            .unwrap_or("")
    }
    /// Bytes of long clusters kept, live or left behind, as a row of
    /// [`Cell`]s keeps them: the row's `text_len`.
    pub(crate) fn len(&self) -> usize {
        self.long.len()
    }
    /// Whether the row keeps no text at all.
    pub(crate) fn is_empty(&self) -> bool {
        self.long.len() == 0 && self.short.is_empty()
    }
    /// Forgets all the text, releasing its memory.
    pub(crate) fn clear(&mut self) {
        self.long.clear();
        if self.short.capacity() != 0 {
            self.short = Vec::new();
        }
    }
    /// The most bytes of short clusters `cells` cells keep: one each.
    fn short_limit(cells: usize) -> usize {
        cells.saturating_mul(SHORT)
    }
}

/// A row's cells with their text, to write into.
pub(crate) struct Line<'a> {
    pub(crate) cells: &'a mut [Compact],
    pub(crate) text: &'a mut Text,
}

impl Line<'_> {
    /// Adds `c` to the cluster in cell `i`; an empty cell first takes a
    /// space for `c` to follow. Whether it was kept: a cluster at
    /// [`Cell::CLUSTER_CAPACITY`], or a row out of room, drops it.
    pub(crate) fn append(&mut self, i: usize, c: char) -> bool {
        let Some(&cell) = self.cells.get(i) else {
            return false;
        };
        let current = if cell.has_contents() {
            self.text.of(&cell)
        } else {
            " "
        };
        let Some(end) = current
            .len()
            .checked_add(c.len_utf8())
            .filter(|end| *end <= Cell::CLUSTER_CAPACITY)
        else {
            return false;
        };
        let mut encoded = [0; 4];
        let encoded = c.encode_utf8(&mut encoded).as_bytes();
        // A cluster a `Cell` holds inline is short: never out of room.
        if end <= SHORT {
            // In place, when the cluster is the last short text the row
            // keeps.
            if let Some(range) = cell.short()
                && range.end == self.text.short.len()
                && let (Ok(start), Ok(len)) = (u32::try_from(range.start), u8::try_from(end))
            {
                self.text.short.extend_from_slice(encoded);
                if let Some(slot) = self.cells.get_mut(i) {
                    *slot = cell.with_spilled(start, len);
                }
                return true;
            }
            // Whole UTF-8 then a character's encoding is whole UTF-8.
            let mut buffer = [0u8; SHORT];
            if let Some(joined) = buffer.get_mut(..end)
                && let Some((head, tail)) = joined.split_at_mut_checked(current.len())
                && crate::copy_from(head, current.as_bytes())
                    .and_then(|()| crate::copy_from(tail, encoded))
                    .is_some()
            {
                self.store_short(i, cell, joined);
                return true;
            }
            return false;
        }
        let mut buffer = [0u8; Cell::CLUSTER_CAPACITY];
        let Some(joined) = buffer.get_mut(..end) else {
            return false;
        };
        let (head, tail) = joined
            .split_at_mut_checked(current.len())
            .unwrap_or_default();
        if crate::copy_from(head, current.as_bytes())
            .and_then(|()| crate::copy_from(tail, encoded))
            .is_none()
        {
            return false;
        }
        let Ok(joined) = std::str::from_utf8(joined) else {
            return false;
        };
        let limit = Spill::limit(self.cells.len());
        let long = &mut self.text.long;
        // In place, when the cluster is the last long text the row keeps.
        if let Some(range) = cell.long()
            && range.end == long.len()
            && long.len().saturating_add(encoded.len()) <= limit
            && let (Ok(start), Ok(len)) = (u32::try_from(range.start), u8::try_from(end))
        {
            long.0.extend_from_slice(encoded);
            if let Some(slot) = self.cells.get_mut(i) {
                *slot = cell.with_spilled(start, len);
            }
            return true;
        }
        // Unlike `set`, the cell keeps what it has if the row has no room,
        // even compacted: an append never loses text.
        if self.text.long.len().saturating_add(joined.len()) > limit {
            self.compact_long();
        }
        let long = &mut self.text.long;
        let (Ok(start), Ok(len)) = (u32::try_from(long.len()), u8::try_from(joined.len())) else {
            return false;
        };
        if long.len().saturating_add(joined.len()) > limit {
            return false;
        }
        long.0.extend_from_slice(joined.as_bytes());
        match self.cells.get_mut(i) {
            // Compaction may have moved it; its halves and style are as
            // they were.
            Some(slot) => {
                *slot = slot.with_spilled(start, len);
                true
            }
            None => false,
        }
    }

    /// Sets cell `i` to `cell`'s halves and style holding `text`: inline
    /// if it fits, else in the row's text. A cluster longer than
    /// [`Cell::CLUSTER_CAPACITY`] is cut there; a long one the row has no
    /// room for, even compacted, is cut to what a [`Cell`] holds inline.
    /// Whether the text was kept whole.
    pub(crate) fn set(&mut self, i: usize, cell: Compact, text: &str) -> bool {
        let kept = floor(text, Cell::CLUSTER_CAPACITY);
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
        let limit = Spill::limit(self.cells.len());
        if self.text.long.len().saturating_add(text.len()) > limit {
            // The text the cell had goes too: it is being replaced.
            if let Some(slot) = self.cells.get_mut(i) {
                *slot = slot.blanked();
            }
            self.compact_long();
        }
        let long = &mut self.text.long;
        let start = u32::try_from(long.len());
        let len = u8::try_from(text.len());
        let room = long.len().saturating_add(text.len()) <= limit;
        match (start, len) {
            (Ok(start), Ok(len)) if room => {
                long.0.extend_from_slice(text.as_bytes());
                if let Some(slot) = self.cells.get_mut(i) {
                    *slot = cell.with_spilled(start, len);
                }
                true
            }
            _ => {
                self.store_short(i, cell, floor(text, SHORT).as_bytes());
                false
            }
        }
    }

    /// Sets cell `i` to `cell`'s halves and style holding `text`, whole
    /// UTF-8 of at most [`Cell::INLINE_CAPACITY`] bytes: inline, or with
    /// the row's short text, which always has room for it once compacted.
    fn store_short(&mut self, i: usize, cell: Compact, text: &[u8]) {
        if text.len() <= INLINE {
            if let Some(slot) = self.cells.get_mut(i) {
                *slot = cell.with_inline(text);
            }
            return;
        }
        let limit = Text::short_limit(self.cells.len());
        if self.text.short.len().saturating_add(text.len()) > limit {
            // The text the cell had goes: it is being replaced.
            if let Some(slot) = self.cells.get_mut(i) {
                *slot = slot.blanked();
            }
            self.compact_short();
        }
        let short = &mut self.text.short;
        if let (Ok(start), Ok(len), Some(slot)) = (
            u32::try_from(short.len()),
            u8::try_from(text.len()),
            self.cells.get_mut(i),
        ) {
            if short.capacity() == 0 {
                // A row with one short cluster usually has more: room for
                // a few at once, rather than growing a few bytes at a time.
                short.reserve(FIRST_SHORT.min(limit));
            }
            short.extend_from_slice(text);
            *slot = cell.with_spilled(start, len);
        }
    }

    /// Keeps only the text of long clusters, relocating their cells.
    fn compact_long(&mut self) {
        let old = std::mem::take(&mut self.text.long.0);
        let long = &mut self.text.long.0;
        for cell in self.cells.iter_mut() {
            let Some(range) = cell.long() else {
                continue;
            };
            let (Some(text), Ok(start)) = (old.get(range), u32::try_from(long.len())) else {
                *cell = cell.blanked();
                continue;
            };
            let Ok(len) = u8::try_from(text.len()) else {
                continue;
            };
            long.extend_from_slice(text);
            *cell = cell.with_spilled(start, len);
        }
    }

    /// Keeps only the text of short clusters, relocating their cells.
    fn compact_short(&mut self) {
        let old = std::mem::take(&mut self.text.short);
        let short = &mut self.text.short;
        for cell in self.cells.iter_mut() {
            let Some(range) = cell.short() else {
                continue;
            };
            let (Some(text), Ok(start)) = (old.get(range), u32::try_from(short.len())) else {
                *cell = cell.blanked();
                continue;
            };
            let Ok(len) = u8::try_from(text.len()) else {
                continue;
            };
            short.extend_from_slice(text);
            *cell = cell.with_spilled(start, len);
        }
    }

    /// The text of `cells`, whose text is in `old`, stored again within
    /// their budget, left to right; `cells` are relocated into it.
    pub(crate) fn rebuilt(cells: &mut [Compact], old: &Text) -> Text {
        let mut text = Text::default();
        if old.is_empty() {
            return text;
        }
        // Every spilled cell blanked first: until stored again, none may
        // locate text in the new store, which a compaction would misread.
        let mut spilled = Vec::new();
        for (i, cell) in cells.iter_mut().enumerate() {
            if cell.is_spilled() {
                spilled.push((i, *cell));
                *cell = cell.blanked();
            }
        }
        let mut line = Line {
            cells,
            text: &mut text,
        };
        for (i, cell) in spilled {
            line.set(i, cell, old.of(&cell));
        }
        text
    }

    /// Cell `i`'s text.
    pub(crate) fn text(&self, i: usize) -> &str {
        self.cells.get(i).map_or("", |cell| self.text.of(cell))
    }
}

#[cfg(test)]
mod tests;
