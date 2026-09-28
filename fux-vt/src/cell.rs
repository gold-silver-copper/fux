//! Fixed-size cells: no output-dependent heap allocation for combining marks.

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

/// Attributes are independent of glyph storage and copied onto erased cells.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Attributes {
    pub foreground: Color,
    pub background: Color,
    /// SGR 58; `Default` draws underlines in the foreground colour.
    pub underline_color: Color,
    pub(crate) flags: u16,
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
            foreground,
            background,
            underline_color: Color::Default,
            flags: 0,
        }
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
        self.underline_color = color;
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

/// One fixed-size glyph cell. A continuation has empty contents and default
/// attributes; its leader owns the wide glyph's appearance. The bytes of
/// `text` past its length are always zero: every constructor starts from
/// zeros, and `append` only writes past the length and lengthens it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cell {
    text: [u8; 25],
    length: u8,
    pub(crate) attributes: Attributes,
}

impl Cell {
    /// The most UTF-8 bytes a cell stores.
    pub const CONTENTS_CAPACITY: usize = 25;
    // The bits of `length`: how many bytes of `text` are the contents, and
    // which half of a wide glyph the cell is, if either.
    const LENGTH: u8 = 0b0001_1111;
    const CONTINUATION: u8 = 0b0100_0000;
    const WIDE: u8 = 0b1000_0000;

