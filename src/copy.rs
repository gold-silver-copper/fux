//! Copy/select mode: a keyboard cursor over a pane and its history.
//!
//! For its client the pane holds still: the view and the cursor are
//! anchored to fux-vt row IDs, so new output does not move them, while other
//! clients see the pane live. The mode ends if the pane closes or its history
//! drops the rows it holds.
use crate::command::ClientId;
use crate::keys::{Direction, Key, KeyPress};
use crate::layout::PaneId;
use crate::session::{Outgoing, Session};
use crate::view::Mode;
use fux_vt::{RowId, Screen};

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

/// A selection's kind and its two ends, in order, as (row, column).
type Ends = (Select, (usize, u16), (usize, u16));

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Search {
    pub query: String,
    pub forward: bool,
}

pub struct Copy {
    pub pane: PaneId,
    /// The row at the top of the view.
    pub top: RowId,
    pub cursor: (RowId, u16),
    pub selection: Option<(Select, (RowId, u16))>,
    pub search: Option<Search>,
    /// A search being typed: its direction and text.
    pub typing: Option<(bool, String)>,
}

// ------------------------------------------------------------- positions

/// Rows the screen keeps: history, then the live screen.
pub fn retained(screen: &Screen) -> usize {
    screen.history_len() + usize::from(screen.size().0)
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
    let rows = usize::from(screen.size().0);
    (0..rows).find_map(|i| (screen.row_from_bottom(i)?.id == id).then(|| retained(screen) - 1 - i))
}

impl Copy {
    /// Whether the rows it holds are still there.
    pub fn check(&self, screen: &Screen) -> Result<(), String> {
        let held = [
            Some(self.top),
            Some(self.cursor.0),
            self.selection.map(|(_, (id, _))| id),
        ];
        if held
            .into_iter()
            .flatten()
            .all(|id| index_of(screen, id).is_some())
        {
            Ok(())
        } else {
            Err("copy mode ended: the history dropped the rows it held".into())
        }
    }

    /// The history offset of the view, for `Screen::window`.
    pub fn offset(&self, screen: &Screen) -> usize {
        let top = index_of(screen, self.top).unwrap_or(screen.history_len());
        screen.history_len().saturating_sub(top)
    }

    fn at(&self, screen: &Screen, point: (RowId, u16)) -> Option<(usize, u16)> {
        Some((index_of(screen, point.0)?, point.1))
    }

    /// The cursor, as a row and column of the view, if it is in view.
    pub fn cursor_in_view(&self, screen: &Screen, height: u16) -> Option<(u16, u16)> {
        let top = index_of(screen, self.top)?;
        let (row, col) = self.at(screen, self.cursor)?;
        let y = row.checked_sub(top)?;
        (y < usize::from(height)).then_some((y as u16, col))
    }

    /// The selection's two ends, in order.
    fn ends(&self, screen: &Screen) -> Option<Ends> {
        let (kind, anchor) = self.selection?;
        let a = self.at(screen, anchor)?;
        let b = self.at(screen, self.cursor)?;
        Some((kind, a.min(b), a.max(b)))
    }

