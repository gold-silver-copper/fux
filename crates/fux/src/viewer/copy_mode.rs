//! Copy mode (prompt 3.11, `prefix [`): a viewer-local [`CopyView`] on the focused leaf is a
//! `ScrollArea`-style viewport over the pane's history plus its live screen. The history is
//! fetched once over BRP (`fux/pane.capture` on a worker thread) and prepended when it lands;
//! the live rows are the replicated `Grid`, so they keep their styles. `v`/Space anchors a
//! selection, motions move the cursor and scroll the minimum needed to keep it visible
//! (`ScrollIntoView`), `y` copies the selection to the outer terminal through the painter's
//! OSC 52 path (subject to the configured clipboard policy), `/` searches with `n`/`N`, and
//! `q`/Escape leaves. Selection and matches are painted as highlights by the painter.

use bevy_app::prelude::*;
use bevy_ecs::prelude::*;
use bevy_input::keyboard::Key;
use bevy_input_focus::InputFocus;
use bevy_state::prelude::*;

use super::chrome::{ModeIndicator, PendingNotice, Text};
use super::focus::Bindings;
use super::keys::KeyChord;
use super::replicate::{ClipboardWrite, Grid, Replicated};
use super::{Brp, BrpReply, BrpTag, Mode};
use crate::model::{PaneId, Shows};

/// History rows requested from the server (`Limits::scrollback_lines`'s default).
const CAPTURE_ROWS: usize = 2000;

/// A cell position in the view's row space: history rows first, then the live screen.
pub type Pos = (usize, usize);

/// Copy-mode state on the leaf whose pane it views.
#[derive(Component, Debug)]
pub struct CopyView {
    history: Vec<String>,
    /// First visible row.
    offset: usize,
    cursor: Pos,
    anchor: Option<Pos>,
    query: String,
    /// The query is being typed.
    searching: bool,
    /// `(row, column, length)` in characters.
    matches: Vec<(usize, usize, usize)>,
}

impl CopyView {
    fn new(grid: &Grid) -> Self {
        Self {
            history: Vec::new(),
            offset: 0,
            cursor: (
                usize::from(grid.cursor.row.min(grid.rows().saturating_sub(1))),
                usize::from(grid.cursor.col.min(grid.cols().saturating_sub(1))),
            ),
            anchor: None,
            query: String::new(),
            searching: false,
            matches: Vec::new(),
        }
    }

    pub fn offset(&self) -> usize {
        self.offset
    }

    pub fn cursor(&self) -> Pos {
        self.cursor
    }

    pub fn history_len(&self) -> usize {
        self.history.len()
    }

    /// A history row's text, or `None` for a live row (the grid has it).
    pub fn history_row(&self, row: usize) -> Option<&str> {
        self.history.get(row).map(String::as_str)
    }

