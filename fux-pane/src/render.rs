//! A grid of cells: a pane's screen composed on one, and the bytes that
//! turn the grid a terminal shows into another. Every paint leaves the
//! terminal's attributes at their default, which the next one assumes.
use fux_vt::clusters;
use fux_vt::sixel::Picture;
use fux_vt::{Attributes, CellRef, Cells, Color, CursorShape, Hyperlink, Rgb, Row, Screen};
use fux_wire::answer::sgr;
use fux_wire::id::PaneId;
use fux_wire::layout::{Axis, Rect};
use std::io::Write;
use std::sync::Arc;

#[derive(Debug, Default)]
pub struct Grid {
    pub rows: u16,
    pub cols: u16,
    /// Row after row; clusters too long to hold inline are kept whole.
    pub cells: Cells,
    /// Each cell's hyperlink.
    pub links: Links,
    /// Where the terminal cursor is shown, if it is.
    pub cursor: Option<(u16, u16)>,
    /// The modes the client's terminal is set to.
    pub modes: Modes,
    /// Whether the client's terminal draws underline styles (`outer`):
    /// painted as they are, else as plain underlines (`sgr`).
    pub underline_styles: bool,
    /// Explicitly position after non-ASCII glyphs.
    pub cell_placement: bool,
    /// What composed the grid, if it can say (`Memo`). Not what the grid
    /// shows: grids compare equal whatever composed them, and a copy has
    /// none.
    pub memo: Option<Memo>,
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
    /// The focused pane's explicit cursor colour; none restores the
    /// terminal's configured colour.
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
    pub fn diff(&self, shown: &Modes, known: bool, out: &mut Vec<u8>) {
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

/// The pictures pane `id`'s `screen` shows laid out in cells of `cell`,
/// placed at `rect` and clipped to it.
pub fn placed(id: PaneId, rect: Rect, screen: &Screen, cell: (u16, u16)) -> Vec<Visible> {
    let mut visible = Vec::new();
    for shown in screen.pictures().filter(|s| s.cell == cell) {
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
    visible
}

/// The sixel data of the pictures a client was last sent, each with its
/// picture and crop: a picture that only moves is sent again with the same
/// data at its new place, encoded once (`fux-vt/THREATS.md` B08). At most
/// `SIXEL_BYTES` are kept; a picture past them is encoded each time it is
/// sent.
#[derive(Debug, Default)]
pub struct Sixels {
    pub kept: Vec<Kept>,
}

/// A picture's sixel data for its crop.
#[derive(Debug)]
pub struct Kept {
    pub picture: Arc<Picture>,
    pub crop: (u32, u32, u32, u32),
    pub data: Vec<u8>,
}

impl Kept {
    pub fn shows(&self, picture: &Visible) -> bool {
        Arc::ptr_eq(&self.picture, &picture.picture) && self.crop == picture.crop
    }
}

/// The most sixel data a client keeps to send again: 16 MiB, at
/// `fux-vt/THREATS.md` B08's worst of 12 bytes a pixel a 1,600 by 870
/// picture, and a screenful of most. One past it is encoded each time it is
/// sent, its work bounded by B08.
pub const SIXEL_BYTES: usize = 16 << 20;

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
        let head = |g: &Grid| {
            (
                (g.rows, g.cols, g.cursor),
                (g.modes, g.underline_styles, g.cell_placement),
            )
        };
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
pub struct Memo {
    pub frame: Frame,
    /// For each of the frame's panes, in its order: each of its window's
    /// rows on the grid, as many as the frame gives it.
    pub keys: Vec<Vec<RowKey>>,
}

/// A pane row's identity and version, `None` where the window has no row.
pub type RowKey = Option<(fux_vt::RowId, u64)>;

/// Everything about a grid composed in normal mode but its panes' rows: its
/// size, each pane's place, program (`Pane::starts`) and window size, the
/// separators, and the focused pane and styles, which colour them. None for
/// a client shown no tab, or with a pane in colours of its own (`recolour`),
/// whose rows depend on more than their keys, or its own pane
/// (`View::runs_in`). A pane's screen alone (`pane_grid`) has no styles.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub rows: u16,
    pub cols: u16,
    pub panes: Vec<(PaneId, u64, Rect, (u16, u16))>,
    pub separators: Vec<fux_wire::layout::Separator>,
    pub focus: PaneId,
    pub styles: Option<fux_wire::presentation::Styles>,
    pub bar_position: fux_wire::presentation::BarPosition,
}

impl Memo {
    /// Puts again on `grid`, which this memo's frame composed and composes
    /// now, the pane rows that changed since: the grid's memo then.
    pub fn update<'s>(
        mut self,
        grid: &mut Grid,
        screens: &dyn Fn(PaneId) -> Option<&'s Screen>,
    ) -> Memo {
        for ((id, _, rect, _), keys) in self.frame.panes.iter().zip(&mut self.keys) {
            let Some(screen) = screens(*id) else {
                continue;
            };
            let window = screen.window();
            for (((_, row), line), key) in window.iter().zip(rect.lines()).zip(keys.iter_mut()) {
                let now = Some((row.id(), row.version()));
                if now != *key {
                    draw_row(grid, *id, Some(row), (line, window.cols()), None);
                    *key = now;
                }
            }
        }
        self
    }
}

/// Two grids composed for one frame (`Memo`): proof that their keys can be
/// compared.
pub struct SameFrame<'a> {
    pub frame: &'a Frame,
    pub keys: (&'a [Vec<RowKey>], &'a [Vec<RowKey>]),
}

