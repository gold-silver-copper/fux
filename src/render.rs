//! Painting a client's screen: compose a grid of cells from its panes, the
//! separators, the bar and any overlay; diff it against what the client has;
//! send only the changed runs, inside synchronized output.
use crate::command::ClientId;
use crate::keys::KeyPress;
use crate::layout::{PaneId, Rect};
use crate::overlay::{self, ColumnRow};
use crate::session::Session;
use crate::view::{Mode, View};
use fux_vt::{Attributes, Cell, Color};
use std::io::Write;
use unicode_width::UnicodeWidthChar;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Grid {
    pub rows: u16,
    pub cols: u16,
    pub cells: Vec<Cell>,
    /// Where the terminal cursor is shown, if it is.
    pub cursor: Option<(u16, u16)>,
    /// DECSCUSR shape for the cursor; 0 is the terminal's default.
    pub cursor_shape: u16,
}

impl Grid {
    pub fn new(rows: u16, cols: u16) -> Grid {
        Grid {
            rows,
            cols,
            // Exact: a u16 by a u16 fits even a 32-bit usize.
            cells: vec![Cell::default(); usize::from(rows).saturating_mul(usize::from(cols))],
            cursor: None,
            cursor_shape: 0,
        }
    }
    /// Blanks the grid at `rows` by `cols`, resizing its cells only if the
    /// size changed: a grid composed into again allocates nothing.
    fn reset(&mut self, rows: u16, cols: u16) {
        if (self.rows, self.cols) == (rows, cols) {
            self.cells.fill(Cell::default());
        } else {
            self.rows = rows;
            self.cols = cols;
            self.cells.clear();
            // Exact: a u16 by a u16 fits even a 32-bit usize.
            let len = usize::from(rows).saturating_mul(usize::from(cols));
            self.cells.resize(len, Cell::default());
        }
        self.cursor = None;
        self.cursor_shape = 0;
    }
    fn index(&self, y: u16, x: u16) -> Option<usize> {
        if y >= self.rows || x >= self.cols {
            return None;
        }
        usize::from(y)
            .checked_mul(usize::from(self.cols))?
            .checked_add(usize::from(x))
    }
    pub fn get(&self, y: u16, x: u16) -> Option<&Cell> {
        self.index(y, x).and_then(|i| self.cells.get(i))
    }
    fn get_mut(&mut self, y: u16, x: u16) -> Option<&mut Cell> {
        self.index(y, x).and_then(|i| self.cells.get_mut(i))
    }
    /// The cells of row `y`; none past the last row.
    pub fn row(&self, y: u16) -> &[Cell] {
        if y >= self.rows {
            return &[];
        }
        let cols = usize::from(self.cols);
        // Exact: a u16 by a u16 fits even a 32-bit usize.
        let start = usize::from(y).saturating_mul(cols);
        self.cells
            .get(start..start.saturating_add(cols))
            .unwrap_or_default()
    }
    /// Copies `cells` into row `y` from column `x`, clipped at the grid's
    /// edge, without `set`'s repairs: for a run of one well-formed row onto
    /// blank cells, which keeps its wide glyphs whole by itself.
    fn put_row(&mut self, y: u16, x: u16, cells: &[Cell]) {
        let Some(start) = self.index(y, x) else {
            return;
        };
        let room = usize::from(self.cols.saturating_sub(x));
        let slots = self.cells.iter_mut().skip(start).take(room);
        for (slot, cell) in slots.zip(cells) {
            *slot = *cell;
        }
    }
    /// Sets a cell, keeping wide glyphs whole: overwriting either half of
    /// one blanks the other, as a terminal would.
    fn set(&mut self, y: u16, x: u16, cell: Cell) {
        let Some(index) = self.index(y, x) else {
            return;
        };
        let old = self.cells.get(index).copied().unwrap_or_default();
        if old.is_wide_continuation()
            && !cell.is_wide_continuation()
            && x > 0
            && let Some(leader) = index.checked_sub(1).and_then(|i| self.cells.get_mut(i))
            && leader.is_wide()
        {
            *leader = Cell::default();
        }
        if old.is_wide()
            && !cell.is_wide()
            && let Some(rest) = x
                .checked_add(1)
                .and_then(|x| self.index(y, x))
                .and_then(|i| self.cells.get_mut(i))
            && rest.is_wide_continuation()
        {
            *rest = Cell::default();
        }
        if let Some(slot) = self.cells.get_mut(index) {
            *slot = cell;
        }
    }
    /// The text of a row, trailing blanks trimmed: for `capture-client`.
    pub fn row_text(&self, y: u16) -> String {
        let mut out = String::new();
        for x in 0..self.cols {
            if let Some(cell) = self.get(y, x) {
                if cell.is_wide_continuation() {
                    continue;
                }
                out.push_str(if cell.has_contents() {
                    cell.contents()
                } else {
                    " "
                });
            }
        }
        out.trim_end().to_owned()
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
            let cell = Cell::new(c.encode_utf8(&mut buffer), width == 2, style).unwrap_or_default();
            self.set(y, x, cell);
            if width == 2
                && let Some(second) = x.checked_add(1)
            {
                self.set(y, second, Cell::wide_continuation());
            }
            x = end;
        }
        x
    }

    fn fill(&mut self, y: u16, from: u16, to: u16, style: Attributes) {
        let blank = Cell::new(" ", false, style).unwrap_or_default();
        for x in from..to.min(self.cols) {
            self.set(y, x, blank);
        }
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

/// Cuts `text` to `cols` cells, with an ellipsis when cut.
pub fn fit(text: &str, cols: u16) -> String {
    if width(text) <= cols {
        return text.to_owned();
    }
    // Room for the text, less a cell for the ellipsis.
    let Some(mut room) = cols.checked_sub(1) else {
        return String::new();
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
    out
}

/// The client's screen as it should look now, in a grid of its own.
pub fn compose(session: &Session, client: ClientId) -> Option<Grid> {
    let mut grid = Grid::new(0, 0);
    compose_into(session, client, &mut grid).then_some(grid)
}

/// Composes the client's screen as it should look now into `grid`, whatever
/// it held; false if there is no such client.
pub fn compose_into(session: &Session, client: ClientId, grid: &mut Grid) -> bool {
    let Some(view) = session.views.get(&client) else {
        return false;
    };
    grid.reset(view.rows, view.cols);
    let area = Session::pane_area(view);
    let placement = session.placement(view);
    let focus = view.focus();
    // Copy mode and its positions, their rows found once for the paint.
    let copy = match &view.mode {
        Mode::Copy(copy) => session
            .panes
            .get(&copy.pane)
            .map(|pane| (copy.as_ref(), copy.resolve(pane.screen()))),
        Mode::Normal
        | Mode::Column { .. }
        | Mode::Repeat { .. }
        | Mode::List(_)
        | Mode::Prompt(_)
        | Mode::Confirm(_) => None,
    };
    for (id, rect) in &placement.panes {
        let Some(pane) = session.panes.get(id) else {
            continue;
        };
        let screen = pane.screen();
        let at = copy.filter(|(c, _)| c.pane == *id).map(|(_, at)| at);
        let offset = at.map_or(0, |at| at.offset(screen));
        let (rows, cols) = screen.size();
        let window = screen.window(offset, rows, cols);
        let width = rect.w.min(window.cols);
        for y in 0..rect.h.min(window.rows) {
            // Past the largest position is off the grid anyway.
            let Some((gy, gx)) = rect.at(y, 0) else {
                continue;
            };
            // The pane's own row, well formed, goes onto blank cells whole.
            let cells = window.row(y).map_or(&[][..], |row| row.cells);
            grid.put_row(gy, gx, cells.get(..usize::from(width)).unwrap_or(cells));
            // A wide glyph in the window's last column is cut off, as
            // `Window::cell` has it.
            if let Some(last) = window.cols.checked_sub(1)
                && last < width
                && cells.get(usize::from(last)).is_some_and(Cell::is_wide)
                && let Some(cell) = gx.checked_add(last).and_then(|x| grid.get_mut(gy, x))
            {
                *cell = Cell::default();
            }
            let Some(at) = at else { continue };
            for x in 0..width {
                if at.selected(y, x)
                    && let Some(cell) = gx.checked_add(x).and_then(|x| grid.get_mut(gy, x))
                    && !cell.is_wide_continuation()
                {
                    let attrs = cell.attributes();
                    let text = if cell.has_contents() {
                        cell.contents()
                    } else {
                        " "
                    };
                    *cell = Cell::new(text, cell.is_wide(), attrs.with_inverse(!attrs.inverse()))
                        .unwrap_or(*cell);
                }
            }
        }
    }
    separators(grid, &placement, focus);
    if view
        .tab()
        .and_then(|t| session.tab(t))
        .is_some_and(|t| t.root.is_none())
        && area.h > 0
    {
        // The keys bound to these commands, whatever they are; the command
        // itself if none is.
        let key_for = |argv: &[&str]| {
            session
                .config
                .bindings
                .iter()
                .find(|b| {
                    b.command
                        .iter()
                        .map(String::as_str)
                        .eq(argv.iter().copied())
                })
                .map_or_else(
                    || argv.join(" "),
                    |b| {
                        format!(
                            "{} {}",
                            session.config.prefix,
                            crate::config::keys_text(&b.keys)
                        )
                    },
                )
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
    // The cursor: the focused pane's, unless an overlay or copy mode owns it.
    if let Some(focus) = focus
        && let Some(rect) = placement.rect(focus)
        && let Some(pane) = session.panes.get(&focus)
    {
        let screen = pane.screen();
        match copy.filter(|(c, _)| c.pane == focus) {
            Some((_, at)) => {
                if let Some((y, x)) = at.cursor_in_view(rect.h)
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
                if !screen.hide_cursor()
                    && y < rect.h
                    && x < rect.w
                    && matches!(view.mode, Mode::Normal)
                    && let Some(at) = rect.at(y, x)
                {
                    grid.cursor = Some(at);
                    grid.cursor_shape = pane.modes.cursor_shape;
                }
            }
        }
    }
    bar(grid, session, view, copy);
    match &view.mode {
        Mode::Column { path, selected } => column(grid, session, view, path, *selected),
        Mode::List(list) => {
            let mut lines: Vec<(String, Attributes)> =
                vec![(list.title.clone(), panel().with_bold(true))];
            let capacity = overlay::list_capacity(view.rows);
            // The window ends at the selection, or at the last item.
            let start = list
                .selected
                .saturating_sub(capacity.saturating_sub(1))
                .min(list.items.len().saturating_sub(capacity));
            if start > 0 {
                lines.push((format!("▲ {start} more"), panel().with_dim(true)));
            }
            let ctx = crate::session::Ctx::client(view.id);
            for (i, item) in list.items.iter().enumerate().skip(start).take(capacity) {
                let dim = !list.chooser && session.unavailable(&item.command, &ctx).is_some();
                let marker = if item.current { "*" } else { " " };
                let mut attrs = panel();
                if i == list.selected {
                    attrs = attrs.with_inverse(true);
                }
                if dim {
                    attrs = attrs.with_dim(true);
                }
                lines.push((format!("{marker} {}", item.label), attrs));
            }
            let below = list
                .items
                .len()
                .saturating_sub(start.saturating_add(capacity));
            if below > 0 {
                lines.push((format!("▼ {below} more"), panel().with_dim(true)));
            }
            if list.items.is_empty() {
                lines.push(("nothing to choose".into(), panel().with_dim(true)));
            }
            let help = if list.chooser {
                "Enter selects · r renames · x closes · Esc"
            } else {
                "Enter runs · Esc cancels"
            };
            lines.push((help.into(), panel().with_dim(true)));
            surface(grid, view, &lines);
        }
        Mode::Prompt(prompt) => {
            let lines = vec![
                (prompt.title.clone(), panel().with_bold(true)),
                (with_cursor(&prompt.text, prompt.cursor), panel()),
                ("Enter accepts · Esc cancels".into(), panel().with_dim(true)),
            ];
            surface(grid, view, &lines);
            grid.cursor = None;
        }
        Mode::Confirm(confirm) => {
            let lines = vec![
                (confirm.question.clone(), panel().with_bold(true)),
                ("y confirms · n or Esc cancels".into(), panel()),
            ];
            surface(grid, view, &lines);
            grid.cursor = None;
        }
        // A repeat mode shows in the bar, leaving the layout in view.
        Mode::Normal | Mode::Copy(_) | Mode::Repeat { .. } => {}
    }
    true
}

/// A prompt's text with a bar at `cursor`, counted in chars; past the end
/// the bar follows the text.
fn with_cursor(text: &str, cursor: usize) -> String {
    let at = text
        .char_indices()
        .nth(cursor)
        .map_or(text.len(), |(i, _)| i);
    let (before, after) = text.split_at_checked(at).unwrap_or((text, ""));
    format!("{before}▏{after}")
}

fn panel() -> Attributes {
    style(Color::Idx(255), PANEL_BG)
}

/// Separator lines, with tees where one meets another; the ones beside the
/// focused pane are highlighted.
fn separators(grid: &mut Grid, placement: &crate::layout::Placement, focus: Option<PaneId>) {
    const UP: u8 = 1;
    const DOWN: u8 = 2;
    const LEFT: u8 = 4;
    const RIGHT: u8 = 8;
    const VERTICAL: u8 = 1;
    const HORIZONTAL: u8 = 2;
    let (rows, cols) = (grid.rows, grid.cols);
    let index = |x: i32, y: i32| -> Option<usize> {
        let (x, y) = (usize::try_from(x).ok()?, usize::try_from(y).ok()?);
        if x >= usize::from(cols) || y >= usize::from(rows) {
            return None;
        }
        y.checked_mul(usize::from(cols))?.checked_add(x)
    };
    let mut kind = vec![0u8; grid.cells.len()];
    let mut bits = vec![0u8; grid.cells.len()];
    for s in &placement.separators {
        for i in 0..s.len {
            let at = if s.vertical {
                s.y.checked_add(i).map(|y| (s.x, y))
            } else {
                s.x.checked_add(i).map(|x| (x, s.y))
            };
            // Past the largest position is off the grid.
            let Some((x, y)) = at else {
                break;
            };
            if let Some(at) = index(i32::from(x), i32::from(y)) {
                if let Some(k) = kind.get_mut(at) {
                    *k = if s.vertical { VERTICAL } else { HORIZONTAL };
                }
                if let Some(b) = bits.get_mut(at) {
                    *b = if s.vertical { UP | DOWN } else { LEFT | RIGHT };
                }
            }
        }
    }
    // A line whose end meets a perpendicular line joins it with a tee.
    for y in 0..i32::from(rows) {
        for x in 0..i32::from(cols) {
            let here = index(x, y).and_then(|i| kind.get(i)).copied().unwrap_or(0);
            let neighbours: &[(i32, i32, u8, u8)] = match here {
                HORIZONTAL => &[(-1, 0, VERTICAL, RIGHT), (1, 0, VERTICAL, LEFT)],
                VERTICAL => &[(0, -1, HORIZONTAL, DOWN), (0, 1, HORIZONTAL, UP)],
                _ => &[],
            };
            for (dx, dy, want, bit) in neighbours {
                if let Some(at) = x
                    .checked_add(*dx)
                    .zip(y.checked_add(*dy))
                    .and_then(|(x, y)| index(x, y))
                    && kind.get(at) == Some(want)
                    && let Some(b) = bits.get_mut(at)
                {
                    *b |= bit;
                }
            }
        }
    }
    let focused = focus.and_then(|f| placement.rect(f));
    // Within a cell of the rect. Exact: values from u16s never saturate an i32.
    let near = |x: u16, y: u16, r: &Rect| {
        let (x, y, rx, ry) = (i32::from(x), i32::from(y), i32::from(r.x), i32::from(r.y));
        x >= rx.saturating_sub(1)
            && x <= rx.saturating_add(i32::from(r.w))
            && y >= ry.saturating_sub(1)
            && y <= ry.saturating_add(i32::from(r.h))
    };
    for y in 0..rows {
        for x in 0..cols {
            let b = index(i32::from(x), i32::from(y))
                .and_then(|i| bits.get(i))
                .copied()
                .unwrap_or(0);
            if b == 0 {
                continue;
            }
            let glyph = match b {
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
            grid.set(
                y,
                x,
                Cell::new(glyph, false, style(color, Color::Default)).unwrap_or_default(),
            );
        }
    }
}

/// The bottom bar: the workspace and its tabs on the left; the focused
/// pane, or copy mode's position, or a notice on the right.
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
    let right = if let Some(notice) = &view.notice {
        Some((
            notice.text.clone(),
            if notice.error {
                style(Color::Idx(9), GRAY_BG)
            } else {
                style(Color::Idx(11), GRAY_BG)
            },
        ))
    } else if let Some(copy) = &copy_bar {
        Some((copy.position.clone(), base))
    } else if let Mode::Column { path, .. } = &view.mode {
        let typed = std::iter::once(session.config.prefix.to_string())
            .chain(path.iter().map(|key| key.to_string()))
            .collect::<Vec<_>>()
            .join(" ");
        Some((format!("{typed} …"), style(Color::Idx(0), Color::Idx(11))))
    } else if let Mode::Repeat { path } = &view.mode {
        // The mode's name and its keys: `RESIZE  h j k l · Esc`.
        let title = overlay::layer_title(session, path).unwrap_or_default();
        let keys: Vec<String> = session
            .config
            .bindings
            .iter()
            .filter_map(|b| match b.keys.strip_prefix(path.as_slice()) {
                Some([key]) => Some(key.to_string()),
                Some(_) | None => None,
            })
            .collect();
        Some((
            format!("{}  {} · Esc", title.to_uppercase(), keys.join(" ")),
            style(Color::Idx(0), Color::Idx(11)).with_bold(true),
        ))
    } else {
        view.focus().and_then(|f| session.panes.get(&f)).map(|p| {
            let mut text = format!("{} {}", p.id, p.label());
            if view.zoom {
                text.push_str(" [zoom]");
            }
            (text, base)
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
    if let Some(copy) = copy_bar {
        // Copy mode's keys replace the tabs.
        let badge = style(Color::Idx(0), Color::Idx(11)).with_bold(true);
        x = grid.text(
            y,
            x,
            &fit(&format!(" {} ", copy.badge), left_limit),
            badge,
            left_limit,
        );
        for (key, label) in copy.hints {
            let hint = format!("  {key} {label}");
            if x.saturating_add(width(&hint)) > left_limit {
                break;
            }
            x = grid.text(y, x, "  ", base, left_limit);
            x = grid.text(y, x, key, base.with_bold(true), left_limit);
            x = grid.text(y, x, &format!(" {label}"), base, left_limit);
        }
    } else if let Some(ws) = session.workspace(view.workspace) {
        let name = format!(" {} ", ws.name);
        x = grid.text(
            y,
            x,
            &fit(&name, left_limit),
            base.with_bold(true),
            left_limit,
        );
        let current = view.tab();
        for tab in &ws.tabs {
            let Some(room) = left_limit.checked_sub(x).filter(|r| *r > 0) else {
                break;
            };
            let label = format!(" {} ", tab.name);
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

/// A panel in the bottom-right corner, above the bar, sized to its lines.
fn surface(grid: &mut Grid, view: &View, lines: &[(String, Attributes)]) {
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
    // On a short screen the last lines (the selection and help) matter
    // most, so the first lines give way.
    let skip = lines.len().saturating_sub(usize::from(height));
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

/// The command column: the bindings and layers of the layer at `path`,
/// grouped, the selected one highlighted and those that cannot run now
/// dimmed.
fn column(grid: &mut Grid, session: &Session, view: &View, path: &[KeyPress], selected: usize) {
    let rows = overlay::column_rows(session, path);
    let key_width = rows
        .iter()
        .filter_map(|r| match r {
            ColumnRow::Binding { key, .. } | ColumnRow::Layer { key, .. } => {
                Some(width(&key.to_string()))
            }
            ColumnRow::Heading(_) => None,
        })
        .max()
        .unwrap_or(0);
    let ctx = crate::session::Ctx::client(view.id);
    let mut entries: Vec<(String, Attributes, bool)> = Vec::new();
    let mut index = 0usize;
    let mut selected_row = 0usize;
    for row in &rows {
        match row {
            ColumnRow::Heading(group) => {
                entries.push((group.clone(), panel().with_bold(true), false))
            }
            ColumnRow::Binding {
                key,
                label,
                command,
            } => {
                let key = key.to_string();
                let pad: String =
                    std::iter::repeat_n(' ', usize::from(key_width.saturating_sub(width(&key))))
                        .collect();
                let mut attrs = panel();
                if session.unavailable(command, &ctx).is_some() {
                    attrs = attrs.with_dim(true);
                }
                if index == selected {
                    attrs = attrs.with_inverse(true);
                    selected_row = entries.len();
                }
                entries.push((format!("{pad}{key}  {label}"), attrs, true));
                // At most the number of rows.
                index = index.saturating_add(1);
            }
            ColumnRow::Layer { key, title } => {
                let key = key.to_string();
                let pad: String =
                    std::iter::repeat_n(' ', usize::from(key_width.saturating_sub(width(&key))))
                        .collect();
                let mut attrs = panel();
                if index == selected {
                    attrs = attrs.with_inverse(true);
                    selected_row = entries.len();
                }
                entries.push((format!("{pad}{key}  {title}…"), attrs, true));
                index = index.saturating_add(1);
            }
        }
    }
    let available = usize::from(view.rows.saturating_sub(1));
    let heading = available >= 4;
    // Room less the heading and the two "more" lines, but at least one.
    let body_room = available
        .saturating_sub(usize::from(heading).saturating_add(2))
        .max(1);
    // The rows scrolled off so the selection is the last shown, if any.
    let start = selected_row.saturating_add(1).saturating_sub(body_room);
    let mut lines: Vec<(String, Attributes)> = Vec::new();
    if heading {
        // Right after the prefix, every command; in a layer, its keys so far
        // and its title.
        let title = match overlay::layer_title(session, path) {
            Some(title) if !path.is_empty() => format!(
                "{} {}: {title}",
                session.config.prefix,
                crate::config::keys_text(path)
            ),
            Some(_) | None => "Commands".to_owned(),
        };
        lines.push((title, panel().with_bold(true)));
    }
    if start > 0 {
        lines.push((format!("▲ {start} more"), panel().with_dim(true)));
    }
    for (text, attrs, _) in entries.iter().skip(start).take(body_room) {
        lines.push((text.clone(), *attrs));
    }
    let below = entries
        .len()
        .saturating_sub(start.saturating_add(body_room));
    if below > 0 {
        lines.push((format!("▼ {below} more"), panel().with_dim(true)));
    }
    if rows.is_empty() {
        lines.push(("no bindings".into(), panel().with_dim(true)));
    }
    surface(grid, view, &lines);
}

// ------------------------------------------------------------------ paint

fn sgr(out: &mut Vec<u8>, a: Attributes) {
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
    if a.underline() {
        out.extend_from_slice(b";4");
    }
    if a.inverse() {
        out.extend_from_slice(b";7");
    }
    // `base` is 30 or 40, so no code comes near 255: every sum is exact.
    let color = |out: &mut Vec<u8>, c: Color, base: u8| match c {
        Color::Default => {}
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
    };
    color(out, a.foreground, 30);
    color(out, a.background, 40);
    out.push(b'm');
}

/// A row or column as the terminal counts it, from 1; exact in a u32.
fn one_based(n: u16) -> u32 {
    u32::from(n).saturating_add(1)
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
    out.extend_from_slice(b"\x1b[?2026h\x1b[?25l");
    let full = old.is_none_or(|o| o.rows != new.rows || o.cols != new.cols);
    if full {
        out.extend_from_slice(b"\x1b[0m\x1b[H\x1b[2J");
    }
    let mut current: Option<Attributes> = None;
    for y in 0..new.rows {
        let row = new.row(y);
        // What the client shows of the row, unless it is painted whole.
        let shown = old.filter(|_| !full).map(|o| o.row(y));
        // An unchanged row costs this one comparison.
        if shown == Some(row) {
            continue;
        }
        let cell = |x: u16| row.get(usize::from(x));
        let changed = |x: u16| shown.is_none_or(|o| o.get(usize::from(x)) != cell(x));
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
                let attrs = cell.attributes();
                if current != Some(attrs) {
                    sgr(out, attrs);
                    current = Some(attrs);
                }
                if cell.is_wide() && cx.saturating_add(1) >= new.cols {
                    // A wide glyph cannot fit in the last column.
                    out.push(b' ');
                } else {
                    out.extend_from_slice(if cell.has_contents() {
                        cell.contents().as_bytes()
                    } else {
                        b" "
                    });
                }
                cx = cx.saturating_add(if cell.is_wide() { 2 } else { 1 });
            }
            x = cx.max(x.saturating_add(1));
        }
    }
    out.extend_from_slice(b"\x1b[0m");
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

    fn apply(bytes: &[u8], rows: u16, cols: u16, parser: &mut fux_vt::Parser) -> Vec<String> {
        let _ = parser.process(bytes);
        let screen = parser.screen();
        (0..rows)
            .map(|y| {
                let window = screen.window(0, rows, cols);
                window
                    .row(y)
                    .map(|r| crate::session::row_text(r.cells))
                    .unwrap_or_default()
            })
            .collect()
    }

    /// At the widest a terminal can be, the last column is u16::MAX - 1:
    /// one past it must stop a wide glyph, not overflow (in release, wrap).
    /// Copy mode's bar replaces the tabs with what it is doing and the keys
    /// that act now; a narrow bar drops the least important.
    #[test]
    fn copy_mode_replaces_the_tabs_with_its_keys() -> Result<(), Box<dyn std::error::Error>> {
        let mut s = Session::new(
            crate::config::Config::default(),
            "/nonexistent/fux.sock".into(),
            false,
        );
        s.start()?;
        let c = s.attach(10, 120, None)?;
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

    /// The selection test compose used to make for every cell, finding
    /// the view's top and both ends in the history each time.
    fn reference_selected(
        copy: &crate::copy::Copy,
        screen: &fux_vt::Screen,
        view_row: u16,
        col: u16,
    ) -> bool {
        use crate::copy::{Select, index_of};
        let at = |(id, col)| Some((index_of(screen, id)?, col));
        let Some(top) = index_of(screen, copy.top) else {
            return false;
        };
        let Some((kind, anchor)) = copy.selection else {
            return false;
        };
        let (Some(a), Some(b)) = (at(anchor), at(copy.cursor)) else {
            return false;
        };
        let (start, end) = (a.min(b), a.max(b));
        let Some(row) = top.checked_add(usize::from(view_row)) else {
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

    /// Copy mode finds its rows once per paint: the cells compose inverts
    /// are exactly those the old per-cell test selected, for each kind of
    /// selection, over a screen with history and a view scrolled into it.
    #[test]
    fn compose_inverts_exactly_the_selected_cells() -> Result<(), Box<dyn std::error::Error>> {
        let mut s = Session::new(
            crate::config::Config::default(),
            "/nonexistent/fux.sock".into(),
            false,
        );
        s.start()?;
        let c = s.attach(10, 30, None)?;
        let pane = PaneId(1);
        let text: String = (0..60)
            .map(|i| {
                let tail: String = std::iter::repeat_n('x', i % 20).collect();
                format!("line {i} 界 {tail}\r\n")
            })
            .collect();
        s.output(pane, text.as_bytes());
        s.input(c, b"\x02c");
        let mut kinds = Vec::new();
        // Up into the history, then each kind of selection, with its ends
        // moved about, a block's end left of its start, and the view
        // scrolled past the anchor.
        for keys in [
            &b"kkkkkkkkkkkkkkkllllllllv"[..],
            b"kkklllll",
            b"s",
            b"jjjjjjjjjjjjjj",
            b"x",
            b"hhhhhhhhhh",
            b"kkkkkkkkkkkkkkkkkkkkkkkkk",
            b"uv",
            b"ollll",
        ] {
            s.input(c, keys);
            let grid = compose(&s, c).ok_or("a screen")?;
            let view = s.views.get(&c).ok_or("the client")?;
            let Mode::Copy(copy) = &view.mode else {
                return Err("copy mode ended".into());
            };
            kinds.push(copy.selection.map(|(kind, _)| kind));
            let screen = s.panes.get(&pane).ok_or("the pane")?.screen();
            let rect = s.placement(view).rect(pane).ok_or("the pane's place")?;
            // The view shows the rows from its top.
            let top = crate::copy::index_of(screen, copy.top).ok_or("the top row")?;
            for y in 0..rect.h {
                let row = crate::copy::row_at(screen, top + usize::from(y)).ok_or("a row")?;
                assert_eq!(grid.row_text(y), crate::session::row_text(row.cells));
            }
            let mut selected = 0;
            for y in 0..rect.h {
                for x in 0..rect.w {
                    let cell = rect.at(y, x).and_then(|(gy, gx)| grid.get(gy, gx));
                    let cell = cell.copied().unwrap_or_default();
                    let expected =
                        reference_selected(copy, screen, y, x) && !cell.is_wide_continuation();
                    assert_eq!(cell.inverse(), expected, "{keys:?} at {y},{x}");
                    selected += usize::from(expected);
                }
            }
            if copy.selection.is_some() {
                assert!(selected > 0, "{keys:?}: nothing selected in view");
            }
        }
        use crate::copy::Select;
        for kind in [Select::Char, Select::Line, Select::Block] {
            assert!(kinds.contains(&Some(kind)), "{kind:?}: {kinds:?}");
        }
        Ok(())
    }

    /// A grid composed into again, whatever it held and whatever its size,
    /// comes out as a fresh one would.
    #[test]
    fn composing_into_a_used_grid_is_composing_afresh() -> Result<(), Box<dyn std::error::Error>> {
        let mut s = Session::new(
            crate::config::Config::default(),
            "/nonexistent/fux.sock".into(),
            false,
        );
        s.start()?;
        let c = s.attach(12, 50, None)?;
        s.output(PaneId(1), b"first screen\r\n\x1b[5 q");
        let before = compose(&s, c).ok_or("a screen")?;
        let outcome = s.run(
            &["split".to_owned(), "-h".to_owned()],
            &crate::session::Ctx::client(c),
        );
        assert_eq!(outcome.status, 0, "{}", outcome.stderr);
        // The focused pane hides its cursor, and a smaller client makes the
        // panes smaller than their places: compose leaves cells untouched.
        s.output(PaneId(2), "界 second\r\n\x1b[?25l".as_bytes());
        s.attach(8, 30, None)?;
        let fresh = compose(&s, c).ok_or("a screen")?;
        assert_ne!(before, fresh);
        assert!(before.cursor.is_some() && fresh.cursor.is_none());
        assert!(fresh.cells.contains(&Cell::default()));
        // The previous screen, with its cursor and shape.
        let mut used = before;
        assert!(compose_into(&s, c, &mut used));
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
            assert!(compose_into(&s, c, &mut other));
            assert_eq!(other, fresh, "{rows}x{cols}");
        }
        Ok(())
    }

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
        let wide = Cell::new("界", true, Attributes::default()).unwrap_or_default();
        grid.set(0, u16::MAX.saturating_sub(1), wide);
        // The glyph is painted as a blank, and moving past it stops the run.
        let bytes = paint(None, &grid);
        assert!(bytes.ends_with(b"\x1b[?2026l"));
    }

    #[test]
    fn the_prompt_cursor_falls_between_chars_not_bytes() {
        for (text, cursor, shown) in [
            ("", 0, "▏"),
            ("", 3, "▏"),
            ("héllo", 0, "▏héllo"),
            ("héllo", 2, "hé▏llo"),
            ("héllo", 5, "héllo▏"),
            ("héllo", 9, "héllo▏"),
            ("界a界", 1, "界▏a界"),
            ("界a界", 2, "界a▏界"),
        ] {
            assert_eq!(with_cursor(text, cursor), shown, "{text:?} at {cursor}");
        }
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
        // Nothing changed: nothing but the envelope.
        let quiet = String::from_utf8_lossy(&paint(Some(&b), &b)).into_owned();
        assert_eq!(quiet, "\x1b[?2026h\x1b[?25l\x1b[0m\x1b[?2026l");
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

    #[test]
    fn fit_cuts_at_glyph_boundaries() {
        assert_eq!(fit("hello", 10), "hello");
        assert_eq!(fit("hello", 4), "hel…");
        assert_eq!(fit("界界界", 4), "界…");
        assert_eq!(fit("x", 0), "");
    }
}
