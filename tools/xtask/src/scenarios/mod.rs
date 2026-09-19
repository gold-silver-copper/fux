//! Multi-machine scenarios (prompt 4.5, capability rows 51–52): the built `fux` and `zor`
//! binaries as real processes on two disposable stacks — "Local" (the controller) and
//! "Remote" (the owner of the supervised tasks) — driven through the public CLIs and BRP
//! surfaces only. Every scenario prints one `PASS`/`FAIL` line with timings; the command exits
//! non-zero when any scenario fails. Direct loopback endpoints require no transport helper.

pub(crate) mod brp;
mod dashboard;
mod faults;
mod layouts;
mod plugin;
pub(crate) mod pty;
mod resume;
mod stack;
mod stress;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use brp::Descriptor;
use pty::Terminal;
use stack::Stack;

pub type Result<T, E = Box<dyn std::error::Error + Send + Sync>> = std::result::Result<T, E>;

pub fn err(message: impl Into<String>) -> Box<dyn std::error::Error + Send + Sync> {
    message.into().into()
}

/// Poll period of every wait.
const TICK: Duration = Duration::from_millis(25);
/// How long an observable product state change gets before the scenario gives up.
const WAIT: Duration = Duration::from_secs(15);
/// The supervision poll cadence is the product's; a freshness change gets a wider window.
const FRESHNESS_WAIT: Duration = Duration::from_secs(30);
const MACHINE: &str = "remote";
const TARGET: &str = "scn-target";
const DECOY: &str = "scn-decoy";

/// Polls `probe` until it yields a value or `timeout` passes.
pub fn until<T>(
    timeout: Duration,
    what: &str,
    mut probe: impl FnMut() -> Result<Option<T>>,
) -> Result<T> {
    let started = Instant::now();
    loop {
        if let Some(value) = probe()? {
            return Ok(value);
        }
        if started.elapsed() > timeout {
            return Err(format!("{what}: not observed within {timeout:?}").into());
        }
        std::thread::sleep(TICK);
    }
}

#[derive(Clone, Debug)]
pub struct Binaries {
    pub fux: PathBuf,
    pub zor: PathBuf,
    /// The directory holding both, put first on the stacks' PATH (`zor … attach` finds `fux`).
    pub dir: PathBuf,
}

enum Outcome {
    Pass(String),
    Fail(String),
}

struct Report {
    number: usize,
    name: &'static str,
    elapsed: Duration,
    outcome: Outcome,
}

impl Report {
    fn print(&self) {
        let (verdict, detail) = match &self.outcome {
            Outcome::Pass(detail) => ("PASS", detail),
            Outcome::Fail(detail) => ("FAIL", detail),
        };
        println!(
            "{verdict:<11} {} {:<34} {:>6} ms  {detail}",
            self.number,
            self.name,
            self.elapsed.as_millis()
        );
    }
}

/// Everything the scenarios share: both stacks, the supervised task's identity on Remote and
/// the live viewer handed off in scenario 1 (killed in scenario 4).
struct Fixture {
    local: Stack,
    remote: Stack,
    artifacts: PathBuf,
    /// `(pane, pid)` of the target attempt on Remote once scenario 1 launched it.
    target: Option<(u64, u32)>,
    decoy_pane: Option<u64>,
    viewer: Option<Terminal>,
}

