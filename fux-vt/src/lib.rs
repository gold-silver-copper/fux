//! Bounded terminal emulation with immutable history windows and stable row IDs.
//! See the crate README for the sequence and resource contracts.

pub mod bytes;
mod cell;
mod compact;
mod grid;
mod history;
pub mod keys;
mod link;
mod mode;
mod palette;
mod parser;
mod screen;
mod style;
#[cfg(test)]
mod test_rng;
mod unicode;

pub use cell::{Attributes, Blink, CLUSTER_CAPACITY, CellRef, Cells, Color, UnderlineStyle};
pub use link::{Hyperlink, ID_LIMIT, URI_LIMIT};
pub use mode::Mode;
pub use parser::{
    Event, Feature, Identity, OSC_PAYLOAD_LIMIT, Options, Params, Parser, Sink, Unhandled,
};
pub use screen::{MouseProtocolEncoding, MouseProtocolMode, Screen};
pub use unicode::{UNICODE_VERSION, continues_cluster};

/// `slice::copy_from_slice`, checked: copies `src` over `dst`, if they are the
/// same length.
///
/// `copy_from_slice` panics when the lengths differ, so fux-vt calls it here,
/// after this check, and nowhere else (fux's clippy.toml).
pub(crate) fn copy_from<T: Copy>(dst: &mut [T], src: &[T]) -> Option<()> {
    (dst.len() == src.len()).then(|| dst.copy_from_slice(src))
}

/// A reply to a query, built where it is kept rather than on the heap: the
/// longest fux-vt makes, XTVERSION's `DCS > | name version ST` with
/// [`Identity::MAX_LEN`] bytes of name and version, is 55 bytes.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Reply {
    bytes: [u8; 64],
    len: usize,
}

impl Default for Reply {
    fn default() -> Self {
        Self {
            bytes: [0; 64],
            len: 0,
        }
    }
}

impl Reply {
    /// The reply `args` write.
    pub(crate) fn of(args: std::fmt::Arguments<'_>) -> Reply {
        let mut reply = Reply::default();
        // Every reply fits, so the write cannot run out of room.
        let _ = std::fmt::Write::write_fmt(&mut reply, args);
        reply
    }
    pub(crate) fn as_bytes(&self) -> &[u8] {
        self.bytes.get(..self.len).unwrap_or_default()
    }
}

impl std::fmt::Write for Reply {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        let end = self.len.checked_add(text.len()).ok_or(std::fmt::Error)?;
        let room = self.bytes.get_mut(self.len..end).ok_or(std::fmt::Error)?;
        copy_from(room, text.as_bytes()).ok_or(std::fmt::Error)?;
        self.len = end;
        Ok(())
    }
}

/// A row identity, never recycled within one parser's lifetime.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RowId(pub(crate) u64);

/// Non-destructive observation mark. Only use with the screen that produced it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mark(pub(crate) u64);

/// A retained row. Attributes and text are immutable through this view.
#[derive(Clone, Copy, Debug)]
pub struct Row<'a> {
    pub(crate) id: RowId,
    pub(crate) version: u64,
    pub(crate) wrapped: bool,
    /// Whether a prompt starts on the row (OSC 133 ; A).
    pub(crate) prompt: bool,
    /// The cells the row keeps: all of them, or those before its blank
    /// tail, a history row's; those past them, to `width`, are blank.
    pub(crate) cells: &'a [compact::Compact],
    pub(crate) width: usize,
    pub(crate) text: &'a compact::Text,
    /// Each cell's link, if any cell of the row has had one (`link.rs`).
    pub(crate) links: Option<&'a [u16]>,
    pub(crate) table: &'a link::Links,
    /// The attributes of the cells' styles.
    pub(crate) styles: &'a style::Styles,
}

