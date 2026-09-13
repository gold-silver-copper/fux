//! `zor run` against a real fux: it consumes retained final evidence and owns only the
//! workspace it created, whether a session server already exists or it has to start one.
use crate::support::{
    local::{Root, completed, rpc, stop_servers, until},
    process,
};
use anyhow::{Context, Result, ensure};
use serde_json::json;
use std::{
    path::Path,
    process::Command,
    time::{Duration, Instant},
};

/// A `zor run` command in the disposable root, with the fux under test first on PATH so a
/// fresh server is started from that binary.
fn zor_run(root: &Root, fux: &Path, zor: &Path, args: &[&str]) -> Result<Command> {
    let mut command = root.command(zor);
    let fux_dir = fux.parent().context("fux binary directory")?;
    let path = root.env.get("PATH").cloned().unwrap_or_default();
    command.env("PATH", format!("{}:{path}", fux_dir.display()));
    command.arg("run").args(args);
    Ok(command)
}

pub(super) fn run(fux: &Path, zor: &Path) -> Result<()> {
    let root = Root::new("zrun-rs-", &["/bin/sh".into()])?;
    let mut server = root.server(fux)?;
    let manager = root.path().join("fux/manager.sock");
    let names = || rpc(&manager, json!({"request":"list"}));
    for (index, code) in [0, 17, 29].into_iter().enumerate() {
        let name = format!("immediate-{index}");
        let command = zor_run(
            &root,
            fux,
            zor,
            &[
                "--workspace",
                &name,
                "--timeout",
                "5000",
                "--",
                "/bin/sh",
                "-c",
                &format!("printf 'FINAL-{code}'; exit {code}"),
            ],
        )?;
        let result = process::output(command, Duration::from_secs(10), 1024 * 1024)?;
        ensure!(
            result.status.code() == Some(code),
            "wrong exit: {:?}: {}",
            result.status,
            String::from_utf8_lossy(&result.stderr)
        );
        ensure!(
            result.stdout == format!("FINAL-{code}\n").as_bytes(),
            "immediate final bytes lost: {:?}",
            result.stdout
        );
        until(Duration::from_secs(3), || {
            Ok((names()?["names"] == json!(["default"])).then_some(()))
        })?;
    }
    let command = zor_run(
        &root,
        fux,
        zor,
        &[
            "--workspace",
            "child-options",
            "--",
            "/usr/bin/printf",
            "%s|%s",
            "--timeout",
            "--workspace",
        ],
    )?;
    let result = process::output(command, Duration::from_secs(10), 1024 * 1024)?;
    ensure!(
        result.status.success() && result.stdout == b"--timeout|--workspace\n",
        "child arguments were parsed as run flags: {:?} {}",
        result.stdout,
        String::from_utf8_lossy(&result.stderr)
    );
    let command = zor_run(
        &root,
        fux,
        zor,
        &[
            "--workspace",
            "default",
            "--",
            "/bin/sh",
            "-c",
            "touch \"$HOME/should-not-launch\"",
        ],
    )?;
    let rejected = process::output(command, Duration::from_secs(5), 1024 * 1024)?;
    ensure!(
        !rejected.status.success(),
        "pre-existing workspace was borrowed"
    );
    ensure!(
        !root.path().join("should-not-launch").exists(),
        "rejected command launched"
    );
    ensure!(
        names()?["names"] == json!(["default"]),
        "pre-existing workspace was destroyed"
    );
    let command = zor_run(
        &root,
        fux,
        zor,
        &[
            "--workspace",
            "timeout",
            "--timeout",
            "300",
            "--",
            "/bin/sh",
            "-c",
            "trap '' HUP; printf started > \"$HOME/timeout-started\"; sleep 2; touch \"$HOME/escaped-timeout\"",
        ],
    )?;
    let start = Instant::now();
    let timed = process::output(command, Duration::from_secs(5), 1024 * 1024)?;
    ensure!(!timed.status.success(), "timeout reported success");
    ensure!(
        start.elapsed() < Duration::from_secs(3),
        "run reset or exceeded its deadline"
    );
    ensure!(
        root.path().join("timeout-started").exists(),
        "timeout fixture never started"
    );
    until(Duration::from_secs(3), || {
        Ok((names()?["names"] == json!(["default"])).then_some(()))
    })?;
    std::thread::sleep(Duration::from_millis(2200));
    ensure!(
        !root.path().join("escaped-timeout").exists(),
        "timed-out process escaped cleanup"
    );
    moved_run(&root, fux, zor, false, false)?;
    moved_run(&root, fux, zor, true, false)?;
    moved_run(&root, fux, zor, true, true)?;
    stop_servers(root.path())?;
    server.finish()?;
    // The server-start path separately: no manager exists and none is required beforehand.
    let fresh = Root::new("zrun-new-rs-", &["/bin/sh".into()])?;
    let command = zor_run(
        &fresh,
        fux,
        zor,
        &["--timeout", "5000", "--", "/bin/sh", "-c", "printf FRESH"],
    )?;
    let result = process::output(command, Duration::from_secs(15), 1024 * 1024)?;
    stop_servers(fresh.path())?;
    ensure!(
        result.status.success() && result.stdout == b"FRESH\n",
        "fresh server run failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    println!(
        "PASS zor run: immediate final bytes/status, existing manager, fresh server, ownership refusal and timeout cleanup"
    );
    Ok(())
}