pub fn run() -> Result<()> {
    let root = repo_root()?;
    let started = Instant::now();
    let bins = build(&root)?;
    println!(
        "built {} and {} in {} ms",
        bins.fux.display(),
        bins.zor.display(),
        started.elapsed().as_millis()
    );
    let started = Instant::now();
    let local = Stack::start("Local", &bins)?;
    let remote = Stack::start("Remote", &bins)?;
    println!(
        "stacks up in {} ms: Local zor {} fux {}; Remote zor {} fux {}",
        started.elapsed().as_millis(),
        local.zor()?.port,
        local.fux()?.port,
        remote.zor()?.port,
        remote.fux()?.port
    );
    let artifacts = std::env::var_os("FUX_SCENARIO_ARTIFACTS")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("target/scenario-artifacts").join(nonce()));
    std::fs::create_dir_all(&artifacts)?;
    println!("scenario evidence: {}", artifacts.display());
    let mut fixture = Fixture {
        local,
        remote,
        artifacts,
        target: None,
        decoy_pane: None,
        viewer: None,
    };
    type Scenario = (&'static str, fn(&mut Fixture) -> Result<Outcome>);
    let scenarios: [Scenario; 13] = [
        ("exact-target input", exact_target_input),
        ("independent authorization failure", authorization_failure),
        ("catalog reload", catalog_reload),
        ("viewer SIGKILL", viewer_sigkill),
        ("remote-owner survival", remote_owner_survival),
        ("connection loss and recovery", connection_recovery),
        ("guarded CLI mutation journal", guarded_cli_mutations),
        ("plugin action and owned cleanup", plugin::lifecycle),
        ("hosted dashboard exact handoff", dashboard::interaction),
        ("explicit native session resume", resume::native_resume),
        ("lost-reply crash reconciliation", faults::recovery),
        ("bounded pressure and shutdown", stress::limits),
        ("multi-viewer layout and gestures", layouts::interaction),
    ];
    let mut reports = Vec::with_capacity(scenarios.len());
    for (index, (name, scenario)) in scenarios.into_iter().enumerate() {
        let started = Instant::now();
        let outcome = match scenario(&mut fixture) {
            Ok(outcome) => outcome,
            Err(error) => Outcome::Fail(error.to_string()),
        };
        let report = Report {
            number: index + 1,
            name,
            elapsed: started.elapsed(),
            outcome,
        };
        report.print();
        reports.push(report);
    }
    let evidence: Vec<Value> = reports
        .iter()
        .map(|report| {
            let (verdict, detail) = match &report.outcome {
                Outcome::Pass(detail) => ("PASS", detail),
                Outcome::Fail(detail) => ("FAIL", detail),
            };
            json!({
                "scenario": report.name,
                "verdict": verdict,
                "elapsed_ms": report.elapsed.as_millis(),
                "detail": detail,
            })
        })
        .collect();
    std::fs::write(
        fixture.artifacts.join("results.json"),
        serde_json::to_vec_pretty(&json!({
            "fux": bins.fux,
            "zor": bins.zor,
            "transport": "direct-loopback",
            "scenarios": evidence,
        }))?,
    )?;
    for stack in [&fixture.local, &fixture.remote] {
        for log in ["zor-serve.log", "fux-serve.log"] {
            std::fs::write(
                fixture
                    .artifacts
                    .join(format!("{}-{log}", stack.label.to_ascii_lowercase())),
                stack.log(log),
            )?;
        }
    }
    let failed: Vec<usize> = reports
        .iter()
        .filter(|r| matches!(r.outcome, Outcome::Fail(_)))
        .map(|r| r.number)
        .collect();
    if failed.is_empty() {
        Ok(())
    } else {
        for stack in [&fixture.local, &fixture.remote] {
            eprintln!(
                "--- {} zor-serve.log tail ---\n{}",
                stack.label,
                stack.log("zor-serve.log")
            );
            eprintln!(
                "--- {} fux-serve.log tail ---\n{}",
                stack.label,
                stack.log("fux-serve.log")
            );
        }
        Err(format!("scenarios failed: {failed:?}").into())
    }
}

fn repo_root() -> Result<PathBuf> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = manifest
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| err("xtask manifest has no repository root"))?;
    if !root.join("crates/fux/Cargo.toml").is_file() {
        return Err(format!("{} is not the fux repository", root.display()).into());
    }
    Ok(root.to_path_buf())
}