impl<'a> Row<'a> {
    /// The row's identity, which it keeps as long as it is retained, edits
    /// and scrolls included, and which no later row takes.
    pub fn id(&self) -> RowId {
        self.id
    }
    /// The row's version: it changes with every edit that changes the
    /// row's cells, its soft wrap or its cells' links, and with every
    /// resize, which lays the row out anew. While it is the same, the row
    /// shows the same.
    pub fn version(&self) -> u64 {
        self.version
    }
    /// Whether the row is soft-wrapped: its line goes on in the next row.
    pub fn wrapped(&self) -> bool {
        self.wrapped
    }
    /// How many cells the row has: a history row keeps the width it had.
    pub fn len(&self) -> usize {
        self.width
    }
    /// Whether the row has no cells.
    pub fn is_empty(&self) -> bool {
        self.width == 0
    }
    /// The stored cell at column `col`, blank past those kept; `None` past
    /// the row's width.
    pub(crate) fn stored(&self, col: usize) -> Option<&'a compact::Compact> {
        if col >= self.width {
            return None;
        }
        Some(self.cells.get(col).unwrap_or(&compact::BLANK))
    }
    /// The row's stored cells, left to right, blank past those kept.
    pub(crate) fn padded(
        &self,
    ) -> impl DoubleEndedIterator<Item = &'a compact::Compact> + ExactSizeIterator + Clone + use<'a>
    {
        let cells = self.cells;
        (0..self.width).map(move |col| cells.get(col).unwrap_or(&compact::BLANK))
    }
    /// The cell at column `col`.
    pub fn cell(&self, col: usize) -> Option<CellRef<'a>> {
        let (text, styles) = (self.text, self.styles);
        self.stored(col).map(|cell| cell.read(text, styles))
    }
    /// The hyperlink (OSC 8) of the cell at column `col`: the link that was
    /// open when its glyph was printed, if one was. A blank cell has none;
    /// the second half of a wide glyph has its first half's.
    pub fn link(&self, col: usize) -> Option<Hyperlink<'a>> {
        let links = self.links?;
        let col = if self.stored(col)?.is_wide_continuation() {
            col.checked_sub(1)?
        } else {
            col
        };
        if !self.stored(col)?.has_contents() {
            return None;
        }
        self.table.get(*links.get(col)?)
    }
    /// Whether any cell of the row may have a hyperlink: `false` means
    /// [`Row::link`] is `None` for every cell, and need not be asked.
    pub fn has_links(&self) -> bool {
        self.links.is_some()
    }
    /// Whether a prompt starts on the row: a shell marked it, with
    /// `OSC 133 ; A` while the cursor was on it. The mark goes with the row
    /// as it scrolls and reflows; ED, erasing the row whole, removes it.
    pub fn starts_prompt(&self) -> bool {
        self.prompt
    }
    /// Bytes of text the row keeps for clusters over 17 bytes, overwritten
    /// ones included until the row is compacted: at most
    /// [`Cells::text_limit`] of its length. Shorter clusters held off the
    /// cells are not counted. For memory diagnostics.
    pub fn text_len(&self) -> usize {
        self.text.long_len()
    }
    /// The row's cells, left to right.
    pub fn cells(
        &self,
    ) -> impl DoubleEndedIterator<Item = CellRef<'a>> + ExactSizeIterator + Clone + use<'a> {
        let (text, styles) = (self.text, self.styles);
        // Cells side by side mostly share a style: its attributes are found
        // once for a run of them. Style 0 is the default attributes.
        let mut last = (0, Attributes::default());
        self.padded().map(move |cell| {
            let style = cell.style();
            if style != last.0 {
                last = (style, styles.get(style));
            }
            cell.read_as(text, last.1)
        })
    }
}

/// Input/resource errors. Failing size changes leave the old screen intact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// A size with no rows or no columns.
    ZeroSize,
    /// More rows or cells than a grid may hold, or an allocation that failed.
    Capacity,
    /// Row identities or versions ran out: they are never reused.
    IdentityExhausted,
    /// A copy longer than its cell or byte limit.
    CopyLimit,
    /// A copy endpoint outside the window.
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

