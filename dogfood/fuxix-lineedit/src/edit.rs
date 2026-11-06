//! The editor's state, its keys and how it draws: no terminal here.

/// A key, decoded from the bytes a terminal sends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Char(char),
    /// A control byte (0x01..=0x1a) as its letter: `Ctrl(b'a')`.
    Ctrl(u8),
    Enter,
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    WordLeft,
    WordRight,
    DeleteWordBack,
    /// A lone ESC, or a sequence this editor has no use for.
    Ignored,
}

/// The first key in `bytes` and how many bytes it took; `None` if `bytes`
/// is empty or ends part-way through a key, which more input may finish.
pub fn decode(bytes: &[u8]) -> Option<(Key, usize)> {
    let (&first, rest) = bytes.split_first()?;
    let key = match first {
        b'\r' | b'\n' => Key::Enter,
        0x7f | 0x08 => Key::Backspace,
        0x1b => return decode_escape(rest).map(|(key, n)| (key, n.saturating_add(1))),
        0x01..=0x1a => Key::Ctrl(first.saturating_add(b'a' - 1)),
        0x00..=0x1f => Key::Ignored,
        _ => return decode_utf8(bytes),
    };
    Some((key, 1))
}

/// What follows an ESC.
fn decode_escape(rest: &[u8]) -> Option<(Key, usize)> {
    let (&kind, body) = rest.split_first()?;
    match kind {
        b'[' => {
            // CSI: parameters and intermediates, then a final byte.
            let end = body.iter().position(|b| (0x40..=0x7e).contains(b))?;
            let params = body.get(..end).unwrap_or_default();
            let key = match (params, body.get(end)) {
                (_, Some(b'A')) => Key::Up,
                (_, Some(b'B')) => Key::Down,
                (b"" | b"1", Some(b'C')) => Key::Right,
                (b"" | b"1", Some(b'D')) => Key::Left,
                (b"1;5" | b"1;3", Some(b'C')) => Key::WordRight,
                (b"1;5" | b"1;3", Some(b'D')) => Key::WordLeft,
                (_, Some(b'H')) | (b"1" | b"7", Some(b'~')) => Key::Home,
                (_, Some(b'F')) | (b"4" | b"8", Some(b'~')) => Key::End,
                (b"3", Some(b'~')) => Key::Delete,
                _ => Key::Ignored,
            };
            Some((key, end.saturating_add(2)))
        }
        b'O' => {
            let key = match body.first()? {
                b'A' => Key::Up,
                b'B' => Key::Down,
                b'C' => Key::Right,
                b'D' => Key::Left,
                b'H' => Key::Home,
                b'F' => Key::End,
                _ => Key::Ignored,
            };
            Some((key, 2))
        }
        b'b' => Some((Key::WordLeft, 1)),
        b'f' => Some((Key::WordRight, 1)),
        0x7f | 0x08 => Some((Key::DeleteWordBack, 1)),
        // ESC ESC: the first is a lone ESC.
        0x1b => Some((Key::Ignored, 0)),
        _ => Some((Key::Ignored, 1)),
    }
}

/// One UTF-8 character; an invalid byte is skipped.
fn decode_utf8(bytes: &[u8]) -> Option<(Key, usize)> {
    let len = match bytes.first()? {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => return Some((Key::Ignored, 1)),
    };
    let encoded = bytes.get(..len)?;
    match std::str::from_utf8(encoded)
        .ok()
        .and_then(|s| s.chars().next())
    {
        Some(c) => Some((Key::Char(c), len)),
        None => Some((Key::Ignored, 1)),
    }
}

/// What a key asks of the program around the editor.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    /// Draw the line again.
    Redraw,
    /// A line was entered.
    Submit(String),
    /// The line was given up (Ctrl-C).
    Cancel,
    /// End of input (Ctrl-D on an empty line).
    Eof,
    /// Stop the program, as Ctrl-Z does (Ctrl-Z).
    Suspend,
    /// Clear the screen, then draw (Ctrl-L).
    ClearScreen,
}

