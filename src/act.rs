//! What an attached client sends the server about its terminal: typed
//! actions, never keys to be read. The client reads its keyboard, runs its
//! own bindings and owns every mode: the command column, repeat modes,
//! menus, choosers, prompts, confirmations, copy mode and its search
//! (`crate::screens`). The server is sent a key only as literal input to
//! the pane the client focuses (`Act::Key`), what to draw of the client's
//! screens (`Act::Show`), and the moves of copy mode, whose cursor is the
//! server's as it alone holds the scrollback (`Act::Copy`).
use crate::command::AnyRef;
use crate::keys::colour::Scheme;
use crate::keys::decode::Reply;
use crate::keys::mouse::{MouseAction, MouseEvent};
use crate::keys::{Keystroke, Kitty, Modifiers};
use crate::wire::{closed_wire, wire_enum, wire_struct};
use fux_vt::ModeStatus;

/// One action of an attached client.
#[derive(Clone, Debug, PartialEq)]
pub enum Act {
    /// A key typed, as literal input to the focused pane.
    Key(Keystroke),
    /// Pasted text, for the focused pane.
    Paste(String),
    Mouse(MouseEvent),
    Focus(bool),
    /// The terminal's answer to a question fux asked it.
    Reply(Reply),
    /// Draw one of the client's screens over its panes.
    Show(Panel),
    /// Draw none: the client is back to its panes (copy mode left too).
    Hide,
    /// A notice, from a binding's program: an error if `error`.
    Notice {
        text: String,
        error: bool,
    },
    Copy(CopyAct),
}

/// What the server draws of a client's screen: the client holds all of its
/// state, and sends it again as it changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Panel {
    /// The command column: the client's bindings of one layer.
    Column(Column),
    /// A repeat mode's bar: `RESIZE  h j k l · Esc`.
    Repeat(String),
    /// A menu or a chooser.
    List(List),
    /// A line being typed: the command prompt, a rename (of `about`).
    Prompt {
        title: String,
        before: String,
        after: String,
        about: Option<AnyRef>,
    },
    /// A question answered y or n, about closing `about`.
    Confirm { question: String, about: AnyRef },
}

/// The command column as a client shows it.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Column {
    pub title: String,
    /// The keys typed so far, as the bar shows them: `C-b t`.
    pub keys: String,
    pub entries: Vec<ColumnEntry>,
    pub selected: usize,
}

/// One binding of a column: its key, what it does, the group it is listed
/// under, and whether it opens a layer of more keys.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColumnEntry {
    pub key: String,
    pub label: String,
    pub group: String,
    pub layer: bool,
    /// Bound without the prefix: listed after the rest, under headings of
    /// their own.
    pub root: bool,
}

/// A menu or a chooser, as drawn: its rows, the one marked current, the one
/// selected, and whether `r` and `x` act on its rows (a chooser's do).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct List {
    pub title: String,
    pub items: Vec<String>,
    pub current: Option<usize>,
    pub selected: usize,
    pub chooser: bool,
    /// What it was opened for: if it goes, the server closes the list.
    pub about: Option<AnyRef>,
}

/// What moves, selects and copies in copy mode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CopyAct {
    /// Copy mode starts, on the focused pane.
    Enter,
    Leave,
    Left,
    Right,
    Up,
    Down,
    WordForward,
    WordBack,
    /// The first glyph of the line.
    FirstInk,
    /// The last glyph of the line.
    LastInk,
    LineStart,
    Top,
    Bottom,
    HalfUp,
    HalfDown,
    PageUp,
    PageDown,
    Next,
    Previous,
    /// The cursor to the next shell prompt, or the one before.
    Prompt(Seek),
    SelectChar,
    SelectLine,
    SelectBlock,
    /// The other end of the selection.
    OtherEnd,
    /// Copy what is selected, and leave.
    Yank,
    /// The search line the client is typing, for the bar: forward or back,
    /// and its text before and after the cursor; none once it is done.
    Typing(Option<(Seek, String, String)>),
    /// Find `query`, forward or back, from the cursor.
    Find {
        query: String,
        seek: Seek,
    },
}

