//! Whether a pane's program waits for its terminal: the moment an agent
//! driving it should look. A program that wants input stops and reads its
//! terminal, and fux, which runs it, can see that: on Linux, what the
//! threads of the terminal's foreground group are blocked in
//! (`fuxix::process::activity`). Where the system does not say, a quiet
//! screen is taken as settled, and said to be a guess.
use crate::pane::{Pane, Process};
use crate::process::Ended;
use fuxix::process::Activity;
use std::path::Path;
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
    /// It waits for something else (the network, a pipe, another program,
    /// keys with a time limit) and has been quiet a while. A program asleep on a timer is busy: it
    /// is waited for, up to the limit.
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
    // Keys not yet written, or output written that fux has not read: at
    // work still.
    if !pane.input_written() || fuxix::io::unread(&child.master).unwrap_or(0) > 0 {
        return State::Busy;
    }
    // Quiet since the screen last changed and input last went in.
    let since = (pane.written_at).map_or(pane.changed_at, |at| at.max(pane.changed_at));
    let quiet = now.saturating_duration_since(since);
    let tty = child.master.slave_path();
    let activity = activity(child, tty.as_deref(), pane.since_key(now));
    // A shell command still runs a program that is not full-screen and
    // does not read: it is at work until it ends or asks.
    let screen = pane.screen();
    let commands = screen.commands();
    let command = commands.running && !screen.mode(fux_vt::Mode::AlternateScreen);
    // A shell back at its prompt, reading, its last command ended: what
    // else runs in its group is a job it left there (job control off, or
    // fish's disowned start-up work), not the agent's command.
    let left = matches!(
        activity,
        Activity::Busy | Activity::Polling | Activity::Waiting
    ) && !commands.running
        && commands.finished > 0
        && tty
            .as_deref()
            .is_some_and(|tty| fuxix::process::reads_terminal(child.leader.pid(), tty));
    let activity = if left { Activity::Reading } else { activity };
    match activity {
        Activity::Reading if quiet >= SETTLE && !keys_unread(tty.as_deref()) => State::Reading,
        Activity::Polling | Activity::Waiting if command => State::Busy,
        // Polling for keys with a time limit: it may draw again on its own
        // (a pause, a clock), so never surely waiting for the agent.
        Activity::Polling | Activity::Waiting if quiet >= QUIET => State::Waiting,
        Activity::Unknown if quiet >= GUESS => State::Settled,
        Activity::Reading
        | Activity::Polling
        | Activity::Waiting
        | Activity::Unknown
        | Activity::Busy => State::Busy,
    }
}

/// What the foreground group of `child`'s terminal, at `tty`, is blocked
/// in, the last key written `since_key` ago. With no foreground group (its
/// program dropped its terminal, or is gone, its end not yet heard), its
/// first process's own group: busy once none is left in it.
fn activity(child: &crate::process::Child, tty: Option<&Path>, since_key: Duration) -> Activity {
    let leader = child.leader.pid();
    let group = fuxix::terminal::foreground_group(&child.master).unwrap_or(leader);
    match tty.map(|tty| fuxix::process::activity(group, tty, leader)) {
        // A read with a time limit is a poll: the program goes on alone
        // when it runs out.
        Some(Activity::Reading)
            if fuxix::terminal::attributes(&child.master)
                .is_ok_and(|t| t.timed_reads(since_key)) =>
        {
            Activity::Polling
        }
        Some(activity) => activity,
        None => Activity::Unknown,
    }
}

/// Whether the program has read every key and waits for more, with no time
/// limit: a lone Escape sent is taken alone by then, its timer run out.
pub fn took_keys(child: &crate::process::Child, since_key: Duration) -> bool {
    let tty = child.master.slave_path();
    activity(child, tty.as_deref(), since_key) == Activity::Reading && !keys_unread(tty.as_deref())
}

/// Whether keys wait in the terminal for its program to read: blocked in a
/// read still, it has yet to be woken (a busy machine) to read them. The
/// terminal is opened only to ask, so that its program's end still closes
/// it.
fn keys_unread(tty: Option<&Path>) -> bool {
    use std::os::unix::fs::OpenOptionsExt;
    tty.and_then(|tty| {
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(fuxix::file::NOCTTY | fuxix::file::NONBLOCK)
            .open(tty)
            .ok()
    })
    .and_then(|terminal| fuxix::io::unread(&terminal).ok())
    .is_some_and(|n| n > 0)
}
