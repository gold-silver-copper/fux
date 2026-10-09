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
use fux_vt::{CellRef, RowId, Screen};
use std::num::NonZeroUsize;

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

/// A selection's two ends, in order, as (row, column).
type Ends = ((usize, u16), (usize, u16));

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

/// Rows the screen keeps: history, then the live screen.
pub fn retained(screen: &Screen) -> usize {
    // Exact: fux-vt bounds the rows it retains far below a usize.
    screen
        .history_len()
        .saturating_add(usize::from(screen.size().rows()))
}

/// The row at `index`, counted from the oldest.
pub fn row_at(screen: &Screen, index: usize) -> Option<fux_vt::Row<'_>> {
    let last = retained(screen).checked_sub(1)?;
    screen.row_from_bottom(last.checked_sub(index)?)
}

/// Where a row is, counted from the oldest.
pub fn index_of(screen: &Screen, id: RowId) -> Option<usize> {
    if let Some(offset) = screen.offset_for_row(id) {
        return screen.history_len().checked_sub(offset);
    }
    let rows = usize::from(screen.size().rows());
    (0..rows).find_map(|i| {
        if screen.row_from_bottom(i)?.id() != id {
            return None;
        }
        retained(screen).checked_sub(1)?.checked_sub(i)
    })
}

/// Copy mode's positions as retained-row indexes, each row found once for
/// the paint or key that needs them rather than at every use: all of them,
/// as copy mode ends when any of its rows is gone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Resolved {
    /// The row at the top of the view.
    pub top: usize,
    pub cursor: (usize, u16),
    /// The selection: its kind, its anchor, and its two ends in order.
    pub selection: Option<(Select, (usize, u16), Ends)>,
}

impl Resolved {
    /// The history offset of the view, for `Screen::window`.
    pub fn offset(&self, screen: &Screen) -> usize {
        screen.history_len().saturating_sub(self.top)
    }

    /// The cursor, as a row and column of the view, if it is in view.
    pub fn cursor_in_view(&self, height: u16) -> Option<(u16, u16)> {
        let (row, col) = self.cursor;
        let y = u16::try_from(row.checked_sub(self.top)?).ok()?;
        (y < height).then_some((y, col))
    }

