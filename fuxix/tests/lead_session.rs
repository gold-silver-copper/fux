//! A program `lead_session` starts has no signal blocked, and the signals
//! this process inherited as ignored stay ignored in it, though this process
//! blocks one and catches another with a `signal::Pipe`. In a test binary
//! of its own, as it changes what the whole process does with SIGHUP.
use fuxix::process::{Pid, Signal, Status, lead_session};
use std::time::{Duration, Instant};

/// How `script`, run by `sh` as a session leader, ended.
fn ended(script: &str) -> Result<Status, Box<dyn std::error::Error>> {
    let mut command = std::process::Command::new("/bin/sh");
    command.args(["-c", script]);
    lead_session(&mut command, false);
    let child = command.spawn()?;
    let pid = Pid::from_raw(i32::try_from(child.id())?).ok_or("a pid")?;
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(10))
        .ok_or("a deadline")?;
    loop {
        if let Some(status) = fuxix::process::reap(pid)? {
            return Ok(status);
        }
        if Instant::now() > deadline {
            return Err("still running".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn a_session_leader_starts_unblocked_with_the_dispositions_inherited()
-> Result<(), Box<dyn std::error::Error>> {
    // SIGTERM blocked in this thread, and SIGHUP ignored, as `nohup`
    // leaves it, before anything catches it.
    // SAFETY: `sigset_t` is plain data, for which all zeros is valid.
    let mut set: libc::sigset_t = unsafe { std::mem::zeroed() };
    // SAFETY: the set is this test's.
    unsafe { libc::sigemptyset(&mut set) };
    // SAFETY: as above.
    unsafe { libc::sigaddset(&mut set, libc::SIGTERM) };
    // SAFETY: the set is whole, and the old mask is not asked for.
    unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut()) };
    // SAFETY: SIG_IGN is a valid disposition for SIGHUP.
    unsafe { libc::signal(libc::SIGHUP, libc::SIG_IGN) };
    let _caught = fuxix::signal::Pipe::new(&[Signal::Hup])?;
    // SIGTERM blocked, it would stay pending and the shell exit 0.
    let term = ended("kill -TERM $$; exit 0")?;
    assert!(
        matches!(term, Status::Signalled(signal) if signal == libc::SIGTERM),
        "{term:?}"
    );
    // Ignored on entry and caught here, it is ignored there again.
    assert_eq!(ended("kill -HUP $$; exit 7")?, Status::Exited(7));
    Ok(())
}
