//! The error a system call reports.
use std::fmt;

/// An `errno` value, from a call that failed.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Errno(i32);

/// A call's result.
pub type Result<T> = std::result::Result<T, Errno>;

impl Errno {
    pub const INTR: Self = Self(libc::EINTR);
    pub const AGAIN: Self = Self(libc::EAGAIN);
    pub const INPROGRESS: Self = Self(libc::EINPROGRESS);
    pub const MFILE: Self = Self(libc::EMFILE);
    pub const NFILE: Self = Self(libc::ENFILE);
    pub const SRCH: Self = Self(libc::ESRCH);
    pub const INVAL: Self = Self(libc::EINVAL);
    /// No room left: as `signal::notify` says, when every slot is held.
    pub const NOSPC: Self = Self(libc::ENOSPC);
    /// I/O error; a PTY master reports this when its slave closes.
    pub const IO: Self = Self(libc::EIO);
    /// XNU's kernel-private `EREDRIVEOPEN` (`bsd/sys/errno.h`), which no POSIX
    /// call should return: macOS leaks it from `open("/dev/ptmx")` when
    /// concurrent openers keep racing for one PTY. See `pty::open`.
    #[cfg(any(target_os = "macos", test))]
    pub(crate) const REDRIVEOPEN: Self = Self(-6);
    pub(crate) const NOTTY: Self = Self(libc::ENOTTY);
    pub(crate) const NAMETOOLONG: Self = Self(libc::ENAMETOOLONG);
    /// What macOS's `open("/dev/ptmx")` returns when it loses a race with
    /// another process's close, as well as when every PTY is in use (see
    /// `pty::open`).
    pub const NXIO: Self = Self(libc::ENXIO);

    /// The number.
    pub fn raw(self) -> i32 {
        self.0
    }

    /// The system description and code, with every owned buffer admitted
    /// before allocation. The ordinary strerror result fits on the stack;
    /// longer locale messages grow under the caller's shared grant.
    pub fn message<A: crate::Allocation>(
        self,
        budget: &mut A,
    ) -> std::result::Result<String, A::Error> {
        let before = budget.remaining();
        let mut stack = [0u8; 128];
        let mut heap = Vec::new();
        let result = (|| {
            loop {
                let bytes = if heap.is_empty() {
                    stack.as_mut_slice()
                } else {
                    heap.as_mut_slice()
                };
                // SAFETY: strerror_r writes only the supplied, initialized buffer.
                let status =
                    unsafe { libc::strerror_r(self.0, bytes.as_mut_ptr().cast(), bytes.len()) };
                if status == libc::ERANGE {
                    let room = bytes.len().checked_mul(2).ok_or_else(A::exceeded)?;
                    let more = room.checked_sub(heap.len()).ok_or_else(A::exceeded)?;
                    budget.reserve(&mut heap, more)?;
                    heap.resize(room, 0);
                    continue;
                }
                let end = bytes.iter().position(|b| *b == 0).ok_or_else(A::exceeded)?;
                let mut text = if (status == 0 || status == libc::EINVAL) && end > 0 {
                    budget.lossy(bytes.get(..end).ok_or_else(A::exceeded)?)?
                } else {
                    let mut text = String::new();
                    budget.write(&mut text, format_args!("Unknown error {}", self.0))?;
                    text
                };
                let capacity = heap.capacity();
                drop(std::mem::take(&mut heap));
                budget.release(capacity)?;
                budget.write(&mut text, format_args!(" (os error {})", self.0))?;
                return Ok(text);
            }
        })();
        drop(heap);
        if result.is_err() {
            budget.release(
                before
                    .checked_sub(budget.remaining())
                    .ok_or_else(A::exceeded)?,
            )?;
        }
        result
    }

    /// The `errno` a call reported as its number rather than in `errno`.
    pub(crate) const fn from_raw(raw: i32) -> Self {
        Self(raw)
    }

    /// The `errno` behind an I/O error, if it came from the system.
    pub fn from_io_error(error: &std::io::Error) -> Option<Self> {
        error.raw_os_error().map(Self)
    }

    /// The calling thread's `errno`, read straight after a call failed.
    pub(crate) fn last() -> Self {
        Self(
            std::io::Error::last_os_error()
                .raw_os_error()
                .unwrap_or(libc::EIO),
        )
    }
}

/// A call's return value, where -1 means it failed and set `errno`.
pub(crate) fn check(value: libc::c_int) -> Result<libc::c_int> {
    if value == -1 {
        Err(Errno::last())
    } else {
        Ok(value)
    }
}

/// `value` as the type a call or a struct field takes, which may differ by
/// platform: [`Errno::INVAL`] if it does not fit.
pub(crate) fn fit<T: TryFrom<U>, U>(value: U) -> Result<T> {
    T::try_from(value).map_err(|_| Errno::INVAL)
}

/// A byte count, where -1 means the call failed and set `errno`.
pub(crate) fn check_size(value: isize) -> Result<usize> {
    usize::try_from(value).map_err(|_| Errno::last())
}

/// Shown as the system describes it: "Interrupted system call (os error 4)".
impl fmt::Display for Errno {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&std::io::Error::from_raw_os_error(self.0), f)
    }
}

impl fmt::Debug for Errno {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Errno({}: {self})", self.0)
    }
}

impl std::error::Error for Errno {}

impl From<Errno> for std::io::Error {
    fn from(errno: Errno) -> Self {
        Self::from_raw_os_error(errno.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_errno_reads_as_the_system_says_and_round_trips() {
        assert_eq!(
            Errno::INTR.to_string(),
            std::io::Error::from_raw_os_error(libc::EINTR).to_string()
        );
        let mut grant = 4096usize;
        let message = Errno::INTR.message(&mut grant).unwrap_or_default();
        assert!(message.starts_with("Interrupted system call"));
        assert!(message.ends_with("(os error 4)"));
        assert_eq!(4096usize.saturating_sub(grant), message.capacity());
        let mut tiny = 1usize;
        assert!(Errno::INTR.message(&mut tiny).is_err());
        assert_eq!(tiny, 1);
        let io: std::io::Error = Errno::MFILE.into();
        assert_eq!(Errno::from_io_error(&io), Some(Errno::MFILE));
        assert_eq!(Errno::from_raw(libc::EINTR), Errno::INTR);
        assert_eq!(check(3), Ok(3));
        assert_eq!(check_size(7), Ok(7));
    }
}
