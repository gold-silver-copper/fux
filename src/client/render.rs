//! Frame compositor: paints one server frame plus viewer-local overlays (history view, selection,
//! popup) into a ratatui buffer, then diffs it against the previous paint.
//!
//! The last row is the bar: workspace, tabs (the current one reversed) and the focused pane's
//! `id: title` or a transient notice, on its own background. Panes have no frame; the one-cell gaps the layout leaves
//! between siblings are drawn as shared separators, bold next to the focused pane.

use super::backend::{CellStyle as BackendStyle, TerminalBackend};
use super::hints::HintPanel;
use crate::config::StyleColor;
use crate::ids::PaneId;
use crate::view::{CellKind, Frame, PaneView};
use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect;
use ratatui_core::style::{Color, Modifier, Style};
use std::io;
use unicode_width::UnicodeWidthStr;

/// A viewer-local replacement for one pane's content: a history viewport and its selection.
pub struct LocalView<'a> {
    pub pane: PaneId,
    pub view: &'a PaneView,
    pub cursor: (u16, u16),
    pub anchor: Option<(u16, u16)>,
}

/// A transient message for the bar's right zone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub text: String,
    pub error: bool,
}

/// Configured colours, resolved for the compositor. `None` means "leave the terminal's colour".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    pub bar: Option<Color>,
    pub bar_background: Option<Color>,
    pub tab_active: Option<Color>,
    pub separator: Option<Color>,
    pub separator_focused: Option<Color>,
    pub notice: Option<Color>,
}

impl Palette {
    #[must_use]
    pub fn from_style(style: &crate::config::Style) -> Self {
        Self {
            bar: color(style.bar),
            bar_background: color(style.bar_background),
            tab_active: color(style.tab_active),
            separator: color(style.separator),
            separator_focused: color(style.separator_focused),
            notice: color(style.notice),
        }
    }
}

impl Default for Palette {
    fn default() -> Self {
        Self::from_style(&crate::config::Style::default())
    }
}

const fn color(value: StyleColor) -> Option<Color> {
    Some(match value {
        StyleColor::None => return None,
        StyleColor::Default => Color::Reset,
        StyleColor::Black => Color::Black,
        StyleColor::Red => Color::Red,
        StyleColor::Green => Color::Green,
        StyleColor::Yellow => Color::Yellow,
        StyleColor::Blue => Color::Blue,
        StyleColor::Magenta => Color::Magenta,
        StyleColor::Cyan => Color::Cyan,
        StyleColor::White => Color::Gray,
        StyleColor::BrightBlack => Color::DarkGray,
        StyleColor::BrightRed => Color::LightRed,
        StyleColor::BrightGreen => Color::LightGreen,
        StyleColor::BrightYellow => Color::LightYellow,
        StyleColor::BrightBlue => Color::LightBlue,
        StyleColor::BrightMagenta => Color::LightMagenta,
        StyleColor::BrightCyan => Color::LightCyan,
        StyleColor::BrightWhite => Color::White,
    })
}

/// Writes `text` at (`x`, `y`) clipped to the buffer; a start outside the buffer paints nothing
/// (ratatui's `set_stringn` indexes the start cell unconditionally).
fn put_str(buffer: &mut Buffer, x: u16, y: u16, text: &str, max_width: u16, style: Style) {
    if x >= buffer.area.right() || y >= buffer.area.bottom() {
        return;
    }
    buffer.set_stringn(x, y, text, usize::from(max_width), style);
}

fn styled(fg: Option<Color>) -> Style {
    fg.map_or_else(Style::default, |fg| Style::default().fg(fg))
}

pub struct Composed {
    pub buffer: Buffer,
    pub cursor: Option<(u16, u16)>,
}

