//! Zor-owned attention view. Fux provides only terminal and navigation primitives.
mod attention;
mod terminal;
use crate::tasks::{model::*, store::Store};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

const MAX_OVERVIEW_ROWS: usize = MAX_TASKS * 2 + crate::tasks::group::MAX_GROUPS;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Row {
    pub key: String,
    pub kind: String,
    pub label: String,
    pub status: String,
    pub task_outcome: Option<TaskOutcome>,
    pub attention: bool,
    pub detail: String,
    pub age_upper_bound_ms: Option<u64>,
    pub target: Option<Target>,
    #[serde(default)]
    pub evidence: Option<Value>,
}

#[derive(Serialize, Deserialize)]
struct Integration {
    target: Target,
    producer: Option<String>,
    heartbeat: Value,
}

#[derive(Serialize, Deserialize)]
struct NativeIntegration {
    target: Target,
    operation: String,
    producer: String,
    phase: String,
    fresh: bool,
    age_ms: Option<u64>,
}
#[derive(Clone, Debug, Serialize)]
pub struct View {
    pub service_instance: String,
    pub observation_sequence: u64,
    pub task_generation: Option<u64>,
    pub state_directory: PathBuf,
    pub stale: bool,
    pub rows: Vec<Row>,
    pub problems: Vec<String>,
}