/// Move an active run, retire/recreate its launch route, and preserve unrelated destination panes.
fn moved_run(root: &Root, fux: &Path, zor: &Path, timeout: bool, eof: bool) -> Result<()> {
    let name = if eof {
        "moving-eof"
    } else if timeout {
        "moving-timeout"
    } else {
        "moving-exit"
    };
    let control = root.path().join(format!("fux/{name}.sock"));
    let manager = root.path().join("fux/manager.sock");
    let before = completed(&root.control(), json!({"command":"list","id":1}))?;
    let instance = before["instance"].clone();
    let original = before["workspaces"][0]["tabs"][0]["panes"][0].clone();
    let destination_stream = before["workspaces"][0]["event_cursor"]["stream"].clone();
    let script = if timeout {
        "trap '' HUP; sleep 30"
    } else {
        r#"while [ ! -e "$HOME/moved-ready" ]; do sleep 0.02; done; printf MOVED_FINAL; exit 19"#
    };
    let executable = std::env::current_exe()?;
    let eof_marker = root.path().join("close-terminal");
    let argv: Vec<&str> = if eof {
        vec![
            executable.to_str().context("fixture executable")?,
            "fixture-worker",
            "run-eof",
            eof_marker.to_str().context("EOF marker")?,
        ]
    } else {
        vec!["/bin/sh", "-c", script]
    };
    let mut args = vec![
        "--workspace",
        name,
        "--timeout",
        if timeout { "5000" } else { "10000" },
        "--",
    ];
    args.extend(argv.iter().copied());
    let command = zor_run(root, fux, zor, &args)?;
    std::thread::scope(|scope| -> Result<()> {
        let child =
            scope.spawn(move || process::output(command, Duration::from_secs(15), 1024 * 1024));
        let listing = until(Duration::from_secs(5), || {
            let Ok(listing) = completed(&control, json!({"command":"list","id":1})) else {
                return Ok(None);
            };
            let ready = listing["workspaces"][0]["tabs"][0]["panes"]
                .as_array()
                .context("run panes")?
                .iter()
                .any(|pane| pane["command"] == json!(argv) && pane["fixed_workspace"] == false);
            Ok(ready.then_some(listing))
        })?;
        let tab = &listing["workspaces"][0]["tabs"][0];
        let target = tab["panes"]
            .as_array()
            .context("run panes")?
            .iter()
            .find(|pane| pane["command"] == json!(argv))
            .context("run pane")?;
        let pane = target["id"].clone();
        let pid = target["pid"].clone();
        let layout = completed(
            &control,
            json!({"command":"layout","id":1,"tab":tab["id"],"action":{"operation":"export"}}),
        )?;
        let moved = rpc(
            &manager,
            json!({"request":"transfer","transfer":{
                "instance":instance,"source":tab["id"],"generation":layout["generation"],"pane":pane,
                "workspace":{"kind":"existing","name":"default","stream":destination_stream},
                "destination":{"kind":"new-tab","label":null},"side":"right"
            }}),
        )?;
        ensure!(
            moved["result"]["status"] == "completed",
            "run transfer failed: {moved}"
        );
        let location = rpc(
            &manager,
            json!({"request":"pane-location","instance":instance,"pane":pane}),
        )?;
        ensure!(
            location["result"]["result"]["value"]["location"]["pid"] == pid,
            "run PID changed"
        );
        completed(
            &control,
            json!({"command":"workspace","id":1,"instance":instance,
            "stream":listing["workspaces"][0]["event_cursor"]["stream"],"action":{"kill":{"name":name}}}),
        )?;
        until(Duration::from_secs(3), || {
            Ok((!control.exists()).then_some(()))
        })?;
        let recreated = rpc(&manager, json!({"request":"create","name":name}))?;
        ensure!(recreated["reply"] == "attach", "recreate: {recreated}");
        if !timeout {
            std::fs::write(root.path().join("moved-ready"), b"ready")?;
        }
        if eof {
            std::fs::write(&eof_marker, b"close")?;
            until(Duration::from_secs(3), || {
                let reply = rpc(
                    &manager,
                    json!({"request":"pane-location","instance":instance,"pane":pane}),
                )?;
                Ok(
                    (reply["result"]["result"]["value"]["location"]["accepts_input"] == false)
                        .then_some(()),
                )
            })?;
        }
        let output = child
            .join()
            .map_err(|_| anyhow::anyhow!("run fixture thread panicked"))??;
        if timeout {
            ensure!(!output.status.success(), "moved timeout succeeded");
            ensure!(
                String::from_utf8_lossy(&output.stderr).contains("did not finish"),
                "unexpected timeout: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        } else {
            ensure!(
                output.status.code() == Some(19) && output.stdout == b"MOVED_FINAL\n",
                "moved final lost: {:?} {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
        }
        until(Duration::from_secs(3), || {
            let reply = rpc(
                &manager,
                json!({"request":"pane-location","instance":instance,"pane":pane}),
            )?;
            Ok((reply["result"]["error"]["code"] == "not-found").then_some(()))
        })?;
        if timeout {
            let pid = nix::unistd::Pid::from_raw(i32::try_from(pid.as_u64().context("run PID")?)?);
            until(Duration::from_secs(3), || {
                match nix::sys::signal::kill(pid, None) {
                    Err(nix::errno::Errno::ESRCH) => Ok(Some(())),
                    Ok(()) => Ok(None),
                    Err(error) => Err(error.into()),
                }
            })?;
        }
        let after = completed(&root.control(), json!({"command":"list","id":1}))?;
        ensure!(
            after["workspaces"][0]["tabs"][0]["panes"][0]["pid"] == original["pid"],
            "destination process was killed"
        );
        completed(&control, json!({"command":"list","id":1}))?;
        completed(
            &control,
            json!({"command":"workspace","id":1,"instance":instance,
            "stream":recreated["descriptor"]["stream"],"action":{"kill":{"name":name}}}),
        )?;
        Ok(())
    })
}

/// Close the slave completely while keeping the process alive beyond the run deadline.
pub(super) fn eof_worker(marker: &Path) -> Result<()> {
    let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGHUP, flag)?;
    until(Duration::from_secs(15), || {
        Ok(marker.exists().then_some(()))
    })?;
    for fd in 0..65536 {
        let _ = nix::unistd::close(fd);
    }
    std::thread::sleep(Duration::from_secs(30));
    Ok(())
}
