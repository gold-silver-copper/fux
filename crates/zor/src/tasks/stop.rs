//! Explicit termination of a recorded managed pane. Adopted resources are never targets.
use super::{launch, model::*, store::Store, submit};
use anyhow::{Context, Result};
use serde_json::Value;
use std::{
    path::Path,
    time::{Duration, Instant},
};

pub fn run(root: &Path, id: &str) -> Result<Value> {
    let mut store = Store::open(root)?;
    run_store(&mut store, id)
}

pub(super) fn run_store(store: &mut Store, id: &str) -> Result<Value> {
    let launch = store
        .journal()
        .launches
        .get(id)
        .context("stop requires a managed launch; adoption grants no termination authority")?
        .clone();
    let target = store.journal().managed_stop_target(id)?;
    if !launch.stop_requested {
        store.transaction(|journal| journal.request_managed_stop(id))?;
    }
    if launch.phase == LaunchPhase::Closed {
        return super::inspect_journal(store.journal(), id);
    }
    // Only the recorded live handle may be closed. Routing may move, but a stale
    // server, pane or PID is never replaced with a newly discovered target.
    if submit::verify_target(&target, Instant::now() + Duration::from_secs(2)).is_err() {
        return confirmed(launch::reconcile_store(store, id)?);
    }
    let killed = submit::mutate(
        &target,
        crate::fux::pane::Action::Kill,
        Instant::now() + Duration::from_secs(2),
    );
    // A completed kill reply accepts closure; only retained final evidence proves
    // release. Lost replies also reconcile without changing the recorded target.
    match launch::reconcile_store(store, id) {
        Ok(value) => {
            if value.pointer("/launch/phase").and_then(Value::as_str) == Some("closed") {
                Ok(value)
            } else {
                killed.context("stop reply lost or rejected; retry stop for the same launch ID")?;
                confirmed(value)
            }
        }
        Err(error) => Err(error.context(
            "stop requested; pane release is not yet confirmed; retry the same launch ID",
        )),
    }
}

fn confirmed(value: Value) -> Result<Value> {
    anyhow::ensure!(
        value.pointer("/launch/phase").and_then(Value::as_str) == Some("closed"),
        "stop requested; pane release is not yet confirmed; retry the same launch ID"
    );
    Ok(value)
}
