//! Reading, writing and the flags of a descriptor.
use crate::errno::{Result, check, check_size};
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};

/// Reads into `buffer`: how many bytes, 0 at the end.
pub fn read(fd: impl AsFd, buffer: &mut [u8]) -> Result<usize> {
    // SAFETY: the descriptor is valid for the call, and `buffer` is
    // writable for its whole length.
    let count = unsafe {
        libc::read(
            fd.as_fd().as_raw_fd(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
        )
    };
    check_size(count)
}

/// Writes from `bytes`: how many were written.
pub fn write(fd: impl AsFd, bytes: &[u8]) -> Result<usize> {
    // SAFETY: the descriptor is valid for the call, and `bytes` is readable
    // for its whole length.
    let count = unsafe { libc::write(fd.as_fd().as_raw_fd(), bytes.as_ptr().cast(), bytes.len()) };
    check_size(count)
}

/// How many bytes wait to be read from `fd` (FIONREAD): output a program
/// wrote to its terminal that its reader has not read yet.
pub fn unread(fd: impl AsFd) -> Result<usize> {
    let mut count: libc::c_int = 0;
    // SAFETY: FIONREAD writes one c_int, and `count` is one.
    check(unsafe { libc::ioctl(fd.as_fd().as_raw_fd(), libc::FIONREAD, &mut count) })?;
    Ok(usize::try_from(count).unwrap_or(0))
}

/// Makes reads and writes on `fd` return `AGAIN` rather than wait.
pub fn set_nonblocking(fd: impl AsFd, nonblocking: bool) -> Result<()> {
    let raw = fd.as_fd().as_raw_fd();
    // SAFETY: F_GETFL takes no argument and touches no memory.
    let flags = check(unsafe { libc::fcntl(raw, libc::F_GETFL) })?;
    let wanted = if nonblocking {
        flags | libc::O_NONBLOCK
    } else {
        flags & !libc::O_NONBLOCK
    };
    // Already as asked: nothing to set.
    if wanted == flags {
        return Ok(());
    }
    // SAFETY: F_SETFL takes an int and touches no memory.
    check(unsafe { libc::fcntl(raw, libc::F_SETFL, wanted) }).map(drop)
}

/// Closes `fd` when this process runs another program: on macOS, for what
/// it cannot open close-on-exec at once.
#[cfg(target_os = "macos")]
pub(crate) fn set_cloexec(fd: impl AsFd) -> Result<()> {
    cloexec_raw(fd.as_fd().as_raw_fd())
}

fn cloexec_raw(raw: libc::c_int) -> Result<()> {
    // SAFETY: F_GETFD takes no argument and touches no memory; on a
    // descriptor that is not open it fails with EBADF.
    let flags = check(unsafe { libc::fcntl(raw, libc::F_GETFD) })?;
    // SAFETY: F_SETFD takes an int and touches no memory.
    check(unsafe { libc::fcntl(raw, libc::F_SETFD, flags | libc::FD_CLOEXEC) }).map(drop)
}

/// A copy of `fd` that a program this process runs inherits, as it would
/// any descriptor opened without close-on-exec. For tests of what a pane's
/// program inherits.
pub fn duplicate_inheritable(fd: impl AsFd) -> Result<OwnedFd> {
    // SAFETY: dup takes a descriptor number and touches no memory; its
    // copy is made without close-on-exec.
    let copy = check(unsafe { libc::dup(fd.as_fd().as_raw_fd()) })?;
    // SAFETY: `copy` was just opened, is valid, and nothing else owns it.
    Ok(unsafe { OwnedFd::from_raw_fd(copy) })
}

/// Marks every descriptor from `first` up close-on-exec, whoever opened it
/// and however: descriptors this process inherited without the flag, and
/// ones another thread opened before it could set it. Nothing is closed,
/// so no owner's descriptor stops being valid; each closes when this
/// process runs another program. It allocates nothing and takes no lock,
/// so it may run between `fork` and `exec`. `listed` ([`open_end`], taken
/// before the fork) bounds the fallback where the kernel has no
/// close_range.
pub fn cloexec_from(first: i32, listed: Option<i32>) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        let start: libc::c_uint = crate::errno::fit(first)?;
        // SAFETY: close_range takes three plain numbers and touches no
        // memory; with CLOSE_RANGE_CLOEXEC it closes nothing.
        let done = unsafe {
            libc::syscall(
                libc::SYS_close_range,
                start,
                libc::c_uint::MAX,
                libc::CLOSE_RANGE_CLOEXEC,
            )
        };
        if done == 0 {
            return Ok(());
        }
        // Older than Linux 5.11: no such call (ENOSYS) or flag (EINVAL).
    }
    cloexec_each(first, listed)
}

/// One past the highest descriptor this process holds open, as `/dev/fd`
/// lists them; `None` where they can't be listed. It allocates, so it is
/// for before `fork`.
pub fn open_end() -> Option<i32> {
    let listed = std::fs::read_dir("/dev/fd").ok()?;
    let fds = listed.filter_map(|entry| entry.ok()?.file_name().to_str()?.parse::<i32>().ok());
    fds.max()?.checked_add(1)
}

