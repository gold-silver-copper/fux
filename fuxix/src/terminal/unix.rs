//! Terminals on Unix: their modes, their size, their foreground group, and
//! which session they control.
use super::{Pixels, Size};
use crate::errno::{Errno, Result, check};
use crate::process::Pid;
use std::num::NonZeroU16;
use std::os::fd::{AsFd, AsRawFd};

/// The type of an `ioctl` request, which differs between C libraries; on
/// macOS some request constants are typed narrower, and `Request::from`
/// widens them.
#[cfg(target_os = "macos")]
pub(crate) type Request = libc::c_ulong;
#[cfg(not(target_os = "macos"))]
pub(crate) type Request = libc::Ioctl;

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

    /// Whether input is edited and read a line at a time (`ICANON`): then
    /// the end-of-file key, typed at a line's start, ends a read.
    pub fn edits_lines(&self) -> bool {
        self.0.c_lflag & libc::ICANON != 0
    }

    /// Whether what is typed is echoed (`ECHO`).
    pub fn echoes(&self) -> bool {
        self.0.c_lflag & libc::ECHO != 0
    }

    /// The end-of-file key (`VEOF`, usually ^D), unless it is disabled.
    pub fn end_of_file(&self) -> Option<u8> {
        let key = *self.0.c_cc.get(libc::VEOF)?;
        (key != libc::_POSIX_VDISABLE).then_some(key)
    }

    /// Whether a newline written goes out as a carriage return and a
    /// newline (`OPOST` and `ONLCR`).
    pub fn newline_as_crlf(&self) -> bool {
        self.0.c_oflag & libc::OPOST != 0 && self.0.c_oflag & libc::ONLCR != 0
    }

    /// Makes a newline written go out as a carriage return and a newline,
    /// or as it is, leaving the rest of output processing alone (on, it
    /// turns `OPOST` on too).
    pub fn set_newline_as_crlf(&mut self, on: bool) {
        match on {
            true => self.0.c_oflag |= libc::OPOST | libc::ONLCR,
            false => self.0.c_oflag &= !libc::ONLCR,
        }
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
    set(fd.as_fd().as_raw_fd(), libc::TCSANOW, termios)
}

/// `tcsetattr` on the descriptor `raw`, open for the call, `when` saying
/// whether first to wait for the output written to be sent.
fn set(raw: libc::c_int, when: libc::c_int, termios: &Termios) -> Result<()> {
    // SAFETY: the pointer is to a whole termios, which the call reads; the
    // descriptor is a number, which the call checks.
    check(unsafe { libc::tcsetattr(raw, when, &termios.0) }).map(drop)
}

/// The terminal `fd` in raw mode ([`Termios::make_raw`]) until dropped,
/// then in the modes it had, however that comes: a return, an error, a
/// panic. [`Raw::enter`] changes them at once (`TCSANOW`), so leaving never
/// waits on a terminal that stopped reading; [`Raw::enter_draining`] first
/// waits for what was written to be sent (`TCSADRAIN`), so it is not sent
/// in the other mode. Putting the modes back is retried while a signal
/// interrupts it, as nothing could retry it for a drop.
///
/// A panic puts back every terminal a live `Raw` holds, the latest first,
/// before its message is printed, in the panic hook the first `enter`
/// installs, which then calls the hook that was there: a program that
/// writes something to its terminal as it leaves (the alternate screen's
/// end) sets its own hook after entering, to run first. An abort (an
/// allocation refused, a stack overflow) runs no hook: a handler of SIGABRT
/// the first `enter` installs does the same, after what [`leaving_on_abort`]
/// says to write.
pub struct Raw<F: AsFd> {
    fd: F,
    saved: Termios,
    when: libc::c_int,
    /// Its registration in [`HELD`].
    id: u64,
}

/// What the panic hook puts back: each live `Raw`'s id, descriptor, modes
/// and how to set them, in the order entered. A `Raw` takes its own out,
/// under the lock, before it lets its descriptor go.
static HELD: std::sync::Mutex<Vec<(u64, libc::c_int, Termios, libc::c_int)>> =
    std::sync::Mutex::new(Vec::new());

