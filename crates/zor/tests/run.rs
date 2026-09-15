//! `zor run -- CMD` end to end: a real `fux serve` and a real `zor serve` on disposable XDG
//! directories, then the `zor` binary runs `sh -c 'echo hi; exit 3'` in an ephemeral workspace
//! and exits with the process's status once zor holds final evidence. Skips with a message when
//! `target/debug/fux` is missing (`cargo build -p fux --bin fux`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::print_stderr,
    reason = "integration-test helpers; clippy.toml only relaxes #[test] bodies"
)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(30);

fn fux_binary() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let debug = exe.parent()?.parent()?;
    let binary = debug.join("fux");
    binary.is_file().then_some(binary)
}

/// Private XDG directories shared by both servers and the CLI.
struct Env {
    dir: tempfile::TempDir,
}

impl Env {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        for d in ["run", "config", "state"] {
            std::fs::create_dir_all(dir.path().join(d)).unwrap();
        }
        Self { dir }
    }

    fn apply(&self, command: &mut Command) {
        let root = self.dir.path();
        command
            .env("XDG_RUNTIME_DIR", root.join("run"))
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("XDG_STATE_HOME", root.join("state"))
            .env("HOME", root)
            .env("FUX_BRP", self.fux_brp())
            .stdin(Stdio::null());
    }

    fn fux_brp(&self) -> PathBuf {
        self.dir.path().join("run/fux/zt.brp.json")
    }

    fn zor_brp(&self) -> PathBuf {
        self.dir.path().join("run/zor/default.brp.json")
    }
}

struct Server(Child);

impl Server {
    fn spawn(env: &Env, binary: &Path, args: &[&str], descriptor: &Path) -> Self {
        let mut command = Command::new(binary);
        command
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        env.apply(&mut command);
        let child = command.spawn().unwrap();
        let deadline = Instant::now() + WAIT;
        while fux::remote::client::read_descriptor(descriptor).is_err() {
            assert!(
                Instant::now() < deadline,
                "{} never published {}",
                binary.display(),
                descriptor.display()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        Self(child)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let pid = nix::unistd::Pid::from_raw(i32::try_from(self.0.id()).unwrap());
        let _ = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGTERM);
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if matches!(self.0.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn run_exits_with_the_process_status_and_prints_its_final_screen() {
    let Some(fux) = fux_binary() else {
        eprintln!("skipping: target/debug/fux is missing; run `cargo build -p fux --bin fux`");
        return;
    };
    let zor = PathBuf::from(env!("CARGO_BIN_EXE_zor"));
    let env = Env::new();
    let _fux = Server::spawn(&env, &fux, &["serve", "--name", "zt"], &env.fux_brp());
    let _zor = Server::spawn(&env, &zor, &["serve"], &env.zor_brp());

    let mut command = Command::new(&zor);
    command
        .args([
            "run",
            "--timeout",
            "20",
            "--",
            "sh",
            "-c",
            "echo hi; exit 3",
        ])
        .current_dir(env.dir.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    env.apply(&mut command);
    let output = command.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(3),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(stdout.contains("hi"), "stdout: {stdout}\nstderr: {stderr}");

    // The task record holds the final evidence and the ephemeral workspace is gone.
    let tasks =
        fux::remote::client::call(&env.zor_brp(), "zor/attempt.list", serde_json::json!({}))
            .unwrap();
    let attempts = tasks["attempts"].as_array().unwrap();
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0]["state"], "finished");
    assert_eq!(attempts[0]["final"]["exit_code"], 3);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let list =
            fux::remote::client::call(&env.fux_brp(), "fux/workspace.list", serde_json::json!({}))
                .unwrap();
        let names: Vec<&str> = list["workspaces"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|w| w["name"].as_str())
            .collect();
        if names.iter().all(|n| !n.starts_with("zor-run-")) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "ephemeral workspace survived: {names:?}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}
