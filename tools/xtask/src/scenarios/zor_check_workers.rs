//! Check worker admission and shutdown cannot occupy the coordination worker.
use crate::support::{
    local::{Root, completed, until},
    logged::Logged,
    process::{OwnedProcess, wait},
    service::{Peer, rpc},
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs,
    path::Path,
    time::{Duration, Instant},
};

pub(super) fn run(fux: &Path, zor: &Path) -> Result<()> {
    let root = Root::new("zpool-rs-", &["/bin/cat".into()])?;
    let mut fux = root.server(fux)?;
    let instance =
        completed(&root.control(), json!({"id":1,"command":"list"}))?["instance"].clone();
    let mut command = root.command(zor);
    command.arg("serve");
    let mut service = Logged::spawn(command)?;
    let endpoint = root.path().join("zor/control.sock");
    let release = root.path().join("release");
    let scenario = (|| -> Result<()> {
        until(Duration::from_secs(5), || {
            ensure!(service.child.0.try_wait()?.is_none(), "service exited");
            Ok(endpoint.exists().then_some(()))
        })?;
        let service_instance = rpc(
            &endpoint,
            &json!({"v":1,"id":1,"op":"ping"}),
            Duration::from_secs(5),
        )?["service_instance"]
            .clone();
        let enqueue = |task: Value, caller: u64| {
            Peer::send(
                &endpoint,
                &json!({"v":1,"id":caller,"op":"task","service_instance":service_instance,"task":task}),
                Duration::from_secs(5),
            )
        };
        let api = |task: Value| -> Result<Value> {
            let reply = enqueue(task, 2)?.receive()?;
            ensure!(
                reply["status"] == "completed",
                "service task failed: {reply}"
            );
            Ok(reply["value"].clone())
        };
        let no_forbidden = || -> Result<()> {
            for entry in fs::read_dir(root.path())? {
                ensure!(
                    !entry?
                        .file_name()
                        .to_string_lossy()
                        .starts_with("forbidden-"),
                    "queued check executed"
                );
            }
            Ok(())
        };
        api(
            json!({"action":"start","id":"worker","title":"pool","instance":instance,"workspace":"default","cwd":root.path(),"argv":["/bin/cat"]}),
        )?;
        let executable = std::env::current_exe()?;
        let held = |id: &str, check: &str, marker: &str| json!({"action":"check","id":id,"check":check,"timeout_ms":20000,"argv":[executable,"fixture-worker","hold",root.path(),marker]});
        let mut active = Vec::new();
        for index in 0..2 {
            active.push(enqueue(
                held(
                    "worker",
                    &format!("held-{index}"),
                    &format!("marker-{index}"),
                ),
                10 + index,
            )?);
            until(Duration::from_secs(5), || {
                Ok(root
                    .path()
                    .join(format!("marker-{index}"))
                    .exists()
                    .then_some(()))
            })?;
        }
        let mut queued = Vec::new();
        for index in 0..8 {
            queued.push(enqueue(json!({"action":"check","id":"worker","check":format!("queued-{index}"),"timeout_ms":1000,"argv":["/bin/sh","-c",format!("touch forbidden-{index}")]}),100+index)?);
        }
        let mut overloaded = 0;
        until(Duration::from_secs(5), || {
            for index in Peer::ready(&queued, 100)?.into_iter().rev() {
                let reply = queued.remove(index).receive()?;
                ensure!(
                    reply["error"] == "task-overloaded",
                    "wrong overload: {reply}"
                );
                overloaded += 1;
            }
            Ok((overloaded == 4).then_some(()))
        })?;
        let started = Instant::now();
        for index in 0..2 {
            ensure!(
                api(json!({"action":"check-inspect","check":format!("held-{index}")}))?["check"]["phase"]
                    == "submitted",
                "active check phase"
            );
        }
        ensure!(
            api(json!({"action":"inspect","id":"worker"}))?["task"]["outcome"] == "open",
            "task not open"
        );
        ensure!(
            api(json!({"action":"cancel","id":"worker"}))?["task"]["outcome"] == "cancelled",
            "cancel failed"
        );
        ensure!(
            started.elapsed() < Duration::from_secs(2),
            "checks blocked coordination"
        );
        no_forbidden()?;
        fs::write(&release, b"")?;
        for peer in active {
            let reply = peer.receive()?;
            ensure!(
                reply["status"] == "completed" && reply["value"]["passed"] == true,
                "active check failed: {reply}"
            );
        }
        for peer in queued {
            let reply = peer.receive()?;
            ensure!(
                reply["error"] == "task-busy"
                    || (reply["error"] == "task-failed"
                        && reply["message"]
                            .as_str()
                            .is_some_and(|m| m.contains("no longer open"))),
                "queued rejection: {reply}"
            );
        }
        no_forbidden()?;
        let inspected = api(json!({"action":"inspect","id":"worker"}))?;
        ensure!(
            inspected["task"]["outcome"] == "cancelled",
            "cancelled task reopened"
        );
        let checks: BTreeSet<_> = inspected["checks"]
            .as_array()
            .context("checks")?
            .iter()
            .filter_map(|c| c["id"].as_str())
            .collect();
        ensure!(
            checks == BTreeSet::from(["held-0", "held-1"]),
            "queued check admitted after cancellation"
        );
        api(
            json!({"action":"start","id":"shutdown-worker","title":"shutdown pool","instance":instance,"workspace":"default","cwd":root.path(),"argv":["/bin/cat"]}),
        )?;
        fs::remove_file(&release)?;
        let mut shutdown_checks = Vec::new();
        for index in 0..2 {
            shutdown_checks.push(enqueue(
                held(
                    "shutdown-worker",
                    &format!("shutdown-held-{index}"),
                    &format!("shutdown-marker-{index}"),
                ),
                200 + index,
            )?);
            until(Duration::from_secs(5), || {
                Ok(root
                    .path()
                    .join(format!("shutdown-marker-{index}"))
                    .exists()
                    .then_some(()))
            })?;
        }
        let mut pending = Vec::new();
        for index in 0..5 {
            pending.push(enqueue(json!({"action":"check","id":"shutdown-worker","check":format!("shutdown-queued-{index}"),"timeout_ms":1000,"argv":["/bin/sh","-c","touch forbidden-shutdown"]}),210+index)?);
        }
        until(Duration::from_secs(5), || {
            let ready = Peer::ready(&pending, 100)?;
            if ready.is_empty() {
                return Ok(None);
            }
            ensure!(ready.len() == 1, "unexpected shutdown queue admission");
            ensure!(
                pending.remove(ready[0]).receive()?["error"] == "task-overloaded",
                "missing shutdown overload"
            );
            Ok(Some(()))
        })?;
        ensure!(
            api(json!({"action":"inspect","id":"shutdown-worker"}))?["task"]["outcome"] == "open",
            "shutdown task not open"
        );
        ensure!(
            rpc(
                &endpoint,
                &json!({"v":1,"id":220,"op":"shutdown","service_instance":service_instance}),
                Duration::from_secs(5)
            )?["status"]
                == "completed",
            "shutdown rejected"
        );
        ensure!(
            service.child.0.try_wait()?.is_none(),
            "shutdown did not join active checks"
        );
        fs::write(&release, b"")?;
        ensure!(
            wait(&mut service.child.0, Duration::from_secs(8))?.success(),
            "service shutdown failed"
        );
        ensure!(
            fux.child.try_wait()?.is_none() && !root.path().join("forbidden-shutdown").exists(),
            "shutdown leaked queued effect or stopped fux"
        );
        let journal: Value =
            serde_json::from_slice(&fs::read(root.path().join("state/zor/journal.json"))?)?;
        ensure!(
            !journal["checks"]
                .as_object()
                .context("journal checks")?
                .keys()
                .any(|key| key.starts_with("shutdown-queued-")),
            "queued shutdown check persisted"
        );
        for index in 0..2 {
            ensure!(
                journal["checks"][format!("shutdown-held-{index}")]["phase"] == "finished",
                "active shutdown check unfinished"
            );
        }
        drop((shutdown_checks, pending));
        Ok(())
    })();
    let released = fs::write(&release, b"").map_err(anyhow::Error::from);
    let cleanup = (|| -> Result<()> {
        service.child.0.terminate()?;
        wait(&mut service.child.0, Duration::from_secs(8))?;
        Ok(())
    })();
    let stopped = fux.finish();
    let errors: Vec<_> = [scenario, released, cleanup, stopped]
        .into_iter()
        .filter_map(Result::err)
        .map(|error| format!("{error:#}"))
        .collect();
    ensure!(errors.is_empty(), "{}", errors.join("; "));
    println!(
        "PASS two concurrent service checks, bounded overload, responsive inspection/cancellation and no queued effects"
    );
    Ok(())
}
