//! Cells: fixed size, with the text of a grapheme cluster too long to keep
//! inline held beside them, in the text of the row (or [`Cells`]) they are
//! in. [`CellRef`] reads a cell with that text; a stored cell alone is never
//! read, so a reader cannot see half a cluster.

/// A default, indexed, or true-colour terminal colour.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Color {
    #[default]
    Default,
    Idx(u8),
    Rgb(u8, u8, u8),
}

/// How a cell blinks: SGR 5 (slow) and 6 (rapid) replace one another, and
/// SGR 25 stops either.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Blink {
    #[default]
    None,
    Slow,
    Rapid,
}

/// A colour as stored: a tag (0 default, 1 indexed, 2 RGB) and its bytes,
/// all of them always set, so attributes compare, copy and clear as plain
/// bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Packed([u8; 4]);

impl Packed {
    const fn new(color: Color) -> Self {
        Self(match color {
            Color::Default => [0; 4],
            Color::Idx(i) => [1, i, 0, 0],
            Color::Rgb(r, g, b) => [2, r, g, b],
        })
    }
    const fn get(self) -> Color {
        match self.0 {
            [1, i, _, _] => Color::Idx(i),
            [2, r, g, b] => Color::Rgb(r, g, b),
            _ => Color::Default,
        }
    }
}

/// Attributes are independent of glyph storage and copied onto erased cells.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct Attributes {
    foreground: Packed,
    background: Packed,
    underline_color: Packed,
    pub(crate) flags: u16,
}

impl std::fmt::Debug for Attributes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Attributes")
            .field("foreground", &self.foreground())
            .field("background", &self.background())
            .field("underline_color", &self.underline_color())
            .field("flags", &self.flags)
            .finish()
    }
}

impl Attributes {
    // The bits of `flags`, one a style.
    pub(crate) const BOLD: u16 = 1;
    pub(crate) const DIM: u16 = 2;
    pub(crate) const ITALIC: u16 = 4;
    pub(crate) const UNDERLINE: u16 = 8;
    pub(crate) const INVERSE: u16 = 16;
    pub(crate) const SLOW_BLINK: u16 = 32;
    pub(crate) const RAPID_BLINK: u16 = 64;
    pub(crate) const HIDDEN: u16 = 128;
    pub(crate) const STRIKEOUT: u16 = 256;
    pub(crate) const BLINK: u16 = Self::SLOW_BLINK | Self::RAPID_BLINK;

