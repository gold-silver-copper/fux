//! Bounded terminal emulation with immutable history windows and stable row IDs.
//! See the crate README for the sequence and resource contracts.

/// Writes an enum and `Ty::ALL`, its variants in order. Given a leading
/// `fn name(self) -> T;` and `Variant => value,` rows, it writes that
/// function too.
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

/// A [`Reply`] built from `format!`-style arguments.
macro_rules! reply {
    ($($arg:tt)*) => {
        $crate::Reply::of(format_args!($($arg)*))
    };
}

/// Counts `n` units of work of the kind `counter` names (THREATS.md 4.1),
/// with the `checks` feature; without it, nothing at all.
macro_rules! work {
    ($counter:ident, $n:expr) => {
        #[cfg(feature = "checks")]
        $crate::work::add(|w| {
            w.$counter = w.$counter.saturating_add($crate::budget::units($n));
        });
    };
}

/// Counts work as `work!` does and spends it from the call's budget
/// (THREATS.md 4.4), with or without `checks`: for work one sequence can
/// multiply.
macro_rules! spend {
    ($counter:ident, $n:expr) => {{
        let n = $crate::budget::units($n);
        $crate::budget::spend(n, $crate::budget::weight::$counter);
        work!($counter, n);
    }};
}

mod budget;
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
pub use check::{Checked, Fault, refuse_next};
pub use geometry::Size;
pub use link::{Hyperlink, ID_LIMIT, URI_LIMIT};
pub use mode::{Mode, ModeStatus};
pub use parser::{
    Event, Feature, FrameScan, Identity, Kept, OSC_PAYLOAD_LIMIT, Options, Params, Parser, Sink,
    Stop, Unhandled,
};
pub use screen::{
    CellSize, CommandMark, Commands, CursorShape, Graphics, MouseProtocolEncoding,
    MouseProtocolMode, PICTURE_BYTES, PLACEHOLDER, Screen, Shown,
};
pub use unicode::{UNICODE_VERSION, char_width, continues_cluster, str_width};
#[cfg(feature = "checks")]
pub use work::Work;

/// `slice::copy_from_slice`, checked: it panics when the lengths differ, so
/// fux-vt calls it only here (fux's clippy.toml).
pub(crate) fn copy_from<T: Copy>(dst: &mut [T], src: &[T]) -> Option<()> {
    (dst.len() == src.len()).then(|| dst.copy_from_slice(src))
}

/// `bytes` split at their first `;`, which neither half keeps.
pub(crate) fn split_semicolon(bytes: &[u8]) -> Option<(&[u8], &[u8])> {
    let mut halves = bytes.splitn(2, |b| *b == b';');
    Some((halves.next()?, halves.next()?))
}

thread_local! {
    /// How many refusals of room this thread's calls have met and gone on
    /// past, each with its defined degrade (THREATS.md 1.3).
    static DEGRADED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Counts a refusal of room met by its degrade rather than an error, such
/// as a row let go instead of kept in history ([`Parser::refused`]).
#[cold]
pub(crate) fn degraded() {
    DEGRADED.with(|n| n.set(n.get().saturating_add(1)));
}

pub(crate) fn degrades() -> u64 {
    DEGRADED.with(std::cell::Cell::get)
}

/// Makes room in `v` for `len` items in all, or says there is none.
pub(crate) fn reserve<T>(v: &mut Vec<T>, len: usize) -> Result<(), Error> {
    #[cfg(feature = "checks")]
    if check::refusing() {
        return Err(Error::Capacity);
    }
    v.try_reserve_exact(len.saturating_sub(v.len()))
        .map_err(|_| Error::Capacity)
}

/// A reply to a query, kept off the heap: the longest, XTVERSION's with
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

/// A line's rendition (DEC STD 070, 5.6.1). A line that is not single width
/// is half as long: cells past half the screen's width are lost when it is
/// set, and the cursor stops short of them. The host draws each cell twice
/// as wide.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum LineRendition {
    /// DECSWL (`ESC # 5`), and every line until another is set.
    #[default]
    Single,
    /// DECDWL (`ESC # 6`).
    DoubleWidth,
    /// DECDHL's top half (`ESC # 3`).
    DoubleHeightTop,
    /// DECDHL's bottom half (`ESC # 4`).
    DoubleHeightBottom,
}

