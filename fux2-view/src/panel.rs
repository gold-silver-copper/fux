//! What is drawn over the panes, as fux draws it: one panel at the bottom
//! right, flush against the edge and the row above the bar, a cell of room
//! each side and no frame: the prefix's menu of keys, a list to choose from,
//! or a line to type. A panel takes every key while open, pastes included,
//! so nothing typed into it reaches a pane; the panes keep drawing beneath.
use crate::keys::Line;
use crate::render::{Frame, Tile};
use fux_vt::keys::{Key, KeyPress};

/// The most rows a list holds, and the longest line typed.
pub const MAX_ITEMS: usize = 1024;
pub const MAX_LINE: usize = 4096;

/// The panel's colours, and its title, selected and hint lines'.
const PANEL: &str = ";38;5;255;48;5;238";
const BOLD: &str = ";1;38;5;255;48;5;238";
const SELECTED: &str = ";7;38;5;255;48;5;238";
const DIM: &str = ";2;38;5;255;48;5;238";
/// The line typed's cursor, a cell of its own.
const CURSOR: char = '\u{258f}';

/// One row of a list.
pub struct Row {
    pub value: String,
    pub label: String,
    /// Marked `*`: where you are now.
    pub current: bool,
}

/// A list to choose from, with the keys it offers besides choosing.
pub struct List {
    pub id: u64,
    pub title: String,
    pub rows: Vec<Row>,
    /// A key and what it does to the row selected, by number.
    pub actions: Vec<(String, String)>,
    pub selected: usize,
}

/// A line to type, with a cursor counted in characters.
pub struct Ask {
    pub id: u64,
    pub prompt: String,
    pub text: Vec<char>,
    pub cursor: usize,
}

pub enum Panel {
    List(List),
    Ask(Ask),
}

/// What a key did to an open panel.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Stay,
    /// Closed with nothing chosen.
    Cancel,
    /// Closed with this value.
    Chosen(String),
    /// Closed with action number `n` on this value.
    Action(usize, String),
}

impl Panel {
    pub fn id(&self) -> u64 {
        match self {
            Panel::List(list) => list.id,
            Panel::Ask(ask) => ask.id,
        }
    }

    pub fn key(&mut self, press: KeyPress) -> Outcome {
        match self {
            Panel::List(list) => list.key(press),
            Panel::Ask(ask) => ask.key(press),
        }
    }

    /// Text pasted while open: into the line, never a pane.
    pub fn paste(&mut self, text: &str) {
        if let Panel::Ask(ask) = self {
            text.chars()
                .filter(|c| !c.is_control())
                .for_each(|c| ask.insert(c));
        }
    }

    pub fn draw(&self, frame: &mut Frame, area: Tile) {
        match self {
            Panel::List(list) => list.draw(frame, area),
            Panel::Ask(ask) => ask.draw(frame, area),
        }
    }
}

/// The character a key types, unless Ctrl or Alt is held.
fn plain(press: KeyPress) -> Option<char> {
    match press.plain_key() {
        Some(Key::Char(c)) => Some(c),
        _ => None,
    }
}

/// Where a selection moves for a moving key, among `len` rows.
fn moved(name: &str, selected: usize, len: usize, page: usize) -> Option<usize> {
    let last = len.saturating_sub(1);
    Some(match name {
        "Up" | "k" => selected.saturating_sub(1),
        "Down" | "j" => selected.saturating_add(1).min(last),
        "PageUp" => selected.saturating_sub(page.max(1)),
        "PageDown" => selected.saturating_add(page.max(1)).min(last),
        "Home" => 0,
        "End" => last,
        _ => return None,
    })
}

impl List {
    fn key(&mut self, press: KeyPress) -> Outcome {
        let name = press.to_string();
        if let Some(at) = moved(&name, self.selected, self.rows.len(), 10) {
            self.selected = at;
            return Outcome::Stay;
        }
        let value = || self.rows.get(self.selected).map(|row| row.value.clone());
        match name.as_str() {
            "Escape" | "q" | "n" => return Outcome::Cancel,
            "Enter" | "y" => return value().map_or(Outcome::Stay, Outcome::Chosen),
            _ => {}
        }
        let action = (self.actions.iter()).position(|(key, _)| *key == name);
        match (action, value()) {
            (Some(n), Some(value)) => Outcome::Action(n, value),
            _ => Outcome::Stay,
        }
    }

    fn hints(&self) -> String {
        let mut hints = vec!["Enter y select".to_owned()];
        hints.extend(
            self.actions
                .iter()
                .map(|(key, label)| format!("{key} {label}")),
        );
        hints.push("q n Escape cancel".to_owned());
        hints.join(" \u{b7} ")
    }

