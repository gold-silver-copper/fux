//! What an attached client sends the server about its terminal: typed
//! actions, never keys to be read. The client reads its keyboard, runs its
//! own bindings and owns every mode: the command column, repeat modes,
//! choosers, prompts, confirmations, copy mode and its search
//! (`crate::screens`). The server is sent a key only as literal input to
//! the pane the client focuses (`Act::Key`), what to draw of the client's
//! screens (`Act::Show`), and the moves of copy mode, whose cursor is the
//! server's as it alone holds the scrollback (`Act::Copy`).
use crate::command::AnyRef;
use crate::keys::colour::Scheme;
use crate::keys::decode::Reply;
use crate::keys::mouse::{MouseAction, MouseEvent};
use crate::keys::{KeyPress, Keystroke, Kitty, Modifiers};
use crate::wire::{closed_wire, wire, wire_enum, wire_struct};
use fux_vt::ModeStatus;

wire! {
    /// One action of an attached client.
    #[derive(Clone, Debug, PartialEq)]
    pub enum Act {
        /// A key typed, as literal input to the focused pane.
        Key(stroke: Keystroke),
        /// Pasted text, for the focused pane.
        Paste(text: String),
        Mouse(event: MouseEvent),
        Focus(gained: bool),
        /// The terminal's answer to a question fux asked it.
        Reply(reply: Reply),
        /// Draw one of the client's screens over its panes.
        Show(panel: Panel),
        /// Draw none: the client is back to its panes (copy mode left too).
        Hide,
        /// A notice, from a binding's program: an error if `error`.
        Notice {
            text: String,
            error: bool,
        },
        Copy(act: CopyAct),
    }
}

wire! {
    /// What the server draws of a client's screen: the client holds all of its
    /// state, and sends it again as it changes.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum Panel {
        /// The command column: the client's bindings of one layer.
        Column(column: Column),
        /// A repeat mode's bar: `RESIZE  h j k l · Esc`.
        Repeat(bar: String),
        /// A chooser.
        List(list: List),
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
}

wire! {
    /// The command column as a client shows it.
    #[derive(Clone, Debug, PartialEq, Eq, Default)]
    pub struct Column {
        pub title: String,
        /// The keys typed so far, as the bar shows them: `C-b t`.
        pub keys: String,
        pub entries: Vec<ColumnEntry>,
        pub selected: usize,
    }
}

wire! {
    /// One binding of a column: its key, what it does, the group it is listed
    /// under, and whether it opens a layer of more keys.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct ColumnEntry {
        pub key: KeyPress,
        pub label: String,
        pub group: String,
        pub layer: bool,
        /// Bound without the prefix: listed after the rest, under headings of
        /// their own.
        pub root: bool,
    }
}

wire! {
    /// A chooser, as drawn: its rows, the one marked current, and the one
    /// selected.
    #[derive(Clone, Debug, PartialEq, Eq, Default)]
    pub struct List {
        pub title: String,
        pub items: Vec<String>,
        pub current: Option<usize>,
        pub selected: usize,
        /// What it was opened for: if it goes, the server closes the list.
        pub about: Option<AnyRef>,
    }
}

wire! {
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
        Prompt(seek: Seek),
        SelectChar,
        SelectLine,
        SelectBlock,
        /// The other end of the selection.
        OtherEnd,
        /// Copy what is selected, and leave.
        Yank,
        /// The search line the client is typing, for the bar: forward or back,
        /// and its text before and after the cursor; none once it is done.
        Typing(line: Option<(Seek, String, String)>),
        /// Find `query`, forward or back, from the cursor.
        Find {
            query: String,
            seek: Seek,
        },
    }
}

wire! {
    /// Which way a search goes: toward later rows, or earlier ones.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Seek {
        Forward,
        Backward,
    }
}

/// What moves a chooser, made of the client's keys.
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
    Press(button),
    Release(button),
    Motion(button),
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
