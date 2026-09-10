//! Durable task coordination in zor. Adoption/prepare operations never own or write a PTY.
pub mod artifact;
pub mod binding;
pub mod capabilities;
pub mod changes;
pub mod check;
mod check_artifacts;
pub mod codex;
pub mod focus;
mod git;
pub mod group;
pub mod handoff;
pub mod headless;
pub mod heartbeat;
pub mod integration;
pub mod launch;
pub mod model;
pub mod recovery;
pub mod result;
pub mod resume;
pub mod source;
pub mod stop;
pub mod store;
pub mod submit;
pub mod verify;
pub mod wait;
pub mod worktree;
mod worktree_use;
use anyhow::{Context, Result};
use model::{
    Attempt, AttemptState, Delivery, Journal, Ownership, Prompt, Session, Target, Task,
    TaskOutcome, WaitOutcome,
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use store::Store;

#[derive(Clone)]
pub struct Adopt {
    pub id: String,
    pub title: String,
    pub runtime: PathBuf,
    pub instance: String,
    pub workspace: String,
    pub pane: u32,
    pub agent: Option<String>,
}
pub(crate) fn now_ms() -> Result<u64> {
    Ok(u64::try_from(
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
    )?)
}
pub fn state_root(root: Option<PathBuf>) -> Result<PathBuf> {
    root.map(Ok).unwrap_or_else(store::directory)
}

pub fn adopt(root: &Path, mut request: Adopt) -> Result<Value> {
    anyhow::ensure!(
        model::id(&request.id),
        "task ID must be 1..64 ASCII letters, digits, hyphens or underscores"
    );
    anyhow::ensure!(
        model::workspace(&request.workspace),
        "invalid fux workspace name"
    );
    anyhow::ensure!(
        !request.title.is_empty()
            && request.title.len() <= 512
            && !request.title.chars().any(char::is_control),
        "invalid task title"
    );
    if let Some(agent) = &request.agent {
        crate::osc::AgentId::new(agent)?;
    }
    let requested_runtime = std::path::absolute(&request.runtime)?;
    let mut store = Store::open(root)?;
    if let Some(task) = store.journal().tasks.get(&request.id) {
        let session = session_for(store.journal(), task)?;
        anyhow::ensure!(
            session.ownership == Ownership::Adopted
                && task.title == request.title
                && session.agent == request.agent
                && task.requested_runtime == requested_runtime
                && session.target.instance == request.instance
                && session.target.workspace == request.workspace
                && session.target.pane == request.pane,
            "task ID already exists with a different adoption request"
        );
        return inspect_journal(store.journal(), &request.id);
    }
    anyhow::ensure!(
        !store.journal().launches.contains_key(&request.id),
        "task ID belongs to a managed launch"
    );
    request.runtime = std::fs::canonicalize(&requested_runtime).context("resolve fux runtime")?;
    let listing = crate::fux::completed_until(
        &request.runtime.join(format!("{}.sock", request.workspace)),
        json!({"command":"list","id":1,"instance":request.instance}),
        Instant::now() + Duration::from_secs(2),
    )?;
    let listing = listing
        .pointer("/result/value")
        .ok_or_else(|| anyhow::anyhow!("invalid fux listing"))?;
    anyhow::ensure!(
        listing.get("instance").and_then(Value::as_str) == Some(&request.instance),
        "fux instance changed during adoption"
    );
    let workspace = listing
        .get("workspaces")
        .and_then(Value::as_array)
        .and_then(|spaces| {
            spaces
                .iter()
                .find(|space| space.get("name").and_then(Value::as_str) == Some(&request.workspace))
        })
        .ok_or_else(|| anyhow::anyhow!("workspace missing"))?;
    let stream = workspace
        .pointer("/event_cursor/stream")
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow::anyhow!("workspace stream missing"))?;
    let pane = workspace
        .get("tabs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|tab| tab.get("panes").and_then(Value::as_array))
        .flatten()
        .find(|pane| pane.get("id").and_then(Value::as_u64) == Some(u64::from(request.pane)))
        .ok_or_else(|| anyhow::anyhow!("pane missing"))?;
    let pid = pane
        .get("pid")
        .and_then(Value::as_u64)
        .and_then(|pid| u32::try_from(pid).ok())
        .filter(|pid| *pid > 0)
        .ok_or_else(|| anyhow::anyhow!("pane has no running process"))?;
    let target = Target {
        runtime: request.runtime,
        instance: request.instance,
        workspace: request.workspace,
        stream,
        pane: request.pane,
        pid: Some(pid),
    };
    let created_ms = now_ms()?;
    let session_id = store
        .journal()
        .sessions
        .values()
        .find(|session| {
            session.ownership == Ownership::Adopted
                && session.launch.is_none()
                && session.target == target
                && session.agent == request.agent
        })
        .map(|session| Ok(session.id.clone()))
        .unwrap_or_else(store::nonce)?;
    let attempt_id = store::nonce()?;
    store.transaction(|journal| {
        journal
            .sessions
            .entry(session_id.clone())
            .or_insert(Session {
                id: session_id.clone(),
                target,
                agent: request.agent,
                ownership: Ownership::Adopted,
                launch: None,
                created_ms,
            });
        journal.attempts.insert(
            attempt_id.clone(),
            Attempt {
                id: attempt_id.clone(),
                task: request.id.clone(),
                session: session_id,
                state: AttemptState::Active,
            },
        );
        journal.tasks.insert(
            request.id.clone(),
            Task {
                required_checks: Default::default(),
                required_artifacts: Default::default(),
                id: request.id.clone(),
                requested_runtime,
                title: request.title,
                created_ms,
                outcome: TaskOutcome::Open,
                attempt: attempt_id,
            },
        );
        Ok(())
    })?;
    inspect_journal(store.journal(), &request.id)
}
fn session_for<'a>(journal: &'a Journal, task: &Task) -> Result<&'a Session> {
    let attempt = journal
        .attempts
        .get(&task.attempt)
        .ok_or_else(|| anyhow::anyhow!("attempt missing"))?;
    journal
        .sessions
        .get(&attempt.session)
        .ok_or_else(|| anyhow::anyhow!("session missing"))
}
fn inspect_journal(journal: &Journal, id: &str) -> Result<Value> {
    let Some(task) = journal.tasks.get(id) else {
        let launch = journal
            .launches
            .get(id)
            .ok_or_else(|| anyhow::anyhow!("task not found"))?;
        return Ok(
            json!({"generation":journal.generation,"launch":launch,"task":null,"attempt":null,"session":null,"prompts":[],"checks":[]}),
        );
    };
    let attempt = journal
        .attempts
        .get(&task.attempt)
        .ok_or_else(|| anyhow::anyhow!("attempt missing"))?;
    let prompts: Vec<_> = journal
        .prompts
        .values()
        .filter(|prompt| prompt.attempt == attempt.id)
        .collect();
    let checks: Vec<_> = journal
        .checks
        .values()
        .filter(|check| check.attempt == attempt.id)
        .map(|check| {
            json!({"id":check.id,"source":check.source,"artifact_requests":check.artifact_requests,"artifact_problems":check.artifact_problems,"phase":check.phase,"exit_code":check.exit_code,
            "signal":check.signal,"truncated":check.truncated,"requirement":check.requirement,"created_generation":check.created_generation})
        })
        .collect();
    let policy = json!({"sealed":journal.checks.values().any(|check| check.task == task.id),
        "required_count":task.required_checks.len(),
        "passed_count":task.required_checks.keys().filter(|name| journal.checks.values()
            .filter(|check| check.task == task.id && check.attempt == task.attempt && check.requirement.as_ref() == Some(*name))
            .max_by_key(|check| check.created_generation).is_some_and(|check|
            check.phase == model::CheckPhase::Finished && check.exit_code == Some(0))).count()});
    let required_artifacts = artifact::requirements(journal, id);
    let artifact_policy = json!({"sealed":artifact::policy_sealed(journal, id),
        "required_count":required_artifacts.len(),
        "collected_count":required_artifacts.iter().filter(|entry| entry["status"] == "collected").count()});
    let artifacts: Vec<_> = journal
        .artifacts
        .values()
        .filter(|artifact| artifact.task == task.id)
        .map(|artifact| {
            json!({"id":artifact.id,"check":artifact.check,"source":artifact.source,"attempt":artifact.attempt,"path":artifact.path,
            "created_ms":artifact.created_ms,"created_generation":artifact.created_generation,"requirement":artifact.requirement,"size":artifact.bytes.len()})
        })
        .collect();
    Ok(
        json!({"generation":journal.generation,"launch":journal.launches.get(id),"task":task,"attempt":attempt,"session":session_for(journal, task)?,"prompts":prompts,"checks":checks,"check_policy":policy,"artifacts":artifacts,"verification":journal.verifications.get(id),"artifact_policy":artifact_policy,"artifact_captures":check_artifacts::summaries(journal, id)}),
    )
}
pub fn inspect(root: &Path, id: &str) -> Result<Value> {
    inspect_journal(Store::open(root)?.journal(), id)
}
pub fn list(root: &Path) -> Result<Value> {
    let store = Store::open(root)?;
    Ok(
        json!({"generation":store.journal().generation,"tasks":store.journal().tasks.values().collect::<Vec<_>>(),"launches":store.journal().launches.values().collect::<Vec<_>>()}),
    )
}

