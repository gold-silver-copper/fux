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
    ($(#[$doc:meta])* $($variant:ident $name:literal $raw:ident $linux:literal,)*) => {
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

            /// Its number as Linux has it: what a record says of it on
            /// Windows, which has no signal numbers.
            #[cfg(windows)]
            pub fn raw(self) -> i32 {
                match self {
                    $(Signal::$variant => $linux,)*
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
    Hup "HUP" SIGHUP 1,
    Int "INT" SIGINT 2,
    Term "TERM" SIGTERM 15,
    Kill "KILL" SIGKILL 9,
    Stop "STOP" SIGSTOP 19,
    Tstp "TSTP" SIGTSTP 20,
    Cont "CONT" SIGCONT 18,
    Winch "WINCH" SIGWINCH 28,
    Quit "QUIT" SIGQUIT 3,
    Usr1 "USR1" SIGUSR1 10,
    Usr2 "USR2" SIGUSR2 12,
    Ill "ILL" SIGILL 4,
    Abrt "ABRT" SIGABRT 6,
    Fpe "FPE" SIGFPE 8,
    Segv "SEGV" SIGSEGV 11,
    Pipe "PIPE" SIGPIPE 13,
    Alrm "ALRM" SIGALRM 14,
    Bus "BUS" SIGBUS 7,
    Trap "TRAP" SIGTRAP 5,
    Sys "SYS" SIGSYS 31,
    Xcpu "XCPU" SIGXCPU 24,
    Xfsz "XFSZ" SIGXFSZ 25,
    Vtalrm "VTALRM" SIGVTALRM 26,
    Prof "PROF" SIGPROF 27,
    Ttin "TTIN" SIGTTIN 21,
    Ttou "TTOU" SIGTTOU 22,
    Urg "URG" SIGURG 23,
    Chld "CHLD" SIGCHLD 17,
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

/// What a program started on a terminal (`lead_session` on Unix,
/// [`crate::pty::Slave::spawn`]) does with each signal as it begins. Either way no signal is blocked.
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

/// The most processes a `Tree` holds: a runaway tree is cut short here.
pub(crate) const TREE_MAX: usize = 4096;

/// What a scan (Unix's `tree`, Windows's `Job::attached`) found, and
/// whether it looked everywhere.
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

/// Who a process is: its pid, and when it was made, in its own clock's counts
/// (Windows: 100 ns counts since 1601). A pid is reused once its process is
/// gone and every handle to it closed; the two together are never.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Identity {
    pub pid: Pid,
    pub created: u64,
}

/// What a process offers another of the program it started: who the
/// program is, and the value of its handle to it in the offering process,
/// which the other copies (`adopt` (Windows only)). Its numbers are what crosses
/// between the two.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Offer {
    pub program: Identity,
    pub handle: u64,
}

/// How a signal reaches a pane's programs ([`delivery`]): on Unix always
/// as a signal to its foreground group; on Windows (D-136) as one of the
/// others.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Delivery {
    /// Sent to the foreground process group, as Unix sends it.
    Group,
    /// Written to the terminal as these keys, after what was typed before.
    Keys(&'static [u8]),
    /// Every process on the console but the shell is ended
    /// (`Job::kill` with `Reach::Attached`).
    Attached,
    /// The pane's job is ended (`Job::kill` with `Reach::Session`).
    Job,
    /// It has no counterpart here: refused.
    Unsupported,
}

/// What a job this process is in lets the processes it starts do
/// ([`job_fate`]): on Unix, which has no jobs, always `Free`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fate {
    /// In no job: what it starts outlives whatever started it.
    Free,
    /// In a job its children may leave, as fux's starts do (D-135).
    Leaves,
    /// In a job its children may not leave: they end as the job does,
    /// when the terminal or launcher that made it ends.
    Bound,
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
    /// Every process but the leader: on Windows each on its pseudo
    /// console that is in its job (`Job::attached`), what a shell runs,
    /// while the shell lives on (D-136's KILL of a shell pane); on Unix
    /// each of [`Job::members`], which fux never asks for (D-136 is
    /// Windows').
    Attached,
}

/// What a [`Job::signal`] may have missed: the members it did not find
/// (the scan for them, [`Scan::Complete`] for [`Reach::Group`]), and each
/// process or group (its leader's pid) the system did not signal, with
/// the error as it is: [`Errno::SRCH`] for one gone already.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sent {
    pub scan: Scan,
    pub failed: Vec<(Pid, Errno)>,
    /// How many processes the scan found and each was sent to, one by one
    /// (beside a group, or a job object ended whole): 0 with a complete
    /// scan says none was there but what the reach left out.
    pub found: usize,
}