/// A line being edited, with the lines entered before it.
#[derive(Default)]
pub struct Editor {
    line: Vec<char>,
    /// Where the cursor is, as an index into `line`.
    cursor: usize,
    /// The first character shown, when the line is wider than the screen.
    scroll: usize,
    history: Vec<String>,
    /// Which entry of `history` is shown, while browsing it.
    browsing: Option<usize>,
    /// The line as it was when browsing began.
    draft: Vec<char>,
}

impl Editor {
    pub fn with_history(history: Vec<String>) -> Editor {
        Editor {
            history,
            ..Editor::default()
        }
    }

    pub fn history(&self) -> &[String] {
        &self.history
    }

    pub fn line(&self) -> String {
        self.line.iter().collect()
    }

    /// Adds `line` to the history, but not an empty line or a repeat of the
    /// last: whether it was added.
    pub fn remember(&mut self, line: &str) -> bool {
        let new = !line.trim().is_empty() && self.history.last().map(String::as_str) != Some(line);
        if new {
            self.history.push(line.to_owned());
        }
        new
    }

    fn reset(&mut self) {
        self.line.clear();
        self.cursor = 0;
        self.scroll = 0;
        self.browsing = None;
    }

    fn show(&mut self, line: Vec<char>) {
        self.line = line;
        self.cursor = self.line.len();
    }

    /// The start of the word before the cursor.
    fn word_start(&self) -> usize {
        let before = self.line.get(..self.cursor).unwrap_or_default();
        let end = before
            .iter()
            .rposition(|c| !c.is_whitespace())
            .map_or(0, |i| i.saturating_add(1));
        let head = before.get(..end).unwrap_or_default();
        head.iter()
            .rposition(|c| c.is_whitespace())
            .map_or(0, |i| i.saturating_add(1))
    }

    /// The end of the word after the cursor.
    fn word_end(&self) -> usize {
        let after = self.line.get(self.cursor..).unwrap_or_default();
        let start = after
            .iter()
            .position(|c| !c.is_whitespace())
            .unwrap_or(after.len());
        let tail = after.get(start..).unwrap_or_default();
        let len = tail
            .iter()
            .position(|c| c.is_whitespace())
            .unwrap_or(tail.len());
        self.cursor.saturating_add(start).saturating_add(len)
    }

    pub fn apply(&mut self, key: Key) -> Option<Action> {
        let len = self.line.len();
        match key {
            Key::Char(c) => {
                self.line.insert(self.cursor, c);
                self.cursor = self.cursor.saturating_add(1);
            }
            Key::Enter => {
                let line = self.line();
                self.reset();
                return Some(Action::Submit(line));
            }
            Key::Ctrl(b'c') => {
                self.reset();
                return Some(Action::Cancel);
            }
            Key::Ctrl(b'd') if len == 0 => return Some(Action::Eof),
            Key::Ctrl(b'd') | Key::Delete if self.cursor < len => {
                self.line.remove(self.cursor);
            }
            Key::Backspace | Key::Ctrl(b'h') if self.cursor > 0 => {
                self.cursor = self.cursor.saturating_sub(1);
                self.line.remove(self.cursor);
            }
            Key::Left | Key::Ctrl(b'b') => self.cursor = self.cursor.saturating_sub(1),
            Key::Right | Key::Ctrl(b'f') => self.cursor = self.cursor.saturating_add(1).min(len),
            Key::Home | Key::Ctrl(b'a') => self.cursor = 0,
            Key::End | Key::Ctrl(b'e') => self.cursor = len,
            Key::WordLeft => self.cursor = self.word_start(),
            Key::WordRight => self.cursor = self.word_end(),
            Key::Ctrl(b'k') => self.line.truncate(self.cursor),
            Key::Ctrl(b'u') => {
                self.line.drain(..self.cursor);
                self.cursor = 0;
            }
            Key::Ctrl(b'w') | Key::DeleteWordBack => {
                let start = self.word_start();
                self.line.drain(start..self.cursor);
                self.cursor = start;
            }
            Key::Up | Key::Ctrl(b'p') => {
                let at = match self.browsing {
                    Some(0) => return None,
                    Some(at) => at.saturating_sub(1),
                    None => {
                        self.draft = self.line.clone();
                        self.history.len().checked_sub(1)?
                    }
                };
                self.browsing = Some(at);
                let entry = self.history.get(at)?.chars().collect();
                self.show(entry);
            }
            Key::Down | Key::Ctrl(b'n') => {
                let at = self.browsing?.saturating_add(1);
                match self.history.get(at) {
                    Some(entry) => {
                        let entry = entry.chars().collect();
                        self.browsing = Some(at);
                        self.show(entry);
                    }
                    None => {
                        self.browsing = None;
                        let draft = std::mem::take(&mut self.draft);
                        self.show(draft);
                    }
                }
            }
            Key::Ctrl(b'l') => return Some(Action::ClearScreen),
            Key::Ctrl(b'z') => return Some(Action::Suspend),
            Key::Ctrl(_) | Key::Delete | Key::Backspace | Key::Ignored => return None,
        }
        Some(Action::Redraw)
    }