/// Preparation persists intent and excludes another pending prompt on the same target.
/// A later submitter must durably record the fux reservation before writing any input.
pub fn prepare(root: &Path, task_id: &str, id: &str, text: &str, timeout_ms: u64) -> Result<Value> {
    prepare_store(&mut Store::open(root)?, task_id, id, text, timeout_ms, None)
}

fn prepare_store(
    store: &mut Store,
    task_id: &str,
    id: &str,
    text: &str,
    timeout_ms: u64,
    handoff: Option<model::Handoff>,
) -> Result<Value> {
    anyhow::ensure!(model::id(id), "invalid prompt operation ID");
    anyhow::ensure!(
        !text.is_empty() && text.len() <= 65536 && (1..=86_400_000).contains(&timeout_ms),
        "invalid prompt text/timeout"
    );
    submit::keys(text)?;
    let journal = store.journal();
    let task = journal
        .tasks
        .get(task_id)
        .ok_or_else(|| anyhow::anyhow!("task not found"))?;
    if let Some(existing) = journal.prompts.get(id) {
        anyhow::ensure!(
            existing.attempt == task.attempt
                && existing.text == text
                && existing.handoff == handoff
                && existing.deadline_ms - existing.created_ms == timeout_ms,
            "prompt ID already exists with different intent"
        );
        return Ok(serde_json::to_value(existing)?);
    }
    anyhow::ensure!(task.outcome == TaskOutcome::Open, "task is no longer open");
    let attempt = journal
        .attempts
        .get(&task.attempt)
        .ok_or_else(|| anyhow::anyhow!("attempt missing"))?;
    anyhow::ensure!(
        matches!(
            attempt.state,
            AttemptState::Active | AttemptState::NeedsInput
        ),
        "attempt cannot accept a new prompt"
    );
    let target = &session_for(journal, task)?.target;
    for prompt in journal.prompts.values().filter(|prompt| active(prompt)) {
        let other = journal
            .attempts
            .get(&prompt.attempt)
            .and_then(|attempt| journal.sessions.get(&attempt.session))
            .ok_or_else(|| anyhow::anyhow!("prompt session missing"))?;
        anyhow::ensure!(
            other.target.identity() != target.identity(),
            "another prompt is pending or delivery is uncertain for this pane"
        );
    }
    let created_ms = now_ms()?;
    let prompt = Prompt {
        id: id.into(),
        attempt: task.attempt.clone(),
        text: text.into(),
        handoff,
        created_ms,
        deadline_ms: created_ms
            .checked_add(timeout_ms)
            .ok_or_else(|| anyhow::anyhow!("deadline overflow"))?,
        delivery: Delivery::Prepared,
        receipt: None,
        wait: WaitOutcome::Pending,
        released: false,
        report_token: Some(store::nonce()?),
        response: None,
        report_binding: None,
        arm: None,
        wait_problem: None,
        wait_exit_status: None,
    };
    store.transaction(|journal| {
        journal.prompts.insert(id.into(), prompt.clone());
        Ok(())
    })?;
    Ok(serde_json::to_value(prompt)?)
}
pub(super) fn active(prompt: &Prompt) -> bool {
    !prompt.released
        && (matches!(
            prompt.wait,
            WaitOutcome::Pending | WaitOutcome::Uncertain | WaitOutcome::TimedOut
        ) || matches!(
            prompt.delivery,
            Delivery::Reserved | Delivery::Submitting | Delivery::Queued | Delivery::Uncertain
        ))
}
pub fn discard_prepared(root: &Path, id: &str) -> Result<Value> {
    let mut store = Store::open(root)?;
    let prompt = store
        .journal()
        .prompts
        .get(id)
        .ok_or_else(|| anyhow::anyhow!("prompt not found"))?;
    anyhow::ensure!(
        prompt.delivery == Delivery::Prepared && prompt.receipt.is_none(),
        "only unsent prepared prompts can be discarded"
    );
    if prompt.wait != WaitOutcome::Cancelled {
        store.transaction(|journal| {
            let prompt = journal
                .prompts
                .get_mut(id)
                .ok_or_else(|| anyhow::anyhow!("prompt missing"))?;
            prompt.wait = WaitOutcome::Cancelled;
            Ok(())
        })?;
    }
    Ok(serde_json::to_value(store.journal().prompts.get(id))?)
}
/// Stop coordinating this prompt. This does not retract any terminal bytes.
pub fn abandon(root: &Path, id: &str) -> Result<Value> {
    let mut store = Store::open(root)?;
    let prompt = store
        .journal()
        .prompts
        .get(id)
        .context("prompt not found")?;
    if !prompt.released {
        store.transaction(|journal| {
            let prompt = journal.prompts.get_mut(id).context("prompt missing")?;
            prompt.released = true;
            prompt.wait = WaitOutcome::Cancelled;
            Ok(())
        })?;
    }
    integration::disarm(&mut store, id)?;
    Ok(serde_json::to_value(store.journal().prompts.get(id))?)
}

