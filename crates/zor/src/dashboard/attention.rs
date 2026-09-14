use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};

/// Keep last authoritative attention across transport outages, but only deliver
/// from currently fresh evidence. Each machine is reconciled independently.
pub(super) struct Machines {
    seen: BTreeMap<String, (String, BTreeSet<String>)>,
    attention: Attention,
}
impl Machines {
    pub(super) fn new() -> Self {
        Self {
            seen: BTreeMap::new(),
            attention: Attention::new(),
        }
    }
    pub(super) fn observe(
        &mut self,
        observations: &[crate::machines::supervision::Observation],
        now: Instant,
    ) -> BTreeSet<String> {
        self.seen
            .retain(|id, _| observations.iter().any(|item| &item.machine_id == id));
        let mut fresh = BTreeSet::new();
        for item in observations {
            if !item.fresh(now) {
                continue;
            }
            let Some(view) = &item.view else { continue };
            let saved = self.seen.entry(item.machine_id.clone()).or_default();
            if saved.0 != view.service_instance {
                *saved = (view.service_instance.clone(), BTreeSet::new());
            }
            let mut current = BTreeSet::new();
            for row in &view.rows {
                let Some(selection) = item.selection(&row.key) else {
                    continue;
                };
                let key = format!("{selection:?}");
                if item.row_fresh(row, now) {
                    if row.attention {
                        fresh.insert(key.clone());
                        current.insert(key);
                    }
                } else if saved.1.contains(&key) {
                    current.insert(key);
                }
            }
            saved.1 = current;
        }
        self.attention.observe(
            self.seen
                .values()
                .flat_map(|(_, keys)| keys.iter().cloned())
                .collect(),
        );
        // Remove pending stale evidence even while the delivery subprocess is busy.
        self.attention.pending.retain(|key| fresh.contains(key));
        fresh
    }
    pub(super) fn take(&mut self, fresh: &BTreeSet<String>, now: Instant) -> Option<usize> {
        self.attention.take(fresh, now)
    }
}

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
    fn machine(id: &str, now: Instant) -> crate::machines::supervision::Observation {
        crate::machines::supervision::Observation {
            machine_id: id.into(),
            machine_name: "same-name".into(),
            observed_at: Some(now),
            problem: None,
            view: Some(crate::dashboard::View {
                service_instance: "same-service".into(),
                observation_sequence: 1,
                task_generation: None,
                state_directory: "/remote/state".into(),
                stale: false,
                problems: vec![],
                rows: vec![crate::dashboard::Row {
                    expected: None,
                    target: None,
                    key: "same-row".into(),
                    kind: "task".into(),
                    label: "private task title".into(),
                    status: "blocked".into(),
                    task_outcome: None,
                    attention: true,
                    detail: "private details".into(),
                    age_upper_bound_ms: None,
                    evidence: None,
                }],
            }),
        }
    }
    #[test]
    fn machines_coalesce_distinct_identities_without_realerting_on_transport_recovery()
    -> anyhow::Result<()> {
        let mut notices = Machines::new();
        let now = Instant::now();
        let mut first = machine("first", now);
        let mut second = machine("second", now);
        let fresh = notices.observe(&[first.clone(), second.clone()], now);
        assert_eq!(notices.take(&fresh, now), Some(2));
        first.problem = Some("offline".into());
        second.observed_at = Some(now + Duration::from_secs(6));
        let fresh = notices.observe(
            &[first.clone(), second.clone()],
            now + Duration::from_secs(6),
        );
        assert_eq!(fresh.len(), 1);
        assert_eq!(notices.take(&fresh, now + Duration::from_secs(6)), None);
        first.problem = None;
        first.observed_at = Some(now + Duration::from_secs(6));
        first.machine_name = "renamed".into();
        let fresh = notices.observe(
            &[first.clone(), second.clone()],
            now + Duration::from_secs(6),
        );
        assert_eq!(notices.take(&fresh, now + Duration::from_secs(6)), None);
        first
            .view
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("view"))?
            .service_instance = "new-service".into();
        let fresh = notices.observe(&[first, second], now + Duration::from_secs(6));
        assert_eq!(notices.take(&fresh, now + Duration::from_secs(6)), Some(1));
        Ok(())
    }
    #[test]
    fn machine_pending_attention_is_discarded_when_stale_even_if_delivery_is_busy() {
        let mut notices = Machines::new();
        let now = Instant::now();
        let mut first = machine("first", now);
        notices.observe(&[first.clone()], now);
        let fresh = notices.observe(&[first.clone()], now + Duration::from_secs(6));
        assert!(fresh.is_empty());
        first.observed_at = Some(now + Duration::from_secs(6));
        let fresh = notices.observe(&[first], now + Duration::from_secs(6));
        assert_eq!(notices.take(&fresh, now + Duration::from_secs(6)), None);
        notices.observe(&[], now);
        assert!(notices.seen.is_empty());
    }
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
