//! One attached client's own view of the shared state.
use crate::command::{AnyRef, Command};
use crate::copy::Copy;
use crate::decode::Decoder;
use crate::id::ClientId;
use crate::keys::KeyPress;

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
    pub text: String,
    /// A char index into `text`.
    pub cursor: usize,
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
