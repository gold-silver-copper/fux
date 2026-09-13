//! Durable managed creation intent. Ambiguous creation is reconciled, never blindly repeated.
use super::{model::*, store::Store};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

pub struct Start {
    pub id: String,
    pub title: String,
    pub agent: Option<String>,
    pub runtime: PathBuf,
    pub instance: String,
    pub workspace: String,
    pub cwd: Option<PathBuf>,
    pub worktree: Option<String>,
    pub argv: Vec<String>,
    pub integration: Option<IntegrationKind>,
}

pub(super) fn validate_argv(argv: &[String]) -> Result<()> {
    anyhow::ensure!(
        !argv.is_empty()
            && argv.len() <= 125
            && argv
                .iter()
                .all(|arg| arg.len() <= 4096 && !arg.contains('\0'))
            && argv.iter().map(String::len).sum::<usize>() + 64 <= 16384
            && argv
                .first()
                .is_some_and(|program| !program.is_empty() && !program.contains('=')),
        "launch argv exceeds fux limits or has an unsupported executable name"
    );
    Ok(())
}
pub(super) fn command(launch: &Launch) -> Vec<String> {
    let mut argv = vec!["/usr/bin/env".into()];
    if let Some(resume) = &launch.resume {
        argv.extend(["-u".into(), "OPENCODE_TEST_HOME".into()]);
        for (key, value) in &resume.environment {
            if value.is_none() {
                argv.extend(["-u".into(), key.clone()]);
            }
        }
    }
    argv.push("--".into());
    if let Some(resume) = &launch.resume {
        for (key, value) in &resume.environment {
            if let Some(value) = value {
                argv.push(format!("{key}={value}"));
            }
        }
    }
    argv.push(format!("ZOR_LAUNCH_ID={}", launch.marker));
    if let Some(integration) = &launch.integration {
        argv.extend([
            format!("ZOR_STATE_DIRECTORY={}", integration.root.display()),
            format!("ZOR_TASK_ID={}", launch.task_id()),
            format!("ZOR_ADAPTER_SOCKET={}", integration.socket.display()),
            format!("ZOR_BIN={}", integration.binary.display()),
            integration.binary.to_string_lossy().into_owned(),
            "opencode-exec".into(),
            "--plugin".into(),
            integration.plugin.to_string_lossy().into_owned(),
            "--".into(),
        ]);
    }
    argv.extend(launch.argv.clone());
    argv
}
pub(super) fn listing(launch: &Launch, deadline: Instant) -> Result<Value> {
    let response = crate::fux::completed_until(
        &launch.runtime.join(format!("{}.sock", launch.workspace)),
        json!({"command":"list","id":1,"instance":launch.instance}),
        deadline,
    )?;
    anyhow::ensure!(
        response.get("id").and_then(Value::as_u64) == Some(1),
        "fux reply ID mismatch"
    );
    let listing = response
        .pointer("/result/value")
        .context("missing fux listing")?;
    anyhow::ensure!(
        listing.get("instance").and_then(Value::as_str) == Some(&launch.instance),
        "fux incarnation changed"
    );
    let workspace = listing
        .get("workspaces")
        .and_then(Value::as_array)
        .and_then(|spaces| {
            spaces
                .iter()
                .find(|space| space.get("name").and_then(Value::as_str) == Some(&launch.workspace))
        })
        .context("launch workspace unavailable")?;
    if launch.stream != 0 {
        anyhow::ensure!(
            workspace
                .pointer("/event_cursor/stream")
                .and_then(Value::as_u64)
                == Some(launch.stream),
            "launch workspace lifetime changed"
        );
    }
    Ok(workspace.clone())
}

