//! Frame compositor: paints one server frame plus viewer-local overlays (history view, selection,
//! tab strip, popup) into a ratatui buffer, then diffs it against the previous paint.

use super::backend::{CellStyle as BackendStyle, TerminalBackend};
use super::hints::HintPanel;
use crate::ids::PaneId;
use crate::view::{CellKind, Frame, PaneView};
use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect;
use ratatui_core::style::{Color, Modifier, Style};
use std::io;

/// A viewer-local replacement for one pane's content: a history viewport and its selection.
pub struct LocalView<'a> {
    pub pane: PaneId,
    pub view: &'a PaneView,
    pub cursor: (u16, u16),
    pub anchor: Option<(u16, u16)>,
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
    rows: u16,
    cols: u16,
) -> Composed {
    let mut buffer = Buffer::empty(Rect::new(0, 0, cols, rows));
    let mut cursor = None;
    if rows == 0 || cols == 0 {
        return Composed { buffer, cursor };
    }
    if frame.tabs.len() > 1 {
        paint_tab_strip(&mut buffer, frame);
    }
    for entry in &frame.layout {
        let rect = Rect::new(
            entry.rect.x,
            entry.rect.y,
            entry.rect.width,
            entry.rect.height,
        );
        let focused = frame.focused == Some(entry.pane);
        draw_border(&mut buffer, rect, focused);
        let content = inner(rect);
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
        if let Some(code) = pane.exit {
            let label = format!(" exited {code} ");
            let x = rect.x.saturating_add(1);
            let y = rect.y;
            buffer.set_stringn(
                x,
                y,
                &label,
                usize::from(rect.width.saturating_sub(2)),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            );
        }
    }
    if let Some(panel) = panel {
        panel.paint(&mut buffer);
        if !panel.is_thin() || local.is_none() {
            cursor = None;
        }
    }
    Composed { buffer, cursor }
}

fn paint_tab_strip(buffer: &mut Buffer, frame: &Frame) {
    let base = Style::default().add_modifier(Modifier::REVERSED);
    for col in 0..buffer.area.width {
        if let Some(cell) = buffer.cell_mut((col, 0)) {
            cell.set_symbol(" ").set_style(base);
        }
    }
    let mut x: u16 = 0;
    let width = buffer.area.width;
    for tab in &frame.tabs {
        let active = frame.active_tab == Some(tab.id);
        let label = if active {
            format!("[{}]", tab.label)
        } else {
            format!(" {} ", tab.label)
        };
        let style = if active {
            base.add_modifier(Modifier::BOLD)
        } else {
            base
        };
        if x >= width {
            break;
        }
        buffer.set_stringn(x, 0, &label, usize::from(width - x), style);
        x = x.saturating_add(
            u16::try_from(unicode_width::UnicodeWidthStr::width(label.as_str()))
                .unwrap_or(u16::MAX),
        );
    }
    let name = format!(" {} ", frame.workspace);
    let name_width =
        u16::try_from(unicode_width::UnicodeWidthStr::width(name.as_str())).unwrap_or(0);
    if x.saturating_add(name_width) < width {
        buffer.set_stringn(
            width - name_width,
            0,
            &name,
            usize::from(name_width),
            base.add_modifier(Modifier::DIM),
        );
    }
}

fn inner(rect: Rect) -> Rect {
    Rect::new(
        rect.x.saturating_add(1),
        rect.y.saturating_add(1),
        rect.width.saturating_sub(2),
        rect.height.saturating_sub(2),
    )
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

fn draw_border(buffer: &mut Buffer, rect: Rect, focused: bool) {
    if rect.width == 0 || rect.height == 0 {
        return;
    }
    let style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    for x in rect.x..rect.x.saturating_add(rect.width) {
        set(buffer, x, rect.y, "─", style);
        set(
            buffer,
            x,
            rect.y.saturating_add(rect.height.saturating_sub(1)),
            "─",
            style,
        );
    }
    for y in rect.y..rect.y.saturating_add(rect.height) {
        set(buffer, rect.x, y, "│", style);
        set(
            buffer,
            rect.x.saturating_add(rect.width.saturating_sub(1)),
            y,
            "│",
            style,
        );
    }
    let corners = [
        (rect.x, rect.y, "┌"),
        (
            rect.x.saturating_add(rect.width.saturating_sub(1)),
            rect.y,
            "┐",
        ),
        (
            rect.x,
            rect.y.saturating_add(rect.height.saturating_sub(1)),
            "└",
        ),
        (
            rect.x.saturating_add(rect.width.saturating_sub(1)),
            rect.y.saturating_add(rect.height.saturating_sub(1)),
            "┘",
        ),
    ];
    if rect.width > 1 && rect.height > 1 {
        for (x, y, symbol) in corners {
            set(buffer, x, y, symbol, style);
        }
    }
}

fn set(buffer: &mut Buffer, x: u16, y: u16, symbol: &str, style: Style) {
    if let Some(cell) = buffer.cell_mut((x, y)) {
        cell.set_symbol(symbol).set_style(style);
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

    fn frame(tabs: usize) -> Frame {
        let mut parser = vt100::Parser::new(3, 8, 0);
        parser.process(b"hello");
        let view = PaneView::from_screen(parser.screen(), "", 0, None).unwrap_or_default();
        let mut frame = Frame::default();
        for index in 0..tabs {
            frame.tabs.push(TabEntry {
                id: TabId(index as u32 + 1),
                label: format!("t{index}"),
            });
        }
        frame.active_tab = Some(TabId(1));
        frame.focused = Some(PaneId(1));
        let y = u16::from(tabs > 1);
        frame.layout.push(PaneRect {
            pane: PaneId(1),
            rect: crate::layout::Rect {
                x: 0,
                y,
                width: 10,
                height: 5,
            },
        });
        frame.panes.insert(PaneId(1), view);
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
    fn single_tab_has_no_strip_and_focused_pane_shows_cursor() {
        let composed = compose(&frame(1), None, None, 5, 10);
        let lines = text(&composed.buffer);
        assert!(lines[0].starts_with("┌"));
        assert!(lines[1].contains("hello"));
        assert_eq!(composed.cursor, Some((1, 6)));
    }

    #[test]
    fn multiple_tabs_show_one_strip_line() {
        let composed = compose(&frame(2), None, None, 6, 20);
        let lines = text(&composed.buffer);
        assert!(lines[0].contains("[t0]"));
        assert!(lines[0].contains(" t1 "));
        assert!(lines[1].starts_with("┌"));
    }

    #[test]
    fn tiny_and_zero_terminals_never_panic() {
        for (rows, cols) in [(0, 0), (1, 1), (2, 3), (1, 80), (24, 1)] {
            let composed = compose(&frame(2), None, Some(&HintPanel::bar("x")), rows, cols);
            assert_eq!(composed.buffer.area.height, rows);
            let mut backend = super::super::backend::CaptureBackend::new(rows, cols);
            paint(&mut backend, None, &composed.buffer, composed.cursor).unwrap_or_default();
        }
    }
}
