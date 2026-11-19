//! The pane daemon: one process, one poll loop, one program on one terminal.
//! It answers whoever connects, keeps running with nobody connected, and
//! when its program ends it keeps the last screen until it is killed.
use crate::conn::{Broken, Conn};
use crate::dir::{self, Session};
use crate::model::{Model, REPLY_NEED, SLICE, checked_size};
use crate::sys::{self, Listener};
use crate::wire::{
    Answer, Code, Ending, Failure, Hello, MAX_REQUEST, MAX_WAIT_MS, PaneId, REVISION, Request,
    State, Token,
};
use fux_vt::Sink;
use fuxix::Errno;
use fuxix::poll::{Events, Poll, PollFd};
use fuxix::process::{Delivery, Job, Reach, Scan, Signal, Status};
use fuxix::pty::Master;
use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

/// The most bytes of output read at a time.
const READ: usize = 64 << 10;
/// The most typed bytes waiting for the program. A send that does not fit
/// whole is refused whole.
pub const MAX_INPUT: usize = 64 << 10;
/// Room kept apart for the terminal's replies to the program.
const REPLY_ROOM: usize = 64 << 10;
/// Room for what must not wait behind typed input: Windows' Ctrl-C.
const URGENT_ROOM: usize = 64;
/// How many connections are served at once.
const MAX_PEERS: usize = 16;
/// How many of them may be parked in a wait, so that some slot can always
/// serve a read or a kill.
const MAX_WAITS: usize = 12;
/// The most answer bytes held for slow readers, over all connections.
const MAX_STORED: usize = 16 << 20;
/// How long a frame may take to arrive, and an answer to start draining.
const STALL: Duration = Duration::from_secs(5);
/// How long an answer may take to drain, however steadily.
const ANSWER_MOST: Duration = Duration::from_secs(30);
/// How long output may still arrive after the program ended: at least long
/// enough for what it wrote last to cross the terminal.
const SETTLE_LEAST: Duration = Duration::from_millis(20);
/// The longest the last output is read, against something still writing.
const DRAIN_MOST: Duration = Duration::from_secs(1);
/// How long a hung-up program gets before it is killed.
const HANGUP_GRACE: Duration = Duration::from_millis(200);
/// How long a kill waits for its program to be gone.
const KILL_WAIT: Duration = Duration::from_secs(2);
/// How long a kill reads on for the terminal's close to finish.
const CLOSE_WAIT: Duration = Duration::from_millis(500);
/// How long a kill rests between looks.
const KILL_STEP: Duration = Duration::from_millis(5);
/// How many reads one look at a dying terminal makes.
const DISCARD_READS: usize = 64;
/// The history a pane keeps unless told otherwise.
pub const DEFAULT_HISTORY: usize = 1 << 20;
/// The most history a pane may keep.
pub const MAX_HISTORY: usize = 16 << 20;

/// What `start` hands the daemon.
#[derive(Debug)]
pub struct Spec {
    pub root: PathBuf,
    pub name: String,
    pub cols: u16,
    pub rows: u16,
    pub history: usize,
    pub cwd: PathBuf,
    pub labels: Vec<(String, String)>,
    pub argv: Vec<OsString>,
}

impl Spec {
    /// As the command line of the daemon: native strings stay native.
    pub fn to_args(&self) -> Vec<OsString> {
        let mut args: Vec<OsString> = vec![
            "__pane".into(),
            self.root.clone().into(),
            self.name.clone().into(),
            self.cols.to_string().into(),
            self.rows.to_string().into(),
            self.history.to_string().into(),
            self.cwd.clone().into(),
            self.labels.len().to_string().into(),
        ];
        for (key, value) in &self.labels {
            args.push(format!("{key}={value}").into());
        }
        args.extend(self.argv.iter().cloned());
        args
    }

    fn parse(mut args: impl Iterator<Item = OsString>) -> Result<Spec, Failure> {
        let bad = |what: &str| Failure::new(Code::Invalid, format!("pane arguments: {what}"));
        let mut next = |what: &'static str| args.next().ok_or_else(|| bad(what));
        let text = |value: OsString, what: &str| value.into_string().map_err(|_| bad(what));
        let root = PathBuf::from(next("root")?);
        let name = text(next("name")?, "name")?;
        let cols = text(next("cols")?, "cols")?
            .parse()
            .map_err(|_| bad("cols"))?;
        let rows = text(next("rows")?, "rows")?
            .parse()
            .map_err(|_| bad("rows"))?;
        let history = text(next("history")?, "history")?
            .parse()
            .map_err(|_| bad("history"))?;
        let cwd = PathBuf::from(next("cwd")?);
        let count: usize = text(next("labels")?, "labels")?
            .parse()
            .map_err(|_| bad("labels"))?;
        let mut labels = Vec::new();
        for _ in 0..count.min(crate::wire::MAX_LABELS) {
            labels.push(dir::parse_label(&text(next("label")?, "label")?)?);
        }
        Ok(Spec {
            root,
            name,
            cols,
            rows,
            history,
            cwd,
            labels,
            argv: args.collect(),
        })
    }
}

