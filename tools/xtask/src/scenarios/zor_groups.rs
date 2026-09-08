//! Real durable group orchestration through public zor CLI/API and owned fux.
use crate::support::{
    contention::{RealClock, api_busy, cli_busy, retry_busy},
    local::{Root, completed, until},
    process::{self, Guard, OwnedProcess},
    service,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};
fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(12)
}
fn field<'a>(v: &'a Value, p: &str) -> Result<&'a Value> {
    v.pointer(p).with_context(|| format!("missing {p}"))
}
struct Harness<'a> {
    root: &'a Root,
    zor: &'a Path,
    state: PathBuf,
    journal: PathBuf,
    endpoint: PathBuf,
}
impl Harness<'_> {
    fn raw(&self, args: &[&str], timeout: Duration) -> Result<process::Output> {
        let mut c = self.root.command(self.zor);
        c.arg("--state-directory").arg(&self.state).args(args);
        process::output(c, timeout, 1024 * 1024)
    }
    fn cli(&self, args: &[&str], ok: bool, end: Instant) -> Result<Value> {
        let replay = args == ["dashboard", "--once"]
            || args.first() == Some(&"worktree")
                && matches!(args.get(1), Some(&"create" | &"remove"))
            || args.first() == Some(&"task")
                && matches!(
                    args.get(1),
                    Some(
                        &"start"
                            | &"require-check"
                            | &"require-artifact"
                            | &"prepare"
                            | &"group-create"
                            | &"adopt"
                            | &"abandon"
                            | &"group-inspect"
                            | &"inspect"
                            | &"group-run"
                            | &"submit"
                            | &"reconcile"
                            | &"report"
                            | &"wait"
                            | &"source-collect"
                            | &"check"
                            | &"verify"
                            | &"artifact-inspect"
                            | &"group-cancel"
                    )
                );
        let r = retry_busy(
            |left| self.raw(args, left.min(Duration::from_secs(8))),
            |r| Ok(cli_busy(r.status.code(), &r.stderr)),
            end,
            replay,
            &RealClock,
        )?;
        ensure!(
            r.status.success() == ok,
            "CLI {args:?}: {}",
            String::from_utf8_lossy(&r.stderr)
        );
        if ok {
            Ok(serde_json::from_slice(&r.stdout)?)
        } else {
            Ok(json!(String::from_utf8(r.stderr)?))
        }
    }
    fn reject(&self, args: &[&str], reason: Option<&str>) -> Result<()> {
        let before = fs::read(&self.journal)?;
        let e = self.cli(args, false, deadline())?;
        ensure!(
            fs::read(&self.journal)? == before,
            "rejection mutated journal"
        );
        if let Some(reason) = reason {
            ensure!(
                e.as_str().context("error")?.contains(reason),
                "rejection: {e}"
            );
        }
        Ok(())
    }
    fn git(&self, path: &Path, args: &[&str]) -> Result<()> {
        let mut c = self.root.command(Path::new("/usr/bin/git"));
        c.arg("-C").arg(path).args(args);
        let r = process::output(c, Duration::from_secs(5), 1024 * 1024)?;
        ensure!(
            r.status.success(),
            "fixture git: {}",
            String::from_utf8_lossy(&r.stderr)
        );
        Ok(())
    }
    fn row(&self, name: &str) -> Result<Value> {
        let v = self.cli(&["dashboard", "--once"], true, deadline())?;
        let key = format!("group:{name}");
        let r = v["rows"]
            .as_array()
            .context("rows")?
            .iter()
            .find(|r| r["key"] == key)
            .context("group row")?;
        ensure!(
            r["kind"] == "group"
                && field(r, "/target")?.is_null()
                && field(r, "/task_outcome")?.is_null()
                && r["evidence"]["source"] == "group-journal"
                && field(r, "/evidence/generation")? == field(&v, "/task_generation")?,
            "group row provenance"
        );
        Ok(r.clone())
    }
    fn start(&self, owner: &mut Option<Guard>, previous: Option<&Value>) -> Result<Value> {
        let mut c = self.root.command(self.zor);
        c.arg("--state-directory")
            .arg(&self.state)
            .arg("serve")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .stdin(Stdio::null());
        *owner = Some(Guard(c.spawn()?));
        until(Duration::from_secs(12), || {
            ensure!(
                owner.as_mut().unwrap().0.try_wait()?.is_none(),
                "service exited"
            );
            match service::rpc(
                &self.endpoint,
                &json!({"v":1,"id":1,"op":"ping"}),
                Duration::from_secs(8),
            ) {
                Ok(v) => {
                    let id = field(&v, "/service_instance")?;
                    Ok((Some(id) != previous).then_some(id.clone()))
                }
                Err(e)
                    if e.downcast_ref::<std::io::Error>().is_some_and(|e| {
                        matches!(
                            e.kind(),
                            std::io::ErrorKind::NotFound
                                | std::io::ErrorKind::ConnectionRefused
                                | std::io::ErrorKind::ConnectionReset
                        )
                    }) || e.downcast_ref::<nix::errno::Errno>().is_some_and(|e| {
                        matches!(
                            e,
                            nix::errno::Errno::ENOENT
                                | nix::errno::Errno::ECONNREFUSED
                                | nix::errno::Errno::ECONNRESET
                        )
                    }) =>
                {
                    Ok(None)
                }
                Err(e) => Err(e),
            }
        })
    }
    fn api(
        &self,
        instance: &Value,
        payload: Value,
        id: u64,
        once: bool,
        end: Instant,
    ) -> Result<Option<Value>> {
        let action = payload["action"].as_str().context("action")?;
        ensure!(
            action != "group-step" || once,
            "step requires reconciliation"
        );
        let request = json!({"v":1,"id":id,"op":"task","service_instance":instance,"task":payload});
        let call = |left: Duration| {
            service::rpc(&self.endpoint, &request, left.min(Duration::from_secs(8)))
        };
        let r = if once {
            let r = call(
                end.checked_duration_since(Instant::now())
                    .context("API deadline")?,
            )?;
            if api_busy(&r, &request)? {
                return Ok(None);
            }
            r
        } else {
            retry_busy(
                call,
                |r| api_busy(r, &request),
                end,
                matches!(
                    action,
                    "group-create" | "group-inspect" | "group-run" | "group-pause" | "group-cancel"
                ),
                &RealClock,
            )?
        };
        ensure!(r["status"] == "completed", "task API: {r}");
        Ok(Some(field(&r, "/value")?.clone()))
    }
    fn group(&self, instance: &Value, action: &str, end: Instant) -> Result<Value> {
        self.api(
            instance,
            json!({"action":action,"id":"workers"}),
            2,
            false,
            end,
        )?
        .context("group response")
    }
    fn step(&self, instance: &Value, operation: Option<&str>) -> Result<Value> {
        let end = deadline();
        loop {
            if let Some(v) = self.api(
                instance,
                json!({"action":"group-step","id":"workers"}),
                2,
                true,
                end,
            )? {
                ensure!(
                    v.get("selected").unwrap_or(&Value::Null) == &json!(operation),
                    "selected wrong member: {v}"
                );
                return Ok(v);
            }
            let v = self.group(instance, "group-inspect", end)?;
            if let Some(op) = operation
                && v["members"]
                    .as_array()
                    .context("members")?
                    .iter()
                    .any(|m| m["operation"] == op && m["admitted"] == true)
            {
                self.cli(&["task", "submit", op], true, end)?;
                return self.group(instance, "group-inspect", end);
            }
            ensure!(Instant::now() < end, "step recovery deadline");
            std::thread::sleep(Duration::from_millis(30));
        }
    }
    fn report(&self, name: &str, op: &str) -> Result<()> {
        let p = until(Duration::from_secs(12), || {
            let v = self.cli(&["task", "reconcile", op], true, deadline())?;
            Ok((v["delivery"] == "delivered").then_some(v))
        })?;
        self.cli(
            &[
                "task",
                "report",
                op,
                "--token",
                p["report_token"].as_str().context("token")?,
                "--producer",
                &format!("{name}-fixture"),
                "--sequence",
                "1",
                "--input-operation",
                &field(&p, "/receipt/operation")?.to_string(),
                "--kind",
                "response-observed",
            ],
            true,
            deadline(),
        )?;
        ensure!(
            self.cli(&["task", "wait", op], true, deadline())?["wait"] == "response-observed",
            "report wait"
        );
        Ok(())
    }
    fn stop_task(&self, name: &str) -> Result<()> {
        until(Duration::from_secs(12), || {
            let r = self.raw(&["task", "stop", name], Duration::from_secs(5))?;
            let err = String::from_utf8_lossy(&r.stderr);
            ensure!(
                r.status.success()
                    || err.contains("stop requested")
                    || err.contains("lifecycle uncertain"),
                "task stop: {err}"
            );
            Ok(r.status.success().then_some(()))
        })
    }
}
fn stop(owner: &mut Option<Guard>) -> Result<()> {
    if let Some(mut owner) = owner.take() {
        owner.0.terminate()?;
        ensure!(
            process::wait(&mut owner.0, Duration::from_secs(10))?.success(),
            "service cleanup"
        );
    }
    Ok(())
}
pub fn run(fux: &Path, zor: &Path, automatic: bool) -> Result<()> {
    let root = Root::new("zgroup-rs-", &["/bin/cat".into()])?;
    let mut server = root.server(fux)?;
    let mut service = None;
    let mut background = false;
    let h = Harness {
        root: &root,
        zor,
        state: root.path().join("tasks"),
        journal: root.path().join("tasks/journal.json"),
        endpoint: root.path().join("zor/control.sock"),
    };
    let result = (|| -> Result<()> {
        macro_rules! c{($($a:expr),*$(,)?)=>{h.cli(&[$($a),*],true,deadline())?};}
        let listing = completed(&root.control(), json!({"id":1,"command":"list"}))?;
        let instance = field(&listing, "/instance")?.as_str().context("instance")?;
        let repo = root.path().join("repo");
        fs::create_dir(&repo)?;
        h.git(&repo, &["init", "-b", "main"])?;
        h.git(&repo, &["config", "user.name", "Fixture"])?;
        h.git(&repo, &["config", "user.email", "fixture@example.invalid"])?;
        fs::write(repo.join("base"), "preserve")?;
        h.git(&repo, &["add", "."])?;
        h.git(&repo, &["commit", "-m", "baseline"])?;
        let executable = std::env::current_exe()?;
        let worker = executable.to_str().context("worker")?;
        let mut tasks = std::collections::BTreeMap::new();
        let mut trees = std::collections::BTreeMap::new();
        let mut ops = ["op-alpha", "op-beta", "op-gamma"];
        let check = |name: &str, source: &str| -> Result<Value> {
            c!("task", "source-collect", name, source);
            Ok(c!(
                "task",
                "check",
                name,
                &format!("{source}-check"),
                "--source",
                source,
                "--requirement",
                "content",
                "--artifact",
                &format!("output={source}-output"),
                "--",
                worker,
                "fixture-worker",
                "workflow-check",
                &format!("result-{name}")
            ))
        };
        for (i, name) in ["alpha", "beta", "gamma"].iter().enumerate() {
            let tree = c!(
                "worktree",
                "create",
                name,
                "--repo",
                repo.to_str().context("repo")?,
                "--branch",
                name
            );
            trees.insert(
                *name,
                PathBuf::from(tree["worktree"]["path"].as_str().context("tree")?),
            );
            tasks.insert(
                *name,
                c!(
                    "task",
                    "start",
                    name,
                    "--title",
                    name,
                    "--instance",
                    instance,
                    "--workspace",
                    "default",
                    "--worktree",
                    name,
                    "--",
                    worker,
                    "fixture-worker",
                    "group",
                    root.path()
                        .join(format!("{name}.log"))
                        .to_str()
                        .context("log")?
                ),
            );
            c!(
                "task",
                "require-check",
                name,
                "content",
                "--",
                worker,
                "fixture-worker",
                "workflow-check",
                &format!("result-{name}")
            );
            c!("task", "require-artifact", name, "output", "output.txt");
            c!(
                "task",
                "prepare",
                name,
                "--operation",
                ops[i],
                "--text",
                if *name == "beta" {
                    "wrong-beta"
                } else {
                    if *name == "alpha" {
                        "result-alpha"
                    } else {
                        "result-gamma"
                    }
                },
                "--timeout-ms",
                "60000"
            );
        }
        let group_args = |ops: [&str; 3]| {
            let mut a = vec![
                "task".to_owned(),
                "group-create".into(),
                "workers".into(),
                "--concurrency".into(),
                "2".into(),
            ];
            for op in ops {
                a.extend(["--operation".into(), op.into()]);
            }
            a
        };
        let args = group_args(ops);
        for (extra, reason) in [
            (
                vec!["--after", "op-alpha=gamma", "--after", "op-gamma=alpha"],
                "cycle",
            ),
            (vec!["--operation", "op-alpha"], "duplicate"),
            (vec!["--after", "op-gamma=missing"], "dependency"),
        ] {
            let mut a = args.iter().map(String::as_str).collect::<Vec<_>>();
            a.extend(extra);
            h.reject(&a, Some(reason))?;
        }
        c!(
            "task",
            "group-create",
            "cycle-left",
            "--concurrency",
            "1",
            "--operation",
            "op-alpha",
            "--after",
            "op-alpha=beta"
        );
        h.reject(
            &[
                "task",
                "group-create",
                "cycle-right",
                "--concurrency",
                "1",
                "--operation",
                "op-beta",
                "--after",
                "op-beta=alpha",
            ],
            Some("cycle"),
        )?;
        let alternate = root.path().join("alternate");
        fs::create_dir(&alternate)?;
        std::os::unix::fs::symlink(root.control(), alternate.join("default.sock"))?;
        c!(
            "task",
            "adopt",
            "route-alias",
            "--title",
            "same pane via alias",
            "--runtime",
            alternate.to_str().context("alias")?,
            "--instance",
            instance,
            "--workspace",
            "default",
            "--pane",
            &field(&tasks["alpha"], "/session/target/pane")?.to_string()
        );
        h.reject(
            &[
                "task",
                "prepare",
                "route-alias",
                "--operation",
                "route-work",
                "--text",
                "never",
            ],
            Some("another prompt"),
        )?;
        c!("task", "abandon", "op-alpha");
        c!(
            "task",
            "prepare",
            "route-alias",
            "--operation",
            "route-work",
            "--text",
            "never"
        );
        h.reject(&["task", "submit", "route-work"], Some("not been admitted"))?;
        c!("task", "abandon", "route-work");
        let cancelled = c!("task", "group-cancel", "cycle-left");
        ensure!(
            cancelled["state"] == "cancelled"
                && c!("task", "group-cancel", "cycle-left") == cancelled,
            "group cancellation"
        );
        ops[0] = "op-alpha-retry";
        c!(
            "task",
            "prepare",
            "alpha",
            "--operation",
            ops[0],
            "--text",
            "result-alpha",
            "--timeout-ms",
            "60000"
        );
        let mut args = group_args(ops);
        args.extend(["--after".into(), "op-gamma=alpha".into()]);
        let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
        let created = h.cli(&refs, true, deadline())?;
        ensure!(
            created["active_count"] == 0 && h.cli(&refs, true, deadline())? == created,
            "group create idempotence"
        );
        h.reject(&["task", "reserve", "op-beta"], Some("not been admitted"))?;
        for fault in [
            "admit-dependent",
            "over-capacity",
            "group-limit",
            "member-limit",
            "retirement-cursor",
            "scheduling",
        ] {
            let saved = fs::read(&h.journal)?;
            let mut v: Value = serde_json::from_slice(&saved)?;
            match fault {
                "admit-dependent" => v["groups"]["workers"]["members"][2]["admitted"] = json!(true),
                "over-capacity" => {
                    for m in v["groups"]["workers"]["members"]
                        .as_array_mut()
                        .context("members")?
                    {
                        m["admitted"] = json!(true);
                        m["request"]["after"] = json!([]);
                    }
                }
                "group-limit" => {
                    let original = v["groups"]["workers"].clone();
                    let mut groups = serde_json::Map::new();
                    for i in 0..33 {
                        let id = format!("group-{i}");
                        let mut g = original.clone();
                        g["id"] = json!(id);
                        groups.insert(id, g);
                    }
                    v["groups"] = json!(groups);
                }
                "member-limit" => {
                    let m = v["groups"]["workers"]["members"]
                        .as_array()
                        .context("members")?
                        .clone();
                    v["groups"]["workers"]["members"] =
                        json!(m.iter().cycle().take(m.len() * 3).collect::<Vec<_>>());
                }
                "retirement-cursor" => {
                    v["groups"]["workers"]["retirement_cursor"] = json!("op-beta")
                }
                _ => {
                    v["groups"]["workers"]["automatic"] = json!(true);
                    v["groups"]["workers"]["run_generation"] = json!(0);
                }
            }
            fs::write(&h.journal, serde_json::to_vec(&v)?)?;
            let rejected = h.cli(&["task", "group-inspect", "workers"], false, deadline());
            fs::write(&h.journal, saved)?;
            rejected?;
        }
        if automatic {
            let broken = c!(
                "task",
                "start",
                "broken",
                "--title",
                "broken",
                "--instance",
                instance,
                "--workspace",
                "default",
                "--cwd",
                root.path().to_str().context("root")?,
                "--",
                "/bin/cat"
            );
            c!(
                "task",
                "prepare",
                "broken",
                "--operation",
                "broken-op",
                "--text",
                "never"
            );
            c!(
                "task",
                "group-create",
                "a-broken",
                "--concurrency",
                "1",
                "--operation",
                "broken-op"
            );
            let pane = field(&broken, "/session/target/pane")?;
            completed(
                &root.control(),
                json!({"id":1,"command":"kill","instance":instance,"pane":pane}),
            )?;
            until(Duration::from_secs(12), || {
                let v = completed(&root.control(), json!({"id":1,"command":"list"}))?;
                Ok(v["workspaces"]
                    .as_array()
                    .context("workspaces")?
                    .iter()
                    .flat_map(|w| w["tabs"].as_array().into_iter().flatten())
                    .flat_map(|t| t["panes"].as_array().into_iter().flatten())
                    .all(|p| p.get("id") != Some(pane))
                    .then_some(()))
            })?;
            background = true;
            ensure!(
                c!("task", "group-run", "a-broken")["group"]["automatic"] == true,
                "automatic enable"
            );
            c!("task", "group-run", "workers");
            until(Duration::from_secs(12), || {
                Ok((root.path().join("alpha.log").exists()
                    && root.path().join("beta.log").exists())
                .then_some(()))
            })?;
            let failure = until(Duration::from_secs(12), || {
                let v = c!("task", "group-inspect", "a-broken");
                Ok((v["group"]["automatic"] == false).then_some(v))
            })?;
            let problem = field(&failure, "/group/run_problem")?
                .as_str()
                .context("problem")?;
            ensure!(
                failure["state"] == "needs-attention" && !problem.is_empty(),
                "failed group not paused"
            );
            let row = h.row("a-broken")?;
            ensure!(
                row["status"] == "needs-attention"
                    && row["attention"] == true
                    && row["evidence"]["scheduling"] == "paused"
                    && row["evidence"]["run_problem"] == problem
                    && row["detail"].as_str().context("detail")?.contains(problem),
                "failed group row"
            );
            ensure!(
                field(&c!("task", "inspect", "broken"), "/prompts/0/receipt")?.is_null(),
                "failed target received input"
            );
            c!("task", "group-cancel", "a-broken");
            let row = h.row("a-broken")?;
            ensure!(
                row["status"] == "cancelled" && row["attention"] == false,
                "cancelled row"
            );
            h.stop_task("broken")?;
            c!("shutdown");
            until(Duration::from_secs(12), || {
                Ok((!h.endpoint.exists()).then_some(()))
            })?;
            background = false;
        }
        let mut service_instance = h.start(&mut service, None)?;
        let retry=h.api(&service_instance,json!({"action":"group-create","id":"workers","concurrency":2,"members":ops.iter().enumerate().map(|(i,op)|json!({"operation":op,"after":if i==2{vec!["alpha"]}else{vec![]}})).collect::<Vec<_>>()}),3,false,deadline())?.context("group retry")?;
        if !automatic {
            ensure!(retry["group"] == created["group"], "API group retry");
        }
        if automatic {
            let paused = h.group(&service_instance, "group-pause", deadline())?;
            ensure!(
                paused["group"]["automatic"] == false
                    && h.group(&service_instance, "group-pause", deadline())? == paused,
                "pause idempotence"
            );
            let row = h.row("workers")?;
            ensure!(
                row["status"] == "paused" && row["attention"] == true,
                "paused row"
            );
            ensure!(
                h.group(&service_instance, "group-run", deadline())?["group"]["automatic"] == true,
                "run"
            );
        } else {
            let row = h.row("workers")?;
            ensure!(
                row["status"] == "active"
                    && row["attention"] == false
                    && row["evidence"]["scheduling"] == "manual",
                "manual row"
            );
            ensure!(
                h.step(&service_instance, Some(ops[0]))?["active_count"] == 1,
                "first admission"
            );
        }
        until(Duration::from_secs(12), || {
            Ok(root.path().join("alpha.log").exists().then_some(()))
        })?;
        let child = &mut service.as_mut().unwrap().0;
        child.kill()?;
        use std::os::unix::process::ExitStatusExt;
        ensure!(
            process::wait(child, Duration::from_secs(5))?.signal() == Some(9),
            "service crash"
        );
        service_instance = h.start(&mut service, Some(&service_instance))?;
        let active = if automatic { 2 } else { 1 };
        ensure!(
            h.group(&service_instance, "group-inspect", deadline())?["active_count"] == active,
            "retained admissions"
        );
        let row = h.row("workers")?;
        ensure!(
            row["evidence"]["scheduling"] == if automatic { "automatic" } else { "manual" }
                && row["evidence"]["active_count"] == active
                && row["evidence"]["concurrency"] == 2
                && row["evidence"]["member_count"] == 3,
            "retained dashboard"
        );
        if !automatic {
            h.step(&service_instance, Some(ops[1]))?;
        }
        until(Duration::from_secs(12), || {
            Ok(root.path().join("beta.log").exists().then_some(()))
        })?;
        ensure!(
            !root.path().join("gamma.log").exists(),
            "dependent admitted early"
        );
        for (i, name) in ["alpha", "beta"].iter().enumerate() {
            h.report(name, ops[i])?;
        }
        let before = fs::read(&h.journal)?;
        ensure!(
            h.step(&service_instance, None)?["active_count"] == 2
                && fs::read(&h.journal)? == before
                && !root.path().join("gamma.log").exists(),
            "response released capacity/dependency"
        );
        if automatic {
            let wrong = root.path().join("wrong-tasks");
            use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
            fs::DirBuilder::new().mode(0o700).create(&wrong)?;
            fs::write(wrong.join("journal.json"), fs::read(&h.journal)?)?;
            fs::set_permissions(
                wrong.join("journal.json"),
                fs::Permissions::from_mode(0o600),
            )?;
            let mut c = root.command(zor);
            c.arg("--state-directory")
                .arg(&wrong)
                .args(["task", "group-run", "workers"]);
            let r = process::output(c, Duration::from_secs(8), 1024 * 1024)?;
            ensure!(
                !r.status.success()
                    && String::from_utf8_lossy(&r.stderr)
                        .contains("different task state directory")
                    && fs::read(wrong.join("journal.json"))? == fs::read(&h.journal)?,
                "wrong namespace accepted/mutated"
            );
        }
        ensure!(
            check("alpha", "alpha-source")?["passed"] == true,
            "alpha check"
        );
        if automatic {
            ensure!(
                h.group(&service_instance, "group-pause", deadline())?["group"]["automatic"]
                    == false,
                "pause"
            );
        }
        c!("task", "verify", "alpha", "alpha-source");
        ensure!(
            h.group(&service_instance, "group-inspect", deadline())?["active_count"] == 1,
            "verification capacity"
        );
        if automatic {
            std::thread::sleep(Duration::from_millis(1300));
            ensure!(
                !root.path().join("gamma.log").exists(),
                "paused dependent advanced"
            );
            ensure!(
                c!("task", "group-run", "workers")["group"]["automatic"] == true,
                "resume scheduler"
            );
        } else {
            let replies = std::thread::scope(|scope| {
                let handles = (40..44)
                    .map(|id| {
                        let h = &h;
                        let instance = &service_instance;
                        scope.spawn(move || {
                            h.api(
                                instance,
                                json!({"action":"group-step","id":"workers"}),
                                id,
                                true,
                                deadline(),
                            )
                        })
                    })
                    .collect::<Vec<_>>();
                handles
                    .into_iter()
                    .map(|t| {
                        t.join()
                            .map_err(|_| anyhow::anyhow!("concurrent step panic"))?
                    })
                    .collect::<Result<Vec<_>>>()
            })?;
            for r in replies.into_iter().flatten() {
                ensure!(
                    r.get("selected").is_none_or(|v| v.is_null() || v == ops[2]),
                    "concurrent selection"
                );
            }
            let v = h.group(&service_instance, "group-inspect", deadline())?;
            let m = v["members"]
                .as_array()
                .context("members")?
                .iter()
                .find(|m| m["operation"] == ops[2])
                .context("gamma member")?;
            if m["admitted"] == true {
                c!("task", "submit", ops[2]);
            } else {
                h.step(&service_instance, Some(ops[2]))?;
            }
        }
        until(Duration::from_secs(12), || {
            Ok(root.path().join("gamma.log").exists().then_some(()))
        })?;
        h.report("gamma", ops[2])?;
        ensure!(
            check("beta", "beta-bad")?["passed"] == false,
            "bad beta passed"
        );
        h.reject(&["task", "verify", "beta", "beta-bad"], None)?;
        ensure!(
            h.group(&service_instance, "group-inspect", deadline())?["state"] != "complete",
            "premature completion"
        );
        c!(
            "task",
            "prepare",
            "beta",
            "--operation",
            "beta-repair",
            "--text",
            "result-beta"
        );
        c!("task", "submit", "beta-repair");
        until(Duration::from_secs(12), || {
            Ok(
                (fs::read_to_string(root.path().join("beta.log"))? == "wrong-beta\nresult-beta\n")
                    .then_some(()),
            )
        })?;
        h.report("beta", "beta-repair")?;
        for name in ["beta", "gamma"] {
            let source = format!("{name}-source");
            ensure!(check(name, &source)?["passed"] == true, "repaired check");
            c!("task", "verify", name, &source);
        }
        let done = h.group(&service_instance, "group-inspect", deadline())?;
        ensure!(
            done["state"] == "complete"
                && done["active_count"] == 0
                && done["members"]
                    .as_array()
                    .context("members")?
                    .iter()
                    .all(|m| m["state"] == "verified"),
            "group not verified"
        );
        let before = fs::read(&h.journal)?;
        let row = h.row("workers")?;
        ensure!(
            row["status"] == "complete"
                && row["attention"] == false
                && row["evidence"]["active_count"] == 0
                && fs::read(&h.journal)? == before,
            "read-only completed row"
        );
        for name in ["alpha", "gamma"] {
            ensure!(
                fs::read_to_string(root.path().join(format!("{name}.log")))?
                    == format!("result-{name}\n"),
                "duplicate input"
            );
        }
        h.group(&service_instance, "group-cancel", deadline())?;
        let row = h.row("workers")?;
        ensure!(
            row["status"] == "cancelled" && row["attention"] == false,
            "cancelled row"
        );
        if automatic {
            h.reject(
                &["task", "group-run", "workers"],
                Some("cancelled group cannot run"),
            )?;
        }
        for name in tasks.keys() {
            ensure!(
                c!("task", "inspect", name)["task"]["outcome"] == "verified",
                "cancel rewrote outcome"
            );
        }
        stop(&mut service)?;
        for name in tasks.keys() {
            h.stop_task(name)?;
            c!("worktree", "remove", name);
            ensure!(!trees[name].exists(), "worktree retained");
            let v = c!("task", "artifact-inspect", &format!("{name}-source-output"));
            ensure!(
                serde_json::from_value::<Vec<u8>>(v["artifact"]["bytes"].clone())?
                    == format!("result-{name}").as_bytes(),
                "artifact lost after cleanup"
            );
        }
        ensure!(
            fs::read_to_string(repo.join("base"))? == "preserve",
            "base repository changed"
        );
        Ok(())
    })();
    let service_cleanup = stop(&mut service);
    let background_cleanup = (|| -> Result<()> {
        if background && h.endpoint.exists() {
            h.cli(&["shutdown"], true, deadline())?;
            until(Duration::from_secs(12), || {
                Ok((!h.endpoint.exists()).then_some(()))
            })?;
        }
        Ok(())
    })();
    let server_cleanup = server.finish();
    let errors = [result, service_cleanup, background_cleanup, server_cleanup]
        .into_iter()
        .filter_map(Result::err)
        .map(|e| format!("{e:#}"))
        .collect::<Vec<_>>();
    ensure!(errors.is_empty(), "{}", errors.join("; "));
    println!(
        "PASS durable groups, route-independent admission, verified dependencies, repair, service restart and owned cleanup (automatic={automatic})"
    );
    Ok(())
}
