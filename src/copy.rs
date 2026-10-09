//! Copy/select mode: a keyboard cursor over a pane and its history.
//!
//! For its client the pane holds still: the view and the cursor are
//! anchored to fux-vt row IDs, so new output does not move them, while other
//! clients see the pane live. The mode ends if the pane closes or its history
//! drops the rows it holds.
use crate::id::ClientId;
use crate::id::PaneId;
use crate::keys::{Direction, Key, KeyPress};
use crate::render::shown;
use crate::session::{Error, Outgoing, Session};
use crate::view::{Mode, Notice, View};
use fux_vt::{CellRef, Row, RowId, Screen};

/// The most cells one copy takes.
pub const MAX_CELLS: usize = 262_144;
/// The largest OSC 52 payload, encoded.
pub const MAX_CLIPBOARD: usize = 1 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Select {
    Char,
    Line,
    Block,
}

/// Which way a search goes: toward later rows, or earlier ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Seek {
    Forward,
    Backward,
}

impl Seek {
    /// The other way.
    pub fn reversed(self) -> Seek {
        match self {
            Seek::Forward => Seek::Backward,
            Seek::Backward => Seek::Forward,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Search {
    pub query: String,
    pub seek: Seek,
}

pub struct Copy {
    pub pane: PaneId,
    /// The row at the top of the view.
    pub top: RowId,
    pub cursor: (RowId, u16),
    pub selection: Option<(Select, (RowId, u16))>,
    pub search: Option<Search>,
    /// A search being typed: its direction and text.
    pub typing: Option<(Seek, String)>,
    /// The screen as it was when the rows copy mode holds were last found
    /// still there; forgotten when a key moves it.
    pub held_at: Option<fux_vt::Mark>,
}

// ------------------------------------------------------------- positions

/// A place in the rows retained: a row and a column of it.
pub type Place<'a> = (Row<'a>, u16);

/// Copy mode's positions, each row found once for the paint or key that
/// needs them rather than at every use: all of them, as copy mode ends when
/// any of its rows is gone.
#[derive(Clone, Copy, Debug)]
pub struct Resolved<'a> {
    /// The row at the top of the view: no lower than the screen's top,
    /// where its window starts (`Row::window`).
    pub top: Row<'a>,
    pub cursor: Place<'a>,
    /// The selection: its kind and its anchor.
    pub selection: Option<(Select, Place<'a>)>,
}

impl<'a> Resolved<'a> {
    /// The cursor, as a row and column of the view, if it is in its first
    /// `height` rows.
    pub fn cursor_in_view(&self, height: u16) -> Option<(u16, u16)> {
        let (row, col) = self.cursor;
        let y = self.top.window().place(&row)?;
        (y < height).then_some((y, col))
    }

    /// The selection's kind and its two ends, in order.
    fn ends(&self) -> Option<(Select, Place<'a>, Place<'a>)> {
        let (kind, anchor) = self.selection?;
        let place = |(row, col): Place<'_>| (row.index(), col);
        Some(if place(anchor) <= place(self.cursor) {
            (kind, anchor, self.cursor)
        } else {
            (kind, self.cursor, anchor)
        })
    }

    /// Whether the cell at `col` of `row` is selected.
    pub fn selected(&self, row: &Row<'_>, col: u16) -> bool {
        let Some((kind, start, end)) = self.ends() else {
            return false;
        };
        let (row, start, end) = (
            row.index(),
            (start.0.index(), start.1),
            (end.0.index(), end.1),
        );
        if row < start.0 || row > end.0 {
            return false;
        }
        match kind {
            Select::Line => true,
            Select::Block => {
                let (left, right) = (start.1.min(end.1), start.1.max(end.1));
                (left..=right).contains(&col)
            }
            Select::Char => (row, col) >= start && (row, col) <= end,
        }
    }
}

impl Copy {
    /// Finds the rows it holds, once each; none if any is gone.
    pub fn resolve<'a>(&self, screen: &'a Screen) -> Option<Resolved<'a>> {
        let at = |(id, col): (RowId, u16)| Some((screen.row_by_id(id)?, col));
        let selection = match self.selection {
            Some((kind, anchor)) => Some((kind, at(anchor)?)),
            None => None,
        };
        Some(Resolved {
            top: screen.row_by_id(self.top)?.window().row(0)?,
            cursor: at(self.cursor)?,
            selection,
        })
    }

