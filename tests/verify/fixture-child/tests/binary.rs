#![allow(clippy::expect_used, clippy::panic)]

use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use serde_json::{Value, json};
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

#[allow(dead_code)]
#[path = "../../mod.rs"]
mod verification;

const DEADLINE: Duration = Duration::from_secs(20);

#[test]
fn serialized_input_scenarios_agree_through_model_in_process_and_real_binaries() {
    use verification::schema::Scenario;

    for (name, source) in [
        (
            "prefix-literal",
            include_str!("../../corpus/input/prefix_literal.json"),
        ),
        (
            "prefix-paste",
            include_str!("../../corpus/input/prefix_and_paste.json"),
        ),
        (
            "signal-hup",
            include_str!("../../corpus/input/signal_hup.json"),
        ),
        (
            "signal-int",
            include_str!("../../corpus/input/signal_int.json"),
        ),
        (
            "signal-term",
            include_str!("../../corpus/input/signal_term.json"),
        ),
        (
            "signal-kill",
            include_str!("../../corpus/input/signal_kill.json"),
        ),
        (
            "kill-pane",
            include_str!("../../corpus/input/kill_pane.json"),
        ),
        (
            "ws",
            include_str!("../../corpus/input/workspace_lifecycle.json"),
        ),
        (
            "wsc",
            include_str!("../../corpus/input/workspace_shutdown_cleanup.json"),
        ),
        (
            "wss",
            include_str!("../../corpus/input/workspace_switch.json"),
        ),
    ] {
        let scenario: Scenario = serde_json::from_str(source).expect("strict scenario");
        assert_binary_scenario(&scenario, name);
    }
}

fn assert_binary_scenario(scenario: &verification::schema::Scenario, environment_name: &str) {
    use verification::interpreters::{
        BinaryInterpreter, InProcessInterpreter, Interpreter, ModelInterpreter,
    };

    let model = ModelInterpreter.run(scenario).expect("model transcript");
    assert_eq!(
        InProcessInterpreter
            .run(scenario)
            .expect("in-process transcript"),
        model
    );

    let fux = binary("FUX_BIN", "target/debug/fux");
    let zor = binary("ZOR_BIN", "zor/target/debug/zor");
    let fixture_program = PathBuf::from(env!("CARGO_BIN_EXE_fux-fixture-child"));
    let environment = PrivateEnvironment::new(environment_name);
    let fixture_listener =
        UnixListener::bind(environment.fixture_socket()).expect("fixture socket");
    environment.write_config(&fixture_program, &zor);
    let config_path = environment.root.join("config/fux/config.toml");
    let config = fs::read_to_string(&config_path).expect("read private config");
    fs::write(&config_path, format!("prefix = 'C-b'\n{config}")).expect("set scenario prefix");
    let id = run(&fux, ["id"], &environment);
    let allow = String::from_utf8(id.stdout)
        .expect("endpoint id")
        .trim()
        .to_owned();
    let driver = PrefixBinaryDriver {
        fux: fux.clone(),
        allow,
        client: None,
        client_workspace: None,
        primary: None,
        primary_pid: None,
        secondary: Vec::new(),
        workspaces: std::collections::BTreeMap::new(),
        listener: &fixture_listener,
        server: None,
        environment: &environment,
        mouse_enabled: false,
        subscribers: Vec::new(),
        client_size: scenario.initial_size,
        detached_topology: None,
        retained_output: None,
        lifecycle_subscriber: None,
        disconnected_viewer: None,
        primary_exited: false,
    };
    let binary = BinaryInterpreter::new(driver)
        .run(scenario)
        .expect("binary transcript");
    assert_eq!(binary, model);
}

struct PrefixBinaryDriver<'a> {
    fux: PathBuf,
    allow: String,
    client: Option<TerminalChild>,
    client_workspace: Option<String>,
    primary: Option<Jsonl>,
    primary_pid: Option<i32>,
    secondary: Vec<Jsonl>,
    workspaces: std::collections::BTreeMap<String, WorkspaceFixture>,
    listener: &'a UnixListener,
    server: Option<OwnedChild>,
    environment: &'a PrivateEnvironment,
    mouse_enabled: bool,
    subscribers: Vec<(u64, Jsonl)>,
    client_size: verification::schema::Size,
    detached_topology: Option<Value>,
    retained_output: Option<String>,
    lifecycle_subscriber: Option<Jsonl>,
    disconnected_viewer: Option<u64>,
    primary_exited: bool,
}

struct WorkspaceFixture {
    control: Jsonl,
    pid: i32,
}

impl PrefixBinaryDriver<'_> {
    fn primary_mut(&mut self) -> Result<&mut Jsonl, String> {
        self.primary
            .as_mut()
            .ok_or_else(|| "binary daemon is not running".to_owned())
    }
}