/// Composes `frame` for a terminal of `rows` x `cols`.
pub fn compose(
    frame: &Frame,
    local: Option<&LocalView<'_>>,
    panel: Option<&HintPanel>,
    notice: Option<&Notice>,
    palette: &Palette,
    rows: u16,
    cols: u16,
) -> Composed {
    let mut buffer = Buffer::empty(Rect::new(0, 0, cols, rows));
    let mut cursor = None;
    if rows == 0 || cols == 0 {
        return Composed { buffer, cursor };
    }
    for entry in &frame.layout {
        let content = Rect::new(
            entry.rect.x,
            entry.rect.y,
            entry.rect.width,
            entry.rect.height,
        );
        let focused = frame.focused == Some(entry.pane);
        let Some(pane) = frame.pane(entry.pane) else {
            continue;
        };
        let overlay = local.filter(|local| local.pane == entry.pane);
        let view = overlay.map_or(pane, |local| local.view);
        paint_pane(&mut buffer, content, view);
        if let Some(local) = overlay {
            paint_selection(&mut buffer, content, local);
            if focused && local.cursor.0 < content.height && local.cursor.1 < content.width {
                cursor = Some((
                    content.y.saturating_add(local.cursor.0),
                    content.x.saturating_add(local.cursor.1),
                ));
            }
            continue;
        }
        if focused
            && !pane.cursor.hidden
            && pane.exit.is_none()
            && pane.cursor.row < content.height
            && pane.cursor.column < content.width
        {
            cursor = Some((
                content.y.saturating_add(pane.cursor.row),
                content.x.saturating_add(pane.cursor.column),
            ));
        }
        if let Some(code) = pane.exit
            && !focused
        {
            // The bar reports the focused pane; other exited panes keep a dim marker in their
            // last row so they cannot pass for a quiet live pane.
            paint_exit_marker(&mut buffer, content, code, palette);
        }
    }
    paint_separators(&mut buffer, frame, palette);
    // The bar is painted last so a stale, taller frame (repainted after a shrink) cannot cover it.
    paint_bar(&mut buffer, frame, notice, palette);
    if let Some(panel) = panel {
        // Popups sit above the bar, never on it.
        let above_bar = Rect::new(0, 0, cols, rows.saturating_sub(1));
        panel.paint(&mut buffer, above_bar);
        if !panel.is_thin() || local.is_none() {
            cursor = None;
        }
    }
    Composed { buffer, cursor }
}

fn paint_exit_marker(buffer: &mut Buffer, content: Rect, code: u32, palette: &Palette) {
    if content.height == 0 || content.width == 0 {
        return;
    }
    let label = format!(" exit {code} ");
    let label_width = width_of(&label);
    if label_width > content.width {
        return;
    }
    let x = content
        .x
        .saturating_add(content.width)
        .saturating_sub(label_width);
    let y = content.y.saturating_add(content.height).saturating_sub(1);
    put_str(
        buffer,
        x,
        y,
        &label,
        label_width,
        styled(palette.bar)
            .add_modifier(Modifier::DIM)
            .add_modifier(Modifier::REVERSED),
    );
}

fn width_of(text: &str) -> u16 {
    u16::try_from(text.width()).unwrap_or(u16::MAX)
}

/// Keeps the head of `text` within `width` cells, ending with `…` when something was cut.
fn truncate_tail(text: &str, width: u16) -> String {
    if width_of(text) <= width {
        return text.to_owned();
    }
    let Some(keep) = width.checked_sub(1) else {
        return String::new();
    };
    let mut out = String::new();
    let mut used = 0_u16;
    for grapheme in unicode_segmentation::UnicodeSegmentation::graphemes(text, true) {
        let w = width_of(grapheme);
        if used.saturating_add(w) > keep {
            break;
        }
        out.push_str(grapheme);
        used = used.saturating_add(w);
    }
    out.push('…');
    out
}

/// Keeps the tail of `text` within `width` cells, starting with `…` when something was cut.
fn truncate_head(text: &str, width: u16) -> String {
    if width_of(text) <= width {
        return text.to_owned();
    }
    let Some(keep) = width.checked_sub(1) else {
        return String::new();
    };
    let graphemes: Vec<&str> =
        unicode_segmentation::UnicodeSegmentation::graphemes(text, true).collect();
    let mut kept: Vec<&str> = Vec::new();
    let mut used = 0_u16;
    for grapheme in graphemes.iter().rev() {
        let w = width_of(grapheme);
        if used.saturating_add(w) > keep {
            break;
        }
        kept.push(grapheme);
        used = used.saturating_add(w);
    }
    kept.reverse();
    let mut out = String::from("…");
    out.extend(kept);
    out
}

