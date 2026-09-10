//! Restricted git invocation policy over the shared bounded subprocess effects.
use anyhow::Result;
use std::{ffi::OsStr, path::Path, process::Command, time::Instant};

pub(super) fn run(repo: &Path, args: &[&OsStr], deadline: Instant) -> Result<Vec<u8>> {
    let mut command = Command::new("/usr/bin/git");
    command
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "credential.helper=",
            "-c",
            "core.fsmonitor=false",
            "-C",
        ])
        .arg(repo)
        .args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("LC_ALL", "C");
    let output = crate::platform::process::run_command(&mut command, deadline)?;
    anyhow::ensure!(
        output.status.success(),
        "git failed ({}): {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
            .chars()
            .take(512)
            .collect::<String>()
    );
    Ok(output.stdout)
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use std::time::Duration;
    #[test]
    fn subprocess_deadline_and_output_pressure_are_bounded() {
        let root = tempfile::tempdir().expect("private fixture");
        let start = Instant::now();
        let timeout = run(
            root.path(),
            &[
                OsStr::new("-c"),
                OsStr::new("alias.fixture=!sleep 30"),
                OsStr::new("fixture"),
            ],
            start + Duration::from_millis(100),
        )
        .expect_err("deadline");
        assert!(timeout.to_string().contains("deadline"), "{timeout:#}");
        assert!(start.elapsed() < Duration::from_secs(3));
        let pressure = run(
            root.path(),
            &[
                OsStr::new("-c"),
                OsStr::new("alias.fixture=!yes x"),
                OsStr::new("fixture"),
            ],
            Instant::now() + Duration::from_secs(3),
        )
        .expect_err("output cap");
        assert!(
            pressure.to_string().contains("output limit"),
            "{pressure:#}"
        );
    }
}