    /// What the bar shows in place of the tabs, with the cursor where `at`
    /// found it.
    pub fn bar(&self, screen: &Screen, at: &Resolved) -> Bar {
        // Counted from 1; exact, as rows are far fewer than a usize holds.
        let line = at.cursor.0.index().saturating_add(1);
        let position = format!("{line}/{}", screen.rows().len());
        if let Some((seek, text)) = &self.typing {
            let prompt = match seek {
                Seek::Forward => "/",
                Seek::Backward => "?",
            };
            return Bar {
                badge: format!("{prompt}{text}▏"),
                hints: vec![("Enter", "search"), ("Esc", "cancel")],
                position,
            };
        }
        let (badge, hints) = match self.selection {
            // `y` only acts on a selection, so it shows with one.
            Some((kind, _)) => {
                let (badge, clear) = match kind {
                    Select::Char => ("COPY select", "v"),
                    Select::Line => ("COPY lines", "s"),
                    Select::Block => ("COPY block", "x"),
                };
                let hints = vec![
                    ("y", "copy"),
                    ("q", "quit"),
                    ("o", "other end"),
                    (clear, "clear"),
                    ("hjkl", "extend"),
                ];
                (badge, hints)
            }
            None => {
                let mut hints = vec![("v s x", "select"), ("q", "quit"), ("f r", "search")];
                if self.search.is_some() {
                    hints.push(("n p", "next"));
                }
                hints.extend([
                    ("hjkl", "move"),
                    ("w b", "words"),
                    ("u d", "half page"),
                    ("t z", "top bottom"),
                    ("a e", "line"),
                    ("[ ]", "prompts"),
                ]);
                ("COPY", hints)
            }
        };
        Bar {
            badge: badge.to_owned(),
            hints,
            position,
        }
    }
}

/// Copy mode's bar: what it is doing, the keys that act now, most
/// important first so a narrow bar drops the least, and where the cursor
/// is.
pub struct Bar {
    pub badge: String,
    pub hints: Vec<(&'static str, &'static str)>,
    /// The cursor's line, counted from 1, of all the rows retained.
    pub position: String,
}

// ------------------------------------------------------------ text classes

/// A row's cells as (column, class): 0 blank, 1 word, 2 other. The row end
/// is a blank.
fn classes(row: Row<'_>) -> Vec<(u16, u8)> {
    let class = |cell: CellRef<'_>| {
        let c = cell.contents().chars().next().unwrap_or(' ');
        if c.is_whitespace() || !cell.has_contents() {
            0
        } else if c.is_alphanumeric() || c == '_' {
            1
        } else {
            2
        }
    };
    let mut out: Vec<(u16, u8)> = glyphs(row).map(|(col, cell)| (col, class(cell))).collect();
    let end = out.last().map_or(0, |(c, _)| c.saturating_add(1));
    out.push((end, 0));
    out
}

/// A row's cells and their columns, but for the second halves of wide
/// glyphs.
fn glyphs(row: Row<'_>) -> impl Iterator<Item = (u16, CellRef<'_>)> {
    // A row is never wider than a u16 screen.
    (0..=u16::MAX)
        .zip(row.cells())
        .filter(|(_, cell)| !cell.is_wide_continuation())
}

/// A position in the flat sequence of cells: a row and an index into its
/// `classes`.
#[derive(Clone, Copy)]
struct Flat<'a> {
    row: Row<'a>,
    k: usize,
}

/// The classes of the row a walk is on.
struct Walker<'a> {
    row: Row<'a>,
    cells: Vec<(u16, u8)>,
}