/// What waits to be written to the program, in order: typed input and the
/// terminal's replies share one queue and have separate limits.
#[derive(Default)]
struct Input {
    queue: VecDeque<(bool, Vec<u8>)>,
    head_sent: usize,
    typed: usize,
    replies: usize,
    urgent: Vec<u8>,
    lost_replies: u64,
}

impl Input {
    fn admit(&mut self, bytes: Vec<u8>) -> Result<(), Failure> {
        let total = self.typed.saturating_add(bytes.len());
        if total > MAX_INPUT {
            let why = format!(
                "{} bytes would make {total} waiting, at most {MAX_INPUT}; nothing was queued",
                bytes.len()
            );
            return Err(Failure::new(Code::Busy, why));
        }
        self.typed = total;
        if !bytes.is_empty() {
            self.queue.push_back((true, bytes));
        }
        Ok(())
    }

    fn reply_room(&self) -> usize {
        REPLY_ROOM.saturating_sub(self.replies)
    }

    fn has_output(&self) -> bool {
        !self.urgent.is_empty() || !self.queue.is_empty()
    }

    fn unwritten(&self) -> u64 {
        let queued = self.typed.saturating_add(self.replies);
        u64::try_from(queued.saturating_sub(self.head_sent)).unwrap_or(u64::MAX)
    }

    /// Writes what the terminal takes now, what is urgent first.
    fn write(&mut self, master: &Master) -> Result<(), Errno> {
        while !self.urgent.is_empty() {
            match fuxix::io::write(master, &self.urgent) {
                Ok(0) | Err(Errno::AGAIN) => return Ok(()),
                Err(Errno::INTR) => {}
                Ok(n) => {
                    self.urgent = self.urgent.get(n..).unwrap_or_default().to_vec();
                }
                Err(errno) => return Err(errno),
            }
        }
        while let Some((typed, bytes)) = self.queue.front() {
            let typed = *typed;
            let rest = bytes.get(self.head_sent..).unwrap_or_default();
            match fuxix::io::write(master, rest) {
                Ok(0) | Err(Errno::AGAIN) => return Ok(()),
                Err(Errno::INTR) => {}
                Ok(n) => {
                    self.head_sent = self.head_sent.saturating_add(n);
                    if self.head_sent >= bytes.len() {
                        let len = bytes.len();
                        self.queue.pop_front();
                        self.head_sent = 0;
                        let count = if typed {
                            &mut self.typed
                        } else {
                            &mut self.replies
                        };
                        *count = count.saturating_sub(len);
                    }
                }
                Err(errno) => return Err(errno),
            }
        }
        Ok(())
    }
}

impl Sink for Input {
    fn reply(&mut self, bytes: &[u8]) {
        if bytes.len() > self.reply_room() {
            self.lost_replies = self.lost_replies.saturating_add(1);
            return;
        }
        self.replies = self.replies.saturating_add(bytes.len());
        self.queue.push_back((false, bytes.to_vec()));
    }
}

/// One client.
struct Peer {
    conn: Conn,
    greeted: bool,
    wait: Option<(u64, Instant)>,
    /// When it must have made progress by.
    due: Option<Instant>,
    /// When its answer must be gone by, however steadily it drains.
    latest: Option<Instant>,
    closing: bool,
    broken: bool,
    idle_since: Instant,
}

/// What a request comes to.
enum Reply {
    Answer(Vec<u8>),
    Park(u64, Instant),
    /// Answer, then close the connection.
    Close(Vec<u8>),
    /// Answer, then end the pane.
    Finish(Vec<u8>),
}

fn after(now: Instant, wait: Duration) -> Instant {
    now.checked_add(wait).unwrap_or(now)
}

fn ending(status: Status) -> Ending {
    match status {
        Status::Exited(code) => Ending::Code(code),
        #[cfg(unix)]
        Status::Signalled {
            signal,
            core_dumped,
        } => Ending::Signal {
            number: signal,
            name: Signal::from_raw(signal).map_or_else(|| signal.to_string(), |s| s.name().into()),
            core: core_dumped,
        },
    }
}

/// The program's end, seen and not yet final: its last output is being read.
struct Exit {
    ending: Ending,
    settled_at: Instant,
    latest: Instant,
}

struct Pane {
    session: Session,
    dir: PathBuf,
    claim: Option<std::fs::File>,
    name: String,
    labels: Vec<(String, String)>,
    model: Model,
    master: Master,
    job: Option<Job>,
    listener: Listener,
    children: fuxix::signal::Pipe,
    input: Input,
    unparsed: Vec<u8>,
    parsed: usize,
    open: bool,
    quiet: bool,
    check_exit: bool,
    exit: Option<Exit>,
    peers: Vec<Peer>,
    /// How many connections are parked, and the answer bytes held, as of
    /// the connection being served.
    parked: usize,
    stored: usize,
    buffer: Vec<u8>,
}