pub fn start(root: &Path, request: Start) -> Result<Value> {
    anyhow::ensure!(
        super::model::id(&request.id)
            && super::model::workspace(&request.workspace)
            && !request.title.is_empty()
            && request.title.len() <= 512
            && !request.title.chars().any(char::is_control),
        "invalid managed task identity/title/workspace"
    );
    validate_argv(&request.argv)?;
    if let Some(agent) = &request.agent {
        crate::osc::AgentId::new(agent)?;
    }
    let requested_runtime = std::path::absolute(&request.runtime)?;
    anyhow::ensure!(
        request.cwd.is_some() != request.worktree.is_some(),
        "specify exactly one of cwd or worktree"
    );
    let explicit_cwd = request.cwd.as_ref().map(std::path::absolute).transpose()?;
    let mut store = Store::open(root)?;
    if let Some(existing) = store.journal().launches.get(&request.id) {
        anyhow::ensure!(
            existing.title == request.title
                && existing.agent == request.agent
                && existing.requested_runtime == requested_runtime
                && existing.worktree == request.worktree
                && explicit_cwd
                    .as_ref()
                    .is_none_or(|cwd| *cwd == existing.requested_cwd)
                && existing.instance == request.instance
                && existing.workspace == request.workspace
                && existing.argv == request.argv,
            "task ID already has different launch intent"
        );
        anyhow::ensure!(
            existing.integration.as_ref().map(|i| &i.kind) == request.integration.as_ref(),
            "task ID already has different integration intent"
        );
        if matches!(existing.phase, LaunchPhase::Attached | LaunchPhase::Closed) {
            return super::inspect_journal(store.journal(), &request.id);
        }
    } else {
        anyhow::ensure!(
            !store.journal().tasks.contains_key(&request.id),
            "task ID already exists"
        );
        let requested_cwd = match &request.worktree {
            Some(id) => super::worktree::launch_path(root, &mut store, id)?,
            None => explicit_cwd.context("launch cwd missing")?,
        };
        let marker = super::store::nonce()?;
        let integration = request
            .integration
            .map(|kind| super::integration::prepare(root, &marker, kind))
            .transpose()?;
        let mut launch = Launch {
            id: request.id.clone(),
            task: None,
            resume: None,
            title: request.title,
            agent: request.agent,
            runtime: std::fs::canonicalize(&requested_runtime).context("resolve fux runtime")?,
            requested_runtime,
            cwd: std::fs::canonicalize(&requested_cwd).context("resolve launch cwd")?,
            requested_cwd,
            worktree: request.worktree,
            instance: request.instance,
            workspace: request.workspace,
            stream: 0,
            event_sequence: 0,
            argv: request.argv,
            marker,
            integration,
            final_evidence: None,
            stop_requested: false,
            created_ms: super::now_ms()?,
            phase: LaunchPhase::Prepared,
            pane: None,
            session: None,
            problem: None,
        };
        anyhow::ensure!(launch.cwd.is_dir(), "launch cwd must be a directory");
        anyhow::ensure!(
            !store
                .journal()
                .worktrees
                .values()
                .any(|tree| tree.remove_force.is_some() && launch.cwd.starts_with(&tree.path)),
            "launch cwd belongs to a worktree with removal intent"
        );
        let workspace = listing(&launch, Instant::now() + Duration::from_secs(2))?;
        launch.stream = workspace
            .pointer("/event_cursor/stream")
            .and_then(Value::as_u64)
            .context("workspace stream missing")?;
        launch.event_sequence = workspace
            .pointer("/event_cursor/sequence")
            .and_then(Value::as_u64)
            .context("workspace sequence missing")?;
        store.transaction(|journal| {
            journal.launches.insert(launch.id.clone(), launch);
            Ok(())
        })?;
    }
    submit_prepared(root, &mut store, &request.id)
}

/// The final-record retention a managed launch asks fux for. Nobody polls a launch's exit the
/// way `zor run` does: the record is read by `recover_final` when the service's 1 s recovery
/// loop or a `wait`/`follow` (up to 24 h) finds the pane gone, or after a stopped service is
/// restarted. That horizon is open-ended on zor's side, so a launch asks for everything fux
/// grants, and fux's ceiling (four hours) is the documented bound on how long a supervisor
/// may be away before the exit evidence is gone.
pub(super) const LAUNCH_FINAL_RETAIN_MS: u64 = crate::fux::MAX_FINAL_RETENTION_MS;

pub(super) fn submit_prepared(root: &Path, store: &mut Store, id: &str) -> Result<Value> {
    let launch = store
        .journal()
        .launches
        .get(id)
        .context("launch missing")?
        .clone();
    if launch.phase == LaunchPhase::Prepared {
        super::resume::authorize_submission(store.journal(), &launch)?;
        // No creation request can have been sent while the durable phase was prepared.
        listing(&launch, Instant::now() + Duration::from_secs(2))?;
        if let Some(id) = &launch.worktree {
            anyhow::ensure!(
                super::worktree::launch_path(root, store, id)? == launch.cwd,
                "launch worktree path changed"
            );
        }
        store.transaction(|journal| {
            journal
                .launches
                .get_mut(id)
                .context("launch missing")?
                .begin_submission()
        })?;
        let response = crate::fux::completed_until(
            &launch.runtime.join(format!("{}.sock", launch.workspace)),
            json!({"command":"split","axis":"horizontal","id":1,"instance":launch.instance,"stream":launch.stream,"cwd":launch.cwd,"argv":command(&launch),"fixed_workspace":true,"final_retain_ms":LAUNCH_FINAL_RETAIN_MS}),
            Instant::now() + Duration::from_secs(6),
        );
        match response {
            Ok(response) => {
                if let Some(pane) = response
                    .pointer("/result/value/pane")
                    .and_then(Value::as_u64)
                    .and_then(|p| u32::try_from(p).ok())
                    .filter(|p| *p > 0)
                {
                    store.transaction(|journal| {
                        journal
                            .launches
                            .get_mut(id)
                            .context("launch missing")?
                            .record_created_pane(pane)
                    })?;
                }
            }
            Err(error) => {
                uncertain(store, id, &error)?;
                return Err(error.context(
                    "creation outcome uncertain; reconcile this launch ID before any new launch",
                ));
            }
        }
    }
    reconcile_store(store, id)
}

