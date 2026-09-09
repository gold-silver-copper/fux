//! Real shared-service identity, framing, reload and client isolation.
use super::{service_fixture::*, service_tasks, service_worktrees};
use crate::support::{
    local::{Root, connect, until},
    process,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    os::unix::{
        fs::{MetadataExt, PermissionsExt},
        net::UnixListener,
    },
    path::Path,
    time::{Duration, Instant},
};
const SOURCE: &str = "id='test'\n[[rules]]\nid='ready'\nstate='working'\nregion='title'\ncontains=['SVC_READY']\nvisible_working=true\n";
fn blocked_source() -> String {
    SOURCE
        .replace("state='working'", "state='blocked'")
        .replace("visible_working", "visible_blocker")
}
fn hup(c: &mut Caller) -> Result<()> {
    ensure!(c.child.0.try_wait()?.is_none(), "reload child exited");
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(i32::try_from(c.child.0.id())?),
        nix::sys::signal::Signal::SIGHUP,
    )?;
    Ok(())
}
fn recovery_error(e: &anyhow::Error) -> bool {
    if e.downcast_ref::<nix::errno::Errno>().is_some_and(|e| {
        matches!(
            e,
            nix::errno::Errno::ENOENT
                | nix::errno::Errno::ECONNREFUSED
                | nix::errno::Errno::ECONNRESET
        )
    }) {
        return true;
    }
    if e.downcast_ref::<std::io::Error>().is_some_and(|e| {
        matches!(
            e.kind(),
            std::io::ErrorKind::NotFound
                | std::io::ErrorKind::ConnectionRefused
                | std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::UnexpectedEof
        )
    }) {
        return true;
    }
    e.to_string() == "service reply EOF"
}
pub fn run(fux: &Path, zor: &Path) -> Result<()> {
    let root = Root::new(
        "zsvc-",
        &[
            "/bin/sh".into(),
            "-c".into(),
            "printf '\\033]2;SVC_READY\\007READY'; sleep 120".into(),
        ],
    )?;
    let rules_dir = root.path().join("rules");
    fs::create_dir(&rules_dir)?;
    let rules = rules_dir.join("test.toml");
    fs::write(&rules, SOURCE)?;
    let mut fux = root.server(fux)?;
    let mut h = Harness {
        root: &root,
        zor,
        endpoint: root.path().join("zor/control.sock"),
        instance: Value::Null,
        handle: Value::Null,
        journal: root.path().join("tasks/journal.json"),
    };
    // Declared after the multiplexer and before foreground children: cleanup shuts
    // down foreground clients/service, then any authenticated detached service.
    let background = Background(h.endpoint.clone());
    let mut service = Caller::spawn(h.service_command()?)?;
    let result = (|| -> Result<()> {
        until(Duration::from_secs(12), || {
            ensure!(
                service.child.0.try_wait()?.is_none(),
                "initial service exited"
            );
            Ok(h.endpoint.exists().then_some(()))
        })?;
        let first = until(Duration::from_secs(12), || {
            let v = h.snapshot()?;
            Ok((state(&v) == "working").then_some(v))
        })?;
        h.handle = first["snapshot"]["observations"][0]["handle"].clone();
        h.instance = first["service_instance"].clone();
        ensure!(
            first["stale"] == false && first["sequence"].as_u64().context("sequence")? > 0,
            "initial snapshot"
        );
        ensure!(
            fs::metadata(&h.endpoint)?.permissions().mode() & 0o777 == 0o600
                && fs::metadata(h.endpoint.parent().context("endpoint parent")?)?
                    .permissions()
                    .mode()
                    & 0o777
                    == 0o700,
            "service socket permissions"
        );
        let duplicate = process::output(h.service_command()?, Duration::from_secs(5), 1048576)?;
        ensure!(
            !duplicate.status.success()
                && String::from_utf8_lossy(&duplicate.stderr).contains("already running")
                && h.snapshot()?["service_instance"] == first["service_instance"],
            "duplicate service replaced owner"
        );
        let managed = service_worktrees::run(&h)?;
        let cancelled = service_tasks::run(&h, &managed)?;
        for (request, error) in [
            (b"{oops}\n".to_vec(), "invalid-request"),
            (
                b"{\"v\":99,\"id\":2,\"op\":\"snapshot\"}\n".to_vec(),
                "incompatible-version",
            ),
            (
                b"{\"v\":1,\"id\":3,\"op\":\"prompt\"}\n".to_vec(),
                "invalid-request",
            ),
        ] {
            ensure!(
                raw(&h.endpoint, &request)?["error"] == error,
                "framing rejection"
            );
        }
        let mut stalled = Vec::new();
        for _ in 0..16 {
            let mut p = connect(&h.endpoint, Instant::now() + Duration::from_secs(3))?;
            p.set_write_timeout(Some(Duration::from_secs(3)))?;
            p.write_all(b"{")?;
            stalled.push(p);
        }
        let began = Instant::now();
        ensure!(
            h.snapshot()?["status"] == "completed" && began.elapsed() < Duration::from_secs(2),
            "partial clients monopolized service"
        );
        std::thread::sleep(Duration::from_millis(3500));
        for mut p in stalled {
            p.set_nonblocking(true)?;
            let mut byte = [0];
            let n = p.read(&mut byte)?;
            ensure!(n == 0, "stalled client not evicted");
        }
        fs::write(&rules, "malformed[")?;
        hup(&mut service)?;
        let bad = until(Duration::from_secs(12), || {
            let v = h.snapshot()?;
            Ok(v["snapshot"]["problems"]
                .get("rules")
                .is_some()
                .then_some(v))
        })?;
        ensure!(
            state(&bad) == "working" && bad["snapshot"]["rules_generation"] == 1,
            "failed reload changed generation"
        );
        ensure!(
            h.cli(
                &["--rules", path(&rules_dir)?, "status"],
                true,
                Duration::from_secs(5)
            )?["service_instance"]
                == first["service_instance"],
            "status parsed caller rules"
        );
        fs::write(&rules, blocked_source())?;
        hup(&mut service)?;
        until(Duration::from_secs(12), || {
            Ok((state(&h.snapshot()?) == "blocked").then_some(()))
        })?;
        service.stop(true)?;
        ensure!(h.endpoint.exists(), "SIGKILL unexpectedly cleaned socket");
        service = Caller::spawn(h.service_command()?)?;
        let restarted = until(Duration::from_secs(12), || match h.snapshot() {
            Ok(v) => Ok((v["service_instance"] != first["service_instance"]
                && state(&v) == "blocked")
                .then_some(v)),
            Err(e) if recovery_error(&e) => Ok(None),
            Err(e) => Err(e),
        })?;
        ensure!(
            restarted["snapshot"]["observations"][0]["handle"] == h.handle,
            "service restart changed pane"
        );
        let old = h.instance.clone();
        h.instance = restarted["service_instance"].clone();
        ensure!(
            h.api(
                json!({"action":"launch-reconcile","id":"api-managed"}),
                true
            )?["session"]
                == managed.managed["session"],
            "launch reconciliation after service restart"
        );
        h.api_instance(json!({"action":"inspect","id":"api-task"}), false, &old)?;
        ensure!(
            h.api(json!({"action":"inspect","id":"api-task"}), true)? == cancelled,
            "task changed across restart"
        );
        let stopped = until(Duration::from_secs(12), || {
            let r=h.exchange(&json!({"v":1,"id":45,"op":"task","service_instance":h.instance,"task":{"action":"stop","id":"api-managed"}}))?;
            if r["status"] == "completed" {
                Ok(Some(r["value"].clone()))
            } else {
                ensure!(r["error"] == "task-failed", "managed stop: {r}");
                Ok(None)
            }
        })?;
        ensure!(
            stopped["launch"]["phase"] == "closed"
                && stopped["launch"]["stop_requested"] == true
                && stopped["task"]["outcome"] == "cancelled"
                && stopped["session"] == managed.managed["session"],
            "managed stop evidence"
        );
        let refused = h.api(
            json!({"action":"worktree-remove","id":"api-tree","force":true}),
            false,
        )?;
        ensure!(
            contains(&refused, "use is uncertain")
                && managed.tree.exists()
                && h.api(managed.launch.clone(), true)?["launch"]["worktree"] == "api-tree",
            "closed forwarding runtime uncertainty"
        );
        let after = h.api(json!({"action":"inspect","id":"api-task"}), true)?;
        ensure!(
            without(&after, "generation")? == without(&cancelled, "generation")?,
            "sibling stop changed task"
        );
        let cancelled = after;
        ensure!(
            h.fux("list", json!({}))?["workspaces"][0]["tabs"][0]["panes"][0]["pid"]
                == h.handle["pid"],
            "pane restarted"
        );
        service.stop(false)?;
        let (child, mut parent) = daemon(&h)?;
        service = child;
        ensure!(
            line(&mut parent, 4096, Duration::from_secs(5))?["status"] == "ready",
            "daemon readiness"
        );
        drop(parent);
        ensure!(
            !service.finish(Duration::from_secs(8))?.status.success() && !h.endpoint.exists(),
            "unactivated daemon survived caller loss"
        );
        let (child, mut parent) = daemon(&h)?;
        service = child;
        let committed_instance =
            line(&mut parent, 4096, Duration::from_secs(5))?["service_instance"].clone();
        activate(parent)?;
        let committed = until(Duration::from_secs(12), || {
            let v = h.snapshot()?;
            Ok((state(&v) == "blocked").then_some(v))
        })?;
        ensure!(
            committed["service_instance"] == committed_instance
                && service.child.0.try_wait()?.is_none()
                && committed["snapshot"]["observations"][0]["handle"] == h.handle,
            "activation lost acknowledgement stopped daemon"
        );
        service.stop(false)?;
        fs::write(&rules, "malformed[")?;
        h.cli(
            &["--rules", path(&rules_dir)?, "status", "--start"],
            false,
            Duration::from_secs(6),
        )?;
        ensure!(!h.endpoint.exists(), "invalid startup left daemon");
        fs::write(&rules, blocked_source())?;
        drop(UnixListener::bind(&h.endpoint)?);
        let mut starters = Vec::new();
        for _ in 0..4 {
            starters.push(Caller::spawn(h.command(&[
                "--state-directory",
                path(&root.path().join("tasks"))?,
                "--rules",
                path(&rules_dir)?,
                "--agent",
                "test",
                "status",
                "--start",
            ]))?);
        }
        let mut results = Vec::new();
        for c in &mut starters {
            results.push(c.value(Duration::from_secs(12))?);
        }
        let daemon_instance = results[0]["service_instance"].clone();
        ensure!(
            results
                .iter()
                .all(|v| v["service_instance"] == daemon_instance)
                && daemon_instance != restarted["service_instance"],
            "concurrent starters diverged"
        );
        h.instance = daemon_instance.clone();
        ensure!(
            h.api(json!({"action":"inspect","id":"api-task"}), true)? == cancelled,
            "background service lost selected state directory"
        );
        let fresh = until(Duration::from_secs(12), || {
            let v = h.snapshot()?;
            Ok((state(&v) == "blocked").then_some(v))
        })?;
        ensure!(
            fresh["snapshot"]["observations"][0]["handle"] == h.handle,
            "detached observation changed pane"
        );
        ensure!(h.exchange(&json!({"v":1,"id":9,"op":"shutdown","service_instance":restarted["service_instance"]}))?["error"]=="service-instance-conflict" && h.snapshot()?["service_instance"]==daemon_instance,"stale shutdown stopped daemon");
        fs::write(&rules, "malformed[")?;
        ensure!(
            h.cli(
                &["--rules", path(&rules_dir)?, "status", "--start"],
                true,
                Duration::from_secs(6)
            )?["service_instance"]
                == daemon_instance,
            "existing startup parsed invalid caller rules"
        );
        fs::write(&rules, SOURCE)?;
        fux.finish()?;
        let lost = until(Duration::from_secs(12), || {
            let v = h.snapshot()?;
            Ok(items(&v["snapshot"]["observations"])?
                .is_empty()
                .then_some(v))
        })?;
        ensure!(
            lost["snapshot"]["problems"].get("manager").is_some(),
            "lost fux problem missing"
        );
        ensure!(
            h.cli(&["shutdown"], true, Duration::from_secs(6))?["stopping"] == true,
            "background shutdown"
        );
        until(Duration::from_secs(12), || {
            Ok((!h.endpoint.exists()).then_some(()))
        })?;
        let incompatible = UnixListener::bind(&h.endpoint)?;
        incompatible.set_nonblocking(true)?;
        let inode = fs::metadata(&h.endpoint)?.ino();
        let mut client = Caller::spawn(h.command(&["status", "--start"]))?;
        let (mut peer, _) = until(Duration::from_secs(5), || match incompatible.accept() {
            Ok(p) => Ok(Some(p)),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
            Err(e) => Err(e.into()),
        })?;
        peer.set_nonblocking(false)?;
        peer.set_read_timeout(Some(Duration::from_secs(3)))?;
        peer.set_write_timeout(Some(Duration::from_secs(3)))?;
        let mut request = [0; 4096];
        ensure!(peer.read(&mut request)? > 0, "incompatible probe EOF");
        peer.write_all(b"{\"v\":99,\"id\":1,\"status\":\"completed\"}\n")?;
        drop(peer);
        let r = client.finish(Duration::from_secs(6))?;
        ensure!(
            !r.status.success()
                && String::from_utf8_lossy(&r.stderr).contains("refusing to start a replacement")
                && fs::metadata(&h.endpoint)?.ino() == inode,
            "incompatible endpoint replaced"
        );
        drop(incompatible);
        fs::remove_file(&h.endpoint)?;
        Ok(())
    })();
    let stopped = service.stop(false);
    let detached = background.close();
    let fux_stopped = fux.finish();
    result?;
    stopped?;
    detached?;
    fux_stopped?;
    println!("PASS shared service identity, task evidence, client isolation, reload and restart");
    Ok(())
}
