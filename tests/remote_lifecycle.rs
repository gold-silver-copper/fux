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

type Fail = Box<dyn std::error::Error>;
#[path = "../src/testing.rs"]
mod testing;
use testing::{Need, Outcome, ScreenText};

/// JSON lookup with `Value`'s own null-on-miss semantics, without the index lint.
trait At {
    fn at<I: serde_json::value::Index>(&self, index: I) -> Value;
}
impl At for Value {
    fn at<I: serde_json::value::Index>(&self, index: I) -> Value {
        self.get(index).cloned().unwrap_or(Value::Null)
    }
}
/// Iterate a JSON array; anything else is an empty set of rows.
trait Rows {
    fn rows(&self) -> impl Iterator<Item = &Value>;
}
impl Rows for Value {
    fn rows(&self) -> impl Iterator<Item = &Value> {
        self.as_array().into_iter().flatten()
    }
}

struct Server {
    child: Child,
    endpoint: String,
    directory: PathBuf,
}

impl Server {
    fn start() -> Result<Self, Fail> {
        let _spawn = SPAWN.lock().unwrap_or_else(|error| error.into_inner());
        let directory = std::env::temp_dir().join(format!(
            "fux-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&directory)?;
        let directory = directory.canonicalize()?;
        let config = directory.join("fux.json");
        fs::write(
            &config,
            r#"{"shell":["/bin/sh"],"history_lines":100,"clipboard":"write-only"}"#,
        )?;
        // Distinct non-ephemeral ports avoid port-0 reservations being reused by
        // parallel fixtures or outgoing HTTP sockets before the child binds.
        let listener = (0..20_000)
            .find_map(|_| {
                let index =
                    u64::from(std::process::id()) + NEXT_PORT.fetch_add(1, Ordering::Relaxed);
                TcpListener::bind(("127.0.0.1", 10_000 + (index % 20_000) as u16)).ok()
            })
            .ok_or("no free test port")?;
        let address = listener.local_addr()?;
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
            .stderr(fs::File::create(directory.join("server.log"))?)
            .spawn()?;
        let server = Self {
            child,
            endpoint: format!("http://{address}"),
            directory,
        };
        eventually(|| Ok(server.request("rpc.discover", Value::Null).is_ok()))?;
        Ok(server)
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
        Ok(response.at("result"))
    }

    fn rpc(&self, method: &str, params: Value) -> Result<Value, String> {
        self.request(method, params)
            .map_err(|error| format!("{method}: {error}"))
    }

    fn query(&self, component: &str) -> Result<Value, String> {
        let rows = self.rpc("world.query", json!({"data":{"components":[component]}}))?;
        if !rows.is_array() {
            return Err(format!("world.query {component} did not return rows"));
        }
        Ok(rows)
    }

    /// Sends one `Command` as JSON; see `control.rs` for the shapes.
    fn control(&self, viewer: u64, command: Value) -> Result<(), String> {
        self.rpc(
            "world.trigger_event",
            json!({"event":"fux::control::Control","value":{"viewer":viewer,"command":command}}),
        )?;
        Ok(())
    }
    /// A command that is only its kind.
    fn command(&self, viewer: u64, kind: &str) -> Result<(), String> {
        self.control(viewer, json!({"kind":kind}))
    }
    fn scoped(&self, viewer: u64, kind: &str, scope: &str) -> Result<(), String> {
        self.control(viewer, json!({"kind":kind,"scope":scope}))
    }
    fn split(&self, viewer: u64, axis: &str, program: Option<&str>) -> Result<(), String> {
        self.control(
            viewer,
            json!({"kind":"split","axis":axis,"program":program}),
        )
    }
    fn focus(&self, viewer: u64, pane: u64) -> Result<(), String> {
        self.control(viewer, json!({"kind":"focus","pane":pane}))
    }
    fn tab_new(&self, viewer: u64, name: Option<&str>) -> Result<(), String> {
        self.control(viewer, json!({"kind":"tab_new","name":name}))
    }
    fn close(&self, viewer: u64, subject: Value) -> Result<(), String> {
        self.control(viewer, json!({"kind":"close","subject":subject}))
    }
    /// One relationship component of a viewer, or null when it has none.
    fn relation(&self, viewer: u64, component: &str) -> Result<Value, String> {
        Ok(self
            .query(component)?
            .rows()
            .find(|row| row.at("entity") == viewer)
            .map_or(Value::Null, |row| row.at("components").at(component)))
    }
    fn viewing(&self, viewer: u64) -> Result<Value, String> {
        self.relation(viewer, "fux::model::Viewing")
    }
    fn on_tab(&self, viewer: u64) -> Result<Value, String> {
        self.relation(viewer, "fux::model::OnTab")
    }
    fn focused(&self, viewer: u64) -> Result<Value, String> {
        self.relation(viewer, "fux::model::Focused")
    }
    /// The workspace this viewer is looking at.
    fn workspace_of(&self, viewer: u64) -> Result<u64, String> {
        self.viewing(viewer)?
            .as_u64()
            .ok_or_else(|| "viewer has no workspace".into())
    }

    fn input(&self, viewer: u64, input: Value) -> Result<(), String> {
        self.rpc(
            "world.trigger_event",
            json!({"event":"fux::control::UserInput","value":{"viewer":viewer,"input":input}}),
        )?;
        Ok(())
    }

    fn enter(&self, viewer: u64) -> Result<(), String> {
        self.input(
            viewer,
            json!({"kind":"key","key":"enter","ctrl":false,"alt":false,"shift":false}),
        )
    }

    fn attach(&self) -> Result<u64, String> {
        self.rpc("fux.attach", json!({"rows":24,"cols":80}))?
            .at("viewer")
            .as_u64()
            .ok_or_else(|| "fux.attach did not return a viewer".into())
    }

    fn screen(&self, viewer: u64) -> Result<String, String> {
        let frame = self.rpc("fux.frame", json!({"viewer":viewer}))?;
        let paint = frame.at("paint");
        let mut parser = fux_vt::Parser::new(24, 80, 0).need()?;
        parser
            .process(paint.as_str().ok_or("frame has no paint")?.as_bytes())
            .need()?;
        Ok(parser.screen().contents())
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

/// Polls until the observation holds; an observation error or timeout is the failure.
#[track_caller]
fn eventually(mut observed: impl FnMut() -> Result<bool, Fail>) -> Result<(), Fail> {
    let location = std::panic::Location::caller();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !observed()? {
        if Instant::now() >= deadline {
            return Err(format!(
                "observable state did not settle within five seconds at {location}"
            )
            .into());
        }
        thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

fn alive(pid: i32) -> bool {
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_ok()
}

#[test]
fn stock_launch_removal_settles_without_another_request() -> Outcome {
    let server = Server::start()?;
    let pid_file = server.directory.join("child.pid");
    let entity = server.rpc("world.spawn_entity", json!({"components":{
        "fux::model::Launch":{"argv":["/bin/sh","-c",format!("echo $$ > '{}'; exec sleep 60",pid_file.display())],"cwd":server.directory,"history_lines":20}
    }}))?.at("entity").as_u64().need()?;
    // Observe the OS, not another RPC: a later request would mask a missing wake.
    eventually(|| Ok(fs::read_to_string(&pid_file).is_ok_and(|text| !text.trim().is_empty())))?;
    let pid = fs::read_to_string(pid_file)?.trim().parse()?;
    assert!(alive(pid));
    server.rpc(
        "world.remove_components",
        json!({"entity":entity,"components":["fux::model::Launch"]}),
    )?;
    eventually(|| Ok(!alive(pid)))?;
    let states = server.query("fux::model::ProcessState")?;
    let state = &states
        .rows()
        .find(|row| row.at("entity") == entity)
        .need()?
        .at("components")
        .at("fux::model::ProcessState");
    assert!(state.at("status").at("pid").is_null());
    assert_eq!(state.at("status").at("kind"), "exited", "{state}");
    assert!(state.at("status").at("code").is_number(), "{state}");
    Ok(())
}

#[test]
fn interactive_background_jobs_hang_up_when_pane_terminates() -> Outcome {
    let server = Server::start()?;
    let viewer = server.attach()?;
    let shell = server
        .query("fux::model::ProcessState")?
        .at(0)
        .at("components")
        .at("fux::model::ProcessState")
        .at("status")
        .at("pid")
        .as_i64()
        .need()? as i32;
    let pid_file = server.directory.join("background.pid");
    server.input(
        viewer,
        json!({"kind":"paste","text":format!("sleep 60 & echo $! > '{}'",pid_file.display())}),
    )?;
    server.enter(viewer)?;
    eventually(|| Ok(fs::read_to_string(&pid_file).is_ok_and(|text| !text.trim().is_empty())))?;
    let background: i32 = fs::read_to_string(pid_file)?.trim().parse()?;
    assert_ne!(
        nix::unistd::getpgid(Some(nix::unistd::Pid::from_raw(background)))?.as_raw(),
        shell
    );
    server.command(viewer, "terminate")?;
    eventually(|| Ok(!alive(shell) && !alive(background)))?;
    let state = &server
        .query("fux::model::ProcessState")?
        .at(0)
        .at("components")
        .at("fux::model::ProcessState");
    assert!(state.at("status").at("pid").is_null());
    assert_eq!(state.at("status").at("kind"), "exited", "{state}");
    assert!(state.at("status").at("code").is_number(), "{state}");
    Ok(())
}

#[test]
fn layout_mapping_and_prompt_paste_preserve_live_process_identity() -> Outcome {
    let server = Server::start()?;
    let viewer = server.attach()?;
    let first = server
        .query("fux::model::Launch")?
        .at(0)
        .at("entity")
        .as_u64()
        .need()?;
    server.command(viewer, "zoom")?;
    server.control(viewer, json!({"kind":"scroll","order":"previous"}))?;
    server.split(viewer, "horizontal", None)?;
    let screen = server.screen(viewer)?;
    let chrome = screen.lines().last().need()?;
    assert!(!chrome.contains("zoom"));
    assert!(!chrome.contains('↑'));
    let launches = server.query("fux::model::Launch")?;
    let second = launches
        .rows()
        .filter_map(|row| row.at("entity").as_u64())
        .find(|entity| *entity != first)
        .need()?;
    for (entity, name) in [(first, "alpha"), (second, "beta")] {
        server.rpc(
            "world.insert_components",
            json!({"entity":entity,"components":{"bevy_ecs::name::Name":name}}),
        )?;
    }
    // Distinct real terminal contents, not decorative pane-header labels.
    for marker in ["BETA", "ALPHA"] {
        server.input(
            viewer,
            json!({"kind":"paste","text":format!("printf '\\033[2J\\033[H{marker}\\n'")}),
        )?;
        server.enter(viewer)?;
        eventually(|| Ok(server.screen(viewer)?.contains(marker)))?;
        server.command(viewer, "focus_next")?;
        server.screen(viewer)?;
    }
    let before = server.query("fux::model::ProcessState")?;
    let scene = server.directory.join("layout.scn.ron");
    let workspace = server.workspace_of(viewer)?;
    server.control(
        viewer,
        json!({"kind":"save_layout","workspace":workspace,"path":scene}),
    )?;
    eventually(|| Ok(fs::metadata(&scene).is_ok_and(|metadata| metadata.len() > 0)))?;
    server.control(viewer, json!({
        "kind":"load_layout","workspace":workspace,"path":scene,"mapping":[[first,second],[second,first]]
    }))?;
    eventually(|| Ok(server.screen(viewer)?.contains("loaded ")))?;
    let screen = server.screen(viewer)?;
    let content = screen.lines().next().need()?;
    assert!(
        content.find("BETA").need()? < content.find("ALPHA").need()?,
        "{screen}"
    );
    let after = server.query("fux::model::ProcessState")?;
    for old in before.rows() {
        let new = after
            .rows()
            .find(|row| row.at("entity") == old.at("entity"))
            .need()?;
        assert_eq!(
            old.at("components")
                .at("fux::model::ProcessState")
                .at("status")
                .at("pid"),
            new.at("components")
                .at("fux::model::ProcessState")
                .at("status")
                .at("pid")
        );
    }
    let focus = server.focused(viewer)?.as_u64().need()?;
    server.rpc(
        "world.insert_components",
        json!({"entity":focus,"components":{"bevy_camera::visibility::Visibility":"Hidden"}}),
    )?;
    eventually(|| Ok(!server.screen(viewer)?.contains("BETA")))?;
    server.rpc(
        "world.remove_components",
        json!({"entity":focus,"components":["bevy_camera::visibility::Visibility"]}),
    )?;
    eventually(|| Ok(server.screen(viewer)?.contains("BETA")))?;
    // The first load replaced the workspace entity; a command names the live one.
    let workspace = server.workspace_of(viewer)?;
    server.control(
        viewer,
        json!({"kind":"load_layout","workspace":workspace,"path":scene,"mapping":[[first,first+1_000_000]]}),
    )?;
    eventually(|| Ok(server.screen(viewer)?.contains("missing live pane")))?;
    let remaining = server.query("fux::model::PaneView")?;
    assert!(remaining.rows().any(|row| row.at("entity") == focus));
    assert_eq!(remaining.rows().count(), 2);
    Ok(())
}

#[test]
fn stock_viewer_removal_releases_its_native_size_constraint() -> Outcome {
    let server = Server::start()?;
    let viewer = server.attach()?;
    for despawn in [false, true] {
        let small = server
            .rpc("fux.attach", json!({"rows":12,"cols":40}))?
            .at("viewer")
            .as_u64()
            .need()?;
        server.screen(viewer)?;
        server.input(viewer, json!({"kind":"paste","text":"stty size"}))?;
        server.enter(viewer)?;
        eventually(|| Ok(server.screen(viewer)?.contains("11 40")))?;
        if despawn {
            server.rpc("world.despawn_entity", json!({"entity":small}))?;
        } else {
            server.rpc(
                "world.remove_components",
                json!({"entity":small,"components":["fux::model::Viewer"]}),
            )?;
        }
        assert_eq!(
            server.rpc("fux.frame", json!({"viewer":small}))?,
            json!({"paint":"", "detach":true})
        );
        server.screen(viewer)?;
        server.input(viewer, json!({"kind":"paste","text":"clear; stty size"}))?;
        server.enter(viewer)?;
        eventually(|| Ok(server.screen(viewer)?.contains("23 80")))?;
    }
    Ok(())
}

#[test]
fn repeated_copy_effects_are_not_replaceable_paints() -> Outcome {
    use std::io::{BufRead, BufReader};
    let server = Server::start()?;
    let viewer = server.attach()?;
    let endpoint = server.endpoint.clone();
    let (sender, received) = std::sync::mpsc::channel();
    let reader = thread::spawn(move || -> Result<(), String> {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(5)))
            .build()
            .into();
        let response = agent
            .post(&endpoint)
            .send_json(json!({
                "jsonrpc":"2.0", "id":2, "method":"fux.frame+watch", "params":{"viewer":viewer}
            }))
            .need()?;
        for line in BufReader::new(response.into_body().into_reader()).lines() {
            let line = line.need()?;
            if let Some(data) = line.strip_prefix("data:") {
                let value: Value = serde_json::from_str(data).need()?;
                let paint = value.at("result").at("paint");
                let paint = paint.as_str().need()?;
                if sender.send(paint.matches("\x1b]52;").count()).is_err() {
                    break;
                }
            }
        }
        Ok(())
    });
    assert_eq!(received.recv_timeout(Duration::from_secs(5))?, 0);
    // These are explicit equal copies, not four interchangeable paint snapshots.
    for _ in 0..4 {
        server.command(viewer, "copy")?;
    }
    let mut copies = 0;
    while copies < 4 {
        copies += received.recv_timeout(Duration::from_secs(5))?;
    }
    assert_eq!(copies, 4);
    // Direct snapshots must not replay effects already delivered to the stream.
    assert!(
        !server
            .rpc("fux.frame", json!({"viewer":viewer}))?
            .at("paint")
            .as_str()
            .need()?
            .contains("\x1b]52;")
    );
    drop(received);
    let workspace = server.workspace_of(viewer)?;
    server.control(
        viewer,
        json!({"kind":"rename","subject":{"workspace":workspace},"name":"reader-finished"}),
    )?;
    reader.join().map_err(|_| "watch reader panicked")??;
    Ok(())
}

#[test]
fn blocked_terminal_paint_does_not_block_stream_drain() -> Outcome {
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

    let server = Server::start()?;
    let pair = native_pty_system().openpty(PtySize {
        rows: 24,
        cols: 80,
        pixel_width: 0,
        pixel_height: 0,
    })?;
    let mut reader = pair.master.try_clone_reader()?;
    let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_fux"));
    command.arg("attach");
    command.env("FUX_ENDPOINT", &server.endpoint);
    let mut terminal = SlowTerminal {
        child: {
            let _spawn = SPAWN.lock().unwrap_or_else(|error| error.into_inner());
            pair.slave.spawn_command(command)?
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
            output.extend_from_slice(chunk.get(..count).unwrap_or_default());
            if output.windows(8).any(|bytes| bytes == b"\x1b[?2026l") {
                let _ = ready.send(());
                return;
            }
        }
    });
    received.recv_timeout(Duration::from_secs(5))?;
    first_paint
        .join()
        .map_err(|_| "first paint reader panicked")?;
    let viewer = server
        .query("fux::model::Viewer")?
        .at(0)
        .at("entity")
        .as_u64()
        .need()?;
    let original = server
        .query("fux::model::ProcessState")?
        .at(0)
        .at("entity")
        .clone();
    server.split(viewer, "vertical", Some("exec /usr/bin/yes load"))?;
    // Deliberately stop reading the outer PTY. Paint will block, but HTTP must
    // continue draining/coalescing rather than fill Bevy's watch channel.
    thread::sleep(Duration::from_millis(500));
    assert!(
        server
            .query("fux::model::Viewer")?
            .rows()
            .any(|row| row.at("entity") == viewer)
    );
    assert!(terminal.child.try_wait()?.is_none());
    let states = server.query("fux::model::ProcessState")?;
    let hot = states
        .rows()
        .find(|row| row.at("entity") != original)
        .need()?;
    let entity = hot.at("entity");
    let pid = hot
        .at("components")
        .at("fux::model::ProcessState")
        .at("status")
        .at("pid")
        .as_u64()
        .need()? as i32;
    server.command(viewer, "terminate")?;
    eventually(|| Ok(!alive(pid)))?;
    let states = server.query("fux::model::ProcessState")?;
    let state = &states
        .rows()
        .find(|row| row.at("entity") == entity)
        .need()?
        .at("components")
        .at("fux::model::ProcessState");
    assert!(state.at("status").at("pid").is_null(), "{state}");
    assert_eq!(state.at("status").at("kind"), "exited", "{state}");
    assert!(state.at("status").at("code").is_number(), "{state}");
    Ok(())
}