    /// A cell built by a consumer that stores or transports screen contents.
    /// `None` if `contents` exceeds [`Cell::CONTENTS_CAPACITY`]. Parser output
    /// never needs this; it exists so a copy can be reconstructed exactly.
    pub fn new(contents: &str, wide: bool, attributes: Attributes) -> Option<Self> {
        let bytes = contents.as_bytes();
        if bytes.len() > Self::CONTENTS_CAPACITY {
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
    pub fn contents(&self) -> &str {
        // Every write uses encode_utf8, and length always ends on a scalar boundary.
        self.text
            .get(..usize::from(self.length & Self::LENGTH))
            .and_then(|s| std::str::from_utf8(s).ok())
            .unwrap_or("")
    }
    pub fn has_contents(&self) -> bool {
        self.length & Self::LENGTH != 0
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
    pub fn fgcolor(&self) -> Color {
        self.attributes.foreground
    }
    pub fn bgcolor(&self) -> Color {
        self.attributes.background
    }
    pub fn bold(&self) -> bool {
        self.attributes.bold()
    }
    pub fn dim(&self) -> bool {
        self.attributes.dim()
    }
    pub fn italic(&self) -> bool {
        self.attributes.italic()
    }
    pub fn underline(&self) -> bool {
        self.attributes.underline()
    }
    pub fn inverse(&self) -> bool {
        self.attributes.inverse()
    }
    pub fn blink(&self) -> Blink {
        self.attributes.blink()
    }
    pub fn hidden(&self) -> bool {
        self.attributes.hidden()
    }
    pub fn strikeout(&self) -> bool {
        self.attributes.strikeout()
    }
    pub fn underline_color(&self) -> Color {
        self.attributes.underline_color
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
        let mut text = [0; Self::CONTENTS_CAPACITY];
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
    /// the text in use, as what follows it is zero in every cell.
    pub(crate) fn same(&self, other: &Cell) -> bool {
        let used = usize::from(self.length & Self::LENGTH);
        self.length == other.length
            && self.attributes == other.attributes
            && self.text.get(..used) == other.text.get(..used)
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
    /// Whether `append(c)` has room for `c`: an empty cell takes a space
    /// before it.
    pub(crate) fn can_append(&self, c: char) -> bool {
        usize::from(self.length & Self::LENGTH)
            .max(1)
            .checked_add(c.len_utf8())
            .is_some_and(|end| end <= Self::CONTENTS_CAPACITY)
    }
    /// Adds `c` to the contents, if it fits; an empty cell first takes a
    /// space for `c` to follow.
    pub(crate) fn append(&mut self, c: char) {
        if !self.can_append(c) {
            return;
        }
        let mut len = usize::from(self.length & Self::LENGTH);
        if len == 0 {
            if let Some(first) = self.text.first_mut() {
                *first = b' ';
            }
            len = 1;
        }
        // Encoded in place after what is there: `can_append` found room.
        let mut encoded = [0; 4];
        let encoded = c.encode_utf8(&mut encoded).as_bytes();
        if let Some(end) = len.checked_add(encoded.len())
            && let Some(free) = self.text.get_mut(len..end)
            && crate::copy_from(free, encoded).is_some()
            && let Ok(length) = u8::try_from(end)
        {
            self.length = (self.length & !Self::LENGTH) | length;
        }
    }
    /// Marks the cell as the leading half of a wide glyph.
    pub(crate) fn widen(&mut self) {
        self.length |= Self::WIDE;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What follows a cell's text is zero, however it was made, so `same`
    /// and `is_ascii`, which compare only the text in use, agree with `==`.
    #[test]
    fn text_past_the_length_is_zero_and_the_quick_comparisons_agree() {
        let bold = Attributes::default().with_bold(true);
        let red = Attributes::new(Color::Idx(1), Color::Rgb(1, 2, 3));
        let mut cells = vec![
            Cell::default(),
            Cell::blank(bold),
            Cell::continuation(),
            Cell::ascii(b'a', Attributes::default()),
            Cell::ascii(b'a', bold),
            Cell::ascii(b'b', red),
            Cell::glyph('界', 2, red),
            Cell::glyph('é', 1, Attributes::default()),
            Cell::glyph(' ', 1, bold),
            Cell::new("xy", false, red).unwrap_or_default(),
        ];
        let mut marked = Cell::glyph('a', 1, bold);
        let mut blank = Cell::blank(red);
        for _ in 0..30 {
            marked.append('\u{301}');
            blank.append('\u{302}');
            cells.push(marked);
            cells.push(blank);
        }
        for cell in &cells {
            let used = usize::from(cell.length & Cell::LENGTH);
            assert!(cell.text.iter().skip(used).all(|b| *b == 0), "{cell:?}");
            for other in &cells {
                assert_eq!(cell.same(other), cell == other, "{cell:?} {other:?}");
            }
            for (byte, attributes) in [(b'a', Attributes::default()), (b'a', bold), (b'b', red)] {
                assert_eq!(
                    cell.is_ascii(byte, attributes),
                    *cell == Cell::ascii(byte, attributes),
                    "{cell:?}"
                );
            }
        }
    }

    #[test]
    fn text_and_attributes_are_fixed_size_and_bounded() {
        assert_eq!(std::mem::size_of::<Cell>(), 40);
        let attributes = Attributes {
            foreground: Color::Idx(9),
            background: Color::Rgb(1, 2, 3),
            underline_color: Color::Idx(4),
            flags: 0x1ff & !Attributes::RAPID_BLINK,
        };
        let mut c = Cell::glyph('界', 2, attributes);
        c.append('\u{301}');
        assert_eq!(c.contents(), "界\u{301}");
        assert!(c.has_contents() && c.is_wide() && !c.is_wide_continuation());
        assert_eq!(c.attributes(), attributes);
        assert_eq!(c.fgcolor(), Color::Idx(9));
        assert_eq!(c.bgcolor(), Color::Rgb(1, 2, 3));
        assert!(c.bold() && c.dim() && c.italic() && c.underline() && c.inverse());
        assert!(c.hidden() && c.strikeout() && c.blink() == Blink::Slow);
        assert_eq!(c.underline_color(), Color::Idx(4));
        // Marks are two bytes: the glyph's three and eleven marks fill it.
        for _ in 0..1000 {
            c.append('\u{301}');
        }
        assert_eq!(c.contents().len(), Cell::CONTENTS_CAPACITY);
        assert!(!c.can_append('a'));
        assert!(Cell::continuation().is_wide_continuation());
        assert!(!Cell::continuation().has_contents());
        let mut blank = Cell::default();
        blank.append('\u{301}');
        assert_eq!(blank.contents(), " \u{301}");
        assert_eq!(Cell::ascii(b'A', Attributes::default()).contents(), "A");
    }
}
