//! `zor run` against a real fux: it consumes retained final evidence and owns only the
//! workspace it created, whether a session server already exists or it has to start one.
use crate::support::{
    local::{Root, rpc, stop_servers, until},
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
