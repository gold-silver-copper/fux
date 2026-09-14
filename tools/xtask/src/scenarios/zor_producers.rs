//! Real fux/zor and bundled Node adapter reloads; native SDK events are synthetic.
use crate::support::{
    contention::{RealClock, cli_busy, retry_busy},
    local::{Root, Server, completed},
    process,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    time::{Duration, Instant},
};
struct Harness<'a> {
    // Fields drop in declaration order: reap the server before removing its root.
    server: Server,
    root: Root,
    zor: &'a Path,
}
impl Harness<'_> {
    fn task(&self, args: &[&str], error: Option<&str>) -> Result<Value> {
        let replay = matches!(
            args.first(),
            Some(
                &"start"
                    | &"inspect"
                    | &"prepare"
                    | &"submit"
                    | &"wait"
                    | &"register-adapter"
                    | &"result"
                    | &"heartbeat-adapter"
                    | &"report"
                    | &"abandon"
                    | &"reserve"
                    | &"list"
            )
        );
        let reply = retry_busy(
            |remaining| {
                let mut c = self.root.command(self.zor);
                c.arg("task").args(args);
                process::output(c, Duration::from_secs(12).min(remaining), 1024 * 1024)
            },
            |r| Ok(cli_busy(r.status.code(), &r.stderr)),
            Instant::now() + Duration::from_secs(5),
            replay,
            &RealClock,
        )?;
        if let Some(error) = error {
            ensure!(
                !reply.status.success() && String::from_utf8_lossy(&reply.stderr).contains(error),
                "task {args:?}: expected {error}: {}",
                String::from_utf8_lossy(&reply.stderr)
            );
            return Ok(Value::Null);
        }
        ensure!(
            reply.status.success(),
            "task {args:?}: {} {}",
            String::from_utf8_lossy(&reply.stdout),
            String::from_utf8_lossy(&reply.stderr)
        );
        Ok(serde_json::from_slice(&reply.stdout)?)
    }
    fn until<T>(&mut self, mut check: impl FnMut(&Self) -> Result<Option<T>>) -> Result<T> {
        let end = Instant::now() + Duration::from_secs(10);
        loop {
            ensure!(self.server.child.try_wait()?.is_none(), "server exited");
            let error = self.root.path().join("error");
            ensure!(
                !error.exists(),
                "adapter worker: {}",
                fs::read_to_string(error).unwrap_or_default()
            );
            if let Some(v) = check(self)? {
                return Ok(v);
            }
            ensure!(Instant::now() < end, "producer fixture deadline");
            std::thread::sleep(Duration::from_millis(30));
        }
    }
    fn state(&self) -> Result<Value> {
        match fs::read(self.root.path().join("worker.json")) {
            Ok(b) => Ok(serde_json::from_slice(&b).unwrap_or_else(|_| json!({}))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
            Err(e) => Err(e.into()),
        }
    }
    fn inspect(&self) -> Result<Value> {
        self.task(&["inspect", "worker"], None)
    }
    fn profile(&self) -> Result<Value> {
        Ok(self
            .inspect()?
            .pointer("/launch/integration")
            .context("profile")?
            .clone())
    }
    fn prompt(&self, name: &str) -> Result<Value> {
        self.inspect()?["prompts"]
            .as_array()
            .context("prompts")?
            .iter()
            .find(|p| p["id"] == name)
            .cloned()
            .context("missing prompt")
    }
    fn prepare(&self, name: &str, text: &str) -> Result<Value> {
        self.task(
            &["prepare", "worker", "--operation", name, "--text", text],
            None,
        )
    }
    fn submit(&mut self, name: &str, text: &str) -> Result<Value> {
        self.prepare(name, text)?;
        self.task(&["submit", name], None)?;
        self.until(|h| {
            let p = h.prompt(name)?;
            Ok((!p["report_binding"].is_null()).then_some(p))
        })
    }
    fn response(&mut self, name: &str) -> Result<Value> {
        self.until(|h| {
            let p = h.task(&["wait", name], None)?;
            Ok((p["wait"] == "response-observed").then_some(p))
        })
    }
    fn capture(&self, instance: &str, target: &Value) -> Result<Value> {
        completed(
            &self.root.control(),
            json!({"id":1,"command":"capture","instance":instance,"pane":target["pane"],"max_bytes":4096}),
        )
    }
    fn registration(&self, marker: &str, producer: &str, error: Option<&str>) -> Result<Value> {
        self.task(
            &[
                "register-adapter",
                "worker",
                "--marker",
                marker,
                "--producer",
                producer,
            ],
            error,
        )
    }
    fn reload(&mut self, generation: u64, target: &Value) -> Result<Value> {
        fs::write(self.root.path().join("reload"), "reload")?;
        self.until(|h| Ok((h.state()?["generation"] == generation).then_some(())))?;
        ensure!(
            self.state()?["pid"] == target["pid"]
                && self.inspect()?["session"]["target"] == *target,
            "reload replaced process"
        );
        self.profile()
    }
}
fn string(v: &Value, key: &str) -> Result<String> {
    Ok(v[key]
        .as_str()
        .with_context(|| format!("missing {key}"))?
        .into())
}
pub(super) fn run(fux: &Path, zor: &Path) -> Result<()> {
    let node = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|p| p.join("node"))
        .find(|p| p.is_file() && fs::metadata(p).is_ok_and(|m| m.permissions().mode() & 0o111 != 0))
        .context("node required for bundled adapter recovery")?
        .canonicalize()?;
    let root = Root::new("zprod-rs-", &["/bin/cat".into()])?;
    let worker = root.path().join("worker.mjs");
    fs::write(&worker, include_str!("producer-worker.mjs"))?;
    let server = root.server(fux)?;
    let mut h = Harness { root, zor, server };
    let scenario = (|| -> Result<()> {
        let instance = string(
            &completed(&h.root.control(), json!({"id":1,"command":"list"}))?,
            "instance",
        )?;
        let started = h.task(
            &[
                "start",
                "worker",
                "--title",
                "producer reload fixture",
                "--integration",
                "opencode",
                "--instance",
                &instance,
                "--workspace",
                "default",
                "--cwd",
                h.root.path().to_str().context("root")?,
                "--",
                node.to_str().context("node")?,
                worker.to_str().context("worker")?,
            ],
            None,
        )?;
        let target = started["session"]["target"].clone();
        let marker = string(&started["launch"], "marker")?;
        let journal = h.root.path().join("state/zor/journal.json");
        h.until(|h| {
            Ok(
                (h.state()?["generation"] == 1
                    && h.root.path().join("duplicate-rejected").exists())
                .then_some(()),
            )
        })?;
        ensure!(h.state()?["pid"] == target["pid"], "worker identity");
        ensure!(
            h.profile()?["storage_environment"]
                == json!({"HOME":h.root.path(),"XDG_CONFIG_HOME":h.root.path().join("config"),"XDG_STATE_HOME":h.root.path().join("state"),"XDG_DATA_HOME":null,"XDG_CACHE_HOME":null}),
            "storage environment"
        );
        let old = string(&h.profile()?, "producer")?;
        h.submit("completed", "respond")?;
        let first = h.response("completed")?;
        h.submit("pending", "HOLD")?;
        let pending = h.prompt("pending")?;
        h.until(|h| {
            Ok((h
                .task(&["result", "worker"], None)?
                .pointer("/integration/heartbeat/observation/operation")
                == Some(&json!("pending")))
            .then_some(()))
        })?;
        let before = h.capture(&instance, &target)?["input_sequence"].clone();
        ensure!(before == 2, "initial input");
        let new = h.reload(2, &target)?;
        let producer = string(&new, "producer")?;
        ensure!(
            producer != old && new["retired"][0]["producer"] == old,
            "retired ancestry"
        );
        let fresh = h.until(|h| {
            let p = h.task(&["result", "worker"], None)?["integration"]["heartbeat"].clone();
            Ok((p["status"] == "current").then_some(p))
        })?;
        ensure!(
            fresh["observation"].is_null()
                && fresh["input_sequence"].is_null()
                && fresh["received_ms"].as_u64().context("heartbeat time")?
                    >= new["registered_ms"].as_u64().context("registration time")?,
            "heartbeat failed to reset"
        );
        ensure!(
            h.registration(&marker, &producer, None)? == new,
            "registration replay"
        );
        h.registration(&marker, &old, Some("retired"))?;
        h.registration(&marker, "foreign-producer", Some("handshake mismatch"))?;
        ensure!(
            h.capture(&instance, &target)?["input_sequence"] == before && h.state()?["count"] == 2,
            "reload typed input"
        );
        let lost = h.task(&["wait", "pending"], None)?;
        ensure!(
            lost["wait"] == "uncertain"
                && lost["released"] == false
                && lost["delivery"] == "delivered"
                && string(&lost, "wait_problem")?.contains("producer retired"),
            "pending producer-loss state"
        );
        h.task(&["submit", "pending"], Some("retired"))?;
        h.task(
            &[
                "prepare",
                "worker",
                "--operation",
                "competing",
                "--text",
                "respond",
            ],
            Some("another prompt is pending"),
        )?;
        h.task(
            &[
                "heartbeat-adapter",
                "worker",
                "--marker",
                &marker,
                "--producer",
                &old,
                "--sequence",
                "100",
            ],
            Some("producer mismatch"),
        )?;
        let binding = &pending["report_binding"];
        h.task(
            &[
                "report",
                "pending",
                "--token",
                &string(&pending, "report_token")?,
                "--producer",
                &old,
                "--sequence",
                "100",
                "--input-operation",
                &binding["input_operation"].to_string(),
                "--agent-session",
                &string(&binding["message"], "session")?,
                "--message",
                &string(&binding["message"], "id")?,
                "--kind",
                "response-observed",
            ],
            Some("retired"),
        )?;
        let report = &first["response"];
        ensure!(
            h.task(
                &[
                    "report",
                    "completed",
                    "--token",
                    &string(&first, "report_token")?,
                    "--producer",
                    &old,
                    "--sequence",
                    &report["sequence"].to_string(),
                    "--input-operation",
                    &report["input_operation"].to_string(),
                    "--agent-session",
                    &string(&report["message"], "session")?,
                    "--message",
                    &string(&report["message"], "id")?,
                    "--kind",
                    &string(report, "kind")?
                ],
                None
            )? == *report
                && h.task(&["wait", "completed"], None)?["response"] == *report,
            "historical report replay"
        );
        h.task(&["abandon", "pending"], None)?;
        h.submit("after-reload", "respond")?;
        h.response("after-reload")?;
        ensure!(
            h.capture(&instance, &target)?["input_sequence"] == 3,
            "third input"
        );
        h.prepare("unsent", "respond")?;
        let receipt = h.task(&["reserve", "unsent"], None)?["receipt"].clone();
        let mut state: Value = serde_json::from_slice(&fs::read(&journal)?)?;
        state["prompts"]["unsent"]["arm"] = json!({"producer":producer,"input_operation":receipt["operation"],"acknowledged":false,"input_started":false,"disarm_requested":false,"disarmed":false});
        fs::write(&journal, serde_json::to_vec(&state)?)?;
        let third = h.reload(3, &target)?;
        ensure!(
            third["retired"].as_array().context("retired")?.len() == 2
                && h.capture(&instance, &target)?["input_sequence"] == 3
                && third["storage_environment"].is_null(),
            "third reload namespace/input"
        );
        let unsent = h.task(&["wait", "unsent"], None)?;
        ensure!(
            unsent["wait"] == "uncertain"
                && unsent["delivery"] == "reserved"
                && unsent["receipt"]["bytes_written"] == 0
                && unsent["arm"]["input_started"] == false,
            "unsent recovery"
        );
        h.task(&["submit", "unsent"], Some("retired"))?;
        let abandoned = h.task(&["abandon", "unsent"], None)?;
        ensure!(
            abandoned["released"] == true && abandoned["arm"]["disarmed"] == false,
            "unsent abandonment"
        );
        h.submit("final", "respond")?;
        h.response("final")?;
        ensure!(
            h.capture(&instance, &target)?["input_sequence"] == 4
                && h.state()?["count"] == 4
                && h.inspect()?["task"]["outcome"] == "open",
            "final input/task outcome"
        );
        let saved = fs::read(&journal)?;
        let mut state: Value = serde_json::from_slice(&saved)?;
        let retained = state["launches"]["worker"]["integration"]["retired"]
            .as_array_mut()
            .context("retired")?;
        retained.push(retained[0].clone());
        fs::write(&journal, serde_json::to_vec(&state)?)?;
        let rejection = h.task(&["list"], Some("retired adapter lifetime"));
        fs::write(&journal, &saved)?;
        rejection?;
        let mut state: Value = serde_json::from_slice(&saved)?;
        let integration = &mut state["launches"]["worker"]["integration"];
        let at = integration["registered_ms"].clone();
        let retired = integration["retired"].as_array_mut().context("retired")?;
        for i in 0..14 {
            retired.push(
                json!({"producer":format!("retired-{i}"),"registered_ms":at,"retired_ms":at}),
            );
        }
        fs::write(&journal, serde_json::to_vec(&state)?)?;
        let rejection = h.registration(&marker, "one-too-many", Some("retention limit"));
        fs::write(&journal, &saved)?;
        rejection?;
        Ok(())
    })();
    let cleanup = h.server.finish();
    cleanup?;
    scenario?;
    println!(
        "PASS producer reload, heartbeat reset, historical replay and arm-before-input recovery"
    );
    Ok(())
}

