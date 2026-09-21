//! A bounded viewer-local viewport, never a second history or terminal emulator.
#[cfg(test)]
mod tests;
use crate::{
    assets::{ClipboardPolicy, Settings},
    model::*,
    protocol::{Direction, Input, Key, MouseAction},
    terminal::Terminal,
};
use bevy_ecs::prelude::*;

pub const MAX_COPY_BYTES: usize = 1024 * 1024 / 4 * 3;
pub const MAX_CELLS: usize = 262_144;

#[derive(Clone, PartialEq, Eq)]
pub struct Grid {
    pub offset: usize,
    pub size: (u16, u16),
    pub cells: Vec<Vec<fux_vt::Cell>>,
    pub wrapped: Vec<bool>,
    ids: Vec<fux_vt::RowId>,
    versions: Vec<u64>,
    backing_size: (u16, u16),
}
impl Grid {
    pub fn capture(screen: &fux_vt::Screen, offset: usize) -> Result<Self, String> {
        let (rows, cols) = screen.size();
        if usize::from(rows) * usize::from(cols) > MAX_CELLS {
            return Err("copy viewport exceeds 262144 cells".into());
        }
        let window = screen.window(offset, rows, cols);
        let rows: Vec<_> = (0..rows).filter_map(|y| window.row(y)).collect();
        Ok(Self {
            offset: window.offset,
            size: screen.size(),
            backing_size: screen.size(),
            cells: rows.iter().map(|r| r.cells.to_vec()).collect(),
            wrapped: rows.iter().map(|r| r.wrapped).collect(),
            ids: rows.iter().map(|r| r.id).collect(),
            versions: rows.iter().map(|r| r.version).collect(),
        })
    }
    fn clip(mut self, visible: (u16, u16)) -> Result<Self, String> {
        let size = (self.size.0.min(visible.0), self.size.1.min(visible.1));
        if size.0 == 0 || size.1 == 0 {
            return Err("no visible content to select".into());
        }
        if size.1 < self.size.1 {
            self.wrapped.fill(false);
        }
        self.size = size;
        self.cells.truncate(usize::from(size.0));
        self.wrapped.truncate(usize::from(size.0));
        self.ids.truncate(usize::from(size.0));
        self.versions.truncate(usize::from(size.0));
        for row in &mut self.cells {
            row.truncate(usize::from(size.1));
        }
        Ok(self)
    }

    fn cell(&self, (y, x): (u16, u16)) -> Option<&fux_vt::Cell> {
        self.cells.get(usize::from(y))?.get(usize::from(x))
    }
    fn point(&self, (y, x): (u16, u16)) -> (u16, u16) {
        let y = y.min(self.size.0.saturating_sub(1));
        let mut x = x.min(self.size.1.saturating_sub(1));
        if self
            .cell((y, x))
            .is_some_and(fux_vt::Cell::is_wide_continuation)
        {
            x = x.saturating_sub(1);
        }
        (y, x)
    }
    fn identity(&self, (y, x): (u16, u16)) -> Option<(fux_vt::RowId, u16)> {
        Some((*self.ids.get(usize::from(y))?, x))
    }
    fn position(&self, (id, col): (fux_vt::RowId, u16)) -> Option<(u16, u16)> {
        Some((
            u16::try_from(self.ids.iter().position(|&row| row == id)?).ok()?,
            col,
        ))
    }
    /// Compare every required row and selected span, not just endpoint IDs.
    /// Versions skip untouched rows; an unrelated write or SGR change is safe.
    fn retains(&self, screen: &fux_vt::Screen, a: (u16, u16), b: (u16, u16)) -> bool {
        let (start, end) = if a <= b { (a, b) } else { (b, a) };
        (start.0..=end.0).all(|y| {
            let index = usize::from(y);
            let Some(row) = self.ids.get(index).and_then(|&id| screen.row_by_id(id)) else {
                return false;
            };
            if self.versions.get(index) == Some(&row.version) {
                return true;
            }
            if y < end.0
                && self.wrapped.get(index).copied()
                    != Some(row.wrapped && self.size.1 == self.backing_size.1)
            {
                return false;
            }
            let left = if y == start.0 { start.1 } else { 0 };
            let mut right = if y == end.0 { end.1 } else { self.size.1 - 1 };
            if self.cell((y, right)).is_some_and(fux_vt::Cell::is_wide) {
                right = right.saturating_add(1).min(self.size.1 - 1);
            }
            (left..=right).all(
                |x| match (self.cell((y, x)), row.cells.get(usize::from(x))) {
                    (Some(a), Some(b)) => {
                        a.contents() == b.contents()
                            && a.is_wide() == b.is_wide()
                            && a.is_wide_continuation() == b.is_wide_continuation()
                    }
                    _ => false,
                },
            )
        })
    }
    pub fn text(&self, a: (u16, u16), b: (u16, u16)) -> Result<String, String> {
        let (a, b) = (self.point(a), self.point(b));
        let (start, end) = if a <= b { (a, b) } else { (b, a) };
        let mut text = String::new();
        for y in start.0..=end.0 {
            let left = if y == start.0 { start.1 } else { 0 };
            let right = if y == end.0 { end.1 } else { self.size.1 - 1 };
            let mut line = String::new();
            for x in left..=right {
                let Some(cell) = self.cell((y, x)) else {
                    continue;
                };
                if cell.is_wide_continuation() || cell.is_wide() && x + 1 >= self.size.1 {
                    continue;
                }
                let contents = if cell.has_contents() {
                    cell.contents()
                } else {
                    " "
                };
                if text.len() + line.len() + contents.len() > MAX_COPY_BYTES {
                    return Err("copy exceeds 1 MiB encoded clipboard limit".into());
                }
                line.push_str(contents);
            }
            if y < end.0 && self.wrapped.get(usize::from(y)).copied().unwrap_or(false) {
                text.push_str(&line);
            } else {
                text.push_str(line.trim_end_matches(' '));
                if y < end.0 {
                    if text.len() == MAX_COPY_BYTES {
                        return Err("copy exceeds 1 MiB encoded clipboard limit".into());
                    }
                    text.push('\n');
                }
            }
        }
        Ok(text)
    }
}

