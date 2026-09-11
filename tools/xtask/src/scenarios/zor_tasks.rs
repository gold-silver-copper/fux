//! Real-pane task coordination, receipt recovery, reports, and bounded waiters.
use crate::support::{
    local::{Root, completed, until},
    process::{self, Guard},
    task_proxy::Proxy,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Seek, SeekFrom},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};
struct Caller {
    child: Guard,
    stdout: fs::File,
    stderr: fs::File,
}
impl Caller {
    fn spawn(mut c: Command) -> Result<Self> {
        let stdout = tempfile::tempfile()?;
        let stderr = tempfile::tempfile()?;
        let child = Guard(
            c.stdin(Stdio::null())
                .stdout(stdout.try_clone()?)
                .stderr(stderr.try_clone()?)
                .spawn()?,
        );
        Ok(Self {
            child,
            stdout,
            stderr,
        })
    }
    fn finish(&mut self, timeout: Duration) -> Result<process::Output> {
        let status = process::wait(&mut self.child.0, timeout)?;
        let read = |f: &mut fs::File| -> Result<Vec<u8>> {
            f.seek(SeekFrom::Start(0))?;
            let mut bytes = Vec::new();
            f.take(1048577).read_to_end(&mut bytes)?;
            ensure!(bytes.len() <= 1048576, "caller output bound");
            Ok(bytes)
        };
        Ok(process::Output {
            status,
            stdout: read(&mut self.stdout)?,
            stderr: read(&mut self.stderr)?,
        })
    }
}
struct Harness<'a> {
    root: &'a Root,
    zor: &'a Path,
    instance: String,
    pane: Value,
    state: PathBuf,
}
impl Harness<'_> {
    fn command(&self, args: &[&str]) -> Command {
        let mut c = self.root.command(self.zor);
        c.env("XDG_STATE_HOME", &self.state).arg("task").args(args);
        c
    }
    fn raw(&self, args: &[&str]) -> Result<process::Output> {
        process::output(self.command(args), Duration::from_secs(6), 1048576)
    }
    fn task(&self, args: &[&str], ok: bool) -> Result<Value> {
        let r = self.raw(args)?;
        ensure!(
            r.status.success() == ok,
            "task {args:?}: stdout={} stderr={}",
            String::from_utf8_lossy(&r.stdout),
            String::from_utf8_lossy(&r.stderr)
        );
        if ok {
            Ok(serde_json::from_slice(&r.stdout)?)
        } else {
            Ok(json!(String::from_utf8(r.stderr)?))
        }
    }
    fn adopt(&self, name: &str, instance: &str, runtime: Option<&Path>, ok: bool) -> Result<Value> {
        let pane = self.pane["id"].to_string();
        let mut args = vec![
            "adopt",
            name,
            "--title",
            "durable fixture",
            "--instance",
            instance,
            "--workspace",
            "default",
            "--pane",
            &pane,
        ];
        if let Some(p) = runtime {
            args.extend(["--runtime", p.to_str().context("runtime")?]);
        }
        self.task(&args, ok)
    }
    fn capture(&self) -> Result<Value> {
        completed(
            &self.root.control(),
            json!({"command":"capture","id":1,"instance":self.instance,"pane":self.pane["id"],"max_bytes":4096}),
        )
    }
    fn delivered(&self, op: &str) -> Result<Value> {
        self.task(&["submit", op], true)?;
        until(Duration::from_secs(5), || {
            let v = self.task(&["reconcile", op], true)?;
            Ok((v["delivery"] == "delivered").then_some(v))
        })
    }
    fn report(
        &self,
        op: &str,
        token: &Value,
        input: &Value,
        kind: &str,
        ok: bool,
    ) -> Result<Value> {
        self.task(
            &[
                "report",
                op,
                "--token",
                token.as_str().context("report token")?,
                "--producer",
                "fixture-lifetime-1",
                "--sequence",
                "1",
                "--input-operation",
                &input.to_string(),
                "--kind",
                kind,
            ],
            ok,
        )
    }
    // Only the original report/cancel operations are replayed after an explicit busy refusal.
    fn available(&self, args: &[&str]) -> Result<Value> {
        ensure!(
            matches!(args.first(), Some(&"report" | &"cancel")),
            "unreviewed retry"
        );
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            let r = self.raw(args)?;
            if r.status.success() {
                return Ok(serde_json::from_slice(&r.stdout)?);
            }
            ensure!(
                String::from_utf8_lossy(&r.stderr).contains("journal is busy")
                    && Instant::now() < end,
                "busy retry: {}",
                String::from_utf8_lossy(&r.stderr)
            );
            std::thread::sleep(Duration::from_millis(30));
        }
    }
    fn waiter(&self, op: &str) -> Result<Caller> {
        let mut c = Caller::spawn(self.command(&["wait", op, "--follow", "--timeout-ms", "5000"]))?;
        let path = self.state.join("zor/waiter-0.lock");
        until(Duration::from_secs(3), || {
            ensure!(
                c.child.0.try_wait()?.is_none(),
                "waiter exited before admission"
            );
            if path.exists() {
                let file = fs::OpenOptions::new().read(true).write(true).open(&path)?;
                match nix::fcntl::Flock::lock(file, nix::fcntl::FlockArg::LockExclusiveNonblock) {
                    Ok(lock) => drop(lock),
                    Err((_, nix::errno::Errno::EWOULDBLOCK)) => return Ok(Some(())),
                    Err((_, e)) => return Err(e.into()),
                }
            }
            Ok(None)
        })?;
        Ok(c)
    }
}
pub fn run(fux: &Path, zor: &Path) -> Result<()> {
    let root = Root::new(
        "ztask-rs-",
        &[
            "/bin/sh".into(),
            "-c".into(),
            "stty raw -echo; printf READY; cat".into(),
        ],
    )?;
    let mut server = root.server(fux)?;
    let listing = completed(&root.control(), json!({"command":"list","id":1}))?;
    let instance = listing["instance"].as_str().context("instance")?.to_owned();
    let pane = listing
        .pointer("/workspaces/0/tabs/0/panes/0")
        .context("pane")?
        .clone();
    let mut h = Harness {
        root: &root,
        zor,
        instance: instance.clone(),
        pane: pane.clone(),
        state: root.path().join("state"),
    };
    let mut proxy = None;
    let result = (|| -> Result<()> {
        macro_rules! t { ($($a:expr),* $(,)?) => { h.task(&[$($a),*], true)? }; }
        macro_rules! reject { ($($a:expr),* $(,)?) => { h.task(&[$($a),*], false)? }; }
        let first = h.adopt("one", &instance, None, true)?;
        ensure!(
            first["session"]["ownership"] == "adopted" && first["task"]["outcome"] == "open",
            "adoption"
        );
        ensure!(
            first["session"]["target"]["pid"] == pane["pid"],
            "adopted PID"
        );
        ensure!(
            h.adopt("one", &instance, None, true)? == first,
            "adoption replay"
        );
        fs::rename(root.path().join("fux"), root.path().join("fux-offline"))?;
        let offline = (|| -> Result<()> {
            ensure!(
                h.adopt("one", &instance, None, true)? == first,
                "offline retained adoption"
            );
            h.adopt("new-offline", &instance, None, false)?;
            Ok(())
        })();
        fs::rename(root.path().join("fux-offline"), root.path().join("fux"))?;
        offline?;
        h.adopt("one", "stale", None, false)?;
        h.adopt("stale-new", "stale", None, false)?;
        let second = h.adopt("two", &instance, None, true)?;
        ensure!(
            second["session"]["id"] == first["session"]["id"]
                && second["attempt"]["id"] != first["attempt"]["id"],
            "shared session, distinct attempts"
        );
        let before = h.capture()?;
        let prepared = t!(
            "prepare",
            "one",
            "--operation",
            "op1",
            "--text",
            "do not submit yet"
        );
        ensure!(
            prepared["delivery"] == "prepared"
                && prepared.get("receipt") == Some(&Value::Null)
                && prepared["wait"] == "pending",
            "preparation"
        );
        ensure!(
            t!(
                "prepare",
                "one",
                "--operation",
                "op1",
                "--text",
                "do not submit yet"
            ) == prepared,
            "prepare replay"
        );
        reject!(
            "prepare",
            "one",
            "--operation",
            "op1",
            "--text",
            "changed intent"
        );
        reject!(
            "prepare",
            "two",
            "--operation",
            "op2",
            "--text",
            "competing writer"
        );
        reject!("forget", "one");
        ensure!(
            t!("inspect", "one")["prompts"][0] == prepared,
            "persisted prepare"
        );
        ensure!(
            h.capture()?["input_sequence"] == before["input_sequence"]
                && before["input_sequence"] == 0,
            "prepare typed input"
        );
        ensure!(
            !h.capture()?["text"]
                .as_str()
                .context("capture")?
                .contains("do not submit"),
            "prepared text typed"
        );
        for _ in 0..2 {
            ensure!(
                t!("discard-prepared", "op1")["wait"] == "cancelled",
                "discard replay"
            );
        }
        ensure!(
            t!(
                "prepare",
                "one",
                "--operation",
                "op1",
                "--text",
                "do not submit yet"
            )["wait"]
                == "cancelled",
            "discarded prepare replay"
        );
        let mut callers = Vec::new();
        for (owner, op) in [("one", "op3"), ("two", "op4")] {
            callers.push(Caller::spawn(h.command(&[
                "prepare",
                owner,
                "--operation",
                op,
                "--text",
                "concurrent intent",
            ]))?);
        }
        let mut successes = Vec::<Value>::new();
        for c in &mut callers {
            let r = c.finish(Duration::from_secs(6))?;
            if r.status.success() {
                successes.push(serde_json::from_slice(&r.stdout)?);
            } else {
                let s = String::from_utf8_lossy(&r.stderr);
                ensure!(
                    s.contains("pending") || s.contains("journal is busy"),
                    "concurrent preparation: {s}"
                );
            }
        }
        ensure!(
            successes.len() == 1,
            "concurrent writers admitted {}",
            successes.len()
        );
        t!(
            "discard-prepared",
            successes[0]["id"].as_str().context("operation ID")?
        );
        let journal = h.state.join("zor/journal.json");
        ensure!(
            fs::metadata(&journal)?.permissions().mode() & 0o777 == 0o600
                && fs::metadata(journal.parent().unwrap())?
                    .permissions()
                    .mode()
                    & 0o777
                    == 0o700,
            "journal permissions"
        );
        let saved = fs::read(&journal)?;
        fs::write(&journal, b"corrupt committed state")?;
        reject!("list");
        ensure!(
            fs::read(&journal)? == b"corrupt committed state",
            "corrupt journal overwritten"
        );
        fs::write(&journal, saved)?;
        ensure!(
            t!("list")["tasks"].as_array().context("tasks")?.len() == 2,
            "retained tasks"
        );
        ensure!(t!("forget", "one")["forgotten"] == true, "forget one");
        ensure!(
            t!("list")["tasks"].as_array().context("tasks")?.len() == 1,
            "remaining task"
        );
        ensure!(
            t!("inspect", "two")["session"]["id"] == second["session"]["id"],
            "shared session survived"
        );
        ensure!(
            t!("forget", "two")["forgotten"] == true && t!("forget", "two")["forgotten"] == false,
            "forget replay"
        );
        let final_state: Value = serde_json::from_slice(&fs::read(&journal)?)?;
        for field in ["tasks", "attempts", "sessions", "prompts"] {
            ensure!(
                final_state[field]
                    .as_object()
                    .context("journal map")?
                    .is_empty(),
                "orphaned {field}"
            );
        }
        let after = completed(&root.control(), json!({"command":"list","id":2}))?;
        ensure!(
            after["workspaces"][0]["tabs"][0]["panes"][0]["pid"] == pane["pid"]
                && h.capture()?["input_sequence"] == 0,
            "forget touched pane"
        );
        let proxy_root = root.path().join("proxy");
        fs::create_dir(&proxy_root)?;
        proxy = Some(Proxy::start(&proxy_root, &root.control())?);
        let p = proxy.as_ref().unwrap();
        h.adopt("delivery", &instance, Some(&proxy_root), true)?;
        t!(
            "prepare",
            "delivery",
            "--operation",
            "send",
            "--text",
            r"literal\n",
            "--timeout-ms",
            "60000"
        );
        reject!("reserve", "send");
        let unrecorded = t!("inspect", "delivery")["prompts"][0].clone();
        ensure!(
            unrecorded["delivery"] == "prepared" && unrecorded.get("receipt") == Some(&Value::Null),
            "lost reservation reply"
        );
        ensure!(
            p.orphaned().len() == 1 && h.capture()?["input_sequence"] == 0,
            "orphan reservation"
        );
        let reserved = t!("reserve", "send");
        ensure!(
            reserved["receipt"]["operation"] != p.orphaned()[0]
                && reserved["delivery"] == "reserved"
                && reserved["receipt"]["bytes_written"] == 0,
            "fresh unsent reservation"
        );
        ensure!(
            h.capture()?["input_sequence"] == 0
                && t!("reconcile", "send")["delivery"] == "reserved",
            "reserve does not type"
        );
        reject!("submit", "send");
        let pending = t!("inspect", "delivery")["prompts"][0].clone();
        ensure!(
            pending["delivery"] == "uncertain" && pending["receipt"] == reserved["receipt"],
            "pre-forward loss"
        );
        let actual = completed(
            &root.control(),
            json!({"command":"input-status","id":1,"instance":instance,"operation":reserved["receipt"]["operation"]}),
        )?;
        ensure!(
            actual["receipt"]["state"] == "reserved"
                && actual["receipt"]["bytes_written"] == 0
                && h.capture()?["input_sequence"] == 0,
            "unsent reservation"
        );
        reject!("submit", "send");
        let uncertain = t!("inspect", "delivery")["prompts"][0].clone();
        ensure!(
            uncertain["delivery"] == "uncertain"
                && uncertain["receipt"]["operation"] == reserved["receipt"]["operation"],
            "lost submit receipt"
        );
        p.update(|f| f.expire_status = true);
        reject!("submit", "send");
        ensure!(
            h.capture()?["input_sequence"] == 1,
            "ambiguous retry reserved fresh input"
        );
        p.update(|f| f.expire_status = false);
        let delivered = until(Duration::from_secs(5), || {
            let v = t!("reconcile", "send");
            Ok((v["delivery"] == "delivered").then_some(v))
        })?;
        ensure!(
            delivered["receipt"]["bytes_written"] == r"literal\n".len() + 1
                && delivered["wait"] == "pending",
            "PTY delivery is not task completion"
        );
        for _ in 0..3 {
            ensure!(t!("submit", "send") == delivered, "submit replay");
        }
        ensure!(h.capture()?["input_sequence"] == 1, "repeated input");
        reject!(
            "prepare",
            "delivery",
            "--operation",
            "competing",
            "--text",
            "second"
        );
        reject!("forget", "delivery");
        h.state = root.path().join("interference-state");
        h.adopt("human", &instance, None, true)?;
        t!(
            "prepare",
            "human",
            "--operation",
            "conflict",
            "--text",
            "must not arrive"
        );
        t!("reserve", "conflict");
        completed(
            &root.control(),
            json!({"command":"send-keys","id":1,"instance":instance,"pane":pane["id"],"keys":"H"}),
        )?;
        for _ in 0..2 {
            reject!("submit", "conflict");
            ensure!(
                h.capture()?["input_sequence"] == 2,
                "human interference retry"
            );
        }
        ensure!(
            !h.capture()?["text"]
                .as_str()
                .context("text")?
                .contains("must not arrive"),
            "conflicting text typed"
        );
        reject!("wait", "conflict");
        h.state = root.path().join("deadline-state");
        h.adopt("deadline", &instance, Some(&proxy_root), true)?;
        t!(
            "prepare",
            "deadline",
            "--operation",
            "late",
            "--text",
            "too late",
            "--timeout-ms",
            "500"
        );
        t!("reserve", "late");
        p.update(|f| f.delay_list = Duration::from_millis(700));
        reject!("submit", "late");
        p.update(|f| f.delay_list = Duration::ZERO);
        ensure!(
            h.capture()?["input_sequence"] == 2,
            "slow target check bypassed deadline"
        );
        ensure!(
            t!("reconcile", "late")["delivery"] == "reserved",
            "late reservation"
        );
        reject!("submit", "late");
        ensure!(h.capture()?["input_sequence"] == 2, "late submission");
        reject!(
            "prepare",
            "deadline",
            "--operation",
            "bad",
            "--text",
            "first\nsecond"
        );
        p.update(|f| f.expire_status = true);
        reject!("reconcile", "late");
        let released = t!("abandon", "late");
        ensure!(
            released["released"] == true
                && released["delivery"] == "uncertain"
                && released["wait"] == "cancelled",
            "abandon uncertain"
        );
        let generation = t!("inspect", "deadline")["generation"].clone();
        ensure!(
            t!("abandon", "late") == released
                && t!("inspect", "deadline")["generation"] == generation,
            "abandon replay"
        );
        t!(
            "prepare",
            "deadline",
            "--operation",
            "after-release",
            "--text",
            "new intent"
        );
        reject!("reconcile", "late");
        ensure!(
            t!("inspect", "deadline")["prompts"][1]["released"] == true,
            "release retained"
        );
        p.update(|f| f.expire_status = false);
        let reconciled = t!("reconcile", "late");
        ensure!(
            reconciled["delivery"] == "reserved"
                && reconciled["released"] == true
                && reconciled["wait"] == "cancelled",
            "receipt refresh reactivated coordination"
        );
        reject!("submit", "late");
        let cancelled = t!("cancel", "deadline");
        ensure!(
            cancelled["task"]["outcome"] == "cancelled"
                && cancelled["prompts"]
                    .as_array()
                    .context("prompts")?
                    .iter()
                    .all(|v| v["released"] == true && v["wait"] == "cancelled"),
            "cancel releases all prompts"
        );
        ensure!(t!("cancel", "deadline") == cancelled, "cancel replay");
        let cancelled_path = h.state.join("zor/journal.json");
        let committed = fs::read(&cancelled_path)?;
        let mut invalid: Value = serde_json::from_slice(&committed)?;
        invalid["prompts"]["after-release"]["released"] = json!(false);
        fs::write(&cancelled_path, serde_json::to_vec(&invalid)?)?;
        reject!("list");
        let retained: Value = serde_json::from_slice(&fs::read(&cancelled_path)?)?;
        ensure!(
            retained["prompts"]["after-release"]["released"] == false,
            "invalid journal rewritten"
        );
        fs::write(&cancelled_path, committed)?;
        reject!("submit", "after-release");
        reject!(
            "prepare",
            "deadline",
            "--operation",
            "after-cancel",
            "--text",
            "no"
        );
        reject!("forget", "deadline");
        ensure!(
            h.capture()?["input_sequence"] == 2,
            "cancel/abandon typed control characters"
        );
        h.adopt("new-task", &instance, None, true)?;
        t!(
            "prepare",
            "new-task",
            "--operation",
            "new-task-op",
            "--text",
            "new task intent"
        );
        t!("cancel", "new-task");
        ensure!(
            t!("forget", "new-task")["forgotten"] == true,
            "forget cancelled prepared task"
        );
        h.state = root.path().join("crash-state");
        h.adopt("crash", &instance, Some(&proxy_root), true)?;
        t!(
            "prepare",
            "crash",
            "--operation",
            "crashed",
            "--text",
            "one crash input"
        );
        p.update(|f| f.hold_submit = true);
        let mut caller = Caller::spawn(h.command(&["submit", "crashed"]))?;
        until(Duration::from_secs(5), || Ok(p.accepted().then_some(())))?;
        caller.child.0.kill()?;
        caller.finish(Duration::from_secs(3))?;
        p.release();
        p.update(|f| f.hold_submit = false);
        ensure!(
            t!("inspect", "crash")["prompts"][0]["delivery"] == "submitting",
            "crash persisted submitting"
        );
        until(Duration::from_secs(5), || {
            Ok((t!("reconcile", "crashed")["delivery"] == "delivered").then_some(()))
        })?;
        ensure!(
            t!("submit", "crashed")["delivery"] == "delivered"
                && h.capture()?["input_sequence"] == 3,
            "crash repeated input"
        );
        let receipt_before = t!("inspect", "crash")["prompts"][0]["receipt"].clone();
        let released = t!("abandon", "crashed");
        ensure!(
            released["delivery"] == "delivered" && released["receipt"] == receipt_before,
            "abandon retained delivery"
        );
        reject!("submit", "crashed");
        ensure!(
            t!("reconcile", "crashed") == released && h.capture()?["input_sequence"] == 3,
            "released reconcile"
        );
        let listing = completed(&root.control(), json!({"command":"list","id":1}))?;
        ensure!(
            listing["workspaces"][0]["tabs"][0]["panes"][0]["pid"] == pane["pid"],
            "original process changed"
        );
        let fresh = t!(
            "prepare",
            "crash",
            "--operation",
            "fresh",
            "--text",
            "READY blocked idle"
        );
        h.report(
            "fresh",
            &fresh["report_token"],
            &json!(1),
            "response-observed",
            false,
        )?;
        reject!("wait", "fresh");
        let fresh = h.delivered("fresh")?;
        ensure!(
            t!("wait", "fresh")["wait"] == "pending",
            "old/echoed text is not fresh evidence"
        );
        h.report(
            "fresh",
            &json!("wrong-token"),
            &fresh["receipt"]["operation"],
            "response-observed",
            false,
        )?;
        h.report(
            "fresh",
            &fresh["report_token"],
            &json!(
                fresh["receipt"]["operation"]
                    .as_u64()
                    .context("receipt operation")?
                    + 1
            ),
            "response-observed",
            false,
        )?;
        let first_report = h.report(
            "fresh",
            &fresh["report_token"],
            &fresh["receipt"]["operation"],
            "response-observed",
            true,
        )?;
        ensure!(
            h.report(
                "fresh",
                &fresh["report_token"],
                &fresh["receipt"]["operation"],
                "response-observed",
                true
            )? == first_report,
            "report replay"
        );
        h.report(
            "fresh",
            &fresh["report_token"],
            &fresh["receipt"]["operation"],
            "needs-input",
            false,
        )?;
        ensure!(
            t!("wait", "fresh")["wait"] == "response-observed",
            "fast report requires no working phase"
        );
        ensure!(
            t!("inspect", "crash")["task"]["outcome"] == "open",
            "report verified task"
        );
        t!(
            "prepare",
            "crash",
            "--operation",
            "next",
            "--text",
            "next prompt"
        );
        let next = h.delivered("next")?;
        h.report(
            "next",
            &fresh["report_token"],
            &next["receipt"]["operation"],
            "response-observed",
            false,
        )?;
        ensure!(
            t!("wait", "next")["wait"] == "pending",
            "previous report satisfied next prompt"
        );
        h.report(
            "next",
            &next["report_token"],
            &next["receipt"]["operation"],
            "needs-input",
            true,
        )?;
        completed(
            &root.control(),
            json!({"command":"send-keys","id":1,"instance":instance,"pane":pane["id"],"keys":"H"}),
        )?;
        ensure!(
            t!("wait", "next")["wait"] == "uncertain",
            "human input must weaken correlation"
        );
        t!("abandon", "next");
        ensure!(t!("wait", "next")["wait"] == "cancelled", "abandoned wait");
        t!(
            "prepare",
            "crash",
            "--operation",
            "timeout",
            "--text",
            "silent request",
            "--timeout-ms",
            "500"
        );
        p.update(|f| f.hold_receipt = true);
        ensure!(
            t!("submit", "timeout")["delivery"] == "queued",
            "queued synthetic receipt"
        );
        std::thread::sleep(Duration::from_millis(600));
        ensure!(
            t!("wait", "timeout")["wait"] == "timed-out"
                && t!("reconcile", "timeout")["wait"] == "timed-out",
            "prompt deadline"
        );
        p.update(|f| f.expire_status = true);
        reject!("reconcile", "timeout");
        ensure!(
            t!("wait", "timeout")["wait"] == "timed-out",
            "receipt loss erased terminal evidence"
        );
        p.update(|f| {
            f.expire_status = false;
            f.hold_receipt = false;
        });
        ensure!(
            t!("reconcile", "timeout")["wait"] == "timed-out",
            "late receipt erased timeout"
        );
        reject!(
            "prepare",
            "crash",
            "--operation",
            "after-timeout",
            "--text",
            "no implicit release"
        );
        t!("abandon", "timeout");
        let created = completed(
            &root.control(),
            json!({"command":"split","axis":"horizontal","id":1,"instance":instance,"argv":["/bin/sh","-c","read line; exit 17"],"final_retain_ms":60000}),
        )?;
        t!(
            "adopt",
            "exiting",
            "--title",
            "exit fixture",
            "--instance",
            &instance,
            "--workspace",
            "default",
            "--pane",
            &created["pane"].to_string()
        );
        t!("prepare", "exiting", "--operation", "exit", "--text", "go");
        t!("submit", "exit");
        let exited = until(Duration::from_secs(5), || {
            let v = t!("wait", "exit");
            Ok((v["wait"] == "process-exited").then_some(v))
        })?;
        ensure!(
            exited["wait_exit_status"] == 17
                && t!("inspect", "exiting")["task"]["outcome"] == "open",
            "exit is not verified success"
        );
        t!(
            "prepare",
            "crash",
            "--operation",
            "follow",
            "--text",
            "follow response",
            "--timeout-ms",
            "60000"
        );
        let following = h.delivered("follow")?;
        let sequence_before = h.capture()?["input_sequence"].clone();
        let short = t!("wait", "follow", "--follow", "--timeout-ms", "200");
        ensure!(
            short["stop"] == "caller-deadline"
                && short["prompt"]["wait"] == "pending"
                && t!("wait", "follow")["wait"] == "pending",
            "caller deadline became prompt deadline"
        );
        let before_budget = t!("inspect", "crash");
        p.update(|f| {
            f.delay_list = Duration::from_millis(400);
            f.drop_delayed = true;
        });
        ensure!(
            t!("wait", "follow", "--follow", "--timeout-ms", "100")["stop"] == "caller-deadline",
            "capture budget"
        );
        ensure!(
            t!("inspect", "crash") == before_budget,
            "capture timeout changed coordination"
        );
        p.update(|f| {
            f.delay_list = Duration::ZERO;
            f.drop_delayed = false;
        });
        let mut waiter = h.waiter("follow")?;
        h.available(&[
            "report",
            "follow",
            "--token",
            following["report_token"].as_str().context("token")?,
            "--producer",
            "follow-fixture",
            "--sequence",
            "1",
            "--input-operation",
            &following["receipt"]["operation"].to_string(),
            "--kind",
            "response-observed",
        ])?;
        let r = waiter.finish(Duration::from_secs(5))?;
        ensure!(
            r.status.success(),
            "waiter: {}",
            String::from_utf8_lossy(&r.stderr)
        );
        let v: Value = serde_json::from_slice(&r.stdout)?;
        ensure!(
            v["stop"] == "terminal"
                && v["prompt"]["wait"] == "response-observed"
                && h.capture()?["input_sequence"] == sequence_before,
            "follow response mutated input"
        );
        t!(
            "prepare",
            "crash",
            "--operation",
            "cancel-follow",
            "--text",
            "cancel waiting",
            "--timeout-ms",
            "60000"
        );
        h.delivered("cancel-follow")?;
        let mut waiter = h.waiter("cancel-follow")?;
        h.available(&["cancel", "crash"])?;
        let r = waiter.finish(Duration::from_secs(5))?;
        ensure!(
            r.status.success(),
            "cancel waiter: {}",
            String::from_utf8_lossy(&r.stderr)
        );
        let v: Value = serde_json::from_slice(&r.stdout)?;
        ensure!(v["prompt"]["wait"] == "cancelled", "follow cancellation");
        h.adopt("wait-crash", &instance, Some(&proxy_root), true)?;
        t!(
            "prepare",
            "wait-crash",
            "--operation",
            "wait-crash-op",
            "--text",
            "still running",
            "--timeout-ms",
            "60000"
        );
        h.delivered("wait-crash-op")?;
        p.reset_hold();
        p.update(|f| f.hold_list = true);
        let mut waiter = h.waiter("wait-crash-op")?;
        until(Duration::from_secs(5), || Ok(p.accepted().then_some(())))?;

        waiter.child.0.kill()?;
        waiter.finish(Duration::from_secs(3))?;
        p.release();
        p.update(|f| f.hold_list = false);
        let resumed_wait = t!("wait", "wait-crash-op", "--follow", "--timeout-ms", "100");
        let retained_task = t!("inspect", "wait-crash");
        ensure!(
            resumed_wait["stop"] == "caller-deadline" && retained_task["task"]["outcome"] == "open",
            "waiter crash leaked admission or changed task: wait={resumed_wait}, task={retained_task}"
        );
        t!("abandon", "wait-crash-op");
        t!(
            "prepare",
            "wait-crash",
            "--operation",
            "status-budget",
            "--text",
            "budget evidence",
            "--timeout-ms",
            "60000"
        );
        p.update(|f| f.hold_receipt = true);
        ensure!(
            t!("submit", "status-budget")["delivery"] == "queued",
            "status budget queued receipt"
        );
        p.update(|f| f.hold_receipt = false);
        let before_budget = t!("inspect", "wait-crash");
        p.update(|f| {
            f.delay_status = Duration::from_millis(400);
            f.drop_delayed = true;
        });
        ensure!(
            t!("wait", "status-budget", "--follow", "--timeout-ms", "100")["stop"]
                == "caller-deadline",
            "status budget"
        );
        ensure!(
            t!("inspect", "wait-crash") == before_budget,
            "caller timeout downgraded delivery evidence"
        );
        p.update(|f| {
            f.delay_status = Duration::ZERO;
            f.drop_delayed = false;
        });
        t!("abandon", "status-budget");
        Ok(())
    })();
    let proxy_result = proxy.as_mut().map_or(Ok(()), Proxy::close);
    let server_result = server.finish();
    if let Err(e) = &proxy_result {
        eprintln!("task proxy: {e:#}");
    }
    result?;
    proxy_result?;
    server_result?;
    println!(
        "PASS durable adoption, preparation, receipt recovery, retry deduplication and human interference"
    );
    Ok(())
}