/// Cancel task coordination, preserving all delivery evidence and adopted resources.
pub fn cancel(root: &Path, id: &str) -> Result<Value> {
    let mut store = Store::open(root)?;
    let task = store.journal().tasks.get(id).context("task not found")?;
    if task.outcome != TaskOutcome::Cancelled {
        anyhow::ensure!(
            task.outcome == TaskOutcome::Open,
            "task outcome is already final"
        );
        store.transaction(|journal| cancel_journal(journal, id))?;
    }
    inspect_journal(store.journal(), id)
}

pub(super) fn cancel_journal(journal: &mut Journal, id: &str) -> Result<()> {
    let task = journal.tasks.get_mut(id).context("task missing")?;
    anyhow::ensure!(
        matches!(task.outcome, TaskOutcome::Open | TaskOutcome::Cancelled),
        "task outcome is already final"
    );
    task.outcome = TaskOutcome::Cancelled;
    for prompt in journal.prompts.values_mut() {
        let attempt = journal
            .attempts
            .get(&prompt.attempt)
            .context("attempt missing")?;
        if attempt.task == id {
            prompt.released = true;
            prompt.wait = WaitOutcome::Cancelled;
        }
    }
    Ok(())
}

pub fn forget(root: &Path, id: &str) -> Result<Value> {
    let mut store = Store::open(root)?;
    anyhow::ensure!(
        !store.journal().launches.contains_key(id),
        "managed launch history cannot be forgotten"
    );
    if !store.journal().tasks.contains_key(id) {
        return Ok(json!({"forgotten":false}));
    }
    store.transaction(|journal| {
        let attempts: Vec<_> = journal
            .attempts
            .values()
            .filter(|attempt| attempt.task == id)
            .map(|attempt| attempt.id.clone())
            .collect();
        anyhow::ensure!(
            journal
                .prompts
                .values()
                .filter(|prompt| attempts.contains(&prompt.attempt))
                .all(|prompt| prompt.delivery == Delivery::Prepared
                    && prompt.wait == WaitOutcome::Cancelled
                    && prompt.receipt.is_none()),
            "task has a pending prompt or delivery history; cannot forget"
        );
        journal
            .prompts
            .retain(|_, prompt| !attempts.contains(&prompt.attempt));
        journal.attempts.retain(|_, attempt| attempt.task != id);
        journal.tasks.remove(id);
        journal.sessions.retain(|_, session| {
            journal
                .attempts
                .values()
                .any(|attempt| attempt.session == session.id)
        });
        Ok(())
    })?;
    Ok(json!({"forgotten":true}))
}
