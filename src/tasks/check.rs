//! Explicit check-command evidence. Command success is not verified task completion.
use super::{
    check_artifacts,
    model::*,
    store::{Busy, Store},
};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    os::unix::process::ExitStatusExt,
    path::Path,
    process::Command,
    time::{Duration, Instant},
};

fn view(store: &Store, id: &str) -> Result<Value> {
    let check = store.journal().checks.get(id).context("check not found")?;
    Ok(
        json!({"generation":store.journal().generation,"check":check,
        "passed":(check.phase == CheckPhase::Finished).then_some(check.exit_code == Some(0))}),
    )
}
pub fn inspect(root: &Path, id: &str) -> Result<Value> {
    view(&Store::open(root)?, id)
}

pub fn require(root: &Path, task_id: &str, check_id: &str, argv: Vec<String>) -> Result<Value> {
    anyhow::ensure!(super::model::id(check_id), "invalid required check ID");
    super::launch::validate_argv(&argv)?;
    let mut store = Store::open(root)?;
    let task = store
        .journal()
        .tasks
        .get(task_id)
        .context("task not found")?;
    if let Some(existing) = task.required_checks.get(check_id) {
        anyhow::ensure!(
            *existing == argv,
            "required check already has different command"
        );
        return super::inspect_journal(store.journal(), task_id);
    }
    anyhow::ensure!(task.outcome == TaskOutcome::Open, "task is no longer open");
    anyhow::ensure!(
        store.journal().launches.contains_key(task_id),
        "required checks need a managed task"
    );
    anyhow::ensure!(
        !store
            .journal()
            .checks
            .values()
            .any(|check| check.task == task_id),
        "check policy is sealed once the first check is submitted"
    );
    store.transaction(|journal| {
        journal
            .tasks
            .get_mut(task_id)
            .context("task disappeared")?
            .required_checks
            .insert(check_id.into(), argv);
        Ok(())
    })?;
    super::inspect_journal(store.journal(), task_id)
}

fn clipped(bytes: &[u8]) -> (String, bool) {
    let text = String::from_utf8_lossy(bytes);
    let mut end = text.len().min(4096);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (
        text.get(..end).unwrap_or_default().to_owned(),
        end < text.len(),
    )
}

