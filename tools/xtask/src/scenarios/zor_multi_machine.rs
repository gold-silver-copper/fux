//! Local plus two isolated remote stacks through real koh gateways under a controlling PTY:
//! saved machines, same-named tasks, unauthorized and unreachable hosts, exact attachment,
//! detach/return, missing and unauthorized attachment bindings, viewer failure, cancellation,
//! terminal restoration, owned-helper cleanup and remote owner survival.
//!
//! Every process is disposable and owned here; nothing touches personal sessions. The koh
//! executable is the exact companion the caller supplies (CI uses the clean published pin).
use crate::support::{
    local::{Root, completed, until},
    process::{self, Guard},
    terminal::Terminal,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

const WAIT: Duration = Duration::from_secs(15);

struct Binaries<'a> {
    fux: &'a Path,
    zor: &'a Path,
}

/// One disposable fux/zor owner. Fields drop in order: services before their root.
struct Stack {
    zor: Guard,
    fux: Guard,
    root: Root,
}
impl Stack {
    fn start(bins: &Binaries, prefix: &str) -> Result<Self> {
        let mut root = Root::new(prefix, &["/bin/sh".into()])?;
        for name in ["KOH_KEY_PASSPHRASE", "KOH_KEY_NEW_PASSPHRASE"] {
            root.env
                .insert(name.into(), "disposable-multi-machine-scenario".into());
        }
        let log = |name: &str| fs::File::create(root.path().join(name));
        let fux = Guard(
            root.command(bins.fux)
                .arg("serve")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(log("fux-serve.log")?)
                .spawn()?,
        );
        let zor = Guard(
            root.command(bins.zor)
                .arg("serve")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(log("zor-serve.log")?)
                .spawn()?,
        );
        let mut stack = Self { zor, fux, root };
        until(Duration::from_secs(10), || {
            stack.alive()?;
            Ok((stack.root.path().join("zor/control.sock").exists()
                && stack.root.control().exists())
            .then_some(()))
        })?;
        Ok(stack)
    }
    fn alive(&mut self) -> Result<()> {
        ensure!(self.fux.0.try_wait()?.is_none(), "owned fux exited");
        ensure!(self.zor.0.try_wait()?.is_none(), "owned zor exited");
        Ok(())
    }
    fn run(&self, binary: &Path, args: &[&str]) -> Result<Vec<u8>> {
        let mut command = self.root.command(binary);
        command.args(args);
        let reply = process::output(command, WAIT, 1024 * 1024)?;
        ensure!(
            reply.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&reply.stderr)
        );
        Ok(reply.stdout)
    }
    fn fail(&self, binary: &Path, args: &[&str]) -> Result<String> {
        let mut command = self.root.command(binary);
        command.args(args);
        let reply = process::output(command, WAIT, 1024 * 1024)?;
        ensure!(!reply.status.success(), "{args:?} unexpectedly succeeded");
        Ok(String::from_utf8_lossy(&reply.stderr).into_owned())
    }
    fn json(&self, binary: &Path, args: &[&str]) -> Result<Value> {
        Ok(serde_json::from_slice(&self.run(binary, args)?)?)
    }
    /// A CLI call while the interactive controller owns a PTY: its output must keep draining,
    /// otherwise the product's own stalled-terminal guard exits the dashboard.
    fn live(
        &self,
        binary: &Path,
        args: &[&str],
        terminal: &mut Terminal,
        expect_success: bool,
    ) -> Result<Vec<u8>> {
        use std::io::{Read, Seek, SeekFrom};
        let mut stdout = tempfile::tempfile()?;
        let mut stderr = tempfile::tempfile()?;
        let mut child = Guard(
            self.root
                .command(binary)
                .args(args)
                .stdin(Stdio::null())
                .stdout(stdout.try_clone()?)
                .stderr(stderr.try_clone()?)
                .spawn()?,
        );
        let status = until(WAIT, || {
            terminal.pump()?;
            Ok(child.0.try_wait()?)
        })
        .with_context(|| format!("{args:?} deadline"))?;
        let read = |file: &mut fs::File| -> Result<Vec<u8>> {
            file.seek(SeekFrom::Start(0))?;
            let mut bytes = Vec::new();
            file.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
            ensure!(bytes.len() <= 1024 * 1024, "CLI output limit exceeded");
            Ok(bytes)
        };
        let (out, err) = (read(&mut stdout)?, read(&mut stderr)?);
        ensure!(
            status.success() == expect_success,
            "{args:?} exited {status}: {}",
            String::from_utf8_lossy(&err)
        );
        Ok(if expect_success { out } else { err })
    }
    fn live_json(&self, binary: &Path, args: &[&str], terminal: &mut Terminal) -> Result<Value> {
        Ok(serde_json::from_slice(
            &self.live(binary, args, terminal, true)?,
        )?)
    }
    fn control(&self, workspace: &str, request: Value) -> Result<Value> {
        let mut request = request;
        request["id"] = json!(1);
        completed(
            &self.root.path().join(format!("fux/{workspace}.sock")),
            request,
        )
    }
    fn input_sequence(&self, pane: u64) -> Result<u64> {
        self.control(
            "agent",
            json!({"command":"capture","pane":pane,"max_bytes":65536}),
        )?["input_sequence"]
            .as_u64()
            .context("input sequence")
    }
}

