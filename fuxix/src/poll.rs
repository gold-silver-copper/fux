//! Waiting on several descriptors at once.
use crate::errno::{Result, check, fit};
use std::marker::PhantomData;
use std::ops::{BitOr, BitOrAssign};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::time::Duration;

/// What a descriptor is waited for, or was found ready for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Events(libc::c_short);

impl Events {
    /// None asked for: what is told unasked (`HUP`, `ERR`, `NVAL`) only.
    pub const NONE: Self = Self(0);
    pub const IN: Self = Self(libc::POLLIN);
    pub const OUT: Self = Self(libc::POLLOUT);
    pub const ERR: Self = Self(libc::POLLERR);
    pub const HUP: Self = Self(libc::POLLHUP);
    pub const NVAL: Self = Self(libc::POLLNVAL);

    /// Whether any event of `other` is here.
    pub fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl BitOr for Events {
    type Output = Self;
    fn bitor(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

impl BitOrAssign for Events {
    fn bitor_assign(&mut self, other: Self) {
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
    pub fn new(fd: &'fd impl AsFd, events: Events) -> Self {
        Self {
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

/// Reusable poll storage. Only a live list borrows descriptors; the next
/// list clears the saved descriptor numbers before any are waited on.
#[derive(Default)]
pub struct Poll {
    fds: Vec<libc::pollfd>,
}

/// Descriptors borrowed until this list has finished waiting on them.
pub struct List<'poll, 'fd> {
    fds: &'poll mut Vec<libc::pollfd>,
    fd: PhantomData<BorrowedFd<'fd>>,
}

impl Poll {
    pub fn list<'fd>(&mut self) -> List<'_, 'fd> {
        self.fds.clear();
        List {
            fds: &mut self.fds,
            fd: PhantomData,
        }
    }
}

impl<'fd> List<'_, 'fd> {
    pub fn push(&mut self, fd: PollFd<'fd>) {
        self.fds.push(fd.raw);
    }

    pub fn poll(&mut self, timeout: Option<Duration>) -> Result<usize> {
        let count = fit(self.fds.len())?;
        fit(wait(self.fds.as_mut_ptr(), count, timeout)?)
    }

    pub fn revents(&self) -> impl Iterator<Item = Events> + '_ {
        self.fds.iter().map(|fd| Events(fd.revents))
    }
}

/// Waits until one of `fds` is ready, or `timeout` passes (`None`: no
/// timeout). How many are ready. On Linux and Android the timeout is exact
/// (`ppoll`); on macOS it is rounded up to a whole millisecond, so a short
/// one does not become a busy loop.
pub fn poll(fds: &mut [PollFd<'_>], timeout: Option<Duration>) -> Result<usize> {
    let count: libc::nfds_t = fit(fds.len())?;
    let ready = wait(fds.as_mut_ptr().cast(), count, timeout)?;
    fit(ready)
}

#[cfg(not(target_os = "macos"))]
fn wait(
    fds: *mut libc::pollfd,
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
    // SAFETY: Both callers supply `count` initialized pollfds, writable
    // for this call, with every descriptor borrowed until it returns.
    // The timeout is null or a whole timespec, and no signal mask is passed.
    check(unsafe { libc::ppoll(fds, count, limit, std::ptr::null()) })
}

#[cfg(target_os = "macos")]
fn wait(
    fds: *mut libc::pollfd,
    count: libc::nfds_t,
    timeout: Option<Duration>,
) -> Result<libc::c_int> {
    let millis = timeout.map_or(-1, |t| {
        libc::c_int::try_from(t.as_nanos().div_ceil(1_000_000)).unwrap_or(libc::c_int::MAX)
    });
    // SAFETY: Both callers supply `count` initialized pollfds, writable
    // for this call, with every descriptor borrowed until it returns.
    check(unsafe { libc::poll(fds, count, millis) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::time::Instant;

    #[test]
    fn a_reused_list_keeps_only_its_current_descriptors() -> std::result::Result<(), String> {
        let (ready, mut writer) = std::io::pipe().map_err(|e| e.to_string())?;
        let (idle, _writer) = std::io::pipe().map_err(|e| e.to_string())?;
        writer.write_all(b"x").map_err(|e| e.to_string())?;
        let mut storage = Poll::default();
        {
            let mut list = storage.list();
            list.push(PollFd::new(&ready, Events::IN));
            assert_eq!(list.poll(Some(Duration::ZERO)), Ok(1));
            assert!(list.revents().all(|flags| flags.intersects(Events::IN)));
        }
        let capacity = storage.fds.capacity();
        let mut list = storage.list();
        list.push(PollFd::new(&idle, Events::IN));
        assert_eq!(list.poll(Some(Duration::ZERO)), Ok(0));
        assert_eq!(list.revents().count(), 1);
        assert!(list.revents().all(|flags| flags.is_empty()));
        assert_eq!(list.fds.capacity(), capacity);
        Ok(())
    }

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
        assert!(read.revents().intersects(Events::IN));
        assert!(write.revents().intersects(Events::OUT | Events::HUP));
        assert!(!write.revents().intersects(Events::IN));
        Ok(())
    }
}
