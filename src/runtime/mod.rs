//! Process-level wiring for configuration, control sockets, hooks, and CLI clients.

use anyhow::{Context, Result, anyhow, bail};
use fux::config::{Config, Hook};
use fux::control::{
    Event, EventKind, EventQueue, Reply, Request, error_reply, read_request, write_frame,
};
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub const FINAL_STATE_MIN_GRACE: Duration = Duration::from_millis(250);
pub const FINAL_STATE_MAX_GRACE: Duration = Duration::from_secs(5);

pub fn terminal_workspace_retirement_due(
    empty_since: Instant,
    attached_clients: usize,
    now: Instant,
) -> bool {
    let elapsed = now.saturating_duration_since(empty_since);
    elapsed >= FINAL_STATE_MIN_GRACE && (attached_clients == 0 || elapsed >= FINAL_STATE_MAX_GRACE)
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(tag = "request", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ManagerRequest {
    Resolve {
        name: Option<String>,
        server_key: Option<Vec<u8>>,
    },
    List,
    Kill {
        name: String,
    },
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(tag = "reply", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ManagerReply {
    Attach { descriptor: fux::daemon::Descriptor },
    Pick { names: Vec<String> },
    Failed { message: String },
}

pub fn manager_request(path: &Path, request: &ManagerRequest) -> Result<ManagerReply> {
    let mut stream = UnixStream::connect(path)?;
    set_rpc_deadlines(&stream)?;
    write_frame(&mut stream, request)?;
    serde_json::from_slice(&read_json_frame(&mut stream)?).context("decoding manager reply")
}

pub fn select_workspace(names: &[String], selection: &str) -> Result<String> {
    let index = selection.trim().parse::<usize>()?;
    names
        .get(index.saturating_sub(1))
        .cloned()
        .ok_or_else(|| anyhow!("workspace selection is out of range"))
}

pub fn read_manager_request(stream: &mut UnixStream) -> Result<ManagerRequest> {
    serde_json::from_slice(&read_json_frame(stream)?).context("decoding manager request")
}

pub fn write_manager_reply(stream: &mut UnixStream, reply: &ManagerReply) -> Result<()> {
    write_frame(stream, reply)?;
    Ok(())
}

pub trait ControlHandler: Send + Sync + 'static {
    fn handle(&self, request: Request) -> Reply;
}

pub struct WorkspaceControlHandler {
    control: fux::host::WorkspaceControl,
    events: EventHub,
    name: String,
}

impl WorkspaceControlHandler {
    pub fn new(control: fux::host::WorkspaceControl, events: EventHub, name: String) -> Self {
        Self {
            control,
            events,
            name,
        }
    }
}

impl ControlHandler for WorkspaceControlHandler {
    fn handle(&self, request: Request) -> Reply {
        if let Request::List { id } = &request {
            return Reply::Completed {
                id: *id,
                result: fux::control::CommandResult::Listing {
                    workspaces: vec![self.control.summary(self.name.clone())],
                },
            };
        }
        let (reply, events) = self.control.dispatch(request);
        for event in events {
            // Pane-open is published by the host before its PTY drain starts, preserving causal
            // order for immediate-output children.
            if !matches!(event, Event::PaneOpened { .. }) {
                self.events.publish(event);
            }
        }
        reply
    }
}

#[derive(Clone, Default)]
pub struct EventHub {
    subscribers: Arc<Mutex<Vec<Subscriber>>>,
    next_subscriber: Arc<AtomicU64>,
    notifications: Option<Arc<NotificationRuntime>>,
}

struct NotificationRuntime {
    gate: Mutex<NotificationGate>,
    policy: RwLock<fux::config::NotificationPolicy>,
    stop: Arc<AtomicBool>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl Drop for NotificationRuntime {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        for task in self
            .tasks
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .drain(..)
        {
            let _ = task.join();
        }
    }
}

struct Subscriber {
    registration: u64,
    id: u64,
    filters: Vec<EventKind>,
    queue: EventQueue,
}

impl EventHub {
    pub fn same_instance(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.subscribers, &other.subscribers)
    }

    pub fn with_notifications(policy: fux::config::NotificationPolicy) -> Self {
        let notifications = std::env::var_os("TERMUX_VERSION").is_none().then(|| {
            Arc::new(NotificationRuntime {
                gate: Mutex::new(NotificationGate::default()),
                policy: RwLock::new(policy),
                stop: Arc::new(AtomicBool::new(false)),
                tasks: Mutex::new(Vec::new()),
            })
        });
        Self {
            subscribers: Arc::default(),
            next_subscriber: Arc::default(),
            notifications,
        }
    }

    pub fn publish(&self, event: Event) {
        if let (Some(runtime), Event::PaneClosed { pane, .. }) = (&self.notifications, &event) {
            lock(&runtime.gate).remove(*pane);
        }
        if let (
            Some(runtime),
            Event::AgentState {
                pane,
                new_state,
                agent,
                ..
            },
        ) = (&self.notifications, &event)
            && lock(&runtime.gate).observe(*pane, *new_state, &read_lock(&runtime.policy))
        {
            let message = agent.as_deref().unwrap_or("agent state changed");
            let termux = std::env::var_os("TERMUX_VERSION").is_some();
            let display = std::env::var_os("DISPLAY").is_some()
                || std::env::var_os("WAYLAND_DISPLAY").is_some();
            if let Some(argv) = notification_command(
                "fux",
                message,
                display,
                termux,
                cfg!(target_os = "macos"),
                executable_on_path,
            ) && let Some((program, arguments)) = argv.split_first()
                && let Ok(child) = Command::new(program)
                    .args(arguments)
                    .process_group(0)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
            {
                let stop = Arc::clone(&runtime.stop);
                let task = std::thread::spawn(move || reap_notification(child, &stop));
                let mut tasks = lock(&runtime.tasks);
                let mut active = Vec::with_capacity(tasks.len().saturating_add(1));
                for finished in tasks.drain(..) {
                    if finished.is_finished() {
                        let _ = finished.join();
                    } else {
                        active.push(finished);
                    }
                }
                if active.len() < 16 {
                    active.push(task);
                } else {
                    // Dropping the handle would detach a live notifier. Signal cancellation and
                    // synchronously join this bounded overflow instead.
                    runtime.stop.store(true, Ordering::Release);
                    let _ = task.join();
                    runtime.stop.store(false, Ordering::Release);
                }
                *tasks = active;
            }
        }
        let kind = event.kind();
        let mut subscribers = lock(&self.subscribers);
        subscribers.retain(|subscriber| {
            if subscriber.filters.is_empty() || subscriber.filters.contains(&kind) {
                !matches!(
                    subscriber
                        .queue
                        .publish(with_event_id(event.clone(), subscriber.id)),
                    fux::control::PublishOutcome::Disconnected
                        | fux::control::PublishOutcome::DisconnectedSlowClient
                )
            } else {
                true
            }
        });
    }

    pub fn update_notification_policy(&self, policy: fux::config::NotificationPolicy) {
        if let Some(runtime) = &self.notifications {
            *write_lock(&runtime.policy) = policy;
        }
    }

    pub fn subscriber_count(&self) -> usize {
        lock(&self.subscribers).len()
    }
}

pub fn reap_notification(mut child: Child, stop: &AtomicBool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !stop.load(Ordering::Acquire) && Instant::now() < deadline {
        if child.try_wait().ok().flatten().is_some() {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    if let Ok(pid) = i32::try_from(child.id()) {
        let _ = nix::sys::signal::killpg(
            nix::unistd::Pid::from_raw(pid),
            nix::sys::signal::Signal::SIGKILL,
        );
    }
    let _ = child.wait();
}

fn executable_on_path(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|directory| directory.join(program).is_file())
    })
}

impl fux::host::WorkspaceEventSink for EventHub {
    fn publish(&self, event: Event) {
        Self::publish(self, event);
    }
}

/// Serves validated newline-delimited control frames until `shutdown` is set.
pub fn serve_control(
    socket: fux::control::BoundControlSocket,
    handler: Arc<dyn ControlHandler>,
    events: EventHub,
    shutdown: Arc<AtomicBool>,
) -> std::io::Result<JoinHandle<()>> {
    socket.listener().set_nonblocking(true)?;
    Ok(std::thread::spawn(move || {
        const MAX_CONNECTIONS: usize = 64;
        let active = Arc::new(AtomicUsize::new(0));
        let mut connections: Vec<JoinHandle<()>> = Vec::new();
        while !shutdown.load(Ordering::Acquire) {
            let mut live = Vec::with_capacity(connections.len());
            for connection in connections.drain(..) {
                if connection.is_finished() {
                    let _ = connection.join();
                } else {
                    live.push(connection);
                }
            }
            connections = live;
            match socket.accept() {
                Ok(stream) => {
                    if active
                        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                            (count < MAX_CONNECTIONS).then_some(count + 1)
                        })
                        .is_err()
                    {
                        drop(stream);
                        continue;
                    }
                    if stream.set_nonblocking(false).is_err() {
                        active.fetch_sub(1, Ordering::AcqRel);
                        continue;
                    }
                    let _ = stream.set_read_timeout(Some(Duration::from_millis(250)));
                    let _ = stream.set_write_timeout(Some(Duration::from_millis(250)));
                    let handler = Arc::clone(&handler);
                    let events = events.clone();
                    let connection_shutdown = Arc::clone(&shutdown);
                    let connection_active = Arc::clone(&active);
                    connections.push(std::thread::spawn(move || {
                        serve_connection(stream, handler, events, &connection_shutdown);
                        connection_active.fetch_sub(1, Ordering::AcqRel);
                    }));
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(_) => break,
            }
        }
        for connection in connections {
            let _ = connection.join();
        }
    }))
}

