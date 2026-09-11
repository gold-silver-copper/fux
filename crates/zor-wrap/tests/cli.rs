#![allow(clippy::indexing_slicing)]

use std::{
    fs,
    io::Read as _,
    os::fd::AsRawFd as _,
    os::unix::fs::PermissionsExt as _,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

fn temp_dir(label: &str) -> Result<std::path::PathBuf, std::io::Error> {
    let path = std::env::temp_dir().join(format!("zor-wrap-{label}-{}", std::process::id()));
    fs::create_dir_all(&path)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
    Ok(path)
}

#[test]
fn sigusr1_writes_the_detection_window_fixture() -> Result<(), Box<dyn std::error::Error>> {
    // Phase Z §7: SIGUSR1 writes the exact observed window to TMPDIR.
    let root = temp_dir("signal")?;
    let mut child = Command::new(env!("CARGO_BIN_EXE_zor-wrap"))
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", &root)
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_RUNTIME_DIR", &root)
        .env("TMPDIR", &root)
        .args([
            "--title",
            "never",
            "--",
            "/bin/sh",
            "-c",
            "printf observed; sleep 0.3",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let ready_and_exit = (|| -> Result<(), Box<dyn std::error::Error>> {
        let stdout = child.stdout.as_mut().ok_or("stdout missing")?;
        nix::fcntl::fcntl(
            stdout.as_raw_fd(),
            nix::fcntl::FcntlArg::F_SETFL(nix::fcntl::OFlag::O_NONBLOCK),
        )?;
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut observed = Vec::new();
        // Forwarded bytes prove the wrapper reached its event loop after installing handlers.
        // Rule compilation and process startup are not bounded by an arbitrary 100 ms sleep.
        while observed != b"observed" {
            let mut bytes = [0; 8];
            match child
                .stdout
                .as_mut()
                .ok_or("stdout missing")?
                .read(&mut bytes)
            {
                Ok(0) => return Err("wrapper exited before readiness".into()),
                Ok(count) => observed.extend_from_slice(bytes.get(..count).ok_or("read length")?),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error.into()),
            }
            if observed.len() > 8 || Instant::now() >= deadline {
                return Err("unexpected output or wrapper readiness deadline".into());
            }
            thread::sleep(Duration::from_millis(5));
        }
        nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(child.id().try_into()?),
            nix::sys::signal::Signal::SIGUSR1,
        )?;
        while child.try_wait()?.is_none() {
            if Instant::now() >= deadline {
                return Err("wrapper exit deadline".into());
            }
            thread::sleep(Duration::from_millis(5));
        }
        Ok(())
    })();
    if ready_and_exit.is_err() {
        let _ = child.kill();
    }
    let output = child.wait_with_output()?;
    ready_and_exit?;
    assert!(
        output.status.success(),
        "status: {}; stderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8(output.stderr)?;
    let path = stderr
        .lines()
        .find_map(|line| line.strip_prefix("zor-wrap: fixture written to "))
        .map(std::path::PathBuf::from);
    assert!(path.as_ref().is_some_and(|path| path.exists()));
    assert_eq!(
        path.as_ref()
            .map(fs::metadata)
            .transpose()?
            .map(|metadata| metadata.permissions().mode() & 0o777),
        Some(0o600)
    );
    let contents = path
        .map(fs::read_to_string)
        .transpose()?
        .unwrap_or_default();
    assert!(contents.contains("observed"));
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn wrapper_clears_an_earlier_state_by_publishing_none() -> Result<(), Box<dyn std::error::Error>> {
    // A matched title rule publishes its state; an unmatched screen publishes state=none.
    let root = temp_dir("state-clearing")?;
    let rules = root.join("rules");
    fs::create_dir_all(&rules)?;
    fs::write(
        rules.join("test.toml"),
        "id='test'\nprompt_marker='>'\nblock_markers=[]\n[[rules]]\nid='working'\nstate='working'\nregion='progress'\ncontains=['1:50']\nvisible_working=true\n[[rules]]\nid='idle'\nstate='blocked'\nregion='title'\ncontains=['OBS_IDLE']\nvisible_blocker=true\n",
    )?;
    let output = Command::new(env!("CARGO_BIN_EXE_zor-wrap"))
        .arg("--rules")
        .arg(&rules)
        .args([
            "--agent",
            "test",
            "--title",
            "never",
            "--",
            "/bin/sh",
            "-c",
            "printf '\\033]2;OBS_IDLE\\007'; sleep .3; printf '\\033[2J\\033[HUNKNOWN\\033]2;\\007'; sleep .3",
        ])
        .output()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success()
            && stdout.contains("state=blocked")
            && stdout.contains("state=none"),
        "wrapper state clearing: {stdout:?}"
    );
    fs::remove_dir_all(root)?;
    Ok(())
}
