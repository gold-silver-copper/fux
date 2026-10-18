//! Bounded terminal emulation with immutable history windows and stable row IDs.
//! See the crate README for the sequence and resource contracts.

/// Expands `/// doc, #[derive(..)] pub enum Ty { /// doc, Variant, ... }` to
/// that enum and `Ty::ALL`, every variant in the order declared. Given a
/// function first, `/// doc, fn name(self) -> T;`, and a value on each row,
/// `Variant => value,`, it writes that function too, each variant's value:
/// `pub enum Direction { fn name(self) -> &'static str; Left => "left", ... }`
/// gives `Direction::ALL` and `Direction::name`.
macro_rules! closed {
    (
        $(#[$meta:meta])*
        $vis:vis enum $ty:ident {
            $(#[$fdoc:meta])*
            $fvis:vis fn $f:ident(self) -> $ret:ty;
            $($(#[$doc:meta])* $variant:ident => $value:expr,)*
        }
    ) => {
        closed! {
            $(#[$meta])*
            $vis enum $ty {
                $($(#[$doc])* $variant,)*
            }
        }

        impl $ty {
            $(#[$fdoc])*
            $fvis fn $f(self) -> $ret {
                match self {
                    $($ty::$variant => $value,)*
                }
            }
        }
    };
    (
        $(#[$meta:meta])*
        $vis:vis enum $ty:ident {
            $($(#[$doc:meta])* $variant:ident,)*
        }
    ) => {
        $(#[$meta])*
        $vis enum $ty {
            $($(#[$doc])* $variant,)*
        }

        impl $ty {
            /// Every variant, in the order declared.
            pub const ALL: [$ty; [$(stringify!($variant)),*].len()] = [$($ty::$variant),*];
        }
    };
}

/// The [`Reply`] `Reply::of` writes, in its own buffer, from arguments as
/// `format!` takes them: `reply!("\x1b[{row};{col}R")` expands to
/// `$crate::Reply::of(format_args!("\x1b[{row};{col}R"))`.
macro_rules! reply {
    ($($arg:tt)*) => {
        $crate::Reply::of(format_args!($($arg)*))
    };
}

/// Counts `n` units of work of the kind `counter` names (threats.md 4.1),
/// with the `checks` feature; without it, nothing at all, so a build without
/// it carries no trace of the count: `work!(cells, n)` expands to
/// `#[cfg(feature = "checks")] crate::work::add(|w| w.cells(crate::work::units(n)));`.
macro_rules! work {
    ($counter:ident, $n:expr) => {
        #[cfg(feature = "checks")]
        $crate::work::add(|w| w.$counter($crate::work::units($n)));
    };
}

pub mod bytes;
mod cell;
#[cfg(feature = "checks")]
mod check;
mod compact;
mod geometry;
mod grid;
mod history;
pub mod keys;
mod link;
mod mode;
mod palette;
mod parser;
mod screen;
pub mod sixel;
mod style;
#[cfg(test)]
mod test_rng;
mod unicode;
#[cfg(feature = "checks")]
mod work;

use std::{fmt, ops::Range};

pub use cell::{
    Attributes, Blink, CLUSTER_CAPACITY, CellRef, Cells, Color, Rgb, SgrForm, UnderlineStyle,
};
#[cfg(feature = "checks")]
pub use check::{Fault, Report};
pub use geometry::Size;
pub use link::{Hyperlink, ID_LIMIT, URI_LIMIT};
pub use mode::{Mode, ModeStatus};
pub use parser::{
    Event, Feature, Frame, FrameScan, Identity, OSC_PAYLOAD_LIMIT, Options, Params, Parser, Sink,
    Unhandled,
};
pub use screen::{
    CommandMark, Commands, CursorShape, Graphics, MouseProtocolEncoding, MouseProtocolMode,
    PICTURE_BYTES, PLACEHOLDER, Report, Reported, Screen, Shown,
};
pub use unicode::{UNICODE_VERSION, continues_cluster};
#[cfg(feature = "checks")]
pub use work::Work;

/// `slice::copy_from_slice`, checked: copies `src` over `dst`, if they are the
/// same length.
///
/// `copy_from_slice` panics when the lengths differ, so fux-vt calls it here,
/// after this check, and nowhere else (fux's clippy.toml).
pub(crate) fn copy_from<T: Copy>(dst: &mut [T], src: &[T]) -> Option<()> {
    (dst.len() == src.len()).then(|| dst.copy_from_slice(src))
}

/// `bytes` split at their first `;`, which neither half keeps.
pub(crate) fn split_semicolon(bytes: &[u8]) -> Option<(&[u8], &[u8])> {
    let mut halves = bytes.splitn(2, |b| *b == b';');
    Some((halves.next()?, halves.next()?))
}

/// Makes room in `v` for `len` items in all, or says there is none.
pub(crate) fn reserve<T>(v: &mut Vec<T>, len: usize) -> Result<(), Error> {
    v.try_reserve_exact(len.saturating_sub(v.len()))
        .map_err(|_| Error::Capacity)
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
    pub(crate) fn of(args: fmt::Arguments<'_>) -> Self {
        let mut reply = Self::default();
        // Every reply fits, so the write cannot run out of room.
        let _ = fmt::Write::write_fmt(&mut reply, args);
        reply
    }
    pub(crate) fn as_bytes(&self) -> &[u8] {
        self.bytes.get(..self.len).unwrap_or_default()
    }
    /// Adds `bytes`, if they fit; every reply does.
    pub(crate) fn push(&mut self, bytes: &[u8]) -> Option<()> {
        let end = self.len.checked_add(bytes.len())?;
        copy_from(self.bytes.get_mut(self.len..end)?, bytes)?;
        self.len = end;
        Some(())
    }
}

impl fmt::Write for Reply {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        self.push(text.as_bytes()).ok_or(fmt::Error)
    }
}

/// A row identity, never recycled within one parser's lifetime.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RowId(pub(crate) u64);

/// Non-destructive observation mark. Only use with the screen that produced it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mark(pub(crate) u64);

/// A retained row, and its place among the rows retained: history's,
/// oldest first, then the screen's from its top. It borrows the screen, so
/// no row is held past a change that could move it; to find a row again
/// later, keep its [`RowId`]. Attributes and text are immutable through
/// this view.
#[derive(Clone, Copy)]
pub struct Row<'a> {
    pub(crate) grid: &'a grid::Grid,
    pub(crate) index: usize,
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
    pub(crate) links: Option<&'a link::RowLinks>,
}

impl fmt::Debug for Row<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Row")
            .field("index", &self.index)
            .field("id", &self.id)
            .field("version", &self.version)
            .field("wrapped", &self.wrapped)
            .finish_non_exhaustive()
    }
}

impl<'a> Row<'a> {
    /// Where the row is among those retained: 0 is history's oldest, and
    /// the screen's top row is [`Screen::history_len`].
    pub fn index(&self) -> usize {
        self.index
    }
    /// The row above it, older; none above the oldest.
    pub fn above(&self) -> Option<Self> {
        self.grid.row_at(self.index.checked_sub(1)?)
    }
    /// The row below it, newer; none below the screen's last.
    pub fn below(&self) -> Option<Self> {
        self.grid.row_at(self.index.checked_add(1)?)
    }
    /// The row `n` above it, or the oldest.
    pub fn up(&self, n: usize) -> Self {
        self.grid
            .row_at(self.index.saturating_sub(n))
            .unwrap_or(*self)
    }
    /// The row `n` below it, or the screen's last.
    pub fn down(&self, n: usize) -> Self {
        let last = self.grid.retained_len().saturating_sub(1);
        self.grid
            .row_at(self.index.saturating_add(n).min(last))
            .unwrap_or(*self)
    }
    /// The window with the row at its top; for a row of the screen below
    /// its top, where no window of the screen's height starts, the screen.
    pub fn window(&self) -> Window<'a> {
        Window {
            grid: self.grid,
            start: self.index.min(self.grid.history_len()),
        }
    }
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
    /// Whether the row changed since `mark` was taken: an edit, or a resize.
    pub fn changed_since(&self, mark: Mark) -> bool {
        self.version > mark.0
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
    /// Whether the row, soft-wrapped, ends in a spacer: the blank in its
    /// own last column that a wide glyph starting the next row did not fit
    /// in, no part of the text (reflow and copies skip it). A blank erased
    /// in a colour is a cell painted, and no spacer.
    pub fn ends_in_spacer(&self) -> bool {
        self.wrapped
            && (self.width.checked_sub(1))
                .and_then(|col| self.stored(col))
                .is_some_and(|c| c.is_blank(0))
            && (self.below())
                .and_then(|next| next.stored(0))
                .is_some_and(compact::Compact::is_wide)
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
        let (text, styles) = (self.text, self.grid.styles());
        self.stored(col).map(|cell| cell.read(text, styles))
    }
    /// The hyperlink (OSC 8) of the cell at column `col`: the link that was
    /// open when its glyph was printed, if one was. A blank cell has none;
    /// the second half of a wide glyph has its first half's.
    pub fn link(&self, col: usize) -> Option<&'a Hyperlink> {
        let links = self.links?;
        let col = if self.stored(col)?.is_wide_continuation() {
            col.checked_sub(1)?
        } else {
            col
        };
        if !self.stored(col)?.has_contents() {
            return None;
        }
        links.get(col)
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
        let (text, styles) = (self.text, self.grid.styles());
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
    /// The glyphs of columns `from` through `to` as a copy takes them, each
    /// with its column: a wide glyph whole, from either half; a blank cell,
    /// or one a picture covers, as a space (`CellRef::shown`); and the
    /// spacer the row may end in (`ends_in_spacer`) left out.
    pub fn copied(
        &self,
        from: usize,
        to: usize,
    ) -> impl Iterator<Item = (usize, &'a str)> + use<'a> {
        let from = match self.stored(from) {
            Some(cell) if cell.is_wide_continuation() => from.saturating_sub(1),
            Some(_) | None => from,
        };
        let spacer = usize::from(self.ends_in_spacer());
        let end = self.width.saturating_sub(spacer).min(to.saturating_add(1));
        let glyphs = self.cells().enumerate().take(end).skip(from);
        glyphs
            .filter(|(_, cell)| !cell.is_wide_continuation())
            .map(|(col, cell)| (col, cell.shown()))
    }
    /// The row's text, a cell at a time: a blank cell, or one holding a
    /// placeholder, as a space, the second half of a wide glyph left out,
    /// and the blanks at its end kept.
    pub fn cells_text(&self) -> String {
        (self.cells().filter(|c| !c.is_wide_continuation()))
            .map(|c| c.shown())
            .collect()
    }
}

/// What was printed while the host records ([`Parser::record_prints`]):
/// each run of text that changed cells, in the order printed, though later
/// drawn over, its texts end to end. A row moved within the screen, its
/// text as it was (lines deleted or put in above it, a region scrolled),
/// is a run with no text and no columns: the topmost of those moved.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Prints {
    runs: Vec<(RowId, Range<u16>, Range<usize>)>,
    text: String,
}

/// A run of [`Prints`]: the row it was printed on, the columns it took,
/// and its text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Print<'a> {
    /// The row it was printed on.
    pub row: RowId,
    /// The columns it took.
    pub cols: (u16, u16),
    /// What was printed there.
    pub text: &'a str,
}

impl Prints {
    /// The runs, in the order printed.
    pub fn iter(&self) -> impl Iterator<Item = Print<'_>> {
        self.runs.iter().map(|(row, cols, text)| Print {
            row: *row,
            cols: (cols.start, cols.end),
            text: self.text.get(text.clone()).unwrap_or_default(),
        })
    }
    /// The bytes it takes on the heap: its text and its runs.
    pub fn heap(&self) -> usize {
        let run = size_of::<(RowId, Range<u16>, Range<usize>)>();
        (self.text.capacity()).saturating_add(self.runs.capacity().saturating_mul(run))
    }
    /// Lets go of the room it grew into and does not use: for one kept.
    pub fn shrink_to_fit(&mut self) {
        self.text.shrink_to_fit();
        self.runs.shrink_to_fit();
    }
    /// Whether nothing was printed.
    pub fn is_empty(&self) -> bool {
        self.runs.is_empty()
    }
    /// Adds `text` printed at `cols` of `row`: to the last run if it goes
    /// on where that left off, on its row.
    pub(crate) fn push(&mut self, row: RowId, cols: Range<u16>, text: &str) {
        work!(prints, text.len());
        let start = self.text.len();
        self.text.push_str(text);
        let end = self.text.len();
        match self.runs.last_mut() {
            Some((last, at, bytes)) if *last == row && at.end == cols.start && !cols.is_empty() => {
                at.end = cols.end;
                bytes.end = end;
            }
            _ => self.runs.push((row, cols, start..end)),
        }
    }
}

/// `Prints`' part of the invariant checker (threats.md section 6).
#[cfg(feature = "checks")]
impl Prints {
    /// 34: each run's text lies inside the text, the runs end to end in
    /// the order printed, and each run's columns are in order.
    pub(crate) fn check(&self) -> Result<(), String> {
        let mut end = 0;
        for (row, cols, text) in &self.runs {
            check::ensure(text.start == end && text.start <= text.end, || {
                format!("a run of row {row:?} has text {text:?} after {end}")
            })?;
            check::ensure(cols.start <= cols.end, || {
                format!("a run of row {row:?} has columns {cols:?}")
            })?;
            end = text.end;
        }
        check::ensure(end == self.text.len(), || {
            format!("the runs end at {end} in {} bytes of text", self.text.len())
        })
    }
}

/// Input/resource errors. Failing size changes leave the old screen intact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// More rows or cells than a grid may hold, or an allocation that failed.
    Capacity,
    /// A copy longer than its cell or byte limit.
    CopyLimit,
    /// A copy endpoint outside the window.
    InvalidRange,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Capacity => "terminal allocation limit exceeded",
            Self::CopyLimit => "copy cell or byte limit exceeded",
            Self::InvalidRange => "copy range is outside the visible window",
        })
    }
}
impl std::error::Error for Error {}

/// The rows the screen retained, in order, as history's oldest first,
/// then the screen's (`Screen::rows`).
#[derive(Clone)]
pub struct Rows<'a> {
    pub(crate) grid: &'a grid::Grid,
    pub(crate) range: Range<usize>,
}
impl<'a> Rows<'a> {
    /// Every row `grid` retains, its history's oldest first.
    pub(crate) fn of(grid: &'a grid::Grid) -> Self {
        Self {
            grid,
            range: 0..grid.retained_len(),
        }
    }
}
impl<'a> Iterator for Rows<'a> {
    type Item = Row<'a>;
    fn next(&mut self) -> Option<Row<'a>> {
        self.grid.row_at(self.range.next()?)
    }
    fn nth(&mut self, n: usize) -> Option<Row<'a>> {
        self.grid.row_at(self.range.nth(n)?)
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.range.size_hint()
    }
}
impl<'a> DoubleEndedIterator for Rows<'a> {
    fn next_back(&mut self) -> Option<Row<'a>> {
        self.grid.row_at(self.range.next_back()?)
    }
    fn nth_back(&mut self, n: usize) -> Option<Row<'a>> {
        self.grid.row_at(self.range.nth_back(n)?)
    }
}
impl ExactSizeIterator for Rows<'_> {}

