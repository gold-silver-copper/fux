//! One attached client's own view of the shared state.
use crate::command::{AnyRef, ClientId, TabId, WsId};
use crate::copy::Copy;
use crate::decode::Decoder;
use crate::keys::KeyPress;
use crate::layout::PaneId;
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub text: String,
    pub error: bool,
}

/// One row of a chooser or an action menu: what it shows and the command
/// line it runs, with every target written out, so it acts on the item it
/// was built for and never on another.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub label: String,
    pub argv: Vec<String>,
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
    /// The `:` prompt: any fux command.
    Command,
    /// A new name for this target.
    Rename(AnyRef),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prompt {
    pub title: String,
    pub purpose: PromptFor,
    pub text: String,
    /// A char index into `text`.
    pub cursor: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Confirm {
    pub question: String,
    pub argv: Vec<String>,
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
    pub tab_of: HashMap<WsId, TabId>,
    /// Per tab, the focused pane and the one focused before it.
    pub focus_of: HashMap<TabId, PaneId>,
    pub last_of: HashMap<TabId, PaneId>,
    pub zoom: bool,
    pub mode: Mode,
    pub notice: Option<Notice>,
    pub decoder: Decoder,
    /// Something this view shows may have changed since its last paint.
    pub dirty: bool,
}

impl View {
    pub fn new(id: ClientId, rows: u16, cols: u16, workspace: WsId) -> View {
        View {
            id,
            rows,
            cols,
            workspace,
            tab_of: HashMap::new(),
            focus_of: HashMap::new(),
            last_of: HashMap::new(),
            zoom: false,
            mode: Mode::Normal,
            notice: None,
            decoder: Decoder::default(),
            dirty: true,
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
