#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
use serde_json::{Value, json};
use std::{
    fs,
    net::TcpListener,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
static NEXT_PORT: AtomicU64 = AtomicU64::new(0);
// A concurrent fork can temporarily inherit a reserved listener until exec.
// Serialize reservation-to-ready with outer-PTY spawns, not the test scenarios.
static SPAWN: std::sync::Mutex<()> = std::sync::Mutex::new(());

mod design;

struct Server {
    child: Child,
    endpoint: String,
    directory: PathBuf,
}

impl Server {
    fn start() -> Self {
        let _spawn = SPAWN.lock().unwrap_or_else(|error| error.into_inner());
        let directory = std::env::temp_dir().join(format!(
            "fux-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&directory).unwrap();
        let directory = directory.canonicalize().unwrap();
        let config = directory.join("fux.json");
        fs::write(
            &config,
            r#"{"shell":["/bin/sh"],"history_lines":100,"clipboard":"write-only"}"#,
        )
        .unwrap();
        // Distinct non-ephemeral ports avoid port-0 reservations being reused by
        // parallel fixtures or outgoing HTTP sockets before the child binds.
        let listener = (0..20_000)
            .find_map(|_| {
                let index =
                    u64::from(std::process::id()) + NEXT_PORT.fetch_add(1, Ordering::Relaxed);
                TcpListener::bind(("127.0.0.1", 10_000 + (index % 20_000) as u16)).ok()
            })
            .expect("no free test port");
        let address = listener.local_addr().unwrap();
        drop(listener);
        let child = Command::new(env!("CARGO_BIN_EXE_fux"))
            .args(["server", "--port", &address.port().to_string(), "--config"])
            .arg(config)
            .env("SHELL", "/bin/sh")
            .env("PS1", "$ ")
            .env("HOME", &directory)
            .env("HISTFILE", "/dev/null")
            .env_remove("ENV")
            .env_remove("BASH_ENV")
            .current_dir(&directory)
            .stdout(Stdio::null())
            .stderr(fs::File::create(directory.join("server.log")).unwrap())
            .spawn()
            .unwrap();
        let server = Self {
            child,
            endpoint: format!("http://{address}"),
            directory,
        };
        eventually(|| server.request("rpc.discover", Value::Null).is_ok());
        server
    }

    fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(5)))
            .build()
            .into();
        let response: Value = agent
            .post(&self.endpoint)
            .send_json(json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}))
            .map_err(|error| error.to_string())?
            .body_mut()
            .read_json()
            .map_err(|error| error.to_string())?;
        if let Some(error) = response.get("error") {
            return Err(error.to_string());
        }
        Ok(response["result"].clone())
    }

    fn rpc(&self, method: &str, params: Value) -> Value {
        self.request(method, params)
            .unwrap_or_else(|error| panic!("{method}: {error}"))
    }

    fn query(&self, component: &str) -> Vec<Value> {
        self.rpc("world.query", json!({"data":{"components":[component]}}))
            .as_array()
            .unwrap()
            .clone()
    }

    fn control(&self, viewer: u64, action: &str, value: &str) {
        self.rpc("world.trigger_event", json!({"event":"fux::control::Control","value":{"viewer":viewer,"action":action,"value":value}}));
    }

    fn input(&self, viewer: u64, input: Value) {
        self.rpc(
            "world.trigger_event",
            json!({"event":"fux::control::UserInput","value":{"viewer":viewer,"input":input}}),
        );
    }

    fn enter(&self, viewer: u64) {
        self.input(
            viewer,
            json!({"kind":"key","key":"enter","ctrl":false,"alt":false,"shift":false}),
        );
    }

    fn attach(&self) -> u64 {
        self.rpc("fux.attach", json!({"rows":24,"cols":80}))["viewer"]
            .as_u64()
            .unwrap()
    }

    fn screen(&self, viewer: u64) -> String {
        let frame = self.rpc("fux.frame", json!({"viewer":viewer}));
        let mut parser = vt100::Parser::new(24, 80, 0);
        parser.process(frame["paint"].as_str().unwrap().as_bytes());
        parser.screen().contents()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.request(
            "world.trigger_event",
            json!({"event":"fux::control::Shutdown","value":null}),
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        while matches!(self.child.try_wait(), Ok(None)) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        if thread::panicking() {
            eprintln!(
                "server log: {}",
                fs::read_to_string(self.directory.join("server.log")).unwrap_or_default()
            );
        }
        let _ = fs::remove_dir_all(&self.directory);
    }
}