impl<'a> Walker<'a> {
    fn new(row: Row<'a>) -> Self {
        Self {
            row,
            cells: classes(row),
        }
    }
    fn load(&mut self, row: Row<'a>) {
        if row.index() != self.row.index() {
            *self = Self::new(row);
        }
    }
    fn at(&mut self, p: Flat<'a>) -> (u16, u8) {
        self.load(p.row);
        self.cells.get(p.k).copied().unwrap_or((0, 0))
    }
    fn len(&mut self, row: Row<'a>) -> usize {
        self.load(row);
        self.cells.len()
    }
    fn next(&mut self, p: Flat<'a>) -> Option<Flat<'a>> {
        let k = p.k.checked_add(1)?;
        if k < self.len(p.row) {
            return Some(Flat { row: p.row, k });
        }
        Some(Flat {
            row: p.row.below()?,
            k: 0,
        })
    }
    fn prev(&mut self, p: Flat<'a>) -> Option<Flat<'a>> {
        if let Some(k) = p.k.checked_sub(1) {
            return Some(Flat { row: p.row, k });
        }
        let row = p.row.above()?;
        let len = self.len(row);
        Some(Flat {
            row,
            k: len.saturating_sub(1),
        })
    }
    fn find(&mut self, col: u16) -> Flat<'a> {
        let k = self
            .cells
            .iter()
            .position(|(c, _)| *c >= col)
            .unwrap_or(self.cells.len().saturating_sub(1));
        Flat { row: self.row, k }
    }
    fn class(&mut self, p: Flat<'a>) -> u8 {
        self.at(p).1
    }
}

/// `w`: the start of the next word.
fn word_forward(row: Row<'_>, col: u16) -> Place<'_> {
    let mut w = Walker::new(row);
    let mut p = w.find(col);
    let start = w.class(p);
    while start != 0
        && w.class(p) == start
        && let Some(n) = w.next(p)
    {
        p = n;
    }
    while w.class(p) == 0
        && let Some(n) = w.next(p)
    {
        p = n;
    }
    (p.row, w.at(p).0)
}

/// `b`: the start of this word, or of the previous one.
fn word_back(row: Row<'_>, col: u16) -> Place<'_> {
    let mut w = Walker::new(row);
    let mut p = w.find(col);
    if let Some(n) = w.prev(p) {
        p = n;
    }
    while w.class(p) == 0
        && let Some(n) = w.prev(p)
    {
        p = n;
    }
    let class = w.class(p);
    while let Some(n) = w.prev(p)
        && w.class(n) == class
    {
        p = n;
    }
    (p.row, w.at(p).0)
}

fn fold(c: char, ignore_case: bool) -> char {
    if !ignore_case {
        c
    } else if c.is_ascii() {
        c.to_ascii_lowercase()
    } else {
        c.to_lowercase().next().unwrap_or(c)
    }
}

/// The next match of `query` from the cursor, literal and smart-case, over
/// the whole history, wrapping around.
pub fn find<'a>(query: &str, from: Place<'a>, seek: Seek) -> Option<Place<'a>> {
    let ignore_case = !query.chars().any(char::is_uppercase);
    let needle: Vec<char> = query.chars().map(|c| fold(c, ignore_case)).collect();
    // An empty needle finds nothing.
    let &first = needle.first()?;
    // One row's folded characters and their columns, and its matches,
    // reused row after row: a search over a long history allocates nothing
    // for a row without a match, and tries the needle only where its first
    // character is.
    let (mut folded, mut cols) = (Vec::new(), Vec::new());
    let mut matches_in = |row: Row<'_>, cols: &mut Vec<u16>| {
        cols.clear();
        folded.clear();
        folded.extend(
            glyphs(row).flat_map(|(_, cell)| shown(cell).chars().map(|c| fold(c, ignore_case))),
        );
        let starts: Vec<usize> = (0..folded.len())
            .filter(|at| {
                folded.get(*at) == Some(&first)
                    && folded
                        .get(*at..)
                        .is_some_and(|rest| rest.starts_with(&needle))
            })
            .collect();
        if starts.is_empty() {
            return;
        }
        // A match: the columns of this row, then, and of each match in it.
        let columns: Vec<u16> = glyphs(row)
            .flat_map(|(col, cell)| shown(cell).chars().map(move |_| col))
            .collect();
        cols.extend(starts.iter().filter_map(|at| columns.get(*at).copied()));
    };
    // Whether a column is past the cursor, the way the search goes.
    let past = |col: u16| match seek {
        Seek::Forward => col > from.1,
        Seek::Backward => col < from.1,
    };
    // Around the rows and back to the cursor's.
    let mut row = from.0;
    let mut first_row = true;
    loop {
        matches_in(row, &mut cols);
        if seek == Seek::Backward {
            cols.reverse();
        }
        // On the cursor's row, what is past it; back at that row after
        // going round, the rest of it.
        let back = !first_row && row.index() == from.0.index();
        let hit = cols
            .iter()
            .copied()
            .find(|col| (!first_row || past(*col)) && (!back || !past(*col)));
        if let Some(col) = hit {
            return Some((row, col));
        }
        if back {
            return None;
        }
        first_row = false;
        row = match seek {
            Seek::Forward => row.below().unwrap_or(row.up(usize::MAX)),
            Seek::Backward => row.above().unwrap_or(row.down(usize::MAX)),
        };
    }
}

