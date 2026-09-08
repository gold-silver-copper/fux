//! Zor-owned local observation service. No pane lifecycle authority is acquired by observation.
use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::{
        fd::AsFd,
        unix::{
            fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
            net::{UnixListener, UnixStream},
        },
    },
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

const MAX_CLIENTS: usize = 32;
const MAX_REQUEST: usize = 256 * 1024;
const MAX_RESPONSE: usize = 512 * 1024;
const CLIENT_TIMEOUT: Duration = Duration::from_secs(3);
const STALE_AFTER: Duration = Duration::from_secs(5);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    v: u32,
    id: u64,
    op: Operation,
    #[serde(default)]
    service_instance: Option<String>,
    #[serde(default)]
    task: Option<crate::service_tasks::Request>,
}
#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Operation {
    Snapshot,
    Ping,
    Shutdown,
    Task,
}

#[derive(Default)]
struct Published {
    sequence: u64,
    at: Option<Instant>,
    snapshot: crate::watch::Snapshot,
}
impl Published {
    fn response(&self, instance: &str, id: u64, now: Instant) -> Value {
        let age = self.at.map(|at| now.saturating_duration_since(at));
        let age_ms = age.map_or(0, millis);
        let stale = age.is_none()
            || self.snapshot.scan_duration_ms.saturating_add(age_ms) > millis(STALE_AFTER);
        let mut snapshot = self.snapshot.clone();
        for observation in &mut snapshot.observations {
            observation.age_upper_bound_ms = observation.age_upper_bound_ms.saturating_add(age_ms);
            if stale || observation.age_upper_bound_ms > millis(STALE_AFTER) {
                observation.state = "unknown".into();
                observation.rule = None;
                observation.problem = Some("observation service snapshot is stale".into());
            }
        }
        if self.at.is_none() {
            snapshot
                .problems
                .insert("service".into(), "initial scan pending".into());
        } else if stale {
            snapshot
                .problems
                .insert("service".into(), "observation scan overdue".into());
        }
        json!({"v":1,"id":id,"status":"completed","service_instance":instance,
            "sequence":self.sequence,"stale":stale,"published_age_ms":age.map(millis),
            "snapshot":snapshot})
    }
}
fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

pub fn directory() -> Result<PathBuf> {
    if let Some(root) = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
    {
        return Ok(root.join("zor"));
    }
    #[cfg(target_os = "macos")]
    if let Some(home) = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
    {
        return Ok(home.join("Library/Caches/zor/runtime"));
    }
    anyhow::bail!("set XDG_RUNTIME_DIR or specify --directory for zor's service")
}

