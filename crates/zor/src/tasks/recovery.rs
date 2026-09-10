//! Reconcile abandoned checks and previously persisted stop intent; never infer cleanup authority.
use super::{
    model::{CheckPhase, LaunchPhase},
    store::Store,
};
use anyhow::Result;
use serde_json::{Value, json};
use std::{collections::VecDeque, path::Path};

/// One finite sweep of retained launches when a service starts. It never submits
/// Prepared launches or creates storage for an observation-only service.
#[derive(Default)]
pub(crate) struct Startup {
    pending: Option<VecDeque<String>>,
}
impl Startup {
    pub(crate) fn step(&mut self, root: &Path) -> Result<()> {
        if self.pending.as_ref().is_some_and(VecDeque::is_empty) {
            return Ok(());
        }
        if !root.join("journal.json").try_exists()? {
            self.pending = Some(VecDeque::new());
            return Ok(());
        }
        // Busy leaves the cursor untouched. The journal bounds the snapshot to
        // 128 launches; later commands reconcile their own new launch identities.
        let mut store = Store::open(root)?;
        let pending = self.pending.get_or_insert_with(|| {
            store
                .journal()
                .launches
                .values()
                .filter(|launch| {
                    (launch.task.is_none() || (launch.resume.is_some() && launch.session.is_none()))
                        && !matches!(launch.phase, LaunchPhase::Prepared | LaunchPhase::Closed)
                })
                .map(|launch| launch.id.clone())
                .collect()
        });
        let Some(id) = pending.pop_front() else {
            return Ok(());
        };
        // Re-read under this lock: a stop may have closed the launch since the
        // snapshot. Reconciliation records uncertainty on unavailable evidence
        // and cannot relaunch, send input, or infer termination authority.
        super::launch::reconcile_store(&mut store, &id).map(|_| ())
    }
}

/// One bounded attempt per call. Cursor rotation prevents an uncertain target starving others.
pub fn resume(root: &Path, after: Option<&str>) -> Result<Value> {
    // Observation-only service startup must not initialize task storage.
    if !root.join("journal.json").try_exists()? {
        return Ok(json!({"selected":null,"pending":0}));
    }
    let mut store = Store::open(root)?;
    let recovered_checks = recover_checks(&mut store)?;
    let candidates: Vec<_> = store
        .journal()
        .launches
        .values()
        .filter(|launch| {
            launch.task.is_none() && launch.stop_requested && launch.phase == LaunchPhase::Attached
        })
        .map(|launch| launch.id.clone())
        .collect();
    let selected = candidates
        .iter()
        .find(|id| after.is_none_or(|after| id.as_str() > after))
        .or_else(|| candidates.first());
    let Some(id) = selected else {
        return Ok(json!({"selected":null,"pending":0,"recovered_checks":recovered_checks}));
    };
    // Selection and execution share the same Store lock. No new stop intent is created:
    // only this exact already-authorized managed launch can reach run_store.
    let result = super::stop::run_store(&mut store, id);
    Ok(match result {
        Ok(_) => {
            json!({"selected":id,"pending":candidates.len()-1,"recovered_checks":recovered_checks,"outcome":"closed"})
        }
        Err(error) => {
            json!({"selected":id,"pending":candidates.len(),"recovered_checks":recovered_checks,"outcome":"unresolved",
            "problem":format!("{error:#}").chars().take(512).collect::<String>()})
        }
    })
}

/// One bounded journal transaction, deferred while any cooperating runner is alive.
/// Vacant admission proves only that no runner can still publish a result. A child
/// may remain alive: retain Uncertain (including the worktree removal guard).
fn recover_checks(store: &mut Store) -> Result<usize> {
    if !store
        .journal()
        .checks
        .values()
        .any(|check| check.phase == CheckPhase::Submitted)
    {
        return Ok(0);
    }
    let Some(_exclusive) = store.check_runners(true)? else {
        return Ok(0);
    };
    store.transaction(|journal| {
        let mut count = 0;
        for check in journal.checks.values_mut().filter(|check| check.phase == CheckPhase::Submitted) {
            check.phase = CheckPhase::Uncertain;
            check.problem = Some("check runner lost before result publication; execution may continue; do not replay".into());
            for name in check.artifact_requests.keys() {
                check.artifact_problems.insert(name.clone(), "runner lost; artifact capture outcome unavailable".into());
            }
            count += 1;
        }
        Ok(count)
    })
}
