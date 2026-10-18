//! Painting a client's screen: compose a grid of cells from its panes, the
//! separators, the bar and any overlay; diff it against what the client has;
//! send only the changed runs, inside synchronized output. A keystroke's
//! echo, when it is all that changed, is written as a terminal shows it
//! typed, without the synchronized envelope (`echo`). Every paint leaves
//! the terminal's attributes at their default, which the next one assumes.
use crate::act::{List, Panel};
use crate::id::ClientId;
use crate::id::PaneId;
use crate::layout::{Axis, Placement, Rect};
use crate::overlay;
use crate::session::Session;
use crate::view::{Mode, View};
use fux_vt::{Attributes, CellRef, Cells, Color, CursorShape, Hyperlink, Rgb, Row, Screen};
use std::borrow::Cow;
use std::io::Write;

#[derive(Clone, Debug)]
pub struct Grid {
    pub rows: u16,
    pub cols: u16,
    /// Row after row; clusters too long to hold inline are kept whole.
    pub cells: Cells,
    /// Each cell's hyperlink.
    links: Links,
    /// Where the terminal cursor is shown, if it is.
    pub cursor: Option<(u16, u16)>,
    /// The modes the client's terminal is set to.
    pub modes: Modes,
    /// Whether the client's terminal draws underline styles (`outer`):
    /// painted as they are, else as plain underlines (`sgr`).
    pub underline_styles: bool,
    /// What composed the grid, if it can say (`Memo`). Not what the grid
    /// shows: grids compare equal whatever composed them.
    memo: Option<Memo>,
}

/// The modes a paint sets on the client's terminal from its focused pane,
/// each a field here and a line in `Modes::diff`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modes {
    /// The cursor's shape (DECSCUSR); `Initial` is the terminal's own.
    pub cursor_shape: CursorShape,
    /// The mouse tracking asked for (1000, 1002 or 1003, always
    /// SGR-encoded), 0 for none: the focused pane's program's, while it
    /// has the keys ([`mouse_level`]).
    pub mouse: u16,
}

impl Modes {
    /// Appends what turns the terminal's modes, `shown` if `known`, into
    /// these; its cursor's shape is `shown`'s either way, as a resize,
    /// which forgets what it shows, leaves the shape.
    fn diff(&self, shown: &Modes, known: bool, out: &mut Vec<u8>) {
        if !known || shown.mouse != self.mouse {
            // Off first, as a terminal not known may have any on.
            out.extend_from_slice(MOUSE_OFF.as_bytes());
            if self.mouse != 0 {
                let _ = write!(out, "\x1b[?{}h\x1b[?1006h", self.mouse);
            }
        }
        cursor_shape(shown.cursor_shape, self.cursor_shape, out);
    }
}

/// Appends what turns the cursor of a client's terminal from `was` into
/// `shape` (DECSCUSR), nothing if they are the same: its Ps; for
/// `Initial`, the terminal's own, 0, which most terminals take for theirs,
/// then 7, xterm's initial resources (ctlseqs), as xterm takes 0 for a
/// blinking block and the others ignore 7.
pub fn cursor_shape(was: CursorShape, shape: CursorShape, out: &mut Vec<u8>) {
    if was == shape {
        return;
    }
    if shape == CursorShape::Initial {
        out.extend_from_slice(b"\x1b[0 q\x1b[7 q");
    } else {
        let _ = write!(out, "\x1b[{} q", shape.number());
    }
}

/// A picture a client is shown: the pane it is in and its number there,
/// the cells of the client's screen it takes (top row, first column, rows,
/// columns), and the part of its pixels those show (left, top, width,
/// height).
#[derive(Clone, Debug)]
pub struct Visible {
    pub pane: PaneId,
    pub id: u64,
    pub area: (u16, u16, u16, u16),
    pub crop: (u32, u32, u32, u32),
    pub picture: std::sync::Arc<fux_vt::sixel::Picture>,
}

/// The same picture in the same place, showing the same part.
impl PartialEq for Visible {
    fn eq(&self, other: &Visible) -> bool {
        (self.pane, self.id, self.area, self.crop) == (other.pane, other.id, other.area, other.crop)
    }
}

/// The pictures client `client`'s panes, placed as `placement` has them,
/// show on its screen: none unless its terminal draws sixel, nothing covers
/// its panes (an overlay, copy mode), and a picture was laid out in its
/// cell size. Each is clipped to its pane.
pub fn pictures(session: &Session, client: ClientId, placement: &Placement) -> Vec<Visible> {
    let Some(view) = session.views.get(&client) else {
        return Vec::new();
    };
    let graphics = view.terminal.graphics();
    let Some(cell) = graphics
        .cell
        .filter(|_| graphics.sixel && view.mode.is_normal())
    else {
        return Vec::new();
    };
    let mut visible = Vec::new();
    for &(id, rect) in &placement.panes {
        let Some(pane) = session.panes.get(&id) else {
            continue;
        };
        for shown in pane.screen().pictures().filter(|s| s.cell == cell) {
            // The rows of the pane it is on, and the columns.
            let end = shown
                .row
                .saturating_add(i64::from(shown.rows))
                .min(i64::from(rect.h()));
            let start = shown.row.max(0);
            let right = shown.col.saturating_add(shown.cols).min(rect.w());
            let (Ok(top), Ok(bottom)) = (u16::try_from(start), u16::try_from(end)) else {
                continue;
            };
            if bottom <= top || right <= shown.col {
                continue;
            }
            let (rows, cols) = (bottom.saturating_sub(top), right.saturating_sub(shown.col));
            let skipped = u32::try_from(start.saturating_sub(shown.row)).unwrap_or(0);
            let (width, height) = (u32::from(cell.0), u32::from(cell.1));
            visible.push(Visible {
                pane: id,
                id: shown.id,
                area: (
                    rect.y().saturating_add(top),
                    rect.x().saturating_add(shown.col),
                    rows,
                    cols,
                ),
                crop: (
                    0,
                    skipped.saturating_mul(height),
                    u32::from(cols).saturating_mul(width),
                    u32::from(rows).saturating_mul(height),
                ),
                picture: std::sync::Arc::clone(shown.picture),
            });
        }
    }
    visible
}

/// The sixel data of the pictures a client was last sent, each by its pane,
/// number and crop: a picture that only moves is sent again with the same
/// data at its new place, encoded once (`threats.md` B08). At most
/// `SIXEL_BYTES` are kept; a picture past them is encoded each time it is
/// sent.
#[derive(Debug, Default)]
pub struct Sixels {
    kept: Vec<(SixelKey, Vec<u8>)>,
    /// How many pictures were encoded: for the tests.
    #[cfg(test)]
    encoded: usize,
}

/// A picture's pane, number and crop: what its sixel data is kept by.
type SixelKey = (PaneId, u64, (u32, u32, u32, u32));

/// The most sixel data a client keeps to send again: 16 MiB, a 4K screen
/// of one picture's worth in 256 colours at half its 12 bytes a pixel.
const SIXEL_BYTES: usize = 16 << 20;

impl Sixels {
    /// What shows `picture` on a terminal that draws sixel, its cursor
    /// kept: at its area's top left, its crop of pixels, from the data
    /// kept for its crop or encoded now.
    pub fn picture_into(&mut self, picture: &Visible, out: &mut Vec<u8>) {
        let (row, col, _, _) = picture.area;
        out.extend_from_slice(b"\x1b7");
        goto(out, row, col);
        let key = (picture.pane, picture.id, picture.crop);
        if let Some((_, data)) = self.kept.iter().find(|(k, _)| *k == key) {
            out.extend_from_slice(data);
        } else {
            let mut data = Vec::new();
            fux_vt::sixel::encode(&picture.picture, picture.crop, &mut data);
            #[cfg(test)]
            {
                self.encoded = self.encoded.saturating_add(1);
            }
            out.extend_from_slice(&data);
            let held: usize = self.kept.iter().map(|(_, d)| d.len()).sum();
            if held.saturating_add(data.len()) <= SIXEL_BYTES {
                self.kept.push((key, data));
            }
        }
        out.extend_from_slice(b"\x1b8");
    }

    /// Lets go of the data of every picture not in `shown`, the pictures
    /// the client now shows.
    pub fn keep(&mut self, shown: &[Visible]) {
        self.kept
            .retain(|(key, _)| shown.iter().any(|p| (p.pane, p.id, p.crop) == *key));
    }
}

/// Grids are equal when they show the same.
impl PartialEq for Grid {
    fn eq(&self, other: &Grid) -> bool {
        self.head() == other.head() && self.cells == other.cells
    }
}
impl Eq for Grid {}

/// What composed a grid in normal mode: its frame, and the
/// identity and version of each pane row put in it. A row of a fux-vt
/// screen keeps its identity while it is retained, no later row takes it,
/// and its version changes with every edit of its cells, soft wrap or
/// links; pane ids are never reused. So a grid composed again for its
/// memo's frame needs only the pane rows whose keys changed put again
/// (`Memo::update`), and two grids of one frame show the same cells on a
/// row of the pane area whose pane rows have the same keys in both
/// (`SameFrame`).
#[derive(Clone, Debug)]
struct Memo {
    frame: Frame,
    /// For each of the frame's panes, in its order: each of its window's
    /// rows on the grid, as many as the frame gives it.
    keys: Vec<Vec<RowKey>>,
}

/// A pane row's identity and version, `None` where the window has no row.
type RowKey = Option<(fux_vt::RowId, u64)>;

/// Everything about a grid composed in normal mode but its panes' rows: its
/// size, where each pane and separator is and its window's size, and the
/// focused pane, which colours the separators. Only for a tab with panes
/// (one without shows a hint, and focuses none) none of which is in
/// colours of its own (`recolour`), whose rows depend on more than their
/// keys.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Frame {
    rows: u16,
    cols: u16,
    panes: Vec<(PaneId, Rect, (u16, u16))>,
    separators: Vec<crate::layout::Separator>,
    focus: PaneId,
}

impl Frame {
    fn of(
        session: &Session,
        view: &View,
        placement: &Placement,
        focus: Option<PaneId>,
    ) -> Option<Frame> {
        if !view.mode.is_normal() {
            return None;
        }
        let mut panes = Vec::with_capacity(placement.panes.len());
        for (id, rect) in &placement.panes {
            let screen = session.panes.get(id)?.screen();
            if own_colours(screen) {
                return None;
            }
            panes.push((*id, *rect, screen.size().into()));
        }
        Some(Frame {
            rows: view.rows,
            cols: view.cols,
            panes,
            separators: placement.separators.clone(),
            focus: focus?,
        })
    }
}

impl Memo {
    /// Puts again on `grid`, which this memo's frame composed and composes
    /// now, the pane rows that changed since: the grid's memo then.
    fn update(mut self, grid: &mut Grid, session: &Session) -> Memo {
        for ((id, rect, _), keys) in self.frame.panes.iter().zip(&mut self.keys) {
            let Some(pane) = session.panes.get(id) else {
                continue;
            };
            let window = pane.screen().window();
            let lines = (0..window.rows()).zip(rect.lines());
            for ((y, line), key) in lines.zip(keys.iter_mut()) {
                let row = window.row(y);
                let now = row.map(|r| (r.id(), r.version()));
                if now != *key {
                    draw_row(grid, *id, row, (line, window.cols()), None);
                    *key = now;
                }
            }
        }
        self
    }
}

/// Two grids composed for one frame (`Memo`): proof that their keys can be
/// compared.
struct SameFrame<'a> {
    frame: &'a Frame,
    keys: (&'a [Vec<RowKey>], &'a [Vec<RowKey>]),
}

impl<'a> SameFrame<'a> {
    fn of(a: &'a Grid, b: &'a Grid) -> Option<SameFrame<'a>> {
        let (a, b) = (a.memo.as_ref()?, b.memo.as_ref()?);
        (a.frame == b.frame).then_some(SameFrame {
            frame: &a.frame,
            keys: (&a.keys, &b.keys),
        })
    }
    /// Whether row `y` is in the pane area and has the same pane rows in
    /// both grids: then it shows the same cells.
    fn same_row(&self, y: u16) -> bool {
        // The bar's row is not memo'd.
        y < self.frame.rows.saturating_sub(1)
            && (self.frame.panes.iter())
                .zip(self.keys.0.iter().zip(self.keys.1))
                .all(|((_, rect, _), (a, b))| {
                    let i = y.checked_sub(rect.y()).filter(|i| *i < rect.h());
                    i.is_none_or(|i| a.get(usize::from(i)) == b.get(usize::from(i)))
                })
    }
}

/// Puts a pane's window row on the grid at `line`, the pane's row there; the
/// window is `window_cols` wide. What the row does not cover of the pane's
/// width is blanked. All of it is in the pane's own colours if it has them.
fn draw_row(
    grid: &mut Grid,
    pane: PaneId,
    row: Option<Row<'_>>,
    (line, window_cols): (Rect, u16),
    colours: Option<&Screen>,
) {
    let (gy, gx, width) = (line.y(), line.x(), line.w().min(window_cols));
    // The pane's own row, well formed, goes whole onto the cells; one drawn
    // twice as wide (DECDWL, DECDHL) covers the width itself.
    let mut len = row.map_or(0, |row| row.len());
    if let Some(row) = row {
        if row.rendition().is_double() {
            grid.put_doubled_row(gy, gx, pane, row, width);
            len = usize::from(width);
        } else {
            grid.put_row(gy, gx, pane, row, width);
        }
    }
    // At most `width`, which is at most the line's.
    let end = u16::try_from(len).unwrap_or(width).min(width);
    grid.blank(line.split(Axis::Horizontal, end).1);
    // A wide glyph in the window's last column, if the line shows it, is
    // cut off, as `Window::cell` has it.
    if let Some(last) = window_cols.checked_sub(1)
        && row
            .and_then(|r| r.cell(usize::from(last)))
            .is_some_and(|c| c.is_wide())
        && let Some((gy, x)) = line.at(0, last)
    {
        grid.put(gy, x, CellRef::default());
    }
    if let Some(screen) = colours {
        grid.recolour(gy, gx, line.w(), screen);
    }
}

/// Each cell's hyperlink (OSC 8), with the pane whose it is: none past
/// the end, so that a grid none of whose cells has one, as most, holds
/// nothing; and two grids whose cells have the same links are equal.
#[derive(Clone, Debug, Default)]
struct Links(Vec<Option<(PaneId, Hyperlink)>>);

