//! A program `lead_session` starts has no signal blocked, though this
//! process blocks one; with `Inherited` a signal this process inherited as
//! ignored stays ignored in it though a `signal::Pipe` caught it, and with
//! `Default` it is the default. In a test binary of its own, as it changes
//! what the whole process does with SIGHUP.
use fuxix::process::{Dispositions, Pid, Signal, Status, lead_session};
use std::time::{Duration, Instant};

/// How `script`, run by `sh` as a session leader, ended.
fn ended(script: &str, dispositions: Dispositions) -> Result<Status, Box<dyn std::error::Error>> {
    let mut command = std::process::Command::new("/bin/sh");
    command.args(["-c", script]);
    lead_session(&mut command, false, dispositions);
    let child = command.spawn()?;
    let pid = Pid::of(&child).ok_or("a pid")?;
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
    let term = ended("kill -TERM $$; exit 0", Dispositions::Inherited)?;
    assert!(
        matches!(term, Status::Signalled { signal, .. } if signal == libc::SIGTERM),
        "{term:?}"
    );
    // Ignored on entry and caught here, it is ignored there again.
    let hup = "kill -HUP $$; exit 7";
    assert_eq!(ended(hup, Dispositions::Inherited)?, Status::Exited(7));
    // As a new terminal's program, it is the default.
    let hung_up = ended(hup, Dispositions::Default)?;
    assert!(
        matches!(hung_up, Status::Signalled { signal, .. } if signal == libc::SIGHUP),
        "{hung_up:?}"
    );
    Ok(())
}
