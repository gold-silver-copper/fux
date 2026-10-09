//! Painting a client's screen: compose a grid of cells from its panes, the
//! separators, the bar and any overlay; diff it against what the client has;
//! send only the changed runs, inside synchronized output. A keystroke's
//! echo, when it is all that changed, is written as a terminal shows it
//! typed, without the synchronized envelope (`echo`). Every paint leaves
//! the terminal's attributes at their default, which the next one assumes.
use crate::id::ClientId;
use crate::id::PaneId;
use crate::layout::{Axis, Placement, Rect, Separator};
use crate::overlay;
use crate::session::Session;
use crate::view::{Choice, List, Mode, View};
use fux_vt::{Attributes, CellRef, Cells, Color, Row, UnderlineStyle};
use std::borrow::Cow;
use std::io::Write;
use unicode_width::UnicodeWidthChar;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Grid {
    pub rows: u16,
    pub cols: u16,
    /// Row after row; clusters too long to hold inline are kept whole.
    pub cells: Cells,
    /// Each cell's hyperlink: one more than its place in `links`, 0 for
    /// none. Empty while no cell has one, as most grids have none.
    link_of: Vec<u32>,
    /// The hyperlinks the cells have, each once.
    links: Vec<Link>,
    /// Their URIs, one after another.
    uris: String,
    /// Where the terminal cursor is shown, if it is.
    pub cursor: Option<(u16, u16)>,
    /// DECSCUSR shape for the cursor; 0 is the terminal's default.
    pub cursor_shape: u16,
    /// The mouse tracking the client's terminal is asked for (1000, 1002
    /// or 1003, always SGR-encoded), 0 for none: the focused pane's
    /// program's, while it has the keys ([`mouse_level`]).
    pub mouse: u16,
    /// Whether the client's terminal draws underline styles (`outer`):
    /// painted as they are, else as plain underlines (`sgr`).
    pub underline_styles: bool,
    /// What composed the grid, so that composing it again puts only what
    /// changed, and painting it compares only rows that may differ. Not
    /// what the grid shows: grids compare equal whatever it holds.
    memo: Memo,
}

/// What composed a grid in normal mode: its frame, everything that decides
/// the grid but its panes' rows, and the identity and version of each pane
/// row put in it. A row of a fux-vt screen keeps its identity while it is
/// retained, no later row takes it, and its version changes with every
/// edit of its cells or soft wrap; pane ids are never reused. So two grids
/// of the same frame show the same cells on a row of the pane area whose
/// pane rows have the same keys in both.
#[derive(Clone, Debug, Default)]
struct Memo {
    frame: Option<Frame>,
    /// For each pane of the frame's placement, in its order: each of its
    /// window's rows on the grid, `None` where it has none.
    keys: Vec<(PaneId, Vec<RowKey>)>,
}

/// A pane row's identity and version, `None` where the window has no row.
type RowKey = Option<(fux_vt::RowId, u64)>;

impl PartialEq for Memo {
    fn eq(&self, _: &Memo) -> bool {
        true
    }
}
impl Eq for Memo {}

/// Everything about a composed grid but its panes' rows: its size, where
/// each pane and separator is and its window's size, the focus (which
/// colours the separators), whether the panes cover the pane area, and how
/// underlines are painted. Only in normal mode, with no overlay, no
/// copy-mode selection, no empty-tab hint, and no pane in colours of its
/// own (`recolour`), whose rows depend on more than their keys.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Frame {
    rows: u16,
    cols: u16,
    panes: Vec<(PaneId, Rect, (u16, u16))>,
    separators: Vec<crate::layout::Separator>,
    focus: Option<PaneId>,
    tiled: bool,
    underline_styles: bool,
}

impl Frame {
    fn of(
        session: &Session,
        view: &View,
        placement: &Placement,
        tiled: bool,
        focus: Option<PaneId>,
    ) -> Option<Frame> {
        // A client focuses a pane of its tab unless the tab is empty.
        if !matches!(view.mode, Mode::Normal) || focus.is_none() {
            return None;
        }
        let mut panes = Vec::with_capacity(placement.panes.len());
        for (id, rect) in &placement.panes {
            let screen = session.panes.get(id)?.screen();
            if screen.colors_changed() {
                return None;
            }
            panes.push((*id, *rect, screen.size()));
        }
        Some(Frame {
            rows: view.rows,
            cols: view.cols,
            panes,
            separators: placement.separators.clone(),
            focus,
            tiled,
            underline_styles: view.terminal.underline_styles,
        })
    }
}

/// Where a pane row goes on the grid: its first cell, the width of the
/// window put there, the pane's width, whether the panes cover the pane
/// area, and the window's columns.
struct Place {
    gy: u16,
    gx: u16,
    width: u16,
    rect_w: u16,
    tiled: bool,
    window_cols: u16,
}

/// Puts a pane's window row on the grid at `place`, in the pane's own
/// colours if it has them. With `tiled`, what the row does not cover of the
/// pane's width is blanked here, as nothing blanked the area first.
/// Without, a whole composition blanked the area before; and the memo path
/// (`put_changed_rows`) puts only live rows, each as wide as its window, so
/// the cells past one are the blanks the frame before left there.
fn draw_row(
    grid: &mut Grid,
    pane: PaneId,
    row: Option<Row<'_>>,
    place: &Place,
    colours: Option<&fux_vt::Screen>,
) {
    let Place {
        gy,
        gx,
        width,
        rect_w,
        tiled,
        window_cols,
    } = *place;
    // The pane's own row, well formed, goes whole onto the cells.
    let len = row.map_or(0, |row| row.len());
    if let Some(row) = row {
        grid.put_row(gy, gx, pane, row, width);
        if let Some(screen) = colours {
            grid.recolour(gy, gx, width, screen);
        }
    }
    if tiled {
        // At most `width`, which is at most the place's width.
        let end = u16::try_from(len).unwrap_or(width).min(width);
        grid.blank(gy, gx.saturating_add(end), gx.saturating_add(rect_w));
    }
    // A wide glyph in the window's last column is cut off, as `Window::cell`
    // has it.
    if let Some(last) = window_cols.checked_sub(1)
        && last < width
        && row
            .and_then(|r| r.cell(usize::from(last)))
            .is_some_and(|c| c.is_wide())
        && let Some(x) = gx.checked_add(last)
    {
        grid.put(gy, x, CellRef::default());
    }
}

/// Puts again the pane rows that changed since `grid`, whose memo has the
/// frame composed now, was composed: whether that sufficed; false if the
/// panes are not the memo's, or a row now has links, which only a whole
/// composition numbers.
fn put_changed_rows(
    grid: &mut Grid,
    session: &Session,
    placement: &Placement,
    tiled: bool,
) -> bool {
    let mut keys = std::mem::take(&mut grid.memo.keys);
    let mut sufficed = keys.len() == placement.panes.len();
    for ((id, rect), (memo_id, pane_keys)) in placement.panes.iter().zip(&mut keys) {
        let Some(pane) = session.panes.get(id).filter(|_| id == memo_id && sufficed) else {
            sufficed = false;
            break;
        };
        let screen = pane.screen();
        let (rows, cols) = screen.size();
        let window = screen.window(0, rows, cols);
        let width = rect.w.min(window.cols());
        for (y, key) in (0..rect.h.min(window.rows())).zip(pane_keys.iter_mut()) {
            let row = window.row(y);
            let now = row.map(|r| (r.id(), r.version()));
            if now == *key {
                continue;
            }
            let Some((gy, gx)) = rect.at(y, 0) else {
                continue;
            };
            if row.is_some_and(|r| r.has_links()) {
                sufficed = false;
                break;
            }
            let place = Place {
                gy,
                gx,
                width,
                rect_w: rect.w,
                tiled,
                window_cols: window.cols(),
            };
            draw_row(grid, *id, row, &place, None);
            *key = now;
        }
    }
    grid.memo.keys = keys;
    sufficed
}