    pub fn matches(&self) -> &[(usize, usize, usize)] {
        &self.matches
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn is_searching(&self) -> bool {
        self.searching
    }

    /// The selection as an inclusive `(start, end)` in row-major order.
    pub fn selection(&self) -> Option<(Pos, Pos)> {
        let anchor = self.anchor?;
        Some(if anchor <= self.cursor {
            (anchor, self.cursor)
        } else {
            (self.cursor, anchor)
        })
    }

    fn total_rows(&self, grid: &Grid) -> usize {
        self.history.len() + usize::from(grid.rows())
    }

    /// Text of a row, live rows read from the grid with trailing blanks trimmed.
    fn row_text(&self, grid: &Grid, row: usize) -> String {
        if let Some(line) = self.history.get(row) {
            return line.clone();
        }
        let Some(live) = row
            .checked_sub(self.history.len())
            .and_then(|r| u16::try_from(r).ok())
        else {
            return String::new();
        };
        let mut text = String::with_capacity(usize::from(grid.cols()));
        for col in 0..grid.cols() {
            match grid.cell(col, live) {
                Some(cell) if cell.width > 0 => text.push_str(cell.text.as_str()),
                Some(_) => {}
                None => break,
            }
        }
        let trimmed = text.trim_end().len();
        text.truncate(trimmed);
        text
    }

    /// Scrolls the minimum needed to show the cursor row (`ScrollIntoView`).
    fn scroll_into_view(&mut self, grid: &Grid) {
        let view = usize::from(grid.rows()).max(1);
        let total = self.total_rows(grid);
        let row = self.cursor.0.min(total.saturating_sub(1));
        self.cursor.0 = row;
        if row < self.offset {
            self.offset = row;
        } else if row >= self.offset + view {
            self.offset = row + 1 - view;
        }
        self.offset = self.offset.min(total.saturating_sub(view));
    }

    fn move_cursor(&mut self, grid: &Grid, rows: isize, cols: isize) {
        let total = self.total_rows(grid);
        let row = (self.cursor.0 as isize + rows).clamp(0, total.saturating_sub(1) as isize);
        let col = (self.cursor.1 as isize + cols).clamp(0, (grid.cols().max(1) as isize) - 1);
        self.cursor = (row as usize, col as usize);
        self.scroll_into_view(grid);
    }

    /// Recomputes the matches of the query over every row.
    fn search(&mut self, grid: &Grid) {
        self.matches.clear();
        if self.query.is_empty() {
            return;
        }
        let len = self.query.chars().count();
        for row in 0..self.total_rows(grid) {
            let text = self.row_text(grid, row);
            let mut from = 0;
            while let Some(found) = text.get(from..).and_then(|t| t.find(&self.query)) {
                let byte = from + found;
                let column = text.get(..byte).map_or(0, |t| t.chars().count());
                self.matches.push((row, column, len));
                from = byte + self.query.len();
            }
        }
    }

    /// Moves to the next (`forward`) or previous match after the cursor, wrapping.
    fn jump(&mut self, grid: &Grid, forward: bool) -> bool {
        if self.matches.is_empty() {
            return false;
        }
        let at = (self.cursor.0, self.cursor.1);
        let next = if forward {
            self.matches
                .iter()
                .find(|(r, c, _)| (*r, *c) > at)
                .or_else(|| self.matches.first())
        } else {
            self.matches
                .iter()
                .rev()
                .find(|(r, c, _)| (*r, *c) < at)
                .or_else(|| self.matches.last())
        };
        if let Some(&(row, col, _)) = next {
            self.cursor = (row, col);
            self.scroll_into_view(grid);
        }
        true
    }

    /// The selected text, rows joined by newlines.
    fn selected_text(&self, grid: &Grid) -> Option<String> {
        let ((r0, c0), (r1, c1)) = self.selection()?;
        let mut out = String::new();
        for row in r0..=r1 {
            let text = self.row_text(grid, row);
            let start = if row == r0 { c0 } else { 0 };
            let end = if row == r1 { c1 + 1 } else { usize::MAX };
            let slice: String = text.chars().skip(start).take(end - start).collect();
            out.push_str(&slice);
            if row != r1 {
                out.push('\n');
            }
        }
        Some(out)
    }

    /// Prepends captured history, keeping the view and the cursor on the same rows.
    fn prepend(&mut self, grid: &Grid, lines: Vec<String>) {
        let added = lines.len();
        self.history.splice(0..0, lines);
        self.offset += added;
        self.cursor.0 += added;
        if let Some(anchor) = &mut self.anchor {
            anchor.0 += added;
        }
        if !self.query.is_empty() {
            self.search(grid);
        }
    }
}

/// The leaf in copy mode: the focused replicated leaf and its grid.
fn viewed(
    focus: &InputFocus,
    leaves: &Query<&Shows, With<Replicated>>,
    panes: &Query<&PaneId>,
) -> Option<(Entity, Entity, PaneId)> {
    let leaf = focus.get()?;
    let pane = leaves.get(leaf).ok()?.0;
    let id = *panes.get(pane).ok()?;
    Some((leaf, pane, id))
}

/// `prefix [`: enters copy mode on the focused pane and asks the server for its history.
pub fn enter(
    focus: Res<InputFocus>,
    brp: Res<Brp>,
    leaves: Query<&Shows, With<Replicated>>,
    panes: Query<&PaneId>,
    grids: Query<&Grid>,
    mut next: ResMut<NextState<Mode>>,
    mut commands: Commands,
) {
    let Some((leaf, pane, id)) = viewed(&focus, &leaves, &panes) else {
        return;
    };
    let Ok(grid) = grids.get(pane) else {
        return;
    };
    commands.entity(leaf).insert(CopyView::new(grid));
    next.set(Mode::CopyMode);
    brp.call(
        BrpTag::Capture(id),
        "fux/pane.capture",
        serde_json::json!({ "pane": id.0, "scrollback": CAPTURE_ROWS }),
    );
}

fn leave(commands: &mut Commands, leaf: Entity, next: &mut NextState<Mode>) {
    commands.entity(leaf).remove::<CopyView>();
    next.set(Mode::Normal);
}

/// Keys in `Mode::CopyMode`, run for each chord by `focus::on_key`.
#[allow(clippy::too_many_arguments)]
pub fn handle_key(
    In(chord): In<KeyChord>,
    bindings: Res<Bindings>,
    focus: Res<InputFocus>,
    leaves: Query<&Shows, With<Replicated>>,
    panes: Query<&PaneId>,
    grids: Query<&Grid>,
    mut views: Query<&mut CopyView>,
    mut next: ResMut<NextState<Mode>>,
    mut notice: ResMut<PendingNotice>,
    mut clipboard: MessageWriter<ClipboardWrite>,
    mut commands: Commands,
) {
    let Some((leaf, pane, _)) = viewed(&focus, &leaves, &panes) else {
        next.set(Mode::Normal);
        return;
    };
    let (Ok(mut view), Ok(grid)) = (views.get_mut(leaf), grids.get(pane)) else {
        leave(&mut commands, leaf, &mut next);
        return;
    };
    if chord == *bindings.prefix() {
        leave(&mut commands, leaf, &mut next);
        next.set(Mode::Prefix);
        return;
    }
    if view.searching {
        match &chord.key {
            Key::Escape => {
                view.searching = false;
                view.query.clear();
                view.matches.clear();
            }
            Key::Enter => {
                view.searching = false;
                view.search(grid);
                if !view.jump(grid, true) {
                    notice.0 = Some(format!("no match for {:?}", view.query));
                }
            }
            Key::Backspace => {
                view.query.pop();
            }
            Key::Character(c) if !chord.ctrl && !chord.alt => view.query.push_str(c),
            _ => {}
        }
        return;
    }
    let page = grid.rows().max(2) as isize - 1;
    let half = (page / 2).max(1);
    match (&chord.key, chord.ctrl) {
        (Key::Escape, _) => leave(&mut commands, leaf, &mut next),
        (Key::ArrowUp, _) => view.move_cursor(grid, -1, 0),
        (Key::ArrowDown, _) => view.move_cursor(grid, 1, 0),
        (Key::ArrowLeft, _) => view.move_cursor(grid, 0, -1),
        (Key::ArrowRight, _) => view.move_cursor(grid, 0, 1),
        (Key::PageUp, _) => view.move_cursor(grid, -page, 0),
        (Key::PageDown, _) => view.move_cursor(grid, page, 0),
        (Key::Home, _) => view.move_cursor(grid, 0, -(isize::MAX / 2)),
        (Key::End, _) => view.move_cursor(grid, 0, isize::MAX / 2),
        (Key::Enter, _) => yank(
            &view,
            grid,
            &mut clipboard,
            &mut notice,
            &mut commands,
            leaf,
            &mut next,
        ),
        (Key::Character(c), false) => match c.as_str() {
            "q" => leave(&mut commands, leaf, &mut next),
            "k" => view.move_cursor(grid, -1, 0),
            "j" => view.move_cursor(grid, 1, 0),
            "h" => view.move_cursor(grid, 0, -1),
            "l" => view.move_cursor(grid, 0, 1),
            "0" => view.move_cursor(grid, 0, -(isize::MAX / 2)),
            "$" => view.move_cursor(grid, 0, isize::MAX / 2),
            "g" => view.move_cursor(grid, -(isize::MAX / 2), 0),
            "G" => view.move_cursor(grid, isize::MAX / 2, 0),
            "v" | " " => {
                view.anchor = match view.anchor {
                    Some(_) => None,
                    None => Some(view.cursor),
                };
            }
            "y" => yank(
                &view,
                grid,
                &mut clipboard,
                &mut notice,
                &mut commands,
                leaf,
                &mut next,
            ),
            "/" => {
                view.searching = true;
                view.query.clear();
            }
            "n" => {
                view.jump(grid, true);
            }
            "N" => {
                view.jump(grid, false);
            }
            _ => {}
        },
        (Key::Character(c), true) => match c.as_str() {
            "u" => view.move_cursor(grid, -half, 0),
            "d" => view.move_cursor(grid, half, 0),
            _ => {}
        },
        _ => {}
    }
}

/// Copies the selection as an OSC 52 payload and leaves copy mode.
fn yank(
    view: &CopyView,
    grid: &Grid,
    clipboard: &mut MessageWriter<ClipboardWrite>,
    notice: &mut PendingNotice,
    commands: &mut Commands,
    leaf: Entity,
    next: &mut NextState<Mode>,
) {
    let Some(text) = view.selected_text(grid) else {
        notice.0 = Some("nothing selected (v or Space starts a selection)".into());
        return;
    };
    clipboard.write(ClipboardWrite(base64(text.as_bytes())));
    notice.0 = Some(format!("copied {} bytes", text.len()));
    leave(commands, leaf, next);
}

/// Standard base64 with padding, as OSC 52 carries it.
pub fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let byte = |i: usize| chunk.get(i).copied().map_or(0, u32::from);
        let n = (byte(0) << 16) | (byte(1) << 8) | byte(2);
        for i in 0..4 {
            let symbol = if i <= chunk.len() {
                TABLE.get(((n >> (18 - 6 * i)) & 63) as usize).copied()
            } else {
                None
            };
            out.push(char::from(symbol.unwrap_or(b'=')));
        }
    }
    out
}