fn serve_connection(
    mut stream: UnixStream,
    handler: Arc<dyn ControlHandler>,
    events: EventHub,
    shutdown: &AtomicBool,
) {
    loop {
        if shutdown.load(Ordering::Acquire) {
            return;
        }
        match read_request(&mut stream) {
            Ok(Some(Request::Subscribe {
                id,
                events: filters,
            })) => {
                let Some((queue, receiver)) =
                    EventQueue::bounded(fux::control::MAX_SUBSCRIBER_QUEUE)
                else {
                    return;
                };
                let registration = events.next_subscriber.fetch_add(1, Ordering::Relaxed);
                lock(&events.subscribers).push(Subscriber {
                    registration,
                    id,
                    filters,
                    queue,
                });
                if write_frame(&mut stream, &Reply::Accepted { id }).is_err() {
                    lock(&events.subscribers).retain(|entry| entry.registration != registration);
                    return;
                }
                let _ = stream.set_nonblocking(true);
                while !shutdown.load(Ordering::Acquire) {
                    if let Some(event) = receiver.recv_timeout(Duration::from_millis(250)) {
                        if write_frame(&mut stream, &event).is_err() {
                            break;
                        }
                    } else if receiver.is_disconnected() {
                        break;
                    }
                    let mut probe = [0_u8; 1];
                    match std::io::Read::read(&mut stream, &mut probe) {
                        Ok(0) => break,
                        Ok(_) => break,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                        Err(_) => break,
                    }
                }
                lock(&events.subscribers).retain(|entry| entry.registration != registration);
                return;
            }
            Ok(Some(request)) => {
                let id = request.id();
                let reply = bounded_reply(handler.handle(request), id);
                if write_frame(&mut stream, &reply).is_err() {
                    return;
                }
            }
            Ok(None) => return,
            Err(error) if error.code == fux::control::ErrorCode::Internal => {}
            Err(error) => {
                if write_frame(&mut stream, &error_reply(&error)).is_err() {
                    return;
                }
            }
        }
    }
}