/// A hyperlink of a pane's cells (OSC 8): the pane, the link's key there
/// (`fux_vt::Hyperlink::key`, which no other link of the pane has), and
/// where its URI is in the grid's `uris`.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Link {
    pane: PaneId,
    key: u64,
    uri: std::ops::Range<usize>,
}

impl Grid {
    pub fn new(rows: u16, cols: u16) -> Grid {
        Grid {
            rows,
            cols,
            // Exact: a u16 by a u16 fits even a 32-bit usize.
            cells: Cells::new(usize::from(rows).saturating_mul(usize::from(cols))),
            link_of: Vec::new(),
            links: Vec::new(),
            uris: String::new(),
            cursor: None,
            cursor_shape: 0,
            mouse: 0,
            underline_styles: false,
            memo: Memo::default(),
        }
    }
    /// The hyperlink of the cell at `y`, `x`: the pane whose it is, its key
    /// there and its URI.
    pub fn link(&self, y: u16, x: u16) -> Option<(PaneId, u64, &str)> {
        let n = *self.link_of.get(self.index(y, x)?)?;
        self.numbered(n)
    }
    /// Link `n` of the grid.
    fn numbered(&self, n: u32) -> Option<(PaneId, u64, &str)> {
        let link = self.links.get(usize::try_from(n).ok()?.checked_sub(1)?)?;
        Some((link.pane, link.key, self.uris.get(link.uri.clone())?))
    }
    /// The number of pane `pane`'s link `link` in the grid, given one if it
    /// has none yet.
    fn number(&mut self, pane: PaneId, link: fux_vt::Hyperlink<'_>) -> u32 {
        let found = self
            .links
            .iter()
            .rposition(|l| l.pane == pane && l.key == link.key());
        let at = found.unwrap_or_else(|| {
            let start = self.uris.len();
            self.uris.push_str(link.uri());
            self.links.push(Link {
                pane,
                key: link.key(),
                uri: start..self.uris.len(),
            });
            self.links.len().saturating_sub(1)
        });
        // A grid has at most a link a cell, far fewer than a u32 counts.
        u32::try_from(at.saturating_add(1)).unwrap_or(0)
    }
    /// The number of the link of the cell at `y`, `x`; 0 for none.
    fn number_at(&self, y: u16, x: u16) -> u32 {
        self.index(y, x)
            .and_then(|i| self.link_of.get(i))
            .copied()
            .unwrap_or(0)
    }
    /// Cells `range` of the grid have no link.
    fn unlink(&mut self, range: std::ops::Range<usize>) {
        if let Some(run) = self.link_of.get_mut(range) {
            run.fill(0);
        }
    }
    /// Makes the grid `rows` by `cols`, resizing its cells only if the size
    /// changed, so that a grid composed into again allocates nothing; and
    /// blanks it if `blank`, as a resized grid is anyway.
    fn reset(&mut self, rows: u16, cols: u16, blank: bool) {
        self.link_of.clear();
        self.links.clear();
        self.uris.clear();
        if (self.rows, self.cols) == (rows, cols) {
            if blank {
                self.cells.fill(0..self.cells.len(), CellRef::default());
            }
        } else {
            self.rows = rows;
            self.cols = cols;
            self.cells.resize(0, CellRef::default());
            // Exact: a u16 by a u16 fits even a 32-bit usize.
            let len = usize::from(rows).saturating_mul(usize::from(cols));
            self.cells.resize(len, CellRef::default());
        }
        self.cursor = None;
        self.cursor_shape = 0;
        self.mouse = 0;
        self.memo = Memo::default();
    }
    fn index(&self, y: u16, x: u16) -> Option<usize> {
        if y >= self.rows || x >= self.cols {
            return None;
        }
        usize::from(y)
            .checked_mul(usize::from(self.cols))?
            .checked_add(usize::from(x))
    }
    pub fn get(&self, y: u16, x: u16) -> Option<CellRef<'_>> {
        self.index(y, x).and_then(|i| self.cells.get(i))
    }
    /// Forgets what composed the grid (`Memo`): it shows the same, and is
    /// composed whole next time and painted by comparing every row. For
    /// the oracles that check the memo against working without it.
    #[doc(hidden)]
    pub fn forget_memo(&mut self) {
        self.memo = Memo::default();
    }
    /// Whether the grid shows what `other` shows: the same as `==`, without
    /// comparing the rows their memos show are the same (`Memo`).
    pub fn same_as(&self, other: &Grid) -> bool {
        if (
            self.rows,
            self.cols,
            self.cursor,
            self.cursor_shape,
            self.mouse,
        ) != (
            other.rows,
            other.cols,
            other.cursor,
            other.cursor_shape,
            other.mouse,
        ) || self.underline_styles != other.underline_styles
            || self.link_of != other.link_of
            || self.links != other.links
            || self.uris != other.uris
        {
            return false;
        }
        let memo = self.same_frame(other);
        (0..self.rows).all(|y| memo && same_keys(self, other, y) || self.row_eq(other, y))
    }
    /// Whether the two grids were composed for one frame (`Memo`): then a
    /// pane row whose keys are the same in both shows the same cells.
    #[inline]
    fn same_frame(&self, other: &Grid) -> bool {
        self.memo.frame.is_some() && self.memo.frame == other.memo.frame
    }
    /// Whether row `y` has the same cells as `other`'s, a grid of its size:
    /// compared by `Cells::range_eq`, without reading each cell's text.
    fn row_eq(&self, other: &Grid, y: u16) -> bool {
        let cols = usize::from(self.cols);
        // Exact: a u16 by a u16 fits even a 32-bit usize.
        let start = usize::from(y).saturating_mul(cols);
        self.cells
            .range_eq(&other.cells, start..start.saturating_add(cols))
    }
    /// The cells of row `y`; none past the last row.
    pub fn row(&self, y: u16) -> impl Iterator<Item = CellRef<'_>> + Clone {
        let cols = usize::from(self.cols);
        // Exact: a u16 by a u16 fits even a 32-bit usize.
        let start = usize::from(y).saturating_mul(cols);
        let end = if y < self.rows {
            start.saturating_add(cols)
        } else {
            start
        };
        self.cells.range(start..end)
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
            return;
        }
        let mut last: Option<(u64, u32)> = None;
        for (i, col) in (start..).zip(0..room) {
            let n = match row.link(col) {
                None => 0,
                Some(link) => match last {
                    Some((key, n)) if key == link.key() => n,
                    Some(_) | None => {
                        let n = self.number(pane, link);
                        last = Some((link.key(), n));
                        n
                    }
                },
            };
            if n != 0 && self.link_of.is_empty() {
                self.link_of.resize(self.cells.len(), 0);
            }
            if let Some(at) = self.link_of.get_mut(i) {
                *at = n;
            }
        }
    }
    /// Draws the first `width` cells of row `y` from column `x`, a pane's
    /// row `put_row` copied, in the colours its program set
    /// (`pane_colours`): so a pane whose program changed a palette entry,
    /// or its foreground or background, looks through fux as it would
    /// directly in a terminal, and the client's own palette is never
    /// changed (fux sends it no OSC 4, 10 or 11). Each pane's colours are
    /// its own.
    fn recolour(&mut self, y: u16, x: u16, width: u16, screen: &fux_vt::Screen) {
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
            self.unlink(i..i.saturating_add(1));
        }
    }
    /// Blanks row `y` from column `from` to `to`, clipped at the grid's edge.
    fn blank(&mut self, y: u16, from: u16, to: u16) {
        let Some(start) = self.index(y, from) else {
            return;
        };
        let count = usize::from(to.min(self.cols).saturating_sub(from));
        let cells = start..start.saturating_add(count);
        self.cells.fill(cells.clone(), CellRef::default());
        self.unlink(cells);
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
            self.unlink(leader..index);
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
            self.unlink(rest..rest.saturating_add(1));
        }
        self.cells.set(index, cell);
        self.unlink(index..index.saturating_add(1));
    }
    /// The text of a row, trailing blanks trimmed: for `capture-client`.
    pub fn row_text(&self, y: u16) -> String {
        let row = self.row(y).filter(|c| !c.is_wide_continuation());
        row.map(shown).collect::<String>().trim_end().to_owned()
    }

    /// Writes `text` from (y, x), clipped at `limit`, wide glyphs whole; the
    /// column after it is returned.
    fn text(&mut self, y: u16, x: u16, text: &str, style: Attributes, limit: u16) -> u16 {
        let mut x = x;
        for c in text.chars() {
            if c.is_control() {
                continue;
            }
            let width = cells(c);
            if width == 0 {
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
            if width == 2
                && let Some(second) = x.checked_add(1)
            {
                self.set(y, second, CellRef::wide_continuation());
            }
            x = end;
        }
        x
    }

    fn fill(&mut self, y: u16, from: u16, to: u16, style: Attributes) {
        let blank = CellRef::new(" ", false, style);
        for x in from..to.min(self.cols) {
            self.set(y, x, blank);
        }
    }
}

