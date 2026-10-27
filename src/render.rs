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
use fux_vt::clusters;
use fux_vt::sixel::Picture;
use fux_vt::{Attributes, CellRef, Cells, Color, CursorShape, Hyperlink, Rgb, Row, Screen};
use std::borrow::Cow;
use std::io::Write;
use std::sync::Arc;

#[derive(Debug)]
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
    /// shows: grids compare equal whatever composed them, and a copy has
    /// none.
    memo: Option<Memo>,
}

impl Clone for Grid {
    fn clone(&self) -> Grid {
        Grid {
            cells: self.cells.clone(),
            links: self.links.clone(),
            memo: None,
            ..*self
        }
    }
}

/// The modes a paint sets on the client's terminal from its focused pane,
/// each a field here and a line in `Modes::diff`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modes {
    /// The cursor's shape (DECSCUSR); `Initial` is the terminal's own.
    pub cursor_shape: CursorShape,
    /// An explicit colour of the focused pane's cursor; none restores the terminal's.
    pub cursor_colour: Option<Rgb>,
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
        cursor_colour(shown.cursor_colour, self.cursor_colour, out);
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

/// Changes an explicit cursor colour, or restores the terminal's own.
pub fn cursor_colour(was: Option<Rgb>, colour: Option<Rgb>, out: &mut Vec<u8>) {
    if was == colour {
        return;
    }
    if let Some(Rgb { r, g, b }) = colour {
        let _ = write!(out, "\x1b]12;#{r:02x}{g:02x}{b:02x}\x1b\\");
    } else {
        out.extend_from_slice(b"\x1b]112\x1b\\");
    }
}

/// A picture a client is shown: the pane it is in, the cells of the
/// client's screen it takes (top row, first column, rows, columns), and the
/// part of its pixels those show (left, top, width, height).
#[derive(Clone, Debug)]
pub struct Visible {
    pub pane: PaneId,
    pub area: (u16, u16, u16, u16),
    pub crop: (u32, u32, u32, u32),
    pub picture: Arc<Picture>,
}

/// The same picture in the same place, showing the same part. Pictures are
/// told apart by their `Arc`, which both sides hold, so no other picture
/// can take its address meanwhile.
impl PartialEq for Visible {
    fn eq(&self, other: &Visible) -> bool {
        Arc::ptr_eq(&self.picture, &other.picture)
            && self.crop == other.crop
            && self.area == other.area
    }
}

/// Whether pane `id`'s pictures show at all: only while its host draws
/// sixel (`Session::graphics`); kept, not shown, meanwhile.
pub fn shows_pictures(session: &Session, id: PaneId) -> bool {
    matches!(session.graphics(id), fux_vt::Graphics::Sixel(_))
}

/// The pictures client `client`'s panes, placed as `placement` has them,
/// show on its screen: none unless its terminal draws sixel and nothing
/// covers its panes (an overlay, copy mode); of a pane, only while its
/// pictures show (`shows_pictures`), those laid out in this terminal's
/// cell size. Each is clipped to its pane.
pub fn pictures(session: &Session, client: ClientId, placement: &Placement) -> Vec<Visible> {
    let Some(view) = session.views.get(&client) else {
        return Vec::new();
    };
    let (fux_vt::Graphics::Sixel(cell), true) = (view.terminal.graphics(), view.mode.is_normal())
    else {
        return Vec::new();
    };
    let cell = (cell.width(), cell.height());
    let mut visible = Vec::new();
    for &(id, rect) in &placement.panes {
        let Some(pane) = session.parts.panes().get(&id) else {
            continue;
        };
        if !shows_pictures(session, id) {
            continue;
        }
        for shown in pane.screen().pictures().filter(|s| s.cell == cell) {
            // The rows of the pane it is on, and the columns.
            let end = shown
                .row
                .saturating_add(i64::from(shown.rows))
                .min(i64::from(rect.h()));
            let start = shown.row.saturating_add(i64::from(shown.cropped)).max(0);
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
                picture: Arc::clone(shown.picture),
            });
        }
    }
    visible
}

/// The sixel data of the pictures a client was last sent, each with its
/// picture and crop: a picture that only moves is sent again with the same
/// data at its new place, encoded once (`fux-vt/THREATS.md` B08). At most
/// `SIXEL_BYTES` are kept; a picture past them is encoded each time it is
/// sent.
#[derive(Debug, Default)]
pub struct Sixels {
    kept: Vec<Kept>,
}

/// A picture's sixel data for its crop.
#[derive(Debug)]
struct Kept {
    picture: Arc<Picture>,
    crop: (u32, u32, u32, u32),
    data: Vec<u8>,
}

impl Kept {
    fn shows(&self, picture: &Visible) -> bool {
        Arc::ptr_eq(&self.picture, &picture.picture) && self.crop == picture.crop
    }
}

/// The most sixel data a client keeps to send again: 16 MiB, at
/// `fux-vt/THREATS.md` B08's worst of 12 bytes a pixel a 1,600 by 870
/// picture, and a screenful of most. One past it is encoded each time it is
/// sent, its work bounded by B08.
const SIXEL_BYTES: usize = 16 << 20;

impl Sixels {
    /// What shows `picture` on a terminal that draws sixel, its cursor
    /// kept: at its area's top left, its crop of pixels, from the data
    /// kept for its crop or encoded now.
    pub fn picture_into(&mut self, picture: &Visible, out: &mut Vec<u8>) {
        let (row, col, _, _) = picture.area;
        out.extend_from_slice(b"\x1b7");
        goto(out, row, col);
        if let Some(kept) = self.kept.iter().find(|kept| kept.shows(picture)) {
            out.extend_from_slice(&kept.data);
        } else {
            let mut data = Vec::new();
            fux_vt::sixel::encode(&picture.picture, picture.crop, &mut data);
            out.extend_from_slice(&data);
            let held: usize = self.kept.iter().map(|k| k.data.len()).sum();
            if held.saturating_add(data.len()) <= SIXEL_BYTES {
                let (picture, crop) = (Arc::clone(&picture.picture), picture.crop);
                self.kept.push(Kept {
                    picture,
                    crop,
                    data,
                });
            }
        }
        out.extend_from_slice(b"\x1b8");
    }

    /// Lets go of the data of every picture not in `shown`, the pictures
    /// the client now shows.
    pub fn keep(&mut self, shown: &[Visible]) {
        self.kept.retain(|kept| shown.iter().any(|p| kept.shows(p)));
    }
}

/// Grids are equal when they show the same.
impl PartialEq for Grid {
    fn eq(&self, other: &Grid) -> bool {
        // All the grid shows but its cells and links.
        let head = |g: &Grid| ((g.rows, g.cols, g.cursor), (g.modes, g.underline_styles));
        head(self) == head(other)
            && self.cells == other.cells
            && (0..self.rows).all(|y| self.links.row_eq(&other.links, y))
    }
}
impl Eq for Grid {}

