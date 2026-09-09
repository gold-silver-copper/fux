//! Real fux/zor event observation, including deliberate replay gaps and duplicate frames.
use super::events_proxy::Proxy;
use crate::support::{
    local::{self, Root, until},
    process::{self, Guard, OwnedProcess},
    service,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    process::Stdio,
    time::{Duration, Instant},
};
const RULES: &str = "id='test'\n[[rules]]\nid='ready'\nstate='working'\nregion='title'\ncontains=['EVENT_READY']\nvisible_working=true\n[[rules]]\nid='blocked'\nstate='blocked'\nregion='title'\ncontains=['EVENT_BLOCKED']\nvisible_blocker=true\n";
fn observed(v: &Value, state: &str) -> bool {
    v.pointer("/observations/0/state")
        .is_some_and(|v| v == state)
}
fn stop(child: &mut Guard) -> Result<()> {
    if child.0.try_wait()?.is_none() {
        child.0.terminate()?;
        if let Err(e) = process::wait(&mut child.0, Duration::from_secs(8)) {
            child.0.kill()?;
            process::wait(&mut child.0, Duration::from_secs(3))?;
            return Err(e);
        }
    }
    ensure!(
        child.0.try_wait()?.context("child exit")?.success(),
        "owned process failed"
    );
    Ok(())
}
pub fn run(fux: &Path, zor: &Path) -> Result<()> {
    let root=Root::new("zevt-rs-",&["/bin/sh".into(),"-c".into(),"printf '\\033]2;EVENT_READY\\007'; while read value; do printf '\\033]2;%s\\007' \"$value\"; done".into()])?;
    fs::create_dir(root.path().join("rules"))?;
    let rules = root.path().join("rules/test.toml");
    fs::write(&rules, RULES)?;
    let front = root.control();
    let upstream = root.path().join("fux/upstream.sock");
    let endpoint = root.path().join("zor/control.sock");
    let log = tempfile::tempfile()?;
    let mut c = root.command(fux);
    c.arg("serve")
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log.try_clone()?);
    let mut server = Guard(c.spawn()?);
    let mut proxy = None;
    let mut controller = None;
    let outcome = (|| -> Result<()> {
        until(Duration::from_secs(12), || {
            ensure!(server.0.try_wait()?.is_none(), "fux exited");
            Ok(front.exists().then_some(()))
        })?;
        proxy = Some(Proxy::start(&front, &upstream)?);
        let proxy = proxy.as_ref().unwrap();
        let mut c = root.command(zor);
        c.arg("--rules")
            .arg(root.path().join("rules"))
            .args(["--agent", "test", "serve"])
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log.try_clone()?);
        controller = Some(Guard(c.spawn()?));
        until(Duration::from_secs(12), || {
            ensure!(
                controller.as_mut().unwrap().0.try_wait()?.is_none(),
                "zor exited"
            );
            Ok(endpoint.exists().then_some(()))
        })?;
        let snapshot = || -> Result<Value> {
            let v = service::rpc(
                &endpoint,
                &json!({"v":1,"id":1,"op":"snapshot"}),
                Duration::from_secs(3),
            )?;
            Ok(v.get("snapshot").context("snapshot")?.clone())
        };
        let first = until(Duration::from_secs(12), || {
            let v = snapshot()?;
            Ok(observed(&v, "working").then_some(v))
        })?;
        ensure!(first["event_streams"] == 1, "initial event stream");
        let identity = first
            .pointer("/observations/0/handle")
            .context("handle")?
            .clone();
        let instance = identity.get("instance").context("instance")?;
        let pane = identity.get("pane").context("pane")?;
        let input = |text: &str| -> Result<()> {
            let v = local::rpc(
                &upstream,
                json!({"id":9,"command":"send-keys","instance":instance,"pane":pane,"keys":format!("{text}\\r")}),
            )?;
            ensure!(v["status"] == "completed", "input: {v}");
            Ok(())
        };
        std::thread::sleep(Duration::from_millis(400));
        let lists = proxy.count("list");
        let captures = proxy.count("capture");
        std::thread::sleep(Duration::from_millis(4200));
        let idle_lists = proxy.count("list") - lists;
        let idle_captures = proxy.count("capture") - captures;
        let before = proxy.count("list");
        ensure!(
            idle_lists <= 2 && idle_captures == 0,
            "idle traffic: {idle_lists} lists, {idle_captures} captures"
        );
        until(Duration::from_secs(12), || {
            Ok((proxy.count("list") > before).then_some(()))
        })?;
        let start = Instant::now();
        input("EVENT_BLOCKED")?;
        let blocked = until(Duration::from_secs(2), || {
            let v = snapshot()?;
            Ok(observed(&v, "blocked").then_some(v))
        })?;
        let elapsed = start.elapsed().as_secs_f64();
        ensure!(
            blocked.pointer("/observations/0/input_sequence") == Some(&json!(1)),
            "input sequence"
        );
        proxy.gap(true);
        let unavailable = until(Duration::from_secs(12), || {
            let v = snapshot()?;
            Ok((v["event_streams"] == 0
                && v["event_failures"].as_u64().context("failures")? > 0
                && observed(&v, "unknown"))
            .then_some(v))
        })?;
        ensure!(
            unavailable.pointer("/observations/0/rule") == Some(&Value::Null),
            "gap retained rule evidence"
        );
        proxy.gap(false);
        let recovered = until(Duration::from_secs(12), || {
            let v = snapshot()?;
            Ok((v["event_streams"] == 1 && observed(&v, "blocked")).then_some(v))
        })?;
        let failures = recovered["event_failures"].as_u64().context("failures")?;
        ensure!(
            failures >= unavailable["event_failures"].as_u64().context("failures")?,
            "failure accounting"
        );
        ensure!(
            recovered.pointer("/observations/0/handle") == Some(&identity),
            "handle changed"
        );
        proxy.duplicate();
        input("EVENT_READY")?;
        until(Duration::from_secs(12), || {
            Ok(
                (snapshot()?["event_failures"].as_u64().context("failures")? > failures)
                    .then_some(()),
            )
        })?;
        until(Duration::from_secs(12), || {
            Ok(observed(&snapshot()?, "working").then_some(()))
        })?;
        fs::write(
            &rules,
            RULES
                .replace("state='working'", "state='idle'")
                .replace("visible_working=true", "visible_idle=true"),
        )?;
        let child = &mut controller.as_mut().unwrap().0;
        ensure!(
            child.try_wait()?.is_none(),
            "controller exited before reload"
        );
        nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(i32::try_from(child.id())?),
            nix::sys::signal::Signal::SIGHUP,
        )?;
        until(Duration::from_secs(2), || {
            let v = snapshot()?;
            Ok((v["rules_generation"] == 2 && observed(&v, "idle")).then_some(()))
        })?;
        println!(
            "{}",
            json!({"idle_window_seconds":4.2,"idle_workspace_lists":idle_lists,"idle_captures":idle_captures,"event_refresh_seconds":(elapsed*1000.).round()/1000.,"failure_recovery":true,"same_pane_process":true})
        );
        Ok(())
    })();
    let mut errors = Vec::new();
    if let Some(c) = controller.as_mut()
        && let Err(e) = stop(c)
    {
        errors.push(format!("controller: {e:#}"));
    }
    if let Some(p) = proxy.as_mut()
        && let Err(e) = p.close()
    {
        errors.push(format!("proxy: {e:#}"));
    }
    if let Err(e) = stop(&mut server) {
        errors.push(format!("fux: {e:#}"));
    }
    if outcome.is_err() || !errors.is_empty() {
        use std::io::{Read, Seek};
        let mut reader = log;
        reader.rewind()?;
        let mut bytes = Vec::new();
        reader.take(16384).read_to_end(&mut bytes)?;
        eprintln!("{}", String::from_utf8_lossy(&bytes));
    }
    if let Err(e) = outcome {
        return Err(e.context(format!("cleanup errors: {errors:?}")));
    }
    ensure!(errors.is_empty(), "{}", errors.join("; "));
    Ok(())
}
