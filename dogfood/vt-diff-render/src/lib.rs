//! An incremental renderer: it keeps a model of what an outer terminal
//! shows, and given each new fux-vt screen writes the escape output that
//! takes the outer terminal from the last frame to this one.
//!
//! Rows are skipped by their identity and version (`Row::id`,
//! `Row::version`), a scroll up the screen reports (`Screen::scrolled_since`)
//! is sent as `CSI n S` when the row identities confirm it, changed cells
//! are sent as runs with the library's SGR writer
//! (`Attributes::write_sgr_change`), and blank runs as ECH or EL.
use std::collections::HashMap;

use fux_vt::{Attributes, CellRef, Cells, LineRendition, Mode, RowId, Screen, Size};

/// Two changed runs this close are sent as one: a cursor move costs about
/// as much as repainting a few cells.
const MERGE_GAP: usize = 4;

/// Counters of what the renderer did, for a report.
#[derive(Clone, Copy, Debug, Default)]
pub struct Stats {
    /// Frames rendered.
    pub frames: u64,
    /// Whole repaints (the first frame, a resize).
    pub repaints: u64,
    /// Scrolls sent as `CSI n S`.
    pub scrolls: u64,
    /// Rows those scrolls moved.
    pub scrolled_rows: u64,
    /// Rows compared cell by cell.
    pub rows_compared: u64,
    /// Rows skipped because their identity and version were unchanged.
    pub rows_skipped: u64,
}

/// What the outer terminal shows, as far as this renderer has painted it.
pub struct Renderer {
    size: Option<Size>,
    rows: Vec<Cells>,
    /// The screen row each model row was last painted from, and its version.
    painted: Vec<Option<(RowId, u64)>>,
    renditions: Vec<LineRendition>,
    pen: Attributes,
    /// The outer cursor, if known (unknown after a glyph in the last column).
    cursor: Option<(u16, u16)>,
    show_cursor: Option<bool>,
    reverse: Option<bool>,
    pub stats: Stats,
}

impl Default for Renderer {
    fn default() -> Self {
        Self::new()
    }
}

impl Renderer {
    /// A renderer that knows nothing of the outer terminal: its first frame
    /// clears it and paints everything.
    pub fn new() -> Self {
        Renderer {
            size: None,
            rows: Vec::new(),
            painted: Vec::new(),
            renditions: Vec::new(),
            pen: Attributes::default(),
            cursor: None,
            show_cursor: None,
            reverse: None,
            stats: Stats::default(),
        }
    }

    /// Appends to `out` what repaints the outer terminal from the last frame
    /// rendered to `screen`. Fails only if the model's cells can't be
    /// allocated.
    pub fn render(&mut self, screen: &Screen, out: &mut Vec<u8>) -> Result<(), fux_vt::Error> {
        self.stats.frames += 1;
        let size = screen.size();
        let (rows, cols) = (size.rows(), size.cols());
        if self.size != Some(size) {
            self.repaint(size, out)?;
        } else {
            self.scroll(screen, out)?;
        }
        let window = screen.window();
        for (y, row) in window.iter() {
            let now = Some((row.id(), row.version()));
            let Some(painted) = self.painted.get_mut(usize::from(y)) else {
                continue;
            };
            if *painted == now {
                self.stats.rows_skipped += 1;
                continue;
            }
            *painted = now;
            self.stats.rows_compared += 1;
            let rendition = row.rendition();
            let mut force = false;
            if self.renditions.get(usize::from(y)) != Some(&rendition) {
                self.move_to(y, 0, out);
                out.extend_from_slice(match rendition {
                    LineRendition::Single => b"\x1b#5",
                    LineRendition::DoubleWidth => b"\x1b#6",
                    LineRendition::DoubleHeightTop => b"\x1b#3",
                    LineRendition::DoubleHeightBottom => b"\x1b#4",
                });
                if let Some(r) = self.renditions.get_mut(usize::from(y)) {
                    *r = rendition;
                }
                force = true;
            }
            let width = match rendition.is_double() {
                true => (usize::from(cols) / 2).max(1),
                false => usize::from(cols),
            };
            let new = |x: usize| cell_at(&window, y, x);
            for (start, end) in self.changed_runs(y, width, force, &new) {
                self.paint_run(y, start, end, width, &new, out);
            }
        }
        self.finish(screen, rows, cols, out);
        Ok(())
    }