/// `attributes` in the colours a pane's program set (`fux_vt::Options::
/// palette`): an indexed colour whose palette entry it changed (OSC 4) as
/// the colour it set, and the default foreground and background, if it set
/// them (OSC 10, 11), as those; each colour it left alone, or reset, as it
/// is, for the client's terminal to draw in its own. A default underline
/// colour is the foreground's, and stays.
fn pane_colours(attributes: Attributes, screen: &fux_vt::Screen) -> Attributes {
    let rgb = |(r, g, b): (u8, u8, u8)| Color::Rgb(r, g, b);
    let colour = |c: Color, default: Option<(u8, u8, u8)>| match c {
        Color::Idx(n) => screen.palette_color(n).map_or(c, rgb),
        Color::Default => default.map_or(c, rgb),
        Color::Rgb(..) | _ => c,
    };
    attributes
        .with_foreground(colour(attributes.foreground(), screen.dynamic_color(10)))
        .with_background(colour(attributes.background(), screen.dynamic_color(11)))
        .with_underline_color(colour(attributes.underline_color(), None))
}

/// What a cell shows: its text, or a space if it has none.
pub fn shown(cell: CellRef<'_>) -> &str {
    if cell.has_contents() {
        cell.contents()
    } else {
        " "
    }
}

fn style(foreground: Color, background: Color) -> Attributes {
    Attributes::new(foreground, background)
}
const GRAY_BG: Color = Color::Idx(236);
const BAR_FG: Color = Color::Idx(250);
const PANEL_BG: Color = Color::Idx(238);