/// `cargo build -p fux -p zor`; the executables are taken from cargo's own artifact messages,
/// so a custom target directory is honoured.
fn build(root: &Path) -> Result<Binaries> {
    let output = Command::new("cargo")
        .args([
            "build",
            "-p",
            "fux",
            "-p",
            "zor",
            "--message-format=json-render-diagnostics",
        ])
        .current_dir(root)
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()?;
    if !output.status.success() {
        return Err(format!("cargo build -p fux -p zor exited {}", output.status).into());
    }
    let mut fux = None;
    let mut zor = None;
    for line in output.stdout.split(|b| *b == b'\n') {
        let Ok(message) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        if message.get("reason").and_then(Value::as_str) != Some("compiler-artifact") {
            continue;
        }
        let Some(executable) = message.get("executable").and_then(Value::as_str) else {
            continue;
        };
        let kinds = message["target"]["kind"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if !kinds.iter().any(|k| k.as_str() == Some("bin")) {
            continue;
        }
        match message["target"]["name"].as_str() {
            Some("fux") => fux = Some(PathBuf::from(executable)),
            Some("zor") => zor = Some(PathBuf::from(executable)),
            _ => {}
        }
    }
    let fux = fux.ok_or_else(|| err("cargo reported no fux executable"))?;
    let zor = zor.ok_or_else(|| err("cargo reported no zor executable"))?;
    let dir = fux
        .parent()
        .ok_or_else(|| err("fux executable has no directory"))?
        .to_path_buf();
    if zor.parent() != Some(dir.as_path()) {
        return Err("fux and zor were built into different directories".into());
    }
    Ok(Binaries { fux, zor, dir })
}

// ---------------------------------------------------------------------------------------------
// Product observations shared by the scenarios.

/// `fux/pane.capture` lines joined with newlines.
fn capture(fux: &Descriptor, pane: u64) -> Result<String> {
    let reply = fux.call(
        "fux/pane.capture",
        json!({ "pane": pane, "scrollback": 200 }),
    )?;
    let lines = reply["lines"]
        .as_array()
        .ok_or_else(|| err("pane.capture without lines"))?;
    Ok(lines
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join("\n"))
}

/// The `fux/workspace.list` row of `pane` (state, pid, …).
fn pane_row(fux: &Descriptor, pane: u64) -> Result<Value> {
    let reply = fux.call("fux/workspace.list", json!({}))?;
    let workspaces = reply["workspaces"].as_array().cloned().unwrap_or_default();
    for workspace in workspaces {
        for root in workspace["roots"].as_array().cloned().unwrap_or_default() {
            for row in root["panes"].as_array().cloned().unwrap_or_default() {
                if row["id"].as_u64() == Some(pane) {
                    return Ok(row);
                }
            }
        }
    }
    Err(format!("pane {pane} is not listed").into())
}

/// The first pane of the default workspace (the bootstrapped shell).
fn first_pane(fux: &Descriptor) -> Result<u64> {
    let reply = fux.call("fux/workspace.list", json!({}))?;
    reply["workspaces"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|w| w["name"].as_str() == Some("default"))
        .flat_map(|w| w["roots"].as_array().cloned().unwrap_or_default())
        .flat_map(|r| r["panes"].as_array().cloned().unwrap_or_default())
        .find_map(|p| p["id"].as_u64())
        .ok_or_else(|| err("default workspace has no pane"))
}

/// Number of exact-attachment viewers on `fux`.
fn exact_viewers(fux: &Descriptor) -> Result<usize> {
    let reply = fux.call("fux/viewer.list", json!({}))?;
    Ok(reply["viewers"]
        .as_array()
        .map(|v| {
            v.iter()
                .filter(|r| r["exact"].as_bool() == Some(true))
                .count()
        })
        .unwrap_or(0))
}

/// The live attempt of `task` as seen through Local's `--machine` routing: `(pane, pid)`.
fn live_attempt(fixture: &Fixture, task: &str) -> Result<Option<(u64, u32)>> {
    let view = fixture
        .local
        .zor_json(&["--machine", MACHINE, "task", "inspect", task])?;
    Ok(view["attempts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|a| a["state"].as_str() == Some("live") && a["lost"].as_bool() != Some(true))
        .find_map(|a| {
            let pane = a["pane"].as_u64().filter(|p| *p != 0)?;
            let pid = u32::try_from(a["pid"].as_u64()?).ok()?;
            Some((pane, pid))
        }))
}

/// A machine row of `zor machine list`, by name.
fn machine_row(stack: &Stack, name: &str) -> Result<Option<Value>> {
    let list = stack.zor_json(&["machine", "list"])?;
    Ok(list["machines"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|m| m["name"].as_str() == Some(name))
        .cloned())
}

/// Waits until Local's supervision reports `name` with `freshness`.
fn await_freshness(stack: &Stack, name: &str, freshness: &str) -> Result<Duration> {
    let started = Instant::now();
    until(
        FRESHNESS_WAIT,
        &format!("machine {name} {freshness}"),
        || {
            let row = machine_row(stack, name)?;
            Ok(row
                .filter(|m| m["freshness"]["freshness"].as_str() == Some(freshness))
                .map(|_| started.elapsed()))
        },
    )
}

fn require_target(fixture: &Fixture) -> Result<(u64, u32)> {
    fixture
        .target
        .ok_or_else(|| err("scenario 1 did not launch the target task"))
}

/// `zor --machine remote attach TARGET` on a fresh controlling terminal, returned once Remote's
/// fux lists the exact viewer; also the time that took.
fn attach_viewer(fixture: &Fixture) -> Result<(Terminal, Duration)> {
    let remote_fux = fixture.remote.fux()?;
    let before = exact_viewers(&remote_fux)?;
    let mut command = fixture.local.command(fixture.local.zor_binary());
    command.args(["--machine", MACHINE, "attach", TARGET]);
    let started = Instant::now();
    let mut terminal = Terminal::spawn(command, 24, 80)?;
    let attached = until(WAIT, "exact viewer on Remote", || {
        if !terminal.running()? {
            return Err(format!(
                "attach exited early: {}",
                String::from_utf8_lossy(&terminal.output()).trim()
            )
            .into());
        }
        Ok((exact_viewers(&remote_fux)? > before).then(|| started.elapsed()))
    })?;
    Ok((terminal, attached))
}

/// Types `marker` on `terminal` and waits for it in `pane`'s capture on Remote.
fn type_and_observe(
    fixture: &Fixture,
    terminal: &Terminal,
    pane: u64,
    marker: &str,
) -> Result<Duration> {
    let remote_fux = fixture.remote.fux()?;
    // The viewer needs its first frame before it forwards keys; a short settle covers it.
    std::thread::sleep(Duration::from_millis(150));
    terminal.send(format!("{marker}\r").as_bytes())?;
    let started = Instant::now();
    until(WAIT, "marker in target pane", || {
        Ok(capture(&remote_fux, pane)?
            .contains(marker)
            .then(|| started.elapsed()))
    })
}

/// A copy of `descriptor` with one string field replaced, written 0600 (the product refuses
/// descriptors other users can read).
fn tampered_descriptor(
    dir: &Path,
    name: &str,
    descriptor: &Descriptor,
    field: &str,
) -> Result<PathBuf> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut raw = descriptor.raw.clone();
    let original = raw[field]
        .as_str()
        .ok_or_else(|| format!("descriptor `{field}` is not a string"))?;
    raw[field] = Value::String("0".repeat(original.len()));
    let path = dir.join(format!("{name}.brp.json"));
    let mut file = std::fs::File::options()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)?;
    std::io::Write::write_all(&mut file, serde_json::to_string_pretty(&raw)?.as_bytes())?;
    Ok(path)
}