fn paint_bar(buffer: &mut Buffer, frame: &Frame, notice: Option<&Notice>, palette: &Palette) {
    let width = buffer.area.width;
    let row = buffer.area.height.saturating_sub(1);
    let base = palette
        .bar_background
        .map_or_else(|| styled(palette.bar), |bg| styled(palette.bar).bg(bg));
    for col in 0..width {
        if let Some(cell) = buffer.cell_mut((col, row)) {
            cell.set_symbol(" ").set_style(base);
        }
    }
    // Priority: the current tab, the workspace name, the right zone, then neighbouring tabs. The
    // name is truncated with `…` rather than cut so the current tab always fits when it can.
    let active_width = frame
        .tabs
        .iter()
        .find(|tab| Some(tab.id) == frame.active_tab)
        .map_or(0, |tab| width_of(&tab.label).saturating_add(2));
    let mut left = format!(" {} │", frame.workspace);
    if width_of(&left).saturating_add(active_width) > width {
        // The name never disappears entirely: it keeps at least a quarter of the bar.
        let allowed = width
            .saturating_sub(active_width)
            .max(width / 4)
            .clamp(3, width.max(3));
        left = format!(
            " {} │",
            truncate_tail(&frame.workspace, allowed.saturating_sub(3))
        );
    }
    let (right_text, right_style) = match notice {
        Some(notice) => (
            notice.text.clone(),
            if notice.error {
                base.fg(Color::Red)
            } else {
                palette.notice.map_or(base, |fg| base.fg(fg))
            },
        ),
        None => (focused_label(frame), base),
    };
    // Layout: left, then tabs, then the right zone flush right. The right zone yields first.
    let left_width = width_of(&left).min(width);
    let mut right = if right_text.is_empty() {
        String::new()
    } else {
        format!("│ {right_text} ")
    };
    let mut room = width.saturating_sub(left_width);
    let allowance = room.saturating_sub(active_width.min(room));
    if width_of(&right) > allowance {
        right = if allowance >= 4 {
            // Titles are paths: keep their tail. Notices are sentences: keep their head.
            let text = if notice.is_some() {
                truncate_tail(&right_text, allowance.saturating_sub(3))
            } else {
                truncate_head(&right_text, allowance.saturating_sub(3))
            };
            format!("│ {text} ")
        } else {
            String::new()
        };
    }
    let right_width = width_of(&right);
    room = room.saturating_sub(right_width);
    let mut x = 0_u16;
    put_str(buffer, x, row, &left, left_width, base);
    x = x.saturating_add(left_width);
    for (label, active) in fit_tabs(frame, room) {
        let style = if active {
            palette
                .tab_active
                .map_or(base, |fg| base.fg(fg))
                .add_modifier(Modifier::REVERSED)
        } else {
            base
        };
        let w = width_of(&label);
        put_str(buffer, x, row, &label, w, style);
        x = x.saturating_add(w);
    }
    if right_width > 0 && right_width <= width {
        let start = width - right_width;
        put_str(buffer, start, row, "│ ", 2, base);
        put_str(
            buffer,
            start.saturating_add(2),
            row,
            &right_text_only(&right),
            right_width.saturating_sub(2),
            right_style,
        );
    }
}

fn right_text_only(right: &str) -> String {
    right.strip_prefix("│ ").unwrap_or(right).to_owned()
}

fn focused_label(frame: &Frame) -> String {
    let Some(id) = frame.focused else {
        return String::new();
    };
    let Some(pane) = frame.pane(id) else {
        return id.to_string();
    };
    let title: String = pane.title.chars().filter(|c| !c.is_control()).collect();
    let mut label = if title.is_empty() {
        id.to_string()
    } else {
        format!("{id}: {title}")
    };
    if let Some(code) = pane.exit {
        label.push_str(&format!(" (exit {code})"));
    }
    label
}