    /// Plain attributes with the given colours; add styles with the `with_*`
    /// builders. For consumers that store or transport cells.
    pub const fn new(foreground: Color, background: Color) -> Self {
        Self {
            foreground: Packed::new(foreground),
            background: Packed::new(background),
            underline_color: Packed::new(Color::Default),
            flags: 0,
        }
    }
    pub const fn foreground(self) -> Color {
        self.foreground.get()
    }
    /// What an erase, a scroll or an insertion fills cells with while these
    /// attributes are the pen: its colours alone, as xterm fills them (the
    /// `bce` terminfo capability, background colour erase).
    pub(crate) const fn erased(self) -> Self {
        Self {
            foreground: self.foreground,
            background: self.background,
            underline_color: Packed([0; 4]),
            flags: 0,
        }
    }
    pub const fn background(self) -> Color {
        self.background.get()
    }
    /// SGR 58; `Default` draws underlines in the foreground colour.
    pub const fn underline_color(self) -> Color {
        self.underline_color.get()
    }
    #[must_use]
    pub const fn with_foreground(mut self, color: Color) -> Self {
        self.foreground = Packed::new(color);
        self
    }
    #[must_use]
    pub const fn with_background(mut self, color: Color) -> Self {
        self.background = Packed::new(color);
        self
    }
    const fn with_flag(mut self, bit: u16, on: bool) -> Self {
        self.flags = if on {
            self.flags | bit
        } else {
            self.flags & !bit
        };
        self
    }
    #[must_use]
    pub const fn with_bold(self, on: bool) -> Self {
        self.with_flag(Self::BOLD, on)
    }
    #[must_use]
    pub const fn with_dim(self, on: bool) -> Self {
        self.with_flag(Self::DIM, on)
    }
    #[must_use]
    pub const fn with_italic(self, on: bool) -> Self {
        self.with_flag(Self::ITALIC, on)
    }
    #[must_use]
    pub const fn with_underline(self, on: bool) -> Self {
        self.with_flag(Self::UNDERLINE, on)
    }
    #[must_use]
    pub const fn with_inverse(self, on: bool) -> Self {
        self.with_flag(Self::INVERSE, on)
    }
    #[must_use]
    pub const fn with_blink(self, blink: Blink) -> Self {
        let bit = match blink {
            Blink::None => 0,
            Blink::Slow => Self::SLOW_BLINK,
            Blink::Rapid => Self::RAPID_BLINK,
        };
        self.with_flag(Self::BLINK, false).with_flag(bit, true)
    }
    #[must_use]
    pub const fn with_hidden(self, on: bool) -> Self {
        self.with_flag(Self::HIDDEN, on)
    }
    #[must_use]
    pub const fn with_strikeout(self, on: bool) -> Self {
        self.with_flag(Self::STRIKEOUT, on)
    }
    #[must_use]
    pub const fn with_underline_color(mut self, color: Color) -> Self {
        self.underline_color = Packed::new(color);
        self
    }
    pub fn bold(self) -> bool {
        self.flags & Self::BOLD != 0
    }
    pub fn dim(self) -> bool {
        self.flags & Self::DIM != 0
    }
    pub fn italic(self) -> bool {
        self.flags & Self::ITALIC != 0
    }
    pub fn underline(self) -> bool {
        self.flags & Self::UNDERLINE != 0
    }
    pub fn inverse(self) -> bool {
        self.flags & Self::INVERSE != 0
    }
    pub fn blink(self) -> Blink {
        if self.flags & Self::SLOW_BLINK != 0 {
            Blink::Slow
        } else if self.flags & Self::RAPID_BLINK != 0 {
            Blink::Rapid
        } else {
            Blink::None
        }
    }
    pub fn hidden(self) -> bool {
        self.flags & Self::HIDDEN != 0
    }
    pub fn strikeout(self) -> bool {
        self.flags & Self::STRIKEOUT != 0
    }
}

/// One fixed-size glyph cell, 32 bytes. A cluster of up to
/// [`Cell::INLINE_CAPACITY`] bytes is held in the cell; a longer one, up to
/// [`Cell::CLUSTER_CAPACITY`], in the text of the row it is in, which the
/// cell locates. A continuation has empty contents and default attributes;
/// its leader owns the wide glyph's appearance.
///
/// A `Cell` a host makes ([`Cell::new`]) is always inline. Read cells of a
/// screen through [`CellRef`], which knows where their text is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cell {
    /// Inline: the text, zeros past its length. Spilled: the text's offset
    /// in its row's text (four bytes, little-endian) and its length.
    text: [u8; Cell::INLINE_CAPACITY],
    length: u8,
    pub(crate) attributes: Attributes,
}

const _: () = assert!(std::mem::size_of::<Cell>() == 32);

impl Cell {
    /// The most UTF-8 bytes a cell holds inline.
    pub const INLINE_CAPACITY: usize = 17;
    /// The most UTF-8 bytes of one grapheme cluster kept. Characters past it
    /// still belong to the cluster, and its cell, but are dropped: a cluster
    /// is never split, so what follows it is always where a program laid
    /// out with UAX #29 expects. 128 is the Unicode stream-safe limit of a
    /// starter and 30 combining marks, and holds every emoji sequence.
    pub const CLUSTER_CAPACITY: usize = 128;
    // The bits of `length`: how many bytes of `text` are the inline contents,
    // whether the text is spilled instead, and which half of a wide glyph the
    // cell is, if either.
    const LENGTH: u8 = 0b0001_1111;
    const SPILLED: u8 = 0b0010_0000;
    const CONTINUATION: u8 = 0b0100_0000;
    const WIDE: u8 = 0b1000_0000;
    const HALVES: u8 = Self::WIDE | Self::CONTINUATION;

