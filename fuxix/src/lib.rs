//! The system calls std does not give. The only crate with `unsafe`.
//! Every `extern` is declared by hand after the C prototypes; the constants
//! are those of Linux (x86-64 and arm64) and of macOS, and differ where the two differ.

use std::ffi::{CStr, CString, c_char, c_int, c_ulong, c_void};
use std::fs::{File, OpenOptions};
use std::io::{self, Error, Write};
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

#[cfg(target_os = "linux")]
mod os {
    use std::ffi::{c_int, c_ulong};
    pub const O_NONBLOCK: c_int = 0o4000;
    pub const SIGCHLD: c_int = 17;
    pub const TIOCSCTTY: c_ulong = 0x540E;
    pub const TIOCGWINSZ: c_ulong = 0x5413;
    pub const TIOCSWINSZ: c_ulong = 0x5414;
    pub type Nfds = c_ulong;
    pub type Mode = u32;
}
#[cfg(target_os = "macos")]
mod os {
    use std::ffi::{c_int, c_uint, c_ulong};
    pub const O_NONBLOCK: c_int = 0x0004;
    pub const SIGCHLD: c_int = 20;
    pub const TIOCSCTTY: c_ulong = 0x2000_7461;
    pub const TIOCGWINSZ: c_ulong = 0x4008_7468;
    pub const TIOCSWINSZ: c_ulong = 0x8008_7467;
    pub type Nfds = c_uint;
    pub type Mode = u16;
}
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
struct PollFd {
    fd: c_int,
    events: i16,
    revents: i16,
}

/// `struct termios` is 60 bytes on Linux and 72 on macOS. Nothing here looks
/// inside it (`cfmakeraw` does), so it is a block big enough for both.
#[repr(C, align(8))]
#[derive(Clone, Copy)]
struct Termios([u8; 128]);

#[cfg_attr(target_os = "linux", link(name = "util"))]
unsafe extern "C" {
    fn openpty(
        master: *mut c_int,
        slave: *mut c_int,
        name: *mut c_char,
        termios: *mut Termios,
        winsize: *mut Winsize,
    ) -> c_int;
    fn setsid() -> c_int;
    fn signal(signal: c_int, handler: usize) -> usize;
    fn ioctl(fd: c_int, request: c_ulong, ...) -> c_int;
    fn fcntl(fd: c_int, cmd: c_int, ...) -> c_int;
    fn poll(fds: *mut PollFd, nfds: os::Nfds, timeout: c_int) -> c_int;
    fn mkfifo(path: *const c_char, mode: os::Mode) -> c_int;
    fn tcgetattr(fd: c_int, termios: *mut Termios) -> c_int;
    fn tcsetattr(fd: c_int, actions: c_int, termios: *const Termios) -> c_int;
    fn cfmakeraw(termios: *mut Termios);
    fn gethostname(name: *mut c_char, len: usize) -> c_int;
}

fn check(result: c_int) -> io::Result<c_int> {
    if result < 0 { Err(Error::last_os_error()) } else { Ok(result) }
}

/// A new pty of the given size: (master, slave). The master does not wait on
/// reads or writes and neither end is inherited by a program started later
/// (the slave reaches the pane's program as its standard streams only).
pub fn pty(mut size: Winsize) -> io::Result<(File, File)> {
    let (mut master, mut slave) = (-1, -1);
    // SAFETY: the out-pointers and the size are valid for the call; name and termios may be null.
    check(unsafe { openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null_mut(), &mut size) })?;
    // SAFETY: openpty succeeded, so both are open descriptors nobody else owns.
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    for fd in [&master, &slave] {
        // SAFETY: fcntl on a descriptor we own, with an integer argument.
        check(unsafe { fcntl(fd.as_raw_fd(), F_SETFD, FD_CLOEXEC) })?;
    }
    // SAFETY: fcntl on a descriptor we own; F_GETFL takes no third argument.
    let flags = check(unsafe { fcntl(master.as_raw_fd(), F_GETFL) })?;
    // SAFETY: fcntl on a descriptor we own, with an integer argument.
    check(unsafe { fcntl(master.as_raw_fd(), F_SETFL, flags | os::O_NONBLOCK) })?;
    Ok((File::from(master), File::from(slave)))
}

/// Makes the command lead a new session when it starts. With `tty`, its
/// standard input (already the pty's slave) becomes the controlling terminal.
pub fn new_session(command: &mut Command, tty: bool) {
    // SAFETY: the closure runs between fork and exec, where only async-signal-safe functions may
    // be called and nothing allocated. setsid and signal are such by POSIX. ioctl is not in
    // general: relied on is that on Linux (glibc) and macOS it is the system call and no more,
    // and that TIOCSCTTY follows its argument nowhere (C reads it as a pointer: a null one, for
    // the number 0). An error is errno, read into a number: nothing is allocated.
    unsafe {
        command.pre_exec(move || {
            check(setsid())?;
            if tty {
                check(ioctl(0, os::TIOCSCTTY, std::ptr::null_mut::<c_void>()))?;
            }
            // The classic signals (1 to 31) back to their defaults (0): what is
            // started runs as on any new terminal, whatever whoever started it
            // ignored. A hangup, Ctrl-C and Ctrl-Z must be able to reach a
            // program, and a holder must be told of its program's end.
            for number in 1..32 {
                signal(number, 0);
            }
            Ok(())
        });
    }
}