pub(crate) fn overview(root: &Path, runtime: &Path) -> Result<Value> {
    if !root.join("journal.json").try_exists()? {
        return Ok(
            json!({"runtime":runtime,"state_directory":root,"generation":null,"rows":[],"integrations":[]}),
        );
    }
    let store = Store::open(root)?;
    let journal = store.journal();
    let mut rows = Vec::new();
    for task in journal.tasks.values() {
        let attempt = journal
            .attempts
            .get(&task.attempt)
            .context("task attempt missing")?;
        let session = journal
            .sessions
            .get(&attempt.session)
            .context("task session missing")?;
        let launch = journal.launches.get(&task.id);
        let worktree_problem = launch
            .and_then(|launch| launch.worktree.as_ref())
            .and_then(|id| journal.worktrees.get(id))
            .and_then(|tree| tree.problem.clone());
        let unresolved = matches!(attempt.state, AttemptState::Uncertain | AttemptState::Lost)
            || launch.is_some_and(|launch| launch.problem.is_some())
            || worktree_problem.is_some();
        let input = journal.prompts.values().any(|prompt| {
            prompt.attempt == task.attempt
                && !prompt.released
                && prompt.wait == WaitOutcome::NeedsInput
        });
        let prompt_problem = journal.prompts.values().any(|prompt| {
            prompt.attempt == task.attempt
                && !prompt.released
                && matches!(prompt.wait, WaitOutcome::Uncertain | WaitOutcome::TimedOut)
        });
        let check_failed = task.required_checks.keys().any(|name| {
            journal
                .checks
                .values()
                .filter(|check| check.task == task.id && check.requirement.as_ref() == Some(name))
                .max_by_key(|check| check.created_generation)
                .is_some_and(|check| {
                    check.phase == CheckPhase::Uncertain
                        || (check.phase == CheckPhase::Finished
                            && (check.exit_code != Some(0) || !check.artifact_problems.is_empty()))
                })
        });
        let status = match task.outcome {
            TaskOutcome::Verified => "verified",
            TaskOutcome::Cancelled => "cancelled",
            TaskOutcome::Failed => "failed",
            TaskOutcome::Open if unresolved || prompt_problem => "unresolved",
            TaskOutcome::Open if input => "needs-input",
            TaskOutcome::Open if check_failed => "checks-failed",
            TaskOutcome::Open => "open",
        };
        let verification = journal.verifications.get(&task.id);
        let capture_failed = task.required_artifacts.keys().any(|name| {
            journal
                .checks
                .values()
                .filter(|check| check.task == task.id && check.artifact_requests.contains_key(name))
                .max_by_key(|check| check.created_generation)
                .is_some_and(|check| check.artifact_problems.contains_key(name))
        });
        let status = if task.outcome == TaskOutcome::Open && capture_failed && status == "open" {
            "artifact-failed"
        } else {
            status
        };
        rows.push(Row {
            evidence: None,
            key: format!("task:{}", task.id),
            kind: "task".into(),
            label: format!("{}: {}", task.id, task.title),
            status: status.into(),
            task_outcome: Some(task.outcome.clone()),
            attention: unresolved
                || prompt_problem
                || input
                || (task.outcome == TaskOutcome::Open && (check_failed || capture_failed))
                || task.outcome == TaskOutcome::Failed,
            detail: verification
                .map(|record| {
                    format!(
                        "Verified source {}; {} checks, {} artifacts",
                        record.source,
                        record.checks.len(),
                        record.artifacts.len()
                    )
                })
                .unwrap_or_else(|| {
                    format!(
                        "Recorded task outcome {:?}; coordination {status}; attempt {:?}; agent {}",
                        task.outcome,
                        attempt.state,
                        session.agent.as_deref().unwrap_or("unclassified")
                    )
                })
                + &launch
                    .and_then(|launch| launch.problem.clone())
                    .or(worktree_problem)
                    .map(|problem| format!("; {problem}"))
                    .unwrap_or_default(),
            age_upper_bound_ms: None,
            target: if launch.is_some_and(|launch| launch.phase == LaunchPhase::Closed) {
                None
            } else {
                Some(session.target.clone())
            },
        });
    }
    for launch in journal
        .launches
        .values()
        .filter(|launch| !journal.tasks.contains_key(launch.task_id()))
    {
        rows.push(Row {
            evidence: None,
            key: format!("launch:{}", launch.id),
            age_upper_bound_ms: None,
            kind: "launch".into(),
            label: launch.title.clone(),
            status: format!("{:?}", launch.phase).to_lowercase(),
            task_outcome: None,
            attention: true,
            detail: launch
                .problem
                .clone()
                .unwrap_or_else(|| "Launch has not attached; inspect launch intent".into()),
            target: None,
        });
    }
    for group in journal.groups.values() {
        let summary = crate::tasks::group::view(journal, &group.id)?;
        let state = summary
            .get("state")
            .and_then(Value::as_str)
            .context("group state missing")?;
        let active_count = summary
            .get("active_count")
            .and_then(Value::as_u64)
            .context("group active count missing")?;
        let scheduling = if group.automatic {
            "automatic"
        } else if group.run_generation > 0 {
            "paused"
        } else {
            "manual"
        };
        let paused = state == "active" && scheduling == "paused";
        let retirement_pending = summary
            .get("retirement_pending_count")
            .and_then(Value::as_u64)
            .context("group retirement count missing")?;
        let retirement_ids = summary
            .get("retirement_pending")
            .context("group retirement IDs missing")?;
        rows.push(Row {
            key: format!("group:{}", group.id),
            kind: "group".into(),
            label: group.id.clone(),
            status: if paused { "paused" } else { state }.into(),
            task_outcome: None,
            attention: paused || state == "needs-attention" || retirement_pending > 0,
            detail: format!(
                "Scheduling {scheduling}; group {state}; {} admitted-unverified of {}; concurrency {}{}{}",
                active_count,
                group.members.len(),
                group.concurrency,
                group.run_problem.as_ref().map(|problem| format!("; recorded scheduling problem: {problem}")).unwrap_or_default(),
                if retirement_pending > 0 {
                    format!("; {retirement_pending} unsent arms await retirement; {}",
                        if group.cancelled { "retry group-cancel" } else { "inspect retained operations" })
                } else { String::new() },
            ),
            age_upper_bound_ms: None,
            target: None,
            evidence: Some(json!({"source":"group-journal","generation":journal.generation,
                "group_state":state,"scheduling":scheduling,"automatic":group.automatic,
                "run_generation":group.run_generation,"run_problem":group.run_problem,
                "active_count":active_count,"member_count":group.members.len(),
                "retirement_pending_count":retirement_pending,"retirement_pending":retirement_ids,
                "concurrency":group.concurrency,"scope":"retained-coordination; not-service-health"})),
        });
    }
    let integrations: Vec<_> = journal
        .launches
        .values()
        .filter(|launch| launch.integration.is_some())
        .filter_map(|launch| {
            let target = launch
                .session
                .as_ref()
                .and_then(|id| journal.sessions.get(id))?
                .target
                .clone();
            Some(Integration {
                target,
                producer: launch
                    .integration
                    .as_ref()
                    .and_then(|value| value.producer.clone()),
                heartbeat: crate::tasks::heartbeat::view(root, launch, journal),
            })
        })
        .collect();
    let now = crate::tasks::now_ms()?;
    let mut native_integrations = Vec::new();
    for worker in journal.native_workers.values() {
        let Some(task) = journal.tasks.get(&worker.task) else {
            continue;
        };
        if task.attempt != worker.attempt {
            continue;
        }
        let Some(entry) = journal
            .native_turns
            .values()
            .filter(|entry| entry.task == worker.task && entry.attempt == worker.attempt)
            .max_by_key(|entry| {
                (
                    !entry.evidence.phase.terminal(),
                    entry.created_ms,
                    &entry.evidence.operation,
                )
            })
        else {
            continue;
        };
        let view =
            crate::tasks::codex::state::inspect_retained(journal, &entry.evidence.operation, now)?;
        let attempt = journal
            .attempts
            .get(&worker.attempt)
            .context("native attempt")?;
        let session = journal
            .sessions
            .get(&attempt.session)
            .context("native session")?;
        native_integrations.push(NativeIntegration {
            target: session.target.clone(),
            operation: entry.evidence.operation.clone(),
            producer: worker.producer.clone(),
            phase: view
                .pointer("/operation/evidence/phase")
                .and_then(Value::as_str)
                .context("native phase")?
                .into(),
            fresh: view
                .pointer("/operation/evidence/fresh")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            age_ms: view.get("evidence_age_ms").and_then(Value::as_u64),
        });
    }
    Ok(
        json!({"runtime":runtime,"state_directory":root,"generation":journal.generation,"rows":rows,"integrations":integrations,"native_integrations":native_integrations}),
    )
}