/// Chooses which tab labels fit in `room`, the current tab first, then its neighbours outward;
/// the first label that does not fit whole is truncated. Returns labels in tab order.
fn fit_tabs(frame: &Frame, room: u16) -> Vec<(String, bool)> {
    let active = frame
        .tabs
        .iter()
        .position(|tab| Some(tab.id) == frame.active_tab)
        .unwrap_or(0);
    let count = frame.tabs.len();
    let mut chosen: Vec<Option<String>> = vec![None; count];
    let mut used = 0_u16;
    let mut order = Vec::with_capacity(count);
    order.push(active);
    for distance in 1..=count {
        if let Some(index) = active.checked_sub(distance) {
            order.push(index);
        }
        if active + distance < count {
            order.push(active + distance);
        }
    }
    for index in order {
        let Some(tab) = frame.tabs.get(index) else {
            continue;
        };
        let label = format!(" {} ", tab.label);
        let w = width_of(&label);
        let remaining = room.saturating_sub(used);
        if w <= remaining {
            used = used.saturating_add(w);
            if let Some(slot) = chosen.get_mut(index) {
                *slot = Some(label);
            }
        } else {
            if remaining >= 4 {
                let cut = truncate_tail(&tab.label, remaining.saturating_sub(2));
                if let Some(slot) = chosen.get_mut(index) {
                    *slot = Some(format!(" {cut} "));
                }
            }
            break;
        }
    }
    chosen
        .into_iter()
        .enumerate()
        .filter_map(|(index, label)| label.map(|label| (label, index == active)))
        .collect()
}

fn paint_pane(buffer: &mut Buffer, area: Rect, pane: &PaneView) {
    for row in 0..area.height.min(pane.rows) {
        for col in 0..area.width.min(pane.columns) {
            let Some(source) = pane.cell(row, col) else {
                continue;
            };
            if source.kind == CellKind::WideContinuation {
                continue;
            }
            if source.kind == CellKind::WideLeading && col.saturating_add(1) >= area.width {
                continue;
            }
            let Some(target) =
                buffer.cell_mut((area.x.saturating_add(col), area.y.saturating_add(row)))
            else {
                continue;
            };
            target.set_symbol(if source.kind == CellKind::Blank {
                " "
            } else {
                &source.text
            });
            target.set_style(cell_style(source.style));
        }
    }
}

/// Draws the one-cell gaps between panes as shared separators with proper junctions. Gaps exist
/// only inside the bounding box of the layout, so a viewer larger than the negotiated area keeps
/// blank margins.
fn paint_separators(buffer: &mut Buffer, frame: &Frame, palette: &Palette) {
    if frame.layout.len() < 2 {
        return;
    }
    let (mut left, mut top, mut right, mut bottom) = (u16::MAX, u16::MAX, 0_u16, 0_u16);
    for entry in &frame.layout {
        let rect = entry.rect;
        if rect.width == 0 || rect.height == 0 {
            continue;
        }
        left = left.min(rect.x);
        top = top.min(rect.y);
        right = right.max(rect.x.saturating_add(rect.width));
        bottom = bottom.max(rect.y.saturating_add(rect.height));
    }
    if left >= right || top >= bottom {
        return;
    }
    let right = right.min(buffer.area.width);
    let bottom = bottom.min(buffer.area.height);
    if left >= right || top >= bottom {
        return;
    }
    // One pass over the leaves marks pane cells; every lookup after that is a table read.
    let box_width = usize::from(right - left);
    let box_height = usize::from(bottom - top);
    let mut pane_cells = vec![false; box_width.saturating_mul(box_height)];
    for entry in &frame.layout {
        let rect = entry.rect;
        for y in rect.y.max(top)..rect.y.saturating_add(rect.height).min(bottom) {
            for x in rect.x.max(left)..rect.x.saturating_add(rect.width).min(right) {
                let index = usize::from(y - top)
                    .saturating_mul(box_width)
                    .saturating_add(usize::from(x - left));
                if let Some(cell) = pane_cells.get_mut(index) {
                    *cell = true;
                }
            }
        }
    }
    let is_gap = |x: u16, y: u16| {
        if x < left || x >= right || y < top || y >= bottom {
            return false;
        }
        let index = usize::from(y - top)
            .saturating_mul(box_width)
            .saturating_add(usize::from(x - left));
        pane_cells.get(index).is_some_and(|cell| !cell)
    };
    let focused = frame
        .focused
        .and_then(|id| frame.layout.iter().find(|entry| entry.pane == id))
        .map(|entry| entry.rect);
    let touches_focus = |x: u16, y: u16| {
        focused.is_some_and(|rect| {
            x.saturating_add(1) >= rect.x
                && x <= rect.x.saturating_add(rect.width)
                && y.saturating_add(1) >= rect.y
                && y <= rect.y.saturating_add(rect.height)
        })
    };
    for y in top..bottom {
        for x in left..right {
            if !is_gap(x, y) {
                continue;
            }
            let up = y > 0 && is_gap(x, y - 1);
            let down = is_gap(x, y.saturating_add(1));
            let l = x > 0 && is_gap(x - 1, y);
            let r = is_gap(x.saturating_add(1), y);
            let symbol = match (up, down, l, r) {
                (true, true, true, true) => "┼",
                (true, true, true, false) => "┤",
                (true, true, false, true) => "├",
                (false, true, true, true) => "┬",
                (true, false, true, true) => "┴",
                (true, false, false, true) => "└",
                (true, false, true, false) => "┘",
                (false, true, false, true) => "┌",
                (false, true, true, false) => "┐",
                (false, false, true, _) | (false, false, _, true) => "─",
                _ => "│",
            };
            let style = if touches_focus(x, y) {
                styled(palette.separator_focused).add_modifier(Modifier::BOLD)
            } else {
                styled(palette.separator)
            };
            if let Some(cell) = buffer.cell_mut((x, y)) {
                cell.set_symbol(symbol).set_style(style);
            }
        }
    }
}

