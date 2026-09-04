#![allow(
    dead_code,
    unused_imports,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]

#[path = "../src/daemon/mod.rs"]
mod daemon;

use daemon::*;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ffi::OsString;
use std::fs;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::sync::{Barrier, mpsc};
use std::time::Duration;

#[derive(Default)]
struct EndpointLog {
    closed: bool,
    reaps: Vec<(u64, u64)>,
}

struct FakeEndpoint {
    id: String,
    port: u16,
    log: Arc<Mutex<EndpointLog>>,
}

impl EndpointHandle for FakeEndpoint {
    fn endpoint_id(&self) -> &str {
        &self.id
    }
    fn direct_addr(&self) -> SocketAddrV4 {
        SocketAddrV4::new(Ipv4Addr::LOCALHOST, self.port)
    }
    fn close(&mut self) {
        self.log.lock().expect("log").closed = true;
    }
    fn reap_terminal_sessions(&mut self, now_ms: u64, ttl_ms: u64) {
        self.log.lock().expect("log").reaps.push((now_ms, ttl_ms));
    }
}

#[derive(Default)]
struct FakeFactory {
    next: u16,
    logs: BTreeMap<String, Arc<Mutex<EndpointLog>>>,
    observed_allow: Vec<BTreeSet<String>>,
}

impl EndpointFactory for FakeFactory {
    fn create(
        &mut self,
        name: &str,
        key: &Path,
        allow: &BTreeSet<String>,
    ) -> Result<Box<dyn EndpointHandle>, ManagerError> {
        assert!(key.ends_with(format!("{name}.key")));
        self.next = self.next.saturating_add(1);
        let log = Arc::new(Mutex::new(EndpointLog::default()));
        self.logs.insert(name.to_owned(), Arc::clone(&log));
        self.observed_allow.push(allow.clone());
        Ok(Box::new(FakeEndpoint {
            id: format!("endpoint-{name}"),
            port: 10_000 + self.next,
            log,
        }))
    }
}