/// A koh service gateway owned by the scenario, plus its advertisement.
struct Served {
    child: Guard,
    endpoint: String,
    direct: String,
}
fn serve(controller: &Stack, koh: &Path, socket: &Path, key: &Path, allow: &str) -> Result<Served> {
    let advertisement = controller.root.path().join(format!(
        "ad-{}.json",
        key.file_name().context("key name")?.to_string_lossy()
    ));
    let log = fs::File::create(advertisement.with_extension("log"))?;
    let mut child = Guard(
        controller
            .root
            .command(koh)
            .args(["gateway", "serve", "--local", "--socket"])
            .arg(socket)
            .arg("--key-file")
            .arg(key)
            .args(["--allow", allow])
            .stdin(Stdio::null())
            .stdout(fs::File::create(&advertisement)?)
            .stderr(log)
            .spawn()?,
    );
    let value: Value = until(Duration::from_secs(15), || {
        ensure!(
            child.0.try_wait()?.is_none(),
            "koh gateway serve exited: {}",
            fs::read_to_string(advertisement.with_extension("log"))?
        );
        let bytes = fs::read(&advertisement)?;
        ensure!(bytes.len() <= 65536, "advertisement too large");
        Ok(if bytes.ends_with(b"\n") {
            Some(serde_json::from_slice(&bytes)?)
        } else {
            None
        })
    })?;
    Ok(Served {
        child,
        endpoint: value["endpoint_id"].as_str().context("endpoint id")?.into(),
        direct: value["direct_addr"]
            .as_str()
            .context("direct address")?
            .into(),
    })
}

struct Remote {
    stack: Stack,
    pane: u64,
    pid: u64,
    instance: String,
    control: Served,
    attachment: Option<Served>,
}

fn helpers() -> Result<BTreeSet<PathBuf>> {
    Ok(fs::read_dir("/tmp")?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("zor-gw-"))
        })
        .collect())
}

/// Process table rows as (pid, ppid, pgid, command).
fn processes() -> Result<Vec<(i32, i32, i32, String)>> {
    let listing = process::output(
        {
            let mut command = std::process::Command::new("/bin/ps");
            command.args(["-axo", "pid=,ppid=,pgid=,command="]);
            command
        },
        Duration::from_secs(5),
        16 * 1024 * 1024,
    )?;
    ensure!(listing.status.success(), "ps failed");
    Ok(String::from_utf8_lossy(&listing.stdout)
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            Some((
                parts.next()?.parse().ok()?,
                parts.next()?.parse().ok()?,
                parts.next()?.parse().ok()?,
                line.trim_start()
                    .splitn(4, char::is_whitespace)
                    .nth(3)
                    .unwrap_or_default()
                    .to_owned(),
            ))
        })
        .collect())
}
/// Owned koh connect helpers for one endpoint and credential, identified by their arguments.
fn gateways(endpoint: &str, key: &str) -> Result<BTreeSet<i32>> {
    Ok(processes()?
        .into_iter()
        .filter(|(_, _, _, command)| {
            command.contains("gateway connect")
                && command.contains(endpoint)
                && command.contains(&format!("--key-file {key}"))
        })
        .map(|(pid, ..)| pid)
        .collect())
}