    fn draw(&self, frame: &mut Frame, area: Tile) {
        let mut lines = vec![(self.title.clone(), BOLD)];
        let rows: Vec<(String, &str)> = (self.rows.iter().enumerate())
            .map(|(index, row)| {
                let marker = if row.current { '*' } else { ' ' };
                let sgr = if index == self.selected {
                    SELECTED
                } else {
                    PANEL
                };
                (format!("{marker} {}", row.label), sgr)
            })
            .collect();
        let room = usize::from(area.h).saturating_sub(4).max(1);
        lines.extend(window(rows, self.selected, room));
        lines.push((self.hints(), DIM));
        draw_block(frame, area, &lines);
    }
}

/// The lines of `rows` that fit in `room`, ending at the selection or the
/// last, with `▲ N more` and `▼ N more` for those left out.
fn window(rows: Vec<(String, &str)>, selected: usize, room: usize) -> Vec<(String, &str)> {
    if rows.len() <= room {
        return rows;
    }
    let shown = room.saturating_sub(2).max(1);
    let end = selected.saturating_add(1).max(shown).min(rows.len());
    let start = end.saturating_sub(shown);
    let after = rows.len().saturating_sub(end);
    let mut out = Vec::with_capacity(room);
    if start > 0 {
        out.push((format!("\u{25b2} {start} more"), DIM));
    }
    out.extend(rows.into_iter().skip(start).take(shown));
    if after > 0 {
        out.push((format!("\u{25bc} {after} more"), DIM));
    }
    out
}

impl Ask {
    fn insert(&mut self, c: char) {
        if self.text.len() < MAX_LINE {
            let at = self.cursor.min(self.text.len());
            let tail = split_off(&mut self.text, at);
            self.text.push(c);
            self.text.extend(tail);
            self.cursor = at.saturating_add(1);
        }
    }

    fn remove(&mut self, at: usize) {
        if at < self.text.len() {
            let tail = split_off(&mut self.text, at);
            self.text.extend(tail.into_iter().skip(1));
        }
    }

    fn key(&mut self, press: KeyPress) -> Outcome {
        let name = press.to_string();
        match name.as_str() {
            "Escape" | "C-c" | "C-g" => return Outcome::Cancel,
            "Enter" => return Outcome::Chosen(self.text.iter().collect()),
            "Left" | "C-b" => self.cursor = self.cursor.saturating_sub(1),
            "Right" | "C-f" => self.cursor = self.cursor.saturating_add(1).min(self.text.len()),
            "Home" | "C-a" => self.cursor = 0,
            "End" | "C-e" => self.cursor = self.text.len(),
            "C-u" => {
                self.text = split_off(&mut self.text, self.cursor);
                self.cursor = 0;
            }
            "BSpace" => {
                if let Some(at) = self.cursor.checked_sub(1) {
                    self.remove(at);
                    self.cursor = at;
                }
            }
            "Delete" | "C-d" => self.remove(self.cursor),
            _ => {
                if let Some(c) = plain(press) {
                    self.insert(c);
                }
            }
        }
        Outcome::Stay
    }

    /// The line with its cursor, cut to `room` cells around the cursor, a
    /// `…` on each side cut off.
    fn shown(&self, room: usize) -> String {
        let cursor = self.cursor.min(self.text.len());
        let mut all: Vec<char> = self.text.iter().take(cursor).copied().collect();
        all.push(CURSOR);
        all.extend(self.text.iter().skip(cursor));
        if all.len() <= room {
            return all.into_iter().collect();
        }
        let after = room.saturating_sub(2) / 4;
        let end = cursor
            .saturating_add(1)
            .saturating_add(after)
            .min(all.len());
        let start = end.saturating_sub(room.saturating_sub(2));
        let mut out = String::new();
        if start > 0 {
            out.push('\u{2026}');
        }
        out.extend(all.iter().skip(start).take(end.saturating_sub(start)));
        if end < all.len() {
            out.push('\u{2026}');
        }
        out
    }

    fn draw(&self, frame: &mut Frame, area: Tile) {
        let hints = "Enter accept \u{b7} Escape cancel".to_owned();
        let room = usize::from(area.w).saturating_sub(2);
        let lines = [
            (self.prompt.clone(), BOLD),
            (self.shown(room), PANEL),
            (hints, DIM),
        ];
        draw_block(frame, area, &lines);
    }
}

/// Draws `lines` as a block at the bottom right of `area`, a cell of room
/// each side, each line filling its row in its own colours; a line too wide
/// is cut with `…`. If the block is taller than `area`, its first lines go.
pub fn draw_block(frame: &mut Frame, area: Tile, lines: &[(String, &str)]) {
    let widest = lines
        .iter()
        .map(|(text, _)| fux_vt::str_width(text))
        .max()
        .unwrap_or(0);
    let w = u16::try_from(widest.saturating_add(2))
        .unwrap_or(u16::MAX)
        .min(area.w);
    let skip = lines.len().saturating_sub(usize::from(area.h));
    let h = u16::try_from(lines.len().saturating_sub(skip)).unwrap_or(0);
    let x = area.x.saturating_add(area.w.saturating_sub(w));
    let top = area.y.saturating_add(area.h.saturating_sub(h));
    for (row, (text, sgr)) in lines.iter().skip(skip).enumerate() {
        let y = top.saturating_add(u16::try_from(row).unwrap_or(0));
        frame.fill(x, y, w, " ", sgr);
        let room = w.saturating_sub(2);
        frame.label(x.saturating_add(1), y, room, &cut(text, room), sgr);
    }
}