/// Makes sure this process is told of its children's ends, even when
/// whoever started it had that signal ignored (waiting for a child then fails).
pub fn own_children() {
    // SAFETY: signal with the default handler (0) for SIGCHLD, before any thread starts.
    unsafe { signal(os::SIGCHLD, 0) };
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
/// ever) is over. Each entry is (file, events wanted), a file that is not there
/// being no file to wait on; the answer has the events that happened, in the
/// same order. An interrupted wait is an answer with no events.
pub fn wait<const N: usize>(wanted: [(Option<BorrowedFd<'_>>, i16); N], timeout: i32) -> io::Result<[i16; N]> {
    let mut fds = wanted.map(|(fd, events)| PollFd { fd: fd.map_or(-1, |fd| fd.as_raw_fd()), events, revents: 0 });
    // (Linux refuses a count over the limit of open files, also for entries that are no file:
    // those at the end are not passed.)
    let count = fds.iter().rposition(|fd| fd.fd >= 0).map_or(0, |last| last + 1);
    // SAFETY: the pointer and count describe the start of `fds`, whose descriptors are borrowed for the call.
    match check(unsafe { poll(fds.as_mut_ptr(), count as os::Nfds, timeout) }) {
        Err(error) if error.kind() != io::ErrorKind::Interrupted => Err(error),
        Err(_) => Ok([0; N]),
        Ok(_) => Ok(fds.map(|fd| fd.revents)),
    }
}

/// This machine's name, as programs on it know it.
pub fn host() -> Option<String> {
    // (A name is 255 bytes at most, 256 with its end on macOS: one more stays zero.)
    let mut name = [0u8; 257];
    // SAFETY: gethostname writes at most the length given into a buffer that is longer.
    check(unsafe { gethostname(name.as_mut_ptr().cast(), name.len() - 1) }).ok()?;
    CStr::from_bytes_until_nul(&name).ok()?.to_str().ok().map(String::from)
}

/// Opens a file to read, or to write, without waiting: a FIFO with nobody at its other end
/// holds no caller (to read it is to find it empty; to write it is an error at once).
pub fn open(path: impl AsRef<Path>, write: bool) -> io::Result<File> {
    OpenOptions::new().read(!write).write(write).custom_flags(os::O_NONBLOCK).open(path)
}

/// Whether an open to write failed because the FIFO has no reader (ENXIO, 6 on both systems).
pub fn no_reader(error: &Error) -> bool {
    error.raw_os_error() == Some(6)
}

/// A FIFO of the caller's own, made anew (what was there of that name goes) and opened once, to
/// read and to write without waiting: a writer leaving is then never the end of the file.
pub fn fifo(path: impl AsRef<Path>) -> io::Result<File> {
    let _ = std::fs::remove_file(&path);
    let name = CString::new(path.as_ref().as_os_str().as_bytes())?;
    // SAFETY: the name is a valid NUL-terminated string for the call.
    check(unsafe { mkfifo(name.as_ptr(), 0o600) })?;
    OpenOptions::new().read(true).write(true).custom_flags(os::O_NONBLOCK).open(path)
}

/// A terminal in raw mode. Dropped, it writes the terminal what it was given to say on leaving
/// and then puts its mode back as it was.
pub struct Raw(File, Termios, &'static [u8]);

impl Drop for Raw {
    fn drop(&mut self) {
        let _ = self.0.write_all(self.2);
        let _ = set_mode(&self.0, &self.1);
    }
}

/// Puts the terminal in raw mode until what is given back is dropped.
pub fn raw_mode(tty: File, leaving: &'static [u8]) -> io::Result<Raw> {
    let mut saved = Termios([0; 128]);
    // SAFETY: tcgetattr writes one termios, which fits in the block.
    check(unsafe { tcgetattr(tty.as_raw_fd(), &mut saved) })?;
    let mut raw = saved;
    // SAFETY: cfmakeraw edits the termios tcgetattr just filled.
    unsafe { cfmakeraw(&mut raw) };
    set_mode(&tty, &raw)?;
    Ok(Raw(tty, saved, leaving))
}

fn set_mode(tty: &File, termios: &Termios) -> io::Result<()> {
    // SAFETY: tcsetattr reads one termios that tcgetattr filled. 0 is TCSANOW.
    check(unsafe { tcsetattr(tty.as_raw_fd(), 0, termios) }).map(drop)
}