/// Runs the daemon `args` describe; what it says goes to its starter.
pub fn main(args: impl Iterator<Item = OsString>) -> ExitCode {
    let mut stdout = std::io::stdout();
    let mut go = String::new();
    if std::io::stdin().lock().read_line(&mut go).is_err() {
        return ExitCode::FAILURE;
    }
    let bound = match go.trim() {
        "go" => false,
        "go bound" => true,
        _ => return ExitCode::FAILURE,
    };
    match Spec::parse(args).and_then(|spec| prepare(spec, bound)) {
        Ok(pane) => {
            // Never dropped: closing a terminal can wait on what runs on it,
            // and a pane that has answered its kill has nothing left to do.
            let mut pane = std::mem::ManuallyDrop::new(pane);
            let token = pane.model.token();
            let said = writeln!(stdout, "ok {token}").and_then(|()| stdout.flush());
            // The starter is gone or going: nothing may be written to, or
            // read from, what it left, and later sockets must not land on
            // descriptors 0 and 1.
            if sys::detach_stdio().is_err() || said.is_err() {
                pane.note("its standard streams could not be detached".to_owned());
            }
            pane.serve();
            ExitCode::SUCCESS
        }
        Err(failure) => {
            let message = failure.message.replace('\n', " ");
            let _ = writeln!(stdout, "err {} {message}", failure.code.name());
            let _ = stdout.flush();
            ExitCode::FAILURE
        }
    }
}

/// Claims the name, opens the terminal and starts the program. Whatever
/// fails gives the name back.
fn prepare(spec: Spec, bound: bool) -> Result<Pane, Failure> {
    dir::check_name(&spec.name)?;
    let session = Session {
        root: spec.root.clone(),
    };
    let size = checked_size(spec.cols, spec.rows)?;
    let note = sys::prepare_terminal(&spec.root);
    let id = PaneId::random();
    let mut model = Model::new(id, size, spec.history.min(MAX_HISTORY))?;
    if let Some(note) = note {
        model.limitation(note);
    }
    if bound {
        model.limitation("this pane ends when the job that started it does".to_owned());
    }
    let io = |what: &str, e: &dyn std::fmt::Display| Failure::new(Code::Io, format!("{what}: {e}"));
    // Before anything could start: what a program leaves behind stays this
    // process's to find and kill.
    #[cfg(unix)]
    fuxix::process::adopt_orphans().map_err(|e| io("adopting orphans", &e))?;
    let children =
        fuxix::signal::Pipe::new(&[Signal::Chld]).map_err(|e| io("the signal pipe", &e))?;
    let lock = session.lock()?;
    let (dir, claim) = session.claim(&lock, &spec.name)?;
    let bound = sys::bind(&dir, id);
    let listener = match bound {
        Ok(listener) => listener,
        Err(failure) => {
            dir::remove_pane(&dir, claim);
            return Err(failure);
        }
    };
    drop(lock);
    let started = start_program(&spec, size);
    let (master, job) = match started {
        Ok(started) => started,
        Err(failure) => {
            drop(listener);
            withdraw(&session, &dir, claim);
            return Err(failure);
        }
    };
    Ok(Pane {
        session,
        dir,
        claim: Some(claim),
        name: spec.name,
        labels: spec.labels,
        model,
        master,
        job: Some(job),
        listener,
        children,
        input: Input::default(),
        unparsed: Vec::new(),
        parsed: 0,
        open: true,
        quiet: false,
        check_exit: true,
        exit: None,
        peers: Vec::new(),
        parked: 0,
        stored: 0,
        buffer: vec![0; READ],
    })
}

fn start_program(spec: &Spec, size: fux_vt::Size) -> Result<(Master, Job), Failure> {
    let (master, slave) = fuxix::pty::open(size.nonzero().into())
        .map_err(|e| Failure::new(Code::Io, format!("opening a terminal: {e}")))?;
    let program = spec
        .argv
        .first()
        .map_or_else(|| "?".into(), |p| p.to_string_lossy().into_owned());
    let job = sys::spawn(slave, &spec.argv, |command| {
        command
            .current_dir(&spec.cwd)
            .env("TERM", "xterm-256color")
            .env("TERM_PROGRAM", "fux2")
            .env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"))
            .env("FUX2_PANE", &spec.name);
    })
    .map_err(|e| {
        let unusable = matches!(
            e.kind(),
            std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
        );
        let code = if unusable { Code::Invalid } else { Code::Io };
        let cwd = spec.cwd.display();
        Failure::new(code, format!("starting {program} in {cwd}: {e}"))
    })?;
    Ok((master, job))
}

/// Takes the pane's name out of the session, under its lock. With the lock
/// out of reach the leftovers are the next start's to clear: the claim goes
/// with this process.
fn withdraw(session: &Session, dir: &std::path::Path, claim: std::fs::File) {
    let lock = session.lock();
    dir::remove_pane(dir, claim);
    drop(lock);
}