/// Which way a search goes: toward later rows, or earlier ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Seek {
    Forward,
    Backward,
}

impl Seek {
    /// The other way.
    pub fn reversed(self) -> Seek {
        match self {
            Seek::Forward => Seek::Backward,
            Seek::Backward => Seek::Forward,
        }
    }
}

/// What moves a list (a menu, a chooser), made of the client's keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListAct {
    Move(Move),
    Choose,
    Cancel,
    /// Rename what the chosen entry is about.
    Rename,
    /// Close what the chosen entry is about.
    Close,
}

/// A move of the selection in a list, or in the command column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Move {
    Up,
    Down,
    PageUp,
    PageDown,
    First,
    Last,
}

/// The rows a list, and the command column, page by.
pub const PAGE: usize = 10;

impl Move {
    /// The row it selects from row `at` of a list whose last is `last`.
    pub fn from(self, at: usize, last: usize) -> usize {
        match self {
            Move::Up => at.saturating_sub(1),
            Move::Down => at.saturating_add(1).min(last),
            Move::PageUp => at.saturating_sub(PAGE),
            Move::PageDown => at.saturating_add(PAGE).min(last),
            Move::First => 0,
            Move::Last => last,
        }
    }
}

/// What a line being typed (the prompt, a copy-mode search) is given, made
/// of the client's keys: an edit, or Enter (`Submit`) or Esc (`Cancel`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LineAct {
    Edit(Edit),
    Submit,
    Cancel,
}

/// An edit of a line being typed: text, a move or a deletion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Edit {
    Insert(String),
    Backspace,
    Delete,
    Left,
    Right,
    Home,
    End,
}

wire_enum!(MouseAction {
    Press,
    Release,
    Motion,
});
closed_wire!(ModeStatus, "mode status"; Scheme, "scheme");
wire_struct!(Modifiers { ctrl, alt, shift });
wire_struct!(Kitty {
    code,
    shifted,
    base,
    mods
});
wire_struct!(Keystroke { press, kitty });
wire_struct!(MouseEvent {
    action,
    button,
    mods,
    row,
    col
});
wire_struct!(fux_vt::Rgb { r, g, b });
wire_enum!(Reply {
    Foreground(rgb),
    Background(rgb),
    Palette { index, rgb },
    Scheme(scheme),
    Mode { mode, status },
    KittyFlags(flags),
    Attributes { sixel },
    CellSize { width, height },
    UnderlineStyles,
});
wire_struct!(ColumnEntry {
    key,
    label,
    group,
    layer,
    root
});
wire_struct!(Column {
    title,
    keys,
    entries,
    selected
});
wire_struct!(List {
    title,
    items,
    current,
    selected,
    chooser,
    about
});
wire_enum!(Panel {
    Column(column),
    Repeat(bar),
    List(list),
    Prompt { title, before, after, about },
    Confirm { question, about },
});
wire_enum!(CopyAct {
    Enter,
    Leave,
    Left,
    Right,
    Up,
    Down,
    WordForward,
    WordBack,
    FirstInk,
    LastInk,
    LineStart,
    Top,
    Bottom,
    HalfUp,
    HalfDown,
    PageUp,
    PageDown,
    Next,
    Previous,
    Prompt(seek),
    SelectChar,
    SelectLine,
    SelectBlock,
    OtherEnd,
    Yank,
    Typing(line),
    Find { query, seek },
});
wire_enum!(Seek { Forward, Backward });
wire_enum!(Act {
    Key(k),
    Paste(text),
    Mouse(m),
    Focus(gained),
    Reply(reply),
    Show(panel),
    Hide,
    Notice { text, error },
    Copy(a),
});
