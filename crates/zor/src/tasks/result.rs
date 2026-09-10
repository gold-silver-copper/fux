//! A bounded, read-only assembly of retained evidence, separate from task verification.
use super::{model::*, store::Store};
use anyhow::Result;
use serde_json::{Value, json};
use std::path::Path;

pub fn read(root: &Path, id: &str) -> Result<Value> {
    let store = Store::open(root)?;
    assemble(root, store.journal(), id)
}
fn assemble(root: &Path, journal: &Journal, id: &str) -> Result<Value> {
    let task = journal.tasks.get(id);
    let launch = journal.launches.get(id);
    anyhow::ensure!(task.is_some() || launch.is_some(), "task not found");
    let attempt = task.and_then(|task| journal.attempts.get(&task.attempt));
    let session = attempt.and_then(|attempt| journal.sessions.get(&attempt.session));
    let worktree = launch
        .and_then(|launch| launch.worktree.as_ref())
        .and_then(|id| journal.worktrees.get(id));
    let checks: Vec<_> = journal
        .checks
        .values()
        .filter(|check| check.task == id)
        .collect();
    let requirements: Vec<_> = task
        .into_iter()
        .flat_map(|task| task.required_checks.keys())
        .map(|name| {
            let latest = checks
                .iter()
                .filter(|check| {
                    attempt.is_some_and(|attempt| check.attempt == attempt.id)
                        && check.requirement.as_ref() == Some(name)
                })
                .max_by_key(|check| check.created_generation);
            let status = match latest {
                None => "missing",
                Some(check) => match check.phase {
                    CheckPhase::Submitted => "submitted",
                    CheckPhase::Uncertain => "uncertain",
                    CheckPhase::Finished if check.exit_code == Some(0) => "passed",
                    CheckPhase::Finished => "failed",
                },
            };
            json!({"name":name,"status":status,"execution":latest.map(|check| &check.id)})
        })
        .collect();
    let check_summaries: Vec<_> = checks.iter().map(|check| json!({"id":check.id,"attempt":check.attempt,"source":check.source,"artifact_requests":check.artifact_requests,"artifact_problems":check.artifact_problems,"requirement":check.requirement,
        "phase":check.phase,"created_generation":check.created_generation,"exit_code":check.exit_code,
        "signal":check.signal,"truncated":check.truncated})).collect();
    let artifacts: Vec<_> = journal
        .artifacts
        .values()
        .filter(|artifact| artifact.task == id)
        .map(|artifact| {
            json!({"id":artifact.id,"check":artifact.check,"source":artifact.source,"attempt":artifact.attempt,"size":artifact.bytes.len(),
            "created_ms":artifact.created_ms,"created_generation":artifact.created_generation,"requirement":artifact.requirement})
        })
        .collect();
    let mut prompts: Vec<_> = journal
        .prompts
        .values()
        .filter(|prompt| attempt.is_some_and(|attempt| prompt.attempt == attempt.id))
        .collect();
    prompts.sort_by(|a, b| (b.created_ms, &b.id).cmp(&(a.created_ms, &a.id)));
    let omitted_prompts = prompts.len().saturating_sub(32);
    let unresolved_prompts = prompts
        .iter()
        .filter(|prompt| {
            !prompt.released
                && !matches!(
                    prompt.wait,
                    WaitOutcome::ResponseObserved
                        | WaitOutcome::ProcessExited
                        | WaitOutcome::Cancelled
                )
        })
        .count();
    let reports: Vec<_> = prompts.iter().take(32).map(|prompt| json!({"operation":prompt.id,
        "handoff_predecessors":prompt.handoff.as_ref().map(|handoff| &handoff.predecessors),
        "delivery":prompt.delivery,"wait":prompt.wait,"released":prompt.released,"claim":prompt.response})).collect();
    let required_artifacts = super::artifact::requirements(journal, id);
    let captures = super::check_artifacts::summaries(journal, id);
    let mut blockers = Vec::new();
    if captures.iter().any(|entry| entry["status"] == "pending") {
        blockers.push("artifact-capture-pending");
    }
    if captures.iter().any(|entry| entry["status"] == "failed") {
        blockers.push("artifact-capture-failed");
    }
    if required_artifacts
        .iter()
        .any(|entry| entry["status"] == "missing")
    {
        blockers.push("required-artifacts-missing");
    }
    if task.is_none() {
        blockers.push("launch-not-attached");
    }
    if task.is_some_and(|task| task.outcome == TaskOutcome::Cancelled) {
        blockers.push("task-cancelled");
    }
    if task.is_some_and(|task| task.outcome == TaskOutcome::Failed) {
        blockers.push("task-failed");
    }
    if attempt.is_some_and(|attempt| {
        matches!(attempt.state, AttemptState::Lost | AttemptState::Uncertain)
    }) {
        blockers.push("attempt-unresolved");
    }
    if unresolved_prompts > 0 {
        blockers.push("prompt-unresolved");
    }
    if requirements
        .iter()
        .any(|required| required.get("status").and_then(Value::as_str) != Some("passed"))
    {
        blockers.push("required-checks-not-passed");
    }
    if launch.is_some_and(|launch| launch.problem.is_some()) {
        blockers.push("launch-problem");
    }
    if worktree.is_some_and(|tree| tree.problem.is_some()) {
        blockers.push("worktree-problem");
    }
    let final_output = launch.and_then(|launch| launch.final_evidence.as_ref());
    let changes = journal
        .changes
        .values()
        .filter(|changes| changes.task == id)
        .max_by_key(|changes| changes.created_generation);
    let sources: Vec<_> = journal
        .sources
        .values()
        .filter(|source| source.task == id)
        .map(super::source::summary)
        .collect();
    let value = json!({"v":1,"generation":journal.generation,"task_id":id,
        "task_outcome":task.map(|task| &task.outcome),"attempt":attempt,
        "agent":session.and_then(|session| session.agent.as_ref()).or_else(|| launch.and_then(|launch| launch.agent.as_ref())),
        "session":session,
        "integration":launch.and_then(|launch| launch.integration.as_ref()).map(|integration|
            json!({"kind":integration.kind,"producer":integration.producer,
                "registered_ms":integration.registered_ms,"availability":"not-probed",
                "heartbeat":launch.map(|launch| super::heartbeat::view(root, launch, journal))})),
        "launch":launch.map(|launch| json!({"id":launch.id,"phase":launch.phase,"problem":launch.problem,"cwd":launch.cwd})),
        "worktree":worktree.map(|tree| json!({"id":tree.id,"path":tree.path,"branch":tree.branch,"phase":tree.phase,
            "base_commit":tree.commit,"problem":tree.problem})),
        "output":{"source":"retained-launch-final","available":final_output.is_some(),"evidence":final_output},
        "artifact_captures":captures,"sources":sources,"artifacts":artifacts,"required_artifacts":required_artifacts,"checks":check_summaries,"required_checks":requirements,
        "prompt_evidence":{"source":"caller-reported-claims-and-receipts","latest":reports,
            "omitted_count":omitted_prompts,"unresolved_count":unresolved_prompts},
        "blockers":blockers,
        "verification":{"status":if journal.verifications.contains_key(id) {"verified"} else {"unverified"},"record":journal.verifications.get(id),"scope":"declared-checks-and-artifacts-for-retained-source","source_check_binding":"per-check-optional-retained-git-tree","required_artifact_policy":"retained-bytes"},
        "changed_files":{"status":if changes.is_some() { "retained-observation" } else { "not-collected" },
            "evidence":changes,"ignored_files":"excluded","atomic_snapshot":false}});
    anyhow::ensure!(
        serde_json::to_vec(&value)?.len() <= 262144,
        "result view exceeds 256 KiB; inspect individual records"
    );
    Ok(value)
}
