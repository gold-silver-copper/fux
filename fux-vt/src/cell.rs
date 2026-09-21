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
        if let Some(dst) = cell.text.get_mut(..text.len()) {
            dst.copy_from_slice(text.as_bytes());
        }
        cell.length = text.len() as u8 | if width == 2 { 128 } else { 0 };
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
        if let Some(dst) = self.text.get_mut(len..len + text.len()) {
            dst.copy_from_slice(text.as_bytes());
            self.length = (self.length & 0xe0) | (len + text.len()) as u8;
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