impl Pane {
    fn note(&mut self, text: String) {
        self.model.limitation(text);
    }

    fn serve(&mut self) {
        let mut poll = Poll::default();
        loop {
            let now = Instant::now();
            let timeout = self.timeout(now);
            let (master, listener, children, peers) = self.wait(&mut poll, timeout);
            let now = Instant::now();
            if children {
                while let Some(signal) = self.children.next() {
                    self.check_exit |= signal == Signal::Chld;
                }
            }
            self.notice_exit(now);
            let before = self.model.version();
            if master.intersects(Events::IN | Events::HUP | Events::ERR) || self.exit.is_some() {
                self.read_terminal();
            }
            self.parse();
            self.settle(now);
            if self.model.running()
                && self.open
                && self.input.has_output()
                && let Err(errno) = self.input.write(&self.master)
            {
                self.note(format!("writing to the program: {errno}"));
                self.input = Input::default();
            }
            if self.input.lost_replies > 0 {
                let lost = self.input.lost_replies;
                self.note(format!("{lost} replies to the program were dropped"));
                self.input.lost_replies = 0;
            }
            // Served first: what was polled lines up with the connections
            // as they were, and accepting may close an idle one.
            if self.service(&peers, now) {
                return;
            }
            if listener {
                self.accept(now);
            }
            if self.model.version() != before {
                self.wake(now);
            }
            self.expire(Instant::now());
        }
    }

    /// How long the next poll may sleep.
    fn timeout(&self, now: Instant) -> Option<Duration> {
        let parsing = self.parsed < self.unparsed.len() || self.model.owes_work();
        if parsing && self.input.reply_room() >= REPLY_NEED {
            return Some(Duration::ZERO);
        }
        if self.check_exit {
            return Some(Duration::ZERO);
        }
        let settle = (self.exit.as_ref())
            .filter(|_| self.model.running())
            .map(|exit| exit.settled_at.max(now).min(exit.latest));
        let peers = self.peers.iter().flat_map(|peer| {
            let wait = peer.wait.map(|(_, at)| at);
            [peer.due, peer.latest, wait].into_iter().flatten()
        });
        let next = peers.chain(settle).min()?;
        // A pane still settling looks again soon: output may come unasked.
        let wait = next.saturating_duration_since(now);
        Some(match self.exit.is_some() && self.model.running() {
            true => wait.min(Duration::from_millis(5)),
            false => wait,
        })
    }

    /// Waits for anything to do; what was ready, in the order polled.
    fn wait(
        &self,
        poll: &mut Poll,
        timeout: Option<Duration>,
    ) -> (Events, bool, bool, Vec<Events>) {
        let reading = self.open && self.model.running() && self.parsed >= self.unparsed.len();
        let writing = self.open && self.model.running() && self.input.has_output();
        let mut interest = Events::NONE;
        if reading {
            interest |= Events::IN;
        }
        if writing {
            interest |= Events::OUT;
        }
        let mut fds = poll.list();
        let children = fds.push(PollFd::new(self.children.reader(), Events::IN));
        let listener = fds.push(PollFd::new(&self.listener, Events::IN));
        let master = (reading || writing).then(|| fds.push(PollFd::new(&self.master, interest)));
        let peers: Vec<_> = (self.peers.iter())
            .map(|peer| fds.push(PollFd::new(&peer.conn.stream, peer.conn.events())))
            .collect();
        match fds.poll(timeout) {
            Ok(_) | Err(Errno::INTR) => {}
            // A failing poll must not spin; the turn's work still runs.
            Err(_) => std::thread::sleep(Duration::from_millis(10)),
        }
        (
            master.map_or(Events::NONE, |entry| fds.events(entry)),
            !fds.events(listener).is_empty(),
            !fds.events(children).is_empty(),
            peers.into_iter().map(|entry| fds.events(entry)).collect(),
        )
    }

    fn notice_exit(&mut self, now: Instant) {
        if !std::mem::take(&mut self.check_exit) || self.exit.is_some() {
            return;
        }
        #[cfg(unix)]
        {
            let leader = self.job.as_ref().map(Job::leader);
            fuxix::process::reap_orphans(|pid| Some(pid) == leader);
        }
        // Read without reaping: the job stays whole, so a later kill can
        // still reach what the program left behind.
        let Some(Ok(Some(status))) = self.job.as_ref().map(Job::ended) else {
            return;
        };
        self.exit = Some(Exit {
            ending: ending(status),
            settled_at: after(now, fuxix::pty::SETTLE.max(SETTLE_LEAST)),
            latest: after(now, DRAIN_MOST),
        });
    }

    fn read_terminal(&mut self) {
        if !self.open || self.parsed < self.unparsed.len() || !self.model.running() {
            return;
        }
        match fuxix::io::read(&self.master, &mut self.buffer) {
            Ok(0) | Err(Errno::IO) => self.open = false,
            Ok(n) => {
                self.unparsed = self.buffer.get(..n).unwrap_or_default().to_vec();
                self.parsed = 0;
                self.quiet = false;
            }
            Err(Errno::AGAIN) => self.quiet = true,
            Err(Errno::INTR) => {}
            Err(errno) if errno.is_peer_gone() => self.open = false,
            Err(errno) => {
                self.open = false;
                self.note(format!("reading the program's output: {errno}"));
            }
        }
    }

