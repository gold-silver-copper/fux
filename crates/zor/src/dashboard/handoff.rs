//! Controller-owned preparation and viewer lifecycle. No terminal or process ownership crosses hosts.
use crate::{
    machines::{
        connection::{Gateway, State},
        supervision::{MachineSource, Selection, Source},
    },
    service::client::Client,
    tasks::{
        model::Target,
        supervise::{Action as TaskAction, Expected},
    },
};
use anyhow::{Context, Result, ensure};
use std::{
    io::Read,
    os::{
        fd::{AsFd, AsRawFd},
        unix::process::ExitStatusExt,
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

#[derive(Clone)]
pub(super) enum Action {
    Inspect,
    Result,
    Cancel,
    Stop,
    Reconcile,
    Resume { operation: String, instance: String },
    ResumeStatus { operation: String },
    Attach,
}
pub(super) enum Subject {
    Task(Box<Expected>),
    Observed(crate::watch::Handle),
}
impl Subject {
    pub fn row(row: &super::Row) -> Result<Self> {
        if let Some(expected) = &row.expected {
            return Ok(Self::Task(Box::new(expected.clone())));
        }
        ensure!(
            row.kind == "observation",
            "this row has no attachable agent or task"
        );
        let target = row
            .target
            .as_ref()
            .context("observed agent has no fresh process target")?;
        Ok(Self::Observed(crate::watch::Handle {
            instance: target.instance.clone(),
            workspace: target.workspace.clone(),
            stream: target.stream,
            pane: target.pane,
            pid: target.pid,
        }))
    }
}
pub(super) enum Outcome {
    Detail(String),
    Viewer(Box<PreparedViewer>),
}
pub(super) struct Pending {
    pub selection: Selection,
    pub label: String,
    receiver: mpsc::Receiver<Result<Outcome>>,
    cancelled: Arc<AtomicBool>,
    mutation_started: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}
impl Pending {
    pub fn start(
        source: MachineSource,
        selection: Selection,
        subject: Subject,
        action: Action,
        intent_path: PathBuf,
    ) -> Result<Self> {
        let (sender, receiver) = mpsc::sync_channel(1);
        let mut label = match &subject {
            Subject::Task(expected) => format!("{} / {}", source.name, expected.task),
            Subject::Observed(handle) => format!(
                "{} / {} / pane {}",
                source.name, handle.workspace, handle.pane
            ),
        };
        if let Action::Resume {
            operation,
            instance,
        } = &action
        {
            label.push_str(&format!(" / resume {operation} / fux {instance}"));
        }
        ensure!(
            matches!(subject, Subject::Task(_))
                || matches!(action, Action::Inspect | Action::Attach),
            "task-only action unavailable for an observed agent"
        );
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancel = Arc::clone(&cancelled);
        let mutation_started = Arc::new(AtomicBool::new(false));
        let started = Arc::clone(&mutation_started);
        let selected = selection.clone();
        let thread = std::thread::Builder::new()
            .name("zor-dashboard-action".into())
            .spawn(move || {
                let result = prepare(
                    source,
                    &selected,
                    &subject,
                    action,
                    &cancel,
                    &started,
                    &intent_path,
                );
                let _ = sender.send(result);
            })?;
        Ok(Self {
            selection,
            label,
            receiver,
            cancelled,
            mutation_started,
            thread: Some(thread),
        })
    }
    pub fn poll(&self) -> Option<Result<Outcome>> {
        match self.receiver.try_recv() {
            Ok(value) => Some(value),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                Some(Err(anyhow::anyhow!("action worker ended without a result")))
            }
        }
    }
    pub fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }
    pub fn mutation_started(&self) -> bool {
        self.mutation_started.load(Ordering::Acquire)
    }
}
impl Drop for Pending {
    fn drop(&mut self) {
        self.cancel();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
fn prepare(
    source: MachineSource,
    selected: &Selection,
    subject: &Subject,
    action: Action,
    cancelled: &AtomicBool,
    mutation_started: &AtomicBool,
    intent_path: &Path,
) -> Result<Outcome> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let check = || -> Result<()> {
        ensure!(
            !cancelled.load(Ordering::Acquire),
            "preparation cancelled; no new action sent"
        );
        Ok(())
    };
    check()?;
    let control = match &source.source {
        Source::Remote { .. } => Some(
            source
                .control
                .current()?
                .context("control connection unavailable; wait for a fresh machine observation")?,
        ),
        Source::Local { .. } => None,
        Source::Unavailable { reason } => anyhow::bail!("{reason}"),
    };
    let client = match &source.source {
        Source::Local { socket } => Client::socket(socket.clone())?,
        _ => control
            .as_ref()
            .context("control helper missing")?
            .client()?,
    };
    check()?;
    let capabilities = client.capabilities(deadline)?;
    ensure!(
        capabilities.service_instance == selected.service_instance,
        "service restarted; refresh selection before acting"
    );
    let expected = match subject {
        Subject::Task(expected) => expected.as_ref(),
        Subject::Observed(handle) => {
            ensure!(
                capabilities.features.contains("observed-attachment-v1"),
                "service lacks observed-agent attachment"
            );
            if matches!(action, Action::Inspect) {
                let snapshot = client.snapshot(deadline)?;
                ensure!(
                    snapshot.service_instance == selected.service_instance && !snapshot.stale,
                    "observation changed or became stale"
                );
                let observation = snapshot
                    .snapshot
                    .observations
                    .iter()
                    .find(|observation| {
                        observation.handle == *handle
                            && observation.agent.is_some()
                            && observation.problem.is_none()
                            && observation.age_upper_bound_ms <= 5000
                    })
                    .context("observed agent is stale or replaced")?;
                check()?;
                return Ok(Outcome::Detail(format!(
                    "{} / observed agent / pane {}\n{}",
                    source.name,
                    handle.pane,
                    serde_json::to_string_pretty(observation)?
                )));
            }
            ensure!(
                matches!(action, Action::Attach),
                "task-only action unavailable for an observed agent"
            );
            return prepare_viewer(&source, deadline, cancelled, || {
                client.observed_attachment(&selected.service_instance, handle, deadline)
            });
        }
    };
    ensure!(
        capabilities.features.contains("task-read-v1"),
        "service lacks task inspection"
    );
    let inspection = client.task_inspect(&selected.service_instance, &expected.task, deadline)?;
    let actual = inspection.expected()?;
    ensure!(
        actual.task == expected.task
            && actual.attempt == expected.attempt
            && actual.session == expected.session
            && actual.target.identity() == expected.target.identity(),
        "selected task/attempt/process changed; select a current row"
    );
    check()?;
    let value = match action {
        Action::Inspect => serde_json::to_value(inspection)?,
        Action::Result => {
            let result =
                client.task_result(&selected.service_instance, &expected.task, deadline)?;
            ensure!(
                result
                    .attempt
                    .as_ref()
                    .is_some_and(|attempt| attempt.id == expected.attempt),
                "task attempt changed during result read"
            );
            serde_json::to_value(result)?
        }
        Action::Cancel | Action::Stop | Action::Reconcile => {
            ensure!(
                capabilities.features.contains("task-supervise-v1"),
                "service lacks guarded supervision"
            );
            let action = match action {
                Action::Cancel => TaskAction::Cancel,
                Action::Stop => TaskAction::Stop,
                _ => TaskAction::Reconcile,
            };
            inspection.check_action(action)?;
            check()?;
            // An error after dispatch can follow a committed operation.
            mutation_started.store(true, Ordering::Release);
            serde_json::to_value(client.task_supervise(
                &selected.service_instance,
                expected,
                action,
                deadline,
            )?)?
        }
        Action::ResumeStatus { operation } => {
            ensure!(
                capabilities.features.contains("task-resume-status-v1"),
                "service lacks resume operation inspection"
            );
            let intent = crate::machines::intents::list(intent_path)?
                .into_iter()
                .find(|intent| intent.machine == source.id && intent.operation == operation);
            if let Some(intent) = &intent {
                let endpoint = match &source.source {
                    Source::Remote { binding, .. } => binding.endpoint.clone(),
                    Source::Local { socket } => format!("local:{}", socket.display()),
                    Source::Unavailable { reason } => anyhow::bail!("{reason}"),
                };
                ensure!(
                    intent.expected.task == expected.task && intent.endpoint == endpoint,
                    "saved operation belongs to another task or control endpoint; inspect controller intents"
                );
            }
            check()?;
            let status = client.task_resume_status(
                &selected.service_instance,
                &expected.task,
                &operation,
                deadline,
            )?;
            if let (Some(intent), Some(record)) = (&intent, &status.record) {
                ensure!(
                    record.instance == intent.fux_instance,
                    "retained operation has a different fux intent"
                );
            }
            return Ok(Outcome::Detail(format!(
                "{} / {} / operation {}\nRead only; an absent record does not authorize replay.\nRemote evidence:\n{}\nSaved controller intent:\n{}",
                source.name,
                expected.task,
                operation,
                serde_json::to_string_pretty(&status)?,
                serde_json::to_string_pretty(&intent)?,
            )));
        }
        Action::Resume {
            operation,
            instance,
        } => {
            ensure!(
                capabilities.features.contains("task-resume-v1"),
                "service lacks guarded application resume"
            );
            check()?;
            let endpoint = match &source.source {
                Source::Remote { binding, .. } => binding.endpoint.clone(),
                Source::Local { socket } => format!("local:{}", socket.display()),
                Source::Unavailable { reason } => anyhow::bail!("{reason}"),
            };
            crate::machines::intents::record(
                intent_path,
                crate::machines::intents::ResumeIntent {
                    machine: source.id.clone(),
                    endpoint,
                    service_instance: selected.service_instance.clone(),
                    operation: operation.clone(),
                    fux_instance: instance.clone(),
                    expected: expected.clone(),
                },
            )?;
            check()?;
            mutation_started.store(true, Ordering::Release);
            serde_json::to_value(client.task_resume(
                &selected.service_instance,
                expected,
                &operation,
                &instance,
                deadline,
            )?)?
        }
        Action::Attach => {
            ensure!(
                capabilities.features.contains("task-attachment-v1"),
                "service lacks exact attachment resolution"
            );
            return prepare_viewer(&source, deadline, cancelled, || {
                client.task_attachment(&selected.service_instance, expected, deadline)
            });
        }
    };
    Ok(Outcome::Detail(format!(
        "{} / {} / attempt {}\n{}",
        source.name,
        expected.task,
        expected.attempt,
        serde_json::to_string_pretty(&value)?
    )))
}

fn prepare_viewer(
    source: &MachineSource,
    deadline: Instant,
    cancelled: &AtomicBool,
    mut resolve: impl FnMut() -> Result<Target>,
) -> Result<Outcome> {
    ensure!(!cancelled.load(Ordering::Acquire), "preparation cancelled");
    let target = resolve()?;
    let gateway = match &source.source {
        Source::Local { .. } => None,
        Source::Remote { koh_binary, .. } => {
            let binding = source.attachments.get(&target.workspace).with_context(|| {
                format!(
                    "{} has no attachment binding for workspace {}; use machine bind",
                    source.name, target.workspace
                )
            })?;
            Some(Gateway::start(binding, koh_binary, deadline)?)
        }
        Source::Unavailable { reason } => anyhow::bail!("{reason}"),
    };
    ensure!(!cancelled.load(Ordering::Acquire), "preparation cancelled");
    ensure!(
        resolve()? == target,
        "pane route changed while opening attachment; refresh and attach again"
    );
    let socket = gateway.as_ref().map_or_else(
        || {
            target
                .runtime
                .join(format!("{}.attach.sock", target.workspace))
        },
        Gateway::socket,
    );
    Ok(Outcome::Viewer(Box::new(PreparedViewer {
        target,
        socket,
        gateway,
    })))
}

pub(super) struct PreparedViewer {
    target: Target,
    socket: PathBuf,
    gateway: Option<Gateway>,
}
impl PreparedViewer {
    /// Parent stops reading its terminal. The viewer shares the foreground terminal group;
    /// cleanup owns only its direct child PID, never the controller's process group.
    pub fn run(mut self, binary: &Path, stop: &AtomicBool) -> Result<()> {
        let _terminal = TerminalRestore::capture()?;
        let pid = self.target.pid.context("attachment PID missing")?;
        let child = Command::new(binary)
            .arg("attach")
            .arg("--report-exit")
            .arg("--socket")
            .arg(&self.socket)
            .arg("--target-instance")
            .arg(&self.target.instance)
            .arg("--target-workspace")
            .arg(&self.target.workspace)
            .arg("--target-stream")
            .arg(self.target.stream.to_string())
            .arg("--target-pane")
            .arg(self.target.pane.to_string())
            .arg("--target-pid")
            .arg(pid.to_string())
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::piped())
            .spawn()
            .context("start fux viewer")?;
        let mut child = crate::platform::process::Running::foreground(child);
        let mut stderr = child
            .child_mut()?
            .stderr
            .take()
            .context("viewer diagnostics missing")?;
        nix::fcntl::fcntl(
            stderr.as_raw_fd(),
            nix::fcntl::FcntlArg::F_SETFL(nix::fcntl::OFlag::O_NONBLOCK),
        )?;
        let mut diagnostic = Vec::new();
        let mut transport_failure: Option<(Instant, anyhow::Error)> = None;
        loop {
            drain_viewer(&mut stderr, &mut diagnostic)?;
            if let Some(status) = child.try_wait()? {
                drain_viewer(&mut stderr, &mut diagnostic)?;
                // Expiry closes the proxy and may surface as a viewer EOF error.
                // A signal remains a viewer failure; never mask SIGKILL with transport state.
                if !status.success()
                    && status.signal().is_none()
                    && let Some(gateway) = &mut self.gateway
                {
                    let deadline = Instant::now() + Duration::from_millis(200);
                    loop {
                        gateway.poll()?;
                        if gateway
                            .latest()
                            .is_some_and(|report| matches!(report.state, State::SessionExpired))
                        {
                            attachment_transport(State::SessionExpired)?;
                        }
                        if Instant::now() >= deadline {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(20));
                    }
                }
                // Without koh status reporting a refused or lost attachment connection
                // surfaces only as the viewer's own stream error; say so instead of
                // letting fux's generic server hint stand alone.
                let transport_note = match &self.gateway {
                    Some(gateway) if !gateway.reports_status() => format!(
                        "; {}: the attachment gateway may have refused authorization or lost its connection; verify the workspace grant",
                        gateway.transport_description()
                    ),
                    _ => String::new(),
                };
                ensure!(
                    status.success(),
                    "viewer ended ({status}): {}{transport_note}",
                    String::from_utf8_lossy(&diagnostic).trim_end()
                );
                if viewer_detached(&diagnostic) {
                    return Ok(());
                }
                if let Some((_, error)) = transport_failure.take() {
                    return Err(error);
                }
                // A clean viewer EOF alone is not evidence of a user detach: koh
                // may have closed the stream after expiry. Consult its last report.
                if let Some(gateway) = &mut self.gateway {
                    gateway.poll()?;
                    if let Some(status) = gateway.latest() {
                        attachment_transport(status.state)?;
                    }
                    ensure!(
                        gateway.reports_status(),
                        "attachment ended without a detach request; {}; select Attach to connect again",
                        gateway.transport_description()
                    );
                }
                return Ok(());
            }
            if let Some(gateway) = &mut self.gateway {
                let state = gateway
                    .poll()
                    .context("attachment transport failed; select Attach to connect again")
                    .and_then(|_| {
                        gateway
                            .latest()
                            .map_or(Ok(()), |status| attachment_transport(status.state))
                    });
                if let Err(error) = state {
                    transport_failure.get_or_insert((Instant::now(), error));
                }
            }
            // A local viewer close can race koh's final socket write. Allow only a
            // short exit-report drain; this never reconnects or sends application input.
            if transport_failure
                .as_ref()
                .is_some_and(|(at, _)| at.elapsed() >= Duration::from_millis(200))
            {
                return Err(transport_failure
                    .take()
                    .context("transport failure missing")?
                    .1);
            }
            if stop.load(Ordering::Acquire) {
                child.stop(Instant::now() + Duration::from_secs(2))?;
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

fn drain_viewer(reader: &mut impl Read, diagnostic: &mut Vec<u8>) -> Result<()> {
    let mut bytes = [0; 2048];
    loop {
        match reader.read(&mut bytes) {
            Ok(0) => return Ok(()),
            Ok(count) => {
                ensure!(diagnostic.len() + count <= 16384, "viewer diagnostic limit");
                diagnostic.extend_from_slice(bytes.get(..count).context("diagnostic count")?);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        }
    }
}
fn viewer_detached(diagnostic: &[u8]) -> bool {
    diagnostic
        .split(|byte| *byte == b'\n')
        .rfind(|line| !line.is_empty())
        .and_then(|line| serde_json::from_slice::<serde_json::Value>(line).ok())
        .is_some_and(|value| {
            value == serde_json::json!({"fux_attach_exit":1,"code":null,"detached":true})
        })
}

fn attachment_transport(state: State) -> Result<()> {
    match state {
        State::SessionExpired => {
            anyhow::bail!("attachment session expired; select Attach for a fresh connection")
        }
        State::Unauthorized => {
            anyhow::bail!("attachment authorization refused; verify the workspace grant")
        }
        State::Rejected => {
            anyhow::bail!("attachment connection rejected; verify the workspace binding")
        }
        State::Unavailable | State::Failed => {
            anyhow::bail!("attachment transport unavailable; select Attach to connect again")
        }
        State::Ready
        | State::Connecting
        | State::Connected
        | State::Reconnecting
        | State::Closed
        | State::SessionEnded => Ok(()),
    }
}

struct TerminalRestore {
    input: std::fs::File,
    output: std::fs::File,
    attributes: nix::sys::termios::Termios,
    flags: nix::fcntl::OFlag,
}
impl TerminalRestore {
    fn capture() -> Result<Self> {
        let input = std::fs::File::from(std::io::stdin().as_fd().try_clone_to_owned()?);
        let output = std::fs::File::from(std::io::stdout().as_fd().try_clone_to_owned()?);
        let attributes = nix::sys::termios::tcgetattr(&input)?;
        let flags = nix::fcntl::OFlag::from_bits_truncate(nix::fcntl::fcntl(
            output.as_raw_fd(),
            nix::fcntl::FcntlArg::F_GETFL,
        )?);
        Ok(Self {
            input,
            output,
            attributes,
            flags,
        })
    }
}
impl Drop for TerminalRestore {
    fn drop(&mut self) {
        let _ = nix::sys::termios::tcsetattr(
            &self.input,
            nix::sys::termios::SetArg::TCSANOW,
            &self.attributes,
        );
        let _ = nix::fcntl::fcntl(
            self.output.as_raw_fd(),
            nix::fcntl::FcntlArg::F_SETFL(self.flags | nix::fcntl::OFlag::O_NONBLOCK),
        );
        // A killed viewer cannot disable its terminal reporting modes. Restore the
        // dashboard's plain-input contract as well as leaving its alternate screen.
        let _ = nix::unistd::write(
            &self.output,
            b"\x1b[?2026l\x1b[?9l\x1b[?2004l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1005l\x1b[?1006l\x1b[?1l\x1b>\x1b[0m\x1b[?25h\x1b[?1049l",
        );
        let _ = nix::fcntl::fcntl(
            self.output.as_raw_fd(),
            nix::fcntl::FcntlArg::F_SETFL(self.flags),
        );
    }
}

pub(super) fn flush_input() -> Result<()> {
    nix::sys::termios::tcflush(
        std::io::stdin().as_fd(),
        nix::sys::termios::FlushArg::TCIFLUSH,
    )?;
    Ok(())
}
pub(super) struct Signals(Vec<signal_hook::SigId>);
impl Signals {
    pub fn new(stop: &Arc<AtomicBool>) -> Result<Self> {
        let mut signals = Self(Vec::new());
        for signal in [
            signal_hook::consts::SIGINT,
            signal_hook::consts::SIGTERM,
            signal_hook::consts::SIGHUP,
        ] {
            signals
                .0
                .push(signal_hook::flag::register(signal, Arc::clone(stop))?);
        }
        Ok(signals)
    }
}
impl Drop for Signals {
    fn drop(&mut self) {
        for id in &self.0 {
            signal_hook::low_level::unregister(*id);
        }
    }
}

#[cfg(test)]
mod transport_tests {
    use super::*;

    #[test]
    fn only_explicit_final_detach_report_authorizes_clean_return() {
        let valid = br#"{"fux_attach_exit":1,"code":null,"detached":true}"#;
        assert!(viewer_detached(valid));
        for invalid in [
            &b""[..],
            &br#"{"fux_attach_exit":1,"code":null,"detached":false}"#[..],
            &br#"{"fux_attach_exit":1,"code":1,"detached":true}"#[..],
            &br#"{"fux_attach_exit":2,"code":null,"detached":true}"#[..],
            &br#"{"fux_attach_exit":1,"code":null,"detached":true,"extra":0}"#[..],
        ] {
            assert!(!viewer_detached(invalid));
        }
        let mut superseded = valid.to_vec();
        superseded.extend_from_slice(b"\nlater diagnostic\n");
        assert!(!viewer_detached(&superseded));
    }

    #[test]
    fn transient_resume_remains_with_koh_but_terminal_failures_require_new_action() {
        for state in [
            State::Ready,
            State::Connecting,
            State::Connected,
            State::Reconnecting,
            State::Closed,
            State::SessionEnded,
        ] {
            assert!(attachment_transport(state).is_ok());
        }
        for state in [
            State::SessionExpired,
            State::Unauthorized,
            State::Rejected,
            State::Unavailable,
            State::Failed,
        ] {
            assert!(attachment_transport(state).is_err());
        }
        assert!(
            attachment_transport(State::SessionExpired)
                .err()
                .is_some_and(|error| error
                    .to_string()
                    .contains("select Attach for a fresh connection"))
        );
    }
}