/// What composed a grid in normal mode: its frame, and the identity and
/// version of each pane row put in it. A fux-vt row keeps its identity
/// while retained, and its version changes with every edit of its cells,
/// wrap or links; pane ids are never reused, and a respawned pane makes
/// another frame. So a grid composed again for its memo's frame needs only
/// the rows whose keys changed put again (`Memo::update`), and two grids of
/// one frame show the same on a row whose keys match (`SameFrame`).
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
/// size, each pane's place, program (`Pane::starts`) and window size, the
/// separators, and the focused pane, which colours them. None for a client
/// shown no tab, or with a pane in colours of its own (`recolour`), whose
/// rows depend on more than their keys.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Frame {
    rows: u16,
    cols: u16,
    panes: Vec<(PaneId, u64, Rect, (u16, u16))>,
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
            let pane = session.parts.panes().get(id)?;
            let screen = pane.screen();
            if own_colours(screen) {
                return None;
            }
            panes.push((*id, pane.respawns, *rect, screen.size().into()));
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
        for ((id, _, rect, _), keys) in self.frame.panes.iter().zip(&mut self.keys) {
            let Some(pane) = session.parts.panes().get(id) else {
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
                .all(|((_, _, rect, _), (a, b))| {
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
    // twice as wide (DECDWL, DECDHL) is drawn over it, covering the width
    // itself.
    let mut len = row.map_or(0, |row| row.len());
    if let Some(row) = row {
        grid.put_row(gy, gx, pane, row, width);
        if row.rendition().is_double() {
            grid.put_doubled_row(gy, gx, pane, &row, width);
            len = usize::from(width);
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

/// Each row's hyperlinks (OSC 8), a cell each, with the pane whose each
/// is: none for a row with none, as most, so that two such rows compare by
/// their cells alone; rows past the end have none.
#[derive(Clone, Debug, Default)]
struct Links(Vec<Option<Vec<Link>>>);

/// A cell's hyperlink, and the pane whose it is.
type Link = Option<(PaneId, Hyperlink)>;

impl Links {
    fn row(&self, y: u16) -> Option<&[Link]> {
        self.0.get(usize::from(y))?.as_deref()
    }
    /// Gives cell `x` of row `y`, `cols` wide, `link`.
    fn set(&mut self, y: u16, x: u16, link: Option<(PaneId, &Hyperlink)>, cols: u16) {
        let y = usize::from(y);
        if link.is_none() && self.0.get(y).is_none_or(Option::is_none) {
            return;
        }
        if self.0.len() <= y {
            self.0.resize(y.saturating_add(1), None);
        }
        let row = self
            .0
            .get_mut(y)
            .map(|r| r.get_or_insert_with(|| vec![None; usize::from(cols)]));
        if let Some(at) = row.and_then(|r| r.get_mut(usize::from(x))) {
            *at = link.map(|(pane, link)| (pane, link.clone()));
        }
    }
    /// Cells `xs` of row `y` have no link.
    fn unlink(&mut self, y: u16, xs: std::ops::Range<u16>) {
        if let Some(Some(row)) = self.0.get_mut(usize::from(y)) {
            let run = usize::from(xs.end.saturating_sub(xs.start));
            row.iter_mut()
                .skip(usize::from(xs.start))
                .take(run)
                .for_each(|l| *l = None);
        }
    }
    /// Whether row `y` has the same links as `other`'s.
    fn row_eq(&self, other: &Links, y: u16) -> bool {
        match (self.row(y), other.row(y)) {
            (Some(a), Some(b)) => a == b,
            (Some(row), None) | (None, Some(row)) => row.iter().all(Option::is_none),
            (None, None) => true,
        }
    }
}

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
        let (pane, link) = self.links.row(y)?.get(usize::from(x))?.as_ref()?;
        Some((*pane, link))
    }
    /// Makes the grid `rows` by `cols`, resizing its cells only if the size
    /// changed, so that a grid composed into again allocates nothing; and
    /// blanks it if `blank`, as a resized grid is anyway.
    fn reset(&mut self, rows: u16, cols: u16, blank: bool) {
        self.links.0.clear();
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
    /// Whether row `y` shows what `other`'s, a grid of its size, does: its
    /// cells, compared by `Cells::range_eq` without reading each one's
    /// text, and its links.
    fn row_eq(&self, other: &Grid, y: u16) -> bool {
        self.cells.range_eq(&other.cells, self.span(y)) && self.links.row_eq(&other.links, y)
    }
    /// Whether the cell at (`y`, `x`) shows what `other`'s, a grid of its
    /// size, does.
    fn cell_eq(&self, other: &Grid, y: u16, x: u16) -> bool {
        let i = cell_count(y, self.cols).saturating_add(usize::from(x));
        self.cells.range_eq(&other.cells, i..i.saturating_add(1))
            && self.link(y, x) == other.link(y, x)
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
        // At most the grid's width, a u16.
        let room = u16::try_from(room).unwrap_or(u16::MAX);
        if !row.has_links() {
            self.links.unlink(y, x..x.saturating_add(room));
            return;
        }
        for (gx, col) in (x..).zip(0..room) {
            let link = row.link(usize::from(col)).map(|link| (pane, link));
            self.links.set(y, gx, link, self.cols);
        }
    }
    /// `put_row` for a row drawn twice as wide (`fux_vt::LineRendition`),
    /// as no terminal can draw half a line so: each cell takes two
    /// columns, its glyph and a blank in its colours, a wide glyph four;
    /// what the doubled cells do not reach of `width` is blank. A double
    /// height half is drawn as double width.
    fn put_doubled_row(&mut self, y: u16, x: u16, pane: PaneId, row: &Row<'_>, width: u16) {
        let Some(start) = self.index(y, x) else {
            return;
        };
        let room = self.cols.saturating_sub(x).min(width);
        for at in 0..room {
            let (col, second) = (usize::from(at / 2), at % 2 == 1);
            let i = start.saturating_add(usize::from(at));
            let gx = x.saturating_add(at);
            let Some(cell) = row.cell(col) else {
                self.put(y, gx, CellRef::default());
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
            self.links.set(y, gx, link, self.cols);
        }
    }
    /// Draws the first `width` cells of row `y` from column `x`, a pane's
    /// row as `draw_row` put it, blanks too, in the colours its program set
    /// (`pane_colours`): the client's own palette is never changed.
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
            self.links.unlink(y, x..x.saturating_add(1));
        }
    }
    /// Blanks `area`, clipped at the grid's edge.
    fn blank(&mut self, area: Rect) {
        let count = usize::from(area.right().min(self.cols).saturating_sub(area.x()));
        for y in area.rows() {
            let Some(start) = self.index(y, area.x()) else {
                return;
            };
            self.cells
                .fill(start..start.saturating_add(count), CellRef::default());
            self.links.unlink(y, area.x()..area.right());
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
            && let Some(left) = x.checked_sub(1)
            && self.get(y, left).is_some_and(|c| c.is_wide())
        {
            self.put(y, left, CellRef::default());
        }
        if was_wide
            && !cell.is_wide()
            && let Some(right) = x.checked_add(1)
            && self.get(y, right).is_some_and(|c| c.is_wide_continuation())
        {
            self.put(y, right, CellRef::default());
        }
        self.put(y, x, cell);
    }
    /// The text of a row, trailing blanks trimmed: for `capture-client`.
    pub fn row_text(&self, y: u16) -> String {
        let row = self.row(y).filter(|c| !c.is_wide_continuation());
        row.map(|c| c.shown())
            .collect::<String>()
            .trim_end()
            .to_owned()
    }

    /// Writes `text` from (y, x), a cluster a cell (`clusters`), clipped at
    /// `limit`, wide glyphs whole; the column after it is returned.
    fn text(&mut self, y: u16, x: u16, text: &str, style: Attributes, limit: u16) -> u16 {
        self.text_cells(y, x, clusters(text), style, limit)
    }

    /// Draws measured cells without segmenting their retained text again.
    fn text_cells<'a>(
        &mut self,
        y: u16,
        mut x: u16,
        cells: impl Iterator<Item = (&'a str, u16)>,
        style: Attributes,
        limit: u16,
    ) -> u16 {
        for (cluster, width) in cells {
            let Some(end) = x
                .checked_add(width)
                .filter(|end| *end <= limit.min(self.cols))
            else {
                break;
            };
            self.set(y, x, CellRef::new(cluster, width == 2, style));
            if width == 2
                && let Some(second) = x.checked_add(1)
            {
                self.set(y, second, CellRef::wide_continuation());
            }
            x = end;
        }
        x
    }

    /// Shows `area` as `shown`, a grid of the same size, shows it
    /// (`compose_held`).
    fn keep_area(&mut self, shown: &Grid, area: Rect) {
        for y in area.rows() {
            for x in area.cols() {
                if let Some(i) = self.index(y, x)
                    && let Some(cell) = shown.cells.get(i)
                {
                    self.cells.set(i, cell);
                    self.links.set(y, x, shown.link(y, x), self.cols);
                }
            }
        }
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

/// `attributes` in the colours a pane's program set: an indexed colour
/// whose entry it changed (OSC 4), and the default foreground and
/// background (OSC 10, 11), as it set them; the rest as they are, for the
/// client's terminal to draw in its own. With DECSCNM (reverse video) set,
/// every cell's inverse is turned over.
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

/// Display width of a string, as a pane prints it (`fux_vt::str_width`).
pub fn width(text: &str) -> u16 {
    u16::try_from(fux_vt::str_width(text))
        .unwrap_or(u16::MAX)
        .min(crate::view::MOST)
}

/// Cuts `text` to `cols` cells, preserving their measured boundaries.
pub fn fit(text: &str, cols: u16) -> impl Iterator<Item = (&str, u16)> + Clone {
    fit_cells(clusters(text), cols, width(text) > cols)
}

fn cell_width<'a>(cells: impl Iterator<Item = (&'a str, u16)>) -> u16 {
    cells.fold(0u16, |sum, (_, width)| sum.saturating_add(width))
}

fn fit_cells<'a>(
    cells: impl Iterator<Item = (&'a str, u16)> + Clone,
    cols: u16,
    cut: bool,
) -> impl Iterator<Item = (&'a str, u16)> + Clone {
    let room = cols.saturating_sub(u16::from(cut));
    cells
        .scan(room, |room, (text, width)| {
            *room = room.checked_sub(width)?;
            Some((text, width))
        })
        .chain((cut && cols > 0).then_some(("…", 1)))
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
    compose_held(session, view, grid, placement, None);
}

/// What a client was last painted: the grid, and where each pane was on
/// it. A pane part way through a frame (`Pane::mid_frame`) shows as it was
/// there.
#[derive(Clone, Copy)]
pub struct Painted<'a> {
    pub grid: &'a Grid,
    pub panes: &'a [(PaneId, Rect)],
}

impl Painted<'_> {
    /// Whether pane `id` was painted at `rect` on a grid of this size.
    pub fn kept(&self, id: PaneId, rect: Rect, rows: u16, cols: u16) -> bool {
        (self.grid.rows, self.grid.cols) == (rows, cols) && self.panes.contains(&(id, rect))
    }
}

/// [`compose_view`], each pane part way through a frame shown as `painted`
/// showed it where it is still in the same place, else blank, so a client
/// is never painted half a frame; the rest of the screen, fux's bar and
/// panels over it too, as it is, so one pane holds up no other
/// (fux-vt/THREATS.md 4.4). Its cursor, if focused, is the one painted
/// there, if any.
pub fn compose_held(
    session: &Session,
    view: &View,
    grid: &mut Grid,
    placement: &mut Placement,
    painted: Option<Painted<'_>>,
) {
    session.placement_into(view, placement);
    let placement = &*placement;
    let tab = session.shown_tab(view.id);
    let focus = tab.and_then(|t| t.focus(view.id));
    // Copy mode and its positions, their rows found once for the paint.
    let copy = if let Mode::Copy(copy) = &view.mode {
        let at = (session.parts.panes().get(&copy.pane)).and_then(|p| copy.resolve(p.screen()));
        at.map(|at| (copy.as_ref(), at))
    } else {
        None
    };
    put_panes(grid, session, view, placement, focus, copy);
    let held =
        |id: &PaneId| (session.parts.panes().get(id)).is_some_and(crate::pane::Pane::mid_frame);
    let kept = |id: PaneId, rect: Rect| painted.filter(|p| p.kept(id, rect, view.rows, view.cols));
    for &(id, rect) in placement.panes.iter().filter(|(id, _)| held(id)) {
        match kept(id, rect) {
            Some(painted) => grid.keep_area(painted.grid, rect),
            None => grid.blank(rect),
        }
        grid.memo = None;
    }
    // The cursor: copy mode's in its pane, else the focused pane's, but only
    // in normal mode: none under an overlay, nor in a repeat mode, whose
    // keys are fux's.
    if let Some(focus) = focus
        && let Some(rect) = placement.rect(focus)
        && let Some(pane) = session.parts.panes().get(&focus)
    {
        let screen = pane.screen();
        match copy.filter(|(c, _)| c.pane == focus) {
            Some((_, at)) => {
                // Within the rows shown, which a smaller client can make
                // fewer than the rect; on a row drawn twice as wide, at its
                // glyph's first half, as the normal cursor.
                if let Some((y, x)) = at.cursor_in_view(rect.h().min(screen.size().rows()))
                    && let doubled =
                        (at.top.window().row(y)).is_some_and(|r| r.rendition().is_double())
                    && let Some(at) = rect.at(y, if doubled { x.saturating_mul(2) } else { x })
                {
                    grid.cursor = Some(at);
                    // The copy cursor is a block.
                    grid.modes.cursor_shape = CursorShape::SteadyBlock;
                }
            }
            None => {
                let (y, x) = screen.cursor_position();
                // On a row drawn twice as wide, at its glyph's first half.
                let doubled = screen.line_rendition(y).is_double();
                let x = if doubled { x.saturating_mul(2) } else { x };
                if screen.mode(fux_vt::Mode::ShowCursor)
                    && view.mode.is_normal()
                    && let Some(at) = rect.at(y, x)
                {
                    grid.cursor = Some(at);
                    grid.modes.cursor_shape = screen.cursor_shape();
                    grid.modes.cursor_colour = screen.dynamic_color(12);
                }
            }
        }
        // Only where a pane's own cursor shows: in normal mode (none under
        // an overlay, copy mode's its own).
        if held(&focus) && view.mode.is_normal() {
            let painted = kept(focus, rect);
            let at = (painted.and_then(|p| p.grid.cursor)).filter(|&(y, x)| rect.contains(y, x));
            grid.cursor = at;
            let modes = painted
                .filter(|_| at.is_some())
                .map(|p| p.grid.modes)
                .unwrap_or_default();
            grid.modes.cursor_shape = modes.cursor_shape;
            grid.modes.cursor_colour = modes.cursor_colour;
        }
    }
    // The mouse: reported by the client's terminal only while the focused
    // pane's program asked for it, still takes input, and nothing of fux's
    // has the keys, so that the rest of the time the terminal selects text
    // as it does. A client that only watches sends no clicks: its terminal
    // selects.
    if view.mode.is_normal()
        && !view.read_only
        && let Some(focus) = focus
        && placement.rect(focus).is_some()
        && let Some(pane) = session.parts.panes().get(&focus)
        && crate::session::refuses_input(pane).is_none()
    {
        grid.modes.mouse = mouse_level(pane.screen().mouse_protocol_mode());
    }
    bar(grid, session, view, placement, copy);
    match &view.mode {
        Mode::Panel(Panel::Column(c)) => column(grid, view, c),
        Mode::Panel(Panel::List(list)) => list_panel(grid, view, list),
        Mode::Panel(Panel::Prompt {
            title,
            before,
            after,
            hints,
        }) => {
            // The panel's border takes a cell each side.
            let room = view.cols.saturating_sub(2);
            let lines: [Line<'_>; 3] = [
                (title.as_str().into(), PANEL.with_bold(true)),
                (prompt_line(before, after, room), PANEL),
                (hinted(hints).into(), PANEL.with_dim(true)),
            ];
            surface(grid, view, &lines);
        }
        Mode::Panel(Panel::Confirm { question, hints }) => {
            let lines: [Line<'_>; 2] = [
                (question.as_str().into(), PANEL.with_bold(true)),
                (hinted(hints).into(), PANEL),
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
    let tiled = covered == view.pane_area().area();
    grid.reset(view.rows, view.cols, !tiled);
    let mut keys = Vec::new();
    for (id, rect) in &placement.panes {
        let Some(pane) = session.parts.panes().get(id) else {
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
            // A row drawn twice as wide shows each cell in two columns.
            let doubled = row.rendition().is_double();
            for x in 0..width {
                // A wide glyph is selected if either half is, as `y`
                // copies it whole.
                let col = if doubled { x / 2 } else { x };
                if let Some(i) = line.at(0, x).and_then(|(gy, gx)| grid.index(gy, gx))
                    && let Some(cell) = grid.cells.get(i)
                    && !cell.is_wide_continuation()
                    && (at.selected(&row, col)
                        || cell.is_wide()
                            && col.checked_add(1).is_some_and(|col| at.selected(&row, col)))
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
fn prompt_line<'a>(before: &'a str, after: &'a str, room: u16) -> Text<'a> {
    let mut chars: Vec<(&str, u16)> = clusters(before).collect();
    let bar = chars.len();
    // The inserted cursor owns a cell, including after a Prepend character.
    chars.push(("▏", 1));
    chars.extend(clusters(after));
    if chars.iter().fold(0u16, |n, (_, w)| n.saturating_add(*w)) <= room {
        return Text::Measured(chars);
    }
    // An ellipsis each side, at most.
    let inner = room.saturating_sub(2);
    let ahead = inner / 4;
    let (mut start, mut end) = (bar, bar.saturating_add(1));
    let mut used: u16 = 1;
    let fits = |used: u16, c: Option<&(&str, u16)>| {
        c.and_then(|(_, width)| used.checked_add(*width))
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
    let tail = end < chars.len();
    let mut out = Vec::new();
    if start > 0 {
        out.push(("…", 1));
    }
    out.extend(
        chars
            .into_iter()
            .skip(start)
            .take(end.saturating_sub(start)),
    );
    if tail {
        out.push(("…", 1));
    }
    Text::Measured(out)
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
        let near = focused.is_some_and(|r| r.near(y, x));
        let style = Attributes::new(Color::Idx(if near { 2 } else { 240 }), Color::Default);
        // A separator's cell is no pane's, so no wide glyph has a half there
        // to repair: it is only ever drawn over.
        grid.put(y, x, CellRef::new(glyph, false, style));
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
    placement: &Placement,
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
    let pane = |c: &crate::copy::Copy| session.parts.panes().get(&c.pane);
    let copy_bar = copy.and_then(|(c, at)| pane(c).map(|p| c.bar(p.screen(), &at)));
    // Panes this screen cuts off or hides, named before the title, and
    // given room past the title's share of the bar.
    let mut unseen_width = 0;
    let right: Option<(Cow<'_, str>, Attributes)> = if let Some(notice) = &view.notice {
        let colour = Color::Idx(if notice.error { 9 } else { 11 });
        Some((
            notice.text.as_str().into(),
            Attributes::new(colour, GRAY_BG),
        ))
    } else if let Some(copy) = &copy_bar {
        Some((copy.position.as_str().into(), base))
    } else if let Mode::Panel(Panel::Column(column)) = &view.mode {
        Some((format!("{} …", column.keys).into(), HIGHLIGHT))
    } else if let Mode::Panel(Panel::Repeat(bar)) = &view.mode {
        Some((bar.as_str().into(), HIGHLIGHT.with_bold(true)))
    } else {
        (session.focused(view.id))
            .and_then(|f| session.parts.panes().get(&f))
            .map(|p| {
                let mut text = unseen(session, view, placement);
                unseen_width = width(&text);
                text.push_str(&format!("{} {}", p.id(), p.label()));
                if view.zoom {
                    text.push_str(" [zoom]");
                }
                (text.into(), base)
            })
    };
    // At most three quarters of the bar, or what names the unseen panes if
    // wider, and a gap before it. Exact: the quarters of a u16 add up to
    // less than one.
    let right_width = right.as_ref().map_or(0, |(t, _)| {
        let share = (view.cols / 2).saturating_add(view.cols / 4);
        width(t).min(share.max(unseen_width)).min(view.cols)
    });
    let left_limit = view
        .cols
        .saturating_sub(right_width.saturating_add(u16::from(right_width > 0)));
    let mut x = 0;
    if let Some(copy) = &copy_bar {
        // Copy mode's keys replace the tabs.
        let badge = HIGHLIGHT.with_bold(true);
        match &copy.badge {
            crate::copy::Badge::Label(label) => {
                x = grid.text_cells(
                    y,
                    x,
                    fit(&format!(" {label} "), left_limit),
                    badge,
                    left_limit,
                );
            }
            crate::copy::Badge::Typing(prompt, before, after) => {
                let room = left_limit.saturating_sub(width(prompt).saturating_add(2));
                x = grid.text(y, x, &format!(" {prompt}"), badge, left_limit);
                x = grid.text_cells(
                    y,
                    x,
                    prompt_line(before, after, room).cells(),
                    badge,
                    left_limit,
                );
                x = grid.text(y, x, " ", badge, left_limit);
            }
        }
        for crate::act::Hint { keys: key, label } in copy.hints {
            let hint = format!("  {key} {label}");
            if x.saturating_add(width(&hint)) > left_limit {
                break;
            }
            x = grid.text(y, x, "  ", base, left_limit);
            x = grid.text(y, x, key, base.with_bold(true), left_limit);
            x = grid.text(y, x, &format!(" {label}"), base, left_limit);
        }
    } else if let Some(ws) = session.shown_workspace(view.id) {
        let name = format!(" {} ", ws.name());
        let bold = base.with_bold(true);
        x = grid.text_cells(y, x, fit(&name, left_limit), bold, left_limit);
        let current = ws.tab_of(view.id).map(|t| t.id());
        for tab in ws.tabs() {
            let Some(room) = left_limit.checked_sub(x).filter(|r| *r > 0) else {
                break;
            };
            // A bell rang in a tab the client does not show (`outer`).
            let rang = tab.seat(view.id).is_some_and(|s| s.rang) && Some(tab.id()) != current;
            let label = format!(" {}{} ", tab.name(), if rang { "!" } else { "" });
            let attrs = if Some(tab.id()) == current {
                Attributes::new(Color::Idx(0), Color::Idx(2)).with_bold(true)
            } else {
                base
            };
            x = grid.text_cells(y, x, fit(&label, room), attrs, left_limit);
        }
    }
    if let Some((text, attrs)) = right {
        let text = fit(&text, right_width);
        // Right-aligned, a cell from the edge, or from the left if wider.
        let start = view
            .cols
            .saturating_sub(cell_width(text.clone()).saturating_add(1));
        grid.text_cells(y, start, text, attrs, view.cols);
    }
}

/// What of the tab `view` shows its screen does not: `[hidden %3]` for each
/// pane it has no room for (when zoomed, the focus alone is shown), first,
/// as nothing of it shows; then `[cut %2]` for each placed in less room
/// than its size; each followed by a space; empty if it shows them all
/// whole.
fn unseen(session: &Session, view: &View, placement: &Placement) -> String {
    let Some(tab) = session.shown_tab(view.id) else {
        return String::new();
    };
    let panes = match tab.focus(view.id).filter(|_| view.zoom) {
        Some(focus) => vec![focus],
        None => tab.root().panes(),
    };
    let (mut cut, mut hidden) = (String::new(), String::new());
    for id in panes {
        let size = session.parts.panes().get(&id).map(|p| p.size());
        match placement.rect(id) {
            None => hidden.push_str(&format!(" {id}")),
            Some(r) if size.is_some_and(|s| r.h() < s.rows() || r.w() < s.cols()) => {
                cut.push_str(&format!(" {id}"));
            }
            Some(_) => {}
        }
    }
    [("hidden", hidden), ("cut", cut)]
        .into_iter()
        .filter(|(_, ids)| !ids.is_empty())
        .map(|(word, ids)| format!("[{word}{ids}] "))
        .collect()
}

/// A line of a panel: its text, borrowed where it can be, and its style.
type Line<'a> = (Text<'a>, Attributes);

/// Plain text or cells already measured, as in a scrolled prompt.
enum Text<'a> {
    Plain(Cow<'a, str>),
    Measured(Vec<(&'a str, u16)>),
}

impl Text<'_> {
    fn width(&self) -> u16 {
        match self {
            Self::Plain(text) => width(text),
            Self::Measured(_) => cell_width(self.cells()),
        }
    }

    fn cells(&self) -> impl Iterator<Item = (&str, u16)> + Clone {
        let (plain, measured) = match self {
            Self::Plain(text) => (Some(text.as_ref()), None),
            Self::Measured(cells) => (None, Some(cells)),
        };
        plain
            .into_iter()
            .flat_map(clusters)
            .chain(measured.into_iter().flatten().copied())
    }
}

impl<'a> From<&'a str> for Text<'a> {
    fn from(text: &'a str) -> Self {
        Self::Plain(text.into())
    }
}

impl From<String> for Text<'_> {
    fn from(text: String) -> Self {
        Self::Plain(text.into())
    }
}

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
    let area = view.pane_area();
    if area.is_empty() || lines.is_empty() {
        return;
    }
    // More lines than rows is the same as exactly as many.
    let height = u16::try_from(lines.len()).unwrap_or(u16::MAX);
    let inner = lines
        .iter()
        .map(|(t, _)| cell_width(t.cells()))
        .max()
        .unwrap_or(0);
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
        let (x, end) = (line.x().saturating_add(1), line.right().saturating_sub(1));
        let cols = line.w().saturating_sub(2);
        let text = fit_cells(text.cells(), cols, text.width() > cols);
        grid.text_cells(line.y(), x, text, *attrs, end);
    }
}

/// A panel's hints, as its last line says them: `Enter choose · q Escape cancel`.
fn hinted(hints: &[crate::act::Hint]) -> String {
    let hints: Vec<String> = (hints.iter())
        .map(|h| format!("{} {}", h.keys, h.label))
        .collect();
    hints.join(" · ")
}

/// A chooser: the entries that fit, the selected one highlighted.
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
    lines.push((hinted(&list.hints).into(), PANEL.with_dim(true)));
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
/// Its `id` is the pane's and the link's key there, so that no two panes'
/// links are one, and a link painted in pieces is one, as the spec asks of
/// a multiplexer (`references/modern/osc8_hyperlinks.md`).
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

/// Paints `text`, a glyph `width` columns wide at (`y`, `x`) in a run,
/// where it is not one byte of ASCII or follows a glyph that `joined` the
/// one before it; whether the next glyph is to be placed by a cursor move.
///
/// A glyph that continues the cluster of the one before it (an emoji
/// modifier micro or vim put after an emoji with a cursor move) is placed
/// by a cursor move, and so is what follows it, wherever the terminal puts
/// it. So is what follows a cluster fux-vt measures other than the sum of
/// its characters, as a terminal measuring each alone would advance by it.
///
/// Zero-width characters after a glyph that leaves the cursor in the last
/// column are painted with autowrap on: with it off, Ghostty puts them on
/// the last column's cell, not on the glyph.
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
        M::Press | M::PressRelease | M::Highlight => 1000,
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
/// `out`; false, appending nothing, if the client shows `new` already.
/// Each row is compared once.
pub fn paint_into(shown: &Grid, known: bool, new: &Grid, out: &mut Vec<u8>) -> bool {
    let full = !known
        || (shown.rows, shown.cols, shown.underline_styles)
            != (new.rows, new.cols, new.underline_styles);
    let rows = differing_rows((!full).then_some(shown), new);
    if !full && rows.is_empty() && (shown.cursor, shown.modes) == (new.cursor, new.modes) {
        return false;
    }
    if !full && echo(shown, new, &rows, out) {
        return true;
    }
    paint_whole(shown, known, (!full).then_some(shown), new, &rows, out);
    true
}

/// The rows of `new` that differ from `old`, a grid of its size (every
/// row, if there is none): those whose memo does not show them the same
/// and whose cells or links differ.
fn differing_rows(old: Option<&Grid>, new: &Grid) -> Vec<u16> {
    let memo = old.and_then(|o| SameFrame::of(o, new));
    (0..new.rows)
        .filter(|&y| {
            // An unchanged row costs this one comparison.
            !memo.as_ref().is_some_and(|m| m.same_row(y)) && old.is_none_or(|o| !o.row_eq(new, y))
        })
        .collect()
}

/// A keystroke's echo, painted as a terminal shows it typed: when all that
/// changed is a run of glyphs of one column on the cursor's row, ending
/// where the cursor now is, short of the last column, the cursor is moved
/// to the run (by nothing, a carriage return or a column if it is on the
/// row, else by row and column) and the glyphs alone are written, in their
/// attributes, from the default every paint leaves. Nothing else changes
/// for the terminal: not the cursor's shape or visibility, the mouse, a
/// link, a wide glyph. Whether it was so; if not, nothing is written.
fn echo(old: &Grid, new: &Grid, rows: &[u16], out: &mut Vec<u8>) -> bool {
    let (Some((oy, ox)), Some((y, to))) = (old.cursor, new.cursor) else {
        return false;
    };
    if old.modes != new.modes
        || to == 0
        || to >= new.cols
        || rows != [y]
        || old.links.row(y).is_some()
        || new.links.row(y).is_some()
    {
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
    // (two make a flag), short of the emoji's planes, and none that joins
    // what is before it (an Extend or SpacingMark character alone, ZWJ),
    // which the terminal would join to the cell on its left.
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
                        && !fux_vt::continues_cluster("a", ch)
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
/// [`paint_into`] without the echo's short way: `rows` painted, compared
/// cell by cell with `before`, what the client shows if it is not painted
/// whole.
fn paint_whole(
    shown: &Grid,
    known: bool,
    before: Option<&Grid>,
    new: &Grid,
    rows: &[u16],
    out: &mut Vec<u8>,
) {
    out.extend_from_slice(b"\x1b[?2026h\x1b[?25l");
    if before.is_none() {
        out.extend_from_slice(b"\x1b[0m\x1b[H\x1b[2J");
    }
    let mut current: Option<Attributes> = None;
    // The hyperlink the client's terminal has open.
    let mut open = None;
    for &y in rows {
        let cell = |x: u16| new.get(y, x);
        let changed = |x: u16| before.is_none_or(|o| !o.cell_eq(new, y, x));
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
                let link = new.link(y, cx);
                if link != open {
                    hyperlink(out, link);
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
    /// A picture that moves, its crop the same, is encoded once and sent at
    /// each place with the same data; a new crop is encoded again; another
    /// picture in the same place and crop (a respawned pane's) is sent its
    /// own data; and a picture no longer shown is let go of
    /// (`fux-vt/THREATS.md` B08).
    #[test]
    fn a_moving_picture_is_encoded_once() -> Result<(), String> {
        let solid = |rgba: [u8; 4]| {
            let pixels = std::iter::repeat_n(rgba, 20 * 12).flatten().collect();
            Picture::new(20, 12, pixels).map(Arc::new).ok_or("20 by 12")
        };
        let (red, blue) = (solid([255, 0, 0, 255])?, solid([0, 0, 255, 255])?);
        let at = |picture: &Arc<Picture>, row: u16, crop| Visible {
            pane: PaneId::of(1),
            area: (row, 0, 1, 2),
            crop,
            picture: Arc::clone(picture),
        };
        let whole = (0, 0, 20, 12);
        let sent = |picture: &Arc<Picture>, row: u16| {
            let mut alone = Vec::new();
            fux_vt::sixel::encode(picture, whole, &mut alone);
            [
                format!("\x1b7\x1b[{};1H", row + 1).as_bytes(),
                &alone,
                b"\x1b8",
            ]
            .concat()
        };
        let mut sixels = Sixels::default();
        for row in 0..100 {
            let mut out = Vec::new();
            sixels.picture_into(&at(&red, row, whole), &mut out);
            sixels.keep(&[at(&red, row, whole)]);
            assert_eq!(out, sent(&red, row));
            assert_eq!(sixels.kept.len(), 1, "encoded again as it moved");
        }
        sixels.picture_into(&at(&red, 0, (0, 6, 20, 6)), &mut Vec::new());
        assert_eq!(sixels.kept.len(), 2, "a new crop is encoded");
        assert!(at(&red, 0, whole) != at(&blue, 0, whole));
        let mut out = Vec::new();
        sixels.picture_into(&at(&blue, 0, whole), &mut out);
        assert_eq!(out, sent(&blue, 0));
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
                let panes: Vec<PaneId> = s.parts.panes().keys().copied().collect();
                if let Some(&id) = panes.get(r.below(panes.len().max(1))) {
                    s.output(id, out);
                }
                s.hold_copies();
                if !compose_into(&s, c, &mut spare, &mut placement) {
                    continue;
                }
                let bytes = paint(painted.then_some(&showing), &spare);
                // Copies, which have no memo.
                let (plain_old, plain_new) = (showing.clone(), spare.clone());
                let before = painted.then_some(&plain_old);
                let rows = differing_rows(before, &plain_new);
                let mut slow = Vec::new();
                paint_whole(&plain_old, painted, before, &plain_new, &rows, &mut slow);
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

    /// A lone mark that joins what is before it (a spacing mark after a
    /// cursor move, U+0CCB, U+09BE) is no echo:
    /// written raw, the terminal joins it to the cell on its left, which
    /// draws that glyph wrong. A letter there is one.
    #[test]
    fn a_mark_that_joins_its_neighbour_is_not_echoed() -> Result<(), String> {
        for (typed, echoed) in [("\u{ccb}", false), ("\u{9be}", false), ("x", true)] {
            let (mut s, c) = crate::session::testing::attached(10, 40)?;
            let id = *s.parts.panes().keys().next().ok_or("no pane")?;
            let mut placement = Placement::default();
            let (mut before, mut after) = (Grid::new(0, 0), Grid::new(0, 0));
            s.output(id, b"$ ab");
            compose_into(&s, c, &mut before, &mut placement);
            s.output(id, format!("\x1b[C{typed}").as_bytes());
            compose_into(&s, c, &mut after, &mut placement);
            let alone =
                (0..after.cols).any(|x| after.get(0, x).is_some_and(|c| c.contents() == typed));
            assert!(alone, "{typed:?} is no cell of its own");
            let rows = differing_rows(Some(&before), &after);
            let echoes = echo(&before, &after, &rows, &mut Vec::new());
            assert_eq!(echoes, echoed, "{typed:?}");
        }
        Ok(())
    }

    /// Composing into the grid composed last but one, as the server does,
    /// puts again only the rows that changed (`Memo`), and paints only the
    /// rows that may differ: through random output into split panes,
    /// scrolling, the alternate screen, links, resizes, copy mode, a
    /// notice and splits, after every step the grid is what composing
    /// whole gives, its paint is the paint comparing every row, and it
    /// paints nothing just when the grids are equal.
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
                        s.resize(c, rows, cols).map_err(|e| e.to_string())?;
                    }
                    3 => {
                        crate::session::testing::run(&mut s, "str @1 tab str n rename")?;
                    }
                    _ => {
                        let panes: Vec<PaneId> = s.parts.panes().keys().copied().collect();
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
                // Copies, which have no memo.
                let (plain_old, plain_new) = (shown.clone(), spare.clone());
                let slow = paint(painted.then_some(&plain_old), &plain_new);
                assert!(
                    fast == slow,
                    "case {case}, step {step}: the memo's paint differs"
                );
                assert_eq!(
                    fast.is_empty(),
                    painted && spare == shown,
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

    /// Copy mode's bar replaces the tabs with what it is doing and its keys'
    /// hints; a narrow bar drops the last.
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
            copying.starts_with(
                " COPY   v s x select  y copy  o other end  q Escape quit  f r search"
            ),
            "{copying}"
        );
        assert!(!copying.contains("main"));
        // Where the cursor is, on the right: its line of all the rows.
        assert!(copying.ends_with("1/9"), "{copying}");
        s.input(c, b"v");
        let selecting = bar(&s);
        assert!(
            selecting.starts_with(" COPY select   v s x select  y copy"),
            "{selecting}"
        );
        // Letters in either case: S selects lines; C-v is no key's.
        s.input(c, b"S\x16");
        let lines = bar(&s);
        assert!(
            lines.starts_with(" COPY lines   v s x select  y copy"),
            "{lines}"
        );
        s.input(c, b"fab");
        assert!(
            bar(&s).starts_with(" /ab▏   Enter accept  Escape cancel"),
            "{}",
            bar(&s)
        );
        s.input(c, b"\x1b");
        s.escape(c);
        s.resize(c, 10, 36)?;
        let narrow = bar(&s);
        assert!(narrow.contains("v s x select"), "{narrow}");
        assert!(!narrow.contains("quit"), "{narrow}");
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

    /// fux's own text (the bar, titles, panels) takes the columns a pane
    /// gives the same text, cluster by cluster (fux-vt's width rule): an
    /// emoji presentation sequence, U+2764 U+FE0F, takes two, though its
    /// characters alone take one and none.
    #[test]
    fn fux_text_takes_a_cluster_s_columns() {
        let heart = "\u{2764}\u{FE0F}";
        assert_eq!(width(heart), 2);
        let mut grid = Grid::new(1, 10);
        let end = grid.text(0, 0, &format!("{heart}x"), Attributes::default(), 10);
        assert_eq!(end, 3);
        let at = |x| {
            grid.get(0, x)
                .map(|c| (c.contents().to_owned(), c.is_wide()))
        };
        assert_eq!(at(0), Some((heart.to_owned(), true)));
        assert_eq!(at(2), Some(("x".to_owned(), false)));
        assert_eq!(
            fit(&format!("{heart}{heart}{heart}"), 4)
                .map(|(t, _)| t)
                .collect::<String>(),
            format!("{heart}…")
        );
    }

    #[test]
    fn fitted_text_keeps_measured_cell_boundaries_through_drawing() {
        let long = format!(
            "👩{}👩XYZ",
            std::iter::repeat_n("\u{200d}👩", 18).collect::<String>()
        );
        assert_eq!(width(&long), 7);
        let mut grid = Grid::new(1, 6);
        let fitted = fit(&long, 6);
        assert_eq!(cell_width(fitted.clone()), 6);
        assert_eq!(grid.text_cells(0, 0, fitted, Attributes::default(), 6), 6);
        let cell = |x| {
            grid.get(0, x)
                .map(|c| (c.contents().to_owned(), c.is_wide()))
        };
        assert!(cell(0).is_some_and(|(t, wide)| wide && t.ends_with('\u{200d}')));
        assert_eq!(cell(2), Some(("👩".to_owned(), true)));
        assert_eq!(cell(4), Some(("X".to_owned(), false)));
        assert_eq!(cell(5), Some(("…".to_owned(), false)));
    }

    #[test]
    fn a_scrolled_prompt_keeps_its_cursor_visible() {
        for (before, room) in [
            ("a\u{600}".to_owned(), 3),
            ("\u{d4e}".to_owned(), 3),
            (
                format!(
                    "👩{}👩",
                    std::iter::repeat_n("\u{200d}👩", 18).collect::<String>()
                ),
                7,
            ),
        ] {
            let prompt = prompt_line(&before, "tail", room);
            assert!(cell_width(prompt.cells()) <= room);
            let mut grid = Grid::new(1, room);
            grid.text_cells(0, 0, prompt.cells(), Attributes::default(), room);
            assert!((0..room).any(|x| grid.get(0, x).is_some_and(|c| c.contents().contains('▏'))));
        }
    }

    #[test]
    fn only_the_focused_pane_owns_the_outer_cursor_colour() -> Result<(), String> {
        let (mut session, client) = crate::session::testing::attached(10, 40)?;
        let mut shown = compose(&session, client).ok_or("a screen")?;
        let paint = |s: &Session, shown: &mut Grid| -> Result<String, String> {
            let new = compose(s, client).ok_or("a screen")?;
            let mut out = Vec::new();
            paint_into(shown, true, &new, &mut out);
            *shown = new;
            Ok(String::from_utf8_lossy(&out).into_owned())
        };
        session.output(PaneId::of(1), b"\x1b]12;#00ff00\x07");
        assert!(paint(&session, &mut shown)?.contains("\x1b]12;#00ff00\x1b\\"));
        assert!(paint(&session, &mut shown)?.is_empty(), "unchanged colour");
        crate::session::testing::run(&mut session, "str %1 pane horizontal split")?;
        crate::session::testing::run(&mut session, "str c1 client str %2 pane focus-pane")?;
        assert!(paint(&session, &mut shown)?.contains("\x1b]112\x1b\\"));
        session.output(PaneId::of(1), b"\x1b]12;#ff0000\x07");
        assert!(!paint(&session, &mut shown)?.contains("\x1b]12"));
        crate::session::testing::run(&mut session, "str c1 client str %1 pane focus-pane")?;
        assert!(paint(&session, &mut shown)?.contains("\x1b]12;#ff0000\x1b\\"));
        if let Some(view) = session.views.get_mut(&client) {
            view.mode = Mode::Panel(Panel::Repeat("repeat".into()));
        }
        assert!(paint(&session, &mut shown)?.contains("\x1b]112\x1b\\"));
        if let Some(view) = session.views.get_mut(&client) {
            view.mode = Mode::Normal;
        }
        assert!(paint(&session, &mut shown)?.contains("\x1b]12;#ff0000\x1b\\"));
        session.output(PaneId::of(1), b"\x1b]112\x07");
        assert!(paint(&session, &mut shown)?.contains("\x1b]112\x1b\\"));
        Ok(())
    }

    /// On a line drawn twice as wide (DECDWL, DEC STD 070 5.6.1), copy
    /// mode's cursor and selection take each cell's two columns, as the
    /// normal cursor does: a cell at column 1 shows at columns 2 and 3.
    #[test]
    fn copy_mode_on_a_double_width_line_takes_two_columns_a_cell() -> Result<(), String> {
        let (mut s, c) = crate::session::testing::attached(10, 40)?;
        s.output(PaneId::of(1), b"\x1b[2J\x1b[H\x1b#6ABCD\x1b[H");
        s.input(c, b"\x02cl");
        let grid = compose(&s, c).ok_or("a screen")?;
        assert_eq!(grid.cursor, Some((0, 2)), "on B's first column");
        s.input(c, b"vl");
        let grid = compose(&s, c).ok_or("a screen")?;
        assert_eq!(grid.cursor, Some((0, 4)), "on C's first column");
        let inverse: Vec<bool> = (0..8)
            .map(|x| grid.get(0, x).is_some_and(|cell| cell.inverse()))
            .collect();
        assert_eq!(
            inverse,
            [false, false, true, true, true, true, false, false],
            "B and C selected"
        );
        Ok(())
    }

    /// A pane's program started again (`respawn`) draws on a new screen,
    /// whose rows are numbered from the start again: a grid composed
    /// again from its memo shows the new screen, not the rows of the old
    /// one that had the same numbers and versions.
    #[test]
    fn a_respawned_pane_is_composed_anew() -> Result<(), String> {
        let (mut s, c) = crate::session::testing::attached(6, 30)?;
        let id = PaneId::of(1);
        s.output(id, b"A");
        let mut grid = Grid::new(0, 0);
        let mut placement = Placement::default();
        assert!(compose_into(&s, c, &mut grid, &mut placement));
        let launch = crate::pane::Launch {
            argv: vec!["x".into()],
            cwd: "/".into(),
            env: Vec::new(),
        };
        let runs = crate::pane::Runs::Explicit(launch);
        let size = (s.parts.panes().get(&id)).ok_or("the pane")?.size();
        let fresh = crate::pane::Pane::new(id, String::new(), runs, size, 1 << 10)
            .map_err(|e| e.to_string())?;
        let mut pane = s.parts.pane_mut(id).ok_or("the pane")?;
        pane.respawned(fresh, crate::pane::Process::Absent);
        s.output(id, b"B");
        assert!(compose_into(&s, c, &mut grid, &mut placement));
        assert_eq!(grid.row_text(0), "B");
        Ok(())
    }

    /// While a pane is part way through a frame, its cursor is the one the
    /// client was last painted there, if any: focus moved away from it
    /// takes the cursor to the pane focused, and focus moved onto it shows
    /// none there till the frame is whole.
    #[test]
    fn a_held_pane_keeps_only_its_own_cursor() -> Result<(), String> {
        let (mut s, c) = crate::session::testing::attached(10, 40)?;
        crate::session::testing::run(&mut s, "str %1 pane horizontal split")?;
        s.output(PaneId::of(1), &frame_past_a_budget());
        assert!(
            s.parts
                .panes()
                .get(&PaneId::of(1))
                .is_some_and(crate::pane::Pane::mid_frame)
        );
        let (held, other) = (place(&s, c, 1)?, place(&s, c, 2)?);
        let panes = [(PaneId::of(1), held), (PaneId::of(2), other)];
        let compose = |s: &Session, painted: &Grid| {
            let mut grid = Grid::new(0, 0);
            let painted = Some(Painted {
                grid: painted,
                panes: &panes,
            });
            if let Some(view) = s.views.get(&c) {
                compose_held(s, view, &mut grid, &mut Placement::default(), painted);
            }
            grid
        };
        // Focus on %2, the cursor last painted in %1: %2's own.
        let mut painted = compose(&s, &Grid::new(0, 0));
        painted.cursor = Some((held.y(), held.x()));
        painted.modes.cursor_colour = Some([0, 255, 0].into());
        let fresh = compose(&s, &painted);
        assert!(
            fresh.cursor.is_some_and(|(y, x)| other.contains(y, x)),
            "{:?}",
            fresh.cursor
        );
        assert_eq!(fresh.modes.cursor_colour, None);
        // Focus on %1, the cursor last painted in %2: none.
        crate::session::testing::run(&mut s, "str c1 client str next focus")?;
        painted.cursor = Some((other.y(), other.x()));
        assert_eq!(compose(&s, &painted).cursor, None);
        assert_eq!(compose(&s, &painted).modes.cursor_colour, None);
        // And its own, as painted.
        painted.cursor = Some((held.y(), held.x().saturating_add(1)));
        assert_eq!(compose(&s, &painted).cursor, painted.cursor);
        assert_eq!(
            compose(&s, &painted).modes.cursor_colour,
            painted.modes.cursor_colour
        );
        // But none under fux's own panel, as for any pane.
        s.input(c, b"\x02x");
        assert_eq!(compose(&s, &painted).cursor, None, "under a confirmation");
        Ok(())
    }

    /// A frame whose reading spends more than a parser call's budget.
    fn frame_past_a_budget() -> Vec<u8> {
        let mut frame = b"\x1b[?2026h".to_vec();
        frame.extend(std::iter::repeat_n(b"X\x1b[65535b", 4000).flatten());
        frame.extend_from_slice(b"\x1b[?2026l");
        frame
    }

    /// Where client `c` shows pane `%id`.
    fn place(s: &Session, c: ClientId, id: u32) -> Result<Rect, String> {
        let mut placement = Placement::default();
        if let Some(view) = s.views.get(&c) {
            s.placement_into(view, &mut placement);
        }
        placement
            .rect(PaneId::of(id))
            .ok_or_else(|| format!("%{id}'s place"))
    }

    /// A pane part way through a frame that the client was never painted
    /// where it is (a client attached mid-frame, a layout changed) shows
    /// blank, never half the frame; fux's own panels show over it.
    #[test]
    fn a_held_pane_not_painted_before_is_blank_under_fux_s_panels() -> Result<(), String> {
        let (mut s, c) = crate::session::testing::attached(10, 40)?;
        s.output(PaneId::of(1), &frame_past_a_budget());
        assert!(
            s.parts
                .panes()
                .get(&PaneId::of(1))
                .is_some_and(crate::pane::Pane::mid_frame)
        );
        let compose = |s: &Session, painted: Option<Painted<'_>>| {
            let mut grid = Grid::new(0, 0);
            if let Some(view) = s.views.get(&c) {
                compose_held(s, view, &mut grid, &mut Placement::default(), painted);
            }
            (0..grid.rows).map(|y| grid.row_text(y)).collect::<Vec<_>>()
        };
        let rows = compose(&s, None);
        assert!(rows.iter().all(|r| !r.contains('X')), "{rows:#?}");
        // A confirmation over it, painted as it was before (all X): shown.
        let mut before = Grid::new(10, 40);
        let xs: String = std::iter::repeat_n('X', 40).collect();
        for y in 0..9 {
            before.text(y, 0, &xs, Attributes::default(), 40);
        }
        s.input(c, b"\x02x");
        let panes = [(PaneId::of(1), place(&s, c, 1)?)];
        let rows = compose(
            &s,
            Some(Painted {
                grid: &before,
                panes: &panes,
            }),
        );
        assert!(rows.iter().any(|r| r.contains("close")), "{rows:#?}");
        assert!(
            rows.iter().any(|r| r.contains("XXXX")),
            "the pane as painted: {rows:#?}"
        );
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
    /// (xterm, through wcwidth) would put what follows elsewhere.
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
            (0..left).for_each(|_| drop(line.edit(&crate::act::Edit::Left)));
            line
        };
        let end = text.chars().count();
        for (left, starts, ends) in [(0, "…", "TAIL▏"), (end, "▏spl", "…"), (20, "…", "…")]
        {
            let line = line(text, left);
            let cells = prompt_line(line.before(), line.after(), 12);
            let shown: String = cells.cells().map(|(text, _)| text).collect();
            assert!(width(&shown) <= 12, "{shown:?} fits");
            assert!(shown.contains('▏'), "{shown:?} shows the cursor");
            assert!(shown.starts_with(starts), "{shown:?} starts {starts:?}");
            assert!(shown.ends_with(ends), "{shown:?} ends {ends:?}");
        }
        // Wide chars count two cells; short text is as it was.
        assert!(cell_width(prompt_line("界界界界界界界界", "", 7).cells()) <= 7);
        assert_eq!(
            prompt_line("ls", "", 12)
                .cells()
                .map(|(t, _)| t)
                .collect::<String>(),
            "ls▏"
        );
    }

    fn grid_lines(g: &Grid) -> Vec<String> {
        (0..g.rows).map(|y| g.row_text(y)).collect()
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
        // Read back by a terminal, each cell has what it was painted with.
        let mut parser = terminal(1, 12)?;
        parser
            .process(&paint(None, &grid))
            .map_err(|e| e.to_string())?;
        for (x, a) in attributes.iter().enumerate() {
            let x = u16::try_from(x).map_err(|e| e.to_string())?;
            let cell = parser.screen().cell(0, x).ok_or("a cell")?;
            assert_eq!(cell.attributes(), *a, "cell {x}");
        }
        Ok(())
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
        assert!(!plain.contains("4:"), "{plain:?}");
        let mut styled = grid.clone();
        styled.underline_styles = true;
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
        // Each pane answers its program with its own colours, and one that
        // set none, with no terminal's to give, answers nothing.
        s.output(PaneId::of(1), b"\x1b]4;1;#00ff00\x07");
        for (id, answer) in [(1, "\x1b]4;1;rgb:0000/ffff/0000\x07"), (2, "")] {
            let mut pane = s.parts.pane_mut(PaneId::of(id)).ok_or("a pane")?;
            pane.drain_input();
            pane.output(b"\x1b]4;1;?\x07");
            let asked = pane.drain_input();
            assert_eq!(String::from_utf8_lossy(&asked), answer, "pane {id}");
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
        assert_eq!(
            fit("hello", 10).map(|(t, _)| t).collect::<String>(),
            "hello"
        );
        assert_eq!(fit("hello", 4).map(|(t, _)| t).collect::<String>(), "hel…");
        assert_eq!(fit("界界界", 4).map(|(t, _)| t).collect::<String>(), "界…");
        assert_eq!(fit("x", 0).map(|(t, _)| t).collect::<String>(), "");
    }
}