/// `text` in `room` cells, its last a `…` if it had to be cut.
pub fn cut(text: &str, room: u16) -> String {
    if fux_vt::str_width(text) <= usize::from(room) {
        return text.to_owned();
    }
    let mut out = String::new();
    let mut used = 0usize;
    for (glyph, width) in fux_vt::clusters(text) {
        let width = usize::from(width);
        if used.saturating_add(width) >= usize::from(room) {
            break;
        }
        out.push_str(glyph);
        used = used.saturating_add(width);
    }
    out.push('\u{2026}');
    out
}

/// The prefix's menu: the keys typed so far, the keys after them under their
/// headings, one selected.
pub struct Column {
    pub keys: String,
    pub selected: usize,
}

impl Column {
    /// The entries' keys, in order: what the arrows move between.
    pub fn entries(lines: &[Line]) -> Vec<String> {
        (lines.iter())
            .filter_map(|line| match line {
                Line::Key(key, _, _) => Some(key.clone()),
                Line::Heading(_) => None,
            })
            .collect()
    }

    /// Moves the selection for a moving key that is not a letter: true if
    /// it was one.
    pub fn moves(&mut self, press: KeyPress, count: usize) -> bool {
        let name = press.to_string();
        if name.chars().count() == 1 {
            return false;
        }
        match moved(&name, self.selected, count, 10) {
            Some(at) => {
                self.selected = at;
                true
            }
            None => false,
        }
    }

    pub fn draw(&self, frame: &mut Frame, area: Tile, lines: &[Line]) {
        let pad = (lines.iter())
            .filter_map(|line| match line {
                Line::Key(key, _, _) => Some(fux_vt::str_width(key)),
                Line::Heading(_) => None,
            })
            .max()
            .unwrap_or(0);
        let mut out: Vec<(String, &str)> = Vec::new();
        let header = area.h.saturating_sub(1) >= 4;
        if header {
            out.push((self.keys.clone(), BOLD));
        }
        let mut entry = 0usize;
        let mut selected_line = 0usize;
        let mut body: Vec<(String, &str)> = Vec::new();
        for line in lines {
            match line {
                Line::Heading(heading) => body.push((heading.clone(), BOLD)),
                Line::Key(key, caption, group) => {
                    let more = if *group { "\u{2026}" } else { "" };
                    let lead = pad.saturating_sub(fux_vt::str_width(key));
                    let text = format!("{:lead$}{key}  {caption}{more}", "");
                    let sgr = if entry == self.selected {
                        selected_line = body.len();
                        SELECTED
                    } else {
                        PANEL
                    };
                    body.push((text, sgr));
                    entry = entry.saturating_add(1);
                }
            }
        }
        if body.is_empty() {
            body.push(("no bindings".to_owned(), DIM));
        }
        let room = usize::from(area.h.saturating_sub(1))
            .saturating_sub(usize::from(header))
            .saturating_sub(2)
            .max(1);
        out.extend(window(body, selected_line, room));
        draw_block(frame, area, &out);
    }
}

/// The items from `at` on, taken out of `items`.
fn split_off<T>(items: &mut Vec<T>, at: usize) -> Vec<T> {
    let mut tail = Vec::new();
    while items.len() > at {
        tail.extend(items.pop());
    }
    tail.reverse();
    tail
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(name: &str) -> KeyPress {
        name.parse().unwrap_or(KeyPress::char('?'))
    }

    #[test]
    fn a_list_offers_its_keys_and_a_line_edits_where_the_cursor_is() {
        let row = |value: &str, current| Row {
            value: value.to_owned(),
            label: value.to_owned(),
            current,
        };
        let mut list = Panel::List(List {
            id: 1,
            title: "tabs".to_owned(),
            rows: vec![row("t1", false), row("t2", true)],
            actions: vec![("r".to_owned(), "rename".to_owned())],
            selected: 1,
        });
        assert_eq!(list.key(press("r")), Outcome::Action(0, "t2".to_owned()));
        assert_eq!(list.key(press("k")), Outcome::Stay);
        assert_eq!(list.key(press("y")), Outcome::Chosen("t1".to_owned()));
        assert_eq!(list.key(press("q")), Outcome::Cancel);
        let mut ask = Panel::Ask(Ask {
            id: 2,
            prompt: "rename".to_owned(),
            text: "ac".chars().collect(),
            cursor: 1,
        });
        ask.paste("b\x1b");
        ask.key(press("End"));
        ask.key(press("BSpace"));
        assert_eq!(ask.key(press("Enter")), Outcome::Chosen("ab".to_owned()));
        assert_eq!(ask.key(press("Escape")), Outcome::Cancel);
    }
}