/// A char's display width in cells.
fn cells(c: char) -> u16 {
    // Widths are 0, 1 or 2; a larger one could never fit, so it saturates.
    c.width()
        .map_or(0, |w| u16::try_from(w).unwrap_or(u16::MAX))
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

/// The client's screen as it should look now, in a grid of its own.
pub fn compose(session: &Session, client: ClientId) -> Option<Grid> {
    let mut grid = Grid::new(0, 0);
    compose_into(session, client, &mut grid, &mut Placement::default()).then_some(grid)
}

/// Composes the client's screen as it should look now into `grid`, whatever
/// it held, laying out its panes in `placement`; false if there is no such
/// client. The grid's cells and the placement are reused, not made again;
/// the frame's memo, the bar's text and the like are made each time.
pub fn compose_into(
    session: &Session,
    client: ClientId,
    grid: &mut Grid,
    placement: &mut Placement,
) -> bool {
    let view = session.views.get(&client);
    view.map(|view| compose_view(session, view, grid, placement))
        .is_some()
}

/// Composes a view's screen, as `compose_into` a client's.
pub fn compose_view(session: &Session, view: &View, grid: &mut Grid, placement: &mut Placement) {
    let area = Session::pane_area(view);
    session.placement_into(view, placement);
    let placement = &*placement;
    // The panes and separators, which never overlap, cover the pane area
    // unless a split has no room for even its first child; the bar covers
    // its row. Covered, every cell is written below, and the grid needs no
    // blanking first.
    let covered = placement
        .panes
        .iter()
        .map(|(_, r)| u32::from(r.w).saturating_mul(u32::from(r.h)))
        .chain(placement.separators.iter().map(|s| u32::from(s.len)))
        .fold(0u32, u32::saturating_add);
    let tiled = covered == u32::from(area.w).saturating_mul(u32::from(area.h));
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
    // The grid holds this very frame: only the pane rows that changed since
    // it was composed are put again (`Memo`).
    let frame = Frame::of(session, view, placement, tiled, focus);
    let reused = frame.is_some()
        && grid.memo.frame == frame
        && (grid.rows, grid.cols) == (view.rows, view.cols)
        && put_changed_rows(grid, session, placement, tiled);
    if reused {
        grid.cursor = None;
        grid.cursor_shape = 0;
        grid.mouse = 0;
    } else {
        grid.reset(view.rows, view.cols, !tiled);
        grid.underline_styles = view.terminal.underline_styles;
        let mut keys = frame.as_ref().map(|_| Vec::new());
        for (id, rect) in &placement.panes {
            let Some(pane) = session.panes.get(id) else {
                for (gy, gx) in (0..rect.h).filter_map(|y| rect.at(y, 0)).filter(|_| tiled) {
                    grid.blank(gy, gx, gx.saturating_add(rect.w));
                }
                continue;
            };
            let screen = pane.screen();
            // A pane whose program changed its colours is drawn in them.
            let colours = screen.colors_changed().then_some(screen);
            let at = copy.filter(|(c, _)| c.pane == *id).map(|(_, at)| at);
            let offset = at.map_or(0, |at| at.offset(screen));
            let (rows, cols) = screen.size();
            let window = screen.window(offset, rows, cols);
            let width = rect.w.min(window.cols());
            let screen_rows = rect.h.min(window.rows());
            // What the pane's screen does not cover of its place is blank.
            for (gy, gx) in (screen_rows..rect.h)
                .filter_map(|y| rect.at(y, 0))
                .filter(|_| tiled)
            {
                grid.blank(gy, gx, gx.saturating_add(rect.w));
            }
            let mut pane_keys = Vec::new();
            for y in 0..screen_rows {
                // Past the largest position is off the grid anyway.
                let Some((gy, gx)) = rect.at(y, 0) else {
                    pane_keys.push(None);
                    continue;
                };
                let row = window.row(y);
                if row.is_some_and(|r| r.has_links()) {
                    // Links are numbered per paint: no memo for this grid.
                    keys = None;
                }
                pane_keys.push(row.map(|r| (r.id(), r.version())));
                let place = Place {
                    gy,
                    gx,
                    width,
                    rect_w: rect.w,
                    tiled,
                    window_cols: window.cols(),
                };
                draw_row(grid, *id, row, &place, colours);
                let Some(at) = at else { continue };
                for x in 0..width {
                    // A wide glyph is selected if either half is, as `y`
                    // copies it whole.
                    if let Some(i) = gx.checked_add(x).and_then(|x| grid.index(gy, x))
                        && let Some(cell) = grid.cells.get(i)
                        && !cell.is_wide_continuation()
                        && (at.selected(y, x)
                            || cell.is_wide()
                                && x.checked_add(1).is_some_and(|x| at.selected(y, x)))
                    {
                        let attrs = cell.attributes().with_inverse(!cell.inverse());
                        grid.cells.set_attributes(i, attrs);
                    }
                }
            }
            if let Some(keys) = &mut keys {
                keys.push((*id, pane_keys));
            }
        }
        separators(grid, placement, focus);
        grid.memo = match (frame, keys) {
            (Some(frame), Some(keys)) => Memo {
                frame: Some(frame),
                keys,
            },
            (None | Some(_), None) | (None, Some(_)) => Memo::default(),
        };
    }
    if tab.is_some_and(|t| t.root().is_none()) && area.h > 0 {
        // The keys bound to these commands, whatever they are; the command
        // itself if none is.
        let key_for = |argv: &[&str]| {
            session
                .config
                .bindings
                .iter()
                .find(|b| b.command == argv)
                .map_or_else(|| argv.join(" "), |b| session.keys_named(&b.keys))
        };
        let hint = format!(
            "empty tab: {} splits it, {} closes it",
            key_for(&["split", "-h"]),
            key_for(&["confirm-close", "tab"])
        );
        let y = area.h / 2;
        let x = area.w.saturating_sub(width(&hint)) / 2;
        grid.text(y, x, &hint, style(Color::Idx(244), Color::Default), area.w);
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
                if let Some((y, x)) = at.cursor_in_view(rect.h.min(screen.size().0))
                    && x < rect.w
                    && let Some(at) = rect.at(y, x)
                {
                    grid.cursor = Some(at);
                    // The copy cursor is a block.
                    grid.cursor_shape = 2;
                }
            }
            None => {
                let (y, x) = screen.cursor_position();
                if screen.mode(fux_vt::Mode::ShowCursor)
                    && y < rect.h
                    && x < rect.w
                    && matches!(view.mode, Mode::Normal)
                    && let Some(at) = rect.at(y, x)
                {
                    grid.cursor = Some(at);
                    grid.cursor_shape = screen.cursor_shape();
                }
            }
        }
    }
    // The mouse: reported by the client's terminal only while the focused
    // pane's program asked for it and nothing of fux's has the keys, so
    // that the rest of the time the terminal selects text as it does.
    if matches!(view.mode, Mode::Normal)
        && let Some(focus) = focus
        && placement.rect(focus).is_some()
        && let Some(pane) = session.panes.get(&focus)
    {
        grid.mouse = mouse_level(pane.screen().mouse_protocol_mode());
    }
    bar(grid, session, view, copy);
    match &view.mode {
        Mode::Column(c) => column(grid, session, view, c),
        Mode::List(list) => list_panel(grid, session, view, list),
        Mode::Prompt(prompt) => {
            // The panel's border takes a cell each side.
            let room = view.cols.saturating_sub(2);
            let lines: [Line<'_>; 3] = [
                (prompt.title.as_str().into(), panel().with_bold(true)),
                (prompt_line(&prompt.line, room).into(), panel()),
                ("Enter accepts · Esc cancels".into(), panel().with_dim(true)),
            ];
            surface(grid, view, &lines);
        }
        Mode::Confirm(confirm) => {
            let lines: [Line<'_>; 2] = [
                (confirm.question.as_str().into(), panel().with_bold(true)),
                ("y confirms · n or Esc cancels".into(), panel()),
            ];
            surface(grid, view, &lines);
        }
        // A repeat mode shows in the bar, leaving the layout in view.
        Mode::Normal | Mode::Copy(_) | Mode::Repeat(_) => {}
    }
}

