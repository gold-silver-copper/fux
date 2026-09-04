use crate::client::{active_tab, rat_color};
use crate::state::{CellKind, PaneId, Rect as LayoutRect, WorkspaceState};
use koh::predict::Overlay;
use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect;
use ratatui_core::style::{Color, Modifier, Style};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Selection {
    pub start: (u16, u16),
    pub end: (u16, u16),
}

#[derive(Clone)]
pub struct ComposedFrame {
    pub buffer: Buffer,
    pub cursor: Option<(u16, u16)>,
    pub pane_rects: BTreeMap<PaneId, Rect>,
}

#[derive(Default)]
pub struct Compositor {
    selection: Option<Selection>,
}

impl Compositor {
    pub fn set_selection(&mut self, selection: Option<Selection>) {
        self.selection = selection;
    }

    pub fn compose(
        &self,
        state: &WorkspaceState,
        overlay: &Overlay,
        status: Option<&str>,
        rows: u16,
        cols: u16,
    ) -> ComposedFrame {
        let mut buffer = Buffer::empty(Rect::new(0, 0, cols, rows));
        let mut pane_rects = BTreeMap::new();
        let mut cursor = None;
        if rows == 0 || cols == 0 {
            return ComposedFrame {
                buffer,
                cursor,
                pane_rects,
            };
        }
        let body_height = rows.saturating_sub(1);
        let has_popup = !state.popups().is_empty();
        if let Some(tab) = active_tab(state) {
            let area = LayoutRect {
                x: 0,
                y: 0,
                width: cols,
                height: body_height,
            };
            let geometry = if let Some(zoomed) = tab.zoomed {
                vec![(zoomed, area)]
            } else {
                tab.layout.geometry(area).unwrap_or_default()
            };
            for (id, outer) in geometry {
                let rect = Rect::new(outer.x, outer.y, outer.width, outer.height);
                pane_rects.insert(id, rect);
                let blocked = state
                    .pane(id)
                    .is_some_and(|pane| pane.agent.state == crate::state::AgentState::Blocked);
                draw_border(&mut buffer, rect, id == tab.focused, blocked);
                let content = inner(rect);
                if let Some(pane) = state.pane(id).filter(|pane| pane.valid()) {
                    paint_pane(&mut buffer, content, pane);
                    if id == tab.focused && !has_popup {
                        paint_overlay(&mut buffer, content, overlay);
                        let relative = if pane.copy.active {
                            let start = pane
                                .copy
                                .anchor
                                .unwrap_or((pane.copy.cursor_row, pane.copy.cursor_column));
                            paint_selection(
                                &mut buffer,
                                Selection {
                                    start: (
                                        content.y.saturating_add(start.0),
                                        content.x.saturating_add(start.1),
                                    ),
                                    end: (
                                        content.y.saturating_add(pane.copy.cursor_row),
                                        content.x.saturating_add(pane.copy.cursor_column),
                                    ),
                                },
                            );
                            (pane.copy.cursor_row, pane.copy.cursor_column)
                        } else {
                            overlay
                                .cursor()
                                .unwrap_or((pane.cursor.row, pane.cursor.column))
                        };
                        if (pane.copy.active || !pane.cursor.hidden)
                            && relative.0 < content.height
                            && relative.1 < content.width
                        {
                            cursor = Some((
                                content.y.saturating_add(relative.0),
                                content.x.saturating_add(relative.1),
                            ));
                        }
                    }
                }
            }
        }
        let mut popups = state.popups().to_vec();
        popups.sort_by_key(|popup| popup.z_index);
        let top = popups.len().saturating_sub(1);
        for (index, popup) in popups.into_iter().enumerate() {
            let width = popup.width.min(cols);
            let height = popup.height.min(body_height);
            let rect = Rect::new(
                cols.saturating_sub(width) / 2,
                body_height.saturating_sub(height) / 2,
                width,
                height,
            );
            let blocked = state
                .pane(popup.pane)
                .is_some_and(|pane| pane.agent.state == crate::state::AgentState::Blocked);
            draw_border(&mut buffer, rect, true, blocked);
            if let Some(pane) = state.pane(popup.pane).filter(|pane| pane.valid()) {
                let content = inner(rect);
                paint_pane(&mut buffer, content, pane);
                if index == top {
                    paint_overlay(&mut buffer, content, overlay);
                }
                if !pane.cursor.hidden
                    && pane.cursor.row < content.height
                    && pane.cursor.column < content.width
                {
                    cursor = Some((
                        content.y.saturating_add(pane.cursor.row),
                        content.x.saturating_add(pane.cursor.column),
                    ));
                }
            }
        }
        if let Some(selection) = self.selection {
            paint_selection(&mut buffer, selection);
        }
        draw_status(&mut buffer, state, status, rows.saturating_sub(1));
        ComposedFrame {
            buffer,
            cursor,
            pane_rects,
        }
    }