/// An immutable window of the screen's size: the screen
/// (`Screen::window`), or as many rows from a row of history
/// (`Row::window`). A cell past its row's width (a narrower history row's)
/// reads as `None`, and so does a wide glyph the window's last column
/// clips, never half a glyph; blank cells within a row read as blanks.
#[derive(Clone, Copy)]
pub struct Window<'a> {
    pub(crate) grid: &'a grid::Grid,
    /// The index of its top row: at most the screen's top's.
    pub(crate) start: usize,
}
impl<'a> Window<'a> {
    /// How many rows the window has: the screen's.
    pub fn rows(&self) -> u16 {
        self.grid.size().rows()
    }
    /// How many columns the window has: the screen's.
    pub fn cols(&self) -> u16 {
        self.grid.size().cols()
    }
    /// Row `row` of the window, as it is retained: a history row keeps its
    /// own width.
    pub fn row(&self, row: u16) -> Option<Row<'a>> {
        if row >= self.rows() {
            return None;
        }
        self.grid.row_at(self.start.checked_add(usize::from(row))?)
    }
    /// Where `row` is in the window, if it shows there.
    pub fn place(&self, row: &Row<'_>) -> Option<u16> {
        let y = u16::try_from(row.index.checked_sub(self.start)?).ok()?;
        (y < self.rows()).then_some(y)
    }
    /// The cell at `row`, `col` of the window; `None` past its edges, and for
    /// a wide glyph whose second half the window's last column cuts off.
    pub fn cell(&self, row: u16, col: u16) -> Option<CellRef<'a>> {
        if col >= self.cols() {
            return None;
        }
        let cell = self.row(row)?.cell(usize::from(col))?;
        // A wide glyph in the window's last column is clipped.
        if cell.is_wide() && col.checked_add(1).is_none_or(|next| next >= self.cols()) {
            None
        } else {
            Some(cell)
        }
    }
    /// Whether row `row` of the window is soft-wrapped, its line going on in
    /// the next row.
    pub fn row_wrapped(&self, row: u16) -> bool {
        self.row(row).is_some_and(|r| r.wrapped)
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
        if a.0.max(b.0) >= self.rows() || a.1.max(b.1) >= self.cols() {
            return Err(Error::InvalidRange);
        }
        let (start, end) = (a.min(b), a.max(b));
        // Exact: both points are in the window, so it has a last column.
        let last = self.cols().saturating_sub(1);
        let count = (start.0..=end.0)
            .len()
            .checked_mul(usize::from(self.cols()))
            .ok_or(Error::CopyLimit)?;
        if count > max_cells {
            return Err(Error::CopyLimit);
        }
        let mut output = String::new();
        for y in start.0..=end.0 {
            let left = if y == start.0 { start.1 } else { 0 };
            let right = if y == end.0 { end.1 } else { last };
            let line_start = output.len();
            let row = self.row(y).ok_or(Error::InvalidRange)?;
            // A wide glyph the window clips is a display blank, not stored
            // copy text, and so is padding past a history row's width: it
            // must not enter a soft-wrapped join.
            let glyphs = row.copied(usize::from(left), usize::from(right));
            let inside =
                |(x, _): &(usize, &str)| u16::try_from(*x).is_ok_and(|x| self.cell(y, x).is_some());
            for (_, text) in glyphs.filter(inside) {
                if output
                    .len()
                    .checked_add(text.len())
                    .is_none_or(|n| n > max_bytes)
                {
                    return Err(Error::CopyLimit);
                }
                output.push_str(text);
            }
            if y == end.0 || !row.wrapped {
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

/// Whether a screen of `size` can be made: the most cells one holds are
/// bounded.
pub fn fits(size: Size) -> bool {
    grid::Grid::check_size(size).is_ok()
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
