//! What an attached client sends the server about its terminal: typed
//! actions, never keys to be read. The client reads its keyboard, runs its
//! own bindings and owns every mode: the command column, repeat modes,
//! menus, choosers, prompts, confirmations, copy mode and its search
//! (`crate::screens`). The server is sent a key only as literal input to
//! the pane the client focuses (`Act::Key`), what to draw of the client's
//! screens (`Act::Show`), and the moves of copy mode, whose cursor is the
//! server's as it alone holds the scrollback (`Act::Copy`).
use crate::command::AnyRef;
use crate::id::WsId;
use crate::keys::colour::Scheme;
use crate::keys::decode::Reply;
use crate::keys::mouse::{MouseAction, MouseEvent};
use crate::keys::{Keystroke, Kitty, Modifiers};
use crate::wire::{plain_wire, wire_enum, wire_struct};

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
        about: Option<AnyRef<WsId>>,
    },
    /// A question answered y or n, about closing `about`.
    Confirm {
        question: String,
        about: AnyRef<WsId>,
    },
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
    pub about: Option<AnyRef<WsId>>,
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
    PromptBack,
    PromptForward,
    SelectChar,
    SelectLine,
    SelectBlock,
    /// The other end of the selection.
    OtherEnd,
    /// Copy what is selected, and leave.
    Yank,
    /// The search line the client is typing, for the bar: forward or back,
    /// and its text before and after the cursor; none once it is done.
    Typing(Option<(bool, String, String)>),
    /// Find `query`, forward or back, from the cursor.
    Find {
        query: String,
        forward: bool,
    },
}

/// What moves a list (a menu, a chooser), made of the client's keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListAct {
    Up,
    Down,
    PageUp,
    PageDown,
    First,
    Last,
    Choose,
    Cancel,
    /// Rename what the chosen entry is about.
    Rename,
    /// Close what the chosen entry is about.
    Close,
}

/// What edits a line being typed (the prompt, a copy-mode search), made of
/// the client's keys.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LineAct {
    Insert(String),
    Backspace,
    Delete,
    Left,
    Right,
    Home,
    End,
    Submit,
    Cancel,
}

plain_wire!(
    MouseAction,
    "mouse action",
    [
        MouseAction::Press,
        MouseAction::Release,
        MouseAction::Motion
    ]
);
plain_wire!(Scheme, "scheme", [Scheme::Light, Scheme::Dark]);
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
wire_enum!(Reply, "reply" {
    0 => Foreground(rgb),
    1 => Background(rgb),
    2 => Palette { index, rgb },
    3 => Scheme(scheme),
    4 => Mode { mode, status },
    5 => KittyFlags(flags),
    6 => Attributes { sixel },
    7 => CellSize { width, height },
    8 => UnderlineStyles,
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
wire_enum!(Panel, "panel" {
    0 => Column(column),
    1 => Repeat(bar),
    2 => List(list),
    3 => Prompt { title, before, after, about },
    4 => Confirm { question, about },
});
wire_enum!(CopyAct, "copy action" {
    0 => Enter,
    1 => Leave,
    2 => Left,
    3 => Right,
    4 => Up,
    5 => Down,
    6 => WordForward,
    7 => WordBack,
    8 => FirstInk,
    9 => LastInk,
    10 => LineStart,
    11 => Top,
    12 => Bottom,
    13 => HalfUp,
    14 => HalfDown,
    15 => PageUp,
    16 => PageDown,
    17 => Next,
    18 => Previous,
    19 => PromptBack,
    20 => PromptForward,
    21 => SelectChar,
    22 => SelectLine,
    23 => SelectBlock,
    24 => OtherEnd,
    25 => Yank,
    254 => Typing(line),
    255 => Find { query, forward },
});
wire_enum!(Act, "action" {
    0 => Key(k),
    1 => Paste(text),
    2 => Mouse(m),
    3 => Focus(gained),
    4 => Reply(reply),
    5 => Show(panel),
    6 => Hide,
    7 => Notice { text, error },
    8 => Copy(a),
});
