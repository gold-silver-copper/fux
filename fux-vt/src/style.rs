//! Styles: a cell holds a number of 28 bits for its attributes, its style,
//! rather than the attributes themselves.
//!
//! The styles programs mostly use are their own numbers
//! (`Attributes::inline_style`): no colour but the default and the 256
//! indexed ones, no underline colour, no underline but a single one, and any
//! other rendition but rapid blink; or a direct foreground on the default
//! background, bold, italic, both or neither, as syntax highlighting
//! prints.
//! Their number says what they are, so printing in them costs no search,
//! and reading them no table. The default attributes are style 0, so a
//! blank cell is all zeros.
//!
//! Every other style (a direct background, an underline colour, a double,
//! curly, dotted or dashed underline, rapid blink) is kept once in a grid's
//! table, and its number, with
//! [`TABLE`] set, is its place there. Nothing counts the cells of each:
//! writing a cell costs no more than writing its number. Instead, when the
//! table has grown to twice the styles in use at the last sweep, and past a
//! floor that grows with the grid, the grid sweeps it (`Grid::sweep`): it
//! marks the styles its cells use, keeps those, in the order they had, and
//! writes their new numbers into its cells. A sweep reads every cell, but
//! comes only after as many new styles as there were in use, and at least
//! an eighth of the grid's cells, so a style costs a few cell reads however
//! many there are. No reader sees a style's number.

use crate::Attributes;

/// The bit of a style's number that says it is in the table.
pub(crate) const TABLE: u32 = 1 << 27;

/// The most styles a table holds: the numbers below [`TABLE`]. More than a
/// grid's cells (`grid::MAX_CELLS`), so there is always room after a sweep.
pub(crate) const MAX_STYLES: usize = 1 << 27;

/// The fewest styles a table holds before it is swept.
const FLOOR: usize = 4096;

/// An index slot with no style in it.
const EMPTY: u32 = u32::MAX;

/// A grid's table of styles: their attributes by place, and an index that
/// finds a style's place from its attributes (open addressing, linear
/// probing, at most half full).
#[derive(Clone, Debug, Default)]
pub(crate) struct Styles {
    table: Vec<Attributes>,
    slots: Vec<u32>,
    /// How many styles were in use when the table was last swept.
    live: usize,
}

/// Where a search for `attributes` starts in `slots` of length `mask + 1`.
#[inline]
fn home(attributes: Attributes, mask: usize) -> usize {
    let (a, b) = attributes.bits();
    let hash = (a ^ b.rotate_left(23)).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    // The top bits, which mix every bit of the key.
    usize::try_from(hash >> 32).unwrap_or(0) & mask
}

impl Styles {
    /// The attributes of style `style`; the default for a number with no
    /// style.
    #[inline]
    pub(crate) fn get(&self, style: u32) -> Attributes {
        if style & TABLE == 0 {
            return Attributes::from_inline_style(style);
        }
        Self::place(style)
            .and_then(|i| self.table.get(i))
            .copied()
            .unwrap_or_default()
    }

    /// Where style `style` is in the table, if it is a style of the table.
    #[inline]
    pub(crate) fn place(style: u32) -> Option<usize> {
        if style & TABLE == 0 {
            return None;
        }
        usize::try_from(style & !TABLE).ok()
    }

    /// How many styles the table holds, in use or not.
    pub(crate) fn len(&self) -> usize {
        self.table.len()
    }

    /// The number of the style with `attributes`, if the table has it.
    pub(crate) fn find(&self, attributes: Attributes) -> Option<u32> {
        let mask = self.slots.len().checked_sub(1)?;
        let mut at = home(attributes, mask);
        loop {
            let place = *self.slots.get(at)?;
            if place == EMPTY {
                return None;
            }
            let found = usize::try_from(place).ok().and_then(|i| self.table.get(i));
            if found == Some(&attributes) {
                return Some(place | TABLE);
            }
            at = at.wrapping_add(1) & mask;
        }
    }