#[derive(Component)]
pub struct Selection {
    pub leaf: Entity,
    pub cursor: (u16, u16),
    pub anchor: Option<(fux_vt::RowId, u16)>,
    pub grid: Grid,
    pub revision: u64,
    pub dragging: bool,
    pub mouse_origin: bool,
}

impl Selection {
    fn renew(
        &self,
        screen: &fux_vt::Screen,
        offset: usize,
        visible: (u16, u16),
    ) -> Result<(Grid, (u16, u16), bool), String> {
        let capture = |offset| Grid::capture(screen, offset)?.clip(visible);
        let fallback = || {
            let grid = capture(offset)?;
            let cursor = grid.point(self.cursor);
            Ok((grid, cursor, self.anchor.is_some()))
        };
        let Some(anchor) = self.anchor.and_then(|a| self.grid.position(a)) else {
            return fallback();
        };
        if offset != self.grid.offset
            || screen.size() != self.grid.backing_size
            || self.grid.size
                != (
                    visible.0.min(screen.size().0),
                    visible.1.min(screen.size().1),
                )
            || !self.grid.retains(screen, anchor, self.cursor)
        {
            return fallback();
        }
        let (start, end) = if anchor <= self.cursor {
            (anchor, self.cursor)
        } else {
            (self.cursor, anchor)
        };
        let required = self
            .grid
            .ids
            .get(usize::from(start.0)..=usize::from(end.0))
            .ok_or("selection rows missing")?;
        let first = *required.first().ok_or("selection has no rows")?;
        let preferred = self
            .grid
            .ids
            .first()
            .and_then(|&id| screen.offset_for_row(id))
            .or_else(|| screen.offset_for_row(first))
            .unwrap_or(0);
        let mut grid = capture(preferred)?;
        let fits = |grid: &Grid| {
            grid.ids
                .iter()
                .position(|&id| id == first)
                .and_then(|i| grid.ids.get(i..i + required.len()))
                == Some(required)
        };
        if !fits(&grid) {
            grid = capture(screen.offset_for_row(first).unwrap_or(0))?;
        }
        if !fits(&grid) {
            return fallback();
        }
        let cursor = self
            .grid
            .identity(self.cursor)
            .and_then(|id| grid.position(id))
            .ok_or("selection cursor row lost")?;
        Ok((grid, cursor, false))
    }
}

pub fn validate_clipboard(settings: &Settings, text: &str) -> Result<(), &'static str> {
    if settings.clipboard != ClipboardPolicy::WriteOnly {
        return Err("clipboard disabled; configure clipboard: write-only");
    }
    if text.len() > MAX_COPY_BYTES {
        return Err("copy exceeds 1 MiB encoded clipboard limit");
    }
    Ok(())
}

pub fn start(world: &mut World, id: Entity, leaf: Entity) -> Result<(), String> {
    let pane = world.get::<PaneView>(leaf).ok_or("pane removed")?.pane;
    let v = world.get::<Viewer>(id).ok_or("viewer removed")?;
    let offset = if focused(world, id) == Some(leaf) {
        v.scrollback
    } else {
        0
    };
    let visible =
        crate::frame::content_size(world, id, leaf).ok_or("no visible content to select")?;
    let terminal = world.get::<Terminal>(pane).ok_or("terminal not found")?;
    let revision = terminal.revision();
    let grid = terminal.selection_grid(offset)?.clip(visible)?;
    let offset = grid.offset;
    world.entity_mut(id).insert(Selection {
        leaf,
        cursor: (0, 0),
        anchor: None,
        grid,
        revision,
        dragging: false,
        mouse_origin: false,
    });
    crate::interaction::close_prefix(world, id);
    world
        .get_entity_mut(id)
        .map_err(|_| "viewer removed")?
        .insert(Focused(leaf));
    let mut v = world.get_mut::<Viewer>(id).ok_or("viewer removed")?;
    v.scrollback = offset;
    v.notice = Notice::info("Copy: arrows/hjkl · Space select · y copy · g live · q exit");
    Ok(())
}

