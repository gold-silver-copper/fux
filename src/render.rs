//! Painting a client's screen: compose a grid of cells from its panes, the
//! separators, the bar and any overlay; diff it against what the client has;
//! send only the changed runs, inside synchronized output.
use crate::command::ClientId;
use crate::layout::{PaneId, Rect};
use crate::overlay::{self, ColumnRow};
use crate::session::Session;
use crate::view::{Mode, View};
use fux_vt::{Attributes, Cell, Color};
use std::fmt::Write;
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
            cells: vec![Cell::default(); usize::from(rows) * usize::from(cols)],
            cursor: None,
            cursor_shape: 0,
        }
    }
    fn index(&self, y: u16, x: u16) -> Option<usize> {
        (y < self.rows && x < self.cols)
            .then(|| usize::from(y) * usize::from(self.cols) + usize::from(x))
    }
    pub fn get(&self, y: u16, x: u16) -> Option<&Cell> {
        self.index(y, x).and_then(|i| self.cells.get(i))
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
            && let Some(rest) = self.index(y, x + 1).and_then(|i| self.cells.get_mut(i))
            && rest.is_wide_continuation()
        {
            *rest = Cell::default();
        }
        if let Some(slot) = self.cells.get_mut(index) {
            *slot = cell;
        }
    }
    /// The text of a row, for tests.
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
            let width = c.width().unwrap_or(0) as u16;
            if width == 0 {
                continue;
            }
            if x.saturating_add(width) > limit.min(self.cols) {
                break;
            }
            let mut buffer = [0u8; 4];
            let cell = Cell::new(c.encode_utf8(&mut buffer), width == 2, style).unwrap_or_default();
            self.set(y, x, cell);
            if width == 2 {
                self.set(y, x + 1, Cell::wide_continuation());
            }
            x += width;
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

/// Display width of a string, controls dropped.
pub fn width(text: &str) -> u16 {
    text.chars()
        .filter(|c| !c.is_control())
        .map(|c| c.width().unwrap_or(0))
        .sum::<usize>()
        .min(4096) as u16
}

/// Cuts `text` to `cols` cells, with an ellipsis when cut.
pub fn fit(text: &str, cols: u16) -> String {
    if width(text) <= cols {
        return text.to_owned();
    }
    if cols == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut room = cols - 1;
    for c in text.chars().filter(|c| !c.is_control()) {
        let w = c.width().unwrap_or(0) as u16;
        if w > room {
            break;
        }
        room -= w;
        out.push(c);
    }
    out.push('…');
    out
}