    /// Gives waiting output to the terminal, a slice at a time, as long as
    /// the replies it may cause have room.
    fn parse(&mut self) {
        let mut any = false;
        loop {
            let rest = self.unparsed.get(self.parsed..).unwrap_or_default();
            if rest.is_empty() && !self.model.owes_work() {
                break;
            }
            if self.input.reply_room() < REPLY_NEED {
                break;
            }
            let slice = rest.get(..SLICE.min(rest.len())).unwrap_or_default();
            let (taken, stopped) = self.model.parse(slice, &mut self.input);
            self.parsed = self.parsed.saturating_add(taken);
            any = true;
            if stopped {
                break;
            }
        }
        if self.parsed >= self.unparsed.len() {
            self.unparsed = Vec::new();
            self.parsed = 0;
        }
        if any {
            self.model.advance();
        }
    }

    /// Freezes the screen once the ended program's last output is in.
    fn settle(&mut self, now: Instant) {
        let Some(exit) = &self.exit else {
            return;
        };
        if !self.model.running() {
            return;
        }
        let parsed = self.unparsed.is_empty() && !self.model.owes_work();
        let settled = now >= exit.settled_at && self.quiet;
        if !(parsed && (!self.open || settled)) && now < exit.latest {
            return;
        }
        let state = State::Exited {
            ending: exit.ending.clone(),
            drained: !self.open,
            unwritten: self.input.unwritten(),
        };
        self.input = Input::default();
        self.model.set_state(state);
    }

    fn accept(&mut self, now: Instant) {
        for _ in 0..MAX_PEERS {
            let stream = match sys::accept(&mut self.listener) {
                Ok(Some(stream)) => stream,
                Ok(None) => return,
                Err(failure) => {
                    self.note(failure.message);
                    return;
                }
            };
            if self.peers.len() >= MAX_PEERS && !self.evict_idle() {
                // Every slot is mid-request, each bounded by its deadline.
                sys::abandon(&stream);
                continue;
            }
            self.peers.push(Peer {
                conn: Conn::new(stream, MAX_REQUEST),
                greeted: false,
                wait: None,
                due: None,
                latest: None,
                closing: false,
                broken: false,
                idle_since: now,
            });
        }
    }

    /// Closes the connection idle longest, for a newcomer: it asked for
    /// nothing, and a client reconnects.
    fn evict_idle(&mut self) -> bool {
        let oldest = (self.peers.iter().enumerate())
            .filter(|(_, p)| p.wait.is_none() && p.conn.pending() == 0 && !p.conn.mid_frame())
            .min_by_key(|(_, p)| p.idle_since)
            .map(|(index, _)| index);
        let Some(oldest) = oldest else {
            return false;
        };
        let mut index = 0usize;
        self.peers.retain(|_| {
            let keep = index != oldest;
            index = index.saturating_add(1);
            keep
        });
        true
    }

    /// Each ready connection gets one turn. True once the pane is to end.
    fn service(&mut self, ready: &[Events], now: Instant) -> bool {
        let mut peers = std::mem::take(&mut self.peers);
        let mut finishing = false;
        for (index, events) in ready.iter().enumerate() {
            if events.is_empty() || finishing {
                continue;
            }
            self.parked = peers.iter().filter(|p| p.wait.is_some()).count();
            self.stored = peers.iter().map(|p| p.conn.pending()).sum();
            let Some(peer) = peers.get_mut(index) else {
                continue;
            };
            match self.turn(peer, now) {
                Ok(done) => finishing = done,
                // Broken: nothing more can reach it, so nothing is kept
                // for it, and a poll would only report it again.
                Err(_) => {
                    sys::abandon(&peer.conn.stream);
                    peer.broken = true;
                }
            }
        }
        peers
            .retain(|peer| !peer.broken && (!peer.closing || peer.conn.pending() > 0 || finishing));
        self.peers = peers;
        if finishing {
            self.finish();
        }
        finishing
    }

