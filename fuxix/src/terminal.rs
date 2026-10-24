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

/// A request constant as the C library's `ioctl` takes it: on macOS, some
/// are typed narrower.
pub(crate) fn request(value: impl Into<Request>) -> Request {
    value.into()
}

/// A terminal's modes.
#[derive(Clone)]
pub struct Termios(libc::termios);

/// The modes only one kernel has, as nothing on the other.
#[cfg(not(target_os = "macos"))]
mod only {
    use libc::tcflag_t;
    pub const IUCLC: tcflag_t = libc::IUCLC;
    pub const OLCUC: tcflag_t = libc::OLCUC;
    pub const ALTWERASE: tcflag_t = 0;
    pub const ONOEOT: tcflag_t = 0;
    /// The bits that say how tabs are written, and their value for spaces.
    pub const TABS: (tcflag_t, tcflag_t) = (libc::TABDLY, libc::XTABS);
    pub const VSTATUS: Option<usize> = None;
}

#[cfg(target_os = "macos")]
mod only {
    use libc::tcflag_t;
    pub const IUCLC: tcflag_t = 0;
    pub const OLCUC: tcflag_t = 0;
    pub const ALTWERASE: tcflag_t = libc::ALTWERASE;
    pub const ONOEOT: tcflag_t = libc::ONOEOT;
    pub const TABS: (tcflag_t, tcflag_t) = (libc::OXTABS, libc::OXTABS);
    pub const VSTATUS: Option<usize> = Some(libc::VSTATUS);
}

/// Declares `Modes`, the modes a terminal's echo of typed input depends
/// on, as plain data, with how each is read from a termios and set in one:
/// each flag, its field and bit; tabs written as spaces; each character, at
/// its place in `c_cc` (`None` when disabled).
macro_rules! modes {
    (
        flags { $($flag:ident: $field:ident $bit:expr),* $(,)? }
        chars { $($char:ident: $at:expr),* $(,)? }
    ) => {
        /// The modes a terminal's echo of typed input depends on, as plain
        /// data: local, input and output modes by their termios names,
        /// `expand_tabs` for tabs written as spaces (`XTABS` on Linux,
        /// `OXTABS` on macOS), and the editing, signal and flow characters,
        /// each `None` when disabled. A mode one kernel lacks is always off
        /// on the other: `iuclc` and `olcuc` are Linux's, `altwerase`,
        /// `onoeot` and `status` macOS's.
        #[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
        pub struct Modes {
            $(pub $flag: bool,)*
            pub expand_tabs: bool,
            $(pub $char: Option<u8>,)*
        }

        impl Termios {
            /// The modes its echo depends on.
            pub fn modes(&self) -> Modes {
                let t = &self.0;
                let at = |at: Option<usize>| {
                    at.and_then(|at| t.c_cc.get(at).copied())
                        .filter(|&c| c != libc::_POSIX_VDISABLE)
                };
                Modes {
                    $($flag: t.$field & $bit != 0,)*
                    expand_tabs: t.c_oflag & only::TABS.0 == only::TABS.1,
                    $($char: at($at),)*
                }
            }

            /// Sets the modes `Modes` covers to `modes`, leaving the rest as
            /// they are; one this kernel lacks stays off.
            pub fn set_modes(&mut self, modes: &Modes) {
                let t = &mut self.0;
                $(
                    t.$field = if modes.$flag { t.$field | $bit } else { t.$field & !$bit };
                )*
                t.c_oflag &= !only::TABS.0;
                if modes.expand_tabs {
                    t.c_oflag |= only::TABS.1;
                }
                $(
                    if let Some(slot) = ($at).and_then(|at: usize| t.c_cc.get_mut(at)) {
                        *slot = modes.$char.unwrap_or(libc::_POSIX_VDISABLE);
                    }
                )*
            }
        }
    };
}