fn machines_file(stack: &Stack) -> PathBuf {
    stack.config_dir().join("zor/machines.json")
}

fn read_catalog(stack: &Stack) -> Result<Value> {
    let path = machines_file(stack);
    Ok(serde_json::from_slice(
        &std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?,
    )?)
}

/// Atomic replace, as an editor would (temp file + rename).
fn write_catalog(stack: &Stack, catalog: &Value) -> Result<()> {
    let path = machines_file(stack);
    let temp = path.with_extension("json.tmp");
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temp)?;
    std::io::Write::write_all(&mut file, &serde_json::to_vec_pretty(catalog)?)?;
    file.sync_all()?;
    std::fs::rename(&temp, &path)?;
    Ok(())
}

fn catalog_entry_mut<'a>(catalog: &'a mut Value, id: &str) -> Result<&'a mut Value> {
    catalog["machines"]
        .as_array_mut()
        .into_iter()
        .flatten()
        .find(|m| m["id"].as_str() == Some(id))
        .ok_or_else(|| format!("catalog has no machine {id}").into())
}

// ---------------------------------------------------------------------------------------------
// Scenarios.

/// A task launched on Remote through Local's `--machine` routing; the viewer handoff
/// `zor --machine remote attach TASK` on a controlling pty; typed keys reach exactly that
/// pane: not a second Remote pane, not Local's own pane.
fn exact_target_input(fixture: &mut Fixture) -> Result<Outcome> {
    let local = &fixture.local;
    let remote_zor = fixture.remote.zor_descriptor_path();
    let remote_fux_path = fixture.remote.fux_descriptor_path();
    let attach = format!("default={}", remote_fux_path.display());
    local.zor_run(&[
        "machine",
        "add",
        MACHINE,
        "--control-brp",
        remote_zor.to_str().ok_or_else(|| err("path"))?,
        "--attach",
        &attach,
    ])?;
    let home = fixture.remote.path().join("home");
    let home = home.to_str().ok_or_else(|| err("path"))?;
    let started = Instant::now();
    for (task, title) in [(TARGET, "exact target"), (DECOY, "decoy")] {
        local.zor_run(&[
            "--machine",
            MACHINE,
            "task",
            "create",
            task,
            "--title",
            title,
            "--cwd",
            home,
        ])?;
        let operation = format!("{task}-launch-1");
        local.zor_run(&[
            "--machine",
            MACHINE,
            "task",
            "launch",
            task,
            "--operation",
            &operation,
            "--",
            "/bin/cat",
        ])?;
    }
    let target = until(WAIT, "target attempt live on Remote", || {
        live_attempt(fixture, TARGET)
    })?;
    let decoy = until(WAIT, "decoy attempt live on Remote", || {
        live_attempt(fixture, DECOY)
    })?;
    let launched = started.elapsed();
    if target.0 == decoy.0 {
        return Err(format!("target and decoy share pane {}", target.0).into());
    }
    fixture.target = Some(target);
    fixture.decoy_pane = Some(decoy.0);

    let remote_fux = fixture.remote.fux()?;
    let local_fux = fixture.local.fux()?;
    let local_pane = first_pane(&local_fux)?;
    let remote_pid = pane_row(&remote_fux, target.0)?["pid"].as_u64();
    if remote_pid != Some(u64::from(target.1)) {
        return Err(format!(
            "Remote fux lists pid {remote_pid:?} for pane {}, zor reported {}",
            target.0, target.1
        )
        .into());
    }

    let (terminal, attached) = attach_viewer(fixture)?;
    let marker = format!("SCN1-{}", nonce());
    let visible = type_and_observe(fixture, &terminal, target.0, &marker)?;
    let decoy_text = capture(&remote_fux, decoy.0)?;
    let local_text = capture(&local_fux, local_pane)?;
    if decoy_text.contains(&marker) {
        return Err("marker leaked into the decoy pane on Remote".into());
    }
    if local_text.contains(&marker) {
        return Err("marker leaked into Local's pane".into());
    }
    until(WAIT, "viewer painted the target marker", || {
        Ok(terminal
            .output()
            .windows(marker.len())
            .any(|w| w == marker.as_bytes())
            .then_some(()))
    })?;
    std::fs::write(
        fixture.artifacts.join("exact-target.ansi"),
        terminal.output(),
    )?;
    std::fs::write(
        fixture.artifacts.join("target.txt"),
        capture(&remote_fux, target.0)?,
    )?;
    std::fs::write(fixture.artifacts.join("decoy.txt"), decoy_text)?;
    std::fs::write(fixture.artifacts.join("local.txt"), local_text)?;
    fixture.viewer = Some(terminal);
    Ok(Outcome::Pass(format!(
        "2 tasks launched on Remote in {} ms; viewer attached (pane {}, pid {}) in {} ms; \
         keys visible in the target pane after {} ms; decoy pane {} and Local pane {} unchanged; \
         viewer painted the target marker",
        launched.as_millis(),
        target.0,
        target.1,
        attached.as_millis(),
        visible.as_millis(),
        decoy.0,
        local_pane
    )))
}

