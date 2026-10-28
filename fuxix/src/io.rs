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

/// Points this process's stdout at what `to` is open on (`dup2` onto 1),
/// closing what it was: a daemon done with its stdout points it at
/// `/dev/null`, so nothing later opened takes descriptor 1.
pub fn redirect_stdout(to: impl AsFd) -> Result<()> {
    // SAFETY: dup2 takes two descriptor numbers and touches no memory; the
    // one it replaces is stdout, which std writes through by number.
    check(unsafe { libc::dup2(to.as_fd().as_raw_fd(), libc::STDOUT_FILENO) }).map(drop)
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
/// so it may run between `fork` and `exec`, with `listed` taken before
/// the fork. Linux's close_range, else one at a time ([`cloexec_each`]),
/// which on macOS leaves one window: a descriptor another thread opens
/// between the listing and the fork, past where it then lowers the soft
/// limit.
pub fn cloexec_from(first: i32, listed: Listed) -> Result<()> {
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

/// What [`cloexec_from`] must learn before `fork`, as learning it
/// allocates: on macOS, one past the highest descriptor this process holds
/// open, as `/dev/fd` lists them (`None` where they can't be listed).
/// Elsewhere the child lists them itself, so nothing.
#[derive(Clone, Copy, Debug)]
pub struct Listed {
    #[cfg(target_os = "macos")]
    end: Option<i32>,
}

impl Listed {
    pub fn now() -> Listed {
        Listed {
            #[cfg(target_os = "macos")]
            end: (std::fs::read_dir("/dev/fd").ok()).and_then(|listed| {
                let fds = listed.filter_map(|e| e.ok()?.file_name().to_str()?.parse::<i32>().ok());
                fds.max()?.checked_add(1)
            }),
        }
    }
}

/// `cloexec_from` one descriptor at a time, as the kernel lists them
/// (`/proc/self/fd`), read with no allocation: on Android, and on Linux
/// older than 5.11 or where close_range is denied. What it lists is what
/// is open as it reads, at any number. It fails where `/proc` is not
/// mounted, or with no number free for the listing (EMFILE).
#[cfg(not(target_os = "macos"))]
fn cloexec_each(first: i32, _: Listed) -> Result<()> {
    /// Entries as getdents64 writes them: each 8-byte aligned, as the
    /// kernel's own buffers are, whose fields it writes whole.
    #[repr(align(8))]
    struct Entries([u8; 1024]);
    let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC;
    // SAFETY: the path is a NUL-ended literal; open touches nothing else.
    let dir = check(unsafe { libc::open(c"/proc/self/fd".as_ptr(), flags) })?;
    // SAFETY: `dir` was just opened, and nothing else owns it.
    let dir = unsafe { OwnedFd::from_raw_fd(dir) };
    let mut buffer = Entries([0; 1024]);
    let room: libc::c_uint = crate::errno::fit(buffer.0.len())?;
    loop {
        // SAFETY: getdents64 writes at most `room` bytes, the buffer's
        // length, into it.
        let read = unsafe {
            libc::syscall(
                libc::SYS_getdents64,
                dir.as_raw_fd(),
                buffer.0.as_mut_ptr(),
                room,
            )
        };
        let read = usize::try_from(read).map_err(|_| crate::Errno::last())?;
        let mut entries = buffer.0.get(..read).unwrap_or_default();
        if entries.is_empty() {
            return Ok(());
        }
        // Each: its inode (8 bytes), offset (8), length (2) and type (1),
        // then its name, ended by a NUL: a number, or `.` and `..`.
        while let Some(&[low, high]) = entries.get(16..18) {
            let length = usize::from(u16::from_ne_bytes([low, high]));
            let (entry, rest) = (entries.split_at_checked(length))
                .filter(|_| length > 19)
                .ok_or(crate::Errno::INVAL)?;
            let name = entry.get(19..).unwrap_or_default().iter();
            let fd = (name.take_while(|b| **b != 0)).try_fold(0i32, |fd, b| {
                let digit = b.checked_sub(b'0').filter(|d| *d < 10)?;
                fd.checked_mul(10)?.checked_add(i32::from(digit))
            });
            match fd.filter(|fd| *fd >= first).map(cloexec_raw) {
                Some(Err(errno)) if errno.raw() != libc::EBADF => return Err(errno),
                Some(Ok(()) | Err(_)) | None => {}
            }
            entries = rest;
        }
    }
}

/// `cloexec_from` one descriptor at a time on macOS, which has neither
/// close_range nor a list a child can read without allocating: every
/// number below the listed end or the descriptor table's size, whichever
/// is larger. One opened since the listing is below the table's size, the
/// soft `RLIMIT_NOFILE`, which no new descriptor may reach; one opened
/// before the limit fell, as one a parent passes past `ulimit -Sn`, was
/// listed. Not one another thread, between the listing and the fork, both
/// opens past where the limit then falls to: nothing a child may call
/// lists it. Its cost follows the limit. With nothing listed it fails
/// (ENOENT, as `/dev/fd` is not there) rather than miss one.
#[cfg(target_os = "macos")]
fn cloexec_each(first: i32, listed: Listed) -> Result<()> {
    let listed = listed.end.ok_or(crate::Errno::from_raw(libc::ENOENT))?;
    // SAFETY: getdtablesize takes nothing and touches no memory.
    let table = unsafe { libc::getdtablesize() };
    for fd in first..listed.max(table) {
        match cloexec_raw(fd) {
            Err(errno) if errno.raw() != libc::EBADF => return Err(errno),
            Ok(()) | Err(_) => {}
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
        let mut buffer = [0u8; 16];
        assert_eq!(read(&reader, &mut buffer), Ok(5));
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
        cloexec_from(copy.as_raw_fd(), Listed::now()).map_err(|e| e.to_string())?;
        assert!(is_cloexec(&copy));
        // And one at a time, as where the kernel has no close_range.
        let listed = duplicate_inheritable(&file).map_err(|e| e.to_string())?;
        assert!(!is_cloexec(&listed));
        cloexec_each(listed.as_raw_fd(), Listed::now()).map_err(|e| e.to_string())?;
        assert!(is_cloexec(&listed));
        // With nothing listed, macOS fails rather than miss one.
        #[cfg(target_os = "macos")]
        assert_eq!(
            cloexec_each(listed.as_raw_fd(), Listed { end: None }),
            Err(Errno::from_raw(libc::ENOENT))
        );
        // Nothing was closed: both still read.
        let mut buffer = [0u8; 1];
        assert_eq!(read(&copy, &mut buffer), Ok(0));
        assert_eq!(read(&file, &mut buffer), Ok(0));
        Ok(())
    }

    /// One at a time, a descriptor at or above the soft limit (open before
    /// the limit fell, as one a parent passes past `ulimit -Sn`) is marked
    /// too. In a child, whose limit it lowers, making only
    /// async-signal-safe calls, one number freed below the limit, as Linux's
    /// listing takes one.
    #[test]
    fn one_at_a_time_marks_past_the_soft_limit() -> std::result::Result<(), String> {
        let file = std::fs::File::open("/dev/null").map_err(|e| e.to_string())?;
        let free = duplicate_inheritable(&file).map_err(|e| e.to_string())?;
        // SAFETY: F_DUPFD takes a number and touches no memory; the copy is
        // the lowest number free from it, without close-on-exec.
        let fd =
            check(unsafe { libc::fcntl(file.as_raw_fd(), libc::F_DUPFD, free.as_raw_fd() + 64) })
                .map_err(|e| e.to_string())?;
        // SAFETY: `fd` was just opened, and nothing else owns it.
        let high = unsafe { OwnedFd::from_raw_fd(fd) };
        let mut low = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: getrlimit fills one rlimit, and `low` is one.
        check(unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut low) })
            .map_err(|e| e.to_string())?;
        low.rlim_cur = libc::rlim_t::try_from(fd).map_err(|e| e.to_string())?;
        let listed = Listed::now();
        // SAFETY: fork takes nothing; the child makes only async-signal-safe
        // calls and ends with _exit.
        let pid = unsafe { libc::fork() };
        if pid == 0 {
            // A number free below the limit, held till now so no other
            // thread took it: Linux's listing opens one.
            // SAFETY: close takes a number, and `free` is the child's copy,
            // closed once, never used again as the child ends with _exit.
            unsafe { libc::close(free.as_raw_fd()) };
            // SAFETY: setrlimit reads one rlimit, and `low` is one; a lower
            // soft limit is always allowed.
            let lowered = unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raw const low) };
            let marked = lowered == 0 && cloexec_each(fd, listed).is_ok();
            let code = i32::from(!(marked && is_cloexec(&high)));
            // SAFETY: _exit ends the child at once, running nothing of the
            // parent's.
            unsafe { libc::_exit(code) };
        }
        assert!(pid > 0, "fork failed");
        let mut status = 0;
        // SAFETY: waitpid writes one int, and `status` is one.
        let waited = unsafe { libc::waitpid(pid, &raw mut status, 0) };
        assert_eq!(waited, pid);
        assert!(
            libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
            "not marked"
        );
        Ok(())
    }

    /// One at a time, every descriptor is marked: one opened after the
    /// listing, past a free number, too.
    #[test]
    fn one_at_a_time_marks_those_opened_since_the_listing() -> std::result::Result<(), String> {
        let file = std::fs::File::open("/dev/null").map_err(|e| e.to_string())?;
        let at = |n: i32| -> std::result::Result<OwnedFd, String> {
            // SAFETY: F_DUPFD takes a number and touches no memory; the copy
            // is the lowest number free from `n`, without close-on-exec.
            let raw = check(unsafe { libc::fcntl(file.as_raw_fd(), libc::F_DUPFD, n) })
                .map_err(|e| e.to_string())?;
            // SAFETY: `raw` was just opened, and nothing else owns it.
            Ok(unsafe { OwnedFd::from_raw_fd(raw) })
        };
        // Well below the soft limit (256 on macOS), well above std's own.
        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: getrlimit fills one rlimit, and `limit` is one.
        check(unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit) })
            .map_err(|e| e.to_string())?;
        let low = at(i32::try_from(limit.rlim_cur / 2).unwrap_or(700).min(700))?;
        let below = at(low.as_raw_fd() + 5)?;
        let listed = Listed::now();
        let next = at(below.as_raw_fd() + 1)?;
        let far = at(next.as_raw_fd() + 10)?;
        cloexec_each(low.as_raw_fd(), listed).map_err(|e| e.to_string())?;
        assert!(is_cloexec(&low) && is_cloexec(&below) && is_cloexec(&next));
        assert!(is_cloexec(&far), "one past a free number was left");
        Ok(())
    }
}