impl LineRendition {
    /// Whether the line is drawn twice as wide, and so half as long.
    pub fn is_double(self) -> bool {
        self != Self::Single
    }
    pub(crate) fn from_bits(bits: u8) -> Self {
        match bits & 3 {
            0 => Self::Single,
            1 => Self::DoubleWidth,
            2 => Self::DoubleHeightTop,
            _ => Self::DoubleHeightBottom,
        }
    }
    pub(crate) fn bits(self) -> u8 {
        self as u8
    }
}

/// A retained row, history's oldest first, then the screen's. It borrows
/// the screen, so no row is held past a change that could move it; to find
/// a row again later, keep its [`RowId`].
#[derive(Clone, Copy)]
pub struct Row<'a> {
    pub(crate) grid: &'a grid::Grid,
    pub(crate) index: usize,
    pub(crate) id: RowId,
    pub(crate) version: u64,
    /// `history::WRAPPED`, `PROMPT` and the rendition bits.
    pub(crate) flags: u8,
    /// The cells kept; those past them, to `width`, are blank.
    pub(crate) cells: &'a [compact::Compact],
    pub(crate) width: usize,
    pub(crate) text: &'a compact::Text,
    /// Each cell's link, if any cell of the row has had one (`link.rs`).
    pub(crate) links: Option<link::Runs<'a>>,
}

