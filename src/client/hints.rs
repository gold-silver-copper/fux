//! Viewer-local contextual panels, painted into the existing ratatui frame.
use crate::commands::{BuiltinAction, ClientBindings, CommandGroup, key_name};
use ratatui_core::buffer::Buffer;
use ratatui_core::style::{Color, Modifier, Style};
use std::collections::BTreeSet;
use unicode_segmentation::UnicodeSegmentation as _;
use unicode_width::UnicodeWidthStr as _;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HintPanel {
    title: String,
    entries: Vec<String>,
    page: usize,
    footer: Option<String>,
    focus: Option<usize>,
    single_column: bool,
    thin: bool,
    input: bool,
    headings: BTreeSet<usize>,
    disabled: BTreeSet<usize>,
}

impl HintPanel {
    pub fn commands(
        bindings: &ClientBindings,
        manager: bool,
        page: usize,
        state: &crate::state::WorkspaceState,
    ) -> Self {
        let mut bound: Vec<_> = bindings.entries().collect();
        bound.sort_by_key(|(key, _)| {
            (
                bindings
                    .action(*key)
                    .map_or(CommandGroup::Custom, BuiltinAction::group),
                *key,
            )
        });
        let mut entries = Vec::new();
        let mut headings = BTreeSet::new();
        let mut disabled = BTreeSet::new();
        let mut previous = None;
        for (key, description) in bound {
            let action = bindings.action(key);
            let group = action.map_or(CommandGroup::Custom, BuiltinAction::group);
            if previous != Some(group) {
                headings.insert(entries.len());
                entries.push(group.label().to_owned());
                previous = Some(group);
            }
            if action
                .and_then(|action| action.unavailable(state, manager))
                .is_some()
            {
                disabled.insert(entries.len());
            }
            entries.push(format!("{}  {description}", key_name(key)));
        }
        Self {
            title: format!("Commands ({})", key_name(bindings.prefix())),
            entries,
            page,
            footer: None,
            focus: None,
            single_column: true,
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
            single_column: true,
            thin: false,
            input: false,
            headings: BTreeSet::new(),
            disabled: BTreeSet::new(),
        }
    }

    pub fn popup(bindings: &ClientBindings) -> Self {
        let prefix = key_name(bindings.prefix());
        let mut text = format!("Popup · {prefix} commands");
        if let Some((key, _)) = bindings
            .entries()
            .find(|(key, _)| bindings.action(*key) == Some(BuiltinAction::ClosePane))
        {
            text.push_str(&format!(" · {prefix} {} close (confirm)", key_name(key)));
        }
        Self::bar(&text)
    }

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

    pub fn paint(&self, buffer: &mut Buffer) {
        let area = buffer.area;
        if area.width == 0 || area.height == 0 {
            return;
        }
        if self.thin {
            let style = Style::reset().fg(Color::White).bg(Color::DarkGray);
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
        let columns = if self.single_column {
            1
        } else {
            usize::from((area.width / 30).max(1))
        };
        let available = usize::from(area.height.saturating_sub(2).max(1));
        let rows = self
            .entries
            .len()
            .div_ceil(columns)
            .max(1)
            .min(available)
            .min(10);
        let per_page = rows * columns;
        let pages = self.entries.len().div_ceil(per_page).max(1);
        let page = self
            .focus
            .map_or(self.page % pages, |focus| (focus / per_page).min(pages - 1));
        let start = page * per_page;
        let height = u16::try_from(rows + 2)
            .unwrap_or(area.height)
            .min(area.height);
        let top = area.bottom().saturating_sub(height);
        let style = Style::reset().fg(Color::White).bg(Color::DarkGray);
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
        let width = usize::from(area.width) / columns;
        for (index, entry) in self
            .entries
            .iter()
            .skip(start)
            .take(body_rows * columns)
            .enumerate()
        {
            let row = index / columns;
            let column = index % columns;
            let entry = if self.input {
                let mut used = 0;
                let mut start = entry.len();
                for (index, grapheme) in entry.grapheme_indices(true).rev() {
                    used += grapheme.width();
                    if used > width {
                        break;
                    }
                    start = index;
                }
                entry.get(start..).unwrap_or_default()
            } else {
                entry.as_str()
            };
            buffer.set_stringn(
                area.x + u16::try_from(column * width).unwrap_or(0),
                body_top + u16::try_from(row).unwrap_or(0),
                entry,
                width,
                if self.headings.contains(&(start + index)) {
                    style.add_modifier(Modifier::BOLD)
                } else if self.disabled.contains(&(start + index)) {
                    style.add_modifier(Modifier::DIM)
                } else {
                    style
                },
            );
        }
        if height > 1 {
            let default_footer = format!(
                "Esc cancel · ↑/↓ {}/{} · dim unavailable · prefix twice: literal",
                page + 1,
                pages
            );
            buffer.set_stringn(
                area.x,
                area.bottom() - 1,
                self.footer.as_deref().unwrap_or(&default_footer),
                usize::from(area.width),
                style,
            );
        }
    }
}
