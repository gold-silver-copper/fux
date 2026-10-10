//! Terminals: their modes, their size, their foreground group, and which
//! session they control.
use crate::errno::{Errno, Result, check};
use crate::process::Pid;
use std::num::NonZeroU16;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};

/// The type of an `ioctl` request, which differs between C libraries.
#[cfg(target_os = "macos")]
type Request = libc::c_ulong;
#[cfg(not(target_os = "macos"))]
type Request = libc::Ioctl;

/// A request constant as the C library's `ioctl` takes it.
pub(crate) fn request(value: impl TryInto<Request>) -> Result<Request> {
    value.try_into().map_err(|_| Errno::INVAL)
}

/// A terminal's modes.
#[derive(Clone)]
pub struct Termios(libc::termios);

impl Termios {
    /// Whether the terminal echoes what is typed (`ECHO`).
    pub fn echoes(&self) -> bool {
        self.0.c_lflag & libc::ECHO != 0
    }

    /// Whether the terminal edits input a line at a time (`ICANON`), as a
    /// program reading a line, or a password, has it; a line editor or a
    /// full-screen program turns it off and reads each key.
    pub fn line_mode(&self) -> bool {
        self.0.c_lflag & libc::ICANON != 0
    }

    /// Raw mode: no echo, no line editing, no signals from keys, and no
    /// output processing.
    pub fn make_raw(&mut self) {
        // SAFETY: the pointer is to a whole termios, which the call changes.
        unsafe { libc::cfmakeraw(&mut self.0) };
    }
}

/// The modes of the terminal `fd`.
pub fn attributes(fd: impl AsFd) -> Result<Termios> {
    let mut termios = std::mem::MaybeUninit::<libc::termios>::zeroed();
    // SAFETY: `termios` is a whole termios, which tcgetattr fills.
    check(unsafe { libc::tcgetattr(fd.as_fd().as_raw_fd(), termios.as_mut_ptr()) })?;
    // SAFETY: filled, as the call succeeded; zeroed before that besides.
    Ok(Termios(unsafe { termios.assume_init() }))
}

/// Sets the modes of the terminal `fd`, now.
pub fn set_attributes(fd: impl AsFd, termios: &Termios) -> Result<()> {
    // SAFETY: the pointer is to a whole termios, which the call reads.
    check(unsafe { libc::tcsetattr(fd.as_fd().as_raw_fd(), libc::TCSANOW, &termios.0) }).map(drop)
}

/// A terminal's size: never no rows or no columns, which a program would
/// divide by, or draw nothing in.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Size {
    pub rows: NonZeroU16,
    pub cols: NonZeroU16,
}

impl Size {
    /// `rows` by `cols`, if neither is zero.
    pub fn new(rows: u16, cols: u16) -> Option<Size> {
        Some(Size::from((NonZeroU16::new(rows)?, NonZeroU16::new(cols)?)))
    }
}

impl From<(NonZeroU16, NonZeroU16)> for Size {
    fn from((rows, cols): (NonZeroU16, NonZeroU16)) -> Size {
        Size { rows, cols }
    }
}

/// The size of the terminal `fd`: `None` if it is no terminal, or says it
/// has no rows or no columns, as one over a serial line may.
pub fn window_size(fd: impl AsFd) -> Option<Size> {
    let mut size = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let get = request(libc::TIOCGWINSZ).ok()?;
    // SAFETY: TIOCGWINSZ writes one winsize, and `size` is one.
    check(unsafe { libc::ioctl(fd.as_fd().as_raw_fd(), get, &mut size) }).ok()?;
    Size::new(size.ws_row, size.ws_col)
}

