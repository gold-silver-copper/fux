//! The command column and the small panels that share its presentation: a box anchored at the
//! bottom-right corner, directly above the bar, only as wide as its widest line and only as tall
//! as its content (or the space available). Long lists scroll row by row with `▲ n more` /
//! `▼ n more` indicators; the thin one-line hints stay full-width rows above the bar.

use crate::commands::{ClientBindings, Group, key_name};
use crate::view::Frame;
use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect;
use ratatui_core::style::{Color, Modifier, Style};
use std::collections::BTreeSet;
use unicode_segmentation::UnicodeSegmentation as _;
use unicode_width::UnicodeWidthStr as _;

/// One cell of padding on each side of the column's text.
const PADDING: u16 = 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HintPanel {
    title: Option<String>,
    entries: Vec<String>,
    /// Rows scrolled off the top of a long list (clamped while painting).
    scroll: usize,
    footer: Option<String>,
    focus: Option<usize>,
    thin: bool,
    input: bool,
    headings: BTreeSet<usize>,
    disabled: BTreeSet<usize>,
}

/// Where the column's entries land after clamping the scroll to the space available.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Window {
    /// First entry shown.
    start: usize,
    /// Entries shown.
    body: usize,
    /// Whether an indicator row precedes / follows the body.
    above: bool,
    below: bool,
}

