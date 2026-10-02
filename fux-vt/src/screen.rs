use crate::unicode::Cluster;
use crate::{
    Attributes, Blink, Cell, CellRef, Color, Error, Mark, Options, Reply, Row, RowId, Window,
    grid::{Grid, Scroll},
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

/// The cell printed last, the state of its grapheme cluster, and whether
/// the cluster is full: once a character of it is dropped, so is every one
/// after, so a cell always holds a start of its cluster.
#[derive(Clone, Copy, Debug)]
struct Printed {
    at: (u16, u16),
    cluster: Cluster,
    full: bool,
}

impl Printed {
    fn new(at: (u16, u16), c: char) -> Self {
        Self {
            at,
            cluster: Cluster::start(c),
            full: false,
        }
    }
}

/// The character sets a program designates and shifts between (ECMA-35;
/// DEC STD 070, ch. 3; the VT520 manual's SCS): G0 and G1, each ASCII or
/// DEC Special Graphics, and which of them is in GL, G1 after SO and G0
/// after SI. G2, G3 and the national sets are not kept: a set other than
/// Special Graphics is ASCII, as xterm reads one in UTF-8.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Charsets {
    g0_graphics: bool,
    g1_graphics: bool,
    shifted: bool,
}

impl Charsets {
    /// Whether printable ASCII prints as DEC Special Graphics.
    fn graphics(self) -> bool {
        if self.shifted {
            self.g1_graphics
        } else {
            self.g0_graphics
        }
    }
}

