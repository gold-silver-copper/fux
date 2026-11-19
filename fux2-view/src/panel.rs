//! What is drawn over the panes: the prefix menu, a list to pick from and a
//! line to type. They take every key while open, pastes included, so
//! nothing typed into them reaches a pane; the panes keep drawing beneath.
use crate::keys::Line;
use crate::render::{Frame, Tile};
use fux_vt::keys::{Key, KeyPress};

/// The most items a list holds, and the longest line typed.
pub const MAX_ITEMS: usize = 1024;
pub const MAX_LINE: usize = 4096;

/// A list to pick from: values and the labels shown for them.
pub struct Pick {
    pub id: u64,
    pub title: String,
    pub items: Vec<(String, String)>,
    pub filter: String,
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
    Pick(Pick),
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
}

impl Panel {
    pub fn id(&self) -> u64 {
        match self {
            Panel::Pick(pick) => pick.id,
            Panel::Ask(ask) => ask.id,
        }
    }

    pub fn key(&mut self, press: KeyPress) -> Outcome {
        match self {
            Panel::Pick(pick) => pick.key(press),
            Panel::Ask(ask) => ask.key(press),
        }
    }

    /// Text pasted while open: into the filter or the line, never a pane.
    pub fn paste(&mut self, text: &str) {
        let text: String = text.chars().filter(|c| !c.is_control()).collect();
        match self {
            Panel::Pick(pick) => {
                for c in text.chars() {
                    pick.type_char(c);
                }
            }
            Panel::Ask(ask) => text.chars().for_each(|c| ask.insert(c)),
        }
    }