fn paint_selection(buffer: &mut Buffer, content: Rect, local: &LocalView<'_>) {
    let Some(anchor) = local.anchor else {
        return;
    };
    if content.width == 0 || content.height == 0 {
        return;
    }
    let clamp = |point: (u16, u16)| {
        (
            point.0.min(content.height - 1),
            point.1.min(content.width - 1),
        )
    };
    let cursor = clamp(local.cursor);
    let anchor = clamp(anchor);
    let (start, end) = if anchor <= cursor {
        (anchor, cursor)
    } else {
        (cursor, anchor)
    };
    for row in start.0..=end.0 {
        let first = if row == start.0 { start.1 } else { 0 };
        let last = if row == end.0 {
            end.1
        } else {
            content.width - 1
        };
        for column in first..=last {
            if let Some(cell) = buffer.cell_mut((content.x + column, content.y + row)) {
                cell.modifier.insert(Modifier::REVERSED);
            }
        }
    }
}

fn cell_style(style: crate::view::CellStyle) -> Style {
    let mut modifiers = Modifier::empty();
    if style.bold {
        modifiers.insert(Modifier::BOLD);
    }
    if style.dim {
        modifiers.insert(Modifier::DIM);
    }
    if style.italic {
        modifiers.insert(Modifier::ITALIC);
    }
    if style.underline {
        modifiers.insert(Modifier::UNDERLINED);
    }
    if style.inverse {
        modifiers.insert(Modifier::REVERSED);
    }
    Style::default()
        .fg(rat_color(style.foreground))
        .bg(rat_color(style.background))
        .add_modifier(modifiers)
}

const fn rat_color(color: crate::view::Color) -> Color {
    match color {
        crate::view::Color::Default => Color::Reset,
        crate::view::Color::Indexed(index) => Color::Indexed(index),
        crate::view::Color::Rgb(red, green, blue) => Color::Rgb(red, green, blue),
    }
}

/// Paints the difference between `previous` and `next`, positions the cursor and flushes.
pub fn paint<B: TerminalBackend>(
    backend: &mut B,
    previous: Option<&Buffer>,
    next: &Buffer,
    cursor: Option<(u16, u16)>,
) -> io::Result<()> {
    backend.begin_frame()?;
    let painted: io::Result<()> = (|| {
        let empty = Buffer::empty(next.area);
        let previous = previous.filter(|previous| previous.area == next.area);
        if previous.is_none() {
            backend.write_bytes(b"\x1b[2J")?;
        }
        let previous = previous.unwrap_or(&empty);
        for (col, row, cell) in previous.diff(next) {
            backend.move_to(row, col)?;
            backend.set_style(backend_style(cell))?;
            backend.print(cell.symbol())?;
        }
        if let Some((row, col)) = cursor {
            backend.move_to(row, col)?;
            backend.show_cursor()?;
        } else {
            backend.write_bytes(b"\x1b[?25l")?;
        }
        Ok(())
    })();
    let ended = backend.end_frame();
    painted?;
    ended?;
    backend.flush()
}