fn machine<'a>(view: &'a Value, name: &str) -> Result<&'a Value> {
    view["machines"]
        .as_array()
        .context("machines")?
        .iter()
        .find(|machine| machine["name"] == name)
        .with_context(|| format!("machine {name} missing from {view}"))
}
fn task_row(machine: &Value, task: &str) -> Result<usize> {
    machine["view"]["rows"]
        .as_array()
        .context("rows")?
        .iter()
        .position(|row| row["expected"]["task"] == task)
        .with_context(|| format!("task {task} row missing: {machine}"))
}

pub fn run(fux: &Path, zor: &Path, koh: &Path) -> Result<()> {
    let bins = Binaries { fux, zor };
    let koh_text = koh.to_str().context("koh path")?;
    let fux_text = fux.to_str().context("fux path")?;
    let helpers_before = helpers()?;
    let controller = Stack::start(&bins, "zmm-controller-")?;
    let key = controller.root.path().join("client.key");
    let key_text = key.to_str().context("key path")?;
    let identity = String::from_utf8(controller.run(koh, &["id", "--key-file", key_text])?)?
        .trim()
        .to_owned();
    let stranger_key = controller.root.path().join("stranger.key");
    let stranger = String::from_utf8(controller.run(
        koh,
        &["id", "--key-file", stranger_key.to_str().context("key")?],
    )?)?
    .trim()
    .to_owned();
    ensure!(identity != stranger, "distinct identities expected");

    // Two isolated remotes with identically named tasks in a non-default workspace.
    let mut remotes = Vec::new();
    for name in ["first", "second"] {
        let stack = Stack::start(&bins, &format!("zmm-{name}-"))?;
        stack.run(fux, &["workspace", "new", "agent"])?;
        until(Duration::from_secs(10), || {
            Ok(stack
                .root
                .path()
                .join("fux/agent.sock")
                .exists()
                .then_some(()))
        })?;
        let listing = stack.control("agent", json!({"command":"list"}))?;
        let workspace = listing["workspaces"]
            .as_array()
            .context("workspaces")?
            .iter()
            .find(|workspace| workspace["name"] == "agent")
            .context("agent workspace")?;
        let pane = &workspace["tabs"][0]["panes"][0];
        let pane_id = pane["id"].as_u64().context("pane id")?;
        let instance = listing["instance"].as_str().context("instance")?.to_owned();
        stack.run(
            zor,
            &[
                "task",
                "adopt",
                "same",
                "--title",
                &format!("{name} task"),
                "--instance",
                &instance,
                "--workspace",
                "agent",
                "--pane",
                &pane_id.to_string(),
            ],
        )?;
        let control = serve(
            &controller,
            koh,
            &stack.root.path().join("zor/control.sock"),
            &controller.root.path().join(format!("{name}-control.key")),
            &identity,
        )?;
        controller.run(
            zor,
            &[
                "machine",
                "add",
                name,
                "--endpoint",
                &control.endpoint,
                "--key-file",
                key_text,
                "--direct",
                &control.direct,
            ],
        )?;
        // Attachment is a separate grant: only the first machine's workspace is authorized
        // for this identity; the second machine's workspace is served for a stranger.
        let attachment = serve(
            &controller,
            koh,
            &stack.root.path().join("fux/agent.attach.sock"),
            &controller.root.path().join(format!("{name}-attach.key")),
            if name == "first" {
                &identity
            } else {
                &stranger
            },
        )?;
        if name == "first" {
            controller.run(
                zor,
                &[
                    "machine",
                    "bind",
                    name,
                    "--workspace",
                    "agent",
                    "--endpoint",
                    &attachment.endpoint,
                    "--key-file",
                    key_text,
                    "--direct",
                    &attachment.direct,
                ],
            )?;
        }
        remotes.push(Remote {
            pid: pane["pid"].as_u64().context("pane pid")?,
            pane: pane_id,
            instance,
            stack,
            control,
            attachment: Some(attachment),
        });
    }
    let (first, second) = (&remotes[0], &remotes[1]);
    ensure!(
        first.instance != second.instance,
        "remote fux incarnations collide"
    );

    // Unauthorized control: a saved profile whose key is not in the first server's allow list.
    controller.run(
        zor,
        &[
            "machine",
            "add",
            "intruder",
            "--endpoint",
            &first.control.endpoint,
            "--key-file",
            stranger_key.to_str().context("key")?,
            "--direct",
            &first.control.direct,
        ],
    )?;
    // Unreachable control: a syntactically valid endpoint nobody serves.
    let offline_key = controller.root.path().join("offline-server.key");
    let offline_endpoint = String::from_utf8(controller.run(
        koh,
        &["id", "--key-file", offline_key.to_str().context("key")?],
    )?)?
    .trim()
    .to_owned();
    controller.run(
        zor,
        &[
            "machine",
            "add",
            "offline",
            "--endpoint",
            &offline_endpoint,
            "--key-file",
            key_text,
            "--direct",
            "127.0.0.1:1",
        ],
    )?;

    // Profile lifecycle: listing, inspection and rename keep the stable identity.
    let listing: Value = controller.json(zor, &["machine", "list"])?;
    ensure!(
        listing["local"]["name"] == "Local",
        "Local missing from {listing}"
    );
    let names: Vec<&str> = listing["machines"]
        .as_array()
        .context("machine list")?
        .iter()
        .filter_map(|machine| machine["name"].as_str())
        .collect();
    for name in ["first", "second", "intruder", "offline"] {
        ensure!(
            names.contains(&name),
            "machine {name} missing from {listing}"
        );
    }
    let second_before: Value = controller.json(zor, &["machine", "inspect", "second"])?;
    controller.run(zor, &["machine", "rename", "second", "second-renamed"])?;
    let second_after: Value = controller.json(zor, &["machine", "inspect", "second-renamed"])?;
    ensure!(
        second_before["id"] == second_after["id"] && second_before["id"].is_string(),
        "rename changed the stable machine identity"
    );
    controller.run(zor, &["machine", "rename", "second-renamed", "second"])?;
    ensure!(
        controller
            .fail(
                zor,
                &[
                    "machine",
                    "add",
                    "first",
                    "--endpoint",
                    &first.control.endpoint,
                    "--key-file",
                    key_text
                ]
            )?
            .contains("duplicate machine name"),
        "duplicate name accepted"
    );
    ensure!(
        controller
            .fail(zor, &["machine", "add", "Local"])?
            .to_lowercase()
            .contains("local"),
        "Local alias shadowing accepted"
    );

    // One-shot aggregate read: healthy hosts are fresh and distinct while the unauthorized
    // and unreachable profiles fail independently and never resolve to Local.
    let aggregate: Value = controller.json(
        zor,
        &[
            "--koh-binary",
            koh_text,
            "dashboard",
            "--all-machines",
            "--once",
        ],
    )?;
    for name in ["Local", "first", "second"] {
        ensure!(
            machine(&aggregate, name)?["fresh"] == true,
            "{name} not fresh: {aggregate}"
        );
    }
    for name in ["intruder", "offline"] {
        let entry = machine(&aggregate, name)?;
        ensure!(
            entry["fresh"] == false && entry["view"].is_null(),
            "{name} produced authoritative evidence: {entry}"
        );
    }
    let first_view = machine(&aggregate, "first")?;
    let second_view = machine(&aggregate, "second")?;
    ensure!(
        first_view["id"] != second_view["id"]
            && first_view["view"]["service_instance"] != second_view["view"]["service_instance"],
        "remote identities collide"
    );
    let first_index = task_row(first_view, "same")?;
    let second_index = task_row(second_view, "same")?;
    ensure!(
        first_view["view"]["rows"][first_index]["expected"]["target"]["instance"] == first.instance
            && second_view["view"]["rows"][second_index]["expected"]["target"]["instance"]
                == second.instance,
        "same-named tasks lost their fux identity"
    );
    let refused = controller.fail(
        zor,
        &[
            "--machine",
            "intruder",
            "--koh-binary",
            koh_text,
            "dashboard",
            "--once",
        ],
    )?;
    ensure!(
        refused.contains("intruder") && !refused.contains("Local"),
        "unauthorized control did not fail explicitly: {refused}"
    );
    ensure!(
        controller
            .fail(
                zor,
                &["--machine", "missing-profile", "dashboard", "--once"]
            )?
            .contains("missing-profile"),
        "unknown selector did not fail explicitly"
    );
    ensure!(
        !controller
            .root
            .path()
            .join("state/zor/journal.json")
            .exists(),
        "remote routing created a controller task store"
    );

    // The interactive controller runs under its own controlling terminal; a shell wrapper
    // outlives it so the restored terminal attributes can be read after it exits.
    let before_path = controller.root.path().join("stty-before");
    let after_path = controller.root.path().join("stty-after");
    let script = format!(
        "stty -a > '{}'; \"$@\"; code=$?; stty -a > '{}'; exit $code",
        before_path.display(),
        after_path.display()
    );
    let mut args = vec![
        "-c",
        &script,
        "zor",
        zor.to_str().context("zor path")?,
        "--koh-binary",
        koh_text,
        "--fux-binary",
        fux_text,
        "dashboard",
        "--all-machines",
    ];
    let mut terminal =
        Terminal::start_with_size(&controller.root, Path::new("/bin/sh"), &args, 30, 180)?;
    args.clear();
    terminal.wait_for("first:live", WAIT)?;
    terminal.wait_for("second:live", WAIT)?;
    terminal.wait_for("intruder:unavailable", WAIT)?;
    terminal.wait_for("offline:unavailable", WAIT)?;
    terminal.wait_for("zor dashboard | All machines", WAIT)?;
    terminal.checkpoint("multi-machine-aggregate")?;
    let concurrent = controller.live_json(
        zor,
        &[
            "--koh-binary",
            koh_text,
            "dashboard",
            "--all-machines",
            "--once",
        ],
        &mut terminal,
    )?;
    ensure!(
        machine(&concurrent, "first")?["fresh"] == true,
        "a live dashboard blocked a concurrent read"
    );

    // Navigate by scope and identity into the first machine's task.
    let mut mark = terminal.raw_len()?;
    terminal.send(b"\t")?;
    terminal.wait_for_since("zor dashboard | Local", mark, WAIT)?;
    mark = terminal.raw_len()?;
    terminal.send(b"\t")?;
    terminal.wait_for_since("zor dashboard | first", mark, WAIT)?;
    for _ in 0..first_index {
        terminal.send(b"j")?;
    }
    terminal.wait_for_since("> first", mark, WAIT)?;
    mark = terminal.raw_len()?;
    terminal.send(b"\r")?;
    terminal.wait_for_since("preparing action...", mark, WAIT)?;
    let control_helpers = (
        gateways(&first.control.endpoint, key_text)?,
        gateways(&second.control.endpoint, key_text)?,
    );
    ensure!(
        control_helpers.0.len() == 1 && control_helpers.1.len() == 1,
        "inspection did not reuse exactly one control helper per machine: {control_helpers:?}"
    );
    let attach_endpoint = &first
        .attachment
        .as_ref()
        .context("first attachment")?
        .endpoint;
    terminal.wait_for_since("first / same / attempt", mark, WAIT)?;
    terminal.checkpoint("multi-machine-inspection")?;
    terminal.send(b"\x1b")?;
    terminal.wait_for_since("zor dashboard | first", mark, WAIT)?;

    // Exact attachment to the first remote's non-default workspace pane.
    mark = terminal.raw_len()?;
    terminal.send(b"a")?;
    terminal.wait_for_raw(b"\x1b[?2026l", mark, WAIT)?;
    terminal.send(b"printf \"HANDOFF_%s\\n\" OK\r")?;
    terminal.wait_for_since("HANDOFF_OK", mark, WAIT)?;
    terminal.checkpoint("multi-machine-viewer")?;
    let target = first.stack.control(
        "agent",
        json!({"command":"capture","pane":first.pane,"max_bytes":65536}),
    )?;
    ensure!(
        target["text"]
            .as_str()
            .is_some_and(|text| text.contains("HANDOFF_OK")),
        "input did not reach the exact pane: {target}"
    );
    let delivered = target["input_sequence"].as_u64().context("sequence")?;
    ensure!(
        second.stack.input_sequence(second.pane)? == 0,
        "input leaked to the other remote's same-named task"
    );
    ensure!(
        gateways(attach_endpoint, key_text)?.len() == 1,
        "attachment did not use exactly one owned attachment helper"
    );

    // Detach: bytes after the detach chord never reach the pane; selection is retained.
    mark = terminal.raw_len()?;
    terminal.send(b"\x01dSUFFIX_MUST_NOT_REACH_PANE")?;
    terminal.wait_for_since("Detached; refreshing the selection", mark, WAIT)?;
    terminal.wait_for_since("zor dashboard | first", mark, WAIT)?;
    terminal.checkpoint("multi-machine-return")?;
    ensure!(
        first.stack.input_sequence(first.pane)? == delivered,
        "detach suffix reached the pane"
    );
    until(Duration::from_secs(5), || {
        Ok(gateways(attach_endpoint, key_text)?
            .is_empty()
            .then_some(()))
    })
    .context("attachment helper was not cleaned up after detach")?;
    mark = terminal.raw_len()?;
    terminal.send(b"\r")?;
    terminal.wait_for_since("first / same / attempt", mark, WAIT)?;
    terminal.send(b"\x1b")?;
    terminal.wait_for_since("zor dashboard | first", mark, WAIT)?;

    // The second machine's same-named task has no binding for its workspace.
    mark = terminal.raw_len()?;
    terminal.send(b"\t")?;
    terminal.wait_for_since("zor dashboard | second", mark, WAIT)?;
    for _ in 0..second_index {
        terminal.send(b"j")?;
    }
    terminal.wait_for_since("> second", mark, WAIT)?;
    mark = terminal.raw_len()?;
    terminal.send(b"a")?;
    terminal.wait_for_since("no attachment binding for workspace agent", mark, WAIT)?;
    terminal.checkpoint("multi-machine-missing-binding")?;
    ensure!(
        !terminal.raw_contains_since(b"\x1b[?2026h", mark)?,
        "missing binding fell back to a viewer"
    );

    // Bind the second workspace to a gateway that does not authorize this identity and
    // reload the catalog live: control stays connected, attachment fails independently.
    let unauthorized = second.attachment.as_ref().context("second attachment")?;
    controller.live(
        zor,
        &[
            "machine",
            "bind",
            "second",
            "--workspace",
            "agent",
            "--endpoint",
            &unauthorized.endpoint,
            "--key-file",
            key_text,
            "--direct",
            &unauthorized.direct,
        ],
        &mut terminal,
        true,
    )?;
    mark = terminal.raw_len()?;
    terminal.send(b"R")?;
    terminal.wait_for_since("Machine catalog reloaded", mark, WAIT)?;
    ensure!(
        (
            gateways(&first.control.endpoint, key_text)?,
            gateways(&second.control.endpoint, key_text)?
        ) == control_helpers,
        "attachment-only edit restarted a control helper"
    );
    terminal.wait_for_since("> second", mark, WAIT)?;
    mark = terminal.raw_len()?;
    terminal.send(b"a")?;
    terminal.wait_for_since("Attachment ended", mark, Duration::from_secs(30))?;
    terminal.wait_for_since("zor dashboard | second", mark, WAIT)?;
    terminal.checkpoint("multi-machine-unauthorized-attachment")?;
    ensure!(
        second.stack.input_sequence(second.pane)? == 0,
        "unauthorized attachment delivered input"
    );
    until(Duration::from_secs(5), || {
        Ok(gateways(&unauthorized.endpoint, key_text)?
            .is_empty()
            .then_some(()))
    })
    .context("failed attachment helper leaked")?;

    // Scope cycling through every saved machine keeps the first machine's original selection.
    for scope in ["intruder", "offline", "All machines", "Local", "first"] {
        mark = terminal.raw_len()?;
        terminal.send(b"\t")?;
        terminal.wait_for_since(&format!("zor dashboard | {scope}"), mark, WAIT)?;
    }
    terminal.wait_for_since("> first", mark, WAIT)?;
    mark = terminal.raw_len()?;
    terminal.send(b"a")?;
    terminal.wait_for_raw(b"\x1b[?2026l", mark, WAIT)?;
    let viewer = viewer_pid(&terminal, fux)?;
    mark = terminal.raw_len()?;
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(viewer),
        nix::sys::signal::Signal::SIGKILL,
    )?;
    terminal.wait_for_since("Attachment ended: viewer ended", mark, WAIT)?;
    terminal.wait_for_since("zor dashboard | first", mark, WAIT)?;
    ensure!(
        terminal.raw_contains_since(b"\x1b[?1003l", mark)?
            && terminal.raw_contains_since(b"\x1b[?2004l", mark)?,
        "viewer reporting modes were not reset after SIGKILL"
    );
    terminal.checkpoint("multi-machine-viewer-killed")?;
    mark = terminal.raw_len()?;
    terminal.send(b"\r")?;
    terminal.wait_for_since("first / same / attempt", mark, WAIT)?;
    terminal.send(b"\x1b")?;
    terminal.wait_for_since("zor dashboard | first", mark, WAIT)?;

    // Cancelled preparation never opens a viewer or sends input.
    mark = terminal.raw_len()?;
    terminal.send(b"a")?;
    terminal.wait_for_since("preparing action...", mark, WAIT)?;
    terminal.send(b"\x1b")?;
    terminal.wait_for_since("preparation cancelled", mark, WAIT)?;
    ensure!(
        !terminal.raw_contains_since(b"\x1b[?2026h", mark)?,
        "cancelled preparation opened a viewer"
    );
    terminal.checkpoint("multi-machine-cancelled")?;
    ensure!(
        first.stack.input_sequence(first.pane)? == delivered,
        "recovery or cancellation replayed input"
    );

    // Removing a profile removes configuration and owned connections only.
    controller.live(zor, &["machine", "remove", "intruder"], &mut terminal, true)?;
    mark = terminal.raw_len()?;
    terminal.send(b"R")?;
    terminal.wait_for_since("Machine catalog reloaded", mark, WAIT)?;
    let names: Vec<String> =
        controller.live_json(zor, &["machine", "list"], &mut terminal)?["machines"]
            .as_array()
            .context("list")?
            .iter()
            .filter_map(|machine| machine["name"].as_str().map(str::to_owned))
            .collect();
    ensure!(
        !names.iter().any(|name| name == "intruder"),
        "removal kept the profile"
    );

    // Quit: terminal restored, helpers cleaned, every remote owner alive and untouched.
    terminal.send(b"q")?;
    let status = terminal.wait(Duration::from_secs(10))?;
    ensure!(status.success(), "controller exited with {status}");
    let normalize = |bytes: Vec<u8>| -> String {
        String::from_utf8_lossy(&bytes)
            .split_whitespace()
            .filter(|word| !word.ends_with("pendin"))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let (before, after) = (fs::read(&before_path)?, fs::read(&after_path)?);
    ensure!(!before.is_empty(), "terminal attributes were not captured");
    ensure!(
        normalize(before) == normalize(after),
        "terminal attributes not restored"
    );
    until(Duration::from_secs(5), || {
        Ok((helpers()? == helpers_before).then_some(()))
    })
    .context("owned helper leaked after controller exit")?;
    ensure!(
        first.stack.input_sequence(first.pane)? == delivered
            && second.stack.input_sequence(second.pane)? == 0,
        "controller exit changed pane input"
    );
    for remote in &mut remotes {
        remote.stack.alive()?;
        ensure!(
            remote.control.child.0.try_wait()?.is_none(),
            "controller exit stopped a remote gateway"
        );
        let listing = remote.stack.control("agent", json!({"command":"list"}))?;
        ensure!(
            listing["instance"] == remote.instance,
            "remote fux incarnation changed"
        );
        let pane = listing["workspaces"]
            .as_array()
            .context("workspaces")?
            .iter()
            .flat_map(|workspace| workspace["tabs"].as_array().cloned().unwrap_or_default())
            .flat_map(|tab| tab["panes"].as_array().cloned().unwrap_or_default())
            .find(|pane| pane["id"] == remote.pane)
            .context("remote pane disappeared")?;
        ensure!(
            pane["pid"] == remote.pid,
            "remote pane process was replaced"
        );
        let tasks: Value = remote.stack.json(zor, &["task", "list"])?;
        ensure!(
            tasks["tasks"]
                .as_array()
                .is_some_and(|tasks| tasks.len() == 1),
            "remote task store changed: {tasks}"
        );
    }
    println!(
        "{}",
        json!({
            "scenario": "zor-multi-machine",
            "machines": ["Local", "first", "second", "intruder", "offline"],
            "delivered_input_sequence": delivered,
            "koh": koh_text,
        })
    );
    Ok(())
}

/// The one live viewer: a child of the controller inside the terminal's foreground group.
fn viewer_pid(terminal: &Terminal, fux: &Path) -> Result<i32> {
    let leader = i32::try_from(terminal.child.0.id())?;
    let rows = processes()?;
    let controllers: Vec<i32> = rows
        .iter()
        .filter(|(_, ppid, _, _)| *ppid == leader)
        .map(|(pid, ..)| *pid)
        .collect();
    ensure!(
        controllers.len() == 1,
        "expected one controller: {controllers:?}"
    );
    let prefix = format!("{} attach", fux.display());
    let viewers: Vec<&(i32, i32, i32, String)> = rows
        .iter()
        .filter(|(_, ppid, _, command)| *ppid == controllers[0] && command.starts_with(&prefix))
        .collect();
    ensure!(viewers.len() == 1, "expected one viewer: {viewers:?}");
    ensure!(
        viewers[0].2 == leader,
        "viewer left the controlling terminal's foreground group"
    );
    Ok(viewers[0].0)
}