fn uncertain(store: &mut Store, id: &str, error: &anyhow::Error) -> Result<()> {
    store.transaction(|journal| {
        let launch = journal.launches.get_mut(id).context("launch missing")?;
        launch.creation_uncertain(&error.to_string());
        Ok(())
    })
}

pub fn reconcile(root: &Path, id: &str) -> Result<Value> {
    reconcile_store(&mut Store::open(root)?, id)
}
pub(super) fn reconcile_store(store: &mut Store, id: &str) -> Result<Value> {
    let result = reconcile_locked(store, id);
    store.recovery_observed(id, result.is_ok());
    result
}
fn reconcile_locked(store: &mut Store, id: &str) -> Result<Value> {
    let launch = store
        .journal()
        .launches
        .get(id)
        .context("launch not found")?
        .clone();
    anyhow::ensure!(
        launch.task.is_none()
            || !matches!(launch.phase, LaunchPhase::Attached | LaunchPhase::Closed),
        "historical launch is retained evidence; reconcile the current task launch"
    );
    if matches!(launch.phase, LaunchPhase::Closed | LaunchPhase::Prepared) {
        return super::inspect_journal(store.journal(), id);
    }
    if launch.phase == LaunchPhase::Attached {
        return reconcile_attached(store, &launch);
    }
    let result = (|| -> Result<()> {
        let workspace = listing(&launch, Instant::now() + Duration::from_secs(2))?;
        let expected = serde_json::to_value(command(&launch))?;
        let panes: Vec<_> = workspace
            .get("tabs")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|tab| tab.get("panes").and_then(Value::as_array))
            .flatten()
            .filter(|pane| pane.get("command") == Some(&expected))
            .collect();
        anyhow::ensure!(
            panes.len() == 1,
            "launch marker has no unique live pane; creation may be pending, closed or lost"
        );
        let pane = panes.first().context("launch pane missing")?;
        let pane_id = pane
            .get("id")
            .and_then(Value::as_u64)
            .and_then(|p| u32::try_from(p).ok())
            .filter(|p| *p > 0)
            .context("invalid pane ID")?;
        anyhow::ensure!(
            launch.pane.is_none_or(|expected| expected == pane_id)
                && pane.get("cwd") == Some(&serde_json::to_value(&launch.cwd)?),
            "launch pane identity/cwd mismatch"
        );
        let pid = pane
            .get("pid")
            .and_then(Value::as_u64)
            .and_then(|p| u32::try_from(p).ok())
            .filter(|p| *p > 0)
            .context("launch process is no longer live")?;
        attach(store, &launch, pane_id, Some(pid), None)
    })();
    if let Err(live_error) = result {
        if store.journal().launches.get(id).is_some_and(|current| {
            matches!(current.phase, LaunchPhase::Attached | LaunchPhase::Closed)
        }) {
            // A committed attachment may report a directory sync error. Preserve it.
            return Err(live_error);
        }
        let recovery = recover_final(&launch)
            .and_then(|(pane, evidence)| attach(store, &launch, pane, None, Some(evidence)));
        if recovery.is_ok() {
            return super::inspect_journal(store.journal(), launch.task_id());
        }
        let error = recovery
            .err()
            .context("missing recovery error")?
            .context(live_error);
        uncertain(store, id, &error)?;
        return Err(error.context("launch reconciliation incomplete; no replacement was created"));
    }
    super::inspect_journal(store.journal(), launch.task_id())
}