impl verification::interpreters::BinaryDriver for PrefixBinaryDriver<'_> {
    fn start_daemon(&mut self) -> Result<(), String> {
        if self.server.is_some() || self.primary.is_some() {
            return Err("binary daemon started twice".into());
        }
        self.server = Some(OwnedChild::spawn(
            Command::new(&self.fux)
                .args(["serve", "--allow", &self.allow, "--name", "binary"])
                .env_clear()
                .envs(self.environment.variables())
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        ));
        wait_for_path(&self.environment.manager_socket());
        wait_for_path(&self.environment.control_socket());
        let mut primary = Jsonl::new(accept_with_deadline(self.listener));
        let ready = primary.receive();
        if ready["event"] != "ready" {
            return Err("primary fixture did not become ready".into());
        }
        self.primary_pid = ready["pid"]
            .as_u64()
            .and_then(|pid| i32::try_from(pid).ok());
        if self.primary_pid.is_none() {
            return Err(format!("primary fixture omitted a valid pid: {ready}"));
        }
        self.primary = Some(primary);
        Ok(())
    }

    fn attach(&mut self, client: &str) -> Result<(), String> {
        if client != "alice" {
            return Err(format!("binary fixture has no client {client:?}"));
        }
        if self.primary.is_none() || self.server.is_none() || self.client.is_some() {
            return Err(format!("client {client:?} cannot attach"));
        }
        let process = TerminalChild::spawn(
            &self.fux,
            self.environment,
            self.client_size.rows,
            self.client_size.columns,
        );
        process.wait_for_output_bytes(b"connected.");
        self.client = Some(process);
        self.client_workspace = Some("binary".into());
        Ok(())
    }

    fn detach(&mut self, client: &str) -> Result<(), String> {
        if client != "alice" {
            return Err(format!("binary fixture has no client {client:?}"));
        }
        let workspace = self
            .client_workspace
            .as_deref()
            .ok_or_else(|| format!("client {client:?} workspace is unknown"))?;
        self.detached_topology = Some(workspace_topology(
            &self.fux,
            self.environment,
            workspace,
        )?);
        let mut process = self
            .client
            .take()
            .ok_or_else(|| format!("client {client:?} is already detached"))?;
        process.detach_with_prefix(2);
        Ok(())
    }

    fn reconnect(&mut self, client: &str) -> Result<(), String> {
        if client != "alice" || self.client.is_some() {
            return Err(format!("client {client:?} cannot reconnect"));
        }
        let workspace = self
            .client_workspace
            .as_deref()
            .ok_or_else(|| format!("client {client:?} has no previous workspace"))?;
        let process = TerminalChild::spawn_workspace(
            &self.fux,
            self.environment,
            self.client_size.rows,
            self.client_size.columns,
            workspace,
        );
        process.wait_for_output_bytes(b"connected.");
        if let Some(subscriber) = self.lifecycle_subscriber.as_mut() {
            let attached = expect_viewer_event(subscriber, 91, "client.attached")?;
            if self.disconnected_viewer == Some(attached) {
                return Err(format!("reconnect reused stale viewer identity {attached}"));
            }
        }
        let before = self
            .detached_topology
            .take()
            .ok_or_else(|| "reconnect had no detached workspace snapshot".to_owned())?;
        let after = workspace_topology(&self.fux, self.environment, workspace)?;
        if after != before {
            return Err(format!(
                "workspace topology changed across reconnect: before={before}, after={after}"
            ));
        }
        if let Some(expected) = self.retained_output.as_deref() {
            wait_for_captured_line(&self.fux, self.environment, 1, expected)?;
        }
        self.client = Some(process);
        Ok(())
    }

    fn disconnect(&mut self, client: &str) -> Result<(), String> {
        if client != "alice" {
            return Err(format!("binary fixture has no client {client:?}"));
        }
        let workspace = self
            .client_workspace
            .as_deref()
            .ok_or_else(|| format!("client {client:?} workspace is unknown"))?;
        let stream = UnixStream::connect(self.environment.workspace_control_socket(workspace))
            .map_err(|error| error.to_string())?;
        let mut subscriber = Jsonl::new(stream);
        subscriber.send(json!({
            "command": "subscribe",
            "id": 91,
            "events": ["client.attached", "client.detached"],
        }));
        let accepted = subscriber.receive();
        if accepted["id"] != 91 || accepted["status"] != "accepted" {
            return Err(format!("lifecycle subscription was not accepted: {accepted}"));
        }
        self.detached_topology = Some(workspace_topology(
            &self.fux,
            self.environment,
            workspace,
        )?);
        let mut process = self
            .client
            .take()
            .ok_or_else(|| format!("client {client:?} is already disconnected"))?;
        process.disconnect()?;
        self.disconnected_viewer = Some(expect_viewer_event(
            &mut subscriber,
            91,
            "client.detached",
        )?);
        self.lifecycle_subscriber = Some(subscriber);
        Ok(())
    }

    fn create_workspace(&mut self, workspace: &str) -> Result<(), String> {
        if workspace == "binary" || self.workspaces.contains_key(workspace) {
            return Err(format!("binary workspace {workspace:?} already exists"));
        }
        let output = run(
            &self.fux,
            ["workspace", "new", workspace],
            self.environment,
        );
        if !output.status.success() {
            return Err(format!(
                "workspace creation failed: stdout={} stderr={}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        let mut control = Jsonl::new(accept_with_deadline(self.listener));
        let ready = control.receive();
        if ready["event"] != "ready" {
            return Err(format!("workspace fixture did not become ready: {ready}"));
        }
        let pid = ready["pid"]
            .as_u64()
            .and_then(|pid| i32::try_from(pid).ok())
            .ok_or_else(|| format!("workspace fixture omitted a valid pid: {ready}"))?;
        wait_for_path(&self.environment.workspace_descriptor(workspace));
        self.workspaces
            .insert(workspace.to_owned(), WorkspaceFixture { control, pid });
        Ok(())
    }

    fn select_workspace(&mut self, workspace: &str) -> Result<(), String> {
        if workspace != "binary" && !self.workspaces.contains_key(workspace) {
            return Err(format!("binary workspace {workspace:?} does not exist"));
        }
        let output = run(
            &self.fux,
            [workspace, "list"],
            self.environment,
        );
        if !output.status.success() {
            return Err(format!(
                "workspace selection failed: stdout={} stderr={}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        let value: Value = serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())?;
        if value.pointer("/result/value/workspaces/0/name").and_then(Value::as_str)
            != Some(workspace)
        {
            return Err(format!("selected workspace listing was not authoritative: {value}"));
        }
        Ok(())
    }

    fn delete_workspace(&mut self, workspace: &str) -> Result<(), String> {
        if self.client.is_some() && self.client_workspace.as_deref() == Some(workspace) {
            return Err(format!("binary workspace {workspace:?} is attached"));
        }
        let fixture = if workspace == "binary" {
            if self.primary_exited || self.primary.is_none() {
                return Err(format!("binary workspace {workspace:?} does not exist"));
            }
            None
        } else {
            Some(
                self.workspaces
                    .remove(workspace)
                    .ok_or_else(|| format!("binary workspace {workspace:?} does not exist"))?,
            )
        };
        let output = run(
            &self.fux,
            ["workspace", "kill", workspace],
            self.environment,
        );
        if !output.status.success() {
            if let Some(fixture) = fixture {
                self.workspaces.insert(workspace.to_owned(), fixture);
            }
            return Err(format!(
                "workspace deletion failed: stdout={} stderr={}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        wait_for_absent(&self.environment.workspace_descriptor(workspace));
        wait_for_absent(&self.environment.workspace_control_socket(workspace));
        let pid = fixture
            .as_ref()
            .map_or(self.primary_pid, |fixture| Some(fixture.pid))
            .ok_or_else(|| format!("workspace {workspace:?} fixture pid is unavailable"))?;
        wait_for_process_absent(pid);
        if workspace == "binary" {
            self.primary_exited = true;
            self.primary = None;
            self.primary_pid = None;
        } else if let Some(fixture) = fixture {
            drop(fixture.control);
        }
        Ok(())
    }

    fn switch_workspace(&mut self, client: &str, workspace: &str) -> Result<(), String> {
        if client != "alice" || self.client.is_none() {
            return Err(format!("binary client {client:?} is not attached"));
        }
        if workspace != "binary" && !self.workspaces.contains_key(workspace) {
            return Err(format!("binary workspace {workspace:?} does not exist"));
        }
        let current = self
            .client_workspace
            .clone()
            .ok_or_else(|| "binary client workspace is unknown".to_owned())?;
        let mut detached = Jsonl::new(
            UnixStream::connect(self.environment.workspace_control_socket(&current))
                .map_err(|error| error.to_string())?,
        );
        detached.send(json!({
            "command": "subscribe",
            "id": 97,
            "events": ["client.detached"],
        }));
        let accepted = detached.receive();
        if accepted["id"] != 97 || accepted["status"] != "accepted" {
            return Err(format!("source workspace lifecycle subscription failed: {accepted}"));
        }
        let mut attached = Jsonl::new(
            UnixStream::connect(self.environment.workspace_control_socket(workspace))
                .map_err(|error| error.to_string())?,
        );
        attached.send(json!({
            "command": "subscribe",
            "id": 98,
            "events": ["client.attached"],
        }));
        let accepted = attached.receive();
        if accepted["id"] != 98 || accepted["status"] != "accepted" {
            return Err(format!("target workspace lifecycle subscription failed: {accepted}"));
        }
        let output = run(&self.fux, ["workspace", "list"], self.environment);
        let listing: Value = serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())?;
        let names = listing["names"]
            .as_array()
            .ok_or_else(|| format!("manager workspace list omitted names: {listing}"))?;
        let selection = names
            .iter()
            .position(|name| name.as_str() == Some(workspace))
            .map(|index| format!("{}\n", index + 1))
            .ok_or_else(|| format!("manager did not list workspace {workspace:?}: {listing}"))?;

        let terminal = self.client.as_mut().ok_or("client is detached")?;
        terminal.write(&[2, b's']);
        terminal.wait_for_text("workspaces:");
        terminal.write(selection.as_bytes());
        expect_viewer_event(&mut detached, 97, "client.detached")?;
        expect_viewer_event(&mut attached, 98, "client.attached")?;
        self.client_workspace = Some(workspace.to_owned());
        Ok(())
    }

    fn child_output(
        &mut self,
        pane: u32,
        bytes: &[u8],
    ) -> Result<verification::schema::ExpectedTerminalFrame, String> {
        if pane != 1 {
            return Err(format!("binary fixture has no pane {pane}"));
        }
        let chunk = std::str::from_utf8(bytes)
            .map_err(|error| format!("binary child output is not UTF-8: {error}"))?;
        self.primary_mut()?.send(json!({
            "command": "write",
            "chunks_hex": [verification::transcript::hex(bytes)],
        }));
        if self.primary_mut()?.receive()["bytes"] != bytes.len() {
            return Err("fixture reported a short child output write".into());
        }
        let mut expected = self.retained_output.take().unwrap_or_default();
        expected.push_str(chunk);
        let observed = wait_for_captured_line(&self.fux, self.environment, pane, &expected)?;
        self.retained_output = Some(observed.clone());
        let observed_size = fixture_size(self.primary_mut()?);
        let workspace = self.client_workspace.as_deref().unwrap_or("binary");
        let semantics = terminal_semantics(&self.fux, self.environment, workspace, pane)?;
        Ok(verification::schema::ExpectedTerminalFrame {
            rows: observed_size.0,
            columns: observed_size.1,
            cells: vec![observed],
            cursor: semantics.cursor,
            synchronized: None,
            modes: semantics.modes,
            status: semantics.status,
            selection: semantics.selection,
            prediction_target: semantics.prediction_target,
        })
    }

    fn terminal_reply(
        &mut self,
        pane: u32,
        query: &[u8],
        expected: &[u8],
    ) -> Result<Vec<u8>, String> {
        if pane != 1 {
            return Err(format!("binary fixture has no pane {pane}"));
        }
        self.primary_mut()?.send(json!({
            "command": "query",
            "bytes_hex": verification::transcript::hex(query),
            "reply_bytes": expected.len(),
            "withhold": false,
        }));
        let response = self.primary_mut()?.receive();
        let encoded = response["bytes_hex"]
            .as_str()
            .ok_or_else(|| format!("fixture query omitted reply bytes: {response}"))?;
        decode_hex(encoded)
    }

    fn copy_input(&mut self, client: &str, bytes: &[u8]) -> Result<(), String> {
        if client != "alice" || self.client.is_none() {
            return Err(format!("copy_input references unattached client {client:?}"));
        }
        if bytes != b"q" {
            return Err("binary copy_input currently supports exiting with q".into());
        }
        self.client
            .as_mut()
            .ok_or("client is detached")?
            .write(bytes);
        Ok(())
    }

    fn child_exit(&mut self, pane: u32, status: i32) -> Result<i32, String> {
        if pane != 1 || self.primary_exited {
            return Err(format!("binary cannot exit pane {pane}"));
        }
        let stream = UnixStream::connect(self.environment.control_socket())
            .map_err(|error| error.to_string())?;
        let mut subscriber = Jsonl::new(stream);
        subscriber.send(json!({
            "command": "subscribe",
            "id": 93,
            "events": ["pane.closed"],
        }));
        let accepted = subscriber.receive();
        if accepted["id"] != 93 || accepted["status"] != "accepted" {
            return Err(format!("pane-close subscription was not accepted: {accepted}"));
        }
        self.primary_mut()?
            .send(json!({"command": "exit", "status": status}));
        let cleanup = self.primary_mut()?.receive();
        if cleanup["event"] != "cleanup" || cleanup["descendants"] != 0 {
            return Err(format!("primary fixture exit did not clean up: {cleanup}"));
        }
        self.primary_exited = true;
        let event = subscriber.receive();
        if event["event"] != "pane.closed"
            || event["id"] != 93
            || event["pane"] != pane
            || event["exit_status"] != status
        {
            return Err(format!("unexpected raw pane-close event: {event}"));
        }
        Ok(status)
    }

    fn signal(
        &mut self,
        pane: u32,
        signal: verification::schema::Signal,
    ) -> Result<i32, String> {
        if pane != 1 || self.primary_exited {
            return Err(format!("binary cannot signal pane {pane}"));
        }
        let stream = UnixStream::connect(self.environment.control_socket())
            .map_err(|error| error.to_string())?;
        let mut subscriber = Jsonl::new(stream);
        subscriber.send(json!({
            "command": "subscribe",
            "id": 94,
            "events": ["pane.closed"],
        }));
        let accepted = subscriber.receive();
        if accepted["id"] != 94 || accepted["status"] != "accepted" {
            return Err(format!("pane-close subscription was not accepted: {accepted}"));
        }
        let native = match signal {
            verification::schema::Signal::Hup => Signal::SIGHUP,
            verification::schema::Signal::Int => Signal::SIGINT,
            verification::schema::Signal::Term => Signal::SIGTERM,
            verification::schema::Signal::Kill => Signal::SIGKILL,
        };
        kill(
            Pid::from_raw(
                self.primary_pid
                    .ok_or_else(|| "primary fixture pid is unavailable".to_owned())?,
            ),
            native,
        )
        .map_err(|error| error.to_string())?;
        self.primary_exited = true;
        self.primary = None;
        let event = subscriber.receive();
        let status = event["exit_status"]
            .as_i64()
            .and_then(|status| i32::try_from(status).ok())
            .ok_or_else(|| format!("pane-close signal event omitted status: {event}"))?;
        if event["event"] != "pane.closed" || event["id"] != 94 || event["pane"] != pane {
            return Err(format!("unexpected raw pane-close event: {event}"));
        }
        Ok(status)
    }

    fn kill_pane(&mut self, pane: u32) -> Result<i32, String> {
        if pane != 1 || self.primary_exited {
            return Err(format!("binary cannot kill pane {pane}"));
        }
        let mut subscriber = Jsonl::new(
            UnixStream::connect(self.environment.control_socket())
                .map_err(|error| error.to_string())?,
        );
        subscriber.send(json!({
            "command": "subscribe",
            "id": 95,
            "events": ["pane.closed"],
        }));
        let accepted = subscriber.receive();
        if accepted["id"] != 95 || accepted["status"] != "accepted" {
            return Err(format!("pane-close subscription was not accepted: {accepted}"));
        }
        let mut control = Jsonl::new(
            UnixStream::connect(self.environment.control_socket())
                .map_err(|error| error.to_string())?,
        );
        control.send(json!({"command": "kill", "id": 96, "pane": pane}));
        let reply = control.receive();
        if reply["id"] != 96 || reply["status"] != "completed" {
            return Err(format!("pane kill did not complete: {reply}"));
        }
        let event = subscriber.receive();
        let status = event["exit_status"]
            .as_i64()
            .and_then(|status| i32::try_from(status).ok())
            .ok_or_else(|| format!("pane-close kill event omitted status: {event}"))?;
        if event["event"] != "pane.closed" || event["id"] != 95 || event["pane"] != pane {
            return Err(format!("unexpected raw pane-close event: {event}"));
        }
        self.primary_exited = true;
        self.primary = None;
        Ok(status)
    }

    fn input(
        &mut self,
        bytes: &[u8],
    ) -> Result<Vec<verification::interpreters::ObservedAction>, String> {
        use verification::interpreters::ObservedAction;
        if bytes == [2, b'['] {
            self.client.as_mut().ok_or("client is detached")?.write(bytes);
            let workspace = self.client_workspace.as_deref().unwrap_or("binary");
            let deadline = Instant::now() + DEADLINE;
            loop {
                let semantics = terminal_semantics(&self.fux, self.environment, workspace, 1)?;
                if semantics.selection.is_some() {
                    break;
                }
                if Instant::now() >= deadline {
                    return Err("copy mode did not become observable".into());
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            return Ok(vec![ObservedAction::Command("copy_mode".into())]);
        }
        if bytes == [2, b'|'] || bytes == [2, b'|', 2, b'-'] {
            self.client.as_mut().ok_or("client is detached")?.write(bytes);
            let split_count: usize = if bytes.len() == 2 { 1 } else { 2 };
            for _ in 0..split_count {
                let mut secondary = Jsonl::new(accept_with_deadline(self.listener));
                if secondary.receive()["event"] != "ready" {
                    return Err("split fixture did not become ready".into());
                }
                self.secondary.push(secondary);
            }
            if split_count == 2 {
                assert_horizontal_then_vertical_layout(&self.fux, self.environment)?;
            }
            let commands = ["split_horizontal", "split_vertical"];
            let mut actions = Vec::with_capacity(split_count.saturating_mul(2));
            let mut opened_panes = Vec::new();
            for command in commands.into_iter().take(split_count) {
                actions.push(ObservedAction::Command(command.into()));
                if let Some((request_id, subscriber)) = self.subscribers.first_mut() {
                    let frame = subscriber.receive();
                    if frame["event"] != "pane.opened" || frame["id"] != *request_id {
                        return Err(format!("unexpected raw split event: {frame}"));
                    }
                    opened_panes.push(
                        frame["pane"]
                            .as_u64()
                            .ok_or_else(|| format!("split event omitted pane id: {frame}"))?,
                    );
                    actions.push(ObservedAction::ControlEvent(
                        verification::schema::ExpectedControlEvent {
                            name: "pane.opened".into(),
                            request_id: *request_id,
                            subscription_id: *request_id,
                        },
                    ));
                }
            }
            if let Some((_, subscriber)) = self.subscribers.first_mut() {
                if opened_panes != [2, 3] {
                    return Err(format!(
                        "split events did not identify both new panes: {opened_panes:?}"
                    ));
                }
                subscriber.expect_no_frame(Duration::from_millis(150));
            }
            return Ok(actions);
        }
        let expected = if bytes == [2, 2] { 1 } else { bytes.len() };
        self.primary_mut()?
            .send(json!({"command":"read_exact", "bytes":expected}));
        self.client.as_mut().ok_or("client is detached")?.write(bytes);
        let response = self.primary_mut()?.receive();
        let encoded = response["bytes_hex"]
            .as_str()
            .ok_or_else(|| "fixture did not report forwarded bytes".to_owned())?;
        Ok(vec![ObservedAction::Forward(decode_hex(encoded)?)])
    }

    fn mouse_input(
        &mut self,
        bytes: &[u8],
    ) -> Result<verification::interpreters::ObservedAction, String> {
        use verification::interpreters::ObservedAction;
        if !self.mouse_enabled {
            return Err("mouse input requires an enable_mouse_tracking step".into());
        }
        let (code, column, row, release) = parse_sgr_mouse(bytes)?;
        self.primary_mut()?
            .send(json!({"command":"read_exact", "bytes":bytes.len()}));
        let outer = format!(
            "\x1b[<{code};{};{}{}",
            column.saturating_add(1),
            row.saturating_add(1),
            if release { 'm' } else { 'M' }
        );
        self.client
            .as_mut()
            .ok_or("client is detached")?
            .write(outer.as_bytes());
        let observed = self.primary_mut()?.receive()["bytes_hex"]
            .as_str()
            .ok_or_else(|| "fixture did not report mouse bytes".to_owned())?
            .to_owned();
        if decode_hex(&observed)? != bytes {
            return Err(format!("host mouse re-encoding mismatch: {observed}"));
        }
        Ok(ObservedAction::Mouse {
            code,
            column,
            row,
            release,
        })
    }

    fn enable_mouse_tracking(&mut self, pane: u32) -> Result<(), String> {
        if pane != 1 || self.mouse_enabled {
            return Err(format!("cannot enable mouse tracking for pane {pane}"));
        }
        self.primary_mut()?.send(json!({
            "command":"write",
            "chunks_hex":["1b5b3f31303033681b5b3f3130303668"]
        }));
        if self.primary_mut()?.receive()["bytes"] != 16 {
            return Err("fixture did not enable SGR mouse mode".into());
        }
        let workspace = self.client_workspace.as_deref().unwrap_or("binary");
        let deadline = Instant::now() + DEADLINE;
        loop {
            let semantics = terminal_semantics(&self.fux, self.environment, workspace, pane)?;
            if semantics.modes.mouse_mode == "anymotion"
                && semantics.modes.mouse_encoding == "sgr"
            {
                break;
            }
            if Instant::now() >= deadline {
                return Err("mouse tracking did not become observable".into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        self.mouse_enabled = true;
        Ok(())
    }

    fn subscribe(
        &mut self,
        request_id: u64,
        events: &[String],
    ) -> Result<verification::schema::ExpectedSubscription, String> {
        let stream = UnixStream::connect(self.environment.control_socket())
            .map_err(|error| error.to_string())?;
        let mut subscriber = Jsonl::new(stream);
        subscriber.send(json!({
            "command": "subscribe",
            "id": request_id,
            "events": events,
        }));
        let reply = subscriber.receive();
        if reply["id"] != request_id || reply["status"] != "accepted" {
            return Err(format!("subscription was not accepted: {reply}"));
        }
        self.subscribers.push((request_id, subscriber));
        Ok(verification::schema::ExpectedSubscription {
            request_id,
            events: events.to_vec(),
        })
    }

    fn control(
        &mut self,
        request: &Value,
    ) -> Result<verification::schema::ExpectedControlReply, String> {
        let stream = UnixStream::connect(self.environment.control_socket())
            .map_err(|error| error.to_string())?;
        let mut connection = Jsonl::new(stream);
        connection.send(request.clone());
        let reply = connection.receive();
        let request_id = reply["id"]
            .as_u64()
            .ok_or_else(|| format!("control reply omitted id: {reply}"))?;
        let status = reply["status"]
            .as_str()
            .ok_or_else(|| format!("control reply omitted status: {reply}"))?
            .to_owned();
        let result_kind = reply
            .pointer("/result/kind")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("control reply omitted result kind: {reply}"))?
            .to_owned();
        Ok(verification::schema::ExpectedControlReply {
            request_id,
            status,
            result_kind,
        })
    }

    fn resize(
        &mut self,
        client: &str,
        size: verification::schema::Size,
    ) -> Result<verification::schema::ExpectedResize, String> {
        if client != "alice" {
            return Err(format!("binary fixture has no client {client:?}"));
        }
        let expected = (size.rows.saturating_sub(3), size.columns.saturating_sub(2));
        self.client
            .as_ref()
            .ok_or("client is detached")?
            .resize(size.rows, size.columns)?;
        self.client_size = size;
        let deadline = Instant::now() + DEADLINE;
        loop {
            let observed = fixture_size(self.primary_mut()?);
            if observed == expected {
                return Ok(verification::schema::ExpectedResize {
                    pane: 1,
                    rows: observed.0,
                    columns: observed.1,
                });
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "fixture size did not converge: expected={expected:?}, observed={observed:?}"
                ));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn shutdown(&mut self) -> Result<usize, String> {
        let workspace_ownership: Vec<_> = self
            .workspaces
            .iter()
            .map(|(name, fixture)| (name.clone(), fixture.pid))
            .collect();
        self.subscribers.clear();
        if !self.primary_exited {
            self.primary_mut()?.send(json!({"command":"quit"}));
            if self.primary_mut()?.receive()["event"] != "cleanup" {
                return Err("primary fixture did not clean up".into());
            }
        }
        for secondary in &mut self.secondary {
            secondary.send(json!({"command":"quit"}));
            if secondary.receive()["event"] != "cleanup" {
                return Err("secondary fixture did not clean up".into());
            }
        }
        if let Some(mut client) = self.client.take() {
            client.detach_with_prefix(2);
        }
        let mut server = self
            .server
            .take()
            .ok_or_else(|| "binary daemon was not running during cleanup".to_owned())?;
        server.terminate(Signal::SIGTERM);
        server.wait();
        wait_for_absent(&self.environment.manager_socket());
        wait_for_absent(&self.environment.control_socket());
        for (name, pid) in workspace_ownership {
            wait_for_absent(&self.environment.workspace_descriptor(&name));
            wait_for_absent(&self.environment.workspace_control_socket(&name));
            wait_for_process_absent(pid);
        }
        self.workspaces.clear();
        if self.environment.descriptor().exists() {
            return Err("workspace descriptor leaked".into());
        }
        Ok(0)
    }
}

struct FrameSemantics {
    cursor: Option<(u16, u16)>,
    modes: verification::transcript::TerminalModes,
    status: std::collections::BTreeMap<String, String>,
    selection: Option<verification::transcript::TerminalSelection>,
    prediction_target: Option<u32>,
}

fn terminal_semantics(
    fux: &Path,
    environment: &PrivateEnvironment,
    workspace: &str,
    pane: u32,
) -> Result<FrameSemantics, String> {
    let output = run(fux, [workspace, "list"], environment);
    if !output.status.success() {
        return Err(format!(
            "terminal semantics listing failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let value: Value = serde_json::from_slice(&output.stdout).map_err(|error| error.to_string())?;
    let workspaces = value
        .pointer("/result/value/workspaces")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("terminal semantics omitted workspaces: {value}"))?;
    let workspace_value = workspaces
        .iter()
        .find(|candidate| candidate["name"].as_str() == Some(workspace))
        .ok_or_else(|| format!("terminal semantics omitted workspace {workspace:?}: {value}"))?;
    let pane_value = workspace_value["tabs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|tab| tab["panes"].as_array())
        .flatten()
        .find(|candidate| candidate["id"].as_u64() == Some(u64::from(pane)))
        .ok_or_else(|| format!("terminal semantics omitted pane {pane}: {value}"))?;
    let cursor = (!pane_value["cursor"]["hidden"].as_bool().unwrap_or(true))
        .then(|| {
            Some((
                u16::try_from(pane_value["cursor"]["row"].as_u64()?).ok()?,
                u16::try_from(pane_value["cursor"]["column"].as_u64()?).ok()?,
            ))
        })
        .flatten();
    let modes = serde_json::from_value(pane_value["modes"].clone())
        .map_err(|error| format!("invalid pane modes: {error}"))?;
    let copy = &pane_value["copy"];
    let selection = copy["active"].as_bool().unwrap_or(false).then(|| {
        let cursor_row = u16::try_from(copy["cursor_row"].as_u64()?).ok()?;
        let cursor_column = u16::try_from(copy["cursor_column"].as_u64()?).ok()?;
        let anchor = copy["anchor"].as_array().and_then(|anchor| {
            Some((
                u16::try_from(anchor.first()?.as_u64()?).ok()?,
                u16::try_from(anchor.get(1)?.as_u64()?).ok()?,
            ))
        });
        Some(verification::transcript::TerminalSelection {
            cursor: (cursor_row, cursor_column),
            anchor,
        })
    }).flatten();
    let status = serde_json::from_value(workspace_value["status"].clone())
        .map_err(|error| format!("invalid workspace status: {error}"))?;
    let prediction_target = (pane_value["focused"].as_bool() == Some(true)
        && pane_value["viewport_offset"].as_u64() == Some(0)
        && selection.is_none())
        .then_some(pane);
    Ok(FrameSemantics {
        cursor,
        modes,
        status,
        selection,
        prediction_target,
    })
}

fn assert_horizontal_then_vertical_layout(
    fux: &Path,
    environment: &PrivateEnvironment,
) -> Result<(), String> {
    let output = run(fux, ["binary", "list"], environment);
    if !output.status.success() {
        return Err(format!(
            "layout listing failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let value: Value = serde_json::from_slice(&output.stdout).map_err(|error| error.to_string())?;
    let panes = value
        .pointer("/result/value/workspaces/0/tabs/0/panes")
        .and_then(Value::as_array)
        .ok_or_else(|| "layout listing omitted panes".to_owned())?;
    let mut geometry = panes
        .iter()
        .map(|pane| {
            let value = pane
                .get("geometry")
                .ok_or_else(|| "pane omitted geometry".to_owned())?;
            Ok((
                value.get("x").and_then(Value::as_u64),
                value.get("y").and_then(Value::as_u64),
                value.get("width").and_then(Value::as_u64),
                value.get("height").and_then(Value::as_u64),
            ))
        })
        .collect::<Result<Vec<_>, String>>()?;
    if geometry
        .iter()
        .any(|item| item.0.is_none() || item.1.is_none() || item.2.is_none() || item.3.is_none())
    {
        return Err("pane geometry was not numeric".into());
    }
    geometry.sort_unstable();
    if geometry.len() != 3 {
        return Err(format!("expected three panes, observed {geometry:?}"));
    }
    let left = geometry[0];
    let upper_right = geometry[1];
    let lower_right = geometry[2];
    let shape_matches = left.0 < upper_right.0
        && upper_right.0 == lower_right.0
        && upper_right.1 < lower_right.1
        && upper_right.2 == lower_right.2
        && left.3 == upper_right.3.zip(lower_right.3).map(|(a, b)| a + b);
    if !shape_matches {
        return Err(format!(
            "real layout was not horizontal then vertical: {geometry:?}"
        ));
    }
    Ok(())
}

fn workspace_topology(
    fux: &Path,
    environment: &PrivateEnvironment,
    workspace: &str,
) -> Result<Value, String> {
    let output = run(fux, [workspace, "list"], environment);
    if !output.status.success() {
        return Err(format!(
            "workspace listing failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let value: Value = serde_json::from_slice(&output.stdout).map_err(|error| error.to_string())?;
    let workspaces = value
        .pointer("/result/value/workspaces")
        .and_then(Value::as_array)
        .ok_or_else(|| "workspace listing omitted workspaces".to_owned())?;
    let workspaces = workspaces
        .iter()
        .map(|workspace| {
            let tabs = workspace
                .get("tabs")
                .and_then(Value::as_array)
                .ok_or_else(|| "workspace omitted tabs".to_owned())?;
            let tabs = tabs
                .iter()
                .map(|tab| {
                    let panes = tab
                        .get("panes")
                        .and_then(Value::as_array)
                        .ok_or_else(|| "tab omitted panes".to_owned())?;
                    let panes = panes
                        .iter()
                        .map(|pane| {
                            Ok(json!({
                                "id": required_field(pane, "id", "pane")?,
                                "geometry": required_field(pane, "geometry", "pane")?,
                                "focused": required_field(pane, "focused", "pane")?,
                            }))
                        })
                        .collect::<Result<Vec<_>, String>>()?;
                    Ok(json!({
                        "index": required_field(tab, "index", "tab")?,
                        "name": required_field(tab, "name", "tab")?,
                        "focused": required_field(tab, "focused", "tab")?,
                        "panes": panes,
                    }))
                })
                .collect::<Result<Vec<_>, String>>()?;
            Ok(json!({
                "name": required_field(workspace, "name", "workspace")?,
                "focused": required_field(workspace, "focused", "workspace")?,
                "tabs": tabs,
            }))
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(json!({ "workspaces": workspaces }))
}

fn wait_for_captured_line(
    fux: &Path,
    environment: &PrivateEnvironment,
    pane: u32,
    expected: &str,
) -> Result<String, String> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let output = run(
            fux,
            ["binary", "capture", &pane.to_string()],
            environment,
        );
        if output.status.success() {
            let reply: Value = serde_json::from_slice(&output.stdout)
                .map_err(|error| format!("invalid capture reply: {error}"))?;
            let captured = reply
                .pointer("/result/value/text")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("capture reply omitted text: {reply}"))?;
            if let Some(observed) = captured
                .lines()
                .find(|line| line.contains(expected))
                .map(str::trim_end)
            {
                if observed == expected {
                    return Ok(observed.to_owned());
                }
                return Err(format!(
                    "child output was not retained exactly: expected={expected:?}, observed={observed:?}"
                ));
            }
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "pane {pane} did not capture child output {expected:?} before deadline"
            ));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn expect_viewer_event(
    subscriber: &mut Jsonl,
    request_id: u64,
    event_name: &str,
) -> Result<u64, String> {
    let frame = subscriber.receive();
    if frame["event"] != event_name || frame["id"] != request_id {
        return Err(format!("unexpected raw client lifecycle event: {frame}"));
    }
    frame
        .pointer("/client/viewer")
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("client lifecycle event omitted viewer identity: {frame}"))
}

fn required_field<'a>(value: &'a Value, field: &str, owner: &str) -> Result<&'a Value, String> {
    value
        .get(field)
        .ok_or_else(|| format!("{owner} omitted {field}"))
}

fn parse_sgr_mouse(bytes: &[u8]) -> Result<(u16, u16, u16, bool), String> {
    let tail = bytes
        .strip_prefix(b"\x1b[<")
        .ok_or_else(|| "mouse input is not SGR encoded".to_owned())?;
    let (terminator, body) = tail
        .split_last()
        .ok_or_else(|| "mouse input has no SGR terminator".to_owned())?;
    let release = match terminator {
        b'M' => false,
        b'm' => true,
        _ => return Err("mouse input has no SGR terminator".into()),
    };
    let body = std::str::from_utf8(body).map_err(|error| error.to_string())?;
    let mut fields = body.split(';');
    let mut next = || {
        fields
            .next()
            .ok_or_else(|| "missing mouse field".to_owned())?
            .parse::<u16>()
            .map_err(|error| error.to_string())
    };
    let result = (next()?, next()?, next()?, release);
    if fields.next().is_some() || result.1 == 0 || result.2 == 0 {
        return Err("invalid mouse fields".into());
    }
    Ok(result)
}

fn decode_hex(value: &str) -> Result<Vec<u8>, String> {
    if !value.len().is_multiple_of(2) {
        return Err("odd fixture hex length".into());
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let text = std::str::from_utf8(pair).map_err(|error| error.to_string())?;
            u8::from_str_radix(text, 16).map_err(|error| error.to_string())
        })
        .collect()
}

// Binary boundary: real fux manager/control sockets, daemon process, Zor wrapper, and PTY child.
#[test]
fn real_binaries_publish_agent_state_and_remove_every_private_runtime_artifact() {
    let fux = binary("FUX_BIN", "target/debug/fux");
    let zor = binary("ZOR_BIN", "zor/target/debug/zor");
    let fixture = PathBuf::from(env!("CARGO_BIN_EXE_fux-fixture-child"));
    let environment = PrivateEnvironment::new("binary");
    let fixture_listener =
        UnixListener::bind(environment.fixture_socket()).expect("fixture socket");

    let id = run(&fux, ["id"], &environment);
    assert!(
        id.status.success(),
        "id failed: {}",
        String::from_utf8_lossy(&id.stderr)
    );
    let allow = String::from_utf8(id.stdout)
        .expect("endpoint id")
        .trim()
        .to_owned();
    assert_eq!(allow.len(), 64);

    environment.write_config(&fixture, &zor);
    let mut server = OwnedChild::spawn(
        Command::new(&fux)
            .args(["serve", "--allow", &allow, "--name", "binary"])
            .env_clear()
            .envs(environment.variables())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    );
    wait_for_path(&environment.manager_socket());
    wait_for_path(&environment.control_socket());

    let mut events = Jsonl::new(
        UnixStream::connect(environment.control_socket()).expect("subscribe to binary events"),
    );
    events.send(json!({"command":"subscribe", "id":77, "events":["agent.state"]}));
    assert_eq!(events.receive()["id"], 77);

    let mut fixture = Jsonl::new(accept_with_deadline(&fixture_listener));
    assert_eq!(fixture.receive()["event"], "ready");
    let listed = run(&fux, ["binary", "list"], &environment);
    assert!(
        listed.status.success(),
        "list failed: {}",
        String::from_utf8_lossy(&listed.stderr)
    );
    assert!(String::from_utf8_lossy(&listed.stdout).contains("fux-fixture-child"));

    let split = run(&fux, ["binary", "split", "horizontal"], &environment);
    assert!(
        split.status.success(),
        "split failed: {}",
        String::from_utf8_lossy(&split.stderr)
    );
    let mut second_fixture = Jsonl::new(accept_with_deadline(&fixture_listener));
    assert_eq!(second_fixture.receive()["event"], "ready");
    let mut client = TerminalChild::spawn(&fux, &environment, 40, 120);
    wait_for_list(&fux, &environment, "\"width\":60");
    // Zor samples an outer resize on its bounded 50 ms coordinator cadence.
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(fixture_size(&mut fixture), (37, 58));
    assert_eq!(fixture_size(&mut second_fixture), (37, 58));

    let popup = run(&fux, ["binary", "popup", "--size", "30x8"], &environment);
    assert!(
        popup.status.success(),
        "popup failed: {}",
        String::from_utf8_lossy(&popup.stderr)
    );
    let mut popup_fixture = Jsonl::new(accept_with_deadline(&fixture_listener));
    assert_eq!(popup_fixture.receive()["event"], "ready");
    wait_for_list(&fux, &environment, "\"name\":\"popups\"");
    popup_fixture.send(json!({
        "command":"write",
        "chunks_hex":["1b5b3f31303030681b5b3f3130303668"]
    }));
    assert_eq!(popup_fixture.receive()["bytes"], 16);
    client.wait_for_output_bytes(b"\x1b[?1006h");
    popup_fixture.send(json!({"command":"read_exact", "bytes":9}));
    // A 30x8 popup in the 120x39 workspace body has its border origin at (45,15).
    // Clicking the first content cell is therefore outer (47,17), re-encoded as popup (1,1).
    client.write(b"\x1b[<0;47;17M");
    assert_eq!(popup_fixture.receive()["bytes_hex"], "1b5b3c303b313b314d");
    popup_fixture.send(json!({"command":"read_exact", "bytes":1}));
    client.write(b"q");
    assert_eq!(popup_fixture.receive()["bytes_hex"], "71");
    popup_fixture.send(json!({"command":"exit", "status":0}));
    assert_eq!(popup_fixture.receive()["event"], "cleanup");
    wait_for_list_absent(&fux, &environment, "\"name\":\"popups\"");

    for (sequence, state) in [(1, "working"), (2, "blocked"), (3, "idle")] {
        fixture.send(json!({
            "command":"agent",
            "payload":format!("state={state};agent=fixture;seq={sequence}")
        }));
        wait_for_list(&fux, &environment, state);
        let event = events.receive();
        assert_eq!(event["event"], "agent.state");
        assert_eq!(event["id"], 77);
        assert_eq!(event["new_state"], state, "event={event}");
        let displayed = match state {
            "working" => "Working",
            "blocked" => "Blocked",
            "idle" => "Idle",
            _ => unreachable!(),
        };
        client.wait_for_text(displayed);
    }
    fixture.send(json!({
        "command":"agent",
        "payload":"state=idle;agent=fixture;seq=3"
    }));
    events.expect_no_frame(Duration::from_millis(150));
    wait_for_notification_count(&environment.notification_log(), 2);
    let notifications = fs::read_to_string(environment.notification_log())
        .expect("private notification transcript");
    let decoded: Vec<Vec<String>> = notifications
        .lines()
        .map(|line| serde_json::from_str(line).expect("notification arguments"))
        .collect();
    let expected_arguments = if cfg!(target_os = "macos") {
        vec!["-title", "fux", "-message", "fixture"]
    } else {
        vec!["fux", "fixture"]
    };
    assert_eq!(
        decoded,
        vec![expected_arguments.clone(), expected_arguments]
    );
    assert_notification_count_stays(
        &environment.notification_log(),
        2,
        Duration::from_millis(300),
    );
    client.detach();
    fixture.send(json!({"command":"write", "chunks_hex":["62696e617279"]}));
    assert_eq!(fixture.receive()["bytes"], 6);
    let captured = run(&fux, ["binary", "capture", "1"], &environment);
    assert!(captured.status.success());
    assert!(String::from_utf8_lossy(&captured.stdout).contains("binary"));
    let mut reattached = TerminalChild::spawn(&fux, &environment, 40, 120);
    reattached.wait_for_text("binary");
    reattached.detach();

    server.terminate(Signal::SIGTERM);
    server.wait();
    wait_for_absent(&environment.manager_socket());
    wait_for_absent(&environment.control_socket());
    assert!(
        !environment.descriptor().exists(),
        "workspace descriptor leaked"
    );
}

// Golden path 7: the installed-style binary boundary retains final output, OSC state,
// and the real status until the final client detaches, then retires every artifact.
#[test]
fn natural_last_pane_exit_is_observable_before_binary_workspace_retirement() {
    let fux = binary("FUX_BIN", "target/debug/fux");
    let zor = binary("ZOR_BIN", "zor/target/debug/zor");
    let fixture = PathBuf::from(env!("CARGO_BIN_EXE_fux-fixture-child"));
    let environment = PrivateEnvironment::new("nat");
    let fixture_listener =
        UnixListener::bind(environment.fixture_socket()).expect("fixture socket");

    let id = run(&fux, ["id"], &environment);
    assert!(
        id.status.success(),
        "id failed: {}",
        String::from_utf8_lossy(&id.stderr)
    );
    let allow = String::from_utf8(id.stdout)
        .expect("endpoint id")
        .trim()
        .to_owned();
    environment.write_config(&fixture, &zor);
    let mut server = OwnedChild::spawn(
        Command::new(&fux)
            .args(["serve", "--allow", &allow, "--name", "binary"])
            .env_clear()
            .envs(environment.variables())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    );
    wait_for_path(&environment.manager_socket());
    wait_for_path(&environment.control_socket());
    let mut fixture = Jsonl::new(accept_with_deadline(&fixture_listener));
    assert_eq!(fixture.receive()["event"], "ready");
    let mut client = TerminalChild::spawn(&fux, &environment, 24, 80);
    wait_for_list(&fux, &environment, "\"width\":80");

    fixture.send(json!({"command":"write", "chunks_hex":["61cc81", "e7958c", "6263"]}));
    assert_eq!(fixture.receive()["bytes"], 8);
    client.wait_for_text("á界bc");
    client.write(&[1, b'[']);
    client.wait_for_inverse(1, 6);
    client.write(b" hhhhhy");
    client.wait_for_output_bytes(b"\x1b]52;c;YcyB55WMYmM=\x07");
    fixture.send(json!({"command":"read_exact", "bytes":1}));
    client.write(b"Z");
    assert_eq!(fixture.receive()["bytes_hex"], "5a");

    fixture.send(json!({"command":"write", "chunks_hex":["46494e414c5f42494e415259"]}));
    assert_eq!(fixture.receive()["bytes"], 12);
    fixture.send(json!({
        "command":"agent",
        "payload":"state=blocked;agent=final-child;seq=41"
    }));
    wait_for_list(&fux, &environment, "blocked");
    fixture.send(json!({"command":"exit", "status":29}));
    assert_eq!(fixture.receive()["event"], "cleanup");

    client.wait_for_text("FINAL_BINARY");
    client.wait_status(29);

    server.wait();
    wait_for_absent(&environment.manager_socket());
    wait_for_absent(&environment.control_socket());
    assert!(
        !environment.descriptor().exists(),
        "workspace descriptor leaked"
    );
}

// Golden path 8: a control kill reaches the pane process group, including a
// descendant that ignores HUP, and the client observes the real signal status.
#[test]
fn binary_control_kill_reaps_an_ignore_hup_descendant_and_reports_status() {
    let fux = binary("FUX_BIN", "target/debug/fux");
    let zor = binary("ZOR_BIN", "zor/target/debug/zor");
    let fixture = PathBuf::from(env!("CARGO_BIN_EXE_fux-fixture-child"));
    let environment = PrivateEnvironment::new("kill");
    let fixture_listener =
        UnixListener::bind(environment.fixture_socket()).expect("fixture socket");

    let id = run(&fux, ["id"], &environment);
    assert!(
        id.status.success(),
        "id failed: {}",
        String::from_utf8_lossy(&id.stderr)
    );
    let allow = String::from_utf8(id.stdout)
        .expect("endpoint id")
        .trim()
        .to_owned();
    environment.write_config(&fixture, &zor);
    let mut server = OwnedChild::spawn(
        Command::new(&fux)
            .args(["serve", "--allow", &allow, "--name", "binary"])
            .env_clear()
            .envs(environment.variables())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    );
    wait_for_path(&environment.manager_socket());
    wait_for_path(&environment.control_socket());
    let mut fixture = Jsonl::new(accept_with_deadline(&fixture_listener));
    assert_eq!(fixture.receive()["event"], "ready");
    let mut client = TerminalChild::spawn(&fux, &environment, 24, 80);
    wait_for_list(&fux, &environment, "\"width\":80");

    fixture.send(json!({
        "command":"spawn", "mode":"ignore_hup", "exit_status":47
    }));
    let spawned = fixture.receive();
    assert_eq!(spawned["event"], "spawned");
    let descendant_pid =
        i32::try_from(spawned["pid"].as_u64().expect("descendant pid")).expect("pid range");
    let mut descendant = Jsonl::new(accept_with_deadline(&fixture_listener));
    let ready = descendant.receive();
    assert_eq!(ready["event"], "descendant_ready");
    assert_eq!(ready["pid"], spawned["pid"]);

    let killed = run(&fux, ["binary", "kill", "1"], &environment);
    assert!(
        killed.status.success(),
        "control kill failed: stdout={} stderr={}",
        String::from_utf8_lossy(&killed.stdout),
        String::from_utf8_lossy(&killed.stderr)
    );
    wait_for_process_absent(descendant_pid);
    client.wait_status(129);
    server.wait();
    wait_for_absent(&environment.manager_socket());
    wait_for_absent(&environment.control_socket());
    assert!(
        !environment.descriptor().exists(),
        "workspace descriptor leaked"
    );
}

// Golden path 2: two bare clients race from an empty runtime. Exactly one daemon,
// workspace, and fixture pane are created, and both clients attach to its state.
#[test]
fn simultaneous_first_binary_clients_elect_one_workspace_and_both_attach() {
    let fux = binary("FUX_BIN", "target/debug/fux");
    let zor = binary("ZOR_BIN", "zor/target/debug/zor");
    let fixture = PathBuf::from(env!("CARGO_BIN_EXE_fux-fixture-child"));
    let environment = std::sync::Arc::new(PrivateEnvironment::new("race"));
    environment.write_config(&fixture, &zor);
    let fixture_listener =
        UnixListener::bind(environment.fixture_socket()).expect("fixture socket");
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));

    let first = {
        let fux = fux.clone();
        let environment = std::sync::Arc::clone(&environment);
        let barrier = std::sync::Arc::clone(&barrier);
        std::thread::spawn(move || {
            barrier.wait();
            TerminalChild::spawn(&fux, &environment, 24, 80)
        })
    };
    let second = {
        let fux = fux.clone();
        let environment = std::sync::Arc::clone(&environment);
        let barrier = std::sync::Arc::clone(&barrier);
        std::thread::spawn(move || {
            barrier.wait();
            TerminalChild::spawn(&fux, &environment, 24, 80)
        })
    };
    barrier.wait();
    let mut first = first.join().expect("first client spawn");
    let mut second = second.join().expect("second client spawn");
    let mut fixture = Jsonl::new(accept_with_deadline(&fixture_listener));
    assert_eq!(fixture.receive()["event"], "ready");
    wait_for_path(&environment.manager_socket());
    wait_for_path(&environment.control_socket());
    wait_for_listing_shape(&fux, &environment, 1, 1);

    fixture.send(json!({"command":"write", "chunks_hex":["454c45435445445f4f4e4345"]}));
    assert_eq!(fixture.receive()["bytes"], 12);
    first.wait_for_text("ELECTED_ONCE");
    second.wait_for_text("ELECTED_ONCE");

    fixture.send(json!({"command":"exit", "status":0}));
    assert_eq!(fixture.receive()["event"], "cleanup");
    first.wait_status(0);
    second.wait_status(0);
    wait_for_absent(&environment.manager_socket());
    wait_for_absent(&environment.control_socket());
    assert!(
        !environment.descriptor().exists(),
        "workspace descriptor leaked"
    );
}

// Golden path 9: terminate the foreground daemon at each externally observable
// startup boundary and require the same complete rollback every time.
#[test]
fn sigterm_at_each_binary_startup_phase_rolls_back_all_owned_resources() {
    for phase in ["manager", "pane", "descriptor", "control"] {
        let fux = binary("FUX_BIN", "target/debug/fux");
        let zor = binary("ZOR_BIN", "zor/target/debug/zor");
        let fixture = PathBuf::from(env!("CARGO_BIN_EXE_fux-fixture-child"));
        let environment = PrivateEnvironment::new(&format!("p{}", &phase[..1]));
        let fixture_listener =
            UnixListener::bind(environment.fixture_socket()).expect("fixture socket");
        let id = run(&fux, ["id"], &environment);
        assert!(id.status.success(), "id failed in {phase}");
        let allow = String::from_utf8(id.stdout)
            .expect("endpoint id")
            .trim()
            .to_owned();
        environment.write_config(&fixture, &zor);
        let mut server = OwnedChild::spawn(
            Command::new(&fux)
                .args(["serve", "--allow", &allow, "--name", "binary"])
                .env_clear()
                .envs(environment.variables())
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        );

        let fixture_pid = match phase {
            "manager" => {
                wait_for_path(&environment.manager_socket());
                None
            }
            "pane" => {
                let mut child = Jsonl::new(accept_with_deadline(&fixture_listener));
                let ready = child.receive();
                assert_eq!(ready["event"], "ready");
                ready["pid"]
                    .as_u64()
                    .and_then(|pid| i32::try_from(pid).ok())
            }
            "descriptor" => {
                wait_for_path(&environment.descriptor());
                None
            }
            "control" => {
                wait_for_path(&environment.control_socket());
                None
            }
            _ => unreachable!(),
        };
        server.terminate(Signal::SIGTERM);
        server.wait();
        wait_for_absent(&environment.manager_socket());
        wait_for_absent(&environment.control_socket());
        wait_for_absent(&environment.descriptor());
        if let Some(pid) = fixture_pid {
            wait_for_process_absent(pid);
        }
        let lock = fux::daemon::StartupLock::acquire(&environment.root.join("run/fux"))
            .expect("startup lock released");
        drop(lock);
    }
}

#[test]
fn sigterm_cancels_a_stalled_startup_secret_transfer() {
    use std::io::Read as _;

    let fux = binary("FUX_BIN", "target/debug/fux");
    let zor = binary("ZOR_BIN", "zor/target/debug/zor");
    let fixture = PathBuf::from(env!("CARGO_BIN_EXE_fux-fixture-child"));
    let environment = PrivateEnvironment::new("stalled-secret");
    environment.write_config(&fixture, &zor);
    let startup_path = environment.root.join("startup.sock");
    let listener = UnixListener::bind(&startup_path).expect("startup listener");
    std::fs::set_permissions(
        &startup_path,
        std::os::unix::fs::PermissionsExt::from_mode(0o600),
    )
    .expect("private startup socket");
    let mut server = OwnedChild::spawn(
        Command::new(&fux)
            .args(["serve", "--startup-channel"])
            .arg(&startup_path)
            .env_clear()
            .envs(environment.variables())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    );
    let mut startup = accept_with_deadline(&listener);
    startup
        .set_read_timeout(Some(DEADLINE))
        .expect("bound startup read");
    let mut request = [0_u8; 6];
    startup.read_exact(&mut request).expect("startup request");
    assert_eq!(&request, b"SECRET");

    server.terminate(Signal::SIGTERM);
    server.wait();
    wait_for_absent(&environment.manager_socket());
    assert!(!environment.descriptor().exists());
    drop(startup);
}

struct PrivateEnvironment {
    root: PathBuf,
}

impl PrivateEnvironment {
    fn new(label: &str) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let nonce = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("fux-verify-{label}-{}-{nonce}", std::process::id()));
        for directory in [
            root.clone(),
            root.join("run"),
            root.join("state"),
            root.join("config/fux"),
        ] {
            fs::create_dir_all(&directory).expect("private directory");
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
                .expect("private mode");
        }
        Self { root }
    }

    fn variables(&self) -> Vec<(String, String)> {
        vec![
            ("HOME".into(), self.root.display().to_string()),
            (
                "XDG_RUNTIME_DIR".into(),
                self.root.join("run").display().to_string(),
            ),
            (
                "XDG_STATE_HOME".into(),
                self.root.join("state").display().to_string(),
            ),
            (
                "XDG_CONFIG_HOME".into(),
                self.root.join("config").display().to_string(),
            ),
            (
                "KOH_KEY_NEW_PASSPHRASE".into(),
                "verification-only-passphrase".into(),
            ),
            (
                "KOH_KEY_PASSPHRASE".into(),
                "verification-only-passphrase".into(),
            ),
            (
                "PATH".into(),
                format!("{}:/usr/bin:/bin", self.root.join("bin").display()),
            ),
            ("DISPLAY".into(), "fixture-display".into()),
            (
                "FUX_FIXTURE_NOTIFICATION_LOG".into(),
                self.notification_log().display().to_string(),
            ),
            ("TERM".into(), "xterm-256color".into()),
        ]
    }

    fn write_config(&self, fixture: &Path, zor: &Path) {
        fs::create_dir_all(self.root.join("bin")).expect("fixture bin directory");
        for name in ["terminal-notifier", "notify-send"] {
            let target = self.root.join("bin").join(name);
            if !target.exists() {
                fs::hard_link(fixture, target).expect("install fixture notifier");
            }
        }
        let document = format!(
            "default-command = {{ argv = [{:?}, {:?}, \"--deadline-ms=30000\"] }}\nzor-path = {:?}\nclipboard = \"write-only\"\nlocal-network = true\n[notifications]\nenabled = true\nnotify-blocked = true\nnotify-idle = true\n",
            fixture.display().to_string(),
            format!("--control={}", self.fixture_socket().display()),
            zor.display().to_string(),
        );
        fs::write(self.root.join("config/fux/config.toml"), document).expect("private config");
    }

    fn fixture_socket(&self) -> PathBuf {
        self.root.join("fixture.sock")
    }
    fn manager_socket(&self) -> PathBuf {
        self.root.join("run/fux/manager.sock")
    }
    fn control_socket(&self) -> PathBuf {
        self.root.join("run/fux/binary.sock")
    }
    fn descriptor(&self) -> PathBuf {
        self.workspace_descriptor("binary")
    }
    fn workspace_descriptor(&self, name: &str) -> PathBuf {
        self.root.join("run/fux/workspaces").join(format!("{name}.json"))
    }
    fn workspace_control_socket(&self, name: &str) -> PathBuf {
        self.root.join("run/fux").join(format!("{name}.sock"))
    }
    fn notification_log(&self) -> PathBuf {
        self.root.join("notifications.jsonl")
    }
}

fn wait_for_notification_count(path: &Path, expected: usize) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let count = fs::read_to_string(path)
            .map(|contents| contents.lines().count())
            .unwrap_or(0);
        if count == expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "expected {expected} notifications, observed {count}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn assert_notification_count_stays(path: &Path, expected: usize, duration: Duration) {
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline {
        let count = fs::read_to_string(path)
            .map(|contents| contents.lines().count())
            .unwrap_or(0);
        assert_eq!(
            count, expected,
            "notification count changed after duplicate state"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

impl Drop for PrivateEnvironment {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!(
                "daemon log: {}",
                fs::read_to_string(self.root.join("state/fux/daemon.log")).unwrap_or_default()
            );
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

struct OwnedChild {
    child: Option<Child>,
}

struct TerminalChild {
    master: Box<dyn portable_pty::MasterPty + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    writer: Box<dyn Write + Send>,
    reader: Option<std::thread::JoinHandle<()>>,
    reader_done: std::sync::mpsc::Receiver<()>,
    output: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
}

impl TerminalChild {
    fn spawn(fux: &Path, environment: &PrivateEnvironment, rows: u16, columns: u16) -> Self {
        Self::spawn_workspace(fux, environment, rows, columns, "binary")
    }

    fn spawn_workspace(
        fux: &Path,
        environment: &PrivateEnvironment,
        rows: u16,
        columns: u16,
        workspace: &str,
    ) -> Self {
        use portable_pty::{CommandBuilder, PtySize, native_pty_system};
        let pair = native_pty_system()
            .openpty(PtySize {
                rows,
                cols: columns,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("client PTY");
        let mut command = CommandBuilder::new(fux);
        command.arg(workspace);
        for (key, value) in environment.variables() {
            command.env(key, value);
        }
        let child = pair.slave.spawn_command(command).expect("spawn fux client");
        drop(pair.slave);
        let mut terminal = pair.master.try_clone_reader().expect("client reader");
        let output = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured = std::sync::Arc::clone(&output);
        let (reader_done_tx, reader_done) = std::sync::mpsc::sync_channel(1);
        let reader = std::thread::spawn(move || {
            let mut chunk = [0_u8; 4096];
            while let Ok(count) = terminal.read(&mut chunk) {
                if count == 0 {
                    break;
                }
                let mut captured = captured
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let remaining = 1024 * 1024 - captured.len().min(1024 * 1024);
                captured.extend_from_slice(&chunk[..count.min(remaining)]);
            }
            let _ = reader_done_tx.send(());
        });
        let writer = pair.master.take_writer().expect("client writer");
        Self {
            master: pair.master,
            child,
            writer,
            reader: Some(reader),
            reader_done,
            output,
        }
    }

    fn resize(&self, rows: u16, columns: u16) -> Result<(), String> {
        self.master
            .resize(portable_pty::PtySize {
                rows,
                cols: columns,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|error| error.to_string())
    }

    fn detach(&mut self) {
        self.detach_with_prefix(1);
    }

    fn detach_with_prefix(&mut self, prefix: u8) {
        self.write(&[prefix, b'd']);
        self.wait_success();
    }

    fn disconnect(&mut self) -> Result<(), String> {
        self.child.kill().map_err(|error| error.to_string())?;
        self.wait_exit();
        Ok(())
    }

    fn wait_success(&mut self) {
        self.wait_status(0);
    }

    fn wait_status(&mut self, expected: u32) {
        let status = self.wait_exit();
        assert_eq!(status.exit_code(), expected, "client status {status}");
    }

    fn wait_exit(&mut self) -> portable_pty::ExitStatus {
        let deadline = Instant::now() + DEADLINE;
        let status = loop {
            if let Some(status) = self.child.try_wait().expect("wait client") {
                break status;
            }
            assert!(Instant::now() < deadline, "client detach deadline expired");
            std::thread::sleep(Duration::from_millis(5));
        };
        if let Some(reader) = self.reader.take() {
            self.reader_done
                .recv_timeout(DEADLINE)
                .expect("client reader completion deadline expired");
            reader.join().expect("client reader");
        }
        status
    }

    fn wait_for_text(&self, needle: &str) {
        let deadline = Instant::now() + DEADLINE;
        loop {
            let bytes = self
                .output
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            let mut terminal = vt100::Parser::new(24, 80, 0);
            terminal.process(&bytes);
            if terminal.screen().contents().contains(needle) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "client never painted final output; bytes={}",
                String::from_utf8_lossy(&bytes)
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn wait_for_inverse(&self, row: u16, column: u16) {
        let deadline = Instant::now() + DEADLINE;
        loop {
            let bytes = self
                .output
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            let mut terminal = vt100::Parser::new(24, 80, 0);
            terminal.process(&bytes);
            if terminal
                .screen()
                .cell(row, column)
                .is_some_and(vt100::Cell::inverse)
            {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "copy-mode cursor was not painted"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn wait_for_output_bytes(&self, needle: &[u8]) {
        let deadline = Instant::now() + DEADLINE;
        loop {
            let found = self
                .output
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .windows(needle.len())
                .any(|window| window == needle);
            if found {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "client never emitted expected terminal bytes; output={:?}",
                String::from_utf8_lossy(
                    &self
                        .output
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                )
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn write(&mut self, bytes: &[u8]) {
        self.writer.write_all(bytes).expect("terminal input");
        self.writer.flush().expect("detach flush");
    }
}

impl Drop for TerminalChild {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

impl OwnedChild {
    fn spawn(command: &mut Command) -> Self {
        Self {
            child: Some(command.spawn().expect("spawn fux server")),
        }
    }
    fn terminate(&mut self, signal: Signal) {
        if let Some(child) = &self.child {
            kill(
                Pid::from_raw(i32::try_from(child.id()).expect("pid")),
                signal,
            )
            .expect("signal server");
        }
    }
    fn wait(&mut self) {
        let deadline = Instant::now() + DEADLINE;
        let child = self.child.as_mut().expect("owned server");
        loop {
            if let Some(status) = child.try_wait().expect("wait server") {
                assert!(status.success(), "server exited with {status}");
                self.child.take();
                return;
            }
            assert!(
                Instant::now() < deadline,
                "server shutdown deadline expired"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

struct Jsonl {
    writer: UnixStream,
    reader: UnixStream,
    read_buffer: Vec<u8>,
}
impl Jsonl {
    fn new(stream: UnixStream) -> Self {
        let reader = stream.try_clone().expect("clone fixture control");
        reader
            .set_nonblocking(true)
            .expect("fixture control nonblocking reader");
        Self {
            reader,
            read_buffer: Vec::new(),
            writer: stream,
        }
    }
    fn send(&mut self, value: Value) {
        let mut frame = serde_json::to_vec(&value).expect("fixture request");
        frame.push(b'\n');
        let deadline = Instant::now() + DEADLINE;
        let mut written = 0;
        while written < frame.len() {
            match self.writer.write(&frame[written..]) {
                Ok(0) => panic!("fixture control closed during write"),
                Ok(count) => written += count,
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) => {}
                Err(error) => panic!("fixture control write: {error}"),
            }
            assert!(Instant::now() < deadline, "fixture write deadline expired");
            if written < frame.len() {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }
    fn receive(&mut self) -> Value {
        let deadline = Instant::now() + DEADLINE;
        loop {
            if let Some(newline) = self.read_buffer.iter().position(|byte| *byte == b'\n') {
                assert!(
                    newline < fux::control::MAX_FRAME_BYTES + 1,
                    "JSONL response exceeded control protocol frame bound"
                );
                let frame: Vec<u8> = self.read_buffer.drain(..=newline).collect();
                return serde_json::from_slice(&frame).expect("fixture JSON");
            }
            let mut chunk = [0_u8; 4096];
            match self.reader.read(&mut chunk) {
                Ok(0) => panic!("fixture closed unexpectedly"),
                Ok(count) => self.read_buffer.extend_from_slice(&chunk[..count]),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) => {}
                Err(error) => panic!("fixture response: {error}"),
            }
            if !self.read_buffer.contains(&b'\n') {
                assert!(
                    self.read_buffer.len() <= fux::control::MAX_FRAME_BYTES,
                    "JSONL response exceeded control protocol frame bound"
                );
            }
            assert!(
                Instant::now() < deadline,
                "fixture response deadline expired"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn expect_no_frame(&mut self, timeout: Duration) {
        assert!(
            self.read_buffer.is_empty(),
            "buffered fixture bytes before duplicate-event check"
        );
        let deadline = Instant::now() + timeout;
        loop {
            let mut chunk = [0_u8; 4096];
            match self.reader.read(&mut chunk) {
                Ok(0) => panic!("fixture closed while checking for duplicate event"),
                Ok(count) => panic!(
                    "unexpected duplicate event bytes: {:?}",
                    &chunk[..count]
                ),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) => {}
                Err(error) => panic!("fixture duplicate-event read: {error}"),
            }
            if Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

fn run<const N: usize>(
    program: &Path,
    arguments: [&str; N],
    environment: &PrivateEnvironment,
) -> Output {
    Command::new(program)
        .args(arguments)
        .env_clear()
        .envs(environment.variables())
        .stdin(Stdio::null())
        .output()
        .expect("run fux binary")
}

fn wait_for_list(fux: &Path, environment: &PrivateEnvironment, state: &str) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let output = run(fux, ["binary", "list"], environment);
        if output.status.success() && String::from_utf8_lossy(&output.stdout).contains(state) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "`{state}` did not reach binary list; stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn wait_for_list_absent(fux: &Path, environment: &PrivateEnvironment, value: &str) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let output = run(fux, ["binary", "list"], environment);
        if output.status.success() && !String::from_utf8_lossy(&output.stdout).contains(value) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "`{value}` remained in binary list"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn wait_for_listing_shape(
    fux: &Path,
    environment: &PrivateEnvironment,
    workspaces: usize,
    panes: usize,
) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let output = run(fux, ["binary", "list"], environment);
        if output.status.success()
            && serde_json::from_slice::<Value>(&output.stdout).is_ok_and(|value| {
                let listed = &value["result"]["value"]["workspaces"];
                listed
                    .as_array()
                    .is_some_and(|items| items.len() == workspaces)
                    && listed
                        .as_array()
                        .into_iter()
                        .flatten()
                        .flat_map(|workspace| workspace["tabs"].as_array().into_iter().flatten())
                        .flat_map(|tab| tab["panes"].as_array().into_iter().flatten())
                        .count()
                        == panes
            })
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "listing did not converge to {workspaces} workspace(s) and {panes} pane(s): stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn fixture_size(fixture: &mut Jsonl) -> (u16, u16) {
    fixture.send(json!({"command":"size"}));
    let response = fixture.receive();
    let rows = response["rows"]
        .as_u64()
        .and_then(|value| u16::try_from(value).ok());
    let columns = response["columns"]
        .as_u64()
        .and_then(|value| u16::try_from(value).ok());
    (
        rows.expect("fixture rows"),
        columns.expect("fixture columns"),
    )
}

fn accept_with_deadline(listener: &UnixListener) -> UnixStream {
    listener
        .set_nonblocking(true)
        .expect("nonblocking listener");
    let deadline = Instant::now() + DEADLINE;
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream
                    .set_nonblocking(false)
                    .expect("blocking fixture stream");
                let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
                return stream;
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => panic!("accept fixture: {error}"),
        }
        assert!(
            Instant::now() < deadline,
            "fixture readiness deadline expired"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn wait_for_path(path: &Path) {
    let deadline = Instant::now() + DEADLINE;
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "{} was not created",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn wait_for_absent(path: &Path) {
    let deadline = Instant::now() + DEADLINE;
    while path.exists() {
        assert!(
            Instant::now() < deadline,
            "{} leaked after shutdown",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn wait_for_process_absent(pid: i32) {
    let deadline = Instant::now() + DEADLINE;
    while kill(Pid::from_raw(pid), None).is_ok() {
        assert!(
            Instant::now() < deadline,
            "descendant {pid} survived control kill"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn binary(variable: &str, relative: &str) -> PathBuf {
    let path = std::env::var_os(variable)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../..")
                .join(relative)
        });
    assert!(
        path.is_file(),
        "{variable} must name the built binary at {}",
        path.display()
    );
    path
}