/// A prompt's text with its cursor bar, in at most `room` cells: when it is
/// wider, the line scrolls so the bar shows, with a few cells of what
/// follows it, and an ellipsis marks each side cut off.
fn prompt_line(text: &crate::view::Line, room: u16) -> String {
    let line = format!("{}▏{}", text.before(), text.after());
    if width(&line) <= room {
        return line;
    }
    let chars: Vec<char> = line.chars().filter(|c| !c.is_control()).collect();
    let bar = text.before().chars().filter(|c| !c.is_control()).count();
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

fn panel() -> Attributes {
    style(Color::Idx(255), PANEL_BG)
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
    let (rows, cols) = (grid.rows, grid.cols);
    // The axis of the line drawn at a cell on the grid, if one is: the last
    // separator there, as it is drawn last.
    let axis_at = |x: Option<u16>, y: Option<u16>| {
        let (x, y) = (x?, y?);
        if x >= cols || y >= rows {
            return None;
        }
        lines
            .iter()
            .rev()
            .find(|s| s.rect().contains(x, y))
            .map(|s| s.axis)
    };
    // A line's own directions, and a tee toward each perpendicular line
    // beside it.
    let bits_at = |x: u16, y: u16| {
        let beside = |bit: u8, x: Option<u16>, y: Option<u16>, axis: Axis| {
            if axis_at(x, y) == Some(axis) { bit } else { 0 }
        };
        let (at_x, at_y) = (Some(x), Some(y));
        // A separator's axis is its split's: a horizontal split's panes
        // are side by side, divided by vertical lines.
        Some(match axis_at(at_x, at_y)? {
            // A vertical line.
            Axis::Horizontal => {
                UP | DOWN
                    | beside(LEFT, x.checked_sub(1), at_y, Axis::Vertical)
                    | beside(RIGHT, x.checked_add(1), at_y, Axis::Vertical)
            }
            // A horizontal line.
            Axis::Vertical => {
                LEFT | RIGHT
                    | beside(UP, at_x, y.checked_sub(1), Axis::Horizontal)
                    | beside(DOWN, at_x, y.checked_add(1), Axis::Horizontal)
            }
        })
    };
    let focused = focus.and_then(|f| placement.rect(f));
    // Within a cell of the rect. Exact: values from u16s never saturate an i32.
    let near = |x: u16, y: u16, r: &Rect| {
        let (x, y, rx, ry) = (i32::from(x), i32::from(y), i32::from(r.x), i32::from(r.y));
        x >= rx.saturating_sub(1)
            && x <= rx.saturating_add(i32::from(r.w))
            && y >= ry.saturating_sub(1)
            && y <= ry.saturating_add(i32::from(r.h))
    };
    let draw = |grid: &mut Grid, x: u16, y: u16, bits: u8| {
        let glyph = match bits {
            b if b == UP | DOWN => "│",
            b if b == LEFT | RIGHT => "─",
            b if b == UP | DOWN | RIGHT => "├",
            b if b == UP | DOWN | LEFT => "┤",
            b if b == LEFT | RIGHT | DOWN => "┬",
            b if b == LEFT | RIGHT | UP => "┴",
            _ => "┼",
        };
        let color = if focused.is_some_and(|r| near(x, y, &r)) {
            Color::Idx(2)
        } else {
            Color::Idx(240)
        };
        // A separator's cell is no pane's, so no wide glyph has a half there
        // to repair: it is only ever drawn over.
        grid.put(
            y,
            x,
            CellRef::new(glyph, false, style(color, Color::Default)),
        );
    };
    // Each line plain, in order, so the last one drawn at a cell is its.
    for s in lines {
        let bits = match s.axis {
            Axis::Horizontal => UP | DOWN,
            Axis::Vertical => LEFT | RIGHT,
        };
        for i in 0..s.len {
            let at = match s.axis {
                Axis::Horizontal => s.y.checked_add(i).map(|y| (s.x, y)),
                Axis::Vertical => s.x.checked_add(i).map(|x| (x, s.y)),
            };
            // Past the largest position is off the grid.
            let Some((x, y)) = at else {
                break;
            };
            if x < cols && y < rows {
                draw(grid, x, y, bits);
            }
        }
    }
    // A tee can only be where a vertical line and a horizontal one meet or
    // cross, at the vertical one's column and the horizontal one's row.
    // (The vertical lines are a horizontal split's, as above.)
    let vertical = lines.iter().filter(|s| s.axis == Axis::Horizontal);
    let horizontal = || lines.iter().filter(|s| s.axis == Axis::Vertical);
    for v in vertical.map(Separator::rect) {
        for h in horizontal().map(Separator::rect) {
            let (x, y) = (v.x, h.y);
            let near_x = [x.checked_sub(1), Some(x), x.checked_add(1)];
            let near_y = [y.checked_sub(1), Some(y), y.checked_add(1)];
            let meet = near_x.iter().flatten().any(|x| h.contains(*x, y))
                && near_y.iter().flatten().any(|y| v.contains(x, *y));
            if meet && let Some(bits) = bits_at(x, y) {
                draw(grid, x, y, bits);
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
    let base = style(BAR_FG, GRAY_BG);
    grid.fill(y, 0, view.cols, base);
    let copy_bar =
        copy.and_then(|(c, at)| session.panes.get(&c.pane).map(|p| c.bar(p.screen(), &at)));
    let right: Option<(Cow<'_, str>, Attributes)> = if let Some(notice) = &view.notice {
        Some((
            notice.text.as_str().into(),
            if notice.error {
                style(Color::Idx(9), GRAY_BG)
            } else {
                style(Color::Idx(11), GRAY_BG)
            },
        ))
    } else if let Some(copy) = &copy_bar {
        Some((copy.position.as_str().into(), base))
    } else if let Mode::Column(column) = &view.mode {
        Some((
            format!("{} …", session.keys_named(&column.path)).into(),
            style(Color::Idx(0), Color::Idx(11)),
        ))
    } else if let Mode::Repeat(repeat) = &view.mode {
        Some((
            repeat.bar.as_str().into(),
            style(Color::Idx(0), Color::Idx(11)).with_bold(true),
        ))
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
        let badge = style(Color::Idx(0), Color::Idx(11)).with_bold(true);
        x = grid.text(
            y,
            x,
            &fit(&format!(" {} ", copy.badge), left_limit),
            badge,
            left_limit,
        );
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
                style(Color::Idx(0), Color::Idx(2)).with_bold(true)
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

/// Adds the entries `shown`, those from `start` that fit `room` of
/// `total`, to a panel's `lines`, with how many more there are above and
/// below them.
fn windowed<'a>(
    lines: &mut Vec<Line<'a>>,
    total: usize,
    start: usize,
    room: usize,
    shown: impl Iterator<Item = Line<'a>>,
) {
    if start > 0 {
        lines.push((format!("▲ {start} more").into(), panel().with_dim(true)));
    }
    lines.extend(shown);
    let below = total.saturating_sub(start.saturating_add(room));
    if below > 0 {
        lines.push((format!("▼ {below} more").into(), panel().with_dim(true)));
    }
}

/// A panel in the bottom-right corner, above the bar, sized to its lines.
fn surface(grid: &mut Grid, view: &View, lines: &[Line<'_>]) {
    let available = view.rows.saturating_sub(1);
    if available == 0 || view.cols == 0 || lines.is_empty() {
        return;
    }
    // More lines than rows is the same as exactly as many.
    let height = u16::try_from(lines.len())
        .unwrap_or(u16::MAX)
        .min(available);
    let inner = lines.iter().map(|(t, _)| width(t)).max().unwrap_or(0);
    let w = inner.saturating_add(2).min(view.cols);
    // The panel fits: `w` is at most the width and `height` the rows above
    // the bar, and the text starts inside it.
    let (Some(x), Some(top)) = (view.cols.checked_sub(w), available.checked_sub(height)) else {
        return;
    };
    let Some(text_x) = x.checked_add(1) else {
        return;
    };
    // On a short screen the first lines give way, as the last (the help)
    // matter more; but not past the selected entry, which stays in view.
    let skip = lines.len().saturating_sub(usize::from(height));
    let selected = lines.iter().position(|(_, attrs)| attrs.inverse());
    let skip = selected.map_or(skip, |selected| skip.min(selected));
    for (y, (text, attrs)) in (top..available).zip(lines.iter().skip(skip)) {
        grid.fill(y, x, view.cols, *attrs);
        grid.text(
            y,
            text_x,
            &fit(text, w.saturating_sub(2)),
            *attrs,
            view.cols.saturating_sub(1).max(text_x),
        );
    }
}

/// A list to choose from or run: the entries that fit, the selected one
/// highlighted and, but in a chooser, those that cannot run now dimmed.
fn list_panel(grid: &mut Grid, session: &Session, view: &View, list: &List) {
    let mut lines: Vec<Line<'_>> = vec![(list.title.as_str().into(), panel().with_bold(true))];
    let capacity = overlay::list_room(view.rows);
    let (len, chosen) = (list.items.iter().count(), list.items.index());
    let start = overlay::window_start(len, chosen, capacity);
    let ctx = crate::session::Ctx::client(view.id);
    let shown = list.items.iter().enumerate().skip(start).take(capacity);
    let shown = shown.map(|(i, item)| {
        let dim = item.subject.is_none() && session.unavailable(&item.command, &ctx).is_some();
        let marker = if item.current { "*" } else { " " };
        let attrs = panel().with_inverse(i == chosen).with_dim(dim);
        (format!("{marker} {}", item.label).into(), attrs)
    });
    windowed(&mut lines, len, start, capacity, shown);
    let help = if list.items.chosen().subject.is_some() {
        "Enter selects · r renames · x closes · Esc"
    } else {
        "Enter runs · Esc cancels"
    };
    lines.push((help.into(), panel().with_dim(true)));
    surface(grid, view, &lines);
}

/// The command column: the bindings and layers of its layer, grouped, the
/// selected one highlighted and those that cannot run now dimmed.
fn column(grid: &mut Grid, session: &Session, view: &View, column: &overlay::Column) {
    let all = column.entries.iter().flat_map(Choice::iter);
    let chosen = column.entries.as_ref().map(Choice::index);
    // Each entry's key as it is typed, written out once.
    let keys: Vec<String> = all.clone().map(|e| e.key.to_string()).collect();
    let key_width = keys.iter().map(|k| width(k)).max().unwrap_or(0);
    let ctx = crate::session::Ctx::client(view.id);
    let mut entries: Vec<Line<'_>> = Vec::new();
    let mut heading = None;
    let mut selected_row = 0usize;
    for (index, (entry, key)) in all.zip(&keys).enumerate() {
        let group = (entry.root, entry.group.as_str());
        if heading != Some(group) {
            entries.push((group.1.into(), panel().with_bold(true)));
            heading = Some(group);
        }
        // Whether it cannot run now; a layer's entry opens it.
        let dim = entry
            .command
            .as_ref()
            .is_some_and(|command| session.unavailable(command, &ctx).is_some());
        let more = if entry.command.is_none() { "…" } else { "" };
        let pad = usize::from(key_width.saturating_sub(width(key)));
        let mut attrs = panel().with_dim(dim);
        if Some(index) == chosen {
            attrs = attrs.with_inverse(true);
            selected_row = entries.len();
        }
        let text = &entry.label;
        entries.push((format!("{:pad$}{key}  {text}{more}", "").into(), attrs));
    }
    let (heading, body_room) = overlay::column_room(view.rows);
    let start = overlay::window_start(entries.len(), selected_row, body_room);
    let mut lines: Vec<Line<'_>> = Vec::new();
    if heading {
        lines.push((column.title.as_str().into(), panel().with_bold(true)));
    }
    let total = entries.len();
    let shown = entries.into_iter().skip(start).take(body_room);
    windowed(&mut lines, total, start, body_room, shown);
    if column.entries.is_none() {
        lines.push(("no bindings".into(), panel().with_dim(true)));
    }
    surface(grid, view, &lines);
}

// ------------------------------------------------------------------ paint

/// The SGR that sets `a` from nothing: an underline style (kitty's `4:n`)
/// as it is if the client's terminal draws them (`styles`, `outer::styles!`),
/// else a plain underline, as a terminal that does not know `4:3` draws no
/// underline at all (xterm, avt) or reads the colon as a semicolon,
/// underline and italic, and one that does not know 21 may read it as bold
/// off (alacritty, avt). The underline colour goes either way: see below.
fn sgr(out: &mut Vec<u8>, a: Attributes, styles: bool) {
    out.extend_from_slice(b"\x1b[0");
    if a.bold() {
        out.extend_from_slice(b";1");
    }
    if a.dim() {
        out.extend_from_slice(b";2");
    }
    if a.italic() {
        out.extend_from_slice(b";3");
    }
    match a.underline_style() {
        UnderlineStyle::None => {}
        style @ (UnderlineStyle::Double
        | UnderlineStyle::Curly
        | UnderlineStyle::Dotted
        | UnderlineStyle::Dashed)
            if styles =>
        {
            let _ = write!(out, ";4:{}", style.number());
        }
        // Single, a style the terminal does not draw, or one fux-vt does
        // not know yet: a plain underline.
        UnderlineStyle::Single | _ => out.extend_from_slice(b";4"),
    }
    // A kind of blink fux-vt does not know yet is drawn as none.
    match a.blink() {
        fux_vt::Blink::Slow => out.extend_from_slice(b";5"),
        fux_vt::Blink::Rapid => out.extend_from_slice(b";6"),
        fux_vt::Blink::None | _ => {}
    }
    if a.inverse() {
        out.extend_from_slice(b";7");
    }
    if a.hidden() {
        out.extend_from_slice(b";8");
    }
    if a.strikeout() {
        out.extend_from_slice(b";9");
    }
    // The underline colour in ITU-T T.416's colon form (§13.1.8), with its
    // empty colour-space slot: a terminal that does not know SGR 58 skips
    // the whole parameter, where in the semicolon form it would take the
    // colour's numbers for attributes of their own (`58;2;…` would be dim).
    // So it goes to every terminal, styles or not: each engine in
    // `fux-vt/compare` reads `58:5:9` and `58:2::1:2:3` beside 4 and 1 as
    // 4 and 1 alone, xterm, which has no SGR 58, included (xterm 411
    // skips an SGR parameter with subparameters other than 38's and 48's).
    match a.underline_color() {
        Color::Idx(n) => {
            let _ = write!(out, ";58:5:{n}");
        }
        Color::Rgb(r, g, b) => {
            let _ = write!(out, ";58:2::{r}:{g}:{b}");
        }
        // A kind of colour fux-vt does not know yet is drawn as the default.
        Color::Default | _ => {}
    }
    // `base` is 30 or 40, so no code comes near 255: every sum is exact.
    let color = |out: &mut Vec<u8>, c: Color, base: u8| match c {
        Color::Idx(n) if n < 8 => {
            let _ = write!(out, ";{}", base.saturating_add(n));
        }
        // The bright colours 8–15 are 90–97 and 100–107.
        Color::Idx(n) if n < 16 => {
            let _ = write!(out, ";{}", base.saturating_add(52).saturating_add(n));
        }
        Color::Idx(n) => {
            let _ = write!(out, ";{};5;{n}", base.saturating_add(8));
        }
        Color::Rgb(r, g, b) => {
            let _ = write!(out, ";{};2;{r};{g};{b}", base.saturating_add(8));
        }
        Color::Default | _ => {}
    };
    color(out, a.foreground(), 30);
    color(out, a.background(), 40);
    out.push(b'm');
}

/// OSC 8: opens `link` in the client's terminal, or closes the open link.
/// Its `id` is the pane's and the link's key there, which no other link of
/// the pane has: no two panes' links are one to the terminal, and a link
/// painted in pieces, or whose program gave no `id`, is one link, as the
/// spec asks a multiplexer to make it (`references/modern/osc8_hyperlinks.md`,
/// "Hover underlining and the `id` parameter"). A terminal that does not
/// know OSC 8 ignores it.
fn hyperlink(out: &mut Vec<u8>, link: Option<(PaneId, u64, &str)>) {
    match link {
        Some((pane, key, uri)) => {
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
        .is_some_and(|glyph| fux_vt::continues_cluster(shown(glyph), first))
}

/// Paints `text`, a glyph `width` columns wide at (`y`, `x`) in a run of
/// cells painted one after another, where it is not one byte of ASCII or
/// follows a glyph that `joined` the one before it; whether it joins the one
/// before it.
///
/// A glyph that continues the cluster of the one before it (an emoji
/// modifier a program put after an emoji with a cursor move of its own, as
/// micro and vim do) is placed by a cursor move, and so is what follows it:
/// the client's terminal joins it to the glyph before, or not, as it does
/// when the program writes it directly, and the next glyph is where fux-vt
/// has it either way.
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
        let _ = write!(out, "\x1b[{};{}H", one_based(y), one_based(x));
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
    joins
}

/// A row or column as the terminal counts it, from 1; exact in a u32.
fn one_based(n: u16) -> u32 {
    u32::from(n).saturating_add(1)
}

/// Whether row `y` of two grids of one frame (`Memo`) is in the
/// pane area and has the same pane rows in both: then it shows the same
/// cells.
fn same_keys(a: &Grid, b: &Grid, y: u16) -> bool {
    let Some(frame) = &a.memo.frame else {
        return false;
    };
    // The bar's row, and any row past the panes', are not memo'd.
    if y >= frame.rows.saturating_sub(1) {
        return false;
    }
    a.memo.keys.len() == frame.panes.len()
        && b.memo.keys.len() == frame.panes.len()
        && frame
            .panes
            .iter()
            .zip(a.memo.keys.iter().zip(&b.memo.keys))
            .all(|((_, rect, _), ((_, ka), (_, kb)))| {
                if y < rect.y || u32::from(y) >= u32::from(rect.y).saturating_add(u32::from(rect.h))
                {
                    return true;
                }
                let i = usize::from(y.saturating_sub(rect.y));
                ka.get(i) == kb.get(i)
            })
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
        // `AnyMotion`, and a mode fux-vt may add later: the most.
        M::AnyMotion | _ => 1003,
    }
}

/// The bytes that turn `old` (what the client shows, or nothing) into `new`.
pub fn paint(old: Option<&Grid>, new: &Grid) -> Vec<u8> {
    let mut out = Vec::new();
    paint_into(old, new, &mut out);
    out
}

/// Appends the bytes that turn `old` (what the client shows, or nothing)
/// into `new` to `out`.
pub fn paint_into(old: Option<&Grid>, new: &Grid, out: &mut Vec<u8>) {
    if let Some(old) = old
        && echo(old, new, out)
    {
        return;
    }
    paint_whole(old, new, out);
}

/// Whether row `y` of two grids of a size shows the same: its cells, and,
/// if either grid has links (`links`), its cells' links.
#[inline]
fn same_row(old: &Grid, new: &Grid, y: u16, links: bool) -> bool {
    old.row_eq(new, y) && (!links || (0..new.cols).all(|x| old.link(y, x) == new.link(y, x)))
}

/// The rows of two grids of a size that differ: those whose memo does not
/// show them the same and whose cells or links differ.
fn differing_rows<'a>(old: &'a Grid, new: &'a Grid) -> impl Iterator<Item = u16> + 'a {
    let links = !new.link_of.is_empty() || !old.link_of.is_empty();
    let memo = old.same_frame(new);
    (0..new.rows)
        .filter(move |&y| !(memo && same_keys(old, new, y)) && !same_row(old, new, y, links))
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
    if (
        old.rows,
        old.cols,
        old.underline_styles,
        old.mouse,
        old.cursor_shape,
    ) != (
        new.rows,
        new.cols,
        new.underline_styles,
        new.mouse,
        new.cursor_shape,
    ) || !old.link_of.is_empty()
        || !new.link_of.is_empty()
    {
        return false;
    }
    let (Some((oy, ox)), Some((y, to))) = (old.cursor, new.cursor) else {
        return false;
    };
    if to == 0 || to >= new.cols {
        return false;
    }
    let mut rows = differing_rows(old, new);
    if rows.next() != Some(y) || rows.next().is_some() {
        return false;
    }
    // The run is from the first changed cell of the row to the cursor's
    // cell; nothing after it changed. Cells in it that did not change are
    // written as they are.
    let start = usize::from(y).saturating_mul(usize::from(new.cols));
    let same = |x: u16| {
        let i = start.saturating_add(usize::from(x));
        old.cells.range_eq(&new.cells, i..i.saturating_add(1))
    };
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
                        && UnicodeWidthChar::width(ch) == Some(1)
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
        let _ = write!(out, "\x1b[{};{}H", one_based(y), one_based(from));
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
fn paint_whole(old: Option<&Grid>, new: &Grid, out: &mut Vec<u8>) {
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
    // The hyperlink the client's terminal has open: a number of `new`'s.
    let mut open = 0u32;
    // Whether either grid has a link: if neither does, no cell's changed.
    let links = !new.link_of.is_empty() || old.is_some_and(|o| !o.link_of.is_empty());
    // Grids of one frame show the same cells on a pane row whose keys are
    // the same in both (`Memo`): no comparison needed.
    let memo = old.filter(|o| !full && o.same_frame(new));
    // What the client shows, unless all is painted whole: a grid of the same
    // size, so the two index their cells alike.
    let before = old.filter(|_| !full);
    for y in 0..new.rows {
        if memo.is_some_and(|o| same_keys(o, new, y)) {
            continue;
        }
        // An unchanged row costs this one comparison.
        if before.is_some_and(|o| same_row(o, new, y, links)) {
            continue;
        }
        let cell = |x: u16| new.get(y, x);
        let row_start = usize::from(y).saturating_mul(usize::from(new.cols));
        let changed = |x: u16| {
            before.is_none_or(|o| {
                let i = row_start.saturating_add(usize::from(x));
                !o.cells.range_eq(&new.cells, i..i.saturating_add(1))
                    || links && o.link(y, x) != new.link(y, x)
            })
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
            let _ = write!(out, "\x1b[{};{}H", one_based(y), one_based(start));
            let mut cx = start;
            // Whether the glyph before was one the client's terminal may
            // have joined to the one before it (see `cluster`).
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
                    shown(cell)
                };
                let attrs = cell.attributes();
                if current != Some(attrs) {
                    sgr(out, attrs, new.underline_styles);
                    current = Some(attrs);
                }
                let link = new.number_at(y, cx);
                if link != open {
                    hyperlink(out, new.numbered(link));
                    open = link;
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
    if open != 0 {
        hyperlink(out, None);
    }
    out.extend_from_slice(b"\x1b[0m");
    // From nothing, said either way: a client repainted whole may have had
    // reporting on.
    if old.is_none_or(|o| o.mouse != new.mouse) {
        out.extend_from_slice(MOUSE_OFF.as_bytes());
        if new.mouse != 0 {
            let _ = write!(out, "\x1b[?{}h\x1b[?1006h", new.mouse);
        }
    }
    let shape_changed = old.is_none_or(|o| o.cursor_shape != new.cursor_shape);
    if shape_changed {
        let _ = write!(out, "\x1b[{} q", new.cursor_shape);
    }
    if let Some((y, x)) = new.cursor {
        let _ = write!(out, "\x1b[{};{}H\x1b[?25h", one_based(y), one_based(x));
    }
    out.extend_from_slice(b"\x1b[?2026l");
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let screen = parser.screen();
        let (rows, cols) = screen.size();
        let window = screen.window(0, rows, cols);
        let cells = (0..rows)
            .map(|y| {
                window.row(y).map_or_else(Vec::new, |row| {
                    row.cells()
                        .map(|c| (c.contents().to_owned(), c.attributes()))
                        .collect()
                })
            })
            .collect();
        (cells, screen.cursor_position())
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
                let argv = [
                    "split".to_owned(),
                    "-h".to_owned(),
                    "-t".to_owned(),
                    "%1".to_owned(),
                ];
                s.run(&argv, &crate::session::Ctx::default());
            }
            let mut fast = fux_vt::Parser::new(10, 40, 0).map_err(|e| e.to_string())?;
            let mut whole = fux_vt::Parser::new(10, 40, 0).map_err(|e| e.to_string())?;
            let (mut spare, mut showing) = (Grid::new(0, 0), Grid::new(0, 0));
            let mut placement = Placement::default();
            let mut painted = false;
            for step in 0..200 {
                let out = typed.get(r.below(typed.len())).copied().unwrap_or(b"");
                let panes: Vec<PaneId> = s.panes.keys().copied().collect();
                if let Some(&id) = panes.get(r.below(panes.len().max(1))) {
                    s.output(id, out);
                }
                s.settle_if_needed();
                if !compose_into(&s, c, &mut spare, &mut placement) {
                    continue;
                }
                let bytes = paint(painted.then_some(&showing), &spare);
                let (mut plain_old, mut plain_new) = (showing.clone(), spare.clone());
                plain_old.forget_memo();
                plain_new.forget_memo();
                let mut slow = Vec::new();
                paint_whole(painted.then_some(&plain_old), &plain_new, &mut slow);
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
            "split -h -t %1",
            "split -v -t %1",
            "zoom -c c1",
            "copy-mode -c c1",
            "command-column -c c1",
            "select-pane -c c1 --next",
            "kill-pane -t %2",
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
                        let argv: Vec<String> = line.split(' ').map(str::to_owned).collect();
                        s.run(&argv, &crate::session::Ctx::default());
                    }
                    1 => s.input(c, b"\x1b"),
                    2 => {
                        let rows = u16::try_from(4 + r.below(12)).unwrap_or(8);
                        let cols = u16::try_from(10 + r.below(50)).unwrap_or(40);
                        s.resize(c, rows, cols);
                    }
                    3 => {
                        let argv = [
                            "rename".to_owned(),
                            "-t".to_owned(),
                            "@1".to_owned(),
                            "n".to_owned(),
                        ];
                        s.run(&argv, &crate::session::Ctx::default());
                    }
                    _ => {
                        let panes: Vec<PaneId> = s.panes.keys().copied().collect();
                        if let Some(&id) = panes.get(r.below(panes.len().max(1))) {
                            let out = outputs.get(r.below(outputs.len())).copied().unwrap_or(b"");
                            s.output(id, out);
                        }
                    }
                }
                s.settle_if_needed();
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
        on.mouse = 1002;
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
        any.mouse = 1003;
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

    fn apply(bytes: &[u8], rows: u16, cols: u16, parser: &mut fux_vt::Parser) -> Vec<String> {
        let _ = parser.process(bytes);
        let screen = parser.screen();
        (0..rows)
            .map(|y| {
                let window = screen.window(0, rows, cols);
                window
                    .row(y)
                    .map(crate::session::row_text)
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
        let outcome = s.run(
            &["split".to_owned(), "-h".to_owned()],
            &crate::session::Ctx::client(c),
        );
        assert_eq!(outcome.status, 0, "{}", outcome.stderr);
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
            other.cursor_shape = 3;
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
        let mut parser = fux_vt::Parser::new(1, 12, 0).map_err(|e| e.to_string())?;
        assert_eq!(apply(&bytes, 1, 12, &mut parser), grid_lines(&grid));
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
            grid.fill(0, 0, 6, Attributes::default());
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
        let mut parser = fux_vt::Parser::new(1, 6, 0).map_err(|e| e.to_string())?;
        assert_eq!(apply(text.as_bytes(), 1, 6, &mut parser), grid_lines(&grid));
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
            let mut line = crate::view::Line::new(text.into());
            (0..left).for_each(|_| line.left());
            line
        };
        let end = text.chars().count();
        for (left, starts, ends) in [(0, "…", "TAIL▏"), (end, "▏spl", "…"), (20, "…", "…")]
        {
            let shown = prompt_line(&line(text, left), 12);
            assert!(width(&shown) <= 12, "{shown:?} fits");
            assert!(shown.contains('▏'), "{shown:?} shows the cursor");
            assert!(shown.starts_with(starts), "{shown:?} starts {starts:?}");
            assert!(shown.ends_with(ends), "{shown:?} ends {ends:?}");
        }
        // Wide chars count two cells; short text is as it was.
        assert!(width(&prompt_line(&line("界界界界界界界界", 0), 7)) <= 7);
        assert_eq!(prompt_line(&line("ls", 0), 12), "ls▏");
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
        let mut parser = fux_vt::Parser::new(4, 12, 0).map_err(|e| e.to_string())?;
        apply(&paint(None, &a), 4, 12, &mut parser);
        let lines = apply(&paint(Some(&a), &b), 4, 12, &mut parser);
        assert_eq!(lines, grid_lines(&b));
        // Full repaint from nothing matches too.
        let mut fresh = fux_vt::Parser::new(4, 12, 0).map_err(|e| e.to_string())?;
        assert_eq!(apply(&paint(None, &b), 4, 12, &mut fresh), grid_lines(&b));
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
        let mut parser = fux_vt::Parser::new(6, 20, 0).map_err(|e| e.to_string())?;
        apply(&paint(None, &a), 6, 20, &mut parser);
        assert_eq!(apply(&diff, 6, 20, &mut parser), grid_lines(&b));
        // The first and last rows, and a wide glyph's second half.
        let mut c = b.clone();
        c.text(0, 19, "!", Attributes::default(), 20);
        c.text(5, 7, "y", Attributes::default(), 20);
        let diff = paint(Some(&b), &c);
        assert_eq!(rows_written(&diff), [0, 5]);
        assert_eq!(apply(&diff, 6, 20, &mut parser), grid_lines(&c));
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
                .with_underline_color(Color::Rgb(1, 2, 3)),
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
        let mut parser = fux_vt::Parser::new(1, 12, 0).map_err(|e| e.to_string())?;
        parser.process(&bytes).map_err(|e| e.to_string())?;
        for (x, a) in attributes.iter().enumerate().take(5) {
            let x = u16::try_from(x).map_err(|e| e.to_string())?;
            let cell = parser.screen().cell(0, x).ok_or("a cell")?;
            assert_eq!(cell.attributes(), *a, "cell {x}");
        }
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
                .with_underline_color(Color::Rgb(255, 0, 0));
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
            let mut parser = fux_vt::Parser::new(1, 6, 0).map_err(|e| e.to_string())?;
            parser
                .process(&paint(None, grid))
                .map_err(|e| e.to_string())?;
            for (x, style) in styles_of(keeps).iter().enumerate() {
                let x = u16::try_from(x).map_err(|e| e.to_string())?;
                let cell = parser.screen().cell(0, x).ok_or("a cell")?;
                assert_eq!(cell.underline_style(), *style, "cell {x}");
                assert_eq!(cell.underline_color(), Color::Rgb(255, 0, 0));
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
        let outcome = s.run(
            &["split".to_owned(), "-h".to_owned()],
            &crate::session::Ctx::client(c),
        );
        assert_eq!(outcome.status, 0, "{}", outcome.stderr);
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
        let mut client = fux_vt::Parser::with_options(6, 41, 0, options)?;
        client.process(&bytes)?;
        let navy = Color::Rgb(0, 0, 0x80);
        let right: u16 = 21;
        let cells = |p: &fux_vt::Parser, x: u16| {
            let c = p.screen().cell(0, x)?;
            Some((c.contents().to_owned(), c.fgcolor(), c.bgcolor()))
        };
        assert!(!client.screen().colors_changed(), "the client's colours");
        assert_eq!(
            cells(&client, 0),
            Some(("R".into(), Color::Rgb(0xff, 0, 0), navy))
        );
        assert_eq!(
            cells(&client, 1),
            Some(("D".into(), Color::Rgb(0x11, 0x22, 0x33), navy))
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

    /// A pane's hyperlinks reach the client: OSC 8 before the linked cells,
    /// with an id made of the pane and the link's key, and an OSC 8 that
    /// closes it after them. Two panes' links never share an id, though
    /// each pane's first link has the same key; a terminal reading the
    /// paint has each cell's link.
    #[test]
    fn hyperlinks_are_painted_with_their_panes_ids() -> Result<(), Box<dyn std::error::Error>> {
        let (mut s, c) = crate::session::testing::attached(6, 41)?;
        let outcome = s.run(
            &["split".to_owned(), "-h".to_owned()],
            &crate::session::Ctx::client(c),
        );
        assert_eq!(outcome.status, 0, "{}", outcome.stderr);
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
        let mut parser =
            fux_vt::Parser::with_options(6, 41, 0, fux_vt::Feature::Hyperlinks.into())?;
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
