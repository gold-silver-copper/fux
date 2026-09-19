//! Real-process safety contracts: EOF and inherited pipes cannot pin provider teardown.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, reason = "process regression assertions")]

use std::process::{Child, Command};
use std::time::{Duration, Instant};

use bevy_ecs::prelude::World;
use bevy_tasks::{IoTaskPool, TaskPool};
use nix::errno::Errno;
use nix::sys::signal::{Signal, kill, killpg};
use nix::sys::wait::{WaitPidFlag, waitpid};
use nix::unistd::Pid;
use zor::model::{Effect, Inbound};
use zor::providers::ProviderAdapter;
use zor::runner::Adapter;

const LIMIT: Duration = Duration::from_secs(6);
const RETAIN_STDOUT: &str = "sleep 60 & printf '%s\\n' \"$!\"; cat >/dev/null; wait";
const BLOCK_STDIN: &str = "sleep 60 & printf '%s\\n' \"$!\"; wait";

fn adapter() -> (ProviderAdapter, async_channel::Receiver<Inbound>, bevy_ecs::entity::Entity) {
    IoTaskPool::get_or_init(TaskPool::new);
    let (tx, rx) = async_channel::unbounded();
    let attempt = World::new().spawn_empty().id();
    (ProviderAdapter::with_executable(tx, env!("CARGO_BIN_EXE_zor").into()), rx, attempt)
}

fn spawn(attempt: bevy_ecs::entity::Entity, script: &str) -> Effect {
    Effect::SpawnProvider {
        attempt,
        argv: vec!["/bin/sh".into(), "-c".into(), script.into()],
        cwd: None,
        env: Vec::new(),
    }
}

fn until(mut predicate: impl FnMut() -> bool) {
    let start = Instant::now();
    while !predicate() {
        assert!(start.elapsed() < LIMIT, "provider process failed to settle boundedly");
        std::thread::sleep(Duration::from_millis(10));
    }
}

// A failure in an assertion must not leave the fixture processes behind.
struct Group(Pid);
impl Drop for Group {
    fn drop(&mut self) { let _ = killpg(self.0, Signal::SIGKILL); }
}

fn started(rx: &async_channel::Receiver<Inbound>, attempt: bevy_ecs::entity::Entity) -> (Group, Pid) {
    let mut leader = None;
    let mut output = Vec::new();
    until(|| {
        while let Ok(message) = rx.try_recv() {
            match message {
                Inbound::ProviderStarted { attempt: owner, pid } => {
                    assert_eq!(owner, attempt);
                    assert!(leader.is_none(), "duplicate process start");
                    leader = Some(Group(Pid::from_raw(i32::try_from(pid).unwrap())));
                }
                Inbound::ProviderOutput { attempt: owner, bytes } => {
                    assert_eq!(owner, attempt);
                    assert!(leader.is_some(), "output arrived before provenance");
                    output.extend(bytes);
                }
                Inbound::ProviderExited { .. } => panic!("fixture exited before retaining stdout"),
                _ => {}
            }
        }
        leader.is_some() && output.contains(&b'\n')
    });
    let descendant = String::from_utf8(output).unwrap().trim().parse().unwrap();
    (leader.unwrap(), Pid::from_raw(descendant))
}

fn not_running(pid: Pid) -> bool {
    if kill(pid, None).is_err() { return true; }
    // Grandchildren are adopted by init, not waitable by this adapter. A zombie has
    // terminated and cannot retain pipes; init controls when its bookkeeping disappears.
    let status = Command::new("ps").args(["-p", &pid.to_string(), "-o", "stat="])
        .output().unwrap();
    let status = String::from_utf8_lossy(&status.stdout);
    status.trim().is_empty() || status.trim().starts_with('Z')
}

#[test]
fn close_reaps_group_even_when_eof_is_ignored_and_stdout_is_inherited() {
    let (mut adapter, rx, attempt) = adapter();
    adapter.apply(spawn(attempt, RETAIN_STDOUT)).unwrap();
    let (leader, descendant) = started(&rx, attempt);
    adapter.apply(Effect::WriteProvider { attempt, bytes: Vec::new() }).unwrap();
    assert!(adapter.apply(spawn(attempt, RETAIN_STDOUT)).is_err());
    assert!(adapter.apply(Effect::WriteProvider { attempt, bytes: b"late".to_vec() }).is_err());
    let mut exits = 0;
    until(|| {
        while let Ok(message) = rx.try_recv() {
            if let Inbound::ProviderExited { attempt: owner, code } = message {
                assert_eq!(owner, attempt);
                assert_eq!(code, 128 + Signal::SIGKILL as i32);
                exits += 1;
            }
        }
        exits != 0 && !adapter.pending()
    });
    assert_eq!(exits, 1);
    assert_eq!(waitpid(leader.0, Some(WaitPidFlag::WNOHANG)), Err(Errno::ECHILD));
    until(|| not_running(descendant));
}