impl Links {
    /// The link of cell `i`.
    fn get(&self, i: usize) -> Option<(PaneId, &Hyperlink)> {
        let (pane, link) = self.0.get(i)?.as_ref()?;
        Some((*pane, link))
    }
    /// Gives cell `i` of a grid of `len` cells `link`.
    fn set(&mut self, i: usize, link: Option<(PaneId, &Hyperlink)>, len: usize) {
        if link.is_some() && self.0.is_empty() {
            self.0.resize(len, None);
        }
        if let Some(at) = self.0.get_mut(i) {
            *at = link.map(|(pane, link)| (pane, link.clone()));
        }
    }
    /// Cells `range` have no link. Inlined, as most grids have none: the
    /// paths that set cells cost no call for them.
    #[inline(always)]
    fn unlink(&mut self, range: std::ops::Range<usize>) {
        if !self.0.is_empty() {
            self.unlink_some(range);
        }
    }
    /// `unlink` for a grid whose cells may have links.
    #[inline(never)]
    fn unlink_some(&mut self, range: std::ops::Range<usize>) {
        if let Some(run) = self.0.get_mut(range) {
            run.fill(None);
        }
    }
    /// Whether no cell has a link, as far as can be told without reading
    /// them all: `false` if one may.
    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl PartialEq for Links {
    fn eq(&self, other: &Links) -> bool {
        let (long, short) = if self.0.len() < other.0.len() {
            (&other.0, &self.0)
        } else {
            (&self.0, &other.0)
        };
        let short = short.iter().chain(std::iter::repeat(&None));
        long.iter().zip(short).all(|(a, b)| a == b)
    }
}
impl Eq for Links {}

impl Grid {
    /// A blank grid `rows` by `cols`; one of no cells if that is more than
    /// fux-vt's cells hold (`Cells::new`), which draws nothing.
    pub fn new(rows: u16, cols: u16) -> Grid {
        let (rows, cols, cells) = match Cells::new(cell_count(rows, cols)) {
            Ok(cells) => (rows, cols, cells),
            Err(_) => (0, 0, Cells::default()),
        };
        Grid {
            rows,
            cols,
            cells,
            links: Links::default(),
            cursor: None,
            modes: Modes::default(),
            underline_styles: false,
            memo: None,
        }
    }
    /// The hyperlink of the cell at `y`, `x`, and the pane whose it is.
    pub fn link(&self, y: u16, x: u16) -> Option<(PaneId, &Hyperlink)> {
        self.links.get(self.index(y, x)?)
    }
    /// Makes the grid `rows` by `cols`, resizing its cells only if the size
    /// changed, so that a grid composed into again allocates nothing; and
    /// blanks it if `blank`, as a resized grid is anyway.
    fn reset(&mut self, rows: u16, cols: u16, blank: bool) {
        self.links = Links::default();
        if (self.rows, self.cols) == (rows, cols) {
            if blank {
                self.cells.fill(0..self.cells.len(), CellRef::default());
            }
        } else {
            (self.rows, self.cols) = (rows, cols);
            let blank = CellRef::default();
            // Past what fux-vt's cells hold, a grid of none, which draws
            // nothing.
            let made = (self.cells.resize(0, blank))
                .and_then(|()| self.cells.resize(cell_count(rows, cols), blank));
            if made.is_err() {
                (self.rows, self.cols) = (0, 0);
            }
        }
        self.cursor = None;
        self.modes = Modes::default();
        self.memo = None;
    }
    fn index(&self, y: u16, x: u16) -> Option<usize> {
        (y < self.rows && x < self.cols)
            .then(|| cell_count(y, self.cols).saturating_add(usize::from(x)))
    }
    pub fn get(&self, y: u16, x: u16) -> Option<CellRef<'_>> {
        self.index(y, x).and_then(|i| self.cells.get(i))
    }
    /// Whether the cells of `area` (top row, first column, rows, columns)
    /// are the same as `other`'s.
    pub fn same_in(&self, other: &Grid, (row, col, rows, cols): (u16, u16, u16, u16)) -> bool {
        fn shows(c: CellRef<'_>) -> (&str, Attributes) {
            (c.contents(), c.attributes())
        }
        (row..row.saturating_add(rows)).all(|y| {
            (col..col.saturating_add(cols))
                .all(|x| self.get(y, x).map(shows) == other.get(y, x).map(shows))
        })
    }
    /// Takes it that the cells of `area` show something no composed grid
    /// does, as a picture's pixels: the next paint against it paints them
    /// all again.
    pub fn forget_area(&mut self, (row, col, rows, cols): (u16, u16, u16, u16)) {
        self.memo = None;
        let unlike = CellRef::new(fux_vt::PLACEHOLDER, false, Attributes::default());
        for y in row..row.saturating_add(rows) {
            for x in col..col.saturating_add(cols) {
                if let Some(i) = self.index(y, x) {
                    self.cells.set(i, unlike);
                }
            }
        }
    }
    /// Forgets what composed the grid (`Memo`): it shows the same, and is
    /// composed whole next time and painted by comparing every row. For
    /// the oracles that check the memo against working without it.
    #[doc(hidden)]
    pub fn forget_memo(&mut self) {
        self.memo = None;
    }
    /// All the grid shows but its cells.
    fn head(&self) -> impl PartialEq + '_ {
        (
            (self.rows, self.cols, self.cursor),
            (self.modes, self.underline_styles),
            &self.links,
        )
    }
    /// Whether the grid shows what `other` shows: the same as `==`, without
    /// comparing the rows their memos show are the same (`SameFrame`).
    pub fn same_as(&self, other: &Grid) -> bool {
        if self.head() != other.head() {
            return false;
        }
        let memo = SameFrame::of(self, other);
        (0..self.rows)
            .all(|y| memo.as_ref().is_some_and(|m| m.same_row(y)) || self.row_eq(other, y))
    }
    /// Whether row `y` has the same cells as `other`'s, a grid of its size:
    /// compared by `Cells::range_eq`, without reading each cell's text.
    fn row_eq(&self, other: &Grid, y: u16) -> bool {
        self.cells.range_eq(&other.cells, self.span(y))
    }
    /// Whether the cell at (`y`, `x`) is the same as `other`'s, a grid of
    /// its size, as `row_eq` compares them.
    fn cell_eq(&self, other: &Grid, y: u16, x: u16) -> bool {
        let i = cell_count(y, self.cols).saturating_add(usize::from(x));
        self.cells.range_eq(&other.cells, i..i.saturating_add(1))
    }
    /// The cells of row `y`; none past the last row.
    pub fn row(&self, y: u16) -> impl Iterator<Item = CellRef<'_>> + Clone {
        self.cells
            .range(if y < self.rows { self.span(y) } else { 0..0 })
    }
    /// Where row `y` is in `cells`, were the grid that tall.
    fn span(&self, y: u16) -> std::ops::Range<usize> {
        let start = cell_count(y, self.cols);
        start..start.saturating_add(usize::from(self.cols))
    }
    /// Copies the first `width` cells of pane `pane`'s row `row` into row `y`
    /// from column `x`, with their links, clipped at the grid's edge,
    /// without `set`'s repairs: for a run of one well-formed row onto blank
    /// cells, which keeps its wide glyphs whole by itself.
    fn put_row(&mut self, y: u16, x: u16, pane: PaneId, row: Row<'_>, width: u16) {
        let Some(start) = self.index(y, x) else {
            return;
        };
        let room = usize::from(self.cols.saturating_sub(x).min(width));
        for (i, cell) in (start..).zip(row.cells().take(room)) {
            self.cells.set(i, cell);
        }
        if !row.has_links() {
            self.links.unlink(start..start.saturating_add(room));
            return;
        }
        let len = self.cells.len();
        for (i, col) in (start..).zip(0..room) {
            let link = row.link(col).map(|link| (pane, link));
            self.links.set(i, link, len);
        }
    }
    /// `put_row` for a row drawn twice as wide (`fux_vt::LineRendition`),
    /// as no terminal can draw half a line so: each cell takes two
    /// columns, its glyph and a blank in its colours, a wide glyph four;
    /// what the doubled cells do not reach of `width` is blank. A double
    /// height half is drawn as double width.
    fn put_doubled_row(&mut self, y: u16, x: u16, pane: PaneId, row: Row<'_>, width: u16) {
        let Some(start) = self.index(y, x) else {
            return;
        };
        let room = usize::from(self.cols.saturating_sub(x).min(width));
        let len = self.cells.len();
        for at in 0..room {
            let (col, second) = (at / 2, at % 2 == 1);
            let i = start.saturating_add(at);
            let Some(cell) = row.cell(col) else {
                self.cells.set(i, CellRef::default());
                self.links.unlink(i..i.saturating_add(1));
                continue;
            };
            // A wide glyph's two halves take the first two of its four
            // columns, if both fit.
            let fits = at.saturating_add(1) < room;
            let shown = match (cell.is_wide(), second) {
                (true, false) if fits => Some(cell),
                (true, true) => row.cell(col.saturating_add(1)),
                (false, false) if !cell.is_wide_continuation() => Some(cell),
                _ => None,
            };
            match shown {
                Some(shown) => {
                    self.cells.set(i, shown);
                }
                None => {
                    self.cells.set(i, CellRef::default());
                    self.cells.set_attributes(i, cell.attributes());
                }
            }
            let link = row.link(col).map(|link| (pane, link));
            self.links.set(i, link, len);
        }
    }
    /// Draws the first `width` cells of row `y` from column `x`, a pane's
    /// row as `draw_row` put it, blanks too, in the colours its program set
    /// (`pane_colours`): so a pane whose program changed a palette entry,
    /// or its foreground or background, looks through fux as it would
    /// directly in a terminal, and the client's own palette is never
    /// changed (fux sends it no OSC 4, 10 or 11). Each pane's colours are
    /// its own.
    fn recolour(&mut self, y: u16, x: u16, width: u16, screen: &Screen) {
        let Some(start) = self.index(y, x) else {
            return;
        };
        let room = usize::from(self.cols.saturating_sub(x).min(width));
        for i in start..start.saturating_add(room) {
            let Some(cell) = self.cells.get(i).filter(|c| !c.is_wide_continuation()) else {
                continue;
            };
            let attributes = cell.attributes();
            let colours = pane_colours(attributes, screen);
            if colours != attributes {
                self.cells.set_attributes(i, colours);
            }
        }
    }
    /// Sets one cell as it is, without `set`'s repairs.
    fn put(&mut self, y: u16, x: u16, cell: CellRef<'_>) {
        if let Some(i) = self.index(y, x) {
            self.cells.set(i, cell);
            self.links.unlink(i..i.saturating_add(1));
        }
    }
    /// Blanks `area`, clipped at the grid's edge.
    fn blank(&mut self, area: Rect) {
        let count = usize::from(area.right().min(self.cols).saturating_sub(area.x()));
        for y in area.rows() {
            let Some(start) = self.index(y, area.x()) else {
                return;
            };
            let cells = start..start.saturating_add(count);
            self.cells.fill(cells.clone(), CellRef::default());
            self.links.unlink(cells);
        }
    }
    /// Sets a cell, keeping wide glyphs whole: overwriting either half of
    /// one blanks the other, as a terminal would.
    fn set(&mut self, y: u16, x: u16, cell: CellRef<'_>) {
        let Some(index) = self.index(y, x) else {
            return;
        };
        let (was_wide, was_continuation) = self.cells.get(index).map_or((false, false), |old| {
            (old.is_wide(), old.is_wide_continuation())
        });
        if was_continuation
            && !cell.is_wide_continuation()
            && let Some(leader) = index.checked_sub(1).filter(|_| x > 0)
            && self.cells.get(leader).is_some_and(|c| c.is_wide())
        {
            self.cells.set(leader, CellRef::default());
            self.links.unlink(leader..index);
        }
        if was_wide
            && !cell.is_wide()
            && let Some(rest) = x.checked_add(1).and_then(|x| self.index(y, x))
            && self
                .cells
                .get(rest)
                .is_some_and(|c| c.is_wide_continuation())
        {
            self.cells.set(rest, CellRef::default());
            self.links.unlink(rest..rest.saturating_add(1));
        }
        self.cells.set(index, cell);
        self.links.unlink(index..index.saturating_add(1));
    }
    /// The text of a row, trailing blanks trimmed: for `capture-client`.
    pub fn row_text(&self, y: u16) -> String {
        let row = self.row(y).filter(|c| !c.is_wide_continuation());
        row.map(|c| c.shown())
            .collect::<String>()
            .trim_end()
            .to_owned()
    }

    /// Writes `text` from (y, x), clipped at `limit`, wide glyphs whole; the
    /// column after it is returned.
    fn text(&mut self, y: u16, x: u16, text: &str, style: Attributes, limit: u16) -> u16 {
        let mut x = x;
        // The cell last written, where its cluster starts in `text`, and
        // whether it is wide: a char of no width joins that cell's cluster,
        // as fux-vt's `Screen::print` appends one to the cell before the
        // cursor, on a wide glyph's first half.
        let mut last: Option<(u16, usize, bool)> = None;
        for (at, c) in text.char_indices() {
            if c.is_control() {
                last = None;
                continue;
            }
            let width = cells(c);
            if width == 0 {
                let end = at.saturating_add(c.len_utf8());
                if let Some((x, start, wide)) = last
                    && let Some(cluster) = text.get(start..end)
                {
                    self.set(y, x, CellRef::new(cluster, wide, style));
                }
                continue;
            }
            let Some(end) = x
                .checked_add(width)
                .filter(|end| *end <= limit.min(self.cols))
            else {
                break;
            };
            let mut buffer = [0u8; 4];
            let cell = CellRef::new(c.encode_utf8(&mut buffer), width == 2, style);
            self.set(y, x, cell);
            last = Some((x, at, width == 2));
            if width == 2
                && let Some(second) = x.checked_add(1)
            {
                self.set(y, second, CellRef::wide_continuation());
            }
            x = end;
        }
        x
    }

    fn fill(&mut self, area: Rect, style: Attributes) {
        let blank = CellRef::new(" ", false, style);
        for y in area.rows() {
            for x in area.cols() {
                self.set(y, x, blank);
            }
        }
    }
}

