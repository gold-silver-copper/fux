//! Cell-sized terminal chrome. Native Bevy UI remains the pane geometry authority.
#[cfg(test)]
mod tests;
use crate::{assets::Settings, model::Viewer};
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

pub fn bar(out: &mut String, v: &Viewer, workspace: &str, focused: &str) {
    if v.rows == 0 || v.cols == 0 {
        return;
    }
    let row = v.rows - 1;
    at(
        out,
        0,
        row,
        format_args!("{BAR}{}", " ".repeat(usize::from(v.cols))),
    );
    let mode = if v.zoom { " zoom" } else { "" };
    let history = if v.scrollback > 0 {
        format!(" ↑{}", v.scrollback)
    } else {
        String::new()
    };
    let left = format!(" {workspace}{mode}{history}");
    let right = if v.notice.is_empty() {
        focused
    } else {
        &v.notice
    };
    // Both identities get space before secondary details; tiny bars keep the workspace.
    let allowance = if right.is_empty() || v.cols < 12 {
        v.cols
    } else {
        v.cols / 2
    };
    let left = fit(&left, allowance, false);
    at(out, 0, row, &left);
    let room = v.cols.saturating_sub(width(&left) + 3);
    if room > 0 && !right.is_empty() {
        let right = fit(right, room, v.notice.is_empty());
        let x = v.cols.saturating_sub(width(&right) + 1);
        at(out, x.saturating_sub(2), row, "│ ");
        if !v.notice.is_empty() {
            out.push_str(if v.notice_error {
                "\x1b[31m"
            } else {
                "\x1b[33m"
            });
        }
        out.push_str(&right);
    }
    out.push_str("\x1b[0m");
}

#[derive(Clone, Copy, Debug)]
pub struct Bounds {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}
impl Bounds {
    pub fn contains(self, x: u16, y: u16) -> bool {
        x >= self.x && y >= self.y && x - self.x < self.width && y - self.y < self.height
    }
}

// A heading is expendable on tiny screens; at least one command remains reachable.
fn capacity(rows: u16) -> usize {
    let available = rows.saturating_sub(1);
    usize::from(available.saturating_sub(u16::from(available >= 3)))
}
pub fn help_limit(settings: &Settings, rows: u16) -> usize {
    let cap = capacity(rows).max(1);
    if settings.bindings.len() <= cap {
        0
    } else {
        settings
            .bindings
            .len()
            .saturating_sub(if cap >= 3 { cap - 1 } else { cap })
    }
}
pub fn scroll(v: &mut Viewer, settings: &Settings, down: bool, page: bool) {
    let step = if page {
        capacity(v.rows).saturating_sub(2).max(1)
    } else {
        1
    };
    let current = v.help_scroll.min(help_limit(settings, v.rows));
    v.help_scroll = if down {
        current
            .saturating_add(step)
            .min(help_limit(settings, v.rows))
    } else {
        current.saturating_sub(step)
    };
}

pub fn panel(out: &mut String, v: &Viewer, settings: &Settings) -> Option<Bounds> {
    let available = v.rows.saturating_sub(1);
    if available == 0 || v.cols == 0 || (!v.prefix && v.prompt.is_none()) {
        return None;
    }
    let mut lines = Vec::new();
    if v.prefix || v.prompt.as_deref() == Some("help") {
        if available >= 3 {
            lines.push(("Commands".to_owned(), "\x1b[1m"));
        }
        let cap = capacity(v.rows);
        let start = v.help_scroll.min(help_limit(settings, v.rows));
        let above = cap >= 3 && start > 0;
        let below =
            cap >= 3 && settings.bindings.len().saturating_sub(start) > cap - usize::from(above);
        let body = cap.saturating_sub(usize::from(above) + usize::from(below));
        if above {
            lines.push((format!("▲ {start} more"), "\x1b[2m"));
        }
        let key_width = settings
            .bindings
            .iter()
            .map(|b| width(&b.key))
            .max()
            .unwrap_or(0)
            .min((v.cols.saturating_sub(4) / 3).max(1));
        for binding in settings.bindings.iter().skip(start).take(body) {
            let key = fit(&binding.key, key_width, false);
            let padding = " ".repeat(usize::from(key_width.saturating_sub(width(&key))));
            lines.push((
                format!("{padding}{key}  {}", binding.action.replace('_', " ")),
                "",
            ));
        }
        if settings.bindings.is_empty() {
            lines.push(("No bindings".into(), "\x1b[2m"));
        }
        if below {
            lines.push((
                format!("▼ {} more", settings.bindings.len() - start - body),
                "\x1b[2m",
            ));
        }
    } else if let Some(prompt) = &v.prompt {
        if available >= 3 {
            lines.push((prompt.replace('_', " "), "\x1b[1m"));
        }
        // Keep the editable tail/caret visible rather than the beginning of a long path.
        lines.push((
            fit(
                &format!("{}▏", v.buffer),
                v.cols.saturating_sub(2).max(1),
                true,
            ),
            "\x1b[7m",
        ));
        if available >= 2 {
            lines.push(("Enter accept · Esc cancel".into(), "\x1b[2m"));
        }
    }
    let height = (lines.len() as u16).min(available);
    let width = lines
        .iter()
        .map(|(s, _)| width(s))
        .max()
        .unwrap_or(0)
        .saturating_add(2)
        .min(v.cols);
    let bounds = Bounds {
        x: v.cols - width,
        y: available - height,
        width,
        height,
    };
    let padding = u16::from(width >= 3);
    for (row, (text, style)) in lines.iter().take(usize::from(height)).enumerate() {
        let y = bounds.y + row as u16;
        at(
            out,
            bounds.x,
            y,
            format_args!("{PANEL}{}", " ".repeat(usize::from(width))),
        );
        at(
            out,
            bounds.x + padding,
            y,
            format_args!("{style}{}", fit(text, width - padding * 2, false)),
        );
    }
    out.push_str("\x1b[0m");
    Some(bounds)
}