    /// Whether the cell at a view row and column is selected.
    pub fn selected(&self, screen: &Screen, view_row: u16, col: u16) -> bool {
        let Some(top) = index_of(screen, self.top) else {
            return false;
        };
        let Some((kind, start, end)) = self.ends(screen) else {
            return false;
        };
        let row = top + usize::from(view_row);
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

    /// The line the bar shows: `COPY` and where the cursor is.
    pub fn status(&self, screen: &Screen) -> String {
        let line = self.at(screen, self.cursor).map_or(0, |(r, _)| r + 1);
        let mode = match self.selection {
            Some((Select::Char, _)) => " select",
            Some((Select::Line, _)) => " lines",
            Some((Select::Block, _)) => " block",
            None => "",
        };
        match &self.typing {
            Some((forward, text)) => format!("{}{text}▏", if *forward { "/" } else { "?" }),
            None => format!("COPY{mode} {line}/{}", retained(screen)),
        }
    }
}

// ------------------------------------------------------------ text classes

/// A row's cells as (column, class): 0 blank, 1 word, 2 other. With `big`,
/// every non-blank is one class. The row end is a blank.
fn classes(screen: &Screen, index: usize, big: bool) -> Vec<(u16, u8)> {
    let mut out = Vec::new();
    if let Some(row) = row_at(screen, index) {
        for (col, cell) in row.cells.iter().enumerate() {
            if cell.is_wide_continuation() {
                continue;
            }
            let c = cell.contents().chars().next().unwrap_or(' ');
            let class = if c.is_whitespace() || !cell.has_contents() {
                0
            } else if big || c.is_alphanumeric() || c == '_' {
                1
            } else {
                2
            };
            out.push((col as u16, class));
        }
    }
    let end = out.last().map_or(0, |(c, _)| c.saturating_add(1));
    out.push((end, 0));
    out
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
    big: bool,
    row: usize,
    cells: Vec<(u16, u8)>,
}

impl<'a> Walker<'a> {
    fn new(screen: &'a Screen, row: usize, big: bool) -> Self {
        Self {
            screen,
            big,
            row,
            cells: classes(screen, row, big),
        }
    }
    fn load(&mut self, row: usize) {
        if row != self.row {
            self.row = row;
            self.cells = classes(self.screen, row, self.big);
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
        if p.k + 1 < self.len(p.row) {
            Some(Flat {
                row: p.row,
                k: p.k + 1,
            })
        } else if p.row + 1 < retained(self.screen) {
            Some(Flat {
                row: p.row + 1,
                k: 0,
            })
        } else {
            None
        }
    }
    fn prev(&mut self, p: Flat) -> Option<Flat> {
        if p.k > 0 {
            Some(Flat {
                row: p.row,
                k: p.k - 1,
            })
        } else if p.row > 0 {
            let len = self.len(p.row - 1);
            Some(Flat {
                row: p.row - 1,
                k: len.saturating_sub(1),
            })
        } else {
            None
        }
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

/// `w`/`W`: the start of the next word.
fn word_forward(screen: &Screen, row: usize, col: u16, big: bool) -> (usize, u16) {
    let mut w = Walker::new(screen, row, big);
    let mut p = w.find(row, col);
    let start = w.class(p);
    while start != 0 && w.class(p) == start {
        match w.next(p) {
            Some(n) => p = n,
            None => return (p.row, w.at(p).0),
        }
    }
    while w.class(p) == 0 {
        match w.next(p) {
            Some(n) => p = n,
            None => break,
        }
    }
    (p.row, w.at(p).0)
}

/// `e`/`E`: the end of this word, or of the next one.
fn word_end(screen: &Screen, row: usize, col: u16, big: bool) -> (usize, u16) {
    let mut w = Walker::new(screen, row, big);
    let mut p = w.find(row, col);
    if let Some(n) = w.next(p) {
        p = n;
    }
    while w.class(p) == 0 {
        match w.next(p) {
            Some(n) => p = n,
            None => return (p.row, w.at(p).0),
        }
    }
    let class = w.class(p);
    while let Some(n) = w.next(p) {
        if w.class(n) != class {
            break;
        }
        p = n;
    }
    (p.row, w.at(p).0)
}

/// `b`/`B`: the start of this word, or of the previous one.
fn word_back(screen: &Screen, row: usize, col: u16, big: bool) -> (usize, u16) {
    let mut w = Walker::new(screen, row, big);
    let mut p = w.find(row, col);
    match w.prev(p) {
        Some(n) => p = n,
        None => return (p.row, w.at(p).0),
    }
    while w.class(p) == 0 {
        match w.prev(p) {
            Some(n) => p = n,
            None => return (p.row, w.at(p).0),
        }
    }
    let class = w.class(p);
    while let Some(n) = w.prev(p) {
        if w.class(n) != class {
            break;
        }
        p = n;
    }
    (p.row, w.at(p).0)
}

/// A row's characters and the column of each.
fn row_chars(screen: &Screen, index: usize) -> Vec<(char, u16)> {
    let mut out = Vec::new();
    if let Some(row) = row_at(screen, index) {
        for (col, cell) in row.cells.iter().enumerate() {
            if cell.is_wide_continuation() {
                continue;
            }
            let text = if cell.has_contents() {
                cell.contents()
            } else {
                " "
            };
            for c in text.chars() {
                out.push((c, col as u16));
            }
        }
    }
    out
}

fn fold(c: char, ignore_case: bool) -> char {
    if ignore_case {
        c.to_lowercase().next().unwrap_or(c)
    } else {
        c
    }
}

/// The next match of `query` from the cursor, literal and smart-case, over
/// the whole history, wrapping around.
pub fn find(
    screen: &Screen,
    query: &str,
    from: (usize, u16),
    forward: bool,
) -> Option<(usize, u16)> {
    let ignore_case = !query.chars().any(char::is_uppercase);
    let needle: Vec<char> = query.chars().map(|c| fold(c, ignore_case)).collect();
    if needle.is_empty() {
        return None;
    }
    let total = retained(screen);
    let matches_in = |index: usize| -> Vec<u16> {
        let chars = row_chars(screen, index);
        let folded: Vec<char> = chars.iter().map(|(c, _)| fold(*c, ignore_case)).collect();
        let mut cols = Vec::new();
        if folded.len() >= needle.len() {
            for start in 0..=folded.len() - needle.len() {
                if folded.get(start..start + needle.len()) == Some(needle.as_slice())
                    && let Some((_, col)) = chars.get(start)
                {
                    cols.push(*col);
                }
            }
        }
        cols
    };
    for step in 0..=total {
        let index = if forward {
            (from.0 + step) % total.max(1)
        } else {
            (from.0 + total * 2 - step) % total.max(1)
        };
        let cols = matches_in(index);
        let hit = if step == 0 {
            if forward {
                cols.into_iter().find(|c| *c > from.1)
            } else {
                cols.into_iter().rev().find(|c| *c < from.1)
            }
        } else if step == total {
            // Back at the start row: the part before (or after) the cursor.
            if forward {
                cols.into_iter().find(|c| *c <= from.1)
            } else {
                cols.into_iter().rev().find(|c| *c >= from.1)
            }
        } else if forward {
            cols.into_iter().next()
        } else {
            cols.into_iter().next_back()
        };
        if let Some(col) = hit {
            return Some((index, col));
        }
    }
    None
}

/// The selected text. Wide glyphs and combining marks stay whole; a
/// soft-wrapped row joins the next without a newline; trailing blanks are
/// trimmed; a block is a rectangle, one line per row.
pub fn text(
    screen: &Screen,
    kind: Select,
    start: (usize, u16),
    end: (usize, u16),
) -> Result<String, String> {
    let cols = screen.size().1;
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
        let mut col = from;
        // Starting on the second half of a wide glyph takes the glyph.
        if row
            .cells
            .get(usize::from(col))
            .is_some_and(|c| c.is_wide_continuation())
        {
            col = col.saturating_sub(1);
        }
        while col <= to {
            let Some(cell) = row.cells.get(usize::from(col)) else {
                break;
            };
            cells += 1;
            if cells > MAX_CELLS {
                return Err(format!("the selection is larger than {MAX_CELLS} cells"));
            }
            if !cell.is_wide_continuation() {
                line.push_str(if cell.has_contents() {
                    cell.contents()
                } else {
                    " "
                });
            }
            col = col.saturating_add(1);
            if col == u16::MAX {
                break;
            }
        }
        let joined = kind != Select::Block && row.wrapped && index < end.0;
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
pub fn enter(session: &mut Session, client: ClientId) -> Result<String, String> {
    let view = session.views.get(&client).ok_or("no such client")?;
    let pane = view.focus().ok_or("no pane to copy from")?;
    let screen = session.panes.get(&pane).ok_or("no such pane")?.screen();
    let (cy, cx) = screen.cursor_position();
    let history = screen.history_len();
    let cursor = row_at(screen, history + usize::from(cy))
        .ok_or("the pane has no rows")?
        .id;
    let top = row_at(screen, history).ok_or("the pane has no rows")?.id;
    let copy = Copy {
        pane,
        top,
        cursor: (cursor, cx),
        selection: None,
        search: None,
        typing: None,
    };
    let view = session.views.get_mut(&client).ok_or("no such client")?;
    view.mode = Mode::Copy(Box::new(copy));
    // The bar shows COPY and the cursor's line; a notice would hide it.
    view.notice = None;
    Ok(String::new())
}

fn leave(session: &mut Session, client: ClientId) {
    if let Some(view) = session.views.get_mut(&client) {
        view.mode = Mode::Normal;
        view.notice = None;
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
    let pane_id = copy.pane;
    let height = placement.rect(pane_id).map_or(1, |r| r.h.max(1));
    let Some(pane) = session.panes.get(&pane_id) else {
        return;
    };
    let screen = pane.screen();
    view.dirty = true;
    view.notice = None;

    // A search being typed takes the keys.
    if let Some((forward, text)) = &mut copy.typing {
        match press.key {
            Key::Escape => copy.typing = None,
            Key::Enter => {
                let search = Search {
                    query: text.clone(),
                    forward: *forward,
                };
                copy.typing = None;
                if !search.query.is_empty() {
                    copy.search = Some(search.clone());
                    jump(
                        copy,
                        screen,
                        height,
                        &search,
                        true,
                        view_error(&mut view.notice),
                    );
                }
            }
            Key::Backspace => {
                text.pop();
            }
            Key::Char(c)
                if !press.mods.ctrl && !press.mods.alt && !c.is_control() && text.len() < 1024 =>
            {
                text.push(c);
            }
            Key::Char(_)
            | Key::Tab
            | Key::Delete
            | Key::Insert
            | Key::Arrow(_)
            | Key::Home
            | Key::End
            | Key::PageUp
            | Key::PageDown
            | Key::F(_) => {}
        }
        return;
    }

    let Some(cursor) = copy.at(screen, copy.cursor) else {
        return;
    };
    let Some(top) = index_of(screen, copy.top) else {
        return;
    };
    let (row, col) = cursor;
    let last_row = retained(screen).saturating_sub(1);
    let last_col = screen.size().1.saturating_sub(1);
    let half = usize::from(height / 2).max(1);
    let page = usize::from(height).max(1);
    let line_start = |r: usize| {
        classes(screen, r, true)
            .iter()
            .find(|(_, k)| *k != 0)
            .map_or(0, |(c, _)| *c)
    };
    let line_end = |r: usize| {
        classes(screen, r, true)
            .iter()
            .rev()
            .find(|(_, k)| *k != 0)
            .map_or(0, |(c, _)| *c)
    };
    let ctrl = press.mods.ctrl && !press.mods.alt;
    let mut target: Option<(usize, u16)> = None;
    let mut scroll: Option<isize> = None;
    match (press.key, ctrl) {
        (Key::Char('q'), false) | (Key::Escape, _) => {
            leave(session, client);
            return;
        }
        (Key::Char('h'), false) | (Key::Arrow(Direction::Left), _) => {
            target = Some((row, col.saturating_sub(1)));
        }
        (Key::Char('l'), false) | (Key::Arrow(Direction::Right), _) => {
            let wide = row_at(screen, row)
                .and_then(|r| r.cells.get(usize::from(col)).copied())
                .is_some_and(|c| c.is_wide());
            target = Some((row, (col + if wide { 2 } else { 1 }).min(last_col)));
        }
        (Key::Char('j'), false) | (Key::Arrow(Direction::Down), _) => {
            target = Some(((row + 1).min(last_row), col))
        }
        (Key::Char('k'), false) | (Key::Arrow(Direction::Up), _) => {
            target = Some((row.saturating_sub(1), col))
        }
        (Key::Char('w'), false) => target = Some(word_forward(screen, row, col, false)),
        (Key::Char('W'), false) => target = Some(word_forward(screen, row, col, true)),
        (Key::Char('b'), false) => target = Some(word_back(screen, row, col, false)),
        (Key::Char('B'), false) => target = Some(word_back(screen, row, col, true)),
        (Key::Char('e'), false) => target = Some(word_end(screen, row, col, false)),
        (Key::Char('E'), false) => target = Some(word_end(screen, row, col, true)),
        (Key::Char('0'), false) | (Key::Home, _) => target = Some((row, 0)),
        (Key::Char('^'), false) => target = Some((row, line_start(row))),
        (Key::Char('$'), false) | (Key::End, _) => target = Some((row, line_end(row))),
        (Key::Char('H'), false) => target = Some((top, col)),
        (Key::Char('M'), false) => {
            target = Some(((top + usize::from(height) / 2).min(last_row), col))
        }
        (Key::Char('L'), false) => {
            target = Some(((top + usize::from(height) - 1).min(last_row), col))
        }
        (Key::Char('g'), false) => target = Some((0, 0)),
        (Key::Char('G'), false) => target = Some((last_row, col)),
        (Key::Char('u'), true) => scroll = Some(-(half as isize)),
        (Key::Char('d'), true) => scroll = Some(half as isize),
        (Key::Char('b'), true) | (Key::PageUp, _) => scroll = Some(-(page as isize)),
        (Key::Char('f'), true) | (Key::PageDown, _) => scroll = Some(page as isize),
        (Key::Char('/'), false) => copy.typing = Some((true, String::new())),
        (Key::Char('?'), false) => copy.typing = Some((false, String::new())),
        (Key::Char('n'), false) | (Key::Char('N'), false) => {
            match copy.search.clone() {
                Some(search) => {
                    let same = press.key == Key::Char('n');
                    let search = Search {
                        forward: if same {
                            search.forward
                        } else {
                            !search.forward
                        },
                        ..search
                    };
                    jump(
                        copy,
                        screen,
                        height,
                        &search,
                        false,
                        view_error(&mut view.notice),
                    );
                }
                None => view.error("no search yet: / or ? starts one"),
            }
            return;
        }
        (Key::Char('v'), false) => toggle(copy, Select::Char),
        (Key::Char('V'), false) => toggle(copy, Select::Line),
        (Key::Char('v'), true) => toggle(copy, Select::Block),
        (Key::Char('o'), false) => {
            if let Some((kind, anchor)) = copy.selection {
                copy.selection = Some((kind, copy.cursor));
                copy.cursor = anchor;
                if let Some(at) = copy.at(screen, anchor) {
                    target = Some(at);
                }
            }
        }
        (Key::Char('y'), false) | (Key::Enter, _) => {
            yank(session, client);
            return;
        }
        _ => {}
    }
    if let Some(delta) = scroll {
        let row = (row as isize + delta).clamp(0, last_row as isize) as usize;
        let top = (top as isize + delta).clamp(0, screen.history_len() as isize) as usize;
        if let Some(r) = row_at(screen, top) {
            copy.top = r.id;
        }
        target = Some((row, col));
    }
    if let Some(target) = target {
        move_to(copy, screen, height, target);
    }
}

/// Moves the cursor, scrolling the view to keep it in sight.
fn move_to(copy: &mut Copy, screen: &Screen, height: u16, (row, col): (usize, u16)) {
    let last_col = screen.size().1.saturating_sub(1);
    let mut col = col.min(last_col);
    if let Some(r) = row_at(screen, row) {
        if r.cells
            .get(usize::from(col))
            .is_some_and(|c| c.is_wide_continuation())
        {
            col = col.saturating_sub(1);
        }
        copy.cursor = (r.id, col);
    }
    let top = index_of(screen, copy.top).unwrap_or(screen.history_len());
    let height = usize::from(height).max(1);
    let new_top = if row < top {
        row
    } else if row >= top + height {
        row + 1 - height
    } else {
        top
    };
    if let Some(r) = row_at(screen, new_top.min(screen.history_len())) {
        copy.top = r.id;
    }
}

fn toggle(copy: &mut Copy, kind: Select) {
    copy.selection = match copy.selection {
        Some((current, _)) if current == kind => None,
        Some((_, anchor)) => Some((kind, anchor)),
        None => Some((kind, copy.cursor)),
    };
}

/// Where an error from a jump goes: the view's notice.
fn view_error(notice: &mut Option<crate::view::Notice>) -> impl FnMut(String) + '_ {
    move |text| {
        *notice = Some(crate::view::Notice { text, error: true });
    }
}

fn jump(
    copy: &mut Copy,
    screen: &Screen,
    height: u16,
    search: &Search,
    _first: bool,
    mut error: impl FnMut(String),
) {
    let Some(from) = copy.at(screen, copy.cursor) else {
        return;
    };
    match find(screen, &search.query, from, search.forward) {
        Some(at) => move_to(copy, screen, height, at),
        None => error(format!("not found: {}", search.query)),
    }
}

/// Copies the selection into the paste buffers, and to the client's
/// clipboard through OSC 52 when allowed; then leaves copy mode.
fn yank(session: &mut Session, client: ClientId) {
    let Some(view) = session.views.get(&client) else {
        return;
    };
    let Mode::Copy(copy) = &view.mode else { return };
    let Some(pane) = session.panes.get(&copy.pane) else {
        return;
    };
    let screen = pane.screen();
    let Some((kind, start, end)) = copy.ends(screen) else {
        if let Some(view) = session.views.get_mut(&client) {
            view.error("nothing selected: v, V or C-v starts a selection");
        }
        return;
    };
    let copied = match text(screen, kind, start, end) {
        Ok(copied) => copied,
        Err(error) => {
            if let Some(view) = session.views.get_mut(&client) {
                view.error(error);
            }
            return;
        }
    };
    let characters = copied.chars().count();
    session.buffers.push_front(copied.clone());
    session.buffers.truncate(session.config.buffers);
    let mut note = format!(
        "copied {characters} character{}",
        if characters == 1 { "" } else { "s" }
    );
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
    leave(session, client);
    if let Some(view) = session.views.get_mut(&client) {
        view.info(note);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(text: &[u8], rows: u16, cols: u16) -> Result<fux_vt::Parser, String> {
        let mut parser = fux_vt::Parser::new(rows, cols, 100).map_err(|e| e.to_string())?;
        parser.process(text).map_err(|e| e.to_string())?;
        Ok(parser)
    }

    #[test]
    fn rows_are_addressed_from_the_oldest() -> Result<(), String> {
        let p = screen(b"one\r\ntwo\r\nthree\r\nfour", 2, 10)?;
        let s = p.screen();
        assert_eq!(retained(s), 4);
        let text = |i| row_at(s, i).map(|r| crate::session::row_text(r.cells));
        assert_eq!(text(0), Some("one".into()));
        assert_eq!(text(3), Some("four".into()));
        for i in 0..4 {
            let id = row_at(s, i).map(|r| r.id);
            assert_eq!(id.and_then(|id| index_of(s, id)), Some(i));
        }
        Ok(())
    }

    #[test]
    fn word_motions_cross_rows() -> Result<(), String> {
        let p = screen(b"foo.bar  baz\r\nqux", 2, 20)?;
        let s = p.screen();
        assert_eq!(word_forward(s, 0, 0, false), (0, 3));
        assert_eq!(word_forward(s, 0, 3, false), (0, 4));
        assert_eq!(word_forward(s, 0, 0, true), (0, 9));
        assert_eq!(word_forward(s, 0, 9, false), (1, 0));
        assert_eq!(word_end(s, 0, 0, false), (0, 2));
        assert_eq!(word_end(s, 0, 0, true), (0, 6));
        assert_eq!(word_back(s, 1, 0, false), (0, 9));
        assert_eq!(word_back(s, 0, 9, true), (0, 0));
        Ok(())
    }

    #[test]
    fn search_is_literal_smart_case_and_wraps() -> Result<(), String> {
        let p = screen(b"Alpha beta\r\ngamma BETA\r\nx.y", 3, 20)?;
        let s = p.screen();
        assert_eq!(find(s, "beta", (0, 0), true), Some((0, 6)));
        assert_eq!(find(s, "beta", (0, 6), true), Some((1, 6)));
        assert_eq!(find(s, "BETA", (0, 0), true), Some((1, 6)));
        assert_eq!(find(s, "beta", (1, 6), true), Some((0, 6)), "wraps");
        assert_eq!(find(s, "beta", (1, 6), false), Some((0, 6)));
        assert_eq!(
            find(s, ".", (0, 0), true),
            Some((2, 1)),
            "literal, not a regex"
        );
        assert_eq!(find(s, "zzz", (0, 0), true), None);
        Ok(())
    }

    #[test]
    fn selections_keep_glyphs_whole_join_wraps_and_trim() -> Result<(), String> {
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
