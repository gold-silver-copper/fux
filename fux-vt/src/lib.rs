//! Bounded terminal emulation with immutable history windows and stable row IDs.
//! See the crate README for the sequence and resource contracts.

mod cell;
mod grid;
mod parser;
mod screen;

pub use cell::{Attributes, Cell, Color};
pub use parser::{Event, OSC_PAYLOAD_LIMIT, Options, Parser, Sink};
pub use screen::{MouseProtocolEncoding, MouseProtocolMode, Screen};

/// A row identity, never recycled within one parser's lifetime.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RowId(pub(crate) u64);

/// Non-destructive observation mark. Only use with the screen that produced it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mark(pub(crate) u64);

/// A retained row. Attributes and text are immutable through this view.
#[derive(Clone, Copy, Debug)]
pub struct Row<'a> {
    pub id: RowId,
    pub version: u64,
    pub wrapped: bool,
    pub cells: &'a [Cell],
}

/// Input/resource errors. Failing size changes leave the old screen intact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    ZeroSize,
    Capacity,
    IdentityExhausted,
    CopyLimit,
    InvalidRange,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::ZeroSize => "terminal dimensions must be nonzero",
            Self::Capacity => "terminal allocation limit exceeded",
            Self::IdentityExhausted => "terminal identity/version space exhausted",
            Self::CopyLimit => "copy cell or byte limit exceeded",
            Self::InvalidRange => "copy range is outside the visible window",
        })
    }
}
impl std::error::Error for Error {}

/// An immutable clipped window. Blank padding is represented by `None`; a
/// clipped wide leader is also blank, never a half glyph. Offsets are clamped.
#[derive(Clone, Copy)]
pub struct Window<'a> {
    grid: &'a grid::Grid,
    start: usize,
    pub rows: u16,
    pub cols: u16,
    pub offset: usize,
}
impl<'a> Window<'a> {
    pub fn row(&self, row: u16) -> Option<Row<'a>> {
        if row >= self.rows {
            return None;
        }
        self.grid.row_at(self.start.checked_add(usize::from(row))?)
    }
    pub fn cell(&self, row: u16, col: u16) -> Option<&'a Cell> {
        if col >= self.cols {
            return None;
        }
        let cell = self.row(row)?.cells.get(usize::from(col))?;
        // A wide glyph in the window's last column is clipped.
        if cell.is_wide() && col.checked_add(1).is_none_or(|next| next >= self.cols) {
            None
        } else {
            Some(cell)
        }
    }
    pub fn row_wrapped(&self, row: u16) -> bool {
        self.cols == self.grid.cols.get() && self.row(row).is_some_and(|r| r.wrapped)
    }
    /// Inclusive endpoints, normalized to wide leaders. Limits are checked
    /// before allocation and before every append; an oversized copy is refused.
    pub fn text(
        &self,
        a: (u16, u16),
        b: (u16, u16),
        max_cells: usize,
        max_bytes: usize,
    ) -> Result<String, Error> {
        let point = |(y, x): (u16, u16)| -> Result<(u16, u16), Error> {
            if y >= self.rows || x >= self.cols {
                return Err(Error::InvalidRange);
            }
            let x = if self.cell(y, x).is_some_and(Cell::is_wide_continuation) {
                x.saturating_sub(1)
            } else {
                x
            };
            Ok((y, x))
        };
        let (a, b) = (point(a)?, point(b)?);
        let (start, end) = if a <= b { (a, b) } else { (b, a) };
        // Both points are in the window, so it has a last column.
        let last = self.cols.checked_sub(1).ok_or(Error::InvalidRange)?;
        let count = (start.0..=end.0)
            .len()
            .checked_mul(usize::from(self.cols))
            .ok_or(Error::CopyLimit)?;
        if count > max_cells {
            return Err(Error::CopyLimit);
        }
        let mut output = String::new();
        for y in start.0..=end.0 {
            let left = if y == start.0 { start.1 } else { 0 };
            let right = if y == end.0 { end.1 } else { last };
            let line_start = output.len();
            for x in left..=right {
                // Clipped wide glyphs and padding beyond a historical row's
                // original extent are display blanks, not stored copy text.
                // In particular, padding must not enter a soft-wrapped join.
                let Some(cell) = self.cell(y, x) else {
                    continue;
                };
                if cell.is_wide_continuation() {
                    continue;
                }
                let text = if cell.has_contents() {
                    cell.contents()
                } else {
                    " "
                };
                if output
                    .len()
                    .checked_add(text.len())
                    .is_none_or(|n| n > max_bytes)
                {
                    return Err(Error::CopyLimit);
                }
                output.push_str(text);
            }
            if y == end.0 || !self.row_wrapped(y) {
                while output.len() > line_start && output.ends_with(' ') {
                    output.pop();
                }
                if y != end.0 {
                    if output.len() == max_bytes {
                        return Err(Error::CopyLimit);
                    }
                    output.push('\n');
                }
            }
        }
        Ok(output)
    }
}