/// The nearest row before `from`, or after it, where a prompt starts: one a
/// shell marked with `OSC 133 ; A` (fux-vt's `Row::starts_prompt`).
pub fn prompt(from: Row<'_>, seek: Seek) -> Option<Row<'_>> {
    let step = match seek {
        Seek::Backward => Row::above,
        Seek::Forward => Row::below,
    };
    std::iter::successors(step(&from), step).find(Row::starts_prompt)
}

/// The selected text. Wide glyphs and combining marks stay whole; a
/// soft-wrapped row joins the next without a newline; trailing blanks are
/// trimmed; a block is a rectangle, one line per row.
pub fn text(
    screen: &Screen,
    kind: Select,
    start: Place<'_>,
    end: Place<'_>,
) -> Result<String, Error> {
    let cols = screen.size().cols();
    let mut out = String::new();
    let mut cells = 0usize;
    let (left, right) = (start.1.min(end.1), start.1.max(end.1));
    let (first, last) = (start.0.index(), end.0.index());
    let rows = std::iter::successors(Some(start.0), Row::below);
    for row in rows.take_while(|row| row.index() <= last) {
        let index = row.index();
        let (from, to) = match kind {
            Select::Char => (
                if index == first { start.1 } else { 0 },
                if index == last {
                    end.1
                } else {
                    cols.saturating_sub(1)
                },
            ),
            Select::Line => (0, cols.saturating_sub(1)),
            Select::Block => (left, right),
        };
        let mut line = String::new();
        // Starting on the second half of a wide glyph takes the glyph.
        let row_cells = glyph_start(row, from)..=to;
        for cell in row_cells.map_while(|col| row.cell(usize::from(col))) {
            // Refused long before it could saturate.
            cells = cells.saturating_add(1);
            if cells > MAX_CELLS {
                return Err(Error::SelectionTooLarge);
            }
            if !cell.is_wide_continuation() {
                line.push_str(shown(cell));
            }
        }
        let joined = kind != Select::Block && row.wrapped() && index < last;
        if joined {
            out.push_str(&line);
        } else {
            out.push_str(line.trim_end_matches(' '));
            if index < last {
                out.push('\n');
            }
        }
    }
    Ok(out)
}

// ------------------------------------------------------------------- keys

/// Enters copy mode on the client's focused pane.
pub fn enter(session: &Session, view: &mut View) -> Result<String, Error> {
    let pane = session.focused(view.id).ok_or(Error::NoPaneToCopy)?;
    let screen = session
        .panes
        .get(&pane)
        .ok_or(Error::NoPane(pane))?
        .screen();
    let (cy, cx) = screen.cursor_position();
    let window = screen.window();
    let row = window.row(cy).ok_or(Error::NoRows)?;
    let (cursor, cx) = (row.id(), glyph_start(row, cx));
    let top = window.row(0).ok_or(Error::NoRows)?.id();
    let copy = Copy {
        pane,
        top,
        cursor: (cursor, cx),
        selection: None,
        search: None,
        typing: None,
        held_at: None,
    };
    view.mode = Mode::Copy(Box::new(copy));
    // The bar shows where the cursor is; a notice would hide it.
    view.notice = None;
    Ok(String::new())
}

fn leave(session: &mut Session, client: ClientId) {
    if let Some(view) = session.views.get_mut(&client) {
        view.mode = Mode::Normal;
        view.notice = None;
    }
}

/// A scroll by some rows, toward older output or newer.
#[derive(Clone, Copy)]
enum Scroll {
    Up(usize),
    Down(usize),
}

/// A key in copy mode.
pub fn key(session: &mut Session, client: ClientId, press: KeyPress) {
    let Some(view) = session.views.get(&client) else {
        return;
    };
    let placement = session.placement(view);
    let Some(view) = session.views.get_mut(&client) else {
        return;
    };
    let Mode::Copy(copy) = &mut view.mode else {
        return;
    };
    // The key may move the rows it holds.
    copy.held_at = None;
    let pane_id = copy.pane;
    let Some(pane) = session.panes.get(&pane_id) else {
        return;
    };
    let screen = pane.screen();
    // The rows shown: the pane's rect, but no more than the screen has, as
    // a smaller client can size the pane below this one's room for it.
    let height = placement
        .rect(pane_id)
        .map_or(1, |r| r.h().min(screen.size().rows()).max(1));
    view.dirty = true;
    view.notice = None;

    // A search being typed takes the keys.
    if let Some((seek, text)) = &mut copy.typing {
        if press.key == Key::Escape {
            copy.typing = None;
        } else if press.key == Key::Enter {
            let search = Search {
                query: text.clone(),
                seek: *seek,
            };
            copy.typing = None;
            if !search.query.is_empty() {
                copy.search = Some(search.clone());
                if let Some(at) = copy.resolve(screen) {
                    jump(copy, screen, height, at, &search, &mut view.notice);
                }
            }
        } else if press.key == Key::Backspace {
            text.pop();
        } else if let Key::Char(c) = press.key
            && !press.mods.ctrl
            && !press.mods.alt
            && !c.is_control()
            && text.len() < 1024
        {
            text.push(c);
        }
        return;
    }

    let Some(at) = copy.resolve(screen) else {
        return;
    };
    let ((row, col), mut top) = (at.cursor, at.top);
    let last_col = screen.size().cols().saturating_sub(1);
    let half = usize::from(height / 2).max(1);
    let page = usize::from(height);
    // The columns of a row's glyphs that are not blank.
    let ink = |r: Row<'_>| {
        let classes = classes(r).into_iter();
        classes.filter(|(_, k)| *k != 0).map(|(c, _)| c)
    };
    // Keys are letters, in either case, and the brackets; one with Ctrl or
    // Alt is no key's. The arrows, paging keys, Home, End, Enter and Esc
    // also work.
    let letter = match press.plain_key() {
        Some(Key::Char(c)) => Some(c),
        _ => None,
    };
    let mut target: Option<Place<'_>> = None;
    let mut scroll: Option<Scroll> = None;
    match (letter, press.key) {
        (Some('q'), _) | (_, Key::Escape) => {
            leave(session, client);
            return;
        }
        (Some('h'), _) | (_, Key::Arrow(Direction::Left)) => {
            target = Some((row, col.saturating_sub(1)));
        }
        (Some('l'), _) | (_, Key::Arrow(Direction::Right)) => {
            let wide = row.cell(usize::from(col)).is_some_and(|c| c.is_wide());
            target = Some((
                row,
                col.saturating_add(if wide { 2 } else { 1 }).min(last_col),
            ));
        }
        (Some('j'), _) | (_, Key::Arrow(Direction::Down)) => target = Some((row.down(1), col)),
        (Some('k'), _) | (_, Key::Arrow(Direction::Up)) => target = Some((row.up(1), col)),
        (Some('w'), _) => target = Some(word_forward(row, col)),
        (Some('b'), _) => target = Some(word_back(row, col)),
        (Some('a'), _) => target = Some((row, ink(row).next().unwrap_or(0))),
        (Some('e'), _) | (_, Key::End) => target = Some((row, ink(row).next_back().unwrap_or(0))),
        (_, Key::Home) => target = Some((row, 0)),
        (Some('t'), _) => target = Some((row.up(usize::MAX), 0)),
        (Some('z'), _) => target = Some((row.down(usize::MAX), col)),
        (Some('u'), _) => scroll = Some(Scroll::Up(half)),
        (Some('d'), _) => scroll = Some(Scroll::Down(half)),
        (_, Key::PageUp) => scroll = Some(Scroll::Up(page)),
        (_, Key::PageDown) => scroll = Some(Scroll::Down(page)),
        (Some('f'), _) => copy.typing = Some((Seek::Forward, String::new())),
        (Some('r'), _) => copy.typing = Some((Seek::Backward, String::new())),
        (Some(key @ ('n' | 'p')), _) => {
            match copy.search.clone() {
                Some(search) => {
                    let search = Search {
                        seek: if key == 'n' {
                            search.seek
                        } else {
                            search.seek.reversed()
                        },
                        ..search
                    };
                    jump(copy, screen, height, at, &search, &mut view.notice);
                }
                None => view.error("no search yet: f or r starts one"),
            }
            return;
        }
        (Some(key @ ('[' | ']')), _) => {
            let seek = if key == '[' {
                Seek::Backward
            } else {
                Seek::Forward
            };
            match prompt(row, seek) {
                // The prompt at the top of the view, what came of it below.
                Some(found) => {
                    top = found;
                    target = Some((found, 0));
                }
                None => {
                    let which = if key == '[' { "earlier" } else { "later" };
                    view.error(format!("no {which} prompt (shells mark them with OSC 133)"));
                    return;
                }
            }
        }
        (Some('v'), _) => toggle(copy, Select::Char),
        (Some('s'), _) => toggle(copy, Select::Line),
        (Some('x'), _) => toggle(copy, Select::Block),
        (Some('o'), _) => {
            if let Some((kind, anchor)) = copy.selection {
                copy.selection = Some((kind, copy.cursor));
                copy.cursor = anchor;
                target = at.selection.map(|(_, anchor)| anchor);
            }
        }
        (Some('y'), _) | (_, Key::Enter) => {
            let copied = match at.ends() {
                Some((kind, start, end)) => {
                    text(screen, kind, start, end).map_err(|e| e.to_string())
                }
                None => Err("nothing selected: v, s or x starts a selection".to_owned()),
            };
            yank(session, client, copied);
            return;
        }
        _ => {}
    }
    if let Some(scroll) = scroll {
        let (moved, scrolled) = match scroll {
            Scroll::Up(n) => (row.up(n), top.up(n)),
            Scroll::Down(n) => (row.down(n), top.down(n)),
        };
        top = scrolled;
        target = Some((moved, col));
    }
    if let Some(target) = target {
        move_to(copy, screen, height, top, target);
    }
}

