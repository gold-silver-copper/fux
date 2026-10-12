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
use crate::keys::decode::Reply;
use crate::keys::mouse::{MouseAction, MouseButton, MouseEvent};
use crate::keys::{Keystroke, Kitty, Modifiers};
use crate::wire::{Error, Reader, Wire};

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

fn tag(out: &mut Vec<u8>, tag: u8) {
    out.push(tag);
}

/// An enum of plain variants, as its index in `$all`.
macro_rules! plain {
    ($ty:ty, $what:literal, [$($variant:expr),+ $(,)?]) => {
        impl Wire for $ty {
            fn put(&self, out: &mut Vec<u8>) {
                let all = [$($variant),+];
                let index = all.iter().position(|v| v == self).unwrap_or(0);
                out.push(u8::try_from(index).unwrap_or(0));
            }
            fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
                let all = [$($variant),+];
                let index = u8::get(r)?;
                all.get(usize::from(index)).cloned().ok_or(Error::Tag($what, index))
            }
        }
    };
}

plain!(
    MouseAction,
    "mouse action",
    [
        MouseAction::Press,
        MouseAction::Release,
        MouseAction::Motion
    ]
);

impl Wire for Modifiers {
    fn put(&self, out: &mut Vec<u8>) {
        for flag in [self.ctrl, self.alt, self.shift] {
            flag.put(out);
        }
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        Ok(Modifiers {
            ctrl: bool::get(r)?,
            alt: bool::get(r)?,
            shift: bool::get(r)?,
        })
    }
}

impl Wire for Kitty {
    fn put(&self, out: &mut Vec<u8>) {
        self.code.put(out);
        self.shifted.put(out);
        self.base.put(out);
        self.mods.put(out);
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        Ok(Kitty {
            code: Option::get(r)?,
            shifted: Option::get(r)?,
            base: Option::get(r)?,
            mods: u8::get(r)?,
        })
    }
}

impl Wire for Keystroke {
    fn put(&self, out: &mut Vec<u8>) {
        self.press.put(out);
        self.kitty.put(out);
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        Ok(Keystroke {
            press: Wire::get(r)?,
            kitty: Option::get(r)?,
        })
    }
}

impl Wire for MouseEvent {
    fn put(&self, out: &mut Vec<u8>) {
        self.action.put(out);
        self.button.put(out);
        self.mods.put(out);
        self.row.put(out);
        self.col.put(out);
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        Ok(MouseEvent {
            action: MouseAction::get(r)?,
            button: Option::<MouseButton>::get(r)?,
            mods: Modifiers::get(r)?,
            row: u16::get(r)?,
            col: u16::get(r)?,
        })
    }
}

impl Wire for fux_vt::Rgb {
    fn put(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&[self.r, self.g, self.b]);
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        Ok(fux_vt::Rgb {
            r: u8::get(r)?,
            g: u8::get(r)?,
            b: u8::get(r)?,
        })
    }
}

impl Wire for Reply {
    fn put(&self, out: &mut Vec<u8>) {
        match self {
            Reply::Foreground(rgb) => {
                tag(out, 0);
                rgb.put(out);
            }
            Reply::Background(rgb) => {
                tag(out, 1);
                rgb.put(out);
            }
            Reply::Palette { index, rgb } => {
                tag(out, 2);
                index.put(out);
                rgb.put(out);
            }
            Reply::Scheme(scheme) => {
                tag(out, 3);
                (*scheme == crate::keys::colour::Scheme::Dark).put(out);
            }
            Reply::Mode { mode, status } => {
                tag(out, 4);
                mode.put(out);
                status.put(out);
            }
            Reply::KittyFlags(flags) => {
                tag(out, 5);
                flags.put(out);
            }
            Reply::Attributes { sixel } => {
                tag(out, 6);
                sixel.put(out);
            }
            Reply::CellSize { width, height } => {
                tag(out, 7);
                width.put(out);
                height.put(out);
            }
            Reply::UnderlineStyles => tag(out, 8),
        }
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        use crate::keys::colour::Scheme;
        Ok(match u8::get(r)? {
            0 => Reply::Foreground(Wire::get(r)?),
            1 => Reply::Background(Wire::get(r)?),
            2 => Reply::Palette {
                index: u8::get(r)?,
                rgb: Wire::get(r)?,
            },
            3 => Reply::Scheme(if bool::get(r)? {
                Scheme::Dark
            } else {
                Scheme::Light
            }),
            4 => Reply::Mode {
                mode: u16::get(r)?,
                status: u8::get(r)?,
            },
            5 => Reply::KittyFlags(u8::get(r)?),
            6 => Reply::Attributes {
                sixel: bool::get(r)?,
            },
            7 => Reply::CellSize {
                width: u16::get(r)?,
                height: u16::get(r)?,
            },
            8 => Reply::UnderlineStyles,
            other => return Err(Error::Tag("reply", other)),
        })
    }
}

impl Wire for ColumnEntry {
    fn put(&self, out: &mut Vec<u8>) {
        self.key.put(out);
        self.label.put(out);
        self.group.put(out);
        self.layer.put(out);
        self.root.put(out);
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        Ok(ColumnEntry {
            key: String::get(r)?,
            label: String::get(r)?,
            group: String::get(r)?,
            layer: bool::get(r)?,
            root: bool::get(r)?,
        })
    }
}

impl Wire for Column {
    fn put(&self, out: &mut Vec<u8>) {
        self.title.put(out);
        self.keys.put(out);
        self.entries.put(out);
        self.selected.put(out);
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        Ok(Column {
            title: String::get(r)?,
            keys: String::get(r)?,
            entries: Vec::get(r)?,
            selected: usize::get(r)?,
        })
    }
}