    /// One connection's turn: true if it asked the pane to end.
    fn turn(&mut self, peer: &mut Peer, now: Instant) -> Result<bool, Broken> {
        if peer.conn.flush()? {
            peer.due = peer.latest.map(|latest| after(now, STALL).min(latest));
        }
        if peer.conn.pending() > 0 {
            return Ok(false);
        }
        peer.latest = None;
        peer.due = None;
        if peer.closing {
            return Ok(false);
        }
        let Some(body) = peer.conn.receive()? else {
            if peer.conn.mid_frame() {
                peer.due = Some(after(now, STALL));
            }
            return Ok(false);
        };
        peer.idle_since = now;
        if peer.wait.is_some() {
            // One request at a time: this one came on top of a wait.
            let refusal = Failure::new(Code::Protocol, "a request during a wait");
            return Err(Broken::Frame(refusal));
        }
        let reply = match Request::decode(&body) {
            Ok(request) => self.answer(request, peer, now),
            Err(failure) => Reply::Close(Answer::Refused(failure).encode()),
        };
        let (bytes, close, finish) = match reply {
            Reply::Answer(bytes) => (bytes, false, false),
            Reply::Close(bytes) => (bytes, true, false),
            Reply::Finish(bytes) => (bytes, true, true),
            Reply::Park(version, at) => {
                peer.wait = Some((version, at));
                return Ok(false);
            }
        };
        let bytes = match !finish && self.stored.saturating_add(bytes.len()) > MAX_STORED {
            true => {
                let why = format!(
                    "{} bytes of answers wait, at most {MAX_STORED}",
                    self.stored
                );
                Answer::Refused(Failure::new(Code::Busy, why)).encode()
            }
            false => bytes,
        };
        self.respond(peer, &bytes, now);
        peer.closing = peer.closing || close;
        Ok(finish)
    }

    fn respond(&mut self, peer: &mut Peer, bytes: &[u8], now: Instant) {
        if peer.conn.queue(bytes).is_err() {
            peer.closing = true;
            return;
        }
        peer.due = Some(after(now, STALL));
        peer.latest = Some(after(now, ANSWER_MOST));
        if peer.conn.flush().is_err() {
            peer.closing = true;
        }
    }

    fn answer(&mut self, request: Request, peer: &mut Peer, now: Instant) -> Reply {
        let refused = |failure: Failure| Answer::Refused(failure).encode();
        if let Request::Hello { revision } = request {
            if revision != REVISION {
                let why = format!("revision {revision}; this pane speaks {REVISION}");
                return Reply::Close(refused(Failure::new(Code::Protocol, why)));
            }
            peer.greeted = true;
            return Reply::Answer(Answer::Hello(self.hello()).encode());
        }
        if !peer.greeted {
            let why = "the first request must be hello";
            return Reply::Close(refused(Failure::new(Code::Protocol, why)));
        }
        if let Request::Kill { expected } = request {
            return match self.model.check_expected(expected) {
                Ok(()) => Reply::Finish(self.kill()),
                Err(failure) => Reply::Answer(refused(failure)),
            };
        }
        if let Request::Wait { after: token, ms } = request {
            return match self.wait_for(token, ms, now) {
                Ok(reply) => reply,
                Err(failure) => Reply::Answer(refused(failure)),
            };
        }
        match self.act(request) {
            Ok(bytes) => Reply::Answer(bytes),
            Err(failure) => Reply::Answer(refused(failure)),
        }
    }

    fn hello(&self) -> Hello {
        let (cols, rows) = self.model.size();
        let mut labels = vec![("name".to_owned(), self.name.clone())];
        labels.extend(self.labels.iter().cloned());
        Hello {
            revision: REVISION,
            id: self.model.id,
            labels,
            cols,
            rows,
            state: self.model.state().clone(),
            pid: std::process::id(),
        }
    }

    fn wait_for(&mut self, token: Token, ms: u32, now: Instant) -> Result<Reply, Failure> {
        self.model.check_token(token)?;
        if ms > MAX_WAIT_MS {
            let why = format!("a wait of {ms} ms, at most {MAX_WAIT_MS}");
            return Err(Failure::new(Code::Invalid, why));
        }
        if token.version < self.model.version() || !self.model.running() {
            return Ok(Reply::Answer(self.waited()));
        }
        if ms == 0 {
            return Err(self.no_change(token.version));
        }
        if self.parked >= MAX_WAITS {
            let why = format!("{} waits are parked, at most {MAX_WAITS}", self.parked);
            return Err(Failure::new(Code::Busy, why));
        }
        Ok(Reply::Park(
            token.version,
            after(now, Duration::from_millis(ms.into())),
        ))
    }

    fn waited(&mut self) -> Vec<u8> {
        let token = self.model.token();
        let state = self.model.state().clone();
        Answer::Waited { token, state }.encode()
    }

    fn no_change(&mut self, version: u64) -> Failure {
        let token = self.model.token();
        let why = format!("no change after version {version}; the pane is at {token}");
        Failure::new(Code::Timeout, why)
    }

    /// Answers every wait the pane has moved past.
    fn wake(&mut self, now: Instant) {
        let mut peers = std::mem::take(&mut self.peers);
        for peer in &mut peers {
            if peer
                .wait
                .is_some_and(|(version, _)| version < self.model.version())
            {
                peer.wait = None;
                let answer = self.waited();
                self.respond(peer, &answer, now);
            }
        }
        self.peers = peers;
    }