/// The client's screen as it should look now.
pub fn compose(session: &Session, client: ClientId) -> Option<Grid> {
    let view = session.views.get(&client)?;
    let mut grid = Grid::new(view.rows, view.cols);
    let area = Session::pane_area(view);
    let placement = session.placement(view);
    let focus = view.focus();
    let copy = match &view.mode {
        Mode::Copy(copy) => Some(copy.as_ref()),
        Mode::Normal | Mode::Column { .. } | Mode::List(_) | Mode::Prompt(_) | Mode::Confirm(_) => {
            None
        }
    };
    for (id, rect) in &placement.panes {
        let Some(pane) = session.panes.get(id) else {
            continue;
        };
        let screen = pane.screen();
        let offset = copy
            .filter(|c| c.pane == *id)
            .map_or(0, |c| c.offset(screen));
        let (rows, cols) = screen.size();
        let window = screen.window(offset, rows, cols);
        for y in 0..rect.h.min(window.rows) {
            for x in 0..rect.w.min(window.cols) {
                let mut cell = window.cell(y, x).copied().unwrap_or_default();
                if let Some(copy) = copy.filter(|c| c.pane == *id)
                    && copy.selected(screen, y, x)
                {
                    let attrs = cell.attributes();
                    let text = if cell.has_contents() {
                        cell.contents()
                    } else {
                        " "
                    };
                    if !cell.is_wide_continuation() {
                        cell =
                            Cell::new(text, cell.is_wide(), attrs.with_inverse(!attrs.inverse()))
                                .unwrap_or(cell);
                    }
                }
                grid.set(rect.y + y, rect.x + x, cell);
            }
        }
    }
    separators(&mut grid, &placement, focus);
    if view
        .tab()
        .and_then(|t| session.tab(t))
        .is_some_and(|t| t.root.is_none())
        && area.h > 0
    {
        let hint = format!(
            "empty tab: {} h splits it, {} s closes it",
            session.config.prefix, session.config.prefix
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
        match copy.filter(|c| c.pane == focus) {
            Some(copy) => {
                if let Some((y, x)) = copy.cursor_in_view(screen, rect.h)
                    && x < rect.w
                {
                    grid.cursor = Some((rect.y + y, rect.x + x));
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
                {
                    grid.cursor = Some((rect.y + y, rect.x + x));
                    grid.cursor_shape = pane.modes.cursor_shape;
                }
            }
        }
    }
    bar(&mut grid, session, view, copy);
    match &view.mode {
        Mode::Column { selected } => column(&mut grid, session, view, *selected),
        Mode::List(list) => {
            let mut lines: Vec<(String, Attributes)> =
                vec![(list.title.clone(), panel().with_bold(true))];
            let capacity = overlay::list_capacity(view.rows);
            let start = list
                .selected
                .saturating_sub(capacity - 1)
                .min(list.items.len().saturating_sub(capacity));
            if start > 0 {
                lines.push((format!("▲ {start} more"), panel().with_dim(true)));
            }
            let ctx = crate::session::Ctx::client(view.id);
            for (i, item) in list.items.iter().enumerate().skip(start).take(capacity) {
                let dim = !list.chooser && session.unavailable(&item.argv, &ctx).is_some();
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
            let below = list.items.len().saturating_sub(start + capacity);
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
            surface(&mut grid, view, &lines);
        }
        Mode::Prompt(prompt) => {
            let lines = vec![
                (prompt.title.clone(), panel().with_bold(true)),
                (with_cursor(&prompt.text, prompt.cursor), panel()),
                ("Enter accepts · Esc cancels".into(), panel().with_dim(true)),
            ];
            surface(&mut grid, view, &lines);
            grid.cursor = None;
        }
        Mode::Confirm(confirm) => {
            let lines = vec![
                (confirm.question.clone(), panel().with_bold(true)),
                ("y confirms · n or Esc cancels".into(), panel()),
            ];
            surface(&mut grid, view, &lines);
            grid.cursor = None;
        }
        Mode::Normal | Mode::Copy(_) => {}
    }
    Some(grid)
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
        (x >= 0 && y >= 0 && x < i32::from(cols) && y < i32::from(rows))
            .then(|| y as usize * usize::from(cols) + x as usize)
    };
    let mut kind = vec![0u8; grid.cells.len()];
    let mut bits = vec![0u8; grid.cells.len()];
    for s in &placement.separators {
        for i in 0..s.len {
            let (x, y) = if s.vertical {
                (s.x, s.y + i)
            } else {
                (s.x + i, s.y)
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
                if let Some(at) = index(x + dx, y + dy)
                    && kind.get(at) == Some(want)
                    && let Some(b) = bits.get_mut(at)
                {
                    *b |= bit;
                }
            }
        }
    }
    let focused = focus.and_then(|f| placement.rect(f));
    let near = |x: u16, y: u16, r: &Rect| {
        let (x, y, rx, ry) = (i32::from(x), i32::from(y), i32::from(r.x), i32::from(r.y));
        x >= rx - 1 && x <= rx + i32::from(r.w) && y >= ry - 1 && y <= ry + i32::from(r.h)
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
fn bar(grid: &mut Grid, session: &Session, view: &View, copy: Option<&crate::copy::Copy>) {
    if view.rows == 0 {
        return;
    }
    let y = view.rows - 1;
    let base = style(BAR_FG, GRAY_BG);
    grid.fill(y, 0, view.cols, base);
    let right = if let Some(notice) = &view.notice {
        Some((
            notice.text.clone(),
            if notice.error {
                style(Color::Idx(9), GRAY_BG)
            } else {
                style(Color::Idx(11), GRAY_BG)
            },
        ))
    } else if let Some(copy) = copy {
        session.panes.get(&copy.pane).map(|p| {
            (
                copy.status(p.screen()),
                style(Color::Idx(0), Color::Idx(11)).with_bold(true),
            )
        })
    } else if matches!(view.mode, Mode::Column { .. }) {
        Some((
            format!("{} …", session.config.prefix),
            style(Color::Idx(0), Color::Idx(11)),
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
    let right_width = right
        .as_ref()
        .map_or(0, |(t, _)| width(t).min(view.cols / 2 + view.cols / 4));
    let left_limit = view
        .cols
        .saturating_sub(right_width + u16::from(right_width > 0));
    let mut x = 0;
    if let Some(ws) = session.workspace(view.workspace) {
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
            if x >= left_limit {
                break;
            }
            let label = format!(" {} ", tab.name);
            let attrs = if Some(tab.id) == current {
                style(Color::Idx(0), Color::Idx(2)).with_bold(true)
            } else {
                base
            };
            x = grid.text(y, x, &fit(&label, left_limit - x), attrs, left_limit);
        }
    }
    if let Some((text, attrs)) = right {
        let text = fit(&text, right_width);
        let start = view.cols.saturating_sub(width(&text) + 1);
        grid.text(y, start, &text, attrs, view.cols);
    }
}

/// A panel in the bottom-right corner, above the bar, sized to its lines.
fn surface(grid: &mut Grid, view: &View, lines: &[(String, Attributes)]) {
    let available = view.rows.saturating_sub(1);
    if available == 0 || view.cols == 0 || lines.is_empty() {
        return;
    }
    let height = (lines.len() as u16).min(available);
    let inner = lines.iter().map(|(t, _)| width(t)).max().unwrap_or(0);
    let w = inner.saturating_add(2).min(view.cols);
    let x = view.cols - w;
    let top = available - height;
    // On a short screen the last lines (the selection and help) matter
    // most, so the first lines give way.
    let skip = lines.len().saturating_sub(usize::from(height));
    for (row, (text, attrs)) in lines.iter().skip(skip).enumerate() {
        let y = top + row as u16;
        grid.fill(y, x, view.cols, *attrs);
        grid.text(
            y,
            x + 1,
            &fit(text, w.saturating_sub(2)),
            *attrs,
            view.cols.saturating_sub(1).max(x + 1),
        );
    }
}

/// The command column: every binding, grouped, the selected one highlighted
/// and those that cannot run now dimmed.
fn column(grid: &mut Grid, session: &Session, view: &View, selected: usize) {
    let rows = overlay::column_rows(session);
    let key_width = rows
        .iter()
        .filter_map(|r| match r {
            ColumnRow::Binding { key, .. } => Some(width(key)),
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
            ColumnRow::Binding { key, label, argv } => {
                let pad = " ".repeat(usize::from(key_width.saturating_sub(width(key))));
                let mut attrs = panel();
                if session.unavailable(argv, &ctx).is_some() {
                    attrs = attrs.with_dim(true);
                }
                if index == selected {
                    attrs = attrs.with_inverse(true);
                    selected_row = entries.len();
                }
                entries.push((format!("{pad}{key}  {label}"), attrs, true));
                index += 1;
            }
        }
    }
    let available = usize::from(view.rows.saturating_sub(1));
    let heading = available >= 4;
    let body_room = available.saturating_sub(usize::from(heading) + 2).max(1);
    let start = if selected_row < body_room {
        0
    } else {
        selected_row + 1 - body_room
    };
    let mut lines: Vec<(String, Attributes)> = Vec::new();
    if heading {
        lines.push(("Commands".into(), panel().with_bold(true)));
    }
    if start > 0 {
        lines.push((format!("▲ {start} more"), panel().with_dim(true)));
    }
    for (text, attrs, _) in entries.iter().skip(start).take(body_room) {
        lines.push((text.clone(), *attrs));
    }
    let below = entries.len().saturating_sub(start + body_room);
    if below > 0 {
        lines.push((format!("▼ {below} more"), panel().with_dim(true)));
    }
    if rows.is_empty() {
        lines.push(("no bindings".into(), panel().with_dim(true)));
    }
    surface(grid, view, &lines);
}

// ------------------------------------------------------------------ paint

fn sgr(out: &mut String, a: Attributes) {
    out.push_str("\x1b[0");
    if a.bold() {
        out.push_str(";1");
    }
    if a.dim() {
        out.push_str(";2");
    }
    if a.italic() {
        out.push_str(";3");
    }
    if a.underline() {
        out.push_str(";4");
    }
    if a.inverse() {
        out.push_str(";7");
    }
    let color = |out: &mut String, c: Color, base: u8| match c {
        Color::Default => {}
        Color::Idx(n) if n < 8 => {
            let _ = write!(out, ";{}", base + n);
        }
        Color::Idx(n) if n < 16 => {
            let _ = write!(out, ";{}", base + 60 + n - 8);
        }
        Color::Idx(n) => {
            let _ = write!(out, ";{};5;{n}", base + 8);
        }
        Color::Rgb(r, g, b) => {
            let _ = write!(out, ";{};2;{r};{g};{b}", base + 8);
        }
    };
    color(out, a.foreground, 30);
    color(out, a.background, 40);
    out.push('m');
}

/// The bytes that turn `old` (what the client shows, or nothing) into `new`.
pub fn paint(old: Option<&Grid>, new: &Grid) -> Vec<u8> {
    let mut out = String::from("\x1b[?2026h\x1b[?25l");
    let full = old.is_none_or(|o| o.rows != new.rows || o.cols != new.cols);
    if full {
        out.push_str("\x1b[0m\x1b[H\x1b[2J");
    }
    let mut current: Option<Attributes> = None;
    for y in 0..new.rows {
        let mut x = 0u16;
        while x < new.cols {
            let changed = |x: u16| full || old.and_then(|o| o.get(y, x)) != new.get(y, x);
            if !changed(x) {
                x += 1;
                continue;
            }
            // A run of changed cells, starting at a glyph's first half.
            let mut start = x;
            if new.get(y, start).is_some_and(|c| c.is_wide_continuation()) {
                start = start.saturating_sub(1);
            }
            let _ = write!(out, "\x1b[{};{}H", y + 1, start + 1);
            let mut cx = start;
            while cx < new.cols
                && (cx == start
                    || changed(cx)
                    || new.get(y, cx).is_some_and(|c| c.is_wide_continuation()))
            {
                let Some(cell) = new.get(y, cx) else { break };
                if cell.is_wide_continuation() {
                    cx += 1;
                    continue;
                }
                let attrs = cell.attributes();
                if current != Some(attrs) {
                    sgr(&mut out, attrs);
                    current = Some(attrs);
                }
                if cell.is_wide() && cx + 1 >= new.cols {
                    // A wide glyph cannot fit in the last column.
                    out.push(' ');
                } else {
                    out.push_str(if cell.has_contents() {
                        cell.contents()
                    } else {
                        " "
                    });
                }
                cx += if cell.is_wide() { 2 } else { 1 };
            }
            x = cx.max(x + 1);
        }
    }
    out.push_str("\x1b[0m");
    let shape_changed = old.is_none_or(|o| o.cursor_shape != new.cursor_shape);
    if shape_changed {
        let _ = write!(out, "\x1b[{} q", new.cursor_shape);
    }
    if let Some((y, x)) = new.cursor {
        let _ = write!(out, "\x1b[{};{}H\x1b[?25h", y + 1, x + 1);
    }
    out.push_str("\x1b[?2026l");
    out.into_bytes()
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

    #[test]
    fn fit_cuts_at_glyph_boundaries() {
        assert_eq!(fit("hello", 10), "hello");
        assert_eq!(fit("hello", 4), "hel…");
        assert_eq!(fit("界界界", 4), "界…");
        assert_eq!(fit("x", 0), "");
    }
}