    /// Whether the cell at a view row and column is selected.
    pub fn selected(&self, view_row: u16, col: u16) -> bool {
        let Some((kind, _, (start, end))) = self.selection else {
            return false;
        };
        let Some(row) = self.top.checked_add(usize::from(view_row)) else {
            return false;
        };
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
    pub fn resolve(&self, screen: &Screen) -> Option<Resolved> {
        let at = |(id, col): (RowId, u16)| Some((index_of(screen, id)?, col));
        // The view's top row, as the window can show it: no lower than
        // the end of history, which rows pulled back out of it, as a pane
        // grows, can leave behind it. The cursor and the selection are
        // placed from this top, as the window is.
        let top = index_of(screen, self.top)?.min(screen.history_len());
        let cursor = at(self.cursor)?;
        let selection = match self.selection {
            Some((kind, anchor)) => {
                let anchor = at(anchor)?;
                Some((kind, anchor, (anchor.min(cursor), anchor.max(cursor))))
            }
            None => None,
        };
        Some(Resolved {
            top,
            cursor,
            selection,
        })
    }

    /// What the bar shows in place of the tabs, with the cursor where `at`
    /// found it.
    pub fn bar(&self, screen: &Screen, at: &Resolved) -> Bar {
        // Counted from 1; exact, as rows are far fewer than a usize holds.
        let line = at.cursor.0.saturating_add(1);
        let position = format!("{line}/{}", retained(screen));
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
fn classes(screen: &Screen, index: usize) -> Vec<(u16, u8)> {
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
    let mut out: Vec<(u16, u8)> = glyphs(screen, index)
        .map(|(col, cell)| (col, class(cell)))
        .collect();
    let end = out.last().map_or(0, |(c, _)| c.saturating_add(1));
    out.push((end, 0));
    out
}

/// A row's cells and their columns, but for the second halves of wide
/// glyphs.
fn glyphs(screen: &Screen, index: usize) -> impl Iterator<Item = (u16, CellRef<'_>)> {
    let cells = row_at(screen, index)
        .into_iter()
        .flat_map(|row| row.cells());
    // A row is never wider than a u16 screen.
    (0..=u16::MAX)
        .zip(cells)
        .filter(|(_, cell)| !cell.is_wide_continuation())
}

/// A position in the flat sequence of cells: a row and an index into its
/// `classes`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Flat {
    row: usize,
    k: usize,
}

struct Walker<'a> {
    screen: &'a Screen,
    row: usize,
    cells: Vec<(u16, u8)>,
}

impl<'a> Walker<'a> {
    fn new(screen: &'a Screen, row: usize) -> Self {
        Self {
            screen,
            row,
            cells: classes(screen, row),
        }
    }
    fn load(&mut self, row: usize) {
        if row != self.row {
            self.row = row;
            self.cells = classes(self.screen, row);
        }
    }
    fn at(&mut self, p: Flat) -> (u16, u8) {
        self.load(p.row);
        self.cells.get(p.k).copied().unwrap_or((0, 0))
    }
    fn len(&mut self, row: usize) -> usize {
        self.load(row);
        self.cells.len()
    }
    fn next(&mut self, p: Flat) -> Option<Flat> {
        let k = p.k.checked_add(1)?;
        if k < self.len(p.row) {
            return Some(Flat { row: p.row, k });
        }
        let row = p.row.checked_add(1)?;
        (row < retained(self.screen)).then_some(Flat { row, k: 0 })
    }
    fn prev(&mut self, p: Flat) -> Option<Flat> {
        if let Some(k) = p.k.checked_sub(1) {
            return Some(Flat { row: p.row, k });
        }
        let row = p.row.checked_sub(1)?;
        let len = self.len(row);
        Some(Flat {
            row,
            k: len.saturating_sub(1),
        })
    }
    fn find(&mut self, row: usize, col: u16) -> Flat {
        self.load(row);
        let k = self
            .cells
            .iter()
            .position(|(c, _)| *c >= col)
            .unwrap_or(self.cells.len().saturating_sub(1));
        Flat { row, k }
    }
    fn class(&mut self, p: Flat) -> u8 {
        self.at(p).1
    }
}

/// `w`: the start of the next word.
fn word_forward(screen: &Screen, row: usize, col: u16) -> (usize, u16) {
    let mut w = Walker::new(screen, row);
    let mut p = w.find(row, col);
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
fn word_back(screen: &Screen, row: usize, col: u16) -> (usize, u16) {
    let mut w = Walker::new(screen, row);
    let mut p = w.find(row, col);
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
pub fn find(screen: &Screen, query: &str, from: (usize, u16), seek: Seek) -> Option<(usize, u16)> {
    let ignore_case = !query.chars().any(char::is_uppercase);
    let needle: Vec<char> = query.chars().map(|c| fold(c, ignore_case)).collect();
    // An empty needle finds nothing.
    let &first = needle.first()?;
    let total = retained(screen);
    // One row's folded characters and their columns, and its matches,
    // reused row after row: a search over a long history allocates nothing
    // for a row without a match, and tries the needle only where its first
    // character is.
    let (mut folded, mut cols) = (Vec::new(), Vec::new());
    let mut matches_in = |index: usize, cols: &mut Vec<u16>| {
        cols.clear();
        folded.clear();
        folded.extend(
            glyphs(screen, index)
                .flat_map(|(_, cell)| shown(cell).chars().map(|c| fold(c, ignore_case))),
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
        let columns: Vec<u16> = glyphs(screen, index)
            .flat_map(|(col, cell)| shown(cell).chars().map(move |_| col))
            .collect();
        cols.extend(starts.iter().filter_map(|at| columns.get(*at).copied()));
    };
    // Whether a column is past the cursor, the way the search goes.
    let past = |col: u16| match seek {
        Seek::Forward => col > from.1,
        Seek::Backward => col < from.1,
    };
    // Around the rows and back to the start. Exact: row counts are far
    // below a usize, and `rows` is at least 1.
    let rows = NonZeroUsize::new(total).unwrap_or(NonZeroUsize::MIN);
    for step in 0..=total {
        let index = match seek {
            Seek::Forward => from.0.saturating_add(step) % rows,
            Seek::Backward => {
                from.0
                    .saturating_add(total.saturating_mul(2))
                    .saturating_sub(step)
                    % rows
            }
        };
        matches_in(index, &mut cols);
        if seek == Seek::Backward {
            cols.reverse();
        }
        // On the cursor's row, what is past it; back at that row after
        // going round, the rest of it.
        let hit = cols
            .iter()
            .copied()
            .find(|col| (step != 0 || past(*col)) && (step != total || !past(*col)));
        if let Some(col) = hit {
            return Some((index, col));
        }
    }
    None
}

/// The nearest row before `from`, or after it, where a prompt starts: one a
/// shell marked with `OSC 133 ; A` (fux-vt's `Row::starts_prompt`).
pub fn prompt(screen: &Screen, from: usize, seek: Seek) -> Option<usize> {
    let starts = |index: &usize| row_at(screen, *index).is_some_and(|r| r.starts_prompt());
    match seek {
        Seek::Backward => (0..from).rev().find(starts),
        Seek::Forward => (from.saturating_add(1)..retained(screen)).find(starts),
    }
}

/// The selected text. Wide glyphs and combining marks stay whole; a
/// soft-wrapped row joins the next without a newline; trailing blanks are
/// trimmed; a block is a rectangle, one line per row.
pub fn text(
    screen: &Screen,
    kind: Select,
    start: (usize, u16),
    end: (usize, u16),
) -> Result<String, Error> {
    let cols = screen.size().cols();
    let mut out = String::new();
    let mut cells = 0usize;
    let (left, right) = (start.1.min(end.1), start.1.max(end.1));
    for index in start.0..=end.0 {
        let Some(row) = row_at(screen, index) else {
            continue;
        };
        let (from, to) = match kind {
            Select::Char => (
                if index == start.0 { start.1 } else { 0 },
                if index == end.0 {
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
        let joined = kind != Select::Block && row.wrapped() && index < end.0;
        if joined {
            out.push_str(&line);
        } else {
            out.push_str(line.trim_end_matches(' '));
            if index < end.0 {
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
    let history = screen.history_len();
    let row = history
        .checked_add(usize::from(cy))
        .and_then(|i| row_at(screen, i))
        .ok_or(Error::NoRows)?;
    let (cursor, cx) = (row.id(), glyph_start(row, cx));
    let top = row_at(screen, history).ok_or(Error::NoRows)?.id();
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

impl Scroll {
    /// Where a row index lands, stopping at 0 and at `last`.
    fn from(self, at: usize, last: usize) -> usize {
        match self {
            Scroll::Up(n) => at.saturating_sub(n),
            Scroll::Down(n) => at.saturating_add(n),
        }
        .min(last)
    }
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
                    jump(copy, screen, height, &at, &search, &mut view.notice);
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
    let last_row = retained(screen).saturating_sub(1);
    let last_col = screen.size().cols().saturating_sub(1);
    let half = usize::from(height / 2).max(1);
    let page = usize::from(height);
    // The columns of a row's glyphs that are not blank.
    let ink = |r: usize| {
        let classes = classes(screen, r).into_iter();
        classes.filter(|(_, k)| *k != 0).map(|(c, _)| c)
    };
    // Keys are letters, in either case, and the brackets; one with Ctrl or
    // Alt is no key's. The arrows, paging keys, Home, End, Enter and Esc
    // also work.
    let letter = match press.plain_key() {
        Some(Key::Char(c)) => Some(c),
        _ => None,
    };
    let mut target: Option<(usize, u16)> = None;
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
            let wide = row_at(screen, row)
                .and_then(|r| r.cell(usize::from(col)))
                .is_some_and(|c| c.is_wide());
            target = Some((
                row,
                col.saturating_add(if wide { 2 } else { 1 }).min(last_col),
            ));
        }
        (Some('j'), _) | (_, Key::Arrow(Direction::Down)) => {
            target = Some((row.saturating_add(1).min(last_row), col))
        }
        (Some('k'), _) | (_, Key::Arrow(Direction::Up)) => {
            target = Some((row.saturating_sub(1), col))
        }
        (Some('w'), _) => target = Some(word_forward(screen, row, col)),
        (Some('b'), _) => target = Some(word_back(screen, row, col)),
        (Some('a'), _) => target = Some((row, ink(row).next().unwrap_or(0))),
        (Some('e'), _) | (_, Key::End) => target = Some((row, ink(row).next_back().unwrap_or(0))),
        (_, Key::Home) => target = Some((row, 0)),
        (Some('t'), _) => target = Some((0, 0)),
        (Some('z'), _) => target = Some((last_row, col)),
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
                    jump(copy, screen, height, &at, &search, &mut view.notice);
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
            match prompt(screen, row, seek) {
                // The prompt at the top of the view, what came of it below.
                Some(found) => {
                    top = found.min(screen.history_len());
                    if let Some(r) = row_at(screen, top) {
                        copy.top = r.id();
                    }
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
                target = at.selection.map(|(_, anchor, _)| anchor);
            }
        }
        (Some('y'), _) | (_, Key::Enter) => {
            let copied = match at.selection {
                Some((kind, _, (start, end))) => {
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
        let row = scroll.from(row, last_row);
        let scrolled = scroll.from(top, screen.history_len());
        if let Some(r) = row_at(screen, scrolled) {
            copy.top = r.id();
            top = scrolled;
        }
        target = Some((row, col));
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

/// Moves the cursor, scrolling the view, whose top row is at `top`, to keep
/// it in sight.
fn move_to(copy: &mut Copy, screen: &Screen, height: u16, top: usize, (row, col): (usize, u16)) {
    let last_col = screen.size().cols().saturating_sub(1);
    let col = col.min(last_col);
    if let Some(r) = row_at(screen, row) {
        copy.cursor = (r.id(), glyph_start(r, col));
    }
    let height = usize::from(height);
    // Scroll just enough that the row shows; `height` is at least 1.
    let new_top = if row < top {
        row
    } else if row >= top.saturating_add(height) {
        row.saturating_add(1).saturating_sub(height)
    } else {
        top
    };
    if let Some(r) = row_at(screen, new_top.min(screen.history_len())) {
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
    at: &Resolved,
    search: &Search,
    notice: &mut Option<Notice>,
) {
    match find(screen, &search.query, at.cursor, search.seek) {
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

    #[test]
    fn a_scroll_stops_at_both_ends() {
        assert_eq!(Scroll::Up(3).from(5, 10), 2);
        assert_eq!(Scroll::Up(9).from(5, 10), 0);
        assert_eq!(Scroll::Down(3).from(5, 10), 8);
        assert_eq!(Scroll::Down(9).from(5, 10), 10);
        assert_eq!(Scroll::Down(usize::MAX).from(5, 10), 10);
        // A row past the end, as after output shrank history, comes back.
        assert_eq!(Scroll::Up(1).from(20, 10), 10);
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
        let starts: Vec<usize> = (0..retained(&first))
            .filter(|i| row_at(&first, *i).is_some_and(|r| r.starts_prompt()))
            .collect();
        assert_eq!(starts, [0, 5, 10, 15]);
        assert_eq!(prompt(&first, 15, Seek::Backward), Some(10));
        assert_eq!(prompt(&first, 7, Seek::Forward), Some(10));
        assert_eq!(prompt(&first, 15, Seek::Forward), None);
        assert_eq!(prompt(&first, 0, Seek::Backward), None);
        // In copy mode, from the cursor on the last prompt.
        s.input(c, b"\x02c");
        let at = |s: &Session| -> Option<(usize, u16, usize)> {
            let view = s.views.get(&c)?;
            let Mode::Copy(copy) = &view.mode else {
                return None;
            };
            let screen = s.panes.get(&pane)?.screen();
            let r = copy.resolve(screen)?;
            Some((r.cursor.0, r.cursor.1, r.top))
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

    /// After the pane grows, pulling rows back out of history, the view's
    /// top is past what history holds: the cursor and the selection are
    /// drawn on the rows the window shows from there, the ones `y` copies.
    #[test]
    fn the_view_after_rows_leave_history_shows_the_cursor_where_it_is() -> Result<(), String> {
        let mut text = Vec::new();
        for n in 0..60 {
            text.extend_from_slice(format!("line {n}\r\n").as_bytes());
        }
        let mut p = screen(&text, 11, 20)?;
        let history = p.screen().history_len();
        let id = |s: &Screen, i| row_at(s, i).map(|r| r.id()).ok_or("no row");
        let cursor = history.saturating_add(8);
        let copy = Copy {
            pane: PaneId::of(1),
            top: id(p.screen(), history.saturating_sub(2))?,
            cursor: (id(p.screen(), cursor)?, 0),
            selection: Some((Select::Line, (id(p.screen(), cursor)?, 0))),
            search: None,
            typing: None,
            held_at: None,
        };
        let size = fux_vt::Size::new(21, 20).map_err(|e| e.to_string())?;
        p.resize(size).map_err(|e| e.to_string())?;
        let s = p.screen();
        let r = copy.resolve(s).ok_or("a row gone")?;
        let shown_top = s.history_len().saturating_sub(r.offset(s));
        let (y, _) = r.cursor_in_view(21).ok_or("cursor not in view")?;
        let cursor_now = index_of(s, copy.cursor.0).ok_or("cursor gone")?;
        assert_eq!(shown_top.saturating_add(usize::from(y)), cursor_now);
        assert!(
            r.selected(y, 0),
            "the selection is drawn on the cursor's row"
        );
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
    fn rows_are_addressed_from_the_oldest() -> Result<(), String> {
        let p = screen(b"one\r\ntwo\r\nthree\r\nfour", 2, 10)?;
        let s = p.screen();
        assert_eq!(retained(s), 4);
        let text = |i| row_at(s, i).map(crate::session::row_text);
        assert_eq!(text(0), Some("one".into()));
        assert_eq!(text(3), Some("four".into()));
        for i in 0..4 {
            let id = row_at(s, i).map(|r| r.id());
            assert_eq!(id.and_then(|id| index_of(s, id)), Some(i));
        }
        Ok(())
    }

    #[test]
    fn word_motions_cross_rows() -> Result<(), String> {
        let p = screen(b"foo.bar  baz\r\nqux", 2, 20)?;
        let s = p.screen();
        assert_eq!(word_forward(s, 0, 0), (0, 3));
        assert_eq!(word_forward(s, 0, 3), (0, 4));
        assert_eq!(word_forward(s, 0, 9), (1, 0));
        assert_eq!(word_back(s, 1, 0), (0, 9));
        Ok(())
    }

    #[test]
    fn search_is_literal_smart_case_and_wraps() -> Result<(), String> {
        let p = screen(b"Alpha beta\r\ngamma BETA\r\nx.y", 3, 20)?;
        let s = p.screen();
        assert_eq!(find(s, "beta", (0, 0), Seek::Forward), Some((0, 6)));
        assert_eq!(find(s, "beta", (0, 6), Seek::Forward), Some((1, 6)));
        assert_eq!(find(s, "BETA", (0, 0), Seek::Forward), Some((1, 6)));
        assert_eq!(
            find(s, "beta", (1, 6), Seek::Forward),
            Some((0, 6)),
            "wraps"
        );
        assert_eq!(find(s, "beta", (1, 6), Seek::Backward), Some((0, 6)));
        assert_eq!(
            find(s, ".", (0, 0), Seek::Forward),
            Some((2, 1)),
            "literal, not a regex"
        );
        assert_eq!(find(s, "zzz", (0, 0), Seek::Forward), None);
        Ok(())
    }

    /// Several matches in a row are found in turn, each at its glyph's
    /// column: a wide glyph before a match counts two columns, a combining
    /// mark none, and a match may start with a wide glyph.
    #[test]
    fn search_finds_each_match_in_a_row_at_its_column() -> Result<(), String> {
        let p = screen("ab 界ab e\u{301}ab 界x".as_bytes(), 1, 20)?;
        let s = p.screen();
        assert_eq!(find(s, "ab", (0, 0), Seek::Forward), Some((0, 5)));
        assert_eq!(find(s, "ab", (0, 5), Seek::Forward), Some((0, 9)));
        assert_eq!(find(s, "ab", (0, 9), Seek::Forward), Some((0, 0)), "wraps");
        assert_eq!(find(s, "ab", (0, 9), Seek::Backward), Some((0, 5)));
        assert_eq!(find(s, "界x", (0, 0), Seek::Forward), Some((0, 12)));
        assert_eq!(find(s, "AB", (0, 0), Seek::Forward), None, "smart case");
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
        let last = retained(s).saturating_sub(1);
        let copied = text(s, Select::Line, (0, 0), (last, 0));
        assert!(matches!(copied, Err(Error::SelectionTooLarge)));
        assert_eq!(
            copied.map_err(|e| e.to_string()),
            Err(format!("the selection is larger than {MAX_CELLS} cells"))
        );
        assert!(text(s, Select::Line, (0, 0), (100, 0)).is_ok());
        Ok(())
    }

    #[test]
    fn selections_keep_glyphs_whole_join_wraps_and_trim() -> Result<(), Box<dyn std::error::Error>>
    {
        // "abcdef" wraps at 4 columns; then a line with a wide glyph.
        let p = screen("abcdef\r\n界x\r\nlast".as_bytes(), 3, 4)?;
        let s = p.screen();
        let rows = retained(s);
        let top = rows - 4;
        assert_eq!(text(s, Select::Char, (top, 0), (top + 1, 1))?, "abcdef");
        assert_eq!(
            text(s, Select::Line, (top, 0), (top + 2, 0))?,
            "abcdef\n界x"
        );
        // Starting on the glyph's second half takes the whole glyph.
        assert_eq!(text(s, Select::Char, (top + 2, 1), (top + 2, 2))?, "界x");
        assert_eq!(
            text(s, Select::Block, (top, 1), (top + 3, 2))?,
            "bc\nf\n界x\nas"
        );
        Ok(())
    }
}
