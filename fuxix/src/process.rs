//! Processes: their IDs, signals, and how they end. What a process ID is
//! differs by OS, and so does what can be done to a process: on Unix, the
//! signals, sessions, reaping, tree and `Job` in `unix.rs`; on Windows, a
//! `Job` is a job object (`windows.rs`). The signal names and how a process
//! ended are fux's on every OS, as the wire carries them, and so are a
//! job's [`Tree`], [`Reach`] and [`Sent`].
use crate::errno::Errno;
#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::*;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;

/// Expands `/// doc, then Variant "NAME" SIGNAL, ...` to the enum `Signal` of those
/// variants, `Signal::ALL` in that order, `name` (its `NAME`), and on
/// Unix `raw` (each one's `libc::SIGNAL`) and `from_raw` (the variant of a
/// number): `Term "TERM" SIGTERM,` gives `Signal::Term`, named "TERM", raw
/// `libc::SIGTERM`. Windows numbers no signal, so there a signal has no
/// number at all.
macro_rules! signals {
    ($(#[$doc:meta])* $($variant:ident $name:literal $raw:ident,)*) => {
        $(#[$doc])*
        #[derive(Clone, Copy, PartialEq, Eq, Debug)]
        pub enum Signal {
            $($variant,)*
        }

        impl Signal {
            /// Every signal, in the order they are declared.
            pub const ALL: [Signal; [$($name),*].len()] = [$(Signal::$variant),*];

            /// The signal's number.
            #[cfg(unix)]
            pub fn raw(self) -> i32 {
                match self {
                    $(Signal::$variant => libc::$raw,)*
                }
            }

            /// The signal numbered `raw`, if it is one of these.
            #[cfg(unix)]
            pub fn from_raw(raw: i32) -> Option<Signal> {
                match raw {
                    $(libc::$raw => Some(Signal::$variant),)*
                    _ => None,
                }
            }

            /// Its name without `SIG`: `TERM`.
            pub fn name(self) -> &'static str {
                match self {
                    $(Signal::$variant => $name,)*
                }
            }
        }
    };
}

signals! {
    /// The signals fux and its tests and walk send (fux itself: `Hup`, `Term`,
    /// `Kill`, and any of these for `fux signal`), and `Tstp`, which a
    /// client sends itself to suspend: a shell reports a job `Tstp` stopped
    /// as "Stopped", and one `Stop` stopped as "Stopped (signal)". From
    /// `Ill` on, the rest of POSIX's, each numbered as the system numbers
    /// it: those a program may die of on its own, which fux names when it
    /// tells a death. Every OS has the whole set, as `fux signal` names
    /// them on the wire; what Windows does with each is fux-server's.
    Hup "HUP" SIGHUP,
    Int "INT" SIGINT,
    Term "TERM" SIGTERM,
    Kill "KILL" SIGKILL,
    Stop "STOP" SIGSTOP,
    Tstp "TSTP" SIGTSTP,
    Cont "CONT" SIGCONT,
    Winch "WINCH" SIGWINCH,
    Quit "QUIT" SIGQUIT,
    Usr1 "USR1" SIGUSR1,
    Usr2 "USR2" SIGUSR2,
    Ill "ILL" SIGILL,
    Abrt "ABRT" SIGABRT,
    Fpe "FPE" SIGFPE,
    Segv "SEGV" SIGSEGV,
    Pipe "PIPE" SIGPIPE,
    Alrm "ALRM" SIGALRM,
    Bus "BUS" SIGBUS,
    Trap "TRAP" SIGTRAP,
    Sys "SYS" SIGSYS,
    Xcpu "XCPU" SIGXCPU,
    Xfsz "XFSZ" SIGXFSZ,
    Vtalrm "VTALRM" SIGVTALRM,
    Prof "PROF" SIGPROF,
    Ttin "TTIN" SIGTTIN,
    Ttou "TTOU" SIGTTOU,
    Urg "URG" SIGURG,
    Chld "CHLD" SIGCHLD,
}

/// How a process ended.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    /// It exited with this code. On Windows, the program's own 32-bit
    /// exit code, read as `i32` as std, cmd's `%ERRORLEVEL%` and
    /// PowerShell's `$LASTEXITCODE` read it: `0xC000013A`
    /// (`STATUS_CONTROL_C_EXIT`, a program Ctrl-C ended) is -1073741510.
    /// Windows ends no process by a signal, so every process there ends so.
    Exited(i32),
    /// The signal numbered `signal` killed it ([`Signal::from_raw`] names
    /// it, unless it is one Signal has no name for: a real-time one), and
    /// whether it dumped core.
    #[cfg(unix)]
    Signalled { signal: i32, core_dumped: bool },
}

/// What a program [`lead_session`] starts does with each signal as it
/// begins. Either way no signal is blocked.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dispositions {
    /// As this process inherited them, as from a shell: a signal it ignores
    /// stays ignored (`nohup`'s SIGHUP), and so does one ignored before a
    /// [`crate::signal::Pipe`] caught it; one it catches is the default.
    /// For a wrapper, which runs a command as it was itself run.
    Inherited,
    /// Every signal the default, as a new terminal's program begins
    /// (tmux's and screen's choice): ^C interrupts, whatever this process
    /// was started ignoring.
    Default,
}

/// The most processes a [`tree`] holds: a runaway tree is cut short here.
pub(crate) const TREE_MAX: usize = 4096;

/// What a [`tree`] found, and whether it looked everywhere.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tree {
    pub pids: Vec<Pid>,
    pub scan: Scan,
}

/// How a scan of the tree ended. `Complete` says it read every process it
/// found, not that what it found is all there is once used: a process may
/// start or end meanwhile.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Scan {
    #[default]
    Complete,
    /// There may be more: past `TREE_MAX`, or a list of a session's
    /// members that filled its buffer on macOS.
    Truncated,
    /// The system refused a list, with this error; it read the others.
    Failed(Errno),
}

/// Which of a [`Job`]'s processes a signal or [`Job::kill`] reaches.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reach {
    /// Its leader's process group: what it started, but a job a shell put
    /// in a group of its own, or one that left the group. On Windows,
    /// which has no process groups, the leader alone.
    Group,
    /// Its group and every other process in its session ([`Job::members`]).
    /// On Windows, every process in its job object.
    Session,
}

/// What a [`Job::signal`] may have missed: the members it did not find
/// (the scan for them, [`Scan::Complete`] for [`Reach::Group`]), and each
/// process or group (its leader's pid) the system did not signal, with
/// the error as it is: [`Errno::SRCH`] for one gone already.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sent {
    pub scan: Scan,
    pub failed: Vec<(Pid, Errno)>,
}
