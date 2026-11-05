//! A pane's own process: its terminal, its input queue and its program,
//! which it starts, reaps and starts again (D-120). What it does that the
//! server must know it tells it, in order (`link::FromPane`): each event
//! with the counts it left the pane at, so that the server journals it and
//! updates its copy of the pane in one step (`copy`); its screen, a frame at
//! a time; and its answers to what it is asked.
//!
//! `Host` is that process's state, run by its loop (`run`); a session held
//! in one process (the tests', the tools') holds its panes' hosts itself
//! (`Link::Local`).
use crate::journal::{Sent, Told};
use crate::link::{Answered, Ask, Begin, FromPane, Hosting, Refusal, ToPane, Typed, Update};
use crate::pane::{self, Pane, PaneMut, Process, Runs};
use crate::process::{self, Child, Pid};
use crate::render::Grid;
use fux_vt::Size;
use fux_wire::answer::{self, Ended};
use fux_wire::code::{Error, Raise, fail};
use fux_wire::command::{self, Input, Mouse, Run, Version};
use fux_wire::events::Actor;
use fux_wire::id::{ClientId, PaneId};
use fuxix::process::Job;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// How long a closing pane's program has to exit after the hangup.
pub const GRACE: Duration = Duration::from_millis(100);

/// A closed pane's program, or one respawned over, hung up and given until
/// the deadline to exit; then its terminal is closed and its group killed.
pub struct Dying {
    pub child: Child,
    pub deadline: Instant,
}

impl Dying {
    /// Kills the program, its terminal closed first (a dying writer can hold
    /// on to it); its leader, if it has yet to exit.
    pub fn kill(self) -> Option<Job> {
        drop(self.child.master);
        self.child.leader.finish()
    }
}

/// A respawn under way: the pane made for the program started, and the
/// program, which take the place of the pane's own once all its old
/// program left is read (`Host::after_turn`); the new program's output
/// waits on its terminal till then. Its answer, the version the pane
/// starts at, waits for it.
struct Replacement {
    ask: u64,
    actor: Actor,
    fresh: Pane,
    process: Process,
    /// The program it is respawned over, while what it left on its
    /// terminal is still being read (`Host::read_left`).
    old: Option<Child>,
    /// When the old program's grace after the hangup ends: then it is
    /// killed, and what it left is still read. `None` once it is.
    grace: Option<Instant>,
    /// When it began: its turns are due from then.
    since: Instant,
}

/// Something a pane waits for until a moment (`Host::timer`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Timer {
    /// Its frame of synchronized output is read, ended or not.
    Frame,
    /// Its output, waiting since the parser did all the work one call may,
    /// is read on; a respawn under way takes its turn too.
    Unread,
    /// Its closed program's grace is over: it is killed.
    Grace,
}

/// A pane's process's state (see the module documentation).
pub struct Host {
    pane: Pane,
    run: Run,
    /// The socket its programs are told (`FUX_SOCKET`), if it starts real
    /// ones.
    socket: Option<PathBuf>,
    exe: Option<PathBuf>,
    /// What its host's terminal said, as the server last told it.
    hosting: Hosting,
    /// Who causes what it does now: the asker of the request it serves.
    actor: Actor,
    told: Vec<(Told, Actor)>,
    /// New input waits to be written.
    pub input: bool,
    respawning: Option<Replacement>,
    /// A program whose exit is told, its terminal still read till it is
    /// empty or closed (`read_left`); then its pane is frozen (D-90).
    leftover: Option<Child>,
    dying: Vec<Dying>,
    /// Programs killed and not yet reaped.
    killed: Vec<Job>,
    out: Vec<FromPane>,
    /// It was closed: by fux if `true` (`kill`), else by its program's end.
    closed: Option<bool>,
    /// Whether a client shows it: what it let go of is told only then.
    shown: bool,
    /// Its facts, title, counts and size as last sent its server.
    sent_facts: Option<Facts>,
    /// Something happened since its server was last sent its facts.
    dirty: bool,
    /// Counts what happened to it, so that a connection watching its
    /// screen is sent it again once it may have changed (`Watch`); and its
    /// programs, a respawn's screen being sent whole.
    revision: u64,
    whole: u64,
    /// Bodies its asker's connections sent (`ToSide::Body`), the questions
    /// that wait for theirs, and the captures it holds for its askers'
    /// connections to fetch (`ToSide::Fetch`): each by its token.
    bodies: BTreeMap<u64, Vec<Input>>,
    awaiting: BTreeMap<u64, Awaiting>,
    held: BTreeMap<u64, Box<fux_wire::answer::Captured>>,
}

/// What waits for a body: a question's keys, or a client's paste.
enum Awaiting {
    Keys {
        ask: u64,
        actor: Actor,
        seen: Option<Version>,
    },
    Paste {
        client: ClientId,
    },
}

/// What a pane's server is told of it beside its events: its facts, title,
/// counts and size.
type Facts = (fux_wire::inspect::Facts, pane::Said, command::Counts, Size);

/// A connection's process watching a pane's screen (`ToSide::Watch`): the
/// screen it was last sent, at which revision, and whether it took it.
#[derive(Default)]
pub struct Watch {
    revision: Option<u64>,
    whole: u64,
    sent: Option<Grid>,
    /// The grid its next screen is composed on: the one sent before
    /// `sent`, whose rows that changed since are drawn again.
    spare: Grid,
    pictures: Vec<crate::render::Visible>,
    own: crate::render::Own,
    /// It was sent a screen it did not take yet.
    pub waiting: bool,
}

impl Host {
    /// A pane as `begin` says, its first program started, if it starts
    /// real ones: else why not.
    pub fn begin(begin: Begin, now: Instant) -> Result<Host, String> {
        let launch = pane::Launch {
            argv: begin.argv,
            cwd: begin.cwd,
        };
        let runs = match begin.shell {
            true => Runs::Shell(launch.clone()),
            false => Runs::Explicit(launch.clone()),
        };
        let mut pane = Pane::new(begin.id, None, runs, begin.size, begin.options, now)
            .map_err(|e| e.to_string())?;
        let graphics = fux_vt::Graphics::from(begin.hosting.draws);
        let _ = pane.host(begin.hosting.colours(), begin.hosting.palette(), graphics);
        let told = Env::of(
            begin.id,
            begin.run,
            begin.socket.as_deref(),
            begin.exe.as_deref(),
        );
        let child =
            spawn(&told, 0, &launch, (begin.size, graphics.cell())).map_err(|e| e.to_string())?;
        if let Some(child) = child {
            pane.run(child);
        }
        Ok(Host {
            pane,
            run: begin.run,
            socket: begin.socket,
            exe: begin.exe,
            hosting: begin.hosting,
            actor: Actor::Server,
            told: Vec::new(),
            input: false,
            respawning: None,
            leftover: None,
            dying: Vec::new(),
            killed: Vec::new(),
            out: Vec::new(),
            closed: None,
            shown: false,
            sent_facts: None,
            dirty: true,
            revision: 0,
            whole: 0,
            bodies: BTreeMap::new(),
            awaiting: BTreeMap::new(),
            held: BTreeMap::new(),
        })
    }

    pub fn id(&self) -> PaneId {
        self.pane.id()
    }

    /// Its pane, to read.
    pub fn pane(&self) -> &Pane {
        &self.pane
    }