fn integrated_state(
    integration: &Integration,
    input_sequence: Option<u64>,
    elapsed: u64,
) -> (String, Option<u64>, Value) {
    let heartbeat = &integration.heartbeat;
    let age = heartbeat
        .get("age_ms")
        .and_then(Value::as_u64)
        .map(|age| age.saturating_add(elapsed));
    let fresh = heartbeat.get("status").and_then(Value::as_str) == Some("current")
        && age.is_some_and(|age| age < 6000);
    let claim = heartbeat
        .get("observation")
        .filter(|value| !value.is_null())
        .and_then(|value| {
            serde_json::from_value::<crate::tasks::heartbeat::Observation>(value.clone()).ok()
        });
    let correlated = input_sequence.is_some()
        && heartbeat.get("input_sequence").and_then(Value::as_u64) == input_sequence;
    let state = if fresh && correlated {
        claim
            .as_ref()
            .map(|claim| claim.state.as_str())
            .unwrap_or("unknown")
    } else {
        "unknown"
    };
    (
        state.into(),
        age,
        json!({"source":"integration","producer":integration.producer,
        "heartbeat_sequence":heartbeat.get("sequence"),"heartbeat_status":heartbeat.get("status"),
        "heartbeat_age_upper_bound_ms":age,"correlated":correlated,"fresh":fresh,
        "claim":claim,"scope":"agent-observation; not-task-completion"}),
    )
}

pub fn snapshot(directory: Option<PathBuf>) -> Result<View> {
    let began = std::time::Instant::now();
    let status = crate::service::status(directory.clone())?;
    let instance = status
        .get("service_instance")
        .and_then(Value::as_str)
        .context("service identity missing")?;
    let tasks = crate::service::overview(directory, instance)?;
    let elapsed = u64::try_from(began.elapsed().as_millis()).unwrap_or(u64::MAX);
    compose(&status, &tasks, elapsed)
}

