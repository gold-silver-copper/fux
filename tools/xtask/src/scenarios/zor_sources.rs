//! Retained committed sources, independent check directories and sealed verification.
use crate::support::{
    local::{Root, completed, until},
    process::{self, Guard},
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Seek, SeekFrom},
    os::unix::fs::PermissionsExt,
    path::Path,
    process::Stdio,
    time::{Duration, Instant},
};
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
fn s(p: &Path) -> Result<&str> {
    p.to_str().context("path")
}
fn n(v: &Value) -> Result<u64> {
    v.as_u64().context("integer")
}
fn arr(v: &Value) -> Result<&Vec<Value>> {
    v.as_array().context("array")
}
fn named<'a>(v: &'a Value, key: &str, name: &str) -> Result<&'a Value> {
    arr(v)?
        .iter()
        .find(|v| v[key] == name)
        .context("named record")
}
fn has(v: &Value, name: &str) -> bool {
    v.as_str().is_some_and(|v| v.contains(name))
}
fn restore<T>(p: &Path, saved: &[u8], run: impl FnOnce() -> Result<T>) -> Result<T> {
    let result = run();
    let written = fs::write(p, saved);
    result.and_then(|v| {
        written?;
        Ok(v)
    })
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
    fn spawn(&self, args: &[&str]) -> Result<Caller> {
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
    fn git(&self, path: &Path, args: &[&str]) -> Result<String> {
        let mut c = self.root.command(Path::new("/usr/bin/git"));
        c.arg("-C").arg(path).args(args);
        let r = process::output(c, Duration::from_secs(5), 1048576)?;
        ensure!(
            r.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&r.stderr)
        );
        Ok(String::from_utf8(r.stdout)?)
    }
    fn stopped(&self, name: &str) -> Result<()> {
        let end = Instant::now() + Duration::from_secs(8);
        loop {
            let r = self.raw(&["task", "stop", name], Duration::from_secs(5))?;
            if r.status.success() {
                return Ok(());
            }
            let error = String::from_utf8_lossy(&r.stderr);
            ensure!(
                Instant::now() < end
                    && (error.contains("stop requested")
                        || error.contains("managed process lifecycle uncertain")),
                "stop {name}: {error}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}
fn dirs(root: &Path) -> Result<Vec<std::path::PathBuf>> {
    let mut v = fs::read_dir(root)?
        .map(|e| e.map(|e| e.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    v.retain(|p| {
        p.file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with("check-"))
    });
    v.sort();
    Ok(v)
}
pub fn run(fux: &Path, zor: &Path) -> Result<()> {
    let root = Root::new("zsource-rs-", &["/bin/cat".into()])?;
    let mut server = root.server(fux)?;
    let h = Harness { root: &root, zor };
    let result = (|| -> Result<()> {
        macro_rules! c{($($a:expr),*$(,)?)=>{h.cli(&[$($a),*],true)?};}
        macro_rules! reject{($($a:expr),*$(,)?)=>{h.cli(&[$($a),*],false)?};}
        let listing = completed(&root.control(), json!({"command":"list","id":1}))?;
        let instance = listing["instance"].as_str().context("instance")?;
        let repo = root.path().join("repo");
        fs::create_dir(&repo)?;
        h.git(&repo, &["init", "-b", "main"])?;
        h.git(&repo, &["config", "user.name", "Fixture"])?;
        h.git(&repo, &["config", "user.email", "fixture@example.invalid"])?;
        let binary = (0..=255u8).collect::<Vec<_>>();
        fs::write(repo.join("source"), "committed\n")?;
        fs::write(repo.join("binary"), &binary)?;
        fs::write(repo.join(".gitignore"), "ignored\n")?;
        fs::write(repo.join(".gitattributes"), "source filter=fixture\n")?;
        fs::create_dir(repo.join("a"))?;
        fs::write(repo.join("a/file"), "nested")?;
        fs::write(repo.join("a.c"), "tree sort order")?;
        fs::write(
            repo.join("verify"),
            "#!/bin/sh\nset -eu\ntest \"$(cat source)\" = committed\ntest ! -e untracked\ntest ! -e ignored\ntest ! -e .git\nprintf verified-input\n",
        )?;
        fs::set_permissions(repo.join("verify"), fs::Permissions::from_mode(0o755))?;
        h.git(&repo, &["add", "."])?;
        h.git(&repo, &["commit", "-m", "baseline"])?;
        let base = h.git(&repo, &["rev-parse", "HEAD"])?.trim().to_owned();
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
            "source checks",
            "--instance",
            instance,
            "--workspace",
            "default",
            "--worktree",
            "tree",
            "--",
            "/bin/cat"
        );
        fs::write(cwd.join("source"), "dirty agent edit\n")?;
        fs::write(cwd.join("untracked"), "not committed")?;
        fs::write(cwd.join("ignored"), "not committed")?;
        let source_args = ["task", "source-collect", "worker", "baseline"];
        let retained = h.cli(&source_args, true)?;
        ensure!(
            retained["source"]["commit"] == base && retained["source"]["file_count"] == 7,
            "retained commit/tree"
        );
        ensure!(
            c!("task", "source-file", "baseline", "source")["file"]["bytes"]
                == json!(b"committed\n".to_vec())
                && c!("task", "source-file", "baseline", "binary")["file"]["bytes"]
                    == json!(binary)
                && c!("task", "source-file", "baseline", "verify")["file"]["executable"] == true,
            "retained bytes/mode"
        );
        reject!("task", "source-file", "baseline", "../source");
        let journal = root.path().join("state/zor/journal.json");
        let before = fs::read(&journal)?;
        let mut capacity: Value = serde_json::from_slice(&before)?;
        let original = capacity["sources"]["baseline"].clone();
        let generation = n(&capacity["generation"])?;
        for i in 0..127 {
            let mut v = original.clone();
            v["id"] = json!(format!("capacity-{i}"));
            v["created_generation"] = json!(generation + i + 1);
            capacity["sources"][format!("capacity-{i}")] = v;
        }
        capacity["generation"] = json!(generation + 127);
        fs::write(&journal, serde_json::to_vec(&capacity)?)?;
        restore(&journal, &before, || {
            c!("task", "source-inspect", "baseline");
            let full = fs::read(&journal)?;
            reject!("task", "source-collect", "worker", "over-capacity");
            ensure!(fs::read(&journal)? == full, "source count capacity mutated");
            Ok(())
        })?;
        reject!(
            "task",
            "source-collect",
            "worker",
            "baseline",
            "--revision",
            &base
        );
        ensure!(fs::read(&journal)? == before, "source intent changed");
        for (name, path) in [
            ("report", "report.bin"),
            ("log", "log.txt"),
            ("missing", "missing.bin"),
        ] {
            c!("task", "require-artifact", "worker", name, path);
        }
        c!(
            "task",
            "require-check",
            "worker",
            "verify",
            "--",
            "./verify"
        );
        let check_args = [
            "task",
            "check",
            "worker",
            "verified-input",
            "--source",
            "baseline",
            "--requirement",
            "verify",
            "--",
            "./verify",
        ];
        let checked = h.cli(&check_args, true)?;
        ensure!(
            checked["passed"] == true && checked["check"]["stdout"] == "verified-input",
            "source-bound check"
        );
        let private = Path::new(checked["check"]["cwd"].as_str().context("check cwd")?);
        ensure!(
            private.parent() == Some(root.path().join("state/zor").canonicalize()?.as_path())
                && private
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("check-")
                && fs::metadata(private)?.permissions().mode() & 0o777 == 0o700,
            "private check root"
        );
        ensure!(
            fs::read(private.join("source"))? == b"committed\n"
                && fs::read_to_string(cwd.join("source"))? == "dirty agent edit\n"
                && !private.join(".git").exists(),
            "independent committed checkout"
        );
        fs::write(private.join("source"), "changed in old check checkout")?;
        ensure!(
            h.cli(&check_args, true)?["check"] == checked["check"],
            "check replay recreated directory"
        );
        let rerun = c!(
            "task",
            "check",
            "worker",
            "verified-again",
            "--source",
            "baseline",
            "--requirement",
            "verify",
            "--",
            "./verify"
        );
        ensure!(
            rerun["passed"] == true && rerun["check"]["cwd"] != checked["check"]["cwd"],
            "fresh independent execution"
        );
        reject!(
            "task",
            "check",
            "worker",
            "verified-input",
            "--requirement",
            "verify",
            "--",
            "./verify"
        );
        reject!(
            "task",
            "check",
            "worker",
            "verified-input",
            "--source",
            "absent",
            "--requirement",
            "verify",
            "--",
            "./verify"
        );
        let before = fs::read(&journal)?;
        let directories = dirs(&root.path().join("state/zor"))?;
        reject!(
            "task",
            "check",
            "worker",
            "absent-source",
            "--source",
            "absent",
            "--",
            "/usr/bin/true"
        );
        ensure!(
            fs::read(&journal)? == before && dirs(&root.path().join("state/zor"))? == directories,
            "missing source mutated state/directory"
        );
        let marker = root.path().join("must-not-run");
        let helper = root.path().join("helper");
        fs::write(&helper, format!("#!/bin/sh\ntouch \"{}\"\n", s(&marker)?))?;
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o700))?;
        fs::write(cwd.join("source"), "new commit\n")?;
        h.git(cwd, &["-c", "core.fsmonitor=false", "add", "source"])?;
        h.git(
            cwd,
            &["-c", "core.fsmonitor=false", "commit", "-m", "advance"],
        )?;
        let head = h.git(cwd, &["rev-parse", "HEAD"])?.trim().to_owned();
        for key in [
            "core.fsmonitor",
            "filter.fixture.clean",
            "filter.fixture.smudge",
        ] {
            h.git(cwd, &["config", key, s(&helper)?])?;
        }
        h.git(cwd, &["replace", &base, &head])?;
        let historical = c!(
            "task",
            "source-collect",
            "worker",
            "historical",
            "--revision",
            &base
        );
        ensure!(
            historical["source"]["commit"] == base
                && c!("task", "source-file", "historical", "source")["file"]["bytes"]
                    == json!(b"committed\n".to_vec())
                && !marker.exists(),
            "Git helper/replacement changed source"
        );
        for key in ["filter.fixture.clean", "filter.fixture.smudge"] {
            h.git(cwd, &["config", "--unset", key])?;
        }
        ensure!(
            h.cli(&source_args, true)?["source"] == retained["source"],
            "retained source replay"
        );
        let newer = c!("task", "source-collect", "worker", "newer");
        ensure!(newer["source"]["commit"] == head, "new HEAD source");
        ensure!(
            c!(
                "task",
                "check",
                "worker",
                "newer-fails",
                "--source",
                "newer",
                "--requirement",
                "verify",
                "--",
                "./verify"
            )["passed"]
                == false,
            "new source failure"
        );
        let view = c!("task", "result", "worker");
        ensure!(
            arr(&view["sources"])?.len() == 3
                && arr(&view["sources"])?
                    .iter()
                    .all(|v| v.get("files").is_none())
                && view["verification"]["status"] == "unverified"
                && view["required_checks"][0]["status"] == "failed"
                && named(&view["checks"], "id", "verified-input")?["source"] == "baseline",
            "source result view"
        );
        let executable = std::env::current_exe()?.canonicalize()?;
        let exe = s(&executable)?;
        let capture_args = [
            "task",
            "check",
            "worker",
            "capture",
            "--source",
            "baseline",
            "--artifact",
            "report=captured-report",
            "--artifact",
            "log=captured-log",
            "--artifact",
            "missing=captured-missing",
            "--",
            exe,
            "fixture-worker",
            "source-produce",
        ];
        let captured = h.cli(&capture_args, true)?;
        ensure!(
            captured["passed"] == true
                && captured["check"]["artifact_problems"]
                    .as_object()
                    .context("problems")?
                    .keys()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    == ["missing"],
            "bound capture problems"
        );
        let report = c!("task", "artifact-inspect", "captured-report")["artifact"].clone();
        let log = c!("task", "artifact-inspect", "captured-log")["artifact"].clone();
        ensure!(
            report["bytes"] == json!(binary)
                && log["bytes"] == json!(b"log".to_vec())
                && report["check"] == "capture"
                && log["check"] == "capture"
                && report["source"] == "baseline",
            "bound artifact bytes/association"
        );
        ensure!(
            report["created_generation"] == log["created_generation"]
                && report["created_generation"] == captured["generation"]
                && n(&report["created_generation"])? > n(&captured["check"]["created_generation"])?,
            "atomic publication generation"
        );
        fs::write(
            Path::new(captured["check"]["cwd"].as_str().context("cwd")?).join("report.bin"),
            "changed after publication",
        )?;
        ensure!(
            h.cli(&capture_args, true)?["check"] == captured["check"]
                && c!("task", "artifact-inspect", "captured-report")["artifact"] == report
                && arr(&c!("task", "result", "worker")["blockers"])?
                    .contains(&json!("artifact-capture-failed")),
            "capture replay/blocker"
        );
        let before = fs::read(&journal)?;
        let changes = c!("task", "changes-collect", "worker", "history-changes")["changes"].clone();
        let mut history: Value = serde_json::from_slice(&fs::read(&journal)?)?;
        let old_attempt = history["tasks"]["worker"]["attempt"]
            .as_str()
            .context("attempt")?
            .to_owned();
        let old_session = history["attempts"][&old_attempt]["session"]
            .as_str()
            .context("session")?
            .to_owned();
        let mut previous = history["launches"]["worker"].clone();
        previous["id"] = json!("previous-worker");
        previous["task"] = json!("worker");
        history["launches"]["previous-worker"] = previous;
        history["sessions"][&old_session]["launch"] = json!("previous-worker");
        let mut next = history["sessions"][&old_session].clone();
        next["id"] = json!("next-session");
        next["launch"] = json!("worker");
        history["sessions"]["next-session"] = next;
        let mut next = history["attempts"][&old_attempt].clone();
        next["id"] = json!("next-attempt");
        next["session"] = json!("next-session");
        history["attempts"]["next-attempt"] = next;
        history["tasks"]["worker"]["attempt"] = json!("next-attempt");
        history["launches"]["worker"]["session"] = json!("next-session");
        history["launches"]["worker"]["marker"] = json!("f".repeat(32));
        fs::write(&journal, serde_json::to_vec(&history)?)?;
        restore(&journal, &before, || {
            let current = c!("task", "inspect", "worker");
            ensure!(
                current["attempt"]["id"] == "next-attempt"
                    && current["check_policy"]["passed_count"] == 0
                    && current["artifact_policy"]["collected_count"] == 0
                    && current["artifact_captures"] == json!([]),
                "historical inputs satisfied current attempt"
            );
            let view = c!("task", "result", "worker");
            ensure!(
                view["required_checks"][0]["status"] == "missing"
                    && arr(&view["required_artifacts"])?
                        .iter()
                        .all(|v| v["status"] == "missing"),
                "historical requirements"
            );
            ensure!(
                c!("task", "artifact-inspect", "captured-report")["artifact"] == report
                    && c!("task", "changes-inspect", "history-changes")["changes"] == changes
                    && c!("task", "source-inspect", "baseline")["source"] == retained["source"]
                    && c!("task", "check-inspect", "verified-input")["check"] == checked["check"],
                "historical evidence unavailable"
            );
            let saved = fs::read(&journal)?;
            ensure!(
                has(
                    &reject!("task", "verify", "worker", "baseline"),
                    "another task/attempt"
                ) && has(
                    &reject!("task", "stop", "previous-worker"),
                    "historical launch"
                ) && has(
                    &reject!("task", "launch-reconcile", "previous-worker"),
                    "historical launch"
                ) && fs::read(&journal)? == saved,
                "historical authority refused without mutation"
            );
            for (table, key) in [
                ("sources", "baseline"),
                ("checks", "verified-input"),
                ("artifacts", "captured-report"),
                ("changes", "history-changes"),
            ] {
                let mut v: Value = serde_json::from_slice(&saved)?;
                v[table][key]["task"] = json!("foreign");
                fs::write(&journal, serde_json::to_vec(&v)?)?;
                reject!("task", "list");
            }
            let mut v: Value = serde_json::from_slice(&saved)?;
            let mut duplicate = v["attempts"][&old_attempt].clone();
            duplicate["id"] = json!("duplicate-history");
            v["attempts"]["duplicate-history"] = duplicate;
            fs::write(&journal, serde_json::to_vec(&v)?)?;
            ensure!(
                has(
                    &reject!("task", "list"),
                    "managed attempt owner/session mismatch"
                ),
                "duplicate history refused"
            );
            Ok(())
        })?;
        let before = fs::read(&journal)?;
        for args in [
            vec!["--artifact", "report=captured-report"],
            vec!["--artifact", "report=new-id", "--artifact", "log=new-id"],
            vec!["--artifact", "unknown=new-id"],
            vec!["--artifact", "report=one", "--artifact", "report=two"],
        ] {
            let mut cmd = vec!["task", "check", "worker", "refused", "--source", "baseline"];
            cmd.extend(args);
            cmd.extend(["--", "/usr/bin/true"]);
            h.cli(&cmd, false)?;
            ensure!(
                fs::read(&journal)? == before,
                "invalid capture intent mutated journal"
            );
        }
        reject!(
            "task",
            "check",
            "worker",
            "unbound",
            "--artifact",
            "report=unbound-output",
            "--",
            "/usr/bin/true"
        );
        reject!(
            "task",
            "artifact-collect",
            "worker",
            "captured-missing",
            "missing.bin",
            "--requirement",
            "missing"
        );
        reject!(
            "task",
            "artifact-collect",
            "worker",
            "captured-report",
            "report.bin",
            "--requirement",
            "report"
        );
        ensure!(fs::read(&journal)? == before, "reserved capture IDs reused");
        let missing = c!(
            "task",
            "check",
            "worker",
            "capture-missing",
            "--source",
            "baseline",
            "--artifact",
            "report=missing-report",
            "--",
            "/usr/bin/true"
        );
        ensure!(
            missing["passed"] == true
                && missing["check"]["artifact_problems"]
                    .get("report")
                    .is_some(),
            "missing capture evidence"
        );
        let view = c!("task", "result", "worker");
        ensure!(
            named(&view["required_artifacts"], "name", "report")?["status"] == "collected"
                && named(&view["artifact_captures"], "name", "report")?["status"] == "failed",
            "new failed capture hidden behind old artifact"
        );
        for (index, mode) in ["symlink", "hardlink", "fifo", "oversized"]
            .into_iter()
            .enumerate()
        {
            let id = format!("unsafe-{index}");
            let artifact_id = format!("unsafe-output-{index}");
            let mapping = format!("report={artifact_id}");
            let v = c!(
                "task",
                "check",
                "worker",
                &id,
                "--source",
                "baseline",
                "--artifact",
                &mapping,
                "--",
                exe,
                "fixture-worker",
                "source-unsafe",
                mode
            );
            ensure!(
                v["passed"] == true
                    && v["check"]["artifact_problems"]
                        .as_object()
                        .context("problems")?
                        .keys()
                        .map(String::as_str)
                        .collect::<Vec<_>>()
                        == ["report"],
                "unsafe capture {mode}"
            );
            reject!("task", "artifact-inspect", &artifact_id);
        }
        let recovered = c!(
            "task",
            "check",
            "worker",
            "capture-recovered",
            "--source",
            "baseline",
            "--artifact",
            "report=recovered-report",
            "--artifact",
            "missing=recovered-missing",
            "--",
            exe,
            "fixture-worker",
            "source-recovered"
        );
        ensure!(
            recovered["check"]["artifact_problems"]
                .as_object()
                .context("problems")?
                .is_empty()
                && !arr(&c!("task", "result", "worker")["blockers"])?
                    .contains(&json!("artifact-capture-failed")),
            "capture recovery"
        );
        let timed = c!(
            "task",
            "check",
            "worker",
            "capture-timeout",
            "--source",
            "baseline",
            "--timeout-ms",
            "20",
            "--artifact",
            "log=timeout-log",
            "--",
            "/bin/sleep",
            "1"
        );
        ensure!(
            timed["check"]["phase"] == "uncertain"
                && timed["check"]["artifact_problems"].get("log").is_some(),
            "timeout capture evidence"
        );
        reject!(
            "task",
            "artifact-collect",
            "worker",
            "timeout-log",
            "log.txt",
            "--requirement",
            "log"
        );
        let renamed = c!(
            "task",
            "check",
            "worker",
            "capture-renamed",
            "--source",
            "baseline",
            "--artifact",
            "report=renamed-report",
            "--",
            exe,
            "fixture-worker",
            "source-renamed"
        );
        ensure!(
            renamed["passed"] == true
                && renamed["check"]["artifact_problems"]
                    .as_object()
                    .context("problems")?
                    .is_empty(),
            "renamed cwd capture"
        );
        ensure!(
            c!("task", "artifact-inspect", "renamed-report")["artifact"]["bytes"]
                == json!(b"original directory".to_vec())
                && fs::read_to_string(
                    Path::new(renamed["check"]["cwd"].as_str().context("cwd")?).join("report.bin")
                )? == "replacement",
            "capture followed replacement path"
        );
        let gate = root.path().join("capture-release");
        let started = root.path().join("capture-started");
        let held_args = [
            "task",
            "check",
            "worker",
            "capture-held",
            "--source",
            "baseline",
            "--timeout-ms",
            "20000",
            "--artifact",
            "report=held-report",
            "--",
            exe,
            "fixture-worker",
            "source-held",
            s(&started)?,
            s(&gate)?,
        ];
        let mut caller = h.spawn(&held_args)?;
        let held = (|| -> Result<()> {
            until(Duration::from_secs(8), || {
                Ok(started.exists().then_some(()))
            })?;
            let view = c!("task", "result", "worker");
            ensure!(
                named(&view["artifact_captures"], "name", "report")?["status"] == "pending",
                "pending capture"
            );
            reject!(
                "task",
                "artifact-collect",
                "worker",
                "held-report",
                "report.bin",
                "--requirement",
                "report"
            );
            let before = fs::read(&journal)?;
            ensure!(
                has(
                    &reject!("task", "verify", "worker", "baseline"),
                    "outstanding check executions"
                ) && fs::read(&journal)? == before,
                "verify ignored pending execution"
            );
            reject!(
                "task",
                "check",
                "worker",
                "steal-held",
                "--source",
                "baseline",
                "--artifact",
                "report=held-report",
                "--",
                "/usr/bin/true"
            );
            let later = c!(
                "task",
                "check",
                "worker",
                "capture-later",
                "--source",
                "baseline",
                "--artifact",
                "report=later-report",
                "--",
                "/usr/bin/true"
            );
            ensure!(
                later["passed"] == true
                    && later["check"]["artifact_problems"].get("report").is_some()
                    && caller.child.0.try_wait()?.is_none(),
                "new failure while older held"
            );
            fs::write(&gate, b"")?;
            let output = caller.finish(Duration::from_secs(10))?;
            ensure!(
                output["check"]["artifact_problems"]
                    .as_object()
                    .context("problems")?
                    .is_empty(),
                "older successful capture"
            );
            let view = c!("task", "result", "worker");
            ensure!(
                named(&view["required_artifacts"], "name", "report")?["artifact"] == "held-report"
                    && named(&view["artifact_captures"], "name", "report")?["check"]
                        == "capture-later"
                    && arr(&view["blockers"])?.contains(&json!("artifact-capture-failed")),
                "late older capture hid newer failure"
            );
            Ok(())
        })();
        fs::write(&gate, b"")?;
        if caller.child.0.try_wait()?.is_none()
            && let Err(e) = caller.finish(Duration::from_secs(12))
        {
            caller.kill()?;
            return Err(e);
        }
        held?;
        // Model-level pre-publication crash; real caller death is covered by zor_checks.rs.
        let mut abandoned: Value = serde_json::from_slice(&fs::read(&journal)?)?;
        for (k, v) in [
            ("phase", json!("submitted")),
            ("exit_code", Value::Null),
            ("signal", Value::Null),
            ("stdout", json!("")),
            ("stderr", json!("")),
            ("truncated", json!(false)),
            ("problem", Value::Null),
            ("artifact_problems", json!({})),
        ] {
            abandoned["checks"]["capture-later"][k] = v;
        }
        fs::write(&journal, serde_json::to_vec(&abandoned)?)?;
        ensure!(
            c!("task", "recover")["recovered_checks"] == 1,
            "capture crash recovery count"
        );
        let recovered = c!("task", "check-inspect", "capture-later")["check"].clone();
        ensure!(
            recovered["phase"] == "uncertain"
                && has(&recovered["artifact_problems"]["report"], "runner lost"),
            "lost runner capture problem"
        );
        let state: Value = serde_json::from_slice(&fs::read(&journal)?)?;
        ensure!(
            state["artifacts"].get("later-report").is_none(),
            "recovery invented artifact"
        );
        reject!(
            "task",
            "check",
            "worker",
            "steal-abandoned",
            "--source",
            "baseline",
            "--artifact",
            "report=later-report",
            "--",
            "/usr/bin/true"
        );
        let original = fs::read(&journal)?;
        let gate = root.path().join("capacity-release");
        let started = root.path().join("capacity-started");
        let cap_args = [
            "task",
            "check",
            "worker",
            "capture-capacity",
            "--source",
            "baseline",
            "--timeout-ms",
            "20000",
            "--artifact",
            "report=capacity-report",
            "--",
            exe,
            "fixture-worker",
            "source-capacity",
            s(&started)?,
            s(&gate)?,
        ];
        let mut caller = h.spawn(&cap_args)?;
        let cap = (|| -> Result<()> {
            until(Duration::from_secs(8), || {
                Ok(started.exists().then_some(()))
            })?;
            let mut capacity: Value = serde_json::from_slice(&fs::read(&journal)?)?;
            let mut template = capacity["checks"]["verified-input"].clone();
            for (k, v) in [
                ("phase", json!("submitted")),
                ("exit_code", Value::Null),
                ("signal", Value::Null),
                ("stdout", json!("")),
                ("stderr", json!("")),
                ("truncated", json!(false)),
                ("problem", Value::Null),
                ("requirement", Value::Null),
                ("artifact_requests", json!({})),
                ("artifact_problems", json!({})),
            ] {
                template[k] = v;
            }
            let generation = n(&capacity["generation"])?;
            for i in 0..52 {
                let mut v = template.clone();
                v["id"] = json!(format!("capacity-{i}"));
                v["created_generation"] = json!(generation + i + 1);
                v["argv"] = json!(["/usr/bin/true", "x", "x", "x", "x"]);
                capacity["checks"][format!("capacity-{i}")] = v;
            }
            capacity["generation"] = json!(generation + 52);
            let reserve = 53 * 65536 + 300000;
            for i in 0..52 {
                for arg in 1..5 {
                    let room = (4usize * 1024 * 1024)
                        .checked_sub(reserve + serde_json::to_vec(&capacity)?.len())
                        .context("capacity overflow")?;
                    let text = capacity["checks"][format!("capacity-{i}")]["argv"][arg]
                        .as_str()
                        .context("argv")?
                        .to_owned()
                        + &"x".repeat(room.min(3999));
                    capacity["checks"][format!("capacity-{i}")]["argv"][arg] = json!(text);
                }
            }
            let encoded = serde_json::to_vec(&capacity)?;
            ensure!(
                encoded.len() + reserve == 4 * 1024 * 1024,
                "exact artifact/check reservation boundary"
            );
            fs::write(&journal, encoded)?;
            c!("task", "check-inspect", "capture-capacity");
            let full = fs::read(&journal)?;
            reject!(
                "task",
                "artifact-collect",
                "worker",
                "steal-space",
                "source"
            );
            ensure!(fs::read(&journal)? == full, "stole reserved artifact space");
            fs::write(&gate, b"")?;
            let result = caller.finish(Duration::from_secs(10))?;
            ensure!(
                result["passed"] == true
                    && result["check"]["artifact_problems"]
                        .as_object()
                        .context("problems")?
                        .is_empty()
                    && result["check"]["stdout"] == "\0".repeat(4096)
                    && result["check"]["stderr"] == "\0".repeat(4096),
                "maximum escaped output publication"
            );
            ensure!(
                c!("task", "artifact-inspect", "capacity-report")["artifact"]["bytes"]
                    == json!(vec![255u8; 65536]),
                "maximum artifact publication"
            );
            Ok(())
        })();
        fs::write(&gate, b"")?;
        if caller.child.0.try_wait()?.is_none()
            && let Err(e) = caller.finish(Duration::from_secs(12))
        {
            caller.kill()?;
            return Err(e);
        }
        fs::write(&journal, original)?;
        cap?;
        let before = fs::read(&journal)?;
        for mutation in ["source", "check", "reservation", "problem", "phase"] {
            let mut v: Value = serde_json::from_slice(&before)?;
            match mutation {
                "source" => v["artifacts"]["captured-report"]["source"] = json!("newer"),
                "check" => v["artifacts"]["captured-report"]["check"] = json!("verified-input"),
                "reservation" => {
                    v["checks"]["capture"]["artifact_requests"]["report"] = json!("other-id")
                }
                "problem" => {
                    v["checks"]["capture"]["artifact_problems"]["report"] =
                        json!("both collected and failed")
                }
                _ => v["checks"]["capture"]["phase"] = json!("submitted"),
            };
            fs::write(&journal, serde_json::to_vec(&v)?)?;
            restore(&journal, &before, || {
                let encoded = fs::read(&journal)?;
                reject!("task", "result", "worker");
                ensure!(
                    fs::read(&journal)? == encoded,
                    "artifact association repaired"
                );
                Ok(())
            })?;
        }
        c!(
            "task",
            "start",
            "other",
            "--title",
            "other",
            "--instance",
            instance,
            "--workspace",
            "default",
            "--cwd",
            s(root.path())?,
            "--",
            "/bin/cat"
        );
        reject!(
            "task",
            "check",
            "other",
            "wrong-task",
            "--source",
            "baseline",
            "--",
            "/usr/bin/true"
        );
        std::os::unix::fs::symlink("source", cwd.join("link"))?;
        h.git(cwd, &["-c", "core.fsmonitor=false", "add", "link"])?;
        h.git(cwd, &["-c", "core.fsmonitor=false", "commit", "-m", "link"])?;
        let before = fs::read(&journal)?;
        reject!("task", "source-collect", "worker", "symlink");
        ensure!(
            fs::read(&journal)? == before,
            "unsupported symlink partial source"
        );
        h.git(cwd, &["-c", "core.fsmonitor=false", "rm", "link"])?;
        fs::write(cwd.join("large"), vec![b'x'; 65537])?;
        h.git(cwd, &["-c", "core.fsmonitor=false", "add", "large"])?;
        h.git(
            cwd,
            &["-c", "core.fsmonitor=false", "commit", "-m", "large"],
        )?;
        let before = fs::read(&journal)?;
        reject!("task", "source-collect", "worker", "oversized");
        ensure!(fs::read(&journal)? == before, "oversized partial source");
        h.git(cwd, &["-c", "core.fsmonitor=false", "rm", "large"])?;
        for i in 0..257 {
            fs::File::create(cwd.join(format!("excess-{i}")))?;
        }
        h.git(cwd, &["-c", "core.fsmonitor=false", "add", "-A"])?;
        h.git(
            cwd,
            &["-c", "core.fsmonitor=false", "commit", "-m", "too many"],
        )?;
        let before = fs::read(&journal)?;
        reject!("task", "source-collect", "worker", "too-many");
        ensure!(
            fs::read(&journal)? == before,
            "too many files partial source"
        );
        for mutation in [
            "task",
            "path",
            "generation",
            "size",
            "same-length-bytes",
            "rename",
            "mode",
            "commit",
        ] {
            let mut v: Value = serde_json::from_slice(&before)?;
            let item = &mut v["sources"]["baseline"];
            match mutation {
                "task" => item["task"] = json!("other"),
                "path" => item["files"][0]["path"] = json!("../outside"),
                "generation" => item["created_generation"] = json!(0),
                "size" => item["files"][0]["bytes"] = json!(vec![0; 65537]),
                "same-length-bytes" => {
                    item["files"][0]["bytes"][0] = json!(n(&item["files"][0]["bytes"][0])? ^ 1)
                }
                "rename" => item["files"][0]["path"] = json!("renamed"),
                "mode" => {
                    item["files"][0]["executable"] =
                        json!(!item["files"][0]["executable"].as_bool().context("mode")?)
                }
                _ => {
                    let bytes = item["commit_bytes"]
                        .as_array_mut()
                        .context("commit bytes")?;
                    let last = bytes.last_mut().context("commit content")?;
                    *last = json!(n(last)? ^ 1);
                }
            }
            fs::write(&journal, serde_json::to_vec(&v)?)?;
            restore(&journal, &before, || {
                let encoded = fs::read(&journal)?;
                reject!("task", "source-inspect", "baseline");
                ensure!(
                    fs::read(&journal)? == encoded,
                    "invalid retained source repaired"
                );
                Ok(())
            })?;
        }
        let repo256 = root.path().join("repo256");
        fs::create_dir(&repo256)?;
        h.git(&repo256, &["init", "--object-format=sha256", "-b", "main"])?;
        h.git(&repo256, &["config", "user.name", "Fixture"])?;
        h.git(
            &repo256,
            &["config", "user.email", "fixture@example.invalid"],
        )?;
        fs::write(repo256.join("sha256"), "sha256 input")?;
        h.git(&repo256, &["add", "."])?;
        h.git(&repo256, &["commit", "-m", "sha256 source"])?;
        c!(
            "worktree",
            "create",
            "tree256",
            "--repo",
            s(&repo256)?,
            "--branch",
            "worker256"
        );
        c!(
            "task",
            "start",
            "worker256",
            "--title",
            "sha256",
            "--instance",
            instance,
            "--workspace",
            "default",
            "--worktree",
            "tree256",
            "--",
            "/bin/cat"
        );
        let source256 = c!("task", "source-collect", "worker256", "sha256");
        ensure!(
            source256["source"]["commit"]
                .as_str()
                .context("SHA256 commit")?
                .len()
                == 64,
            "SHA256 source"
        );
        let before = fs::read(&journal)?;
        ensure!(
            has(
                &reject!("task", "verify", "worker256", "sha256"),
                "at least one declared check"
            ) && fs::read(&journal)? == before,
            "no-check verification"
        );
        c!(
            "task",
            "require-check",
            "worker256",
            "read",
            "--",
            "/bin/cat",
            "sha256"
        );
        c!("task", "require-artifact", "worker256", "input", "sha256");
        ensure!(
            c!(
                "task",
                "check",
                "worker256",
                "sha256-check",
                "--source",
                "sha256",
                "--",
                "/bin/cat",
                "sha256"
            )["check"]["stdout"]
                == "sha256 input",
            "SHA256 materialization"
        );
        ensure!(
            has(
                &reject!("task", "verify", "worker256", "sha256"),
                "has no execution"
            ),
            "unbound check satisfies requirement"
        );
        let bound = [
            "--source",
            "sha256",
            "--requirement",
            "read",
            "--artifact",
            "input=sealed-input",
            "--",
            "/bin/cat",
            "sha256",
        ];
        let mut args = vec!["task", "check", "worker256", "seal-check"];
        args.extend(bound);
        h.cli(&args, true)?;
        c!(
            "task",
            "check",
            "worker256",
            "unbound-latest",
            "--requirement",
            "read",
            "--",
            "/bin/cat",
            "sha256"
        );
        ensure!(
            has(
                &reject!("task", "verify", "worker256", "sha256"),
                "latest required check"
            ),
            "older bound check hides newer unbound"
        );
        let recovery = [
            "--source",
            "sha256",
            "--requirement",
            "read",
            "--artifact",
            "input=recovered-input",
            "--",
            "/bin/cat",
            "sha256",
        ];
        let mut args = vec!["task", "check", "worker256", "seal-recovery"];
        args.extend(recovery);
        h.cli(&args, true)?;
        let original = fs::read(&journal)?;
        restore(&journal, &original, || {
            c!(
                "task",
                "artifact-collect",
                "worker256",
                "manual-newer",
                "sha256",
                "--requirement",
                "input"
            );
            ensure!(
                has(
                    &reject!("task", "verify", "worker256", "sha256"),
                    "latest retained artifact"
                ),
                "manual artifact replaced check capture"
            );
            Ok(())
        })?;
        c!(
            "task",
            "prepare",
            "worker256",
            "--operation",
            "seal-pending",
            "--text",
            "pending",
            "--timeout-ms",
            "60000"
        );
        ensure!(
            has(
                &reject!("task", "verify", "worker256", "sha256"),
                "unresolved prompt coordination"
            ),
            "verified unresolved prompt"
        );
        c!("task", "abandon", "seal-pending");
        let fresh = c!(
            "task",
            "prepare",
            "worker256",
            "--operation",
            "seal-response",
            "--text",
            "response fixture"
        );
        c!("task", "submit", "seal-response");
        let delivered = until(Duration::from_secs(5), || {
            let v = c!("task", "reconcile", "seal-response");
            Ok((v["delivery"] == "delivered").then_some(v))
        })?;
        c!(
            "task",
            "report",
            "seal-response",
            "--token",
            fresh["report_token"].as_str().context("token")?,
            "--producer",
            "seal-fixture",
            "--sequence",
            "1",
            "--input-operation",
            &delivered["receipt"]["operation"].to_string(),
            "--kind",
            "response-observed"
        );
        ensure!(
            c!("task", "wait", "seal-response")["wait"] == "response-observed",
            "bound response"
        );
        let before = fs::read(&journal)?;
        reject!("task", "verify", "worker256", "baseline");
        ensure!(
            fs::read(&journal)? == before,
            "foreign source verification mutated"
        );
        let sealed = c!("task", "verify", "worker256", "sha256")["verification"].clone();
        ensure!(
            sealed["checks"] == json!({"read":"seal-recovery"})
                && sealed["artifacts"] == json!({"input":"recovered-input"})
                && sealed["source"] == "sha256",
            "sealed input selection"
        );
        let before = fs::read(&journal)?;
        ensure!(
            c!("task", "verify", "worker256", "sha256")["verification"] == sealed
                && fs::read(&journal)? == before,
            "seal replay mutation"
        );
        let view = c!("task", "result", "worker256");
        ensure!(
            view["verification"]["status"] == "verified"
                && view["verification"]["record"] == sealed,
            "verified result"
        );
        let state: Value = serde_json::from_slice(&before)?;
        ensure!(
            state["prompts"]["seal-response"]["released"] == true
                && state["prompts"]["seal-response"]["wait"] == "response-observed",
            "seal coordination release"
        );
        let mut after_check = vec!["task", "check", "worker256", "after-seal"];
        after_check.extend(recovery);
        for args in [
            vec!["task", "verify", "worker256", "baseline"],
            vec!["task", "cancel", "worker256"],
            vec!["task", "source-collect", "worker256", "after-seal"],
            after_check,
            vec![
                "task",
                "artifact-collect",
                "worker256",
                "after-seal",
                "sha256",
            ],
        ] {
            h.cli(&args, false)?;
            ensure!(fs::read(&journal)? == before, "sealed task mutation");
        }
        for mutation in [
            "missing-record",
            "check",
            "artifact",
            "source",
            "generation",
            "outcome",
        ] {
            let mut v: Value = serde_json::from_slice(&before)?;
            match mutation {
                "missing-record" => {
                    v["verifications"]
                        .as_object_mut()
                        .unwrap()
                        .remove("worker256");
                }
                "check" => v["verifications"]["worker256"]["checks"]["read"] = json!("seal-check"),
                "artifact" => {
                    v["verifications"]["worker256"]["artifacts"]["input"] = json!("sealed-input")
                }
                "source" => v["verifications"]["worker256"]["source"] = json!("baseline"),
                "generation" => v["verifications"]["worker256"]["created_generation"] = json!(1),
                _ => v["tasks"]["worker256"]["outcome"] = json!("open"),
            };
            fs::write(&journal, serde_json::to_vec(&v)?)?;
            restore(&journal, &before, || {
                let encoded = fs::read(&journal)?;
                reject!("task", "result", "worker256");
                ensure!(
                    fs::read(&journal)? == encoded,
                    "forged verification repaired"
                );
                Ok(())
            })?;
        }
        h.stopped("worker256")?;
        c!("worktree", "remove", "tree256");
        ensure!(
            c!("task", "verify", "worker256", "sha256")["verification"] == sealed
                && c!("task", "result", "worker256")["verification"]["status"] == "verified",
            "verification lost after cleanup"
        );
        h.stopped("worker")?;
        ensure!(
            c!("worktree", "remove", "tree", "--force")["worktree"]["phase"] == "removed"
                && !cwd.exists(),
            "owned worktree cleanup"
        );
        ensure!(
            c!("task", "source-file", "baseline", "source")["file"]["bytes"]
                == json!(b"committed\n".to_vec())
                && h.cli(&source_args, true)?["source"] == retained["source"],
            "source lost after cleanup"
        );
        reject!("task", "source-collect", "worker", "after-cancel");
        reject!(
            "task",
            "check",
            "worker",
            "after-cancel",
            "--source",
            "baseline",
            "--",
            "/usr/bin/true"
        );
        Ok(())
    })();
    let cleanup = server.finish();
    if let Err(e) = &cleanup {
        eprintln!("server cleanup: {e:#}");
    }
    result?;
    cleanup?;
    println!(
        "PASS committed sources, isolated checks, capture bounds, sealed verification and owned cleanup"
    );
    Ok(())
}
