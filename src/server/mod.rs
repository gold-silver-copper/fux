//! The session server: one ECS `Session`, one event-driven owner loop, and the adapters that
//! connect it to PTYs and Unix sockets. Idle means asleep: the loop wakes only for inbound events,
//! spawn completions, or a deadline the ECS asked for.

pub mod adapter;
pub mod connections;

use crate::config::Config;
use crate::daemon::{DaemonPaths, ManagerIdentity, ManagerLock};
use crate::ecs::{Effect, Inbound, ManagerAction, ManagerOutcome, Session};
use crate::proto::socket::bind_local_socket;
use adapter::{Adapter, OpenWorkspace};
use connections::Owner;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::{Duration, Instant};
use tokio::sync::{Notify, mpsc, oneshot};

/// Per-step budgets: how many queued items each source may contribute before the step runs.
const PANE_BUDGET: usize = 64;
const INGRESS_BUDGET: usize = 256;
/// Shutdown waits this long for panes to exit before the process ends anyway.
const SHUTDOWN_DEADLINE: Duration = Duration::from_secs(5);
/// A batch that is only pane output, arriving this soon after the previous output step and
/// not this soon after any viewer input (an echo), is a stream: the loop waits `STREAM_WAIT` for
/// more chunks before stepping, so a burst of tiny pty reads costs hundreds of steps instead of
/// thousands. Input, requests and echoes never wait.
const STREAM_GAP: Duration = Duration::from_millis(3);
const STREAM_WAIT: Duration = Duration::from_millis(1);

pub struct ServeOptions {
    pub name: String,
    pub daemon: bool,
    pub startup_channel: Option<std::path::PathBuf>,
    /// The client-side startup lock; released as soon as the manager election is decided.
    pub startup_lock: Option<crate::daemon::StartupLock>,
}

/// Runs the session server until every workspace has retired or a shutdown signal arrives.
pub async fn run(
    config: Config,
    paths: DaemonPaths,
    mut options: ServeOptions,
) -> anyhow::Result<()> {
    let startup_lock = options.startup_lock.take();
    let report = |error: Option<&str>| {
        if let Some(channel) = &options.startup_channel {
            let _ = crate::daemon::report_startup(channel, error);
        }
    };
    let result = start(config, paths, &options, startup_lock).await;
    match result {
        Ok(state) => {
            report(None);
            if !options.daemon {
                eprintln!(
                    "fux: serving {} at {}",
                    options.name,
                    state
                        .adapter
                        .paths
                        .attach_socket(&options.name)
                        .map(|path| path.display().to_string())
                        .unwrap_or_default()
                );
            }
            run_loop(state).await
        }
        Err(error) => {
            report(Some(&format!("{error:#}")));
            Err(error)
        }
    }
}

struct ServerState {
    session: Session,
    adapter: Adapter,
    owner: Owner,
    pane_rx: mpsc::Receiver<Inbound>,
    ingress_rx: mpsc::Receiver<Inbound>,
    control_reply_rx: mpsc::Receiver<(u64, oneshot::Sender<crate::proto::control::Reply>)>,
    manager_reply_rx: mpsc::Receiver<(u64, oneshot::Sender<ManagerOutcome>)>,
    outbox_rx: mpsc::Receiver<(crate::ids::ViewerId, adapter::ViewerOutbox)>,
    started: Instant,
    /// When a step last fed pane output and when one last carried viewer input, for stream
    /// detection.
    last_output: Option<Instant>,
    last_input: Option<Instant>,
    _manager_lock: ManagerLock,
    manager_stop: Arc<Notify>,
    manager_task: tokio::task::JoinHandle<()>,
    listeners: std::collections::BTreeMap<String, Vec<tokio::task::JoinHandle<()>>>,
}

