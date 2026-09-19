//! Check lifecycle regressions over real process groups and bounded completion delivery.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "process regression assertions"
)]

use std::process::Command;
use std::time::{Duration, Instant};

use bevy_ecs::prelude::*;
use bevy_tasks::{IoTaskPool, TaskPool};
use nix::errno::Errno;
use nix::sys::signal::{Signal, kill, killpg};
use nix::sys::wait::{WaitPidFlag, waitpid};
use nix::unistd::Pid;
use zor::checks::runner::{CheckRunner, Registry, execute};
use zor::model::{Effect, Inbound};
use zor::runner::Adapter;

const LIMIT: Duration = Duration::from_secs(4);
const HOLD: &str = "sleep 60 & printf '%s %s' \"$PPID\" \"$!\" > pids; wait";

fn until(mut predicate: impl FnMut() -> bool) {
    let start = Instant::now();
    while !predicate() {
        assert!(start.elapsed() < LIMIT, "check failed to settle boundedly");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn adapter(tx: async_channel::Sender<Inbound>) -> CheckRunner {
    IoTaskPool::get_or_init(TaskPool::new);
    CheckRunner::with_executable(tx, env!("CARGO_BIN_EXE_zor").into())
}

fn spawn(check: Entity, cwd: &std::path::Path, script: &str, timeout_ms: u64) -> Effect {
    Effect::RunCheck {
        check,
        argv: vec!["/bin/sh".into(), "-c".into(), script.into()],
        cwd: cwd.display().to_string(),
        timeout_ms,
    }
}

struct Group(Pid);
impl Drop for Group {
    fn drop(&mut self) {
        let _ = killpg(self.0, Signal::SIGKILL);
    }
}

fn started(dir: &std::path::Path) -> (Group, Pid) {
    let mut pids = Vec::new();
    until(|| {
        pids = std::fs::read_to_string(dir.join("pids"))
            .unwrap_or_default()
            .split_whitespace()
            .filter_map(|s| s.parse::<i32>().ok())
            .collect();
        pids.len() == 2
    });
    let mut pids = pids.into_iter();
    (
        Group(Pid::from_raw(pids.next().unwrap())),
        Pid::from_raw(pids.next().unwrap()),
    )
}

fn not_running(pid: Pid) -> bool {
    if kill(pid, None).is_err() {
        return true;
    }
    // Grandchildren are adopted by init; terminated zombies cannot hold pipes.
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "stat="])
        .output()
        .unwrap();
    let status = String::from_utf8_lossy(&output.stdout);
    status.trim().is_empty() || status.trim().starts_with('Z')
}

fn done(
    adapter: &CheckRunner,
    rx: &async_channel::Receiver<Inbound>,
    check: Entity,
) -> (Option<i32>, String, Option<String>) {
    until(|| !adapter.pending());
    let Inbound::CheckDone {
        check: owner,
        code,
        stdout,
        problem,
        ..
    } = rx.try_recv().unwrap()
    else {
        panic!("not CheckDone");
    };
    assert_eq!(owner, check);
    assert!(rx.try_recv().is_err(), "exactly one completion");
    (code, stdout, problem)
}

#[test]
fn immediate_cancel_and_drop_never_leave_a_queued_spawn_alive() {
    for drop_immediately in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let (tx, rx) = async_channel::bounded(1);
        let mut runner = adapter(tx);
        let check = World::new().spawn_empty().id();
        assert!(
            runner
                .apply(spawn(check, dir.path(), HOLD, 60_000))
                .unwrap()
                .is_none()
        );
        assert!(runner.apply(Effect::KillCheck { check }).unwrap().is_none());
        if drop_immediately {
            drop(runner);
        } else {
            let (code, _, problem) = done(&runner, &rx, check);
            assert_eq!(code, None);
            assert!(problem.is_some());
        }
        std::thread::sleep(Duration::from_millis(100));
        if dir.path().join("pids").exists() {
            let (leader, descendant) = started(dir.path());
            assert_eq!(
                waitpid(leader.0, Some(WaitPidFlag::WNOHANG)),
                Err(Errno::ECHILD)
            );
            until(|| not_running(descendant));
        }
    }
}

#[test]
fn shutdown_and_drop_reap_leader_and_kill_stdout_holding_grandchild() {
    for drop_immediately in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let (tx, rx) = async_channel::bounded(1);
        let mut runner = adapter(tx);
        let check = World::new().spawn_empty().id();
        assert!(
            runner
                .apply(spawn(check, dir.path(), HOLD, 60_000))
                .unwrap()
                .is_none()
        );
        let (leader, descendant) = started(dir.path());
        let start = Instant::now();
        if drop_immediately {
            drop(runner);
        } else {
            runner.shutdown();
            assert!(
                runner
                    .apply(spawn(check, dir.path(), HOLD, 60_000))
                    .is_err()
            );
            let (code, _, problem) = done(&runner, &rx, check);
            assert_eq!(code, None);
            assert!(problem.is_some());
        }
        assert!(start.elapsed() < LIMIT);
        assert_eq!(
            waitpid(leader.0, Some(WaitPidFlag::WNOHANG)),
            Err(Errno::ECHILD)
        );
        until(|| not_running(descendant));
    }
}

