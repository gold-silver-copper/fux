//! Explicit check/artifact evidence, bounded journals, no replay and cancellation.
use crate::support::{
    local::{Root, completed, until},
    process::{self, Guard},
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
    process::Stdio,
    time::{Duration, Instant},
};
fn s(p: &Path) -> Result<&str> {
    p.to_str().context("fixture path")
}
fn num(v: &Value) -> Result<u64> {
    v.as_u64().context("integer")
}
fn array(v: &Value) -> Result<&Vec<Value>> {
    v.as_array().context("array")
}
fn contains(v: &Value, text: &str) -> bool {
    v.as_str().is_some_and(|s| s.contains(text))
}
fn named<'a>(v: &'a Value, name: &str) -> Result<&'a Value> {
    array(v)?
        .iter()
        .find(|v| v["name"] == name)
        .context("named requirement")
}
fn touch(p: &Path) -> Result<()> {
    fs::OpenOptions::new().create(true).append(true).open(p)?;
    Ok(())
}
fn injected<T>(path: &Path, value: &Value, check: impl FnOnce() -> Result<T>) -> Result<T> {
    let saved = fs::read(path)?;
    fs::write(path, serde_json::to_vec(value)?)?;
    let result = check();
    let restore = fs::write(path, saved);
    result.and_then(|v| {
        restore?;
        Ok(v)
    })
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
            let mut bytes = Vec::new();
            f.take(1048577).read_to_end(&mut bytes)?;
            ensure!(bytes.len() <= 1048576, "caller output bound");
            Ok(bytes)
        };
        let stdout = read(&mut self.out)?;
        let stderr = read(&mut self.err)?;
        ensure!(
            status.success(),
            "caller: {}",
            String::from_utf8_lossy(&stderr)
        );
        Ok(serde_json::from_slice(&stdout)?)
    }
    fn kill(&mut self) -> Result<()> {
        if self.child.0.try_wait()?.is_none() {
            self.child.0.kill()?;
        }
        process::wait(&mut self.child.0, Duration::from_secs(3))?;
        Ok(())
    }
}
struct Harness<'a> {
    root: &'a Root,
    zor: &'a Path,
}
impl Harness<'_> {
    fn raw(&self, args: &[&str], timeout: Duration) -> Result<process::Output> {
        let mut c = self.root.command(self.zor);
        c.args(args);
        process::output(c, timeout, 1048576)
    }
    fn cli(&self, args: &[&str], ok: bool) -> Result<Value> {
        let r = self.raw(args, Duration::from_secs(12))?;
        ensure!(
            r.status.success() == ok,
            "{args:?}: stdout={} stderr={}",
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
        self.cli(&args.iter().map(String::as_str).collect::<Vec<_>>(), ok)
    }
    fn spawn(&self, args: &[String]) -> Result<Caller> {
        let out = tempfile::tempfile()?;
        let err = tempfile::tempfile()?;
        let mut c = self.root.command(self.zor);
        c.args(args)
            .stdin(Stdio::null())
            .stdout(out.try_clone()?)
            .stderr(err.try_clone()?);
        Ok(Caller {
            child: Guard(c.spawn()?),
            out,
            err,
        })
    }
    fn check(
        &self,
        name: &str,
        command: &[&str],
        timeout: u64,
        requirement: Option<&str>,
    ) -> Vec<String> {
        let mut args = vec![
            "task".into(),
            "check".into(),
            "worker".into(),
            name.into(),
            "--timeout-ms".into(),
            timeout.to_string(),
        ];
        if let Some(r) = requirement.or_else(|| matches!(name, "pass" | "fail").then_some(name)) {
            args.extend(["--requirement".into(), r.into()]);
        }
        args.push("--".into());
        args.extend(command.iter().map(|s| (*s).into()));
        args
    }
    fn closed(&self, name: &str) -> Result<()> {
        until(Duration::from_secs(8), || {
            let r = self.raw(&["task", "launch-reconcile", name], Duration::from_secs(5))?;
            Ok((r.status.success()
                && serde_json::from_slice::<Value>(&r.stdout)?["launch"]["phase"] == "closed")
                .then_some(()))
        })
    }
}
/// Rust replacements for the scenario's three embedded Python workers.
pub fn worker(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("check-large") => {
            println!("{}", "界".repeat(10000));
        }
        Some("check-ordered") => {
            let root = Path::new(args.get(1).context("root")?);
            let mode = fs::read_to_string(root.join("ordered-mode"))?;
            if mode == "hold" {
                touch(&root.join("ordered-marker"))?;
                let end = Instant::now() + Duration::from_secs(5);
                while !root.join("ordered-release").exists() && Instant::now() < end {
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
            if !matches!(mode.as_str(), "pass" | "hold") {
                std::process::exit(7);
            }
        }
        Some("check-hold") => {
            let marker = Path::new(args.get(1).context("marker")?);
            let release = Path::new(args.get(2).context("release")?);
            let count = Path::new(args.get(3).context("count")?);
            fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(count)?
                .write_all(b"x")?;
            fs::write(marker, nix::unistd::getpgrp().as_raw().to_string())?;
            let end = Instant::now() + Duration::from_secs(5);
            while !release.exists() && Instant::now() < end {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        _ => anyhow::bail!("unknown check worker"),
    }
    Ok(())
}
pub fn run(fux: &Path, zor: &Path) -> Result<()> {
    let root = Root::new("zcheck-rs-", &["/bin/cat".into()])?;
    let mut server = root.server(fux)?;
    let h = Harness { root: &root, zor };
    let release = root.path().join("release");
    let crash_release = root.path().join("crash-release");
    let ordered_release = root.path().join("ordered-release");
    let mut callers: Vec<Caller> = Vec::new();
    let mut crash_group = None;
    let result = (|| -> Result<()> {
        macro_rules! c{($($a:expr),*$(,)?)=>{h.cli(&[$($a),*],true)?};}
        macro_rules! reject{($($a:expr),*$(,)?)=>{h.cli(&[$($a),*],false)?};}
        macro_rules! check {
            ($name:expr,$cmd:expr,$timeout:expr,$req:expr) => {
                h.owned(&h.check($name, $cmd, $timeout, $req), true)?
            };
        }
        let listing = completed(&root.control(), json!({"command":"list","id":1}))?;
        let instance = listing["instance"].as_str().context("instance")?;
        let repo = root.path().join("repo");
        fs::create_dir(&repo)?;
        let git = |args: &[&str]| -> Result<()> {
            let mut cmd = root.command(Path::new("/usr/bin/git"));
            cmd.arg("-C").arg(&repo).args(args);
            let r = process::output(cmd, Duration::from_secs(5), 1048576)?;
            ensure!(
                r.status.success(),
                "git: {}",
                String::from_utf8_lossy(&r.stderr)
            );
            Ok(())
        };
        for args in [
            vec!["init", "-b", "main"],
            vec!["config", "user.name", "Fixture"],
            vec!["config", "user.email", "fixture@example.invalid"],
        ] {
            git(&args)?;
        }
        fs::write(repo.join("source"), "baseline")?;
        git(&["add", "."])?;
        git(&["commit", "-m", "fixture"])?;
        let tree = c!(
            "worktree",
            "create",
            "tree",
            "--repo",
            s(&repo)?,
            "--branch",
            "worker"
        )["worktree"]
            .clone();
        let cwd = Path::new(tree["path"].as_str().context("worktree path")?);
        c!(
            "task",
            "start",
            "worker",
            "--title",
            "checks",
            "--instance",
            instance,
            "--workspace",
            "default",
            "--worktree",
            "tree",
            "--",
            "/bin/cat"
        );
        let policy = c!("task", "require-artifact", "worker", "report", "output.bin");
        ensure!(
            policy["artifact_policy"]
                == json!({"sealed":false,"required_count":1,"collected_count":0}),
            "artifact policy"
        );
        ensure!(
            c!("task", "require-artifact", "worker", "report", "output.bin") == policy,
            "policy replay"
        );
        reject!("task", "require-artifact", "worker", "report", "different");
        reject!("task", "require-artifact", "worker", "bad", "../outside");
        c!(
            "task",
            "require-artifact",
            "worker",
            "missing-report",
            "missing.bin"
        );
        let missing = c!("task", "result", "worker");
        ensure!(
            array(&missing["required_artifacts"])?
                .iter()
                .all(|v| v["status"] == "missing")
                && array(&missing["blockers"])?.contains(&json!("required-artifacts-missing")),
            "missing artifact blockers"
        );
        let journal = root.path().join("state/zor/journal.json");
        let mut capacity: Value = serde_json::from_slice(&fs::read(&journal)?)?;
        for i in 0..30 {
            capacity["tasks"]["worker"]["required_artifacts"][format!("extra-{i}")] =
                json!("source");
        }
        injected(&journal, &capacity, || {
            ensure!(
                c!("task", "inspect", "worker")["artifact_policy"]["required_count"] == 32,
                "capacity load"
            );
            let before = fs::read(&journal)?;
            reject!("task", "require-artifact", "worker", "over-limit", "source");
            ensure!(fs::read(&journal)? == before, "capacity mutation");
            Ok(())
        })?;
        let output = cwd.join("output.bin");
        fs::write(&output, b"\x00\xffresult\n")?;
        let artifact = c!("task", "artifact-collect", "worker", "output", "output.bin");
        ensure!(
            artifact["artifact"]["bytes"] == json!(b"\x00\xffresult\n".to_vec()),
            "binary artifact"
        );
        ensure!(
            array(&c!("task", "result", "worker")["required_artifacts"])?
                .iter()
                .all(|v| v["status"] == "missing"),
            "diagnostic satisfies requirement"
        );
        ensure!(
            c!("task", "inspect", "worker")["artifact_policy"]["sealed"] == true,
            "sealed artifact policy"
        );
        reject!("task", "require-artifact", "worker", "late", "source");
        reject!(
            "task",
            "artifact-collect",
            "worker",
            "output",
            "output.bin",
            "--requirement",
            "report"
        );
        fs::write(&output, "changed")?;
        ensure!(
            c!("task", "artifact-collect", "worker", "output", "output.bin") == artifact,
            "artifact replay changed bytes"
        );
        fs::remove_file(&output)?;
        ensure!(
            c!("task", "artifact-inspect", "output") == artifact,
            "retained artifact"
        );
        reject!("task", "artifact-collect", "worker", "output", "source");
        reject!(
            "task",
            "artifact-collect",
            "worker",
            "missing-output",
            "output.bin"
        );
        let outside = root.path().join("outside");
        fs::write(&outside, "private outside")?;
        std::os::unix::fs::symlink(&outside, cwd.join("symlink"))?;
        std::os::unix::fs::symlink(root.path(), cwd.join("linked-dir"))?;
        fs::hard_link(&outside, cwd.join("hardlink"))?;
        nix::unistd::mkfifo(
            &cwd.join("fifo"),
            nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
        )?;
        fs::write(cwd.join("oversized"), vec![b'x'; 65537])?;
        for path in [
            "../outside",
            s(&outside)?,
            "symlink",
            "linked-dir/outside",
            "hardlink",
            "fifo",
            "oversized",
            "./source",
            "source/",
        ] {
            reject!("task", "artifact-collect", "worker", "bad-output", path);
        }
        let summary = c!("task", "inspect", "worker")["artifacts"].clone();
        ensure!(
            array(&summary)?.len() == 1
                && summary[0]["size"] == 9
                && summary[0].get("bytes").is_none(),
            "artifact summary"
        );
        let retained: Value = serde_json::from_slice(&fs::read(&journal)?)?;
        for (count, payload) in [(128, vec![0]), (8, vec![0; 65536])] {
            let mut v = retained.clone();
            v["artifacts"] = json!({});
            for i in 0..count {
                let mut a = artifact["artifact"].clone();
                a["id"] = json!(i.to_string());
                a["bytes"] = json!(payload);
                a["created_generation"] = json!(i + 1);
                v["artifacts"][i.to_string()] = a;
            }
            v["generation"] = json!(num(&v["generation"])?.max(count));
            injected(&journal, &v, || {
                c!("task", "artifact-inspect", "0");
                let before = fs::read(&journal)?;
                reject!(
                    "task",
                    "artifact-collect",
                    "worker",
                    "over-artifact-limit",
                    "source"
                );
                ensure!(fs::read(&journal)? == before, "artifact capacity mutation");
                Ok(())
            })?;
        }
        let mut malformed = retained;
        malformed["artifacts"]["output"]["attempt"] = json!("missing");
        injected(&journal, &malformed, || {
            let before = fs::read(&journal)?;
            reject!("task", "artifact-inspect", "output");
            ensure!(fs::read(&journal)? == before, "invalid artifact repaired");
            Ok(())
        })?;
        let before = fs::read(&journal)?;
        for (name, path) in [
            ("report", "source"),
            ("unknown", "source"),
            ("missing-report", "missing.bin"),
        ] {
            reject!(
                "task",
                "artifact-collect",
                "worker",
                "refused",
                path,
                "--requirement",
                name
            );
            ensure!(fs::read(&journal)? == before, "failed collection mutation");
        }
        fs::write(&output, "required version 1")?;
        let required_args = [
            "task",
            "artifact-collect",
            "worker",
            "zzz-required",
            "output.bin",
            "--requirement",
            "report",
        ];
        let first_required = h.cli(&required_args, true)?;
        fs::write(&output, "required version 2")?;
        let second_required = c!(
            "task",
            "artifact-collect",
            "worker",
            "zzz-revision",
            "output.bin",
            "--requirement",
            "report"
        );
        ensure!(
            num(&second_required["artifact"]["created_generation"])?
                > num(&first_required["artifact"]["created_generation"])?,
            "artifact ordering"
        );
        let requirements = c!("task", "result", "worker")["required_artifacts"].clone();
        ensure!(
            named(&requirements, "report")?
                == &json!({"name":"report","path":"output.bin","status":"collected","artifact":"zzz-revision"})
                && named(&requirements, "missing-report")?["status"] == "missing",
            "required artifacts"
        );
        ensure!(
            c!("task", "inspect", "worker")["artifact_policy"]
                == json!({"sealed":true,"required_count":2,"collected_count":1}),
            "collected policy"
        );
        fs::remove_file(&output)?;
        ensure!(
            h.cli(&required_args, true)?["artifact"] == first_required["artifact"],
            "required replay"
        );
        let before = fs::read(&journal)?;
        reject!(
            "task",
            "artifact-collect",
            "worker",
            "missing-revision",
            "output.bin",
            "--requirement",
            "report"
        );
        ensure!(
            fs::read(&journal)? == before
                && named(
                    &c!("task", "result", "worker")["required_artifacts"],
                    "report"
                )?["artifact"]
                    == "zzz-revision",
            "failed revision replaced evidence"
        );
        let retained: Value = serde_json::from_slice(&fs::read(&journal)?)?;
        for change in [
            "path",
            "requirement",
            "generation",
            "duplicate-generation",
            "limit",
        ] {
            let mut v = retained.clone();
            match change {
                "path" => v["tasks"]["worker"]["required_artifacts"]["report"] = json!("source"),
                "requirement" => v["artifacts"]["zzz-required"]["requirement"] = json!("unknown"),
                "generation" => v["artifacts"]["zzz-required"]["created_generation"] = json!(0),
                "duplicate-generation" => {
                    v["artifacts"]["zzz-required"]["created_generation"] =
                        v["artifacts"]["zzz-revision"]["created_generation"].clone()
                }
                _ => {
                    for i in 0..31 {
                        v["tasks"]["worker"]["required_artifacts"][format!("extra-{i}")] =
                            json!("source");
                    }
                }
            }
            injected(&journal, &v, || {
                let before = fs::read(&journal)?;
                reject!("task", "result", "worker");
                ensure!(
                    fs::read(&journal)? == before,
                    "corrupt requirement repaired"
                );
                Ok(())
            })?;
        }
        let command = [
            "/bin/sh",
            "-c",
            "printf x >> executions; printf out; printf err >&2",
        ];
        let policy = c!(
            "task",
            "require-check",
            "worker",
            "pass",
            "--",
            command[0],
            command[1],
            command[2]
        );
        ensure!(
            policy["task"]["required_checks"] == json!({"pass":command})
                && policy["check_policy"]["sealed"] == false,
            "check policy"
        );
        ensure!(
            c!(
                "task",
                "require-check",
                "worker",
                "pass",
                "--",
                command[0],
                command[1],
                command[2]
            ) == policy,
            "check policy replay"
        );
        reject!("task", "require-check", "worker", "pass", "--", "/bin/true");
        c!(
            "task",
            "require-check",
            "worker",
            "fail",
            "--",
            "/bin/sh",
            "-c",
            "exit 7"
        );
        let missing_command = ["/bin/sh", "-c", "touch forbidden-required"];
        c!(
            "task",
            "require-check",
            "worker",
            "missing-required",
            "--",
            missing_command[0],
            missing_command[1],
            missing_command[2]
        );
        let mutable = ["/bin/sh", "-c", "test -f ready"];
        c!(
            "task",
            "require-check",
            "worker",
            "mutable",
            "--",
            mutable[0],
            mutable[1],
            mutable[2]
        );
        let executable = std::env::current_exe()?.canonicalize()?;
        let ordered = [
            s(&executable)?,
            "fixture-worker",
            "check-ordered",
            s(root.path())?,
        ];
        let ordered_mode = root.path().join("ordered-mode");
        let ordered_marker = root.path().join("ordered-marker");
        c!(
            "task",
            "require-check",
            "worker",
            "ordered",
            "--",
            ordered[0],
            ordered[1],
            ordered[2],
            ordered[3]
        );
        h.owned(&h.check("pass", &["/bin/true"], 1000, None), false)?;
        ensure!(
            !cwd.join("executions").exists(),
            "mismatched requirement executed"
        );
        let passed = check!("pass", &command, 1000, None);
        ensure!(
            passed["passed"] == true
                && passed["check"]["exit_code"] == 0
                && passed["check"]["stdout"] == "out"
                && passed["check"]["stderr"] == "err",
            "check evidence"
        );
        ensure!(
            check!("pass", &command, 1000, None) == passed
                && fs::read_to_string(cwd.join("executions"))? == "x",
            "check replay executed"
        );
        h.owned(&h.check("pass", &["/bin/false"], 1000, None), false)?;
        let inspected = c!("task", "inspect", "worker");
        ensure!(
            inspected["task"]["outcome"] == "open"
                && inspected["checks"][0]["id"] == "pass"
                && inspected["checks"][0].get("stdout").is_none()
                && inspected["checks"][0]["requirement"] == "pass"
                && inspected["check_policy"]
                    == json!({"sealed":true,"required_count":5,"passed_count":1}),
            "check summary"
        );
        reject!("task", "require-check", "worker", "late", "--", "/bin/true");
        ensure!(
            c!("task", "inspect", "worker") == inspected,
            "late policy mutation"
        );
        ensure!(
            check!("mutable-1", &mutable, 1000, Some("mutable"))["passed"] == false,
            "missing mutable"
        );
        touch(&cwd.join("ready"))?;
        ensure!(
            check!("mutable-2", &mutable, 1000, Some("mutable"))["passed"] == true
                && c!("task", "inspect", "worker")["check_policy"]["passed_count"] == 2,
            "mutable pass"
        );
        fs::remove_file(cwd.join("ready"))?;
        ensure!(
            check!("mutable-3", &mutable, 1000, Some("mutable"))["passed"] == false
                && c!("task", "inspect", "worker")["check_policy"]["passed_count"] == 1,
            "mutable failure replaced pass"
        );
        fs::write(&ordered_mode, "pass")?;
        ensure!(
            check!("ordered-base", &ordered, 1000, Some("ordered"))["passed"] == true
                && c!("task", "inspect", "worker")["check_policy"]["passed_count"] == 2,
            "ordered base"
        );
        fs::write(&ordered_mode, "hold")?;
        let args = h.check("ordered-older", &ordered, 7000, Some("ordered"));
        callers.push(h.spawn(&args)?);
        let older_index = callers.len() - 1;
        until(Duration::from_secs(8), || {
            Ok(ordered_marker.exists().then_some(()))
        })?;
        ensure!(
            c!("task", "inspect", "worker")["check_policy"]["passed_count"] == 1,
            "pending older stale pass"
        );
        fs::write(&ordered_mode, "fail")?;
        let newer = check!("ordered-newer", &ordered, 7000, Some("ordered"));
        ensure!(
            newer["passed"] == false && callers[older_index].child.0.try_wait()?.is_none(),
            "newer failure while older held"
        );
        touch(&ordered_release)?;
        let older = callers[older_index].finish(Duration::from_secs(6))?;
        ensure!(
            older["passed"] == true
                && num(&older["check"]["created_generation"])?
                    < num(&newer["check"]["created_generation"])?
                && c!("task", "inspect", "worker")["check_policy"]["passed_count"] == 1,
            "older result replaced newer evidence"
        );
        fs::write(&ordered_mode, "pass")?;
        ensure!(
            check!("ordered-pass", &ordered, 1000, Some("ordered"))["passed"] == true
                && c!("task", "inspect", "worker")["check_policy"]["passed_count"] == 2,
            "ordered latest pass"
        );
        fs::remove_file(&ordered_release)?;
        fs::write(&ordered_mode, "hold")?;
        let uncertain = check!("ordered-timeout", &ordered, 100, Some("ordered"));
        ensure!(
            uncertain.get("passed") == Some(&Value::Null)
                && uncertain["check"]["phase"] == "uncertain"
                && c!("task", "inspect", "worker")["check_policy"]["passed_count"] == 1,
            "ordered timeout"
        );
        let failed = check!("fail", &["/bin/sh", "-c", "exit 7"], 1000, None);
        ensure!(
            failed["passed"] == false && failed["check"]["exit_code"] == 7,
            "failed check"
        );
        let before = fs::read(&journal)?;
        let view = c!("task", "result", "worker");
        ensure!(
            fs::read(&journal)? == before
                && view["verification"]["status"] == "unverified"
                && view["task_outcome"] == "open"
                && view["worktree"]["id"] == "tree",
            "read-only result"
        );
        ensure!(
            view["artifacts"][0]["id"] == "output"
                && view["artifacts"][0].get("bytes").is_none()
                && view["output"]["available"] == false
                && view["changed_files"]["status"] == "not-collected",
            "result artifacts/output"
        );
        let required = &view["required_checks"];
        ensure!(
            named(required, "missing-required")?["status"] == "missing"
                && named(required, "fail")?["status"] == "failed"
                && named(required, "pass")?["status"] == "passed"
                && named(required, "ordered")?["status"] == "uncertain"
                && named(required, "ordered")?["execution"] == "ordered-timeout"
                && array(&view["blockers"])?.contains(&json!("required-checks-not-passed"))
                && c!("task", "result", "worker") == view,
            "required check result"
        );
        let signalled = check!("signal", &["/bin/sh", "-c", "kill -TERM $$"], 1000, None);
        ensure!(
            signalled["passed"] == false && signalled["check"]["signal"] == 15,
            "signal evidence"
        );
        let large = check!(
            "large",
            &[s(&executable)?, "fixture-worker", "check-large"],
            1000,
            None
        );
        ensure!(
            large["passed"] == true
                && large["check"]["truncated"] == true
                && large["check"]["stdout"].as_str().context("stdout")?.len() <= 4096,
            "UTF-8 cap"
        );
        c!(
            "task",
            "start",
            "literal",
            "--title",
            "literal cwd",
            "--instance",
            instance,
            "--workspace",
            "default",
            "--cwd",
            s(cwd)?,
            "--",
            "/bin/sh",
            "-c",
            "sleep 0.2"
        );
        h.closed("literal")?;
        let closed = c!("task", "result", "literal");
        ensure!(
            closed["output"]["available"] == true
                && closed["output"]["evidence"]["exit_status"] == 0
                && closed["verification"]["status"] == "unverified"
                && closed["task_outcome"] == "open",
            "closed output not verification"
        );
        let before = fs::read(&journal)?;
        reject!(
            "task",
            "check",
            "literal",
            "wrong-task",
            "--requirement",
            "missing-required",
            "--",
            missing_command[0],
            missing_command[1],
            missing_command[2]
        );
        ensure!(
            fs::read(&journal)? == before && !cwd.join("forbidden-required").exists(),
            "cross-task requirement executed"
        );
        c!("task", "require-artifact", "literal", "report", "source");
        reject!(
            "task",
            "artifact-collect",
            "literal",
            "zzz-required",
            "output.bin",
            "--requirement",
            "report"
        );
        c!(
            "task",
            "require-check",
            "literal",
            "missing-required",
            "--",
            missing_command[0],
            missing_command[1],
            missing_command[2]
        );
        c!(
            "task",
            "check",
            "literal",
            "seal-artifact-policy",
            "--",
            "/bin/true"
        );
        reject!("task", "require-artifact", "literal", "late", "source");
        let mut malformed: Value = serde_json::from_slice(&fs::read(&journal)?)?;
        malformed["tasks"]["worker"]["required_checks"]["pass"] = json!(["/bin/true"]);
        injected(&journal, &malformed, || {
            let before = fs::read(&journal)?;
            reject!("task", "inspect", "worker");
            ensure!(fs::read(&journal)? == before, "mismatched check repaired");
            Ok(())
        })?;
        let mut removing: Value = serde_json::from_slice(&fs::read(&journal)?)?;
        removing["worktrees"]["tree"]["phase"] = json!("removing");
        removing["worktrees"]["tree"]["remove_force"] = json!(false);
        injected(&journal, &removing, || {
            let r = reject!(
                "task",
                "check",
                "literal",
                "forbidden",
                "--",
                "/bin/sh",
                "-c",
                "touch forbidden"
            );
            ensure!(
                contains(&r, "removal intent") && !cwd.join("forbidden").exists(),
                "removal guard"
            );
            Ok(())
        })?;
        let original: Value = serde_json::from_slice(&fs::read(&journal)?)?;
        let mut template = original["checks"]["pass"].clone();
        for (k, v) in [
            ("phase", json!("submitted")),
            ("exit_code", Value::Null),
            ("signal", Value::Null),
            ("stdout", json!("")),
            ("stderr", json!("")),
            ("truncated", json!(false)),
            ("problem", Value::Null),
            ("requirement", Value::Null),
        ] {
            template[k] = v;
        }
        let mut state = original.clone();
        state["checks"] = json!({});
        for i in 0..63 {
            let mut v = template.clone();
            v["id"] = json!(i.to_string());
            v["created_generation"] = json!(i + 1);
            state["checks"][i.to_string()] = v;
        }
        state["generation"] = json!(num(&state["generation"])?.max(63));
        injected(&journal, &state, || {
            c!("task", "check-inspect", "0");
            let before = fs::read(&journal)?;
            h.owned(
                &h.check(
                    "over-budget",
                    &["/bin/sh", "-c", "touch should-not-run"],
                    1000,
                    None,
                ),
                false,
            )?;
            ensure!(
                fs::read(&journal)? == before && !cwd.join("should-not-run").exists(),
                "budget admitted check"
            );
            Ok(())
        })?;
        let cap_marker = root.path().join("cap-marker");
        let cap_args = h.check(
            "cap-error",
            &[
                "/bin/sh",
                "-c",
                "printf ready > \"$1\"; exec sleep 30",
                "fixture",
                s(&cap_marker)?,
            ],
            2000,
            None,
        );
        callers.push(h.spawn(&cap_args)?);
        let cap_index = callers.len() - 1;
        until(Duration::from_secs(8), || {
            Ok(cap_marker.exists().then_some(()))
        })?;
        let original: Value = serde_json::from_slice(&fs::read(&journal)?)?;
        let mut state = original.clone();
        let running = state["checks"]["cap-error"].clone();
        let base = num(&state["generation"])?;
        state["checks"] = json!({"cap-error":running});
        for i in 0..62 {
            let mut v = template.clone();
            v["id"] = json!(i.to_string());
            v["created_generation"] = json!(base + i + 1);
            state["checks"][i.to_string()] = v;
        }
        state["generation"] = json!(base + 62);
        for i in 0..62 {
            let bytes = serde_json::to_vec(&state)?;
            let room = (4usize * 1024 * 1024)
                .checked_sub(63 * 65536 + bytes.len())
                .context("capacity overflow")?;
            if room == 0 {
                break;
            }
            let text = state["checks"][i.to_string()]["argv"][1]
                .as_str()
                .context("argv")?
                .to_owned()
                + &"x".repeat(room.min(4000));
            state["checks"][i.to_string()]["argv"][1] = json!(text);
        }
        let encoded = serde_json::to_vec(&state)?;
        ensure!(
            encoded.len() + 63 * 65536 == 4 * 1024 * 1024,
            "exact journal capacity"
        );
        fs::write(&journal, encoded)?;
        let cap_result = (|| -> Result<()> {
            c!("task", "check-inspect", "cap-error");
            let v = callers[cap_index].finish(Duration::from_secs(6))?;
            ensure!(
                v["check"]["phase"] == "uncertain" && contains(&v["check"]["problem"], "deadline"),
                "reserved result error publication"
            );
            Ok(())
        })();
        // Quiesce the real result writer before restoring the artificial capacity fixture.
        if callers[cap_index].child.0.try_wait()?.is_none() {
            callers[cap_index].finish(Duration::from_secs(6))?;
        }
        let current: Value = serde_json::from_slice(&fs::read(&journal)?)?;
        let mut restored = original;
        restored["checks"]["cap-error"] = current["checks"]["cap-error"].clone();
        restored["generation"] = current["generation"].clone();
        fs::write(&journal, serde_json::to_vec(&restored)?)?;
        cap_result?;
        let timeout = check!("timeout", &["/bin/sleep", "30"], 100, None);
        ensure!(
            timeout.get("passed") == Some(&Value::Null)
                && timeout["check"]["phase"] == "uncertain"
                && contains(&timeout["check"]["problem"], "deadline")
                && check!("timeout", &["/bin/sleep", "30"], 100, None) == timeout,
            "timeout no-replay"
        );
        let pressure = check!("pressure", &["/bin/sh", "-c", "yes x"], 1000, None);
        ensure!(
            pressure.get("passed") == Some(&Value::Null)
                && contains(&pressure["check"]["problem"], "output limit"),
            "output pressure"
        );
        ensure!(
            contains(
                &reject!("worktree", "remove", "tree", "--force"),
                "unfinished check"
            ),
            "unfinished removal guard"
        );
        c!(
            "worktree",
            "create",
            "crash-tree",
            "--repo",
            s(&repo)?,
            "--branch",
            "crash-worker"
        );
        c!(
            "task",
            "start",
            "crash-worker",
            "--title",
            "crash check",
            "--instance",
            instance,
            "--workspace",
            "default",
            "--worktree",
            "crash-tree",
            "--",
            "/bin/sh",
            "-c",
            "exit 0"
        );
        h.closed("crash-worker")?;
        let crash_marker = root.path().join("crash-marker");
        let crash_count = root.path().join("crash-count");
        let mut crash_args = h.check(
            "crash",
            &[
                s(&executable)?,
                "fixture-worker",
                "check-hold",
                s(&crash_marker)?,
                s(&crash_release)?,
                s(&crash_count)?,
            ],
            7000,
            None,
        );
        crash_args[2] = "crash-worker".into();
        callers.push(h.spawn(&crash_args)?);
        let crash_index = callers.len() - 1;
        let group = until(Duration::from_secs(8), || {
            if !crash_marker.exists() {
                return Ok(None);
            }
            Ok(fs::read_to_string(&crash_marker)?.parse::<i32>().ok())
        })?;
        ensure!(
            group > 1 && group != nix::unistd::getpgrp().as_raw(),
            "crash group ownership"
        );
        crash_group = Some(group);
        callers[crash_index].kill()?;
        nix::sys::signal::killpg(nix::unistd::Pid::from_raw(group), None)?;
        ensure!(
            c!("task", "recover")["recovered_checks"] == 1,
            "recover killed runner"
        );
        nix::sys::signal::killpg(nix::unistd::Pid::from_raw(group), None)?;
        let retry = h.owned(&crash_args, true)?;
        ensure!(
            retry["check"]["phase"] == "uncertain"
                && retry.get("passed") == Some(&Value::Null)
                && contains(&retry["check"]["problem"], "execution may continue"),
            "crash uncertainty"
        );
        let before = fs::read(&journal)?;
        ensure!(
            c!("task", "recover")["recovered_checks"] == 0 && fs::read(&journal)? == before,
            "recover replay"
        );
        ensure!(
            contains(
                &reject!("worktree", "remove", "crash-tree", "--force"),
                "unfinished check"
            ),
            "crash removal guard"
        );
        touch(&crash_release)?;
        ensure!(
            fs::read_to_string(&crash_count)? == "x",
            "crash execution repeated"
        );
        let marker = root.path().join("marker");
        let count = root.path().join("count");
        let args = h.check(
            "concurrent",
            &[
                s(&executable)?,
                "fixture-worker",
                "check-hold",
                s(&marker)?,
                s(&release)?,
                s(&count)?,
            ],
            7000,
            None,
        );
        callers.push(h.spawn(&args)?);
        let index = callers.len() - 1;
        until(Duration::from_secs(8), || {
            Ok(
                (marker.exists() && fs::read_to_string(&marker)?.parse::<i32>().is_ok())
                    .then_some(()),
            )
        })?;
        let before = fs::read(&journal)?;
        ensure!(
            c!("task", "recover")["recovered_checks"] == 0 && fs::read(&journal)? == before,
            "live runner recovered"
        );
        ensure!(
            h.owned(&args, true)?["check"]["phase"] == "submitted",
            "concurrent replay"
        );
        ensure!(
            c!("task", "cancel", "worker")["task"]["outcome"] == "cancelled",
            "cancel concurrent"
        );
        touch(&release)?;
        let completed = callers[index].finish(Duration::from_secs(5))?;
        ensure!(
            completed["passed"] == true
                && fs::read_to_string(&count)? == "x"
                && c!("task", "inspect", "worker")["task"]["outcome"] == "cancelled",
            "late completion changed cancellation"
        );
        ensure!(
            h.cli(&required_args, true)?["artifact"] == first_required["artifact"],
            "cancelled artifact replay"
        );
        c!("task", "require-artifact", "worker", "report", "output.bin");
        reject!(
            "task",
            "require-artifact",
            "worker",
            "after-cancel",
            "source"
        );
        let cancelled = c!("task", "result", "worker");
        ensure!(
            cancelled["task_outcome"] == "cancelled"
                && array(&cancelled["blockers"])?.contains(&json!("task-cancelled"))
                && cancelled["verification"]["status"] == "unverified",
            "cancelled result"
        );
        ensure!(
            c!("task", "artifact-inspect", "output")["artifact"] == artifact["artifact"]
                && c!("task", "artifact-collect", "worker", "output", "output.bin")["artifact"]
                    == artifact["artifact"],
            "cancelled retained diagnostic"
        );
        reject!(
            "task",
            "artifact-collect",
            "worker",
            "after-cancel-output",
            "source"
        );
        h.owned(&h.check("after-cancel", &["/bin/true"], 1000, None), false)?;
        Ok(())
    })();
    let mut errors = Vec::new();
    for p in [&release, &crash_release, &ordered_release] {
        if let Err(e) = touch(p) {
            errors.push(e.to_string());
        }
    }
    for c in &mut callers {
        if let Err(e) = c.kill() {
            errors.push(e.to_string());
        }
    }
    if let Some(group) = crash_group
        && let Err(e) = until(Duration::from_secs(8), || {
            match nix::sys::signal::killpg(nix::unistd::Pid::from_raw(group), None) {
                Err(nix::errno::Errno::ESRCH) => Ok(Some(())),
                Ok(()) => Ok(None),
                Err(e) => Err(e.into()),
            }
        })
    {
        errors.push(e.to_string());
    }
    if let Err(e) = server.finish() {
        errors.push(e.to_string());
    }
    if !errors.is_empty() {
        eprintln!("cleanup: {}", errors.join("; "));
    }
    result?;
    ensure!(errors.is_empty(), "{}", errors.join("; "));
    println!(
        "PASS explicit checks, bounded evidence, no replay, crash uncertainty and concurrent task cancellation"
    );
    Ok(())
}