async fn start(
    config: Config,
    paths: DaemonPaths,
    options: &ServeOptions,
    startup_lock: Option<crate::daemon::StartupLock>,
) -> anyhow::Result<ServerState> {
    paths.prepare()?;
    let manager_lock = ManagerLock::bind(&paths)?;
    // The election is decided by the manager socket; other launches may proceed to connect.
    drop(startup_lock);
    let identity = ManagerIdentity::current()?;
    crate::daemon::recover_stale_descriptors(&paths, &identity)?;
    let (pane_tx, pane_rx) = mpsc::channel(256);
    let (ingress_tx, ingress_rx) = mpsc::channel(1024);
    let (control_reply_tx, control_reply_rx) = mpsc::channel(256);
    let (manager_reply_tx, manager_reply_rx) = mpsc::channel(64);
    let (outbox_tx, outbox_rx) = mpsc::channel(64);
    let session = Session::new(&config)?;
    let adapter = Adapter::new(paths.clone(), identity, pane_tx);
    let owner = Owner {
        inbound: ingress_tx,
        tokens: Arc::new(AtomicU64::new(1)),
        control_replies: control_reply_tx,
        manager_replies: manager_reply_tx,
        viewer_outboxes: outbox_tx,
        viewer_ids: Arc::new(AtomicU64::new(1)),
    };
    let descriptors: connections::DescriptorHook = {
        let paths = paths.clone();
        let identity = adapter.identity.clone();
        Arc::new(move |name: &str| adapter::descriptor(&paths, &identity, name).ok())
    };
    connections::DESCRIPTOR_HOOK.install(descriptors);
    manager_lock.listener().set_nonblocking(true)?;
    let manager_listener =
        tokio::net::UnixListener::from_std(manager_lock.listener().try_clone()?)?;
    let manager_stop = Arc::new(Notify::new());
    let manager_task = tokio::spawn(connections::serve_manager(
        manager_listener,
        owner.clone(),
        Arc::clone(&manager_stop),
    ));
    let mut state = ServerState {
        session,
        adapter,
        owner,
        pane_rx,
        ingress_rx,
        control_reply_rx,
        manager_reply_rx,
        outbox_rx,
        started: Instant::now(),
        last_output: None,
        last_input: None,
        _manager_lock: manager_lock,
        manager_stop,
        manager_task,
        listeners: std::collections::BTreeMap::new(),
    };
    // Create the initial workspace synchronously: the server is ready only once its first pane
    // is live and its sockets are bound, so a viewer never attaches to an empty workspace.
    let (sender, receiver) = oneshot::channel();
    let token = 0;
    state.adapter.manager_replies.insert(token, sender);
    let mut pending = vec![Inbound::Manager {
        action: ManagerAction::Resolve {
            name: Some(options.name.clone()),
        },
        token,
    }];
    let deadline = Instant::now() + crate::daemon::STARTUP_TIMEOUT;
    let mut receiver = receiver;
    loop {
        state.run_step(std::mem::take(&mut pending)).await?;
        match receiver.try_recv() {
            Ok(ManagerOutcome::Attach { .. }) => break,
            Ok(ManagerOutcome::Failed(message)) => anyhow::bail!("initial workspace: {message}"),
            Ok(ManagerOutcome::Names(_)) => anyhow::bail!("unexpected manager outcome"),
            Err(oneshot::error::TryRecvError::Closed) => {
                anyhow::bail!("initial workspace creation was abandoned")
            }
            Err(oneshot::error::TryRecvError::Empty) => {}
        }
        anyhow::ensure!(
            Instant::now() < deadline,
            "initial pane did not start in time"
        );
        state
            .wait_for_activity(Some(deadline), &mut pending, std::future::pending())
            .await;
    }
    Ok(state)
}