    /// Clears the outer terminal and the model, for a first frame or a new
    /// size.
    fn repaint(&mut self, size: Size, out: &mut Vec<u8>) -> Result<(), fux_vt::Error> {
        let (rows, cols) = (usize::from(size.rows()), usize::from(size.cols()));
        self.rows.truncate(rows);
        for row in &mut self.rows {
            row.reset(cols)?;
        }
        while self.rows.len() < rows {
            self.rows.push(Cells::new(cols)?);
        }
        self.painted = vec![None; rows];
        self.renditions = vec![LineRendition::Single; rows];
        self.set_pen(Attributes::default(), out);
        out.extend_from_slice(b"\x1b[H\x1b[2J");
        self.cursor = Some((0, 0));
        self.size = Some(size);
        self.stats.repaints += 1;
        Ok(())
    }

    /// Sends the largest block of rows that moved as one since the last
    /// frame, told by their identities, as a scroll of a region (DECSTBM
    /// and SU or SD; the whole screen needs no region), and moves the
    /// model with it.
    fn scroll(&mut self, screen: &Screen, out: &mut Vec<u8>) -> Result<(), fux_vt::Error> {
        let Some((top, bottom, by)) = moved_block(&self.painted, screen) else {
            return Ok(());
        };
        let last = self.rows.len().saturating_sub(1);
        let whole = top == 0 && bottom == last;
        // A scroll fills with the pen's background (bce): reset it first.
        self.set_pen(Attributes::default(), out);
        if !whole {
            out.extend_from_slice(format!("\x1b[{};{}r", top + 1, bottom + 1).as_bytes());
        }
        let n = by.unsigned_abs();
        out.extend_from_slice(format!("\x1b[{n}{}", if by > 0 { 'S' } else { 'T' }).as_bytes());
        if !whole {
            // DECSTBM homes the cursor.
            out.extend_from_slice(b"\x1b[r");
            self.cursor = None;
        }
        let cols = self.rows.first().map_or(0, Cells::len);
        let (Some(rows), Some(painted), Some(renditions)) = (
            self.rows.get_mut(top..=bottom),
            self.painted.get_mut(top..=bottom),
            self.renditions.get_mut(top..=bottom),
        ) else {
            return Ok(());
        };
        let len = rows.len();
        let blank = match by > 0 {
            true => {
                rows.rotate_left(n);
                painted.rotate_left(n);
                renditions.rotate_left(n);
                len - n..len
            }
            false => {
                rows.rotate_right(n);
                painted.rotate_right(n);
                renditions.rotate_right(n);
                0..n
            }
        };
        for y in blank {
            if let Some(row) = rows.get_mut(y) {
                row.reset(cols)?;
            }
            if let Some(p) = painted.get_mut(y) {
                *p = None;
            }
            if let Some(r) = renditions.get_mut(y) {
                *r = LineRendition::Single;
            }
        }
        self.stats.scrolls += 1;
        self.stats.scrolled_rows += n as u64;
        Ok(())
    }