fn bounded_reply(reply: Reply, id: u64) -> Reply {
    if serde_json::to_vec(&reply).is_ok_and(|bytes| bytes.len() <= fux::control::MAX_FRAME_BYTES) {
        reply
    } else {
        Reply::Failed {
            id,
            error: fux::control::ReplyError {
                code: fux::control::ErrorCode::FrameTooLarge,
                message: "control response exceeds the 1 MiB frame limit".into(),
            },
        }
    }
}

pub fn request(socket: &Path, request: &Request) -> Result<Reply> {
    let mut stream = UnixStream::connect(socket)
        .with_context(|| format!("connecting to {}", socket.display()))?;
    set_rpc_deadlines(&stream)?;
    write_frame(&mut stream, request)?;
    let frame = read_json_frame(&mut stream)?;
    serde_json::from_slice(&frame).context("decoding control reply")
}

pub fn subscribe(socket: &Path, request: &Request, mut output: impl Write) -> Result<()> {
    let mut stream = UnixStream::connect(socket)
        .with_context(|| format!("connecting to {}", socket.display()))?;
    set_rpc_deadlines(&stream)?;
    write_frame(&mut stream, request)?;
    let accepted = read_json_frame(&mut stream)?;
    output.write_all(&accepted)?;
    output.write_all(b"\n")?;
    stream.set_read_timeout(None)?;
    loop {
        let frame = read_json_frame(&mut stream)?;
        output.write_all(&frame)?;
        output.write_all(b"\n")?;
    }
}