/// An immutable clipped window. A cell past its row's width (a narrower
/// history row's) reads as `None`, and so does a wide glyph the window's
/// last column clips, never half a glyph; blank cells within a row read as
/// blanks. Offsets are clamped.
#[derive(Clone, Copy)]
pub struct Window<'a> {
    grid: &'a grid::Grid,
    start: usize,
    pub(crate) rows: u16,
    pub(crate) cols: u16,
    pub(crate) offset: usize,
}
impl<'a> Window<'a> {
    /// How many rows the window has: at most the screen's.
    pub fn rows(&self) -> u16 {
        self.rows
    }
    /// How many columns the window has: at most the screen's.
    pub fn cols(&self) -> u16 {
        self.cols
    }
    /// How many rows up into history the window starts, clamped to the
    /// history there is.
    pub fn offset(&self) -> usize {
        self.offset
    }
    /// Row `row` of the window, as it is retained: a history row keeps its
    /// own width.
    pub fn row(&self, row: u16) -> Option<Row<'a>> {
        if row >= self.rows {
            return None;
        }
        self.grid.row_at(self.start.checked_add(usize::from(row))?)
    }
    /// The cell at `row`, `col` of the window; `None` past its edges, and for
    /// a wide glyph whose second half the window's last column cuts off.
    pub fn cell(&self, row: u16, col: u16) -> Option<CellRef<'a>> {
        if col >= self.cols {
            return None;
        }
        let cell = self.row(row)?.cell(usize::from(col))?;
        // A wide glyph in the window's last column is clipped.
        if cell.is_wide() && col.checked_add(1).is_none_or(|next| next >= self.cols) {
            None
        } else {
            Some(cell)
        }
    }
    /// Whether the cell is the blank a reflow left at the end of a
    /// soft-wrapped row when the wide glyph after it did not fit: no part of
    /// the text.
    fn spacer(&self, row: u16, col: u16) -> bool {
        col.checked_add(1) == Some(self.cols)
            && self.row_wrapped(row)
            && self
                .cell(row, col)
                .is_some_and(|c| !c.has_contents() && c.attributes() == Attributes::default())
            && row
                .checked_add(1)
                .and_then(|next| self.cell(next, 0))
                .is_some_and(|c| c.is_wide())
    }
    /// Whether row `row` of the window is soft-wrapped, its line going on in
    /// the next row. Only a window as wide as the screen says so.
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
            let x = if self.cell(y, x).is_some_and(|c| c.is_wide_continuation()) {
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
                if cell.is_wide_continuation() || self.spacer(y, x) {
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

#[cfg(test)]
mod tests {
    use std::fmt::Write;

    #[test]
    fn a_reply_holds_what_it_can_and_builds_in_pieces() {
        let (row, col) = (u16::MAX, u16::MAX);
        let reply = super::Reply::of(format_args!("\x1b[?{row};{col}R"));
        assert_eq!(reply.as_bytes(), b"\x1b[?65535;65535R");
        let mut built = super::Reply::default();
        assert!(built.write_str("\x1b[0").is_ok() && built.write_str("n").is_ok());
        assert_eq!(built.as_bytes(), b"\x1b[0n");
        // More than it holds is refused, and what it held stays.
        let long: String = std::iter::repeat_n('x', 70).collect();
        assert!(built.write_str(&long).is_err());
        assert_eq!(built.as_bytes(), b"\x1b[0n");
    }

    #[test]
    fn copies_happen_only_between_slices_of_one_length() {
        let mut cells = [0u8; 4];
        assert_eq!(super::copy_from(&mut cells, &[1, 2, 3, 4]), Some(()));
        assert_eq!(cells, [1, 2, 3, 4]);
        assert_eq!(super::copy_from(&mut cells, &[9, 9]), None);
        assert_eq!(super::copy_from(&mut cells[..2], &[9, 9, 9]), None);
        assert_eq!(cells, [1, 2, 3, 4]);
    }
}