/// Two catalog entries for Remote with a wrong token and a wrong instance nonce are refused
/// by Remote's own authorization, while the correct entry and Remote's task are untouched.
fn authorization_failure(fixture: &mut Fixture) -> Result<Outcome> {
    let (pane, pid) = require_target(fixture)?;
    let local = &fixture.local;
    let remote = fixture.remote.zor()?;
    let dir = local.path().join("tmp");
    let bad_token = tampered_descriptor(&dir, "bad-token", &remote, "token")?;
    let bad_nonce = tampered_descriptor(&dir, "bad-nonce", &remote, "instance")?;
    for (name, path) in [("badtoken", &bad_token), ("badnonce", &bad_nonce)] {
        local.zor_run(&[
            "machine",
            "add",
            name,
            "--control-brp",
            path.to_str().ok_or_else(|| err("path"))?,
        ])?;
    }
    let started = Instant::now();
    let token_error = local.zor_fail(&["--machine", "badtoken", "status"])?;
    let nonce_error = local.zor_fail(&["--machine", "badnonce", "status"])?;
    let refused = started.elapsed();
    let token_error = token_error.trim().to_owned();
    let nonce_error = nonce_error.trim().to_owned();
    // Error wording is not an authorization contract: refusal plus independently classified
    // observations and unchanged owner state are the evidence.
    // The other machine: still authorized, still supervising, its task untouched.
    let started = Instant::now();
    local.zor_run(&["--machine", MACHINE, "status"])?;
    let healthy = started.elapsed();
    let token_stale = await_freshness(local, "badtoken", "unauthorized")?;
    let nonce_stale = await_freshness(local, "badnonce", "expired")?;
    let fresh = await_freshness(local, MACHINE, "fresh")?;
    let after = live_attempt(fixture, TARGET)?;
    if after != Some((pane, pid)) {
        return Err(format!(
            "Remote's target attempt changed: {after:?} != {:?}",
            (pane, pid)
        )
        .into());
    }
    let row = pane_row(&fixture.remote.fux()?, pane)?;
    if row["state"].as_str() != Some("live") {
        return Err(format!("Remote pane {pane} is {}", row["state"]).into());
    }
    for name in ["badtoken", "badnonce"] {
        local.zor_run(&["machine", "remove", name])?;
    }
    Ok(Outcome::Pass(format!(
        "wrong token and wrong nonce refused in {} ms (`{}` / `{}`); `--machine {MACHINE} status` \
         answered in {} ms; supervision marked token unauthorized and nonce expired after {} / {} ms with {MACHINE} \
         fresh ({} ms); Remote pane {pane} pid {pid} unchanged",
        refused.as_millis(),
        token_error,
        nonce_error,
        healthy.as_millis(),
        token_stale.as_millis(),
        nonce_stale.as_millis(),
        fresh.as_millis()
    )))
}

/// An external edit of `machines.json` (rename, id kept) is observed by `zor machine list`
/// after `zor machine reload`; routing follows the new name; `zor machine rename` writes back.
fn catalog_reload(fixture: &mut Fixture) -> Result<Outcome> {
    let local = &fixture.local;
    let mut catalog = read_catalog(local)?;
    let id = catalog["machines"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|m| m["name"].as_str() == Some(MACHINE))
        .and_then(|m| m["id"].as_str())
        .ok_or_else(|| err("catalog lacks the remote machine"))?
        .to_owned();
    let renamed = format!("{MACHINE}-renamed");
    catalog_entry_mut(&mut catalog, &id)?["name"] = Value::String(renamed.clone());
    write_catalog(local, &catalog)?;
    let started = Instant::now();
    local.zor_run(&["machine", "reload"])?;
    let observed = until(WAIT, "renamed machine listed", || {
        let row = machine_row(local, &renamed)?;
        Ok(row
            .filter(|m| m["id"].as_str() == Some(id.as_str()))
            .map(|_| started.elapsed()))
    })?;
    if machine_row(local, MACHINE)?.is_some() {
        return Err("old name still listed after reload".into());
    }
    local.zor_run(&["--machine", &renamed, "status"])?;
    let stale_name = local.zor_fail(&["--machine", MACHINE, "status"])?;
    let fresh = await_freshness(local, &renamed, "fresh")?;
    // Back through the product's own rename; the file must follow.
    local.zor_run(&["machine", "rename", &renamed, MACHINE])?;
    let restored = until(WAIT, "renamed back", || {
        Ok(machine_row(local, MACHINE)?
            .filter(|m| m["id"].as_str() == Some(id.as_str()))
            .map(|_| ()))
    })
    .map(|()| started.elapsed())?;
    let catalog = read_catalog(local)?;
    let file_name = catalog["machines"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|m| m["id"].as_str() == Some(id.as_str()))
        .and_then(|m| m["name"].as_str())
        .unwrap_or_default()
        .to_owned();
    if file_name != MACHINE {
        return Err(format!("machines.json names the machine {file_name:?} after rename").into());
    }
    // Invalid edits must not replace the last active catalog or retarget the stable id.
    let mut invalid = catalog.clone();
    let duplicate = catalog_entry_mut(&mut invalid, &id)?.clone();
    invalid["machines"]
        .as_array_mut()
        .ok_or_else(|| err("catalog machines"))?
        .push(duplicate);
    write_catalog(local, &invalid)?;
    let rejected = local.zor_fail(&["machine", "reload"]);
    let still_routed = local.zor_run(&["--machine", MACHINE, "status"]);
    write_catalog(local, &catalog)?;
    local.zor_run(&["machine", "reload"])?;
    rejected?;
    still_routed?;
    let row =
        machine_row(local, MACHINE)?.ok_or_else(|| err("machine lost after invalid reload"))?;
    if row["id"].as_str() != Some(id.as_str()) {
        return Err(err("invalid catalog edit changed machine identity"));
    }
    Ok(Outcome::Pass(format!(
        "external rename observed by `machine list` {} ms after reload (id {id} kept); \
         `--machine {renamed} status` routed, old name refused (`{}`); fresh after {} ms; \
         `machine rename` written back to machines.json ({} ms total); duplicate-id reload \
         refused while previous catalog continued routing",
        observed.as_millis(),
        stale_name.trim(),
        fresh.as_millis(),
        restored.as_millis()
    )))
}

