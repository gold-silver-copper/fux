//! Incompatible-manager dialog leaves an old server alive until explicit confirmation.
use crate::support::{
    local::{Root, stop_servers, until},
    process::{self, Guard},
    terminal::Terminal,
};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    os::unix::{fs::PermissionsExt, net::UnixListener},
    path::Path,
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

pub(super) fn old_manager(root: &Path) -> Result<()> {
    let path = root.join("fux/manager.sock");
    let stopping = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGTERM, stopping.clone())?;
    let listener = UnixListener::bind(&path)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    fs::write(root.join("old-ready"), b"1")?;
    while !stopping.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((mut peer, _)) => {
                peer.set_nonblocking(false)?;
                peer.set_read_timeout(Some(Duration::from_secs(2)))?;
                peer.set_write_timeout(Some(Duration::from_secs(2)))?;
                let mut bytes = [0; 8];
                if peer.read(&mut bytes).is_ok() {
                    let _ = peer.write_all(b"FUXCTL1\n");
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(10))
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error.into()),
        }
    }
    fs::remove_file(path)?;
    Ok(())
}
pub(super) fn run(binary: &Path) -> Result<()> {
    let root = Root::new(
        "fmig-rs-",
        &[
            "/bin/sh".into(),
            "-c".into(),
            "printf NEW_SERVER_PANE; exec cat".into(),
        ],
    )?;
    fs::create_dir_all(root.path().join("fux/workspaces"))?;
    for path in ["fux", "fux/workspaces"] {
        fs::set_permissions(root.path().join(path), fs::Permissions::from_mode(0o700))?;
    }
    let mut old = Guard(
        root.command(&std::env::current_exe()?)
            .args(["fixture-worker", "old-manager"])
            .arg(root.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?,
    );
    until(Duration::from_secs(5), || {
        ensure!(old.0.try_wait()?.is_none(), "old manager exited");
        Ok(root.path().join("old-ready").exists().then_some(()))
    })?;
    fs::write(
        root.path().join("fux/workspaces/default.json"),
        serde_json::to_vec(
            &json!({"name":"default","pid":old.0.id(),"instance_nonce":"old","socket_path":root.path().join("fux/default.attach.sock"),"protocol":2}),
        )?,
    )?;
    let scenario = (|| -> Result<()> {
        let result = process::output(root.command(binary), Duration::from_secs(20), 1024 * 1024)?;
        let stderr = String::from_utf8_lossy(&result.stderr);
        ensure!(
            !result.status.success(),
            "noninteractive migration succeeded"
        );
        ensure!(
            stderr.contains("older protocol")
                && stderr.contains("default (pid")
                && stderr.contains("XDG_RUNTIME_DIR"),
            "missing noninteractive explanation: {stderr}"
        );
        ensure!(
            old.0.try_wait()?.is_none(),
            "noninteractive fux stopped old manager"
        );
        {
            let mut terminal = Terminal::start(&root, binary)?;
            terminal.wait_for("[k/s/q]", Duration::from_secs(10))?;
            terminal.send(b"q\n")?;
            ensure!(
                !terminal.wait(Duration::from_secs(10))?.success() && old.0.try_wait()?.is_none(),
                "quit stopped old manager or succeeded"
            );
        }
        {
            let mut terminal = Terminal::start(&root, binary)?;
            terminal.wait_for("[k/s/q]", Duration::from_secs(10))?;
            terminal.send(b"k\n")?;
            terminal.wait_for("Type \"stop\"", Duration::from_secs(10))?;
            terminal.send(b"no\n")?;
            terminal.wait(Duration::from_secs(10))?;
            ensure!(old.0.try_wait()?.is_none(), "refusal stopped old manager");
        }
        let mut terminal = Terminal::start(&root, binary)?;
        terminal.wait_for("[k/s/q]", Duration::from_secs(10))?;
        terminal.send(b"k\n")?;
        terminal.wait_for("Type \"stop\"", Duration::from_secs(10))?;
        terminal.send(b"stop\n")?;
        until(Duration::from_secs(15), || {
            terminal.pump()?;
            Ok(old.0.try_wait()?)
        })?;
        terminal.wait_for("NEW_SERVER_PANE", Duration::from_secs(20))?;
        terminal.send(b"\x01d")?;
        ensure!(
            terminal.wait(Duration::from_secs(10))?.success(),
            "detach failed"
        );
        let mut listing = root.command(binary);
        listing.args(["default", "list"]);
        let listing = process::output(listing, Duration::from_secs(10), 1024 * 1024)?;
        ensure!(
            listing.status.success()
                && serde_json::from_slice::<Value>(&listing.stdout)?["result"]["value"]["workspaces"]
                    [0]["name"]
                    == "default",
            "new workspace not listed"
        );
        Ok(())
    })();
    drop(old);
    let cleanup = stop_servers(root.path());
    let failures: Vec<_> = [scenario, cleanup]
        .into_iter()
        .filter_map(Result::err)
        .map(|error| format!("{error:#}"))
        .collect();
    ensure!(failures.is_empty(), "{}", failures.join("; "));
    println!(
        "PASS incompatible-server dialog: explains, keeps the old server unless confirmed, then replaces it"
    );
    Ok(())
}