/// `col` of `row`, or the first half of the wide glyph whose second half it
/// is: where copy mode's cursor goes, so that what it highlights is what
/// `y` copies.
fn glyph_start(row: fux_vt::Row<'_>, col: u16) -> u16 {
    if row
        .cell(usize::from(col))
        .is_some_and(|c| c.is_wide_continuation())
    {
        col.saturating_sub(1)
    } else {
        col
    }
}

/// Moves the cursor, scrolling the view, whose top row is `top`, to keep
/// it in sight.
fn move_to(copy: &mut Copy, screen: &Screen, height: u16, top: Row<'_>, (row, col): Place<'_>) {
    let col = col.min(screen.size().cols().saturating_sub(1));
    copy.cursor = (row.id(), glyph_start(row, col));
    let height = usize::from(height);
    // Scroll just enough that the row shows; `height` is at least 1.
    let new_top = if row.index() < top.index() {
        row
    } else if row.index() >= top.index().saturating_add(height) {
        row.up(height.saturating_sub(1))
    } else {
        top
    };
    // The view's top as its window has it: no lower than the screen's.
    if let Some(r) = new_top.window().row(0) {
        copy.top = r.id();
    }
}

fn toggle(copy: &mut Copy, kind: Select) {
    copy.selection = match copy.selection {
        Some((current, _)) if current == kind => None,
        Some((_, anchor)) => Some((kind, anchor)),
        None => Some((kind, copy.cursor)),
    };
}

