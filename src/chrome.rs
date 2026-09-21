//! Cell-sized terminal chrome. Native Bevy UI remains the pane geometry authority.
#[cfg(test)]
mod tests;
use crate::{
    assets::{Binding, BindingAction, Settings},
    model::Viewer,
};
use bevy_math::URect;
use std::fmt::Write;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub const BAR: &str = "\x1b[0;37;100m";
const PANEL: &str = "\x1b[0;97;100m";

pub fn at(out: &mut String, x: u16, y: u16, text: impl std::fmt::Display) {
    let _ = write!(
        out,
        "\x1b[{};{}H{}",
        u32::from(y) + 1,
        u32::from(x) + 1,
        text
    );
}

pub fn width(text: &str) -> u16 {
    text.width().min(usize::from(u16::MAX)) as u16
}

/// Strip controls and truncate at display-cell boundaries. Never split a wide glyph.
pub fn fit(text: &str, cols: u16, tail: bool) -> String {
    let clean: String = text.chars().filter(|c| !c.is_control()).collect();
    if width(&clean) <= cols {
        return clean;
    }
    if cols == 0 {
        return String::new();
    }
    let mut room = usize::from(cols - 1);
    if tail {
        let mut chars = Vec::new();
        for c in clean.chars().rev() {
            let w = c.width().unwrap_or(0);
            if w > room {
                break;
            }
            room -= w;
            chars.push(c);
        }
        format!("…{}", chars.into_iter().rev().collect::<String>())
    } else {
        let mut shown = String::new();
        for c in clean.chars() {
            let w = c.width().unwrap_or(0);
            if w > room {
                break;
            }
            room -= w;
            shown.push(c);
        }
        shown.push('…');
        shown
    }
}

/// Render the existing bar with an ordered, active-visible tab window.
pub fn tab_bar(
    out: &mut String,
    v: &Viewer,
    (workspace_id, workspace): (bevy_ecs::entity::Entity, &str),
    (tab, tabs): (
        Option<bevy_ecs::entity::Entity>,
        &[(bevy_ecs::entity::Entity, String)],
    ),
    focused: &str,
) -> Vec<(bevy_ecs::entity::Entity, URect)> {
    if v.rows == 0 || v.cols == 0 {
        return Vec::new();
    }
    let allowance = if focused.is_empty() && v.notice.is_none() || v.cols < 12 {
        v.cols
    } else {
        v.cols / 2
    };
    at(
        out,
        0,
        v.rows - 1,
        format_args!("{BAR}{}", " ".repeat(usize::from(v.cols))),
    );
    let status = format!(
        "{focused}{}{}",
        if v.zoom { " zoom" } else { "" },
        if v.scrollback > 0 {
            format!(" ↑{}", v.scrollback)
        } else {
            String::new()
        }
    );
    let focused = status.as_str();
    let right = v.notice.as_ref().map_or(focused, |n| n.text.as_str());
    let room = v.cols.saturating_sub(allowance + 3);
    if room > 0 && !right.is_empty() {
        let right = fit(right, room, v.notice.is_none());
        let x = v.cols.saturating_sub(width(&right) + 1);
        at(out, x.saturating_sub(2), v.rows - 1, "│ ");
        if let Some(notice) = &v.notice {
            out.push_str(if notice.error { "\x1b[31m" } else { "\x1b[33m" });
        }
        out.push_str(&right);
    }
    let workspace_room = if allowance < 4 && !tabs.is_empty() {
        0
    } else {
        (allowance / 3).max(1)
    };
    let title = fit(&format!(" {workspace}"), workspace_room, false);
    at(
        out,
        0,
        v.rows - 1,
        format_args!("{BAR}{}", " ".repeat(usize::from(allowance))),
    );
    at(out, 0, v.rows - 1, &title);
    let mut hits = vec![(
        workspace_id,
        URect::new(
            0,
            u32::from(v.rows - 1),
            u32::from(width(&title)),
            u32::from(v.rows),
        ),
    )];
    let mut x = width(&title).saturating_add(u16::from(!title.is_empty()));
    let selected = tabs
        .iter()
        .position(|(id, _)| Some(*id) == tab)
        .unwrap_or(0);
    // Start far enough left to retain preceding tabs when they fit, but never
    // spend the selected tab's cell budget on an inactive label.
    let mut start = selected;
    let selected_width = tabs
        .get(selected)
        .map_or(0, |(_, name)| width(name).saturating_add(2));
    let mut needed = selected_width.min(allowance.saturating_sub(x));
    while let Some((_, name)) = start.checked_sub(1).and_then(|i| tabs.get(i)) {
        let previous = width(name).saturating_add(2);
        if needed.saturating_add(previous) > allowance.saturating_sub(x) {
            break;
        }
        needed += previous;
        start -= 1;
    }
    for (id, name) in tabs.iter().skip(start) {
        let room = allowance.saturating_sub(x);
        if room == 0 {
            break;
        }
        let label = fit(
            &if room < 3 {
                name.clone()
            } else {
                format!(" {name} ")
            },
            room,
            false,
        );
        let size = width(&label);
        at(
            out,
            x,
            v.rows - 1,
            format_args!(
                "{BAR}{}{label}{BAR}",
                if Some(*id) == tab { "\x1b[7;1m" } else { "" }
            ),
        );
        hits.push((
            *id,
            URect::new(
                u32::from(x),
                u32::from(v.rows - 1),
                u32::from(x + size),
                u32::from(v.rows),
            ),
        ));
        x += size;
    }
    out.push_str("\x1b[0m");
    hits
}