fn reconcile_attached(store: &mut Store, launch: &Launch) -> Result<Value> {
    let id = launch.id.as_str();
    let task = store
        .journal()
        .tasks
        .get(launch.task_id())
        .context("managed task missing")?;
    let attempt_id = store
        .journal()
        .attempts
        .values()
        .find(|attempt| {
            attempt.task == task.id && launch.session.as_ref() == Some(&attempt.session)
        })
        .context("managed launch attempt missing")?
        .id
        .clone();
    let session = launch
        .session
        .as_ref()
        .and_then(|id| store.journal().sessions.get(id))
        .context("managed session missing")?;
    let target = session.target.clone();
    let live = super::submit::verify_target(&target, Instant::now() + Duration::from_secs(2))
        .and_then(|()| super::route::release_pin(&target, Instant::now() + Duration::from_secs(2)));
    if live.is_ok() {
        if launch.problem.is_some() {
            store.transaction(|journal| {
                journal.observe_attached(
                    id,
                    &attempt_id,
                    super::lifecycle::AttachedObservation::Live,
                )
            })?;
        }
        return super::inspect_journal(store.journal(), id);
    }
    match recover_final(launch) {
        Ok((pane, evidence)) => {
            anyhow::ensure!(pane == target.pane, "final launch pane mismatch");
            close_attached(store, id, &attempt_id, evidence)?;
            super::inspect_journal(store.journal(), id)
        }
        Err(error) => {
            // A replacement server is positive evidence of lost ownership, not
            // an exit receipt. An unavailable endpoint alone remains uncertain.
            let replacement = replacement_instance(&target);
            let previous = &store
                .journal()
                .attempts
                .get(&attempt_id)
                .context("attempt missing")?
                .state;
            let (state, problem) = super::lifecycle::unavailable_state(
                previous,
                replacement.is_some(),
                &error.to_string(),
            );
            let lost = state == AttemptState::Lost;
            if launch.problem.as_ref() != Some(&problem)
                || store
                    .journal()
                    .attempts
                    .get(&attempt_id)
                    .is_some_and(|attempt| attempt.state != state)
            {
                store.transaction(|journal| {
                    journal.observe_attached(
                        id,
                        &attempt_id,
                        super::lifecycle::AttachedObservation::Unavailable {
                            replacement: replacement.is_some(),
                            problem,
                        },
                    )
                })?;
            }
            Err(error.context(if lost {
                "managed process ownership lost after fux replacement; no automatic resume or prompt replay"
            } else {
                "managed process lifecycle uncertain; ownership and prompt history retained"
            }))
        }
    }
}

/// Same-user live listing, deliberately unpinned only to identify a replacement.
/// No pane from this response can become the retained session's target.
fn replacement_instance(target: &Target) -> Option<String> {
    let response = crate::fux::request_until(
        &target.runtime.join("manager.sock"),
        json!({"request":"info"}),
        Instant::now() + Duration::from_secs(2),
    )
    .ok()?;
    if response.get("reply").and_then(Value::as_str) != Some("info") {
        return None;
    }
    let instance = response.pointer("/info/instance_nonce")?.as_str()?;
    (!instance.is_empty() && instance.len() <= 128 && instance != target.instance)
        .then(|| instance.to_owned())
}

fn attach(
    store: &mut Store,
    launch: &Launch,
    pane_id: u32,
    pid: Option<u32>,
    evidence: Option<LaunchFinal>,
) -> Result<()> {
    let session = super::store::nonce()?;
    let attempt = super::store::nonce()?;
    store.transaction(|journal| {
        journal.attach_managed(
            &launch.id,
            super::lifecycle::Attachment {
                session: session.clone(),
                attempt,
                pane: pane_id,
                pid,
                evidence,
            },
        )
    })?;
    if pid.is_some() {
        let target = store
            .journal()
            .sessions
            .get(&session)
            .context("committed session missing")?
            .target
            .clone();
        if let Err(release_error) =
            super::route::release_pin(&target, Instant::now() + Duration::from_secs(2))
        {
            // The process can exit after attachment was durably recorded. Only
            // matching retained evidence can turn this failed release into a
            // completed lifecycle; a live/pending or unavailable result remains
            // an error, preserving lost-reply reconciliation semantics.
            let current = store
                .journal()
                .launches
                // Resume attachment archives the old launch under the pending
                // operation ID and installs the new launch under the task ID.
                .get(launch.task_id())
                .context("committed launch missing")?
                .clone();
            let (pane, evidence) = recover_final(&current).map_err(|error| {
                release_error.context(format!("no matching exit evidence: {error:#}"))
            })?;
            anyhow::ensure!(pane == target.pane, "final launch pane mismatch");
            let attempt = store
                .journal()
                .attempts
                .values()
                .find(|attempt| attempt.session == session && attempt.task == current.task_id())
                .context("committed attempt missing")?
                .id
                .clone();
            close_attached(store, &current.id, &attempt, evidence)?;
        }
    }
    Ok(())
}