/// `attributes` in the colours a pane's program set (`fux_vt::Options::
/// palette`): an indexed colour whose palette entry it changed (OSC 4) as
/// the colour it set, and the default foreground and background, if it set
/// them (OSC 10, 11), as those; each colour it left alone, or reset, as it
/// is, for the client's terminal to draw in its own. A default underline
/// colour is the foreground's, and stays. With DECSCNM set (DEC STD 070,
/// Screen Mode: the screen in reverse video), every cell's inverse is
/// turned over, so a reversed glyph shows plain.
fn pane_colours(attributes: Attributes, screen: &Screen) -> Attributes {
    let colour = |c: Color, default: Option<Rgb>| match c {
        Color::Idx(n) => screen.palette_color(n).map_or(c, Color::Rgb),
        Color::Default => default.map_or(c, Color::Rgb),
        Color::Rgb(..) => c,
    };
    let reverse = screen.mode(fux_vt::Mode::ReverseVideo);
    attributes
        .with_foreground(colour(attributes.foreground(), screen.dynamic_color(10)))
        .with_background(colour(attributes.background(), screen.dynamic_color(11)))
        .with_underline_color(colour(attributes.underline_color(), None))
        .with_inverse(attributes.inverse() != reverse)
}

/// Whether a pane is drawn in colours of its own (`pane_colours`): its
/// program changed some, or set DECSCNM. Its rows then depend on more
/// than their keys, so a frame with it is not memo'd (`Frame::of`).
fn own_colours(screen: &Screen) -> bool {
    screen.colors_changed() || screen.mode(fux_vt::Mode::ReverseVideo)
}

const GRAY_BG: Color = Color::Idx(236);
const BAR_FG: Color = Color::Idx(250);
/// A panel's lines.
const PANEL: Attributes = Attributes::new(Color::Idx(255), Color::Idx(238));
/// What the bar shows in black on yellow: a layer's keys, copy mode's.
const HIGHLIGHT: Attributes = Attributes::new(Color::Idx(0), Color::Idx(11));

/// How many cells `rows` rows of `cols` are. Exact: a u16 by a u16 fits
/// even a 32-bit usize.
fn cell_count(rows: u16, cols: u16) -> usize {
    usize::from(rows).saturating_mul(usize::from(cols))
}

/// A char's display width in cells, as fux-vt lays out what it holds
/// (`fux_vt::char_width`: 0, 1 or 2), so the painter and the screen agree.
fn cells(c: char) -> u16 {
    fux_vt::char_width(c).map_or(0, |w| u16::try_from(w).unwrap_or(u16::MAX))
}

/// Display width of a string, controls dropped.
pub fn width(text: &str) -> u16 {
    text.chars()
        .filter(|c| !c.is_control())
        .map(cells)
        .fold(0, u16::saturating_add)
        .min(4096)
}

/// Cuts `text` to `cols` cells, with an ellipsis when cut; as it is, if it
/// fits.
pub fn fit(text: &str, cols: u16) -> Cow<'_, str> {
    if width(text) <= cols {
        return Cow::Borrowed(text);
    }
    // Room for the text, less a cell for the ellipsis.
    let Some(mut room) = cols.checked_sub(1) else {
        return Cow::Borrowed("");
    };
    let mut out = String::new();
    for c in text.chars().filter(|c| !c.is_control()) {
        let Some(left) = room.checked_sub(cells(c)) else {
            break;
        };
        room = left;
        out.push(c);
    }
    out.push('…');
    Cow::Owned(out)
}

/// The client's screen as it should look now, in a grid of its own, as a
/// test reads it.
#[cfg(test)]
pub(crate) fn compose(session: &Session, client: ClientId) -> Option<Grid> {
    let mut grid = Grid::new(0, 0);
    let view = session.views.get(&client)?;
    compose_view(session, view, &mut grid, &mut Placement::default());
    Some(grid)
}

/// Composes a view's screen as it should look now into `grid`, whatever it
/// held, laying out its panes in `placement`. The grid's cells and the
/// placement are reused, not made again; the frame's memo, the bar's text
/// and the like are made each time.
pub fn compose_view(session: &Session, view: &View, grid: &mut Grid, placement: &mut Placement) {
    let area = Session::pane_area(view);
    session.placement_into(view, placement);
    let placement = &*placement;
    let tab = session.shown_tab(view.id);
    let focus = tab.and_then(|t| t.focus(view.id));
    // Copy mode and its positions, their rows found once for the paint.
    let copy = if let Mode::Copy(copy) = &view.mode {
        let at = session
            .panes
            .get(&copy.pane)
            .and_then(|p| copy.resolve(p.screen()));
        at.map(|at| (copy.as_ref(), at))
    } else {
        None
    };
    put_panes(grid, session, view, placement, focus, copy);
    if tab.is_some_and(|t| t.root().is_none()) && area.h() > 0 {
        // Its keys are the client's: the server names what to do.
        let hint = "empty tab: split it or close it";
        let y = area.h() / 2;
        let x = area.w().saturating_sub(width(hint)) / 2;
        grid.text(
            y,
            x,
            hint,
            Attributes::new(Color::Idx(244), Color::Default),
            area.w(),
        );
    }
    // The cursor: copy mode's in its pane, else the focused pane's, but only
    // in normal mode: none under an overlay, nor in a repeat mode, whose
    // keys are fux's.
    if let Some(focus) = focus
        && let Some(rect) = placement.rect(focus)
        && let Some(pane) = session.panes.get(&focus)
    {
        let screen = pane.screen();
        match copy.filter(|(c, _)| c.pane == focus) {
            Some((_, at)) => {
                // Within the rows shown, which a smaller client can make
                // fewer than the rect.
                if let Some((y, x)) = at.cursor_in_view(rect.h().min(screen.size().rows()))
                    && let Some(at) = rect.at(y, x)
                {
                    grid.cursor = Some(at);
                    // The copy cursor is a block.
                    grid.modes.cursor_shape = CursorShape::SteadyBlock;
                }
            }
            None => {
                let (y, x) = screen.cursor_position();
                // On a row drawn twice as wide, at its glyph's first half.
                let doubled = (screen.window().row(y)).is_some_and(|r| r.rendition().is_double());
                let x = if doubled { x.saturating_mul(2) } else { x };
                if screen.mode(fux_vt::Mode::ShowCursor)
                    && view.mode.is_normal()
                    && let Some(at) = rect.at(y, x)
                {
                    grid.cursor = Some(at);
                    grid.modes.cursor_shape = screen.cursor_shape();
                }
            }
        }
    }
    // The mouse: reported by the client's terminal only while the focused
    // pane's program asked for it and nothing of fux's has the keys, so
    // that the rest of the time the terminal selects text as it does.
    // A client that only watches sends no clicks: its terminal selects.
    if view.mode.is_normal()
        && !view.read_only
        && let Some(focus) = focus
        && placement.rect(focus).is_some()
        && let Some(pane) = session.panes.get(&focus)
    {
        grid.modes.mouse = mouse_level(pane.screen().mouse_protocol_mode());
    }
    bar(grid, session, view, copy);
    match &view.mode {
        Mode::Panel(Panel::Column(c)) => column(grid, view, c),
        Mode::Panel(Panel::List(list)) => list_panel(grid, view, list),
        Mode::Panel(Panel::Prompt {
            title,
            before,
            after,
            ..
        }) => {
            // The panel's border takes a cell each side.
            let room = view.cols.saturating_sub(2);
            let lines: [Line<'_>; 3] = [
                (title.as_str().into(), PANEL.with_bold(true)),
                (prompt_line(before, after, room).into(), PANEL),
                ("Enter accepts · Esc cancels".into(), PANEL.with_dim(true)),
            ];
            surface(grid, view, &lines);
        }
        Mode::Panel(Panel::Confirm { question, .. }) => {
            let lines: [Line<'_>; 2] = [
                (question.as_str().into(), PANEL.with_bold(true)),
                ("y confirms · n or Esc cancels".into(), PANEL),
            ];
            surface(grid, view, &lines);
        }
        // A repeat mode shows in the bar, leaving the layout in view.
        Mode::Normal | Mode::Copy(_) | Mode::Panel(Panel::Repeat(_)) => {}
    }
}

/// Puts the panes and separators on the grid, composed for `view` as laid
/// out in `placement`: if the grid's memo is of the frame composed now, only
/// the pane rows that changed since (`Memo`); else all, blanking first what
/// they leave.
fn put_panes(
    grid: &mut Grid,
    session: &Session,
    view: &View,
    placement: &Placement,
    focus: Option<PaneId>,
    copy: Option<(&crate::copy::Copy, crate::copy::Resolved)>,
) {
    grid.underline_styles = view.terminal.underline_styles;
    let frame = Frame::of(session, view, placement, focus);
    if let Some(memo) = grid
        .memo
        .take()
        .filter(|m| frame.as_ref() == Some(&m.frame))
    {
        grid.cursor = None;
        grid.modes = Modes::default();
        grid.memo = Some(memo.update(grid, session));
        return;
    }
    // The panes and separators, which never overlap, cover the pane area
    // unless a split has no room for even its first child; the bar covers
    // its row. Covered, every cell is written below, and the grid needs no
    // blanking first.
    let lines = placement.separators.iter().map(|s| &s.rect);
    let covered = (placement.panes.iter().map(|(_, r)| r).chain(lines))
        .map(Rect::area)
        .fold(0u32, u32::saturating_add);
    let tiled = covered == Session::pane_area(view).area();
    grid.reset(view.rows, view.cols, !tiled);
    let mut keys = Vec::new();
    for (id, rect) in &placement.panes {
        let Some(pane) = session.panes.get(id) else {
            grid.blank(*rect);
            continue;
        };
        let screen = pane.screen();
        // A pane whose program changed its colours is drawn in them.
        let colours = own_colours(screen).then_some(screen);
        let at = copy.filter(|(c, _)| c.pane == *id).map(|(_, at)| at);
        let window = at.map_or_else(|| screen.window(), |at| at.top.window());
        let width = rect.w().min(window.cols());
        // What the pane's screen does not cover of its place, rows below
        // its window's included, is blank.
        let mut pane_keys = Vec::new();
        for (y, line) in (0..rect.h()).zip(rect.lines()) {
            let row = window.row(y);
            pane_keys.push(row.map(|r| (r.id(), r.version())));
            draw_row(grid, *id, row, (line, window.cols()), colours);
            let (Some(at), Some(row)) = (at, row) else {
                continue;
            };
            for x in 0..width {
                // A wide glyph is selected if either half is, as `y`
                // copies it whole.
                if let Some(i) = line.at(0, x).and_then(|(gy, gx)| grid.index(gy, gx))
                    && let Some(cell) = grid.cells.get(i)
                    && !cell.is_wide_continuation()
                    && (at.selected(&row, x)
                        || cell.is_wide() && x.checked_add(1).is_some_and(|x| at.selected(&row, x)))
                {
                    let attrs = cell.attributes().with_inverse(!cell.inverse());
                    grid.cells.set_attributes(i, attrs);
                }
            }
        }
        keys.push(pane_keys);
    }
    separators(grid, placement, focus);
    grid.memo = frame.map(|frame| Memo { frame, keys });
}

/// A prompt's text with its cursor bar, in at most `room` cells: when it is
/// wider, the line scrolls so the bar shows, with a few cells of what
/// follows it, and an ellipsis marks each side cut off.
fn prompt_line(before: &str, after: &str, room: u16) -> String {
    let line = format!("{before}▏{after}");
    if width(&line) <= room {
        return line;
    }
    let chars: Vec<char> = line.chars().filter(|c| !c.is_control()).collect();
    let bar = before.chars().filter(|c| !c.is_control()).count();
    // An ellipsis each side, at most.
    let inner = room.saturating_sub(2);
    let ahead = inner / 4;
    let (mut start, mut end) = (bar, bar.saturating_add(1));
    let mut used: u16 = 1;
    let fits = |used: u16, c: Option<&char>| {
        c.and_then(|c| used.checked_add(cells(*c)))
            .filter(|n| *n <= inner)
    };
    // A little of what follows, then what comes before, then the rest after.
    while let Some(n) = fits(used, chars.get(end)).filter(|n| *n <= ahead.saturating_add(1)) {
        used = n;
        end = end.saturating_add(1);
    }
    while let Some(previous) = start.checked_sub(1)
        && let Some(n) = fits(used, chars.get(previous))
    {
        used = n;
        start = previous;
    }
    while let Some(n) = fits(used, chars.get(end)) {
        used = n;
        end = end.saturating_add(1);
    }
    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    out.extend(chars.get(start..end).unwrap_or_default());
    if end < chars.len() {
        out.push('…');
    }
    out
}

/// Separator lines, with tees where one meets another; the ones beside the
/// focused pane are highlighted.
fn separators(grid: &mut Grid, placement: &Placement, focus: Option<PaneId>) {
    const UP: u8 = 1;
    const DOWN: u8 = 2;
    const LEFT: u8 = 4;
    const RIGHT: u8 = 8;
    let lines = &placement.separators;
    if lines.is_empty() {
        return;
    }
    // The axis of the line drawn at a cell, if one is: the last separator
    // there, as it is drawn last.
    let axis_at = |y: u16, x: u16| {
        let at = lines.iter().rev().find(|s| s.rect.contains(y, x));
        at.map(|s| s.axis)
    };
    // A line's own directions, and a tee toward each perpendicular line
    // beside it. A line's cell is short of the last column and row a u16
    // counts, and the one before the first is past every rect: a step
    // either way wraps to no line's cell or is exact.
    let bits_at = |y: u16, x: u16| {
        let beside = |bit: u8, y: u16, x: u16, axis: Axis| {
            if axis_at(y, x) == Some(axis) { bit } else { 0 }
        };
        // A separator's axis is its split's: a horizontal split's panes
        // are side by side, divided by vertical lines.
        Some(match axis_at(y, x)? {
            // A vertical line.
            Axis::Horizontal => {
                UP | DOWN
                    | beside(LEFT, y, x.wrapping_sub(1), Axis::Vertical)
                    | beside(RIGHT, y, x.wrapping_add(1), Axis::Vertical)
            }
            // A horizontal line.
            Axis::Vertical => {
                LEFT | RIGHT
                    | beside(UP, y.wrapping_sub(1), x, Axis::Horizontal)
                    | beside(DOWN, y.wrapping_add(1), x, Axis::Horizontal)
            }
        })
    };
    let focused = focus.and_then(|f| placement.rect(f));
    let draw = |grid: &mut Grid, y: u16, x: u16, bits: u8| {
        let glyph = match bits {
            b if b == UP | DOWN => "│",
            b if b == LEFT | RIGHT => "─",
            b if b == UP | DOWN | RIGHT => "├",
            b if b == UP | DOWN | LEFT => "┤",
            b if b == LEFT | RIGHT | DOWN => "┬",
            b if b == LEFT | RIGHT | UP => "┴",
            _ => "┼",
        };
        let color = if focused.is_some_and(|r| r.near(y, x)) {
            Color::Idx(2)
        } else {
            Color::Idx(240)
        };
        // A separator's cell is no pane's, so no wide glyph has a half there
        // to repair: it is only ever drawn over.
        grid.put(
            y,
            x,
            CellRef::new(glyph, false, Attributes::new(color, Color::Default)),
        );
    };
    // Each line plain, in order, so the last one drawn at a cell is its.
    for s in lines {
        let bits = match s.axis {
            Axis::Horizontal => UP | DOWN,
            Axis::Vertical => LEFT | RIGHT,
        };
        for y in s.rect.rows() {
            for x in s.rect.cols() {
                draw(grid, y, x, bits);
            }
        }
    }
    // A tee can only be where a vertical line and a horizontal one meet or
    // cross, at the vertical one's column and the horizontal one's row:
    // where each is within a cell of that point. (The vertical lines are a
    // horizontal split's, as above.)
    let vertical = lines.iter().filter(|s| s.axis == Axis::Horizontal);
    let horizontal = || lines.iter().filter(|s| s.axis == Axis::Vertical);
    for v in vertical.map(|s| s.rect) {
        for h in horizontal().map(|s| s.rect) {
            let (y, x) = (h.y(), v.x());
            if h.near(y, x)
                && v.near(y, x)
                && let Some(bits) = bits_at(y, x)
            {
                draw(grid, y, x, bits);
            }
        }
    }
}

