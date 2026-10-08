//! What a cell is to a reader: its text, halves and [`Attributes`], read
//! through a [`CellRef`], which borrows the text from wherever it is kept.
//! Cells are stored one way only (`compact.rs`), in a screen's rows and in
//! a host's [`Cells`] alike, so a reader never sees half a cluster.

use crate::compact::{BLANK, Compact, Line, Text};

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

/// The most UTF-8 bytes of one grapheme cluster a cell keeps. Characters
/// past it still belong to the cluster, and its cell, but are dropped: a
/// cluster is never split, so what follows it is always where a program
/// laid out with UAX #29 expects. 128 is the Unicode stream-safe limit of a
/// starter and 30 combining marks, and holds every emoji sequence.
pub const CLUSTER_CAPACITY: usize = 128;

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
/// read from wherever the cell keeps them, or made by a host to store
/// ([`CellRef::new`]). It borrows its text, so it cannot outlive where the
/// text is kept.
#[derive(Clone, Copy, Default)]
pub struct CellRef<'a> {
    text: &'a str,
    attributes: Attributes,
    /// `WIDE`, `CONTINUATION`, and `CONTENTS` if the cell holds text.
    flags: u8,
}

impl<'a> CellRef<'a> {
    /// Whether the cell holds text, among `flags`: a bit neither half's.
    const CONTENTS: u8 = 1;
    /// The second half of a wide glyph; and its first.
    pub(crate) const CONTINUATION: u8 = 0b0100_0000;
    pub(crate) const WIDE: u8 = 0b1000_0000;
    pub(crate) const HALVES: u8 = Self::WIDE | Self::CONTINUATION;

