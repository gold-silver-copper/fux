//! Processes on Windows: their IDs, and a child's end told as `Chld`.
use crate::errno::{Errno, Result};
use std::fmt;
use std::num::NonZeroU32;
use std::os::windows::io::{AsHandle, AsRawHandle, OwnedHandle};

/// A process ID: a nonzero `DWORD`. 0 is the System Idle Process, which
/// fux never names.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct Pid(NonZeroU32);

impl Pid {
    /// `raw` as a process ID, if it is one: nonzero.
    pub fn from_raw(raw: u32) -> Option<Pid> {
        NonZeroU32::new(raw).map(Pid)
    }

    /// The ID of a child std started. `Option`, as on Unix, though std's
    /// `u32` is Windows' own type: only a 0 is refused.
    pub fn of(child: &std::process::Child) -> Option<Pid> {
        Pid::from_raw(child.id())
    }

    pub fn as_raw(self) -> u32 {
        self.0.get()
    }
}

impl fmt::Display for Pid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// A child's end, told as [`super::Signal::Chld`] to every
/// [`crate::signal::Pipe`] that asked for it, as Unix tells every child's.
/// It holds its own copy of the child's process handle, so the child may
/// be waited for or dropped meanwhile; dropped, it tells no more.
pub struct Watch {
    wait: windows_sys::Win32::Foundation::HANDLE,
    _process: OwnedHandle,
}

// SAFETY: a registered wait may be unregistered from any thread.
unsafe impl Send for Watch {}

/// Watches `process`, a child's handle (a std `Child`, or the handle a
/// pseudo console's `CreateProcessW` gives): once it ends (or at once, if
/// it has), `Chld` is raised, once, by a thread-pool wait whose callback
/// only raises it.
pub fn watch(process: impl AsHandle) -> Result<Watch> {
    use windows_sys::Win32::System::Threading::{
        INFINITE, RegisterWaitForSingleObject, WT_EXECUTEINWAITTHREAD, WT_EXECUTEONLYONCE,
    };
    let process = (process.as_handle().try_clone_to_owned())
        .map_err(|error| Errno::from_io_error(&error).unwrap_or_else(Errno::last))?;
    // Made now, so that the callback always has a port to wake.
    crate::poll::port()?;
    let mut wait = std::ptr::null_mut();
    // SAFETY: the process handle is this watch's own, open till the wait
    // is unregistered (`Drop`); `ended` takes no context and only raises a
    // signal, short enough to run in the wait thread.
    let registered = unsafe {
        RegisterWaitForSingleObject(
            &mut wait,
            process.as_raw_handle(),
            Some(ended),
            std::ptr::null(),
            INFINITE,
            WT_EXECUTEONLYONCE | WT_EXECUTEINWAITTHREAD,
        )
    };
    match registered {
        0 => Err(Errno::last()),
        _ => Ok(Watch {
            wait,
            _process: process,
        }),
    }
}

/// What the thread pool runs when a watched child ends.
unsafe extern "system" fn ended(_: *mut core::ffi::c_void, _: bool) {
    // A wake that can't be posted has nobody to tell.
    let _ = crate::signal::tell(super::Signal::Chld);
}

impl Drop for Watch {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
        use windows_sys::Win32::System::Threading::UnregisterWaitEx;
        // SAFETY: unregisters this watch's wait, once, waiting for a
        // callback already running to finish, before the process handle
        // it waits on closes.
        unsafe { UnregisterWaitEx(self.wait, INVALID_HANDLE_VALUE) };
    }
}