/// Sets the size of the terminal `fd`; the kernel tells its foreground
/// group with SIGWINCH.
pub fn set_window_size(fd: impl AsFd, size: Size) -> Result<()> {
    let size = libc::winsize {
        ws_row: size.rows.get(),
        ws_col: size.cols.get(),
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let set = request(libc::TIOCSWINSZ)?;
    // SAFETY: TIOCSWINSZ reads one winsize, and `size` is one.
    check(unsafe { libc::ioctl(fd.as_fd().as_raw_fd(), set, &size) }).map(drop)
}

/// The foreground process group of the terminal `fd`, if it has one.
pub fn foreground_group(fd: impl AsFd) -> Option<Pid> {
    // SAFETY: tcgetpgrp takes a descriptor and touches no memory.
    Pid::from_raw(unsafe { libc::tcgetpgrp(fd.as_fd().as_raw_fd()) })
}

/// A new open file for the terminal `fd` is open on, read-write,
/// nonblocking, close-on-exec, and never made this process's controlling
/// terminal: its flags are its own, so making it nonblocking changes
/// nothing for whoever else has the terminal open. `NOTTY` if `fd` is not a
/// terminal.
pub fn reopen(fd: impl AsFd) -> Result<OwnedFd> {
    let mut name = [0 as libc::c_char; 256];
    // SAFETY: the buffer is whole, of the length given, and ttyname_r
    // NUL-terminates what it writes there.
    let failed = unsafe { libc::ttyname_r(fd.as_fd().as_raw_fd(), name.as_mut_ptr(), name.len()) };
    if failed != 0 {
        return Err(Errno::from_raw(failed));
    }
    let flags = libc::O_RDWR | libc::O_NOCTTY | libc::O_NONBLOCK | libc::O_CLOEXEC;
    // SAFETY: `name` is a NUL-terminated path.
    let raw = check(unsafe { libc::open(name.as_ptr(), flags) })?;
    // SAFETY: `raw` was just opened, is valid, and nothing else owns it.
    Ok(unsafe { OwnedFd::from_raw_fd(raw) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pty;

    fn size(rows: u16, cols: u16) -> std::result::Result<Size, String> {
        Size::new(rows, cols).ok_or_else(|| "a size".to_owned())
    }

    /// A terminal reopened is the same terminal, through an open file of
    /// its own: nonblocking without making the original so. A socket is
    /// no terminal.
    #[test]
    fn a_terminal_reopens_nonblocking_on_its_own() -> std::result::Result<(), String> {
        let (master, slave) = pty::open(size(10, 20)?).map_err(|e| e.to_string())?;
        let again = reopen(&slave).map_err(|e| e.to_string())?;
        assert_eq!(window_size(&again), Some(size(10, 20)?));
        // Nothing to read: the new file says so at once, not blocking.
        let mut buffer = [0u8; 4];
        assert_eq!(crate::io::read(&again, &mut buffer), Err(Errno::AGAIN));
        crate::io::write(&master, b"hi\n").map_err(|e| e.to_string())?;
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(crate::io::read(&again, &mut buffer).is_ok_and(|n| n > 0));
        let (a, _b) = std::os::unix::net::UnixStream::pair().map_err(|e| e.to_string())?;
        assert!(reopen(&a).is_err());
        Ok(())
    }

    #[test]
    fn modes_and_size_round_trip_on_a_pty() -> std::result::Result<(), String> {
        let (master, slave) = pty::open(size(24, 80)?).map_err(|e| e.to_string())?;
        assert_eq!(window_size(&slave), Some(size(24, 80)?));
        set_window_size(&master, size(5, 7)?).map_err(|e| e.to_string())?;
        assert_eq!(window_size(&slave), Some(size(5, 7)?));
        let mut modes = attributes(&slave).map_err(|e| e.to_string())?;
        assert!(modes.echoes(), "a new terminal echoes");
        assert!(modes.line_mode(), "and edits a line at a time");
        modes.make_raw();
        set_attributes(&slave, &modes).map_err(|e| e.to_string())?;
        let again = attributes(&slave).map_err(|e| e.to_string())?;
        assert!(!again.echoes() && !again.line_mode());
        // The master reads the modes the program set on its side.
        let seen = attributes(&master).map_err(|e| e.to_string())?;
        assert!(!seen.echoes() && !seen.line_mode());
        // No session has it as its terminal: no foreground group.
        assert_eq!(foreground_group(&master), None);
        assert!(attributes(std::fs::File::open("/dev/null").map_err(|e| e.to_string())?).is_err());
        Ok(())
    }
}