/// Moves to the next match of `search` from the cursor, as `at` found it;
/// if there is none, the view's notice says so.
fn jump(
    copy: &mut Copy,
    screen: &Screen,
    height: u16,
    at: Resolved<'_>,
    search: &Search,
    notice: &mut Option<Notice>,
) {
    match find(&search.query, at.cursor, search.seek) {
        Some(found) => move_to(copy, screen, height, at.top, found),
        None => {
            let text = format!("not found: {}", search.query);
            *notice = Some(Notice { text, error: true });
        }
    }
}

/// Puts the text `copied` from the selection into the paste buffers, and
/// to the client's clipboard through OSC 52 when allowed; then leaves copy
/// mode. Why nothing was copied, if nothing was, is the client's notice.
fn yank(session: &mut Session, client: ClientId, copied: Result<String, String>) {
    let copied = match copied {
        Ok(copied) => copied,
        Err(why) => return session.error_to(client, why),
    };
    let characters = copied.chars().count();
    let mut note = format!(
        "copied {characters} character{}",
        if characters == 1 { "" } else { "s" }
    );
    // Encoded for the clipboard before the text moves into the buffers.
    if session.config.clipboard {
        let encoded = crate::json::base64(copied.as_bytes());
        if encoded.len() <= MAX_CLIPBOARD {
            session.outbox.push(Outgoing::Bytes(
                client,
                format!("\x1b]52;c;{encoded}\x07").into_bytes(),
            ));
        } else {
            note.push_str(" to buffer 0; too large for the clipboard");
        }
    }
    session.buffers.push_front(copied);
    session.buffers.truncate(session.config.buffers);
    leave(session, client);
    session.info_to(client, note);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(text: &[u8], rows: u16, cols: u16) -> Result<fux_vt::Parser, String> {
        let size = fux_vt::Size::new(rows, cols).map_err(|e| e.to_string())?;
        let mut parser = fux_vt::Parser::new(size, 100).map_err(|e| e.to_string())?;
        parser.process(text).map_err(|e| e.to_string())?;
        Ok(parser)
    }

    /// Row `i`, counted from the oldest.
    fn row(s: &Screen, i: usize) -> Result<Row<'_>, String> {
        s.rows().nth(i).ok_or(format!("no row {i}"))
    }

    /// A place as a row counted from the oldest, and a column.
    fn place((row, col): Place<'_>) -> (usize, u16) {
        (row.index(), col)
    }

    /// `[` and `]` go to the previous and next prompt a shell marked (OSC
    /// 133 ; A), each put at the top of the view, its output below; past
    /// the last one there is none to go to, and the bar says so.
    #[test]
    fn brackets_jump_between_prompts() -> Result<(), Box<dyn std::error::Error>> {
        let (mut s, c) = crate::session::testing::attached(6, 30)?;
        let pane = PaneId::of(1);
        let mut output = String::new();
        for command in ["one", "two", "three"] {
            output.push_str(&format!("\x1b]133;A\x07$ {command}\r\n"));
            for line in 0..4 {
                output.push_str(&format!("{command} {line}\r\n"));
            }
        }
        output.push_str("\x1b]133;A\x07$ ");
        s.output(pane, output.as_bytes());
        let screen = |s: &Session| s.panes.get(&pane).map(|p| p.screen().clone());
        let first = screen(&s).ok_or("the pane")?;
        let starts: Vec<usize> = first
            .rows()
            .filter(|r| r.starts_prompt())
            .map(|r| r.index())
            .collect();
        assert_eq!(starts, [0, 5, 10, 15]);
        let prompt =
            |from, seek| Ok::<_, String>(prompt(row(&first, from)?, seek).map(|r| r.index()));
        assert_eq!(prompt(15, Seek::Backward)?, Some(10));
        assert_eq!(prompt(7, Seek::Forward)?, Some(10));
        assert_eq!(prompt(15, Seek::Forward)?, None);
        assert_eq!(prompt(0, Seek::Backward)?, None);
        // In copy mode, from the cursor on the last prompt.
        s.input(c, b"\x02c");
        let at = |s: &Session| -> Option<(usize, u16, usize)> {
            let view = s.views.get(&c)?;
            let Mode::Copy(copy) = &view.mode else {
                return None;
            };
            let screen = s.panes.get(&pane)?.screen();
            let r = copy.resolve(screen)?;
            Some((r.cursor.0.index(), r.cursor.1, r.top.index()))
        };
        assert_eq!(at(&s).map(|(row, _, _)| row), Some(15));
        s.input(c, b"[");
        assert_eq!(at(&s), Some((10, 0, 10)));
        s.input(c, b"[[");
        assert_eq!(at(&s), Some((0, 0, 0)));
        s.input(c, b"[");
        assert_eq!(at(&s), Some((0, 0, 0)), "none before the first");
        let notice = s
            .views
            .get(&c)
            .and_then(|v| v.notice.as_ref())
            .map(|n| n.text.clone());
        assert_eq!(
            notice.as_deref(),
            Some("no earlier prompt (shells mark them with OSC 133)")
        );
        s.input(c, b"]");
        assert_eq!(at(&s), Some((5, 0, 5)));
        // The last prompt is on the screen: the view goes no lower than it.
        s.input(c, b"]]");
        let history = first.history_len();
        assert_eq!(at(&s), Some((15, 0, history)));
        Ok(())
    }

    /// On a client larger than the pane (a smaller client sizes it), copy
    /// mode moves within the rows shown, not the client's room for them:
    /// its cursor stays on a row the client can see.
    #[test]
    fn copy_mode_moves_within_the_rows_shown() -> Result<(), String> {
        let (mut s, big) = crate::session::testing::attached(16, 40)?;
        s.attach(8, 40, None).map_err(|e| e.to_string())?;
        let pane = PaneId::of(1);
        let mut output = String::new();
        for n in 0..60 {
            output.push_str(&format!("line {n}\r\n"));
        }
        s.output(pane, output.as_bytes());
        s.input(big, b"\x02c");
        s.input(big, &[b'k'; 10]);
        s.input(big, &[b'j'; 14]);
        let screen = s.panes.get(&pane).ok_or("the pane")?.screen();
        let shown = screen.size().rows();
        let view = s.views.get(&big).ok_or("the client")?;
        let Mode::Copy(copy) = &view.mode else {
            return Err("not in copy mode".into());
        };
        let r = copy.resolve(screen).ok_or("a row gone")?;
        assert!(
            r.cursor_in_view(shown).is_some(),
            "the cursor on a row of the {shown} shown"
        );
        Ok(())
    }

    /// Entered with the program's cursor on a wide glyph's second half,
    /// copy mode's cursor is on the glyph's first, as every move puts it:
    /// what it highlights and what `y` copies agree.
    #[test]
    fn copy_mode_entered_on_a_wide_glyphs_second_half_starts_at_its_first() -> Result<(), String> {
        let (mut s, c) = crate::session::testing::attached(5, 20)?;
        s.output(PaneId::of(1), "a\u{754c}b\x1b[1;3H".as_bytes());
        s.input(c, b"\x02c");
        let view = s.views.get(&c).ok_or("the client")?;
        let Mode::Copy(copy) = &view.mode else {
            return Err("not in copy mode".into());
        };
        assert_eq!(copy.cursor.1, 1, "the glyph's first half");
        Ok(())
    }

    #[test]
    fn word_motions_cross_rows() -> Result<(), String> {
        let p = screen(b"foo.bar  baz\r\nqux", 2, 20)?;
        let s = p.screen();
        assert_eq!(place(word_forward(row(s, 0)?, 0)), (0, 3));
        assert_eq!(place(word_forward(row(s, 0)?, 3)), (0, 4));
        assert_eq!(place(word_forward(row(s, 0)?, 9)), (1, 0));
        assert_eq!(place(word_back(row(s, 1)?, 0)), (0, 9));
        Ok(())
    }

    /// Where a search from row `from.0` and column `from.1` lands.
    fn found(
        s: &Screen,
        query: &str,
        from: (usize, u16),
        seek: Seek,
    ) -> Result<Option<(usize, u16)>, String> {
        Ok(find(query, (row(s, from.0)?, from.1), seek).map(place))
    }

    #[test]
    fn search_is_literal_smart_case_and_wraps() -> Result<(), String> {
        let p = screen(b"Alpha beta\r\ngamma BETA\r\nx.y", 3, 20)?;
        let s = p.screen();
        assert_eq!(found(s, "beta", (0, 0), Seek::Forward)?, Some((0, 6)));
        assert_eq!(found(s, "beta", (0, 6), Seek::Forward)?, Some((1, 6)));
        assert_eq!(found(s, "BETA", (0, 0), Seek::Forward)?, Some((1, 6)));
        assert_eq!(
            found(s, "beta", (1, 6), Seek::Forward)?,
            Some((0, 6)),
            "wraps"
        );
        assert_eq!(found(s, "beta", (1, 6), Seek::Backward)?, Some((0, 6)));
        assert_eq!(
            found(s, ".", (0, 0), Seek::Forward)?,
            Some((2, 1)),
            "literal, not a regex"
        );
        assert_eq!(found(s, "zzz", (0, 0), Seek::Forward)?, None);
        Ok(())
    }

    /// Several matches in a row are found in turn, each at its glyph's
    /// column: a wide glyph before a match counts two columns, a combining
    /// mark none, and a match may start with a wide glyph.
    #[test]
    fn search_finds_each_match_in_a_row_at_its_column() -> Result<(), String> {
        let p = screen("ab 界ab e\u{301}ab 界x".as_bytes(), 1, 20)?;
        let s = p.screen();
        assert_eq!(found(s, "ab", (0, 0), Seek::Forward)?, Some((0, 5)));
        assert_eq!(found(s, "ab", (0, 5), Seek::Forward)?, Some((0, 9)));
        assert_eq!(
            found(s, "ab", (0, 9), Seek::Forward)?,
            Some((0, 0)),
            "wraps"
        );
        assert_eq!(found(s, "ab", (0, 9), Seek::Backward)?, Some((0, 5)));
        assert_eq!(found(s, "界x", (0, 0), Seek::Forward)?, Some((0, 12)));
        assert_eq!(found(s, "AB", (0, 0), Seek::Forward)?, None, "smart case");
        Ok(())
    }

    /// A copy larger than `MAX_CELLS` is refused whole, saying so.
    #[test]
    fn too_large_a_selection_is_refused() -> Result<(), Box<dyn std::error::Error>> {
        let lines: Vec<u8> = std::iter::repeat_n(&b"x\r\n"[..], 3000)
            .flatten()
            .copied()
            .collect();
        let mut parser = fux_vt::Parser::new(fux_vt::Size::new(10, 100)?, 3000)?;
        parser.process(&lines)?;
        let s = parser.screen();
        let last = s.rows().next_back().ok_or("no rows")?;
        let copied = text(s, Select::Line, (row(s, 0)?, 0), (last, 0));
        assert!(matches!(copied, Err(Error::SelectionTooLarge)));
        assert_eq!(
            copied.map_err(|e| e.to_string()),
            Err(format!("the selection is larger than {MAX_CELLS} cells"))
        );
        assert!(text(s, Select::Line, (row(s, 0)?, 0), (row(s, 100)?, 0)).is_ok());
        Ok(())
    }

    #[test]
    fn selections_keep_glyphs_whole_join_wraps_and_trim() -> Result<(), Box<dyn std::error::Error>>
    {
        // "abcdef" wraps at 4 columns; then a line with a wide glyph.
        let p = screen("abcdef\r\n界x\r\nlast".as_bytes(), 3, 4)?;
        let s = p.screen();
        let top = s.rows().len() - 4;
        let at = |i| row(s, top + i);
        assert_eq!(text(s, Select::Char, (at(0)?, 0), (at(1)?, 1))?, "abcdef");
        assert_eq!(
            text(s, Select::Line, (at(0)?, 0), (at(2)?, 0))?,
            "abcdef\n界x"
        );
        // Starting on the glyph's second half takes the whole glyph.
        assert_eq!(text(s, Select::Char, (at(2)?, 1), (at(2)?, 2))?, "界x");
        assert_eq!(
            text(s, Select::Block, (at(0)?, 1), (at(3)?, 2))?,
            "bc\nf\n界x\nas"
        );
        Ok(())
    }
}
