//! Fux-owned headless Codex worker. The journal is the local input queue.
use super::{protocol::Phase, session::Session, state};
use crate::tasks::{
    model::{LaunchPhase, Ownership, Target},
    store::{self, Store},
    submit,
};
use anyhow::{Context, Result, ensure};
use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

struct Signals(Vec<signal_hook::SigId>);
impl Drop for Signals {
    fn drop(&mut self) {
        for id in self.0.drain(..) {
            signal_hook::low_level::unregister(id);
        }
    }
}

fn retry_busy<T>(
    cancelled: &AtomicBool,
    deadline: Instant,
    mut action: impl FnMut() -> Result<T>,
) -> Result<T> {
    loop {
        ensure!(!cancelled.load(Ordering::Relaxed), "native owner stopped");
        match action() {
            Err(error) if error.is::<store::Busy>() && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10))
            }
            result => return result,
        }
    }
}

pub fn start(root: &Path, mut request: crate::tasks::launch::Start) -> Result<serde_json::Value> {
    ensure!(
        request.integration.is_none(),
        "native Codex cannot use a terminal integration"
    );
    crate::tasks::launch::validate_argv(&request.argv)?;
    let mut argv = vec![
        std::env::current_exe()?
            .to_str()
            .context("zor executable must be UTF-8")?
            .into(),
        "--state-directory".into(),
        root.to_str()
            .context("zor state path must be UTF-8")?
            .into(),
        "codex-worker".into(),
        "--task".into(),
        request.id.clone(),
        "--".into(),
    ];
    argv.extend(request.argv);
    request.argv = argv;
    request.agent = Some("codex".into());
    crate::tasks::launch::start(root, request)
}

fn identity(root: &Path, task: &str, marker: &str) -> Result<(String, Target, PathBuf)> {
    let store = Store::open(root)?;
    let journal = store.journal();
    let launch = journal
        .launches
        .get(task)
        .context("native launch missing")?;
    let task = journal
        .tasks
        .get(task)
        .context("native task not attached")?;
    let attempt = journal
        .attempts
        .get(&task.attempt)
        .context("native attempt missing")?;
    let session = journal
        .sessions
        .get(&attempt.session)
        .context("native session missing")?;
    ensure!(
        launch.phase == LaunchPhase::Attached
            && !launch.stop_requested
            && launch.marker == marker
            && launch.agent.as_deref() == Some("codex")
            && session.ownership == Ownership::Managed
            && session.launch.as_deref() == Some(&task.id)
            && session.target.pid == Some(std::process::id()),
        "native worker is not the current owned Codex pane process"
    );
    Ok((
        task.attempt.clone(),
        session.target.clone(),
        launch.cwd.clone(),
    ))
}