/// `cloexec_from` one descriptor at a time: on macOS, on Android, and on
/// Linux older than 5.11 or where close_range is denied. Below the number
/// this process may open (its soft `RLIMIT_NOFILE`, which may be 2^30),
/// and, with `listed`, to the first free number past the listed end: one
/// opened since the listing took the lowest number free, so those past
/// the end lie next to it. Its cost follows the descriptors open, not the
/// limit.
fn cloexec_each(first: i32, listed: Option<i32>) -> Result<()> {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: getrlimit fills one rlimit, and `limit` is one.
    check(unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit) })?;
    let end = libc::c_int::try_from(limit.rlim_cur).unwrap_or(libc::c_int::MAX);
    for fd in first..end {
        match cloexec_raw(fd) {
            Ok(()) => {}
            Err(errno) if errno.raw() == libc::EBADF => {
                if listed.is_some_and(|end| fd >= end) {
                    return Ok(());
                }
            }
            Err(errno) => return Err(errno),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Errno;
    use std::os::fd::AsRawFd;

    fn is_cloexec(fd: impl AsFd) -> bool {
        // SAFETY: F_GETFD on a valid descriptor touches no memory.
        let flags = unsafe { libc::fcntl(fd.as_fd().as_raw_fd(), libc::F_GETFD) };
        flags >= 0 && flags & libc::FD_CLOEXEC != 0
    }

    #[test]
    fn bytes_go_through_a_pipe_and_flags_stick() -> std::result::Result<(), String> {
        let (reader, writer) = std::io::pipe().map_err(|e| e.to_string())?;
        assert_eq!(write(&writer, b"hello"), Ok(5));
        assert_eq!(unread(&reader), Ok(5));
        let mut buffer = [0u8; 16];
        assert_eq!(read(&reader, &mut buffer), Ok(5));
        assert_eq!(unread(&reader), Ok(0));
        assert_eq!(buffer.get(..5), Some(&b"hello"[..]));
        set_nonblocking(&reader, true).map_err(|e| e.to_string())?;
        assert_eq!(read(&reader, &mut buffer), Err(Errno::AGAIN));
        // Asked again, it is already so; asked the other way, it is not.
        set_nonblocking(&reader, true).map_err(|e| e.to_string())?;
        assert_eq!(read(&reader, &mut buffer), Err(Errno::AGAIN));
        set_nonblocking(&reader, false).map_err(|e| e.to_string())?;
        // SAFETY: F_GETFL on a valid descriptor touches no memory.
        let flags = unsafe { libc::fcntl(reader.as_raw_fd(), libc::F_GETFL) };
        assert!(flags >= 0 && flags & libc::O_NONBLOCK == 0);
        // Not "and 0 once the writer is dropped": on macOS std makes a pipe
        // close-on-exec a step after it makes it, and a child another test
        // spawns in between keeps the writer open.
        Ok(())
    }

    #[test]
    fn an_inheritable_copy_is_marked_again_by_cloexec_from() -> std::result::Result<(), String> {
        let file = std::fs::File::open("/dev/null").map_err(|e| e.to_string())?;
        assert!(is_cloexec(&file), "std opens close-on-exec");
        let copy = duplicate_inheritable(&file).map_err(|e| e.to_string())?;
        assert!(!is_cloexec(&copy));
        cloexec_from(copy.as_raw_fd(), open_end()).map_err(|e| e.to_string())?;
        assert!(is_cloexec(&copy));
        // And one at a time, as where the kernel has no close_range.
        let listed = duplicate_inheritable(&file).map_err(|e| e.to_string())?;
        assert!(!is_cloexec(&listed));
        cloexec_each(listed.as_raw_fd(), None).map_err(|e| e.to_string())?;
        assert!(is_cloexec(&listed));
        // Nothing was closed: both still read.
        let mut buffer = [0u8; 1];
        assert_eq!(read(&copy, &mut buffer), Ok(0));
        assert_eq!(read(&file, &mut buffer), Ok(0));
        Ok(())
    }

    /// One at a time, the walk goes to the listed end, past any number
    /// free below it, then on through the run of descriptors opened next
    /// to it, and ends at the first free number past that: not at the soft
    /// limit, which may be 2^30.
    #[test]
    fn one_at_a_time_ends_past_the_listed_descriptors() -> std::result::Result<(), String> {
        let file = std::fs::File::open("/dev/null").map_err(|e| e.to_string())?;
        let at = |n: i32| -> std::result::Result<OwnedFd, String> {
            // SAFETY: F_DUPFD takes a number and touches no memory; the copy
            // is the lowest number free from `n`, without close-on-exec.
            let raw = check(unsafe { libc::fcntl(file.as_raw_fd(), libc::F_DUPFD, n) })
                .map_err(|e| e.to_string())?;
            // SAFETY: `raw` was just opened, and nothing else owns it.
            Ok(unsafe { OwnedFd::from_raw_fd(raw) })
        };
        let low = at(700)?;
        let below = at(low.as_raw_fd() + 5)?;
        let end = below.as_raw_fd() + 1;
        let next = at(end)?;
        let far = at(next.as_raw_fd() + 10)?;
        cloexec_each(low.as_raw_fd(), Some(end)).map_err(|e| e.to_string())?;
        assert!(is_cloexec(&low) && is_cloexec(&below) && is_cloexec(&next));
        assert!(
            !is_cloexec(&far),
            "the walk went past the first free number"
        );
        Ok(())
    }
}