/// Follow retained row identities while validating the selected text spans.
pub fn refresh(world: &mut World, id: Entity) {
    let visible = world
        .get::<Selection>(id)
        .and_then(|s| crate::frame::content_size(world, id, s.leaf))
        .unwrap_or((0, 0));
    refresh_visible(world, id, visible);
}

pub fn refresh_visible(world: &mut World, id: Entity, visible: (u16, u16)) {
    let Some(selection) = world.get::<Selection>(id) else {
        return;
    };
    let Some(v) = world.get::<Viewer>(id) else {
        return;
    };
    let leaf = selection.leaf;
    let Some(pane) = world.get::<PaneView>(leaf).map(|p| p.pane) else {
        world.entity_mut(id).remove::<Selection>();
        notify(world, id, Notice::error("selection cleared: pane removed"));
        return;
    };
    if focused(world, id) != Some(leaf) {
        world.entity_mut(id).remove::<Selection>();
        return;
    }
    let offset = v.scrollback;
    let old_size = selection.grid.size;
    let revision = selection.revision;
    let old_offset = selection.grid.offset;
    let result = if let Some(terminal) = world.get::<Terminal>(pane) {
        let size = terminal.screen().size();
        if revision == terminal.revision()
            && offset == old_offset
            && old_size == (visible.0.min(size.0), visible.1.min(size.1))
            && selection.grid.backing_size == size
        {
            return;
        }
        let revision = terminal.revision();
        selection
            .renew(terminal.screen(), offset, visible)
            .map(|(grid, cursor, invalidated)| (revision, grid, cursor, invalidated))
    } else {
        Err("terminal removed; copy mode ended".into())
    };
    match result {
        Ok((revision, grid, cursor, invalidated)) => {
            let Some(mut selection) = world.get_mut::<Selection>(id) else {
                return;
            };
            if invalidated {
                selection.anchor = None;
                selection.dragging = false;
            }
            selection.cursor = cursor;
            let actual = grid.offset;
            selection.grid = grid;
            selection.revision = revision;
            if let Some(mut v) = world.get_mut::<Viewer>(id) {
                v.scrollback = actual;
                if invalidated {
                    v.notice = Notice::error(
                        "selection cleared: rows changed, resized, scrolled or evicted",
                    );
                }
            }
        }
        Err(error) => {
            world.entity_mut(id).remove::<Selection>();
            notify(world, id, Notice::error(error));
        }
    }
}