    /// Ends what is overdue: waits that saw nothing, and connections that
    /// stopped sending or stopped reading.
    fn expire(&mut self, now: Instant) {
        let mut peers = std::mem::take(&mut self.peers);
        for peer in &mut peers {
            if let Some((version, at)) = peer.wait
                && now >= at
            {
                peer.wait = None;
                let failure = self.no_change(version);
                self.respond(peer, &Answer::Refused(failure).encode(), now);
            }
        }
        peers.retain(|peer| {
            let overdue = peer.due.is_some_and(|due| now >= due);
            if overdue {
                // A reader that stopped reading leaves nothing behind.
                sys::abandon(&peer.conn.stream);
            }
            !overdue
        });
        self.peers = peers;
    }

    /// Requests answered at once.
    fn act(&mut self, request: Request) -> Result<Vec<u8>, Failure> {
        match request {
            Request::Send { parts, expected } => {
                self.model.check_expected(expected)?;
                self.running()?;
                let bytes = self.model.encode(&parts)?;
                let count = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
                self.input.admit(bytes)?;
                let token = self.model.token();
                Ok(Answer::Sent {
                    bytes: count,
                    token,
                }
                .encode())
            }
            Request::Resize {
                cols,
                rows,
                expected,
            } => {
                self.model.check_expected(expected)?;
                self.running()?;
                let size = checked_size(cols, rows)?;
                if self.model.size() != (cols, rows) {
                    self.resize(size)?;
                }
                let token = self.model.token();
                Ok(Answer::Resized { cols, rows, token }.encode())
            }
            Request::Signal { name, expected } => {
                self.model.check_expected(expected)?;
                self.running()?;
                let delivery = self.signal(&name)?.to_owned();
                Ok(Answer::Signalled { delivery }.encode())
            }
            Request::Screen { since } => self.model.read(since),
            Request::History {
                epoch,
                first,
                count,
            } => self.model.history(epoch, first, count),
            Request::Hello { .. } | Request::Wait { .. } | Request::Kill { .. } => {
                Err(Failure::new(Code::Protocol, "a request answered elsewhere"))
            }
        }
    }

    fn running(&self) -> Result<(), Failure> {
        match self.model.running() {
            true => Ok(()),
            false => {
                let why = format!("the program has ended: {}", self.model.state());
                Err(Failure::new(Code::Ended, why))
            }
        }
    }

    /// Prepares the new screen, asks the system, and only then shows it: a
    /// refused resize leaves the screen and its version as they were.
    fn resize(&mut self, size: fux_vt::Size) -> Result<(), Failure> {
        let next = self.model.prepare_resize(size)?;
        fuxix::terminal::set_window_size(&self.master, size.nonzero().into(), None)
            .map_err(|e| Failure::new(Code::Io, format!("resizing the terminal: {e}")))?;
        if let Some(report) = self.model.commit_resize(next) {
            self.input.reply(&report);
        }
        Ok(())
    }

