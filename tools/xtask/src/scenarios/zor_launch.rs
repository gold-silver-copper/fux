//! Managed launch/stop recovery with real panes and deliberate control/manager faults.
use crate::support::{
    launch_proxy::{Mode, Proxy},
    local::{Root, completed, until},
    process::{self, Guard},
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    process::Stdio,
    time::{Duration, Instant},
};
fn f<'a>(v: &'a Value, p: &str) -> Result<&'a Value> {
    v.pointer(p).with_context(|| format!("missing {p}"))
}
struct Harness<'a> {
    root: &'a Root,
    zor: &'a Path,
    instance: Value,
}
impl Harness<'_> {
    fn raw(&self, args: &[&str], timeout: Duration) -> Result<process::Output> {
        let mut c = self.root.command(self.zor);
        c.arg("task").args(args);
        process::output(c, timeout, 1024 * 1024)
    }
    fn task(&self, args: &[&str], ok: bool) -> Result<Value> {
        let r = self.raw(args, Duration::from_secs(12))?;
        ensure!(
            r.status.success() == ok,
            "task {args:?}: {}",
            String::from_utf8_lossy(&r.stderr)
        );
        if ok {
            Ok(serde_json::from_slice(&r.stdout)?)
        } else {
            Ok(json!(String::from_utf8(r.stderr)?))
        }
    }
    fn start_args(
        &self,
        name: &str,
        runtime: Option<&Path>,
        command: &[&str],
    ) -> Result<Vec<String>> {
        let mut a = [
            "start",
            name,
            "--title",
            "managed fixture",
            "--instance",
            self.instance.as_str().context("instance")?,
            "--workspace",
            "default",
            "--cwd",
            self.root.path().to_str().context("cwd")?,
        ]
        .map(str::to_owned)
        .to_vec();
        if let Some(p) = runtime {
            a.extend(["--runtime".into(), p.to_str().context("runtime")?.into()]);
        }
        a.push("--".into());
        a.extend(command.iter().map(|s| (*s).into()));
        Ok(a)
    }
    fn start(
        &self,
        name: &str,
        runtime: Option<&Path>,
        command: &[&str],
        ok: bool,
    ) -> Result<Value> {
        self.task(
            &self
                .start_args(name, runtime, command)?
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ok,
        )
    }
    fn panes(&self) -> Result<Vec<Value>> {
        let v = completed(&self.root.control(), json!({"id":1,"command":"list"}))?;
        let mut out = Vec::new();
        for w in v["workspaces"].as_array().context("workspaces")? {
            for t in w["tabs"].as_array().context("tabs")? {
                out.extend(t["panes"].as_array().context("panes")?.iter().cloned());
            }
        }
        Ok(out)
    }
    fn capture(&self, pane: &Value) -> Result<Value> {
        completed(
            &self.root.control(),
            json!({"id":1,"command":"capture","instance":self.instance,"pane":pane,"max_bytes":4096}),
        )
    }
    fn closed(&self, name: &str, before: &Value) -> Result<Value> {
        let end = Instant::now() + Duration::from_secs(3);
        loop {
            let r = self.raw(&["launch-reconcile", name], Duration::from_secs(6))?;
            if r.status.success() {
                return Ok(serde_json::from_slice(&r.stdout)?);
            }
            ensure!(
                String::from_utf8_lossy(&r.stderr).contains("lifecycle uncertain")
                    && Instant::now() < end,
                "close reconciliation: {}",
                String::from_utf8_lossy(&r.stderr)
            );
            let pending = self.task(&["inspect", name], true)?;
            ensure!(
                f(&pending, "/task")? == f(before, "/task")?
                    && f(&pending, "/prompts")? == f(before, "/prompts")?,
                "pending close rewrote task/prompts"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    fn stop_complete(&self, name: &str) -> Result<Value> {
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            let r = self.raw(&["stop", name], Duration::from_secs(8))?;
            let s = self.task(&["inspect", name], true)?;
            ensure!(
                s["launch"]["stop_requested"] == true && s["task"]["outcome"] == "cancelled",
                "stop authority"
            );
            if r.status.success() && s["launch"]["phase"] == "closed" {
                return Ok(s);
            }
            ensure!(Instant::now() < end, "stop did not close");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    fn spawn(&self, args: &[String]) -> Result<Guard> {
        let log = tempfile::tempfile()?;
        let mut c = self.root.command(self.zor);
        c.arg("task")
            .args(args)
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log);
        Ok(Guard(c.spawn()?))
    }
}
pub fn run(fux: &Path, zor: &Path) -> Result<()> {
    let root = Root::new("zlaunch-rs-", &["/bin/cat".into()])?;
    let mut server = root.server(fux)?;
    let listing = completed(&root.control(), json!({"id":1,"command":"list"}))?;
    let instance = f(&listing, "/instance")?.clone();
    let host = f(&listing, "/workspaces/0/tabs/0/panes/0")?.clone();
    let h = Harness {
        root: &root,
        zor,
        instance,
    };
    let mut proxy = None;
    let mut callers: Vec<Guard> = Vec::new();
    let result = (|| -> Result<()> {
        macro_rules! t{($($a:expr),*$(,)?)=>{h.task(&[$($a),*],true)?};}
        macro_rules! reject{($($a:expr),*$(,)?)=>{h.task(&[$($a),*],false)?};}
        let argv = [
            "/bin/sh",
            "-c",
            "stty raw -echo; printf \"MARKER=%s\\nARGC=%s FIRST=<%s>\\n\" \"$ZOR_LAUNCH_ID\" \"$#\" \"$1\"; cat",
            "--",
            "",
            "tail",
        ];
        let first = h.start("live", None, &argv, true)?;
        ensure!(
            first["launch"]["phase"] == "attached" && first["session"]["ownership"] == "managed",
            "managed attachment"
        );
        let target = f(&first, "/session/target")?;
        ensure!(
            f(target, "/pane")? != f(&host, "/id")? && h.panes()?.len() == 2,
            "host pane reused"
        );
        ensure!(
            h.start("live", None, &argv, true)? == first,
            "start idempotence"
        );
        let adopted = t!(
            "adopt",
            "observed",
            "--title",
            "observation only",
            "--instance",
            h.instance.as_str().context("instance")?,
            "--workspace",
            "default",
            "--pane",
            &f(target, "/pane")?.to_string()
        );
        ensure!(
            adopted["session"]["ownership"] == "adopted"
                && f(&adopted, "/session/launch")?.is_null()
                && f(&adopted, "/session/id")? != f(&first, "/session/id")?,
            "adoption ownership"
        );
        h.start("live", None, &["/bin/cat"], false)?;
        reject!(
            "adopt",
            "live",
            "--title",
            "managed fixture",
            "--instance",
            h.instance.as_str().context("instance")?,
            "--workspace",
            "default",
            "--pane",
            &f(target, "/pane")?.to_string()
        );
        until(Duration::from_secs(3), || {
            let v = h.capture(f(target, "/pane")?)?;
            let text = v["text"].as_str().context("capture")?;
            Ok((text.contains(&format!(
                "MARKER={}",
                first["launch"]["marker"].as_str().context("marker")?
            )) && text.contains("ARGC=2 FIRST=<>"))
            .then_some(()))
        })?;
        t!(
            "prepare",
            "live",
            "--operation",
            "hello",
            "--text",
            "managed input"
        );
        t!("submit", "hello");
        t!("cancel", "live");
        ensure!(
            h.panes()?
                .iter()
                .any(|p| p.get("id") == target.get("pane") && p.get("pid") == target.get("pid")),
            "cancel killed pane"
        );
        reject!("forget", "live");
        let runtime = root.path().join("proxy");
        fs::create_dir(&runtime)?;
        proxy = Some(Proxy::start(
            &runtime,
            &root.control(),
            &root.path().join("fux/manager.sock"),
            h.instance.clone(),
        )?);
        let p = proxy.as_ref().unwrap();
        h.start("lost", Some(&runtime), &argv, false)?;
        ensure!(
            t!("inspect", "lost")["launch"]["phase"] == "uncertain",
            "lost reply certainty"
        );
        let pending = t!("result", "lost");
        ensure!(
            f(&pending, "/task_outcome")?.is_null()
                && f(&pending, "/attempt")?.is_null()
                && pending["blockers"]
                    .as_array()
                    .context("blockers")?
                    .contains(&json!("launch-not-attached"))
                && pending["verification"]["status"] == "unverified",
            "unattached result overclaim"
        );
        let recovered = t!("launch-reconcile", "lost");
        ensure!(
            recovered["launch"]["phase"] == "attached" && p.faults().creates == 1,
            "lost creation recovery"
        );
        ensure!(
            h.start("lost", Some(&runtime), &argv, true)? == recovered
                && h.panes()?.len() == 3
                && p.faults().creates == 1,
            "duplicate creation"
        );
        p.update(|f| f.mode = Mode::Hold);
        callers.push(h.spawn(&h.start_args("crash", Some(&runtime), &argv)?)?);
        until(Duration::from_secs(5), || Ok(p.accepted().then_some(())))?;
        let caller = &mut callers.last_mut().unwrap().0;
        caller.kill()?;
        process::wait(caller, Duration::from_secs(3))?;
        p.release();
        ensure!(
            t!("inspect", "crash")["launch"]["phase"] == "submitting"
                && t!("launch-reconcile", "crash")["launch"]["phase"] == "attached"
                && p.faults().creates == 2
                && h.panes()?.len() == 4,
            "killed creator recovery"
        );
        p.update(|f| f.mode = Mode::DropBefore);
        h.start("absent", Some(&runtime), &argv, false)?;
        for _ in 0..2 {
            reject!("launch-reconcile", "absent");
            h.start("absent", Some(&runtime), &argv, false)?;
        }
        ensure!(
            p.faults().creates == 3
                && h.panes()?.len() == 4
                && t!("inspect", "absent")["launch"]["phase"] == "uncertain",
            "absent launch blindly replaced"
        );
        let adopted_before = t!("inspect", "observed");
        reject!("stop", "observed");
        ensure!(
            t!("inspect", "observed") == adopted_before && p.faults().kills == 0,
            "adoption acquired stop authority"
        );
        p.update(|f| f.mode = Mode::Normal);
        let stopped = h.start("stopped", Some(&runtime), &argv, true)?;
        t!(
            "prepare",
            "stopped",
            "--operation",
            "stop-prompt",
            "--text",
            "pending work"
        );
        p.update(|f| f.unavailable = true);
        reject!("stop", "stopped");
        let requested = t!("inspect", "stopped");
        ensure!(
            requested["launch"]["stop_requested"] == true
                && requested["task"]["outcome"] == "cancelled"
                && requested["prompts"][0]["released"] == true
                && requested["prompts"][0]["wait"] == "cancelled"
                && p.faults().kills == 0,
            "observation failure retargeted stop"
        );
        p.update(|f| {
            f.unavailable = false;
            f.list_failures = 1;
        });
        reject!("stop", "stopped");
        ensure!(
            p.faults().kills == 0,
            "recovered observation treated as confirmed stop"
        );
        p.update(|f| f.drop_kill = true);
        let closed = h.stop_complete("stopped")?;
        ensure!(
            f(&closed, "/session")? == f(&stopped, "/session")?
                && p.faults().kills == 1
                && t!("stop", "stopped") == closed
                && p.faults().kills == 1,
            "stop retry duplicated kill"
        );
        p.update(|f| f.drop_kill = false);
        h.start("stop-crash", Some(&runtime), &argv, true)?;
        p.reset_hold();
        p.update(|f| f.hold_kill = true);
        callers.push(h.spawn(&["stop".into(), "stop-crash".into()])?);
        until(Duration::from_secs(5), || Ok(p.accepted().then_some(())))?;
        let caller = &mut callers.last_mut().unwrap().0;
        caller.kill()?;
        process::wait(caller, Duration::from_secs(3))?;
        ensure!(
            t!("inspect", "stop-crash")["launch"]["stop_requested"] == true,
            "stop intent lost"
        );
        p.release();
        p.update(|f| f.hold_kill = false);
        ensure!(
            h.stop_complete("stop-crash")?["launch"]["phase"] == "closed" && p.faults().kills == 2,
            "killed stopper recovery"
        );
        ensure!(
            h.panes()?
                .iter()
                .any(|p| p.get("id") == host.get("id") && p.get("pid") == host.get("pid")),
            "host affected"
        );
        let lifecycle = h.start(
            "lifecycle",
            Some(&runtime),
            &["/bin/sh", "-c", "read line; printf LATER; exit 23"],
            true,
        )?;
        ensure!(
            t!("launch-reconcile", "lifecycle") == lifecycle,
            "attached reconciliation"
        );
        p.update(|f| f.unavailable = true);
        reject!("launch-reconcile", "lifecycle");
        let uncertain = t!("inspect", "lifecycle");
        ensure!(
            uncertain["attempt"]["state"] == "uncertain"
                && uncertain["launch"]["phase"] == "attached"
                && f(&uncertain, "/session")? == f(&lifecycle, "/session")?
                && uncertain["launch"]["problem"]
                    .as_str()
                    .is_some_and(|s| !s.is_empty()),
            "observation loss"
        );
        reject!(
            "prepare",
            "lifecycle",
            "--operation",
            "while-lost",
            "--text",
            "do not send"
        );
        p.update(|f| f.unavailable = false);
        let restored = t!("launch-reconcile", "lifecycle");
        ensure!(
            restored["attempt"]["state"] == "active"
                && f(&restored, "/launch/problem")?.is_null()
                && f(&restored, "/session")? == f(&lifecycle, "/session")?,
            "observation restoration"
        );
        t!(
            "prepare",
            "lifecycle",
            "--operation",
            "later-exit",
            "--text",
            "go"
        );
        t!("submit", "later-exit");
        until(Duration::from_secs(3), || {
            Ok(h.panes()?
                .iter()
                .all(|p| p.get("id") != lifecycle.pointer("/session/target/pane"))
                .then_some(()))
        })?;
        let before = t!("inspect", "lifecycle");
        let closed = h.closed("lifecycle", &before)?;
        ensure!(
            closed["launch"]["phase"] == "closed"
                && closed["launch"]["final_evidence"]["exit_status"] == 23
                && closed["attempt"]["state"] == "finished"
                && f(&closed, "/session")? == f(&lifecycle, "/session")?
                && f(&closed, "/task")? == f(&before, "/task")?
                && f(&closed, "/prompts")? == f(&before, "/prompts")?
                && t!("launch-reconcile", "lifecycle") == closed,
            "later closed evidence"
        );
        reject!(
            "prepare",
            "lifecycle",
            "--operation",
            "after-close",
            "--text",
            "do not send"
        );
        p.update(|f| f.mode = Mode::WaitExit);
        let short = ["/bin/sh", "-c", "printf FINAL-LAUNCH; exit 17"];
        let closed = h.start("short", Some(&runtime), &short, true)?;
        ensure!(
            closed["launch"]["phase"] == "closed"
                && closed["launch"]["final_evidence"]["exit_status"] == 17
                && closed["launch"]["final_evidence"]["text"]
                    .as_str()
                    .context("text")?
                    .contains("FINAL-LAUNCH")
                && f(&closed, "/session/target/pid")?.is_null()
                && closed["attempt"]["state"] == "finished"
                && closed["task"]["outcome"] == "open"
                && h.start("short", Some(&runtime), &short, true)? == closed,
            "short launch final recovery"
        );
        reject!(
            "prepare",
            "short",
            "--operation",
            "too-late",
            "--text",
            "do not send"
        );
        p.update(|f| f.large_final = true);
        let large = h.start(
            "large-final",
            Some(&runtime),
            &["/bin/sh", "-c", "exit 0"],
            true,
        )?;
        p.update(|f| f.large_final = false);
        let evidence = f(&large, "/launch/final_evidence")?;
        ensure!(
            evidence["exit_status"] == 0
                && evidence["truncated"] == true
                && evidence["text"].as_str().context("text")?.len() <= 4096
                && large["task"]["outcome"] == "open",
            "UTF-8 final cap"
        );
        let before = p.faults().creates;
        p.update(|f| f.mode = Mode::DropAfter);
        h.start("short-lost", Some(&runtime), &short, false)?;
        until(Duration::from_secs(3), || {
            Ok((h.panes()?.len() == 4).then_some(()))
        })?;
        ensure!(
            f(&t!("inspect", "short-lost"), "/launch/pane")?.is_null(),
            "lost reply invented pane hint"
        );
        for fault in ["gap", "bad_final", "expired", "duplicate"] {
            p.update(|f| match fault {
                "gap" => f.gap = true,
                "bad_final" => f.bad_final = true,
                "expired" => f.expired = true,
                _ => f.duplicate = true,
            });
            reject!("launch-reconcile", "short-lost");
            ensure!(
                t!("inspect", "short-lost")["launch"]["phase"] == "uncertain",
                "invalid final accepted"
            );
            p.update(|f| {
                f.gap = false;
                f.bad_final = false;
                f.expired = false;
                f.duplicate = false;
            });
        }
        let recovered = t!("launch-reconcile", "short-lost");
        ensure!(
            recovered["launch"]["phase"] == "closed"
                && recovered["launch"]["final_evidence"]["exit_status"] == 17
                && t!("launch-reconcile", "short-lost") == recovered
                && p.faults().creates == before + 1,
            "exit recovery duplicated creation"
        );
        ensure!(
            h.panes()?
                .iter()
                .any(|p| p.get("id") == host.get("id") && p.get("pid") == host.get("pid")),
            "host changed"
        );
        let cancelled = t!("inspect", "live");
        completed(
            &root.control(),
            json!({"id":1,"command":"kill","instance":h.instance,"pane":target["pane"]}),
        )?;
        until(Duration::from_secs(3), || {
            Ok(h.panes()?
                .iter()
                .all(|p| p.get("id") != target.get("pane"))
                .then_some(()))
        })?;
        let closed = h.closed("live", &cancelled)?;
        ensure!(
            closed["launch"]["phase"] == "closed"
                && f(&closed, "/task")? == f(&cancelled, "/task")?
                && closed["task"]["outcome"] == "cancelled"
                && f(&closed, "/prompts")? == f(&cancelled, "/prompts")?,
            "cancelled close rewrote history"
        );
        p.update(|f| {
            f.mode = Mode::WaitExit;
            f.vanish = true;
        });
        let vanished = h.start("vanished", Some(&runtime), &short, true)?;
        ensure!(
            vanished["launch"]["phase"] == "closed"
                && vanished["launch"]["final_evidence"]["exit_status"] == 17
                && !root.control().exists()
                && h.start("vanished", Some(&runtime), &short, true)? == vanished,
            "vanished workspace final recovery"
        );
        Ok(())
    })();
    let mut errors = Vec::new();
    if let Some(p) = proxy.as_mut()
        && let Err(e) = p.close()
    {
        errors.push(format!("proxy: {e:#}"));
    }
    for caller in &mut callers {
        let cleanup = (|| -> Result<()> {
            if caller.0.try_wait()?.is_none() {
                caller.0.kill()?;
                process::wait(&mut caller.0, Duration::from_secs(3))?;
            }
            Ok(())
        })();
        if let Err(e) = cleanup {
            errors.push(format!("caller: {e:#}"));
        }
    }
    if let Err(e) = server.finish() {
        errors.push(format!("server: {e:#}"));
    }
    result.context(format!("cleanup errors: {errors:?}"))?;
    ensure!(errors.is_empty(), "{}", errors.join("; "));
    println!(
        "PASS closed launch evidence, gaps, identity rejection, managed launch, stable-ID replay, lost creation reply, killed creator, and no blind replacement"
    );
    Ok(())
}