fn eventually(mut observed: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !observed() {
        assert!(
            Instant::now() < deadline,
            "observable state did not settle within five seconds"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn alive(pid: i32) -> bool {
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_ok()
}

#[test]
fn stock_launch_removal_settles_without_another_request() {
    let server = Server::start();
    let pid_file = server.directory.join("child.pid");
    let entity = server.rpc("world.spawn_entity", json!({"components":{
        "fux::model::Launch":{"argv":["/bin/sh","-c",format!("echo $$ > '{}'; exec sleep 60",pid_file.display())],"cwd":server.directory,"history_lines":20}
    }}))["entity"].as_u64().unwrap();
    // Observe the OS, not another RPC: a later request would mask a missing wake.
    eventually(|| fs::read_to_string(&pid_file).is_ok_and(|text| !text.trim().is_empty()));
    let pid = fs::read_to_string(pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(alive(pid));
    server.rpc(
        "world.remove_components",
        json!({"entity":entity,"components":["fux::model::Launch"]}),
    );
    eventually(|| !alive(pid));
    let states = server.query("fux::model::ProcessState");
    let state = &states.iter().find(|row| row["entity"] == entity).unwrap()["components"]["fux::model::ProcessState"];
    assert!(state["pid"].is_null());
    assert!(state["exit"].is_number(), "{state}");
    assert!(state["error"].is_null(), "{state}");
}

#[test]
fn interactive_background_jobs_hang_up_when_pane_terminates() {
    let server = Server::start();
    let viewer = server.attach();
    let shell =
        server.query("fux::model::ProcessState")[0]["components"]["fux::model::ProcessState"]["pid"]
            .as_i64()
            .unwrap() as i32;
    let pid_file = server.directory.join("background.pid");
    server.input(
        viewer,
        json!({"kind":"paste","text":format!("sleep 60 & echo $! > '{}'",pid_file.display())}),
    );
    server.enter(viewer);
    eventually(|| fs::read_to_string(&pid_file).is_ok_and(|text| !text.trim().is_empty()));
    let background: i32 = fs::read_to_string(pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_ne!(
        nix::unistd::getpgid(Some(nix::unistd::Pid::from_raw(background)))
            .unwrap()
            .as_raw(),
        shell
    );
    server.control(viewer, "terminate", "");
    eventually(|| !alive(shell) && !alive(background));
    let state =
        &server.query("fux::model::ProcessState")[0]["components"]["fux::model::ProcessState"];
    assert!(state["pid"].is_null());
    assert!(state["exit"].is_number(), "{state}");
    assert!(state["error"].is_null(), "{state}");
}

#[test]
fn layout_mapping_and_prompt_paste_preserve_live_process_identity() {
    let server = Server::start();
    let viewer = server.attach();
    let first = server.query("fux::model::Launch")[0]["entity"]
        .as_u64()
        .unwrap();
    for action in ["zoom", "scroll_up"] {
        server.control(viewer, action, "");
    }
    server.control(viewer, "split_horizontal", "");
    let screen = server.screen(viewer);
    let chrome = screen.lines().last().unwrap();
    assert!(!chrome.contains("zoom"));
    assert!(!chrome.contains('↑'));
    let launches = server.query("fux::model::Launch");
    let second = launches
        .iter()
        .map(|row| row["entity"].as_u64().unwrap())
        .find(|entity| *entity != first)
        .unwrap();
    for (entity, name) in [(first, "alpha"), (second, "beta")] {
        server.rpc(
            "world.insert_components",
            json!({"entity":entity,"components":{"bevy_ecs::name::Name":name}}),
        );
    }
    // Distinct real terminal contents, not decorative pane-header labels.
    for marker in ["BETA", "ALPHA"] {
        server.input(
            viewer,
            json!({"kind":"paste","text":format!("printf '\\033[2J\\033[H{marker}\\n'")}),
        );
        server.enter(viewer);
        eventually(|| server.screen(viewer).contains(marker));
        server.control(viewer, "focus_next", "");
        server.screen(viewer);
    }
    let before = server.query("fux::model::ProcessState");
    let scene = server.directory.join("layout.scn.ron");
    server.control(viewer, "save_layout", "");
    server.input(viewer, json!({"kind":"paste","text":scene}));
    server.enter(viewer);
    eventually(|| fs::metadata(&scene).is_ok_and(|metadata| metadata.len() > 0));
    server.rpc("world.trigger_event", json!({"event":"fux::control::Control","value":{
        "viewer":viewer,"action":"load_layout","value":scene,"mapping":[[first,second],[second,first]]
    }}));
    eventually(|| server.screen(viewer).contains("loaded "));
    let screen = server.screen(viewer);
    let content = screen.lines().next().unwrap();
    assert!(
        content.find("BETA").unwrap() < content.find("ALPHA").unwrap(),
        "{screen}"
    );
    let after = server.query("fux::model::ProcessState");
    for old in &before {
        let new = after
            .iter()
            .find(|row| row["entity"] == old["entity"])
            .unwrap();
        assert_eq!(
            old["components"]["fux::model::ProcessState"]["pid"],
            new["components"]["fux::model::ProcessState"]["pid"]
        );
    }
    let focus = server.query("fux::model::Viewer")[0]["components"]["fux::model::Viewer"]["focus"]
        .as_u64()
        .unwrap();
    server.rpc(
        "world.insert_components",
        json!({"entity":focus,"components":{"bevy_camera::visibility::Visibility":"Hidden"}}),
    );
    eventually(|| !server.screen(viewer).contains("BETA"));
    server.rpc(
        "world.remove_components",
        json!({"entity":focus,"components":["bevy_camera::visibility::Visibility"]}),
    );
    eventually(|| server.screen(viewer).contains("BETA"));
    server.rpc(
        "world.trigger_event",
        json!({"event":"fux::control::Control","value":{
            "viewer":viewer,"action":"load_layout","value":scene,"mapping":[[first,first+1_000_000]]
        }}),
    );
    eventually(|| server.screen(viewer).contains("missing live pane"));
    let remaining = server.query("fux::model::PaneView");
    assert!(remaining.iter().any(|row| row["entity"] == focus));
    assert_eq!(remaining.len(), 2);
}

#[test]
fn stock_viewer_removal_releases_its_native_size_constraint() {
    let server = Server::start();
    let viewer = server.attach();
    for despawn in [false, true] {
        let small = server.rpc("fux.attach", json!({"rows":12,"cols":40}))["viewer"]
            .as_u64()
            .unwrap();
        server.screen(viewer);
        server.input(viewer, json!({"kind":"paste","text":"stty size"}));
        server.enter(viewer);
        eventually(|| server.screen(viewer).contains("11 40"));
        if despawn {
            server.rpc("world.despawn_entity", json!({"entity":small}));
        } else {
            server.rpc(
                "world.remove_components",
                json!({"entity":small,"components":["fux::model::Viewer"]}),
            );
        }
        assert_eq!(
            server.rpc("fux.frame", json!({"viewer":small})),
            json!({"paint":"", "detach":true})
        );
        server.screen(viewer);
        server.input(viewer, json!({"kind":"paste","text":"clear; stty size"}));
        server.enter(viewer);
        eventually(|| server.screen(viewer).contains("23 80"));
    }
}

#[test]
fn repeated_copy_effects_are_not_replaceable_paints() {
    use std::io::{BufRead, BufReader};
    let server = Server::start();
    let viewer = server.attach();
    let endpoint = server.endpoint.clone();
    let (sender, received) = std::sync::mpsc::channel();
    let reader = thread::spawn(move || {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(5)))
            .build()
            .into();
        let response = agent
            .post(&endpoint)
            .send_json(json!({
                "jsonrpc":"2.0", "id":2, "method":"fux.frame+watch", "params":{"viewer":viewer}
            }))
            .unwrap();
        for line in BufReader::new(response.into_body().into_reader()).lines() {
            let line = line.unwrap();
            if let Some(data) = line.strip_prefix("data:") {
                let value: Value = serde_json::from_str(data).unwrap();
                let paint = value["result"]["paint"].as_str().unwrap();
                if sender.send(paint.matches("\x1b]52;").count()).is_err() {
                    break;
                }
            }
        }
    });
    assert_eq!(received.recv_timeout(Duration::from_secs(5)).unwrap(), 0);
    // These are explicit equal copies, not four interchangeable paint snapshots.
    for _ in 0..4 {
        server.control(viewer, "copy", "");
    }
    let mut copies = 0;
    while copies < 4 {
        copies += received.recv_timeout(Duration::from_secs(5)).unwrap();
    }
    assert_eq!(copies, 4);
    // Direct snapshots must not replay effects already delivered to the stream.
    assert!(
        !server.rpc("fux.frame", json!({"viewer":viewer}))["paint"]
            .as_str()
            .unwrap()
            .contains("\x1b]52;")
    );
    drop(received);
    server.control(viewer, "rename_workspace", "reader-finished");
    reader.join().unwrap();
}

#[test]
fn blocked_terminal_paint_does_not_block_stream_drain() {
    use portable_pty::{Child as PtyChild, CommandBuilder, MasterPty, PtySize, native_pty_system};
    use std::io::Read;

    struct SlowTerminal {
        child: Box<dyn PtyChild + Send + Sync>,
        master: Option<Box<dyn MasterPty + Send>>,
    }
    impl Drop for SlowTerminal {
        fn drop(&mut self) {
            self.master.take();
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    let server = Server::start();
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut reader = pair.master.try_clone_reader().unwrap();
    let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_fux"));
    command.arg("attach");
    command.env("FUX_ENDPOINT", &server.endpoint);
    let mut terminal = SlowTerminal {
        child: {
            let _spawn = SPAWN.lock().unwrap_or_else(|error| error.into_inner());
            pair.slave.spawn_command(command).unwrap()
        },
        master: Some(pair.master),
    };
    drop(pair.slave);
    let (ready, received) = std::sync::mpsc::sync_channel(1);
    let first_paint = thread::spawn(move || {
        let mut output = Vec::new();
        let mut chunk = [0; 4096];
        while let Ok(count) = reader.read(&mut chunk) {
            if count == 0 {
                break;
            }
            output.extend_from_slice(&chunk[..count]);
            if output.windows(8).any(|bytes| bytes == b"\x1b[?2026l") {
                let _ = ready.send(());
                return;
            }
        }
    });
    received.recv_timeout(Duration::from_secs(5)).unwrap();
    first_paint.join().unwrap();
    let viewer = server.query("fux::model::Viewer")[0]["entity"]
        .as_u64()
        .unwrap();
    let original = server.query("fux::model::ProcessState")[0]["entity"].clone();
    server.control(viewer, "split_vertical", "exec /usr/bin/yes load");
    // Deliberately stop reading the outer PTY. Paint will block, but HTTP must
    // continue draining/coalescing rather than fill Bevy's watch channel.
    thread::sleep(Duration::from_millis(500));
    assert!(
        server
            .query("fux::model::Viewer")
            .iter()
            .any(|row| row["entity"] == viewer)
    );
    assert!(terminal.child.try_wait().unwrap().is_none());
    let states = server.query("fux::model::ProcessState");
    let hot = states.iter().find(|row| row["entity"] != original).unwrap();
    let entity = hot["entity"].clone();
    let pid = hot["components"]["fux::model::ProcessState"]["pid"]
        .as_u64()
        .unwrap() as i32;
    server.control(viewer, "terminate", "");
    eventually(|| !alive(pid));
    let states = server.query("fux::model::ProcessState");
    let state = &states.iter().find(|row| row["entity"] == entity).unwrap()["components"]["fux::model::ProcessState"];
    assert!(state["pid"].is_null(), "{state}");
    assert!(state["exit"].is_number(), "{state}");
    assert!(state["error"].is_null(), "{state}");
}