    /// Whether the table is to be swept before another style is added:
    /// it has twice the styles in use at the last sweep, and more than the
    /// floor, which is at least an eighth of `cells`, the cells the grid
    /// holds, so a sweep comes after at least that many new styles; or it
    /// is full.
    pub(crate) fn wants_sweep(&self, cells: usize) -> bool {
        let floor = FLOOR.max(cells / 8);
        let len = self.table.len();
        len >= MAX_STYLES || (len >= floor && len >= self.live.saturating_mul(2))
    }

    /// Adds a style with `attributes`, which the table does not have; its
    /// number, or `None` if the table is full.
    pub(crate) fn insert(&mut self, attributes: Attributes) -> Option<u32> {
        if self.table.len() >= MAX_STYLES {
            return None;
        }
        let place = u32::try_from(self.table.len()).ok()?;
        self.table.push(attributes);
        // At most half full.
        if self.table.len().saturating_mul(2) > self.slots.len() {
            self.reindex();
        } else {
            self.index(place);
        }
        Some(place | TABLE)
    }

    /// Puts the style at `place` in the index, which has room for it.
    fn index(&mut self, place: u32) {
        let Some(mask) = self.slots.len().checked_sub(1) else {
            return;
        };
        let Some(&attributes) = usize::try_from(place).ok().and_then(|i| self.table.get(i)) else {
            return;
        };
        let mut at = home(attributes, mask);
        while let Some(slot) = self.slots.get_mut(at) {
            if *slot == EMPTY {
                *slot = place;
                return;
            }
            at = at.wrapping_add(1) & mask;
        }
    }

    /// Builds the index again, at least twice as large as the table.
    fn reindex(&mut self) {
        let size = self
            .table
            .len()
            .saturating_mul(2)
            .max(8)
            .checked_next_power_of_two()
            .unwrap_or(usize::MAX);
        self.slots = vec![EMPTY; size];
        for place in 0..self.table.len() {
            if let Ok(place) = u32::try_from(place) {
                self.index(place);
            }
        }
    }

    /// Keeps the styles `used` marks, by place, in their order. What each
    /// style's number becomes; a style let go becomes 0.
    pub(crate) fn retain(&mut self, used: &[bool]) -> Vec<u32> {
        let mut renumber = vec![0; self.table.len()];
        let mut kept = Vec::with_capacity(used.iter().filter(|u| **u).count());
        for (place, attributes) in self.table.iter().enumerate() {
            if used.get(place).copied().unwrap_or(false) {
                if let (Some(new), Ok(number)) =
                    (renumber.get_mut(place), u32::try_from(kept.len()))
                {
                    *new = number | TABLE;
                }
                kept.push(*attributes);
            }
        }
        self.table = kept;
        self.live = self.table.len();
        self.reindex();
        renumber
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Blink, Color, UnderlineStyle};

    /// A style no number holds: a direct colour.
    fn direct(n: u8) -> Attributes {
        Attributes::new(Color::Idx(n), Color::Rgb(n, 0, n.wrapping_mul(3)))
    }

    #[test]
    fn styles_of_the_table_are_found_by_their_attributes_and_numbered_in_order() {
        let mut styles = Styles::default();
        for n in 0..=255u8 {
            assert_eq!(styles.find(direct(n)), None);
            assert_eq!(styles.insert(direct(n)), Some(u32::from(n) | TABLE));
        }
        for n in 0..=255u8 {
            assert_eq!(styles.find(direct(n)), Some(u32::from(n) | TABLE));
            assert_eq!(styles.get(u32::from(n) | TABLE), direct(n));
        }
        assert_eq!(styles.get(9999 | TABLE), Attributes::default());
        // Keeping the odd ones renumbers them in order.
        let used: Vec<bool> = (0..styles.len()).map(|place| place % 2 == 1).collect();
        let renumber = styles.retain(&used);
        assert_eq!(styles.len(), 128);
        for n in 0..=255u8 {
            let place = usize::from(n);
            let new = renumber.get(place).copied().unwrap_or(u32::MAX);
            if place % 2 == 1 {
                assert_eq!(new, u32::try_from(place / 2).unwrap_or(0) | TABLE);
                assert_eq!(styles.find(direct(n)), Some(new));
            } else {
                assert_eq!((new, styles.find(direct(n))), (0, None));
            }
        }
    }

