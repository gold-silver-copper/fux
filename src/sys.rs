//! The system calls std does not give. The only module with `unsafe`.
//! Every `extern` is declared by hand from the C prototypes; the constants
//! are Linux's and macOS's own and differ where the two systems differ.
#![allow(unsafe_code)]

use std::ffi::{CString, c_char, c_int, c_ulong, c_void};
use std::fs::File;
use std::io::{self, Error};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

#[cfg(target_os = "linux")]
mod os {
    use std::ffi::{c_int, c_ulong};
    pub const O_NONBLOCK: c_int = 0o4000;
    pub const TIOCSCTTY: c_ulong = 0x540E;
    pub const TIOCGWINSZ: c_ulong = 0x5413;
    pub const TIOCSWINSZ: c_ulong = 0x5414;
    pub type Nfds = c_ulong;
}
#[cfg(target_os = "macos")]
mod os {
    use std::ffi::{c_int, c_uint, c_ulong};
    pub const O_NONBLOCK: c_int = 0x0004;
    pub const TIOCSCTTY: c_ulong = 0x2000_7461;
    pub const TIOCGWINSZ: c_ulong = 0x4008_7468;
    pub const TIOCSWINSZ: c_ulong = 0x8008_7467;
    pub type Nfds = c_uint;
}
/// For `OpenOptions::custom_flags`: an open that never waits.
pub const O_NONBLOCK: i32 = os::O_NONBLOCK;
const F_SETFD: c_int = 2;
const F_GETFL: c_int = 3;
const F_SETFL: c_int = 4;
const FD_CLOEXEC: c_int = 1;
pub const POLLIN: i16 = 0x1;
pub const POLLOUT: i16 = 0x4;

/// `struct winsize`, the same four shorts on both systems.
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Winsize {
    pub rows: u16,
    pub cols: u16,
    pub xpixel: u16,
    pub ypixel: u16,
}

/// `struct pollfd`, the same on both systems.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PollFd {
    fd: c_int,
    events: i16,
    pub revents: i16,
}

/// `struct termios` is 60 bytes on Linux and 72 on macOS. Nothing here looks
/// inside it (`cfmakeraw` does), so it is a block big enough for both.
#[repr(C, align(8))]
#[derive(Clone, Copy)]
pub struct Termios([u8; 128]);

#[cfg_attr(target_os = "linux", link(name = "util"))]
unsafe extern "C" {
    fn openpty(
        master: *mut c_int,
        slave: *mut c_int,
        name: *mut c_char,
        termios: *const c_void,
        winsize: *const Winsize,
    ) -> c_int;
    fn setsid() -> c_int;
    fn signal(signal: c_int, handler: usize) -> usize;
    fn ioctl(fd: c_int, request: c_ulong, ...) -> c_int;
    fn fcntl(fd: c_int, cmd: c_int, ...) -> c_int;
    fn poll(fds: *mut PollFd, nfds: os::Nfds, timeout: c_int) -> c_int;
    fn mkfifo(path: *const c_char, mode: c_uint_mode) -> c_int;
    fn tcgetattr(fd: c_int, termios: *mut Termios) -> c_int;
    fn tcsetattr(fd: c_int, actions: c_int, termios: *const Termios) -> c_int;
    fn cfmakeraw(termios: *mut Termios);
}
/// `mode_t`: 32 bits on Linux, 16 on macOS.
#[cfg(target_os = "linux")]
#[allow(non_camel_case_types)]
type c_uint_mode = u32;
#[cfg(target_os = "macos")]
#[allow(non_camel_case_types)]
type c_uint_mode = u16;

fn check(result: c_int) -> io::Result<c_int> {
    if result < 0 { Err(Error::last_os_error()) } else { Ok(result) }
}