fn compose(status: &Value, tasks: &Value, elapsed: u64) -> Result<View> {
    let instance = status
        .get("service_instance")
        .and_then(Value::as_str)
        .context("service identity missing")?;
    let value = tasks.get("value").context("task overview missing")?;
    let runtime: PathBuf =
        serde_json::from_value(value.get("runtime").context("runtime missing")?.clone())?;
    let mut rows: Vec<Row> =
        serde_json::from_value(value.get("rows").context("task rows missing")?.clone())?;
    anyhow::ensure!(
        rows.len() <= MAX_OVERVIEW_ROWS,
        "task overview exceeds row limit"
    );
    let integrations: Vec<Integration> = serde_json::from_value(
        value
            .get("integrations")
            .cloned()
            .unwrap_or_else(|| json!([])),
    )?;
    anyhow::ensure!(
        integrations.len() <= MAX_TASKS,
        "integration overview limit"
    );
    let native_integrations: Vec<NativeIntegration> = serde_json::from_value(
        value
            .get("native_integrations")
            .cloned()
            .unwrap_or_else(|| json!([])),
    )?;
    anyhow::ensure!(
        native_integrations.len() <= MAX_TASKS,
        "native integration overview limit"
    );
    let stale = status.get("stale").and_then(Value::as_bool).unwrap_or(true) || elapsed > 5000;
    let observations = status
        .pointer("/snapshot/observations")
        .and_then(Value::as_array)
        .context("observations missing")?;
    anyhow::ensure!(
        observations.len() <= crate::watch::MAX_OBSERVED_PANES,
        "observation row limit exceeded"
    );
    for observation in observations {
        let handle = observation
            .get("handle")
            .context("observation handle missing")?;
        let target: Target = serde_json::from_value(json!({"runtime":runtime,
            "instance":handle["instance"],"workspace":handle["workspace"],"stream":handle["stream"],
            "pane":handle["pane"],"pid":handle["pid"]}))?;
        let agent = observation
            .get("agent")
            .and_then(Value::as_str)
            .unwrap_or("unclassified");
        let mut age = observation
            .get("age_upper_bound_ms")
            .and_then(Value::as_u64)
            .unwrap_or(u64::MAX)
            .saturating_add(elapsed);
        let row_stale = stale
            || age > 5000
            || observation
                .get("problem")
                .is_some_and(|problem| !problem.is_null());
        let passive_state = if row_stale {
            "unknown"
        } else {
            observation
                .get("state")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
        };
        let matching: Vec<_> = integrations
            .iter()
            .filter(|integration| integration.target.identity() == target.identity())
            .collect();
        let native_matching: Vec<_> = native_integrations
            .iter()
            .filter(|integration| integration.target.identity() == target.identity())
            .collect();
        let (mut state, evidence) = if let [native] = native_matching.as_slice() {
            let native_age = native.age_ms.map(|age| age.saturating_add(elapsed));
            age = age.max(native_age.unwrap_or(u64::MAX));
            let fresh = !row_stale
                && matching.is_empty()
                && native.fresh
                && native_age.is_some_and(|age| age < 5000);
            let state = if fresh {
                match native.phase.as_str() {
                    "working" => "working",
                    "input-required" => "blocked",
                    "completed" | "interrupted" => "idle",
                    _ => "unknown",
                }
            } else {
                "unknown"
            };
            (
                state.into(),
                json!({"source":"codex-native","operation":native.operation,
                "producer":native.producer,"native_phase":native.phase,"fresh":fresh,
                "native_age_upper_bound_ms":native_age,"availability":"not-probed",
                "scope":"correlated-native-evidence; not-task-completion"}),
            )
        } else if !native_matching.is_empty() {
            (
                "unknown".into(),
                json!({"source":"codex-native","fresh":false,"problem":"multiple native workers share this target"}),
            )
        } else if let [integration] = matching.as_slice() {
            let (state, heartbeat_age, mut evidence) = integrated_state(
                integration,
                observation.get("input_sequence").and_then(Value::as_u64),
                elapsed,
            );
            if let Some(heartbeat_age) = heartbeat_age {
                age = age.max(heartbeat_age);
            }
            evidence
                .as_object_mut()
                .context("integration evidence missing")?
                .insert("passive_state".into(), json!(passive_state));
            (state, evidence)
        } else if matching.is_empty() {
            (
                passive_state.into(),
                json!({"source":"passive","rule":observation.get("rule")}),
            )
        } else {
            (
                "unknown".into(),
                json!({"source":"integration","problem":"multiple managed integrations share this target"}),
            )
        };
        // Both sources must still have current pane identity/input evidence. A stale
        // passive snapshot cannot be revived by a fresh heartbeat read afterwards.
        let row_stale = row_stale || age > 5000;
        if row_stale {
            state = "unknown".into();
        }
        let source = evidence
            .get("source")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        rows.push(Row {
            key: format!(
                "pane:{}:{}:{}:{}",
                target.instance, target.workspace, target.stream, target.pane
            ),
            kind: "observation".into(),
            age_upper_bound_ms: Some(age),
            label: format!("{} / {}: {agent}", target.workspace, target.pane),
            status: state.clone(),
            task_outcome: None,
            attention: state == "blocked"
                || ((!matching.is_empty() || !native_matching.is_empty()) && state == "unknown")
                || observation.get("problem").is_some_and(|v| !v.is_null()),
            detail: format!(
                "{} state {}; age at fetch <= {}ms; passive rule {}; {}",
                source,
                state,
                age,
                observation
                    .get("rule")
                    .and_then(Value::as_str)
                    .unwrap_or("none"),
                observation
                    .get("problem")
                    .and_then(Value::as_str)
                    .unwrap_or("agent observation; not task completion")
            ),
            target: if row_stale || target.pid.is_none() {
                None
            } else {
                Some(target)
            },
            evidence: Some(evidence),
        });
    }
    rows.sort_by(|a, b| (!a.attention, &a.kind, &a.key).cmp(&(!b.attention, &b.kind, &b.key)));
    let problems = status
        .pointer("/snapshot/problems")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|map| map.iter())
        .map(|(key, value)| format!("{key}: {value}"))
        .collect();
    Ok(View {
        service_instance: instance.into(),
        observation_sequence: status.get("sequence").and_then(Value::as_u64).unwrap_or(0),
        task_generation: value["generation"].as_u64(),
        state_directory: serde_json::from_value(value["state_directory"].clone())?,
        stale,
        rows,
        problems,
    })
}