fn private_directory(root: &Path) -> Result<()> {
    anyhow::ensure!(root.is_absolute(), "zor service directory must be absolute");
    match fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(root)
    {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    let metadata = fs::symlink_metadata(root)?;
    anyhow::ensure!(
        metadata.is_dir()
            && metadata.uid() == nix::unistd::geteuid().as_raw()
            && metadata.mode() & 0o077 == 0,
        "zor service directory must be an owned private directory (0700)"
    );
    Ok(())
}

struct Endpoint {
    _lock: nix::fcntl::Flock<File>,
    listener: UnixListener,
    path: PathBuf,
    inode: u64,
}
impl Endpoint {
    fn bind(root: &Path) -> Result<Self> {
        private_directory(root)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(root.join("service.lock"))?;
        let metadata = file.metadata()?;
        anyhow::ensure!(
            metadata.is_file()
                && metadata.uid() == nix::unistd::geteuid().as_raw()
                && metadata.mode() & 0o077 == 0
                && metadata.nlink() == 1,
            "unsafe zor service lock"
        );
        let lock = nix::fcntl::Flock::lock(file, nix::fcntl::FlockArg::LockExclusiveNonblock)
            .map_err(|(_, error)| {
                anyhow::anyhow!("zor service already running or lock unavailable: {error}")
            })?;
        let path = root.join("control.sock");
        match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                anyhow::ensure!(
                    metadata.file_type().is_socket()
                        && metadata.uid() == nix::unistd::geteuid().as_raw(),
                    "refusing to replace non-socket or foreign service endpoint"
                );
                fs::remove_file(&path)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let listener = UnixListener::bind(&path)?;
        let inode = fs::symlink_metadata(&path)?.ino();
        let endpoint = Self {
            _lock: lock,
            listener,
            path,
            inode,
        };
        fs::set_permissions(&endpoint.path, fs::Permissions::from_mode(0o600))?;
        endpoint.listener.set_nonblocking(true)?;
        Ok(endpoint)
    }
}
impl Drop for Endpoint {
    fn drop(&mut self) {
        if fs::symlink_metadata(&self.path)
            .is_ok_and(|metadata| metadata.ino() == self.inode && metadata.file_type().is_socket())
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

struct Signals(Vec<signal_hook::SigId>);
impl Signals {
    fn register(&mut self, signal: i32, flag: &Arc<AtomicBool>) -> Result<()> {
        self.0
            .push(signal_hook::flag::register(signal, Arc::clone(flag))?);
        Ok(())
    }
}
impl Drop for Signals {
    fn drop(&mut self) {
        for id in &self.0 {
            signal_hook::low_level::unregister(*id);
        }
    }
}

struct Client {
    stream: UnixStream,
    deadline: Instant,
    input: Vec<u8>,
    output: Option<Vec<u8>>,
    written: usize,
    pending: Option<std::sync::mpsc::Receiver<Value>>,
}
impl Client {
    fn advance(
        &mut self,
        shared: &Mutex<Published>,
        instance: &str,
        stop: &AtomicBool,
        tasks: &crate::service_tasks::Worker,
    ) -> bool {
        if Instant::now() >= self.deadline {
            return false;
        }
        if let Some(pending) = &self.pending {
            match pending.try_recv() {
                Ok(response) => {
                    self.pending = None;
                    self.output = encode(response);
                    if self.output.is_none() {
                        return false;
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => return true,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => return false,
            }
        }
        if self.output.is_none() {
            let mut bytes = [0; 1024];
            match self.stream.read(&mut bytes) {
                Ok(0) => return false,
                Ok(count) => {
                    let Some(bytes) = bytes.get(..count) else {
                        return false;
                    };
                    self.input.extend_from_slice(bytes);
                    if self.input.len() > MAX_REQUEST {
                        return false;
                    }
                    if self.input.contains(&b'\n')
                        && let Some(response) = self.reply(shared, instance, stop, tasks)
                    {
                        self.output = encode(response);
                        if self.output.is_none() {
                            return false;
                        }
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return false,
            }
        }
        if let Some(output) = &self.output {
            let Some(remaining) = output.get(self.written..) else {
                return false;
            };
            match self.stream.write(remaining) {
                Ok(0) => return false,
                Ok(count) => self.written += count,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return false,
            }
            if self.written == output.len() {
                return false;
            }
        }
        true
    }
    fn reply(
        &mut self,
        shared: &Mutex<Published>,
        instance: &str,
        stop: &AtomicBool,
        tasks: &crate::service_tasks::Worker,
    ) -> Option<Value> {
        let request = match serde_json::from_slice::<Request>(&self.input) {
            Ok(request) => request,
            Err(_) => {
                return Some(json!({"v":1,"id":null,"status":"failed","error":"invalid-request"}));
            }
        };
        if request.v != 1 {
            return Some(
                json!({"v":1,"id":request.id,"status":"failed","error":"incompatible-version"}),
            );
        }
        if request.service_instance.is_some()
            && !matches!(request.op, Operation::Shutdown | Operation::Task)
        {
            return Some(
                json!({"v":1,"id":request.id,"status":"failed","error":"invalid-request"}),
            );
        }
        if matches!(request.op, Operation::Task) != request.task.is_some() {
            return Some(
                json!({"v":1,"id":request.id,"status":"failed","error":"invalid-request"}),
            );
        }
        Some(match request.op {
            Operation::Task => {
                if request.service_instance.as_deref() != Some(instance) {
                    return Some(
                        json!({"v":1,"id":request.id,"status":"failed","error":"service-instance-conflict"}),
                    );
                }
                let task = request.task?;
                let deadline = self.deadline + Duration::from_secs(12);
                match tasks.submit(task, request.id, deadline) {
                    Ok(receiver) => {
                        self.pending = Some(receiver);
                        self.deadline = deadline;
                        return None;
                    }
                    Err(_) => {
                        json!({"v":1,"id":request.id,"status":"failed","service_instance":instance,"error":"task-overloaded"})
                    }
                }
            }
            Operation::Shutdown => {
                if request.service_instance.as_deref() != Some(instance) {
                    return Some(
                        json!({"v":1,"id":request.id,"status":"failed","error":"service-instance-conflict"}),
                    );
                }
                stop.store(true, Ordering::Release);
                json!({"v":1,"id":request.id,"status":"completed","service_instance":instance,"stopping":true})
            }
            Operation::Ping => {
                json!({"v":1,"id":request.id,"status":"completed","service_instance":instance})
            }
            Operation::Snapshot => match shared.lock() {
                Ok(published) => published.response(instance, request.id, Instant::now()),
                Err(_) => {
                    json!({"v":1,"id":request.id,"status":"failed","error":"observer-failed"})
                }
            },
        })
    }
}

fn encode(response: Value) -> Option<Vec<u8>> {
    let mut encoded = serde_json::to_vec(&response).ok()?;
    if encoded.len() >= MAX_RESPONSE {
        encoded = serde_json::to_vec(&json!({"v":1,"id":response.get("id"),"status":"failed",
            "service_instance":response.get("service_instance"),"error":"response-too-large"}))
        .ok()?;
    }
    encoded.push(b'\n');
    Some(encoded)
}

pub fn run(
    root: Option<PathBuf>,
    runtime: Option<PathBuf>,
    state: Option<PathBuf>,
    extra: &[PathBuf],
    forced: Option<&str>,
) -> Result<u8> {
    run_inner(root, runtime, state, extra, forced, None)
}

pub fn run_daemon(
    root: Option<PathBuf>,
    runtime: Option<PathBuf>,
    state: Option<PathBuf>,
    extra: &[PathBuf],
    forced: Option<&str>,
) -> Result<u8> {
    // The bootstrap socket is inherited as stdin, never located by a PID/path race.
    let mut channel = UnixStream::from(std::io::stdin().as_fd().try_clone_to_owned()?);
    crate::fux::same_user(&channel)?;
    channel.set_read_timeout(Some(Duration::from_secs(3)))?;
    channel.set_write_timeout(Some(Duration::from_secs(3)))?;
    let result = nix::unistd::setsid()
        .map_err(anyhow::Error::from)
        .and_then(|_| run_inner(root, runtime, state, extra, forced, Some(&mut channel)));
    if let Err(error) = &result {
        let diagnostic: String = format!("{error:#}").chars().take(512).collect();
        let _ = writeln!(channel, "{}", json!({"status":"failed","error":diagnostic}));
    }
    result
}

fn run_inner(
    root: Option<PathBuf>,
    runtime: Option<PathBuf>,
    state: Option<PathBuf>,
    extra: &[PathBuf],
    forced: Option<&str>,
    startup: Option<&mut UnixStream>,
) -> Result<u8> {
    if let Some(agent) = forced {
        crate::osc::AgentId::new(agent)?;
    }
    let runtime = runtime.map(Ok).unwrap_or_else(crate::fux::runtime)?;
    anyhow::ensure!(runtime.is_absolute(), "fux runtime must be absolute");
    anyhow::ensure!(
        state.as_ref().is_none_or(|path| path.is_absolute()),
        "zor state directory must be absolute"
    );
    let root = root.map(Ok).unwrap_or_else(directory)?;
    let endpoint = Endpoint::bind(&root)?;
    let mut nonce = [0u8; 16];
    File::open("/dev/urandom")?.read_exact(&mut nonce)?;
    let instance: String = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
    let mut catalog = crate::rules::bundle::Catalog::load(extra)?;
    let stop = Arc::new(AtomicBool::new(false));
    let reload = Arc::new(AtomicBool::new(false));
    let tasks = crate::service_tasks::Worker::new(
        state,
        runtime.clone(),
        instance.clone(),
        Arc::clone(&stop),
    )?;
    let mut signals = Signals(Vec::new());
    for signal in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT] {
        signals.register(signal, &stop)?;
    }
    signals.register(signal_hook::consts::SIGHUP, &reload)?;
    let shared = Arc::new(Mutex::new(Published::default()));
    let worker_shared = Arc::clone(&shared);
    let worker_stop = Arc::clone(&stop);
    let forced = forced.map(str::to_owned);
    let extra = extra.to_vec();
    let worker = std::thread::Builder::new()
        .name("zor-observe".into())
        .spawn(move || {
            let mut registry = crate::watch::Registry::default();
            let mut events = crate::watch::events::Events::default();
            let mut reload_problem = None;
            while !worker_stop.load(Ordering::Acquire) {
                if reload.swap(false, Ordering::AcqRel) {
                    match catalog.reload(&extra) {
                        Ok(()) => {
                            registry.invalidate_rules();
                            reload_problem = None;
                        }
                        Err(error) => {
                            reload_problem =
                                Some(error.to_string().chars().take(256).collect::<String>())
                        }
                    }
                }
                let started = Instant::now();
                let mut snapshot = registry.subscribed_scan(
                    &runtime,
                    catalog.sets(),
                    forced.as_deref(),
                    &mut events,
                );
                snapshot.rules_generation = catalog.generation();
                if let Some(problem) = &reload_problem {
                    snapshot.problems.insert("rules".into(), problem.clone());
                }
                let Ok(mut published) = worker_shared.lock() else {
                    break;
                };
                let Some(sequence) = published.sequence.checked_add(1) else {
                    break;
                };
                *published = Published {
                    sequence,
                    at: Some(Instant::now()),
                    snapshot,
                };
                drop(published);
                let changes = events.wait(started + crate::watch::events::RESCAN, || {
                    worker_stop.load(Ordering::Acquire) || reload.load(Ordering::Acquire)
                });
                if !changes.is_empty() {
                    registry.invalidate_workspaces(&changes);
                    let Ok(mut published) = worker_shared.lock() else {
                        break;
                    };
                    crate::watch::invalidate_snapshot(&mut published.snapshot, &changes);
                    published.snapshot.event_streams = events.count();
                    published.snapshot.event_failures = events.failures();
                    let Some(sequence) = published.sequence.checked_add(1) else {
                        break;
                    };
                    published.sequence = sequence;
                    // Do not refresh the timestamps of evidence invalidated by this event.
                }
            }
        })?;
    let ready = if let Some(channel) = startup {
        activate(channel, &instance)
    } else {
        Ok(())
    };
    let result = ready.and_then(|()| {
        serve(
            &endpoint.listener,
            &shared,
            &instance,
            &stop,
            &worker,
            &tasks,
        )
    });
    stop.store(true, Ordering::Release);
    worker.thread().unpark();
    worker
        .join()
        .map_err(|_| anyhow::anyhow!("zor observer thread failed"))?;
    result?;
    Ok(0)
}

fn activate(channel: &mut UnixStream, instance: &str) -> Result<()> {
    writeln!(
        channel,
        "{}",
        json!({"status":"ready","service_instance":instance})
    )?;
    let mut ack = [0; 9];
    channel
        .read_exact(&mut ack)
        .context("startup caller disconnected before activation")?;
    anyhow::ensure!(&ack == b"activate\n", "invalid startup activation");
    // Receiving activation commits the service lifetime. A lost acknowledgement must not
    // revoke it; the caller can reconcile through the instance-scoped public endpoint.
    let _ = writeln!(
        channel,
        "{}",
        json!({"status":"activated","service_instance":instance})
    );
    Ok(())
}

fn serve(
    listener: &UnixListener,
    shared: &Mutex<Published>,
    instance: &str,
    stop: &AtomicBool,
    worker: &std::thread::JoinHandle<()>,
    tasks: &crate::service_tasks::Worker,
) -> Result<()> {
    let mut clients: Vec<Client> = Vec::new();
    while !stop.load(Ordering::Acquire) {
        anyhow::ensure!(!tasks.is_finished(), "zor task worker stopped unexpectedly");
        anyhow::ensure!(!worker.is_finished(), "zor observer stopped unexpectedly");
        let mut polls = vec![nix::poll::PollFd::new(
            listener.as_fd(),
            nix::poll::PollFlags::POLLIN,
        )];
        // Pending task clients are checked on the bounded poll cadence, not by readability.
        // In particular a disconnected pending peer must not cause a POLLHUP busy loop.
        for client in clients.iter().filter(|client| client.pending.is_none()) {
            polls.push(nix::poll::PollFd::new(
                client.stream.as_fd(),
                if client.output.is_some() {
                    nix::poll::PollFlags::POLLOUT
                } else {
                    nix::poll::PollFlags::POLLIN
                },
            ));
        }
        match nix::poll::poll(&mut polls, 250u16) {
            Ok(_) | Err(nix::errno::Errno::EINTR) => {}
            Err(error) => return Err(error.into()),
        }
        drop(polls);
        clients.retain_mut(|client| client.advance(shared, instance, stop, tasks));
        for _ in 0..MAX_CLIENTS {
            match listener.accept() {
                Ok((stream, _)) => {
                    if clients.len() >= MAX_CLIENTS || crate::fux::same_user(&stream).is_err() {
                        continue;
                    }
                    stream.set_nonblocking(true)?;
                    clients.push(Client {
                        stream,
                        deadline: Instant::now() + CLIENT_TIMEOUT,
                        input: Vec::new(),
                        output: None,
                        written: 0,
                        pending: None,
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error.into()),
            }
        }
    }
    Ok(())
}

pub fn status(root: Option<PathBuf>) -> Result<Value> {
    let root = root.map(Ok).unwrap_or_else(directory)?;
    anyhow::ensure!(root.is_absolute(), "zor service directory must be absolute");
    request(&root, json!({"v":1,"id":1,"op":"snapshot"}))
}

pub(crate) fn overview(root: Option<PathBuf>, instance: &str) -> Result<Value> {
    let root = root.map(Ok).unwrap_or_else(directory)?;
    request(
        &root,
        json!({"v":1,"id":1,"op":"task","service_instance":instance,
        "task":{"action":"overview"}}),
    )
}

pub fn shutdown(root: Option<PathBuf>) -> Result<Value> {
    let root = root.map(Ok).unwrap_or_else(directory)?;
    let current = status(Some(root.clone()))?;
    let instance = current
        .get("service_instance")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("zor service did not report its identity"))?;
    request(
        &root,
        json!({"v":1,"id":1,"op":"shutdown","service_instance":instance}),
    )
}

/// Enable only the named group's persisted scheduling intent in the expected task store.
pub fn run_group(
    root: Option<PathBuf>,
    state: &Path,
    id: &str,
    extra: &[PathBuf],
    forced: Option<&str>,
) -> Result<Value> {
    let group = crate::tasks::group::inspect(state, id)?;
    anyhow::ensure!(
        group.pointer("/group/cancelled").and_then(Value::as_bool) == Some(false),
        "cancelled group cannot run"
    );
    let current = ensure(root.clone(), extra, forced, Some(state.into()))?;
    let instance = current
        .get("service_instance")
        .and_then(Value::as_str)
        .context("zor service identity missing")?;
    let overview = overview(root.clone(), instance)?;
    let actual: PathBuf = serde_json::from_value(
        overview
            .pointer("/value/state_directory")
            .context("service task state directory missing")?
            .clone(),
    )?;
    anyhow::ensure!(
        std::fs::canonicalize(actual)? == std::fs::canonicalize(state)?,
        "zor service uses a different task state directory; select its matching --directory"
    );
    let root = root.map(Ok).unwrap_or_else(directory)?;
    let response = request(
        &root,
        json!({"v":1,"id":1,"op":"task","service_instance":instance,
        "task":{"action":"group-run","id":id}}),
    )?;
    response
        .get("value")
        .cloned()
        .context("group-run response missing")
}

fn request(root: &Path, value: Value) -> Result<Value> {
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut stream = crate::fux::connect(&root.join("control.sock"), deadline)
        .context("zor service unavailable; start `zor serve` with the same --directory")?;
    crate::fux::same_user(&stream)?;
    stream.set_write_timeout(Some(Duration::from_secs(1)))?;
    serde_json::to_writer(&mut stream, &value)?;
    stream.write_all(b"\n")?;
    let mut output = Vec::new();
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| anyhow::anyhow!("zor service request timed out"))?;
        stream.set_read_timeout(Some(remaining))?;
        let mut chunk = [0; 8192];
        let count = stream.read(&mut chunk)?;
        anyhow::ensure!(count != 0, "zor service closed before replying");
        anyhow::ensure!(
            output.len() + count <= MAX_RESPONSE,
            "zor service response exceeds limit"
        );
        output.extend_from_slice(
            chunk
                .get(..count)
                .ok_or_else(|| anyhow::anyhow!("invalid response length"))?,
        );
        if output.contains(&b'\n') {
            break;
        }
    }
    let response: Value = serde_json::from_slice(&output)?;
    if response.get("v") == Some(&json!(1))
        && response.get("id") == Some(&json!(1))
        && response.get("status") == Some(&json!("failed"))
        && response.get("error") == Some(&json!("task-busy"))
    {
        return Err(crate::tasks::store::Busy.into());
    }
    anyhow::ensure!(
        response.get("v") == Some(&json!(1))
            && response.get("id") == Some(&json!(1))
            && response.get("status") == Some(&json!("completed")),
        "incompatible or failed zor service response"
    );
    Ok(response)
}

/// Start at most one background observer for this service directory. Existing service
/// configuration wins; callers read its actual observations rather than reconfiguring it.
pub fn ensure(
    root: Option<PathBuf>,
    extra: &[PathBuf],
    forced: Option<&str>,
    state: Option<PathBuf>,
) -> Result<Value> {
    let root = root.map(Ok).unwrap_or_else(directory)?;
    private_directory(&root)?;
    match status(Some(root.clone())) {
        Ok(response) => return Ok(response),
        Err(error) if absent(&error) => {}
        Err(error) => {
            return Err(error
                .context("existing zor endpoint did not answer; refusing to start a replacement"));
        }
    }
    if let Some(agent) = forced {
        crate::osc::AgentId::new(agent)?;
    }
    // Fail with a useful diagnostic before creating a child. The child validates again.
    crate::rules::bundle::Catalog::load(extra)?;
    let runtime = crate::fux::runtime()?;
    let extra: Vec<_> = extra
        .iter()
        .map(std::path::absolute)
        .collect::<std::io::Result<_>>()?;
    let started = start_background(&root, &runtime, &extra, forced, state.as_deref());
    match status(Some(root)) {
        Ok(response) => {
            if let Ok(instance) = started {
                anyhow::ensure!(
                    response.get("service_instance").and_then(Value::as_str)
                        == Some(instance.as_str()),
                    "zor service changed during startup; retry status"
                );
            }
            // A concurrent starter may have won the service lock. Its valid response is authoritative.
            Ok(response)
        }
        Err(error) => {
            match started {
                Ok(_) => Err(error
                    .context("zor activated but status is unavailable; it may still be running")),
                Err(startup) => Err(startup.context(format!(
                    "zor startup did not produce a usable service: {error:#}"
                ))),
            }
        }
    }
}

fn absent(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause.downcast_ref::<std::io::Error>().is_some_and(|error| {
            matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            )
        }) || cause
            .downcast_ref::<nix::errno::Errno>()
            .is_some_and(|error| {
                matches!(
                    error,
                    nix::errno::Errno::ENOENT | nix::errno::Errno::ECONNREFUSED
                )
            })
    })
}

struct StartingChild(Option<std::process::Child>);
impl StartingChild {
    fn detach(mut self) -> Result<()> {
        let (sender, receiver) = std::sync::mpsc::sync_channel::<std::process::Child>(1);
        std::thread::Builder::new()
            .name("zor-service-reaper".into())
            .spawn(move || {
                if let Ok(mut child) = receiver.recv() {
                    let _ = child.wait();
                }
            })?;
        if let Some(child) = self.0.take()
            && let Err(error) = sender.send(child)
        {
            self.0 = Some(error.0);
            anyhow::bail!("service reaper stopped before activation");
        }
        Ok(())
    }
}
impl Drop for StartingChild {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn start_background(
    root: &Path,
    runtime: &Path,
    extra: &[PathBuf],
    forced: Option<&str>,
    state: Option<&Path>,
) -> Result<String> {
    anyhow::ensure!(
        state.is_none_or(|path| path.is_absolute()),
        "zor state directory must be absolute"
    );
    let (mut channel, child_channel) = UnixStream::pair()?;
    let mut command = std::process::Command::new(std::env::current_exe()?);
    if let Some(state) = state {
        command.arg("--state-directory").arg(state);
    }
    for path in extra {
        command.arg("--rules").arg(path);
    }
    if let Some(agent) = forced {
        command.arg("--agent").arg(agent);
    }
    command
        .arg("serve")
        .arg("--directory")
        .arg(root)
        .arg("--runtime")
        .arg(runtime)
        .arg("--daemon-child")
        .current_dir(root)
        .stdin(std::process::Stdio::from(std::os::fd::OwnedFd::from(
            child_channel,
        )))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let child = StartingChild(Some(
        command.spawn().context("launch zor background service")?,
    ));
    let deadline = Instant::now() + Duration::from_secs(5);
    let ready = startup_frame(&mut channel, deadline)?;
    anyhow::ensure!(
        ready.get("status").and_then(Value::as_str) == Some("ready"),
        "zor child startup failed: {ready}"
    );
    let instance = ready
        .get("service_instance")
        .and_then(Value::as_str)
        .filter(|value| value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or_else(|| anyhow::anyhow!("invalid zor startup identity"))?
        .to_owned();
    // Before activation the guard owns cleanup; afterward an uncertain reply must not kill
    // a service another controller may already be using. Its reaper has no lifecycle authority.
    child.detach()?;
    channel.set_write_timeout(Some(
        deadline
            .saturating_duration_since(Instant::now())
            .max(Duration::from_millis(1)),
    ))?;
    channel.write_all(b"activate\n")?;
    let activated = startup_frame(&mut channel, deadline)?;
    anyhow::ensure!(
        activated.get("status").and_then(Value::as_str) == Some("activated")
            && activated.get("service_instance").and_then(Value::as_str) == Some(instance.as_str()),
        "zor activation outcome is uncertain: {activated}"
    );
    Ok(instance)
}

fn startup_frame(channel: &mut UnixStream, deadline: Instant) -> Result<Value> {
    let mut bytes = Vec::new();
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| anyhow::anyhow!("zor startup deadline exceeded"))?;
        channel.set_read_timeout(Some(remaining))?;
        let mut byte = [0];
        channel
            .read_exact(&mut byte)
            .context("zor child closed its startup channel")?;
        bytes.extend_from_slice(&byte);
        anyhow::ensure!(bytes.len() <= 4096, "zor startup response exceeds limit");
        if byte == [b'\n'] {
            return serde_json::from_slice(&bytes).context("invalid zor startup response");
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn freshness_includes_scan_and_queue_age_and_clears_stale_evidence() {
        let now = Instant::now();
        let observation = crate::watch::Observation {
            handle: crate::watch::Handle {
                instance: "fux".into(),
                workspace: "default".into(),
                stream: 1,
                pane: 1,
                pid: Some(3),
            },
            age_upper_bound_ms: 2000,
            revision: 2,
            input_sequence: 1,
            agent: Some("test".into()),
            detected_pid: Some(3),
            state: "blocked".into(),
            rule: Some("approval".into()),
            problem: None,
        };
        let snapshot = crate::watch::Snapshot {
            scan_duration_ms: 2000,
            event_streams: 0,
            event_failures: 0,
            observations: vec![observation],
            ..Default::default()
        };
        let published = Published {
            at: Some(now),
            sequence: 4,
            snapshot,
        };
        let fresh = published.response("service", 1, now + Duration::from_secs(2));
        assert_eq!(
            fresh.pointer("/snapshot/observations/0/age_upper_bound_ms"),
            Some(&json!(4000))
        );
        assert_eq!(fresh.get("stale"), Some(&json!(false)));
        let stale = published.response("service", 1, now + Duration::from_secs(4));
        assert_eq!(stale.get("stale"), Some(&json!(true)));
        assert_eq!(
            stale.pointer("/snapshot/observations/0/state"),
            Some(&json!("unknown"))
        );
        assert_eq!(
            stale.pointer("/snapshot/observations/0/rule"),
            Some(&Value::Null)
        );
        assert_eq!(
            stale.pointer("/snapshot/observations/0/handle/instance"),
            Some(&json!("fux"))
        );
        assert_eq!(
            Published::default()
                .response("service", 1, now)
                .get("stale"),
            Some(&json!(true))
        );
    }

    #[test]
    fn endpoint_lock_excludes_duplicate_and_refuses_unsafe_paths() {
        let root = std::env::temp_dir().join(format!("zor-service-test-{}", std::process::id()));
        fs::create_dir(&root).expect("unique directory");
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).expect("private");
        let endpoint = Endpoint::bind(&root).expect("bind");
        assert!(Endpoint::bind(&root).is_err());
        assert!(root.join("control.sock").exists());
        drop(endpoint);
        assert!(!root.join("control.sock").exists());
        fs::write(root.join("control.sock"), b"preserve").expect("file");
        assert!(Endpoint::bind(&root).is_err());
        assert_eq!(
            fs::read(root.join("control.sock")).expect("preserved file"),
            b"preserve"
        );
        fs::remove_file(root.join("control.sock")).expect("remove owned file");
        let stale = UnixListener::bind(root.join("control.sock")).expect("stale endpoint");
        drop(stale);
        drop(Endpoint::bind(&root).expect("replace stale socket under lock"));
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).expect("unsafe permissions");
        assert!(Endpoint::bind(&root).is_err());
        fs::remove_dir_all(root).expect("cleanup");
    }
}
