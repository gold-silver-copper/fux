//! The keybinding popup and the small panels that share its presentation: a compact panel near
//! the bottom of the terminal, painted into the composed frame buffer.

use crate::commands::{Action, ClientBindings, Group, key_name};
use crate::view::Frame;
use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect;
use ratatui_core::style::{Color, Modifier, Style};
use std::collections::BTreeSet;
use unicode_segmentation::UnicodeSegmentation as _;
use unicode_width::UnicodeWidthStr as _;

/// At most this many body rows; larger lists page.
pub const MAX_BODY_ROWS: usize = 10;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HintPanel {
    title: String,
    entries: Vec<String>,
    page: usize,
    footer: Option<String>,
    focus: Option<usize>,
    thin: bool,
    input: bool,
    headings: BTreeSet<usize>,
    disabled: BTreeSet<usize>,
}

impl HintPanel {
    /// The command popup: actual bindings grouped, with unavailable actions dimmed.
    pub fn commands(
        bindings: &ClientBindings,
        workspaces: bool,
        page: usize,
        frame: &Frame,
    ) -> Self {
        let mut entries = Vec::new();
        let mut headings = BTreeSet::new();
        let mut disabled = BTreeSet::new();
        let mut previous: Option<Group> = None;
        for (key, action) in bindings.entries() {
            let group = action.group();
            if previous != Some(group) {
                headings.insert(entries.len());
                entries.push(group.label().to_owned());
                previous = Some(group);
            }
            if action.unavailable(frame, workspaces).is_some() {
                disabled.insert(entries.len());
            }
            entries.push(format!("{}  {}", key_name(key), action.label()));
        }
        Self {
            title: format!(
                "Commands ({}) · workspace {}",
                key_name(bindings.prefix()),
                frame.workspace
            ),
            entries,
            page,
            footer: None,
            focus: None,
            thin: false,
            input: false,
            headings,
            disabled,
        }
    }

    pub fn context(
        title: String,
        entries: Vec<String>,
        footer: &str,
        focus: Option<usize>,
    ) -> Self {
        let clean = |value: String| value.chars().filter(|c| !c.is_control()).collect();
        Self {
            title: clean(title),
            entries: entries.into_iter().map(clean).collect(),
            page: 0,
            footer: Some(footer.into()),
            focus,
            thin: false,
            input: false,
            headings: BTreeSet::new(),
            disabled: BTreeSet::new(),
        }
    }

    /// One reversed line at the bottom, for transient hints.
    pub fn bar(text: &str) -> Self {
        let mut panel = Self::context(String::new(), Vec::new(), text, None);
        panel.thin = true;
        panel
    }

    pub fn text_input(title: &str, text: &str, footer: &str) -> Self {
        let mut panel = Self::context(title.into(), vec![format!("{text}▏")], footer, None);
        panel.input = true;
        panel
    }

    #[must_use]
    pub fn page_count(&self, rows: u16) -> usize {
        let available = usize::from(rows.saturating_sub(2).max(1));
        let body = self.entries.len().max(1).min(available).min(MAX_BODY_ROWS);
        self.entries.len().div_ceil(body).max(1)
    }

    /// The action the user is being asked to confirm, if this panel is a confirmation.
    #[must_use]
    pub fn is_thin(&self) -> bool {
        self.thin
    }