    /// The runs of columns of row `y` that differ from the model, widened so
    /// that no wide glyph, old or new, is cut, and merged when close.
    fn changed_runs<'s>(
        &self,
        y: u16,
        width: usize,
        force: bool,
        new: &impl Fn(usize) -> CellRef<'s>,
    ) -> Vec<(usize, usize)> {
        let Some(old) = self.rows.get(usize::from(y)) else {
            return Vec::new();
        };
        let old_cell = |x: usize| old.get(x).unwrap_or_else(CellRef::wide_continuation);
        let mut runs: Vec<(usize, usize)> = Vec::new();
        let mut x = 0;
        while x < width {
            if !force && new(x) == old_cell(x) {
                x += 1;
                continue;
            }
            let mut start = x;
            let mut end = x + 1;
            // Widen over wide glyphs either frame has across the edges.
            while start > 0 && (new(start).is_wide_continuation() || old_cell(start).is_wide_continuation()) {
                start -= 1;
            }
            while end < width && (new(end - 1).is_wide() || old_cell(end - 1).is_wide()) {
                end += 1;
            }
            match runs.last_mut() {
                Some(last) if start <= last.1 + MERGE_GAP => last.1 = last.1.max(end),
                _ => runs.push((start, end)),
            }
            x = end;
        }
        runs
    }

    /// Paints columns `start..end` of row `y` from `new`, and copies them
    /// into the model.
    fn paint_run<'s>(
        &mut self,
        y: u16,
        start: usize,
        end: usize,
        width: usize,
        new: &impl Fn(usize) -> CellRef<'s>,
        out: &mut Vec<u8>,
    ) {
        let mut x = start;
        while x < end {
            let cell = new(x);
            if cell.is_wide_continuation() {
                x += 1;
                continue;
            }
            let Ok(col) = u16::try_from(x) else { return };
            if cell.contents().is_empty() && is_erased(cell.attributes()) {
                // A run of blanks in one colour: erased, not printed.
                let attributes = cell.attributes();
                let mut k = 1;
                while x + k < end && {
                    let next = new(x + k);
                    next.contents().is_empty() && next.attributes() == attributes
                } {
                    k += 1;
                }
                let to_end = x + k == end && end == width && (x + k..width).all(|c| new(c) == cell);
                self.move_to(y, col, out);
                self.set_pen(attributes, out);
                if to_end {
                    out.extend_from_slice(b"\x1b[K");
                } else {
                    out.extend_from_slice(format!("\x1b[{k}X").as_bytes());
                }
                if let Some(row) = self.rows.get_mut(usize::from(y)) {
                    row.fill(x..x + k, cell);
                }
                x += k;
                continue;
            }
            self.move_to(y, col, out);
            self.set_pen(cell.attributes(), out);
            out.extend_from_slice(cell.shown().as_bytes());
            let w = if cell.is_wide() { 2 } else { 1 };
            if let Some(row) = self.rows.get_mut(usize::from(y)) {
                row.set(x, cell);
                if w == 2 {
                    row.set(x + 1, CellRef::wide_continuation());
                }
            }
            x += w;
            // A glyph in the last column leaves the cursor there, its wrap
            // pending; any other moves it on.
            self.cursor = match x < width {
                true => u16::try_from(x).ok().map(|c| (y, c)),
                false => None,
            };
        }
    }

    /// The modes and the cursor, last.
    fn finish(&mut self, screen: &Screen, rows: u16, cols: u16, out: &mut Vec<u8>) {
        let reverse = screen.mode(Mode::ReverseVideo);
        if self.reverse != Some(reverse) {
            out.extend_from_slice(if reverse { b"\x1b[?5h" } else { b"\x1b[?5l" });
            self.reverse = Some(reverse);
        }
        let (cy, cx) = screen.cursor_position();
        let width = match screen.line_rendition(cy).is_double() {
            true => (cols / 2).max(1),
            false => cols,
        };
        if screen.pending_wrap() && cx + 1 == width && cy < rows {
            // Only printing the last glyph again sets the wrap pending.
            let window = screen.window();
            let mut x = usize::from(cx);
            if cell_at(&window, cy, x).is_wide_continuation() {
                x = x.saturating_sub(1);
            }
            let cell = cell_at(&window, cy, x);
            if let Ok(col) = u16::try_from(x) {
                self.move_to(cy, col, out);
                self.set_pen(cell.attributes(), out);
                out.extend_from_slice(cell.shown().as_bytes());
                self.cursor = None;
            }
        } else {
            self.move_to(cy, cx, out);
        }
        let show = screen.mode(Mode::ShowCursor);
        if self.show_cursor != Some(show) {
            out.extend_from_slice(if show { b"\x1b[?25h" } else { b"\x1b[?25l" });
            self.show_cursor = Some(show);
        }
    }

    fn set_pen(&mut self, to: Attributes, out: &mut Vec<u8>) {
        to.write_sgr_change(self.pen, out, true);
        self.pen = to;
    }

    /// Moves the outer cursor to row `y`, column `x`, by the shortest of
    /// CUP, CR and CUF.
    fn move_to(&mut self, y: u16, x: u16, out: &mut Vec<u8>) {
        match self.cursor {
            Some(at) if at == (y, x) => {}
            Some((cy, _)) if cy == y && x == 0 => out.push(b'\r'),
            Some((cy, cx)) if cy == y && x > cx => {
                let n = x - cx;
                out.extend_from_slice(if n == 1 { b"\x1b[C".to_vec() } else { format!("\x1b[{n}C").into_bytes() }.as_slice());
            }
            _ => out.extend_from_slice(format!("\x1b[{};{}H", y + 1, x + 1).as_bytes()),
        }
        self.cursor = Some((y, x));
    }
}