pub fn run(
    directory: Option<PathBuf>,
    bell: bool,
    notify: bool,
    notification_command: Option<PathBuf>,
) -> Result<u8> {
    terminal::run(directory, bell, notify, notification_command)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_attention_requires_fresh_evidence_and_current_pane_observation() -> Result<()> {
        let mut status = json!({"service_instance":"service","stale":false,"sequence":1,
            "snapshot":{"observations":[{
                "handle":{"instance":"server","workspace":"default","stream":1,"pane":1,"pid":1},
                "agent":"codex","age_upper_bound_ms":100,"state":"idle","problem":null}],"problems":{}}});
        let mut tasks = json!({"value":{"runtime":"/tmp/fixture","state_directory":"/tmp/state",
            "generation":1,"rows":[],"native_integrations":[{
                "target":{"runtime":"/tmp/fixture","instance":"server","workspace":"default","stream":1,"pane":1,"pid":1},
                "operation":"input-a","producer":"producer","phase":"input-required","fresh":true,"age_ms":100}]}});
        let view = compose(&status, &tasks, 0)?;
        let row = view.rows.first().context("native row")?;
        assert_eq!(row.status, "blocked");
        assert!(row.attention);
        assert!(row.task_outcome.is_none());
        for problem in ["event gap", "observer EOF", "resync required"] {
            *status
                .pointer_mut("/snapshot/observations/0/problem")
                .context("problem")? = json!(problem);
            let view = compose(&status, &tasks, 0)?;
            let row = view.rows.first().context("invalidated row")?;
            assert_eq!(row.status, "unknown");
            assert!(row.attention);
            assert!(row.target.is_none());
            assert_eq!(
                row.evidence.as_ref().and_then(|value| value.get("fresh")),
                Some(&json!(false))
            );
        }
        *status
            .pointer_mut("/snapshot/observations/0/problem")
            .context("problem")? = Value::Null;
        let expired = compose(&status, &tasks, 4900)?;
        assert_eq!(
            expired.rows.first().context("expired row")?.status,
            "unknown"
        );
        *tasks
            .pointer_mut("/value/native_integrations/0/fresh")
            .context("fresh")? = json!(false);
        assert_eq!(
            compose(&status, &tasks, 0)?
                .rows
                .first()
                .context("retired row")?
                .status,
            "unknown"
        );
        *tasks
            .pointer_mut("/value/native_integrations/0/fresh")
            .context("fresh")? = json!(true);
        *tasks
            .pointer_mut("/value/native_integrations/0/phase")
            .context("phase")? = json!("completed");
        let completed = compose(&status, &tasks, 0)?;
        let row = completed.rows.first().context("completed row")?;
        assert!(row.task_outcome.is_none());
        assert!(!row.attention);
        Ok(())
    }

    #[test]
    fn invalidated_observation_cannot_be_revived_by_fresh_heartbeat() -> Result<()> {
        for state in ["idle", "working"] {
            let mut status = json!({"service_instance":"service","stale":false,"sequence":1,
                "snapshot":{"observations":[{
                    "handle":{"instance":"server","workspace":"default","stream":1,"pane":1,"pid":1},
                    "agent":"opencode","age_upper_bound_ms":100,"input_sequence":4,
                    "state":"unknown","rule":null,"problem":null}],"problems":{}}});
            let tasks = json!({"value":{"runtime":"/tmp/fixture","state_directory":"/tmp/state",
                "generation":1,"rows":[],"integrations":[{
                    "target":{"runtime":"/tmp/fixture","instance":"server","workspace":"default",
                        "stream":1,"pane":1,"pid":1},"producer":"producer",
                    "heartbeat":{"status":"current","age_ms":100,"sequence":1,"input_sequence":4,
                        "observation":{"state":state,"operation":"prompt","input_operation":2,
                            "message":{"session":"native","id":"message"}}}}]}});
            let current = compose(&status, &tasks, 0)?;
            let row = current.rows.first().context("current observation row")?;
            assert_eq!(row.status, state);
            assert!(row.target.is_some());
            for problem in ["event gap", "observer EOF", "resync required"] {
                *status
                    .pointer_mut("/snapshot/observations/0/problem")
                    .context("observation problem")? = json!(problem);
                let invalidated = compose(&status, &tasks, 0)?;
                let row = invalidated
                    .rows
                    .first()
                    .context("invalidated observation row")?;
                assert_eq!(row.status, "unknown");
                assert!(row.target.is_none());
                assert!(row.attention);
            }
        }
        Ok(())
    }

    #[test]
    fn explicit_state_requires_current_heartbeat_and_current_input_epoch() -> Result<()> {
        let mut integration = Integration {
            target: Target {
                runtime: "/tmp/fixture".into(),
                instance: "server".into(),
                workspace: "default".into(),
                stream: 1,
                pane: 1,
                pid: Some(1),
            },
            producer: Some("producer".into()),
            heartbeat: json!({"status":"current","age_ms":100,"sequence":1,"input_sequence":4,
                "observation":{"state":"blocked","operation":"prompt","input_operation":2,
                    "message":{"session":"native","id":"message"}}}),
        };
        assert_eq!(integrated_state(&integration, Some(4), 0).0, "blocked");
        assert_eq!(integrated_state(&integration, Some(5), 0).0, "unknown");
        assert_eq!(integrated_state(&integration, None, 0).0, "unknown");
        assert_eq!(integrated_state(&integration, Some(4), 5900).0, "unknown");
        integration
            .heartbeat
            .as_object_mut()
            .context("object")?
            .insert("status".into(), json!("expired"));
        assert_eq!(integrated_state(&integration, Some(4), 0).0, "unknown");
        integration
            .heartbeat
            .as_object_mut()
            .context("object")?
            .insert("status".into(), json!("current"));
        integration
            .heartbeat
            .as_object_mut()
            .context("object")?
            .remove("observation");
        assert_eq!(integrated_state(&integration, Some(4), 0).0, "unknown");
        Ok(())
    }
}
