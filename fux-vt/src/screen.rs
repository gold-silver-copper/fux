use crate::{
    Attributes, Blink, Cell, Color, Error, Mark, Options, Reply, Row, RowId, Window, grid::Grid,
    parser::Parameters,
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MouseProtocolMode {
    #[default]
    None,
    Press,
    PressRelease,
    ButtonMotion,
    AnyMotion,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MouseProtocolEncoding {
    #[default]
    Default,
    Utf8,
    Sgr,
}

/// What a CSI sequence came to.
pub(crate) enum Dispatch {
    /// Carried out, with nothing to answer.
    Done,
    /// A query, and its answer.
    Reply(Reply),
    /// Not a sequence the screen implements.
    Unhandled,
}

/// The most flag sets a kitty keyboard stack holds; a push onto a full stack
/// drops the oldest, as kitty does, so a runaway program cannot grow it.
const KEYBOARD_STACK_LIMIT: usize = 32;

/// One screen's kitty keyboard protocol flag stack. The primary and
/// alternate screens each keep one, so a program that pushes flags on the
/// alternate screen and exits without popping them leaves the shell's alone.
#[derive(Clone, Copy, Debug, Default)]
struct KeyboardStack {
    flags: [u8; KEYBOARD_STACK_LIMIT],
    len: usize,
}

impl KeyboardStack {
    fn top(&self) -> u8 {
        self.len
            .checked_sub(1)
            .and_then(|i| self.flags.get(i))
            .copied()
            .unwrap_or(0)
    }
    fn push(&mut self, flags: u8) {
        if self.len >= KEYBOARD_STACK_LIMIT {
            // A turn by one of the whole, non-empty array.
            self.flags.rotate_left(1);
            self.len = KEYBOARD_STACK_LIMIT.saturating_sub(1);
        }
        if let Some(slot) = self.flags.get_mut(self.len)
            && let Some(len) = self.len.checked_add(1)
        {
            *slot = flags;
            self.len = len;
        }
    }
    fn pop(&mut self, count: u16) {
        self.len = self.len.saturating_sub(usize::from(count));
    }
    /// `CSI = flags ; mode u`: mode 1 (the default) replaces the top's
    /// flags, 2 adds to them and 3 removes from them.
    fn set(&mut self, flags: u8, mode: u16) {
        if self.len == 0 {
            self.push(0);
        }
        if let Some(top) = self.len.checked_sub(1).and_then(|i| self.flags.get_mut(i)) {
            *top = match mode {
                2 => *top | flags,
                3 => *top & !flags,
                _ => flags,
            };
        }
    }
}

/// Terminal state. Reading a window never changes where subsequent output lands.
#[derive(Clone, Debug)]
pub struct Screen {
    primary: Grid,
    alternate: Grid,
    next_id: u64,
    version: u64,
    structural: u64,
    alternate_active: bool,
    attributes: Attributes,
    saved_attributes: Attributes,
    autowrap: bool,
    application_cursor: bool,
    application_keypad: bool,
    hide_cursor: bool,
    bracketed_paste: bool,
    focus_reporting: bool,
    cursor_shape: u16,
    mouse: MouseProtocolMode,
    encoding: MouseProtocolEncoding,
    primary_keyboard: KeyboardStack,
    alternate_keyboard: KeyboardStack,
    modify_other_keys: Option<u8>,
    /// The cell the last glyph was printed in, while the cursor has not
    /// moved nor the row been edited since: a character that continues its
    /// grapheme cluster joins it rather than taking a cell of its own.
    last_print: Option<(u16, u16)>,
}

/// Whether `c` continues the last grapheme cluster of `previous`, by UAX #29
/// extended grapheme cluster rules, asking only about the one new boundary.
/// A Prepend character (U+0600 ARABIC NUMBER SIGN and others) joins what
/// follows it under UAX #29, which would swallow a letter or a space into its
/// cell; terminals keep them apart, so a cluster never grows past one.
fn clusters_with(previous: &str, c: char) -> bool {
    let Some(last) = previous.chars().next_back() else {
        return false;
    };
    !is_prepend(last) && !starts_cluster(last, c) && joins_by_tables(previous, c)
}

/// Whether the UAX #29 segmentation tables put no boundary between
/// `previous` and `c`.
fn joins_by_tables(previous: &str, c: char) -> bool {
    use unicode_segmentation::{GraphemeCursor, GraphemeIncomplete};
    let mut encoded = [0; 4];
    let next = c.encode_utf8(&mut encoded);
    let Some(end) = previous.len().checked_add(next.len()) else {
        return false;
    };
    let mut cursor = GraphemeCursor::new(previous.len(), end, true);
    // Each round either answers or takes more of `previous` as context,
    // which it asks for from its start: a cell holds at most 25 bytes.
    for _ in 0..=Cell::CONTENTS_CAPACITY {
        match cursor.is_boundary(next, previous.len()) {
            Ok(boundary) => return !boundary,
            Err(GraphemeIncomplete::PreContext(at)) => match previous.get(..at) {
                Some(context) => cursor.provide_context(context, 0),
                None => return false,
            },
            // Both chunks cover the string and the cursor is at the start of
            // `next`, so no other answer comes; if one did, no cluster.
            Err(_) => return false,
        }
    }
    false
}

/// Whether `c` certainly begins a new cluster after `last`, told without the
/// segmentation tables: the letters and wide characters most text is made
/// of, which UAX #29 joins to nothing before them unless that ends in a
/// zero-width character (a joiner, GB11, or an Indic virama, GB9c) or a
/// Hangul leading jamo (GB6). Emoji modifiers and the two wide spacing marks
/// are wide yet extend what precedes them. A `false` answer decides nothing;
/// the tables do. The screen tests check every scalar value against them.
fn starts_cluster(last: char, c: char) -> bool {
    if last.width() == Some(0) || matches!(u32::from(last), 0x1100..=0x115F | 0xA960..=0xA97F) {
        return false;
    }
    let letter = matches!(
        u32::from(c),
        0x00A0..=0x02FF | 0x0370..=0x0482 | 0x048A..=0x052F
    );
    let wide =
        c.width() == Some(2) && !matches!(u32::from(c), 0x1F3FB..=0x1F3FF | 0x16FF0..=0x16FF1);
    letter || wide
}

/// Grapheme_Cluster_Break=Prepend (Unicode 16).
fn is_prepend(c: char) -> bool {
    matches!(
        u32::from(c),
        0x0600..=0x0605
            | 0x06DD
            | 0x070F
            | 0x0890..=0x0891
            | 0x08E2
            | 0x0D4E
            | 0x110BD
            | 0x110CD
            | 0x111C2..=0x111C3
            | 0x1193F
            | 0x11941
            | 0x11A3A
            | 0x11A84..=0x11A89
            | 0x11D46
            | 0x11F02
    )
}

#[cfg(test)]
mod tests;

/// The palette colour an SGR parameter names: 30–37 and 40–47 are colours
/// 0–7, and 90–97 and 100–107 their bright forms 8–15.
fn palette(n: u16) -> Option<Color> {
    let (base, bright) = match n {
        30..=37 => (30, 0),
        40..=47 => (40, 0),
        90..=97 => (90, 8),
        100..=107 => (100, 8),
        _ => return None,
    };
    let index = u8::try_from(n.checked_sub(base)?).ok()?;
    Some(Color::Idx(index.checked_add(bright)?))
}

/// Whether `cells` are already the ASCII `run` in `attributes`. Kept out of
/// line, so that the write that usually follows compiles as if it were not
/// there.
#[inline(never)]
fn unchanged(cells: &[Cell], run: &[u8], attributes: Attributes) -> bool {
    cells
        .iter()
        .zip(run)
        .all(|(c, b)| c.is_ascii(*b, attributes))
}

/// Whether a glyph `width` wide at `i` has its second half after it, if it
/// needs one. Kept out of line, as `unchanged` is.
#[inline(never)]
fn whole(cells: &[Cell], i: usize, width: u16) -> bool {
    width != 2
        || i.checked_add(1)
            .and_then(|j| cells.get(j))
            .is_some_and(|c| c.same(&Cell::continuation()))
}

impl Screen {
    pub(crate) fn new(rows: u16, cols: u16, history: usize) -> Result<Self, Error> {
        let mut next_id = 1;
        Ok(Self {
            primary: Grid::new(rows, cols, history, &mut next_id, 1)?,
            alternate: Grid::new(rows, cols, 0, &mut next_id, 1)?,
            next_id,
            version: 1,
            structural: 1,
            alternate_active: false,
            attributes: Attributes::default(),
            saved_attributes: Attributes::default(),
            autowrap: true,
            application_cursor: false,
            application_keypad: false,
            hide_cursor: false,
            bracketed_paste: false,
            focus_reporting: false,
            cursor_shape: 0,
            mouse: MouseProtocolMode::None,
            encoding: MouseProtocolEncoding::Default,
            primary_keyboard: KeyboardStack::default(),
            alternate_keyboard: KeyboardStack::default(),
            modify_other_keys: None,
            last_print: None,
        })
    }
    fn grid(&self) -> &Grid {
        if self.alternate_active {
            &self.alternate
        } else {
            &self.primary
        }
    }
    fn grid_mut(&mut self) -> &mut Grid {
        if self.alternate_active {
            &mut self.alternate
        } else {
            &mut self.primary
        }
    }
    fn with_grid<T>(&mut self, f: impl FnOnce(&mut Grid, &mut u64, u64) -> T) -> T {
        let grid = if self.alternate_active {
            &mut self.alternate
        } else {
            &mut self.primary
        };
        f(grid, &mut self.next_id, self.version)
    }
    pub(crate) fn begin(&mut self) -> Result<(), Error> {
        self.version = self
            .version
            .checked_add(1)
            .ok_or(Error::IdentityExhausted)?;
        Ok(())
    }
    pub fn size(&self) -> (u16, u16) {
        (self.grid().rows.get(), self.grid().cols.get())
    }
    /// The column may equal width while autowrap is pending, matching fux's
    /// existing hidden-at-right-edge cursor contract.
    pub fn cursor_position(&self) -> (u16, u16) {
        self.grid().cursor
    }
    pub fn hide_cursor(&self) -> bool {
        self.hide_cursor
    }
    pub fn application_cursor(&self) -> bool {
        self.application_cursor
    }
    /// DECKPAM (`ESC =`) / DECKPNM (`ESC >`) state. Tracked for consumers that
    /// mirror it to another terminal; fux-vt itself encodes no keypad input.
    pub fn application_keypad(&self) -> bool {
        self.application_keypad
    }
    pub fn bracketed_paste(&self) -> bool {
        self.bracketed_paste
    }
    /// `CSI ? 1004 h` / `l` state: whether the program wants focus-in and
    /// focus-out reports. State only: fux-vt sends none.
    pub fn focus_reporting(&self) -> bool {
        self.focus_reporting
    }
    /// The cursor shape last set with DECSCUSR (`CSI Ps SP q`); 0, the
    /// default, is the terminal's own. State only: fux-vt draws no cursor.
    pub fn cursor_shape(&self) -> u16 {
        self.cursor_shape
    }
    pub fn alternate_screen(&self) -> bool {
        self.alternate_active
    }
    pub fn autowrap(&self) -> bool {
        self.autowrap
    }
    pub fn origin_mode(&self) -> bool {
        self.grid().origin
    }
    pub fn scroll_region(&self) -> (u16, u16) {
        (self.grid().top, self.grid().bottom)
    }
    pub fn mouse_protocol_mode(&self) -> MouseProtocolMode {
        self.mouse
    }
    pub fn mouse_protocol_encoding(&self) -> MouseProtocolEncoding {
        self.encoding
    }
    /// The kitty keyboard protocol flags in force: the top of the current
    /// screen's stack, 0 (legacy key reporting) when it is empty. Always 0
    /// without [`Options::kitty_keyboard`].
    pub fn kitty_keyboard_flags(&self) -> u8 {
        self.keyboard().top()
    }
    /// The xterm modifyOtherKeys level set by `CSI > 4 ; Pv m`, `None` when
    /// it is off (`Pv` 0 or absent). Always `None` without
    /// [`Options::kitty_keyboard`].
    pub fn modify_other_keys(&self) -> Option<u8> {
        self.modify_other_keys
    }
    fn keyboard(&self) -> &KeyboardStack {
        if self.alternate_active {
            &self.alternate_keyboard
        } else {
            &self.primary_keyboard
        }
    }
    fn keyboard_mut(&mut self) -> &mut KeyboardStack {
        if self.alternate_active {
            &mut self.alternate_keyboard
        } else {
            &mut self.primary_keyboard
        }
    }
    /// The one-based cursor position a DSR 6n or DECXCPR reports. A cursor
    /// waiting to wrap is one past the last column; with an identity it is
    /// reported at the last column, as xterm does.
    pub(crate) fn reported_cursor(&self, options: &Options) -> (u32, u32) {
        let g = self.grid();
        let (row, mut col) = g.cursor;
        if options.identity.is_some() {
            col = col.min(g.cols.last());
        }
        (u32::from(row) + 1, u32::from(col) + 1)
    }
    pub fn attributes(&self) -> Attributes {
        self.attributes
    }
    pub fn bgcolor(&self) -> Color {
        self.attributes.background
    }
    pub fn inverse(&self) -> bool {
        self.attributes.inverse()
    }
    pub fn cell(&self, row: u16, col: u16) -> Option<&Cell> {
        self.grid().cell(row, col)
    }
    pub fn row_wrapped(&self, row: u16) -> bool {
        self.grid().live_row(row).is_some_and(|r| r.wrapped)
    }
    pub fn history_len(&self) -> usize {
        self.grid().history_len()
    }
    pub fn row_by_id(&self, id: RowId) -> Option<Row<'_>> {
        self.grid().row_by_id(id)
    }
    /// Row offsets count from the last live row; includes all retained history.
    pub fn row_from_bottom(&self, offset: usize) -> Option<Row<'_>> {
        self.grid()
            .retained_len()
            .checked_sub(offset.checked_add(1)?)
            .and_then(|i| self.grid().row_at(i))
    }
    /// History offset putting this row at the top, if a complete window can do so.
    pub fn offset_for_row(&self, id: RowId) -> Option<usize> {
        self.grid()
            .history_len()
            .checked_sub(self.grid().index_of(id)?)
    }
    pub fn window(&self, offset: usize, rows: u16, cols: u16) -> Window<'_> {
        let grid = self.grid();
        let history = grid.history_len();
        let offset = offset.min(history);
        Window {
            grid,
            // Exact: the offset is clamped to the history.
            start: history.saturating_sub(offset),
            rows: rows.min(grid.rows.get()),
            cols: cols.min(grid.cols.get()),
            offset,
        }
    }
    pub fn mark(&self) -> Mark {
        Mark(self.version)
    }
    pub fn changed_since(&self, mark: Mark) -> bool {
        mark.0 != self.version
    }
    pub fn full_refresh_since(&self, mark: Mark) -> bool {
        mark.0 < self.structural || mark.0 > self.version
    }
    /// Independent readers can use the same mark; consuming this iterator does
    /// not acknowledge or clear changes for anyone else.
    pub fn dirty_rows_since(&self, mark: Mark) -> impl Iterator<Item = Row<'_>> {
        let full = self.full_refresh_since(mark);
        (0..self.grid().retained_len())
            .filter_map(|i| self.grid().row_at(i))
            .filter(move |r| full || r.version > mark.0)
    }
    /// The live rows changed since `mark`, each with its place on the screen
    /// (0 at the top), top to bottom; every live row after a full refresh.
    /// Only the screen's rows are read, however much history there is: the
    /// live rows `dirty_rows_since` yields, without walking the history.
    pub fn dirty_live_rows_since(&self, mark: Mark) -> impl Iterator<Item = (u16, Row<'_>)> {
        let full = self.full_refresh_since(mark);
        let grid = self.grid();
        (0..grid.rows.get())
            .filter_map(move |y| grid.live_row(y).map(|row| (y, row)))
            .filter(move |(_, row)| full || row.version > mark.0)
    }
    /// Retained allocation in cells, for capacity/plateau diagnostics.
    pub fn storage_cells(&self) -> usize {
        // Each is a Vec's capacity, far below the limit of a usize.
        self.primary
            .storage_cells()
            .saturating_add(self.alternate.storage_cells())
    }

    pub(crate) fn resize(&mut self, rows: u16, cols: u16, reflow: bool) -> Result<(), Error> {
        if self.size() == (rows, cols) {
            return Ok(());
        }
        self.last_print = None;
        let version = self
            .version
            .checked_add(1)
            .ok_or(Error::IdentityExhausted)?;
        let mut next = self.next_id;
        let primary = if reflow {
            self.primary.reflowed(rows, cols, &mut next, version)?
        } else {
            self.primary.resized(rows, cols, &mut next, version)?
        };
        let alternate = self.alternate.resized(rows, cols, &mut next, version)?;
        self.primary = primary;
        self.alternate = alternate;
        self.next_id = next;
        self.version = version;
        self.structural = version;
        Ok(())
    }

    fn scroll(
        &mut self,
        top: u16,
        bottom: u16,
        count: u16,
        up: bool,
        history: bool,
    ) -> Result<(), Error> {
        // A bounded multi-row scroll can fail after earlier rows have moved
        // (allocation/identity exhaustion). Even that partial result must
        // invalidate every reader's window, not just its newly blank rows.
        self.structural = self.version;
        self.with_grid(|g, next, version| {
            g.scroll((top, bottom), count, up, history, next, version)
        })
    }
    fn linefeed(&mut self) -> Result<(), Error> {
        let g = self.grid();
        if g.cursor.0 == g.bottom {
            self.scroll(g.top, g.bottom, 1, true, true)?;
        } else {
            // Down a row, stopping at the last.
            let row = g.cursor.0.saturating_add(1).min(g.rows.last());
            self.grid_mut().cursor.0 = row;
        }
        Ok(())
    }
    fn reverse_index(&mut self) -> Result<(), Error> {
        let g = self.grid();
        if g.cursor.0 == g.top {
            self.scroll(g.top, g.bottom, 1, false, false)?;
        } else {
            self.grid_mut().cursor.0 = g.cursor.0.saturating_sub(1);
        }
        Ok(())
    }
    fn wrap_for(&mut self, width: u16) -> Result<(), Error> {
        let g = self.grid();
        // The last column a glyph this wide can start in; a wider glyph is
        // never printed.
        let Some(room) = g.cols.get().checked_sub(width) else {
            return Ok(());
        };
        if g.cursor.1 <= room {
            return Ok(());
        }
        let wrap = self.autowrap;
        if !wrap {
            self.grid_mut().cursor.1 = room;
            return Ok(());
        }
        let row = g.cursor.0;
        let wrapped = (row < g.rows.last() || row == g.bottom)
            && g.cell(row, g.cols.last())
                .is_some_and(|c| c.has_contents() || c.is_wide_continuation());
        // Set before scrolling so a departing row carries its soft-wrap into history.
        self.with_grid(|g, _, v| g.wrap(row, wrapped, v));
        self.grid_mut().cursor.1 = 0;
        self.linefeed()
    }

    pub(crate) fn print(&mut self, c: char) -> Result<(), Error> {
        if c == '\u{fffd}' || ('\u{80}'..'\u{a0}').contains(&c) {
            return Ok(());
        }
        let width = c.width();
        if width.is_none() && u32::from(c) < 256 {
            return Ok(());
        }
        // Too wide for the grid, whatever the number, so not printed.
        let Ok(width) = u16::try_from(width.unwrap_or(1)) else {
            return Ok(());
        };
        if width > self.grid().cols.get() {
            return Ok(());
        }
        if self.extend_cluster(c) {
            return Ok(());
        }
        if width == 0 {
            let g = self.grid();
            let (row, col) = g.cursor;
            let above = row.checked_sub(1);
            let previous = if let Some(left) = col.checked_sub(1) {
                Some((row, left))
            } else if let Some(above) = above
                && g.live_row(above).is_some_and(|r| r.wrapped)
            {
                Some((above, g.cols.last()))
            } else {
                None
            };
            if let Some((row, mut col)) = previous {
                if g.cell(row, col).is_some_and(Cell::is_wide_continuation) {
                    col = col.saturating_sub(1);
                }
                self.with_grid(|g, _, v| {
                    g.mutate_row(row, v, |cells| {
                        // A cell already holding all it can takes no more.
                        cells.get_mut(usize::from(col)).is_some_and(|cell| {
                            let before = *cell;
                            cell.append(c);
                            *cell != before
                        })
                    })
                });
            }
            return Ok(());
        }
        self.wrap_for(width)?;
        let (row, col) = self.grid().cursor;
        let attributes = self.attributes;
        self.with_grid(|g, _, version| {
            g.mutate_row(row, version, |cells| {
                let i = usize::from(col);
                let glyph = Cell::glyph(c, usize::from(width), attributes);
                // Already this glyph, whole, as a redraw finds it: the row is
                // as it was. Otherwise what follows writes a cell that
                // differs, the glyph or its second half.
                let Some(current) = cells.get(i) else {
                    return false;
                };
                if current.same(&glyph) && whole(cells, i, width) {
                    return false;
                }
                // An overwrite at either half removes the other half too.
                if cells.get(i).is_some_and(Cell::is_wide_continuation)
                    && let Some(other) = i.checked_sub(1).and_then(|j| cells.get_mut(j))
                {
                    *other = Cell::blank(attributes);
                }
                if cells.get(i).is_some_and(Cell::is_wide)
                    && let Some(other) = cells.get_mut(i + 1)
                {
                    *other = Cell::glyph(' ', 1, attributes);
                }
                if width == 2
                    && cells.get(i + 1).is_some_and(Cell::is_wide)
                    && let Some(other) = cells.get_mut(i + 2)
                {
                    *other = Cell::blank(attributes);
                }
                if let Some(cell) = cells.get_mut(i) {
                    *cell = glyph;
                }
                if width == 2
                    && let Some(cell) = cells.get_mut(i + 1)
                {
                    *cell = Cell::continuation();
                }
                true
            });
            // Past the glyph; at the right edge it waits there to wrap.
            g.cursor.1 = g.cursor.1.saturating_add(width).min(g.cols.get());
        });
        self.last_print = Some((row, col));
        Ok(())
    }

    /// Joins `c` to the cell printed last, if the cursor is just past it and
    /// `c` continues its grapheme cluster: a spacing vowel sign, a variation
    /// selector, a ZWJ sequence, a flag's second regional indicator. Programs
    /// laid out with unicode-width give a cluster one cell of its string
    /// width, so a narrow cell whose cluster becomes two columns wide is
    /// widened, the cell under the cursor becoming its second half, as
    /// kitty and Ghostty (mode 2027) do. A zero-width mark joins the cell
    /// before the cursor even after a cursor move, as it always has. Whether
    /// `c` was taken, joined or, when the cell is full, dropped.
    fn extend_cluster(&mut self, c: char) -> bool {
        let g = self.grid();
        let (row, col) = g.cursor;
        let anchor = self.last_print.or_else(|| {
            if c.width() != Some(0) {
                return None;
            }
            let mut left = col.checked_sub(1)?;
            if g.cell(row, left)?.is_wide_continuation() {
                left = left.checked_sub(1)?;
            }
            Some((row, left))
        });
        let Some((anchor_row, anchor_col)) = anchor else {
            return false;
        };
        let Some(cell) = g.cell(anchor_row, anchor_col) else {
            return false;
        };
        let narrow = !cell.is_wide();
        let next = anchor_col.checked_add(if narrow { 1 } else { 2 });
        if anchor_row != row
            || next != Some(col)
            || !cell.has_contents()
            || cell.is_wide_continuation()
        {
            return false;
        }
        if !cell.can_append(c) {
            // A full cell takes no more marks; a spacing character starts a
            // cell of its own rather than joining a cut-short cluster.
            self.last_print = None;
            return c.width() == Some(0);
        }
        if !clusters_with(cell.contents(), c) {
            return false;
        }
        let mut joined = *cell;
        joined.append(c);
        // Width is kept once wide; the last column has no room to widen.
        let widen = narrow && joined.contents().width() >= 2 && col < g.cols.get();
        if widen {
            joined.widen();
        }
        self.with_grid(|g, _, v| {
            g.mutate_row(row, v, |cells| {
                if let Some(cell) = cells.get_mut(usize::from(anchor_col)) {
                    *cell = joined;
                }
                if widen {
                    let at = usize::from(col);
                    // The cell under the cursor becomes the second half; if
                    // it led a wide glyph, that glyph's half is left blank.
                    if cells.get(at).is_some_and(Cell::is_wide)
                        && let Some(orphan) = at.checked_add(1).and_then(|i| cells.get_mut(i))
                    {
                        *orphan = Cell::default();
                    }
                    if let Some(cell) = cells.get_mut(at) {
                        *cell = Cell::continuation();
                    }
                }
                true
            });
            if widen {
                g.cursor.1 = g.cursor.1.saturating_add(1).min(g.cols.get());
            }
        });
        self.last_print = Some((anchor_row, anchor_col));
        true
    }

    /// Ends the cluster being printed: the next character starts a cell of
    /// its own. Anything that moves the cursor or edits a row does.
    pub(crate) fn break_cluster(&mut self) {
        self.last_print = None;
    }

    /// Copy an ASCII run directly to cells until a wide-cell collision or right
    /// margin requires the general glyph path. Never enters parser dispatch.
    pub(crate) fn ascii(&mut self, mut bytes: &[u8]) -> Result<(), Error> {
        while let Some((&first, tail)) = bytes.split_first() {
            self.wrap_for(1)?;
            let (row, col) = self.grid().cursor;
            // `wrap_for` left room for at least one cell; without it, the run
            // could not advance.
            let Some(room) = self.grid().cols.get().checked_sub(col).filter(|r| *r > 0) else {
                return Ok(());
            };
            // A run too long for a u16 still stops at the margin.
            let count = u16::try_from(bytes.len()).map_or(room, |n| n.min(room));
            let Some(end) = col.checked_add(count) else {
                return Ok(());
            };
            let span = usize::from(col)..usize::from(end);
            let simple = self
                .grid()
                .live_row(row)
                .and_then(|r| r.cells.get(span.clone()))
                .is_some_and(|cells| {
                    cells
                        .iter()
                        .all(|c| !c.is_wide() && !c.is_wide_continuation())
                });
            if !simple {
                self.print(char::from(first))?;
                bytes = tail;
                continue;
            }
            let attributes = self.attributes;
            let run = bytes.get(..usize::from(count)).unwrap_or_default();
            self.with_grid(|g, _, v| {
                g.mutate_row(row, v, |cells| {
                    let Some(dst) = cells.get_mut(span) else {
                        return false;
                    };
                    // Already these very cells, as a redraw finds them: the
                    // row is as it was. New text differs at the first cell.
                    let first = dst.first().zip(run.first());
                    if first.is_some_and(|(c, b)| c.is_ascii(*b, attributes))
                        && unchanged(dst, run, attributes)
                    {
                        return false;
                    }
                    for (cell, byte) in dst.iter_mut().zip(run) {
                        *cell = Cell::ascii(*byte, attributes);
                    }
                    true
                });
                g.cursor.1 = end;
            });
            // The run's last glyph, which a mark or selector may join.
            self.last_print = end.checked_sub(1).map(|last| (row, last));
            bytes = bytes.get(usize::from(count)..).unwrap_or_default();
        }
        Ok(())
    }

    pub(crate) fn control(&mut self, byte: u8) -> Result<(), Error> {
        if (8..=13).contains(&byte) {
            self.break_cluster();
        }
        let g = self.grid_mut();
        match byte {
            8 => g.cursor.1 = g.cursor.1.saturating_sub(1),
            // The next multiple of eight, stopping at the last column.
            9 => {
                g.cursor.1 = (g.cursor.1 / 8)
                    .saturating_add(1)
                    .saturating_mul(8)
                    .min(g.cols.last())
            }
            10..=12 => self.linefeed()?,
            13 => g.cursor.1 = 0,
            _ => {}
        }
        Ok(())
    }
    fn save(&mut self) {
        let g = self.grid_mut();
        g.saved_cursor = g.cursor;
        g.saved_origin = g.origin;
        self.saved_attributes = self.attributes;
    }
    fn restore(&mut self) {
        let g = self.grid_mut();
        g.cursor = g.saved_cursor;
        g.origin = g.saved_origin;
        self.attributes = self.saved_attributes;
    }
    /// Carries out an escape sequence; whether fux-vt implements it.
    pub(crate) fn escape(&mut self, intermediates: &[u8], byte: u8) -> Result<bool, Error> {
        if !intermediates.is_empty() {
            return Ok(false);
        }
        if matches!(byte, b'7' | b'8' | b'M' | b'c') {
            self.break_cluster();
        }
        match byte {
            b'7' => self.save(),
            b'8' => self.restore(),
            b'=' => self.application_keypad = true,
            b'>' => self.application_keypad = false,
            b'M' => self.reverse_index()?,
            b'c' => {
                let (rows, cols) = self.size();
                let mut next = self.next_id;
                // Both grids start again: in the storage they have, if both
                // hold only their live rows at this size, else afresh. Either
                // way nothing changes unless both can.
                let same = |g: &Grid| (g.rows.get(), g.cols.get()) == (rows, cols);
                let recycle = [&self.primary, &self.alternate]
                    .iter()
                    .all(|g| g.recyclable() && same(g));
                if recycle {
                    let needed = u64::from(rows).saturating_mul(2);
                    next.checked_add(needed).ok_or(Error::IdentityExhausted)?;
                    self.primary.clear(&mut next, self.version)?;
                    self.alternate.clear(&mut next, self.version)?;
                } else {
                    let history = self.primary.history_limit;
                    let primary = Grid::new(rows, cols, history, &mut next, self.version)?;
                    let alternate = Grid::new(rows, cols, 0, &mut next, self.version)?;
                    self.primary = primary;
                    self.alternate = alternate;
                }
                self.next_id = next;
                self.alternate_active = false;
                self.attributes = Attributes::default();
                self.saved_attributes = Attributes::default();
                self.autowrap = true;
                self.application_cursor = false;
                self.application_keypad = false;
                self.hide_cursor = false;
                self.bracketed_paste = false;
                self.focus_reporting = false;
                self.cursor_shape = 0;
                self.mouse = MouseProtocolMode::None;
                self.encoding = MouseProtocolEncoding::Default;
                self.primary_keyboard = KeyboardStack::default();
                self.alternate_keyboard = KeyboardStack::default();
                self.modify_other_keys = None;
                self.structural = self.version;
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// DECRQM status for a DEC private mode: 1 set, 2 reset, 0 not recognized.
    pub(crate) fn private_mode_status(&self, n: u16) -> u8 {
        let set = match n {
            1 => self.application_cursor,
            6 => self.grid().origin,
            7 => self.autowrap,
            25 => !self.hide_cursor,
            47 | 1049 => self.alternate_active,
            9 => self.mouse == MouseProtocolMode::Press,
            1000 => self.mouse == MouseProtocolMode::PressRelease,
            1002 => self.mouse == MouseProtocolMode::ButtonMotion,
            1003 => self.mouse == MouseProtocolMode::AnyMotion,
            1005 => self.encoding == MouseProtocolEncoding::Utf8,
            1006 => self.encoding == MouseProtocolEncoding::Sgr,
            2004 => self.bracketed_paste,
            _ => return 0,
        };
        if set { 1 } else { 2 }
    }

    fn mode(&mut self, n: u16, set: bool) -> Result<(), Error> {
        match n {
            1 => self.application_cursor = set,
            6 => {
                let g = self.grid_mut();
                g.origin = set;
                g.position(0, 0);
            }
            7 => self.autowrap = set,
            25 => self.hide_cursor = !set,
            2004 => self.bracketed_paste = set,
            1004 => self.focus_reporting = set,
            47 => {
                self.alternate_active = set;
                self.structural = self.version;
            }
            1049 => {
                if set {
                    self.save();
                    self.alternate.clear(&mut self.next_id, self.version)?;
                    self.alternate_active = true;
                } else {
                    self.alternate_active = false;
                    self.restore();
                }
                self.structural = self.version;
            }
            9 | 1000 | 1002 | 1003 => {
                let mode = match n {
                    9 => MouseProtocolMode::Press,
                    1000 => MouseProtocolMode::PressRelease,
                    1002 => MouseProtocolMode::ButtonMotion,
                    _ => MouseProtocolMode::AnyMotion,
                };
                if set {
                    self.mouse = mode;
                } else if self.mouse == mode {
                    self.mouse = MouseProtocolMode::None;
                }
            }
            1005 | 1006 => {
                let encoding = if n == 1005 {
                    MouseProtocolEncoding::Utf8
                } else {
                    MouseProtocolEncoding::Sgr
                };
                if set {
                    self.encoding = encoding;
                } else if self.encoding == encoding {
                    self.encoding = MouseProtocolEncoding::Default;
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// The kitty keyboard protocol and modifyOtherKeys sequences, with
    /// [`Options::kitty_keyboard`]; `None` for any other sequence.
    fn keyboard_protocol(
        &mut self,
        p: &Parameters,
        intermediates: &[u8],
        byte: u8,
    ) -> Option<Dispatch> {
        // Flags are a bit set below 32, but kitty tolerates larger values:
        // they saturate rather than being refused.
        let flags = |n: u16| u8::try_from(n).unwrap_or(u8::MAX);
        match (intermediates, byte) {
            (b">", b'u') => self.keyboard_mut().push(flags(p.first(0, 0))),
            (b"<", b'u') => self.keyboard_mut().pop(p.first(0, 1)),
            (b"=", b'u') => {
                let (set, mode) = (flags(p.first(0, 0)), p.first(1, 1));
                self.keyboard_mut().set(set, mode);
            }
            (b"?", b'u') => {
                let flags = self.kitty_keyboard_flags();
                return Some(Dispatch::Reply(Reply::of(format_args!("\x1b[?{flags}u"))));
            }
            // Only resource 4 is modifyOtherKeys; other resources are unhandled.
            (b">", b'm') if p.groups().next() == Some(&[4][..]) => {
                self.modify_other_keys = match p.first(1, 0) {
                    0 => None,
                    level => Some(flags(level)),
                };
            }
            _ => return None,
        }
        Some(Dispatch::Done)
    }

    pub(crate) fn csi(
        &mut self,
        p: &Parameters,
        intermediates: &[u8],
        byte: u8,
        options: &Options,
    ) -> Result<Dispatch, Error> {
        let private = intermediates == b"?";
        // DECSCUSR: its intermediate is a space.
        if intermediates == b" " && byte == b'q' {
            self.cursor_shape = p.first(0, 0);
            return Ok(Dispatch::Done);
        }
        if options.kitty_keyboard
            && let Some(dispatch) = self.keyboard_protocol(p, intermediates, byte)
        {
            return Ok(dispatch);
        }
        if !intermediates.is_empty() && !private {
            return Ok(Dispatch::Unhandled);
        }
        if private && matches!(byte, b'h' | b'l') {
            for group in p.groups() {
                if let [n] = group {
                    // A switch of screens leaves the printed cell behind.
                    if matches!(n, 47 | 1049) {
                        self.break_cluster();
                    }
                    self.mode(*n, byte == b'h')?;
                }
            }
            return Ok(Dispatch::Done);
        }
        // Every sequence that moves the cursor or edits a row; not SGR,
        // modes or queries.
        if matches!(
            byte,
            b'A'..=b'H'
                | b'J'
                | b'K'
                | b'L'
                | b'M'
                | b'P'
                | b'S'
                | b'T'
                | b'X'
                | b'@'
                | b'd'
                | b'f'
                | b'r'
                | b's'
                | b'u'
        ) {
            self.break_cluster();
        }
        if private && !matches!(byte, b'J' | b'K') {
            return Ok(Dispatch::Unhandled);
        }
        let n = p.first(0, 1);
        let (row, col) = self.grid().cursor;
        match byte {
            b'A' | b'B' | b'E' | b'F' => {
                let g = self.grid_mut();
                let (top, bottom) = if g.in_region() {
                    (g.top, g.bottom)
                } else {
                    (0, g.rows.last())
                };
                g.cursor.0 = if matches!(byte, b'A' | b'F') {
                    row.saturating_sub(n).max(top)
                } else {
                    row.saturating_add(n).min(bottom)
                };
                if matches!(byte, b'E' | b'F') {
                    g.cursor.1 = 0;
                }
            }
            b'C' => {
                let g = self.grid_mut();
                g.cursor.1 = col.saturating_add(n).min(g.cols.last());
            }
            b'D' => self.grid_mut().cursor.1 = col.saturating_sub(n),
            // Coordinates are one-based, and 0 means 1.
            b'G' => {
                let g = self.grid_mut();
                g.cursor.1 = n.saturating_sub(1).min(g.cols.last());
            }
            // CUP, and HVP, which is CUP with another final byte.
            b'H' | b'f' => self
                .grid_mut()
                .position(n.saturating_sub(1), p.first(1, 1).saturating_sub(1)),
            // SCOSC and SCORC share DECSC's slot and, like DECSC, save and
            // restore the attributes with the position.
            b's' => self.save(),
            b'u' => self.restore(),
            b'd' => {
                let g = self.grid_mut();
                g.cursor.0 = n.saturating_sub(1).min(g.rows.last());
            }
            b'@' | b'P' => self.with_grid(|g, _, v| g.edit_cells(n, byte == b'@', v)),
            b'X' => {
                let a = self.attributes;
                self.with_grid(|g, _, v| g.erase(row, col, col.saturating_add(n), a, v));
            }
            b'J' | b'K' => {
                let mode = p.first(0, 0);
                let a = self.attributes;
                if mode > 2 {
                    return Ok(Dispatch::Unhandled);
                }
                self.with_grid(|g, _, v| {
                    let cols = g.cols.get();
                    if byte == b'J' {
                        for y in 0..g.rows.get() {
                            if (mode == 0 && y > row) || (mode == 1 && y < row) || mode == 2 {
                                g.erase(y, 0, cols, a, v);
                            }
                        }
                    }
                    let (start, end) = match mode {
                        0 => (col, cols),
                        1 => (0, col.saturating_add(1).min(cols)),
                        _ => (0, cols),
                    };
                    g.erase(row, start, end, a, v);
                });
            }
            b'L' | b'M' => {
                let g = self.grid();
                if g.in_region() {
                    self.scroll(row, g.bottom, n, byte == b'M', false)?;
                }
            }
            b'S' | b'T' => {
                let g = self.grid();
                self.scroll(g.top, g.bottom, n, byte == b'S', true)?;
            }
            b'r' => {
                let rows = self.grid().rows;
                let bottom = p.first(1, rows.get()).saturating_sub(1).min(rows.last());
                let top = n.saturating_sub(1);
                let g = self.grid_mut();
                (g.top, g.bottom) = if top < bottom {
                    (top, bottom)
                } else {
                    (0, rows.last())
                };
                g.cursor = (g.top, 0);
            }
            b'm' => self.sgr(p),
            b'n' => match p.first(0, 0) {
                5 => return Ok(Dispatch::Reply(Reply::of(format_args!("\x1b[0n")))),
                6 => {
                    let (row, col) = self.reported_cursor(options);
                    let reply = Reply::of(format_args!("\x1b[{row};{col}R"));
                    return Ok(Dispatch::Reply(reply));
                }
                _ => return Ok(Dispatch::Unhandled),
            },
            b'c' if p.first(0, 0) == 0 => {
                let reply = if options.identity.is_some() {
                    Reply::of(format_args!("\x1b[?62;22c"))
                } else {
                    Reply::of(format_args!("\x1b[?1;2c"))
                };
                return Ok(Dispatch::Reply(reply));
            }
            _ => return Ok(Dispatch::Unhandled),
        }
        Ok(Dispatch::Done)
    }

    fn sgr(&mut self, p: &Parameters) {
        const WEIGHT: u16 = Attributes::BOLD | Attributes::DIM;
        let mut groups = p.groups();
        while let Some(group) = groups.next() {
            match group {
                [0] => self.attributes = Attributes::default(),
                // Bold and dim replace one another.
                [1] => self.attributes.flags = self.attributes.flags & !WEIGHT | Attributes::BOLD,
                [2] => self.attributes.flags = self.attributes.flags & !WEIGHT | Attributes::DIM,
                [3] => self.attributes.flags |= Attributes::ITALIC,
                [4] => self.attributes.flags |= Attributes::UNDERLINE,
                // Slow and rapid blink replace one another.
                [5] => self.attributes = self.attributes.with_blink(Blink::Slow),
                [6] => self.attributes = self.attributes.with_blink(Blink::Rapid),
                [7] => self.attributes.flags |= Attributes::INVERSE,
                [8] => self.attributes.flags |= Attributes::HIDDEN,
                [9] => self.attributes.flags |= Attributes::STRIKEOUT,
                [22] => self.attributes.flags &= !WEIGHT,
                [23] => self.attributes.flags &= !Attributes::ITALIC,
                [24] => self.attributes.flags &= !Attributes::UNDERLINE,
                [25] => self.attributes.flags &= !Attributes::BLINK,
                [27] => self.attributes.flags &= !Attributes::INVERSE,
                [28] => self.attributes.flags &= !Attributes::HIDDEN,
                [29] => self.attributes.flags &= !Attributes::STRIKEOUT,
                [39] => self.attributes.foreground = Color::Default,
                [49] => self.attributes.background = Color::Default,
                [59] => self.attributes.underline_color = Color::Default,
                [n @ (30..=37 | 90..=97)] => {
                    if let Some(color) = palette(*n) {
                        self.attributes.foreground = color;
                    }
                }
                [n @ (40..=47 | 100..=107)] => {
                    if let Some(color) = palette(*n) {
                        self.attributes.background = color;
                    }
                }
                // Foreground, background and underline colour share their forms.
                [selector @ (38 | 48 | 58), rest @ ..] => {
                    let mut parts = [0u16; 4];
                    let count = if rest.is_empty() {
                        let Some([kind]) = groups.next() else {
                            return;
                        };
                        let count = match kind {
                            2 => 4,
                            5 => 2,
                            _ => return,
                        };
                        if let Some(first) = parts.first_mut() {
                            *first = *kind;
                        }
                        for slot in parts.iter_mut().take(count).skip(1) {
                            let Some([n]) = groups.next() else {
                                return;
                            };
                            *slot = *n;
                        }
                        count
                    } else {
                        // All of them, if they fit.
                        let Some(()) = parts
                            .get_mut(..rest.len())
                            .and_then(|start| crate::copy_from(start, rest))
                        else {
                            continue;
                        };
                        rest.len()
                    };
                    let colour = match parts.get(..count) {
                        Some([5, index]) => u8::try_from(*index).ok().map(Color::Idx),
                        Some([2, r, g, b]) => u8::try_from(*r)
                            .ok()
                            .zip(u8::try_from(*g).ok())
                            .zip(u8::try_from(*b).ok())
                            .map(|((r, g), b)| Color::Rgb(r, g, b)),
                        _ => None,
                    };
                    let Some(colour) = colour else {
                        return;
                    };
                    match selector {
                        38 => self.attributes.foreground = colour,
                        48 => self.attributes.background = colour,
                        _ => self.attributes.underline_color = colour,
                    }
                }
                _ => {}
            }
        }
    }
}