    /// A cell holding `text`, two columns wide if `wide`, in `attributes`:
    /// for a host to store in [`Cells`]. Text holds no NUL, as a screen's
    /// never does.
    pub fn new(text: &'a str, wide: bool, attributes: Attributes) -> Self {
        let halves = if wide { Self::WIDE } else { 0 };
        Self::of(text, attributes, halves, !text.is_empty())
    }
    /// The second half of a wide glyph: empty, default attributes.
    pub fn wide_continuation() -> Self {
        Self::of("", Attributes::default(), Self::CONTINUATION, false)
    }
    /// A cell with `text` and `attributes`, the halves of a wide glyph
    /// `halves` says (`WIDE`, `CONTINUATION`), with contents or not.
    #[inline]
    pub(crate) fn of(text: &'a str, attributes: Attributes, halves: u8, contents: bool) -> Self {
        Self {
            text,
            attributes,
            flags: (halves & Self::HALVES) | u8::from(contents),
        }
    }
    /// The halves, as `WIDE` and `CONTINUATION`.
    pub(crate) fn halves(&self) -> u8 {
        self.flags & Self::HALVES
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
        self.flags & Self::WIDE != 0
    }
    /// Whether the cell is the second half of the wide glyph before it.
    pub fn is_wide_continuation(&self) -> bool {
        self.flags & Self::CONTINUATION != 0
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
        self.flags & CellRef::HALVES == other.flags & CellRef::HALVES
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

/// An owned run of cells, for a host that stores screen contents: a
/// composed frame, a copy. It keeps its cells as a screen's rows keep
/// theirs (`compact.rs`), with their attributes beside them, and so keeps
/// and cuts text as a row does: 32 bytes a cell of long clusters, and one
/// cluster more.
#[derive(Clone, Debug, Default)]
pub struct Cells {
    /// Each cell's text and halves; its style number unused (always 0),
    /// its attributes being in `attributes`, one for each cell.
    cells: Vec<Compact>,
    attributes: Vec<Attributes>,
    text: Text,
}

impl Cells {
    /// The most bytes of text `len` cells keep for long clusters: 32 a cell
    /// and one cluster more, so that even one cell holds its longest.
    pub fn text_limit(len: usize) -> usize {
        Text::long_limit(len)
    }
    /// Bytes of text kept for long clusters, overwritten ones included
    /// until compacted. For memory diagnostics.
    pub fn text_len(&self) -> usize {
        self.text.long_len()
    }
    /// `len` blank cells.
    pub fn new(len: usize) -> Self {
        Self {
            cells: vec![BLANK; len],
            attributes: vec![Attributes::default(); len],
            text: Text::default(),
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
        let (cell, attributes) = (self.cells.get(i)?, self.attributes.get(i)?);
        Some(cell.read_as(&self.text, *attributes))
    }
    /// The cells, in order.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = CellRef<'_>> + ExactSizeIterator + Clone {
        self.range(0..self.len())
    }
    /// The cells in `range`, clipped to those there are.
    pub fn range(
        &self,
        range: std::ops::Range<usize>,
    ) -> impl DoubleEndedIterator<Item = CellRef<'_>> + ExactSizeIterator + Clone {
        let text = &self.text;
        let end = range.end.min(self.cells.len());
        let start = range.start.min(end);
        let cells = self.cells.get(start..end).unwrap_or_default();
        let attributes = self.attributes.get(start..end).unwrap_or_default();
        cells
            .iter()
            .zip(attributes)
            .map(move |(cell, attributes)| cell.read_as(text, *attributes))
    }
    /// Whether the cells in `range` of `self` and of `other` look the same,
    /// cell by cell, as their [`CellRef`]s compare: the same as
    /// `self.range(range).eq(other.range(range))`, faster. The cells are
    /// held one way only, so two that keep their text inline are the same
    /// if their bytes are.
    pub fn range_eq(&self, other: &Cells, range: std::ops::Range<usize>) -> bool {
        let (Some(mine), Some(theirs), Some(my_attributes), Some(their_attributes)) = (
            self.cells.get(range.clone()),
            other.cells.get(range.clone()),
            self.attributes.get(range.clone()),
            other.attributes.get(range.clone()),
        ) else {
            return self.range(range.clone()).eq(other.range(range));
        };
        my_attributes == their_attributes
            && mine
                .iter()
                .zip(theirs)
                .all(|(a, b)| a.same_as(&self.text, b, &other.text))
    }
    /// Sets cell `i` to a copy of `cell`, from wherever it keeps its text.
    /// A cluster longer than [`CLUSTER_CAPACITY`] is cut there, and one the
    /// cells have no room left for to its first 17 bytes; whether the text
    /// was kept whole.
    pub fn set(&mut self, i: usize, cell: CellRef<'_>) -> bool {
        let Some(attributes) = self.attributes.get_mut(i) else {
            return false;
        };
        *attributes = cell.attributes;
        self.line()
            .set(i, Compact::shaped(cell.halves()), cell.text)
    }
    /// Gives cell `i` new attributes, keeping its text.
    pub fn set_attributes(&mut self, i: usize, attributes: Attributes) {
        if let Some(at) = self.attributes.get_mut(i) {
            *at = attributes;
        }
    }
    /// Sets the cells in `range`, clipped to those there are, to `cell`.
    pub fn fill(&mut self, range: std::ops::Range<usize>, cell: CellRef<'_>) {
        let end = range.end.min(self.cells.len());
        let start = range.start.min(end);
        if start == 0 && end == self.cells.len() {
            self.text.release();
        }
        // The cell as one with its text inline, if it fits there.
        let mut one = [BLANK];
        let mut text = Text::default();
        Line {
            cells: &mut one,
            text: &mut text,
        }
        .set(0, Compact::shaped(cell.halves()), cell.text);
        let [inline] = one;
        match (
            self.cells.get_mut(start..end),
            self.attributes.get_mut(start..end),
        ) {
            (Some(cells), Some(attributes)) if text.is_empty() => {
                cells.fill(inline);
                attributes.fill(cell.attributes);
            }
            _ => {
                for i in start..end {
                    self.set(i, cell);
                }
            }
        }
    }
    /// Makes the run `len` cells long, new ones `cell`. A shorter run keeps
    /// a shorter run's budget: its cells' text is stored again, left to
    /// right, and what no longer fits is cut to its first 17 bytes.
    pub fn resize(&mut self, len: usize, cell: CellRef<'_>) {
        let was = self.cells.len();
        self.cells.resize(len, BLANK);
        self.attributes.resize(len, Attributes::default());
        if len < was {
            self.text = Line::rebuilt(&mut self.cells, &self.text);
        } else {
            self.fill(was..len, cell);
        }
    }
    fn line(&mut self) -> Line<'_> {
        Line {
            cells: &mut self.cells,
            text: &mut self.text,
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