/// The longest block of the screen's rows that each sit `by` rows from
/// where they were painted (`by` > 0: they moved up), as the region that
/// scrolls them there: `(top, bottom, by)`, inclusive. Two rows at least,
/// or a scroll costs more than it saves.
fn moved_block(painted: &[Option<(RowId, u64)>], screen: &Screen) -> Option<(usize, usize, isize)> {
    let was: HashMap<RowId, usize> =
        painted.iter().enumerate().filter_map(|(y, p)| p.map(|(id, _)| (id, y))).collect();
    let offsets: Vec<Option<isize>> = screen
        .window()
        .iter()
        .map(|(y, row)| {
            let old = was.get(&row.id())?;
            let by = isize::try_from(*old).ok()? - isize::try_from(y).ok()?;
            (by != 0).then_some(by)
        })
        .collect();
    let mut best: Option<(usize, usize, isize)> = None;
    let mut y = 0;
    while y < offsets.len() {
        let Some(Some(by)) = offsets.get(y).copied() else {
            y += 1;
            continue;
        };
        let start = y;
        while offsets.get(y + 1).copied().flatten() == Some(by) {
            y += 1;
        }
        if y - start + 1 >= 2 && best.is_none_or(|(s, e, _)| y - start > e - s) {
            best = Some((start, y, by));
        }
        y += 1;
    }
    // The block and the rows it came from make the region.
    let (start, end, by) = best?;
    let n = by.unsigned_abs();
    match by > 0 {
        true => Some((start, end + n, by)),
        false => Some((start.checked_sub(n)?, end, by)),
    }
}

/// The cell at row `y`, column `x` of `window`; a wide glyph the last
/// column clips reads as a blank.
fn cell_at<'s>(window: &fux_vt::Window<'s>, y: u16, x: usize) -> CellRef<'s> {
    u16::try_from(x)
        .ok()
        .and_then(|x| window.cell(y, x))
        .unwrap_or_else(|| CellRef::new("", false, Attributes::default()))
}

/// Whether `a` is what an erase leaves: colours only.
fn is_erased(a: Attributes) -> bool {
    a == Attributes::new(a.foreground(), a.background())
}

/// What a cell looks like: `CellRef`'s `==` tells a blank from a space in
/// the same style, which look the same. A blank with renditions is reachable
/// only by overwriting half of a wide glyph, so a renderer paints a space.
fn looks(c: CellRef<'_>) -> (&str, Attributes, bool, bool) {
    (c.shown(), c.attributes(), c.is_wide(), c.is_wide_continuation())
}

/// The first way `b` doesn't show what `a` shows, if any: the size, a cell,
/// a line's rendition, the cursor, its pending wrap or its visibility.
pub fn first_difference(a: &Screen, b: &Screen) -> Option<String> {
    if a.size() != b.size() {
        return Some(format!("size {:?} vs {:?}", a.size(), b.size()));
    }
    let (wa, wb) = (a.window(), b.window());
    for y in 0..wa.rows() {
        if a.line_rendition(y) != b.line_rendition(y) {
            return Some(format!("row {y}: rendition {:?} vs {:?}", a.line_rendition(y), b.line_rendition(y)));
        }
        // A double-width row shows half its columns; what an erase leaves
        // past them depends on the order of the erase and the DECDWL.
        let held = match a.line_rendition(y).is_double() {
            true => (wa.cols() / 2).max(1),
            false => wa.cols(),
        };
        for x in 0..held {
            let (ca, cb) = (wa.cell(y, x), wb.cell(y, x));
            if ca.map(looks) != cb.map(looks) {
                return Some(format!("row {y} col {x}: {ca:?} vs {cb:?}"));
            }
        }
    }
    if a.cursor_position() != b.cursor_position() {
        return Some(format!("cursor {:?} vs {:?}", a.cursor_position(), b.cursor_position()));
    }
    // A wrap pending anywhere but a row's last column (fux-vt keeps it
    // through a scroll that brings a row of another width under the
    // cursor) is one no output but that scroll can make: not compared.
    let (cy, cx) = a.cursor_position();
    let width = match a.line_rendition(cy).is_double() {
        true => (wa.cols() / 2).max(1),
        false => wa.cols(),
    };
    if a.pending_wrap() != b.pending_wrap() && cx + 1 == width {
        return Some(format!("pending wrap {} vs {}", a.pending_wrap(), b.pending_wrap()));
    }
    for mode in [Mode::ShowCursor, Mode::ReverseVideo] {
        if a.mode(mode) != b.mode(mode) {
            return Some(format!("{mode:?} {} vs {}", a.mode(mode), b.mode(mode)));
        }
    }
    None
}
