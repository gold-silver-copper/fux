//! Explicit immutable selection of policy evidence, never inferred from agent state.
use super::{model::*, store::Store};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

struct Selection {
    checks: BTreeMap<String, String>,
    artifacts: BTreeMap<String, String>,
}
fn select(journal: &Journal, task: &Task, source: &str) -> Result<Selection> {
    let input = journal
        .sources
        .get(source)
        .context("verification source not found")?;
    anyhow::ensure!(
        input.task == task.id && input.attempt == task.attempt,
        "verification source belongs to another task/attempt"
    );
    anyhow::ensure!(
        !task.required_checks.is_empty(),
        "verification requires at least one declared check"
    );
    anyhow::ensure!(
        !journal
            .checks
            .values()
            .any(|check| check.task == task.id && check.phase == CheckPhase::Submitted),
        "task has outstanding check executions; inspect/reconcile their evidence first"
    );
    let mut checks = BTreeMap::new();
    for name in task.required_checks.keys() {
        let check = journal
            .checks
            .values()
            .filter(|check| check.task == task.id && check.requirement.as_ref() == Some(name))
            .max_by_key(|check| check.created_generation)
            .with_context(|| format!("required check {name} has no execution"))?;
        anyhow::ensure!(
            check.phase == CheckPhase::Finished
                && check.exit_code == Some(0)
                && check.source.as_deref() == Some(source)
                && check.artifact_problems.is_empty(),
            "latest required check {name} must pass on source {source} without capture failures"
        );
        checks.insert(name.clone(), check.id.clone());
    }
    let mut artifacts = BTreeMap::new();
    for name in task.required_artifacts.keys() {
        let check = journal
            .checks
            .values()
            .filter(|check| check.task == task.id && check.artifact_requests.contains_key(name))
            .max_by_key(|check| check.created_generation)
            .with_context(|| format!("required artifact {name} has no bound capture"))?;
        anyhow::ensure!(
            checks.values().any(|id| id == &check.id),
            "latest capture of {name} must belong to a selected required check"
        );
        let artifact = check
            .artifact_requests
            .get(name)
            .and_then(|id| journal.artifacts.get(id))
            .with_context(|| format!("required artifact {name} was not captured"))?;
        let latest = journal
            .artifacts
            .values()
            .filter(|artifact| {
                artifact.task == task.id && artifact.requirement.as_ref() == Some(name)
            })
            .max_by_key(|artifact| artifact.created_generation)
            .context("required artifact disappeared")?;
        anyhow::ensure!(
            latest.id == artifact.id && artifact.source.as_deref() == Some(source),
            "latest retained artifact {name} is not bound to the selected source/check"
        );
        artifacts.insert(name.clone(), artifact.id.clone());
    }
    Ok(Selection { checks, artifacts })
}
pub(super) fn validate(journal: &Journal) -> Result<()> {
    anyhow::ensure!(
        journal.verifications.len() <= 128,
        "verification record limit exceeded"
    );
    let mut generations = BTreeSet::new();
    for (key, record) in &journal.verifications {
        let task = journal
            .tasks
            .get(key)
            .context("verification task missing")?;
        anyhow::ensure!(
            key == &record.task
                && task.outcome == TaskOutcome::Verified
                && record.attempt == task.attempt
                && record.created_generation > 0
                && record.created_generation <= journal.generation
                && generations.insert(record.created_generation),
            "invalid verification identity/generation"
        );
        let selected = select(journal, task, &record.source)?;
        anyhow::ensure!(
            selected.checks == record.checks && selected.artifacts == record.artifacts,
            "sealed verification evidence does not match current immutable policy/history"
        );
        anyhow::ensure!(
            journal
                .sources
                .get(&record.source)
                .is_some_and(|source| source.created_generation < record.created_generation)
                && record.checks.values().all(|id| journal
                    .checks
                    .get(id)
                    .is_some_and(|check| check.created_generation < record.created_generation))
                && record
                    .artifacts
                    .values()
                    .all(
                        |id| journal.artifacts.get(id).is_some_and(|artifact| artifact
                            .created_generation
                            < record.created_generation)
                    ),
            "verification references evidence from its future"
        );
    }
    Ok(())
}
fn view(journal: &Journal, id: &str) -> Result<Value> {
    let record = journal
        .verifications
        .get(id)
        .context("verification not found")?;
    Ok(json!({"generation":journal.generation,"verification":record}))
}
pub fn run(root: &Path, id: &str, source: &str) -> Result<Value> {
    let mut store = Store::open(root)?;
    if let Some(record) = store.journal().verifications.get(id) {
        anyhow::ensure!(
            record.source == source,
            "task was verified against another source"
        );
        return view(store.journal(), id);
    }
    let task = store
        .journal()
        .tasks
        .get(id)
        .context("task not found")?
        .clone();
    anyhow::ensure!(
        task.outcome == TaskOutcome::Open,
        "task outcome is already final"
    );
    let attempt = store
        .journal()
        .attempts
        .get(&task.attempt)
        .context("task attempt missing")?;
    anyhow::ensure!(
        !matches!(attempt.state, AttemptState::Lost | AttemptState::Uncertain),
        "task attempt needs reconciliation"
    );
    let launch = store
        .journal()
        .launches
        .get(id)
        .context("verification needs a managed task")?;
    anyhow::ensure!(
        launch.problem.is_none(),
        "managed launch needs reconciliation"
    );
    anyhow::ensure!(
        !store
            .journal()
            .prompts
            .values()
            .any(|prompt| prompt.attempt == task.attempt
                && (super::active(prompt)
                    || (!prompt.released
                        && !matches!(
                            prompt.wait,
                            WaitOutcome::ResponseObserved
                                | WaitOutcome::ProcessExited
                                | WaitOutcome::Cancelled
                        )))),
        "task has unresolved prompt coordination"
    );
    let selected = select(store.journal(), &task, source)?;
    let record = Verification {
        task: id.into(),
        attempt: task.attempt.clone(),
        source: source.into(),
        created_ms: super::now_ms()?,
        created_generation: store
            .journal()
            .generation
            .checked_add(1)
            .context("journal generation exhausted")?,
        checks: selected.checks,
        artifacts: selected.artifacts,
    };
    store.transaction(|journal| {
        journal
            .tasks
            .get_mut(id)
            .context("task disappeared")?
            .outcome = TaskOutcome::Verified;
        for prompt in journal
            .prompts
            .values_mut()
            .filter(|prompt| prompt.attempt == task.attempt)
        {
            prompt.released = true;
        }
        journal.verifications.insert(id.into(), record);
        Ok(())
    })?;
    view(store.journal(), id)
}