/// [`HELD`], locked. A poisoned lock still holds a whole list: nothing
/// that runs under it leaves one half changed.
fn held() -> std::sync::MutexGuard<'static, Vec<(u64, libc::c_int, Termios, libc::c_int)>> {
    HELD.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl<F: AsFd> Raw<F> {
    /// Raw mode now, and the modes back now once dropped.
    pub fn enter(fd: F) -> Result<Raw<F>> {
        Raw::enter_with(fd, libc::TCSANOW)
    }

    /// Raw mode, and the modes back once dropped, each once what was
    /// written has been sent.
    pub fn enter_draining(fd: F) -> Result<Raw<F>> {
        Raw::enter_with(fd, libc::TCSADRAIN)
    }

    fn enter_with(fd: F, when: libc::c_int) -> Result<Raw<F>> {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let saved = attributes(&fd)?;
        let mut raw = saved.clone();
        raw.make_raw();
        static HOOKED: std::sync::Once = std::sync::Once::new();
        HOOKED.call_once(|| {
            // SAFETY: the handler has the signature `signal` wants, and does
            // only what a signal handler may.
            unsafe {
                libc::signal(
                    libc::SIGABRT,
                    restore_on_abort as extern "C" fn(libc::c_int) as libc::sighandler_t,
                )
            };
            let previous = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                // Waited for: only a push, a retain and this loop run
                // under the lock, none of which panics, so the panicking
                // thread never holds it, and another holds it only for an
                // instant.
                for (_, raw, termios, when) in held().iter().rev() {
                    let _ = set(*raw, *when, termios);
                }
                previous(info);
            }));
        });
        let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        held().push((id, fd.as_fd().as_raw_fd(), saved.clone(), when));
        // Made first, so that failing to set raw mode puts the modes back.
        let entered = Raw {
            fd,
            saved,
            when,
            id,
        };
        set(entered.fd.as_fd().as_raw_fd(), when, &raw)?;
        Ok(entered)
    }
}

impl<F: AsFd> Drop for Raw<F> {
    fn drop(&mut self) {
        held().retain(|(id, ..)| *id != self.id);
        let raw = self.fd.as_fd().as_raw_fd();
        while set(raw, self.when, &self.saved) == Err(Errno::INTR) {}
    }
}

/// The size of the terminal `fd`, and its size in pixels if it says one:
/// `None` if it is no terminal, or says it has no rows or no columns, as
/// one over a serial line may.
pub fn window_size(fd: impl AsFd) -> Option<(Size, Option<Pixels>)> {
    let mut size = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let get = Request::from(libc::TIOCGWINSZ);
    // SAFETY: TIOCGWINSZ writes one winsize, and `size` is one.
    check(unsafe { libc::ioctl(fd.as_fd().as_raw_fd(), get, &mut size) }).ok()?;
    let pixels = (NonZeroU16::new(size.ws_xpixel).zip(NonZeroU16::new(size.ws_ypixel)))
        .map(|(width, height)| Pixels { width, height });
    Some((Size::new(size.ws_row, size.ws_col)?, pixels))
}

/// The size of the terminal on stdin.
pub fn stdin_size() -> Option<(Size, Option<Pixels>)> {
    window_size(std::io::stdin())
}

/// Whether stdin and stdout are one terminal: the same device, or both the
/// session's controlling terminal (`fux </dev/tty`, whose device is
/// `/dev/tty`'s).
pub fn on_one_terminal() -> bool {
    use std::io::IsTerminal as _;
    use std::os::unix::fs::MetadataExt as _;
    let device = |fd: std::os::fd::BorrowedFd<'_>| {
        let file = std::fs::File::from(fd.try_clone_to_owned().ok()?);
        file.metadata().ok().map(|m| m.rdev())
    };
    let (stdin, stdout) = (std::io::stdin(), std::io::stdout());
    stdin.is_terminal()
        && stdout.is_terminal()
        && (device(stdin.as_fd()).is_some_and(|d| device(stdout.as_fd()) == Some(d))
            || controls_session(stdin) && controls_session(stdout))
}

/// Unix has nothing to open: a process's descriptors are what it was
/// given.
pub fn open_stdio() {}

/// What to write to stdout as an abort ends this process in raw mode.
static LEAVE_ON_ABORT: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();

/// `text`, what a program writes to its terminal as it leaves, is written to
/// stdout if this process aborts while a `Raw` is live.
pub fn leaving_on_abort(text: &'static str) {
    let _ = LEAVE_ON_ABORT.set(text);
}

/// Puts the terminals back as an abort ends this process, then lets the
/// signal end it, wherever it came from. `try_lock`: the thread aborting may
/// be the one holding the lock, and then nothing is put back.
extern "C" fn restore_on_abort(signal: libc::c_int) {
    if let Ok(held) = HELD.try_lock()
        && !held.is_empty()
    {
        if let Some(text) = LEAVE_ON_ABORT.get() {
            // SAFETY: `write` may be called in a handler, and `text` is a
            // whole static string.
            let _ = unsafe { libc::write(libc::STDOUT_FILENO, text.as_ptr().cast(), text.len()) };
        }
        for (_, raw, termios, when) in held.iter().rev() {
            let _ = set(*raw, *when, termios);
        }
    }
    // SAFETY: `signal` may be called in a handler; the default ends the process.
    unsafe { libc::signal(signal, libc::SIG_DFL) };
    // SAFETY: `raise` may be called in a handler; it is delivered as this returns.
    unsafe { libc::raise(signal) };
}

/// Runs `leave`, what a program writes to its terminal as a panic ends it,
/// before the panic hook puts the terminal back.
pub fn leaving(leave: fn()) {
    leave();
}