    /// A cell built by a consumer that stores or transports screen contents.
    /// `None` if `contents` exceeds [`Cell::INLINE_CAPACITY`]; store a longer
    /// cluster with [`Cells::set_text`].
    pub fn new(contents: &str, wide: bool, attributes: Attributes) -> Option<Self> {
        let bytes = contents.as_bytes();
        if bytes.len() > Self::INLINE_CAPACITY {
            return None;
        }
        let mut cell = Self::blank(attributes);
        // The length was checked against the capacity above.
        crate::copy_from(cell.text.get_mut(..bytes.len())?, bytes)?;
        cell.length = u8::try_from(bytes.len()).ok()? | if wide { Self::WIDE } else { 0 };
        Some(cell)
    }
    /// The trailing half of a wide glyph: empty, default attributes.
    pub fn wide_continuation() -> Self {
        Self::continuation()
    }
    pub fn has_contents(&self) -> bool {
        self.length & (Self::LENGTH | Self::SPILLED) != 0
    }
    pub fn is_wide(&self) -> bool {
        self.length & Self::WIDE != 0
    }
    pub fn is_wide_continuation(&self) -> bool {
        self.length & Self::CONTINUATION != 0
    }
    pub fn attributes(&self) -> Attributes {
        self.attributes
    }

    pub(crate) fn blank(attributes: Attributes) -> Self {
        Self {
            attributes,
            ..Self::default()
        }
    }
    pub(crate) fn glyph(c: char, width: usize, attributes: Attributes) -> Self {
        // Encoded in place: a char is at most four UTF-8 bytes, so it fits,
        // and so does its length; the rest of the text stays zeros.
        let mut text = [0; Self::INLINE_CAPACITY];
        let Ok(length) = u8::try_from(c.encode_utf8(&mut text).len()) else {
            return Self::blank(attributes);
        };
        Self {
            text,
            length: length | if width == 2 { Self::WIDE } else { 0 },
            attributes,
        }
    }
    pub(crate) fn ascii(byte: u8, attributes: Attributes) -> Self {
        let mut cell = Self::blank(attributes);
        if let Some(first) = cell.text.first_mut() {
            *first = byte;
        }
        cell.length = 1;
        cell
    }
    /// Whether the cell equals `other`, as `==` says: told from the length
    /// and attributes, where cells that differ usually differ, and then only
    /// the bytes of text in use, as what follows them is zero in every cell.
    /// Two spilled cells are the same if they locate the same text of one row.
    pub(crate) fn same(&self, other: &Cell) -> bool {
        let used = self.used();
        self.length == other.length
            && self.attributes == other.attributes
            && self.text.get(..used) == other.text.get(..used)
    }
    /// How many bytes of `text` are in use.
    fn used(&self) -> usize {
        if self.is_spilled() {
            5
        } else {
            usize::from(self.length & Self::LENGTH)
        }
    }
    /// Whether the cell is exactly what `ascii(byte, attributes)` makes: one
    /// byte of text, the rest zero as in every cell.
    pub(crate) fn is_ascii(&self, byte: u8, attributes: Attributes) -> bool {
        self.length == 1 && self.text.first() == Some(&byte) && self.attributes == attributes
    }
    pub(crate) fn continuation() -> Self {
        Self {
            length: Self::CONTINUATION,
            ..Self::default()
        }
    }
    /// Marks the cell as the leading half of a wide glyph.
    pub(crate) fn widen(&mut self) {
        self.length |= Self::WIDE;
    }
    pub(crate) fn is_spilled(&self) -> bool {
        self.length & Self::SPILLED != 0
    }
    /// The inline text; empty for a spilled cell.
    fn inline(&self) -> &str {
        // Every write is whole UTF-8, and length always ends on a boundary.
        self.text
            .get(..usize::from(self.length & Self::LENGTH))
            .and_then(|s| std::str::from_utf8(s).ok())
            .filter(|_| !self.is_spilled())
            .unwrap_or("")
    }
    /// Where a spilled cell's text is in its row's text.
    fn spilled(&self) -> Option<std::ops::Range<usize>> {
        if !self.is_spilled() {
            return None;
        }
        let [a, b, c, d, len, ..] = self.text;
        let start = usize::try_from(u32::from_le_bytes([a, b, c, d])).ok()?;
        Some(start..start.checked_add(usize::from(len))?)
    }
    /// The cell, keeping its halves and attributes, holding `text` inline.
    /// `text` must fit.
    fn with_inline(self, text: &str) -> Self {
        let mut cell =
            Self::new(text, false, self.attributes).unwrap_or(Self::blank(self.attributes));
        cell.length |= self.length & Self::HALVES;
        cell
    }
    /// The cell, keeping its halves and attributes, locating `len` bytes of
    /// its row's text from `start`.
    fn with_spilled(self, start: u32, len: u8) -> Self {
        let [a, b, c, d] = start.to_le_bytes();
        let mut text = [0; Self::INLINE_CAPACITY];
        if let Some(head) = text.get_mut(..5) {
            crate::copy_from(head, &[a, b, c, d, len]);
        }
        Self {
            text,
            length: Self::SPILLED | (self.length & Self::HALVES),
            attributes: self.attributes,
        }
    }
}

