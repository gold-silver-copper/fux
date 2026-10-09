//! One attached client's own view of the shared state.
use crate::command::{AnyRef, Command};
use crate::copy::Copy;
use crate::decode::Decoder;
use crate::id::{ClientId, PaneId, TabId, WsId};
use crate::keys::KeyPress;
use std::collections::{BTreeMap, BTreeSet};

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
    /// What `r` renames and `x` closes, in a chooser.
    pub subject: Option<AnyRef>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct List {
    pub title: String,
    pub items: Vec<Item>,
    pub selected: usize,
    /// A chooser takes `r` and `x`; a menu does not.
    pub chooser: bool,
    /// What the list was opened for; if it goes, the list closes.
    pub about: Option<AnyRef>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PromptFor {
    /// The command prompt: any fux command.
    Command,
    /// A new name for this target.
    Rename(AnyRef),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prompt {
    pub title: String,
    pub purpose: PromptFor,
    pub line: Line,
}

/// The most bytes a prompt's line holds.
const LINE_MAX: usize = 4096;

/// A prompt's one line of text, split at its cursor: the cursor is always
/// between two chars, or at an end, and every edit is a whole char.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    before: String,
    after: String,
}

impl Line {
    /// `text`, the cursor after it.
    pub fn new(text: String) -> Line {
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

    /// Types `text` at the cursor, unless the line would pass `LINE_MAX`
    /// bytes: whether it did.
    pub fn insert(&mut self, text: &str) -> bool {
        let len = self.before.len().saturating_add(self.after.len());
        let fits = len.saturating_add(text.len()) <= LINE_MAX;
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
    pub about: AnyRef,
}

pub enum Mode {
    Normal,
    /// The command column: the layer it shows (none for the bindings right
    /// after the prefix), and its selected entry.
    Column {
        path: Vec<KeyPress>,
        selected: usize,
    },
    /// A repeat mode: the keys of the layer at `path` run its bindings
    /// without the prefix, until Esc.
    Repeat {
        path: Vec<KeyPress>,
    },
    List(List),
    Prompt(Prompt),
    Confirm(Confirm),
    Copy(Box<Copy>),
}

pub struct View {
    pub id: ClientId,
    pub rows: u16,
    pub cols: u16,
    pub workspace: WsId,
    /// Per workspace, the selected tab.
    pub tab_of: BTreeMap<WsId, TabId>,
    /// Per tab, the focused pane.
    pub focus_of: BTreeMap<TabId, PaneId>,
    /// Per tab, the pane focused before it (`select-pane --last`).
    pub last_of: BTreeMap<TabId, PaneId>,
    pub zoom: bool,
    pub mode: Mode,
    pub notice: Option<Notice>,
    pub decoder: Decoder,
    /// What the client's terminal said of itself (`outer`).
    pub terminal: crate::outer::Terminal,
    /// Something this view shows may have changed since its last paint.
    pub dirty: bool,
    /// The pane a mouse button was pressed in and not yet released: its
    /// motion and release go to it, kept to its edge (`Session::mouse`).
    pub mouse_held: Option<PaneId>,
    /// Tabs of its workspace whose panes rang the bell while it showed
    /// another, marked in its bar until it shows them (`Session::ring`).
    pub bells: BTreeSet<TabId>,
    /// When its terminal was last rung.
    pub last_bell: Option<std::time::Instant>,
    /// The title its terminal was last given, with `titles` on; `None`
    /// while it shows its own.
    pub title: Option<String>,
}

impl View {
    pub fn new(id: ClientId, rows: u16, cols: u16, workspace: WsId) -> View {
        View {
            id,
            rows,
            cols,
            workspace,
            tab_of: BTreeMap::new(),
            focus_of: BTreeMap::new(),
            last_of: BTreeMap::new(),
            zoom: false,
            mode: Mode::Normal,
            notice: None,
            decoder: Decoder::default(),
            terminal: crate::outer::Terminal::default(),
            dirty: true,
            mouse_held: None,
            bells: BTreeSet::new(),
            last_bell: None,
            title: None,
        }
    }

    pub fn tab(&self) -> Option<TabId> {
        self.tab_of.get(&self.workspace).copied()
    }

    pub fn focus(&self) -> Option<PaneId> {
        self.tab().and_then(|t| self.focus_of.get(&t)).copied()
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

    /// Focuses `pane` in `tab`, remembering the pane it replaces.
    pub fn set_focus(&mut self, tab: TabId, pane: PaneId) {
        if let Some(old) = self.focus_of.insert(tab, pane)
            && old != pane
        {
            self.last_of.insert(tab, old);
        }
    }
}
