//! Process-group control the wrapper needs beyond zor's shared process helpers.
#![allow(unsafe_code)]
use std::io;
use zor::platform::Pid;

pub fn forward_signal(pgid: Pid, signal: i32) -> io::Result<()> {
    unsafe {
        // SAFETY: kill validates the process-group id and signal number.
        if libc::kill(-pgid, signal) == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

pub fn suspend_self() {
    unsafe {
        // SAFETY: SIGSTOP has defined process-wide semantics and no handler.
        libc::raise(libc::SIGSTOP);
    }
}