    /// The bytes that draw `prompt` and the line on the cursor's row, `cols`
    /// wide, scrolled sideways to keep the cursor in sight. The last column
    /// stays empty, so the terminal never wraps. Each character is taken to
    /// be one column wide.
    pub fn render(&mut self, prompt: &str, cols: u16) -> String {
        let room = usize::from(cols).saturating_sub(1);
        let prompt: String = prompt.chars().take(room.saturating_sub(1)).collect();
        let shown = prompt.chars().count();
        let width = room.saturating_sub(shown).max(1);
        if self.cursor < self.scroll {
            self.scroll = self.cursor;
        } else if self.cursor >= self.scroll.saturating_add(width) {
            self.scroll = self.cursor.saturating_add(1).saturating_sub(width);
        }
        let end = self.scroll.saturating_add(width).min(self.line.len());
        let visible: String = self
            .line
            .get(self.scroll..end)
            .unwrap_or_default()
            .iter()
            .collect();
        let column = shown.saturating_add(self.cursor.saturating_sub(self.scroll));
        let mut out = format!("\r{prompt}{visible}\x1b[K\r");
        if column > 0 {
            out.push_str(&format!("\x1b[{column}C"));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(bytes: &[u8]) -> Vec<Key> {
        let mut rest = bytes;
        let mut keys = Vec::new();
        while let Some((key, n)) = decode(rest) {
            keys.push(key);
            rest = rest.get(n.max(1)..).unwrap_or_default();
        }
        keys
    }

    #[test]
    fn keys_edit_the_line_browse_history_and_scroll() {
        assert_eq!(
            keys(b"a\xc3\xa9\x1b[D\x1b[1;5C\x1bOH\x1b[3~\x7f\x03"),
            [
                Key::Char('a'),
                Key::Char('é'),
                Key::Left,
                Key::WordRight,
                Key::Home,
                Key::Delete,
                Key::Backspace,
                Key::Ctrl(b'c')
            ]
        );
        // Part of a sequence waits for the rest.
        assert_eq!(decode(b"\x1b["), None);
        assert_eq!(decode(b"\xe2\x82"), None);

        let mut editor = Editor::with_history(vec!["first".into(), "second".into()]);
        for key in keys(b"helo\x1b[Dl\x01X\x05!") {
            editor.apply(key);
        }
        assert_eq!(editor.line(), "Xhello!");
        editor.apply(Key::Up);
        assert_eq!(editor.line(), "second");
        editor.apply(Key::Up);
        editor.apply(Key::Up);
        assert_eq!(editor.line(), "first");
        editor.apply(Key::Down);
        editor.apply(Key::Down);
        assert_eq!(editor.line(), "Xhello!", "the draft comes back");
        editor.apply(Key::Ctrl(b'w'));
        assert_eq!(editor.line(), "");
        assert_eq!(editor.apply(Key::Ctrl(b'd')), Some(Action::Eof));

        // A long line scrolls sideways to keep the cursor in sight.
        let mut editor = Editor::default();
        for c in "abcdefghijklmnop".chars() {
            editor.apply(Key::Char(c));
        }
        // 10 columns: "> " and 7 characters, the last column empty.
        assert_eq!(editor.render("> ", 10), "\r> klmnop\x1b[K\r\x1b[8C");
        editor.apply(Key::Home);
        assert_eq!(editor.render("> ", 10), "\r> abcdefg\x1b[K\r\x1b[2C");
    }
}
