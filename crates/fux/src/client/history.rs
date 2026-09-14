//! Bounded viewer-private history retention and read scheduling.
//! Keyboard mode, application focus and UI notifications belong to the controller.
use super::{copy::CopySession, effects::Identity};
use crate::{ids::PaneId, proto::attach::ViewReply, view::Frame};

const MAX_PASSIVE_VIEWS: usize = 64;

#[derive(Default)]
pub(super) struct Histories {
    sessions: Vec<CopySession>,
    identity: Option<Identity>,
    read_cursor: usize,
}

pub(super) enum Install {
    Installed,
    Invalidated,
    Unowned(ViewReply),
}

impl Histories {
    pub fn iter(&self) -> impl Iterator<Item = &CopySession> {
        self.sessions.iter()
    }
    pub fn last(&self) -> Option<&CopySession> {
        self.sessions.last()
    }
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }
    pub fn dismiss(&mut self) -> bool {
        self.sessions.pop().is_some()
    }
    pub fn resume(&mut self, pane: Option<PaneId>) {
        self.sessions.retain(|copy| Some(copy.pane()) != pane);
    }
    /// Taking a passive viewport transfers ownership to keyboard Copy or a new
    /// wheel interaction; remembering it afterwards updates most-recent order.
    pub fn take(&mut self, pane: PaneId) -> Option<CopySession> {
        let index = self.sessions.iter().position(|copy| copy.pane() == pane)?;
        Some(self.sessions.remove(index))
    }
    pub fn remember(&mut self, copy: CopySession) {
        self.take(copy.pane());
        if self.sessions.len() >= MAX_PASSIVE_VIEWS {
            self.sessions.remove(0);
        }
        self.sessions.push(copy);
    }
    pub fn pending(&self, request: u64, pane: PaneId) -> bool {
        self.sessions
            .iter()
            .any(|copy| copy.pending_matches(request, pane))
    }
    pub fn fail(&mut self, request: u64, pane: PaneId) -> bool {
        let before = self.sessions.len();
        self.sessions
            .retain(|copy| !copy.pending_matches(request, pane));
        self.sessions.len() != before
    }
    pub fn install(&mut self, reply: ViewReply) -> Install {
        let Some(index) = self
            .sessions
            .iter()
            .position(|copy| copy.pane() == reply.pane)
        else {
            return Install::Unowned(reply);
        };
        if self
            .sessions
            .get_mut(index)
            .is_some_and(|copy| !copy.install(reply))
        {
            self.sessions.remove(index);
            Install::Invalidated
        } else {
            Install::Installed
        }
    }
    /// Drop old passive views first. False means the separately owned keyboard
    /// viewport alone exceeds the budget; the controller decides how to dismiss it.
    pub fn fit_budget(&mut self, keyboard_bytes: usize, limit: usize) -> bool {
        let mut total = self
            .sessions
            .iter()
            .map(CopySession::retained_bytes)
            .fold(keyboard_bytes, usize::saturating_add);
        while total > limit && !self.sessions.is_empty() {
            total = total.saturating_sub(self.sessions.remove(0).retained_bytes());
        }
        total <= limit
    }
    /// Reconcile attachment identity and visible geometry. Returning true tells
    /// the controller that its separate keyboard interaction may also be stale.
    pub fn reconcile(&mut self, frame: &Frame) -> bool {
        let identity = Identity::of(frame);
        let changed = self
            .identity
            .as_ref()
            .is_some_and(|previous| *previous != identity);
        if changed {
            self.sessions.clear();
        }
        self.identity = Some(identity);
        self.sessions.retain_mut(|copy| {
            let Some(entry) = frame.layout.iter().find(|entry| entry.pane == copy.pane()) else {
                return false;
            };
            let Some(live) = frame.pane(copy.pane()) else {
                return false;
            };
            if !copy.same_buffer(live) {
                return false;
            }
            copy.set_viewport(entry.rect.height, entry.rect.width);
            copy.refresh_live(live);
            true
        });
        changed
    }
    /// One slot per passive view plus the independent keyboard Copy slot.
    /// The caller checks the global outstanding-read capacity before taking work.
    pub fn take_read(
        &mut self,
        mut keyboard: Option<&mut CopySession>,
    ) -> Option<(u64, PaneId, u32)> {
        let count = self.sessions.len() + 1;
        for _ in 0..count {
            let index = self.read_cursor % count;
            self.read_cursor = (index + 1) % count;
            let copy = if index == self.sessions.len() {
                keyboard.as_deref_mut()
            } else {
                self.sessions.get_mut(index)
            };
            if let Some(read) = copy.and_then(CopySession::take_read) {
                return Some(read);
            }
        }
        None
    }
}