/// The bottom bar: the workspace and its tabs on the left; on the right, the
/// first there is of a notice, copy mode's position, the keys typed in the
/// command column, a repeat mode's keys, and the focused pane.
fn bar(
    grid: &mut Grid,
    session: &Session,
    view: &View,
    copy: Option<(&crate::copy::Copy, crate::copy::Resolved)>,
) {
    let Some(y) = view.rows.checked_sub(1) else {
        return;
    };
    let base = Attributes::new(BAR_FG, GRAY_BG);
    grid.fill(
        Rect::screen(view.rows, view.cols).corner(1, view.cols),
        base,
    );
    let copy_bar =
        copy.and_then(|(c, at)| session.panes.get(&c.pane).map(|p| c.bar(p.screen(), &at)));
    let right: Option<(Cow<'_, str>, Attributes)> = if let Some(notice) = &view.notice {
        Some((
            notice.text.as_str().into(),
            if notice.error {
                Attributes::new(Color::Idx(9), GRAY_BG)
            } else {
                Attributes::new(Color::Idx(11), GRAY_BG)
            },
        ))
    } else if let Some(copy) = &copy_bar {
        Some((copy.position.as_str().into(), base))
    } else if let Mode::Panel(Panel::Column(column)) = &view.mode {
        Some((format!("{} …", column.keys).into(), HIGHLIGHT))
    } else if let Mode::Panel(Panel::Repeat(bar)) = &view.mode {
        Some((bar.as_str().into(), HIGHLIGHT.with_bold(true)))
    } else {
        (session.focused(view.id))
            .and_then(|f| session.panes.get(&f))
            .map(|p| {
                let mut text = format!("{} {}", p.id, p.label());
                if view.zoom {
                    text.push_str(" [zoom]");
                }
                (text.into(), base)
            })
    };
    // At most three quarters of the bar, and a gap before it. Exact: the
    // quarters of a u16 add up to less than one.
    let right_width = right.as_ref().map_or(0, |(t, _)| {
        width(t).min((view.cols / 2).saturating_add(view.cols / 4))
    });
    let left_limit = view
        .cols
        .saturating_sub(right_width.saturating_add(u16::from(right_width > 0)));
    let mut x = 0;
    if let Some(copy) = &copy_bar {
        // Copy mode's keys replace the tabs.
        let badge = HIGHLIGHT.with_bold(true);
        let text = match &copy.badge {
            crate::copy::Badge::Label(label) => fit(&format!(" {label} "), left_limit).into_owned(),
            crate::copy::Badge::Typing(prompt, before, after) => {
                let room = left_limit.saturating_sub(width(prompt).saturating_add(2));
                format!(" {prompt}{} ", prompt_line(before, after, room))
            }
        };
        x = grid.text(y, x, &text, badge, left_limit);
        for &(key, label) in &copy.hints {
            let hint = format!("  {key} {label}");
            if x.saturating_add(width(&hint)) > left_limit {
                break;
            }
            x = grid.text(y, x, "  ", base, left_limit);
            x = grid.text(y, x, key, base.with_bold(true), left_limit);
            x = grid.text(y, x, &format!(" {label}"), base, left_limit);
        }
    } else if let Some(ws) = session.shown_workspace(view.id) {
        let name = format!(" {} ", ws.name);
        x = grid.text(
            y,
            x,
            &fit(&name, left_limit),
            base.with_bold(true),
            left_limit,
        );
        let current = ws.tab_of(view.id).map(|t| t.id);
        for tab in ws.tabs() {
            let Some(room) = left_limit.checked_sub(x).filter(|r| *r > 0) else {
                break;
            };
            // A bell rang in a tab the client does not show (`outer`).
            let rang = tab.seat(view.id).is_some_and(|s| s.rang) && Some(tab.id) != current;
            let label = format!(" {}{} ", tab.name, if rang { "!" } else { "" });
            let attrs = if Some(tab.id) == current {
                Attributes::new(Color::Idx(0), Color::Idx(2)).with_bold(true)
            } else {
                base
            };
            x = grid.text(y, x, &fit(&label, room), attrs, left_limit);
        }
    }
    if let Some((text, attrs)) = right {
        let text = fit(&text, right_width);
        // Right-aligned, a cell from the edge, or from the left if wider.
        let start = view.cols.saturating_sub(width(&text).saturating_add(1));
        grid.text(y, start, &text, attrs, view.cols);
    }
}

/// A line of a panel: its text, borrowed where it can be, and its style.
type Line<'a> = (Cow<'a, str>, Attributes);

/// Adds the `entries` that fit `room`, the `selected` one among them, to a
/// panel's `lines`, with how many more there are above and below them.
fn windowed<'a>(
    lines: &mut Vec<Line<'a>>,
    entries: impl ExactSizeIterator<Item = Line<'a>>,
    selected: usize,
    room: usize,
) {
    let total = entries.len();
    let start = overlay::window_start(total, selected, room);
    if start > 0 {
        lines.push((format!("▲ {start} more").into(), PANEL.with_dim(true)));
    }
    lines.extend(entries.skip(start).take(room));
    let below = total.saturating_sub(start.saturating_add(room));
    if below > 0 {
        lines.push((format!("▼ {below} more").into(), PANEL.with_dim(true)));
    }
}