/// Internal entry point, launched as the pane's command. Merely possessing a
/// marker never authorizes registration: the live fux pane must own this PID.
pub fn run(root: &Path, task: &str, argv: Vec<String>) -> Result<u8> {
    let cancelled = Arc::new(AtomicBool::new(false));
    let mut signals = Signals(Vec::new());
    for signal in [
        signal_hook::consts::SIGINT,
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGHUP,
        signal_hook::consts::SIGQUIT,
    ] {
        signals
            .0
            .push(signal_hook::flag::register(signal, cancelled.clone())?);
    }
    let marker = std::env::var("ZOR_LAUNCH_ID").context("native launch marker missing")?;
    ensure!(crate::tasks::model::id(task), "invalid native task ID");
    let deadline = Instant::now() + Duration::from_secs(10);
    // Fux creates the pane before launch::start can attach its journal records.
    let (attempt, target, cwd) = loop {
        ensure!(!cancelled.load(Ordering::Relaxed), "native owner stopped");
        match identity(root, task, &marker) {
            Ok(identity) => break identity,
            Err(error) => {
                ensure!(
                    Instant::now() < deadline,
                    "native attachment failed: {error:#}"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    };
    submit::verify_target(&target, Instant::now() + Duration::from_millis(200))?;
    ensure!(
        !retry_busy(&cancelled, deadline, || Store::open(root))?
            .journal()
            .native_workers
            .contains_key(task),
        "native worker already recorded; explicit session recreation is required"
    );
    let mut command = Command::new(argv.first().context("native executable missing")?);
    command.args(argv.get(1..).context("native arguments missing")?);
    let mut native = Session::open_cancellable(command, &cwd, None, deadline, cancelled.clone())?;
    let mut producer = store::nonce()?;
    let storage = native
        .rollout
        .as_deref()
        .map(super::storage::Storage::capture)
        .transpose()?;
    let owner = state::Worker {
        task: task.into(),
        attempt,
        marker: marker.clone(),
        producer: producer.clone(),
        thread: native.thread.clone(),
        storage,
        ready: true,
        retired: Vec::new(),
    };
    // Recheck after the handshake, which can outlast a concurrent owned stop.
    retry_busy(&cancelled, deadline, || {
        identity(root, task, &marker)?;
        submit::verify_target(&target, Instant::now() + Duration::from_millis(200))?;
        if let Some(storage) = &owner.storage {
            storage.verify()?;
        }
        state::register(root, owner.clone())
    })?;
    loop {
        let result = drive(
            root,
            task,
            &marker,
            &producer,
            &target,
            &mut native,
            &cancelled,
        );
        // Stop the child before journal cleanup, even if the journal is unavailable.
        let cleanup = native
            .client
            .shutdown(Instant::now() + Duration::from_millis(300));
        let reason = match &result {
            Ok(Some(_)) => "native session recreation".to_owned(),
            Ok(None) => "native owner stopped".to_owned(),
            Err(error) => format!("native owner failed: {error:#}"),
        };
        let retired = retry_busy(
            &AtomicBool::new(false),
            Instant::now() + Duration::from_millis(300),
            || state::retire(root, task, &producer, &reason),
        );
        if let Err(error) = cleanup.and(retired) {
            if let Ok(Some(action)) = &result {
                state::fail_recreation(root, action)?;
            }
            return Err(error);
        }
        let Some(action) = result? else {
            return Ok(0);
        };
        let replacement = (|| -> Result<Session> {
            action.storage.require_materialized()?;
            let remaining = action
                .deadline_ms
                .checked_sub(crate::tasks::now_ms()?)
                .context("native recreation deadline expired")?;
            let deadline = Instant::now() + Duration::from_millis(remaining);
            let mut command = Command::new(argv.first().context("native executable missing")?);
            command.args(argv.get(1..).context("native arguments missing")?);
            let replacement = Session::open_cancellable(
                command,
                &cwd,
                Some(&action.thread),
                deadline,
                cancelled.clone(),
            )?;
            action.storage.verify_resumed(
                replacement
                    .rollout
                    .as_deref()
                    .context("resumed storage path missing")?,
            )?;
            let mut candidate = owner.clone();
            candidate.producer = action.replacement.clone();
            retry_busy(&cancelled, deadline, || {
                identity(root, task, &marker)?;
                submit::verify_target(&target, Instant::now() + Duration::from_millis(200))?;
                action.storage.require_materialized()?;
                state::finish_recreation(root, &action, candidate.clone())
            })?;
            Ok(replacement)
        })();
        match replacement {
            Ok(replacement) => {
                native = replacement;
                producer = action.replacement;
            }
            Err(error) => {
                retry_busy(
                    &AtomicBool::new(false),
                    Instant::now() + Duration::from_millis(300),
                    || state::fail_recreation(root, &action),
                )
                .with_context(|| format!("native recreation failed: {error:#}"))?;
                return Err(error);
            }
        }
    }
}

fn drive(
    root: &Path,
    task: &str,
    marker: &str,
    producer: &str,
    target: &Target,
    native: &mut Session,
    cancelled: &AtomicBool,
) -> Result<Option<state::Recreation>> {
    let mut events: VecDeque<_> = std::mem::take(&mut native.early_events).into();
    loop {
        if cancelled.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let step = (|| -> Result<Option<state::Recreation>> {
            let (_, current, _) = identity(root, task, marker)?;
            ensure!(
                current.identity() == target.identity(),
                "native owner identity changed"
            );
            submit::verify_target(target, Instant::now() + Duration::from_millis(200))?;
            let entries: Vec<_> = Store::open(root)?
                .journal()
                .native_turns
                .values()
                .filter(|entry| entry.task == task)
                .cloned()
                .collect();
            // Bound event work before servicing persisted input and shutdown again.
            for _ in 0..32 {
                if events.is_empty() {
                    match native.client.poll()? {
                        Some(frame) => events.push_back(frame),
                        None => break,
                    }
                }
                let frame = events.front().context("native event missing")?;
                for entry in entries
                    .iter()
                    .filter(|entry| entry.evidence.phase != Phase::Prepared)
                {
                    // Production history queries include the durable control ID.
                    // The reducer's bare read ID is only an internal normalization
                    // target, never authority for an unsolicited server response.
                    if frame.get("method").is_none()
                        && frame.get("id").and_then(serde_json::Value::as_str)
                            == Some(format!("read:{}", entry.evidence.operation).as_str())
                    {
                        continue;
                    }
                    state::observe(root, &entry.evidence.operation, producer, frame)?;
                }
                // Persist before polling again: EOF must not erase completed output.
                // Busy leaves the front event queued; duplicate publication is safe.
                events.pop_front();
            }
            for entry in entries
                .iter()
                .filter(|entry| entry.evidence.phase == Phase::Prepared)
            {
                if let Some(request) =
                    state::take_submission(root, &entry.evidence.operation, producer)?
                {
                    let remaining = entry
                        .evidence
                        .deadline_ms
                        .saturating_sub(crate::tasks::now_ms()?);
                    native.client.send(
                        &request,
                        Instant::now() + Duration::from_millis(remaining.min(200)),
                    )?;
                }
            }
            for entry in &entries {
                for (id, control) in &entry.controls {
                    if control.kind == state::ControlKind::Recreate {
                        if let Some(action) =
                            state::take_recreation(root, &entry.evidence.operation, producer, id)?
                        {
                            return Ok(Some(action));
                        }
                        continue;
                    }
                    if let Some(request) =
                        state::take_control(root, &entry.evidence.operation, producer, id)?
                    {
                        let remaining = control
                            .created_ms
                            .saturating_add(control.timeout_ms)
                            .saturating_sub(crate::tasks::now_ms()?);
                        native.client.send(
                            &request,
                            Instant::now() + Duration::from_millis(remaining.min(200)),
                        )?;
                    }
                }
            }
            Ok(None)
        })();
        match step {
            Ok(Some(action)) => return Ok(Some(action)),
            Err(error) if !error.is::<store::Busy>() => return Err(error),
            _ => {}
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