// A heading is expendable on tiny screens; at least one command remains reachable.
fn capacity(rows: u16) -> usize {
    let available = rows.saturating_sub(1);
    usize::from(available.saturating_sub(u16::from(available >= 3)))
}
/// A command-list scroll offset is the selected actionable row, never a heading
/// or indicator.
pub fn help_limit(settings: &Settings, _rows: u16) -> usize {
    settings.bindings.len().saturating_sub(1)
}
pub fn selected_action(
    settings: &Settings,
    rows: u16,
    cols: u16,
    scroll: usize,
) -> Option<&BindingAction> {
    help_entries(settings, cols)
        .into_iter()
        .filter_map(|(_, action)| action)
        .nth(scroll.min(help_limit(settings, rows)))
}
pub fn scroll(settings: &Settings, rows: u16, current: usize, down: bool, page: bool) -> usize {
    let step = if page {
        let cap = capacity(rows);
        if cap >= 3 { cap - 2 } else { cap.max(1) }
    } else {
        1
    };
    let current = current.min(help_limit(settings, rows));
    if down {
        current.saturating_add(step).min(help_limit(settings, rows))
    } else {
        current.saturating_sub(step)
    }
}

#[cfg(test)]
pub fn panel(out: &mut String, v: &Viewer, settings: &Settings, scroll: usize) -> Option<URect> {
    panel_context(out, v, settings, scroll, |_| false)
}

fn help_entries(settings: &Settings, cols: u16) -> Vec<(String, Option<&BindingAction>)> {
    let key_width = settings
        .bindings
        .iter()
        .map(|b| width(b.key.as_str()))
        .max()
        .unwrap_or(0)
        .min((cols.saturating_sub(4) / 3).max(1));
    let mut lines = Vec::new();
    let group_of = |binding: &Binding| match &binding.action {
        BindingAction::Known(action) => action.group(),
        BindingAction::Custom(_) => "Other",
    };
    for group in ["Panes", "Focus", "Tabs", "Workspaces", "Session", "Other"] {
        let bindings: Vec<_> = settings
            .bindings
            .iter()
            .filter(|b| group_of(b) == group)
            .collect();
        if bindings.is_empty() {
            continue;
        }
        lines.push((group.into(), None));
        for binding in bindings {
            let key = fit(binding.key.as_str(), key_width, false);
            let padding = " ".repeat(usize::from(key_width.saturating_sub(width(&key))));
            let label = match &binding.action {
                BindingAction::Known(action) => action.label().to_owned(),
                BindingAction::Custom(name) => name.replace('_', " "),
            };
            lines.push((format!("{padding}{key}  {label}"), Some(&binding.action)));
        }
    }
    lines
}

/// The command column for the prefix key or explicit help, selecting `scroll`.
pub fn panel_context(
    out: &mut String,
    v: &Viewer,
    settings: &Settings,
    scroll: usize,
    disabled: impl Fn(&BindingAction) -> bool,
) -> Option<URect> {
    let available = v.rows.saturating_sub(1);
    if available == 0 || v.cols == 0 {
        return None;
    }
    let mut lines = Vec::new();
    if available >= 3 {
        lines.push(("Commands".to_owned(), "\x1b[1m"));
    }
    let entries = help_entries(settings, v.cols);
    let cap = capacity(v.rows);
    let selected = entries
        .iter()
        .enumerate()
        .filter(|(_, (_, action))| action.is_some())
        .nth(scroll.min(help_limit(settings, v.rows)))
        .map(|(i, _)| i);
    let selected_row = selected.unwrap_or(0);
    let mut start = 0;
    let (above, below, body) = loop {
        let above = cap >= 3 && start > 0;
        let below = cap >= 3 && entries.len().saturating_sub(start) > cap - usize::from(above);
        let body = cap.saturating_sub(usize::from(above) + usize::from(below));
        if selected_row < start + body || body == 0 {
            break (above, below, body);
        }
        start += 1;
    };
    if above {
        lines.push((format!("▲ {start} more"), "\x1b[2m"));
    }
    for (index, (text, action)) in entries.iter().enumerate().skip(start).take(body) {
        lines.push((
            text.clone(),
            match action {
                None => "\x1b[1m",
                Some(action) if disabled(action) && Some(index) == selected => "\x1b[2;7m",
                Some(_) if Some(index) == selected => "\x1b[7m",
                Some(action) if disabled(action) => "\x1b[2m",
                _ => "",
            },
        ));
    }
    if settings.bindings.is_empty() {
        lines.push(("No bindings".into(), "\x1b[2m"));
    }
    if below {
        lines.push((
            format!("▼ {} more", entries.len() - start - body),
            "\x1b[2m",
        ));
    }
    surface(out, v, &lines)
}

/// Shared content-sized corner surface for help, prompts, choosers and menus.
pub fn surface(out: &mut String, v: &Viewer, lines: &[(String, &str)]) -> Option<URect> {
    let available = v.rows.saturating_sub(1);
    if available == 0 || v.cols == 0 || lines.is_empty() {
        return None;
    }
    let height = (lines.len() as u16).min(available);
    let width = lines
        .iter()
        .map(|(s, _)| width(s))
        .max()
        .unwrap_or(0)
        .saturating_add(2)
        .min(v.cols);
    let x = v.cols - width;
    let top = available - height;
    let bounds = URect::new(
        u32::from(x),
        u32::from(top),
        u32::from(v.cols),
        u32::from(available),
    );
    let padding = u16::from(width >= 3);
    for (row, (text, style)) in lines.iter().take(usize::from(height)).enumerate() {
        let y = top + row as u16;
        at(
            out,
            x,
            y,
            format_args!("{PANEL}{}", " ".repeat(usize::from(width))),
        );
        at(
            out,
            x + padding,
            y,
            format_args!(
                "{style}{}",
                fit(text, width - padding * 2, text.ends_with('▏'))
            ),
        );
    }
    out.push_str("\x1b[0m");
    Some(bounds)
}