/// A panel in the bottom-right corner, above the bar, sized to its lines.
fn surface(grid: &mut Grid, view: &View, lines: &[Line<'_>]) {
    let area = Session::pane_area(view);
    if area.is_empty() || lines.is_empty() {
        return;
    }
    // More lines than rows is the same as exactly as many.
    let height = u16::try_from(lines.len()).unwrap_or(u16::MAX);
    let inner = lines.iter().map(|(t, _)| width(t)).max().unwrap_or(0);
    // A border cell each side of the text.
    let panel = area.corner(height, inner.saturating_add(2));
    // On a short screen the first lines give way, as the last (the help)
    // matter more; but not past the selected entry, which stays in view.
    let skip = lines.len().saturating_sub(usize::from(panel.h()));
    let selected = lines.iter().position(|(_, attrs)| attrs.inverse());
    let skip = selected.map_or(skip, |selected| skip.min(selected));
    for (line, (text, attrs)) in panel.lines().zip(lines.iter().skip(skip)) {
        grid.fill(line, *attrs);
        // Exact: the panel is at least a cell wide, inside the screen.
        let text_x = line.x().saturating_add(1);
        let text = fit(text, line.w().saturating_sub(2));
        grid.text(
            line.y(),
            text_x,
            &text,
            *attrs,
            line.right().saturating_sub(1),
        );
    }
}

/// A list to choose from or run: the entries that fit, the selected one
/// highlighted and, but in a chooser, those that cannot run now dimmed.
fn list_panel(grid: &mut Grid, view: &View, list: &List) {
    let mut lines: Vec<Line<'_>> = vec![(list.title.as_str().into(), PANEL.with_bold(true))];
    // Its items' rows: it has a title and a help line.
    let capacity = overlay::panel_room(view.rows, 2);
    let entries = list.items.iter().enumerate().map(|(i, item)| {
        let marker = if list.current == Some(i) { "*" } else { " " };
        let attrs = PANEL.with_inverse(i == list.selected);
        (format!("{marker} {item}").into(), attrs)
    });
    windowed(&mut lines, entries, list.selected, capacity);
    let help = if list.chooser {
        "Enter selects · r renames · x closes · Esc"
    } else {
        "Enter runs · Esc cancels"
    };
    lines.push((help.into(), PANEL.with_dim(true)));
    surface(grid, view, &lines);
}

/// The command column: the bindings and layers of its layer, grouped, the
/// selected one highlighted and those that cannot run now dimmed.
fn column(grid: &mut Grid, view: &View, column: &crate::act::Column) {
    let key_width = column
        .entries
        .iter()
        .map(|e| width(&e.key.to_string()))
        .max()
        .unwrap_or(0);
    let mut entries: Vec<Line<'_>> = Vec::new();
    let mut heading = None;
    let mut selected_row = 0usize;
    for (index, entry) in column.entries.iter().enumerate() {
        let group = (entry.root, entry.group.as_str());
        if heading != Some(group) {
            entries.push((group.1.into(), PANEL.with_bold(true)));
            heading = Some(group);
        }
        let more = if entry.layer { "…" } else { "" };
        let key = entry.key.to_string();
        let pad = usize::from(key_width.saturating_sub(width(&key)));
        let mut attrs = PANEL;
        if index == column.selected {
            attrs = attrs.with_inverse(true);
            selected_row = entries.len();
        }
        let text = &entry.label;
        entries.push((format!("{:pad$}{key}  {text}{more}", "").into(), attrs));
    }
    // A heading, if there is room for it, then the entries.
    let heading = view.rows.saturating_sub(1) >= 4;
    let body_room = overlay::panel_room(view.rows, usize::from(heading));
    let mut lines: Vec<Line<'_>> = Vec::new();
    if heading {
        lines.push((column.title.as_str().into(), PANEL.with_bold(true)));
    }
    windowed(&mut lines, entries.into_iter(), selected_row, body_room);
    if column.entries.is_empty() {
        lines.push(("no bindings".into(), PANEL.with_dim(true)));
    }
    surface(grid, view, &lines);
}

// ------------------------------------------------------------------ paint

/// The SGR that sets `a` from nothing, as painted (`SgrForm::Paint`), with
/// underline styles if the client's terminal draws them (`styles`,
/// `outer::styles!`).
pub(crate) fn sgr(out: &mut Vec<u8>, a: Attributes, styles: bool) {
    out.extend_from_slice(b"\x1b[0");
    a.write_sgr(out, fux_vt::SgrForm::Paint { styles });
    out.push(b'm');
}

/// OSC 8: opens `link` in the client's terminal, or closes the open link.
/// Its `id` is the pane's and the link's key there, which no other link of
/// the pane has: no two panes' links are one to the terminal, and a link
/// painted in pieces, or whose program gave no `id`, is one link, as the
/// spec asks a multiplexer to make it (`references/modern/osc8_hyperlinks.md`,
/// "Hover underlining and the `id` parameter"). A terminal that does not
/// know OSC 8 ignores it.
fn hyperlink(out: &mut Vec<u8>, link: Option<(PaneId, &Hyperlink)>) {
    match link {
        Some((pane, link)) => {
            let (key, uri) = (link.key(), link.uri());
            let _ = write!(out, "\x1b]8;id=fux{}-{key};{uri}\x1b\\", pane.number());
        }
        None => out.extend_from_slice(b"\x1b]8;;\x1b\\"),
    }
}

/// Whether `text`, painted at (`y`, `x`) right after the glyph before it,
/// would continue that glyph's grapheme cluster in a terminal that joins
/// clusters as fux-vt does.
fn joins_the_glyph_before(grid: &Grid, y: u16, x: u16, text: &str) -> bool {
    let Some(first) = text.chars().next() else {
        return false;
    };
    let Some(mut before) = x.checked_sub(1) else {
        return false;
    };
    if grid
        .get(y, before)
        .is_some_and(|c| c.is_wide_continuation())
    {
        before = before.saturating_sub(1);
    }
    grid.get(y, before)
        .is_some_and(|glyph| fux_vt::continues_cluster(glyph.shown(), first))
}

/// Paints `text`, a glyph `width` columns wide at (`y`, `x`) in a run of
/// cells painted one after another, where it is not one byte of ASCII or
/// follows a glyph that `joined` the one before it; whether the next glyph
/// is to be placed by a cursor move: it joins the one before it, or it was
/// measured otherwise.
///
/// A glyph that continues the cluster of the one before it (an emoji
/// modifier a program put after an emoji with a cursor move of its own, as
/// micro and vim do) is placed by a cursor move, and so is what follows it:
/// the client's terminal joins it to the glyph before, or not, as it does
/// when the program writes it directly, and the next glyph is where fux-vt
/// has it either way.
///
/// So is what follows a cluster whose width fux-vt makes other than the sum
/// of its characters' widths, which a terminal measuring each character
/// alone would advance by.
///
/// Zero-width characters after a glyph that leaves the cursor in the last
/// column: with autowrap off, as fux's client has it, Ghostty puts them on
/// the cell under the cursor if it holds anything, a space included
/// (`Terminal.print`, for a glyph printed in the last column, where the
/// cursor stays); with autowrap on, on the glyph before, as everywhere
/// else. So autowrap is on just for them: the glyph ends a column short of
/// the edge, and nothing printed wraps.
fn cluster(
    out: &mut Vec<u8>,
    grid: &Grid,
    (y, x): (u16, u16),
    width: u16,
    text: &str,
    joined: bool,
) -> bool {
    let joins = text.len() > 1 && joins_the_glyph_before(grid, y, x, text);
    if joins || joined {
        goto(out, y, x);
    }
    let marks_at_the_edge = x.saturating_add(width).saturating_add(1) == grid.cols
        && text.chars().nth(1).is_some()
        && text.chars().skip(1).all(|c| cells(c) == 0);
    if marks_at_the_edge {
        out.extend_from_slice(b"\x1b[?7h");
    }
    out.extend_from_slice(text.as_bytes());
    if marks_at_the_edge {
        out.extend_from_slice(b"\x1b[?7l");
    }
    // A cluster fux-vt makes wider or narrower than its characters' sum (a
    // VS16 emoji widened, jamo capped at 2): a terminal that measures each
    // character alone, as xterm does through wcwidth, advances by the sum,
    // so the next glyph is placed too.
    let measured_apart = text.chars().map(cells).fold(0, u16::saturating_add) != width;
    joins || measured_apart
}

/// A row or column as the terminal counts it, from 1; exact in a u32.
fn one_based(n: u16) -> u32 {
    u32::from(n).saturating_add(1)
}

/// Moves the cursor to row `y`, column `x`, both from 0.
fn goto(out: &mut Vec<u8>, y: u16, x: u16) {
    let _ = write!(out, "\x1b[{};{}H", one_based(y), one_based(x));
}

/// Turns every mouse tracking mode and SGR encoding off, as `client::LEAVE`
/// does too.
pub const MOUSE_OFF: &str = "\x1b[?1003l\x1b[?1002l\x1b[?1000l\x1b[?1006l";

/// The tracking the client's terminal is asked for while a program has
/// `mode`: enough to report everything the program wants, which its own
/// mode then filters (`fux_vt::Screen::encode_mouse`). X10's presses come
/// from 1000, whose releases it drops.
pub fn mouse_level(mode: fux_vt::MouseProtocolMode) -> u16 {
    use fux_vt::MouseProtocolMode as M;
    match mode {
        M::None => 0,
        M::Press | M::PressRelease => 1000,
        M::ButtonMotion => 1002,
        M::AnyMotion => 1003,
    }
}

/// The bytes that turn `old` (what the client shows, or nothing) into
/// `new`, as a test reads them.
#[cfg(test)]
pub(crate) fn paint(old: Option<&Grid>, new: &Grid) -> Vec<u8> {
    let mut out = Vec::new();
    let blank = Grid::new(new.rows, new.cols);
    paint_into(old.unwrap_or(&blank), old.is_some(), new, &mut out);
    out
}

/// Appends the bytes that turn `shown`, what the client shows if `known`
/// (else what it last showed, whose cursor shape it keeps), into `new` to
/// `out`.
pub fn paint_into(shown: &Grid, known: bool, new: &Grid, out: &mut Vec<u8>) {
    if known && echo(shown, new, out) {
        return;
    }
    paint_whole(shown, known, new, out);
}

/// Whether row `y` of two grids of a size shows the same: its cells, and,
/// if either grid has links (`links`), its cells' links.
#[inline]
fn same_row(old: &Grid, new: &Grid, y: u16, links: bool) -> bool {
    old.row_eq(new, y) && (!links || (0..new.cols).all(|x| old.link(y, x) == new.link(y, x)))
}

/// The rows of `new` that differ from `old`, a grid of its size (every
/// row, if there is none): those whose memo does not show them the same
/// and whose cells or links differ.
fn differing_rows<'a>(old: Option<&'a Grid>, new: &'a Grid) -> impl Iterator<Item = u16> + 'a {
    let links = !new.links.is_empty() || old.is_some_and(|o| !o.links.is_empty());
    let memo = old.and_then(|o| SameFrame::of(o, new));
    (0..new.rows).filter(move |&y| {
        // An unchanged row costs this one comparison.
        !memo.as_ref().is_some_and(|m| m.same_row(y))
            && old.is_none_or(|o| !same_row(o, new, y, links))
    })
}

/// A keystroke's echo, painted as a terminal shows it typed: when all that
/// changed is a run of glyphs of one column on the cursor's row, ending
/// where the cursor now is, short of the last column, the cursor is moved
/// to the run (by nothing, a carriage return or a column if it is on the
/// row, else by row and column) and the glyphs alone are written, in their
/// attributes, from the default every paint leaves. Nothing else changes
/// for the terminal: not the cursor's shape or visibility, the mouse, a
/// link, a wide glyph. Whether it was so; if not, nothing is written.
fn echo(old: &Grid, new: &Grid, out: &mut Vec<u8>) -> bool {
    if (old.rows, old.cols, old.underline_styles, old.modes)
        != (new.rows, new.cols, new.underline_styles, new.modes)
        || !old.links.is_empty()
        || !new.links.is_empty()
    {
        return false;
    }
    let (Some((oy, ox)), Some((y, to))) = (old.cursor, new.cursor) else {
        return false;
    };
    if to == 0 || to >= new.cols {
        return false;
    }
    let mut rows = differing_rows(Some(old), new);
    if rows.next() != Some(y) || rows.next().is_some() {
        return false;
    }
    // The run is from the first changed cell of the row to the cursor's
    // cell; nothing after it changed. Cells in it that did not change are
    // written as they are.
    let same = |x: u16| old.cell_eq(new, y, x);
    let Some(from) = (0..new.cols).find(|&x| !same(x)) else {
        return false;
    };
    if from >= to || (to..new.cols).any(|x| !same(x)) {
        return false;
    }
    // ASCII, as typing mostly is, moves every terminal's cursor one cell;
    // another glyph of one column is written too, and the cursor put where
    // it is after, should the terminal draw it at another width. Nothing
    // that may join a neighbour: one character, not a regional indicator
    // (two make a flag), and short of the emoji's planes.
    let mut ascii = true;
    let mut printable = |c: Option<CellRef<'_>>| {
        c.is_some_and(|c| {
            if c.is_wide() || c.is_wide_continuation() {
                return false;
            }
            let text = c.contents();
            if matches!(text.as_bytes(), [b' '..=b'~']) {
                return true;
            }
            let mut chars = text.chars();
            match (chars.next(), chars.next()) {
                (Some(ch), None) => {
                    ascii = false;
                    u32::from(ch) < 0x1_F000
                        && !('\u{1F1E6}'..='\u{1F1FF}').contains(&ch)
                        && fux_vt::char_width(ch) == Some(1)
                }
                _ => false,
            }
        })
    };
    // What the run overwrites is narrow too: no half of a wide glyph left.
    if (from..to).any(|x| {
        !printable(new.get(y, x))
            || old
                .get(y, x)
                .is_some_and(|c| c.is_wide() || c.is_wide_continuation())
    }) {
        return false;
    }
    // To the run's first cell: from the cursor on its row by the shortest
    // way, else by row and column.
    if oy != y {
        goto(out, y, from);
    } else if from == 0 && ox != 0 {
        out.push(b'\r');
    } else if from != ox {
        let _ = write!(out, "\x1b[{}G", one_based(from));
    }
    let mut current: Option<Attributes> = None;
    for x in from..to {
        let Some(cell) = new.get(y, x) else {
            return false;
        };
        let attrs = cell.attributes();
        if current.unwrap_or_default() != attrs {
            sgr(out, attrs, new.underline_styles);
            current = Some(attrs);
        }
        out.extend_from_slice(cell.contents().as_bytes());
    }
    if current.is_some() {
        out.extend_from_slice(b"\x1b[0m");
    }
    if !ascii {
        let _ = write!(out, "\x1b[{}G", one_based(to));
    }
    true
}
/// [`paint_into`] without the echo's short way.
fn paint_whole(shown: &Grid, known: bool, new: &Grid, out: &mut Vec<u8>) {
    let old = known.then_some(shown);
    out.extend_from_slice(b"\x1b[?2026h\x1b[?25l");
    // Learning that the terminal draws underline styles repaints it whole,
    // so that what it shows takes them.
    let full = old.is_none_or(|o| {
        o.rows != new.rows || o.cols != new.cols || o.underline_styles != new.underline_styles
    });
    if full {
        out.extend_from_slice(b"\x1b[0m\x1b[H\x1b[2J");
    }
    let mut current: Option<Attributes> = None;
    // The hyperlink the client's terminal has open.
    let mut open = None;
    // Whether either grid has a link: if neither does, no cell's changed.
    let links = !new.links.is_empty() || old.is_some_and(|o| !o.links.is_empty());
    // What the client shows, unless all is painted whole: a grid of the same
    // size, so the two index their cells alike. Grids of one frame show the
    // same cells on a pane row whose keys are the same in both (`Memo`): no
    // comparison needed.
    let before = old.filter(|_| !full);
    for y in differing_rows(before, new) {
        let cell = |x: u16| new.get(y, x);
        let changed = |x: u16| {
            before.is_none_or(|o| !o.cell_eq(new, y, x) || links && o.link(y, x) != new.link(y, x))
        };
        let mut x = 0u16;
        while x < new.cols {
            // Moving right stops at the last column, where the loop ends.
            if !changed(x) {
                x = x.saturating_add(1);
                continue;
            }
            // A run of changed cells, starting at a glyph's first half.
            let mut start = x;
            if cell(start).is_some_and(|c| c.is_wide_continuation()) {
                start = start.saturating_sub(1);
            }
            goto(out, y, start);
            let mut cx = start;
            // Whether the glyph before was one the client's terminal may
            // have joined to the one before it, or measured otherwise (see
            // `cluster`): then the next is placed by a cursor move.
            let mut joined = false;
            while cx < new.cols
                && (cx == start
                    || changed(cx)
                    || cell(cx).is_some_and(|c| c.is_wide_continuation()))
            {
                let Some(cell) = cell(cx) else { break };
                if cell.is_wide_continuation() {
                    cx = cx.saturating_add(1);
                    continue;
                }
                let wide = cell.is_wide();
                // A wide glyph cannot fit in the last column.
                let text = if wide && cx.saturating_add(1) >= new.cols {
                    " "
                } else {
                    cell.shown()
                };
                let attrs = cell.attributes();
                if current != Some(attrs) {
                    sgr(out, attrs, new.underline_styles);
                    current = Some(attrs);
                }
                if links {
                    let link = new.link(y, cx);
                    if link != open {
                        hyperlink(out, link);
                        open = link;
                    }
                }
                let width = if wide { 2 } else { 1 };
                // Most cells hold one byte of ASCII, which neither joins the
                // glyph before nor carries marks, and follow one that joined
                // nothing: one test, and the text.
                if text.len() > 1 || joined {
                    joined = cluster(out, new, (y, cx), width, text, joined);
                } else {
                    out.extend_from_slice(text.as_bytes());
                }
                cx = cx.saturating_add(width);
            }
            x = cx.max(x.saturating_add(1));
        }
    }
    if open.is_some() {
        hyperlink(out, None);
    }
    out.extend_from_slice(b"\x1b[0m");
    new.modes.diff(&shown.modes, known, out);
    if let Some((y, x)) = new.cursor {
        goto(out, y, x);
        out.extend_from_slice(b"\x1b[?25h");
    }
    out.extend_from_slice(b"\x1b[?2026l");
}

#[cfg(test)]
mod tests {
    /// A picture that moves, its crop the same, is encoded once, and sent
    /// at each place with the same data; a new crop is encoded again, and
    /// a picture no longer shown is let go of (`threats.md` B08).
    #[test]
    fn a_moving_picture_is_encoded_once() -> Result<(), String> {
        let rgba: Vec<u8> = std::iter::repeat_n([255, 0, 0, 255], 20 * 12)
            .flatten()
            .collect();
        let picture =
            std::sync::Arc::new(fux_vt::sixel::Picture::new(20, 12, rgba).ok_or("20 by 12")?);
        let at = |row: u16, crop: (u32, u32, u32, u32)| Visible {
            pane: PaneId::of(1),
            id: 7,
            area: (row, 0, 1, 2),
            crop,
            picture: std::sync::Arc::clone(&picture),
        };
        let whole = (0, 0, 20, 12);
        let mut sixels = Sixels::default();
        let mut sent = Vec::new();
        for row in 0..100 {
            let mut out = Vec::new();
            sixels.picture_into(&at(row, whole), &mut out);
            sixels.keep(&[at(row, whole)]);
            sent.push(out);
        }
        assert_eq!(sixels.encoded, 1, "encoded each time it moved");
        let mut alone = Vec::new();
        fux_vt::sixel::encode(&picture, whole, &mut alone);
        assert_eq!(
            sent.last(),
            Some(&[b"\x1b7\x1b[100;1H".as_slice(), &alone, b"\x1b8"].concat())
        );
        let mut out = Vec::new();
        sixels.picture_into(&at(0, (0, 6, 20, 6)), &mut out);
        assert_eq!(sixels.encoded, 2, "a new crop is encoded");
        sixels.keep(&[]);
        assert!(sixels.kept.is_empty(), "kept what no longer shows");
        Ok(())
    }

    /// `compose` into a grid and placement kept, as the server composes:
    /// false if there is no such client.
    fn compose_into(s: &Session, c: ClientId, grid: &mut Grid, placement: &mut Placement) -> bool {
        let view = s.views.get(&c);
        view.map(|view| compose_view(s, view, grid, placement))
            .is_some()
    }
    use super::*;

    /// A client's terminal keeps its own cursor shape until a pane sets
    /// one: the first paint writes none, a shape a pane set is written as
    /// its Ps, a program's 0 as 0, and the terminal's own comes back as 0,
    /// which most terminals take for theirs, then 7, which xterm takes for
    /// its initial resources (ctlseqs, DECSCUSR), where 0 is a blinking
    /// block. A full repaint, after a resize, starts from the shape the
    /// terminal has, which the resize kept.
    #[test]
    fn a_cursor_shape_is_written_once_a_pane_sets_one() {
        // The DECSCUSRs a paint writes, from a terminal whose shape is
        // `was`, its screen known or not.
        let paint = |was: CursorShape, known: bool, shape: CursorShape| {
            let mut old = Grid::new(2, 3);
            old.modes.cursor_shape = was;
            let mut new = Grid::new(2, 3);
            new.modes.cursor_shape = shape;
            let mut out = Vec::new();
            paint_into(&old, known, &new, &mut out);
            let out = String::from_utf8_lossy(&out).into_owned();
            let shapes = ["\x1b[0 q\x1b[7 q", "\x1b[0 q", "\x1b[5 q", "\x1b[6 q"];
            shapes
                .into_iter()
                .find(|s| out.contains(s))
                .unwrap_or_default()
        };
        use CursorShape::{BlinkingBar, Initial, SteadyBar, Zero};
        assert_eq!(paint(Initial, false, Initial), "");
        assert_eq!(paint(Initial, false, BlinkingBar), "\x1b[5 q");
        assert_eq!(paint(BlinkingBar, true, Zero), "\x1b[0 q");
        assert_eq!(paint(SteadyBar, true, SteadyBar), "");
        assert_eq!(paint(SteadyBar, true, Initial), "\x1b[0 q\x1b[7 q");
        // After a resize the screen is not known, but the shape is.
        assert_eq!(paint(SteadyBar, false, Initial), "\x1b[0 q\x1b[7 q");
        assert_eq!(paint(SteadyBar, false, SteadyBar), "");
    }