/// The text of a row's cells too long to hold inline: the clusters of its
/// spilled cells, one after another. Overwritten cells leave their text
/// behind until the row runs out of room and is compacted.
#[derive(Clone, Debug, Default)]
pub(crate) struct Spill(Vec<u8>);

impl Spill {
    /// The most bytes a run of `cells` cells keeps: 32 a cell, about as much
    /// as the cells themselves, and one whole cluster more, so that even a
    /// one-column row holds its longest.
    fn limit(cells: usize) -> usize {
        cells
            .saturating_mul(32)
            .saturating_add(Cell::CLUSTER_CAPACITY)
            .min(usize::try_from(u32::MAX).unwrap_or(usize::MAX))
    }
    /// The text of `cell`, a cell of this row.
    pub(crate) fn text<'a>(&'a self, cell: &'a Cell) -> &'a str {
        match cell.spilled() {
            Some(range) => self
                .0
                .get(range)
                .and_then(|s| std::str::from_utf8(s).ok())
                .unwrap_or(""),
            None => cell.inline(),
        }
    }
    /// Forgets all the text, releasing its memory.
    pub(crate) fn clear(&mut self) {
        if self.0.capacity() != 0 {
            self.0 = Vec::new();
        }
    }
    /// Bytes in use, live or left behind.
    pub(crate) fn len(&self) -> usize {
        self.0.len()
    }
}

/// A row's cells with their text, to write into.
pub(crate) struct Line<'a> {
    pub(crate) cells: &'a mut [Cell],
    pub(crate) spill: &'a mut Spill,
}

