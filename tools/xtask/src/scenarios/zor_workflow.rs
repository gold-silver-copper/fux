//! Two scripted workers, rejected dependency, repair, sealed handoff and owned cleanup.
use crate::support::{
    contention::{RealClock, api_busy, cli_busy, retry_busy},
    local::{Root, completed, until},
    process::{self, Guard, OwnedProcess},
    service,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::Path,
    process::Stdio,
    time::{Duration, Instant},
};

pub(super) fn run(fux: &Path, zor: &Path, native_fixture: &Path) -> Result<()> {
    let root = Root::new("zflow-rs-", &["/bin/cat".into()])?;
    let mut server = root.server(fux)?;
    let mut owner: Option<Guard> = None;
    let scenario = (|| -> Result<()> {
        let instance = completed(&root.control(), json!({"id":1,"command":"list"}))?["instance"]
            .as_str()
            .context("instance")?
            .to_owned();
        let task_root = root.path().join("task-journal");
        let state = task_root.to_str().context("state")?;
        let journal = task_root.join("journal.json");
        let raw = |args: &[&str], timeout| {
            let mut command = root.command(zor);
            command.args(["--state-directory", state]).args(args);
            process::output(command, timeout, 1024 * 1024)
        };
        // Background journal activity (the service scan) can reject admission with Busy.
        // zor's own contract says an operation ID may be retried; only such commands and
        // read-only inspections are replayed, with the identical arguments, for a bounded time.
        let cli = |args: &[&str], ok: bool| -> Result<Value> {
            let replayable = ok
                && (args.contains(&"--operation")
                    || matches!(
                        args.get(1).copied(),
                        Some("inspect" | "artifact-inspect" | "adapter-capabilities")
                    ));
            let reply = retry_busy(
                |_| raw(args, Duration::from_secs(12)),
                |reply| Ok(cli_busy(reply.status.code(), &reply.stderr)),
                Instant::now() + Duration::from_secs(5),
                replayable,
                &RealClock,
            )?;
            ensure!(
                reply.status.success() == ok,
                "CLI {args:?}: {} {}",
                String::from_utf8_lossy(&reply.stdout),
                String::from_utf8_lossy(&reply.stderr)
            );
            if ok {
                Ok(serde_json::from_slice(&reply.stdout)?)
            } else {
                Ok(String::from_utf8(reply.stderr)?.into())
            }
        };
        let git = |path: &Path, args: &[&str]| -> Result<()> {
            let mut command = root.command(Path::new("/usr/bin/git"));
            command.arg("-C").arg(path).args(args);
            let result = process::output(command, Duration::from_secs(5), 1024 * 1024)?;
            ensure!(
                result.status.success(),
                "fixture git: {}",
                String::from_utf8_lossy(&result.stderr)
            );
            Ok(())
        };
        let mut capabilities = BTreeMap::new();
        for agent in ["opencode", "claude", "codex"] {
            let value = cli(&["task", "adapter-capabilities", "--agent", agent], true)?;
            ensure!(
                value["verified_task_completion"] == false
                    && value["availability"] == "not-probed"
                    && value["capabilities"]["native-interrupt"]["level"]
                        == if agent == "codex" {
                            "native"
                        } else {
                            "unavailable"
                        },
                "capability overclaim"
            );
            if agent == "claude" {
                ensure!(
                    value["capabilities"]["native-correlation"]["level"] == "unavailable",
                    "native correlation overclaim"
                );
            }
            capabilities.insert(agent, value);
        }
        ensure!(
            cli(
                &["task", "adapter-capabilities", "--agent", "fixture"],
                false
            )?
            .as_str()
            .context("unsupported error")?
            .contains("unsupported-agent:"),
            "wrong unsupported agent error"
        );
        let repo = root.path().join("repo");
        fs::create_dir(&repo)?;
        git(&repo, &["init", "-b", "main"])?;
        git(&repo, &["config", "user.name", "Fixture"])?;
        git(&repo, &["config", "user.email", "fixture@example.invalid"])?;
        fs::write(repo.join("base"), "shared baseline")?;
        git(&repo, &["add", "."])?;
        git(&repo, &["commit", "-m", "baseline"])?;
        let executable = std::env::current_exe()?;
        let worker = executable.to_str().context("fixture executable")?;
        let mut trees = BTreeMap::new();
        for name in ["alpha", "beta"] {
            let tree = cli(
                &[
                    "worktree",
                    "create",
                    name,
                    "--repo",
                    repo.to_str().context("repo")?,
                    "--branch",
                    name,
                ],
                true,
            )?;
            trees.insert(
                name,
                std::path::PathBuf::from(tree["worktree"]["path"].as_str().context("tree path")?),
            );
            cli(
                &[
                    "task",
                    "start",
                    name,
                    "--title",
                    name,
                    "--instance",
                    &instance,
                    "--workspace",
                    "default",
                    "--worktree",
                    name,
                    "--",
                    worker,
                    "fixture-worker",
                    "workflow",
                    root.path()
                        .join(format!("{name}-done"))
                        .to_str()
                        .context("marker")?,
                ],
                true,
            )?;
            cli(
                &[
                    "task",
                    "require-check",
                    name,
                    "content",
                    "--",
                    worker,
                    "fixture-worker",
                    "workflow-check",
                    &format!("result-{name}"),
                ],
                true,
            )?;
            cli(
                &["task", "require-artifact", name, "output", "output.txt"],
                true,
            )?;
        }
        ensure!(
            trees["alpha"] != trees["beta"] && !repo.join("output.txt").exists(),
            "worktree isolation"
        );
        cli(
            &[
                "task",
                "start",
                "lead",
                "--title",
                "lead",
                "--instance",
                &instance,
                "--workspace",
                "default",
                "--cwd",
                root.path().to_str().context("root")?,
                "--",
                "/bin/cat",
            ],
            true,
        )?;
        let instruction =
            "Review both retained outputs; do not treat worker claims as verification.";
        let handoff = vec![
            "task",
            "handoff",
            "lead",
            "--operation",
            "merge-results",
            "--from",
            "alpha",
            "--from",
            "beta",
            "--text",
            instruction,
            "--timeout-ms",
            "60000",
        ];
        let before = fs::read(&journal)?;
        cli(&handoff, false)?;
        ensure!(
            fs::read(&journal)? == before,
            "unverified handoff wrote journal"
        );
        let send = |name, operation, text| -> Result<()> {
            cli(
                &[
                    "task",
                    "prepare",
                    name,
                    "--operation",
                    operation,
                    "--text",
                    text,
                    "--timeout-ms",
                    "60000",
                ],
                true,
            )?;
            cli(&["task", "submit", operation], true)?;
            Ok(())
        };
        let response = |name: &str, operation: &str, expected: &str| -> Result<()> {
            let deadline = Instant::now() + Duration::from_secs(8);
            let marker = root.path().join(format!("{name}-done"));
            loop {
                match fs::read_to_string(&marker) {
                    Ok(text) if text == expected => break,
                    Ok(_) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                }
                ensure!(Instant::now() < deadline, "worker completion deadline");
                std::thread::sleep(Duration::from_millis(20));
            }
            let prompt = loop {
                let prompt = cli(&["task", "reconcile", operation], true)?;
                if prompt["delivery"] == "delivered" {
                    break prompt;
                }
                ensure!(Instant::now() < deadline, "worker delivery deadline");
                std::thread::sleep(Duration::from_millis(20));
            };
            // Explicitly attributed fixture report, never a native provider guarantee.
            cli(
                &[
                    "task",
                    "report",
                    operation,
                    "--token",
                    prompt["report_token"].as_str().context("report token")?,
                    "--producer",
                    &format!("{name}-fixture"),
                    "--sequence",
                    "1",
                    "--input-operation",
                    &prompt["receipt"]["operation"].to_string(),
                    "--kind",
                    "response-observed",
                ],
                true,
            )?;
            ensure!(
                cli(&["task", "wait", operation], true)?["wait"] == "response-observed",
                "worker response wait"
            );
            Ok(())
        };
        let check = |name: &str, source: &str, execution: &str| -> Result<Value> {
            cli(&["task", "source-collect", name, source], true)?;
            cli(
                &[
                    "task",
                    "check",
                    name,
                    execution,
                    "--source",
                    source,
                    "--requirement",
                    "content",
                    "--artifact",
                    &format!("output={execution}-output"),
                    "--",
                    worker,
                    "fixture-worker",
                    "workflow-check",
                    &format!("result-{name}"),
                ],
                true,
            )
        };
        send("alpha", "alpha-work", "result-alpha")?;
        send("beta", "beta-work", "incorrect-beta")?;
        response("alpha", "alpha-work", "result-alpha")?;
        response("beta", "beta-work", "incorrect-beta")?;
        ensure!(
            check("alpha", "alpha-source", "alpha-check")?["passed"] == true,
            "alpha check"
        );
        cli(&["task", "verify", "alpha", "alpha-source"], true)?;
        ensure!(
            check("beta", "beta-bad-source", "beta-bad-check")?["passed"] == false,
            "bad beta passed"
        );
        let before = fs::read(&journal)?;
        cli(&handoff, false)?;
        cli(&["task", "verify", "beta", "beta-bad-source"], false)?;
        ensure!(
            fs::read(&journal)? == before,
            "rejected verification wrote journal"
        );
        send("beta", "beta-repair", "result-beta")?;
        response("beta", "beta-repair", "result-beta")?;
        ensure!(
            check("beta", "beta-source", "beta-check")?["passed"] == true,
            "repair failed"
        );
        cli(&["task", "verify", "beta", "beta-source"], true)?;
        owner = Some(Guard(
            root.command(zor)
                .args(["--state-directory", state, "serve"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()?,
        ));
        let endpoint = root.path().join("zor/control.sock");
        until(Duration::from_secs(8), || {
            ensure!(
                owner.as_mut().context("service")?.0.try_wait()?.is_none(),
                "service startup exit"
            );
            Ok(endpoint.exists().then_some(()))
        })?;
        let exchange = |request: &Value, timeout| service::rpc(&endpoint, request, timeout);
        let service_instance = exchange(
            &json!({"v":1,"id":1,"op":"ping"}),
            Duration::from_secs(5),
        )?["service_instance"]
            .clone();
        let before = fs::read(&journal)?;
        for (agent, expected) in &capabilities {
            let reply = exchange(
                &json!({"v":1,"id":3,"op":"task","service_instance":service_instance,"task":{"action":"adapter-capabilities","agent":agent}}),
                Duration::from_secs(5),
            )?;
            ensure!(
                reply["status"] == "completed" && reply["value"] == *expected,
                "capability CLI/API mismatch"
            );
        }
        let rejected = exchange(
            &json!({"v":1,"id":4,"op":"task","service_instance":service_instance,"task":{"action":"adapter-capabilities","agent":"fixture"}}),
            Duration::from_secs(5),
        )?;
        ensure!(
            rejected["status"] == "failed" && rejected.to_string().contains("unsupported-agent:"),
            "API unsupported agent error"
        );
        ensure!(
            fs::read(&journal)? == before,
            "capability API wrote journal"
        );
        let request = json!({"v":1,"id":2,"op":"task","service_instance":service_instance,"task":{"action":"handoff","id":"lead","operation":"merge-results","predecessors":["alpha","beta"],"text":instruction,"timeout_ms":60000}});
        let reply = retry_busy(
            |remaining| exchange(&request, remaining.min(Duration::from_secs(5))),
            |reply| api_busy(reply, &request),
            Instant::now() + Duration::from_secs(12),
            true,
            &RealClock,
        )?;
        ensure!(reply["status"] == "completed", "handoff API: {reply}");
        let prepared = reply["value"].clone();
        ensure!(
            cli(&handoff, true)? == prepared
                && prepared["delivery"] == "prepared"
                && prepared["receipt"].is_null(),
            "handoff preparation parity"
        );
        let payload: Value =
            serde_json::from_str(prepared["text"].as_str().context("payload text")?)?;
        ensure!(
            payload["kind"] == "zor-verified-handoff" && payload["destination"] == "lead",
            "payload destination"
        );
        ensure!(
            payload["evidence_cli_prefix"]
                == json!([
                    "zor",
                    "--state-directory",
                    task_root.canonicalize()?,
                    "task"
                ]),
            "evidence prefix"
        );
        let prefix: Vec<String> = serde_json::from_value(payload["evidence_cli_prefix"].clone())?;
        let mut access = root.command(zor);
        access
            .args(&prefix[1..])
            .args(["artifact-inspect", "beta-check-output"]);
        let accessed = process::output(access, Duration::from_secs(5), 1024 * 1024)?;
        ensure!(accessed.status.success(), "artifact prefix not usable");
        let value: Value = serde_json::from_slice(&accessed.stdout)?;
        ensure!(
            serde_json::from_value::<Vec<u8>>(value["artifact"]["bytes"].clone())?
                == b"result-beta",
            "artifact payload"
        );
        let predecessors = payload["predecessors"].as_array().context("predecessors")?;
        ensure!(
            predecessors
                .iter()
                .map(|i| i["task"].clone())
                .collect::<Vec<_>>()
                == vec![json!("alpha"), json!("beta")]
                && predecessors[1]["source"] == "beta-source",
            "predecessor order/source"
        );
        let generations: serde_json::Map<_, _> = predecessors
            .iter()
            .map(|i| {
                Ok((
                    i["task"].as_str().context("task")?.to_owned(),
                    i["created_generation"].clone(),
                ))
            })
            .collect::<Result<_>>()?;
        ensure!(
            prepared["handoff"]["predecessors"] == Value::Object(generations),
            "sealed generation mismatch"
        );
        let before = fs::read(&journal)?;
        ensure!(cli(&handoff, true)? == prepared, "handoff replay changed");
        let mut reordered = handoff.clone();
        reordered.swap(6, 8);
        ensure!(
            cli(&reordered, true)? == prepared && fs::read(&journal)? == before,
            "reordered replay mutated"
        );
        let mut changed = handoff.clone();
        changed[10] = "changed";
        let mut too_many = vec!["task", "handoff", "lead", "--operation", "too-many"];
        for _ in 0..9 {
            too_many.extend(["--from", "alpha"]);
        }
        too_many.extend(["--text", "x"]);
        for args in [
            changed,
            vec![
                "task",
                "handoff",
                "lead",
                "--operation",
                "duplicate",
                "--from",
                "alpha",
                "--from",
                "alpha",
                "--text",
                "x",
            ],
            vec![
                "task",
                "handoff",
                "alpha",
                "--operation",
                "self",
                "--from",
                "alpha",
                "--text",
                "x",
            ],
            too_many,
            vec![
                "task",
                "prepare",
                "lead",
                "--operation",
                "merge-results",
                "--text",
                prepared["text"].as_str().context("text")?,
                "--timeout-ms",
                "60000",
            ],
        ] {
            cli(&args, false)?;
            ensure!(
                fs::read(&journal)? == before,
                "rejected handoff changed journal"
            );
        }
        for mutation in ["generation", "text", "instruction", "destination"] {
            let mut invalid: Value = serde_json::from_slice(&before)?;
            match mutation {
                "generation" => {
                    let generation = invalid["prompts"]["merge-results"]["handoff"]["predecessors"]
                        ["alpha"]
                        .as_u64()
                        .context("generation")?;
                    invalid["prompts"]["merge-results"]["handoff"]["predecessors"]["alpha"] =
                        (generation + 1).into();
                }
                "text" => {
                    invalid["prompts"]["merge-results"]["text"] = format!(
                        "{} changed",
                        invalid["prompts"]["merge-results"]["text"]
                            .as_str()
                            .context("text")?
                    )
                    .into();
                }
                "instruction" => {
                    invalid["prompts"]["merge-results"]["handoff"]["instruction"] = format!(
                        "{} changed",
                        invalid["prompts"]["merge-results"]["handoff"]["instruction"]
                            .as_str()
                            .context("instruction")?
                    )
                    .into();
                }
                _ => {
                    invalid["prompts"]["merge-results"]["attempt"] =
                        invalid["tasks"]["alpha"]["attempt"].clone();
                }
            }
            let encoded = serde_json::to_vec(&invalid)?;
            fs::write(&journal, &encoded)?;
            let checked = (|| -> Result<()> {
                cli(&["task", "inspect", "lead"], false)?;
                ensure!(fs::read(&journal)? == encoded, "invalid journal mutated");
                Ok(())
            })();
            fs::write(&journal, &before)?;
            checked?;
        }
        cli(&["task", "submit", "merge-results"], true)?;
        let delivered = until(Duration::from_secs(5), || {
            let value = cli(&["task", "reconcile", "merge-results"], true)?;
            Ok((value["delivery"] == "delivered").then_some(value))
        })?;
        ensure!(
            cli(&handoff, true)? == delivered,
            "handoff replay resubmitted"
        );
        cli(
            &[
                "task",
                "report",
                "merge-results",
                "--token",
                prepared["report_token"].as_str().context("token")?,
                "--producer",
                "lead-fixture",
                "--sequence",
                "1",
                "--input-operation",
                &delivered["receipt"]["operation"].to_string(),
                "--kind",
                "response-observed",
            ],
            true,
        )?;
        ensure!(
            cli(&["task", "wait", "merge-results"], true)?["wait"] == "response-observed"
                && cli(&["task", "result", "lead"], true)?["verification"]["status"]
                    == "unverified",
            "report became verification"
        );
        let mut saved: Value = serde_json::from_slice(&fs::read(&journal)?)?;
        saved["launches"]["alpha"]["stop_requested"] = true.into();
        let candidate = journal.with_extension("fixture");
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&candidate)?;
        file.write_all(&serde_json::to_vec(&saved)?)?;
        file.sync_all()?;
        drop(file);
        fs::rename(candidate, &journal)?;
        let deadline = Instant::now() + Duration::from_secs(12);
        let recovered = loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .context("stop recovery deadline")?;
            let response = raw(
                &["task", "inspect", "alpha"],
                remaining.min(Duration::from_secs(5)),
            )?;
            if response.status.success() {
                let value: Value = serde_json::from_slice(&response.stdout)?;
                if value["launch"]["phase"] == "closed" {
                    break value;
                }
            } else {
                ensure!(
                    cli_busy(response.status.code(), &response.stderr),
                    "unexpected recovery failure: {}",
                    String::from_utf8_lossy(&response.stderr)
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        ensure!(
            recovered["task"]["outcome"] == "verified",
            "stop erased verification"
        );
        owner.as_mut().context("service")?.0.terminate()?;
        ensure!(
            process::wait(
                &mut owner.as_mut().context("service")?.0,
                Duration::from_secs(10)
            )?
            .success(),
            "service exit"
        );
        for name in ["alpha", "beta", "lead"] {
            let deadline = Instant::now() + Duration::from_secs(8);
            loop {
                let remaining = deadline
                    .checked_duration_since(Instant::now())
                    .context("stop deadline")?;
                let stopped = raw(
                    &["task", "stop", name],
                    remaining.min(Duration::from_secs(5)),
                )?;
                if stopped.status.success() {
                    break;
                }
                let error = String::from_utf8(stopped.stderr)?;
                ensure!(
                    error.contains("stop requested")
                        || error.contains("managed process lifecycle uncertain"),
                    "stop failure: {error}"
                );
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        for name in ["alpha", "beta"] {
            let untracked = trees[name].join("untracked");
            fs::write(&untracked, "preserve unless explicitly forced")?;
            cli(&["worktree", "remove", name], false)?;
            ensure!(untracked.exists(), "untracked work removed without force");
            cli(&["worktree", "remove", name, "--force"], true)?;
            let result = cli(&["task", "result", name], true)?;
            ensure!(
                result["verification"]["status"] == "verified",
                "removed checkout erased verification"
            );
            let artifact = result["verification"]["record"]["artifacts"]["output"]
                .as_str()
                .context("artifact")?;
            let value = cli(&["task", "artifact-inspect", artifact], true)?;
            ensure!(
                serde_json::from_value::<Vec<u8>>(value["artifact"]["bytes"].clone())?
                    == format!("result-{name}").as_bytes(),
                "retained artifact bytes"
            );
        }
        ensure!(
            fs::read_to_string(repo.join("base"))? == "shared baseline"
                && !repo.join("output.txt").exists(),
            "shared repository changed"
        );
        let retry = cli(&handoff, true)?;
        ensure!(
            retry["released"] == true
                && retry["wait"] == "cancelled"
                && retry["handoff"] == prepared["handoff"],
            "released handoff history"
        );
        Ok(())
    })();
    let stopped = (|| -> Result<()> {
        if let Some(owner) = owner.as_mut() {
            owner.0.terminate()?;
            ensure!(
                process::wait(&mut owner.0, Duration::from_secs(10))?.success(),
                "service cleanup"
            );
        }
        Ok(())
    })();
    let server_stopped = server.finish();
    let errors: Vec<_> = [scenario, stopped, server_stopped]
        .into_iter()
        .filter_map(Result::err)
        .map(|e| format!("{e:#}"))
        .collect();
    ensure!(errors.is_empty(), "{}", errors.join("; "));
    println!(
        "PASS two isolated workers, failed dependency repair, pinned handoff, receipt retry and owned cleanup"
    );
    super::zor_native::run_workflow(fux, zor, native_fixture)?;
    Ok(())
}
