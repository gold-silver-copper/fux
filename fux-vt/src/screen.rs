use crate::{
    Attributes, Cell, Color, Error, Mark, Row, RowId, Window, grid::Grid, parser::Parameters,
};
use unicode_width::UnicodeWidthChar;

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
    mouse: MouseProtocolMode,
    encoding: MouseProtocolEncoding,
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
            mouse: MouseProtocolMode::None,
            encoding: MouseProtocolEncoding::Default,
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
    /// Retained allocation in cells, for capacity/plateau diagnostics.
    pub fn storage_cells(&self) -> usize {
        // Each is a Vec's capacity, far below the limit of a usize.
        self.primary
            .storage_cells()
            .saturating_add(self.alternate.storage_cells())
    }

    pub(crate) fn resize(&mut self, rows: u16, cols: u16) -> Result<(), Error> {
        if self.size() == (rows, cols) {
            return Ok(());
        }
        let version = self
            .version
            .checked_add(1)
            .ok_or(Error::IdentityExhausted)?;
        let mut next = self.next_id;
        let primary = self.primary.resized(rows, cols, &mut next, version)?;
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
                        if let Some(cell) = cells.get_mut(usize::from(col)) {
                            cell.append(c);
                        }
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
                    *cell = Cell::glyph(c, usize::from(width), attributes);
                }
                if width == 2
                    && let Some(cell) = cells.get_mut(i + 1)
                {
                    *cell = Cell::continuation();
                }
            });
            // Past the glyph; at the right edge it waits there to wrap.
            g.cursor.1 = g.cursor.1.saturating_add(width).min(g.cols.get());
        });
        Ok(())
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
                    if let Some(dst) = cells.get_mut(span) {
                        for (cell, byte) in dst.iter_mut().zip(run) {
                            *cell = Cell::ascii(*byte, attributes);
                        }
                    }
                });
                g.cursor.1 = end;
            });
            bytes = bytes.get(usize::from(count)..).unwrap_or_default();
        }
        Ok(())
    }

    pub(crate) fn control(&mut self, byte: u8) -> Result<(), Error> {
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
    pub(crate) fn escape(&mut self, intermediates: &[u8], byte: u8) -> Result<(), Error> {
        if !intermediates.is_empty() {
            return Ok(());
        }
        match byte {
            b'7' => self.save(),
            b'8' => self.restore(),
            b'=' => self.application_keypad = true,
            b'>' => self.application_keypad = false,
            b'M' => self.reverse_index()?,
            b'c' => {
                let (rows, cols) = self.size();
                let history = self.primary.history_limit;
                let mut next = self.next_id;
                let primary = Grid::new(rows, cols, history, &mut next, self.version)?;
                let alternate = Grid::new(rows, cols, 0, &mut next, self.version)?;
                self.primary = primary;
                self.alternate = alternate;
                self.next_id = next;
                self.alternate_active = false;
                self.attributes = Attributes::default();
                self.saved_attributes = Attributes::default();
                self.autowrap = true;
                self.application_cursor = false;
                self.application_keypad = false;
                self.hide_cursor = false;
                self.bracketed_paste = false;
                self.mouse = MouseProtocolMode::None;
                self.encoding = MouseProtocolEncoding::Default;
                self.structural = self.version;
            }
            _ => {}
        }
        Ok(())
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

    pub(crate) fn csi(
        &mut self,
        p: &Parameters,
        intermediates: &[u8],
        byte: u8,
    ) -> Result<Option<Vec<u8>>, Error> {
        let private = intermediates == b"?";
        if !intermediates.is_empty() && !private {
            return Ok(None);
        }
        if private && matches!(byte, b'h' | b'l') {
            for group in p.groups() {
                if let [n] = group {
                    self.mode(*n, byte == b'h')?;
                }
            }
            return Ok(None);
        }
        if private && !matches!(byte, b'J' | b'K') {
            return Ok(None);
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
            b'H' => self
                .grid_mut()
                .position(n.saturating_sub(1), p.first(1, 1).saturating_sub(1)),
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
                if mode <= 2 {
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
                5 => return Ok(Some(b"\x1b[0n".to_vec())),
                6 => {
                    return Ok(Some(
                        format!("\x1b[{};{}R", u32::from(row) + 1, u32::from(col) + 1).into_bytes(),
                    ));
                }
                _ => {}
            },
            b'c' if p.first(0, 0) == 0 => return Ok(Some(b"\x1b[?1;2c".to_vec())),
            _ => {}
        }
        Ok(None)
    }

    fn sgr(&mut self, p: &Parameters) {
        let mut groups = p.groups();
        while let Some(group) = groups.next() {
            match group {
                [0] => self.attributes = Attributes::default(),
                [1] => self.attributes.flags = self.attributes.flags & !3 | 1,
                [2] => self.attributes.flags = self.attributes.flags & !3 | 2,
                [3] => self.attributes.flags |= 4,
                [4] => self.attributes.flags |= 8,
                [7] => self.attributes.flags |= 16,
                [22] => self.attributes.flags &= !3,
                [23] => self.attributes.flags &= !4,
                [24] => self.attributes.flags &= !8,
                [27] => self.attributes.flags &= !16,
                [39] => self.attributes.foreground = Color::Default,
                [49] => self.attributes.background = Color::Default,
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
                [selector @ (38 | 48), rest @ ..] => {
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
                        if rest.len() > parts.len() {
                            continue;
                        }
                        if let Some(dst) = parts.get_mut(..rest.len()) {
                            dst.copy_from_slice(rest);
                        }
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
                    if *selector == 38 {
                        self.attributes.foreground = colour;
                    } else {
                        self.attributes.background = colour;
                    }
                }
                _ => {}
            }
        }
    }
}
