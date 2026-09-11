//! Real managed native worker; account-free provider protocol fixture.
use crate::support::{
    local::{Root, completed, until},
    process,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{path::Path, process::Stdio, time::Duration};

fn task_api(socket: &Path, instance: &Value, task: Value) -> Result<Value> {
    until(Duration::from_secs(5), || {
        let reply = super::zor_headless::api(
            socket,
            &json!({"v":1,"id":2,"op":"task","service_instance":instance,"task":task}),
        )?;
        if reply["status"] != "completed" && reply.to_string().contains("journal is busy") {
            return Ok(None);
        }
        ensure!(
            reply["status"] == "completed",
            "native task API failed: {reply}"
        );
        Ok(Some(reply))
    })
}

fn cli(root: &Root, zor: &Path, args: &[&str]) -> Result<Value> {
    until(Duration::from_secs(5), || {
        let mut command = root.command(zor);
        command.arg("task").args(args);
        let output = process::output(command, Duration::from_secs(10), 1024 * 1024)?;
        let error = String::from_utf8_lossy(&output.stderr);
        if !output.status.success() && error.contains("journal is busy") {
            return Ok(None);
        }
        if !output.status.success()
            && args.first() == Some(&"stop")
            && (error.contains(
                "stop requested; pane release is not yet confirmed; retry the same launch ID",
            ) || error.contains("final launch evidence unavailable"))
        {
            return Ok(None);
        }
        ensure!(output.status.success(), "native CLI {args:?}: {error}");
        Ok(Some(serde_json::from_slice(&output.stdout)?))
    })
}

pub(super) fn run(fux: &Path, zor: &Path, fixture: &Path) -> Result<()> {
    for mode in ["--keep-open", "--exit-after-turn", "--controls"] {
        run_case(fux, zor, fixture, mode)?;
    }
    Ok(())
}

struct Paused(nix::unistd::Pid);

/// Second phase of the existing unattended workflow: real native workers, then
/// ordinary source/check/artifact verification. Provider output is fixture data.
pub(super) fn run_workflow(fux: &Path, zor: &Path, fixture: &Path) -> Result<()> {
    let root = Root::new("znflow-", &["/bin/cat".into()])?;
    let mut server = root.server(fux)?;
    let journal_path = root.path().join("state/zor/journal.json");
    let read = || -> Result<Value> { Ok(serde_json::from_slice(&std::fs::read(&journal_path)?)?) };
    let mut providers = Vec::new();
    let scenario = (|| -> Result<Value> {
        let listing = completed(&root.control(), json!({"command":"list","id":1}))?;
        let instance = listing["instance"].as_str().context("instance")?;
        let repo = root.path().join("repo");
        std::fs::create_dir(&repo)?;
        for args in [
            vec!["init", "-b", "main"],
            vec!["config", "user.name", "Fixture"],
            vec!["config", "user.email", "fixture@example.invalid"],
        ] {
            let mut command = root.command(Path::new("/usr/bin/git"));
            command.arg("-C").arg(&repo).args(args);
            ensure!(
                process::output(command, Duration::from_secs(5), 65536)?
                    .status
                    .success(),
                "fixture git setup"
            );
        }
        std::fs::write(repo.join("base"), b"unchanged")?;
        for args in [vec!["add", "."], vec!["commit", "-m", "baseline"]] {
            let mut command = root.command(Path::new("/usr/bin/git"));
            command.arg("-C").arg(&repo).args(args);
            ensure!(
                process::output(command, Duration::from_secs(5), 65536)?
                    .status
                    .success(),
                "fixture git baseline"
            );
        }
        let executable = std::env::current_exe()?;
        let checker = executable.to_str().context("checker")?;
        // Establish both isolated trees before either worker starts writing.
        for name in ["alpha", "beta"] {
            let mut command = root.command(zor);
            command
                .args(["worktree", "create", name, "--repo"])
                .arg(&repo)
                .args(["--branch", name]);
            let output = process::output(command, Duration::from_secs(10), 1048576)?;
            ensure!(
                output.status.success(),
                "native worktree: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        for name in ["alpha", "beta"] {
            let marker = root.path().join(name);
            std::fs::write(root.path().join(format!("{name}.notify")), b"notify")?;
            cli(
                &root,
                zor,
                &[
                    "codex-start",
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
                    fixture.to_str().context("fixture")?,
                    marker.to_str().context("marker")?,
                    "--workflow",
                ],
            )?;
            cli(
                &root,
                zor,
                &[
                    "require-check",
                    name,
                    "content",
                    "--",
                    checker,
                    "fixture-worker",
                    "workflow-check",
                    &format!("result-{name}"),
                ],
            )?;
            cli(
                &root,
                zor,
                &["require-artifact", name, "output", "output.txt"],
            )?;
        }
        until(Duration::from_secs(10), || {
            let journal = read()?;
            Ok(["alpha", "beta"]
                .iter()
                .all(|name| journal["native_workers"][name]["ready"] == true)
                .then_some(()))
        })?;
        let mut evidence = Vec::new();
        for name in ["alpha", "beta"] {
            let pid: i32 =
                std::fs::read_to_string(root.path().join(format!("{name}.pid")))?.parse()?;
            providers.push(pid);
            let operation = format!("{name}-input");
            let text = format!("result-{name}");
            let submit = [
                "codex-submit",
                name,
                "--operation",
                &operation,
                "--text",
                &text,
                "--timeout-ms",
                "30000",
            ];
            cli(&root, zor, &submit)?;
            until(Duration::from_secs(5), || {
                Ok(root.path().join(name).exists().then_some(()))
            })?;
            let read_id = format!("{name}-read");
            cli(
                &root,
                zor,
                &["codex-reconcile", &operation, "--request", &read_id],
            )?;
            until(Duration::from_secs(5), || {
                Ok(
                    (read()?["native_turns"][&operation]["evidence"]["phase"] == "working")
                        .then_some(()),
                )
            })?;
            cli(&root, zor, &submit)?;
            let before = read()?["native_workers"][name].clone();
            cli(
                &root,
                zor,
                &[
                    "codex-interrupt",
                    &operation,
                    "--request",
                    &format!("{name}-interrupt"),
                ],
            )?;
            until(Duration::from_secs(5), || {
                Ok(
                    (read()?["native_turns"][&operation]["evidence"]["phase"] == "interrupted")
                        .then_some(()),
                )
            })?;
            let restart = format!("{name}-restart");
            cli(
                &root,
                zor,
                &["codex-recreate", &operation, "--request", &restart],
            )?;
            until(Duration::from_secs(10), || {
                Ok(
                    (read()?["native_turns"][&operation]["controls"][&restart]["phase"]
                        == "observed")
                        .then_some(()),
                )
            })?;
            cli(
                &root,
                zor,
                &["codex-recreate", &operation, "--request", &restart],
            )?;
            providers
                .push(std::fs::read_to_string(root.path().join(format!("{name}.pid")))?.parse()?);
            let after = read()?["native_workers"][name].clone();
            ensure!(
                before["producer"] != after["producer"]
                    && before["thread"] == after["thread"]
                    && before["storage"] == after["storage"],
                "native recreation identity"
            );
            let resumed_read = format!("{name}-resumed-read");
            cli(
                &root,
                zor,
                &["codex-reconcile", &operation, "--request", &resumed_read],
            )?;
            until(Duration::from_secs(5), || {
                Ok(
                    (read()?["native_turns"][&operation]["controls"][&resumed_read]["phase"]
                        == "observed")
                        .then_some(()),
                )
            })?;
            ensure!(
                std::fs::read(root.path().join(name))? == text.as_bytes(),
                "native replay or changed input"
            );
            ensure!(
                cli(&root, zor, &["result", name])?["verification"]["status"] == "unverified",
                "native interruption inferred verification"
            );
            let source = format!("{name}-source");
            let check = format!("{name}-check");
            // Seal the fixture-generated output into the disposable source tree,
            // just as the first workflow phase's scripted worker does.
            let cwd: std::path::PathBuf =
                serde_json::from_value(read()?["launches"][name]["cwd"].clone())?;
            for args in [
                vec!["add", "output.txt"],
                vec!["commit", "-m", "native fixture output"],
            ] {
                let mut command = root.command(Path::new("/usr/bin/git"));
                command.arg("-C").arg(&cwd).args(args);
                ensure!(
                    process::output(command, Duration::from_secs(5), 65536)?
                        .status
                        .success(),
                    "fixture output commit"
                );
            }
            cli(&root, zor, &["source-collect", name, &source])?;
            let checked = cli(
                &root,
                zor,
                &[
                    "check",
                    name,
                    &check,
                    "--source",
                    &source,
                    "--requirement",
                    "content",
                    "--artifact",
                    &format!("output={name}-artifact"),
                    "--",
                    checker,
                    "fixture-worker",
                    "workflow-check",
                    &text,
                ],
            )?;
            ensure!(
                checked["passed"] == true,
                "native output check failed: {checked}"
            );
            cli(&root, zor, &["verify", name, &source])?;
            let result = cli(&root, zor, &["result", name])?;
            ensure!(
                result["verification"]["status"] == "verified",
                "checked output not verified"
            );
            let artifact = cli(
                &root,
                zor,
                &["artifact-inspect", &format!("{name}-artifact")],
            )?;
            ensure!(
                serde_json::from_value::<Vec<u8>>(artifact["artifact"]["bytes"].clone())?
                    == text.as_bytes(),
                "retained output differs"
            );
            evidence.push(json!({"task":name,"native":cli(&root, zor, &["codex-inspect", &operation])?,"result":result,"artifact":artifact}));
        }
        for name in ["alpha", "beta"] {
            cli(&root, zor, &["stop", name])?;
        }
        ensure!(
            std::fs::read(repo.join("base"))? == b"unchanged" && !repo.join("output.txt").exists(),
            "shared repository changed"
        );
        Ok(
            json!({"kind":"native-two-worker-handoff","evidence":evidence,
            "scope":"account-free production adapter fixtures; native interruption/recreation and independently checked artifacts",
            "limitations":["live model turns and materialized real-provider resume unverified", "R6 explicitly deferred"]}),
        )
    })();
    let cleanup = server.finish();
    let child_cleanup = providers.into_iter().try_for_each(|pid| {
        until(Duration::from_secs(5), || {
            match nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None) {
                Err(nix::errno::Errno::ESRCH) => Ok(Some(())),
                Ok(()) => Ok(None),
                Err(error) => Err(error.into()),
            }
        })
    });
    cleanup?;
    child_cleanup?;
    let handoff = scenario?;
    println!(
        "NATIVE_WORKFLOW_HANDOFF {}",
        serde_json::to_string(&handoff)?
    );
    println!(
        "PASS two native workers, correlation, interruption, recreation, no replay, verified artifacts and cleanup"
    );
    Ok(())
}

impl Drop for Paused {
    fn drop(&mut self) {
        let _ = nix::sys::signal::kill(self.0, nix::sys::signal::Signal::SIGCONT);
    }
}

fn run_case(fux: &Path, zor: &Path, fixture: &Path, mode: &str) -> Result<()> {
    let controls = mode == "--controls";
    let root = Root::new("znative-", &["/bin/cat".into()])?;
    let mut server = root.server(fux)?;
    let mut service = if controls {
        Some(process::Guard(
            root.command(zor)
                .arg("serve")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()?,
        ))
    } else {
        None
    };
    let socket = root.path().join("zor/control.sock");
    let marker = root.path().join("native-input");
    let journal_path = root.path().join("state/zor/journal.json");
    let read = || -> Result<Value> { Ok(serde_json::from_slice(&std::fs::read(&journal_path)?)?) };
    let scenario = (|| -> Result<()> {
        let listing = completed(&root.control(), json!({"command":"list","id":1}))?;
        let instance = listing["instance"].as_str().context("instance")?;
        let service_instance = if controls {
            until(Duration::from_secs(5), || Ok(socket.exists().then_some(())))?;
            super::zor_headless::api(&socket, &json!({"v":1,"id":1,"op":"ping"}))?["service_instance"].clone()
        } else {
            Value::Null
        };
        cli(
            &root,
            zor,
            &[
                "codex-start",
                "native",
                "--title",
                "native",
                "--instance",
                instance,
                "--workspace",
                "default",
                "--cwd",
                root.path().to_str().context("root path")?,
                "--",
                fixture.to_str().context("fixture path")?,
                marker.to_str().context("receipt path")?,
                mode,
            ],
        )?;
        until(Duration::from_secs(10), || {
            Ok(
                (read()?.pointer("/native_workers/native/ready") == Some(&json!(true)))
                    .then_some(()),
            )
        })?;
        if controls {
            task_api(
                &socket,
                &service_instance,
                json!({"action":"codex-start","id":"native","title":"native","instance":instance,"workspace":"default","cwd":root.path(),"worktree":null,"argv":[fixture,marker,mode]}),
            )?;
        }
        ensure!(
            read()?.pointer("/native_workers/native/storage/rollout")
                == Some(&json!(
                    root.path().canonicalize()?.join("native-input.thread.json")
                )),
            "native storage identity was not retained"
        );
        let lock_file = std::fs::File::open(root.path().join("state/zor/journal.lock"))?;
        let lock = nix::fcntl::Flock::lock(lock_file, nix::fcntl::FlockArg::LockExclusive)
            .map_err(|(_, error)| error)?;
        // Normal peer contention must not terminate the owner or provider.
        std::thread::sleep(Duration::from_millis(200));
        let journal = read()?;
        let attempt = journal["tasks"]["native"]["attempt"]
            .as_str()
            .context("attempt")?;
        let session = journal["attempts"][attempt]["session"]
            .as_str()
            .context("session")?;
        let pid = nix::unistd::Pid::from_raw(i32::try_from(
            journal["sessions"][session]["target"]["pid"]
                .as_i64()
                .context("worker PID")?,
        )?);
        nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGSTOP)?;
        let paused = Paused(pid);
        until(Duration::from_secs(2), || {
            let mut command = root.command(Path::new("/bin/ps"));
            command.args(["-o", "state=", "-p", &pid.as_raw().to_string()]);
            let output = process::output(command, Duration::from_secs(1), 4096)?;
            ensure!(output.status.success(), "paused worker disappeared");
            Ok(String::from_utf8_lossy(&output.stdout)
                .contains('T')
                .then_some(()))
        })?;
        // Pausing under the test's lock proves the worker cannot hold that lock
        // while the following CLI submission needs it.
        drop(lock);
        let text = "literal\n\t$(never-execute)\\é";
        let args = [
            "codex-submit",
            "native",
            "--operation",
            "input-a",
            "--text",
            text,
            "--timeout-ms",
            "30000",
        ];
        cli(&root, zor, &args)?;
        ensure!(
            read()?.pointer("/native_turns/input-a/evidence/phase") == Some(&json!("prepared")),
            "fixture failed to pause before submission"
        );
        std::fs::write(marker.with_file_name("native-input.notify"), b"notify")?;
        until(Duration::from_secs(5), || {
            Ok(marker
                .with_file_name("native-input.notified")
                .exists()
                .then_some(()))
        })?;
        drop(paused);
        if controls {
            until(Duration::from_secs(5), || Ok(marker.exists().then_some(())))?;
            std::thread::sleep(Duration::from_millis(150));
            ensure!(
                read()?.pointer("/native_turns/input-a/evidence/phase")
                    == Some(&json!("submitted")),
                "unsolicited history refreshed native evidence"
            );
            let reconcile = json!({"action":"codex-reconcile","operation":"input-a","request":"read-a","timeout_ms":30000});
            task_api(&socket, &service_instance, reconcile.clone())?;
            task_api(&socket, &service_instance, reconcile)?;
            until(Duration::from_secs(5), || {
                Ok((read()?.pointer("/native_turns/input-a/evidence/phase")
                    == Some(&json!("working")))
                .then_some(()))
            })?;
            task_api(
                &socket,
                &service_instance,
                json!({"action":"codex-submit","id":"native","operation":"input-a","text":text,"timeout_ms":30000}),
            )?;
            task_api(
                &socket,
                &service_instance,
                json!({"action":"cancel","id":"native"}),
            )?;
            let interrupt = json!({"action":"codex-interrupt","operation":"input-a","request":"interrupt-a","timeout_ms":30000});
            task_api(&socket, &service_instance, interrupt.clone())?;
            task_api(&socket, &service_instance, interrupt)?;
        }
        until(Duration::from_secs(10), || {
            Ok((read()?.pointer("/native_turns/input-a/evidence/phase")
                == Some(&json!(if controls { "interrupted" } else { "completed" })))
            .then_some(()))
        })?;
        if controls {
            task_api(
                &socket,
                &service_instance,
                json!({"action":"codex-inspect","operation":"input-a"}),
            )?;
            ensure!(
                marker.with_file_name("native-input.read").exists()
                    && marker.with_file_name("native-input.interrupt").exists(),
                "native controls were not sent"
            );
            let before = read()?;
            let original = before
                .pointer("/native_workers/native")
                .context("original owner")?;
            let recreate = json!({"action":"codex-recreate","operation":"input-a","request":"recreate-a","timeout_ms":30000});
            task_api(&socket, &service_instance, recreate.clone())?;
            until(Duration::from_secs(10), || {
                Ok(
                    (read()?.pointer("/native_turns/input-a/controls/recreate-a/phase")
                        == Some(&json!("observed")))
                    .then_some(()),
                )
            })?;
            task_api(&socket, &service_instance, recreate)?;
            let after = read()?;
            let replacement = after
                .pointer("/native_workers/native")
                .context("replacement owner")?;
            ensure!(
                original["producer"] != replacement["producer"] && replacement["ready"] == true,
                "provider recreation did not publish a new producer"
            );
            for field in ["thread", "storage", "attempt", "marker"] {
                ensure!(
                    original[field] == replacement[field],
                    "recreation changed {field}"
                );
            }
            ensure!(
                before["sessions"] == after["sessions"],
                "recreation replaced the multiplexer session"
            );
            ensure!(
                marker.with_file_name("native-input.resumed").exists(),
                "native resume was not exercised"
            );
            let reconcile = json!({"action":"codex-reconcile","operation":"input-a","request":"read-resumed","timeout_ms":30000});
            task_api(&socket, &service_instance, reconcile.clone())?;
            until(Duration::from_secs(5), || {
                Ok(
                    (read()?.pointer("/native_turns/input-a/controls/read-resumed/phase")
                        == Some(&json!("observed")))
                    .then_some(()),
                )
            })?;
            task_api(&socket, &service_instance, reconcile)?;
            ensure!(
                marker.with_file_name("native-input.resumed-read").exists(),
                "resumed native history was not read"
            );
            let overview = task_api(&socket, &service_instance, json!({"action":"overview"}))?;
            let native = overview
                .pointer("/value/native_integrations/0")
                .context("native overview")?;
            ensure!(
                native["operation"] == "input-a"
                    && native["phase"] == "interrupted"
                    && native["producer"] == replacement["producer"]
                    && native["fresh"] == true,
                "native overview lost resumed evidence: {overview}"
            );
        }
        cli(&root, zor, &args)?;
        let retained = cli(&root, zor, &["codex-inspect", "input-a"])?;
        if controls {
            ensure!(
                retained.pointer("/operation/evidence/interrupt_acknowledged")
                    == Some(&json!(true)),
                "native interrupt acknowledgement missing"
            );
            ensure!(
                retained.pointer("/operation/evidence/responses") == Some(&json!({})),
                "interrupted partial snapshot inferred a completed response"
            );
        } else {
            ensure!(
                retained.pointer("/operation/evidence/responses/fixture-answer/text")
                    == Some(&json!("native fixture response")),
                "native response missing: {retained}"
            );
        }
        ensure!(
            std::fs::read(&marker)? == text.as_bytes(),
            "native input was not literal"
        );
        ensure!(
            read()?.pointer("/tasks/native/outcome")
                == Some(&json!(if controls { "cancelled" } else { "open" })),
            "native response inferred task success"
        );
        if controls {
            ensure!(
                read()?.pointer("/native_workers/native/ready") == Some(&json!(true)),
                "coordination cancel or native interrupt stopped the worker"
            );
        }
        cli(&root, zor, &["stop", "native"])?;
        Ok(())
    })();
    let service_cleanup = (|| -> Result<()> {
        if let Some(service) = &mut service {
            use crate::support::process::OwnedProcess;
            service.0.terminate()?;
            ensure!(
                process::wait(&mut service.0, Duration::from_secs(5))?.success(),
                "native service exit"
            );
        }
        Ok(())
    })();
    let cleanup = server.finish();
    let child_cleanup = (|| -> Result<()> {
        let path = marker.with_file_name("native-input.pid");
        if path.exists() {
            let pid: i32 = std::fs::read_to_string(path)?.parse()?;
            until(Duration::from_secs(5), || {
                match nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None) {
                    Err(nix::errno::Errno::ESRCH) => Ok(Some(())),
                    Ok(()) => Ok(None),
                    Err(error) => Err(error.into()),
                }
            })?;
        }
        Ok(())
    })();
    let errors: Vec<_> = [scenario, service_cleanup, cleanup, child_cleanup]
        .into_iter()
        .filter_map(Result::err)
        .map(|error| format!("{error:#}"))
        .collect();
    ensure!(errors.is_empty(), "{}", errors.join("; "));
    println!(
        "PASS native managed ownership, literal submission, correlated response, no replay, separate task outcome and child cleanup (fixture)"
    );
    Ok(())
}
