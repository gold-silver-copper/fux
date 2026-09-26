//! Reading, writing and the flags of a descriptor.
use crate::errno::{Errno, Result, check, check_size};
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

/// Makes reads and writes on `fd` return `AGAIN` rather than wait.
pub fn set_nonblocking(fd: impl AsFd, nonblocking: bool) -> Result<()> {
    let raw = fd.as_fd().as_raw_fd();
    // SAFETY: F_GETFL takes no argument and touches no memory.
    let flags = check(unsafe { libc::fcntl(raw, libc::F_GETFL) })?;
    let flags = if nonblocking {
        flags | libc::O_NONBLOCK
    } else {
        flags & !libc::O_NONBLOCK
    };
    // SAFETY: F_SETFL takes an int and touches no memory.
    check(unsafe { libc::fcntl(raw, libc::F_SETFL, flags) }).map(drop)
}

/// Closes `fd` when this process runs another program.
pub fn set_cloexec(fd: impl AsFd) -> Result<()> {
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
/// process runs another program.
pub fn cloexec_from(first: i32) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        let start = libc::c_uint::try_from(first).map_err(|_| Errno::INVAL)?;
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
    cloexec_listed(first)
}

/// `cloexec_from` by listing this process's descriptors: on macOS, on
/// Android, and on Linux older than 5.11.
fn cloexec_listed(first: i32) -> Result<()> {
    let listing = if cfg!(target_os = "macos") {
        "/dev/fd"
    } else {
        "/proc/self/fd"
    };
    // Listed first, then marked: the listing's own descriptor is closed by
    // then, and is skipped as not open.
    let open: Vec<libc::c_int> = std::fs::read_dir(listing)
        .map_err(|e| Errno::from_io_error(&e).unwrap_or(Errno::INVAL))?
        .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse().ok())
        .filter(|fd| *fd >= first)
        .collect();
    for fd in open {
        match cloexec_raw(fd) {
            Ok(()) => {}
            Err(errno) if errno.raw() == libc::EBADF => {}
            Err(errno) => return Err(errno),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let mut buffer = [0u8; 16];
        assert_eq!(read(&reader, &mut buffer), Ok(5));
        assert_eq!(buffer.get(..5), Some(&b"hello"[..]));
        set_nonblocking(&reader, true).map_err(|e| e.to_string())?;
        assert_eq!(read(&reader, &mut buffer), Err(Errno::AGAIN));
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
        cloexec_from(copy.as_raw_fd()).map_err(|e| e.to_string())?;
        assert!(is_cloexec(&copy));
        // And by listing, as where the kernel has no close_range.
        let listed = duplicate_inheritable(&file).map_err(|e| e.to_string())?;
        assert!(!is_cloexec(&listed));
        cloexec_listed(listed.as_raw_fd()).map_err(|e| e.to_string())?;
        assert!(is_cloexec(&listed));
        // Nothing was closed: both still read.
        let mut buffer = [0u8; 1];
        assert_eq!(read(&copy, &mut buffer), Ok(0));
        assert_eq!(read(&file, &mut buffer), Ok(0));
        Ok(())
    }
}