impl ServerState {
    fn now_ms(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// Collects a bounded, fair batch from every source without blocking.
    fn collect(&mut self, into: &mut Vec<Inbound>) -> bool {
        let mut more = false;
        while let Ok((token, sender)) = self.control_reply_rx.try_recv() {
            self.adapter.control_replies.insert(token, sender);
        }
        while let Ok((token, sender)) = self.manager_reply_rx.try_recv() {
            self.adapter.manager_replies.insert(token, sender);
        }
        while let Ok((viewer, outbox)) = self.outbox_rx.try_recv() {
            self.adapter.viewers.insert(viewer, outbox);
        }
        while let Some(result) = self.adapter.spawns.try_join_next() {
            match result {
                Ok((pane, result)) => into.push(self.adapter.record_spawn(pane, result)),
                Err(error) => tracing::error!(%error, "spawn task failed"),
            }
        }
        while let Some(result) = self.adapter.blocking.try_join_next() {
            if let Err(error) = result {
                tracing::error!(%error, "blocking task failed");
            }
        }
        for _ in 0..INGRESS_BUDGET {
            match self.ingress_rx.try_recv() {
                Ok(event) => into.push(event),
                Err(_) => break,
            }
        }
        more |= !self.ingress_rx.is_empty();
        for _ in 0..PANE_BUDGET {
            match self.pane_rx.try_recv() {
                Ok(event) => into.push(event),
                Err(_) => break,
            }
        }
        more |= !self.pane_rx.is_empty();
        more
    }

    /// Whether `inbound` is the continuation of an output stream worth waiting on.
    fn streaming(&self, inbound: &[Inbound]) -> bool {
        !inbound.is_empty()
            && inbound
                .iter()
                .all(|message| matches!(message, Inbound::PaneOutput { .. }))
            && self
                .last_output
                .is_some_and(|last| last.elapsed() < STREAM_GAP)
            && self
                .last_input
                .is_none_or(|last| last.elapsed() >= STREAM_GAP)
    }

    async fn run_step(&mut self, inbound: Vec<Inbound>) -> anyhow::Result<()> {
        let now = Instant::now();
        if inbound
            .iter()
            .any(|message| matches!(message, Inbound::PaneOutput { .. }))
        {
            self.last_output = Some(now);
        }
        if inbound
            .iter()
            .any(|message| matches!(message, Inbound::ViewerRequest { .. }))
        {
            self.last_input = Some(now);
        }
        let effects: Vec<Effect> = self.session.step(self.now_ms(), inbound);
        self.adapter.apply(effects);
        for name in std::mem::take(&mut self.adapter.opened) {
            self.open_workspace(&name).await?;
        }
        for name in std::mem::take(&mut self.adapter.closed) {
            self.close_workspace(&name).await;
        }
        Ok(())
    }

    async fn open_workspace(&mut self, name: &str) -> anyhow::Result<()> {
        let attach_path = self.adapter.paths.attach_socket(name)?;
        let control_path = self.adapter.paths.control_socket(name)?;
        let attach = bind_local_socket(&attach_path)?;
        let control = bind_local_socket(&control_path)?;
        attach.listener().set_nonblocking(true)?;
        control.listener().set_nonblocking(true)?;
        let attach_listener = tokio::net::UnixListener::from_std(attach.listener().try_clone()?)?;
        let control_listener = tokio::net::UnixListener::from_std(control.listener().try_clone()?)?;
        let stop = Arc::new(Notify::new());
        let subscribers = Arc::new(std::sync::Mutex::new(Vec::new()));
        let descriptor = self.adapter.descriptor_for(name)?;
        self.adapter.register_workspace(OpenWorkspace {
            name: name.to_owned(),
            descriptor,
            stop: Arc::clone(&stop),
            subscribers: Arc::clone(&subscribers),
        })?;
        let attach_task = {
            let stop = Arc::clone(&stop);
            let owner = self.owner.clone();
            let name = name.to_owned();
            tokio::spawn(async move {
                let _bound = attach;
                connections::serve_attachments(attach_listener, name, owner, stop).await;
            })
        };
        let control_task = {
            let stop = Arc::clone(&stop);
            let owner = self.owner.clone();
            let name = name.to_owned();
            tokio::spawn(async move {
                let _bound = control;
                connections::serve_control(control_listener, name, owner, subscribers, stop).await;
            })
        };
        self.listeners
            .insert(name.to_owned(), vec![attach_task, control_task]);
        Ok(())
    }

    async fn close_workspace(&mut self, name: &str) {
        self.adapter.unregister_workspace(name);
        if let Some(tasks) = self.listeners.remove(name) {
            for task in tasks {
                let _ = tokio::time::timeout(Duration::from_secs(1), task).await;
            }
        }
    }

    /// Sleeps until `shutdown` resolves (returns true), an event, a spawn completion, a viewer
    /// registration or the deadline. Whatever woke the loop is kept in `into` for the next step.
    async fn wait_for_activity(
        &mut self,
        deadline: Option<Instant>,
        into: &mut Vec<Inbound>,
        shutdown: impl Future<Output = ()>,
    ) -> bool {
        let sleep = async {
            match deadline {
                Some(deadline) => tokio::time::sleep_until(deadline.into()).await,
                None => std::future::pending::<()>().await,
            }
        };
        tokio::select! {
            biased;
            () = shutdown => return true,
            event = self.ingress_rx.recv() => { if let Some(event) = event { into.push(event); } }
            event = self.pane_rx.recv() => { if let Some(event) = event { into.push(event); } }
            Some((viewer, outbox)) = self.outbox_rx.recv() => { self.adapter.viewers.insert(viewer, outbox); }
            Some((token, sender)) = self.control_reply_rx.recv() => { self.adapter.control_replies.insert(token, sender); }
            Some((token, sender)) = self.manager_reply_rx.recv() => { self.adapter.manager_replies.insert(token, sender); }
            Some(result) = self.adapter.spawns.join_next(), if !self.adapter.spawns.is_empty() => {
                if let Ok((pane, result)) = result { into.push(self.adapter.record_spawn(pane, result)); }
            }
            () = sleep => {}
        }
        false
    }
}

async fn run_loop(mut state: ServerState) -> anyhow::Result<()> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut terminate = signal(SignalKind::terminate())?;
    let mut shutting_down: Option<Instant> = None;
    let mut inbound = Vec::new();
    loop {
        // `wait_for_activity` may have consumed one item from a receiver; keep it.
        let mut more = state.collect(&mut inbound);
        if state.streaming(&inbound) {
            tokio::time::sleep(STREAM_WAIT).await;
            more = state.collect(&mut inbound);
        }
        state.run_step(std::mem::take(&mut inbound)).await?;
        if state.adapter.idle {
            break;
        }
        if let Some(since) = shutting_down
            && since.elapsed() >= SHUTDOWN_DEADLINE
        {
            tracing::warn!("shutdown deadline reached with panes still running");
            break;
        }
        if more {
            // A hot pane must not starve signal delivery: poll the signals without sleeping.
            if poll_signal(&mut interrupt) || poll_signal(&mut terminate) {
                request_shutdown(&mut inbound, &mut shutting_down);
            }
            continue;
        }
        let deadline = state
            .session
            .next_deadline_ms()
            .map(|at| state.started + Duration::from_millis(at))
            .into_iter()
            .chain(shutting_down.map(|since| since + SHUTDOWN_DEADLINE))
            .min();
        let signalled = async {
            tokio::select! {
                _ = interrupt.recv() => {}
                _ = terminate.recv() => {}
            }
        };
        if state
            .wait_for_activity(deadline, &mut inbound, signalled)
            .await
        {
            request_shutdown(&mut inbound, &mut shutting_down);
        }
    }
    state.manager_stop.notify_one();
    let _ = tokio::time::timeout(Duration::from_secs(1), &mut state.manager_task).await;
    let names: Vec<String> = state.listeners.keys().cloned().collect();
    for name in names {
        state.close_workspace(&name).await;
    }
    state.adapter.shutdown().await;
    Ok(())
}

/// Non-blocking check for a delivered signal.
fn poll_signal(signal: &mut tokio::signal::unix::Signal) -> bool {
    let waker = std::task::Waker::noop();
    let mut context = std::task::Context::from_waker(waker);
    matches!(
        signal.poll_recv(&mut context),
        std::task::Poll::Ready(Some(()))
    )
}

fn request_shutdown(inbound: &mut Vec<Inbound>, shutting_down: &mut Option<Instant>) {
    if shutting_down.is_none() {
        *shutting_down = Some(Instant::now());
        inbound.push(Inbound::Shutdown);
    }
}
