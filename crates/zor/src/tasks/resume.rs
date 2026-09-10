//! Explicit native-session recreation; authorization is a retained launch operation.
use super::{model::*, store::Store};
use anyhow::{Context, Result};
use serde_json::Value;
use std::{
    path::Path,
    time::{Duration, Instant},
};

pub fn run(root: &Path, task_id: &str, operation: &str, instance: &str) -> Result<Value> {
    anyhow::ensure!(
        id(operation) && operation != task_id,
        "resume requires a distinct stable operation ID"
    );
    let mut store = Store::open(root)?;
    if let Some(existing) = store
        .journal()
        .launches
        .values()
        .find(|launch| {
            launch
                .resume
                .as_ref()
                .is_some_and(|resume| resume.operation == operation)
        })
        .cloned()
    {
        anyhow::ensure!(
            existing.task_id() == task_id && existing.instance == instance,
            "resume operation already has different intent"
        );
        if matches!(existing.phase, LaunchPhase::Attached | LaunchPhase::Closed) {
            return super::inspect_journal(store.journal(), task_id);
        }
        return super::launch::submit_prepared(root, &mut store, &existing.id);
    }
    anyhow::ensure!(
        !store.journal().launches.contains_key(operation)
            && !store.journal().tasks.contains_key(operation),
        "resume operation ID already exists"
    );
    let task = store
        .journal()
        .tasks
        .get(task_id)
        .context("resume task missing")?
        .clone();
    let old = store
        .journal()
        .launches
        .get(task_id)
        .context("resume requires a managed task")?
        .clone();
    eligible(store.journal(), &task, &old)?;
    let attempt = store
        .journal()
        .attempts
        .get(&task.attempt)
        .context("attempt missing")?;
    let session = store
        .journal()
        .sessions
        .get(&attempt.session)
        .context("session missing")?;
    if let Some(pid) = session.target.pid {
        let pid = i32::try_from(pid).context("invalid original process ID")?;
        anyhow::ensure!(
            matches!(
                nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None),
                Err(nix::errno::Errno::ESRCH)
            ),
            "original process absence is unproven; resume refused"
        );
    }
    if attempt.state == AttemptState::Lost {
        anyhow::ensure!(
            instance != old.instance,
            "lost task needs a replacement fux incarnation"
        );
    }
    let integration = old
        .integration
        .as_ref()
        .context("resume requires the OpenCode integration")?;
    let environment = integration
        .storage_environment
        .clone()
        .context("resume storage metadata unavailable")?;
    let producer = integration
        .producer
        .as_ref()
        .context("resume producer missing")?;
    let native_session = store
        .journal()
        .prompts
        .values()
        .filter(|prompt| prompt.attempt == task.attempt)
        .filter_map(|prompt| prompt.report_binding.as_ref())
        .filter(|binding| &binding.producer == producer)
        .max_by_key(|binding| binding.sequence)
        .map(|binding| binding.message.session.clone())
        .context("resume native session binding unavailable")?;
    let mut original_argv = old.argv.clone();
    if let Some(previous) = &old.resume {
        anyhow::ensure!(
            original_argv.ends_with(&["--session".into(), previous.native_session.clone()]),
            "retained resume command changed"
        );
        original_argv.truncate(original_argv.len().saturating_sub(2));
    }
    // Caller-supplied arbitrary replacement commands are deliberately not a resume policy.
    anyhow::ensure!(
        !original_argv.iter().skip(1).any(|arg| matches!(
            arg.as_str(),
            "--" | "--session" | "-s" | "--continue" | "-c" | "--fork"
        ) || arg.starts_with("--session=")
            || arg.starts_with("-s")),
        "existing command has session selection flags; resume refused"
    );
    let mut launch = old.clone();
    launch.id = operation.into();
    launch.task = Some(task_id.into());
    launch.resume = Some(Resume {
        operation: operation.into(),
        previous_attempt: task.attempt,
        native_session: native_session.clone(),
        environment,
    });
    launch.argv = original_argv;
    launch.argv.extend(["--session".into(), native_session]);
    launch.marker = super::store::nonce()?;
    launch.integration = Some(super::integration::prepare(
        root,
        &launch.marker,
        IntegrationKind::Opencode,
    )?);
    launch.instance = instance.into();
    launch.stream = 0;
    launch.phase = LaunchPhase::Prepared;
    launch.session = None;
    launch.pane = None;
    launch.problem = None;
    launch.final_evidence = None;
    launch.stop_requested = false;
    launch.created_ms = super::now_ms()?;
    if let Some(tree) = &launch.worktree {
        anyhow::ensure!(
            super::worktree::launch_path(root, &mut store, tree)? == launch.cwd,
            "resume worktree changed"
        );
    }
    anyhow::ensure!(
        std::fs::canonicalize(&launch.cwd)? == launch.cwd && launch.cwd.is_dir(),
        "resume cwd changed"
    );
    let workspace = super::launch::listing(&launch, Instant::now() + Duration::from_secs(2))?;
    launch.stream = workspace
        .pointer("/event_cursor/stream")
        .and_then(Value::as_u64)
        .context("workspace stream missing")?;
    launch.event_sequence = workspace
        .pointer("/event_cursor/sequence")
        .and_then(Value::as_u64)
        .context("workspace sequence missing")?;
    store.transaction(|journal| {
        journal.launches.insert(operation.into(), launch);
        Ok(())
    })?;
    super::launch::submit_prepared(root, &mut store, operation)
}

