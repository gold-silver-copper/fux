//! One attached client's own view of the shared state.
use crate::act::{Edit, Panel};
use crate::copy::Copy;
use crate::id::ClientId;
use crate::layout::Rect;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub text: String,
    pub error: bool,
}

/// The most rows, and columns, a client's view has.
pub const MOST: u16 = 4096;

/// `rows` by `cols`, if a client's view can be that size: 1 to `MOST`
/// each way; else why not.
pub fn size(rows: u16, cols: u16) -> Result<(u16, u16), String> {
    let fits = |n: u16| (1..=MOST).contains(&n);
    match fits(rows) && fits(cols) {
        true => Ok((rows, cols)),
        false => Err(format!(
            "{cols} columns by {rows} rows: a client is 1 to {MOST} each way"
        )),
    }
}

/// A size a client's screen, and so a tab, may be: 1 to `MOST` rows and
/// columns. Every size a request gives is one, so each size the server
/// holds is one a dump writes (`COLS ROWS size`) and makes again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Screen(fux_vt::Size);

impl Screen {
    /// `rows` by `cols`, if a screen can be that size; else why not.
    pub fn new(rows: u16, cols: u16) -> Result<Screen, String> {
        let (rows, cols) = size(rows, cols)?;
        fux_vt::Size::new(rows, cols)
            .map(Screen)
            .map_err(|e| e.to_string())
    }
    pub fn size(self) -> fux_vt::Size {
        self.0
    }
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

    /// An edit of the line: typed text (its control characters, and the
    /// line end a pasted line ends in, left out), or a move or a deletion.
    /// Text of more than one line, or that would take the line past `MAX`
    /// bytes, is refused whole, saying why.
    pub fn edit(&mut self, edit: &Edit) -> Result<(), String> {
        match edit {
            Edit::Insert(text) => {
                let text = (text
                    .strip_suffix("\r\n")
                    .or_else(|| text.strip_suffix(['\n', '\r'])))
                .unwrap_or(text);
                if text.contains(['\n', '\r']) {
                    return Err("a line holds one line of text: nothing was typed".to_owned());
                }
                let typed: String = text.chars().filter(|c| !c.is_control()).collect();
                let len = self.before.len().saturating_add(self.after.len());
                if len.saturating_add(typed.len()) > MAX {
                    return Err(format!("a line holds {MAX} bytes: nothing was typed"));
                }
                self.before.push_str(&typed);
            }
            Edit::Backspace => drop(self.before.pop()),
            Edit::Delete => self.after = self.after.chars().skip(1).collect(),
            Edit::Left => {
                if let Some(c) = self.before.pop() {
                    self.after = std::iter::once(c).chain(self.after.chars()).collect();
                }
            }
            Edit::Right => {
                let mut chars = self.after.chars();
                if let Some(c) = chars.next() {
                    self.before.push(c);
                    self.after = chars.as_str().to_owned();
                }
            }
            Edit::Home => {
                self.before.push_str(&self.after);
                self.after = std::mem::take(&mut self.before);
            }
            Edit::End => self.before.push_str(&std::mem::take(&mut self.after)),
        }
        Ok(())
    }
}

/// What a view shows over its panes: nothing, one of its client's
/// screens (the client owns it, and the server draws it), or copy mode,
/// whose cursor the server keeps.
pub enum Mode {
    Normal,
    Panel(Panel),
    Copy(Box<Copy>),
}

impl Mode {
    /// Whether nothing shows over the panes, so they have the client: its
    /// pictures, the program's cursor, the mouse, pastes and the composing
    /// memo are the panes' only then.
    pub fn is_normal(&self) -> bool {
        match self {
            Mode::Normal => true,
            Mode::Panel(_) | Mode::Copy(_) => false,
        }
    }
}

/// Where a client shows what it is sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientKind {
    /// A terminal, which says its size and sends keys.
    Terminal,
    /// Nowhere a person sees (`opts COLS ROWS size attach`): its size is set by
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
    /// The latest opening, retained after it ends to refuse stale redraws.
    pub opening: u64,
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
    pub env: Vec<(String, std::ffi::OsString)>,
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
            opening: 0,
            notice: None,
            terminal: crate::outer::Terminal::default(),
            dirty: true,
            last_bell: None,
            title: None,
            read_only: false,
            env: Vec::new(),
        }
    }

    /// The area panes share on its screen: all but the bar.
    pub fn pane_area(&self) -> Rect {
        Rect::screen(self.rows.saturating_sub(1), self.cols)
    }

    /// Whether it sizes the panes of the tab it shows: one that only
    /// watches shows them as they are, and a screen with no room below its
    /// bar shows none.
    pub fn sizes(&self) -> bool {
        !self.read_only && !self.pane_area().is_empty()
    }

    /// Shows a notice, an error's if `error`.
    pub fn notify(&mut self, text: impl Into<String>, error: bool) {
        let text = text.into();
        self.notice = Some(Notice { text, error });
        self.dirty = true;
    }
}