impl HintPanel {
    /// The command column: every binding grouped under its heading, unavailable actions dimmed.
    pub fn commands(
        bindings: &ClientBindings,
        workspaces: bool,
        scroll: usize,
        frame: &Frame,
    ) -> Self {
        let key_width = bindings
            .entries()
            .iter()
            .map(|(key, _)| key_name(*key).width())
            .max()
            .unwrap_or(1);
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
            let key = key_name(key);
            let pad = " ".repeat(key_width.saturating_sub(key.width()));
            entries.push(format!("  {pad}{key}  {}", action.label()));
        }
        Self {
            title: None,
            entries,
            scroll,
            footer: None,
            focus: None,
            thin: false,
            input: false,
            headings,
            disabled,
        }
    }

    /// A titled list with a key-hint footer: choosers (with a focused row) and confirmations.
    pub fn context(
        title: String,
        entries: Vec<String>,
        footer: &str,
        focus: Option<usize>,
    ) -> Self {
        let clean =
            |value: String| -> String { value.chars().filter(|c| !c.is_control()).collect() };
        let title = clean(title);
        Self {
            title: (!title.is_empty()).then_some(title),
            entries: entries.into_iter().map(clean).collect(),
            scroll: 0,
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
    pub fn is_thin(&self) -> bool {
        self.thin
    }

    fn frame_rows(&self) -> usize {
        usize::from(self.title.is_some()) + usize::from(self.footer.is_some())
    }

    /// Rows available to entries when the column may use `rows` rows.
    fn capacity(&self, rows: u16) -> usize {
        usize::from(rows).saturating_sub(self.frame_rows())
    }

    /// Indicator rows are only worth their space when at least one entry can show beside them;
    /// on one or two rows the column scrolls without them.
    fn indicators_fit(capacity: usize) -> bool {
        capacity >= 3
    }

    /// The largest useful scroll offset for a column of `rows` rows: 0 when everything fits.
    #[must_use]
    pub fn max_scroll(&self, rows: u16) -> usize {
        let capacity = self.capacity(rows);
        if self.entries.len() <= capacity {
            return 0;
        }
        if !Self::indicators_fit(capacity) {
            return self.entries.len().saturating_sub(capacity.max(1));
        }
        // At the very end only the top indicator is needed, so `capacity - 1` entries show.
        self.entries
            .len()
            .saturating_sub(capacity.saturating_sub(1).max(1))
    }

    /// How many rows one page key moves: the body of a scrolled column.
    #[must_use]
    pub fn screenful(&self, rows: u16) -> usize {
        self.capacity(rows).saturating_sub(2).max(1)
    }

    fn window(&self, rows: u16) -> Window {
        let len = self.entries.len();
        let capacity = self.capacity(rows);
        if len <= capacity {
            return Window {
                start: 0,
                body: len,
                above: false,
                below: false,
            };
        }
        let place = |start: usize| -> Window {
            if !Self::indicators_fit(capacity) {
                // Every row goes to entries; at least one always shows.
                return Window {
                    start,
                    body: capacity.max(1).min(len.saturating_sub(start)),
                    above: false,
                    below: false,
                };
            }
            let above = start > 0;
            let remaining = capacity.saturating_sub(usize::from(above));
            let below = start.saturating_add(remaining) < len;
            let body = remaining
                .saturating_sub(usize::from(below))
                .min(len.saturating_sub(start));
            Window {
                start,
                body,
                above,
                below,
            }
        };
        let limit = self.max_scroll(rows);
        let mut start = self.scroll.min(limit);
        if let Some(focus) = self.focus {
            // A focused row (chooser) always stays visible.
            if focus < start {
                start = focus;
            }
            let mut window = place(start);
            while focus >= window.start.saturating_add(window.body)
                && window.start < len.saturating_sub(1)
            {
                window = place(window.start.saturating_add(1));
            }
            return window;
        }
        place(start)
    }

    /// Paints the column at the bottom-right of `area` (the compositor passes everything above
    /// the bar), or a thin full-width row for transient hints.
    pub fn paint(&self, buffer: &mut Buffer, area: Rect) {
        let area = area.intersection(buffer.area);
        if area.width == 0 || area.height == 0 {
            return;
        }
        let style = Style::reset().fg(Color::White).bg(Color::DarkGray);
        if self.thin {
            let row = area.bottom().saturating_sub(1);
            fill(buffer, area.x, area.right(), row, style);
            put(
                buffer,
                area.x,
                row,
                self.footer.as_deref().unwrap_or_default(),
                area.width,
                style,
            );
            return;
        }
        let window = self.window(area.height);
        let mut lines: Vec<(String, Style)> = Vec::new();
        if let Some(title) = &self.title {
            lines.push((title.clone(), style.add_modifier(Modifier::BOLD)));
        }
        if window.above {
            lines.push((
                format!("▲ {} more", window.start),
                style.add_modifier(Modifier::DIM),
            ));
        }
        for (offset, entry) in self
            .entries
            .iter()
            .skip(window.start)
            .take(window.body)
            .enumerate()
        {
            let index = window.start.saturating_add(offset);
            let entry_style = if self.headings.contains(&index) {
                style.add_modifier(Modifier::BOLD)
            } else if self.disabled.contains(&index) {
                style.add_modifier(Modifier::DIM)
            } else if self.focus == Some(index) {
                style.add_modifier(Modifier::REVERSED)
            } else {
                style
            };
            lines.push((entry.clone(), entry_style));
        }
        if window.below {
            let hidden = self
                .entries
                .len()
                .saturating_sub(window.start.saturating_add(window.body));
            lines.push((
                format!("▼ {hidden} more"),
                style.add_modifier(Modifier::DIM),
            ));
        }
        if let Some(footer) = &self.footer {
            lines.push((footer.clone(), style));
        }
        let height = u16::try_from(lines.len())
            .unwrap_or(u16::MAX)
            .min(area.height);
        let widest = lines
            .iter()
            .map(|(text, _)| u16::try_from(text.width()).unwrap_or(u16::MAX))
            .max()
            .unwrap_or(0);
        let width = widest
            .saturating_add(PADDING.saturating_mul(2))
            .clamp(1, area.width);
        let inner = width.saturating_sub(PADDING.saturating_mul(2));
        let x = area.right().saturating_sub(width);
        let top = area.bottom().saturating_sub(height);
        for (row, (text, line_style)) in lines.iter().take(usize::from(height)).enumerate() {
            let y = top.saturating_add(u16::try_from(row).unwrap_or(u16::MAX));
            fill(buffer, x, area.right(), y, style);
            let shown = if self.input {
                keep_tail(text, inner)
            } else {
                keep_head(text, inner)
            };
            put(
                buffer,
                x.saturating_add(PADDING),
                y,
                &shown,
                inner,
                *line_style,
            );
        }
    }
}

/// Paints background cells over `[from, to)` on `row`.
fn fill(buffer: &mut Buffer, from: u16, to: u16, row: u16, style: Style) {
    for x in from..to {
        if let Some(cell) = buffer.cell_mut((x, row)) {
            cell.set_symbol(" ").set_style(style);
        }
    }
}

/// Writes `text` clipped to `max_width`, skipping starts outside the buffer (ratatui indexes the
/// start cell unconditionally).
fn put(buffer: &mut Buffer, x: u16, y: u16, text: &str, max_width: u16, style: Style) {
    if max_width == 0 || x >= buffer.area.right() || y >= buffer.area.bottom() {
        return;
    }
    buffer.set_stringn(x, y, text, usize::from(max_width), style);
}

/// The head of `text` within `width` cells, ending with `…` when cut.
fn keep_head(text: &str, width: u16) -> String {
    if u16::try_from(text.width()).unwrap_or(u16::MAX) <= width {
        return text.to_owned();
    }
    let Some(keep) = width.checked_sub(1) else {
        return String::new();
    };
    let mut out = String::new();
    let mut used = 0_u16;
    for grapheme in text.graphemes(true) {
        let w = u16::try_from(grapheme.width()).unwrap_or(u16::MAX);
        if used.saturating_add(w) > keep {
            break;
        }
        out.push_str(grapheme);
        used = used.saturating_add(w);
    }
    out.push('…');
    out
}

/// The tail of `text` within `width` cells: keeps the insertion point of a text input visible.
fn keep_tail(text: &str, width: u16) -> String {
    let mut used = 0_u16;
    let mut cut = text.len();
    for (offset, grapheme) in text.grapheme_indices(true).rev() {
        used = used.saturating_add(u16::try_from(grapheme.width()).unwrap_or(u16::MAX));
        if used > width {
            break;
        }
        cut = offset;
    }
    text.get(cut..).unwrap_or_default().to_owned()
}

#[cfg(test)]
#[allow(clippy::indexing_slicing, clippy::panic, clippy::string_slice)]
mod tests {
    use super::*;

    fn text(buffer: &Buffer) -> Vec<String> {
        (0..buffer.area.height)
            .map(|row| {
                (0..buffer.area.width)
                    .map(|col| buffer.cell((col, row)).map_or(" ", |cell| cell.symbol()))
                    .collect::<String>()
            })
            .collect()
    }

    fn painted(panel: &HintPanel, rows: u16, cols: u16) -> (Buffer, Vec<String>) {
        let mut buffer = Buffer::empty(Rect::new(0, 0, cols, rows));
        let area = buffer.area;
        panel.paint(&mut buffer, area);
        let lines = text(&buffer);
        (buffer, lines)
    }

    fn modifier(buffer: &Buffer, x: usize, y: usize) -> Modifier {
        buffer
            .cell((
                u16::try_from(x).unwrap_or(u16::MAX),
                u16::try_from(y).unwrap_or(u16::MAX),
            ))
            .map(|cell| cell.modifier)
            .unwrap_or_default()
    }

    #[test]
    fn command_column_sits_bottom_right_as_wide_as_its_widest_line_and_shows_everything() {
        let bindings = ClientBindings::default();
        let panel = HintPanel::commands(&bindings, true, 0, &Frame::default());
        let (buffer, lines) = painted(&panel, 30, 80);
        let entries = panel.entries.len();
        assert_eq!(entries, 23, "18 bindings under 5 headings");
        assert!(
            lines[..30 - entries]
                .iter()
                .all(|line| line.trim().is_empty()),
            "nothing above the column"
        );
        let widest = panel.entries.iter().map(|e| e.width()).max().unwrap_or(0);
        let left = 80 - widest - 2;
        for (index, entry) in panel.entries.iter().enumerate() {
            let line = &lines[30 - entries + index];
            assert!(
                line.chars().take(left).all(|c| c == ' '),
                "text left of the column: {line:?}"
            );
            assert!(
                line.contains(entry.trim_end()),
                "{entry:?} missing: {line:?}"
            );
        }
        assert!(
            !lines
                .iter()
                .any(|line| line.contains("Commands") || line.contains("more"))
        );
        // Headings are bold; the empty frame makes split actions unavailable and dim.
        let heading_row = 30 - entries;
        assert!(modifier(&buffer, left + 1, heading_row).contains(Modifier::BOLD));
        assert!(
            (heading_row..30).any(|row| modifier(&buffer, left + 1, row).contains(Modifier::DIM))
        );
    }

    #[test]
    fn short_terminals_scroll_row_by_row_with_indicators_and_clamp() {
        let bindings = ClientBindings::default();
        let frame = Frame::default();
        let top = HintPanel::commands(&bindings, true, 0, &frame);
        let (_, lines) = painted(&top, 6, 60);
        assert!(lines[0].contains("Panes"), "{:?}", lines[0]);
        assert!(lines[5].contains("▼ 18 more"), "{:?}", lines[5]);
        assert_eq!(top.max_scroll(6), 18);
        assert_eq!(top.screenful(6), 4);
        let middle = HintPanel::commands(&bindings, true, 3, &frame);
        let (_, lines) = painted(&middle, 6, 60);
        assert!(lines[0].contains("▲ 3 more"), "{:?}", lines[0]);
        assert!(lines[5].contains("▼ 16 more"), "{:?}", lines[5]);
        let end = HintPanel::commands(&bindings, true, 999, &frame);
        let (_, lines) = painted(&end, 6, 60);
        assert!(lines[0].contains("▲ 18 more"), "{:?}", lines[0]);
        assert!(lines[5].contains("detach"), "{:?}", lines[5]);
        assert!(!lines.iter().any(|line| line.contains('▼')));
        // Every entry is reachable by scrolling one row at a time.
        let mut seen = String::new();
        for scroll in 0..=top.max_scroll(6) {
            let (_, lines) = painted(&HintPanel::commands(&bindings, true, scroll, &frame), 6, 60);
            seen.push_str(&lines.join("\n"));
        }
        for (key, action) in bindings.entries() {
            assert!(seen.contains(action.label()), "{action:?} unreachable");
            assert!(seen.contains(&key_name(key)));
        }
    }

    #[test]
    fn choosers_keep_their_title_footer_and_focused_row_visible() {
        let names: Vec<String> = (0..12).map(|i| format!("workspace-{i}")).collect();
        let panel = HintPanel::context(
            "Choose workspace".into(),
            names,
            "↑/↓ move · Enter switch · Esc back",
            Some(10),
        );
        let (buffer, lines) = painted(&panel, 8, 50);
        assert!(lines[0].contains("Choose workspace"), "{:?}", lines[0]);
        assert!(lines[7].contains("Enter switch"), "{:?}", lines[7]);
        assert!(lines[1].contains('▲'), "{:?}", lines[1]);
        let row = lines
            .iter()
            .position(|line| line.contains("workspace-10"))
            .unwrap_or_else(|| panic!("focused row hidden: {lines:?}"));
        let x = lines[row].find("workspace-10").unwrap_or(0);
        assert!(modifier(&buffer, x, row).contains(Modifier::REVERSED));
        let column_left = lines[0].find("Choose").unwrap_or(0);
        assert!(
            column_left > 10,
            "column hugs the right edge: {:?}",
            lines[0]
        );
    }

    #[test]
    fn one_and_two_row_columns_still_show_entries_and_focus() {
        let bindings = ClientBindings::default();
        let frame = Frame::default();
        // Two rows above the bar: no indicators, two entries, every entry reachable.
        let mut seen = String::new();
        let panel = HintPanel::commands(&bindings, true, 0, &frame);
        for scroll in 0..=panel.max_scroll(2) {
            let (_, lines) = painted(&HintPanel::commands(&bindings, true, scroll, &frame), 2, 60);
            assert!(!lines.iter().any(|line| line.contains("more")), "{lines:?}");
            assert!(
                lines.iter().any(|line| !line.trim().is_empty()),
                "{lines:?}"
            );
            seen.push_str(&lines.join("\n"));
        }
        for (_, action) in bindings.entries() {
            assert!(
                seen.contains(action.label()),
                "{action:?} unreachable on two rows"
            );
        }
        let (_, lines) = painted(&HintPanel::commands(&bindings, true, 5, &frame), 1, 60);
        assert!(lines[0].trim().starts_with("h  focus left") || !lines[0].trim().is_empty());
        // A chooser on four rows (title + one entry + footer) keeps its focused row visible.
        let names: Vec<String> = (0..9).map(|i| format!("ws-{i}")).collect();
        for focus in 0..9 {
            let panel = HintPanel::context("Choose".into(), names.clone(), "Enter", Some(focus));
            let (_, lines) = painted(&panel, 3, 30);
            assert!(
                lines
                    .iter()
                    .any(|line| line.contains(&format!("ws-{focus}"))),
                "focus {focus} hidden: {lines:?}"
            );
        }
    }

    #[test]
    fn narrow_and_tiny_terminals_truncate_and_never_panic() {
        let bindings = ClientBindings::default();
        let frame = Frame::default();
        let panel = HintPanel::commands(&bindings, true, 0, &frame);
        let (_, lines) = painted(&panel, 30, 12);
        assert!(lines.iter().any(|line| line.contains('…')), "{lines:?}");
        for (rows, cols) in [(0, 0), (1, 1), (1, 40), (2, 3), (3, 1), (40, 2)] {
            let _ = painted(&panel, rows, cols);
            let _ = painted(&HintPanel::bar("Copy · q finish"), rows, cols);
            let _ = painted(
                &HintPanel::text_input("Rename tab", "a-very-long-label-indeed", "Enter save"),
                rows,
                cols,
            );
        }
        let (_, lines) = painted(&HintPanel::bar("Copy · q finish"), 3, 20);
        assert!(lines[2].starts_with("Copy"));
        let input = HintPanel::text_input("Rename tab", "a-very-long-label-indeed", "Enter save");
        let (_, lines) = painted(&input, 4, 8);
        assert!(
            lines.iter().any(|line| line.contains('▏')),
            "insertion marker stays visible: {lines:?}"
        );
    }
}
