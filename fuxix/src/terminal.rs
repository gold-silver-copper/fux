//! Terminals: their modes, their size, their foreground group, and which
//! session they control.
use crate::errno::{Errno, Result, check};
use crate::process::Pid;
use std::os::fd::{AsFd, AsRawFd};

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

/// The size of the terminal `fd`: rows and columns.
pub fn window_size(fd: impl AsFd) -> Result<(u16, u16)> {
    let mut size = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let get = request(libc::TIOCGWINSZ)?;
    // SAFETY: TIOCGWINSZ writes one winsize, and `size` is one.
    check(unsafe { libc::ioctl(fd.as_fd().as_raw_fd(), get, &mut size) })?;
    Ok((size.ws_row, size.ws_col))
}

/// Sets the size of the terminal `fd`; the kernel tells its foreground
/// group with SIGWINCH.
pub fn set_window_size(fd: impl AsFd, rows: u16, cols: u16) -> Result<()> {
    let size = libc::winsize {
        ws_row: rows,
        ws_col: cols,
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

/// Makes the terminal `fd` the controlling terminal of this process's
/// session, which it must lead and which must have none.
pub fn make_controlling(fd: impl AsFd) -> Result<()> {
    let steal: libc::c_int = 0;
    let set = request(libc::TIOCSCTTY)?;
    // SAFETY: TIOCSCTTY takes an int and touches no memory.
    check(unsafe { libc::ioctl(fd.as_fd().as_raw_fd(), set, steal) }).map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pty;

    #[test]
    fn modes_and_size_round_trip_on_a_pty() -> std::result::Result<(), String> {
        let (master, slave) = pty::open(24, 80).map_err(|e| e.to_string())?;
        assert_eq!(window_size(&slave), Ok((24, 80)));
        set_window_size(&master, 5, 7).map_err(|e| e.to_string())?;
        assert_eq!(window_size(&slave), Ok((5, 7)));
        let mut modes = attributes(&slave).map_err(|e| e.to_string())?;
        assert!(modes.0.c_lflag & libc::ECHO != 0, "a new terminal echoes");
        modes.make_raw();
        set_attributes(&slave, &modes).map_err(|e| e.to_string())?;
        let again = attributes(&slave).map_err(|e| e.to_string())?;
        assert_eq!(again.0.c_lflag & (libc::ECHO | libc::ICANON), 0);
        // No session has it as its terminal: no foreground group.
        assert_eq!(foreground_group(&master), None);
        assert!(attributes(std::fs::File::open("/dev/null").map_err(|e| e.to_string())?).is_err());
        Ok(())
    }
}
