//! Generic run consumes retained final evidence and owns only its created workspace lifetime.
use crate::support::{
    local::{Root, rpc, stop_servers, until},
    process,
};
use anyhow::{Result, ensure};
use serde_json::json;
use std::{
    path::Path,
    time::{Duration, Instant},
};

pub(super) fn run(binary: &Path) -> Result<()> {
    let root = Root::new("frun-rs-", &["/bin/sh".into()])?;
    let mut server = root.server(binary)?;
    let manager = root.path().join("fux/manager.sock");
    let names = || rpc(&manager, json!({"request":"list"}));
    for (index, code) in [0, 17, 29].into_iter().enumerate() {
        let mut command = root.command(binary);
        let name = format!("immediate-{index}");
        command.args([
            "run",
            "--workspace",
            &name,
            "--timeout",
            "5000",
            "--",
            "/bin/sh",
            "-c",
            &format!("printf 'FINAL-{code}'; exit {code}"),
        ]);
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
    let mut command = root.command(binary);
    command.args([
        "run",
        "--workspace",
        "child-options",
        "--",
        "/usr/bin/printf",
        "%s|%s",
        "--timeout",
        "--workspace",
    ]);
    let result = process::output(command, Duration::from_secs(10), 1024 * 1024)?;
    ensure!(
        result.status.success() && result.stdout == b"--timeout|--workspace\n",
        "child arguments were parsed as run flags"
    );
    let mut command = root.command(binary);
    command.args([
        "run",
        "--workspace",
        "default",
        "--",
        "/bin/sh",
        "-c",
        "touch \"$HOME/should-not-launch\"",
    ]);
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
    let mut command = root.command(binary);
    command.args(["run", "--workspace", "timeout", "--timeout", "300", "--", "/bin/sh", "-c",
        "trap '' HUP; printf started > \"$HOME/timeout-started\"; sleep 2; touch \"$HOME/escaped-timeout\""]);
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
    // Exercise the daemon-start path separately; an existing manager must not be required.
    let fresh = Root::new("frun-new-rs-", &["/bin/sh".into()])?;
    let mut command = fresh.command(binary);
    command.args([
        "run",
        "--timeout",
        "5000",
        "--",
        "/bin/sh",
        "-c",
        "printf FRESH",
    ]);
    let result = process::output(command, Duration::from_secs(15), 1024 * 1024)?;
    stop_servers(fresh.path())?;
    ensure!(
        result.status.success() && result.stdout == b"FRESH\n",
        "fresh server run failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    println!(
        "PASS immediate final bytes/status, existing manager, fresh server, ownership refusal and timeout cleanup"
    );
    Ok(())
}