    /// A terminal of `rows` by `cols`, as a client's, without history.
    fn terminal(rows: u16, cols: u16) -> Result<fux_vt::Parser, String> {
        let size = fux_vt::Size::new(rows, cols).map_err(|e| e.to_string())?;
        fux_vt::Parser::new(size, 0).map_err(|e| e.to_string())
    }

    /// A small deterministic generator (splitmix64).
    struct Rng(u64);

    impl Rng {
        fn below(&mut self, n: usize) -> usize {
            self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut x = self.0;
            x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            x ^= x >> 31;
            usize::try_from(x.checked_rem(u64::try_from(n).unwrap_or(1)).unwrap_or(0)).unwrap_or(0)
        }
    }

    /// Each row's cells, text and attributes, and the cursor.
    type Shown = (Vec<Vec<(String, Attributes)>>, (u16, u16));

    /// What a terminal shows.
    fn terminal_shows(parser: &fux_vt::Parser) -> Shown {
        let window = parser.screen().window();
        let rows = window.rows();
        let cells = (0..rows)
            .map(|y| {
                window.row(y).map_or_else(Vec::new, |row| {
                    row.cells()
                        .map(|c| (c.contents().to_owned(), c.attributes()))
                        .collect()
                })
            })
            .collect();
        (cells, parser.screen().cursor_position())
    }

    /// A terminal shown the paints as the server sends them, echoes the
    /// short way (`echo`) and rows skipped by their memo, shows what one
    /// shown every paint whole shows: random typing at a shell, in colour,
    /// with the cursor moved, rows erased, wide glyphs and long lines.
    #[test]
    fn echoes_painted_the_short_way_show_what_whole_paints_show() -> Result<(), String> {
        use crate::id::PaneId;
        let typed: [&[u8]; 20] = [
            b"\rw",
            "\r\u{24d1}".as_bytes(),
            b"\x1b[5;3Hx",
            "\u{e9}".as_bytes(),
            "\u{24d0}".as_bytes(),
            "\u{2500}\u{2502}".as_bytes(),
            "\u{1f1e6}".as_bytes(),
            // A glyph typed, and another row written meanwhile.
            b"q\x1b7\x1b[8;1Hzz\x1b8\x1b[C",
            b"a",
            b"Z",
            b"~",
            b" ",
            b"\x1b[32mg\x1b[m",
            b"\x08\x1b[K",
            b"\r\n$ ",
            b"\xe7\x95\x8c",
            b"\x1b[3D",
            b"\x1b[H\x1b[2J$ ",
            b"0123456789012345678901234567890123456789",
            b"\x1b[7mR\x1b[m",
        ];
        let mut r = Rng(0x0ec4_0ec4);
        let mut short = 0usize;
        for case in 0..30 {
            let (mut s, c) = crate::session::testing::attached(10, 40)?;
            if r.below(3) == 0 {
                crate::session::testing::run(&mut s, "str %1 pane horizontal split")?;
            }
            let mut fast = terminal(10, 40)?;
            let mut whole = terminal(10, 40)?;
            let (mut spare, mut showing) = (Grid::new(0, 0), Grid::new(0, 0));
            let mut placement = Placement::default();
            let mut painted = false;
            for step in 0..200 {
                let out = typed.get(r.below(typed.len())).copied().unwrap_or(b"");
                let panes: Vec<PaneId> = s.panes.keys().copied().collect();
                if let Some(&id) = panes.get(r.below(panes.len().max(1))) {
                    s.output(id, out);
                }
                s.hold_copies();
                if !compose_into(&s, c, &mut spare, &mut placement) {
                    continue;
                }
                let bytes = paint(painted.then_some(&showing), &spare);
                let (mut plain_old, mut plain_new) = (showing.clone(), spare.clone());
                plain_old.forget_memo();
                plain_new.forget_memo();
                let mut slow = Vec::new();
                paint_whole(&plain_old, painted, &plain_new, &mut slow);
                if !bytes.starts_with(b"\x1b[?2026h") {
                    short += 1;
                }
                fast.process(&bytes).map_err(|e| e.to_string())?;
                whole.process(&slow).map_err(|e| e.to_string())?;
                assert_eq!(
                    terminal_shows(&fast),
                    terminal_shows(&whole),
                    "case {case}, step {step}: {bytes:?}"
                );
                std::mem::swap(&mut spare, &mut showing);
                painted = true;
            }
        }
        assert!(short > 500, "{short} echoes painted the short way");
        Ok(())
    }

    /// Composing into the grid composed last but one, as the server does,
    /// puts again only the rows that changed (`Memo`), and paints only the
    /// rows that may differ: through random output into split panes,
    /// scrolling, the alternate screen, links, resizes, copy mode, a
    /// notice and splits, after every step the grid is what composing
    /// whole gives, its paint is the paint comparing every row, and
    /// `same_as` is `==`.
    #[test]
    fn composing_in_part_is_composing_whole() -> Result<(), String> {
        use crate::id::PaneId;
        let outputs: [&[u8]; 12] = [
            b"x",
            b"hello\r\n",
            b"\x1b[2J\x1b[H",
            b"\x1b[5;3Hmid\x1b[K",
            b"line\r\nline\r\nline\r\nline\r\nline\r\nline\r\n",
            b"\x1b[?1049h\x1b[Halt",
            b"\x1b[?1049l",
            b"\x1b[31mred\x1b[m \xe7\x95\x8c",
            b"\x1b]8;;http://x\x1b\\link\x1b]8;;\x1b\\",
            b"\x1b]4;1;#00ff00\x07",
            b"\x1b]104\x07",
            b"\x1b[2;4r\x1b[4H\n\n\x1b[r",
        ];
        let commands = [
            "str %1 pane horizontal split",
            "str %1 pane vertical split",
            "str c1 client zoom",
            "str c1 client copy-mode",
            "str c1 client command-prompt",
            "str c1 client str next focus",
            "str %2 pane kill",
        ];
        let mut r = Rng(0x00c0_ffee);
        for case in 0..40 {
            let (mut s, c) = crate::session::testing::attached(12, 50)?;
            let (mut spare, mut shown) = (Grid::new(0, 0), Grid::new(0, 0));
            let mut placement = Placement::default();
            let mut painted = false;
            for step in 0..120 {
                match r.below(12) {
                    0 => {
                        let line = commands.get(r.below(commands.len())).copied().unwrap_or("");
                        let _ = crate::session::testing::run(&mut s, line);
                    }
                    // The column opens, or what is open closes.
                    1 => s.input(c, if r.below(2) == 0 { b"\x02" } else { b"\x1b" }),
                    2 => {
                        let rows = u16::try_from(4 + r.below(12)).unwrap_or(8);
                        let cols = u16::try_from(10 + r.below(50)).unwrap_or(40);
                        s.resize(c, rows, cols);
                    }
                    3 => {
                        crate::session::testing::run(&mut s, "str @1 tab str n rename")?;
                    }
                    _ => {
                        let panes: Vec<PaneId> = s.panes.keys().copied().collect();
                        if let Some(&id) = panes.get(r.below(panes.len().max(1))) {
                            let out = outputs.get(r.below(outputs.len())).copied().unwrap_or(b"");
                            s.output(id, out);
                        }
                    }
                }
                s.hold_copies();
                if !compose_into(&s, c, &mut spare, &mut placement) {
                    continue;
                }
                let whole = compose(&s, c).ok_or("no client")?;
                assert!(spare == whole, "case {case}, step {step}: composed in part");
                let fast = paint(painted.then_some(&shown), &spare);
                let (mut plain_old, mut plain_new) = (shown.clone(), spare.clone());
                plain_old.forget_memo();
                plain_new.forget_memo();
                let slow = paint(painted.then_some(&plain_old), &plain_new);
                assert!(
                    fast == slow,
                    "case {case}, step {step}: the memo's paint differs"
                );
                assert_eq!(
                    spare.same_as(&shown),
                    spare == shown,
                    "case {case}, step {step}"
                );
                std::mem::swap(&mut spare, &mut shown);
                painted = true;
            }
        }
        Ok(())
    }

    /// Mouse reporting is said from nothing, either way, and again only
    /// when it changes.
    #[test]
    fn mouse_reporting_is_painted_when_it_changes() {
        let mut on = Grid::new(2, 4);
        on.modes.mouse = 1002;
        let off = Grid::new(2, 4);
        let text = |b: Vec<u8>| String::from_utf8_lossy(&b).into_owned();
        let first = text(paint(None, &on));
        assert!(
            first.contains(&format!("{MOUSE_OFF}\x1b[?1002h\x1b[?1006h")),
            "{first:?}"
        );
        assert!(text(paint(None, &off)).contains(MOUSE_OFF));
        assert!(!text(paint(Some(&on), &on)).contains("\x1b[?100"));
        let gone = text(paint(Some(&on), &off));
        assert!(
            gone.contains(MOUSE_OFF) && !gone.contains("1006h"),
            "{gone:?}"
        );
        let mut any = on.clone();
        any.modes.mouse = 1003;
        assert!(text(paint(Some(&on), &any)).contains(&format!("{MOUSE_OFF}\x1b[?1003h")));
        use fux_vt::MouseProtocolMode as M;
        let levels = [
            M::None,
            M::Press,
            M::PressRelease,
            M::ButtonMotion,
            M::AnyMotion,
        ];
        assert_eq!(levels.map(mouse_level), [0, 1000, 1000, 1002, 1003]);
    }

    fn apply(bytes: &[u8], parser: &mut fux_vt::Parser) -> Vec<String> {
        let _ = parser.process(bytes);
        let window = parser.screen().window();
        (0..window.rows())
            .map(|y| {
                window
                    .row(y)
                    .map(|row| row.cells_text().trim_end_matches(' ').to_owned())
                    .unwrap_or_default()
            })
            .collect()
    }

    /// Copy mode's bar replaces the tabs with what it is doing and the keys
    /// that act now; a narrow bar drops the least important.
    #[test]
    fn copy_mode_replaces_the_tabs_with_its_keys() -> Result<(), Box<dyn std::error::Error>> {
        let (mut s, c) = crate::session::testing::attached(10, 120)?;
        let bar = |s: &Session| {
            compose(s, c)
                .map(|g| g.row_text(g.rows.saturating_sub(1)))
                .unwrap_or_default()
        };
        assert!(bar(&s).starts_with(" main  main"), "{}", bar(&s));
        s.input(c, b"\x02c");
        let copying = bar(&s);
        assert!(
            copying.starts_with(" COPY   v s x select  q quit  f r search  hjkl move  w b words"),
            "{copying}"
        );
        assert!(!copying.contains("main") && !copying.contains("y copy"));
        // Where the cursor is, on the right: its line of all the rows.
        assert!(copying.ends_with("1/9"), "{copying}");
        s.input(c, b"v");
        let selecting = bar(&s);
        assert!(
            selecting.starts_with(" COPY select   y copy  q quit  o other end  v clear"),
            "{selecting}"
        );
        // Letters in either case: S selects lines; C-v is no key's.
        s.input(c, b"S\x16");
        let lines = bar(&s);
        assert!(
            lines.starts_with(" COPY lines   y copy  q quit  o other end  s clear"),
            "{lines}"
        );
        s.input(c, b"fab");
        assert!(
            bar(&s).starts_with(" /ab▏   Enter search  Esc cancel"),
            "{}",
            bar(&s)
        );
        s.input(c, b"\x1b");
        s.escape(c);
        s.resize(c, 10, 36);
        let narrow = bar(&s);
        assert!(narrow.contains("y copy  q quit"), "{narrow}");
        assert!(!narrow.contains("other end"), "{narrow}");
        Ok(())
    }

    /// A line a program made double width (DECDWL) or double height
    /// (DECDHL) is drawn twice as wide, each cell over two columns, a wide
    /// glyph over four, and the cursor at its glyph's first half; a single
    /// width line beside it is drawn as it is.
    #[test]
    fn a_double_width_line_is_drawn_twice_as_wide() -> Result<(), Box<dyn std::error::Error>> {
        let (mut s, c) = crate::session::testing::attached(10, 30)?;
        s.output(
            PaneId::of(1),
            "\x1b#6a界b\r\n\x1b#3\x1b[7mx\x1b[m\r\nplain\x1b[1;3H".as_bytes(),
        );
        let grid = compose(&s, c).ok_or("a screen")?;
        let text = |y| grid.row_text(y);
        assert_eq!(text(0), "a 界  b");
        assert_eq!(text(1), "x");
        assert_eq!(text(2), "plain");
        // The blank beside x is in its colours.
        assert!(grid.get(1, 1).is_some_and(|cell| cell.inverse()));
        assert_eq!(grid.cursor, Some((0, 4)));
        Ok(())
    }

    /// A block whose edge falls on a wide glyph's second half in one of its
    /// rows highlights that glyph, as `y` copies it whole.
    #[test]
    fn a_block_highlights_the_wide_glyphs_it_copies() -> Result<(), Box<dyn std::error::Error>> {
        let (mut s, c) = crate::session::testing::attached(10, 30)?;
        s.output(PaneId::of(1), "xab\r\n界b".as_bytes());
        // A block from `a` above to `b` below, its left edge on 界's second
        // half.
        s.input(c, b"\x02ckhhxjl");
        let grid = compose(&s, c).ok_or("a screen")?;
        let inverse = |y, x| grid.get(y, x).is_some_and(|cell| cell.inverse());
        assert!(inverse(1, 0) && inverse(1, 2) && !inverse(0, 0));
        s.input(c, b"y");
        assert_eq!(s.buffers.front().map(String::as_str), Some("ab\n界b"));
        Ok(())
    }

    /// A grid composed into again, whatever it held and whatever its size,
    /// comes out as a fresh one would.
    #[test]
    fn composing_into_a_used_grid_is_composing_afresh() -> Result<(), Box<dyn std::error::Error>> {
        let (mut s, c) = crate::session::testing::attached(12, 50)?;
        s.output(PaneId::of(1), b"first screen\r\n\x1b[5 q");
        let before = compose(&s, c).ok_or("a screen")?;
        crate::session::testing::run(&mut s, "str %1 pane horizontal split")?;
        // The focused pane hides its cursor, and a smaller client makes the
        // panes smaller than their places: compose leaves cells untouched.
        s.output(PaneId::of(2), "界 second\r\n\x1b[?25l".as_bytes());
        s.attach(8, 30, None)?;
        let fresh = compose(&s, c).ok_or("a screen")?;
        assert_ne!(before, fresh);
        assert!(before.cursor.is_some() && fresh.cursor.is_none());
        assert!(fresh.cells.iter().any(|c| !c.has_contents()));
        // The previous screen, with its cursor and shape.
        let mut used = before;
        assert!(compose_into(&s, c, &mut used, &mut Placement::default()));
        assert_eq!(used, fresh);
        // Grids of this size and others, full of text.
        for (rows, cols) in [(12, 50), (3, 7), (12, 49), (40, 200)] {
            let mut other = Grid::new(rows, cols);
            for y in 0..rows {
                other.text(
                    y,
                    0,
                    "junk 界 junk",
                    Attributes::default().with_bold(true),
                    cols,
                );
            }
            other.cursor = Some((1, 1));
            other.modes.cursor_shape = CursorShape::BlinkingUnderline;
            assert!(compose_into(&s, c, &mut other, &mut Placement::default()));
            assert_eq!(other, fresh, "{rows}x{cols}");
        }
        Ok(())
    }

