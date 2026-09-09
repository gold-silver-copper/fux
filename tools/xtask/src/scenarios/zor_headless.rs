//! Scripted provider argv proof through real fux/zor CLI and service calls.
use crate::support::{
    local::{Root, completed, connect, until},
    process::{self, Guard, wait},
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::fs::PermissionsExt,
    path::Path,
    process::Stdio,
    time::{Duration, Instant},
};

pub(super) fn api(path: &Path, value: &Value) -> Result<Value> {
    let mut peer = connect(path, Instant::now() + Duration::from_secs(5))?;
    peer.set_read_timeout(Some(Duration::from_secs(5)))?;
    peer.set_write_timeout(Some(Duration::from_secs(5)))?;
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    peer.write_all(&bytes)?;
    let mut reader = BufReader::new(peer);
    let mut bytes = Vec::new();
    // Bound each response without waiting on an inherited pipe or EOF.
    loop {
        let chunk = reader.fill_buf()?;
        ensure!(!chunk.is_empty(), "service reply EOF");
        let end = chunk.iter().position(|byte| *byte == b'\n');
        let count = end.map_or(chunk.len(), |end| end + 1);
        ensure!(bytes.len() + count <= 512 * 1024, "service reply bound");
        bytes.extend_from_slice(&chunk[..count]);
        reader.consume(count);
        if end.is_some() {
            return Ok(serde_json::from_slice(&bytes)?);
        }
    }
}
pub(super) fn run(fux: &Path, zor: &Path, fixture: &Path) -> Result<()> {
    let root = Root::new("zhead-rs-", &["/bin/cat".into()])?;
    let mut server = root.server(fux)?;
    let listing = completed(&root.control(), json!({"command":"list","id":1}))?;
    let instance = &listing["instance"];
    let mut pids = vec![
        listing["workspaces"][0]["tabs"][0]["panes"][0]["pid"]
            .as_i64()
            .context("pane PID")?,
    ];
    let mut service = Guard(
        root.command(zor)
            .arg("serve")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?,
    );
    let socket = root.path().join("zor/control.sock");
    let scenario = (|| -> Result<()> {
        until(Duration::from_secs(8), || {
            ensure!(
                server.child.try_wait()?.is_none() && service.0.try_wait()?.is_none(),
                "fixture owner exited"
            );
            Ok(socket.exists().then_some(()))
        })?;
        let service_instance =
            api(&socket, &json!({"v":1,"id":1,"op":"ping"}))?["service_instance"].clone();
        let prompt = "--literal $(not-shell)";
        for (agent, flags) in [
            (
                "codex",
                vec![
                    "--ask-for-approval",
                    "never",
                    "exec",
                    "--json",
                    "--sandbox",
                    "workspace-write",
                    "--color",
                    "never",
                    "--skip-git-repo-check",
                    "--",
                ],
            ),
            (
                "claude",
                vec![
                    "--print",
                    "--output-format",
                    "stream-json",
                    "--verbose",
                    "--permission-prompts",
                    "none",
                    "--",
                ],
            ),
        ] {
            let program = root.path().join(agent);
            let output = root.path().join(format!("{agent}.jsonl"));
            fs::copy(fixture, &program)?;
            fs::set_permissions(&program, fs::Permissions::from_mode(0o700))?;
            let args = [
                "start",
                agent,
                "--title",
                agent,
                "--instance",
                instance.as_str().context("instance")?,
                "--workspace",
                "default",
                "--cwd",
                root.path().to_str().context("root")?,
                "--headless-agent",
                agent,
                "--",
                program.to_str().context("program")?,
                prompt,
            ];
            let mut command = root.command(zor);
            command.arg("task").args(args);
            let first = process::output(command, Duration::from_secs(10), 1024 * 1024)?;
            ensure!(
                first.status.success(),
                "headless start: {}",
                String::from_utf8_lossy(&first.stderr)
            );
            let first: Value = serde_json::from_slice(&first.stdout)?;
            until(Duration::from_secs(8), || {
                ensure!(
                    server.child.try_wait()?.is_none() && service.0.try_wait()?.is_none(),
                    "fixture owner exited"
                );
                Ok(fs::metadata(&output)
                    .is_ok_and(|m| m.len() > 0)
                    .then_some(()))
            })?;
            let captured: Value = serde_json::from_slice(&fs::read(&output)?)?;
            let mut expected = flags;
            expected.push(prompt);
            ensure!(
                captured["argv"] == json!(expected),
                "nonliteral or interactive argv: {captured}"
            );
            pids.push(captured["pid"].as_i64().context("provider PID")?);
            let replay = api(
                &socket,
                &json!({"v":1,"id":2,"op":"task","service_instance":service_instance,"task":{"action":"start","id":agent,"title":agent,"instance":instance,"workspace":"default","cwd":root.path(),"worktree":null,"argv":[program,prompt],"agent":null,"headless_agent":agent}}),
            )?;
            ensure!(replay["status"] == "completed", "headless replay: {replay}");
            ensure!(
                replay["value"]["launch"]["marker"] == first["launch"]["marker"],
                "replay changed marker"
            );
            ensure!(
                fs::read_to_string(&output)?.lines().count() == 1,
                "replay relaunched provider"
            );
            ensure!(
                first["task"]["outcome"] == "open" && replay["value"]["task"]["outcome"] == "open",
                "provider exit inferred success"
            );
            let mut command = root.command(zor);
            command.arg("task").args(args).arg("--extra");
            let rejected = process::output(command, Duration::from_secs(10), 1024 * 1024)?;
            ensure!(
                !rejected.status.success()
                    && String::from_utf8_lossy(&rejected.stderr)
                        .contains("exactly an executable and one prompt"),
                "extra argument accepted"
            );
        }
        Ok(())
    })();
    let service_cleanup = (|| -> Result<()> {
        use crate::support::process::OwnedProcess;
        service.0.terminate()?;
        ensure!(
            wait(&mut service.0, Duration::from_secs(10))?.success(),
            "service exit"
        );
        Ok(())
    })();
    let fux_cleanup = server.finish();
    let workers = (|| -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(5);
        for pid in pids {
            loop {
                match nix::sys::signal::kill(nix::unistd::Pid::from_raw(i32::try_from(pid)?), None)
                {
                    Err(nix::errno::Errno::ESRCH) => break,
                    Ok(()) => {}
                    Err(error) => return Err(error.into()),
                }
                ensure!(Instant::now() < deadline, "worker remains: {pid}");
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        Ok(())
    })();
    let errors: Vec<_> = [scenario, service_cleanup, fux_cleanup, workers]
        .into_iter()
        .filter_map(Result::err)
        .map(|error| format!("{error:#}"))
        .collect();
    ensure!(errors.is_empty(), "{}", errors.join("; "));
    println!(
        "PASS CLI/API noninteractive argv, literal prompt, stable launch replay and no inferred task success (scripted providers)"
    );
    Ok(())
}