fn set_rpc_deadlines(stream: &UnixStream) -> Result<()> {
    let timeout = Some(Duration::from_secs(2));
    stream.set_read_timeout(timeout)?;
    stream.set_write_timeout(timeout)?;
    Ok(())
}

fn read_json_frame(stream: &mut UnixStream) -> Result<Vec<u8>> {
    use std::io::Read as _;
    let mut frame = Vec::new();
    let mut byte = [0_u8; 1];
    while stream.read(&mut byte)? != 0 {
        if byte[0] == b'\n' {
            return Ok(frame);
        }
        if frame.len() == fux::control::MAX_FRAME_BYTES {
            bail!("control response exceeds frame limit");
        }
        frame.push(byte[0]);
    }
    if frame.is_empty() {
        bail!("control socket closed before a response");
    }
    Ok(frame)
}

pub struct LiveConfig {
    path: PathBuf,
    current: Arc<RwLock<Config>>,
}

impl LiveConfig {
    pub fn load(path: PathBuf) -> Result<Self> {
        let config = Config::load_from_path(&path)?;
        Ok(Self {
            path,
            current: Arc::new(RwLock::new(config)),
        })
    }

    pub fn snapshot(&self) -> Config {
        read_lock(&self.current).clone()
    }

    /// Transactional reload: the live value is replaced only after full parse and validation.
    pub fn reload(&self) -> Result<Config> {
        let candidate = Config::load_from_path(&self.path)?;
        if !single_binding_key(&candidate.prefix)
            || candidate
                .bindings
                .keys()
                .any(|key| !single_binding_key(key))
        {
            bail!("prefix and binding keys must encode exactly one byte at runtime");
        }
        let current = read_lock(&self.current);
        if candidate.local_network != current.local_network
            || candidate.default_command != current.default_command
            || candidate.zor_path != current.zor_path
            || candidate.clipboard != current.clipboard
            || candidate.history != current.history
            || candidate.resources != current.resources
            || candidate.remote_allow_ids != current.remote_allow_ids
        {
            bail!("configuration change requires a workspace restart");
        }
        drop(current);
        *write_lock(&self.current) = candidate.clone();
        Ok(candidate)
    }
}

fn single_binding_key(value: &str) -> bool {
    value.len() == 1
        || value
            .strip_prefix("C-")
            .is_some_and(|value| value.len() == 1 && value.is_ascii())
}

/// Reloads configuration on SIGHUP. Invalid candidates are reported and the previous value stays
/// live; cancellation provides deterministic daemon teardown.
pub async fn reload_on_sighup(
    live: Arc<LiveConfig>,
    hooks: Option<Arc<Mutex<Vec<Arc<LiveHooks>>>>>,
    event_hubs: Option<Arc<Mutex<Vec<EventHub>>>>,
    controls: Option<Arc<Mutex<Vec<fux::host::WorkspaceControl>>>>,
    shutdown: tokio_util::sync::CancellationToken,
) {
    let Ok(mut signal) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())
    else {
        return;
    };
    loop {
        tokio::select! {
            () = shutdown.cancelled() => return,
            value = signal.recv() => {
                if value.is_none() { return; }
                match live.reload() {
                    Ok(config) => {
                        if let Some(hooks) = &hooks {
                            for supervisor in lock(hooks).iter() { supervisor.reconcile(&config.hooks); }
                        }
                        if let Some(event_hubs) = &event_hubs {
                            for hub in lock(event_hubs).iter() { hub.update_notification_policy(config.notifications.clone()); }
                        }
                        if let Some(controls) = &controls {
                            for control in lock(controls).iter() {
                                if let Err(error) = control.reconfigure_bindings(&config) { tracing::error!(%error, "validated binding reload could not be applied"); }
                            }
                        }
                    }
                    Err(error) => {
                        tracing::warn!(%error, "configuration reload rejected; retaining previous configuration");
                    }
                }
            }
        }
    }
}

pub trait HookCommand: Send + Sync + 'static {
    fn spawn(
        &self,
        hook: &Hook,
        environment: &BTreeMap<OsString, OsString>,
    ) -> Result<Box<dyn HookProcess>>;
}

pub trait HookProcess: Send {
    fn exited(&mut self) -> bool;
    fn terminate(&mut self);
}

impl HookProcess for Child {
    fn exited(&mut self) -> bool {
        self.try_wait().map_or(true, |status| status.is_some())
    }
    fn terminate(&mut self) {
        if let Ok(pid) = i32::try_from(self.id()) {
            let _ = nix::sys::signal::killpg(
                nix::unistd::Pid::from_raw(pid),
                nix::sys::signal::Signal::SIGKILL,
            );
        } else {
            let _ = self.kill();
        }
        let _ = self.wait();
    }
}