/// Record process completion without changing task outcome or delivery evidence.
fn close_attached(
    store: &mut Store,
    id: &str,
    attempt_id: &str,
    evidence: LaunchFinal,
) -> Result<()> {
    store.transaction(|journal| {
        journal.observe_attached(
            id,
            attempt_id,
            super::lifecycle::AttachedObservation::Final(evidence),
        )
    })
}

fn recover_final(launch: &Launch) -> Result<(u32, LaunchFinal)> {
    let deadline = Instant::now() + Duration::from_secs(4);
    let pane = if let Some(pane) = launch.pane {
        pane
    } else {
        let response = crate::fux::completed_until(
            &launch.runtime.join(format!("{}.sock", launch.workspace)),
            json!({"command":"events","id":1,"instance":launch.instance,
                "after":{"stream":launch.stream,"sequence":launch.event_sequence}}),
            deadline,
        )?;
        anyhow::ensure!(
            response.get("id").and_then(Value::as_u64) == Some(1),
            "event reply ID mismatch"
        );
        let replay = response
            .pointer("/result/value")
            .context("event replay missing")?;
        anyhow::ensure!(
            replay.pointer("/cursor/stream").and_then(Value::as_u64) == Some(launch.stream),
            "event stream changed"
        );
        let events = replay
            .get("events")
            .and_then(Value::as_array)
            .context("event replay missing")?;
        let expected = serde_json::to_value(command(launch))?;
        let mut found = None;
        for event in events {
            if event.get("event").and_then(Value::as_str) == Some("pane.opened")
                && event.get("command") == Some(&expected)
            {
                anyhow::ensure!(
                    event.pointer("/cursor/stream").and_then(Value::as_u64) == Some(launch.stream)
                        && event
                            .pointer("/cursor/sequence")
                            .and_then(Value::as_u64)
                            .is_some_and(|s| s > launch.event_sequence),
                    "launch event cursor mismatch"
                );
                let pane = event
                    .get("pane")
                    .and_then(Value::as_u64)
                    .and_then(|p| u32::try_from(p).ok())
                    .filter(|p| *p > 0)
                    .context("invalid launch event pane")?;
                anyhow::ensure!(
                    found.is_none(),
                    "launch marker has multiple creation events"
                );
                found = Some(pane);
            }
        }
        found.context("no retained creation event for launch")?
    };
    let record =
        match crate::fux::manager::final_record(&launch.runtime, &launch.instance, pane, deadline)
            .context("final launch evidence unavailable or expired")?
        {
            crate::fux::manager::FinalOutcome::Record(record) => record,
            crate::fux::manager::FinalOutcome::Pending => {
                anyhow::bail!("final launch evidence unavailable: pane is still live")
            }
        };
    anyhow::ensure!(
        record.pane == pane
            && record.workspace == launch.workspace
            && record.stream == launch.stream
            && record.command == command(launch)
            && record.cwd == launch.cwd,
        "final launch identity mismatch"
    );
    let exit_status = record.exit_status;
    let text = &record.capture.text;
    let mut end = text.len().min(4096);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let truncated = record.capture.truncated || end < text.len();
    Ok((
        pane,
        LaunchFinal {
            exit_status,
            text: text.get(..end).context("invalid text boundary")?.into(),
            truncated,
            input_sequence: record.input_sequence,
        },
    ))
}

#[cfg(test)]
mod argv_tests {
    #[test]
    fn launch_final_retention_is_exactly_fux_ceiling() {
        assert_eq!(
            super::LAUNCH_FINAL_RETAIN_MS,
            crate::fux::MAX_FINAL_RETENTION_MS
        );
        const {
            assert!(
                super::LAUNCH_FINAL_RETAIN_MS > 0,
                "fux refuses a zero retention"
            );
        }
    }

    #[test]
    fn empty_arguments_are_preserved_but_executable_and_nul_are_rejected() {
        let arguments = vec!["/bin/printf".to_owned(), "<%s>".to_owned(), String::new()];
        assert!(super::validate_argv(&arguments).is_ok());
        assert!(super::validate_argv(&[String::new(), "argument".to_owned()]).is_err());
        assert!(super::validate_argv(&["/bin/printf".to_owned(), "\0".to_owned()]).is_err());
    }
}