/// A new pty of the given size: (master, slave). The master does not wait on
/// reads or writes and neither end is inherited by a program started later
/// (the slave reaches the pane's program as its standard streams only).
pub fn pty(size: Winsize) -> io::Result<(File, File)> {
    let (mut master, mut slave) = (-1, -1);
    // SAFETY: both out-pointers are valid for the call; name and termios may be null.
    check(unsafe { openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null(), &size) })?;
    // SAFETY: openpty succeeded, so both are open descriptors nobody else owns.
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    for fd in [&master, &slave] {
        // SAFETY: fcntl on a descriptor we own, with an integer argument.
        check(unsafe { fcntl(fd.as_raw_fd(), F_SETFD, FD_CLOEXEC) })?;
    }
    // SAFETY: as above.
    let flags = check(unsafe { fcntl(master.as_raw_fd(), F_GETFL) })?;
    // SAFETY: as above.
    check(unsafe { fcntl(master.as_raw_fd(), F_SETFL, flags | os::O_NONBLOCK) })?;
    Ok((File::from(master), File::from(slave)))
}

/// Makes the command lead a new session when it starts. With `tty`, its
/// standard input (already the pty's slave) becomes the controlling terminal.
pub fn new_session(command: &mut Command, tty: bool) {
    // SAFETY: the closure runs between fork and exec and calls only setsid,
    // ioctl and signal, all async-signal-safe; it allocates nothing.
    unsafe {
        command.pre_exec(move || {
            check(setsid())?;
            if tty {
                check(ioctl(0, os::TIOCSCTTY, 0 as c_int))?;
                // Every signal back to its default (0): the program starts as on
                // any new terminal, whatever whoever started the pane ignored.
                // A hangup, Ctrl-C and Ctrl-Z must be able to reach it.
                for number in 1..32 {
                    signal(number, 0);
                }
            }
            Ok(())
        });
    }
}

pub fn set_winsize(tty: &File, size: Winsize) -> io::Result<()> {
    // SAFETY: TIOCSWINSZ reads one winsize through a valid pointer.
    check(unsafe { ioctl(tty.as_raw_fd(), os::TIOCSWINSZ, &size as *const Winsize) }).map(drop)
}

pub fn winsize(tty: &File) -> io::Result<Winsize> {
    let mut size = Winsize::default();
    // SAFETY: TIOCGWINSZ writes one winsize through a valid pointer.
    check(unsafe { ioctl(tty.as_raw_fd(), os::TIOCGWINSZ, &mut size as *mut Winsize) })?;
    Ok(size)
}

/// Waits until one of the files is ready or the time (milliseconds, -1 for
/// ever) is over. Each entry is (descriptor, events wanted); the answer has
/// the events that happened, in the same order. An interrupted wait is an
/// answer with no events.
pub fn wait(wanted: &[(RawFd, i16)], timeout: i32) -> io::Result<Vec<i16>> {
    let mut fds: Vec<PollFd> = wanted.iter().map(|&(fd, events)| PollFd { fd, events, revents: 0 }).collect();
    // SAFETY: the pointer and length describe `fds`, alive for the call.
    let result = unsafe { poll(fds.as_mut_ptr(), fds.len() as os::Nfds, timeout) };
    if result < 0 {
        let error = Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
        return Ok(vec![0; wanted.len()]);
    }
    Ok(fds.iter().map(|fd| fd.revents).collect())
}

pub fn make_fifo(path: &Path) -> io::Result<()> {
    let path = CString::new(path.as_os_str().as_bytes())?;
    // SAFETY: the path is a valid NUL-terminated string for the call.
    check(unsafe { mkfifo(path.as_ptr(), 0o600) }).map(drop)
}

/// Puts the terminal in raw mode and gives back what to restore.
pub fn raw_mode(tty: &File) -> io::Result<Termios> {
    let mut saved = Termios([0; 128]);
    // SAFETY: tcgetattr writes one termios, which fits in the block.
    check(unsafe { tcgetattr(tty.as_raw_fd(), &mut saved) })?;
    let mut raw = saved;
    // SAFETY: cfmakeraw edits the termios tcgetattr just filled.
    unsafe { cfmakeraw(&mut raw) };
    restore_mode(tty, &raw)?;
    Ok(saved)
}

pub fn restore_mode(tty: &File, termios: &Termios) -> io::Result<()> {
    // SAFETY: tcsetattr reads one termios that tcgetattr filled. 0 is TCSANOW.
    check(unsafe { tcsetattr(tty.as_raw_fd(), 0, termios) }).map(drop)
}
