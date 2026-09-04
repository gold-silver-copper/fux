#![allow(clippy::expect_used, clippy::panic)]

use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use serde_json::{Value, json};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
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
fn serialized_prefix_scenario_agrees_through_model_in_process_and_real_binaries() {
    use verification::interpreters::{
        BinaryInterpreter, InProcessInterpreter, Interpreter, ModelInterpreter,
    };
    use verification::schema::Scenario;

    let scenario: Scenario = serde_json::from_str(include_str!(
        "../../corpus/input/prefix_literal.json"
    ))
    .expect("strict scenario");
    let model = ModelInterpreter.run(&scenario).expect("model transcript");
    assert_eq!(
        InProcessInterpreter
            .run(&scenario)
            .expect("in-process transcript"),
        model
    );

    let fux = binary("FUX_BIN", "target/debug/fux");
    let zor = binary("ZOR_BIN", "zor/target/debug/zor");
    let fixture_program = PathBuf::from(env!("CARGO_BIN_EXE_fux-fixture-child"));
    let environment = PrivateEnvironment::new("scenario");
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
    let server = OwnedChild::spawn(
        Command::new(&fux)
            .args(["serve", "--allow", &allow, "--name", "binary"])
            .env_clear()
            .envs(environment.variables())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    );
    wait_for_path(&environment.manager_socket());
    let mut primary = Jsonl::new(accept_with_deadline(&fixture_listener));
    assert_eq!(primary.receive()["event"], "ready");
    let client = TerminalChild::spawn(&fux, &environment, 24, 80);
    client.wait_for_output_bytes(b"connected.");
    let driver = PrefixBinaryDriver {
        client,
        primary,
        secondary: None,
        listener: &fixture_listener,
        server,
        environment: &environment,
    };
    let binary = BinaryInterpreter::new(driver)
        .run(&scenario)
        .expect("binary transcript");
    assert_eq!(binary, model);
}

struct PrefixBinaryDriver<'a> {
    client: TerminalChild,
    primary: Jsonl,
    secondary: Option<Jsonl>,
    listener: &'a UnixListener,
    server: OwnedChild,
    environment: &'a PrivateEnvironment,
}

impl verification::interpreters::BinaryDriver for PrefixBinaryDriver<'_> {
    fn input(
        &mut self,
        bytes: &[u8],
    ) -> Result<Vec<verification::interpreters::ObservedAction>, String> {
        use verification::interpreters::ObservedAction;
        if bytes == [2, b'|'] {
            self.client.write(bytes);
            let mut secondary = Jsonl::new(accept_with_deadline(self.listener));
            if secondary.receive()["event"] != "ready" {
                return Err("split fixture did not become ready".into());
            }
            self.secondary = Some(secondary);
            return Ok(vec![ObservedAction::Command("split_horizontal".into())]);
        }
        let expected = if bytes == [2, 2] { 1 } else { bytes.len() };
        self.primary
            .send(json!({"command":"read_exact", "bytes":expected}));
        self.client.write(bytes);
        let response = self.primary.receive();
        let encoded = response["bytes_hex"]
            .as_str()
            .ok_or_else(|| "fixture did not report forwarded bytes".to_owned())?;
        Ok(vec![ObservedAction::Forward(decode_hex(encoded)?)] )
    }

    fn cleanup(&mut self) -> Result<usize, String> {
        self.primary.send(json!({"command":"quit"}));
        if self.primary.receive()["event"] != "cleanup" {
            return Err("primary fixture did not clean up".into());
        }
        if let Some(secondary) = &mut self.secondary {
            secondary.send(json!({"command":"quit"}));
            if secondary.receive()["event"] != "cleanup" {
                return Err("secondary fixture did not clean up".into());
            }
        }
        self.client.detach();
        self.server.terminate(Signal::SIGTERM);
        self.server.wait();
        wait_for_absent(&self.environment.manager_socket());
        wait_for_absent(&self.environment.control_socket());
        if self.environment.descriptor().exists() {
            return Err("workspace descriptor leaked".into());
        }
        Ok(0)
    }
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
    assert!(notifications.contains("fixture"));
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
        self.root.join("run/fux/workspaces/binary.json")
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
    child: Box<dyn portable_pty::Child + Send + Sync>,
    writer: Box<dyn Write + Send>,
    reader: Option<std::thread::JoinHandle<()>>,
    output: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
}

impl TerminalChild {
    fn spawn(fux: &Path, environment: &PrivateEnvironment, rows: u16, columns: u16) -> Self {
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
        command.arg("binary");
        for (key, value) in environment.variables() {
            command.env(key, value);
        }
        let child = pair.slave.spawn_command(command).expect("spawn fux client");
        drop(pair.slave);
        let mut terminal = pair.master.try_clone_reader().expect("client reader");
        let output = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured = std::sync::Arc::clone(&output);
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
        });
        let writer = pair.master.take_writer().expect("client writer");
        Self {
            child,
            writer,
            reader: Some(reader),
            output,
        }
    }

    fn detach(&mut self) {
        self.write(&[1, b'd']);
        self.wait_success();
    }

    fn wait_success(&mut self) {
        self.wait_status(0);
    }

    fn wait_status(&mut self, expected: u32) {
        let deadline = Instant::now() + DEADLINE;
        loop {
            if let Some(status) = self.child.try_wait().expect("wait client") {
                assert_eq!(status.exit_code(), expected, "client status {status}");
                break;
            }
            assert!(Instant::now() < deadline, "client detach deadline expired");
            std::thread::sleep(Duration::from_millis(5));
        }
        if let Some(reader) = self.reader.take() {
            reader.join().expect("client reader");
        }
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
    reader: BufReader<UnixStream>,
}
impl Jsonl {
    fn new(stream: UnixStream) -> Self {
        Self {
            reader: BufReader::new(stream.try_clone().expect("clone fixture control")),
            writer: stream,
        }
    }
    fn send(&mut self, value: Value) {
        serde_json::to_writer(&mut self.writer, &value).expect("fixture request");
        self.writer.write_all(b"\n").expect("fixture newline");
        self.writer.flush().expect("fixture flush");
    }
    fn receive(&mut self) -> Value {
        let mut line = String::new();
        let deadline = Instant::now() + DEADLINE;
        loop {
            match self.reader.read_line(&mut line) {
                Ok(_) => break,
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(error) => panic!("fixture response: {error}"),
            }
            assert!(
                Instant::now() < deadline,
                "fixture response deadline expired"
            );
        }
        assert!(!line.is_empty(), "fixture closed unexpectedly");
        serde_json::from_str(&line).expect("fixture JSON")
    }

    fn expect_no_frame(&mut self, timeout: Duration) {
        self.reader
            .get_mut()
            .set_read_timeout(Some(timeout))
            .expect("short no-frame deadline");
        let mut line = String::new();
        let result = self.reader.read_line(&mut line);
        self.reader
            .get_mut()
            .set_read_timeout(Some(DEADLINE))
            .expect("restore frame deadline");
        assert!(
            matches!(
                result,
                Err(ref error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    )
            ),
            "unexpected duplicate event frame: {line:?} ({result:?})"
        );
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
