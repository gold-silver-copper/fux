//! `SIGINT`/`SIGTERM` → `Inbound::Signal` through a self-pipe (prompt 3.1): the handler only
//! `write(2)`s the signal number to a non-blocking pipe; an `IoTaskPool` task reads the pipe
//! and forwards typed messages on the runner's control channel, which the runner polls ahead
//! of pane output. `SIGHUP` is ignored on the server; panes get theirs through
//! `Effect::Terminate`.
//!
//! The only `unsafe` in the crate lives here: installing the handler.
#![allow(unsafe_code)]

use std::os::fd::{IntoRawFd, OwnedFd};
use std::sync::atomic::{AtomicI32, Ordering};

use async_channel::Sender;
use async_io::Async;
use bevy_ecs::error::BevyError;
use bevy_tasks::IoTaskPool;
use nix::fcntl::{FcntlArg, FdFlag, OFlag, fcntl};
use nix::libc;
use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, sigaction};

use crate::model::{Inbound, Signal as Sig};

/// Write end of the self-pipe; `-1` until installed.
static WRITE_FD: AtomicI32 = AtomicI32::new(-1);

extern "C" fn on_signal(signal: libc::c_int) {
    let fd = WRITE_FD.load(Ordering::Relaxed);
    if fd < 0 {
        return;
    }
    let byte = [signal as u8];
    // SAFETY: `write(2)` is async-signal-safe; the fd is a valid, non-blocking pipe end that is
    // never closed after installation, and the buffer outlives the call. A full pipe (EAGAIN)
    // is fine: a pending byte already wakes the reader.
    unsafe {
        libc::write(fd, byte.as_ptr().cast(), 1);
    }
}

/// Installs the handlers once and spawns the reader task, which forwards `Inbound::Signal` on
/// `control` (the runner's priority channel). Calling twice is an error.
pub fn install(control: Sender<Inbound>) -> Result<(), BevyError> {
    install_with(control, Inbound::Signal)
}

/// [`install`] for any runner message type: `wrap` turns the received signal into the host's
/// control message (zor wraps it into its own `Inbound`).
pub fn install_with<I: Send + 'static>(
    control: Sender<I>,
    wrap: fn(Sig) -> I,
) -> Result<(), BevyError> {
    if WRITE_FD.load(Ordering::Relaxed) >= 0 {
        return Err(BevyError::from("signal handlers already installed"));
    }
    let (read, write) = nix::unistd::pipe()?;
    for fd in [&read, &write] {
        fcntl(fd, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC))?;
        fcntl(fd, FcntlArg::F_SETFL(OFlag::O_NONBLOCK))?;
    }
    WRITE_FD.store(write.into_raw_fd(), Ordering::SeqCst);
    let action = SigAction::new(
        SigHandler::Handler(on_signal),
        SaFlags::SA_RESTART,
        SigSet::empty(),
    );
    let ignore = SigAction::new(SigHandler::SigIgn, SaFlags::empty(), SigSet::empty());
    // SAFETY: `on_signal` only touches an atomic and calls `write(2)`, both async-signal-safe;
    // no other code in the process installs handlers for these signals.
    unsafe {
        sigaction(Signal::SIGINT, &action)?;
        sigaction(Signal::SIGTERM, &action)?;
        sigaction(Signal::SIGHUP, &ignore)?;
    }
    let reader = Async::new(read)?;
    IoTaskPool::get()
        .spawn(forward(reader, control, wrap))
        .detach();
    Ok(())
}

async fn forward<I>(reader: Async<OwnedFd>, control: Sender<I>, wrap: fn(Sig) -> I) {
    let mut buf = [0u8; 16];
    loop {
        let n = match reader
            .read_with(|fd| nix::unistd::read(fd, &mut buf).map_err(std::io::Error::from))
            .await
        {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        for &byte in buf.iter().take(n) {
            let signal = match i32::from(byte) {
                libc::SIGINT => Sig::Interrupt,
                libc::SIGTERM => Sig::Terminate,
                _ => continue,
            };
            if control.send(wrap(signal)).await.is_err() {
                return;
            }
        }
    }
}