/// Exercise resume's shared attachment branch with real processes and a synthetic
/// adapter. This proves orchestration identity, not a provider's session restore.
pub(super) fn resume(
    fux: &Path,
    zor: &Path,
    koh: Option<&Path>,
    lose_reply: bool,
    dashboard: bool,
) -> Result<()> {
    ensure!(!(lose_reply && dashboard), "dashboard resume uses the delivered-reply gateway");
    use crate::support::launch_proxy::{Mode, Proxy};
    let node = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|path| path.join("node"))
        .find(|path| {
            path.is_file() && fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
        })
        .context("node required for resume adapter fixture")?
        .canonicalize()?;
    let root = Root::new("zresume-attach-rs-", &["/bin/cat".into()])?;
    let worker = root.path().join("worker.mjs");
    fs::write(&worker, include_str!("producer-worker.mjs"))?;
    let server = root.server(fux)?;
    let mut h = Harness { root, zor, server };
    let instance =
        completed(&h.root.control(), json!({"id":1,"command":"list"}))?["instance"].clone();
    let runtime = h.root.path().join("proxy");
    fs::create_dir(&runtime)?;
    let proxy = Proxy::start(
        &runtime,
        &h.root.control(),
        &h.root.path().join("fux/manager.sock"),
        instance.clone(),
    )?;
    proxy.update(|faults| faults.mode = Mode::Normal);
    let scenario = (|| -> Result<()> {
        let instance_text = instance.as_str().context("instance")?;
        let original = h.task(
            &[
                "start",
                "worker",
                "--title",
                "resume attachment fixture",
                "--integration",
                "opencode",
                "--instance",
                instance_text,
                "--workspace",
                "default",
                "--runtime",
                runtime.to_str().context("runtime")?,
                "--cwd",
                h.root.path().to_str().context("cwd")?,
                "--",
                node.to_str().context("node")?,
                worker.to_str().context("worker")?,
            ],
            None,
        )?;
        h.until(|h| Ok(h.profile()?["producer"].as_str().map(str::to_owned)))?;
        h.submit("completed", "respond")?;
        h.response("completed")?;
        h.prepare("unsent", "must not replay")?;
        let before = h.inspect()?;
        completed(
            &h.root.control(),
            json!({"id":1,"command":"kill","instance":instance,"pane":original["session"]["target"]["pane"]}),
        )?;
        h.until(|h| {
            let reply = crate::support::local::rpc(
                &h.root.path().join("fux/manager.sock"),
                json!({"request":"final", "instance":instance, "pane":original["session"]["target"]["pane"]}),
            )?;
            ensure!(reply["reply"] == "final", "unexpected final reply");
            Ok((reply["result"]["status"] == "completed").then_some(()))
        })?;
        h.until(|h| {
            let closed = h.task(&["launch-reconcile", "worker"], None)?;
            Ok((closed["launch"]["phase"] == "closed").then_some(()))
        })?;
        let closed = h.inspect()?;
        let creates = proxy.faults().creates;
        proxy.update(|faults| faults.exit_before_pin = true);
        let args = [
            "resume",
            "worker",
            "--operation",
            "resume-one",
            "--instance",
            instance_text,
        ];
        // Exercise the service guard with a real successful transition. A stale
        // guard must still fail after success, even for a retained operation ID.
        let service = process::Guard(
            h.root
                .command(zor)
                .arg("serve")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()?,
        );
        let socket = h.root.path().join("zor/control.sock");
        crate::support::local::until(Duration::from_secs(5), || Ok(socket.exists().then_some(())))?;
        let service_instance = super::zor_headless::api(
            &socket,
            &json!({"v":1,"id":1,"op":"ping"}),
        )?["service_instance"]
            .clone();
        let request = |inspection: &Value| {
            json!({
                "v":1,"id":2,"op":"task","service_instance":service_instance,
                "task":{"action":"guarded-resume","operation":"resume-one",
                    "instance":instance_text,"expected":{
                        "task":inspection["task"]["id"],
                        "attempt":inspection["attempt"]["id"],
                        "session":inspection["session"]["id"],
                        "target":inspection["session"]["target"]
                    }}
            })
        };
        // Mutations are dispatched once; a timeout is not permission to retry.
        let mut remote = koh
            .map(|koh| super::resume_remote::Remote::start(zor, koh, &socket, lose_reply))
            .transpose()?;
        let resumed = if let Some(remote) = &mut remote {
            if lose_reply {
                let error = remote
                    .resume(&args)
                    .expect_err("completed reply must be lost");
                ensure!(
                    format!("{error:#}").contains("request was not replayed"),
                    "unknown outcome missing: {error:#}"
                );
                remote.verify_lost_reply(1)?;
                let intents = remote.retained_intents()?;
                let intent = intents
                    .as_array()
                    .context("controller intents")?
                    .first()
                    .context("durable intent missing")?;
                ensure!(
                    intents.as_array().is_some_and(|items| items.len() == 1)
                        && intent["operation"] == "resume-one"
                        && intent["expected"]["task"] == "worker"
                        && intent["expected"]["attempt"] == closed["attempt"]["id"]
                        && intent["fux_instance"] == instance,
                    "wrong durable pre-dispatch intent: {intents}"
                );
                let journal_after_commit = fs::read(h.root.path().join("state/zor/journal.json"))?;
                let retained =
                    remote.resume(&["resume-status", "worker", "--operation", "resume-one"])?;
                ensure!(
                    retained["record"]["phase"] == "closed",
                    "lost reply operation was not retained: {retained}"
                );
                let inspection = remote.resume(&["inspect", "worker"])?;
                ensure!(
                    fs::read(h.root.path().join("state/zor/journal.json"))? == journal_after_commit
                        && proxy.faults().creates == creates + 1,
                    "read-only recovery mutated committed operation"
                );
                remote.verify_lost_reply(1)?;
                println!(
                    "PASS lost completed remote reply: unknown outcome, one mutation, read-only status/inspect recovery"
                );
                inspection
            } else if dashboard {
                remote.dashboard_resume(fux, "resume-one", instance_text)?
            } else {
                remote.resume(&args)?
            }
        } else {
            let reply = super::zor_headless::api(&socket, &request(&closed))?;
            ensure!(reply["status"] == "completed", "guarded resume: {reply}");
            reply["value"].clone()
        };
        ensure!(
            resumed["launch"]["phase"] == "closed"
                && resumed["attempt"]["state"] == "finished"
                && resumed["task"]["outcome"] == "open"
                && resumed["attempt"]["id"] != original["attempt"]["id"]
                && resumed["session"]["target"]["pane"] != original["session"]["target"]["pane"]
                && resumed["launch"]["final_evidence"]["input_sequence"] == 0,
            "resume exit must close the new attempt without replay or task success"
        );
        let state: Value =
            serde_json::from_slice(&fs::read(h.root.path().join("state/zor/journal.json"))?)?;
        ensure!(
            state["launches"]["resume-one"]["final_evidence"] == closed["launch"]["final_evidence"]
                && state["launches"]["resume-one"]["session"] == closed["session"]["id"]
                && state["prompts"]["unsent"]
                    == before["prompts"]
                        .as_array()
                        .context("prompts")?
                        .iter()
                        .find(|p| p["id"] == "unsent")
                        .context("unsent")?
                        .clone(),
            "resume rewrote archived final evidence or unsent prompt"
        );
        ensure!(
            h.task(&args, None)? == resumed && proxy.faults().creates == creates + 1,
            "resume retry changed identity or duplicated creation"
        );
        let journal_path = h.root.path().join("state/zor/journal.json");
        let retained = fs::read(&journal_path)?;
        let stale = super::zor_headless::api(&socket, &request(&closed))?;
        ensure!(
            stale["status"] == "failed"
                && stale
                    .to_string()
                    .contains("selected task/attempt/process changed")
                && fs::read(&journal_path)? == retained,
            "stale retained-operation guard mutated state: {stale}"
        );
        let repeated = super::zor_headless::api(&socket, &request(&resumed))?;
        ensure!(
            repeated["status"] == "completed"
                && repeated["value"] == resumed
                && proxy.faults().creates == creates + 1
                && fs::read(&journal_path)? == retained,
            "fresh guarded reconciliation duplicated resume: {repeated}"
        );
        if let Some(remote) = &mut remote {
            ensure!(
                remote.resume(&args)? == resumed
                    && proxy.faults().creates == creates + 1
                    && fs::read(&journal_path)? == retained,
                "remote retained operation duplicated resume"
            );
        }
        let status_request = |operation: &str| {
            json!({"v":1,"id":3,"op":"task",
            "service_instance":service_instance,"task":{"action":"resume-status","id":"worker","operation":operation}})
        };
        let status_reply = super::zor_headless::api(&socket, &status_request("resume-one"))?;
        ensure!(
            status_reply["status"] == "completed",
            "resume status: {status_reply}"
        );
        let status = status_reply["value"].clone();
        ensure!(
            status["task"] == "worker"
                && status["operation"] == "resume-one"
                && status["record"]["phase"] == "closed"
                && status["record"]["previous_attempt"] == closed["attempt"]["id"]
                && status["record"]["instance"] == instance
                && status["record"]["session"] == resumed["session"]["id"],
            "wrong retained resume evidence: {status}"
        );
        let absent = super::zor_headless::api(&socket, &status_request("not-submitted"))?;
        ensure!(
            absent["status"] == "completed" && absent["value"]["record"].is_null(),
            "missing resume operation did not remain absent: {absent}"
        );
        if let Some(remote) = &mut remote {
            ensure!(
                remote.resume(&["resume-status", "worker", "--operation", "resume-one"])? == status,
                "remote resume evidence differs"
            );
        }
        ensure!(
            fs::read(&journal_path)? == retained && proxy.faults().creates == creates + 1,
            "resume status read mutated journal or launched a pane"
        );
        if let Some(remote) = &mut remote {
            if lose_reply {
                remote.verify_lost_reply(2)?;
            }
            remote.finish_gate()?;
        }
        drop(remote);
        drop(service);
        h.task(&["stop", "resume-one"], Some("historical"))?;
        Ok(())
    })();
    drop(proxy);
    h.server.finish()?;
    scenario?;
    println!(
        "PASS guarded resume, stale guard refusal, retained operation, exit-before-pin and no input replay"
    );
    Ok(())
}
