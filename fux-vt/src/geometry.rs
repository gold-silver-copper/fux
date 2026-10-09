//! A screen's size, and the runs of its rows or columns its margins bound.
use std::num::{NonZeroU16, TryFromIntError};

/// A screen's size: how many rows and columns it has, never none of either,
/// so a screen always has a last row and a last column.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Size {
    rows: NonZeroU16,
    cols: NonZeroU16,
}

impl Size {
    /// `rows` by `cols`; an error if either is zero.
    pub fn new(rows: u16, cols: u16) -> Result<Self, TryFromIntError> {
        Ok(Self::from((rows.try_into()?, cols.try_into()?)))
    }
    /// How many rows.
    pub fn rows(self) -> u16 {
        self.rows.get()
    }
    /// How many columns.
    pub fn cols(self) -> u16 {
        self.cols.get()
    }
    /// How many cells: two u16s multiply within a usize.
    pub(crate) fn cells(self) -> usize {
        usize::from(self.rows()).saturating_mul(usize::from(self.cols()))
    }
    /// Every row.
    pub(crate) fn lines(self) -> Span {
        Span::whole(self.rows)
    }
    /// Every column.
    pub(crate) fn columns(self) -> Span {
        Span::whole(self.cols)
    }
}

impl From<(NonZeroU16, NonZeroU16)> for Size {
    fn from((rows, cols): (NonZeroU16, NonZeroU16)) -> Self {
        Self { rows, cols }
    }
}

impl From<Size> for (u16, u16) {
    fn from(size: Size) -> Self {
        (size.rows(), size.cols())
    }
}

/// Rows, or columns, `first` to `last` of a screen, both included: a pair
/// of margins, or the screen's edges. `first` is never past `last`, nor
/// `last` off the screen the span was made for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Span {
    first: u16,
    last: u16,
}

impl Span {
    fn whole(extent: NonZeroU16) -> Self {
        // Exact: an extent is at least one.
        let last = extent.get().saturating_sub(1);
        Self { first: 0, last }
    }
    /// Margins within this span, the screen's, as DECSTBM and DECSLRM set
    /// them: `last` past it is its last; `None` unless `first` is before it.
    pub fn margins(self, first: u16, last: u16) -> Option<Self> {
        let last = last.min(self.last);
        (first < last).then_some(Self { first, last })
    }
    /// The span from `first` on, if it is in this one.
    pub fn starting_at(self, first: u16) -> Option<Self> {
        self.contains(first).then_some(Self { first, ..self })
    }
    pub fn first(self) -> u16 {
        self.first
    }
    pub fn last(self) -> u16 {
        self.last
    }
    /// One past the last: exact, as the last is below a u16 extent.
    pub fn end(self) -> u16 {
        self.last.saturating_add(1)
    }
    /// How many rows or columns: at least one.
    pub fn len(self) -> u16 {
        self.end().saturating_sub(self.first)
    }
    pub fn contains(self, n: u16) -> bool {
        (self.first..=self.last).contains(&n)
    }
    /// Its `n`th row or column, from 0, as origin mode addresses them; its
    /// last past it.
    pub fn nth(self, n: u16) -> u16 {
        self.first.saturating_add(n).min(self.last)
    }
}

#[cfg(test)]
impl Size {
    /// `rows` by `cols`, for a test, which never asks for none.
    pub(crate) fn of(rows: u16, cols: u16) -> Self {
        let at_least_one = |n| NonZeroU16::new(n).unwrap_or(NonZeroU16::MIN);
        Self::from((at_least_one(rows), at_least_one(cols)))
    }
}

#[cfg(test)]
impl Span {
    /// `first` to `last`, for a test, which never asks for one off the screen.
    pub(crate) fn of(first: u16, last: u16) -> Self {
        let last = last.max(first);
        Self { first, last }
    }
}
