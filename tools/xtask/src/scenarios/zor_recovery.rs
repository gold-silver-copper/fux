//! Local recovery preserves ownership, uncertainty, history and no-replay guarantees.
use crate::support::{
    contention::cli_busy,
    local::{Root, completed, until},
    process::{self, Guard, OwnedProcess},
    service,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::Path,
    process::Stdio,
    time::{Duration, Instant},
};

fn stop(service: &mut Option<Guard>) -> Result<()> {
    if let Some(owner) = service.as_mut() {
        owner.0.terminate()?;
        ensure!(
            process::wait(&mut owner.0, Duration::from_secs(10))?.success(),
            "service exit"
        );
    }
    Ok(())
}
fn panes(listing: &Value) -> Result<Vec<Value>> {
    let mut result = Vec::new();
    for workspace in listing["workspaces"].as_array().context("workspaces")? {
        for tab in workspace["tabs"].as_array().context("tabs")? {
            result.extend(tab["panes"].as_array().context("panes")?.iter().cloned());
        }
    }
    Ok(result)
}
fn parked<T>(control: &Path, run: impl FnOnce() -> Result<T>) -> Result<T> {
    let parked = control.with_file_name("parked-control");
    fs::rename(control, &parked)?;
    let result = run();
    fs::rename(parked, control)?;
    result
}
pub(super) fn run(fux: &Path, zor: &Path) -> Result<()> {
    let root = Root::new("zrecover-rs-", &["/bin/cat".into()])?;
    let mut server = root.server(fux)?;
    let mut service_owner = None;
    let control = root.control();
    let listing = || completed(&control, json!({"id":1,"command":"list"}));
    let instance_value = listing()?["instance"].clone();
    let instance = instance_value.as_str().context("instance")?;
    let raw = |args: &[&str], timeout| {
        let mut command = root.command(zor);
        command.args(args);
        process::output(command, timeout, 1024 * 1024)
    };
    let cli = |args: &[&str], ok: bool| -> Result<Value> {
        let reply = raw(args, Duration::from_secs(12))?;
        ensure!(
            reply.status.success() == ok,
            "CLI {args:?}: {} {}",
            String::from_utf8_lossy(&reply.stdout),
            String::from_utf8_lossy(&reply.stderr)
        );
        if ok {
            Ok(serde_json::from_slice(&reply.stdout)?)
        } else {
            Ok(String::from_utf8(reply.stderr)?.into())
        }
    };
    let inspect = |id| cli(&["task", "inspect", id], true);
    let start_service = || -> Result<Guard> {
        Ok(Guard(
            root.command(zor)
                .arg("serve")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()?,
        ))
    };
    let await_state = |owner: &mut Guard, id: &str, pointer: &str, expected: &str| -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .context("startup lifecycle sweep deadline")?;
            ensure!(
                owner.0.try_wait()?.is_none(),
                "service exited during recovery"
            );
            let reply = raw(
                &["task", "inspect", id],
                remaining.min(Duration::from_secs(5)),
            )?;
            if reply.status.success() {
                let value: Value = serde_json::from_slice(&reply.stdout)?;
                if value.pointer(pointer) == Some(&json!(expected)) {
                    return Ok(());
                }
            } else {
                ensure!(
                    cli_busy(reply.status.code(), &reply.stderr),
                    "unexpected recovery inspect failure: {}",
                    String::from_utf8_lossy(&reply.stderr)
                );
            }
            std::thread::sleep(Duration::from_millis(30));
        }
    };
    let scenario = (|| -> Result<()> {
        ensure!(
            cli(&["task", "recover"], true)? == json!({"selected":null,"pending":0}),
            "empty recovery"
        );
        let journal = root.path().join("state/zor/journal.json");
        ensure!(!journal.exists(), "read-only recovery created storage");
        service_owner = Some(start_service()?);
        let endpoint = root.path().join("zor/control.sock");
        until(Duration::from_secs(8), || {
            ensure!(
                service_owner
                    .as_mut()
                    .context("service")?
                    .0
                    .try_wait()?
                    .is_none(),
                "service startup exit"
            );
            Ok(endpoint.exists().then_some(()))
        })?;
        let until = Instant::now() + Duration::from_millis(2100);
        while Instant::now() < until {
            let ping = service::rpc(
                &endpoint,
                &json!({"v":1,"id":1,"op":"ping"}),
                Duration::from_secs(3),
            )?;
            ensure!(
                ping["service_instance"]
                    .as_str()
                    .is_some_and(|s| !s.is_empty()),
                "missing service instance"
            );
            ensure!(!journal.exists(), "observer cadence created journal");
            std::thread::sleep(Duration::from_millis(100));
        }
        stop(&mut service_owner)?;
        ensure!(!journal.exists(), "observer shutdown created journal");
        for name in ["aaa-stale", "bbb-stop", "cancel-only", "untouched"] {
            cli(
                &[
                    "task",
                    "start",
                    name,
                    "--title",
                    name,
                    "--instance",
                    instance,
                    "--workspace",
                    "default",
                    "--cwd",
                    root.path().to_str().context("root")?,
                    "--",
                    "/bin/cat",
                ],
                true,
            )?;
        }
        let untouched = inspect("untouched")?;
        cli(
            &[
                "task",
                "adopt",
                "observed",
                "--title",
                "observation",
                "--instance",
                instance,
                "--workspace",
                "default",
                "--pane",
                &untouched["session"]["target"]["pane"].to_string(),
            ],
            true,
        )?;
        for name in ["observed", "aaa-stale", "bbb-stop", "cancel-only"] {
            cli(&["task", "cancel", name], true)?;
        }
        // Simulate a crash after durable intent, before issuing any kill.
        let mut data: Value = serde_json::from_slice(&fs::read(&journal)?)?;
        for name in ["aaa-stale", "bbb-stop"] {
            data["launches"][name]["stop_requested"] = true.into();
        }
        let stale_session = data["launches"]["aaa-stale"]["session"]
            .as_str()
            .context("stale session")?
            .to_owned();
        data["launches"]["aaa-stale"]["instance"] = "0".repeat(32).into();
        data["sessions"][&stale_session]["target"]["instance"] = "0".repeat(32).into();
        let candidate = journal.with_extension("fixture");
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&candidate)?;
        file.write_all(&serde_json::to_vec(&data)?)?;
        file.sync_all()?;
        drop(file);
        fs::rename(candidate, &journal)?;
        let first = cli(&["task", "recover"], true)?;
        ensure!(
            first["selected"] == "aaa-stale"
                && first["outcome"] == "unresolved"
                && first["pending"] == 2,
            "initial recovery: {first}"
        );
        ensure!(
            inspect("bbb-stop")?["launch"]["phase"] == "attached",
            "stop prematurely recovered"
        );
        service_owner = Some(start_service()?);
        await_state(
            service_owner.as_mut().context("service")?,
            "bbb-stop",
            "/launch/phase",
            "closed",
        )?;
        stop(&mut service_owner)?;
        let recovered = inspect("bbb-stop")?;
        ensure!(
            recovered["task"]["outcome"] == "cancelled"
                && recovered["launch"]["stop_requested"] == true,
            "stop intent lost"
        );
        ensure!(
            inspect("aaa-stale")?["launch"]["phase"] == "attached",
            "stale target closed"
        );
        for name in ["cancel-only", "untouched"] {
            let live = cli(&["task", "launch-reconcile", name], true)?;
            ensure!(
                live["launch"]["phase"] == "attached"
                    && live["launch"]["problem"].is_null()
                    && live["launch"]["stop_requested"] == false,
                "unrelated launch changed"
            );
        }
        ensure!(
            inspect("observed")?["session"]["ownership"] == "adopted",
            "adopted ownership changed"
        );
        let rotated = cli(&["task", "recover", "--after", "zzz"], true)?;
        ensure!(
            rotated["selected"] == "aaa-stale"
                && rotated["outcome"] == "unresolved"
                && rotated["pending"] == 1,
            "recovery rotation: {rotated}"
        );
        parked(&control, || {
            cli(&["task", "launch-reconcile", "untouched"], false)?;
            ensure!(
                inspect("untouched")?["attempt"]["state"] == "uncertain",
                "outage is not uncertainty"
            );
            Ok(())
        })?;
        let restored = cli(&["task", "launch-reconcile", "untouched"], true)?;
        ensure!(
            restored["attempt"]["state"] == "active" && restored["session"] == untouched["session"],
            "restoration changed session"
        );
        cli(
            &[
                "task",
                "prepare",
                "untouched",
                "--operation",
                "before-restart",
                "--text",
                "never replay",
            ],
            true,
        )?;
        let reserved = cli(&["task", "reserve", "before-restart"], true)?;
        ensure!(reserved["delivery"] == "reserved", "reserve state");
        let retained = inspect("untouched")?;
        server.finish()?;
        server = root.server(fux)?;
        let replacement = listing()?;
        ensure!(
            replacement["instance"] != instance,
            "server instance reused"
        );
        let before_panes = panes(&replacement)?;
        ensure!(before_panes.len() == 1, "replacement has unexpected panes");
        service_owner = Some(start_service()?);
        await_state(
            service_owner.as_mut().context("service")?,
            "untouched",
            "/attempt/state",
            "lost",
        )?;
        stop(&mut service_owner)?;
        let error = cli(&["task", "launch-reconcile", "untouched"], false)?;
        ensure!(
            error.as_str().context("error")?.contains("ownership lost"),
            "wrong ownership error"
        );
        let lost = inspect("untouched")?;
        ensure!(
            lost["attempt"]["state"] == "lost"
                && lost["session"] == retained["session"]
                && lost["prompts"] == retained["prompts"]
                && lost["task"] == retained["task"],
            "history lost"
        );
        ensure!(
            lost["launch"]["phase"] == "attached"
                && lost["launch"]["final_evidence"].is_null()
                && lost["launch"]["problem"]
                    .as_str()
                    .context("launch problem")?
                    .contains("no command or prompt replayed"),
            "unproven final evidence"
        );
        parked(&control, || {
            cli(&["task", "launch-reconcile", "untouched"], false)?;
            ensure!(
                inspect("untouched")?["attempt"]["state"] == "lost",
                "outage erased replacement evidence"
            );
            Ok(())
        })?;
        cli(&["task", "submit", "before-restart"], false)?;
        let prompt = inspect("untouched")?["prompts"][0].clone();
        ensure!(
            prompt["delivery"] == "uncertain" && prompt["receipt"] == reserved["receipt"],
            "old receipt changed"
        );
        let identities = |panes: &[Value]| {
            panes
                .iter()
                .map(|p| (p["id"].clone(), p["pid"].clone()))
                .collect::<Vec<_>>()
        };
        ensure!(
            identities(&panes(&listing()?)?) == identities(&before_panes),
            "replacement panes changed"
        );
        let capture = completed(
            &control,
            json!({"id":1,"command":"capture","instance":replacement["instance"],"pane":before_panes[0]["id"],"max_bytes":4096}),
        )?;
        ensure!(
            capture["input_sequence"] == 0,
            "input replayed into replacement"
        );
        Ok(())
    })();
    let service_stopped = stop(&mut service_owner);
    let server_stopped = server.finish();
    let errors: Vec<_> = [scenario, service_stopped, server_stopped]
        .into_iter()
        .filter_map(Result::err)
        .map(|e| format!("{e:#}"))
        .collect();
    ensure!(errors.is_empty(), "{}", errors.join("; "));
    println!(
        "PASS stop recovery, ownership isolation, unavailable versus replaced fux, retained history and no replay"
    );
    Ok(())
}
