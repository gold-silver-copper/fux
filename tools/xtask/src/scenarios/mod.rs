//! Multi-machine scenarios (prompt 4.5, capability rows 51–52): the built `fux` and `zor`
//! binaries as real processes on two disposable stacks — "Local" (the controller) and
//! "Remote" (the owner of the supervised tasks) — driven through the public CLIs and BRP
//! surfaces only. Every scenario prints one `PASS`/`FAIL`/`UNAVAILABLE` line with timings;
//! the command exits non-zero when any scenario fails. `UNAVAILABLE` is reserved for the koh
//! composition gate, whose prerequisite (a helper forwarding TCP) is recorded, not met.

mod brp;
mod pty;
mod stack;

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
/// The reason koh composition cannot be gated here (prompt 3.10, batch context).
const KOH_REASON: &str = "koh forwards Unix-socket byte streams only; fux's attachment stream \
    is loopback TCP, so a koh transport cannot carry a viewer until koh forwards TCP upstream \
    (companions pin unchanged: upstream-then-publish)";

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
    Unavailable(String),
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
            Outcome::Unavailable(detail) => ("UNAVAILABLE", detail),
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
    let mut fixture = Fixture {
        local,
        remote,
        target: None,
        decoy_pane: None,
        viewer: None,
    };
    let scenarios: [(&'static str, fn(&mut Fixture) -> Result<Outcome>); 6] = [
        ("exact-target input", exact_target_input),
        ("independent authorization failure", authorization_failure),
        ("catalog reload", catalog_reload),
        ("viewer SIGKILL", viewer_sigkill),
        ("remote-owner survival", remote_owner_survival),
        ("koh composition", koh_composition),
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
    let failed: Vec<usize> = reports
        .iter()
        .filter(|r| matches!(r.outcome, Outcome::Fail(_)))
        .map(|r| r.number)
        .collect();
    if failed.is_empty() {
        Ok(())
    } else {
        for stack in [&fixture.local, &fixture.remote] {
            eprintln!("--- {} zor-serve.log tail ---\n{}", stack.label, stack.log("zor-serve.log"));
            eprintln!("--- {} fux-serve.log tail ---\n{}", stack.label, stack.log("fux-serve.log"));
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
        let kinds = message["target"]["kind"].as_array().cloned().unwrap_or_default();
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
    let reply = fux.call("fux/pane.capture", json!({ "pane": pane, "scrollback": 200 }))?;
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
        .map(|v| v.iter().filter(|r| r["exact"].as_bool() == Some(true)).count())
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
    until(FRESHNESS_WAIT, &format!("machine {name} {freshness}"), || {
        let row = machine_row(stack, name)?;
        Ok(row
            .filter(|m| m["freshness"].as_str() == Some(freshness))
            .map(|_| started.elapsed()))
    })
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
fn tampered_descriptor(dir: &Path, name: &str, descriptor: &Descriptor, field: &str) -> Result<PathBuf> {
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
    Ok(serde_json::from_slice(&std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?)?)
}

/// Atomic replace, as an editor would (temp file + rename).
fn write_catalog(stack: &Stack, catalog: &Value) -> Result<()> {
    let path = machines_file(stack);
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, serde_json::to_vec_pretty(catalog)?)?;
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
        local.zor_run(&["--machine", MACHINE, "task", "create", task, "--title", title, "--cwd", home])?;
        local.zor_run(&[
            "--machine",
            MACHINE,
            "task",
            "launch",
            task,
            "--operation",
            "launch-1",
            "--",
            "/bin/cat",
        ])?;
    }
    let target = until(WAIT, "target attempt live on Remote", || live_attempt(fixture, TARGET))?;
    let decoy = until(WAIT, "decoy attempt live on Remote", || live_attempt(fixture, DECOY))?;
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
    let painted = terminal.output().windows(marker.len()).any(|w| w == marker.as_bytes());
    fixture.viewer = Some(terminal);
    Ok(Outcome::Pass(format!(
        "2 tasks launched on Remote in {} ms; viewer attached (pane {}, pid {}) in {} ms; \
         keys visible in the target pane after {} ms; decoy pane {} and Local pane {} unchanged; \
         viewer repainted the line: {painted}",
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
    if !token_error.to_ascii_lowercase().contains("token") {
        return Err(format!("wrong token was refused without naming it: {token_error}").into());
    }
    if !(nonce_error.to_ascii_lowercase().contains("instance")
        || nonce_error.to_ascii_lowercase().contains("incarnation"))
    {
        return Err(format!("wrong nonce was refused without naming it: {nonce_error}").into());
    }
    // The other machine: still authorized, still supervising, its task untouched.
    let started = Instant::now();
    local.zor_run(&["--machine", MACHINE, "status"])?;
    let healthy = started.elapsed();
    let token_stale = await_freshness(local, "badtoken", "unauthorized")?;
    let nonce_stale = await_freshness(local, "badnonce", "unauthorized")?;
    let fresh = await_freshness(local, MACHINE, "fresh")?;
    let after = live_attempt(fixture, TARGET)?;
    if after != Some((pane, pid)) {
        return Err(format!("Remote's target attempt changed: {after:?} != {:?}", (pane, pid)).into());
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
         answered in {} ms; supervision marked them unauthorized after {} / {} ms with {MACHINE} \
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
    Ok(Outcome::Pass(format!(
        "external rename observed by `machine list` {} ms after reload (id {id} kept); \
         `--machine {renamed} status` routed, old name refused (`{}`); fresh after {} ms; \
         `machine rename` written back to machines.json ({} ms total)",
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
    let owned = view["attempts"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|a| {
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

/// The koh transport as the product models it: a catalog entry of kind `koh` is accepted and
/// supervised, and its status carries the recorded reason instead of a viewer. Never a
/// failure: the prerequisite is upstream.
fn koh_composition(fixture: &mut Fixture) -> Result<Outcome> {
    let local = &fixture.local;
    let mut catalog = read_catalog(local)?;
    let entry = json!({
        "id": "scn-koh",
        "name": "kohtest",
        "control": {
            "kind": "koh",
            "helper": "koh",
            "endpoint": "scenario.invalid:4433",
            "key_file": local.path().join("tmp/koh.key"),
            "direct": null,
            "relay_url": null,
        },
        "attachments": {},
    });
    let Some(machines) = catalog["machines"].as_array_mut() else {
        return Ok(Outcome::Unavailable(format!("{KOH_REASON}; catalog unreadable")));
    };
    machines.push(entry);
    write_catalog(local, &catalog)?;
    let started = Instant::now();
    let observed = match local.zor_run(&["machine", "reload"]) {
        Ok(_) => match until(WAIT, "koh machine listed", || {
            Ok(machine_row(local, "kohtest")?.map(|row| (row, started.elapsed())))
        }) {
            Ok((row, elapsed)) => Some((row, elapsed)),
            Err(error) => {
                eprintln!("koh: {error}");
                None
            }
        },
        Err(error) => {
            eprintln!("koh: {error}");
            None
        }
    };
    let status = local
        .zor_fail(&["--machine", "kohtest", "status"])
        .map(|e| e.trim().to_owned())
        .unwrap_or_else(|_| "status unexpectedly succeeded".into());
    // Leave the catalog as it was.
    if let Some(machines) = catalog["machines"].as_array_mut() {
        machines.retain(|m| m["id"].as_str() != Some("scn-koh"));
    }
    write_catalog(local, &catalog)?;
    let _ = local.zor_run(&["machine", "reload"]);
    Ok(Outcome::Unavailable(match observed {
        Some((row, elapsed)) => format!(
            "{KOH_REASON}; koh entry listed after {} ms as freshness {} ({}); `--machine kohtest \
             status`: {status}",
            elapsed.as_millis(),
            row["freshness"],
            row["problem"]
        ),
        None => format!("{KOH_REASON}; koh entry not accepted by the catalog; status: {status}"),
    }))
}

fn nonce() -> String {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{:x}-{:x}", std::process::id(), t & 0xffff_ffff)
}
