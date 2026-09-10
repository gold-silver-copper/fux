use std::{
    collections::BTreeSet,
    time::{Duration, Instant},
};

/// Pending keys are always a subset of the latest bounded dashboard snapshot.
pub(super) struct Attention {
    seen: BTreeSet<String>,
    pending: BTreeSet<String>,
    next: Instant,
}
impl Attention {
    pub(super) fn new() -> Self {
        Self {
            seen: BTreeSet::new(),
            pending: BTreeSet::new(),
            next: Instant::now(),
        }
    }
    pub(super) fn observe(&mut self, current: BTreeSet<String>) -> bool {
        let changed = !current.is_subset(&self.seen);
        self.pending.retain(|key| current.contains(key));
        self.pending.extend(current.difference(&self.seen).cloned());
        self.seen = current;
        changed
    }
    pub(super) fn lost(&mut self) {
        self.pending.clear();
    }
    pub(super) fn take(&mut self, fresh: &BTreeSet<String>, now: Instant) -> Option<usize> {
        self.pending.retain(|key| fresh.contains(key));
        if self.pending.is_empty() || now < self.next {
            return None;
        }
        let count = self.pending.len();
        self.pending.clear();
        self.next = now + Duration::from_secs(5);
        Some(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn keys(values: &[&str]) -> BTreeSet<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }
    #[test]
    fn cooldown_coalesces_new_keys_and_removes_resolved_entries() {
        let mut attention = Attention::new();
        let now = Instant::now();
        assert!(attention.observe(keys(&["a"])));
        assert_eq!(attention.take(&keys(&["a"]), now), Some(1));
        assert!(!attention.observe(keys(&["a"])));
        assert!(attention.observe(keys(&["a", "b", "c"])));
        assert_eq!(
            attention.take(&keys(&["a", "b", "c"]), now + Duration::from_secs(1)),
            None
        );
        assert!(attention.observe(keys(&["a", "c", "d"])));
        assert_eq!(
            attention.take(&keys(&["a", "c", "d"]), now + Duration::from_secs(5)),
            Some(2)
        );
        assert_eq!(
            attention.take(&keys(&["a", "c", "d"]), now + Duration::from_secs(10)),
            None
        );
    }
    #[test]
    fn aged_or_lost_pending_evidence_cannot_deliver_later() {
        let mut attention = Attention::new();
        let now = Instant::now();
        attention.observe(keys(&["a"]));
        assert_eq!(attention.take(&keys(&[]), now), None);
        assert_eq!(
            attention.take(&keys(&["a"]), now + Duration::from_secs(5)),
            None
        );
        attention.observe(keys(&["a", "b"]));
        attention.lost();
        assert_eq!(
            attention.take(&keys(&["a", "b"]), now + Duration::from_secs(10)),
            None
        );
        assert!(!attention.observe(keys(&["a", "b"])));
        attention.observe(keys(&["a"]));
        assert!(attention.observe(keys(&["a", "b"])));
        assert_eq!(
            attention.take(&keys(&["a", "b"]), now + Duration::from_secs(11)),
            Some(1)
        );
    }
}