/// `SIGKILL` to the attached viewer's session: Remote's pane and attempt survive, Remote drops
/// the exact viewer, and a fresh handoff reaches the same pane again.
fn viewer_sigkill(fixture: &mut Fixture) -> Result<Outcome> {
    let (pane, pid) = require_target(fixture)?;
    let mut terminal = match fixture.viewer.take() {
        Some(terminal) => terminal,
        None => attach_viewer(fixture)?.0,
    };
    let remote_fux = fixture.remote.fux()?;
    let before = exact_viewers(&remote_fux)?;
    if before == 0 {
        return Err("no exact viewer attached before the kill".into());
    }
    let viewer_pid = terminal.pid();
    let started = Instant::now();
    terminal.kill_session()?;
    let dropped = until(WAIT, "Remote dropped the killed viewer", || {
        Ok((exact_viewers(&remote_fux)? < before).then(|| started.elapsed()))
    })?;
    drop(terminal);
    let row = pane_row(&remote_fux, pane)?;
    if row["state"].as_str() != Some("live") || row["pid"].as_u64() != Some(u64::from(pid)) {
        return Err(format!("Remote pane {pane} after viewer kill: {row}").into());
    }
    let attempt = live_attempt(fixture, TARGET)?;
    if attempt != Some((pane, pid)) {
        return Err(format!("Remote attempt after viewer kill: {attempt:?}").into());
    }
    let (terminal, reattached) = attach_viewer(fixture)?;
    let marker = format!("SCN4-{}", nonce());
    let visible = type_and_observe(fixture, &terminal, pane, &marker)?;
    std::fs::write(fixture.artifacts.join("reattached.ansi"), terminal.output())?;
    drop(terminal);
    until(WAIT, "Remote dropped the second viewer", || {
        Ok((exact_viewers(&remote_fux)? == 0).then_some(()))
    })?;
    Ok(Outcome::Pass(format!(
        "viewer session {viewer_pid} SIGKILLed; Remote dropped it after {} ms; pane {pane} pid \
         {pid} live; reattached in {} ms and keys visible after {} ms",
        dropped.as_millis(),
        reattached.as_millis(),
        visible.as_millis()
    )))
}

/// `SIGKILL` to Local's zor (the controller): Remote's owner and its task are untouched; after
/// Local's zor restarts, `--machine remote` routing and supervision work again.
fn remote_owner_survival(fixture: &mut Fixture) -> Result<Outcome> {
    let (pane, pid) = require_target(fixture)?;
    let remote_zor_pid = fixture.remote.zor()?.pid;
    let killed = fixture.local.kill_zor()?;
    // Remote, asked directly, still owns the task.
    let started = Instant::now();
    let view = fixture.remote.zor_json(&["task", "inspect", TARGET])?;
    let owned = view["attempts"].as_array().into_iter().flatten().any(|a| {
        a["state"].as_str() == Some("live")
            && a["pane"].as_u64() == Some(pane)
            && a["pid"].as_u64() == Some(u64::from(pid))
    });
    if !owned {
        return Err(format!("Remote lost the attempt after Local's zor died: {view}").into());
    }
    let row = pane_row(&fixture.remote.fux()?, pane)?;
    if row["state"].as_str() != Some("live") {
        return Err(format!("Remote pane {pane} after Local's zor died: {row}").into());
    }
    if fixture.remote.zor()?.pid != remote_zor_pid {
        return Err("Remote's zor changed identity".into());
    }
    let remote_checked = started.elapsed();
    let started = Instant::now();
    fixture.local.start_zor()?;
    let restarted = started.elapsed();
    let started = Instant::now();
    fixture.local.zor_run(&["--machine", MACHINE, "status"])?;
    let routed = started.elapsed();
    let fresh = await_freshness(&fixture.local, MACHINE, "fresh")?;
    let attempt = live_attempt(fixture, TARGET)?;
    if attempt != Some((pane, pid)) {
        return Err(format!("attempt through the restarted controller: {attempt:?}").into());
    }
    Ok(Outcome::Pass(format!(
        "Local zor {killed} SIGKILLed; Remote (zor {remote_zor_pid}) still owns pane {pane} pid \
         {pid} ({} ms check); Local zor restarted in {} ms; `--machine {MACHINE} status` in {} \
         ms; supervision fresh after {} ms",
        remote_checked.as_millis(),
        restarted.as_millis(),
        routed.as_millis(),
        fresh.as_millis()
    )))
}