modes! {
    flags {
        // Input edited a line at a time, as a program reading a line, or a
        // password, has it; a line editor or a full-screen program turns it
        // off and reads each key.
        canonical: c_lflag libc::ICANON,
        echo: c_lflag libc::ECHO,
        echoe: c_lflag libc::ECHOE,
        echok: c_lflag libc::ECHOK,
        echoke: c_lflag libc::ECHOKE,
        echoctl: c_lflag libc::ECHOCTL,
        echonl: c_lflag libc::ECHONL,
        echoprt: c_lflag libc::ECHOPRT,
        iexten: c_lflag libc::IEXTEN,
        isig: c_lflag libc::ISIG,
        noflsh: c_lflag libc::NOFLSH,
        extproc: c_lflag libc::EXTPROC,
        altwerase: c_lflag only::ALTWERASE,
        icrnl: c_iflag libc::ICRNL,
        inlcr: c_iflag libc::INLCR,
        igncr: c_iflag libc::IGNCR,
        istrip: c_iflag libc::ISTRIP,
        iutf8: c_iflag libc::IUTF8,
        ixon: c_iflag libc::IXON,
        parmrk: c_iflag libc::PARMRK,
        iuclc: c_iflag only::IUCLC,
        opost: c_oflag libc::OPOST,
        onlcr: c_oflag libc::ONLCR,
        ocrnl: c_oflag libc::OCRNL,
        onocr: c_oflag libc::ONOCR,
        olcuc: c_oflag only::OLCUC,
        onoeot: c_oflag only::ONOEOT,
    }
    chars {
        erase: Some(libc::VERASE),
        kill: Some(libc::VKILL),
        werase: Some(libc::VWERASE),
        intr: Some(libc::VINTR),
        quit: Some(libc::VQUIT),
        susp: Some(libc::VSUSP),
        eof: Some(libc::VEOF),
        eol: Some(libc::VEOL),
        eol2: Some(libc::VEOL2),
        lnext: Some(libc::VLNEXT),
        reprint: Some(libc::VREPRINT),
        start: Some(libc::VSTART),
        stop: Some(libc::VSTOP),
        discard: Some(libc::VDISCARD),
        status: only::VSTATUS,
    }
}

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
    let get = request(libc::TIOCGWINSZ);
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
    let set = request(libc::TIOCSWINSZ);
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
/// terminal, or is a PTY's master, whose name opens a new PTY.
pub fn reopen(fd: impl AsFd) -> Result<OwnedFd> {
    // Only a master has a slave's name to give.
    if crate::pty::slave_name(&fd).is_ok() {
        return Err(Errno::NOTTY);
    }
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
    /// its own: nonblocking without making the original so. A master, and
    /// a socket, are refused.
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
        assert_eq!(reopen(&master).err(), Some(Errno::NOTTY));
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
        assert!(modes.modes().echo, "a new terminal echoes");
        assert!(modes.modes().canonical, "and edits a line at a time");
        modes.make_raw();
        set_attributes(&slave, &modes).map_err(|e| e.to_string())?;
        let again = attributes(&slave).map_err(|e| e.to_string())?;
        assert!(!again.modes().echo && !again.modes().canonical);
        // The master reads the modes the program set on its side.
        let seen = attributes(&master).map_err(|e| e.to_string())?;
        assert!(!seen.modes().echo && !seen.modes().canonical);
        // The modes echo depends on, set and read back as they were.
        let mut echo = seen.modes();
        (echo.canonical, echo.echoctl, echo.erase, echo.eol) = (true, true, Some(0x08), None);
        let mut termios = seen.clone();
        termios.set_modes(&echo);
        set_attributes(&slave, &termios).map_err(|e| e.to_string())?;
        assert_eq!(attributes(&slave).map_err(|e| e.to_string())?.modes(), echo);
        // No session has it as its terminal: no foreground group.
        assert_eq!(foreground_group(&master), None);
        assert!(attributes(std::fs::File::open("/dev/null").map_err(|e| e.to_string())?).is_err());
        Ok(())
    }
}