    /// Every style a number holds reads back as itself, and only those
    /// styles have numbers: the default and indexed colours, any rendition
    /// but rapid blink, and an underline that is single or none, with no
    /// colour; or a direct foreground on the default background, bold or
    /// italic or both or neither.
    #[test]
    fn a_style_of_its_own_number_reads_back_as_itself() {
        let styles = Styles::default();
        assert_eq!(Attributes::default().inline_style(), Some(0));
        let colours = [
            Color::Default,
            Color::Idx(0),
            Color::Idx(7),
            Color::Idx(255),
            Color::Rgb(0, 0, 0),
            Color::Rgb(1, 2, 3),
            Color::Rgb(255, 255, 255),
        ];
        let blinks = [Blink::None, Blink::Slow, Blink::Rapid];
        let underlines = [
            UnderlineStyle::None,
            UnderlineStyle::Single,
            UnderlineStyle::Double,
            UnderlineStyle::Curly,
            UnderlineStyle::Dotted,
            UnderlineStyle::Dashed,
        ];
        let mut numbers = std::collections::HashSet::new();
        for (fg, bg) in colours
            .iter()
            .flat_map(|f| colours.iter().map(move |b| (*f, *b)))
        {
            for bits in 0..64u8 {
                for blink in blinks {
                    for underline in underlines {
                        let on = |bit: u8| bits & (1 << bit) != 0;
                        let a = Attributes::new(fg, bg)
                            .with_bold(on(0))
                            .with_dim(on(1))
                            .with_italic(on(2))
                            .with_inverse(on(3))
                            .with_hidden(on(4))
                            .with_strikeout(on(5))
                            .with_blink(blink)
                            .with_underline_style(underline);
                        let direct = |c: Color| matches!(c, Color::Rgb(..));
                        let own = if direct(fg) || direct(bg) {
                            direct(fg)
                                && bg == Color::Default
                                && underline == UnderlineStyle::None
                                && blink == Blink::None
                                && bits & 0b11_1010 == 0
                        } else {
                            matches!(underline, UnderlineStyle::None | UnderlineStyle::Single)
                                && blink != Blink::Rapid
                        };
                        let code = a.inline_style();
                        assert_eq!(code.is_some(), own, "{a:?}");
                        if let Some(code) = code {
                            assert!(code < TABLE, "{a:?}");
                            assert_eq!(styles.get(code), a, "{a:?}");
                            assert!(numbers.insert(code), "{a:?}");
                        }
                        // With an underline colour or a direct background,
                        // none.
                        assert_eq!(a.with_underline_color(Color::Idx(1)).inline_style(), None);
                        assert_eq!(a.with_background(Color::Rgb(0, 0, 0)).inline_style(), None);
                    }
                }
            }
        }
    }

    #[test]
    fn a_sweep_is_wanted_past_twice_the_styles_in_use_and_the_floor() {
        let mut styles = Styles::default();
        let mut n = 0u32;
        while !styles.wants_sweep(0) {
            n = n.wrapping_add(1);
            let [_, r, g, b] = n.to_be_bytes();
            let attributes = Attributes::new(Color::Rgb(r, g, b), Color::Default);
            assert!(styles.insert(attributes).is_some());
        }
        assert_eq!(styles.len(), FLOOR);
        // A bigger grid waits for more.
        assert!(!styles.wants_sweep(FLOOR * 16));
        let used = vec![true; styles.len()];
        styles.retain(&used);
        assert!(!styles.wants_sweep(0));
    }
}