#[test]
fn timeout_kills_inherited_pipe_and_reports_uncertain() {
    let dir = tempfile::tempdir().unwrap();
    let (tx, rx) = async_channel::bounded(1);
    let mut runner = adapter(tx);
    let check = World::new().spawn_empty().id();
    assert!(
        runner
            .apply(spawn(check, dir.path(), HOLD, 1_000))
            .unwrap()
            .is_none()
    );
    let (leader, descendant) = started(dir.path());
    let (code, _, problem) = done(&runner, &rx, check);
    assert_eq!(code, None);
    assert!(problem.is_some());
    assert_eq!(
        waitpid(leader.0, Some(WaitPidFlag::WNOHANG)),
        Err(Errno::ECHILD)
    );
    until(|| not_running(descendant));
}

#[test]
fn exited_leader_cannot_leave_descendants_holding_stdout() {
    let dir = tempfile::tempdir().unwrap();
    let (tx, rx) = async_channel::bounded(1);
    let mut runner = adapter(tx);
    let check = World::new().spawn_empty().id();
    assert!(
        runner
            .apply(spawn(
                check,
                dir.path(),
                "sleep 60 & printf '%s %s' \"$PPID\" \"$!\" > pids; printf done; exit 7",
                60_000
            ))
            .unwrap()
            .is_none()
    );
    let (leader, descendant) = started(dir.path());
    assert_eq!(done(&runner, &rx, check), (Some(7), "done".into(), None));
    assert_eq!(
        waitpid(leader.0, Some(WaitPidFlag::WNOHANG)),
        Err(Errno::ECHILD)
    );
    until(|| not_running(descendant));
}

#[test]
fn pending_includes_blocked_completion_delivery() {
    let dir = tempfile::tempdir().unwrap();
    let (tx, rx) = async_channel::bounded(1);
    let mut world = World::new();
    let previous = world.spawn_empty().id();
    let check = world.spawn_empty().id();
    tx.try_send(Inbound::CheckDone {
        check: previous,
        code: Some(0),
        stdout: String::new(),
        stderr: String::new(),
        truncated: false,
        problem: None,
    })
    .unwrap();
    let mut runner = adapter(tx);
    assert!(
        runner
            .apply(spawn(
                check,
                dir.path(),
                "printf '%s %s' \"$PPID\" \"$$\" > pids; printf done",
                60_000
            ))
            .unwrap()
            .is_none()
    );
    let (leader, _) = started(dir.path());
    until(|| not_running(leader.0));
    assert!(
        runner.pending(),
        "completion has not entered the full inbound queue"
    );
    let Inbound::CheckDone { check: owner, .. } = rx.try_recv().unwrap() else {
        panic!("not CheckDone");
    };
    assert_eq!(owner, previous);
    assert_eq!(done(&runner, &rx, check), (Some(0), "done".into(), None));
}

#[test]
fn direct_execute_keeps_normal_output_and_spawn_failure_contract() {
    let check = World::new().spawn_empty().id();
    let registry = Registry::default();
    let outcome = bevy_tasks::futures_lite::future::block_on(execute(
        check,
        &[
            "/bin/sh".into(),
            "-c".into(),
            "printf out; printf err >&2; exit 3".into(),
        ],
        "/",
        5_000,
        &registry,
    ));
    assert_eq!(outcome.code, Some(3));
    assert_eq!(outcome.stdout, "out");
    assert_eq!(outcome.stderr, "err");
    assert_eq!(outcome.problem, None);
    let outcome = bevy_tasks::futures_lite::future::block_on(execute(
        check,
        &["/nonexistent/zor-check".into()],
        "/",
        5_000,
        &registry,
    ));
    assert_eq!(outcome.code, None);
    assert!(outcome.problem.is_some());
}

struct Host(std::process::Child);
impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn host_hard_death_kills_check_descendants() {
    let dir = tempfile::tempdir().unwrap();
    let mut host = Host(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "check_host_fixture", "--nocapture"])
            .env("ZOR_CHECK_HOST_FIXTURE", dir.path())
            .spawn()
            .unwrap(),
    );
    let (leader, descendant) = started(dir.path());
    host.0.kill().unwrap();
    host.0.wait().unwrap();
    until(|| not_running(leader.0) && not_running(descendant));
}

#[test]
fn check_host_fixture() {
    let Some(dir) = std::env::var_os("ZOR_CHECK_HOST_FIXTURE") else {
        return;
    };
    let (tx, _rx) = async_channel::bounded(1);
    let mut runner = adapter(tx);
    let check = World::new().spawn_empty().id();
    assert!(
        runner
            .apply(spawn(check, std::path::Path::new(&dir), HOLD, 60_000))
            .unwrap()
            .is_none()
    );
    loop {
        std::thread::park();
    }
}
