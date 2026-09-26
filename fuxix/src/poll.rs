//! Waiting on several descriptors at once.
use crate::errno::{Errno, Result, check};
use std::marker::PhantomData;
use std::ops::{BitOr, BitOrAssign};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::time::Duration;

/// What a descriptor is waited for, or was found ready for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Events(libc::c_short);

impl Events {
    pub const IN: Events = Events(libc::POLLIN);
    pub const OUT: Events = Events(libc::POLLOUT);
    pub const ERR: Events = Events(libc::POLLERR);
    pub const HUP: Events = Events(libc::POLLHUP);
    pub const NVAL: Events = Events(libc::POLLNVAL);

    /// Whether every event of `other` is here.
    pub fn contains(self, other: Events) -> bool {
        self.0 & other.0 == other.0
    }

    /// Whether any event of `other` is here.
    pub fn intersects(self, other: Events) -> bool {
        self.0 & other.0 != 0
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl BitOr for Events {
    type Output = Events;
    fn bitor(self, other: Events) -> Events {
        Events(self.0 | other.0)
    }
}

impl BitOrAssign for Events {
    fn bitor_assign(&mut self, other: Events) {
        self.0 |= other.0;
    }
}

/// A descriptor to wait on, borrowed for as long as it is waited on.
#[repr(transparent)]
pub struct PollFd<'fd> {
    raw: libc::pollfd,
    fd: PhantomData<BorrowedFd<'fd>>,
}

impl<'fd> PollFd<'fd> {
    pub fn new(fd: &'fd impl AsFd, events: Events) -> PollFd<'fd> {
        PollFd {
            raw: libc::pollfd {
                fd: fd.as_fd().as_raw_fd(),
                events: events.0,
                revents: 0,
            },
            fd: PhantomData,
        }
    }

    /// What `poll` found it ready for.
    pub fn revents(&self) -> Events {
        Events(self.raw.revents)
    }
}

/// Waits until one of `fds` is ready, or `timeout` passes (`None`: no
/// timeout). How many are ready. On Linux and Android the timeout is exact
/// (`ppoll`); on macOS it is rounded up to a whole millisecond, so a short
/// one does not become a busy loop.
pub fn poll(fds: &mut [PollFd<'_>], timeout: Option<Duration>) -> Result<usize> {
    let count = libc::nfds_t::try_from(fds.len()).map_err(|_| Errno::INVAL)?;
    let ready = wait(fds, count, timeout)?;
    usize::try_from(ready).map_err(|_| Errno::INVAL)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn wait(
    fds: &mut [PollFd<'_>],
    count: libc::nfds_t,
    timeout: Option<Duration>,
) -> Result<libc::c_int> {
    // The field types differ between platforms and C libraries, so they
    // are inferred. Seconds past what a 32-bit field holds are 68 years.
    let timespec = timeout.map(|t| libc::timespec {
        tv_sec: t.as_secs().try_into().unwrap_or(i32::MAX.into()),
        // Under a billion, so it fits an i32, and so every platform's type.
        tv_nsec: i32::try_from(t.subsec_nanos()).unwrap_or(0).into(),
    });
    let limit = timespec
        .as_ref()
        .map_or(std::ptr::null(), std::ptr::from_ref);
    // SAFETY: PollFd is repr(transparent) over pollfd, so `fds` is `count`
    // pollfds, which the call reads and whose revents it writes; each
    // descriptor is borrowed for the call. The timeout is null or a whole
    // timespec, and no signal mask is passed.
    check(unsafe { libc::ppoll(fds.as_mut_ptr().cast(), count, limit, std::ptr::null()) })
}

#[cfg(target_os = "macos")]
fn wait(
    fds: &mut [PollFd<'_>],
    count: libc::nfds_t,
    timeout: Option<Duration>,
) -> Result<libc::c_int> {
    let millis = timeout.map_or(-1, |t| {
        libc::c_int::try_from(t.as_nanos().div_ceil(1_000_000)).unwrap_or(libc::c_int::MAX)
    });
    // SAFETY: PollFd is repr(transparent) over pollfd, so `fds` is `count`
    // pollfds, which the call reads and whose revents it writes; each
    // descriptor is borrowed for the call.
    check(unsafe { libc::poll(fds.as_mut_ptr().cast(), count, millis) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::time::Instant;

    #[test]
    fn a_pipe_is_ready_once_written_and_a_timeout_passes() -> std::result::Result<(), String> {
        let (reader, mut writer) = std::io::pipe().map_err(|e| e.to_string())?;
        let start = Instant::now();
        let mut fds = [PollFd::new(&reader, Events::IN)];
        // A tenth of a millisecond still waits.
        assert_eq!(poll(&mut fds, Some(Duration::from_micros(100))), Ok(0));
        assert!(start.elapsed() >= Duration::from_micros(100));
        writer.write_all(b"x").map_err(|e| e.to_string())?;
        let mut fds = [
            PollFd::new(&reader, Events::IN),
            PollFd::new(&writer, Events::OUT | Events::IN),
        ];
        assert_eq!(poll(&mut fds, None), Ok(2));
        let [read, write] = &fds;
        assert!(read.revents().contains(Events::IN));
        assert!(write.revents().intersects(Events::OUT | Events::HUP));
        assert!(!write.revents().contains(Events::IN));
        Ok(())
    }
}
