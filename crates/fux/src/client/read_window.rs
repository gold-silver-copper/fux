//! Bounded, correlated history reads. Local input is independent of this window.
use crate::ids::PaneId;
use std::collections::BTreeMap;
use tokio::time::Instant;

const MAX_READS: usize = 8;

#[derive(Default)]
pub struct ReadWindow {
    pending: BTreeMap<u64, (PaneId, Instant)>,
}
impl ReadWindow {
    pub fn has_capacity(&self) -> bool {
        self.pending.len() < MAX_READS
    }
    pub fn insert(&mut self, request: u64, pane: PaneId, sent: Instant) {
        tracing::debug!(target: "fux::diagnostics", pid = std::process::id(), event = "history_read", request, pane = pane.0, pending = self.pending.len());
        self.pending
            .insert(request, (pane, sent + crate::proto::attach::FRAME_TIMEOUT));
    }
    pub fn complete(&mut self, request: u64, pane: PaneId) -> bool {
        if self
            .pending
            .get(&request)
            .is_some_and(|(expected, _)| *expected == pane)
        {
            tracing::debug!(target: "fux::diagnostics", pid = std::process::id(), event = "history_complete", request, pane = pane.0);
            self.pending.remove(&request);
            true
        } else {
            tracing::debug!(target: "fux::diagnostics", pid = std::process::id(), event = "history_stale_reply", request, pane = pane.0);
            false
        }
    }
    pub fn retain(&mut self, mut keep: impl FnMut(u64, PaneId) -> bool) {
        self.pending
            .retain(|request, (pane, _)| keep(*request, *pane));
    }
    pub fn deadline(&self) -> Option<Instant> {
        self.pending.values().map(|(_, deadline)| *deadline).min()
    }
    pub fn expire(&mut self, now: Instant) -> Vec<(u64, PaneId)> {
        let expired = self
            .pending
            .iter()
            .filter(|(_, (_, deadline))| *deadline <= now)
            .map(|(request, (pane, _))| (*request, *pane))
            .collect::<Vec<_>>();
        for (request, _) in &expired {
            tracing::debug!(target: "fux::diagnostics", pid = std::process::id(), event = "history_timeout", request);
            self.pending.remove(request);
        }
        expired
    }
}

#[cfg(test)]
#[allow(clippy::panic)]
mod tests {
    use super::*;
    #[test]
    fn replies_match_both_identity_fields_and_do_not_extend_deadlines() {
        let mut window = ReadWindow::default();
        let sent = Instant::now();
        window.insert(1, PaneId(2), sent);
        let deadline = window
            .deadline()
            .unwrap_or_else(|| panic!("missing fixture value"));
        assert!(!window.complete(1, PaneId(3)));
        assert!(!window.complete(2, PaneId(2)));
        window.insert(2, PaneId(3), sent + std::time::Duration::from_millis(1));
        assert_eq!(window.deadline(), Some(deadline));
        assert_eq!(window.expire(deadline), vec![(1, PaneId(2))]);
        assert!(window.complete(2, PaneId(3)));
        assert!(window.deadline().is_none());
    }
    #[test]
    fn capacity_is_bounded_and_completion_releases_one_slot() {
        let mut window = ReadWindow::default();
        for id in 0..MAX_READS as u64 {
            window.insert(id, PaneId(1), Instant::now());
        }
        assert!(!window.has_capacity());
        assert!(window.complete(0, PaneId(1)));
        assert!(window.has_capacity());
    }
}