impl fmt::Debug for Row<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Row")
            .field("index", &self.index)
            .field("id", &self.id)
            .field("version", &self.version)
            .field("wrapped", &self.wrapped())
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
    /// The row's identity, kept through edits and scrolls, never reused.
    pub fn id(&self) -> RowId {
        self.id
    }
    /// The row's version: it changes with every edit to its cells, soft
    /// wrap or links, and with every resize.
    pub fn version(&self) -> u64 {
        self.version
    }
    /// Whether the row changed since `mark` was taken: an edit, or a resize.
    pub fn changed_since(&self, mark: Mark) -> bool {
        self.version > mark.0
    }
    /// Whether the row is soft-wrapped: its line goes on in the next row.
    pub fn wrapped(&self) -> bool {
        self.flags & history::WRAPPED != 0
    }
    /// Whether the row's line goes on in the row below: it is soft-wrapped
    /// and that row was not erased whole since, as bash does to redraw a
    /// wrapped prompt after a resize. xterm keeps the soft wrap through
    /// such an erase, as `wrapped` does; a host joining rows reads this.
    pub fn continues(&self) -> bool {
        self.flags & (history::WRAPPED | history::ENDS) == history::WRAPPED
    }
    /// The row's line rendition (DECDWL, DECDHL).
    pub fn rendition(&self) -> LineRendition {
        LineRendition::from_bits(self.flags >> history::RENDITION_SHIFT)
    }
    /// How many cells the row has: a history row keeps the width it had.
    pub fn len(&self) -> usize {
        self.width
    }
    /// Whether the row has no cells.
    pub fn is_empty(&self) -> bool {
        self.width == 0
    }
    /// Whether the row, soft-wrapped, ends in a spacer: the blank last
    /// column a wide glyph starting the next row did not fit in, which
    /// reflow and copies skip. A blank erased in a colour is no spacer.
    pub fn ends_in_spacer(&self) -> bool {
        self.wrapped()
            && (self.width.checked_sub(1))
                .and_then(|col| self.stored(col))
                .is_some_and(|c| c.is_blank(0))
            && (self.below())
                .and_then(|next| next.stored(0))
                .is_some_and(compact::Compact::is_wide)
    }
    /// The stored cell at column `col`, blank past those kept.
    pub(crate) fn stored(&self, col: usize) -> Option<&'a compact::Compact> {
        if col >= self.width {
            return None;
        }
        Some(self.cells.get(col).unwrap_or(&compact::BLANK))
    }
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
    /// The hyperlink (OSC 8) open when the glyph at `col` was printed. A
    /// blank cell has none; a wide glyph's second half has its first's.
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
    /// Whether any cell may have a hyperlink; `false` means [`Row::link`]
    /// is `None` for every cell.
    pub fn has_links(&self) -> bool {
        self.links.is_some()
    }
    /// Whether a prompt starts on the row (`OSC 133 ; A`). The mark follows
    /// the row through scroll and reflow; ED erasing the row removes it.
    pub fn starts_prompt(&self) -> bool {
        self.flags & history::PROMPT != 0
    }
    /// Bytes of text the row keeps for clusters over 17 bytes, overwritten
    /// ones included until compaction; at most [`Cells::text_limit`]. For
    /// memory diagnostics.
    pub fn text_len(&self) -> usize {
        self.text.long_len()
    }
    /// The row's cells, left to right.
    pub fn cells(
        &self,
    ) -> impl DoubleEndedIterator<Item = CellRef<'a>> + ExactSizeIterator + Clone + use<'a> {
        let (text, styles) = (self.text, self.grid.styles());
        // Neighbouring cells mostly share a style: look it up once a run.
        let mut last = (0, Attributes::default());
        self.padded().map(move |cell| {
            let style = cell.style();
            if style != last.0 {
                last = (style, styles.get(style));
            }
            cell.read_as(text, last.1)
        })
    }
    /// The glyphs of columns `from` through `to` as a copy takes them, with
    /// their columns: wide glyphs whole, blanks as spaces, and the trailing
    /// spacer (`ends_in_spacer`) left out.
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
    /// The row's text: blanks and placeholders as spaces, trailing blanks
    /// kept.
    pub fn cells_text(&self) -> String {
        (self.cells().filter(|c| !c.is_wide_continuation()))
            .map(|c| c.shown())
            .collect()
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

/// The rows retained, history's oldest first (`Screen::rows`).
#[derive(Clone)]
pub struct Rows<'a> {
    pub(crate) grid: &'a grid::Grid,
    pub(crate) range: Range<usize>,
}
impl<'a> Rows<'a> {
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

/// An immutable window of the screen's size, from the screen or a row of
/// history. A cell past a narrower history row's width reads as `None`, as
/// does a wide glyph the last column clips: never half a glyph.
#[derive(Clone, Copy)]
pub struct Window<'a> {
    pub(crate) grid: &'a grid::Grid,
    /// The index of its top row: at most the screen's top's.
    pub(crate) start: usize,
}
impl<'a> Window<'a> {
    /// How many rows the window has.
    pub fn rows(&self) -> u16 {
        self.grid.size().rows()
    }
    /// How many columns the window has.
    pub fn cols(&self) -> u16 {
        self.grid.size().cols()
    }
    /// Row `row` of the window; a history row keeps its own width.
    pub fn row(&self, row: u16) -> Option<Row<'a>> {
        if row >= self.rows() {
            return None;
        }
        self.grid.row_at(self.start.checked_add(usize::from(row))?)
    }
    /// Where `row` is in the window, if it shows there: never, for a row
    /// of another grid (the other screen's, or another parser's). Both
    /// borrow their grid, so the same address is the same grid.
    pub fn place(&self, row: &Row<'_>) -> Option<u16> {
        if !std::ptr::eq(self.grid, row.grid) {
            return None;
        }
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
        // A wide glyph's second half past the last column is clipped.
        let clipped = cell.is_wide() && col.checked_add(1).is_none_or(|next| next >= self.cols());
        (!clipped).then_some(cell)
    }
    /// Whether row `row` of the window is soft-wrapped.
    pub fn row_wrapped(&self, row: u16) -> bool {
        self.row(row).is_some_and(|r| r.wrapped())
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
            // Clipped wide glyphs and padding past a history row's width
            // are display blanks and must not enter a soft-wrapped join.
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
            if y == end.0 || !row.wrapped() {
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

/// Whether a screen of `size` fits the bound on cells.
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
