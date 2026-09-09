//! Native message ancestry, receipt recovery, adapter lifetime and retirement.
use super::binding_adapter::Adapter;
use crate::support::{
    contention::{self, RealClock},
    local::{Root, completed, until},
    process::{self, Guard, OwnedProcess},
    service,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Seek, SeekFrom},
    os::unix::fs::symlink,
    path::{Path, PathBuf},
    process::Stdio,
    sync::atomic::Ordering,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
fn s(v: &Value) -> Result<&str> {
    v.as_str().context("string")
}
fn n(v: &Value) -> Result<u64> {
    v.as_u64().context("integer")
}
fn has(v: &Value, needle: &str) -> bool {
    v.as_str().is_some_and(|s| s.contains(needle))
}
fn named(v: &Value, key: &str, name: &str) -> Result<Value> {
    v.as_array()
        .context("array")?
        .iter()
        .find(|v| v[key] == name)
        .cloned()
        .context("named record")
}
struct Caller {
    child: Guard,
    out: fs::File,
    err: fs::File,
}
impl Caller {
    fn finish(&mut self, timeout: Duration) -> Result<Value> {
        let status = process::wait(&mut self.child.0, timeout)?;
        let read = |f: &mut fs::File| -> Result<Vec<u8>> {
            f.seek(SeekFrom::Start(0))?;
            let mut b = Vec::new();
            f.take(1048577).read_to_end(&mut b)?;
            ensure!(b.len() <= 1048576, "caller output bound");
            Ok(b)
        };
        let out = read(&mut self.out)?;
        let err = read(&mut self.err)?;
        ensure!(
            status.success(),
            "caller: {}",
            String::from_utf8_lossy(&err)
        );
        Ok(serde_json::from_slice(&out)?)
    }
}
struct Harness<'a> {
    root: &'a Root,
    zor: &'a Path,
    instance: String,
    journal: PathBuf,
}
impl Harness<'_> {
    fn raw(&self, args: &[&str]) -> Result<process::Output> {
        let mut c = self.root.command(self.zor);
        c.arg("task").args(args);
        process::output(c, Duration::from_secs(12), 1048576)
    }
    fn task(&self, args: &[&str], ok: bool) -> Result<Value> {
        let r = self.raw(args)?;
        ensure!(
            r.status.success() == ok,
            "task {args:?}: out={} err={}",
            String::from_utf8_lossy(&r.stdout),
            String::from_utf8_lossy(&r.stderr)
        );
        if ok {
            Ok(serde_json::from_slice(&r.stdout)?)
        } else {
            Ok(json!(String::from_utf8(r.stderr)?))
        }
    }
    fn owned(&self, args: &[String], ok: bool) -> Result<Value> {
        self.task(&args.iter().map(String::as_str).collect::<Vec<_>>(), ok)
    }
    fn rejected(&self, args: &[String], contains: Option<&str>) -> Result<()> {
        let before = fs::read(&self.journal)?;
        let e = self.owned(args, false)?;
        ensure!(
            fs::read(&self.journal)? == before,
            "rejection mutated journal"
        );
        if let Some(text) = contains {
            ensure!(has(&e, text), "expected {text}: {e}");
        }
        Ok(())
    }
    fn value(&self, command: &str, mut fields: Value) -> Result<Value> {
        fields["id"] = json!(1);
        fields["command"] = json!(command);
        fields["instance"] = json!(self.instance);
        completed(&self.root.control(), fields)
    }
    fn capture(&self, pane: &Value) -> Result<Value> {
        self.value("capture", json!({"pane":pane,"max_bytes":4096}))
    }
    fn prepare(&self, name: &str, owner: &str) -> Result<Value> {
        self.task(
            &[
                "prepare",
                owner,
                "--operation",
                name,
                "--text",
                "same prompt",
            ],
            true,
        )
    }
    fn write(&self, v: &Value) -> Result<()> {
        fs::write(&self.journal, serde_json::to_vec(v)?)?;
        Ok(())
    }
    fn spawn(&self, args: &[&str]) -> Result<Caller> {
        let out = tempfile::tempfile()?;
        let err = tempfile::tempfile()?;
        let child = Guard(
            self.root
                .command(self.zor)
                .args(args)
                .stdin(Stdio::null())
                .stdout(out.try_clone()?)
                .stderr(err.try_clone()?)
                .spawn()?,
        );
        Ok(Caller { child, out, err })
    }
    fn service(&self, args: &[&str]) -> Result<Caller> {
        let mut c = self.spawn(args)?;
        until(Duration::from_secs(8), || {
            ensure!(c.child.0.try_wait()?.is_none(), "service exited");
            Ok(self
                .root
                .path()
                .join("zor/control.sock")
                .exists()
                .then_some(()))
        })?;
        Ok(c)
    }
    fn row(
        &self,
        service: &mut Caller,
        deadline: Instant,
        key: &str,
        suffix: bool,
    ) -> Result<Option<Value>> {
        ensure!(
            service.child.0.try_wait()?.is_none(),
            "dashboard service exited"
        );
        let left = deadline
            .checked_duration_since(Instant::now())
            .context("dashboard freshness deadline")?;
        let mut c = self.root.command(self.zor);
        c.args(["dashboard", "--once"]);
        let r = process::output(c, left.min(Duration::from_secs(8)), 1048576)?;
        if contention::cli_busy(r.status.code(), &r.stderr) {
            return Ok(None);
        }
        ensure!(
            r.status.success(),
            "dashboard: {}",
            String::from_utf8_lossy(&r.stderr)
        );
        let v: Value = serde_json::from_slice(&r.stdout)?;
        Ok(v["rows"]
            .as_array()
            .context("rows")?
            .iter()
            .find(|v| {
                if suffix {
                    v["kind"] == "observation"
                        && v["key"].as_str().is_some_and(|s| s.ends_with(key))
                } else {
                    v["key"] == key
                }
            })
            .cloned())
    }
    fn retirement_row(&self) -> Result<Value> {
        let mut c = self.service(&["serve"])?;
        let end = Instant::now() + Duration::from_secs(8);
        let result = until(Duration::from_secs(8), || {
            self.row(&mut c, end, "group:armed-group", false)
        });
        let stopped = stop(&mut c);
        result.and_then(|v| {
            stopped?;
            Ok(v)
        })
    }
}
fn stop(c: &mut Caller) -> Result<()> {
    if c.child.0.try_wait()?.is_none() {
        c.child.0.terminate()?;
    }
    let status = process::wait(&mut c.child.0, Duration::from_secs(10))?;
    ensure!(status.success(), "service exit: {status}");
    Ok(())
}
fn bind(
    p: &Value,
    r: &Value,
    seq: u64,
    message: &str,
    producer: &str,
    session: &str,
) -> Result<Vec<String>> {
    Ok(vec![
        "bind-report".into(),
        s(&p["id"])?.into(),
        "--token".into(),
        s(&p["report_token"])?.into(),
        "--producer".into(),
        producer.into(),
        "--sequence".into(),
        seq.to_string(),
        "--input-operation".into(),
        r["operation"].to_string(),
        "--agent-session".into(),
        session.into(),
        "--message".into(),
        message.into(),
    ])
}
fn report(
    p: &Value,
    r: &Value,
    seq: u64,
    message: Option<&str>,
    producer: &str,
    session: &str,
) -> Result<Vec<String>> {
    let mut a = bind(p, r, seq, message.unwrap_or(""), producer, session)?;
    a[0] = "report".into();
    if message.is_none() {
        a.truncate(10);
    }
    a.extend(["--kind".into(), "response-observed".into()]);
    Ok(a)
}
fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| (*s).into()).collect()
}
fn replace(a: &[String], field: &str, value: &str) -> Result<Vec<String>> {
    let mut a = a.to_vec();
    let i = a.iter().position(|v| v == field).context("argument")?;
    a[i + 1] = value.into();
    Ok(a)
}
fn pulse(a: &[String], seq: u64, claim: Option<&Value>) -> Result<Vec<String>> {
    let mut a = replace(a, "--sequence", &seq.to_string())?;
    if let Some(v) = claim {
        a.extend(["--observation".into(), serde_json::to_string(v)?]);
    }
    Ok(a)
}
pub fn run(fux: &Path, zor: &Path) -> Result<()> {
    let root = Root::new("zbind-", &["/bin/cat".into()])?;
    let mut server = root.server(fux)?;
    let result = (|| -> Result<()> {
        let listing = completed(&root.control(), json!({"id":1,"command":"list"}))?;
        let h = Harness {
            root: &root,
            zor,
            instance: s(&listing["instance"])?.into(),
            journal: root.path().join("state/zor/journal.json"),
        };
        let host = &listing["workspaces"][0]["tabs"][0]["panes"][0]["id"];
        macro_rules! t{($($a:expr),+ $(,)?)=>{h.task(&[$($a),+],true)?};}
        macro_rules! reject {
            ($a:expr,$text:expr) => {
                h.rejected(&$a, Some($text))?
            };
            ($a:expr) => {
                h.rejected(&$a, None)?
            };
        }
        let directory = root.path().to_str().context("root path")?;
        let started = t!(
            "start",
            "worker",
            "--title",
            "binding fixture",
            "--instance",
            &h.instance,
            "--workspace",
            "default",
            "--cwd",
            directory,
            "--",
            "/bin/sh",
            "-c",
            "stty raw -echo; printf READY; cat"
        );
        let target = &started["session"]["target"];
        let pane = &target["pane"];
        until(Duration::from_secs(5), || {
            Ok(has(&h.capture(pane)?["text"], "READY").then_some(()))
        })?;
        let first = h.prepare("first", "worker")?;
        let r1 = t!("reserve", "first")["receipt"].clone();
        let b1 = bind(&first, &r1, 1, "message-one", "producer-one", "native-root")?;
        reject!(b1, "possibly submitted");
        ensure!(h.capture(pane)?["input_sequence"] == 0, "premature input");
        let mut state: Value = serde_json::from_slice(&fs::read(&h.journal)?)?;
        state["prompts"]["first"]["delivery"] = json!("submitting");
        h.write(&state)?;
        h.value(
            "input-submit",
            json!({"operation":r1["operation"],"keys":"same prompt\\r"}),
        )?;
        reject!(
            report(
                &first,
                &r1,
                2,
                Some("message-one"),
                "producer-one",
                "native-root"
            )?,
            "native message"
        );
        {
            let file = fs::File::open(h.journal.with_file_name("journal.lock"))?;
            let _lock = nix::fcntl::Flock::lock(file, nix::fcntl::FlockArg::LockExclusive)
                .map_err(|(_, e)| e)?;
            ensure!(
                has(&h.owned(&b1, false)?, "journal is busy"),
                "binding lock contention"
            );
        }
        let binding1 = h.owned(&b1, true)?;
        ensure!(
            binding1["message"] == json!({"session":"native-root","id":"message-one"})
                && h.owned(&b1, true)? == binding1,
            "binding replay"
        );
        ensure!(
            h.capture(pane)?["input_sequence"] == 1,
            "binding replayed input"
        );
        ensure!(
            [json!("queued"), json!("delivered")]
                .contains(&t!("inspect", "worker")["prompts"][0]["delivery"]),
            "receipt recovery"
        );
        reject!(
            bind(&first, &r1, 1, "different", "producer-one", "native-root")?,
            "different message binding"
        );
        reject!(replace(&b1, "--token", &"0".repeat(32))?, "token mismatch");
        for args in [
            report(&first, &r1, 2, None, "producer-one", "native-root")?,
            report(
                &first,
                &r1,
                2,
                Some("old-message"),
                "producer-one",
                "native-root",
            )?,
            report(
                &first,
                &r1,
                1,
                Some("message-one"),
                "producer-one",
                "native-root",
            )?,
            report(
                &first,
                &r1,
                2,
                Some("message-one"),
                "other-producer",
                "native-root",
            )?,
            report(
                &first,
                &r1,
                2,
                Some("message-one"),
                "producer-one",
                "child-session",
            )?,
        ] {
            reject!(args);
        }
        let report1args = report(
            &first,
            &r1,
            2,
            Some("message-one"),
            "producer-one",
            "native-root",
        )?;
        let report1 = h.owned(&report1args, true)?;
        ensure!(
            t!("wait", "first")["wait"] == "response-observed"
                && t!("inspect", "worker")["task"]["outcome"] == "open",
            "response is not outcome"
        );
        let second = h.prepare("second", "worker")?;
        let r2 = t!("submit", "second")["receipt"].clone();
        reject!(
            bind(
                &second,
                &r2,
                3,
                "message-one",
                "producer-restarted",
                "native-root"
            )?,
            "already bound"
        );
        reject!(
            bind(
                &second,
                &r2,
                2,
                "message-two",
                "producer-one",
                "native-root"
            )?,
            "sequence is stale"
        );
        reject!(
            bind(
                &second,
                &r2,
                3,
                "message-two",
                "producer-one",
                "child-session"
            )?,
            "lifetime changed"
        );
        h.owned(
            &bind(
                &second,
                &r2,
                3,
                "message-two",
                "producer-one",
                "native-root",
            )?,
            true,
        )?;
        fs::rename(root.path().join("fux"), root.path().join("offline"))?;
        let saved = fs::read(&h.journal)?;
        ensure!(
            h.owned(&b1, true)? == binding1
                && h.owned(&report1args, true)? == report1
                && fs::read(&h.journal)? == saved,
            "offline historical retry"
        );
        fs::rename(root.path().join("offline"), root.path().join("fux"))?;
        ensure!(
            t!("wait", "second")["wait"] == "pending",
            "old response resolved newer prompt"
        );
        reject!(report(
            &second,
            &r2,
            4,
            Some("message-one"),
            "producer-one",
            "native-root"
        )?);
        let saved = fs::read(&h.journal)?;
        for mutation in ["duplicate", "sequence", "delivery", "foreign-report"] {
            let mut v: Value = serde_json::from_slice(&saved)?;
            match mutation {
                "duplicate" => {
                    v["prompts"]["second"]["report_binding"]["message"]["id"] = json!("message-one")
                }
                "sequence" => v["prompts"]["second"]["report_binding"]["sequence"] = json!(2),
                "delivery" => v["prompts"]["second"]["delivery"] = json!("reserved"),
                _ => v["prompts"]["first"]["response"]["message"]["session"] = json!("foreign"),
            };
            h.write(&v)?;
            reject!(strings(&["list"]));
            fs::write(&h.journal, &saved)?;
        }
        h.value("send-keys", json!({"pane":pane,"keys":"human"}))?;
        h.owned(
            &report(
                &second,
                &r2,
                4,
                Some("message-two"),
                "producer-one",
                "native-root",
            )?,
            true,
        )?;
        ensure!(
            t!("wait", "second")["wait"] == "uncertain",
            "human input correlation"
        );
        t!("abandon", "second");
        let third = h.prepare("third", "worker")?;
        let r3 = t!("submit", "third")["receipt"].clone();
        let b3 = bind(
            &third,
            &r3,
            5,
            "message-three",
            "producer-one",
            "native-root",
        )?;
        h.value("send-keys", json!({"pane":pane,"keys":"human-again"}))?;
        reject!(b3, "intervening input");
        t!("abandon", "third");
        reject!(b3, "unresolved");
        let fourth = h.prepare("fourth", "worker")?;
        let r4 = t!("submit", "fourth")["receipt"].clone();
        let b4 = bind(
            &fourth,
            &r4,
            5,
            "message-four",
            "producer-one",
            "native-root",
        )?;
        let saved = fs::read(&h.journal)?;
        for field in ["pid", "instance", "stream"] {
            let mut v: Value = serde_json::from_slice(&saved)?;
            let session = s(&started["session"]["id"])?;
            let replacement = match field {
                "pid" => json!(n(&v["sessions"][session]["target"]["pid"])? + 1),
                "instance" => json!("different-server"),
                _ => json!(n(&target["stream"])? + 1),
            };
            v["sessions"][session]["target"][field] = replacement.clone();
            if field != "pid" {
                v["launches"]["worker"][field] = replacement;
            }
            h.write(&v)?;
            reject!(b4);
            fs::write(&h.journal, &saved)?;
        }
        let mut v: Value = serde_json::from_slice(&saved)?;
        v["prompts"]["fourth"]["deadline_ms"] =
            json!(n(&v["prompts"]["fourth"]["created_ms"])? + 1);
        h.write(&v)?;
        reject!(b4, "outside the prompt deadline");
        t!("abandon", "fourth");
        t!(
            "adopt",
            "adopted",
            "--title",
            "no launch authority",
            "--instance",
            &h.instance,
            "--workspace",
            "default",
            "--pane",
            &host.to_string()
        );
        let adopted = h.prepare("adopted-prompt", "adopted")?;
        let ar = t!("submit", "adopted-prompt")["receipt"].clone();
        reject!(
            bind(
                &adopted,
                &ar,
                1,
                "adopted-message",
                "adopted-producer",
                "native-root"
            )?,
            "managed launch"
        );
        t!("cancel", "adopted");
        ensure!(
            t!("inspect", "worker")["task"]["outcome"] == "open",
            "cancel foreign task"
        );
        until(Duration::from_secs(5), || {
            let r = h.raw(&["stop", "worker"])?;
            if r.status.success() {
                let v: Value = serde_json::from_slice(&r.stdout)?;
                ensure!(v["launch"]["phase"] == "closed", "stop phase");
                Ok(Some(()))
            } else {
                let e = String::from_utf8(r.stderr)?;
                ensure!(
                    e.contains("not yet confirmed") || e.contains("lifecycle uncertain"),
                    "stop: {e}"
                );
                Ok(None)
            }
        })?;
        ensure!(
            h.owned(&b1, true)? == binding1,
            "retained binding needs no live process"
        );
        configured(&h, host, directory)?;
        Ok(())
    })();
    let stopped = server.finish();
    result?;
    stopped?;
    println!("PASS native bindings, heartbeat freshness, adapter arming and retirement");
    Ok(())
}
fn configured(h: &Harness<'_>, host: &Value, directory: &str) -> Result<()> {
    macro_rules! t{($($a:expr),+ $(,)?)=>{h.task(&[$($a),+],true)?};}
    macro_rules! reject {
        ($a:expr,$text:expr) => {
            h.rejected(&$a, Some($text))?
        };
        ($a:expr) => {
            h.rejected(&$a, None)?
        };
    }
    let configured = t!(
        "start",
        "armed-worker",
        "--integration",
        "opencode",
        "--title",
        "arming fixture",
        "--instance",
        &h.instance,
        "--workspace",
        "default",
        "--cwd",
        directory,
        "--",
        "/bin/sh",
        "-c",
        "stty raw -echo; printf ARMED_READY; cat"
    );
    let pane = &configured["session"]["target"]["pane"];
    let profile = &configured["launch"]["integration"];
    let prompt = h.prepare("armed-input", "armed-worker")?;
    ensure!(
        t!("adapter-status", "armed-worker")["availability"] == "not-registered"
            && t!("adapter-status", "worker")["availability"] == "not-configured"
            && t!("result", "armed-worker")["integration"]["availability"] == "not-probed",
        "initial adapter state"
    );
    ensure!(
        has(
            &h.task(&["submit", "armed-input"], false)?,
            "has not registered"
        ),
        "unregistered submit"
    );
    ensure!(
        h.capture(pane)?["input_sequence"] == 0,
        "unregistered input"
    );
    let marker = s(&configured["launch"]["marker"])?;
    let endpoint = Path::new(s(&profile["socket"])?);
    let mut adapter = Adapter::start(endpoint, marker.into())?;
    let result = (|| -> Result<()> {
        let registration = strings(&[
            "register-adapter",
            "armed-worker",
            "--marker",
            marker,
            "--producer",
            "socket-producer",
        ]);
        let wrong = replace(&registration, "--producer", "foreign-producer")?;
        reject!(wrong, "handshake mismatch");
        let registered = h.owned(&registration, true)?;
        ensure!(
            registered["producer"] == "socket-producer"
                && h.owned(&registration, true)? == registered,
            "registration replay"
        );
        reject!(wrong, "handshake mismatch");
        ensure!(
            t!("adapter-status", "armed-worker")["heartbeat"]["status"] == "never-seen",
            "initial heartbeat"
        );
        let pulse_args = strings(&[
            "heartbeat-adapter",
            "armed-worker",
            "--marker",
            marker,
            "--producer",
            "socket-producer",
            "--sequence",
            "1",
        ]);
        let original_journal = fs::read(&h.journal)?;
        let p = h.owned(&pulse_args, true)?;
        let pulse_file = h
            .journal
            .parent()
            .context("journal parent")?
            .join("heartbeats")
            .join(format!("{marker}.json"));
        let original_pulse = fs::read(&pulse_file)?;
        ensure!(
            p["sequence"] == 1
                && p["producer"] == "socket-producer"
                && h.owned(&pulse_args, true)? == p
                && fs::read(&pulse_file)? == original_pulse
                && fs::read(&h.journal)? == original_journal,
            "heartbeat immutable replay"
        );
        ensure!(
            t!("result", "armed-worker")["integration"]["heartbeat"]["status"] == "current",
            "heartbeat freshness"
        );
        let newer = pulse(&pulse_args, 2, None)?;
        for (field, value) in [
            ("--marker", "0".repeat(32)),
            ("--producer", "foreign".into()),
            ("--sequence", "0".into()),
        ] {
            reject!(replace(&newer, field, &value)?);
            ensure!(
                fs::read(&pulse_file)? == original_pulse,
                "rejected pulse mutated sidecar"
            );
        }
        until(Duration::from_secs(8), || {
            Ok(
                (h.task(&["result", "armed-worker"], true)?["integration"]["heartbeat"]["status"]
                    == "expired")
                    .then_some(()),
            )
        })?;
        ensure!(
            h.owned(&pulse_args, true)? == p
                && t!("result", "armed-worker")["integration"]["heartbeat"]["status"] == "expired",
            "replay refreshed expired pulse"
        );
        h.owned(&newer, true)?;
        reject!(pulse_args, "stale");
        let saved_pulse = fs::read(&pulse_file)?;
        let mut adjusted: Value = serde_json::from_slice(&saved_pulse)?;
        adjusted["received_ms"] = json!(n(&adjusted["received_ms"])? - 5500);
        adjusted["monotonic_ms"] = json!(n(&adjusted["monotonic_ms"])? - 6100);
        fs::write(&pulse_file, serde_json::to_vec(&adjusted)?)?;
        ensure!(
            t!("result", "armed-worker")["integration"]["heartbeat"]["status"] == "expired",
            "rollback revived monotonic expiry"
        );
        fs::write(&pulse_file, &saved_pulse)?;
        let mut future: Value = serde_json::from_slice(&saved_pulse)?;
        future["received_ms"] = json!(
            u64::try_from(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis())? + 60000
        );
        fs::write(&pulse_file, serde_json::to_vec(&future)?)?;
        ensure!(
            t!("result", "armed-worker")["integration"]["heartbeat"]["status"] == "clock-rollback",
            "future pulse"
        );
        reject!(pulse(&pulse_args, 3, None)?, "clock moved backwards");
        fs::write(&pulse_file, &saved_pulse)?;
        for bad in [b"{corrupt".to_vec(), vec![b'x'; 1025]] {
            fs::write(&pulse_file, &bad)?;
            ensure!(
                t!("result", "armed-worker")["integration"]["heartbeat"]["status"] == "unavailable",
                "unsafe pulse freshness"
            );
            reject!(pulse(&pulse_args, 3, None)?);
            ensure!(fs::read(&pulse_file)? == bad, "unsafe pulse overwritten");
        }
        fs::write(&pulse_file, &saved_pulse)?;
        let retained = pulse_file.with_extension("saved");
        fs::rename(&pulse_file, &retained)?;
        symlink(&retained, &pulse_file)?;
        ensure!(
            t!("result", "armed-worker")["integration"]["heartbeat"]["status"] == "unavailable",
            "symlink pulse"
        );
        reject!(pulse(&pulse_args, 3, None)?);
        fs::remove_file(&pulse_file)?;
        fs::rename(&retained, &pulse_file)?;
        let mut service = h.service(&["serve"])?;
        let service_result = (|| -> Result<()> {
            let endpoint = h.root.path().join("zor/control.sock");
            let incarnation = service::rpc(
                &endpoint,
                &json!({"v":1,"id":1,"op":"ping"}),
                Duration::from_secs(5),
            )?["service_instance"]
                .clone();
            let mut request = json!({"v":1,"id":2,"op":"task","service_instance":incarnation,"task":{"action":"heartbeat-adapter","id":"armed-worker","marker":marker,"producer":"socket-producer","sequence":2}});
            let call = |request: &Value| -> Result<Value> {
                contention::retry_busy(
                    |left| service::rpc(&endpoint, request, left.min(Duration::from_secs(5))),
                    |reply| contention::api_busy(reply, request),
                    Instant::now() + Duration::from_secs(12),
                    request["task"]["action"] == "heartbeat-adapter",
                    &RealClock,
                )
            };
            let response = call(&request)?;
            ensure!(
                response["status"] == "completed"
                    && response["value"] == serde_json::from_slice::<Value>(&saved_pulse)?,
                "service heartbeat replay: {response}"
            );
            request["task"]["unknown"] = json!(true);
            ensure!(
                call(&request)?["status"] != "completed",
                "unknown field accepted"
            );
            Ok(())
        })();
        let stopped = stop(&mut service);
        service_result?;
        stopped?;
        ensure!(
            fs::read(&h.journal)? == original_journal,
            "heartbeat mutated journal"
        );
        let baseline = fs::read(&h.journal)?;
        let ready = t!("adapter-status", "armed-worker");
        ensure!(
            ready["availability"] == "reachable"
                && ready["stage"] == "complete"
                && ready["producer"] == "socket-producer"
                && ready["scope"] == "point-in-time-endpoint-probe"
                && ready["generation"] == ready["current_generation"],
            "adapter probe: {ready}"
        );
        let retained_integration = t!("result", "armed-worker")["integration"].clone();
        ensure!(
            retained_integration["producer"] == "socket-producer"
                && retained_integration["availability"] == "not-probed",
            "probe retained liveness"
        );
        for fault in ["hello-wrong", "hello-version", "hello-drop"] {
            adapter.mode(fault)?;
            let unavailable = t!("adapter-status", "armed-worker");
            ensure!(
                unavailable["availability"] == "unavailable"
                    && unavailable["stage"] == "adapter"
                    && unavailable["problem"]
                        .as_str()
                        .is_some_and(|s| !s.is_empty()),
                "fault {fault}: {unavailable}"
            );
        }
        fs::rename(endpoint, endpoint.with_extension("offline"))?;
        ensure!(
            t!("adapter-status", "armed-worker")["availability"] == "unavailable",
            "missing adapter"
        );
        let mut current = t!("result", "armed-worker")["integration"].clone();
        ensure!(
            current["heartbeat"]["sequence"] == 2,
            "lost heartbeat sequence"
        );
        let mut retained = retained_integration.clone();
        current
            .as_object_mut()
            .context("integration")?
            .remove("heartbeat");
        retained
            .as_object_mut()
            .context("integration")?
            .remove("heartbeat");
        ensure!(
            current == retained,
            "missing endpoint changed retained integration"
        );
        fs::rename(endpoint.with_extension("offline"), endpoint)?;
        fs::rename(h.root.path().join("fux"), h.root.path().join("offline"))?;
        let unavailable = t!("adapter-status", "armed-worker");
        ensure!(
            unavailable["availability"] == "unavailable" && unavailable["stage"] == "target",
            "missing target probe"
        );
        fs::rename(h.root.path().join("offline"), h.root.path().join("fux"))?;
        ensure!(
            fs::read(&h.journal)? == baseline && h.capture(pane)?["input_sequence"] == 0,
            "probe mutated retained state or input"
        );
        adapter.mode("hello-timeout")?;
        let began = Instant::now();
        let unavailable = t!("adapter-status", "armed-worker");
        ensure!(
            unavailable["availability"] == "unavailable"
                && unavailable["stage"] == "adapter"
                && began.elapsed() < Duration::from_secs(4),
            "probe deadline"
        );
        until(Duration::from_secs(2), || {
            Ok(adapter.done.load(Ordering::SeqCst).then_some(()))
        })?;
        adapter.mode("hello-stall")?;
        let mut probe = h.spawn(&["task", "adapter-status", "armed-worker"])?;
        until(Duration::from_secs(2), || {
            Ok(adapter.seen.load(Ordering::SeqCst).then_some(()))
        })?;
        t!(
            "adopt",
            "probe-concurrent",
            "--title",
            "probe concurrency",
            "--instance",
            &h.instance,
            "--workspace",
            "default",
            "--pane",
            &host.to_string()
        );
        adapter.release.store(true, Ordering::SeqCst);
        let changed = probe.finish(Duration::from_secs(5))?;
        ensure!(
            changed["availability"] == "changed"
                && changed["stage"] == "journal"
                && n(&changed["current_generation"])? > n(&changed["generation"])?,
            "concurrent probe: {changed}"
        );
        t!("forget", "probe-concurrent");
        adapter.mode("ok")?;
        ensure!(
            t!("adapter-status", "armed-worker")["availability"] == "reachable",
            "restored probe"
        );
        for fault in ["wrong", "drop"] {
            adapter.mode(fault)?;
            h.task(&["submit", "armed-input"], false)?;
            let p = named(
                &t!("inspect", "armed-worker")["prompts"],
                "id",
                "armed-input",
            )?;
            ensure!(
                p["arm"]["acknowledged"] == false && h.capture(pane)?["input_sequence"] == 0,
                "unacknowledged arm submitted"
            );
        }
        adapter.mode("ok")?;
        let accepted = t!("submit", "armed-input");
        ensure!(
            accepted["arm"]["acknowledged"] == true && accepted["receipt"]["input_sequence"] == 1,
            "armed submit"
        );
        let arms = adapter.arms()?;
        ensure!(
            arms.len() == 3 && arms.iter().all(|a| a == &arms[0]),
            "arm retry changed identity"
        );
        let saved = fs::read(&h.journal)?;
        for (field, replacement) in [
            ("arm", Value::Null),
            ("acknowledged", json!(false)),
            ("producer", json!("foreign")),
        ] {
            let mut v: Value = serde_json::from_slice(&saved)?;
            if field == "arm" {
                v["prompts"]["armed-input"]["arm"] = replacement;
            } else {
                v["prompts"]["armed-input"]["arm"][field] = replacement;
            }
            h.write(&v)?;
            reject!(strings(&["list"]));
            fs::write(&h.journal, &saved)?;
        }
        let receipt = &accepted["receipt"];
        reject!(
            bind(
                &prompt,
                receipt,
                1,
                "native-message",
                "foreign",
                "native-root"
            )?,
            "armed producer"
        );
        h.owned(
            &bind(
                &prompt,
                receipt,
                1,
                "native-message",
                "socket-producer",
                "native-root",
            )?,
            true,
        )?;
        h.owned(
            &report(
                &prompt,
                receipt,
                2,
                Some("native-message"),
                "socket-producer",
                "native-root",
            )?,
            true,
        )?;
        ensure!(
            t!("wait", "armed-input")["wait"] == "response-observed",
            "armed response"
        );
        let claim = json!({"state":"blocked","operation":"armed-input","input_operation":receipt["operation"],"message":{"session":"native-root","id":"native-message"}});
        let state_args = pulse(&pulse_args, 3, Some(&claim))?;
        let baseline_state = fs::read(&h.journal)?;
        let state_pulse = h.owned(&state_args, true)?;
        ensure!(
            state_pulse["observation"] == claim
                && state_pulse["input_sequence"] == 1
                && h.owned(&state_args, true)? == state_pulse
                && fs::read(&h.journal)? == baseline_state,
            "observation pulse replay"
        );
        let mut wrong = claim.clone();
        wrong["state"] = json!("idle");
        reject!(
            pulse(&pulse_args, 3, Some(&wrong))?,
            "different observation"
        );
        wrong = claim.clone();
        wrong["message"]["session"] = json!("child-session");
        reject!(pulse(&pulse_args, 4, Some(&wrong))?, "identity mismatch");
        wrong = claim.clone();
        wrong["operation"] = json!("missing");
        reject!(pulse(&pulse_args, 4, Some(&wrong))?, "prompt missing");
        let rules = h.root.path().join("rules");
        fs::create_dir(&rules)?;
        fs::write(
            rules.join("test.toml"),
            "id='test'\n[[rules]]\nid='passive-idle'\nstate='idle'\nregion='whole'\ncontains=['ARMED_READY']\nvisible_idle=true\n",
        )?;
        let mut service = h.service(&[
            "--rules",
            rules.to_str().context("rules path")?,
            "--agent",
            "test",
            "serve",
        ])?;
        let dashboard_result = (|| -> Result<()> {
            h.owned(&pulse(&pulse_args, 4, Some(&claim))?, true)?;
            let end = Instant::now() + Duration::from_secs(4);
            let key = format!(":{pane}");
            let row = until(Duration::from_secs(4), || {
                Ok(h.row(&mut service, end, &key, true)?
                    .filter(|v| v["status"] == "blocked"))
            })?;
            ensure!(
                row["evidence"]["source"] == "integration"
                    && row["evidence"]["passive_state"] == "idle"
                    && row["evidence"]["correlated"] == true
                    && row["attention"] == true,
                "native blocker priority: {row}"
            );
            let end = Instant::now() + Duration::from_secs(8);
            let row = until(Duration::from_secs(8), || {
                Ok(h.row(&mut service, end, &key, true)?
                    .filter(|v| v["evidence"]["heartbeat_status"] == "expired"))
            })?;
            ensure!(
                row["status"] == "unknown"
                    && row["attention"] == true
                    && row["evidence"]["passive_state"] == "idle"
                    && row["evidence"]["fresh"] == false,
                "expired native evidence: {row}"
            );
            ensure!(
                fs::read(&h.journal)? == baseline_state,
                "dashboard mutated retained state"
            );
            Ok(())
        })();
        let stopped = stop(&mut service);
        dashboard_result?;
        stopped?;
        ensure!(
            accepted["arm"]["input_started"] == true,
            "input start evidence"
        );
        h.prepare("retire-input", "armed-worker")?;
        adapter.mode("drop")?;
        h.task(&["submit", "retire-input"], false)?;
        h.task(&["abandon", "retire-input"], false)?;
        let pending = named(
            &t!("inspect", "armed-worker")["prompts"],
            "id",
            "retire-input",
        )?;
        ensure!(
            pending["released"] == true
                && pending["wait"] == "cancelled"
                && pending["arm"]["disarm_requested"] == true
                && pending["arm"]["disarmed"] == false
                && pending["arm"]["input_started"] == false
                && pending["delivery"] == "reserved"
                && h.capture(pane)?["input_sequence"] == 1,
            "pending retirement"
        );
        adapter.mode("wrong")?;
        h.task(&["abandon", "retire-input"], false)?;
        let mut v: Value = serde_json::from_slice(&fs::read(&h.journal)?)?;
        v["prompts"]["retire-input"]["receipt"]["expires_server_ms"] = json!(1);
        h.write(&v)?;
        adapter.mode("ok")?;
        let retired = t!("abandon", "retire-input");
        let disarms = adapter.disarms()?;
        ensure!(
            retired["arm"]["disarmed"] == true
                && disarms.len() == 3
                && disarms.iter().all(|d| d == &disarms[0]),
            "retirement proof and identity"
        );
        let count = disarms.len();
        ensure!(
            t!("abandon", "retire-input") == retired
                && t!("reconcile", "retire-input") == retired
                && adapter.disarms()?.len() == count,
            "retirement idempotency"
        );
        reject!(strings(&["submit", "retire-input"]), "cancelled");
        let saved = fs::read(&h.journal)?;
        for (field, replacement) in [("input_started", true), ("disarm_requested", false)] {
            let mut v: Value = serde_json::from_slice(&saved)?;
            v["prompts"]["retire-input"]["arm"][field] = json!(replacement);
            h.write(&v)?;
            reject!(strings(&["list"]));
            fs::write(&h.journal, &saved)?;
        }
        let mut v: Value = serde_json::from_slice(&saved)?;
        v["prompts"]["retire-input"]["receipt"]["bytes_written"] = json!(1);
        h.write(&v)?;
        reject!(strings(&["list"]));
        fs::write(&h.journal, &saved)?;
        h.prepare("attempted-input", "armed-worker")?;
        t!("reserve", "attempted-input");
        let mut v: Value = serde_json::from_slice(&fs::read(&h.journal)?)?;
        let p = &mut v["prompts"]["attempted-input"];
        p["arm"] = json!({"producer":"socket-producer","input_operation":p["receipt"]["operation"],"acknowledged":true,"input_started":true,"disarm_requested":false,"disarmed":false});
        p["delivery"] = json!("submitting");
        h.write(&v)?;
        let attempted = t!("reconcile", "attempted-input");
        ensure!(
            attempted["delivery"] == "reserved" && attempted["arm"]["input_started"] == true,
            "submission history erased"
        );
        t!("abandon", "attempted-input");
        ensure!(
            adapter.disarms()?.len() == count,
            "possibly in-flight arm retired"
        );
        h.prepare("after-retirement", "armed-worker")?;
        let after = t!("submit", "after-retirement");
        ensure!(
            after["arm"]["input_started"] == true && after["receipt"]["input_sequence"] == 2,
            "post-retirement submit"
        );
        t!("abandon", "after-retirement");
        ensure!(adapter.disarms()?.len() == count, "started arm retired");
        h.prepare("group-arm", "armed-worker")?;
        t!(
            "group-create",
            "armed-group",
            "--concurrency",
            "1",
            "--operation",
            "group-arm"
        );
        adapter.mode("drop")?;
        ensure!(
            t!("group-step", "armed-group")["submission"]["status"] == "unresolved",
            "group arming failure"
        );
        let cancelled = t!("group-cancel", "armed-group");
        ensure!(cancelled["state"] == "cancelled", "group cancellation");
        let retained = t!("inspect", "armed-worker");
        let old = named(&retained["prompts"], "id", "armed-input")?;
        ensure!(
            old["wait"] == "cancelled"
                && !old["response"].is_null()
                && retained["task"]["outcome"] == "open",
            "cancel retains historical response"
        );
        ensure!(
            cancelled["retirement"]["status"] == "unresolved"
                && cancelled["retirement_pending_count"] == 1,
            "group retirement pending"
        );
        let saved = fs::read(&h.journal)?;
        let row = h.retirement_row()?;
        ensure!(
            row["status"] == "cancelled"
                && row["attention"] == true
                && row["target"].is_null()
                && row["task_outcome"].is_null()
                && row["evidence"]["retirement_pending_count"] == 1
                && row["evidence"]["retirement_pending"] == json!(["group-arm"])
                && has(&row["detail"], "retry group-cancel"),
            "pending retirement dashboard: {row}"
        );
        ensure!(
            fs::read(&h.journal)? == saved
                && adapter.disarms()?.len() == count + 1
                && h.capture(pane)?["input_sequence"] == 2,
            "dashboard performed retirement or input"
        );
        adapter.mode("ok")?;
        let retired = t!("group-cancel", "armed-group");
        ensure!(
            retired["retirement"]["status"] == "retired"
                && retired["retirement_pending_count"] == 0,
            "group retired"
        );
        let disarms = adapter.disarms()?;
        ensure!(
            disarms.len() == count + 2 && disarms[disarms.len() - 1] == disarms[disarms.len() - 2],
            "group retirement changed identity"
        );
        let row = h.retirement_row()?;
        ensure!(
            row["status"] == "cancelled"
                && row["attention"] == false
                && row["evidence"]["retirement_pending_count"] == 0
                && row["evidence"]["retirement_pending"] == json!([]),
            "retired group dashboard: {row}"
        );
        let count = adapter.disarms()?.len();
        let saved = fs::read(&h.journal)?;
        t!("group-cancel", "armed-group");
        ensure!(
            fs::read(&h.journal)? == saved && adapter.disarms()?.len() == count,
            "group cancel replay"
        );
        reject!(strings(&["submit", "group-arm"]), "cancelled");
        h.prepare("after-group", "armed-worker")?;
        ensure!(
            t!("submit", "after-group")["receipt"]["input_sequence"] == 3,
            "after group input"
        );
        reject!(pulse(&pulse_args, 5, Some(&claim))?, "intervening input");
        let stopped = h.raw(&["stop", "armed-worker"])?;
        ensure!(
            stopped.status.success()
                || String::from_utf8_lossy(&stopped.stderr).contains("stop requested"),
            "stop: {}",
            String::from_utf8_lossy(&stopped.stderr)
        );
        ensure!(
            t!("adapter-status", "armed-worker")["availability"] == "inactive"
                && t!("result", "armed-worker")["integration"]["heartbeat"]["status"] == "inactive",
            "stopped integration active"
        );
        reject!(pulse(&pulse_args, 3, None)?, "not active");
        Ok(())
    })();
    let closed = adapter.close();
    result?;
    closed?;
    Ok(())
}
