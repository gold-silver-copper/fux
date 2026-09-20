//! A bounded viewer-local viewport, never a second history or terminal emulator.
#[cfg(test)]
mod tests;
use crate::{
    assets::{ClipboardPolicy, Settings},
    model::*,
    protocol::Input,
    terminal::Terminal,
};
use bevy_ecs::prelude::*;

pub const MAX_COPY_BYTES: usize = 1024 * 1024 / 4 * 3;
const MAX_CELLS: usize = 262_144;

#[derive(Clone, PartialEq, Eq)]
pub struct Grid {
    pub offset: usize,
    pub size: (u16, u16),
    pub cells: Vec<Vec<vt100::Cell>>,
    pub wrapped: Vec<bool>,
}
impl Grid {
    pub fn capture(screen: &mut vt100::Screen, offset: usize) -> Result<Self, String> {
        let (rows, cols) = screen.size();
        if usize::from(rows) * usize::from(cols) > MAX_CELLS {
            return Err("copy viewport exceeds 262144 cells".into());
        }
        screen.set_scrollback(offset);
        let grid = Self {
            offset: screen.scrollback(),
            size: (rows, cols),
            cells: (0..rows)
                .map(|y| {
                    (0..cols)
                        .map(|x| screen.cell(y, x).unwrap().clone())
                        .collect()
                })
                .collect(),
            wrapped: (0..rows).map(|y| screen.row_wrapped(y)).collect(),
        };
        screen.set_scrollback(0);
        Ok(grid)
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
        for row in &mut self.cells {
            row.truncate(usize::from(size.1));
        }
        Ok(self)
    }

    fn point(&self, (y, x): (u16, u16)) -> (u16, u16) {
        let y = y.min(self.size.0 - 1);
        let mut x = x.min(self.size.1 - 1);
        if self.cells[usize::from(y)][usize::from(x)].is_wide_continuation() {
            x = x.saturating_sub(1);
        }
        (y, x)
    }
    pub fn text(&self, a: (u16, u16), b: (u16, u16)) -> String {
        let (a, b) = (self.point(a), self.point(b));
        let (start, end) = if a <= b { (a, b) } else { (b, a) };
        let mut text = String::new();
        for y in start.0..=end.0 {
            let left = if y == start.0 { start.1 } else { 0 };
            let right = if y == end.0 { end.1 } else { self.size.1 - 1 };
            let mut line = String::new();
            for x in left..=right {
                let cell = &self.cells[usize::from(y)][usize::from(x)];
                if cell.is_wide_continuation() || cell.is_wide() && x + 1 >= self.size.1 {
                    continue;
                }
                if cell.has_contents() {
                    line.push_str(cell.contents());
                } else {
                    line.push(' ');
                }
            }
            if y < end.0 && self.wrapped[usize::from(y)] {
                text.push_str(&line);
            } else {
                text.push_str(line.trim_end_matches(' '));
                if y < end.0 {
                    text.push('\n');
                }
            }
        }
        text
    }
}

#[derive(Component)]
pub struct Selection {
    pub leaf: Entity,
    pub cursor: (u16, u16),
    pub anchor: Option<(u16, u16)>,
    pub grid: Grid,
    pub revision: u64,
    pub dragging: bool,
    pub mouse_origin: bool,
}

pub fn validate_clipboard(settings: &Settings, text: &str) -> Result<(), String> {
    if settings.clipboard != ClipboardPolicy::WriteOnly {
        return Err("clipboard disabled; configure clipboard: write-only".into());
    }
    if text.len() > MAX_COPY_BYTES {
        return Err("copy exceeds 1 MiB encoded clipboard limit".into());
    }
    Ok(())
}

pub fn start(world: &mut World, id: Entity, leaf: Entity) -> Result<(), String> {
    let pane = world.get::<PaneView>(leaf).ok_or("pane removed")?.pane;
    let v = world.get::<Viewer>(id).ok_or("viewer removed")?;
    let offset = if v.focus == Some(leaf) {
        v.scrollback
    } else {
        0
    };
    let visible =
        crate::server::content_size(world, id, leaf).ok_or("no visible content to select")?;
    let mut terminal = world
        .get_mut::<Terminal>(pane)
        .ok_or("terminal not found")?;
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
    let mut v = world.get_mut::<Viewer>(id).unwrap();
    v.focus = Some(leaf);
    v.scrollback = offset;
    v.prefix = false;
    v.prompt = None;
    v.notice = "Copy: arrows/hjkl · Space select · y copy · g live · q exit".into();
    Ok(())
}