impl<'a> SameFrame<'a> {
    pub fn of(a: &'a Grid, b: &'a Grid) -> Option<SameFrame<'a>> {
        let (a, b) = (a.memo.as_ref()?, b.memo.as_ref()?);
        (a.frame == b.frame).then_some(SameFrame {
            frame: &a.frame,
            keys: (&a.keys, &b.keys),
        })
    }
    /// Whether row `y` is in the pane area and has the same pane rows in
    /// both grids: then it shows the same cells.
    pub fn same_row(&self, y: u16) -> bool {
        let bar = match self.frame.bar_position {
            fux_wire::presentation::BarPosition::Top => Some(0),
            fux_wire::presentation::BarPosition::Bottom => self.frame.rows.checked_sub(1),
            fux_wire::presentation::BarPosition::Off => None,
        };
        Some(y) != bar
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
pub fn draw_row(
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
pub struct Links(Vec<Option<Vec<Link>>>);

/// A cell's hyperlink, and the pane whose it is.
pub type Link = Option<(PaneId, Linked)>;

/// A hyperlink as a grid holds it: a pane's own, or as a grid sent to a
/// connection's process has it (`wire`), told by its key and URI alike.
#[derive(Clone, Debug)]
pub enum Linked {
    Vt(Hyperlink),
    Sent { key: u64, uri: Arc<str> },
}

impl Linked {
    pub fn key(&self) -> u64 {
        match self {
            Linked::Vt(link) => link.key(),
            Linked::Sent { key, .. } => *key,
        }
    }
    pub fn uri(&self) -> &str {
        match self {
            Linked::Vt(link) => link.uri(),
            Linked::Sent { uri, .. } => uri,
        }
    }
}

impl PartialEq for Linked {
    fn eq(&self, other: &Linked) -> bool {
        (self.key(), self.uri()) == (other.key(), other.uri())
    }
}

impl Links {
    pub fn at(&self, y: u16, x: u16) -> Option<(PaneId, &Linked)> {
        let (pane, link) = self.row(y)?.get(usize::from(x))?.as_ref()?;
        Some((*pane, link))
    }

    pub fn row(&self, y: u16) -> Option<&[Link]> {
        self.0.get(usize::from(y))?.as_deref()
    }
    /// Gives cell `x` of row `y`, `cols` wide, `link`.
    pub fn set(&mut self, y: u16, x: u16, link: Option<(PaneId, &Linked)>, cols: u16) {
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
    pub fn unlink(&mut self, y: u16, xs: std::ops::Range<u16>) {
        if let Some(Some(row)) = self.0.get_mut(usize::from(y)) {
            let run = usize::from(xs.end.saturating_sub(xs.start));
            row.iter_mut()
                .skip(usize::from(xs.start))
                .take(run)
                .for_each(|l| *l = None);
        }
    }
    /// Whether row `y` has the same links as `other`'s.
    pub fn row_eq(&self, other: &Links, y: u16) -> bool {
        match (self.row(y), other.row(y)) {
            (Some(a), Some(b)) => a == b,
            (Some(row), None) | (None, Some(row)) => row.iter().all(Option::is_none),
            (None, None) => true,
        }
    }
}

impl Grid {
    /// A blank grid `rows` by `cols`, or the cell allocation's refusal.
    pub fn new(rows: u16, cols: u16) -> Result<Grid, fux_vt::Error> {
        Ok(Grid {
            rows,
            cols,
            cells: Cells::new(cell_count(rows, cols))?,
            ..Grid::default()
        })
    }
    /// The hyperlink of the cell at `y`, `x`, and the pane whose it is.
    pub fn link(&self, y: u16, x: u16) -> Option<(PaneId, &Linked)> {
        self.links.at(y, x)
    }
    /// Makes the grid `rows` by `cols`, resizing its cells only if the size
    /// changed, so that a grid composed into again allocates nothing; and
    /// blanks it if `blank`, as a resized grid is anyway.
    pub fn reset(&mut self, rows: u16, cols: u16, blank: bool) -> Result<(), fux_vt::Error> {
        if (self.rows, self.cols) != (rows, cols) {
            // Reserve before changing anything, so refusal leaves the old grid whole.
            self.cells.reset(cell_count(rows, cols))?;
            (self.rows, self.cols) = (rows, cols);
        } else if blank {
            self.cells.fill(0..self.cells.len(), CellRef::default());
        }
        self.links.0.clear();
        self.cursor = None;
        self.modes = Modes::default();
        self.memo = None;
        Ok(())
    }
    pub fn index(&self, y: u16, x: u16) -> Option<usize> {
        (y < self.rows && x < self.cols)
            .then(|| cell_count(y, self.cols).saturating_add(usize::from(x)))
    }
    pub fn get(&self, y: u16, x: u16) -> Option<CellRef<'_>> {
        self.index(y, x).and_then(|i| self.cells.get(i))
    }
    /// Whether painting this grid clears the previous screen, including pictures.
    pub fn clears(&self, shown: &Grid) -> bool {
        (
            self.rows,
            self.cols,
            self.underline_styles,
            self.cell_placement,
        ) != (
            shown.rows,
            shown.cols,
            shown.underline_styles,
            shown.cell_placement,
        )
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
    pub fn row_eq(&self, other: &Grid, y: u16) -> bool {
        self.cells.range_eq(&other.cells, self.span(y)) && self.links.row_eq(&other.links, y)
    }
    /// Whether the cell at (`y`, `x`) shows what `other`'s, a grid of its
    /// size, does.
    pub fn cell_eq(&self, other: &Grid, y: u16, x: u16) -> bool {
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
    pub fn span(&self, y: u16) -> std::ops::Range<usize> {
        let start = cell_count(y, self.cols);
        start..start.saturating_add(usize::from(self.cols))
    }
    /// Copies the first `width` cells of pane `pane`'s row `row` into row `y`
    /// from column `x`, with their links, clipped at the grid's edge,
    /// without `set`'s repairs: for a run of one well-formed row onto blank
    /// cells, which keeps its wide glyphs whole by itself.
    pub fn put_row(&mut self, y: u16, x: u16, pane: PaneId, row: Row<'_>, width: u16) {
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
            let link = row
                .link(usize::from(col))
                .map(|link| Linked::Vt(link.clone()));
            self.links
                .set(y, gx, link.as_ref().map(|link| (pane, link)), self.cols);
        }
    }
    /// `put_row` for a row drawn twice as wide (`fux_vt::LineRendition`),
    /// as no terminal can draw half a line so: each cell takes two
    /// columns, its glyph and a blank in its colours, a wide glyph four;
    /// what the doubled cells do not reach of `width` is blank. A double
    /// height half is drawn as double width.
    pub fn put_doubled_row(&mut self, y: u16, x: u16, pane: PaneId, row: &Row<'_>, width: u16) {
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
            let link = row.link(col).map(|link| Linked::Vt(link.clone()));
            self.links
                .set(y, gx, link.as_ref().map(|link| (pane, link)), self.cols);
        }
    }
    /// Draws the first `width` cells of row `y` from column `x`, a pane's
    /// row as `draw_row` put it, blanks too, in the colours its program set
    /// (`pane_colours`): the client's own palette is never changed.
    pub fn recolour(&mut self, y: u16, x: u16, width: u16, screen: &Screen) {
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
    pub fn put(&mut self, y: u16, x: u16, cell: CellRef<'_>) {
        if let Some(i) = self.index(y, x) {
            self.cells.set(i, cell);
            self.links.unlink(y, x..x.saturating_add(1));
        }
    }
    /// Blanks `area`, clipped at the grid's edge.
    pub fn blank(&mut self, area: Rect) {
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
    pub fn set(&mut self, y: u16, x: u16, cell: CellRef<'_>) {
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
    /// The text of a composed row for tests, trailing blanks trimmed.
    #[cfg(any(test, feature = "testing"))]
    pub fn row_text(&self, y: u16) -> String {
        let row = self.row(y).filter(|c| !c.is_wide_continuation());
        row.map(|c| c.shown())
            .collect::<String>()
            .trim_end()
            .to_owned()
    }

    /// Writes `text` from (y, x), a cluster a cell (`clusters`), clipped at
    /// `limit`, wide glyphs whole; the column after it is returned.
    #[cfg(any(test, feature = "testing"))]
    pub fn text(&mut self, y: u16, x: u16, text: &str, style: Attributes, limit: u16) -> u16 {
        self.text_cells(y, x, clusters(text), style, limit)
    }

    /// Draws measured cells without segmenting their retained text again.
    pub fn text_cells<'a>(
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

    pub fn fill(&mut self, area: Rect, style: Attributes) {
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
pub fn pane_colours(attributes: Attributes, screen: &Screen) -> Attributes {
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

/// The colours a pane's program set (`pane_colours`): DECSCNM, its default
/// foreground and background (OSC 10, 11) and the palette entries it
/// changed (OSC 4). The cursor's colour goes with the cursor (`Modes`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Own {
    pub reverse: bool,
    pub defaults: [Option<Rgb>; 2],
    pub palette: Vec<(u8, Rgb)>,
}

impl Own {
    /// What `screen`'s program set.
    pub fn of(screen: &Screen) -> Own {
        Own {
            reverse: screen.mode(fux_vt::Mode::ReverseVideo),
            defaults: [screen.dynamic_color(10), screen.dynamic_color(11)],
            palette: (0..=u8::MAX)
                .filter_map(|n| Some((n, screen.palette_color(n)?)))
                .collect(),
        }
    }

    /// Appends what sets them on a screen that has none.
    pub fn put(&self, out: &mut Vec<u8>) {
        if self.reverse {
            out.extend_from_slice(b"\x1b[?5h");
        }
        for (number, rgb) in [10u8, 11].into_iter().zip(self.defaults) {
            if let Some(Rgb { r, g, b }) = rgb {
                let _ = write!(out, "\x1b]{number};#{r:02x}{g:02x}{b:02x}\x1b\\");
            }
        }
        for (n, Rgb { r, g, b }) in &self.palette {
            let _ = write!(out, "\x1b]4;{n};#{r:02x}{g:02x}{b:02x}\x1b\\");
        }
    }
}

/// Whether a pane is drawn in colours of its own (`pane_colours`): its
/// program changed some, or set DECSCNM. Its rows then depend on more
/// than their keys, so a frame with it is not memo'd (`Frame::of`).
pub fn own_colours(screen: &Screen) -> bool {
    screen.colors_changed() || screen.mode(fux_vt::Mode::ReverseVideo)
}

/// How many cells `rows` rows of `cols` are. Exact: a u16 by a u16 fits
/// even a 32-bit usize.
pub fn cell_count(rows: u16, cols: u16) -> usize {
    usize::from(rows).saturating_mul(usize::from(cols))
}

/// A char's display width in cells, as fux-vt lays out what it holds
/// (`fux_vt::char_width`: 0, 1 or 2), so the painter and the screen agree.
pub fn cells(c: char) -> u16 {
    fux_vt::char_width(c).map_or(0, |w| u16::try_from(w).unwrap_or(u16::MAX))
}

/// Display width of a string, as a pane prints it (`fux_vt::str_width`).
pub fn width(text: &str) -> u16 {
    u16::try_from(fux_vt::str_width(text)).unwrap_or(u16::MAX)
}

/// Cuts `text` to `cols` cells, preserving their measured boundaries.
pub fn fit(text: &str, cols: u16) -> impl Iterator<Item = (&str, u16)> + Clone {
    fit_cells(clusters(text), cols, width(text) > cols)
}

pub fn cell_width<'a>(cells: impl Iterator<Item = (&'a str, u16)>) -> u16 {
    cells.fold(0u16, |sum, (_, width)| sum.saturating_add(width))
}

pub fn fit_cells<'a>(
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

/// Pane `pane`'s screen alone on `grid`, at its size, with its cursor,
/// cursor shape and colour and mouse tracking as its program set them:
/// what its process sends the connections that show it (`host`), with the
/// colours its program set (`Own`). `respawns` tells its programs apart. A
/// grid this composed before for the same screen has only the rows that
/// changed since drawn again.
pub fn pane_grid(
    pane: PaneId,
    respawns: u64,
    screen: &Screen,
    grid: &mut Grid,
) -> Result<(), fux_vt::Error> {
    let size = screen.size();
    let (rows, cols) = (size.rows(), size.cols());
    let area = Rect::screen(rows, cols);
    let frame = Frame {
        rows,
        cols,
        panes: vec![(pane, respawns, area, size.into())],
        separators: Vec::new(),
        focus: pane,
        styles: None,
        bar_position: fux_wire::presentation::BarPosition::Off,
    };
    let screens = |id: PaneId| (id == pane).then_some(screen);
    // A grid blanked has no rows: every row the screen has is drawn.
    let memo = match grid.memo.take().filter(|m| m.frame == frame) {
        Some(memo) => memo,
        None => {
            grid.reset(rows, cols, true)?;
            let keys = vec![vec![None; usize::from(rows)]];
            Memo { frame, keys }
        }
    };
    grid.memo = Some(memo.update(grid, &screens));
    grid.underline_styles = true;
    grid.cursor = None;
    grid.modes = Modes::default();
    let (y, x) = screen.cursor_position();
    // On a row drawn twice as wide, at its glyph's first half.
    let x = match screen.line_rendition(y).is_double() {
        true => x.saturating_mul(2),
        false => x,
    };
    if screen.mode(fux_vt::Mode::ShowCursor) && y < rows && x < cols {
        grid.cursor = Some((y, x));
        grid.modes.cursor_shape = screen.cursor_shape();
        grid.modes.cursor_colour = screen.dynamic_color(12);
    }
    grid.modes.mouse = mouse_level(screen.mouse_protocol_mode());
    Ok(())
}

/// OSC 8: opens `link` in the client's terminal, or closes the open link.
/// Its `id` is the pane's and the link's key there, so that no two panes'
/// links are one, and a link painted in pieces is one, as the spec asks of
/// a multiplexer (`references/modern/osc8_hyperlinks.md`).
pub fn hyperlink(out: &mut Vec<u8>, link: Option<(PaneId, &Linked)>) {
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
pub fn joins_the_glyph_before(grid: &Grid, y: u16, x: u16, text: &str) -> bool {
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
pub fn cluster(
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
    joins || measured_apart || (grid.cell_placement && !text.is_ascii())
}

/// A row or column as the terminal counts it, from 1; exact in a u32.
pub fn one_based(n: u16) -> u32 {
    u32::from(n).saturating_add(1)
}

/// Moves the cursor to row `y`, column `x`, both from 0.
pub fn goto(out: &mut Vec<u8>, y: u16, x: u16) {
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
#[cfg(any(test, feature = "testing"))]
pub fn paint(old: Option<&Grid>, new: &Grid) -> Vec<u8> {
    let mut out = Vec::new();
    let blank = Grid::default();
    paint_into(old.unwrap_or(&blank), old.is_some(), new, &mut out);
    out
}

/// Appends the bytes that turn `shown`, what the client shows if `known`
/// (else what it last showed, whose cursor shape it keeps), into `new` to
/// `out`; false, appending nothing, if the client shows `new` already.
/// Each row is compared once.
pub fn paint_into(shown: &Grid, known: bool, new: &Grid, out: &mut Vec<u8>) -> bool {
    let full = !known || new.clears(shown);
    let before = (!full).then_some(shown);
    let rows = differing_rows(before, new);
    if !full && rows.is_empty() && (shown.cursor, shown.modes) == (new.cursor, new.modes) {
        return false;
    }
    out.extend_from_slice(b"\x1b[?2026h\x1b[?25l");
    if before.is_none() {
        out.extend_from_slice(b"\x1b[0m\x1b[H\x1b[2J");
    }
    let mut current: Option<Attributes> = None;
    // The hyperlink the client's terminal has open.
    let mut open = None;
    for &y in &rows {
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
    true
}

/// The rows of `new` that differ from `old`, a grid of its size (every
/// row, if there is none): those whose memo does not show them the same
/// and whose cells or links differ.
pub fn differing_rows(old: Option<&Grid>, new: &Grid) -> Vec<u16> {
    let memo = old.and_then(|o| SameFrame::of(o, new));
    (0..new.rows)
        .filter(|&y| {
            // An unchanged row costs this one comparison.
            !memo.as_ref().is_some_and(|m| m.same_row(y)) && old.is_none_or(|o| !o.row_eq(new, y))
        })
        .collect()
}