    /// At the widest a terminal can be, the last column is u16::MAX - 1:
    /// one past it must stop a wide glyph, not overflow (in release, wrap).
    #[test]
    fn a_wide_glyph_does_not_fit_the_widest_last_column() {
        let mut grid = Grid::new(1, u16::MAX);
        let last = u16::MAX.saturating_sub(1);
        let end = grid.text(0, last, "界", Attributes::default(), u16::MAX);
        assert_eq!(end, last);
    }

    #[test]
    fn painting_the_widest_last_column_ends() {
        let mut grid = Grid::new(1, u16::MAX);
        let wide = CellRef::new("界", true, Attributes::default());
        grid.set(0, u16::MAX.saturating_sub(1), wide);
        // The glyph is painted as a blank, and moving past it stops the run.
        let bytes = paint(None, &grid);
        assert!(bytes.ends_with(b"\x1b[?2026l"));
    }

    /// A cluster whose width fux-vt makes other than the sum of its
    /// characters' (a VS16 emoji widened, jamo capped at 2) is followed by
    /// a cursor move: a client terminal that measures each character alone
    /// (xterm, through wcwidth) would put what follows elsewhere (TXT-11).
    #[test]
    fn a_cluster_measured_otherwise_apart_is_followed_by_a_cursor_move() -> Result<(), String> {
        // A heart VS16 widened, and four leading jamo capped at 2: a client
        // terminal measuring each character alone advances 1 and 8, so
        // what follows is placed where fux-vt has it, column 2.
        for cluster in ["\u{2764}\u{fe0f}", "\u{1100}\u{1100}\u{1100}\u{1100}"] {
            let mut grid = Grid::new(1, 12);
            grid.set(0, 0, CellRef::new(cluster, true, Attributes::default()));
            grid.set(0, 1, CellRef::wide_continuation());
            grid.text(0, 2, "ab", Attributes::default(), 12);
            let bytes = paint(None, &grid);
            let text = String::from_utf8_lossy(&bytes);
            assert!(text.contains(&format!("{cluster}\x1b[1;3Hab")), "{text:?}");
            let mut parser = terminal(1, 12)?;
            assert_eq!(apply(&bytes, &mut parser), grid_lines(&grid));
        }
        // A flag is two columns either way, and a mark adds none: no move.
        for cluster in ["\u{1f1ef}\u{1f1f5}", "e\u{301}"] {
            let mut grid = Grid::new(1, 12);
            let wide = cluster.starts_with('\u{1f1ef}');
            grid.set(0, 0, CellRef::new(cluster, wide, Attributes::default()));
            let next = if wide {
                grid.set(0, 1, CellRef::wide_continuation());
                2
            } else {
                1
            };
            grid.text(0, next, "ab", Attributes::default(), 12);
            let text = String::from_utf8_lossy(&paint(None, &grid)).into_owned();
            assert!(text.contains(&format!("{cluster}ab")), "{text:?}");
        }
        Ok(())
    }