/// Conservative row validation: vt100 exposes offsets, not stable history row IDs.
/// Never silently reuse an anchor after its displayed cells change.
pub fn refresh(world: &mut World, id: Entity) {
    let visible = world
        .get::<Selection>(id)
        .and_then(|s| crate::server::content_size(world, id, s.leaf))
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
    if v.focus != Some(leaf) {
        world.entity_mut(id).remove::<Selection>();
        return;
    }
    let Some(pane) = world.get::<PaneView>(leaf).map(|p| p.pane) else {
        world.entity_mut(id).remove::<Selection>();
        return;
    };
    let offset = v.scrollback;
    let old_size = selection.grid.size;
    let revision = selection.revision;
    let old_offset = selection.grid.offset;
    let result = if let Some(mut terminal) = world.get_mut::<Terminal>(pane) {
        let size = terminal.screen().size();
        if revision == terminal.revision()
            && offset == old_offset
            && old_size == (visible.0.min(size.0), visible.1.min(size.1))
        {
            return;
        }
        let revision = terminal.revision();
        terminal
            .selection_grid(offset)
            .and_then(|grid| grid.clip(visible))
            .map(|grid| (revision, grid))
    } else {
        Err("terminal removed; copy mode ended".into())
    };
    match result {
        Ok((revision, grid)) => {
            let mut selection = world.get_mut::<Selection>(id).unwrap();
            let invalidated = selection.anchor.is_some() && grid != selection.grid;
            if invalidated {
                selection.anchor = None;
                selection.dragging = false;
            }
            selection.cursor = grid.point(selection.cursor);
            let actual = grid.offset;
            selection.grid = grid;
            selection.revision = revision;
            let mut v = world.get_mut::<Viewer>(id).unwrap();
            v.scrollback = actual;
            if invalidated {
                v.notice = "selection cleared: rows changed, resized, scrolled or evicted".into();
                v.notice_error = true;
            }
        }
        Err(error) => {
            world.entity_mut(id).remove::<Selection>();
            if let Some(mut v) = world.get_mut::<Viewer>(id) {
                v.notice = error;
                v.notice_error = true;
            }
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
        && selection.anchor == Some(selection.cursor)
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
        Input::Mouse { action, .. } => match action.as_str() {
            "scrollup" => scroll = Some(true),
            "scrolldown" => scroll = Some(false),
            _ => return false,
        },
        Input::Key { key, .. } => match key.as_str() {
            "q" | "escape" => {
                world.entity_mut(id).remove::<Selection>();
                world.get_mut::<Viewer>(id).unwrap().notice.clear();
                return true;
            }
            "g" => {
                let mut v = world.get_mut::<Viewer>(id).unwrap();
                v.scrollback = 0;
                v.notice.clear();
                world.entity_mut(id).remove::<Selection>();
                return true;
            }
            "c" => {
                let mut selection = world.get_mut::<Selection>(id).unwrap();
                selection.anchor = None;
                selection.dragging = false;
                return true;
            }
            " " | "space" => {
                world.get_mut::<Selection>(id).unwrap().anchor = Some(cursor);
                return true;
            }
            "y" | "enter" => {
                let text = selection
                    .anchor
                    .map(|anchor| selection.grid.text(anchor, cursor));
                if let Some(text) = text {
                    match crate::server::clipboard(world, id, text) {
                        Ok(()) => {
                            world.entity_mut(id).remove::<Selection>();
                            world.get_mut::<Viewer>(id).unwrap().scrollback = 0;
                        }
                        Err(error) => {
                            let mut v = world.get_mut::<Viewer>(id).unwrap();
                            v.notice = error;
                            v.notice_error = true;
                        }
                    }
                } else {
                    world.get_mut::<Viewer>(id).unwrap().notice = "Space starts a selection".into();
                }
                return true;
            }
            "left" | "h" => position.1 = position.1.saturating_sub(1),
            "right" | "l" => {
                let cell = &selection.grid.cells[usize::from(cursor.0)][usize::from(cursor.1)];
                position.1 = (position.1 + if cell.is_wide() { 2 } else { 1 }).min(size.1 - 1);
            }
            "up" | "k" => {
                if position.0 > 0 {
                    position.0 -= 1;
                } else {
                    scroll = Some(true);
                }
            }
            "down" | "j" => {
                if position.0 + 1 < size.0 {
                    position.0 += 1;
                } else {
                    scroll = Some(false);
                }
            }
            "u" | "pageup" => {
                scroll = Some(true);
                page = true;
            }
            "d" | "pagedown" => {
                scroll = Some(false);
                page = true;
            }
            "home" => position.1 = 0,
            "end" => position.1 = size.1 - 1,
            _ => {}
        },
    }
    let mut selection = world.get_mut::<Selection>(id).unwrap();
    selection.cursor = selection.grid.point(position);
    if let Some(older) = scroll {
        let mut v = world.get_mut::<Viewer>(id).unwrap();
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
    action: &str,
    point: (u16, u16),
) -> Result<(), String> {
    if action == "press" {
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
        "press" => {
            selection.cursor = point;
            selection.anchor = Some(point);
            selection.dragging = true;
            selection.mouse_origin = true;
        }
        "move" | "release" if selection.dragging => {
            if action == "move" || selection.cursor != point {
                selection.mouse_origin = false;
            }
            selection.cursor = point;
            if action == "release" {
                selection.dragging = false;
            }
        }
        _ => {}
    }
    if action == "release" && selection.mouse_origin {
        world.entity_mut(id).remove::<Selection>();
        world.get_mut::<Viewer>(id).unwrap().notice.clear();
    }
    Ok(())
}

pub fn paint(out: &mut String, selection: &Selection, rect: &crate::protocol::PaneRect) {
    let (a, b) = selection
        .anchor
        .map_or((selection.cursor, selection.cursor), |a| {
            (a, selection.cursor)
        });
    let (start, end) = if a <= b { (a, b) } else { (b, a) };
    for y in start.0..=end.0.min(rect.height.saturating_sub(1)) {
        for x in 0..rect.width.min(selection.grid.size.1) {
            if (y, x) < start || (y, x) > end {
                continue;
            }
            let cell = &selection.grid.cells[usize::from(y)][usize::from(x)];
            if cell.is_wide_continuation() || cell.is_wide() && x + 1 >= rect.width {
                continue;
            }
            crate::chrome::at(
                out,
                rect.x + x,
                rect.y + y,
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