pub trait HookClock: Send + Sync + 'static {
    fn now(&self) -> Duration;
    fn sleep(&self, stop: &AtomicBool, duration: Duration);
}

pub struct SystemHookClock {
    started: Instant,
}
impl Default for SystemHookClock {
    fn default() -> Self {
        Self {
            started: Instant::now(),
        }
    }
}
impl HookClock for SystemHookClock {
    fn now(&self) -> Duration {
        self.started.elapsed()
    }
    fn sleep(&self, stop: &AtomicBool, duration: Duration) {
        interruptible_sleep(stop, duration);
    }
}

pub struct ProcessHookCommand;

impl HookCommand for ProcessHookCommand {
    fn spawn(
        &self,
        hook: &Hook,
        environment: &BTreeMap<OsString, OsString>,
    ) -> Result<Box<dyn HookProcess>> {
        let (program, arguments) = hook
            .command
            .argv
            .split_first()
            .ok_or_else(|| anyhow!("empty hook"))?;
        let child = Command::new(program)
            .args(arguments)
            .process_group(0)
            .env_clear()
            .envs(environment)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("starting hook {}", hook.name))?;
        Ok(Box::new(child))
    }
}

pub struct HookSupervisor {
    stop: Arc<AtomicBool>,
    tasks: Vec<JoinHandle<()>>,
}

pub struct LiveHooks {
    current: Mutex<Option<HookSupervisor>>,
    runner: Arc<dyn HookCommand>,
    environment: Vec<(OsString, OsString)>,
}

impl LiveHooks {
    pub fn new(
        hooks: &[Hook],
        inherited: impl IntoIterator<Item = (OsString, OsString)>,
        runner: Arc<dyn HookCommand>,
    ) -> Self {
        let environment: Vec<_> = inherited.into_iter().collect();
        let current = HookSupervisor::start(hooks, environment.clone(), Arc::clone(&runner));
        Self {
            current: Mutex::new(Some(current)),
            runner,
            environment,
        }
    }

    pub fn reconcile(&self, hooks: &[Hook]) {
        let replacement =
            HookSupervisor::start(hooks, self.environment.clone(), Arc::clone(&self.runner));
        let old = lock(&self.current).replace(replacement);
        if let Some(old) = old {
            old.shutdown();
        }
    }

    pub fn shutdown(&self) {
        if let Some(current) = lock(&self.current).take() {
            current.shutdown();
        }
    }
}

impl HookSupervisor {
    pub fn start(
        hooks: &[Hook],
        inherited: impl IntoIterator<Item = (OsString, OsString)>,
        runner: Arc<dyn HookCommand>,
    ) -> Self {
        Self::start_with_clock(
            hooks,
            inherited,
            runner,
            Arc::new(SystemHookClock::default()),
        )
    }

    pub fn start_with_clock(
        hooks: &[Hook],
        inherited: impl IntoIterator<Item = (OsString, OsString)>,
        runner: Arc<dyn HookCommand>,
        clock: Arc<dyn HookClock>,
    ) -> Self {
        let environment = Arc::new(scrub_environment(inherited));
        let stop = Arc::new(AtomicBool::new(false));
        let tasks = hooks
            .iter()
            .cloned()
            .map(|hook| {
                let stop = Arc::clone(&stop);
                let runner = Arc::clone(&runner);
                let environment = Arc::clone(&environment);
                let clock = Arc::clone(&clock);
                std::thread::spawn(move || supervise_hook(hook, &environment, runner, clock, stop))
            })
            .collect();
        Self { stop, tasks }
    }

    pub fn shutdown(mut self) {
        self.stop.store(true, Ordering::Release);
        for task in self.tasks.drain(..) {
            let _ = task.join();
        }
    }
}

impl Drop for HookSupervisor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

fn supervise_hook(
    hook: Hook,
    environment: &BTreeMap<OsString, OsString>,
    runner: Arc<dyn HookCommand>,
    clock: Arc<dyn HookClock>,
    stop: Arc<AtomicBool>,
) {
    let mut backoff = Duration::from_millis(100);
    while !stop.load(Ordering::Acquire) {
        let started = clock.now();
        let mut child = match runner.spawn(&hook, environment) {
            Ok(child) => child,
            Err(_) => {
                clock.sleep(&stop, backoff);
                backoff = (backoff * 2).min(Duration::from_secs(30));
                continue;
            }
        };
        loop {
            if stop.load(Ordering::Acquire) {
                child.terminate();
                return;
            }
            if child.exited() {
                break;
            }
            clock.sleep(&stop, Duration::from_millis(25));
        }
        backoff = if clock.now().saturating_sub(started) >= Duration::from_secs(30) {
            Duration::from_millis(100)
        } else {
            (backoff * 2).min(Duration::from_secs(30))
        };
        clock.sleep(&stop, backoff);
    }
}