fn eligible(journal: &Journal, task: &Task, launch: &Launch) -> Result<()> {
    anyhow::ensure!(
        task.outcome == TaskOutcome::Open && !launch.stop_requested,
        "resume requires an open task without stop intent"
    );
    anyhow::ensure!(
        journal
            .attempts
            .get(&task.attempt)
            .is_some_and(|attempt| matches!(
                attempt.state,
                AttemptState::Lost | AttemptState::Finished
            )),
        "resume requires reconciled lost or finished state"
    );
    anyhow::ensure!(
        !journal
            .groups
            .values()
            .any(|group| group.members.iter().any(|member| member.task == task.id)),
        "group membership pins the old attempt; resume refused"
    );
    anyhow::ensure!(
        !journal.checks.values().any(|check| check.task == task.id
            && matches!(check.phase, CheckPhase::Submitted | CheckPhase::Uncertain)),
        "unresolved check executions prevent resume"
    );
    Ok(())
}

pub(super) fn authorize_submission(journal: &Journal, launch: &Launch) -> Result<()> {
    if launch.task.is_some() {
        let task = journal
            .tasks
            .get(launch.task_id())
            .context("resume task missing")?;
        let current = journal
            .launches
            .get(&task.id)
            .context("resume original launch missing")?;
        eligible(journal, task, current)?;
    }
    Ok(())
}

pub(super) fn attach_launch(journal: &mut Journal, pending: &Launch) -> Result<()> {
    let task_id = pending.task.as_ref().context("resume owner missing")?;
    let resume = pending
        .resume
        .as_ref()
        .context("resume authorization missing")?;
    anyhow::ensure!(
        journal
            .tasks
            .get(task_id)
            .is_some_and(|task| task.attempt == resume.previous_attempt),
        "resume original attempt changed"
    );
    let mut old = journal
        .launches
        .remove(task_id)
        .context("previous launch missing")?;
    let old_session = old.session.as_ref().context("previous session missing")?;
    journal
        .sessions
        .get_mut(old_session)
        .context("previous session missing")?
        .launch = Some(pending.id.clone());
    old.id = pending.id.clone();
    old.task = Some(task_id.clone());
    let mut current = journal
        .launches
        .remove(&pending.id)
        .context("resume intent missing")?;
    current.id = task_id.clone();
    current.task = None;
    journal.launches.insert(old.id.clone(), old);
    journal.launches.insert(task_id.clone(), current);
    Ok(())
}

pub(super) fn validate(journal: &Journal) -> Result<()> {
    let mut operations = std::collections::BTreeSet::new();
    let mut pending_tasks = std::collections::BTreeSet::new();
    for launch in journal.launches.values() {
        let Some(resume) = &launch.resume else {
            continue;
        };
        anyhow::ensure!(
            id(&resume.operation)
                && id(&resume.native_session)
                && operations.insert(&resume.operation)
                && journal
                    .attempts
                    .get(&resume.previous_attempt)
                    .is_some_and(|attempt| attempt.task == launch.task_id())
                && launch.integration.is_some(),
            "invalid resume authorization"
        );
        anyhow::ensure!(
            launch
                .argv
                .ends_with(&["--session".into(), resume.native_session.clone()]),
            "resume command differs from retained native session"
        );
        super::integration::validate_storage_environment(&resume.environment)?;
        if !matches!(launch.phase, LaunchPhase::Attached | LaunchPhase::Closed) {
            let task = journal
                .tasks
                .get(launch.task_id())
                .context("pending resume task missing")?;
            anyhow::ensure!(
                launch.task.is_some()
                    && launch.id == resume.operation
                    && task.attempt == resume.previous_attempt
                    && pending_tasks.insert(&task.id)
                    && !journal
                        .groups
                        .values()
                        .any(|group| group.members.iter().any(|member| member.task == task.id)),
                "invalid pending resume relationship"
            );
        }
    }
    Ok(())
}