    pub fn draw(&self, frame: &mut Frame, area: Tile) {
        match self {
            Panel::Pick(pick) => pick.draw(frame, area),
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

impl Pick {
    /// The items the filter lets through: their indices.
    pub fn shown(&self) -> Vec<usize> {
        let filter = self.filter.to_lowercase();
        (self.items.iter().enumerate())
            .filter(|(_, (_, label))| label.to_lowercase().contains(&filter))
            .map(|(index, _)| index)
            .collect()
    }

    fn type_char(&mut self, c: char) {
        if self.filter.len() < MAX_LINE {
            self.filter.push(c);
            self.selected = 0;
        }
    }

    fn key(&mut self, press: KeyPress) -> Outcome {
        let shown = self.shown();
        let last = shown.len().saturating_sub(1);
        let name = press.to_string();
        match name.as_str() {
            "Escape" | "C-c" | "C-g" => return Outcome::Cancel,
            "Enter" => {
                let chosen = shown.get(self.selected).and_then(|i| self.items.get(*i));
                return match chosen {
                    Some((value, _)) => Outcome::Chosen(value.clone()),
                    None => Outcome::Stay,
                };
            }
            "Up" | "C-p" => self.selected = self.selected.saturating_sub(1),
            "Down" | "C-n" => self.selected = self.selected.saturating_add(1).min(last),
            "PageUp" => self.selected = self.selected.saturating_sub(10),
            "PageDown" => self.selected = self.selected.saturating_add(10).min(last),
            "Home" => self.selected = 0,
            "End" => self.selected = last,
            "BSpace" => {
                self.filter.pop();
                self.selected = 0;
            }
            _ => {
                if let Some(c) = plain(press) {
                    self.type_char(c);
                }
            }
        }
        Outcome::Stay
    }

    fn draw(&self, frame: &mut Frame, area: Tile) {
        let shown = self.shown();
        let room = usize::from(area.h.saturating_sub(4)).max(1);
        let rows = shown.len().min(room);
        let widest = (self.items.iter())
            .map(|(_, label)| fux_vt::str_width(label))
            .max()
            .unwrap_or(0)
            .max(fux_vt::str_width(&self.title))
            .saturating_add(4);
        let w = u16::try_from(widest)
            .unwrap_or(u16::MAX)
            .clamp(24, area.w.max(1));
        let h = u16::try_from(rows).unwrap_or(0).saturating_add(3);
        let tile = centered(area, w, h);
        let title = format!(" {} ", self.title);
        frame.boxed(tile, &title);
        let filter = format!(" > {}", self.filter);
        frame.label(
            tile.x.saturating_add(1),
            tile.y.saturating_add(1),
            w.saturating_sub(2),
            &filter,
            "",
        );
        // Scrolled to keep the selection in view.
        let first = self.selected.saturating_add(1).saturating_sub(rows);
        for (row, index) in shown.iter().skip(first).take(rows).enumerate() {
            let Some((_, label)) = self.items.get(*index) else {
                continue;
            };
            let y = tile
                .y
                .saturating_add(2)
                .saturating_add(u16::try_from(row).unwrap_or(0));
            let picked = row.saturating_add(first) == self.selected;
            let sgr = if picked { ";7" } else { "" };
            frame.fill(tile.x.saturating_add(1), y, w.saturating_sub(2), " ", sgr);
            frame.label(tile.x.saturating_add(2), y, w.saturating_sub(4), label, sgr);
        }
        if shown.is_empty() {
            let y = tile.y.saturating_add(2);
            frame.label(
                tile.x.saturating_add(2),
                y,
                w.saturating_sub(4),
                "nothing matches",
                ";2",
            );
        }
    }
}

impl Ask {
    fn insert(&mut self, c: char) {
        if self.text.len() < MAX_LINE {
            let at = self.cursor.min(self.text.len());
            let tail = self.text.split_off_checked(at);
            self.text.push(c);
            self.text.extend(tail);
            self.cursor = at.saturating_add(1);
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
                let tail = self.text.split_off_checked(self.cursor);
                self.text = tail;
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

    fn remove(&mut self, at: usize) {
        if at < self.text.len() {
            let mut tail = self.text.split_off_checked(at);
            if !tail.is_empty() {
                tail = tail.into_iter().skip(1).collect();
            }
            self.text.extend(tail);
        }
    }

    fn draw(&self, frame: &mut Frame, area: Tile) {
        let w = area.w.clamp(1, 100);
        let tile = centered(area, w, 3);
        frame.boxed(tile, &format!(" {} ", self.prompt));
        let room = usize::from(w.saturating_sub(4)).max(1);
        // Scrolled to keep the cursor in view.
        let first = self.cursor.saturating_add(1).saturating_sub(room);
        let shown: String = self.text.iter().skip(first).take(room).collect();
        let y = tile.y.saturating_add(1);
        frame.label(tile.x.saturating_add(2), y, w.saturating_sub(4), &shown, "");
        let at = self.cursor.saturating_sub(first);
        let x = tile
            .x
            .saturating_add(2)
            .saturating_add(u16::try_from(at).unwrap_or(0));
        let under: String = self
            .text
            .get(self.cursor)
            .map_or(" ".to_owned(), |c| c.to_string());
        frame.label(x, y, 1, &under, ";7");
    }
}

/// A box `w` by `h` in the middle of `area`.
fn centered(area: Tile, w: u16, h: u16) -> Tile {
    let w = w.min(area.w);
    let h = h.min(area.h);
    Tile {
        x: area.x.saturating_add(area.w.saturating_sub(w) / 2),
        y: area.y.saturating_add(area.h.saturating_sub(h) / 2),
        w,
        h,
    }
}

/// Draws the menu of `prefix`: its lines in columns, in a box at the bottom
/// right of `area`.
pub fn draw_menu(frame: &mut Frame, area: Tile, prefix: &str, lines: &[Line]) {
    let text: Vec<(String, bool)> = lines
        .iter()
        .map(|line| match line {
            Line::Heading(heading) => (heading.clone(), true),
            Line::Key(key, caption, group) => {
                let more = if *group { " …" } else { "" };
                (format!("{key:>7}  {caption}{more}"), false)
            }
        })
        .collect();
    let room = usize::from(area.h.saturating_sub(2)).max(1);
    let columns: Vec<&[(String, bool)]> = {
        let mut out = Vec::new();
        let mut rest = text.as_slice();
        while !rest.is_empty() {
            let (column, after) = rest.split_at_checked(room).unwrap_or((rest, &[]));
            out.push(column);
            rest = after;
        }
        out
    };
    let widths: Vec<u16> = columns
        .iter()
        .map(|column| {
            let widest = column
                .iter()
                .map(|(t, _)| fux_vt::str_width(t))
                .max()
                .unwrap_or(0);
            u16::try_from(widest).unwrap_or(u16::MAX).saturating_add(2)
        })
        .collect();
    let w = widths
        .iter()
        .fold(0u16, |sum, w| sum.saturating_add(*w))
        .saturating_add(2)
        .max(u16::try_from(prefix.len()).unwrap_or(0).saturating_add(6))
        .min(area.w);
    let tallest = columns.iter().map(|c| c.len()).max().unwrap_or(0);
    let h = u16::try_from(tallest)
        .unwrap_or(0)
        .saturating_add(2)
        .min(area.h);
    let tile = Tile {
        x: area.x.saturating_add(area.w.saturating_sub(w)),
        y: area.y.saturating_add(area.h.saturating_sub(h)),
        w,
        h,
    };
    frame.boxed(tile, &format!(" {prefix} "));
    let mut x = tile.x.saturating_add(1);
    for (column, width) in columns.iter().zip(&widths) {
        for (row, (line, heading)) in column.iter().enumerate() {
            let y = tile
                .y
                .saturating_add(1)
                .saturating_add(u16::try_from(row).unwrap_or(0));
            let sgr = if *heading { ";1" } else { "" };
            let room = tile.x.saturating_add(w).saturating_sub(x).saturating_sub(1);
            frame.label(x.saturating_add(1), y, room.min(*width), line, sgr);
        }
        x = x.saturating_add(*width);
    }
}

trait SplitOffChecked<T> {
    fn split_off_checked(&mut self, at: usize) -> Vec<T>;
}

impl<T> SplitOffChecked<T> for Vec<T> {
    /// The items from `at` on, taken out; none if `at` is past the end.
    fn split_off_checked(&mut self, at: usize) -> Vec<T> {
        let mut tail = Vec::new();
        while self.len() > at {
            tail.extend(self.pop());
        }
        tail.reverse();
        tail
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(name: &str) -> KeyPress {
        name.parse().unwrap_or(KeyPress::char('?'))
    }

    #[test]
    fn a_pick_filters_and_a_line_edits_where_the_cursor_is() {
        let mut pick = Panel::Pick(Pick {
            id: 1,
            title: "tabs".to_owned(),
            items: vec![
                ("t1".to_owned(), "build".to_owned()),
                ("t2".to_owned(), "logs".to_owned()),
            ],
            filter: String::new(),
            selected: 0,
        });
        pick.paste("lo\x1b");
        assert_eq!(pick.key(press("Enter")), Outcome::Chosen("t2".to_owned()));
        let mut ask = Panel::Ask(Ask {
            id: 2,
            prompt: "rename".to_owned(),
            text: "ac".chars().collect(),
            cursor: 1,
        });
        ask.key(press("b"));
        ask.key(press("End"));
        ask.key(press("BSpace"));
        assert_eq!(ask.key(press("Enter")), Outcome::Chosen("ab".to_owned()));
        assert_eq!(ask.key(press("Escape")), Outcome::Cancel);
    }
}
