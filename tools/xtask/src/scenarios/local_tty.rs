//! Cold startup through an actual controlling PTY and persistent-server detach.
use crate::support::{
    local::{Root, stop_servers},
    process::Guard,
};
use anyhow::{Result, ensure};
use nix::sys::signal::Signal;
use std::{
    fs,
    os::unix::process::CommandExt,
    path::Path,
    process::Stdio,
    time::{Duration, Instant},
};

pub(super) fn run(binary: &Path) -> Result<()> {
    let root = Root::new("ft-rs-", &["/bin/sh".into()])?;
    let pty = crate::support::pty::open(24, 80)?;
    let mut command = root.command(binary);
    command
        .stdin(Stdio::from(pty.slave.try_clone()?))
        .stdout(Stdio::from(pty.slave.try_clone()?))
        .stderr(Stdio::from(pty.slave.try_clone()?));
    // Only async-signal-safe syscalls run after fork; allocations/configuration happen above.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            #[cfg(target_os = "macos")]
            let request = libc::c_ulong::from(libc::TIOCSCTTY);
            #[cfg(not(target_os = "macos"))]
            let request = libc::TIOCSCTTY;
            if libc::ioctl(0, request, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    nix::fcntl::fcntl(
        &pty.master,
        nix::fcntl::FcntlArg::F_SETFL(nix::fcntl::OFlag::O_NONBLOCK),
    )?;
    let mut child = Guard(command.spawn()?);
    let scenario = (|| -> Result<()> {
        let mut output = Vec::new();
        let read = |output: &mut Vec<u8>| -> Result<()> {
            let mut bytes = [0; 65536];
            match nix::unistd::read(&pty.master, &mut bytes) {
                Ok(count) => output.extend_from_slice(&bytes[..count]),
                Err(nix::errno::Errno::EIO | nix::errno::Errno::EAGAIN) => {}
                Err(error) => return Err(error.into()),
            }
            ensure!(output.len() <= 1024 * 1024, "TTY fixture output limit");
            Ok(())
        };
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            read(&mut output)?;
            let text = String::from_utf8_lossy(&output);
            if text.contains("\x1b[?1049h") && text.contains("?2026h") {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let text = String::from_utf8_lossy(&output);
        ensure!(
            text.contains("\x1b[?1049h"),
            "cold startup did not enter alternate screen: {text}"
        );
        ensure!(!text.contains("Passphrase"), "unexpected credential prompt");
        ensure!(
            nix::unistd::write(&pty.master, b"\x01d")? == 2,
            "detach input short write"
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = child.0.try_wait()? {
                ensure!(status.success(), "detach failed: {status}");
                break;
            }
            ensure!(Instant::now() < deadline, "detach deadline");
            read(&mut output)?;
            std::thread::sleep(Duration::from_millis(20));
        }
        ensure!(
            root.path().join("fux/default.attach.sock").exists(),
            "detach lost persistent server"
        );
        fn no_keys(path: &Path) -> Result<()> {
            for entry in fs::read_dir(path)? {
                let entry = entry?;
                if entry.file_type()?.is_dir() {
                    no_keys(&entry.path())?;
                } else {
                    ensure!(
                        entry.path().extension() != Some(std::ffi::OsStr::new("key")),
                        "unexpected key file"
                    );
                }
            }
            Ok(())
        }
        no_keys(root.path())?;
        Ok(())
    })();
    let client_cleanup = (|| -> Result<()> {
        if child.0.try_wait()?.is_none() {
            nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(i32::try_from(child.0.id())?),
                Signal::SIGKILL,
            )?;
            crate::support::process::wait(&mut child.0, Duration::from_secs(3))?;
        }
        Ok(())
    })();
    drop(pty);
    let cleanup = stop_servers(root.path());
    let failures: Vec<_> = [scenario, client_cleanup, cleanup]
        .into_iter()
        .filter_map(Result::err)
        .map(|error| format!("{error:#}"))
        .collect();
    ensure!(failures.is_empty(), "{}", failures.join("; "));
    println!(
        "PASS: real TTY cold startup and detach, no credential prompts, persistent local server"
    );
    Ok(())
}