    /// Its pane, to change, as `actor`'s request has it.
    pub fn pane_mut(&mut self) -> PaneMut<'_> {
        PaneMut::new(&mut self.pane, self.actor, &mut self.told, &mut self.input)
    }

    /// Whether it is done: closed, with no program left to end.
    pub fn finished(&self) -> bool {
        self.closed.is_some()
            && self.pane.process().child().is_none()
            && self.respawning.is_none()
            && self.leftover.is_none()
            && self.dying.is_empty()
            && self.killed.is_empty()
    }

    /// What it has to tell, in order: nothing part way through a frame,
    /// whose reading takes more than one call of the parser, so that what
    /// the frame does is told whole, with the counts it leaves.
    pub fn take(&mut self) -> Vec<FromPane> {
        if self.pane.mid_frame() {
            return Vec::new();
        }
        self.tell_told();
        if !self.out.is_empty() {
            self.changed();
        }
        std::mem::take(&mut self.out)
    }

    /// What it told goes out first, at the counts it is at now.
    fn tell_told(&mut self) {
        if self.told.is_empty() {
            return;
        }
        // The last of them carries the facts they leave too: a fact a
        // waiter reads once an event is told is never behind it.
        let counts = self.pane.counts();
        let facts = self
            .facts()
            .map_err(|error| log!("{}: its facts: {error}", self.pane.id()));
        let mut told = std::mem::take(&mut self.told).into_iter().peekable();
        while let Some((what, actor)) = told.next() {
            let facts = match told.peek() {
                None => facts.clone().ok().map(Box::new),
                Some(_) => None,
            };
            self.out.push(FromPane::Told {
                told: what,
                actor,
                counts,
                facts,
            });
        }
    }

    /// Tells the server `report`, after what it told before.
    fn report(&mut self, report: FromPane) {
        self.tell_told();
        self.out.push(report);
    }

    /// What the server says.
    pub fn handle(&mut self, message: ToPane, now: Instant) {
        // What may change its screen or facts: keys typed do not (an echo
        // is its program's output), nor its facts taken.
        if !matches!(message, ToPane::Typed { .. } | ToPane::Taken) {
            self.dirty = true;
        }
        match message {
            // Its first word, which `begin` took.
            ToPane::Begin(_) | ToPane::Taken => {}
            // Asked the size it has, it keeps it: a size refused before is
            // a new refusal when asked again.
            ToPane::Resize { size, .. } if self.pane.size() == size => {
                self.pane_mut().keeps_size();
            }
            ToPane::Resize { size, actor } => {
                let previous = std::mem::replace(&mut self.actor, actor);
                self.read_with(|pane| pane.resize(size, now));
                self.actor = previous;
            }
            ToPane::Host(hosting) => self.host(hosting),
            ToPane::Shown(shown) => {
                self.shown = shown;
                self.tell_refused();
            }
            ToPane::Typed { client, typed } => {
                let previous = std::mem::replace(&mut self.actor, Actor::Client(client));
                self.typed(client, typed);
                self.actor = previous;
            }
            ToPane::Ask { ask, actor, what } => {
                let previous = std::mem::replace(&mut self.actor, actor);
                if let Some(answer) = self.ask(ask, what, now) {
                    self.report(FromPane::Answer { ask, answer });
                }
                self.actor = previous;
            }
            ToPane::Close { closed } => self.close(closed, now),
            ToPane::Drop { token } => {
                self.bodies.remove(&token);
                self.held.remove(&token);
                if let Some(Awaiting::Keys { ask, .. }) = self.awaiting.remove(&token) {
                    let refusal = Refusal {
                        code: fux_wire::code::Code::Indeterminate,
                        message: format!("{}: its asker went before its keys came", self.pane.id()),
                    };
                    self.report(FromPane::Answer {
                        ask,
                        answer: Answered::Refused(refusal),
                    });
                }
            }
            // Taken back as its link begins (`serve`).
            ToPane::Take { .. } | ToPane::Who | ToPane::Join { .. } => {}
        }
    }

    // ------------------------------------------------------------ programs

    /// Starts the program it ran again: its words, or `argv` if given,
    /// where it ran; first hanging up the one there with `kill`, else
    /// refused while it runs. What the old program left is read before the
    /// new one takes its place, its exit told if one waits on that, in as
    /// many turns as that takes (`after_turn`): till then the respawn is
    /// under way, and another refused. Its answer, the version the pane
    /// starts at, waits for it.
    fn respawn(
        &mut self,
        ask: u64,
        kill: bool,
        argv: Option<command::Program>,
        now: Instant,
    ) -> Result<(), Error> {
        let pane = &self.pane;
        let id = pane.id();
        let Runs::Explicit(launch) = &pane.runs else {
            let message =
                format!("{id} runs a shell; respawn restarts a program a pane was started with");
            return Err(fail(Raise::NotRun, message));
        };
        if self.respawning.is_some() {
            let message =
                format!("{id} is respawning already: what its program left is still being read");
            return Err(fail(Raise::StillRunning, message));
        }
        // One whose exit waits on what it left has ended.
        if pane.process().child().is_some() && pane.exiting.is_none() && !kill {
            let message = format!("the program in {id} still runs; `kill-first` ends it first");
            return Err(fail(Raise::StillRunning, message));
        }
        let mut launch = launch.clone();
        if let Some(argv) = argv {
            launch.argv = argv;
        }
        // The program it starts is told its host's cell.
        let graphics = fux_vt::Graphics::from(self.hosting.draws);
        let size = (pane.size(), graphics.cell());
        let respawns = pane.respawns.checked_add(1).ok_or_else(|| {
            fail(
                Raise::IdsExhausted,
                format!("{id}'s program generations exhausted"),
            )
        })?;
        let exiting = pane.exiting;
        // All that can refuse it is done before its program starts, which
        // nothing can then leave running.
        let runs = Runs::Explicit(launch.clone());
        let mut fresh = Pane::new(id, None, runs, size.0, pane.options(), now)
            .map_err(|e| fail(Raise::Start, e.to_string()))?;
        fresh.respawns = respawns;
        // Its program starts sized in that cell, so no resize is due.
        let _ = fresh.host(self.hosting.colours(), self.hosting.palette(), graphics);
        let told = Env::of(id, self.run, self.socket.as_deref(), self.exe.as_deref());
        let child = spawn(&told, respawns, &launch, size)?;
        // Once another started (a start refused leaves it running), its
        // program is taken, and input is refused till the new one takes its
        // place. One that runs is hung up; it, or one whose exit waits on
        // what it left, is kept till what it left is read, over as many
        // turns as that takes or its grace lasts (`read_left`); one done
        // with is let go.
        let draining = self.pane.draining;
        let mut old = None;
        let taken = self.pane_mut().let_go();
        if let Some(mut child) = taken.or_else(|| self.leftover.take()) {
            if exiting.is_none() {
                child.leader.closed();
                child.leader.hang_up();
            }
            match exiting.is_none() || draining {
                true => old = Some(child),
                false => self.dying.push(Dying {
                    child,
                    deadline: now,
                }),
            }
        }
        let process = child.map_or(Process::Absent, Process::Reading);
        self.respawning = Some(Replacement {
            ask,
            actor: self.actor,
            fresh,
            process,
            old,
            grace: Some(crate::after(now, GRACE)),
            since: now,
        });
        self.pane_mut().drain(true);
        self.read_left(true, now);
        self.after_turn(now);
        Ok(())
    }

    /// What ends a turn of its reads: once nothing read waits, a program
    /// that exited with output unread exits, and a respawn under way takes
    /// its place, once a frame its old program holds is read too, by a
    /// turn of its own (`fire`).
    fn after_turn(&mut self, now: Instant) {
        let respawning = self.respawning.is_some();
        let pane = &self.pane;
        if pane.has_unread() || respawning && pane.frame_deadline().is_some() {
            return;
        }
        if let Some(ended) = pane.exiting {
            self.exited(ended, now);
        }
        // A respawn takes its place once what the old program left is read.
        if self.closed.is_some() || self.pane.draining {
            return;
        }
        let Some(mut new) = self.respawning.take() else {
            return;
        };
        let actor = std::mem::replace(&mut self.actor, new.actor);
        // At the pane's size and history now, which may have changed while
        // it waited.
        let size = self.pane.size();
        new.fresh
            .set_history_bytes(self.pane.options().history_bytes);
        let run = self.run;
        let mut pane = self.pane_mut();
        let (old, left) = pane.respawned(new.fresh, new.process);
        pane.resize(size, now);
        // An exit that came while it was no pane's program is told now: the
        // pane looks for exits as they are signalled (`reap`).
        let exited = pane.process().child().and_then(|c| c.leader.exited());
        let started = pane.version(run);
        self.end(old, true, now);
        self.actor = actor;
        self.whole = self.whole.wrapping_add(1);
        let left = left.told();
        if !left.is_empty() {
            let id = self.pane.id();
            self.notify(format!("{id} respawned{left}"), true);
        }
        self.report(FromPane::Answer {
            ask: new.ask,
            answer: Answered::Acted(started),
        });
        if let Some(ended) = exited {
            self.exited(ended, now);
        }
        self.tell_schemes();
    }

    /// Hangs up a program; it is killed and reaped after `GRACE`.
    /// `closed`: fux closed it (`kill`), not its own end: the rest of its
    /// session goes too.
    fn end(&mut self, process: Process, closed: bool, now: Instant) {
        if let Process::Reading(mut child)
        | Process::HungUp(mut child)
        | Process::ReadFailed(mut child, _) = process
        {
            if closed {
                child.leader.closed();
            }
            child.leader.hang_up();
            let deadline = crate::after(now, GRACE);
            self.dying.push(Dying { child, deadline });
        }
    }

    /// It is closed, by fux if `closed`: its program, and one a respawn
    /// under way started, are ended.
    fn close(&mut self, closed: bool, now: Instant) {
        if self.closed.is_some() {
            return;
        }
        self.closed = Some(closed);
        let process = self.pane_mut().let_go();
        self.end(
            process.map_or(Process::Absent, Process::Reading),
            closed,
            now,
        );
        if let Some(child) = self.leftover.take() {
            self.end(Process::Reading(child), closed, now);
        }
        // A respawn under way is abandoned, which its answer says.
        if let Some(abandoned) = self.respawning.take() {
            self.end(abandoned.process, true, now);
            if let Some(child) = abandoned.old {
                let deadline = abandoned.grace.unwrap_or(now);
                self.dying.push(Dying { child, deadline });
            }
            let id = self.pane.id();
            let refusal = Refusal {
                code: fux_wire::code::Code::Closed,
                message: format!(
                    "{id} closed while respawning: the program started for it was ended"
                ),
            };
            self.report(FromPane::Answer {
                ask: abandoned.ask,
                answer: Answered::Refused(refusal),
            });
        }
    }

    /// Its program exited with `ended`: a shell's pane closes, and a
    /// program's stays, the status kept, for its last screen to be read.
    /// What either wrote as it exited may still be on its terminal, or read
    /// and waiting for its next turns (`Pane::has_unread`): it is told
    /// exited once all of it is on its screen, from the turn that reads the
    /// last of it (`fire`).
    pub fn exited(&mut self, ended: Ended, now: Instant) {
        let generation = self.pane.respawns;
        let id = self.pane.id();
        let previous = std::mem::replace(&mut self.actor, Actor::Program(id, generation));
        self.program_exited(ended, now);
        self.actor = previous;
    }

    fn program_exited(&mut self, ended: Ended, now: Instant) {
        let pane = &self.pane;
        // Told once.
        if pane.exiting.is_none() && pane.process().exit_status().is_some() {
            return;
        }
        // Once, at the first word of the exit, a turn's worth of its
        // terminal read, and the status kept till what is read is on its
        // screen; at each, a frame it holds read, one begun in output read
        // since too. Its terminal is read on after its exit is told, a turn
        // at a time, till it is empty or closed (D-90).
        if pane.exiting.is_none() {
            let mut pane = self.pane_mut();
            pane.exiting(ended);
            pane.drain(true);
            self.read_left(false, now);
        }
        self.read_with(|p| p.release_frame(now));
        if self.pane.has_unread() {
            return;
        }
        let draining = self.pane.draining;
        let id = self.pane.id();
        let shown = self.shown;
        let mut pane = self.pane_mut();
        match pane.runs {
            // Its pane closes: the server closes it as it is told the exit.
            Runs::Shell(_) => {
                let (_, gone) = pane.exit(ended);
                self.end(gone, false, now);
            }
            Runs::Explicit(_) => {
                // Its leader, exited and unreaped, is reaped with the dying.
                let (end, gone) = pane.exit(ended);
                let errors: Vec<_> = match shown {
                    true => pane.take_terminal_errors().collect(),
                    false => Vec::new(),
                };
                if let Process::Reading(child)
                | Process::HungUp(child)
                | Process::ReadFailed(child, _) = gone
                {
                    match draining {
                        true => self.leftover = Some(child),
                        false => self.dying.push(Dying {
                            child,
                            deadline: now,
                        }),
                    }
                }
                let status = format!("{id} {}{}", answer::told(end.ended), end.left.told());
                let left = end.left != pane::Left::default() || !errors.is_empty();
                let told = std::iter::once(status)
                    .chain(errors)
                    .collect::<Vec<_>>()
                    .join("; ");
                self.notify(told, left);
            }
        }
    }

    /// Reads a turn's worth of what its program left on its terminal as it
    /// ends, or is respawned over (`Replacement::old`); the rest on its
    /// next turns (`fire`), till its terminal is empty or closed. One that
    /// goes on writing there is read for as long as it does, a turn at a
    /// time. `empty_ends`: whether a turn that finds it empty ends that;
    /// the read at an exit's first word does not, as the pane's own read
    /// may have just emptied it.
    fn read_left(&mut self, empty_ends: bool, now: Instant) {
        self.read_left_with(empty_ends, |fd, bytes| fuxix::io::read(fd, bytes), now);
    }

    /// `read_left`, reading its terminal with `read`. A read that fails is
    /// told, from the program whose terminal it is, the one respawned over
    /// or that left a writer too; only its own program stops being read for
    /// it.
    pub(crate) fn read_left_with(
        &mut self,
        empty_ends: bool,
        mut read: impl FnMut(&fuxix::pty::Master, &mut [u8]) -> Result<usize, fuxix::Errno>,
        now: Instant,
    ) {
        if !self.pane.draining {
            return;
        }
        // A program respawned over that outlives its grace is killed; what
        // it left on its terminal is read on.
        if let Some(r) = &mut self.respawning
            && r.grace.is_some_and(|grace| grace <= now)
        {
            r.grace = None;
            if let Some(old) = &r.old {
                old.leader.kill();
            }
        }
        let old = self.respawning.as_ref().and_then(|r| r.old.as_ref());
        let old = old.or(self.leftover.as_ref());
        let own = old.is_none();
        let child = old.or_else(|| self.pane.process().child());
        let (left, drain) = child.map_or((Vec::new(), Drain::Closed), |child| {
            left_turn(|bytes| read(&child.master, bytes))
        });
        if !left.is_empty() {
            self.output(&left, now);
        }
        match drain {
            Drain::More => return,
            Drain::Empty if !empty_ends => return,
            Drain::Empty | Drain::Closed => {}
            Drain::Failed(error) => {
                let mut pane = self.pane_mut();
                match own {
                    true => pane.read_failed(error),
                    false => pane.tell_read_failed(error),
                }
            }
        }
        self.pane_mut().drain(false);
        if let Some(r) = &mut self.respawning
            && let Some(child) = r.old.take()
        {
            let deadline = r.grace.unwrap_or(now);
            self.dying.push(Dying { child, deadline });
        }
        if let Some(child) = self.leftover.take() {
            self.dying.push(Dying {
                child,
                deadline: now,
            });
        }
    }

    /// Output from its program.
    pub fn output(&mut self, bytes: &[u8], now: Instant) {
        self.changed();
        self.read_with(|pane| pane.output(bytes, now));
    }

    /// Reading its program's terminal failed so.
    pub fn read_failed(&mut self, error: fuxix::Errno) {
        self.read_with(|pane| pane.read_failed(error));
    }

    /// Its program closed its terminal: it is read no more, unless its exit
    /// is being told (the rest of its output still to read): then it
    /// exited, and did not hang up.
    pub fn hung_up(&mut self, now: Instant) {
        self.reap(now);
        if self.pane.exiting.is_none() {
            self.pane_mut().stop_reading();
        }
    }

    /// Reads into it with `read`, as every read of its output does: after,
    /// its bell rung and notification told, its copies made in order, and
    /// what it refused told.
    fn read_with(&mut self, read: impl FnOnce(&mut PaneMut<'_>)) {
        self.changed();
        let retold = {
            let graphics = fux_vt::Graphics::from(self.hosting.draws);
            let (colours, palette) = (self.hosting.colours(), self.hosting.palette());
            let mut pane = self.pane_mut();
            let retold = pane.host(colours, palette, graphics);
            read(&mut pane);
            retold
        };
        let mut pane = self.pane_mut();
        // Sized once for the new cell and what was read, a resize sized so
        // already, which the system does not signal again.
        if retold {
            pane.size_terminal();
        }
        let (bell, notified, copies) = pane.take_read();
        if bell || notified.is_some() {
            self.report(FromPane::Alert { bell, notified });
        }
        for text in copies {
            self.report(FromPane::Copied(text));
        }
        self.tell_refused();
    }

    /// Tells the clients showing it what it let go of and has not told,
    /// each kind with how many (`Pane::take_untold`); with no client showing
    /// it, it is kept for one that does.
    fn tell_refused(&mut self) {
        if !self.shown || !(self.pane.loses() || self.pane.has_terminal_errors()) {
            return;
        }
        let id = self.pane.id();
        let mut pane = self.pane_mut();
        let mut notices = pane.take_untold().told(id, true);
        // An exit reports terminal failures with its final status, after
        // its remaining output and frame have been read.
        if pane.exiting.is_none() {
            notices.extend(pane.take_terminal_errors());
        }
        if !notices.is_empty() {
            self.notify(notices.join("; "), true);
        }
    }

    fn notify(&mut self, text: String, error: bool) {
        self.report(FromPane::Notice { text, error });
    }

    /// When it next has something to do by itself, if ever: a frame read,
    /// output waiting read on, a closed program killed.
    pub fn timers(&self) -> impl Iterator<Item = (Instant, Timer)> + use<> {
        let respawn = self.respawning.is_some();
        let frame = (self.pane.frame_deadline().filter(|_| !respawn)).map(|at| (at, Timer::Frame));
        let unread = (self.pane.unread_due()).or(self.respawning.as_ref().map(|r| r.since));
        let unread = unread.map(|at| (at, Timer::Unread));
        let grace = (self.dying.iter().map(|d| d.deadline).min()).map(|at| (at, Timer::Grace));
        [frame, unread, grace].into_iter().flatten()
    }

    /// The soonest of them.
    pub fn timer(&self) -> Option<(Instant, Timer)> {
        self.timers().min_by_key(|(at, _)| *at)
    }

    /// What `timer` waited for is done, now that it is due.
    pub fn fire(&mut self, timer: Timer, now: Instant) {
        self.changed();
        match timer {
            Timer::Frame => self.read_with(|p| p.release_frame(now)),
            Timer::Unread => {
                // A respawn under way reads a frame its old program holds.
                let frame = self.respawning.is_some() && self.pane.frame_deadline().is_some();
                match self.pane.draining && !self.pane.has_unread() {
                    true => self.read_left(true, now),
                    false => self.read_with(|p| match frame {
                        true => p.release_frame(now),
                        false => p.read_unread(now),
                    }),
                }
                self.after_turn(now);
            }
            Timer::Grace => {
                let (due, dying) = std::mem::take(&mut self.dying)
                    .into_iter()
                    .partition(|d| d.deadline <= now);
                self.dying = dying;
                self.killed.extend(due.into_iter().filter_map(Dying::kill));
            }
        }
    }

    /// Checks its program's leader, and those killed, since signals
    /// coalesce, and reaps the orphans it adopted, every child that is no
    /// leader of its own.
    pub fn reap(&mut self, now: Instant) {
        self.changed();
        self.reap_killed();
        let leaders: Vec<Pid> = (self.pane.process().child().into_iter())
            .chain(
                (self.respawning.iter()).flat_map(|r| r.process.child().into_iter().chain(&r.old)),
            )
            .chain(&self.leftover)
            .chain(self.dying.iter().map(|d| &d.child))
            .map(|child| child.leader.pid())
            .chain(self.killed.iter().map(Job::leader))
            .collect();
        fuxix::process::reap_orphans(|pid| leaders.contains(&pid));
        if let Some(status) = (self.pane.process().child()).and_then(|c| c.leader.exited()) {
            self.exited(status, now);
        }
    }

    /// Reaps each program killed that has exited.
    fn reap_killed(&mut self) {
        let killed = std::mem::take(&mut self.killed).into_iter();
        self.killed = killed.filter_map(process::reaped).collect();
    }

    /// Kills every program it has left, and waits for them, at most a
    /// second: a process stuck in the kernel is left to init rather than
    /// hold its pane's end.
    pub fn finish(&mut self) {
        let dying = std::mem::take(&mut self.dying);
        self.killed
            .extend(dying.into_iter().filter_map(Dying::kill));
        for _ in 0..500 {
            self.reap_killed();
            if self.killed.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// Writes the waiting input, as far as its program's terminal takes
    /// it: whether it took any, which makes room for a scheme report.
    pub fn write_input(&mut self) -> bool {
        let wrote = self.pane_mut().write_input();
        // What its terminal did not take waits for it to take more.
        self.input = !self.pane.input().is_empty();
        if wrote {
            self.tell_schemes();
        }
        wrote
    }

    // ------------------------------------------------------------- the host

    /// Its host's terminal said this: its program's colour queries are
    /// answered so from now on, a new cell sizes its terminal, and a scheme
    /// it was not told is told it.
    fn host(&mut self, hosting: Hosting) {
        self.hosting = hosting;
        self.read_with(|_| {});
        self.tell_schemes();
    }

    /// Its program is told its host's scheme, if it takes scheme reports
    /// (mode 2031) and input, and was told another or none.
    fn tell_schemes(&mut self) {
        let Some(scheme) = self.hosting.scheme else {
            return;
        };
        let pane = &self.pane;
        if pane.screen().mode(fux_vt::Mode::ColorSchemeUpdates)
            && pane.scheme_told != Some(scheme)
            && refuses_input(pane).is_none()
        {
            self.pane_mut().offer_scheme(scheme);
        }
    }

    // ------------------------------------------------------------ requests

    /// Keys, a paste, a focus change or a mouse report a client typed into
    /// it, as its program asked for them; told to `client` if refused.
    fn typed(&mut self, client: ClientId, typed: Typed) {
        let pane = &self.pane;
        if let Some(refused) = refuses_input(pane) {
            // A focus change for a program that takes no input is no news.
            if !matches!(typed, Typed::Focus(_)) {
                let why = refused.to_string();
                self.report(FromPane::Refused { client, why });
            }
            return;
        }
        let screen = pane.screen();
        let mut bytes = Vec::new();
        let mut retired = None;
        match typed {
            Typed::Key(stroke) => {
                fux_vt::keys::encode::key_bytes(stroke, screen.key_mode(), &mut bytes);
            }
            Typed::Paste(text) => {
                let bracketed = screen.mode(fux_vt::Mode::BracketedPaste);
                fux_vt::keys::encode::paste(&text, bracketed, &mut bytes);
            }
            Typed::Body(token) => {
                let Some(inputs) = self.bodies.remove(&token) else {
                    self.awaiting.insert(token, Awaiting::Paste { client });
                    return;
                };
                retired = Some(token);
                let bracketed = screen.mode(fux_vt::Mode::BracketedPaste);
                for input in inputs {
                    if let Input::Paste(text) = input {
                        fux_vt::keys::encode::paste(&text, bracketed, &mut bytes);
                    }
                }
            }
            Typed::Focus(gained) => {
                bytes.extend_from_slice(screen.focus_report(gained).unwrap_or_default());
            }
            Typed::Mouse(event) => {
                let lines = pane.options().alternate_scroll_lines;
                if let Some(reports) = screen.encode_mouse(event, lines) {
                    bytes.extend(reports.flatten());
                }
            }
        }
        if let Some(token) = retired {
            self.report(FromPane::Retired(token));
        }
        if !bytes.is_empty() {
            self.pane_mut()
                .push_with(|_, out| out.extend_from_slice(&bytes));
        }
    }

    /// Does what it is asked: its answer, or none yet (a respawn's waits
    /// for it).
    fn ask(&mut self, ask: u64, what: Ask, now: Instant) -> Option<Answered> {
        let done = match what {
            Ask::Keys {
                inputs,
                seen,
                body: None,
            } => self.send_keys(&inputs, seen),
            Ask::Keys {
                seen,
                body: Some(token),
                ..
            } => match self.bodies.remove(&token) {
                Some(inputs) => {
                    self.report(FromPane::Retired(token));
                    self.send_keys(&inputs, seen)
                }
                None => {
                    let actor = self.actor;
                    self.awaiting
                        .insert(token, Awaiting::Keys { ask, actor, seen });
                    return None;
                }
            },
            Ask::Mouse { mouse, row, col } => self.send_mouse(mouse, row, col),
            Ask::Signal(signal) => self.signal(signal),
            Ask::Respawn { kill, argv } => match self.respawn(ask, kill, argv, now) {
                Ok(()) => return None,
                Err(error) => Err(error),
            },
            Ask::ClearHistory => {
                self.pane_mut().clear_history();
                Ok(Answered::Done)
            }
            Ask::HistoryBytes(bytes) => {
                self.pane_mut().set_history_bytes(bytes);
                Ok(Answered::Done)
            }
            Ask::Capture {
                history,
                view,
                tab,
                hold: false,
            } => self.capture(history, view, tab),
            Ask::Capture {
                history,
                view,
                tab,
                hold: true,
            } => self.capture(history, view, tab).map(|answer| match answer {
                Answered::Captured(captured) => {
                    let counts = match &captured.of {
                        fux_wire::answer::Of::Pane { pane, .. } => pane.version.counts,
                        fux_wire::answer::Of::Client { .. } => self.pane.counts(),
                    };
                    self.held.insert(ask, captured);
                    Answered::Held {
                        token: ask,
                        counts,
                        cursor: String::new(),
                    }
                }
                other @ (Answered::Acted(_)
                | Answered::Done
                | Answered::Held { .. }
                | Answered::Refused(_)) => other,
            }),
        };
        Some(done.unwrap_or_else(|error| Answered::Refused(Refusal::from(error))))
    }

    /// Its version now.
    fn version(&self) -> Version {
        self.pane.version(self.run)
    }

    /// Sends `inputs` to its program, each encoded as it asked for keys
    /// (`bytes`): if `seen`, only if it is still at that version as they
    /// are queued, else none, failing `stale`.
    fn send_keys(&mut self, inputs: &[Input], seen: Option<Version>) -> Result<Answered, Error> {
        let now = self.version();
        if let Some(seen) = seen {
            if !self.pane.counts().had(seen.counts) {
                let why = format!(
                    "{seen}: {} never had these counts; it is at {}",
                    seen.pane, now.counts
                );
                return Err(fail(Raise::BadValue, why));
            }
            if now != seen {
                let why = format!("{} changed since {seen}: it is at {now}", seen.pane);
                return Err(fail(Raise::Stale, why));
            }
        }
        if let Some(refused) = refuses_input(&self.pane) {
            return Err(refused);
        }
        let bytes = bytes(&self.pane, inputs);
        let kind = match inputs.iter().any(|input| matches!(input, Input::Paste(_))) {
            true => Sent::Paste,
            false => Sent::Keys,
        };
        self.pane_mut().send(&bytes, kind, seen);
        Ok(Answered::Acted(now))
    }

    /// `send-mouse`: `mouse` at `row` and `col`, encoded as its program
    /// asked for mouse reports.
    fn send_mouse(&mut self, mouse: Mouse, row: u16, col: u16) -> Result<Answered, Error> {
        use fux_vt::MouseProtocolMode as Asked;
        use fux_vt::keys::mouse::MouseAction::{Motion, Press, Release};
        if let Some(refused) = refuses_input(&self.pane) {
            return Err(refused);
        }
        let run = self.run;
        let mut p = self.pane_mut();
        let size = p.size();
        if row >= size.rows() || col >= size.cols() {
            let (rows, cols) = (size.rows(), size.cols());
            let message = format!(
                "{} is {cols}x{rows}: rows count 0-{} and columns 0-{}",
                p.id(),
                rows.saturating_sub(1),
                cols.saturating_sub(1)
            );
            return Err(fail(Raise::Mouse, message));
        }
        let event = |action| fux_vt::keys::mouse::MouseEvent {
            action,
            mods: fux_vt::keys::Modifiers::default(),
            row,
            col,
        };
        // A click is a press and, where the program takes them (not in X10
        // mode), a release; a move while a button is held is a drag.
        // Released, or clicked, it is held no more; another button's release
        // leaves it held. X10 reports presses alone: a click is its press,
        // and a release or the wheel, which it cannot say, is refused.
        let unheld = |button| p.mouse_held.filter(|held| *held != button);
        let (events, held, beyond_x10) = match mouse {
            Mouse::Click(button) => {
                let button = button.reported();
                let events = vec![event(Press(button)), event(Release(Some(button)))];
                (events, unheld(button), false)
            }
            Mouse::Press(button) => {
                let button = button.reported();
                (vec![event(Press(button))], Some(button), false)
            }
            Mouse::Wheel(way) => (vec![event(Press(way.reported()))], p.mouse_held, true),
            Mouse::Release(button) => {
                let button = button.reported();
                (vec![event(Release(Some(button)))], unheld(button), true)
            }
            Mouse::Move => (vec![event(Motion(p.mouse_held))], p.mouse_held, false),
        };
        let mode = p.screen().mouse_protocol_mode();
        let x10 = mode == Asked::Press;
        let pane = p.id();
        if x10 && beyond_x10 {
            let message = format!(
                "the program in {pane} asked for X10 mouse reports: presses alone, no releases or wheel"
            );
            return Err(fail(Raise::Mouse, message));
        }
        let mut report = Vec::new();
        for event in events {
            let release = matches!(event.action, Release(_));
            if let Some(chunks) = p
                .screen()
                .encode_mouse(event, p.options().alternate_scroll_lines)
            {
                report.extend(chunks.flatten());
                continue;
            }
            if release && x10 {
                continue;
            }
            let message = match (mode, event.action) {
                (Asked::None, _) => {
                    format!("the program in {pane} has not asked for mouse reports")
                }
                (_, Motion(_)) => format!(
                    "the program in {pane} takes no motion here: with no button pressed it asked for none, \
                     and asking for clicks alone (mode 1000) it takes no drags either"
                ),
                (_, Press(_) | Release(_)) => format!(
                    "the mouse encoding the program in {pane} asked for cannot say row {row}, column {col}"
                ),
            };
            return Err(fail(Raise::Mouse, message));
        }
        let version = p.version(run);
        p.send_mouse(&report, held);
        Ok(Answered::Acted(version))
    }

    /// `signal` sent to the process group that runs in its foreground: the
    /// terminal's foreground group (a job its shell runs), or the program
    /// it was started with; never the shell of a pane that runs one.
    fn signal(&mut self, signal: fuxix::process::Signal) -> Result<Answered, Error> {
        let pane = &self.pane;
        let target = pane.id();
        if let Process::Respawning = pane.process() {
            let message = format!("{target} is respawning: no program is its own till it is done");
            return Err(fail(Raise::StillRunning, message));
        }
        if let Some(status) = pane.process().exit_status() {
            return Err(ended(target, status));
        }
        let program = match (&pane.runs, pane.process().child()) {
            (Runs::Explicit(_), Some(child)) => Some(child.leader.pid()),
            _ => None,
        };
        let job = pane.process().job().map_err(|errno| {
            let why =
                format!("{target}: its terminal does not say what runs in its foreground: {errno}");
            fail(Raise::Signal, why)
        })?;
        let group = job.or(program).ok_or_else(|| {
            let message = format!("nothing is running in {target} but its shell");
            fail(Raise::OnlyShell, message)
        })?;
        let version = self.version();
        fuxix::process::kill_group(group, signal)
            .map_err(|e| fail(Raise::Signal, format!("sending the signal: {e}")))?;
        self.pane_mut().tell_signalled(signal);
        Ok(Answered::Acted(version))
    }

    /// A capture of its screen, `history` rows before it, as `view` asks:
    /// its observation's place, name and label, and the journal's cursor,
    /// left for the server to give it (`Session::captured`).
    fn capture(
        &mut self,
        history: Option<usize>,
        view: command::CaptureView,
        tab: fux_wire::id::TabId,
    ) -> Result<Answered, Error> {
        let pane = &self.pane;
        let target = pane.id();
        if let Some(since) = view.since {
            if since.run != self.run || since.pane != target {
                let why = format!("{since} is not a version of {target} in this run");
                return Err(fail(Raise::BadValue, why));
            }
            if !pane.counts().had(since.counts) {
                let why = format!(
                    "{since}: {target} never had these counts; it is at {}",
                    pane.counts()
                );
                return Err(fail(Raise::BadValue, why));
            }
        }
        if view.source == command::Source::LastOutput && !pane.marked() {
            return Err(fail(
                Raise::NoMarks,
                format!("{target}: {}", pane::UNMARKED),
            ));
        }
        let observed = self.observed(tab)?;
        let cursor = fux_wire::events::Cursor {
            seq: fux_wire::events::Seq(0),
            run: self.run,
        };
        let captured = crate::capture::pane(observed, pane, cursor, history, view)?;
        Ok(Answered::Captured(Box::new(captured)))
    }

    /// Its own facts, as a capture holds them: its place, name and label
    /// are the server's.
    fn observed(&self, tab: fux_wire::id::TabId) -> Result<fux_wire::inspect::Pane, Error> {
        let pane = &self.pane;
        Ok(fux_wire::inspect::Pane {
            id: pane.id(),
            tab,
            version: self.version(),
            name: None,
            label: String::new(),
            title: pane.title().as_str().to_owned(),
            rows: pane.size().rows(),
            cols: pane.size().cols(),
            row: None,
            col: None,
            facts: Some(self.facts()?),
            cwd: None,
        })
    }

    /// Its program's, launch's and terminal's facts now.
    fn facts(&self) -> Result<fux_wire::inspect::Facts, Error> {
        let pane = &self.pane;
        let terminal = fux_wire::inspect::Terminal::ALL.iter().copied();
        Ok(fux_wire::inspect::Facts {
            program: crate::capture::program(pane)?,
            launch: crate::capture::launch(pane),
            terminal: terminal
                .map(|t| (t, crate::capture::terminal(t, pane)))
                .collect(),
        })
    }

    // -------------------------------------------------------------- screen

    /// Its server is gone: what it did not tell is told to no one, and the
    /// next server it is linked to is sent its screen whole.
    fn forget(&mut self) {
        self.told.clear();
        self.out.clear();
        // What its last server's askers wanted is no one's now.
        self.bodies.clear();
        self.awaiting.clear();
        self.held.clear();
        self.sent_facts = None;
        self.changed();
    }

    /// What it says of itself as a server takes it back.
    fn greeting(&self) -> crate::link::Greeting {
        let pane = &self.pane;
        let launch = pane.runs.launch();
        let (ended, hung_up, read_error) = match pane.process() {
            Process::Exited(ended) => (Some(*ended), false, None),
            Process::HungUp(_) => (None, true, None),
            Process::ReadFailed(_, error) => (None, false, Some(error.to_string())),
            Process::Absent | Process::Reading(_) | Process::Respawning => (None, false, None),
        };
        crate::link::Greeting {
            protocol: crate::link::PANE_PROTOCOL,
            id: pane.id(),
            pid: i32::try_from(std::process::id()).unwrap_or(0),
            run: self.run,
            respawns: pane.respawns,
            shell: matches!(pane.runs, Runs::Shell(_)),
            argv: launch.argv.clone(),
            cwd: launch.cwd.clone(),
            options: pane.options(),
            size: pane.size(),
            title: pane.title().clone(),
            counts: pane.counts(),
            ended,
            hung_up,
            read_error,
        }
    }

    /// Body `token` came from its asker's connection: what waits for it
    /// goes on, else it waits for that.
    pub fn body(&mut self, token: u64, inputs: Vec<Input>) {
        match self.awaiting.remove(&token) {
            Some(Awaiting::Keys { ask, actor, seen }) => {
                self.report(FromPane::Retired(token));
                let previous = std::mem::replace(&mut self.actor, actor);
                let answer = self
                    .send_keys(&inputs, seen)
                    .unwrap_or_else(|error| Answered::Refused(Refusal::from(error)));
                self.report(FromPane::Answer { ask, answer });
                self.actor = previous;
            }
            Some(Awaiting::Paste { client }) => {
                self.bodies.insert(token, inputs);
                let previous = std::mem::replace(&mut self.actor, Actor::Client(client));
                self.typed(client, Typed::Body(token));
                self.actor = previous;
            }
            None => {
                self.bodies.insert(token, inputs);
            }
        }
    }

    /// The capture held as `token`, taken by its asker's connection.
    pub fn fetch(&mut self, token: u64) -> Option<Box<fux_wire::answer::Captured>> {
        let held = self.held.remove(&token);
        if held.is_some() {
            self.report(FromPane::Retired(token));
        }
        held
    }

    /// Its facts are looked at again: what changes with no event of its own
    /// (its foreground group, as its shell starts a job) is told too.
    pub fn look_again(&mut self) {
        self.dirty = true;
    }

    /// Something happened to it: its server is to be told its facts, and
    /// the connections watching it its screen, as they changed.
    fn changed(&mut self) {
        self.dirty = true;
        self.revision = self.revision.wrapping_add(1);
    }

    /// Its facts as its server is to have them, if they changed since it
    /// was last sent them. Part way through a frame, none: a frame is told
    /// whole.
    pub fn update(&mut self) -> Option<Update> {
        if !self.dirty || self.pane.mid_frame() {
            return None;
        }
        self.dirty = false;
        let pane = &self.pane;
        let facts = match self.facts() {
            Ok(facts) => facts,
            Err(error) => {
                log!("{}: its facts: {error}", pane.id());
                return None;
            }
        };
        let now = (facts, pane.title().clone(), pane.counts(), pane.size());
        if self.sent_facts.as_ref() == Some(&now) {
            return None;
        }
        let pid = (pane.process().child()).map(|c| c.leader.pid().as_raw());
        let update = Update {
            counts: now.2,
            title: now.1.clone(),
            size: now.3,
            facts: now.0.clone(),
            pid,
        };
        self.sent_facts = Some(now);
        Some(update)
    }

    /// Its screen as `watch`'s connection is to have it, if it changed
    /// since it was last sent it: the bytes that paint it onto the copy the
    /// connection keeps, whole the first time, after a respawn, or as its
    /// pictures or colours change. Part way through a frame, none: a frame
    /// is sent whole.
    pub fn drawn(&self, watch: &mut Watch) -> Option<crate::link::Drawn> {
        let fresh = watch.revision == Some(self.revision) && watch.whole == self.whole;
        if fresh || self.pane.mid_frame() {
            return None;
        }
        let pane = &self.pane;
        let mut grid = std::mem::take(&mut watch.spare);
        if let Err(error) =
            crate::render::pane_grid(pane.id(), pane.respawns, pane.screen(), &mut grid)
        {
            log!("{}: composing its screen: {error}", pane.id());
            return None;
        }
        let sent = (watch.sent.as_ref()).filter(|sent| {
            watch.whole == self.whole && (sent.rows, sent.cols) == (grid.rows, grid.cols)
        });
        let mut screen = Vec::new();
        let blank = Grid::default();
        crate::render::paint_into(sent.unwrap_or(&blank), sent.is_some(), &grid, &mut screen);
        // Its pictures go whole with its text when they change: placed
        // after it, on cells its text then leaves as they are.
        let graphics = pane.screen().graphics();
        let cell = graphics.cell().map_or((0, 0), |c| (c.width(), c.height()));
        let area = fux_wire::layout::Rect::screen(grid.rows, grid.cols);
        let pictures = crate::render::placed(pane.id(), area, pane.screen(), cell);
        let own = crate::render::Own::of(pane.screen());
        let full = sent.is_none() || pictures != watch.pictures || own != watch.own;
        if full && sent.is_some() {
            screen.clear();
            crate::render::paint_into(&blank, false, &grid, &mut screen);
        }
        if full {
            own.put(&mut screen);
            let mut sixels = crate::render::Sixels::default();
            for picture in &pictures {
                sixels.picture_into(picture, &mut screen);
            }
        }
        watch.revision = Some(self.revision);
        watch.whole = self.whole;
        if screen.is_empty() {
            watch.spare = grid;
            return None;
        }
        watch.spare = watch.sent.replace(grid).unwrap_or_default();
        watch.pictures = pictures;
        watch.own = own;
        Some(crate::link::Drawn {
            size: pane.size(),
            screen,
            full,
            draws: graphics.into(),
            picture_bytes: pane.options().picture_bytes.get(),
        })
    }
}

/// What the tests and the tools look at, and do, in a host held in their
/// process (`Session::host_mut`).
#[cfg(any(test, feature = "in-process"))]
impl Host {
    /// Whether a respawn is under way.
    pub fn respawning(&self) -> bool {
        self.respawning.is_some()
    }
    /// Whether a program whose exit is told is still read.
    pub fn leftover(&self) -> bool {
        self.leftover.is_some()
    }
    /// How many programs it waits to end.
    pub fn dying(&self) -> usize {
        self.dying.len()
    }
    /// Kills and reaps every program it waits to end, at once.
    pub fn end_dying(&mut self) {
        for dying in std::mem::take(&mut self.dying) {
            let _ = dying.kill().and_then(process::reaped);
        }
    }
    /// Gives it `process`, as a test's pane runs a child of the test's.
    #[cfg(test)]
    pub(crate) fn set_process(&mut self, process: Process) -> Process {
        self.pane_mut().set_process(process)
    }
    /// Starts real programs from now on, told `socket`, or none.
    #[cfg(test)]
    pub(crate) fn starts(&mut self, socket: Option<PathBuf>) {
        self.socket = socket;
    }
    /// The program a respawn under way started, and its fresh pane's
    /// history budget.
    #[cfg(test)]
    pub(crate) fn replacement(&self) -> Option<(Option<&Child>, usize)> {
        let r = self.respawning.as_ref()?;
        Some((r.process.child(), r.fresh.history_bytes()))
    }
    /// The program a respawn under way respawns over is `old`.
    #[cfg(test)]
    pub(crate) fn respawned_over(&mut self, old: Child) {
        if let Some(r) = &mut self.respawning {
            r.old = Some(old);
        }
    }
    /// A respawn under way over `old`, into `fresh`, its grace over: as
    /// one over a program that ignores the hangup.
    #[cfg(test)]
    pub(crate) fn respawning_over(&mut self, fresh: Pane, old: Child, now: Instant) {
        if let Process::Reading(child) = self.set_process(Process::Respawning) {
            self.dying.push(Dying {
                child,
                deadline: now,
            });
        }
        self.pane_mut().drain(true);
        self.respawning = Some(Replacement {
            ask: 0,
            actor: self.actor,
            fresh,
            process: Process::Absent,
            old: Some(old),
            grace: Some(now),
            since: now,
        });
    }
    /// The programs it started and has not reaped.
    #[cfg(test)]
    pub(crate) fn children(&self) -> Vec<Pid> {
        (self.pane.process().child().into_iter())
            .chain(
                (self.respawning.iter()).flat_map(|r| r.process.child().into_iter().chain(&r.old)),
            )
            .chain(&self.leftover)
            .chain(self.dying.iter().map(|d| &d.child))
            .map(|child| child.leader.pid())
            .collect()
    }
}

/// What a pane's programs are told of their server: their pane, its
/// socket, its run and the fux running it.
struct Env<'a> {
    pane: PaneId,
    run: Run,
    socket: Option<&'a std::path::Path>,
    exe: Option<&'a std::path::Path>,
}

impl<'a> Env<'a> {
    fn of(
        pane: PaneId,
        run: Run,
        socket: Option<&'a std::path::Path>,
        exe: Option<&'a std::path::Path>,
    ) -> Self {
        Env {
            pane,
            run,
            socket,
            exe,
        }
    }
}

/// Starts `launch` as a pane's program, generation `generation`, at `size`
/// in cells of `cell`, if it starts real ones: told of its server
/// (`FUX_PANE`, `FUX_SOCKET`, `FUX_SERVER`, and `FUX`, the fux running it,
/// unset if it cannot be had).
fn spawn(
    told: &Env<'_>,
    generation: u64,
    launch: &pane::Launch,
    size: (Size, Option<fux_vt::CellSize>),
) -> Result<Option<Child>, Error> {
    let Some(socket) = told.socket else {
        return Ok(None);
    };
    let mut vars: Vec<(&str, std::ffi::OsString)> = vec![
        ("FUX_PANE", format!("{}:{generation}", told.pane).into()),
        ("FUX_SOCKET", socket.as_os_str().to_owned()),
        ("FUX_SERVER", told.run.to_string().into()),
    ];
    vars.extend((told.exe).map(|exe| ("FUX", exe.as_os_str().to_owned())));
    // No `FUX` to give: not one the server was started with either.
    let unset: &[&str] = match told.exe {
        Some(_) => &[],
        None => &["FUX"],
    };
    let child = process::spawn(&launch.argv.argv(), &launch.cwd, (&vars, unset), size)
        .map_err(|m| fail(Raise::Start, m))?;
    Ok(Some(child))
}

/// Why `pane`'s program takes no input, if it takes none: the one check
/// of input typed or sent, of the reports (mouse, focus) a terminal is
/// asked to make for it, and of those fux queues it unasked (in-band
/// resize, colour scheme), so that none waits for a program respawned
/// there.
pub(crate) fn refuses_input(pane: &Pane) -> Option<Error> {
    let why = match pane.process() {
        Process::HungUp(_) => "its program closed its terminal: nothing reads input there",
        Process::ReadFailed(_, error) => {
            return Some(fail(
                Raise::NotRunning,
                format!("{}: reading program terminal failed: {error}", pane.id()),
            ));
        }
        Process::Respawning => {
            "it is respawning: no program of its own reads input till it is done"
        }
        Process::Exited(status) => return Some(ended(pane.id(), *status)),
        Process::Absent | Process::Reading(_) => return None,
    };
    Some(fail(Raise::NotRunning, format!("{}: {why}", pane.id())))
}

/// `pane`'s program has ended, with `status`: it takes no input.
pub(crate) fn ended(pane: PaneId, status: Ended) -> Error {
    let was = match status {
        Ended::Exited(_) | Ended::Lost => "",
        Ended::Signalled(_) => "was ",
    };
    let told = format!("the program in {pane} {was}{}", answer::told(status));
    fail(Raise::NotRunning, told)
}

/// `inputs` as `p`'s program takes them: keys and pastes encoded as it
/// asked for them, text and bytes as they are.
fn bytes(p: &Pane, inputs: &[Input]) -> Vec<u8> {
    let screen = p.screen();
    let mut bytes = Vec::new();
    for input in inputs {
        match input {
            Input::Key(key) => {
                fux_vt::keys::encode::key_bytes((*key).into(), screen.key_mode(), &mut bytes)
            }
            Input::Text(text) => bytes.extend_from_slice(text.as_bytes()),
            Input::Paste(text) => {
                let bracketed = screen.mode(fux_vt::Mode::BracketedPaste);
                fux_vt::keys::encode::paste(text, bracketed, &mut bytes);
            }
            Input::Bytes(given) => bytes.extend_from_slice(given),
        }
    }
    bytes
}

/// How a turn's read of what a program left went (`left_turn`).
#[derive(Debug, PartialEq, Eq)]
enum Drain {
    /// Some read: there may be more, which the next turn reads. A read
    /// that finds nothing just after one that found some is no empty
    /// terminal: a terminal is handed on a piece at a time.
    More,
    /// Nothing to read as the turn began.
    Empty,
    /// The terminal is closed.
    Closed,
    Failed(fuxix::Errno),
}

/// A turn's worth of what a terminal holds (`pane::SLICE`), read with
/// `read`, and whether it may hold more.
fn left_turn(mut read: impl FnMut(&mut [u8]) -> Result<usize, fuxix::Errno>) -> (Vec<u8>, Drain) {
    let mut left = Vec::new();
    let mut buffer = [0u8; pane::SLICE];
    loop {
        let room = pane::SLICE.saturating_sub(left.len());
        let Some(bytes) = buffer.get_mut(..room).filter(|b| !b.is_empty()) else {
            return (left, Drain::More);
        };
        match read(bytes) {
            Ok(n) if n > 0 => left.extend_from_slice(bytes.get(..n).unwrap_or_default()),
            Err(fuxix::Errno::INTR) => {}
            Err(fuxix::Errno::AGAIN) if !left.is_empty() => return (left, Drain::More),
            Err(fuxix::Errno::AGAIN) => return (left, Drain::Empty),
            Ok(_) | Err(fuxix::Errno::IO) => return (left, Drain::Closed),
            Err(error) => return (left, Drain::Failed(error)),
        }
    }
}

/// The least time between two screens a pane sends while its program keeps
/// writing; as soon as it is quiet for `QUIET`, it sends one then. A
/// screen after a pause, a keystroke's echo, goes at once.
const PAINT: Duration = Duration::from_millis(16);
const QUIET: Duration = Duration::from_millis(1);
/// The most a pane reads from its program's terminal before it looks at
/// its link again.
const READ: usize = 64 * 1024;

/// When a pane's next screen may go (`PAINT`, `QUIET`).
struct Clock {
    /// `PAINT` after the last screen went.
    next: Instant,
    /// When its program last wrote.
    wrote: Option<Instant>,
}

impl Clock {
    /// When the next screen may go: at once, but `PAINT` after the last
    /// while the program writes, or once it is quiet if that is sooner.
    fn due(&self, now: Instant) -> Instant {
        self.wrote
            .map_or(now, |at| crate::after(at, QUIET).min(self.next))
    }
}

/// One that reached a pane's socket (`serve`), and what it is once its
/// first word says: a server of its run taking it back, or a connection's
/// process, watching its screen once it asks.
struct Peer {
    end: crate::link::End,
    side: Option<Option<Watch>>,
}

/// The loop of a pane's own process (`fux __pane`), listening at its
/// socket on `listener`, until it is closed and its program is gone: it
/// starts as the first word of its server, the first to reach it, says,
/// and reads its program's terminal, its link, its children's ends and
/// what reaches its socket: a connection's process showing its screen, or,
/// its server gone without closing it (a crash), a server of its run taking
/// it back (D-124), one at a time.
pub fn serve(listener: std::os::unix::net::UnixListener) -> Result<(), String> {
    use crate::link::{End, FromSide, Read, ToSide};
    use fuxix::poll::{Events, PollFd};
    if let Err(errno) = fuxix::process::adopt_orphans() {
        log!("adopting orphans: {errno}");
    }
    let children = fuxix::signal::Pipe::new(&[fuxix::process::Signal::Chld])
        .map_err(|e| format!("the signal pipe: {e}"))?;
    let socket = (listener.local_addr().ok())
        .and_then(|address| address.as_pathname().map(std::path::Path::to_owned));
    // Listened on again, by it: one that reaches it is told its pid, not
    // the server's that made the socket (`SO_PEERCRED`).
    fuxix::socket::listen(&listener, 16).map_err(|e| format!("its socket: {e}"))?;
    let (stream, _) = listener.accept().map_err(|e| format!("its server: {e}"))?;
    listener
        .set_nonblocking(true)
        .map_err(|e| format!("its socket: {e}"))?;
    let mut end = End::new(stream).map_err(|e| format!("its link: {e}"))?;
    // A server gone before its first word started nothing: it ends, and
    // the next server finds it gone.
    let first = end.wait::<ToPane>().map_err(|e| format!("its link: {e}"))?;
    let Some(ToPane::Begin(begin)) = first else {
        if let Some(socket) = &socket {
            let _ = std::fs::remove_file(socket);
        }
        return Err("its server's first word was not its start".to_owned());
    };
    let mut host = match Host::begin(*begin, Instant::now()) {
        Ok(host) => host,
        Err(why) => {
            end.send(&FromPane::Begun(Some(why)));
            let _ = end.flush_all();
            return Ok(());
        }
    };
    end.send(&FromPane::Begun(None));
    // Facts sent and not yet taken: its first, which its server waits for
    // as it starts it.
    let mut waiting = false;
    if let Some(first) = host.update() {
        end.send(&FromPane::Update(Box::new(first)));
        waiting = true;
    }
    let mut buffer = vec![0u8; READ];
    let mut clock = Clock {
        next: Instant::now(),
        wrote: None,
    };
    // Its server's link while it has one; and when it looks at its facts
    // again, a frame after it last sent them (`Host::look_again`).
    let mut link = Some(end);
    let mut again: Option<Instant> = None;
    let mut peers: Vec<Peer> = Vec::new();
    let mut poll = fuxix::poll::Poll::default();
    let mut socket = socket;
    loop {
        let now = Instant::now();
        // Closed, it waits for no server: none finds it, as it ends.
        if host.closed.is_some()
            && let Some(at) = socket.take()
        {
            let _ = std::fs::remove_file(at);
        }
        let due = clock.due(now) <= now;
        let mut sent = false;
        // What it tells its server, taken first: telling marks it changed.
        let mut reports = host.take();
        // Its screen goes first, to the connections that show it, so that a
        // keystroke's echo waits for no facts made for its server; but after
        // a copy, which reaches the clients through the server, it goes
        // after what it tells the server, so the copy comes no later.
        let copied = (reports.iter()).any(|r| matches!(r, FromPane::Copied(_)));
        for screen in [!copied, copied] {
            if screen {
                for peer in &mut peers {
                    if let Some(Some(watch)) = &mut peer.side
                        && !watch.waiting
                        && due
                        && let Some(drawn) = host.drawn(watch)
                    {
                        peer.end.send(&FromSide::Drawn(Box::new(drawn)));
                        (watch.waiting, sent) = (true, true);
                    }
                    let _ = peer.end.flush();
                }
                continue;
            }
            let Some(end) = &mut link else {
                // With no server, what it does is in its state alone.
                drop(std::mem::take(&mut reports));
                continue;
            };
            for report in std::mem::take(&mut reports) {
                end.send(&report);
            }
            if !waiting
                && due
                && let Some(update) = host.update()
            {
                end.send(&FromPane::Update(Box::new(update)));
                (waiting, sent) = (true, true);
                again = Some(crate::after(now, PAINT));
            }
            if end.flush().is_err() {
                (link, waiting) = (None, false);
                host.forget();
            }
        }
        if sent {
            clock = Clock {
                next: crate::after(now, PAINT),
                wrote: None,
            };
        }
        if host.finished() {
            host.finish();
            if let Some(socket) = &socket {
                let _ = std::fs::remove_file(socket);
            }
            return Ok(());
        }
        let timer = host.timer();
        let owed = (link.is_some() && !waiting)
            || (peers.iter()).any(|p| matches!(&p.side, Some(Some(w)) if !w.waiting));
        let paint = (owed && clock.wrote.is_some()).then(|| clock.due(now));
        let at = [timer.map(|(at, _)| at), paint, again]
            .into_iter()
            .flatten()
            .min();
        let timeout = at.map(|at| at.saturating_duration_since(now));
        let reading = match host.pane().process() {
            Process::Reading(child) => (!host.pane().has_unread()).then_some(child),
            Process::Absent
            | Process::HungUp(_)
            | Process::ReadFailed(..)
            | Process::Exited(_)
            | Process::Respawning => None,
        };
        let mut fds = poll.list();
        let writes = |out: bool| match out {
            true => Events::IN | Events::OUT,
            false => Events::IN,
        };
        let linked = (link.as_ref())
            .map(|end| fds.push(PollFd::new(&end.stream, writes(!end.out.is_empty()))));
        let reaped = fds.push(PollFd::new(children.reader(), Events::IN));
        let reached = fds.push(PollFd::new(&listener, Events::IN));
        let program = reading.map(|child| fds.push(PollFd::new(&child.master, writes(host.input))));
        let peered: Vec<_> = (peers.iter())
            .map(|peer| {
                fds.push(PollFd::new(
                    &peer.end.stream,
                    writes(!peer.end.out.is_empty()),
                ))
            })
            .collect();
        match fds.poll(timeout) {
            Ok(_) | Err(fuxix::Errno::INTR) => {}
            Err(error) => return Err(format!("poll: {error}")),
        }
        let ready = |entry: Option<_>| entry.map_or(Events::NONE, |entry| fds.events(entry));
        let (linked, reaped, reached) = (ready(linked), ready(Some(reaped)), ready(Some(reached)));
        let program = ready(program);
        let peered: Vec<Events> = peered.into_iter().map(|peer| ready(Some(peer))).collect();
        let now = Instant::now();
        if !linked.is_empty()
            && let Some(end) = &mut link
        {
            // What it said before it went is taken first: a close it sent
            // last is a close, not a crash.
            let mut closed = end.read(&mut buffer) == Read::Closed;
            loop {
                match end.next_message::<ToPane>() {
                    Ok(Some(ToPane::Taken)) => waiting = false,
                    Ok(Some(message)) => {
                        if matches!(message, ToPane::Typed { .. }) {
                            clock.next = now;
                        }
                        host.handle(message, now);
                    }
                    Ok(None) => break,
                    Err(error) => {
                        log!("{}: its server sent a bad frame: {error}", host.id());
                        closed = true;
                        break;
                    }
                }
            }
            if closed {
                (link, waiting) = (None, false);
                host.forget();
            }
        }
        if !reaped.is_empty() {
            while children.next().is_some() {}
            host.reap(now);
        }
        if !reached.is_empty() {
            while let Ok((stream, _)) = listener.accept() {
                let ours = fuxix::socket::peer(&stream).is_ok_and(|peer| peer.user.is_current());
                if ours
                    && host.closed.is_none()
                    && let Ok(end) = End::new(stream)
                {
                    peers.push(Peer { end, side: None });
                }
            }
        }
        if !program.is_empty() {
            if program.intersects(Events::OUT) {
                host.write_input();
            }
            if program.intersects(Events::IN | Events::HUP | Events::ERR) {
                read_program(&mut host, &mut buffer, &mut clock, now);
            }
        }
        // What reached it: each peer's words, in order, then its end.
        let mut kept = Vec::with_capacity(peers.len());
        // One reached this turn has no entry yet: it is kept, unread.
        let events = peered.into_iter().chain(std::iter::repeat(Events::NONE));
        for (mut peer, events) in std::mem::take(&mut peers).into_iter().zip(events) {
            if events.is_empty() {
                kept.push(peer);
                continue;
            }
            // What it said before it went is taken first, all of it: a body
            // sent last is a body.
            let mut closed = peer.end.read(&mut buffer) == Read::Closed;
            let (mut taken, mut done) = (false, false);
            loop {
                match &mut peer.side {
                    // Its first word.
                    None => match peer.end.next_message::<ToPane>() {
                        Ok(Some(ToPane::Take { run }))
                            if run == host.run && link.is_none() && host.closed.is_none() =>
                        {
                            // Its last server's screen is owed this one
                            // whole, and nothing waits for that one's word.
                            host.forget();
                            waiting = false;
                            clock.next = now;
                            peer.end
                                .send(&FromPane::Greeting(Box::new(host.greeting())));
                            taken = true;
                            break;
                        }
                        Ok(Some(ToPane::Who)) => {
                            peer.end
                                .send(&FromPane::Greeting(Box::new(host.greeting())));
                            let _ = peer.end.flush_all();
                            done = true;
                        }
                        Ok(Some(ToPane::Join { run })) if run == host.run => {
                            peer.side = Some(None);
                        }
                        Ok(None) => break,
                        Ok(Some(_)) | Err(_) => done = true,
                    },
                    Some(watch) => match peer.end.next_message::<ToSide>() {
                        Ok(Some(ToSide::Watch)) => *watch = Some(Watch::default()),
                        Ok(Some(ToSide::Taken)) => {
                            if let Some(watch) = watch {
                                watch.waiting = false;
                            }
                        }
                        Ok(Some(ToSide::Body { token, inputs })) => host.body(token, inputs),
                        Ok(Some(ToSide::Fetch { token })) => {
                            let captured = host.fetch(token);
                            peer.end.send(&FromSide::Held { token, captured });
                        }
                        Ok(None) => break,
                        Err(_) => done = true,
                    },
                }
                if done {
                    break;
                }
            }
            closed |= done;
            // Its server's link now, or gone.
            if taken {
                link = Some(peer.end);
            } else if !closed {
                kept.push(peer);
            }
        }
        peers = kept;
        if host.input {
            host.write_input();
        }
        if let Some((at, timer)) = host.timer()
            && at <= Instant::now()
        {
            host.fire(timer, Instant::now());
        }
        if again.is_some_and(|at| at <= Instant::now()) {
            again = None;
            host.look_again();
        }
    }
}

/// Reads its program's terminal, `READ` at most, into its screen.
fn read_program(host: &mut Host, buffer: &mut [u8], clock: &mut Clock, now: Instant) {
    let mut total = 0usize;
    let ended = loop {
        let Process::Reading(child) = host.pane().process() else {
            return;
        };
        if total >= READ || host.pane().has_unread() {
            break false;
        }
        match fuxix::io::read(&child.master, buffer) {
            Err(fuxix::Errno::AGAIN) => break false,
            Err(fuxix::Errno::INTR) => {}
            // EIO: the slave side is closed; the program is gone.
            Ok(0) | Err(fuxix::Errno::IO) => break true,
            Err(error) => {
                host.read_failed(error);
                break false;
            }
            Ok(n) => {
                total = total.saturating_add(n);
                host.output(buffer.get(..n).unwrap_or_default(), now);
            }
        }
    };
    if total > 0 {
        clock.wrote = Some(now);
    }
    if ended {
        host.hung_up(now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn an_unknown_fux_path_is_not_taken_from_the_starter() -> TestResult {
        let launch = pane::Launch {
            argv: command::Program::new(
                "/bin/sh".into(),
                vec!["-c".into(), "[ -z \"${FUX+x}\" ]".into()],
            )?,
            cwd: "/".into(),
        };
        let run = Run::now().ok_or("a clock")?;
        let socket = std::path::Path::new("/nonexistent/fux.sock");
        let told = Env::of(PaneId::of(2), run, Some(socket), None);
        let child = spawn(&told, 0, &launch, (Size::new(24, 80)?, None))?.ok_or("a child")?;
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut status = None;
        while Instant::now() < deadline {
            status = child.leader.exited();
            if status.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let _ = child.leader.finish();
        assert_eq!(status, Some(Ended::Exited(0)));
        Ok(())
    }

    #[test]
    fn what_is_left_is_read_a_turn_at_a_time_to_its_end() -> TestResult {
        // 2 MiB, then the end: all of it, a turn's worth each turn.
        let total = 2usize << 20;
        let (mut supplied, mut read, mut turns) = (0usize, 0usize, 0usize);
        loop {
            let (left, drain) = left_turn(|bytes| {
                let n = bytes.len().min(total.saturating_sub(supplied));
                supplied = supplied.saturating_add(n);
                Ok(n)
            });
            assert!(left.len() <= pane::SLICE, "{} in a turn", left.len());
            read = read.saturating_add(left.len());
            turns = turns.saturating_add(1);
            match drain {
                Drain::More => {}
                Drain::Closed => break,
                Drain::Empty => return Err("empty before its end".into()),
                Drain::Failed(error) => return Err(error.into()),
            }
        }
        assert_eq!((read, turns), (total, total / pane::SLICE + 1));
        let null = std::fs::File::open("/dev/null")?;
        assert_eq!(
            left_turn(|bytes| fuxix::io::read(&null, bytes)),
            (Vec::new(), Drain::Closed)
        );
        // Found empty just after some was read, it may hold more: the next
        // turn says; found empty as a turn begins, it is drained.
        let mut reads = [Ok(1), Err(fuxix::Errno::AGAIN)].into_iter();
        let mut read = |_: &mut [u8]| reads.next().unwrap_or(Err(fuxix::Errno::AGAIN));
        assert_eq!(left_turn(&mut read), (vec![0], Drain::More));
        assert_eq!(left_turn(&mut read), (Vec::new(), Drain::Empty));
        Ok(())
    }

    #[test]
    fn unexpected_terminal_read_errors_are_not_empty_output() -> TestResult {
        let mut reads = [Ok(1), Err(fuxix::Errno::INTR), Err(fuxix::Errno::SRCH)].into_iter();
        let (left, end) = left_turn(|bytes| {
            if let Some(first) = bytes.first_mut() {
                *first = b'x';
            }
            reads.next().ok_or(fuxix::Errno::SRCH)?
        });
        assert_eq!(
            (left, end),
            (b"x".to_vec(), Drain::Failed(fuxix::Errno::SRCH))
        );
        for end in [Ok(0), Err(fuxix::Errno::IO)] {
            assert_eq!(left_turn(|_| end), (Vec::new(), Drain::Closed));
        }
        let empty = left_turn(|_| Err(fuxix::Errno::AGAIN));
        assert_eq!(empty, (Vec::new(), Drain::Empty));
        let directory = std::fs::File::open(".")?;
        let (_, end) = left_turn(|bytes| fuxix::io::read(&directory, bytes));
        assert!(matches!(end, Drain::Failed(_)), "{end:?}");
        let (reader, _writer) = std::io::pipe()?;
        fuxix::io::set_nonblocking(&reader, true)?;
        assert_eq!(
            left_turn(|bytes| fuxix::io::read(&reader, bytes)),
            (Vec::new(), Drain::Empty)
        );
        Ok(())
    }

    /// A pane alone, starting no program.
    fn lone(rows: u16, cols: u16) -> Result<Host, Box<dyn std::error::Error>> {
        let begin = Begin {
            id: PaneId::of(1),
            run: Run::now().ok_or("a clock")?,
            shell: true,
            argv: command::Program::new("/bin/sh".into(), Vec::new())?,
            cwd: "/".into(),
            size: Size::new(rows, cols)?,
            options: crate::pane::tests::options(1 << 10)?,
            hosting: Hosting::default(),
            socket: None,
            exe: None,
        };
        Ok(Host::begin(begin, Instant::now())?)
    }

    /// A frame whose reading takes more than one call of the parser is
    /// told whole: nothing it did, and no screen, till it is read.
    #[test]
    fn a_frame_is_told_whole() -> TestResult {
        let mut host = lone(50, 200)?;
        drop(host.take());
        drop(host.update());
        let mut frame = b"\x1b[?2026h\x07".to_vec();
        frame.extend(crate::pane::tests::spends_the_budget(50, 200));
        frame.extend_from_slice(b"DONE\x1b[?2026l");
        host.output(&frame, Instant::now());
        assert!(host.pane().mid_frame(), "the budget stopped no call");
        assert!(
            host.take().is_empty() && host.update().is_none(),
            "told mid-frame"
        );
        while host.pane().mid_frame() {
            host.fire(Timer::Unread, Instant::now());
        }
        let told = host.take();
        assert!(
            told.iter().any(|t| matches!(
                t,
                FromPane::Told {
                    told: Told::Bell,
                    ..
                }
            )),
            "{told:?}"
        );
        let update = host.update().ok_or("no screen")?;
        assert_eq!(update.counts, host.pane().counts());
        Ok(())
    }
}