    /// Paints the panel at the bottom of `area` (the compositor passes everything above the bar).
    pub fn paint(&self, buffer: &mut Buffer, area: Rect) {
        let area = area.intersection(buffer.area);
        if area.width == 0 || area.height == 0 {
            return;
        }
        let style = Style::reset().fg(Color::White).bg(Color::DarkGray);
        if self.thin {
            let row = area.bottom() - 1;
            for x in area.x..area.right() {
                if let Some(cell) = buffer.cell_mut((x, row)) {
                    cell.set_symbol(" ").set_style(style);
                }
            }
            buffer.set_stringn(
                area.x,
                row,
                self.footer.as_deref().unwrap_or_default(),
                usize::from(area.width),
                style,
            );
            return;
        }
        let available = usize::from(area.height.saturating_sub(2).max(1));
        let rows = self.entries.len().max(1).min(available).min(MAX_BODY_ROWS);
        let pages = self.entries.len().div_ceil(rows).max(1);
        let page = self
            .focus
            .map_or(self.page % pages, |focus| (focus / rows).min(pages - 1));
        let start = page * rows;
        let height = u16::try_from(rows + 2)
            .unwrap_or(area.height)
            .min(area.height);
        let top = area.bottom().saturating_sub(height);
        for y in top..area.bottom() {
            for x in area.x..area.right() {
                if let Some(cell) = buffer.cell_mut((x, y)) {
                    cell.set_symbol(" ").set_style(style);
                }
            }
        }
        if height > 2 {
            buffer.set_stringn(
                area.x,
                top,
                &self.title,
                usize::from(area.width),
                style.add_modifier(Modifier::BOLD),
            );
        }
        let body_top = top + u16::from(height > 2);
        let body_rows =
            usize::from(height.saturating_sub(u16::from(height > 2) + u16::from(height > 1)));
        let width = usize::from(area.width);
        for (index, entry) in self.entries.iter().skip(start).take(body_rows).enumerate() {
            let text = if self.input {
                // Keep the insertion point visible on narrow terminals: show the tail.
                let mut used = 0;
                let mut cut = entry.len();
                for (offset, grapheme) in entry.grapheme_indices(true).rev() {
                    used += grapheme.width();
                    if used > width {
                        break;
                    }
                    cut = offset;
                }
                entry.get(cut..).unwrap_or_default()
            } else {
                entry.as_str()
            };
            let entry_style = if self.headings.contains(&(start + index)) {
                style.add_modifier(Modifier::BOLD)
            } else if self.disabled.contains(&(start + index)) {
                style.add_modifier(Modifier::DIM)
            } else if self.focus == Some(start + index) {
                style.add_modifier(Modifier::REVERSED)
            } else {
                style
            };
            buffer.set_stringn(
                area.x,
                body_top + u16::try_from(index).unwrap_or(0),
                text,
                width,
                entry_style,
            );
        }
        if height > 1 {
            let default_footer = format!(
                "Esc back · ↑/↓ page {}/{} · dim = unavailable · prefix twice = literal",
                page + 1,
                pages
            );
            buffer.set_stringn(
                area.x,
                area.bottom() - 1,
                self.footer.as_deref().unwrap_or(&default_footer),
                width,
                style,
            );
        }
    }
}

/// A convenience for tests and the controller: labels of every entry as painted.
pub fn action_label(action: Action) -> &'static str {
    action.label()
}

#[cfg(test)]
#[allow(clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;
    use ratatui_core::layout::Rect;

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
    fn command_popup_lists_every_binding_across_pages_and_dims_unavailable() {
        let bindings = ClientBindings::default();
        let frame = Frame::default();
        let panel = HintPanel::commands(&bindings, true, 0, &frame);
        let mut seen = String::new();
        let pages = panel.page_count(6);
        assert!(pages > 3, "tiny screens page");
        for page in 0..pages {
            let panel = HintPanel::commands(&bindings, true, page, &frame);
            let mut buffer = Buffer::empty(Rect::new(0, 0, 40, 6));
            {
                let area = buffer.area;
                panel.paint(&mut buffer, area);
            }
            seen.push_str(&text(&buffer).join("\n"));
        }
        for (key, action) in bindings.entries() {
            assert!(
                seen.contains(action.label()),
                "{action:?} missing from pages"
            );
            assert!(seen.contains(&key_name(key)));
        }
        let mut buffer = Buffer::empty(Rect::new(0, 0, 60, 24));
        {
            let area = buffer.area;
            panel.paint(&mut buffer, area);
        }
        let lines = text(&buffer);
        assert!(lines.iter().any(|line| line.contains("Commands (C-a)")));
        // Unavailable actions are dimmed: the empty frame has no split.
        let dim_rows = (0..24)
            .filter(|row| {
                (0..60).any(|col| {
                    buffer
                        .cell((col, *row))
                        .is_some_and(|cell| cell.modifier.contains(Modifier::DIM))
                })
            })
            .count();
        assert!(dim_rows > 0);
    }

    #[test]
    fn one_cell_terminals_are_safe_and_thin_bars_take_the_last_row() {
        let panel = HintPanel::bar("Copy · q finish");
        let mut buffer = Buffer::empty(Rect::new(0, 0, 1, 1));
        {
            let area = buffer.area;
            panel.paint(&mut buffer, area);
        }
        let mut buffer = Buffer::empty(Rect::new(0, 0, 20, 3));
        {
            let area = buffer.area;
            panel.paint(&mut buffer, area);
        }
        assert!(text(&buffer)[2].starts_with("Copy"));
        let input = HintPanel::text_input("Rename tab", "a-very-long-label-indeed", "Enter save");
        let mut buffer = Buffer::empty(Rect::new(0, 0, 8, 4));
        {
            let area = buffer.area;
            input.paint(&mut buffer, area);
        }
        assert!(
            text(&buffer)[2].contains("▏"),
            "insertion marker stays visible"
        );
    }
}
