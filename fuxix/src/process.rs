//! Processes: their IDs, signals, and how they end. What a process ID is
//! differs by OS, and so does what can be done to a process: on Unix, the
//! signals, sessions, reaping, tree and `Job` in `unix.rs`. The signal names and
//! how a process ended are fux's on every OS, as the wire carries them.
#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::*;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{Pid, Watch, watch};

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
