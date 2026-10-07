//! Cells: fixed size, with the text of a grapheme cluster too long to keep
//! inline held beside them, in the text of the row (or [`Cells`]) they are
//! in. [`CellRef`] reads a cell with that text; a stored cell alone is never
//! read, so a reader cannot see half a cluster.

/// A default, indexed, or true-colour terminal colour.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Color {
    /// The terminal's default colour, foreground or background as it is used.
    #[default]
    Default,
    /// An indexed colour: 0 to 7 the standard colours, 8 to 15 their bright
    /// forms, 16 to 255 xterm's 256-colour palette.
    Idx(u8),
    /// A direct colour: red, green and blue.
    Rgb(u8, u8, u8),
}

/// How a cell blinks: SGR 5 (slow) and 6 (rapid) replace one another, and
/// SGR 25 stops either.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Blink {
    /// Not blinking.
    #[default]
    None,
    /// Blinking slowly (SGR 5).
    Slow,
    /// Blinking rapidly (SGR 6).
    Rapid,
}

/// How a cell is underlined: SGR 4 and 24, 21 (doubly underlined, ECMA-48
/// 8.3.117), and kitty's styles `4:0` to `4:5`
/// (`references/modern/kitty_underlines.html`), numbered as they are there.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum UnderlineStyle {
    /// Not underlined (SGR 24, `4:0`).
    #[default]
    None,
    /// A straight underline (SGR 4, `4:1`).
    Single,
    /// A double underline (SGR 21, `4:2`).
    Double,
    /// A curly underline (`4:3`): neovim's diagnostics.
    Curly,
    /// A dotted underline (`4:4`).
    Dotted,
    /// A dashed underline (`4:5`).
    Dashed,
}

impl UnderlineStyle {
    /// The style kitty's `4:n` names, `n` from 0 to 5; `None` for another
    /// number.
    pub const fn from_number(n: u16) -> Option<Self> {
        Some(match n {
            0 => Self::None,
            1 => Self::Single,
            2 => Self::Double,
            3 => Self::Curly,
            4 => Self::Dotted,
            5 => Self::Dashed,
            _ => return None,
        })
    }
    /// Its number in kitty's `4:n`, 0 to 5.
    pub const fn number(self) -> u16 {
        match self {
            Self::None => 0,
            Self::Single => 1,
            Self::Double => 2,
            Self::Curly => 3,
            Self::Dotted => 4,
            Self::Dashed => 5,
        }
    }
}

/// Every ASCII character, in order: the text of a cell holding one.
pub(crate) const ASCII: &str = "\x00\x01\x02\x03\x04\x05\x06\x07\x08\x09\x0a\x0b\x0c\x0d\x0e\x0f\x10\x11\x12\x13\x14\x15\x16\x17\x18\x19\x1a\x1b\x1c\x1d\x1e\x1f\x20\x21\x22\x23\x24\x25\x26\x27\x28\x29\x2a\x2b\x2c\x2d\x2e\x2f\x30\x31\x32\x33\x34\x35\x36\x37\x38\x39\x3a\x3b\x3c\x3d\x3e\x3f\x40\x41\x42\x43\x44\x45\x46\x47\x48\x49\x4a\x4b\x4c\x4d\x4e\x4f\x50\x51\x52\x53\x54\x55\x56\x57\x58\x59\x5a\x5b\x5c\x5d\x5e\x5f\x60\x61\x62\x63\x64\x65\x66\x67\x68\x69\x6a\x6b\x6c\x6d\x6e\x6f\x70\x71\x72\x73\x74\x75\x76\x77\x78\x79\x7a\x7b\x7c\x7d\x7e\x7f";

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