impl Line<'_> {
    /// Adds `c` to the cluster in cell `i`; an empty cell first takes a
    /// space for `c` to follow. Whether it was kept: a cluster at
    /// [`Cell::CLUSTER_CAPACITY`], or a row out of room, drops it.
    pub(crate) fn append(&mut self, i: usize, c: char) -> bool {
        let Some(&cell) = self.cells.get(i) else {
            return false;
        };
        let mut buffer = [0u8; Cell::CLUSTER_CAPACITY];
        let current = if cell.has_contents() {
            self.spill.text(&cell)
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
        let Some(joined) = buffer.get_mut(..end) else {
            return false;
        };
        let mut encoded = [0; 4];
        let encoded = c.encode_utf8(&mut encoded).as_bytes();
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
        // In place, when the cluster is the last text the row keeps.
        if let Some(range) = cell.spilled()
            && range.end == self.spill.len()
            && self.spill.len().saturating_add(encoded.len()) <= Spill::limit(self.cells.len())
            && let (Ok(start), Ok(len)) = (u32::try_from(range.start), u8::try_from(end))
        {
            self.spill.0.extend_from_slice(encoded);
            if let Some(slot) = self.cells.get_mut(i) {
                *slot = cell.with_spilled(start, len);
            }
            return true;
        }
        if joined.len() <= Cell::INLINE_CAPACITY {
            return self.store(i, cell, joined);
        }
        // Unlike `set`, the cell keeps what it has if the row has no room,
        // even compacted: an append never loses text.
        let limit = Spill::limit(self.cells.len());
        if self.spill.len().saturating_add(joined.len()) > limit {
            self.compact();
        }
        let (Ok(start), Ok(len)) = (u32::try_from(self.spill.len()), u8::try_from(joined.len()))
        else {
            return false;
        };
        if self.spill.len().saturating_add(joined.len()) > limit {
            return false;
        }
        self.spill.0.extend_from_slice(joined.as_bytes());
        match self.cells.get_mut(i) {
            // Compaction may have moved it; its halves and style are as they were.
            Some(slot) => {
                *slot = slot.with_spilled(start, len);
                true
            }
            None => false,
        }
    }

    /// Sets cell `i` to `cell`'s halves and attributes holding `text`: inline
    /// if it fits, else in the row's text. A cluster longer than
    /// [`Cell::CLUSTER_CAPACITY`] is cut there; one the row has no room
    /// for, even compacted, is cut to what fits inline. Whether the text was
    /// kept whole.
    pub(crate) fn set(&mut self, i: usize, cell: Cell, text: &str) -> bool {
        let kept = floor(text, Cell::CLUSTER_CAPACITY);
        self.store(i, cell, kept) && kept.len() == text.len()
    }

    fn store(&mut self, i: usize, cell: Cell, text: &str) -> bool {
        if i >= self.cells.len() {
            return false;
        }
        if text.len() <= Cell::INLINE_CAPACITY {
            if let Some(slot) = self.cells.get_mut(i) {
                *slot = cell.with_inline(text);
            }
            return true;
        }
        let limit = Spill::limit(self.cells.len());
        if self.spill.len().saturating_add(text.len()) > limit {
            // The text the cell had goes too: it is being replaced.
            if let Some(slot) = self.cells.get_mut(i) {
                *slot = Cell::blank(slot.attributes);
            }
            self.compact();
        }
        let start = u32::try_from(self.spill.len());
        let len = u8::try_from(text.len());
        let room = self.spill.len().saturating_add(text.len()) <= limit;
        let Some(slot) = self.cells.get_mut(i) else {
            return false;
        };
        match (start, len) {
            (Ok(start), Ok(len)) if room => {
                self.spill.0.extend_from_slice(text.as_bytes());
                *slot = cell.with_spilled(start, len);
                true
            }
            _ => {
                *slot = cell.with_inline(floor(text, Cell::INLINE_CAPACITY));
                false
            }
        }
    }

    /// Keeps only the text of spilled cells, relocating them.
    fn compact(&mut self) {
        let old = std::mem::take(&mut self.spill.0);
        for cell in self.cells.iter_mut() {
            let Some(range) = cell.spilled() else {
                continue;
            };
            let (Some(text), Ok(start)) = (old.get(range), u32::try_from(self.spill.0.len()))
            else {
                *cell = Cell::blank(cell.attributes);
                continue;
            };
            let Ok(len) = u8::try_from(text.len()) else {
                continue;
            };
            self.spill.0.extend_from_slice(text);
            *cell = cell.with_spilled(start, len);
        }
    }

    /// The text of `cells`, whose text is in `old`, stored again within
    /// their budget, left to right; `cells` are relocated into it.
    pub(crate) fn rebuilt(cells: &mut [Cell], old: &Spill) -> Spill {
        let mut spill = Spill::default();
        if old.len() == 0 {
            return spill;
        }
        // Every spilled cell blanked first: until stored again, none may
        // locate text in the new store, which a compaction would misread.
        let mut spilled = Vec::new();
        for (i, cell) in cells.iter_mut().enumerate() {
            if cell.is_spilled() {
                spilled.push((i, *cell));
                *cell = Cell::blank(cell.attributes);
            }
        }
        let mut line = Line {
            cells,
            spill: &mut spill,
        };
        for (i, cell) in spilled {
            line.set(i, cell, old.text(&cell));
        }
        spill
    }

    /// Cell `i`'s text.
    pub(crate) fn text(&self, i: usize) -> &str {
        self.cells.get(i).map_or("", |cell| self.spill.text(cell))
    }
}