    fn signal(&mut self, name: &str) -> Result<&'static str, Failure> {
        let signal = (Signal::ALL.iter().copied())
            .find(|signal| signal.name() == name)
            .ok_or_else(|| Failure::new(Code::Invalid, format!("no signal {name:?}")))?;
        let unsupported = || {
            Failure::new(
                Code::Unsupported,
                format!("{name} cannot be delivered on this system"),
            )
        };
        match fuxix::process::delivery(signal, false) {
            Delivery::Group => {
                let io = |e: Errno| Failure::new(Code::Io, format!("signalling: {e}"));
                let group = fuxix::terminal::foreground_group(&self.master)
                    .map_err(io)?
                    .or_else(|| self.job.as_ref().map(Job::leader))
                    .ok_or_else(unsupported)?;
                #[cfg(unix)]
                fuxix::process::kill_group(group, signal).map_err(io)?;
                #[cfg(windows)]
                let _ = group;
                Ok("foreground-group")
            }
            // Ahead of anything typed: a full queue must not hold back
            // the way to stop what is not reading it.
            Delivery::Keys(keys) => {
                let total = self.input.urgent.len().saturating_add(keys.len());
                if total > URGENT_ROOM {
                    let why = format!("{total} urgent bytes, at most {URGENT_ROOM}");
                    return Err(Failure::new(Code::Busy, why));
                }
                self.input.urgent.extend_from_slice(keys);
                Ok("console-input")
            }
            Delivery::Attached | Delivery::Job | Delivery::Unsupported => Err(unsupported()),
        }
    }

    /// Reads and drops what the terminal holds, for a program that may be
    /// waiting to write as it dies.
    fn discard_output(&mut self) {
        for _ in 0..DISCARD_READS {
            if !self.open {
                return;
            }
            match fuxix::io::read(&self.master, &mut self.buffer) {
                Ok(0) | Err(Errno::IO) => self.open = false,
                Ok(_) | Err(Errno::INTR) => {}
                Err(errno) if errno.is_peer_gone() => self.open = false,
                Err(_) => return,
            }
        }
    }

    /// Waits a moment for the terminal. On Windows it is the poll that
    /// moves a read along, and a closing console waits for its output to
    /// be read: a plain sleep here could leave it, and this process, stuck.
    fn pause(&self, poll: &mut Poll) {
        if !self.open {
            return std::thread::sleep(KILL_STEP);
        }
        let mut fds = poll.list();
        fds.push(PollFd::new(&self.master, Events::IN));
        if fds.poll(Some(KILL_STEP)).is_err() {
            std::thread::sleep(KILL_STEP);
        }
    }

    fn gone_by(&mut self, poll: &mut Poll, deadline: Instant) -> bool {
        loop {
            self.discard_output();
            if matches!(self.job.as_ref().map(Job::ended), Some(Ok(Some(_))) | None) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            self.pause(poll);
        }
    }

    /// Reads the terminal to its end, a bounded time, so that closing it
    /// has finished before this process goes.
    fn drain_to_close(&mut self, poll: &mut Poll, deadline: Instant) {
        while self.open && Instant::now() < deadline {
            self.discard_output();
            self.pause(poll);
        }
    }

    /// Ends the program and everything it left, gives the name back, and
    /// says how complete that was.
    fn kill(&mut self) -> Vec<u8> {
        let mut poll = Poll::default();
        let mut missed = Vec::new();
        if let Some(job) = &self.job {
            let hung = fuxix::pty::hang_up(&self.master, job);
            drop(hung);
            let now = Instant::now();
            self.gone_by(&mut poll, after(now, HANGUP_GRACE));
        }
        if let Some(job) = &self.job {
            let sent = job.kill(Reach::Session);
            // An ended leader's group may hold nothing to signal, which
            // macOS calls EPERM: what is really left is counted below.
            let leader = job.leader();
            let ended = matches!(job.ended(), Ok(Some(_)));
            for (pid, errno) in sent.failed {
                if errno != Errno::SRCH && !(ended && pid == leader) {
                    missed.push(format!("{pid} could not be killed: {errno}"));
                }
            }
            match sent.scan {
                Scan::Complete => {}
                Scan::Truncated => missed.push("the list of processes was cut short".to_owned()),
                Scan::Failed(errno) => missed.push(format!("listing processes: {errno}")),
            }
        }
        if !self.gone_by(&mut poll, after(Instant::now(), KILL_WAIT)) {
            missed.push("the program did not end in time".to_owned());
        }
        if let Some(alive) = self.members_left(after(Instant::now(), KILL_WAIT)) {
            missed.push(format!("{alive} of its processes are still there"));
        }
        if let Some(job) = self.job.take()
            && let Err(job) = job.reap()
        {
            missed.push(format!("process {} was not reaped", job.leader()));
            drop(job);
        }
        #[cfg(unix)]
        fuxix::process::reap_orphans(|_| false);
        self.drain_to_close(&mut poll, after(Instant::now(), CLOSE_WAIT));
        if let Some(claim) = self.claim.take() {
            withdraw(&self.session, &self.dir, claim);
        }
        Answer::Killed {
            complete: missed.is_empty(),
            detail: missed.join("; "),
        }
        .encode()
    }

    /// How many of the job's other processes outlast `deadline`, if any do.
    /// What it adopted and killed is reaped meanwhile, or it would count.
    fn members_left(&self, deadline: Instant) -> Option<usize> {
        let job = self.job.as_ref()?;
        loop {
            #[cfg(unix)]
            {
                let leader = job.leader();
                fuxix::process::reap_orphans(|pid| pid == leader);
            }
            let alive = job.members().pids.len();
            if alive == 0 {
                return None;
            }
            if Instant::now() >= deadline {
                return Some(alive);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Gives the last answer a bounded time to reach whoever asked.
    fn finish(&mut self) {
        let mut poll = Poll::default();
        let deadline = after(Instant::now(), Duration::from_secs(1));
        loop {
            let mut waiting = false;
            for peer in self.peers.iter_mut().filter(|peer| peer.closing) {
                let flushed = peer.conn.flush().is_ok() && peer.conn.pending() == 0;
                waiting |= !flushed && peer.conn.pending() > 0;
                waiting |= flushed && !fuxix::io::written(&peer.conn.stream);
            }
            if !waiting || Instant::now() >= deadline {
                return;
            }
            let mut fds = poll.list();
            for peer in self.peers.iter().filter(|peer| peer.closing) {
                fds.push(PollFd::new(&peer.conn.stream, Events::OUT));
            }
            let _ = fds.poll(Some(Duration::from_millis(20)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A send that does not fit is refused whole: what waits is what
    /// waited, and everything that still fits is taken.
    #[test]
    fn a_refused_send_adds_nothing() -> Result<(), Failure> {
        let mut input = Input::default();
        input.admit(vec![b'x'; MAX_INPUT.saturating_sub(10)])?;
        let refused = input.admit(vec![b'y'; 11]).err().map(|f| f.code);
        assert_eq!(refused, Some(Code::Busy));
        assert_eq!(input.queue.len(), 1);
        input.admit(vec![b'z'; 10])?;
        assert_eq!(input.typed, MAX_INPUT);
        Ok(())
    }
}