/// Disconnect the configured direct control endpoint without touching the owner. A failed
/// mutation must not reappear after catalog recovery; a completed launch must not be repeated
/// by reconnection or controller restart.
fn connection_recovery(fixture: &mut Fixture) -> Result<Outcome> {
    let (pane, pid) = require_target(fixture)?;
    let task = "scn-recovery";
    let proof = fixture.remote.path().join("home/recovery-launches");
    let cwd = fixture.remote.path().join("home");
    fixture.local.zor_run(&[
        "--machine",
        MACHINE,
        "task",
        "create",
        task,
        "--title",
        "recovery",
        "--cwd",
        cwd.to_str().ok_or_else(|| err("cwd"))?,
    ])?;
    // Closing the reserved listener makes this a real refused TCP connection, not a fake
    // server response, while leaving both owner processes and the attachment endpoint alive.
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
    let dead_port = listener.local_addr()?.port();
    drop(listener);
    let healthy_catalog = read_catalog(&fixture.local)?;
    let machine = machine_row(&fixture.local, MACHINE)?.ok_or_else(|| err("machine row"))?;
    let id = machine["id"]
        .as_str()
        .ok_or_else(|| err("machine id"))?
        .to_owned();
    let mut disconnected = healthy_catalog.clone();
    catalog_entry_mut(&mut disconnected, &id)?["control"]["port"] = json!(dead_port);
    write_catalog(&fixture.local, &disconnected)?;
    fixture.local.zor_run(&["machine", "reload"])?;
    // Always restore the endpoint, even if the expected failure is not observed.
    let failed = fixture.local.zor_fail(&[
        "--machine",
        MACHINE,
        "task",
        "launch",
        task,
        "--operation",
        "disconnected",
        "--",
        "/bin/sh",
        "-c",
        "printf 'launched\\n' >> recovery-launches; exec /bin/cat",
    ]);
    write_catalog(&fixture.local, &healthy_catalog)?;
    fixture.local.zor_run(&["machine", "reload"])?;
    let failure = failed?;
    let recovered = await_freshness(&fixture.local, MACHINE, "fresh")?;
    let view = fixture.remote.zor_json(&["task", "inspect", task])?;
    if view["attempts"]
        .as_array()
        .is_none_or(|rows| !rows.is_empty())
        || proof.exists()
    {
        return Err(format!("failed disconnected launch reached the owner: {view}").into());
    }
    fixture.local.zor_run(&[
        "--machine",
        MACHINE,
        "task",
        "launch",
        task,
        "--operation",
        "connected",
        "--",
        "/bin/sh",
        "-c",
        "printf 'launched\\n' >> recovery-launches; exec /bin/cat",
    ])?;
    let launched = until(WAIT, "recovered launch live", || {
        live_attempt(fixture, task)
    })?;
    until(WAIT, "launch artifact", || {
        Ok(proof.is_file().then_some(()))
    })?;
    // A second endpoint cycle and controller restart exercise persisted intent recovery.
    write_catalog(&fixture.local, &disconnected)?;
    fixture.local.zor_run(&["machine", "reload"])?;
    let _ = fixture.local.zor_fail(&["--machine", MACHINE, "status"])?;
    write_catalog(&fixture.local, &healthy_catalog)?;
    fixture.local.zor_run(&["machine", "reload"])?;
    fixture.local.kill_zor()?;
    fixture.local.start_zor()?;
    await_freshness(&fixture.local, MACHINE, "fresh")?;
    let view = fixture.remote.zor_json(&["task", "inspect", task])?;
    let attempts = view["attempts"].as_array().ok_or_else(|| err("attempts"))?;
    let artifact = std::fs::read_to_string(&proof)?;
    std::fs::write(fixture.artifacts.join("recovery-launches.txt"), &artifact)?;
    if attempts.len() != 1
        || artifact != "launched\n"
        || live_attempt(fixture, task)? != Some(launched)
    {
        return Err(format!(
            "recovery repeated or retargeted a launch: {view}; artifact {artifact:?}"
        )
        .into());
    }
    if live_attempt(fixture, TARGET)? != Some((pane, pid)) {
        return Err("connection recovery changed the original target identity".into());
    }
    Ok(Outcome::Pass(format!(
        "closed direct endpoint refused launch ({}); recovered in {} ms; owner artifact \
         records exactly one connected launch through another disconnect and controller restart; \
         original pane {pane} pid {pid} survived",
        failure.trim(),
        recovered.as_millis()
    )))
}