pub fn input(world: &mut World, id: Entity, input: &Input) -> bool {
    if world.get::<Selection>(id).is_none() {
        return false;
    }
    refresh(world, id);
    let Some(selection) = world.get::<Selection>(id) else {
        return false;
    };
    if selection.mouse_origin
        && selection.anchor == selection.grid.identity(selection.cursor)
        && matches!(input, Input::Key { .. } | Input::Paste { .. })
    {
        world.entity_mut(id).remove::<Selection>();
        return false;
    }
    let cursor = selection.cursor;
    let size = selection.grid.size;
    let mut position = cursor;
    let mut scroll: Option<bool> = None;
    let mut page = false;
    match input {
        Input::Resize { .. } => return false,
        Input::Paste { .. } | Input::PasteBegin => return true,
        Input::Mouse { action, .. } => match action {
            MouseAction::ScrollUp => scroll = Some(true),
            MouseAction::ScrollDown => scroll = Some(false),
            _ => return false,
        },
        Input::Key { key, .. } => match key {
            Key::Char('q') | Key::Escape => {
                world.entity_mut(id).remove::<Selection>();
                if let Some(mut v) = world.get_mut::<Viewer>(id) {
                    v.notice = None;
                }
                return true;
            }
            Key::Char('g') => {
                if let Some(mut v) = world.get_mut::<Viewer>(id) {
                    v.scrollback = 0;
                    v.notice = None;
                }
                world.entity_mut(id).remove::<Selection>();
                return true;
            }
            Key::Char('c') => {
                if let Some(mut selection) = world.get_mut::<Selection>(id) {
                    selection.anchor = None;
                    selection.dragging = false;
                }
                return true;
            }
            Key::Char(' ') => {
                if let Some(mut selection) = world.get_mut::<Selection>(id) {
                    selection.anchor = selection.grid.identity(cursor);
                }
                return true;
            }
            Key::Char('y') | Key::Enter => {
                let text = selection
                    .anchor
                    .and_then(|anchor| selection.grid.position(anchor))
                    .map(|anchor| selection.grid.text(anchor, cursor));
                let copied =
                    text.map(|text| text.and_then(|text| crate::frame::clipboard(world, id, text)));
                if matches!(copied, Some(Ok(()))) {
                    world.entity_mut(id).remove::<Selection>();
                }
                if let Some(mut v) = world.get_mut::<Viewer>(id) {
                    match copied {
                        Some(Ok(())) => v.scrollback = 0,
                        Some(Err(error)) => v.notice = Notice::error(error),
                        None => v.notice = Notice::info("Space starts a selection"),
                    }
                }
                return true;
            }
            Key::Arrow(Direction::Left) | Key::Char('h') => {
                position.1 = position.1.saturating_sub(1);
            }
            Key::Arrow(Direction::Right) | Key::Char('l') => {
                let wide = selection
                    .grid
                    .cell(cursor)
                    .is_some_and(fux_vt::Cell::is_wide);
                position.1 = (position.1 + if wide { 2 } else { 1 }).min(size.1.saturating_sub(1));
            }
            Key::Arrow(Direction::Up) | Key::Char('k') => {
                if position.0 > 0 {
                    position.0 -= 1;
                } else {
                    scroll = Some(true);
                }
            }
            Key::Arrow(Direction::Down) | Key::Char('j') => {
                if position.0 + 1 < size.0 {
                    position.0 += 1;
                } else {
                    scroll = Some(false);
                }
            }
            Key::Char('u') | Key::PageUp => {
                scroll = Some(true);
                page = true;
            }
            Key::Char('d') | Key::PageDown => {
                scroll = Some(false);
                page = true;
            }
            Key::Home => position.1 = 0,
            Key::End => position.1 = size.1.saturating_sub(1),
            _ => {}
        },
    }
    if let Some(mut selection) = world.get_mut::<Selection>(id) {
        selection.cursor = selection.grid.point(position);
    }
    if let Some(older) = scroll
        && let Some(mut v) = world.get_mut::<Viewer>(id)
    {
        let step = if page {
            usize::from(size.0).saturating_sub(1).max(1)
        } else {
            1
        };
        v.scrollback = if older {
            v.scrollback.saturating_add(step)
        } else {
            v.scrollback.saturating_sub(step)
        };
        refresh(world, id);
    }
    true
}

pub fn mouse(
    world: &mut World,
    id: Entity,
    leaf: Entity,
    action: MouseAction,
    point: (u16, u16),
) -> Result<(), String> {
    if action == MouseAction::Press {
        start(world, id, leaf)?;
    }
    let Some(mut selection) = world.get_mut::<Selection>(id) else {
        return Ok(());
    };
    if selection.leaf != leaf {
        return Ok(());
    }
    let point = selection.grid.point(point);
    match action {
        MouseAction::Press => {
            selection.cursor = point;
            selection.anchor = selection.grid.identity(point);
            selection.dragging = true;
            selection.mouse_origin = true;
        }
        MouseAction::Move | MouseAction::Release if selection.dragging => {
            if action == MouseAction::Move || selection.cursor != point {
                selection.mouse_origin = false;
            }
            selection.cursor = point;
            if action == MouseAction::Release {
                selection.dragging = false;
            }
        }
        _ => {}
    }
    if action == MouseAction::Release && selection.mouse_origin {
        world.entity_mut(id).remove::<Selection>();
        if let Some(mut v) = world.get_mut::<Viewer>(id) {
            v.notice = None;
        }
    }
    Ok(())
}

pub fn paint(out: &mut String, selection: &Selection, rect: &crate::protocol::PaneRect) {
    let (a, b) = selection
        .anchor
        .and_then(|anchor| selection.grid.position(anchor))
        .map_or((selection.cursor, selection.cursor), |a| {
            (a, selection.cursor)
        });
    let (start, end) = if a <= b { (a, b) } else { (b, a) };
    for y in start.0..=end.0.min(rect.height().saturating_sub(1)) {
        for x in 0..rect.width().min(selection.grid.size.1) {
            if (y, x) < start || (y, x) > end {
                continue;
            }
            let Some(cell) = selection.grid.cell((y, x)) else {
                continue;
            };
            if cell.is_wide_continuation() || cell.is_wide() && x + 1 >= rect.width() {
                continue;
            }
            crate::chrome::at(
                out,
                rect.x() + x,
                rect.y() + y,
                format_args!(
                    "\x1b[0;7m{}\x1b[0m",
                    if cell.has_contents() {
                        cell.contents()
                    } else {
                        " "
                    }
                ),
            );
        }
    }
}