pub struct Run {
    pub task: String,
    pub id: String,
    pub argv: Vec<String>,
    pub timeout_ms: u64,
    pub requirement: Option<String>,
    pub source: Option<String>,
    pub artifacts: BTreeMap<String, String>,
}
pub fn artifact_arguments(values: Vec<String>) -> Result<BTreeMap<String, String>> {
    let mut requests = BTreeMap::new();
    for value in values {
        let (name, artifact) = value.split_once('=').context("artifact must be NAME=ID")?;
        anyhow::ensure!(
            super::model::id(name)
                && super::model::id(artifact)
                && requests.insert(name.into(), artifact.into()).is_none(),
            "invalid or duplicate artifact requirement"
        );
    }
    anyhow::ensure!(
        requests.len() <= 8,
        "at most eight artifact captures per check"
    );
    Ok(requests)
}
pub fn run(root: &Path, request: Run) -> Result<Value> {
    let Run {
        task: task_id,
        id,
        argv,
        timeout_ms,
        requirement,
        source,
        artifacts,
    } = request;
    let task_id = task_id.as_str();
    let id = id.as_str();
    anyhow::ensure!(
        super::model::id(id) && (1..=300_000).contains(&timeout_ms),
        "invalid check ID or timeout"
    );
    super::launch::validate_argv(&argv)?;
    let mut store = Store::open(root)?;
    if let Some(check) = store.journal().checks.get(id) {
        anyhow::ensure!(
            check.task == task_id
                && check.argv == argv
                && check.timeout_ms == timeout_ms
                && check.requirement == requirement
                && check.source == source
                && check.artifact_requests == artifacts,
            "check ID already has different intent"
        );
        return view(&store, id);
    }
    let task = store
        .journal()
        .tasks
        .get(task_id)
        .context("task not found")?
        .clone();
    anyhow::ensure!(task.outcome == TaskOutcome::Open, "task is no longer open");
    if let Some(name) = &requirement {
        anyhow::ensure!(
            task.required_checks.get(name) == Some(&argv),
            "check does not match its required name and command"
        );
    }
    check_artifacts::prepare(store.journal(), &task, &source, &artifacts)?;
    let launch = store
        .journal()
        .launches
        .get(task_id)
        .context("checks require a managed task launch")?
        .clone();
    let snapshot = source
        .as_ref()
        .map(|id| {
            let snapshot = store
                .journal()
                .sources
                .get(id)
                .context("source not found")?;
            anyhow::ensure!(
                snapshot.task == task_id && snapshot.attempt == task.attempt,
                "source belongs to another task or attempt"
            );
            Ok::<_, anyhow::Error>(snapshot.clone())
        })
        .transpose()?;
    let cwd = if snapshot.is_some() {
        std::fs::canonicalize(root)?.join(format!("check-{}", super::store::nonce()?))
    } else {
        if let Some(tree) = &launch.worktree {
            super::worktree::launch_path(root, &mut store, tree)?;
        }
        anyhow::ensure!(launch.cwd.is_dir(), "check cwd unavailable");
        anyhow::ensure!(
            !store
                .journal()
                .worktrees
                .values()
                .any(|tree| tree.remove_force.is_some() && launch.cwd.starts_with(&tree.path)),
            "check cwd belongs to a worktree with removal intent"
        );
        launch.cwd
    };
    let mut check = Check {
        id: id.into(),
        source,
        artifact_requests: artifacts,
        artifact_problems: BTreeMap::new(),
        requirement,
        created_generation: store
            .journal()
            .generation
            .checked_add(1)
            .context("journal generation exhausted")?,
        task: task_id.into(),
        attempt: task.attempt.clone(),
        cwd,
        argv,
        timeout_ms,
        created_ms: super::now_ms()?,
        phase: CheckPhase::Submitted,
        exit_code: None,
        signal: None,
        stdout: String::new(),
        stderr: String::new(),
        truncated: false,
        problem: None,
    };
    // Held independently of Store, including the publication retry window. Child exec
    // closes this descriptor: runner disappearance does not establish child termination.
    let _runner = store
        .check_runners(false)?
        .context("check recovery is active")?;
    store.transaction(|journal| {
        journal.checks.insert(id.into(), check.clone());
        Ok(())
    })?;
    // Cooperating task controls remain available while this explicit command runs.
    // Stable check IDs and retained records bound concurrent commands and prevent replay.
    drop(store);
    let mut command = Command::new(check.argv.first().context("check executable missing")?);
    command
        .args(check.argv.iter().skip(1))
        .current_dir(&check.cwd);
    let mut directory = None;
    let execution = (|| {
        if let Some(source) = &snapshot {
            directory = Some(super::source::materialize(root, &check.cwd, source)?);
        }
        crate::platform::process::run_command(
            &mut command,
            Instant::now() + Duration::from_millis(timeout_ms),
        )
    })();
    match execution {
        Ok(output) => {
            check.phase = CheckPhase::Finished;
            check.exit_code = output.status.code();
            check.signal = output.status.signal();
            let (stdout, out_truncated) = clipped(&output.stdout);
            let (stderr, err_truncated) = clipped(&output.stderr);
            check.stdout = stdout;
            check.stderr = stderr;
            check.truncated = out_truncated || err_truncated;
        }
        Err(error) => {
            check.phase = CheckPhase::Uncertain;
            check.problem = Some(format!("{error:#}").chars().take(128).collect());
        }
    }
    let artifacts = check_artifacts::capture(&mut check, directory.as_ref(), &task);
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut store = loop {
        match Store::open(root) {
            Ok(store) => break store,
            Err(error) if error.is::<Busy>() && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20))
            }
            Err(error) => {
                return Err(
                    error.context("check ran but its result could not be recorded; do not replay")
                );
            }
        }
    };
    store.transaction(|journal| {
        let existing = journal.checks.get(id).context("check intent disappeared")?;
        anyhow::ensure!(
            existing.phase == CheckPhase::Submitted
                && existing.created_generation == check.created_generation
                && existing.requirement == check.requirement
                && existing.source == check.source
                && existing.artifact_requests == check.artifact_requests
                && existing.created_ms == check.created_ms
                && existing.task == check.task
                && existing.attempt == check.attempt
                && existing.cwd == check.cwd
                && existing.argv == check.argv
                && existing.timeout_ms == check.timeout_ms,
            "check intent changed during execution"
        );
        let generation = journal
            .generation
            .checked_add(1)
            .context("journal generation exhausted")?;
        for mut artifact in artifacts {
            artifact.created_generation = generation;
            anyhow::ensure!(
                !journal.artifacts.contains_key(&artifact.id),
                "reserved artifact ID was replaced"
            );
            journal.artifacts.insert(artifact.id.clone(), artifact);
        }
        journal.checks.insert(id.into(), check);
        Ok(())
    })?;
    view(&store, id)
}