/// The longest start of `text` of at most `max` bytes that ends on a char
/// boundary.
fn floor(text: &str, max: usize) -> &str {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    text.get(..end).unwrap_or("")
}

/// A cell as it is on the screen: its halves, attributes and whole text.
#[derive(Clone, Copy)]
pub struct CellRef<'a> {
    cell: &'a Cell,
    spill: &'a Spill,
}

impl<'a> CellRef<'a> {
    pub(crate) fn new(cell: &'a Cell, spill: &'a Spill) -> Self {
        Self { cell, spill }
    }
    pub(crate) fn stored(&self) -> &'a Cell {
        self.cell
    }
    /// The cell's grapheme cluster; empty for a blank cell or the second
    /// half of a wide glyph.
    pub fn contents(&self) -> &'a str {
        self.spill.text(self.cell)
    }
    pub fn has_contents(&self) -> bool {
        self.cell.has_contents()
    }
    pub fn is_wide(&self) -> bool {
        self.cell.is_wide()
    }
    pub fn is_wide_continuation(&self) -> bool {
        self.cell.is_wide_continuation()
    }
    pub fn attributes(&self) -> Attributes {
        self.cell.attributes
    }
    pub fn fgcolor(&self) -> Color {
        self.cell.attributes.foreground()
    }
    pub fn bgcolor(&self) -> Color {
        self.cell.attributes.background()
    }
    pub fn underline_color(&self) -> Color {
        self.cell.attributes.underline_color()
    }
    pub fn bold(&self) -> bool {
        self.cell.attributes.bold()
    }
    pub fn dim(&self) -> bool {
        self.cell.attributes.dim()
    }
    pub fn italic(&self) -> bool {
        self.cell.attributes.italic()
    }
    pub fn underline(&self) -> bool {
        self.cell.attributes.underline()
    }
    pub fn inverse(&self) -> bool {
        self.cell.attributes.inverse()
    }
    pub fn blink(&self) -> Blink {
        self.cell.attributes.blink()
    }
    pub fn hidden(&self) -> bool {
        self.cell.attributes.hidden()
    }
    pub fn strikeout(&self) -> bool {
        self.cell.attributes.strikeout()
    }
}

/// Equal if they look the same: text, halves and attributes, wherever each
/// keeps its text.
impl PartialEq for CellRef<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.cell.length & Cell::HALVES == other.cell.length & Cell::HALVES
            && self.cell.attributes == other.cell.attributes
            && self.contents() == other.contents()
    }
}
impl Eq for CellRef<'_> {}

impl std::fmt::Debug for CellRef<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CellRef")
            .field("contents", &self.contents())
            .field("wide", &self.is_wide())
            .field("continuation", &self.is_wide_continuation())
            .field("attributes", &self.cell.attributes)
            .finish()
    }
}

/// An owned run of cells that keeps clusters too long to hold inline, for
/// a host that stores screen contents: a composed frame, a copy. Its text
/// is bounded as a row's is: 32 bytes a cell and one cluster more.
#[derive(Clone, Debug, Default)]
pub struct Cells {
    cells: Vec<Cell>,
    spill: Spill,
}