/// A capture reply prepends the pane's history to an open view of that pane.
fn on_capture(
    mut replies: MessageReader<BrpReply>,
    panes: Query<&PaneId>,
    grids: Query<&Grid>,
    mut views: Query<(&Shows, &mut CopyView)>,
    mut notice: ResMut<PendingNotice>,
) {
    for reply in replies.read() {
        let BrpTag::Capture(id) = &reply.tag else {
            continue;
        };
        let Some((shows, mut view)) = views
            .iter_mut()
            .find(|(s, _)| panes.get(s.0).is_ok_and(|p| p == id))
        else {
            continue;
        };
        let Ok(grid) = grids.get(shows.0) else {
            continue;
        };
        match &reply.result {
            Ok(value) => {
                let mut lines: Vec<String> = value
                    .get("lines")
                    .and_then(|l| l.as_array())
                    .into_iter()
                    .flatten()
                    .filter_map(|l| l.as_str().map(str::to_owned))
                    .collect();
                // The capture ends with the live screen, which the grid already shows.
                let screen = value
                    .get("rows")
                    .and_then(|r| r.as_u64())
                    .map_or(usize::from(grid.rows()), |r| r as usize);
                lines.truncate(lines.len().saturating_sub(screen));
                if !lines.is_empty() {
                    view.prepend(grid, lines);
                }
            }
            Err(e) => notice.0 = Some(format!("history: {e}")),
        }
    }
}

/// The mode indicator shows the search query and selection state while in copy mode.
fn copy_status(
    mode: Res<State<Mode>>,
    views: Query<&CopyView, Changed<CopyView>>,
    mut texts: Query<&mut Text, With<ModeIndicator>>,
) {
    if *mode.get() != Mode::CopyMode {
        return;
    }
    let Ok(view) = views.single() else {
        return;
    };
    let Ok(mut text) = texts.single_mut() else {
        return;
    };
    let mut label = String::from(" COPY");
    if view.searching {
        label.push_str(" /");
        label.push_str(&view.query);
    } else if !view.query.is_empty() {
        label.push_str(&format!(" {}× {:?}", view.matches.len(), view.query));
    }
    if view.anchor.is_some() {
        label.push_str(" SEL");
    }
    label.push(' ');
    if text.0 != label {
        text.0 = label;
    }
}

pub struct CopyModePlugin;

impl Plugin for CopyModePlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (on_capture, copy_status.after(super::chrome::sync_mode))
                .in_set(super::ViewerSystems::Chrome)
                .before(super::chrome::size_text_nodes),
        );
    }
}