/// The DEC Special Graphics character for `c`, as xterm draws it: 0x5f to
/// 0x7e are a blank, a diamond, a checkerboard, control pictures, degree
/// and plus-minus, line drawing, scan lines, comparisons, pi, not-equal,
/// pound and a middle dot (VT520 manual, Special Graphics set; xterm,
/// `fux-vt-compare replay --engines xterm` of `ESC ( 0` and the bytes 0x5f to 0x7e).
fn special_graphics(c: char) -> char {
    const GRAPHICS: [char; 32] = [
        ' ', '◆', '▒', '␉', '␌', '␍', '␊', '°', '±', '␤', '␋', '┘', '┐', '┌', '└', '┼', '⎺', '⎻',
        '─', '⎼', '⎽', '├', '┤', '┴', '┬', '│', '≤', '≥', 'π', '≠', '£', '·',
    ];
    u32::from(c)
        .checked_sub(0x5f)
        .and_then(|i| usize::try_from(i).ok())
        .and_then(|i| GRAPHICS.get(i))
        .copied()
        .unwrap_or(c)
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
    charsets: Charsets,
    /// The character sets DECSC saved, which DECRC restores.
    saved_charsets: Charsets,
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
    /// The cell the last glyph was printed in, and the state of its
    /// grapheme cluster, while the cursor has not moved nor the row been
    /// edited since: a character that continues the cluster joins its cell
    /// rather than taking one of its own.
    last_print: Option<Printed>,
    /// The character REP (`CSI b`) repeats: the last one printed that took
    /// a cell of its own, as long as nothing but printing came after it.
    repeat: Option<char>,
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

/// The colour of 38, 48 or 58 in their colon form, `rest` being what
/// follows the selector: `5:index`, `2:r:g:b`, or ITU-T T.416's
/// `2:space:r:g:b`, its colour space ignored and empty, as xterm reads it,
/// with anything after the blue ignored. `None` if the colour is out of
/// range or of another kind.
fn colour_of(rest: &[u16]) -> Option<Color> {
    match rest {
        [5, index, ..] => u8::try_from(*index).ok().map(Color::Idx),
        [2, r, g, b] | [2, _, r, g, b, ..] => rgb(*r, *g, *b),
        _ => None,
    }
}

fn rgb(r: u16, g: u16, b: u16) -> Option<Color> {
    Some(Color::Rgb(
        u8::try_from(r).ok()?,
        u8::try_from(g).ok()?,
        u8::try_from(b).ok()?,
    ))
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
            charsets: Charsets::default(),
            saved_charsets: Charsets::default(),
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
            repeat: None,
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
    /// The cursor's row and column, always on the screen. A glyph printed
    /// in the last column leaves the cursor on it, with
    /// [`pending_wrap`](Self::pending_wrap) set, as xterm does.
    pub fn cursor_position(&self) -> (u16, u16) {
        self.grid().cursor
    }
    /// DEC STD 070's Last Column Flag: a glyph went into the last column,
    /// and the next one, with autowrap on, first moves to the start of the
    /// next line. Cursor movements, line feeds and edits end it; DECSC and
    /// SCOSC save it with the cursor.
    pub fn pending_wrap(&self) -> bool {
        self.grid().pending_wrap
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
    /// waiting to wrap is reported one past the last column, as the vt100
    /// crate did; with an identity it is reported at the last column, as
    /// xterm does.
    pub(crate) fn reported_cursor(&self, options: &Options) -> (u32, u32) {
        let g = self.grid();
        let col = if options.identity.is_some() {
            g.cursor.1
        } else {
            g.next_column()
        };
        (u32::from(g.cursor.0) + 1, u32::from(col) + 1)
    }
    pub fn attributes(&self) -> Attributes {
        self.attributes
    }
    pub fn bgcolor(&self) -> Color {
        self.attributes.background()
    }
    pub fn inverse(&self) -> bool {
        self.attributes.inverse()
    }
    pub fn cell(&self, row: u16, col: u16) -> Option<CellRef<'_>> {
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
        // The rows brought in take the pen's colours (`bce`), as in xterm.
        let blank = self.attributes.erased();
        self.with_grid(|g, next, version| {
            let direction = if up {
                Scroll::Up { history }
            } else {
                Scroll::Down
            };
            g.scroll((top, bottom), count, direction, blank, next, version)
        })
    }
    fn linefeed(&mut self) -> Result<(), Error> {
        self.grid_mut().pending_wrap = false;
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
        self.grid_mut().pending_wrap = false;
        let g = self.grid();
        if g.cursor.0 == g.top {
            self.scroll(g.top, g.bottom, 1, false, false)?;
        } else {
            self.grid_mut().cursor.0 = g.cursor.0.saturating_sub(1);
        }
        Ok(())
    }
    /// Makes room for a glyph `width` wide at the cursor, which is left
    /// where it goes, with no wrap pending: a pending wrap, or a glyph too
    /// wide for what is left of the row, moves it to the start of the next
    /// line with autowrap on (DEC STD 070, Appendix D.6.1), and back to the
    /// last column it fits in with autowrap off.
    fn wrap_for(&mut self, width: u16) -> Result<(), Error> {
        let g = self.grid();
        // The last column a glyph this wide can start in; a wider glyph is
        // never printed.
        let Some(room) = g.cols.get().checked_sub(width) else {
            return Ok(());
        };
        if g.next_column() <= room {
            return Ok(());
        }
        let wrap = self.autowrap;
        if !wrap {
            let g = self.grid_mut();
            g.cursor.1 = room;
            g.pending_wrap = false;
            return Ok(());
        }
        let row = g.cursor.0;
        // The glyph goes on to the next line, so this row is soft-wrapped,
        // whatever its last column holds: blank when a wide glyph did not
        // fit, or after an erase the wrap outlived. Only on the last row,
        // below the scroll region, does the glyph stay on the same row.
        let wrapped = row < g.rows.last() || row == g.bottom;
        // Set before scrolling so a departing row carries its soft-wrap into history.
        self.with_grid(|g, _, v| g.wrap(row, wrapped, v));
        self.grid_mut().cursor.1 = 0;
        self.linefeed()
    }

    pub(crate) fn print(&mut self, raw: char) -> Result<(), Error> {
        let c = if self.charsets.graphics() {
            special_graphics(raw)
        } else {
            raw
        };
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
        if width != 0 {
            self.repeat = Some(raw);
        }
        if width == 0 {
            let g = self.grid();
            let (row, col) = (g.cursor.0, g.next_column());
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
                if g.cell(row, col).is_some_and(|c| c.is_wide_continuation()) {
                    col = col.saturating_sub(1);
                }
                // A cell already holding all it can takes no more.
                self.with_grid(|g, _, v| {
                    g.mutate_line(row, v, |line| line.append(usize::from(col), c))
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
            // Past the glyph; in the last column it waits there to wrap.
            g.advance_to(col.saturating_add(width));
        });
        self.last_print = Some(Printed::new((row, col), c));
        Ok(())
    }

    /// Joins `c` to the cell printed last, if the cursor is just past it and
    /// `c` continues its grapheme cluster (UAX #29, see `unicode.rs`): a
    /// spacing vowel sign, a variation selector, a ZWJ sequence, a flag's
    /// second regional indicator. Programs laid out with unicode-width give a
    /// cluster one cell of its string width, so a narrow cell whose cluster
    /// becomes two columns wide is widened, the cell under the cursor
    /// becoming its second half, as kitty and Ghostty (mode 2027) do. A
    /// zero-width mark joins the cell before the cursor even after a cursor
    /// move, as it always has. Whether `c` was taken: joined, or dropped
    /// because the cluster is full, which never splits it.
    fn extend_cluster(&mut self, c: char) -> bool {
        let g = self.grid();
        let (row, col) = (g.cursor.0, g.next_column());
        let anchor = self.last_print.or_else(|| {
            if c.width() != Some(0) {
                return None;
            }
            let mut left = col.checked_sub(1)?;
            if g.cell(row, left)?.is_wide_continuation() {
                left = left.checked_sub(1)?;
            }
            // Its cluster's state, from its text.
            let cluster = Cluster::of(g.cell(row, left)?.contents());
            Some(Printed {
                at: (row, left),
                cluster,
                full: false,
            })
        });
        let Some(Printed {
            at: (anchor_row, anchor_col),
            mut cluster,
            full,
        }) = anchor
        else {
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
            || !cluster.push(c)
        {
            return false;
        }
        let at = usize::from(anchor_col);
        let cursor = usize::from(col);
        let fits = col < g.cols.get();
        let mut widened = false;
        let mut kept = !full;
        self.with_grid(|g, _, v| {
            g.mutate_line(row, v, |line| {
                kept = kept && line.append(at, c);
                if !kept {
                    return false;
                }
                // Width is kept once wide; the last column has no room to widen.
                if narrow && fits && line.text(at).width() >= 2 {
                    if let Some(cell) = line.cells.get_mut(at) {
                        cell.widen();
                    }
                    // The cell under the cursor becomes the second half; if
                    // it led a wide glyph, that glyph's half is left blank.
                    if line.cells.get(cursor).is_some_and(Cell::is_wide)
                        && let Some(orphan) =
                            cursor.checked_add(1).and_then(|i| line.cells.get_mut(i))
                    {
                        *orphan = Cell::default();
                    }
                    if let Some(cell) = line.cells.get_mut(cursor) {
                        *cell = Cell::continuation();
                    }
                    widened = true;
                }
                true
            });
            if widened {
                g.advance_to(col.saturating_add(1));
            }
        });
        self.last_print = Some(Printed {
            at: (anchor_row, anchor_col),
            cluster,
            full: !kept,
        });
        true
    }

    /// Ends the cluster being printed: the next character starts a cell of
    /// its own. Anything that moves the cursor or edits a row does.
    pub(crate) fn break_cluster(&mut self) {
        self.last_print = None;
    }

    /// Forgets the character REP repeats. The parser calls it for every
    /// control in ground state and every sequence or string it ends, as
    /// xterm forgets its last character whenever its parser returns to the
    /// ground state without printing.
    pub(crate) fn forget_repeat(&mut self) {
        self.repeat = None;
    }

    /// REP (ECMA-48 8.3.103): the preceding graphic character printed
    /// `count` more times, as if it had been sent again. Nothing if anything
    /// but printing came after that character, where ECMA-48 leaves REP
    /// undefined, as in xterm.
    ///
    /// Once the copies have filled the screen and every row of history the
    /// grid keeps, each further row's worth leaves all of it as it was, so
    /// whole rows' worth past that are skipped: no more than a screen and
    /// its history's worth is ever printed, and what shows is exactly what
    /// printing them all would show. An ASCII character is printed in runs,
    /// as text is.
    fn repeat(&mut self, count: u16) -> Result<(), Error> {
        let Some(c) = self.repeat else {
            return Ok(());
        };
        let g = self.grid();
        let width = c.width().unwrap_or(1).max(1);
        // Copies to a row: a wide glyph leaves an odd last column blank.
        let Some(per_row) = usize::from(g.cols.get())
            .checked_div(width)
            .filter(|n| *n > 0)
        else {
            return Ok(());
        };
        let lines = usize::from(g.rows.get())
            .saturating_add(g.history_limit)
            .saturating_add(1);
        let full = lines.saturating_mul(per_row);
        let mut count = usize::from(count);
        if let Some(beyond) = count.checked_sub(full) {
            count = full.saturating_add(beyond.checked_rem(per_row).unwrap_or(0));
        }
        if let Ok(byte) = u8::try_from(c)
            && (0x20..=0x7e).contains(&byte)
        {
            const RUN: usize = 128;
            let run = [byte; RUN];
            while count > 0 {
                let n = count.min(RUN);
                self.ascii(run.get(..n).unwrap_or_default())?;
                count = count.saturating_sub(n);
            }
        } else {
            for _ in 0..count {
                self.print(c)?;
            }
        }
        Ok(())
    }

    /// Copy an ASCII run directly to cells until a wide-cell collision or right
    /// margin requires the general glyph path. Never enters parser dispatch.
    pub(crate) fn ascii(&mut self, mut bytes: &[u8]) -> Result<(), Error> {
        // DEC Special Graphics print other characters, one at a time.
        if self.charsets.graphics() {
            for &byte in bytes {
                self.print(char::from(byte))?;
            }
            return Ok(());
        }
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
                g.advance_to(end);
            });
            // The run's last glyph, which a mark or selector may join, and
            // REP repeats.
            self.last_print = end
                .checked_sub(1)
                .zip(run.last())
                .map(|(last, &byte)| Printed::new((row, last), char::from(byte)));
            if let Some(&last) = run.last() {
                self.repeat = Some(char::from(last));
            }
            bytes = bytes.get(usize::from(count)..).unwrap_or_default();
        }
        Ok(())
    }

    pub(crate) fn control(&mut self, byte: u8) -> Result<(), Error> {
        if (8..=13).contains(&byte) {
            self.break_cluster();
        }
        let g = self.grid_mut();
        // BS, LF, VT, FF and CR end a pending wrap (DEC STD 070, Appendix
        // D.6.1). HT does not: it leaves a cursor in the last column where
        // it is, still waiting to wrap, as xterm does.
        if matches!(byte, 8 | 13) {
            g.pending_wrap = false;
        }
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
            // SO puts G1 in GL, SI G0.
            14 => self.charsets.shifted = true,
            15 => self.charsets.shifted = false,
            _ => {}
        }
        Ok(())
    }
    /// DECSC: the cursor, with its pending wrap (DEC STD 070, Appendix
    /// D.6.1), origin mode and the drawing attributes.
    fn save(&mut self) {
        let g = self.grid_mut();
        g.saved_cursor = g.cursor;
        g.saved_pending_wrap = g.pending_wrap;
        g.saved_origin = g.origin;
        self.saved_attributes = self.attributes;
        self.saved_charsets = self.charsets;
    }
    fn restore(&mut self) {
        let g = self.grid_mut();
        g.cursor = g.saved_cursor;
        g.pending_wrap = g.saved_pending_wrap;
        g.origin = g.saved_origin;
        self.attributes = self.saved_attributes;
        self.charsets = self.saved_charsets;
    }
    /// Carries out an escape sequence; whether fux-vt implements it.
    pub(crate) fn escape(&mut self, intermediates: &[u8], byte: u8) -> Result<bool, Error> {
        // SCS: ESC ( F designates G0 and ESC ) F G1; F `0` is DEC Special
        // Graphics, and any other set is ASCII here.
        match intermediates {
            b"(" => self.charsets.g0_graphics = byte == b'0',
            b")" => self.charsets.g1_graphics = byte == b'0',
            [] => {}
            _ => return Ok(false),
        }
        if !intermediates.is_empty() {
            return Ok(true);
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
                self.charsets = Charsets::default();
                self.saved_charsets = Charsets::default();
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

    /// DECSTR (`CSI ! p`), as xterm does it: the modes a program sets go
    /// back to their defaults, as the VT520 manual's table (p. 5-150) and
    /// DEC STD 070's Soft Terminal Reset (p. 4-37) list them: the cursor
    /// shown, DECOM, DECCKM and DECKPAM off, the scroll region the whole
    /// screen, the pen and the saved cursor's attributes normal, the
    /// character sets ASCII with G0 in GL, and the saved cursor home. DECAWM goes back to its default, which both
    /// leave to the terminal's setting (xterm's: on). The screen, the
    /// cursor, a pending wrap, the alternate screen, bracketed paste, focus
    /// reporting, mouse modes and kitty keyboard flags stay as they are,
    /// as in xterm.
    fn soft_reset(&mut self) {
        self.hide_cursor = false;
        self.autowrap = true;
        self.application_cursor = false;
        self.application_keypad = false;
        self.attributes = Attributes::default();
        self.saved_attributes = Attributes::default();
        self.charsets = Charsets::default();
        self.saved_charsets = Charsets::default();
        for g in [&mut self.primary, &mut self.alternate] {
            g.origin = false;
            g.top = 0;
            g.bottom = g.rows.last();
        }
        let g = self.grid_mut();
        g.saved_cursor = (0, 0);
        g.saved_pending_wrap = false;
        g.saved_origin = false;
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
        if intermediates == b"!" && byte == b'p' {
            self.soft_reset();
            return Ok(Dispatch::Done);
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
        // Every cursor movement, erase and edit ends a pending wrap (DEC STD
        // 070, Appendix D.6.1, which lists them), as xterm does; ED, EL, IL
        // and DL below, once they are carried out. SU and SD are not among
        // them: the cursor stays waiting to wrap, as in xterm.
        if matches!(byte, b'A'..=b'H' | b'X' | b'd' | b'f' | b'r') {
            self.grid_mut().pending_wrap = false;
        }
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
            b'@' | b'P' => {
                let blank = self.attributes.erased();
                self.with_grid(|g, _, v| g.edit_cells(n, byte == b'@', blank, v));
            }
            // Erased cells take the pen's colours alone, as xterm's do.
            b'X' => {
                let a = self.attributes.erased();
                self.with_grid(|g, _, v| g.erase(row, col, col.saturating_add(n), a, v));
            }
            b'J' | b'K' => {
                let mode = p.first(0, 0);
                let a = self.attributes.erased();
                if mode > 2 {
                    return Ok(Dispatch::Unhandled);
                }
                self.with_grid(|g, _, v| {
                    g.pending_wrap = false;
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
                    self.grid_mut().pending_wrap = false;
                    let g = self.grid();
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
            b'b' => self.repeat(n)?,
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
        let mut groups = p.groups().peekable();
        while let Some(group) = groups.next() {
            match group {
                [0] => self.attributes = Attributes::default(),
                // Bold and dim are kept apart, and both can be on, as in
                // xterm; 22 ends both (ECMA-48 8.3.117).
                [1] => self.attributes.flags |= Attributes::BOLD,
                [2] => self.attributes.flags |= Attributes::DIM,
                [3] => self.attributes.flags |= Attributes::ITALIC,
                // 21 is doubly underlined (ECMA-48 8.3.117, xterm's
                // ctlseqs): fux-vt keeps no underline style.
                [4 | 21] => self.attributes.flags |= Attributes::UNDERLINE,
                // Slow and rapid blink replace one another.
                [5] => self.attributes = self.attributes.with_blink(Blink::Slow),
                [6] => self.attributes = self.attributes.with_blink(Blink::Rapid),
                [7] => self.attributes.flags |= Attributes::INVERSE,
                [8] => self.attributes.flags |= Attributes::HIDDEN,
                [9] => self.attributes.flags |= Attributes::STRIKEOUT,
                [22] => self.attributes.flags &= !(Attributes::BOLD | Attributes::DIM),
                [23] => self.attributes.flags &= !Attributes::ITALIC,
                [24] => self.attributes.flags &= !Attributes::UNDERLINE,
                [25] => self.attributes.flags &= !Attributes::BLINK,
                [27] => self.attributes.flags &= !Attributes::INVERSE,
                [28] => self.attributes.flags &= !Attributes::HIDDEN,
                [29] => self.attributes.flags &= !Attributes::STRIKEOUT,
                [39] => self.attributes = self.attributes.with_foreground(Color::Default),
                [49] => self.attributes = self.attributes.with_background(Color::Default),
                [59] => self.attributes = self.attributes.with_underline_color(Color::Default),
                [n @ (30..=37 | 90..=97)] => {
                    if let Some(color) = palette(*n) {
                        self.attributes = self.attributes.with_foreground(color);
                    }
                }
                [n @ (40..=47 | 100..=107)] => {
                    if let Some(color) = palette(*n) {
                        self.attributes = self.attributes.with_background(color);
                    }
                }
                // Underline styles (kitty's, which every engine in
                // `compare/` reads but xterm): fux-vt keeps no style, so
                // 4:0 ends underline and the styles 1 to 5 set it.
                [4, 0, ..] => self.attributes.flags &= !Attributes::UNDERLINE,
                [4, 1..=5, ..] => self.attributes.flags |= Attributes::UNDERLINE,
                // Foreground, background and underline colour share their
                // forms (ITU-T T.416, 13.1.8, and xterm's ctlseqs). An
                // invalid colour is skipped, and the rest of the SGR goes on.
                [selector @ (38 | 48 | 58)] => {
                    if let Some(colour) = Self::colour_after(&mut groups) {
                        self.set_colour(*selector, colour);
                    }
                }
                [selector @ (38 | 48 | 58), rest @ ..] => {
                    if let Some(colour) = colour_of(rest) {
                        self.set_colour(*selector, colour);
                    }
                }
                _ => {}
            }
        }
    }

    fn set_colour(&mut self, selector: u16, colour: Color) {
        self.attributes = match selector {
            38 => self.attributes.with_foreground(colour),
            48 => self.attributes.with_background(colour),
            _ => self.attributes.with_underline_color(colour),
        };
    }

    /// The colour of 38, 48 or 58 in their semicolon form, taken from the
    /// parameters after it, as xterm reads it: `5;index` or `2;r;g;b`, a
    /// value the list ends before being 0. Another kind takes only itself.
    /// `None`, with what it named taken all the same, if the colour is out
    /// of range.
    fn colour_after<'a>(
        groups: &mut std::iter::Peekable<impl Iterator<Item = &'a [u16]>>,
    ) -> Option<Color> {
        let kind = match groups.peek() {
            Some([kind]) => *kind,
            _ => return None,
        };
        groups.next();
        let count = match kind {
            2 => 3,
            5 => 1,
            _ => return None,
        };
        let mut values = [0u16; 3];
        for value in values.iter_mut().take(count) {
            let Some([n]) = groups.peek() else {
                break;
            };
            *value = *n;
            groups.next();
        }
        let [a, b, c] = values;
        if kind == 5 {
            u8::try_from(a).ok().map(Color::Idx)
        } else {
            rgb(a, b, c)
        }
    }
}
