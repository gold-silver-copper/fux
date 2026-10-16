//! One attached client's own view of the shared state.
use crate::copy::Copy;
use crate::id::ClientId;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub text: String,
    pub error: bool,
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

    /// An edit of the line: typed text (one line of it, nothing that
    /// controls), or a move or a deletion; Enter and Esc are the caller's.
    pub fn edit(&mut self, act: &crate::act::LineAct) {
        use crate::act::LineAct;
        match act {
            LineAct::Insert(text) => {
                let typed: String = (text.chars())
                    .take_while(|c| *c != '\n' && *c != '\r')
                    .filter(|c| !c.is_control())
                    .collect();
                self.insert(&typed);
            }
            LineAct::Backspace => self.backspace(),
            LineAct::Delete => self.delete(),
            LineAct::Left => self.left(),
            LineAct::Right => self.right(),
            LineAct::Home => self.home(),
            LineAct::End => self.end(),
            LineAct::Submit | LineAct::Cancel => {}
        }
    }
}

/// What a view shows over its panes: nothing, one of its client's
/// screens (the client owns it, and the server draws it), or copy mode,
/// whose cursor the server keeps.
pub enum Mode {
    Normal,
    Panel(crate::act::Panel),
    Copy(Box<Copy>),
}

/// Where a client shows what it is sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientKind {
    /// A terminal, which says its size and sends keys.
    Terminal,
    /// Nowhere a person sees (`COLS ROWS headless`): its size is set by
    /// `resize-client`, and it sends no keys.
    Headless,
}

pub struct View {
    pub id: ClientId,
    pub kind: ClientKind,
    pub rows: u16,
    pub cols: u16,
    pub zoom: bool,
    pub mode: Mode,
    pub notice: Option<Notice>,
    /// What the client's terminal said of itself (`outer`).
    pub terminal: crate::outer::Terminal,
    /// Something this view shows may have changed since its last paint.
    pub dirty: bool,
    /// When its terminal was last rung.
    pub last_bell: Option<std::time::Instant>,
    /// The title its terminal was last given, with `titles` on; `None`
    /// while it shows its own.
    pub title: Option<String>,
    /// It only watches (`watch`): its keys and clicks do nothing but
    /// detach it.
    pub read_only: bool,
    /// Its terminal's environment, of which panes started from its keys
    /// take what `update-environment` names.
    pub env: Vec<(String, String)>,
}

impl View {
    pub fn new(id: ClientId, rows: u16, cols: u16) -> View {
        View {
            id,
            kind: ClientKind::Terminal,
            rows,
            cols,
            zoom: false,
            mode: Mode::Normal,
            notice: None,
            terminal: crate::outer::Terminal::default(),
            dirty: true,
            last_bell: None,
            title: None,
            read_only: false,
            env: Vec::new(),
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