fn nonce() -> String {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{:x}-{:x}", std::process::id(), t & 0xffff_ffff)
}

fn guarded_cli_mutations(fixture: &mut Fixture) -> Result<Outcome> {
    let target = require_target(fixture)?;
    let home = fixture.remote.path().join("home");
    let home = home.to_str().ok_or_else(|| err("remote cwd"))?;
    let mut evidence = Vec::new();
    for (verb, task) in [("stop", "scn-cli-stop"), ("cancel", "scn-cli-cancel")] {
        fixture.local.zor_run(&[
            "--machine",
            MACHINE,
            "task",
            "create",
            task,
            "--title",
            task,
            "--cwd",
            home,
        ])?;
        let launch = format!("{task}-launch");
        fixture.local.zor_run(&[
            "--machine",
            MACHINE,
            "task",
            "launch",
            task,
            "--operation",
            &launch,
            "--",
            "/bin/cat",
        ])?;
        let pane = until(WAIT, "CLI mutation target live", || {
            live_attempt(fixture, task)
        })?;
        until(FRESHNESS_WAIT, "fresh exact mutation target", || {
            let snapshot = fixture
                .local
                .zor()?
                .call("zor/machine.inspect", json!({"machine":MACHINE}))?;
            let machine = &snapshot["machine"];
            let fresh = machine["freshness"]["freshness"].as_str() == Some("fresh");
            let observed = machine["view"]["agents"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|agent| {
                    agent["task"].as_str() == Some(task)
                        && agent["pane"].as_u64() == Some(pane.0)
                        && agent["pid"].as_u64() == Some(u64::from(pane.1))
                });
            Ok((fresh && observed).then_some(()))
        })?;
        let output = fixture.local.invoke(
            fixture.local.zor_binary(),
            &["--machine", MACHINE, "task", verb, task],
        )?;
        if !matches!(output.status.code(), Some(0 | 2)) {
            return Err(err(format!(
                "guarded {verb}: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        let reply: Value = serde_json::from_slice(&output.stdout)?;
        let operation = reply["operation"]
            .as_str()
            .ok_or_else(|| err("CLI operation identity missing"))?;
        let intent = until(WAIT, "durable remote acknowledgement", || {
            let status = fixture
                .local
                .zor()?
                .call("zor/machine.status", json!({"operation":operation}))?;
            let intent = status["intents"]
                .as_array()
                .and_then(|rows| rows.first())
                .ok_or_else(|| err("accepted CLI action has no local intent"))?;
            match intent["record"]["phase"].as_str() {
                Some("done") => Ok(Some(intent.clone())),
                Some("submitting") => Ok(None),
                _ => Err(err(format!("remote CLI action did not complete: {intent}"))),
            }
        })?;
        if intent["record"]["task"].as_str() != Some(task)
            || intent["record"]["pane"]["pane"].as_u64() != Some(pane.0)
            || intent["record"]["pane"]["pid"].as_u64() != Some(u64::from(pane.1))
            || intent["record"]["instance"].as_str()
                != Some(fixture.remote.zor()?.instance.as_str())
        {
            return Err(err(
                "durable CLI intent did not retain the selected process identity",
            ));
        }
        if verb == "stop" {
            until(WAIT, "guarded task process retired", || {
                Ok(live_attempt(fixture, task)?.is_none().then_some(()))
            })?;
        } else if live_attempt(fixture, task)? != Some(pane) {
            return Err(err(
                "coordination cancellation signalled or replaced the owned process",
            ));
        }
        let owner = fixture
            .remote
            .zor()?
            .call("zor/task.inspect", json!({"task":task}))?;
        if !owner["closed_ms"].is_u64() {
            return Err(err("acknowledged task control did not close coordination"));
        }
        evidence.push(
            json!({"reply":reply,"intent":intent,"owner":owner,"exit_code":output.status.code()}),
        );
    }
    fixture.local.kill_zor()?;
    fixture.local.start_zor()?;
    for item in &evidence {
        let restored = fixture.local.zor()?.call(
            "zor/machine.status",
            json!({"operation":item["reply"]["operation"]}),
        )?;
        if restored["intents"].as_array().and_then(|rows| rows.first()) != Some(&item["intent"]) {
            return Err(err("CLI intent changed across controller restart"));
        }
    }
    if live_attempt(fixture, TARGET)? != Some(target) {
        return Err(err("CLI control changed unrelated target"));
    }
    std::fs::write(
        fixture.artifacts.join("guarded-cli-intents.json"),
        serde_json::to_vec_pretty(&evidence)?,
    )?;
    Ok(Outcome::Pass("stop retired its exact process; cancel closed coordination without signalling its process; both used durable local intents retained across controller restart".into()))
}