/// Attributes are independent of glyph storage. An erase fills cells with
/// the pen's colours alone (`erased`).
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
    // The bits of `flags`: one for each style, but the underline, whose
    // style's number (`UnderlineStyle`, 0 for none) takes three bits from
    // `UNDERLINE_SHIFT`. Bit 3, the underline's while it had no style, is spare.
    pub(crate) const BOLD: u16 = 1;
    pub(crate) const DIM: u16 = 2;
    pub(crate) const ITALIC: u16 = 4;
    pub(crate) const INVERSE: u16 = 16;
    pub(crate) const SLOW_BLINK: u16 = 32;
    pub(crate) const RAPID_BLINK: u16 = 64;
    pub(crate) const HIDDEN: u16 = 128;
    pub(crate) const STRIKEOUT: u16 = 256;
    pub(crate) const BLINK: u16 = Self::SLOW_BLINK | Self::RAPID_BLINK;
    const UNDERLINE_SHIFT: u32 = 9;
    pub(crate) const UNDERLINE: u16 = 0b111 << Self::UNDERLINE_SHIFT;

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
    /// The foreground colour.
    pub const fn foreground(self) -> Color {
        self.foreground.get()
    }
    /// The renditions an indexed style's own number holds
    /// (`inline_style`): every flag but rapid blink, and a single underline.
    const INDEXED: u16 = Self::BOLD
        | Self::DIM
        | Self::ITALIC
        | Self::INVERSE
        | Self::SLOW_BLINK
        | Self::HIDDEN
        | Self::STRIKEOUT
        | 1 << Self::UNDERLINE_SHIFT;
    /// The renditions a direct style's own number holds: bold and italic,
    /// as syntax highlighting draws keywords and comments.
    const DIRECT: u16 = Self::BOLD | Self::ITALIC;
    /// These attributes as a style that is its own number (`style.rs`), of
    /// 27 bits, if they are one of two kinds programs mostly print in.
    /// Indexed: no colour but the default and indexed ones and no
    /// underline colour, any rendition but rapid blink, and an underline
    /// that is single or none; the foreground and the background, 9 bits
    /// each, 0 for the default and one more than its index for an indexed
    /// colour, then the rendition, 8 bits. Direct: a direct foreground, the
    /// default background and underline colour, and bold or italic or
    /// both; bit 26 set, the foreground's red, green and blue below it, then
    /// bold and italic. `None` for any other attributes.
    #[inline]
    pub(crate) fn inline_style(self) -> Option<u32> {
        // A colour is [0, 0, 0, 0] (the default), [1, index, 0, 0] or
        // [2, red, green, blue]: as a little-endian word, its tag in the
        // low byte, the rest above.
        let (fg, bg) = (
            u32::from_le_bytes(self.foreground.0),
            u32::from_le_bytes(self.background.0),
        );
        if self.underline_color.0 != [0; 4] {
            return None;
        }
        let flags = u32::from(self.flags);
        if (fg | bg) & 0xffff_00fe == 0 && self.flags & !Self::INDEXED == 0 {
            // One more than the index for an indexed colour, 0 for the
            // default.
            let colour = |w: u32| ((w >> 8).wrapping_add(1)) & (w & 1).wrapping_neg();
            let rendition =
                (flags & 0b111) | ((flags >> 1) & 0b1_1000) | ((flags >> 2) & 0b1110_0000);
            return Some(colour(fg) | colour(bg) << 9 | rendition << 18);
        }
        if fg & 0xff == 2 && bg == 0 && self.flags & !Self::DIRECT == 0 {
            let bold = flags & u32::from(Self::BOLD);
            let italic = (flags & u32::from(Self::ITALIC)) >> 2;
            return Some(1 << 26 | fg >> 8 | bold << 24 | italic << 25);
        }
        None
    }
    /// The attributes of the style `inline_style` numbers `code`: out
    /// of line, as readers that walk cells find a style once for a run of
    /// them, and keep their loop small.
    #[inline(never)]
    pub(crate) fn from_inline_style(code: u32) -> Self {
        fn colour(c: u32) -> Packed {
            match c.checked_sub(1).and_then(|i| u8::try_from(i).ok()) {
                Some(i) => Packed([1, i, 0, 0]),
                None => Packed([0; 4]),
            }
        }
        if code & 1 << 26 != 0 {
            let [r, g, b, _] = code.to_le_bytes();
            let flags = (code >> 24) & 1 | ((code >> 25) & 1) << 2;
            return Self {
                foreground: Packed([2, r, g, b]),
                background: Packed([0; 4]),
                underline_color: Packed([0; 4]),
                flags: u16::try_from(flags).unwrap_or(0),
            };
        }
        let rendition = (code >> 18) & 0xff;
        let flags =
            (rendition & 0b111) | ((rendition & 0b1_1000) << 1) | ((rendition & 0b1110_0000) << 2);
        Self {
            foreground: colour(code & 0x1ff),
            background: colour((code >> 9) & 0x1ff),
            underline_color: Packed([0; 4]),
            flags: u16::try_from(flags).unwrap_or(0),
        }
    }
    /// The attributes as two words, which differ where the attributes do:
    /// what a style table hashes (`style.rs`).
    pub(crate) fn bits(self) -> (u64, u64) {
        let word = |p: Packed| u64::from(u32::from_le_bytes(p.0));
        (
            word(self.foreground) | word(self.background) << 32,
            word(self.underline_color) | u64::from(self.flags) << 32,
        )
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
    /// The background colour.
    pub const fn background(self) -> Color {
        self.background.get()
    }
    /// SGR 58; `Default` draws underlines in the foreground colour.
    pub const fn underline_color(self) -> Color {
        self.underline_color.get()
    }
    /// These attributes with `color` as the foreground.
    #[must_use]
    pub const fn with_foreground(mut self, color: Color) -> Self {
        self.foreground = Packed::new(color);
        self
    }
    /// These attributes with `color` as the background.
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
    /// These attributes with bold (SGR 1) on or off.
    #[must_use]
    pub const fn with_bold(self, on: bool) -> Self {
        self.with_flag(Self::BOLD, on)
    }
    /// These attributes with dim (SGR 2) on or off.
    #[must_use]
    pub const fn with_dim(self, on: bool) -> Self {
        self.with_flag(Self::DIM, on)
    }
    /// These attributes with italic (SGR 3) on or off.
    #[must_use]
    pub const fn with_italic(self, on: bool) -> Self {
        self.with_flag(Self::ITALIC, on)
    }
    /// These attributes with a single underline (SGR 4), or none of any
    /// style.
    #[must_use]
    pub const fn with_underline(self, on: bool) -> Self {
        self.with_underline_style(if on {
            UnderlineStyle::Single
        } else {
            UnderlineStyle::None
        })
    }
    /// These attributes underlined in `style` (SGR 4, 21, 24 and `4:n`).
    #[must_use]
    pub const fn with_underline_style(mut self, style: UnderlineStyle) -> Self {
        self.flags = (self.flags & !Self::UNDERLINE) | (style.number() << Self::UNDERLINE_SHIFT);
        self
    }
    /// These attributes with inverse (SGR 7) on or off.
    #[must_use]
    pub const fn with_inverse(self, on: bool) -> Self {
        self.with_flag(Self::INVERSE, on)
    }
    /// These attributes blinking as `blink` says (SGR 5, 6 and 25).
    #[must_use]
    pub const fn with_blink(self, blink: Blink) -> Self {
        let bit = match blink {
            Blink::None => 0,
            Blink::Slow => Self::SLOW_BLINK,
            Blink::Rapid => Self::RAPID_BLINK,
        };
        self.with_flag(Self::BLINK, false).with_flag(bit, true)
    }
    /// These attributes with hidden (SGR 8) on or off.
    #[must_use]
    pub const fn with_hidden(self, on: bool) -> Self {
        self.with_flag(Self::HIDDEN, on)
    }
    /// These attributes with strikeout (SGR 9) on or off.
    #[must_use]
    pub const fn with_strikeout(self, on: bool) -> Self {
        self.with_flag(Self::STRIKEOUT, on)
    }
    /// These attributes with `color` as the underline colour (SGR 58).
    #[must_use]
    pub const fn with_underline_color(mut self, color: Color) -> Self {
        self.underline_color = Packed::new(color);
        self
    }
    /// Whether bold (SGR 1) is on.
    pub fn bold(self) -> bool {
        self.flags & Self::BOLD != 0
    }
    /// Whether dim (SGR 2) is on.
    pub fn dim(self) -> bool {
        self.flags & Self::DIM != 0
    }
    /// Whether italic (SGR 3) is on.
    pub fn italic(self) -> bool {
        self.flags & Self::ITALIC != 0
    }
    /// Whether the text is underlined, in any style.
    pub fn underline(self) -> bool {
        self.flags & Self::UNDERLINE != 0
    }
    /// How the text is underlined.
    pub fn underline_style(self) -> UnderlineStyle {
        UnderlineStyle::from_number((self.flags & Self::UNDERLINE) >> Self::UNDERLINE_SHIFT)
            .unwrap_or(UnderlineStyle::None)
    }
    /// Whether inverse (SGR 7) is on.
    pub fn inverse(self) -> bool {
        self.flags & Self::INVERSE != 0
    }
    /// How the text blinks.
    pub fn blink(self) -> Blink {
        if self.flags & Self::SLOW_BLINK != 0 {
            Blink::Slow
        } else if self.flags & Self::RAPID_BLINK != 0 {
            Blink::Rapid
        } else {
            Blink::None
        }
    }
    /// Whether hidden (SGR 8) is on.
    pub fn hidden(self) -> bool {
        self.flags & Self::HIDDEN != 0
    }
    /// Whether strikeout (SGR 9) is on.
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
    pub(crate) const CONTINUATION: u8 = 0b0100_0000;
    pub(crate) const WIDE: u8 = 0b1000_0000;
    pub(crate) const HALVES: u8 = Self::WIDE | Self::CONTINUATION;

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
    /// Whether the cell holds text: neither blank nor the second half of a
    /// wide glyph.
    pub fn has_contents(&self) -> bool {
        self.length & (Self::LENGTH | Self::SPILLED) != 0
    }
    /// Whether the cell holds a glyph two columns wide, the next cell being
    /// its second half.
    pub fn is_wide(&self) -> bool {
        self.length & Self::WIDE != 0
    }
    /// Whether the cell is the second half of the wide glyph before it.
    pub fn is_wide_continuation(&self) -> bool {
        self.length & Self::CONTINUATION != 0
    }
    /// The cell's colours and rendition.
    pub fn attributes(&self) -> Attributes {
        self.attributes
    }

    pub(crate) fn blank(attributes: Attributes) -> Self {
        Self {
            attributes,
            ..Self::default()
        }
    }
    pub(crate) fn continuation() -> Self {
        Self {
            length: Self::CONTINUATION,
            ..Self::default()
        }
    }
    pub(crate) fn is_spilled(&self) -> bool {
        self.length & Self::SPILLED != 0
    }
    /// The inline text; empty for a spilled cell.
    fn inline(&self) -> &str {
        // One ASCII byte, as most cells hold, or none, as a blank holds:
        // its text without validating it, which readers that walk every
        // cell (copy, search, a host's paint) would pay on each.
        let length = self.length & (Self::LENGTH | Self::SPILLED);
        if length == 0 {
            return "";
        }
        if length == 1
            && let Some(&byte) = self.text.first()
            && byte.is_ascii()
        {
            let at = usize::from(byte);
            return at
                .checked_add(1)
                .and_then(|end| ASCII.get(at..end))
                .unwrap_or("");
        }
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
pub(crate) struct Spill(pub(crate) Vec<u8>);

impl Spill {
    /// The most bytes a run of `cells` cells keeps: 32 a cell, about as much
    /// as the cells themselves, and one whole cluster more, so that even a
    /// one-column row holds its longest.
    pub(crate) fn limit(cells: usize) -> usize {
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
            // Never: the range's length was a `u8`. Blanked as above, so
            // that no cell points into the text just taken.
            let Ok(len) = u8::try_from(text.len()) else {
                *cell = Cell::blank(cell.attributes);
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
    #[cfg(test)]
    pub(crate) fn text(&self, i: usize) -> &str {
        self.cells.get(i).map_or("", |cell| self.spill.text(cell))
    }
}

/// The longest start of `text` of at most `max` bytes that ends on a char
/// boundary.
pub(crate) fn floor(text: &str, max: usize) -> &str {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    text.get(..end).unwrap_or("")
}

/// A cell as it is on the screen: its halves, attributes and whole text,
/// read from wherever the cell keeps them.
#[derive(Clone, Copy)]
pub struct CellRef<'a> {
    text: &'a str,
    attributes: Attributes,
    /// `Cell::WIDE`, `Cell::CONTINUATION`, and `CellRef::CONTENTS` if the
    /// cell holds text.
    flags: u8,
}

impl<'a> CellRef<'a> {
    /// Whether the cell holds text, among `flags`: a bit neither half's.
    const CONTENTS: u8 = 1;

    pub(crate) fn new(cell: &'a Cell, spill: &'a Spill) -> Self {
        Self::of(
            spill.text(cell),
            cell.attributes,
            cell.length & Cell::HALVES,
            cell.has_contents(),
        )
    }
    /// A cell with `text` and `attributes`, the halves of a wide glyph
    /// `halves` says (`Cell::WIDE`, `Cell::CONTINUATION`), with contents
    /// or not.
    #[inline]
    pub(crate) fn of(text: &'a str, attributes: Attributes, halves: u8, contents: bool) -> Self {
        Self {
            text,
            attributes,
            flags: (halves & Cell::HALVES) | u8::from(contents),
        }
    }
    /// A cell with the halves and attributes of this one and no text, which
    /// `Line::set` gives its text.
    fn template(&self) -> Cell {
        Cell {
            length: self.flags & Cell::HALVES,
            ..Cell::blank(self.attributes)
        }
    }
    /// The cell's grapheme cluster; empty for a blank cell or the second
    /// half of a wide glyph.
    pub fn contents(&self) -> &'a str {
        self.text
    }
    /// Whether the cell holds text: neither blank nor the second half of a
    /// wide glyph.
    pub fn has_contents(&self) -> bool {
        self.flags & Self::CONTENTS != 0
    }
    /// Whether the cell holds a glyph two columns wide, the next cell being
    /// its second half.
    pub fn is_wide(&self) -> bool {
        self.flags & Cell::WIDE != 0
    }
    /// Whether the cell is the second half of the wide glyph before it.
    pub fn is_wide_continuation(&self) -> bool {
        self.flags & Cell::CONTINUATION != 0
    }
    /// The cell's colours and rendition.
    pub fn attributes(&self) -> Attributes {
        self.attributes
    }
    /// The foreground colour.
    pub fn fgcolor(&self) -> Color {
        self.attributes.foreground()
    }
    /// The background colour.
    pub fn bgcolor(&self) -> Color {
        self.attributes.background()
    }
    /// The underline colour (SGR 58).
    pub fn underline_color(&self) -> Color {
        self.attributes.underline_color()
    }
    /// Whether bold (SGR 1) is on.
    pub fn bold(&self) -> bool {
        self.attributes.bold()
    }
    /// Whether dim (SGR 2) is on.
    pub fn dim(&self) -> bool {
        self.attributes.dim()
    }
    /// Whether italic (SGR 3) is on.
    pub fn italic(&self) -> bool {
        self.attributes.italic()
    }
    /// Whether the text is underlined, in any style.
    pub fn underline(&self) -> bool {
        self.attributes.underline()
    }
    /// How the text is underlined.
    pub fn underline_style(&self) -> UnderlineStyle {
        self.attributes.underline_style()
    }
    /// Whether inverse (SGR 7) is on.
    pub fn inverse(&self) -> bool {
        self.attributes.inverse()
    }
    /// How the text blinks.
    pub fn blink(&self) -> Blink {
        self.attributes.blink()
    }
    /// Whether hidden (SGR 8) is on.
    pub fn hidden(&self) -> bool {
        self.attributes.hidden()
    }
    /// Whether strikeout (SGR 9) is on.
    pub fn strikeout(&self) -> bool {
        self.attributes.strikeout()
    }
}

/// Equal if they look the same: text, halves and attributes, wherever each
/// keeps its text.
impl PartialEq for CellRef<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.flags & Cell::HALVES == other.flags & Cell::HALVES
            && self.attributes == other.attributes
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
            .field("attributes", &self.attributes)
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
    /// How many cells there are.
    pub fn len(&self) -> usize {
        self.cells.len()
    }
    /// Whether there are no cells.
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }
    /// The cell at `i`.
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
    /// Whether the cells in `range` of `self` and of `other` look the same,
    /// cell by cell, as their [`CellRef`]s compare: the same as
    /// `self.range(range).eq(other.range(range))`, faster. Two cells that
    /// hold their text inline, as nearly all do, are compared whole, which
    /// is the same: inline text is zeros past its length, and a text that
    /// fits inline is never spilled.
    pub fn range_eq(&self, other: &Cells, range: std::ops::Range<usize>) -> bool {
        let (Some(mine), Some(theirs)) = (
            self.cells.get(range.clone()),
            other.cells.get(range.clone()),
        ) else {
            return self.range(range.clone()).eq(other.range(range));
        };
        mine.iter().zip(theirs).all(|(a, b)| {
            if !a.is_spilled() && !b.is_spilled() {
                a == b
            } else {
                CellRef::new(a, &self.spill) == CellRef::new(b, &other.spill)
            }
        })
    }
    /// Sets cell `i` to a copy of `cell`, from wherever it keeps its text.
    /// Whether its text was kept whole (see [`Cells::set_text`]).
    pub fn set(&mut self, i: usize, cell: CellRef<'_>) -> bool {
        let text = cell.contents();
        // Text that fits inline, as most does, is stored as `Line::set`
        // stores it, without the template it takes.
        if text.len() <= Cell::INLINE_CAPACITY {
            let Some(slot) = self.cells.get_mut(i) else {
                return false;
            };
            let mut stored = Cell::blank(cell.attributes);
            match (text.as_bytes(), stored.text.first_mut()) {
                // One byte, as most text is: stored without a copy's call.
                ([byte], Some(first)) => *first = *byte,
                (bytes, _) => {
                    if let Some(dst) = stored.text.get_mut(..bytes.len()) {
                        crate::copy_from(dst, bytes);
                    }
                }
            }
            // At most the capacity, which fits the length's bits.
            stored.length = u8::try_from(text.len()).unwrap_or(0) | (cell.flags & Cell::HALVES);
            *slot = stored;
            return true;
        }
        self.line().set(i, cell.template(), text)
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
        self.len() == other.len() && self.range_eq(other, 0..self.len())
    }
}
impl Eq for Cells {}

#[cfg(test)]
mod tests;
