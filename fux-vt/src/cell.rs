//! Fixed-size cells: no output-dependent heap allocation for combining marks.

/// A default, indexed, or true-colour terminal colour.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Color {
    #[default]
    Default,
    Idx(u8),
    Rgb(u8, u8, u8),
}

/// Attributes are independent of glyph storage and copied onto erased cells.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Attributes {
    pub foreground: Color,
    pub background: Color,
    pub(crate) flags: u8,
}

impl Attributes {
    /// Plain attributes with the given colours; add styles with the `with_*`
    /// builders. For consumers that store or transport cells.
    pub const fn new(foreground: Color, background: Color) -> Self {
        Self {
            foreground,
            background,
            flags: 0,
        }
    }
    const fn with_flag(mut self, bit: u8, on: bool) -> Self {
        self.flags = if on {
            self.flags | bit
        } else {
            self.flags & !bit
        };
        self
    }
    #[must_use]
    pub const fn with_bold(self, on: bool) -> Self {
        self.with_flag(1, on)
    }
    #[must_use]
    pub const fn with_dim(self, on: bool) -> Self {
        self.with_flag(2, on)
    }
    #[must_use]
    pub const fn with_italic(self, on: bool) -> Self {
        self.with_flag(4, on)
    }
    #[must_use]
    pub const fn with_underline(self, on: bool) -> Self {
        self.with_flag(8, on)
    }
    #[must_use]
    pub const fn with_inverse(self, on: bool) -> Self {
        self.with_flag(16, on)
    }
    pub fn bold(self) -> bool {
        self.flags & 1 != 0
    }
    pub fn dim(self) -> bool {
        self.flags & 2 != 0
    }
    pub fn italic(self) -> bool {
        self.flags & 4 != 0
    }
    pub fn underline(self) -> bool {
        self.flags & 8 != 0
    }
    pub fn inverse(self) -> bool {
        self.flags & 16 != 0
    }
}

/// One fixed-size glyph cell. A continuation has empty contents and default
/// attributes; its leader owns the wide glyph's appearance.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cell {
    text: [u8; 22],
    length: u8,
    pub(crate) attributes: Attributes,
}

impl Cell {
    /// The most UTF-8 bytes a cell stores.
    pub const CONTENTS_CAPACITY: usize = 22;

    /// A cell built by a consumer that stores or transports screen contents.
    /// `None` if `contents` exceeds [`Cell::CONTENTS_CAPACITY`]. Parser output
    /// never needs this; it exists so a copy can be reconstructed exactly.
    pub fn new(contents: &str, wide: bool, attributes: Attributes) -> Option<Self> {
        let bytes = contents.as_bytes();
        if bytes.len() > Self::CONTENTS_CAPACITY {
            return None;
        }
        let mut cell = Self::blank(attributes);
        cell.text.get_mut(..bytes.len())?.copy_from_slice(bytes);
        cell.length = u8::try_from(bytes.len()).ok()? | if wide { 128 } else { 0 };
        Some(cell)
    }
    /// The trailing half of a wide glyph: empty, default attributes.
    pub fn wide_continuation() -> Self {
        Self::continuation()
    }
    pub fn contents(&self) -> &str {
        // Every write uses encode_utf8, and length always ends on a scalar boundary.
        self.text
            .get(..usize::from(self.length & 31))
            .and_then(|s| std::str::from_utf8(s).ok())
            .unwrap_or("")
    }
    pub fn has_contents(&self) -> bool {
        self.length & 31 != 0
    }
    pub fn is_wide(&self) -> bool {
        self.length & 128 != 0
    }
    pub fn is_wide_continuation(&self) -> bool {
        self.length & 64 != 0
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

    pub(crate) fn blank(attributes: Attributes) -> Self {
        Self {
            attributes,
            ..Self::default()
        }
    }
    pub(crate) fn glyph(c: char, width: usize, attributes: Attributes) -> Self {
        let mut cell = Self::blank(attributes);
        let mut bytes = [0; 4];
        let text = c.encode_utf8(&mut bytes);
        // A char is at most four UTF-8 bytes, so both always hold.
        if let Some(dst) = cell.text.get_mut(..text.len())
            && let Ok(length) = u8::try_from(text.len())
        {
            dst.copy_from_slice(text.as_bytes());
            cell.length = length | if width == 2 { 128 } else { 0 };
        }
        cell
    }
    pub(crate) fn ascii(byte: u8, attributes: Attributes) -> Self {
        let mut cell = Self::blank(attributes);
        if let Some(first) = cell.text.first_mut() {
            *first = byte;
        }
        cell.length = 1;
        cell
    }
    pub(crate) fn continuation() -> Self {
        Self {
            length: 64,
            ..Self::default()
        }
    }
    pub(crate) fn append(&mut self, c: char) {
        let mut len = usize::from(self.length & 31);
        if len >= 18 {
            return;
        }
        if len == 0 {
            if let Some(first) = self.text.first_mut() {
                *first = b' ';
            }
            len = 1;
        }
        let mut bytes = [0; 4];
        let text = c.encode_utf8(&mut bytes);
        if let Some(dst) = self.text.get_mut(len..len + text.len())
            && let Ok(length) = u8::try_from(len + text.len())
        {
            dst.copy_from_slice(text.as_bytes());
            self.length = (self.length & 0xe0) | length;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_and_attributes_are_fixed_size_and_bounded() {
        assert_eq!(std::mem::size_of::<Cell>(), 32);
        let attributes = Attributes {
            foreground: Color::Idx(9),
            background: Color::Rgb(1, 2, 3),
            flags: 31,
        };
        let mut c = Cell::glyph('界', 2, attributes);
        c.append('\u{301}');
        assert_eq!(c.contents(), "界\u{301}");
        assert!(c.has_contents() && c.is_wide() && !c.is_wide_continuation());
        assert_eq!(c.attributes(), attributes);
        assert_eq!(c.fgcolor(), Color::Idx(9));
        assert_eq!(c.bgcolor(), Color::Rgb(1, 2, 3));
        assert!(c.bold() && c.dim() && c.italic() && c.underline() && c.inverse());
        for _ in 0..1000 {
            c.append('\u{301}');
        }
        assert_eq!(c.contents().len(), 19);
        assert!(Cell::continuation().is_wide_continuation());
        assert!(!Cell::continuation().has_contents());
        let mut blank = Cell::default();
        blank.append('\u{301}');
        assert_eq!(blank.contents(), " \u{301}");
        assert_eq!(Cell::ascii(b'A', Attributes::default()).contents(), "A");
    }
}