fn interruptible_sleep(stop: &AtomicBool, duration: Duration) {
    let deadline = Instant::now() + duration;
    while !stop.load(Ordering::Acquire) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
    }
}

pub fn scrub_environment(
    inherited: impl IntoIterator<Item = (OsString, OsString)>,
) -> BTreeMap<OsString, OsString> {
    inherited
        .into_iter()
        .filter(|(key, _)| !secret_key(key))
        .collect()
}

fn secret_key(key: &OsStr) -> bool {
    let key = key.to_string_lossy();
    key.starts_with("FUX_") || key.starts_with("KOH_")
}

#[derive(Default)]
pub struct NotificationGate {
    states: BTreeMap<u32, fux::control::AgentStatus>,
}

impl NotificationGate {
    pub fn observe(
        &mut self,
        pane: u32,
        next: fux::control::AgentStatus,
        policy: &fux::config::NotificationPolicy,
    ) -> bool {
        let previous = self.states.insert(pane, next);
        if !policy.enabled {
            return false;
        }
        match (previous, next) {
            (None, fux::control::AgentStatus::Blocked) => policy.notify_blocked,
            (Some(old), fux::control::AgentStatus::Blocked) => {
                policy.notify_blocked && old != fux::control::AgentStatus::Blocked
            }
            (
                Some(fux::control::AgentStatus::Working | fux::control::AgentStatus::Blocked),
                fux::control::AgentStatus::Idle,
            ) => policy.notify_idle,
            _ => false,
        }
    }

    pub fn remove(&mut self, pane: u32) {
        self.states.remove(&pane);
    }

    pub fn tracked_count(&self) -> usize {
        self.states.len()
    }
}

/// Selects the platform notifier without invoking a shell. `available` is injectable for tests.
pub fn notification_command(
    title: &str,
    message: &str,
    display: bool,
    termux: bool,
    macos: bool,
    available: impl Fn(&str) -> bool,
) -> Option<Vec<String>> {
    if termux && available("termux-notification") {
        return Some(vec![
            "termux-notification".into(),
            "-t".into(),
            title.into(),
            "-c".into(),
            message.into(),
        ]);
    }
    if macos && available("terminal-notifier") {
        return Some(vec![
            "terminal-notifier".into(),
            "-title".into(),
            title.into(),
            "-message".into(),
            message.into(),
        ]);
    }
    if macos && available("osascript") {
        let escaped = message.replace('"', "\\\"");
        return Some(vec![
            "osascript".into(),
            "-e".into(),
            format!("display notification \"{escaped}\" with title \"fux\""),
        ]);
    }
    if display && available("notify-send") {
        return Some(vec!["notify-send".into(), title.into(), message.into()]);
    }
    None
}

fn with_event_id(event: Event, id: u64) -> Event {
    match event {
        Event::PaneOpened { pane, command, .. } => Event::PaneOpened { id, pane, command },
        Event::PaneClosed {
            pane, exit_status, ..
        } => Event::PaneClosed {
            id,
            pane,
            exit_status,
        },
        Event::PaneFocused { pane, .. } => Event::PaneFocused { id, pane },
        Event::PaneTitle { pane, title, .. } => Event::PaneTitle { id, pane, title },
        Event::AgentState {
            pane,
            agent,
            old_state,
            new_state,
            timestamp_ms,
            ..
        } => Event::AgentState {
            id,
            pane,
            agent,
            old_state,
            new_state,
            timestamp_ms,
        },
        Event::PaneOutput { pane, .. } => Event::PaneOutput { id, pane },
        Event::WorkspaceResized { rows, cols, .. } => Event::WorkspaceResized { id, rows, cols },
        Event::ClientAttached { client, .. } => Event::ClientAttached { id, client },
        Event::ClientDetached { client, .. } => Event::ClientDetached { id, client },
    }
}

fn lock<T>(value: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    value
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
fn read_lock<T>(value: &RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    value
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
fn write_lock<T>(value: &RwLock<T>) -> std::sync::RwLockWriteGuard<'_, T> {
    value
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