    /// A glyph that continues the cluster of the one before it (an emoji
    /// modifier micro puts after an emoji with a cursor move of its own) is
    /// placed by a cursor move, and so is the glyph after it: Ghostty joins
    /// the modifier to the emoji, as it does when micro writes it directly,
    /// and the next glyph still lands where fux-vt has it.
    #[test]
    fn a_glyph_that_would_join_the_one_before_is_placed() -> Result<(), String> {
        let mut grid = Grid::new(1, 12);
        grid.text(0, 2, "\u{1F44D}", Attributes::default(), 12);
        grid.text(0, 4, "\u{1F3FD}", Attributes::default(), 12);
        grid.text(0, 6, "x", Attributes::default(), 12);
        let bytes = paint(None, &grid);
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            text.contains("\u{1F44D}\x1b[1;5H\u{1F3FD}\x1b[1;7Hx"),
            "{text:?}"
        );
        let mut parser = terminal(1, 12)?;
        assert_eq!(apply(&bytes, &mut parser), grid_lines(&grid));
        // Glyphs that do not join are painted in one run.
        let mut plain = Grid::new(1, 12);
        plain.text(0, 2, "\u{1F44D}\u{1F680}x", Attributes::default(), 12);
        let text = String::from_utf8_lossy(&paint(None, &plain)).into_owned();
        assert!(text.contains("\u{1F44D}\u{1F680}x"), "{text:?}");
        Ok(())
    }

    /// Zero-width characters after a glyph that leaves the cursor in the
    /// last column are painted with autowrap on, so that Ghostty puts them
    /// on that glyph and not on the last column's; anywhere else, and in
    /// the last column itself, a cluster is painted as it is.
    #[test]
    fn marks_short_of_the_edge_are_painted_with_autowrap() -> Result<(), String> {
        let marked = |text| CellRef::new(text, false, Attributes::default());
        let painted = |x: u16, text| {
            let mut grid = Grid::new(1, 6);
            grid.fill(Rect::screen(1, 6), Attributes::default());
            grid.set(0, x, marked(text));
            (
                String::from_utf8_lossy(&paint(None, &grid)).into_owned(),
                grid,
            )
        };
        let (text, grid) = painted(4, "a\u{301}\u{356}");
        assert!(
            text.contains("\x1b[?7ha\u{301}\u{356}\x1b[?7l "),
            "{text:?}"
        );
        let mut parser = terminal(1, 6)?;
        assert_eq!(apply(text.as_bytes(), &mut parser), grid_lines(&grid));
        for (x, cluster) in [
            (3, "a\u{301}"),
            (5, "a\u{301}"),
            (4, "a"),
            (4, "\u{1F44D}\u{1F3FD}"),
        ] {
            let (text, _) = painted(x, cluster);
            assert!(!text.contains("\x1b[?7h"), "{x} {cluster:?}: {text:?}");
        }
        Ok(())
    }

    /// A prompt wider than its panel scrolls to keep the cursor in view,
    /// with an ellipsis on each side cut off, never wider than the room.
    #[test]
    fn a_long_prompt_scrolls_to_its_cursor() {
        let text = "split -v -- echo aaaaaaaaaaaaaaaaaaaaTAIL";
        // The line with its cursor `left` chars from the end.
        let line = |text: &str, left: usize| {
            let mut line: crate::view::Line = crate::view::Line::new(text.into());
            (0..left).for_each(|_| line.edit(&crate::act::Edit::Left));
            line
        };
        let end = text.chars().count();
        for (left, starts, ends) in [(0, "…", "TAIL▏"), (end, "▏spl", "…"), (20, "…", "…")]
        {
            let line = line(text, left);
            let shown = prompt_line(line.before(), line.after(), 12);
            assert!(width(&shown) <= 12, "{shown:?} fits");
            assert!(shown.contains('▏'), "{shown:?} shows the cursor");
            assert!(shown.starts_with(starts), "{shown:?} starts {starts:?}");
            assert!(shown.ends_with(ends), "{shown:?} ends {ends:?}");
        }
        // Wide chars count two cells; short text is as it was.
        assert!(width(&prompt_line("界界界界界界界界", "", 7)) <= 7);
        assert_eq!(prompt_line("ls", "", 12), "ls▏");
    }

    fn grid_lines(g: &Grid) -> Vec<String> {
        (0..g.rows).map(|y| g.row_text(y)).collect()
    }

    #[test]
    fn a_diff_applied_to_the_old_grid_gives_the_new_one() -> Result<(), String> {
        let mut a = Grid::new(4, 12);
        a.text(0, 0, "hello", Attributes::default(), 12);
        a.text(2, 3, "界界x", Attributes::default().with_bold(true), 12);
        let mut b = a.clone();
        b.text(0, 0, "he", Attributes::default().with_underline(true), 12);
        b.text(2, 4, "ab", Attributes::default(), 12);
        b.text(3, 10, "界", Attributes::default(), 12);
        let mut parser = terminal(4, 12)?;
        apply(&paint(None, &a), &mut parser);
        let lines = apply(&paint(Some(&a), &b), &mut parser);
        assert_eq!(lines, grid_lines(&b));
        // Full repaint from nothing matches too.
        let mut fresh = terminal(4, 12)?;
        assert_eq!(apply(&paint(None, &b), &mut fresh), grid_lines(&b));
        Ok(())
    }

    /// The rows a paint writes to: those it moves the cursor to, from 0.
    fn rows_written(bytes: &[u8]) -> Vec<u16> {
        let text = String::from_utf8_lossy(bytes);
        let mut rows: Vec<u16> = text
            .split("\x1b[")
            .filter_map(|sequence| {
                let (row, rest) = sequence.split_once(';')?;
                let (col, _) = rest.split_once('H')?;
                col.parse::<u16>().ok()?;
                row.parse::<u16>().ok()?.checked_sub(1)
            })
            .collect();
        rows.dedup();
        rows
    }

    /// A row that did not change gets no bytes at all.
    #[test]
    fn only_the_rows_that_changed_are_painted() -> Result<(), String> {
        let mut a = Grid::new(6, 20);
        for y in 0..6 {
            a.text(y, 0, &format!("row {y} 界 text"), Attributes::default(), 20);
        }
        let mut b = a.clone();
        b.text(3, 5, "X", Attributes::default().with_bold(true), 20);
        let diff = paint(Some(&a), &b);
        assert_eq!(rows_written(&diff), [3]);
        let mut parser = terminal(6, 20)?;
        apply(&paint(None, &a), &mut parser);
        assert_eq!(apply(&diff, &mut parser), grid_lines(&b));
        // The first and last rows, and a wide glyph's second half.
        let mut c = b.clone();
        c.text(0, 19, "!", Attributes::default(), 20);
        c.text(5, 7, "y", Attributes::default(), 20);
        let diff = paint(Some(&b), &c);
        assert_eq!(rows_written(&diff), [0, 5]);
        assert_eq!(apply(&diff, &mut parser), grid_lines(&c));
        // Nothing changed: no row is written; painted whole, every row is.
        assert_eq!(rows_written(&paint(Some(&c), &c)), Vec::<u16>::new());
        assert_eq!(rows_written(&paint(None, &c)), [0, 1, 2, 3, 4, 5]);
        Ok(())
    }

    /// Every attribute fux-vt keeps reaches the client: blink, hidden text
    /// (which must stay hidden), strikeout and the underline colour, beside
    /// those painted before.
    #[test]
    fn every_attribute_is_painted() -> Result<(), String> {
        let attributes = [
            Attributes::default().with_blink(fux_vt::Blink::Slow),
            Attributes::default().with_blink(fux_vt::Blink::Rapid),
            Attributes::default().with_hidden(true),
            Attributes::default().with_strikeout(true),
            Attributes::default()
                .with_underline(true)
                .with_underline_color(Color::Idx(9)),
            Attributes::default()
                .with_underline(true)
                .with_underline_color(Color::Rgb([1, 2, 3].into())),
            Attributes::default()
                .with_bold(true)
                .with_italic(true)
                .with_inverse(true),
        ];
        let mut grid = Grid::new(1, 12);
        for (x, a) in attributes.iter().enumerate() {
            let x = u16::try_from(x).map_err(|e| e.to_string())?;
            grid.text(0, x, "x", *a, 12);
        }
        let bytes = paint(None, &grid);
        let text = String::from_utf8_lossy(&bytes);
        for sgr in [
            "\x1b[0;5m",
            "\x1b[0;6m",
            "\x1b[0;8m",
            "\x1b[0;9m",
            "\x1b[0;4;58:5:9m",
            "\x1b[0;4;58:2::1:2:3m",
        ] {
            assert!(text.contains(sgr), "{sgr:?} in {text:?}");
        }
        // Read back by a terminal, each cell has what it was painted with.
        let mut parser = terminal(1, 12)?;
        parser.process(&bytes).map_err(|e| e.to_string())?;
        for (x, a) in attributes.iter().enumerate().take(5) {
            let x = u16::try_from(x).map_err(|e| e.to_string())?;
            let cell = parser.screen().cell(0, x).ok_or("a cell")?;
            assert_eq!(cell.attributes(), *a, "cell {x}");
        }
        Ok(())
    }

    /// Paint's SGR: the flags, then the underline (a style only where the
    /// terminal draws styles) and the blink, then the underline colour in
    /// its colon form, then the colours in their semicolon forms.
    #[test]
    fn paint_writes_the_sgr_every_terminal_reads() {
        let a = Attributes::new(Color::Idx(9), Color::Rgb([1, 2, 3].into()))
            .with_bold(true)
            .with_italic(true)
            .with_underline_style(fux_vt::UnderlineStyle::Curly)
            .with_blink(fux_vt::Blink::Slow)
            .with_underline_color(Color::Idx(5));
        let double = Attributes::new(Color::Idx(208), Color::Idx(15))
            .with_underline_style(fux_vt::UnderlineStyle::Double);
        let painted = |a: Attributes, styles: bool| {
            let mut out = Vec::new();
            sgr(&mut out, a, styles);
            String::from_utf8_lossy(&out).into_owned()
        };
        assert_eq!(painted(a, true), "\x1b[0;1;3;4:3;5;58:5:5;91;48;2;1;2;3m");
        assert_eq!(painted(a, false), "\x1b[0;1;3;4;5;58:5:5;91;48;2;1;2;3m");
        assert_eq!(painted(double, true), "\x1b[0;4:2;38;5;208;107m");
        assert_eq!(painted(double, false), "\x1b[0;4;38;5;208;107m");
    }

    /// A character of no width of its own (a combining mark, ZWJ, VS16)
    /// is part of the cluster before it (UAX #29, GB9 and GB9a), so a panel
    /// or the bar keeps it on that cell: `cafe` and U+0301 is `café`, four
    /// cells wide; on a wide glyph it joins the glyph's first half.
    #[test]
    fn a_mark_of_no_width_stays_on_the_cell_before() -> Result<(), String> {
        let mut grid = Grid::new(1, 8);
        let end = grid.text(0, 0, "cafe\u{301} 界\u{301}x", PANEL, 8);
        assert_eq!(end, 8);
        assert_eq!(grid.row_text(0), "cafe\u{301} 界\u{301}x");
        let fourth = grid.row(0).nth(3).map(|c| c.contents().to_owned());
        assert_eq!(fourth.as_deref(), Some("e\u{301}"));
        // Painted, a terminal reads it back so.
        let mut parser = terminal(1, 8)?;
        parser
            .process(&paint(None, &grid))
            .map_err(|e| e.to_string())?;
        let row = parser.screen().rows().next().map(|r| r.cells_text());
        assert_eq!(row.as_deref(), Some("cafe\u{301} 界\u{301}x"));
        Ok(())
    }

    /// Underline styles reach a client whose terminal draws them as kitty's
    /// `4:n` (`references/modern/kitty_underlines.html`): double, curly,
    /// dotted and dashed, with their colour; a plain underline is 4. Any
    /// other terminal is painted a plain underline for each, with the
    /// colour still. Learning that the terminal draws them repaints it
    /// whole, so what it showed plain takes its style.
    #[test]
    fn underline_styles_are_painted_as_the_terminal_draws_them() -> Result<(), String> {
        use fux_vt::UnderlineStyle::{Curly, Dashed, Dotted, Double, Single};
        let styles = [Single, Double, Curly, Dotted, Dashed];
        let mut grid = Grid::new(1, 6);
        for (x, style) in styles.iter().enumerate() {
            let x = u16::try_from(x).map_err(|e| e.to_string())?;
            let a = Attributes::default()
                .with_underline_style(*style)
                .with_underline_color(Color::Rgb([255, 0, 0].into()));
            grid.text(0, x, "x", a, 6);
        }
        let painted = |grid: &Grid| String::from_utf8_lossy(&paint(None, grid)).into_owned();
        let plain = painted(&grid);
        assert_eq!(plain.matches("\x1b[0;4;58:2::255:0:0mx").count(), 5);
        assert!(!plain.contains("4:"), "{plain:?}");
        let mut styled = grid.clone();
        styled.underline_styles = true;
        let text = painted(&styled);
        for sgr in [
            "\x1b[0;4;58:2::255:0:0mx",
            "\x1b[0;4:2;58:2::255:0:0mx",
            "\x1b[0;4:3;58:2::255:0:0mx",
            "\x1b[0;4:4;58:2::255:0:0mx",
            "\x1b[0;4:5;58:2::255:0:0mx",
        ] {
            assert!(text.contains(sgr), "{sgr:?} in {text:?}");
        }
        // Read back by a terminal that keeps styles, each cell has its own;
        // painted plain, each is a plain underline.
        for (grid, keeps) in [(&styled, true), (&grid, false)] {
            let mut parser = terminal(1, 6)?;
            parser
                .process(&paint(None, grid))
                .map_err(|e| e.to_string())?;
            for (x, style) in styles_of(keeps).iter().enumerate() {
                let x = u16::try_from(x).map_err(|e| e.to_string())?;
                let cell = parser.screen().cell(0, x).ok_or("a cell")?;
                assert_eq!(cell.underline_style(), *style, "cell {x}");
                assert_eq!(cell.underline_color(), Color::Rgb([255, 0, 0].into()));
            }
        }
        // The terminal turns out to draw styles: the screen is painted again
        // whole, not as a diff of nothing.
        let again = paint(Some(&grid), &styled);
        assert!(String::from_utf8_lossy(&again).contains("\x1b[2J"));
        assert!(!String::from_utf8_lossy(&paint(Some(&styled), &styled)).contains("\x1b[2J"));
        Ok(())
    }

    /// What `underline_styles_are_painted_as_the_terminal_draws_them`
    /// expects a terminal to keep: each style, or plain underlines.
    fn styles_of(keeps: bool) -> [fux_vt::UnderlineStyle; 5] {
        use fux_vt::UnderlineStyle::{Curly, Dashed, Dotted, Double, Single};
        if keeps {
            [Single, Double, Curly, Dotted, Dashed]
        } else {
            [Single; 5]
        }
    }

    /// A pane's colours are its own (`fux_vt::Feature::Palette`): a cell
    /// of an entry its program changed (OSC 4) is painted in the colour it
    /// set, and its default foreground and background in those it set (OSC
    /// 10, 11), while the pane beside it, which changed nothing, is painted
    /// its indices and defaults; the client's terminal is sent no OSC 4,
    /// 10 or 11, so its own palette stays as it was. Reset (OSC 104, 110,
    /// 111), the entry is painted as its index again. Each pane answers its
    /// program's queries with its own colours.
    #[test]
    fn a_panes_palette_is_painted_and_stays_the_panes() -> Result<(), Box<dyn std::error::Error>> {
        let (mut s, c) = crate::session::testing::attached(6, 41)?;
        crate::session::testing::run(&mut s, "str %1 pane horizontal split")?;
        let set = b"\x1b]4;1;#ff0000\x1b\\\x1b]10;rgb:11/22/33;#000080\x07";
        s.output(PaneId::of(1), &[&set[..], b"\x1b[31mR\x1b[39mD"].concat());
        s.output(PaneId::of(2), b"\x1b[31mR\x1b[39mD");
        let grid = compose(&s, c).ok_or("a screen")?;
        let bytes = paint(None, &grid);
        let text = String::from_utf8_lossy(&bytes);
        for osc in ["\x1b]4", "\x1b]5", "\x1b]1"] {
            assert!(!text.contains(osc), "{osc:?} sent to the client: {text:?}");
        }
        // Read back by a terminal that keeps colours, as the client's.
        let options = fux_vt::Options::from(fux_vt::Feature::Palette);
        let mut client = fux_vt::Parser::with_options(fux_vt::Size::new(6, 41)?, 0, options)?;
        client.process(&bytes)?;
        let navy = Color::Rgb([0, 0, 0x80].into());
        let right: u16 = 21;
        let cells = |p: &fux_vt::Parser, x: u16| {
            let c = p.screen().cell(0, x)?;
            Some((c.contents().to_owned(), c.fgcolor(), c.bgcolor()))
        };
        assert!(!client.screen().colors_changed(), "the client's colours");
        assert_eq!(
            cells(&client, 0),
            Some(("R".into(), Color::Rgb([0xff, 0, 0].into()), navy))
        );
        assert_eq!(
            cells(&client, 1),
            Some(("D".into(), Color::Rgb([0x11, 0x22, 0x33].into()), navy))
        );
        // The pane's blanks are in its background too.
        assert_eq!(cells(&client, 2).map(|(_, _, bg)| bg), Some(navy));
        assert_eq!(
            cells(&client, right),
            Some(("R".into(), Color::Idx(1), Color::Default))
        );
        assert_eq!(
            cells(&client, right + 1),
            Some(("D".into(), Color::Default, Color::Default))
        );
        // Reset, the entry and the defaults are the client's again.
        s.output(PaneId::of(1), b"\x1b]104;1\x07\x1b]110\x07\x1b]111\x07");
        let reset = compose(&s, c).ok_or("a screen")?;
        let diff = paint(Some(&grid), &reset);
        assert!(!String::from_utf8_lossy(&diff).contains("\x1b]"));
        client.process(&diff)?;
        assert_eq!(
            cells(&client, 0),
            Some(("R".into(), Color::Idx(1), Color::Default))
        );
        assert_eq!(
            cells(&client, 1),
            Some(("D".into(), Color::Default, Color::Default))
        );
        // Each pane answers its program with its own colours.
        s.output(PaneId::of(1), b"\x1b]4;1;#00ff00\x07");
        for (id, answer) in [(1, "0000/ffff/0000"), (2, "cdcd/0000/0000")] {
            let pane = s.panes.get_mut(&PaneId::of(id)).ok_or("a pane")?;
            pane.input.drain_all();
            pane.output(b"\x1b]4;1;?\x07");
            let asked = pane.input.drain_all();
            let expected = format!("\x1b]4;1;rgb:{answer}\x07");
            assert_eq!(String::from_utf8_lossy(&asked), expected, "pane {id}");
        }
        Ok(())
    }

    /// DECSCNM (`CSI ? 5 h`; DEC STD 070, Screen Mode: the whole screen in
    /// reverse video) is drawn: a pane that sets it is painted with every
    /// cell's inverse turned over, blanks too, so a reversed glyph shows
    /// plain; the pane beside it is not; the rows it prints later are
    /// painted so too, and resetting it paints them plain again.
    #[test]
    fn a_pane_in_reverse_video_is_painted_reversed() -> Result<(), Box<dyn std::error::Error>> {
        let (mut s, c) = crate::session::testing::attached(6, 41)?;
        crate::session::testing::run(&mut s, "str %1 pane horizontal split")?;
        s.output(PaneId::of(1), b"\x1b[?5ha\x1b[7mb\x1b[27m");
        s.output(PaneId::of(2), b"a");
        let mut client = fux_vt::Parser::new(fux_vt::Size::new(6, 41)?, 0)?;
        let inverse =
            |p: &fux_vt::Parser, y: u16, x: u16| p.screen().cell(y, x).map(|c| c.inverse());
        let grid = compose(&s, c).ok_or("a screen")?;
        client.process(&paint(None, &grid))?;
        assert_eq!(inverse(&client, 0, 0), Some(true));
        assert_eq!(inverse(&client, 0, 1), Some(false));
        assert_eq!(inverse(&client, 0, 2), Some(true));
        assert_eq!(inverse(&client, 0, 21), Some(false));
        s.output(PaneId::of(1), b"\r\nc");
        let next = compose(&s, c).ok_or("a screen")?;
        client.process(&paint(Some(&grid), &next))?;
        assert_eq!(inverse(&client, 1, 0), Some(true));
        s.output(PaneId::of(1), b"\x1b[?5l");
        let plain = compose(&s, c).ok_or("a screen")?;
        client.process(&paint(Some(&next), &plain))?;
        assert_eq!(inverse(&client, 0, 0), Some(false));
        assert_eq!(inverse(&client, 0, 1), Some(true));
        assert_eq!(inverse(&client, 1, 0), Some(false));
        Ok(())
    }

    /// The blanks fux fills itself where a pane's screen is smaller than
    /// its place (a smaller client attached), past its rows' ends and below
    /// its last row, are in the pane's colours too: with DECSCNM set they
    /// are reversed, as the whole screen is (DEC STD 070, Screen Mode).
    #[test]
    fn a_reversed_panes_own_blanks_are_reversed() -> Result<(), Box<dyn std::error::Error>> {
        let (mut s, c) = crate::session::testing::attached(6, 41)?;
        s.output(PaneId::of(1), b"\x1b[?5ha");
        s.attach(3, 20, None)?;
        let grid = compose(&s, c).ok_or("a screen")?;
        let inverse = |y, x| grid.get(y, x).map(|cell| cell.inverse());
        assert_eq!(inverse(0, 0), Some(true));
        assert_eq!(inverse(0, 30), Some(true), "past the row's end");
        assert_eq!(inverse(4, 0), Some(true), "below the last row");
        Ok(())
    }

    /// A pane's hyperlinks reach the client: OSC 8 before the linked cells,
    /// with an id made of the pane and the link's key, and an OSC 8 that
    /// closes it after them. Two panes' links never share an id, though
    /// each pane's first link has the same key; a terminal reading the
    /// paint has each cell's link.
    #[test]
    fn hyperlinks_are_painted_with_their_panes_ids() -> Result<(), Box<dyn std::error::Error>> {
        let (mut s, c) = crate::session::testing::attached(6, 41)?;
        crate::session::testing::run(&mut s, "str %1 pane horizontal split")?;
        let linked = b"a\x1b]8;;http://x\x1b\\link\x1b]8;;\x1b\\b";
        s.output(PaneId::of(1), linked);
        s.output(PaneId::of(2), linked);
        let grid = compose(&s, c).ok_or("a screen")?;
        let bytes = paint(None, &grid);
        let text = String::from_utf8_lossy(&bytes);
        for pane in [1, 2] {
            let open = format!("\x1b]8;id=fux{pane}-1;http://x\x1b\\link\x1b]8;;\x1b\\");
            assert!(text.contains(&open), "{open:?} in {text:?}");
        }
        // Read back, each linked cell has its link, and only those do.
        let options = fux_vt::Feature::Hyperlinks.into();
        let mut parser = fux_vt::Parser::with_options(fux_vt::Size::new(6, 41)?, 0, options)?;
        parser.process(&bytes)?;
        let screen = parser.screen();
        let ids: Vec<Option<String>> = (0..41)
            .map(|x| screen.link(0, x).and_then(|l| l.id()).map(str::to_owned))
            .collect();
        let right: u16 = 21;
        for (x, id) in ids.iter().enumerate() {
            let expected = match x {
                1..=4 => Some("fux1-1".to_owned()),
                x if (usize::from(right) + 1..=usize::from(right) + 4).contains(&x) => {
                    Some("fux2-1".to_owned())
                }
                _ => None,
            };
            assert_eq!(*id, expected, "column {x}");
        }
        assert_eq!(screen.link(0, 1).map(|l| l.uri()), Some("http://x"));
        // A link the program takes off its cells is taken off the client's:
        // only that row is painted again, and nothing more for no change.
        let mut quiet = s;
        quiet.output(PaneId::of(1), b"\r\x1b[Babc");
        let same = compose(&quiet, c).ok_or("a screen")?;
        quiet.output(PaneId::of(1), b"\x1b[A\ra\x1b[0Klink");
        let unlinked = compose(&quiet, c).ok_or("a screen")?;
        let diff = paint(Some(&same), &unlinked);
        assert_eq!(rows_written(&diff), [0]);
        assert!(!String::from_utf8_lossy(&diff).contains("id="));
        parser.process(&paint(Some(&grid), &same))?;
        parser.process(&diff)?;
        assert_eq!(parser.screen().link(0, 1), None);
        assert!(parser.screen().link(0, right + 1).is_some());
        // Unchanged, links and all: no cell is painted, only the cursor.
        let none = String::from_utf8_lossy(&paint(Some(&unlinked), &unlinked)).into_owned();
        assert!(
            !none.contains("link") && !none.contains("\x1b]8"),
            "{none:?}"
        );
        Ok(())
    }

    #[test]
    fn fit_cuts_at_glyph_boundaries() {
        assert_eq!(fit("hello", 10), "hello");
        assert_eq!(fit("hello", 4), "hel…");
        assert_eq!(fit("界界界", 4), "界…");
        assert_eq!(fit("x", 0), "");
    }
}