#[test]
fn blocked_stdin_rejects_overflow_and_shutdown_still_finishes() {
    let (mut adapter, rx, attempt) = adapter();
    adapter.apply(spawn(attempt, BLOCK_STDIN)).unwrap();
    let (_leader, descendant) = started(&rx, attempt);
    let mut rejected = false;
    for _ in 0..64 {
        if adapter.apply(Effect::WriteProvider { attempt, bytes: vec![b'x'; 1024 * 1024] }).is_err() {
            rejected = true;
            break;
        }
    }
    assert!(rejected, "a blocked child must not accept an unbounded stdin queue");
    adapter.shutdown();
    until(|| !adapter.pending());
    let mut exited = false;
    while let Ok(message) = rx.try_recv() {
        exited |= matches!(message, Inbound::ProviderExited { .. });
    }
    assert!(exited);
    until(|| not_running(descendant));
}

#[test]
fn drop_synchronously_reaps_owned_process_group() {
    let (mut adapter, rx, attempt) = adapter();
    adapter.apply(spawn(attempt, RETAIN_STDOUT)).unwrap();
    let (leader, descendant) = started(&rx, attempt);
    let start = Instant::now();
    drop(adapter);
    assert!(start.elapsed() < LIMIT);
    assert_eq!(waitpid(leader.0, Some(WaitPidFlag::WNOHANG)), Err(Errno::ECHILD));
    until(|| not_running(descendant));
}

#[test]
fn immediate_drop_cannot_leave_a_queued_spawn_alive() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("pid");
    let (mut adapter, _rx, attempt) = adapter();
    adapter.apply(Effect::SpawnProvider {
        attempt,
        argv: vec!["/bin/sh".into(), "-c".into(), "printf '%s' \"$$\" > \"$MARKER\"; exec sleep 60".into()],
        cwd: None,
        env: vec![("MARKER".into(), marker.to_string_lossy().into_owned())],
    }).unwrap();
    drop(adapter);
    // If spawn raced teardown, Drop still owns/reaps it; if queued, it cannot run later.
    std::thread::sleep(Duration::from_millis(100));
    if let Ok(pid) = std::fs::read_to_string(marker) {
        until(|| not_running(Pid::from_raw(pid.parse().unwrap())));
    }
}

#[test]
fn ordinary_stdout_precedes_exit_and_preserves_attempt() {
    let (mut adapter, rx, attempt) = adapter();
    adapter.apply(spawn(attempt, "printf 'evidence-frame\\n'")).unwrap();
    let mut began = false;
    let mut output = Vec::new();
    let mut exited = false;
    until(|| {
        while let Ok(message) = rx.try_recv() {
            match message {
                Inbound::ProviderStarted { attempt: owner, .. } => {
                    assert_eq!(owner, attempt);
                    assert!(!began);
                    began = true;
                }
                Inbound::ProviderOutput { attempt: owner, bytes } => {
                    assert_eq!(owner, attempt);
                    assert!(began && !exited);
                    output.extend(bytes);
                }
                Inbound::ProviderExited { attempt: owner, code } => {
                    assert_eq!(owner, attempt);
                    assert_eq!(code, 0);
                    assert!(began && !exited);
                    assert_eq!(output, b"evidence-frame\n");
                    exited = true;
                }
                _ => {}
            }
        }
        exited && !adapter.pending()
    });
}

struct Host(Child);
impl Drop for Host {
    fn drop(&mut self) { let _ = self.0.kill(); let _ = self.0.wait(); }
}

#[test]
fn host_hard_death_does_not_leave_provider_descendants_running() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("started");
    let mut host = Host(Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "provider_host_fixture", "--nocapture"])
        .env("ZOR_PROVIDER_HOST_FIXTURE", &marker).spawn().unwrap());
    let mut pids = None;
    until(|| {
        if let Ok(text) = std::fs::read_to_string(&marker) {
            pids = text.split_whitespace().map(str::parse::<i32>).collect::<Result<Vec<_>, _>>().ok();
        }
        pids.as_ref().is_some_and(|pids| pids.len() == 2)
    });
    let mut pids = pids.unwrap().into_iter();
    let leader = Group(Pid::from_raw(pids.next().unwrap()));
    let descendant = Pid::from_raw(pids.next().unwrap());
    host.0.kill().unwrap();
    host.0.wait().unwrap();
    until(|| not_running(leader.0) && not_running(descendant));
}

#[test]
fn provider_host_fixture() {
    let Some(marker) = std::env::var_os("ZOR_PROVIDER_HOST_FIXTURE") else { return; };
    let (mut adapter, rx, attempt) = adapter();
    adapter.apply(spawn(attempt, RETAIN_STDOUT)).unwrap();
    let (leader, descendant) = started(&rx, attempt);
    std::fs::write(marker, format!("{} {}", leader.0, descendant)).unwrap();
    loop { std::thread::park(); }
}