impl Cells {
    /// The most bytes of text `len` cells keep for clusters too long to hold
    /// inline: 32 a cell and one cluster more, so that even one cell holds
    /// its longest.
    pub fn text_limit(len: usize) -> usize {
        Spill::limit(len)
    }
    /// Bytes of text kept for clusters too long to hold inline, overwritten
    /// ones included until compacted. For memory diagnostics.
    pub fn text_len(&self) -> usize {
        self.spill.len()
    }
    /// `len` blank cells.
    pub fn new(len: usize) -> Self {
        Self {
            cells: vec![Cell::default(); len],
            spill: Spill::default(),
        }
    }
    pub fn len(&self) -> usize {
        self.cells.len()
    }
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }
    pub fn get(&self, i: usize) -> Option<CellRef<'_>> {
        self.cells
            .get(i)
            .map(|cell| CellRef::new(cell, &self.spill))
    }
    /// The cells, in order.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = CellRef<'_>> + ExactSizeIterator + Clone {
        let spill = &self.spill;
        self.cells.iter().map(move |cell| CellRef::new(cell, spill))
    }
    /// The cells in `range`, clipped to those there are.
    pub fn range(
        &self,
        range: std::ops::Range<usize>,
    ) -> impl DoubleEndedIterator<Item = CellRef<'_>> + ExactSizeIterator + Clone {
        let spill = &self.spill;
        let end = range.end.min(self.cells.len());
        let start = range.start.min(end);
        self.cells
            .get(start..end)
            .unwrap_or_default()
            .iter()
            .map(move |cell| CellRef::new(cell, spill))
    }
    /// Sets cell `i` to a copy of `cell`, from wherever it keeps its text.
    /// Whether its text was kept whole (see [`Cells::set_text`]).
    pub fn set(&mut self, i: usize, cell: CellRef<'_>) -> bool {
        self.line().set(i, *cell.stored(), cell.contents())
    }
    /// Sets cell `i` to `cell`, which holds its text inline.
    pub fn set_cell(&mut self, i: usize, cell: Cell) {
        if let Some(slot) = self.cells.get_mut(i) {
            *slot = if cell.is_spilled() {
                Cell::blank(cell.attributes)
            } else {
                cell
            };
        }
    }
    /// Sets cell `i` to `text`, `wide` or not, in `attributes`. A cluster
    /// longer than [`Cell::CLUSTER_CAPACITY`] is cut there, and one the
    /// cells have no room left for to what fits inline; whether the text was
    /// kept whole.
    pub fn set_text(&mut self, i: usize, text: &str, wide: bool, attributes: Attributes) -> bool {
        let halves = if wide { Cell::WIDE } else { 0 };
        let template = Cell {
            length: halves,
            ..Cell::blank(attributes)
        };
        self.line().set(i, template, text)
    }
    /// Gives cell `i` new attributes, keeping its text.
    pub fn set_attributes(&mut self, i: usize, attributes: Attributes) {
        if let Some(cell) = self.cells.get_mut(i) {
            cell.attributes = attributes;
        }
    }
    /// Sets the cells in `range`, clipped to those there are, to `cell`.
    pub fn fill(&mut self, range: std::ops::Range<usize>, cell: Cell) {
        let end = range.end.min(self.cells.len());
        let start = range.start.min(end);
        let cell = if cell.is_spilled() {
            Cell::blank(cell.attributes)
        } else {
            cell
        };
        if let Some(run) = self.cells.get_mut(start..end) {
            run.fill(cell);
        }
        if start == 0 && end == self.cells.len() {
            self.spill.clear();
        }
    }
    /// Makes the run `len` cells long, new ones `cell`. A shorter run keeps
    /// a shorter run's budget: its cells' text is stored again, left to
    /// right, and what no longer fits is cut to what fits inline.
    pub fn resize(&mut self, len: usize, cell: Cell) {
        let cell = if cell.is_spilled() {
            Cell::blank(cell.attributes)
        } else {
            cell
        };
        let shorter = len < self.cells.len();
        self.cells.resize(len, cell);
        if shorter {
            self.spill = Line::rebuilt(&mut self.cells, &self.spill);
        }
    }
    fn line(&mut self) -> Line<'_> {
        Line {
            cells: &mut self.cells,
            spill: &mut self.spill,
        }
    }
}

/// A copy of the cells, their text with them: `row.cells().collect()`.
impl<'a> FromIterator<CellRef<'a>> for Cells {
    fn from_iter<I: IntoIterator<Item = CellRef<'a>>>(cells: I) -> Self {
        // All the cells first, so the text is stored within the budget of
        // the whole run, as it was where it came from.
        let cells: Vec<CellRef<'a>> = cells.into_iter().collect();
        let mut copy = Cells::new(cells.len());
        for (at, cell) in cells.into_iter().enumerate() {
            copy.set(at, cell);
        }
        copy
    }
}

/// Equal if every cell looks the same (see [`CellRef`]'s `==`).
impl PartialEq for Cells {
    fn eq(&self, other: &Self) -> bool {
        self.iter().eq(other.iter())
    }
}
impl Eq for Cells {}

#[cfg(test)]
mod tests;
