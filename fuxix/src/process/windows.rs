//! Processes on Windows: their IDs.
use std::fmt;
use std::num::NonZeroU32;

/// A process ID: a nonzero `DWORD`. 0 is the System Idle Process, which
/// fux never names.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct Pid(NonZeroU32);

impl Pid {
    /// `raw` as a process ID, if it is one: nonzero.
    pub fn from_raw(raw: u32) -> Option<Pid> {
        NonZeroU32::new(raw).map(Pid)
    }

    /// The ID of a child std started. `Option`, as on Unix, though std's
    /// `u32` is Windows' own type: only a 0 is refused.
    pub fn of(child: &std::process::Child) -> Option<Pid> {
        Pid::from_raw(child.id())
    }

    pub fn as_raw(self) -> u32 {
        self.0.get()
    }
}

impl fmt::Display for Pid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
