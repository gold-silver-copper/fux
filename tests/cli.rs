#![cfg(feature = "cli")]

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
    let path = std::env::temp_dir().join(format!("zor-{label}-{}", std::process::id()));
    fs::create_dir_all(&path)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
    Ok(path)
}

#[test]
fn capabilities_are_read_only_and_unknown_agents_fail() -> Result<(), Box<dyn std::error::Error>> {
    let root = temp_dir("capabilities")?;
    for agent in ["codex", "claude", "opencode", "unknown"] {
        let output = Command::new(env!("CARGO_BIN_EXE_zor"))
            .env_clear()
            .env("HOME", &root)
            .args(["task", "adapter-capabilities", "--agent", agent])
            .output()?;
        if agent == "unknown" {
            assert!(!output.status.success());
            assert!(String::from_utf8_lossy(&output.stderr).contains("unsupported-agent:"));
        } else {
            assert!(output.status.success());
            let value: serde_json::Value = serde_json::from_slice(&output.stdout)?;
            assert_eq!(
                value.get("agent").and_then(serde_json::Value::as_str),
                Some(agent)
            );
            assert_eq!(
                value
                    .get("availability")
                    .and_then(serde_json::Value::as_str),
                Some("not-probed")
            );
        }
        assert_eq!(fs::read_dir(&root)?.count(), 0);
    }
    fs::remove_dir(root)?;
    Ok(())
}

#[test]
fn check_evaluates_fixture_expectation_and_rule() -> Result<(), Box<dyn std::error::Error>> {
    // Phase Z §8: check validates both expected state and matched rule id.
    let root = temp_dir("check")?;
    let rules = root.join("rules");
    fs::create_dir_all(&rules)?;
    fs::write(
        rules.join("agent.toml"),
        "id='agent'\nprompt_marker='>'\nblock_markers=[]\n[[rules]]\nid='working'\nstate='working'\nregion='whole'\ncontains=['busy']\n",
    )?;
    let fixture = root.join("fixture.txt");
    fs::write(
        &fixture,
        "# agent: agent\n# title: test\n# progress: 3:0\n# expect: working\n# matched: working\nbusy\n",
    )?;
    let output = Command::new(env!("CARGO_BIN_EXE_zor"))
        .args([
            "--rules",
            rules.to_string_lossy().as_ref(),
            "check",
            fixture.to_string_lossy().as_ref(),
        ])
        .output()?;
    assert!(
        output.status.success(),
        "status: {}; stderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"working working\n");
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn sigusr1_writes_the_detection_window_fixture() -> Result<(), Box<dyn std::error::Error>> {
    // Phase Z §7: SIGUSR1 writes the exact observed window to TMPDIR.
    let root = temp_dir("signal")?;
    let mut child = Command::new(env!("CARGO_BIN_EXE_zor"))
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
        .find_map(|line| line.strip_prefix("zor: fixture written to "))
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