impl Wire for List {
    fn put(&self, out: &mut Vec<u8>) {
        self.title.put(out);
        self.items.put(out);
        self.current.put(out);
        self.selected.put(out);
        self.chooser.put(out);
        self.about.put(out);
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        Ok(List {
            title: String::get(r)?,
            items: Vec::get(r)?,
            current: Option::get(r)?,
            selected: usize::get(r)?,
            chooser: bool::get(r)?,
            about: Option::get(r)?,
        })
    }
}

impl Wire for Panel {
    fn put(&self, out: &mut Vec<u8>) {
        match self {
            Panel::Column(column) => {
                tag(out, 0);
                column.put(out);
            }
            Panel::Repeat(bar) => {
                tag(out, 1);
                bar.put(out);
            }
            Panel::List(list) => {
                tag(out, 2);
                list.put(out);
            }
            Panel::Prompt {
                title,
                before,
                after,
                about,
            } => {
                tag(out, 3);
                title.put(out);
                before.put(out);
                after.put(out);
                about.put(out);
            }
            Panel::Confirm { question, about } => {
                tag(out, 4);
                question.put(out);
                about.put(out);
            }
        }
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        Ok(match u8::get(r)? {
            0 => Panel::Column(Wire::get(r)?),
            1 => Panel::Repeat(String::get(r)?),
            2 => Panel::List(Wire::get(r)?),
            3 => Panel::Prompt {
                title: String::get(r)?,
                before: String::get(r)?,
                after: String::get(r)?,
                about: Option::get(r)?,
            },
            4 => Panel::Confirm {
                question: String::get(r)?,
                about: Wire::get(r)?,
            },
            other => return Err(Error::Tag("panel", other)),
        })
    }
}

/// Copy mode's moves with no data, in their order on the wire.
const COPY: [CopyAct; 26] = [
    CopyAct::Enter,
    CopyAct::Leave,
    CopyAct::Left,
    CopyAct::Right,
    CopyAct::Up,
    CopyAct::Down,
    CopyAct::WordForward,
    CopyAct::WordBack,
    CopyAct::FirstInk,
    CopyAct::LastInk,
    CopyAct::LineStart,
    CopyAct::Top,
    CopyAct::Bottom,
    CopyAct::HalfUp,
    CopyAct::HalfDown,
    CopyAct::PageUp,
    CopyAct::PageDown,
    CopyAct::Next,
    CopyAct::Previous,
    CopyAct::PromptBack,
    CopyAct::PromptForward,
    CopyAct::SelectChar,
    CopyAct::SelectLine,
    CopyAct::SelectBlock,
    CopyAct::OtherEnd,
    CopyAct::Yank,
];

impl Wire for CopyAct {
    fn put(&self, out: &mut Vec<u8>) {
        match self {
            CopyAct::Typing(line) => {
                tag(out, 254);
                line.put(out);
            }
            CopyAct::Find { query, forward } => {
                tag(out, 255);
                query.put(out);
                forward.put(out);
            }
            plain @ (CopyAct::Enter
            | CopyAct::Leave
            | CopyAct::Left
            | CopyAct::Right
            | CopyAct::Up
            | CopyAct::Down
            | CopyAct::WordForward
            | CopyAct::WordBack
            | CopyAct::FirstInk
            | CopyAct::LastInk
            | CopyAct::LineStart
            | CopyAct::Top
            | CopyAct::Bottom
            | CopyAct::HalfUp
            | CopyAct::HalfDown
            | CopyAct::PageUp
            | CopyAct::PageDown
            | CopyAct::Next
            | CopyAct::Previous
            | CopyAct::PromptBack
            | CopyAct::PromptForward
            | CopyAct::SelectChar
            | CopyAct::SelectLine
            | CopyAct::SelectBlock
            | CopyAct::OtherEnd
            | CopyAct::Yank) => {
                let index = COPY.iter().position(|c| c == plain).unwrap_or(0);
                out.push(u8::try_from(index).unwrap_or(0));
            }
        }
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        match u8::get(r)? {
            254 => Ok(CopyAct::Typing(Option::get(r)?)),
            255 => Ok(CopyAct::Find {
                query: String::get(r)?,
                forward: bool::get(r)?,
            }),
            index => {
                (COPY.get(usize::from(index)).cloned()).ok_or(Error::Tag("copy action", index))
            }
        }
    }
}

impl Wire for Act {
    fn put(&self, out: &mut Vec<u8>) {
        match self {
            Act::Key(k) => {
                tag(out, 0);
                k.put(out);
            }
            Act::Paste(text) => {
                tag(out, 1);
                text.put(out);
            }
            Act::Mouse(m) => {
                tag(out, 2);
                m.put(out);
            }
            Act::Focus(gained) => {
                tag(out, 3);
                gained.put(out);
            }
            Act::Reply(reply) => {
                tag(out, 4);
                reply.put(out);
            }
            Act::Show(panel) => {
                tag(out, 5);
                panel.put(out);
            }
            Act::Hide => tag(out, 6),
            Act::Notice { text, error } => {
                tag(out, 7);
                text.put(out);
                error.put(out);
            }
            Act::Copy(a) => {
                tag(out, 8);
                a.put(out);
            }
        }
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        Ok(match u8::get(r)? {
            0 => Act::Key(Wire::get(r)?),
            1 => Act::Paste(String::get(r)?),
            2 => Act::Mouse(Wire::get(r)?),
            3 => Act::Focus(bool::get(r)?),
            4 => Act::Reply(Wire::get(r)?),
            5 => Act::Show(Wire::get(r)?),
            6 => Act::Hide,
            7 => Act::Notice {
                text: String::get(r)?,
                error: bool::get(r)?,
            },
            8 => Act::Copy(Wire::get(r)?),
            other => return Err(Error::Tag("action", other)),
        })
    }
}