#[test]
fn xdg_paths_are_private_absolute_and_workspace_names_cannot_escape() {
    // Phase F4 paths: XDG roots and all daemon-owned directories are private and non-symlinked.
    let root = test_root("paths");
    let runtime = root.join("run");
    let state = root.join("state");
    fs::create_dir_all(&runtime).expect("runtime root");
    fs::create_dir_all(&state).expect("state root");
    let paths = DaemonPaths::from_env(
        Some(runtime.into_os_string()),
        Some(state.into_os_string()),
        None,
    )
    .expect("paths");
    paths.prepare().expect("prepare");
    for directory in [
        &paths.runtime_dir,
        &paths.state_dir,
        &paths.descriptors_dir,
        &paths.keys_dir,
    ] {
        assert_eq!(
            fs::metadata(directory)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
    for name in ["", ".", "..", "../x", "a/b", "with space"] {
        assert!(validate_workspace_name(name).is_err());
    }
    assert!(validate_workspace_name("safe.one_2-3").is_ok());
    fs::remove_dir_all(root).expect("cleanup");
}

#[cfg(target_os = "macos")]
#[test]
fn macos_without_xdg_runtime_uses_private_home_fallback() {
    let root = test_root("macos-runtime-fallback");
    fs::create_dir_all(&root).expect("home");
    let paths = DaemonPaths::from_env(None, None, Some(root.clone().into_os_string()))
        .expect("macOS fallback");
    assert_eq!(
        paths.runtime_dir,
        root.join("Library/Caches/fux-runtime/fux")
    );
    paths.prepare().expect("prepare fallback");
    assert_eq!(
        fs::metadata(&paths.runtime_dir)
            .expect("fallback metadata")
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn descriptors_are_atomic_private_bounded_and_tied_to_pid_plus_instance_nonce() {
    // Phase F4 descriptor: nonce rejects PID reuse and stale/oversized/symlink files.
    let (root, paths) = prepared_paths("descriptor");
    let manager = ManagerIdentity {
        pid: 11,
        instance_nonce: "nonce-a".into(),
    };
    let descriptor = Descriptor {
        name: "one".into(),
        pid: 11,
        instance_nonce: "nonce-a".into(),
        endpoint_id: "endpoint".into(),
        direct_addr: SocketAddrV4::new(Ipv4Addr::LOCALHOST, 1234),
    };
    let path = write_descriptor(&paths, &descriptor).expect("write");
    assert_eq!(
        fs::metadata(&path).expect("metadata").permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        read_descriptor(&path, "one", &manager).expect("read"),
        descriptor
    );
    assert!(read_descriptor(&path, "other", &manager).is_err());
    let reused = ManagerIdentity {
        pid: 11,
        instance_nonce: "nonce-b".into(),
    };
    assert!(read_descriptor(&path, "one", &reused).is_err());
    assert_eq!(
        recover_stale_descriptors(&paths, &reused).expect("recover"),
        1
    );

    let oversized = paths.descriptor("huge").expect("oversized path");
    fs::write(&oversized, vec![b'x'; MAX_DESCRIPTOR_BYTES as usize + 1]).expect("oversized");
    fs::set_permissions(&oversized, fs::Permissions::from_mode(0o600)).expect("private");
    assert!(matches!(
        read_descriptor(&oversized, "huge", &manager),
        Err(DescriptorError::Invalid | DescriptorError::TooLarge)
    ));
    fs::remove_file(oversized).expect("remove oversized");

    let link = paths.descriptor("link").expect("link path");
    symlink("/dev/null", &link).expect("symlink");
    assert!(read_descriptor(&link, "link", &manager).is_err());
    assert!(recover_stale_descriptors(&paths, &manager).is_err());
    fs::remove_file(link).expect("remove link");
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn elected_manager_recovers_descriptors_from_previous_instance() {
    let (root, paths) = prepared_paths("automatic-descriptor-recovery");
    let stale = Descriptor {
        name: "stale".into(),
        pid: 19,
        instance_nonce: "old-instance".into(),
        endpoint_id: "old-endpoint".into(),
        direct_addr: SocketAddrV4::new(Ipv4Addr::LOCALHOST, 9191),
    };
    let path = write_descriptor(&paths, &stale).expect("stale descriptor");
    let _daemon =
        Daemon::new(paths, 20, BTreeSet::new(), "local".into(), 0).expect("new elected manager");
    assert!(!path.exists());
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn manager_lock_elects_one_daemon_and_recovers_only_stale_socket_nodes() {
    // Phase F4 lock: binding the manager socket serializes simultaneous first clients.
    let (root, paths) = prepared_paths("lock");
    let first = ManagerLock::bind(&paths).expect("first lock");
    assert!(ManagerLock::bind(&paths).is_err());
    drop(first);
    let raw = UnixListener::bind(&paths.manager_socket).expect("stale");
    drop(raw);
    wait_until_socket_refuses(&paths.manager_socket);
    let recovered = ManagerLock::bind(&paths).expect("recover stale");
    drop(recovered);
    fs::write(&paths.manager_socket, "not socket").expect("collision");
    assert!(ManagerLock::bind(&paths).is_err());
    assert_eq!(
        fs::read_to_string(&paths.manager_socket).expect("preserved"),
        "not socket"
    );
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn simultaneous_first_clients_elect_exactly_one_manager() {
    // Phase F4 lock race: concurrent first clients cannot both become the daemon.
    let (root, paths) = prepared_paths("lock-race");
    let stale = UnixListener::bind(&paths.manager_socket).expect("seed stale socket");
    drop(stale);
    wait_until_socket_refuses(&paths.manager_socket);
    let paths = Arc::new(paths);
    let start = Arc::new(Barrier::new(3));
    let release = Arc::new(Barrier::new(3));
    let (sender, receiver) = mpsc::channel();
    let mut threads = Vec::new();
    for _ in 0..2 {
        let paths = Arc::clone(&paths);
        let start = Arc::clone(&start);
        let release = Arc::clone(&release);
        let sender = sender.clone();
        threads.push(std::thread::spawn(move || {
            start.wait();
            let lock = ManagerLock::bind(&paths).ok();
            sender.send(lock.is_some()).expect("send result");
            release.wait();
            drop(lock);
        }));
    }
    start.wait();
    let outcomes = [
        receiver.recv().expect("first"),
        receiver.recv().expect("second"),
    ];
    assert_eq!(outcomes.into_iter().filter(|won| *won).count(), 1);
    assert!(
        std::os::unix::net::UnixStream::connect(&paths.manager_socket).is_ok(),
        "the elected manager socket must survive the losing contender's stale cleanup"
    );
    release.wait();
    for thread in threads {
        thread.join().expect("join contender");
    }
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn two_named_workspaces_have_distinct_endpoints_isolated_lifetimes_and_multiple_viewers() {
    // Phase F4 lifecycle: one endpoint/state owner per name; detach never destroys workspace state.
    let (root, paths) = prepared_paths("lifecycle");
    let mut allow = BTreeSet::new();
    allow.insert("remote".into());
    let mut daemon = Daemon::new(paths, 7, allow, "local".into(), 0).expect("daemon");
    let mut factory = FakeFactory::default();
    let one = daemon.create_or_find("one", &mut factory).expect("one");
    let two = daemon.create_or_find("two", &mut factory).expect("two");
    assert_ne!(one.endpoint_id, two.endpoint_id);
    assert_eq!(
        daemon.resolve(None).expect("resolve"),
        Resolution::Pick(vec!["one".into(), "two".into()])
    );
    let workspace = daemon.workspace_mut("one").expect("workspace");
    workspace
        .authorize_and_attach("local")
        .expect("local attach");
    workspace
        .authorize_and_attach("remote")
        .expect("remote attach");
    assert!(workspace.authorize_and_attach("denied").is_err());
    workspace.detach();
    assert_eq!(workspace.viewers(), 1);
    workspace.authorize_and_attach("local").expect("reattach");
    assert_eq!(
        daemon.kill("one").expect("kill one"),
        DaemonAction::Continue
    );
    assert!(daemon.workspace("two").is_some());
    assert_eq!(
        daemon.kill("two").expect("kill two"),
        DaemonAction::ReplyThenExit
    );
    assert!(factory.logs["one"].lock().expect("log").closed);
    assert!(factory.logs["two"].lock().expect("log").closed);
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn no_name_resolution_and_initial_grace_have_deterministic_actions() {
    // Phase F4 attach/create/picker: zero creates default, one attaches, many picks.
    let (root, paths) = prepared_paths("resolution");
    let mut daemon = Daemon::new(paths, 8, BTreeSet::new(), "local".into(), 100).expect("daemon");
    assert_eq!(
        daemon.resolve(None).expect("empty"),
        Resolution::Create("default".into())
    );
    assert_eq!(daemon.tick(5_099), DaemonAction::Continue);
    assert_eq!(daemon.tick(5_100), DaemonAction::ReplyThenExit);
    let mut factory = FakeFactory::default();
    let descriptor = daemon
        .create_or_find("named", &mut factory)
        .expect("create");
    assert_eq!(
        daemon.resolve(None).expect("one"),
        Resolution::Attach(descriptor)
    );
    assert_eq!(
        daemon.resolve(Some("missing")).expect("missing"),
        Resolution::Create("missing".into())
    );
    daemon.tick(99_000);
    assert_eq!(
        factory.logs["named"].lock().expect("log").reaps,
        vec![(99_000, IDLE_WORKSPACE_TTL_MS)]
    );
    fs::remove_dir_all(root).expect("cleanup");
}

#[derive(Default)]
struct FakeClock {
    now: u64,
}
impl Clock for FakeClock {
    fn now_ms(&self) -> u64 {
        self.now
    }
    fn sleep_ms(&mut self, milliseconds: u64) {
        self.now = self.now.saturating_add(milliseconds);
    }
}
struct Ticket {
    error: Option<String>,
}
impl SpawnTicket for Ticket {
    fn try_error(&mut self) -> Option<String> {
        self.error.take()
    }
}
#[derive(Default)]
struct Spawner {
    requests: Vec<SpawnRequest>,
    error: Option<String>,
}
impl DaemonSpawner for Spawner {
    type Ticket = Ticket;
    fn spawn(&mut self, request: SpawnRequest) -> Result<Ticket, SpawnError> {
        self.requests.push(request);
        Ok(Ticket {
            error: self.error.take(),
        })
    }
}
struct Connector {
    attempts: usize,
    ready_at: Option<usize>,
}
impl DaemonConnector for Connector {
    fn connect(&mut self) -> Result<Option<ManagerIdentity>, SpawnError> {
        self.attempts += 1;
        Ok(self
            .ready_at
            .filter(|ready| self.attempts >= *ready)
            .map(|_| ManagerIdentity {
                pid: 3,
                instance_nonce: "ready".into(),
            }))
    }
}

#[test]
fn startup_is_bounded_reports_child_errors_and_scrubs_process_environment_and_stdio() {
    // Phase F4 spawn: detached daemon readiness has a deadline and private error channel.
    let environment = vec![
        (OsString::from("PATH"), OsString::from("/bin")),
        (OsString::from("FUX_SECRET"), OsString::from("bad")),
        (OsString::from("KOH_KEY_PASSPHRASE"), OsString::from("bad")),
    ];
    let mut spawner = Spawner::default();
    let mut connector = Connector {
        attempts: 0,
        ready_at: Some(3),
    };
    let identity = start_or_connect(
        PathBuf::from("/bin/fux"),
        environment.clone(),
        &mut spawner,
        &mut connector,
        &mut FakeClock::default(),
    )
    .expect("ready");
    assert_eq!(identity.instance_nonce, "ready");
    let request = &spawner.requests[0];
    assert_eq!(
        (request.stdin, request.stdout, request.stderr),
        (StdioPolicy::Null, StdioPolicy::Null, StdioPolicy::Null)
    );
    assert!(request.error_channel);
    assert!(request.environment.contains_key(&OsString::from("PATH")));
    assert!(
        !request
            .environment
            .contains_key(&OsString::from("FUX_SECRET"))
    );
    assert!(
        !request
            .environment
            .contains_key(&OsString::from("KOH_KEY_PASSPHRASE"))
    );

    let mut failed = Spawner {
        requests: Vec::new(),
        error: Some("bind failed".into()),
    };
    let error = start_or_connect(
        PathBuf::from("fux"),
        environment,
        &mut failed,
        &mut Connector {
            attempts: 0,
            ready_at: None,
        },
        &mut FakeClock::default(),
    );
    assert_eq!(
        error.expect_err("child error"),
        SpawnError::Child("bind failed".into())
    );
}

#[test]
fn production_spawner_reports_early_exit_and_removes_its_private_channel() {
    // Phase F4 spawn: the real detached process adapter reports pre-readiness exit without leaks.
    let (root, paths) = prepared_paths("process-spawn");
    let mut spawner = ProcessDaemonSpawner::new(paths.runtime_dir.clone());
    let request = SpawnRequest {
        executable: PathBuf::from("/usr/bin/false"),
        args: Vec::new(),
        environment: BTreeMap::new(),
        stdin: StdioPolicy::Null,
        stdout: StdioPolicy::Null,
        stderr: StdioPolicy::Null,
        error_channel: true,
    };
    let mut ticket = spawner.spawn(request).expect("spawn false");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let error = loop {
        if let Some(error) = ticket.try_error() {
            break error;
        }
        assert!(std::time::Instant::now() < deadline, "child exit deadline");
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    assert!(error.contains("exited before readiness"));
    assert!(!ticket.is_armed());
    assert!(!ticket.is_ready());
    drop(ticket);
    assert!(
        !fs::read_dir(&paths.runtime_dir)
            .expect("runtime entries")
            .any(|entry| entry
                .is_ok_and(|entry| entry.file_name().to_string_lossy().starts_with("s-")))
    );
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn secret_transfer_early_exit_disarms_before_drop() {
    let (root, paths) = prepared_paths("secret-early-exit");
    let mut spawner = ProcessDaemonSpawner::new(paths.runtime_dir.clone());
    let mut ticket = spawner
        .spawn(SpawnRequest {
            executable: PathBuf::from("/usr/bin/false"),
            args: Vec::new(),
            environment: BTreeMap::new(),
            stdin: StdioPolicy::Null,
            stdout: StdioPolicy::Null,
            stderr: StdioPolicy::Null,
            error_channel: true,
        })
        .expect("spawn false");
    assert!(matches!(
        ticket.send_secret(b"secret"),
        Err(SpawnError::Child(_))
    ));
    assert!(!ticket.is_armed());
    assert!(!ticket.is_ready());
    let channel = ticket.channel_path().to_owned();
    drop(ticket);
    assert!(!channel.exists());
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn production_startup_channel_is_private_tokenless_and_drop_reaps_child() {
    let (root, paths) = prepared_paths("process-private");
    let mut spawner = ProcessDaemonSpawner::new(paths.runtime_dir.clone());
    let request = SpawnRequest {
        executable: PathBuf::from("/bin/sh"),
        args: vec!["-c".into(), "sleep 30".into()],
        environment: BTreeMap::new(),
        stdin: StdioPolicy::Null,
        stdout: StdioPolicy::Null,
        stderr: StdioPolicy::Null,
        error_channel: true,
    };
    let ticket = spawner.spawn(request).expect("spawn sleeper");
    let channel = ticket.channel_path().to_owned();
    assert_eq!(
        fs::metadata(&channel)
            .expect("channel metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert!(!channel.to_string_lossy().contains("token"));
    let pid = i32::try_from(ticket.process_id()).expect("pid");
    drop(ticket);
    assert!(!channel.exists());
    assert!(nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_err());
    fs::remove_dir_all(root).expect("cleanup");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn production_factory_binds_and_retains_one_real_local_iroh_endpoint() {
    // Phase F4 endpoint: a named workspace owns a distinct retained real iroh endpoint.
    let (root, paths) = prepared_paths("real-endpoint");
    let local_id =
        koh::transport_iroh::format_endpoint_id(&iroh::SecretKey::from_bytes(&[9_u8; 32]).public());
    let mut daemon = Daemon::new(paths.clone(), 19, BTreeSet::new(), local_id, 0).expect("daemon");
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        daemon.create_or_find_async("real", |_key, allow| async move {
            bind_workspace_endpoint_with_secret(
                iroh::SecretKey::from_bytes(&[8_u8; 32]),
                &allow,
                fux::FUX_ALPN,
                NetworkProfile::Local,
                || fux::host::WorkspaceHost::spawn(vec!["/bin/sh".into()], 100, None),
            )
            .await
        }),
    )
    .await
    .expect("bind deadline");
    let descriptor = match result {
        Ok(descriptor) => descriptor,
        Err(ManagerError::Io(error)) if error.to_string().contains("netmon monitor") => {
            // Some sandboxed macOS runners prohibit iroh's interface monitor. The bounded real
            // bind was still attempted; lifecycle behavior remains covered by injected handles.
            drop(daemon);
            fs::remove_dir_all(root).expect("cleanup");
            return;
        }
        Err(error) => panic!("bind endpoint: {error}"),
    };
    assert!(descriptor.direct_addr.ip().is_loopback());
    assert!(!descriptor.endpoint_id.is_empty());
    drop(daemon);
    fs::remove_dir_all(root).expect("cleanup");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn production_endpoint_close_joins_every_accepted_connection_task() {
    use koh::client::IrohConnector;
    use koh::transport_iroh::{
        bind_endpoint_local, direct_addr, generate_secret_key, parse_endpoint_id,
    };
    let client = match bind_endpoint_local(generate_secret_key(), false).await {
        Ok(endpoint) => endpoint,
        Err(error) if format!("{error:#}").contains("Operation not permitted") => return,
        Err(error) => panic!("bind client: {error:#}"),
    };
    let client_id = koh::transport_iroh::format_endpoint_id(&client.id());
    let mut endpoint = match bind_workspace_endpoint_with_secret(
        iroh::SecretKey::from_bytes(&[7_u8; 32]),
        &BTreeSet::from([client_id]),
        fux::FUX_ALPN,
        NetworkProfile::Local,
        || fux::host::WorkspaceHost::spawn(vec!["/bin/cat".into()], 0, None),
    )
    .await
    {
        Ok(endpoint) => endpoint,
        Err(ManagerError::Io(error)) if error.to_string().contains("netmon monitor") => return,
        Err(error) => panic!("bind server: {error}"),
    };
    let target = direct_addr(
        parse_endpoint_id(endpoint.endpoint_id()).expect("server id"),
        endpoint.direct_addr().into(),
    );
    let channel = IrohConnector::with_alpn(client, target, fux::FUX_ALPN)
        .connect()
        .await
        .expect("admitted connection");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while endpoint.active_tasks() == 0 && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(endpoint.active_tasks(), 1);
    endpoint.close();
    assert_eq!(endpoint.active_tasks(), 0, "connection task survived close");
    drop(channel);
}

fn prepared_paths(label: &str) -> (PathBuf, DaemonPaths) {
    let root = test_root(label);
    let runtime = root.join("run");
    let state = root.join("state");
    fs::create_dir_all(&runtime).expect("runtime");
    fs::create_dir_all(&state).expect("state");
    let paths = DaemonPaths::from_env(
        Some(runtime.into_os_string()),
        Some(state.into_os_string()),
        None,
    )
    .expect("paths");
    paths.prepare().expect("prepare");
    (root, paths)
}

fn test_root(label: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("fux-daemon-{label}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    root
}

fn wait_until_socket_refuses(path: &Path) {
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while std::os::unix::net::UnixStream::connect(path).is_ok() {
        assert!(
            std::time::Instant::now() < deadline,
            "dropped Unix listener remained connectable past teardown deadline"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}
