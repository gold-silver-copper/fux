//! Independent real zor observation over fux's public control protocol.
use crate::support::{
    local::{Root, completed, connect, rpc, until},
    logged::Logged,
    process::{self, OwnedProcess, stop_owned},
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs,
    io::{Read, Write},
    path::Path,
    time::{Duration, Instant},
};

fn wait_report(child: &mut Logged, reports: &mut Vec<Vec<u8>>, marker: &str) -> Result<()> {
    until(Duration::from_secs(10), || {
        reports.extend(child.lines()?);
        ensure!(
            reports.iter().all(|line| line.len() <= 1024),
            "oversized observation report"
        );
        Ok(reports
            .iter()
            .any(|line| String::from_utf8_lossy(line).contains(marker))
            .then_some(()))
    })
}
fn snapshot(child: &mut Logged, check: impl Fn(&Value) -> bool) -> Result<Value> {
    until(Duration::from_secs(10), || {
        for line in child.lines()? {
            let value = serde_json::from_slice(&line)?;
            if check(&value) {
                return Ok(Some(value));
            }
        }
        Ok(None)
    })
}
fn default_state(value: &Value) -> Option<&str> {
    value["observations"]
        .as_array()?
        .iter()
        .find(|e| e["handle"]["workspace"] == "default")?["state"]
        .as_str()
}
fn hup(child: &mut Logged) -> Result<()> {
    ensure!(
        child.child.0.try_wait()?.is_none(),
        "watcher exited before reload"
    );
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(i32::try_from(child.child.0.id())?),
        nix::sys::signal::Signal::SIGHUP,
    )?;
    Ok(())
}
pub(super) fn run(binary: &Path, zor: &Path) -> Result<()> {
    let command = "stty raw -echo; printf 'READY\\033]9;4;1;50\\007'; dd bs=1 count=1 >/dev/null 2>&1; printf '\\033[2J\\033[HIDLE\\033]9;4;0;0\\007\\033]2;OBS_IDLE\\007'; dd bs=1 count=1 >/dev/null 2>&1; printf '\\033[2J\\033[HUNKNOWN\\033]2;\\007'; sleep 60";
    let root = Root::new("fo-rs-", &["/bin/sh".into(), "-c".into(), command.into()])?;
    let rules = root.path().join("rules");
    fs::create_dir(&rules)?;
    let source = "id='test'\nprompt_marker='>'\nblock_markers=[]\n[[rules]]\nid='working'\nstate='working'\nregion='progress'\ncontains=['1:50']\nvisible_working=true\n[[rules]]\nid='idle'\nstate='idle'\nregion='title'\ncontains=['OBS_IDLE']\nvisible_idle=true\n";
    fs::write(rules.join("test.toml"), source)?;
    let mut server = root.server(binary)?;
    let control = root.control();
    let mut observer: Option<Logged> = None;
    let mut watcher: Option<Logged> = None;
    let scenario = (|| -> Result<()> {
        let pane = || -> Result<Value> {
            Ok(
                completed(&control, json!({"command":"list","id":1}))?["workspaces"][0]["tabs"][0]
                    ["panes"][0]
                    .clone(),
            )
        };
        let capture = || -> Result<String> {
            Ok(completed(&control, json!({"command":"capture","id":1,"pane":1,"attrs":false,"scrollback":0,"max_bytes":4096}))?["text"].as_str().context("capture text")?.into())
        };
        let first = pane()?;
        let pid = &first["pid"];
        let instance = completed(&control, json!({"command":"list","id":2}))?["instance"].clone();
        ensure!(
            !instance.as_str().context("instance")?.is_empty(),
            "empty instance"
        );
        for mut op in [
            json!({"command":"capture","pane":1,"max_bytes":4096}),
            json!({"command":"send-keys","pane":1,"keys":"must-not-arrive"}),
            json!({"command":"subscribe","events":[]}),
        ] {
            op["id"] = 9.into();
            op["instance"] = "old-server".into();
            let stale = rpc(&control, op)?;
            ensure!(
                stale["status"] == "failed" && stale["error"]["code"] == "conflict",
                "stale observer request accepted: {stale}"
            );
        }
        until(Duration::from_secs(10), || {
            Ok(capture()?.contains("READY").then_some(()))
        })?;
        until(Duration::from_secs(10), || {
            let capture = completed(
                &control,
                json!({"command":"capture","id":1,"pane":1,"max_bytes":4096}),
            )?;
            Ok((capture["progress"] == json!([1, 50])).then_some(()))
        })?;
        let request = json!({"command":"capture","id":3,"instance":instance,"pane":1,"attrs":true,"scrollback":0,"max_bytes":131072});
        let original = completed(&control, request.clone())?;
        ensure!(
            original["rows"] == first["geometry"]["height"]
                && original["columns"] == first["geometry"]["width"],
            "capture geometry differs"
        );
        ensure!(
            original["progress"] == json!([1, 50]) && original["truncated"] == false,
            "capture metadata differs"
        );
        let mut conditional = request.clone();
        conditional["if_revision"] = original["revision"].clone();
        let cached = completed(&control, conditional.clone())?;
        ensure!(
            cached["unchanged"] == true && cached["text"] == "",
            "conditional capture missed cache"
        );
        for preface_bytes in [b"FUZ\n".as_slice(), b"FUXCTL1\n".as_slice()] {
            let mut bad = connect(&control, Instant::now() + Duration::from_secs(3))?;
            bad.set_read_timeout(Some(Duration::from_secs(3)))?;
            bad.set_write_timeout(Some(Duration::from_secs(3)))?;
            bad.write_all(preface_bytes)?;
            let mut preface = [0; 4];
            let count = match bad.read(&mut preface) {
                Ok(n) => n,
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::BrokenPipe
                    ) =>
                {
                    0
                }
                Err(e) => return Err(e.into()),
            };
            ensure!(
                count == 0 || &preface[..count] == b"FUX\n",
                "bad client unexpected preface"
            );
            drop(bad);
        }
        let reply = rpc(&control, json!({"command":"popup","id":5,"argv":["x"]}))?;
        ensure!(
            reply["status"] == "failed" && reply["error"]["code"] == "unknown-command",
            "unknown command accepted"
        );
        ensure!(pane()?["pid"] == *pid, "bad client changed pane");
        let base_command = || {
            let mut c = root.command(zor);
            c.arg("--rules").arg(&rules).args(["--agent", "test"]);
            c
        };
        let watch_once = || -> Result<Value> {
            let mut c = base_command();
            c.args(["watch", "--once", "--runtime"])
                .arg(control.parent().context("control parent")?);
            let out = process::output(c, Duration::from_secs(10), 1024 * 1024)?;
            ensure!(
                out.status.success(),
                "watch once failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            Ok(serde_json::from_slice(&out.stdout)?)
        };
        let discovered = watch_once()?;
        ensure!(
            discovered["problems"]
                .as_object()
                .is_some_and(|p| p.is_empty())
                || discovered["problems"]
                    .as_array()
                    .is_some_and(|p| p.is_empty()),
            "discovery problems: {discovered}"
        );
        ensure!(
            discovered["observations"][0]["state"] == "working"
                && discovered["observations"][0]["handle"]["pid"] == *pid
                && discovered["observations"][0]["handle"]["instance"] == instance,
            "discovery mismatch: {discovered}"
        );
        let mut c = root.command(binary);
        c.args(["workspace", "new", "other"]);
        ensure!(
            process::output(c, Duration::from_secs(10), 1024 * 1024)?
                .status
                .success(),
            "second workspace failed"
        );
        let multiple = watch_once()?;
        let observations = multiple["observations"]
            .as_array()
            .context("observations")?;
        let names: BTreeSet<_> = observations
            .iter()
            .filter_map(|e| e["handle"]["workspace"].as_str())
            .collect();
        ensure!(
            names == BTreeSet::from(["default", "other"]),
            "workspace discovery: {multiple}"
        );
        ensure!(
            observations
                .iter()
                .all(|e| e["handle"]["instance"] == instance),
            "mixed instances"
        );
        let duration = multiple["scan_duration_ms"]
            .as_u64()
            .context("scan duration")?;
        ensure!(
            observations.iter().all(|e| e["age_upper_bound_ms"]
                .as_u64()
                .is_some_and(|age| age <= duration)),
            "observation age bound"
        );
        let observe_command = || {
            let mut c = base_command();
            c.args(["observe", "--socket"]).arg(&control).args([
                "--pane",
                "1",
                "--pid",
                &pid.to_string(),
            ]);
            c
        };
        observer = Some(Logged::spawn(observe_command())?);
        let mut reports = Vec::new();
        wait_report(
            observer.as_mut().context("observer")?,
            &mut reports,
            "state=working",
        )?;
        let before = pane()?;
        completed(
            &control,
            json!({"command":"send-keys","id":2,"pane":1,"keys":"x"}),
        )?;
        until(Duration::from_secs(10), || {
            Ok(capture()?.contains("IDLE").then_some(()))
        })?;
        let changed = completed(&control, conditional)?;
        ensure!(
            changed["unchanged"] == false
                && changed["title"] == "OBS_IDLE"
                && changed["progress"].is_null(),
            "changed metadata: {changed}"
        );
        wait_report(
            observer.as_mut().context("observer")?,
            &mut reports,
            "state=idle",
        )?;
        let after = pane()?;
        ensure!(
            after["pid"] == *pid
                && after["focused"] == before["focused"]
                && after["title"] == "OBS_IDLE",
            "observer changed pane"
        );
        ensure!(
            watch_once()?["observations"][0]["state"] == "idle",
            "watch missed idle"
        );
        observer.as_mut().context("observer")?.kill()?;
        ensure!(
            pane()?["pid"] == *pid && capture()?.contains("IDLE"),
            "observer loss changed pane"
        );
        let mut c = base_command();
        c.args(["watch", "--runtime"])
            .arg(control.parent().context("control parent")?);
        watcher = Some(Logged::spawn(c)?);
        let watching = watcher.as_mut().context("watcher")?;
        let baseline = snapshot(watching, |v| v["rules_generation"] == 1)?;
        ensure!(
            default_state(&baseline) == Some("idle"),
            "initial watcher state"
        );
        fs::write(rules.join("test.toml"), "malformed[")?;
        hup(watching)?;
        let rejected = snapshot(watching, |v| {
            v["problems"].get("rules").is_some()
                || v["problems"]
                    .as_array()
                    .is_some_and(|p| p.contains(&json!("rules")))
        })?;
        ensure!(
            rejected["rules_generation"] == 1 && default_state(&rejected) == Some("idle"),
            "bad reload discarded old rules"
        );
        fs::write(
            rules.join("test.toml"),
            source
                .replace("id='idle'\nstate='idle'", "id='idle'\nstate='blocked'")
                .replace("visible_idle=true", "visible_blocker=true"),
        )?;
        hup(watching)?;
        let reloaded = snapshot(watching, |v| v["rules_generation"] == 2)?;
        ensure!(
            default_state(&reloaded) == Some("blocked")
                && reloaded["problems"].get("rules").is_none()
                && !reloaded["problems"]
                    .as_array()
                    .is_some_and(|p| p.contains(&json!("rules"))),
            "reload failed: {reloaded}"
        );
        ensure!(pane()?["pid"] == *pid, "reload restarted pane");
        observer = Some(Logged::spawn(observe_command())?);
        reports.clear();
        wait_report(
            observer.as_mut().context("observer")?,
            &mut reports,
            "state=blocked",
        )?;
        completed(
            &control,
            json!({"command":"send-keys","id":8,"pane":1,"keys":"x"}),
        )?;
        until(Duration::from_secs(10), || {
            Ok(capture()?.contains("UNKNOWN").then_some(()))
        })?;
        wait_report(
            observer.as_mut().context("observer")?,
            &mut reports,
            "state=none",
        )?;
        snapshot(watching, |v| default_state(v) == Some("unknown"))?;
        ensure!(pane()?["pid"] == *pid, "unknown restarted pane");
        Ok(())
    })();
    let mut errors = stop_owned(&mut [
        watcher
            .as_mut()
            .map(|c| &mut c.child.0 as &mut dyn OwnedProcess),
        observer
            .as_mut()
            .map(|c| &mut c.child.0 as &mut dyn OwnedProcess),
        Some(&mut server.child),
    ]);
    if let Err(error) = scenario {
        errors.push(format!("{error:#}"));
    }
    match server.diagnostic() {
        Ok(stderr) if stderr.contains("panicked") => errors.push(stderr),
        Err(error) => errors.push(format!("{error:#}")),
        _ => {}
    }
    ensure!(errors.is_empty(), "{}", errors.join("; "));
    println!(
        "PASS zor observes fux through the control protocol; observer loss and bad clients leave panes untouched"
    );
    Ok(())
}
