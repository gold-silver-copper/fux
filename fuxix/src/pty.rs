//! Pseudoterminals.
use crate::errno::{Errno, check};
use crate::terminal;
use std::fmt;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

/// A step of opening a PTY that failed, and why.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Error {
    pub call: &'static str,
    pub errno: Errno,
}

/// "grantpt: Permission denied (os error 13)".
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.call, self.errno)
    }
}

impl std::error::Error for Error {}

fn at(call: &'static str) -> impl FnOnce(Errno) -> Error {
    move |errno| Error { call, errno }
}

/// Opens a PTY of `rows` by `cols`: its master and its slave, both
/// close-on-exec, neither the controlling terminal of this process.
pub fn open(rows: u16, cols: u16) -> Result<(OwnedFd, OwnedFd), Error> {
    let master = open_master().map_err(at("posix_openpt"))?;
    // SAFETY: grantpt takes a descriptor and touches no memory.
    check(unsafe { libc::grantpt(master.as_raw_fd()) }).map_err(at("grantpt"))?;
    // SAFETY: unlockpt takes a descriptor and touches no memory.
    check(unsafe { libc::unlockpt(master.as_raw_fd()) }).map_err(at("unlockpt"))?;
    let name = slave_name(&master).map_err(at("ptsname"))?;
    // std opens close-on-exec, atomically, on every platform.
    let slave = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOCTTY)
        .open(&name)
        .map_err(|e| Errno::from_io_error(&e).unwrap_or(Errno::INVAL))
        .map_err(at("opening the PTY slave"))?;
    terminal::set_window_size(&master, rows.max(1), cols.max(1)).map_err(at("TIOCSWINSZ"))?;
    Ok((master, OwnedFd::from(slave)))
}

/// A new PTY master, close-on-exec: atomically where the system allows,
/// and on macOS, which has no flag for it, straight after.
fn open_master() -> crate::Result<OwnedFd> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    let flags = libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC;
    #[cfg(target_os = "macos")]
    let flags = libc::O_RDWR | libc::O_NOCTTY;
    // SAFETY: posix_openpt takes flags and touches no memory.
    let raw = check(unsafe { libc::posix_openpt(flags) })?;
    // SAFETY: `raw` was just opened, is valid, and nothing else owns it.
    let master = unsafe { OwnedFd::from_raw_fd(raw) };
    #[cfg(target_os = "macos")]
    crate::io::set_cloexec(&master)?;
    Ok(master)
}

/// The path of the slave of the PTY `master`.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn slave_name(master: impl AsFd) -> crate::Result<PathBuf> {
    let mut buffer: [libc::c_char; 128] = [0; 128];
    // SAFETY: the buffer is writable for its length, which is passed; the
    // call reports its errors as a number rather than in errno.
    let error = unsafe {
        libc::ptsname_r(
            master.as_fd().as_raw_fd(),
            buffer.as_mut_ptr(),
            buffer.len(),
        )
    };
    if error != 0 {
        return Err(
            Errno::from_io_error(&std::io::Error::from_raw_os_error(error)).unwrap_or(Errno::INVAL),
        );
    }
    Ok(path(&buffer))
}

/// The path of the slave of the PTY `master`. macOS's `ptsname` returns a
/// shared buffer; its ioctl fills one of ours.
#[cfg(target_os = "macos")]
fn slave_name(master: impl AsFd) -> crate::Result<PathBuf> {
    let mut buffer: [libc::c_char; 128] = [0; 128];
    let get = terminal::request(libc::TIOCPTYGNAME)?;
    // SAFETY: TIOCPTYGNAME writes at most 128 bytes, NUL included, and the
    // buffer is 128 bytes.
    check(unsafe { libc::ioctl(master.as_fd().as_raw_fd(), get, buffer.as_mut_ptr()) })?;
    Ok(path(&buffer))
}

/// A NUL-terminated C string as a path; the whole buffer if there is no NUL.
fn path(buffer: &[libc::c_char]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    let bytes: Vec<u8> = buffer
        .iter()
        .take_while(|c| **c != 0)
        .map(|c| u8::from_ne_bytes(c.to_ne_bytes()))
        .collect();
    PathBuf::from(std::ffi::OsStr::from_bytes(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_cloexec(fd: impl AsFd) -> bool {
        // SAFETY: F_GETFD on a valid descriptor touches no memory.
        let flags = unsafe { libc::fcntl(fd.as_fd().as_raw_fd(), libc::F_GETFD) };
        flags >= 0 && flags & libc::FD_CLOEXEC != 0
    }

    /// Bytes written to the master arrive at the slave and back, and both
    /// ends are close-on-exec.
    #[test]
    fn a_pty_carries_bytes_both_ways() -> std::result::Result<(), String> {
        let (master, slave) = open(10, 40).map_err(|e| e.to_string())?;
        assert!(is_cloexec(&master) && is_cloexec(&slave));
        let mut modes = terminal::attributes(&slave).map_err(|e| e.to_string())?;
        modes.make_raw();
        terminal::set_attributes(&slave, &modes).map_err(|e| e.to_string())?;
        assert_eq!(crate::io::write(&master, b"in"), Ok(2));
        let mut buffer = [0u8; 8];
        assert_eq!(crate::io::read(&slave, &mut buffer), Ok(2));
        assert_eq!(buffer.get(..2), Some(&b"in"[..]));
        assert_eq!(crate::io::write(&slave, b"out"), Ok(3));
        assert_eq!(crate::io::read(&master, &mut buffer), Ok(3));
        assert_eq!(buffer.get(..3), Some(&b"out"[..]));
        Ok(())
    }

    #[test]
    fn a_name_stops_at_its_nul() {
        let raw: Vec<libc::c_char> = b"/dev/pts/7\0junk"
            .iter()
            .map(|b| libc::c_char::from_ne_bytes([*b]))
            .collect();
        assert_eq!(path(&raw), PathBuf::from("/dev/pts/7"));
        let error = Error {
            call: "grantpt",
            errno: Errno::INVAL,
        };
        assert!(error.to_string().starts_with("grantpt: "));
    }
}