    pub fn selected_text(&self, frame: &ComposedFrame) -> String {
        let Some(selection) = self.selection else {
            return String::new();
        };
        let ((start_row, start_col), (end_row, end_col)) = ordered(selection);
        let mut output = String::new();
        for row in start_row..=end_row.min(frame.buffer.area.height.saturating_sub(1)) {
            let first = if row == start_row { start_col } else { 0 };
            let last = if row == end_row {
                end_col
            } else {
                frame.buffer.area.width.saturating_sub(1)
            };
            for col in first..=last.min(frame.buffer.area.width.saturating_sub(1)) {
                if let Some(cell) = frame.buffer.cell((col, row)) {
                    output.push_str(cell.symbol());
                }
            }
            if row != end_row {
                output.push('\n');
            }
        }
        output.trim_end_matches(' ').to_owned()
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

fn paint_pane(buffer: &mut Buffer, area: Rect, pane: &crate::state::PaneView) {
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

fn paint_overlay(buffer: &mut Buffer, area: Rect, overlay: &Overlay) {
    for row in 0..area.height {
        for col in 0..area.width {
            let Some(prediction) = overlay.cell(row, col) else {
                continue;
            };
            let Some(target) =
                buffer.cell_mut((area.x.saturating_add(col), area.y.saturating_add(row)))
            else {
                continue;
            };
            if !prediction.unknown && !prediction.glyph.is_empty() {
                target.set_symbol(&prediction.glyph);
            }
            target.fg = vt_to_rat(prediction.fg);
            target.bg = vt_to_rat(prediction.bg);
            if prediction.underline {
                target.modifier.insert(Modifier::UNDERLINED);
            }
        }
    }
}

fn cell_style(style: crate::state::CellStyle) -> Style {
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

fn draw_border(buffer: &mut Buffer, rect: Rect, focused: bool, blocked: bool) {
    if rect.width == 0 || rect.height == 0 {
        return;
    }
    let style = if blocked {
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
    } else if focused {
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
}

fn draw_status(buffer: &mut Buffer, state: &WorkspaceState, banner: Option<&str>, row: u16) {
    let text = banner.map(str::to_owned).unwrap_or_else(|| {
        let active = state.active_tab();
        let tabs = state
            .tabs()
            .iter()
            .map(|tab| {
                if Some(tab.id) == active {
                    format!("[{}]", tab.name)
                } else {
                    tab.name.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        let segments = state
            .metadata()
            .status
            .values()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(" ");
        let agents = state
            .panes()
            .iter()
            .filter(|(_, pane)| pane.agent.state != crate::state::AgentState::None)
            .map(|(id, pane)| {
                format!(
                    "#{} {}:{:?}",
                    id.0,
                    pane.agent.id.as_deref().unwrap_or("agent"),
                    pane.agent.state
                )
            })
            .collect::<Vec<_>>()
            .join(" ");
        format!("{tabs} {agents} {segments}").trim().to_owned()
    });
    for col in 0..buffer.area.width {
        set(
            buffer,
            col,
            row,
            " ",
            Style::default().add_modifier(Modifier::REVERSED),
        );
    }
    buffer.set_stringn(
        0,
        row,
        text,
        usize::from(buffer.area.width),
        Style::default().add_modifier(Modifier::REVERSED),
    );
}

fn paint_selection(buffer: &mut Buffer, selection: Selection) {
    let ((start_row, start_col), (end_row, end_col)) = ordered(selection);
    for row in start_row..=end_row.min(buffer.area.height.saturating_sub(1)) {
        let first = if row == start_row { start_col } else { 0 };
        let last = if row == end_row {
            end_col
        } else {
            buffer.area.width.saturating_sub(1)
        };
        for col in first..=last.min(buffer.area.width.saturating_sub(1)) {
            if let Some(cell) = buffer.cell_mut((col, row)) {
                cell.modifier.insert(Modifier::REVERSED);
            }
        }
    }
}

fn ordered(selection: Selection) -> ((u16, u16), (u16, u16)) {
    let first = (selection.start.0, selection.start.1);
    let second = (selection.end.0, selection.end.1);
    if first <= second {
        (first, second)
    } else {
        (second, first)
    }
}

fn set(buffer: &mut Buffer, x: u16, y: u16, symbol: &str, style: Style) {
    if let Some(cell) = buffer.cell_mut((x, y)) {
        cell.set_symbol(symbol).set_style(style);
    }
}

const fn vt_to_rat(color: vt100::Color) -> Color {
    match color {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(value) => Color::Indexed(value),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}
