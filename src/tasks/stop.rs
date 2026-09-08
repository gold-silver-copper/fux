//! Explicit termination of a recorded managed pane. Adopted resources are never targets.
use super::{launch, model::*, store::Store, submit};
use anyhow::{Context, Result};
use serde_json::{Value, json};
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
    anyhow::ensure!(
        launch.task.is_none(),
        "historical launch is not the current task stop target"
    );
    anyhow::ensure!(
        matches!(launch.phase, LaunchPhase::Attached | LaunchPhase::Closed),
        "launch has no reconciled ownership; reconcile it before requesting stop"
    );
    let session = launch
        .session
        .as_ref()
        .and_then(|id| store.journal().sessions.get(id))
        .context("managed session missing")?;
    anyhow::ensure!(
        session.ownership == Ownership::Managed && session.launch.as_deref() == Some(id),
        "session is not owned by this launch"
    );
    let target = session.target.clone();
    if !launch.stop_requested {
        store.transaction(|journal| {
            if journal
                .tasks
                .get(id)
                .is_none_or(|task| task.outcome != TaskOutcome::Verified)
            {
                super::cancel_journal(journal, id)?;
            }
            journal
                .launches
                .get_mut(id)
                .context("launch missing")?
                .stop_requested = true;
            Ok(())
        })?;
    }
    if launch.phase == LaunchPhase::Closed {
        return super::inspect_journal(store.journal(), id);
    }
    // Only the recorded live handle may be closed. A stale incarnation, workspace,
    // pane or PID is never replaced with a newly discovered target.
    if submit::verify_target(&target, Instant::now() + Duration::from_secs(2)).is_err() {
        return confirmed(launch::reconcile_store(store, id)?);
    }
    let killed = crate::fux::completed_until(
        &target.runtime.join(format!("{}.sock", target.workspace)),
        json!({"command":"kill","id":1,"instance":target.instance,"pane":target.pane}),
        Instant::now() + Duration::from_secs(2),
    )
    .and_then(|reply| {
        anyhow::ensure!(
            reply.get("id").and_then(Value::as_u64) == Some(1),
            "kill reply ID mismatch"
        );
        Ok(())
    });
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