/// Sets the size of the terminal `fd`, in cells and, if known, in
/// `pixels` (else 0 by 0, as the kernel says unknown); the kernel tells
/// its foreground group with SIGWINCH when either changes.
pub fn set_window_size(fd: impl AsFd, size: Size, pixels: Option<Pixels>) -> Result<()> {
    let (x, y) = pixels.map_or((0, 0), |p| (p.width.get(), p.height.get()));
    let size = libc::winsize {
        ws_row: size.rows.get(),
        ws_col: size.cols.get(),
        ws_xpixel: x,
        ws_ypixel: y,
    };
    let set = Request::from(libc::TIOCSWINSZ);
    // SAFETY: TIOCSWINSZ reads one winsize, and `size` is one.
    check(unsafe { libc::ioctl(fd.as_fd().as_raw_fd(), set, &size) }).map(drop)
}

/// The foreground process group of the terminal `fd`: none if it has
/// none (Linux answers 0) or no session has it as its terminal (`NOTTY`),
/// else why it can't be told.
pub fn foreground_group(fd: impl AsFd) -> Result<Option<Pid>> {
    // SAFETY: tcgetpgrp takes a descriptor and touches no memory.
    match check(unsafe { libc::tcgetpgrp(fd.as_fd().as_raw_fd()) }) {
        Ok(group) => Ok(Pid::from_raw(group)),
        Err(Errno::NOTTY) => Ok(None),
        Err(errno) => Err(errno),
    }
}

/// Whether the terminal `fd` is open on is this process's session's
/// controlling terminal. A session has one: two descriptors that both are
/// reach one terminal, whatever name each was opened by (`/dev/tty`, whose
/// device is its own, opens the controlling terminal).
pub fn controls_session(fd: impl AsFd) -> bool {
    // SAFETY: tcgetsid takes a descriptor and touches no memory.
    let its = unsafe { libc::tcgetsid(fd.as_fd().as_raw_fd()) };
    // SAFETY: getsid takes a number and touches no memory.
    its > 0 && its == unsafe { libc::getsid(0) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pty;

    fn size(rows: u16, cols: u16) -> std::result::Result<Size, String> {
        Size::new(rows, cols).ok_or_else(|| "a size".to_owned())
    }

    #[test]
    fn modes_and_size_round_trip_on_a_pty() -> std::result::Result<(), String> {
        let (master, slave) = pty::open(size(24, 80)?).map_err(|e| e.to_string())?;
        assert_eq!(window_size(&slave), Some((size(24, 80)?, None)));
        set_window_size(&master, size(5, 7)?, None).map_err(|e| e.to_string())?;
        assert_eq!(window_size(&slave), Some((size(5, 7)?, None)));
        let pixels = (NonZeroU16::new(70).zip(NonZeroU16::new(100)))
            .map(|(width, height)| Pixels { width, height });
        set_window_size(&master, size(5, 7)?, pixels).map_err(|e| e.to_string())?;
        assert_eq!(window_size(&slave), Some((size(5, 7)?, pixels)));
        let mut modes = attributes(&slave).map_err(|e| e.to_string())?;
        assert!(modes.echoes(), "a new terminal echoes");
        assert!(modes.edits_lines(), "and edits a line at a time");
        let saved = modes.clone();
        modes.make_raw();
        set_attributes(&slave, &modes).map_err(|e| e.to_string())?;
        let again = attributes(&slave).map_err(|e| e.to_string())?;
        assert!(!again.echoes() && !again.edits_lines());
        // The master reads the modes the program set on its side.
        let seen = attributes(&master).map_err(|e| e.to_string())?;
        assert!(!seen.echoes() && !seen.edits_lines());
        // Restoring the complete saved attributes restores both flags.
        set_attributes(&slave, &saved).map_err(|e| e.to_string())?;
        let restored = attributes(&slave).map_err(|e| e.to_string())?;
        assert!(restored.echoes() && restored.edits_lines());
        // Raw while the guard lives, as it was once it is dropped; its
        // newlines and end-of-file key read as they are set.
        assert!(restored.newline_as_crlf());
        assert_eq!(restored.end_of_file(), Some(0x04), "ended by ^D");
        {
            let _raw = Raw::enter_draining(&slave).map_err(|e| e.to_string())?;
            let raw = attributes(&slave).map_err(|e| e.to_string())?;
            assert!(!raw.echoes() && !raw.edits_lines() && !raw.newline_as_crlf());
            // Another held and dropped leaves this one for the panic hook.
            let (_other_master, other) = pty::open(size(2, 2)?).map_err(|e| e.to_string())?;
            drop(Raw::enter(&other).map_err(|e| e.to_string())?);
            let fd = slave.as_fd().as_raw_fd();
            let kept = (HELD.lock().map_err(|e| e.to_string())?)
                .iter()
                .any(|(_, raw, ..)| *raw == fd);
            assert!(kept);
        }
        let mut back = attributes(&slave).map_err(|e| e.to_string())?;
        assert!(back.echoes() && back.edits_lines() && back.newline_as_crlf());
        back.set_newline_as_crlf(false);
        assert!(!back.newline_as_crlf() && back.echoes());
        // No session has it as its terminal: no foreground group.
        assert_eq!(foreground_group(&master), Ok(None));
        assert!(attributes(std::fs::File::open("/dev/null").map_err(|e| e.to_string())?).is_err());
        Ok(())
    }
}
