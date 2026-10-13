//! Whether a pane's program waits for its terminal: the moment an agent
//! driving it should look. A program that wants input stops and reads its
//! terminal, and fux, which runs it, can see that: on Linux, what the
//! threads of the terminal's foreground group are blocked in
//! (`fuxix::process::activity`). Where the system does not say, a quiet
//! screen is taken as settled, and said to be a guess.
use crate::pane::{Pane, Process};
use crate::process::Ended;
use fuxix::process::Activity;
use std::time::{Duration, Instant};

/// How long a program reading its terminal must leave the screen as it is:
/// what it drew just before it read is in.
pub const SETTLE: Duration = Duration::from_millis(50);
/// How long a program waiting on something else, or polling for keys, must
/// be quiet to be taken as settled: what it waits for may still come.
pub const QUIET: Duration = Duration::from_secs(1);
/// How long a quiet screen is taken as settled where nothing else is known.
pub const GUESS: Duration = Duration::from_millis(300);

/// What a pane's program is doing, as an agent is told.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// It waits for input: blocked reading its terminal, every key sent
    /// read.
    Reading,
    /// It waits for something else (the network, another program, a
    /// timer) and has been quiet a while.
    Waiting,
    /// The system does not say; the screen has been quiet a while.
    Settled,
    /// It runs, sleeps, prints, or has not read the keys sent yet.
    Busy,
    /// It ended so.
    Exited(Ended),
    /// No program was started in it.
    Absent,
}

impl State {
    /// Whether an agent should look now: anything but busy.
    pub fn settled(self) -> bool {
        !matches!(self, State::Busy)
    }

    /// The word an answer starts with.
    pub fn word(self) -> String {
        match self {
            State::Reading => "reading".into(),
            State::Waiting => "waiting".into(),
            State::Settled => "settled".into(),
            State::Busy => "busy".into(),
            State::Absent => "empty".into(),
            State::Exited(ended) => crate::process::told(ended),
        }
    }
}

/// What the program in `pane` is doing at `now`.
pub fn state(pane: &Pane, now: Instant) -> State {
    let child = match &pane.process {
        Process::Exited(ended) => return State::Exited(*ended),
        Process::Absent => return State::Absent,
        Process::HungUp(_) => return State::Busy,
        Process::Reading(child) => child,
    };
    if !pane.input_written() {
        return State::Busy;
    }
    // Quiet since the screen last changed and input last went in.
    let quiet = now.saturating_duration_since(pane.changed_at.max(pane.written_at));
    let activity = fuxix::terminal::foreground_group(&child.master)
        .zip(child.master.slave_path())
        .map_or(Activity::Unknown, |(group, tty)| {
            fuxix::process::activity(group, &tty)
        });
    match activity {
        Activity::Reading if quiet >= SETTLE => State::Reading,
        Activity::Polling if quiet >= QUIET => State::Reading,
        Activity::Waiting if quiet >= QUIET => State::Waiting,
        Activity::Unknown if quiet >= GUESS => State::Settled,
        Activity::Reading
        | Activity::Polling
        | Activity::Waiting
        | Activity::Unknown
        | Activity::Busy => State::Busy,
    }
}

/// Whether the program is surely blocked reading its terminal: a text it
/// prints only after more input cannot come before the agent types.
pub fn reading(pane: &Pane, now: Instant) -> bool {
    state(pane, now) == State::Reading
}