fn backend_style(cell: &ratatui_core::buffer::Cell) -> BackendStyle {
    BackendStyle {
        fg: rat_to_vt(cell.fg),
        bg: rat_to_vt(cell.bg),
        bold: cell.modifier.contains(Modifier::BOLD),
        dim: cell.modifier.contains(Modifier::DIM),
        italic: cell.modifier.contains(Modifier::ITALIC),
        underline: cell.modifier.contains(Modifier::UNDERLINED),
        inverse: cell.modifier.contains(Modifier::REVERSED),
    }
}

const fn rat_to_vt(color: Color) -> vt100::Color {
    match color {
        Color::Reset => vt100::Color::Default,
        Color::Black => vt100::Color::Idx(0),
        Color::Red => vt100::Color::Idx(1),
        Color::Green => vt100::Color::Idx(2),
        Color::Yellow => vt100::Color::Idx(3),
        Color::Blue => vt100::Color::Idx(4),
        Color::Magenta => vt100::Color::Idx(5),
        Color::Cyan => vt100::Color::Idx(6),
        Color::Gray => vt100::Color::Idx(7),
        Color::DarkGray => vt100::Color::Idx(8),
        Color::LightRed => vt100::Color::Idx(9),
        Color::LightGreen => vt100::Color::Idx(10),
        Color::LightYellow => vt100::Color::Idx(11),
        Color::LightBlue => vt100::Color::Idx(12),
        Color::LightMagenta => vt100::Color::Idx(13),
        Color::LightCyan => vt100::Color::Idx(14),
        Color::White => vt100::Color::Idx(15),
        Color::Indexed(value) => vt100::Color::Idx(value),
        Color::Rgb(r, g, b) => vt100::Color::Rgb(r, g, b),
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;
    use crate::ids::TabId;
    use crate::view::{PaneRect, TabEntry};

    fn view(text: &[u8], rows: u16, cols: u16) -> PaneView {
        let mut parser = vt100::Parser::new(rows, cols, 0);
        parser.process(text);
        PaneView::from_screen(parser.screen(), "shell", 0, None).unwrap_or_default()
    }

    fn frame(tabs: usize) -> Frame {
        let mut frame = Frame {
            workspace: "default".into(),
            ..Frame::default()
        };
        for index in 0..tabs {
            frame.tabs.push(TabEntry {
                id: TabId(index as u32 + 1),
                label: format!("t{index}"),
            });
        }
        frame.active_tab = Some(TabId(1));
        frame.focused = Some(PaneId(1));
        frame.layout.push(PaneRect {
            pane: PaneId(1),
            rect: crate::layout::Rect {
                x: 0,
                y: 0,
                width: 10,
                height: 4,
            },
        });
        frame.panes.insert(PaneId(1), view(b"hello", 4, 10));
        frame
    }

    fn split_frame() -> Frame {
        let mut frame = frame(1);
        frame.layout.clear();
        // Two side-by-side panes with a one-cell gap, each split again vertically on the right.
        frame.layout.push(PaneRect {
            pane: PaneId(1),
            rect: crate::layout::Rect {
                x: 0,
                y: 0,
                width: 4,
                height: 5,
            },
        });
        frame.layout.push(PaneRect {
            pane: PaneId(2),
            rect: crate::layout::Rect {
                x: 5,
                y: 0,
                width: 5,
                height: 2,
            },
        });
        frame.layout.push(PaneRect {
            pane: PaneId(3),
            rect: crate::layout::Rect {
                x: 5,
                y: 3,
                width: 5,
                height: 2,
            },
        });
        frame.panes.insert(PaneId(1), view(b"aaaa", 5, 4));
        frame.panes.insert(PaneId(2), view(b"bb", 2, 5));
        frame.panes.insert(PaneId(3), view(b"cc", 2, 5));
        frame
    }

    fn text(buffer: &Buffer) -> Vec<String> {
        (0..buffer.area.height)
            .map(|row| {
                (0..buffer.area.width)
                    .map(|col| buffer.cell((col, row)).map_or(" ", |cell| cell.symbol()))
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn bar_shows_workspace_tab_and_focused_pane_without_any_frame() {
        let composed = compose(&frame(1), None, None, None, &Palette::default(), 5, 30);
        let lines = text(&composed.buffer);
        let bar = &lines[4];
        assert!(bar.starts_with(" default │ t0 "), "{bar:?}");
        assert!(bar.ends_with("│ 1: shell "), "{bar:?}");
        assert_eq!(bar.chars().count(), 30);
        assert!(lines[0].starts_with("hello"));
        assert!(
            !lines
                .iter()
                .any(|line| line.contains(['┌', '┐', '└', '┘', '│'].as_slice()) && line != bar)
        );
        assert_eq!(composed.cursor, Some((0, 5)));
        let active = composed.buffer.cell((11, 4));
        assert_eq!(active.map(|cell| cell.modifier), Some(Modifier::REVERSED));
        assert_eq!(
            active.map(|cell| cell.bg),
            Some(Color::DarkGray),
            "own background"
        );
    }

    #[test]
    fn bar_truncates_from_the_right_zone_first_and_keeps_the_current_tab() {
        let mut frame = frame(3);
        frame.active_tab = Some(TabId(2));
        frame.tabs[1].label = "a-very-long-tab-label".into();
        let composed = compose(&frame, None, None, None, &Palette::default(), 3, 24);
        let line = &text(&composed.buffer)[2];
        // 24 cells: the name shrinks to its quarter share, the current tab takes the rest and
        // the right zone is dropped rather than squeezing the tab.
        assert_eq!(line.trim_end(), " de… │ a-very-long-tab…");
        assert!(
            !line.contains("t0"),
            "neighbours yield before the current tab: {line:?}"
        );
    }

    #[test]
    fn notices_replace_the_pane_title_in_the_bar() {
        let notice = Notice {
            text: "Copied 5 bytes".into(),
            error: false,
        };
        let composed = compose(
            &frame(1),
            None,
            None,
            Some(&notice),
            &Palette::default(),
            3,
            40,
        );
        let line = &text(&composed.buffer)[2];
        assert!(line.ends_with("│ Copied 5 bytes "), "{line:?}");
        assert!(!line.contains("shell"));
    }

    #[test]
    fn separators_join_between_panes_and_brighten_next_to_focus() {
        let frame = split_frame();
        let composed = compose(&frame, None, None, None, &Palette::default(), 6, 12);
        let lines = text(&composed.buffer);
        assert!(lines[0].starts_with("aaaa│bb"), "{:?}", lines[0]);
        assert!(lines[2].starts_with("    ├─────"), "{:?}", lines[2]);
        assert!(lines[3].starts_with("    │cc"), "{:?}", lines[3]);
        // 12 cells: the name shrinks so the current tab fits; the bar is the last row.
        assert_eq!(lines[5].trim_end(), " defa… │ t0", "bar on the last row");
        assert!(lines.iter().all(|line| !line.contains('┌')));
        let bold = |x: u16, y: u16| {
            composed
                .buffer
                .cell((x, y))
                .is_some_and(|cell| cell.modifier.contains(Modifier::BOLD))
        };
        assert!(bold(4, 0), "separator next to the focused pane is bold");
        assert!(bold(4, 2), "the junction touches the focused pane too");
        let mut other = frame;
        other.focused = Some(PaneId(3));
        let composed = compose(&other, None, None, None, &Palette::default(), 6, 12);
        let bold_now = composed
            .buffer
            .cell((4, 0))
            .is_some_and(|cell| cell.modifier.contains(Modifier::BOLD));
        assert!(!bold_now, "far separator is dim when focus moved");
        assert!(
            composed
                .buffer
                .cell((6, 2))
                .is_some_and(|cell| cell.modifier.contains(Modifier::BOLD)),
            "the row separator above pane 3 is bold"
        );
    }

    #[test]
    fn tiny_and_zero_terminals_never_panic() {
        // An unfocused pane that exited sits below the visible rows after a shrink.
        let mut stale = split_frame();
        if let Some(view) = stale.panes.get_mut(&PaneId(3)) {
            view.exit = Some(1);
        }
        for (rows, cols) in [(0, 0), (1, 1), (2, 3), (1, 80), (24, 1), (2, 20), (3, 12)] {
            let composed = compose(
                &stale,
                None,
                Some(&HintPanel::bar("x")),
                None,
                &Palette::default(),
                rows,
                cols,
            );
            assert_eq!(composed.buffer.area.height, rows);
            let mut backend = super::super::backend::CaptureBackend::new(rows, cols);
            paint(&mut backend, None, &composed.buffer, composed.cursor).unwrap_or_default();
        }
    }
}
