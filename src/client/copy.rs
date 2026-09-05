//! Selection and viewport state owned by one attached viewer.
use super::CopyMode;
use crate::state::{PaneId, PaneView, WorkspaceState};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CopyUi {
    pub view: Option<(PaneId, PaneView)>,
    pub clipboard: Option<(u64, String)>,
}

impl CopyUi {
    pub fn apply(&self, shared: &WorkspaceState) -> Option<WorkspaceState> {
        let (pane, view) = self.view.as_ref()?;
        let current = shared.pane(*pane)?;
        if (current.rows, current.columns) != (view.rows, view.columns) {
            return None;
        }
        let mut local = shared.clone();
        local
            .update_pane(*pane, |current| *current = view.clone())
            .ok()?;
        let mut tabs = local.tabs().to_vec();
        if let Some(tab) = tabs.iter_mut().find(|tab| tab.layout.contains(*pane)) {
            tab.focused = *pane;
            if tab.zoomed.is_some() {
                tab.zoomed = Some(*pane);
            }
            let active = tab.id;
            local.replace_tabs(tabs, Some(active)).ok()?;
            local.replace_popups(Vec::new()).ok()?;
        } else {
            let popups = local
                .popups()
                .iter()
                .filter(|popup| popup.pane == *pane)
                .cloned()
                .collect();
            local.replace_popups(popups).ok()?;
        }
        Some(local)
    }
}

pub type MouseLayout = Vec<(PaneId, ratatui_core::layout::Rect)>;

pub struct CopySession {
    pane: PaneId,
    state: WorkspaceState,
    selection: CopyMode,
    read: bool,
    error: Option<&'static str>,
}
impl CopySession {
    pub fn new(pane: PaneId, view: PaneView) -> Option<Self> {
        let mut selection = CopyMode::default();
        selection.enter(&view);
        selection.bind_target(pane);
        let mut state = WorkspaceState::default();
        state.insert_pane(pane, view).ok()?;
        selection.sync(&mut state, pane);
        Some(Self {
            pane,
            state,
            selection,
            read: true,
            error: None,
        })
    }
    pub const fn pane(&self) -> PaneId {
        self.pane
    }

    pub fn view(&self) -> Option<(PaneId, PaneView)> {
        Some((self.pane, self.state.pane(self.pane)?.clone()))
    }
    pub fn reconcile(&mut self, shared: &WorkspaceState) -> bool {
        let Some(current) = shared.pane(self.pane) else {
            return false;
        };
        if self
            .state
            .pane(self.pane)
            .is_some_and(|view| (view.rows, view.columns) != (current.rows, current.columns))
        {
            self.selection.clear_selection();
            self.selection.sync(&mut self.state, self.pane);
            self.read = true;
        }
        true
    }
    pub fn selecting(&self) -> bool {
        self.state
            .pane(self.pane)
            .is_some_and(|view| view.copy.anchor.is_some())
    }
    pub fn active(&self) -> bool {
        self.selection.active()
    }
    pub fn clipboard(&self) -> &str {
        &self.state.metadata().clipboard_base64
    }
    pub fn error(&self) -> Option<&'static str> {
        self.error
    }
    pub fn take_read(&mut self) -> Option<(u32, u32)> {
        if !std::mem::take(&mut self.read) {
            return None;
        }
        Some((self.pane.0, self.state.pane(self.pane)?.viewport_offset))
    }
    pub fn install(&mut self, view: PaneView) -> bool {
        if self
            .state
            .pane(self.pane)
            .is_some_and(|old| (old.rows, old.columns) != (view.rows, view.columns))
        {
            self.selection.enter(&view);
            self.selection.bind_target(self.pane);
        }
        if self
            .state
            .update_pane(self.pane, |pane| *pane = view)
            .is_err()
        {
            return false;
        }
        self.selection.sync(&mut self.state, self.pane);
        true
    }
    pub fn escape(&mut self) -> bool {
        self.error = None;
        if self.selecting() {
            self.selection.clear_selection();
            self.selection.sync(&mut self.state, self.pane);
            true
        } else {
            false
        }
    }
    pub fn mouse(&mut self, row: u16, column: u16, release: bool) {
        self.error = None;
        if let Some(view) = self.state.pane(self.pane) {
            self.selection.shift_drag(row, column, release, view);
            self.selection.sync(&mut self.state, self.pane);
        }
    }
    pub fn key(&mut self, key: char) {
        self.error = None;
        if matches!(key, 'y' | '\r' | '\n')
            && self.selecting()
            && let Some(view) = self.state.pane(self.pane)
            && self.selection.selected_text(view).len().div_ceil(3) * 4
                > crate::state::MAX_CLIPBOARD_BYTES
        {
            self.error =
                Some("Selection exceeds clipboard limit · Esc clear · select a smaller region");
            return;
        }
        let byte = match key {
            'h' | 'j' | 'k' | 'l' | 'u' | 'd' | ' ' | 'y' | '\r' | 'q' => key as u8,
            '\n' => b'\r',
            _ => return,
        };
        let old = self.state.pane(self.pane).map(|view| view.viewport_offset);
        self.selection.key(&[byte], &mut self.state, self.pane);
        if old != self.state.pane(self.pane).map(|view| view.viewport_offset) {
            // A selection refers to the displayed cells. Clear it before replacing
            // that viewport so scrolling cannot silently copy different text.
            self.selection.clear_selection();
            self.selection.sync(&mut self.state, self.pane);
            self.read = true;
        }
    }
}
