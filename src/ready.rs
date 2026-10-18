//! Whether a pane's program waits for its terminal: the moment an agent
//! driving it should look. A program that wants input stops and reads its
//! terminal, and fux, which runs it, can see that, as far as the system
//! says (`fuxix::process::activity`, as built for the system):
//! - Linux and Android name what each thread of the terminal's foreground
//!   group is blocked in: reading the terminal, polling it, waiting on
//!   something else, or busy.
//! - macOS says only whether each thread runs or waits: once every one
//!   waits (idle), every key read and the screen quiet a moment, the
//!   program is settled, as it cannot be said to read.
//! - Where the system does not say (a probe that failed), a quiet screen
//!   is taken as settled after a longer while, and said to be a guess.
//!
//! A relay (`koh connect`) runs a program fux cannot probe, on another
//! machine; the relay can, and says what it does in a state report (`OSC
//! 133 ; S ; activity=STATE`). Its last report stands in for the probe
//! while no mark came after it and it came after the last input fux
//! wrote; the rules below then apply to it as they do to the probe's.
use crate::pane::{Pane, Process};
use crate::process::{Child, Ended, told};
use fux_vt::Reported;
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
/// How long a program that redraws on its own may leave the screen as it
/// is between frames and still be taken to: longer than `QUIET`, so that a
/// frame each second (a select timed so) counts.
pub const REDRAWN: Duration = Duration::from_secs(2);

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
    /// A full-screen program that redraws on its own (a clock, `top`) and
    /// is never quiet: it read every key and drew after them. What it
    /// draws next may differ, keys or not.
    Redrawing,
    /// It runs, sleeps, prints, or has not read the keys sent yet.
    Busy,
    /// It ended so.
    Exited(Ended),
    /// No program was started in it.
    Absent,
}

impl State {
    /// The word an answer starts with.
    pub fn word(self) -> String {
        match self {
            Self::Reading => "reading".into(),
            Self::Waiting | Self::Redrawing => "waiting".into(),
            Self::Settled => "settled".into(),
            Self::Busy => "busy".into(),
            Self::Absent => "empty".into(),
            Self::Exited(ended) => told(ended),
        }
    }
}

/// What the program in `pane` is doing at `now`; `typed`, how many
/// commands its marks had begun as the input of the wait asking was
/// written, if it waits for one that input began to end (`line`).
pub fn state(pane: &Pane, now: Instant, typed: Option<u64>) -> State {
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
    // The command the input typed still runs and does not read: at work
    // until it ends or asks. One that took the full screen since it began
    // (an editor, a pager, polling there) is not at work; one whose output
    // began there, marked by the full-screen program itself (a remote
    // shell's, relayed), runs until it says it ended. A command begun
    // before the input (keys sent into it) is not waited for so.
    let screen = pane.screen();
    let commands = screen.commands();
    let command = commands.running()
        && typed.is_some_and(|n| commands.started != n)
        && (commands.full_screen || !screen.mode(fux_vt::Mode::AlternateScreen));
    // A line editor reading a line takes pastes (bracketed paste on, as
    // Python's REPL turns it on while it reads): a program polling its
    // terminal then waits for keys, not a pause that watches for them.
    let editing = || screen.mode(fux_vt::Mode::BracketedPaste) && !child.master.keys_unread();
    // One that draws on its own as it polls its terminal (`top`, the
    // command typed or not), or on the full screen however it waits (macOS
    // says only idle): not a build printing as it goes, nor a pause that
    // watches for keys and draws nothing.
    let redraws = |activity| {
        (activity == Activity::Polling || (!command && screen.mode(fux_vt::Mode::AlternateScreen)))
            && pane.redraws()
            && pane.written_at.is_none_or(|at| pane.changed_at > at)
            && quiet >= SETTLE
            && !child.master.keys_unread()
    };
    match reported(pane).unwrap_or_else(|| activity(pane, child, now)) {
        Activity::Reading if quiet >= SETTLE && !child.master.keys_unread() => State::Reading,
        // A program that redraws on its own (a clock, `top`) is never
        // quiet: it took the keys once it read them all and the
        // frame it drew after them is done, however it waits between
        // frames. A key's frame follows its read at once, together with
        // a frame due then.
        activity if redraws(activity) => State::Redrawing,
        // Blocked elsewhere (a download, a pipe): the command is at work.
        Activity::Waiting if command => State::Busy,
        // Polling its terminal, editing a line (Python's REPL): it waits for
        // keys once quiet with every key read, the command running on.
        Activity::Polling if command && !editing() => State::Busy,
        // Polling for keys with a time limit: it may draw again on its own
        // (a pause, a clock), so never surely waiting for the agent.
        Activity::Polling | Activity::Waiting if quiet >= QUIET => State::Waiting,
        // Every thread waits, on what macOS will not say (a sleep or a
        // read): settled once quiet as long as where nothing is known.
        Activity::Idle if quiet >= GUESS && !child.master.keys_unread() => State::Settled,
        Activity::Unknown if quiet >= GUESS => State::Settled,
        Activity::Reading
        | Activity::Polling
        | Activity::Waiting
        | Activity::Idle
        | Activity::Unknown
        | Activity::Busy => State::Busy,
    }
}

/// What the foreground group of `child`'s terminal is blocked in, as
/// `pane`'s last key and its screen since say by `now`
/// (`fuxix::process::terminal_activity`): a shell back at its prompt,
/// reading, reads.
fn activity(pane: &Pane, child: &Child, now: Instant) -> Activity {
    // How long ago the last key went to the program: forever, if none has;
    // and whether the screen changed after it: the program answered it.
    let since_key = (pane.written_at).map_or(Duration::MAX, |at| now.saturating_duration_since(at));
    let answered = pane.written_at.is_some_and(|at| pane.changed_at > at);
    let commands = pane.screen().commands();
    let prompt = !commands.running() && commands.finished > 0;
    let leader = child.leader.pid();
    fuxix::process::terminal_activity(&child.master, leader, since_key, answered, prompt)
}

/// What the program a relay in `pane` runs does, as its last state report
/// says (`OSC 133 ; S`), while that holds: no mark came after it, and it
/// came after the last input fux wrote, so it answers that input. The
/// pane's own program runs still, as `state` has found.
fn reported(pane: &Pane) -> Option<Activity> {
    let reported = pane.screen().commands().reported()?;
    let (_, at) = pane.reported_at?;
    if pane.written_at.is_some_and(|written| written >= at) {
        return None;
    }
    Some(match reported {
        Reported::Reading => Activity::Reading,
        Reported::Polling => Activity::Polling,
        Reported::Waiting => Activity::Waiting,
        Reported::Busy => Activity::Busy,
        Reported::Idle => Activity::Idle,
        Reported::Unknown => Activity::Unknown,
    })
}

/// What the program of `child`, `pane`'s, is blocked in by `now`
/// (`activity`); `None` while keys wait in its terminal unread.
pub fn taking(pane: &Pane, child: &Child, now: Instant) -> Option<Activity> {
    let activity = activity(pane, child, now);
    (!child.master.keys_unread()).then_some(activity)
}
