//! The macOS system calls fux makes that neither std nor rustix offers as
//! safe functions, each behind one. Every value returned is valid as it
//! stands: process IDs and groups are positive, a failure is `None` or an
//! error. On other platforms the crate is empty.
//!
//! `foreground_group` and `session` are here only because rustix 1.1 builds
//! a `Pid` from their results unchecked on macOS, where 0 is undefined
//! behaviour. Once rustix rejects 0 there as it does on Linux, they can go
//! back to `rustix::termios::tcgetpgrp` and `rustix::process::getsid`.
#![cfg(target_os = "macos")]

use std::os::fd::{AsFd, AsRawFd};
use std::path::PathBuf;

/// The foreground process group of the terminal `terminal`, if it has one.
pub fn foreground_group(terminal: impl AsFd) -> Option<i32> {
    // SAFETY: the descriptor is valid for the call's duration.
    let group = unsafe { libc::tcgetpgrp(terminal.as_fd().as_raw_fd()) };
    (group > 0).then_some(group)
}

/// The session of process `pid`, which may be 0 for a kernel process.
pub fn session(pid: i32) -> Option<i32> {
    // SAFETY: getsid takes a plain number and touches no memory of ours.
    let sid = unsafe { libc::getsid(pid) };
    (sid >= 0).then_some(sid)
}

/// The effective user ID of the process at the other end of the Unix
/// socket `socket`.
pub fn peer_uid(socket: impl AsFd) -> std::io::Result<u32> {
    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    // SAFETY: the descriptor is valid for the call's duration and both
    // out-pointers point at initialised locals.
    let result = unsafe { libc::getpeereid(socket.as_fd().as_raw_fd(), &mut uid, &mut gid) };
    if result == 0 {
        Ok(uid)
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// The current directory of process `pid`, when the system says.
pub fn cwd(pid: i32) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let mut info = std::mem::MaybeUninit::<libc::proc_vnodepathinfo>::zeroed();
    let bytes = libc::c_int::try_from(std::mem::size_of::<libc::proc_vnodepathinfo>()).ok()?;
    // SAFETY: the buffer is exactly one `proc_vnodepathinfo`, which the call
    // fills; a short return is checked before the value is read.
    let read = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            info.as_mut_ptr().cast(),
            bytes,
        )
    };
    if read != bytes {
        return None;
    }
    // SAFETY: fully written, as checked above.
    let info = unsafe { info.assume_init() };
    let path: Vec<u8> = info
        .pvi_cdir
        .vip_path
        .iter()
        .flatten()
        .take_while(|c| **c != 0)
        .map(|c| c.cast_unsigned())
        .collect();
    (!path.is_empty()).then(|| PathBuf::from(std::ffi::OsStr::from_bytes(&path)))
}

/// Every process ID the system lists.
pub fn processes() -> Vec<i32> {
    // SAFETY: a null buffer asks only for the count.
    let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    // Room for processes started between the two calls.
    let Some(room) = usize::try_from(count).ok().and_then(|c| c.checked_add(64)) else {
        return Vec::new();
    };
    let mut pids: Vec<libc::pid_t> = vec![0; room];
    let Ok(bytes) = libc::c_int::try_from(std::mem::size_of_val(pids.as_slice())) else {
        return Vec::new();
    };
    // SAFETY: the buffer is `bytes` long and writable.
    let listed = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
    pids.truncate(usize::try_from(listed).unwrap_or(0));
    pids.retain(|pid| *pid > 0);
    pids
}
