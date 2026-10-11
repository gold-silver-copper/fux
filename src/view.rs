//! One attached client's own view of the shared state.
use crate::command::{AnyRef, Command};
use crate::copy::Copy;
use crate::decode::Decoder;
use crate::id::{ClientId, WsId};
use crate::overlay::{Column, Repeat};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub text: String,
    pub error: bool,
}

/// One row of a chooser or an action menu: what it shows and the command it
/// runs, with every target given, so it acts on the item it was built for
/// and never on another.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub label: String,
    pub command: Command,
    /// Marked as the current one in a chooser.
    pub current: bool,
    /// What `r` renames and `x` closes: a chooser's items each have one,
    /// a menu's none.
    pub subject: Option<AnyRef<WsId>>,
}

/// Items to choose from, one of them chosen: there is always at least one,
/// and the choice is always one of them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choice<T> {
    before: Vec<T>,
    chosen: T,
    /// The items after the chosen one, the last first.
    after: Vec<T>,
}

impl<T> Choice<T> {
    /// `first` and the `rest`, `first` chosen.
    pub fn of(first: T, mut rest: Vec<T>) -> Choice<T> {
        rest.reverse();
        Choice {
            before: Vec::new(),
            chosen: first,
            after: rest,
        }
    }

    /// `items`, the first chosen; none if there are none.
    pub fn new(items: Vec<T>) -> Option<Choice<T>> {
        let mut items = items.into_iter();
        Some(Choice::of(items.next()?, items.collect()))
    }

    pub fn chosen(&self) -> &T {
        &self.chosen
    }

    /// Where the chosen item is, counted from the first.
    pub fn index(&self) -> usize {
        self.before.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = &T> + Clone {
        let chosen = std::iter::once(&self.chosen);
        self.before
            .iter()
            .chain(chosen)
            .chain(self.after.iter().rev())
    }

    /// Chooses the item `n` before, or the first.
    pub fn up(&mut self, n: usize) {
        for _ in 0..n {
            let Some(item) = self.before.pop() else { break };
            self.after.push(std::mem::replace(&mut self.chosen, item));
        }
    }

    /// Chooses the item `n` after, or the last.
    pub fn down(&mut self, n: usize) {
        for _ in 0..n {
            let Some(item) = self.after.pop() else { break };
            self.before.push(std::mem::replace(&mut self.chosen, item));
        }
    }

    /// Moves the choice for a key that moves it, `page` items for a page,
    /// and says whether `key` was one.
    pub fn moved(&mut self, key: Option<crate::keys::Key>, page: usize) -> bool {
        use crate::keys::{Direction, Key};
        match key {
            Some(Key::Arrow(Direction::Up)) => self.up(1),
            Some(Key::Arrow(Direction::Down)) => self.down(1),
            Some(Key::PageUp) => self.up(page),
            Some(Key::PageDown) => self.down(page),
            Some(Key::Home) => self.up(usize::MAX),
            Some(Key::End) => self.down(usize::MAX),
            _ => return false,
        }
        true
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct List {
    pub title: String,
    pub items: Choice<Item>,
    /// What the list was opened for; if it goes, the list closes.
    pub about: Option<AnyRef<WsId>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PromptFor {
    /// The command prompt: any fux command.
    Command,
    /// A new name for this target.
    Rename(AnyRef<WsId>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prompt {
    pub title: String,
    pub purpose: PromptFor,
    pub line: Line,
}

/// The most bytes a prompt's line holds.
pub const LINE_MAX: usize = 4096;

/// A line of text being typed, of at most `MAX` bytes, split at its
/// cursor: the cursor is always between two chars, or at an end, and every
/// edit is a whole char.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line<const MAX: usize = LINE_MAX> {
    before: String,
    after: String,
}

impl<const MAX: usize> Line<MAX> {
    /// `text`, the cursor after it.
    pub fn new(text: String) -> Line<MAX> {
        Line {
            before: text,
            after: String::new(),
        }
    }

    /// The text before the cursor.
    pub fn before(&self) -> &str {
        &self.before
    }

    /// The text after the cursor.
    pub fn after(&self) -> &str {
        &self.after
    }

    /// The whole text.
    pub fn text(self) -> String {
        self.before + &self.after
    }

    /// Types `text` at the cursor, unless the line would pass `MAX` bytes:
    /// whether it did.
    pub fn insert(&mut self, text: &str) -> bool {
        let len = self.before.len().saturating_add(self.after.len());
        let fits = len.saturating_add(text.len()) <= MAX;
        if fits {
            self.before.push_str(text);
        }
        fits
    }

    pub fn backspace(&mut self) {
        self.before.pop();
    }

    pub fn delete(&mut self) {
        self.after = self.after.chars().skip(1).collect();
    }

    pub fn left(&mut self) {
        if let Some(c) = self.before.pop() {
            self.after = std::iter::once(c).chain(self.after.chars()).collect();
        }
    }

    pub fn right(&mut self) {
        let mut chars = self.after.chars();
        if let Some(c) = chars.next() {
            self.before.push(c);
            self.after = chars.as_str().to_owned();
        }
    }

    pub fn home(&mut self) {
        self.before.push_str(&self.after);
        self.after = std::mem::take(&mut self.before);
    }

    pub fn end(&mut self) {
        self.before.push_str(&std::mem::take(&mut self.after));
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Confirm {
    pub question: String,
    pub command: Command,
    pub about: AnyRef<WsId>,
}

pub enum Mode {
    Normal,
    Column(Column),
    Repeat(Repeat),
    List(List),
    Prompt(Prompt),
    Confirm(Confirm),
    Copy(Box<Copy>),
}

pub struct View {
    pub id: ClientId,
    pub rows: u16,
    pub cols: u16,
    pub zoom: bool,
    pub mode: Mode,
    pub notice: Option<Notice>,
    pub decoder: Decoder,
    /// What the client's terminal said of itself (`outer`).
    pub terminal: crate::outer::Terminal,
    /// Something this view shows may have changed since its last paint.
    pub dirty: bool,
    /// When its terminal was last rung.
    pub last_bell: Option<std::time::Instant>,
    /// The title its terminal was last given, with `titles` on; `None`
    /// while it shows its own.
    pub title: Option<String>,
}

impl View {
    pub fn new(id: ClientId, rows: u16, cols: u16) -> View {
        View {
            id,
            rows,
            cols,
            zoom: false,
            mode: Mode::Normal,
            notice: None,
            decoder: Decoder::default(),
            terminal: crate::outer::Terminal::default(),
            dirty: true,
            last_bell: None,
            title: None,
        }
    }

    pub fn info(&mut self, text: impl Into<String>) {
        self.notice = Some(Notice {
            text: text.into(),
            error: false,
        });
        self.dirty = true;
    }

    pub fn error(&mut self, text: impl Into<String>) {
        self.notice = Some(Notice {
            text: text.into(),
            error: true,
        });
        self.dirty = true;
    }
}
