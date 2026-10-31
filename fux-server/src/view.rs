//! One attached client's own view of the shared state.
use crate::copy::Copy;
use fux_wire::act::Panel;
use fux_wire::id::{ClientId, TabId, WsId};
use fux_wire::layout::Rect;
use fux_wire::presentation::Presentation;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub text: String,
    pub error: bool,
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
    /// `resize`, and it sends no keys.
    Headless,
}

pub struct View {
    /// This connection's committed presentation, supplied at attach.
    pub presentation: Presentation,
    pub id: ClientId,
    pub kind: ClientKind,
    pub rows: u16,
    pub cols: u16,
    pub zoom: bool,
    /// The previous distinct places this attachment showed, across workspaces.
    pub last_tab: Option<TabId>,
    pub last_workspace: Option<WsId>,
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
}

impl View {
    pub fn new(id: ClientId, rows: u16, cols: u16, presentation: Presentation) -> View {
        View {
            presentation,
            id,
            kind: ClientKind::Terminal,
            rows,
            cols,
            zoom: false,
            last_tab: None,
            last_workspace: None,
            mode: Mode::Normal,
            opening: 0,
            notice: None,
            terminal: crate::outer::Terminal::default(),
            dirty: true,
            last_bell: None,
            title: None,
            read_only: false,
        }
    }

    /// The area panes share on its screen: all but the bar.
    pub fn pane_area(&self) -> Rect {
        let screen = Rect::screen(self.rows, self.cols);
        match self.presentation.bar.position() {
            fux_wire::presentation::BarPosition::Top => {
                screen.split(fux_wire::layout::Axis::Vertical, 1).1
            }
            fux_wire::presentation::BarPosition::Bottom => {
                Rect::screen(self.rows.saturating_sub(1), self.cols)
            }
            fux_wire::presentation::BarPosition::Off => screen,
        }
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
