//! The server: one thread (but for fuxix's watchdog while a PTY opens on
//! macOS), one `poll` loop over the listening socket, a signal pipe, every
//! client and every pane's PTY.
use crate::bytes::ByteQueue;
use crate::config::Config;
use crate::id::{ClientId, PaneId};
use crate::layout::Placement;
use crate::pane::{Pane, Process};
use crate::process::Leader;
use crate::protocol::{
    Attach, AttachedFrame, Command, Decoder, Frame, Hello, Mismatch, PROTOCOL, Role, ServerFrame,
};
use crate::render::{self, Grid};
use crate::session::{Dying, Origin, Outgoing, Session, Timer};
use crate::socket::{Endpoint, SocketPath};
use fuxix::poll::{Events as PollFlags, PollFd};
use signal_hook::consts::{SIGCHLD, SIGHUP, SIGINT, SIGTERM};
use std::io::{ErrorKind, Write};
use std::ops::ControlFlow;
use std::os::fd::OwnedFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// The least time between two paints for a client, as a flood is painted.
/// A keystroke's echo (`PaintClock::echo`) and panes gone quiet (`QUIET`) are
/// painted sooner.
const PAINT: Duration = Duration::from_millis(16);
/// Panes quiet for this after their output: painted then, not held to
/// `PAINT`. A program's output in a few bursts, as a screen redrawn is,
/// shows when it ends; a flood, never quiet so long, keeps to `PAINT`.
const QUIET: Duration = Duration::from_millis(1);
/// A client's unsent output may grow to this before paints for it stop.
const OUTPUT_CAP: usize = 4 << 20;
/// A pane's read shorter than this, an echo's or a prompt's, is taken for
/// all it had: the next is left to the poll.
const SHORT_READ: usize = 512;
/// Output read from one pane per tick, so one busy pane cannot starve others.
const PANE_READ: usize = 64 * 1024;
/// Bytes read from one client per tick, so one client cannot hold up the
/// others: a frame larger than this is read over several ticks.
const CONN_READ: usize = 256 * 1024;
/// What a connection's buffers keep once they empty; beyond it, the memory
/// a large frame or paint took is given back.
const CONN_KEEP: usize = 64 * 1024;
/// How long a stopping server waits for its clients and processes.
const STOP_WAIT: Duration = Duration::from_millis(500);
/// A condition that persists is logged once, then at most this often.
const REPORT_EVERY: Duration = Duration::from_secs(10);
/// How long the listener rests after an `accept` failure that retrying at
/// once would only repeat.
const ACCEPT_REST: Duration = Duration::from_millis(100);
/// What a connection refused for want of descriptors is told.
const NO_DESCRIPTORS: &str =
    "the fux server is out of file descriptors; it refuses new connections until some close";

/// Running out of descriptors, while it lasts: connections are refused,
/// each told why.
struct Shortage {
    /// When it was last reported, and how many were refused by then.
    reported: (Instant, u64),
    refused: u64,
}

/// The socket the server is found by: held only while it serves, so the
/// moment it stops, its listener closes and the socket is gone, and a
/// client finds no server rather than one that never answers.
struct Listening {
    listener: UnixListener,
    /// Removes the socket once dropped, after the listener closes.
    _endpoint: Endpoint,
}

/// Where the server is in its life, each stage holding the moment it waits
/// for (`Server::wakes`), and the socket while it has one.
enum Phase {
    /// The listener is polled, unless an `accept` failed in a way that
    /// retrying at once would repeat: then the listener, which stays
    /// readable meanwhile, rests until `resting`.
    Serving {
        listening: Listening,
        resting: Option<Instant>,
    },
    /// Stopping, its socket gone: the server waits for its clients and
    /// processes until then.
    Stopping(Instant),
}

/// Something the loop waits for until a moment (`Server::wakes`), and
/// handles once due (`Server::wake`).
enum Wake {
    Session(Timer),
    /// A closed pane's program's grace is over: it is killed.
    Grace(PaneId),
    /// The listener's rest is over.
    Listen,
    /// A stopping server waits no more.
    Stop,
    /// A connection's client may be painted (`Conn::paint_due`).
    Paint(usize),
}

/// A client's connection: its socket, and where it is in its life.
struct Conn {
    stream: UnixStream,
    decoder: Decoder,
    out: ByteQueue,
    stage: Stage,
}

/// Where a connection is in its life, each stage holding what it alone
/// uses. It goes from `Hello` to `Attaching` and `Attached`, or to
/// `Command`, and ends `Closing` or `Dead`, the only stages it is closed
/// in (`Server::close_conns`): an attached client is detached as its
/// connection leaves `Attached` (`Conn::enter`), however it ends. Each
/// stage that reads frames decodes them as its own type alone
/// (`Server::take_frame`): any other frame is a bad one.
enum Stage {
    /// Its first frame is to be a `Hello`. A client need not wait for the
    /// answer to send its `Attach`, and the descriptor that came with it,
    /// its terminal, waits here until then.
    Hello(Option<OwnedFd>),
    /// It said hello as `fux attach`: its `Attach` is next, with the
    /// descriptor it sent.
    Attaching(Option<OwnedFd>),
    /// Its client is attached: its input, resizes and detach come next.
    Attached(Box<Attached>),
    /// It said hello as a command client: its `Command` is next.
    Command,
    /// Its last frame is sent: it closes once what waits is written, and
    /// what it sends is dropped unread.
    Closing,
    /// It failed or hung up: it closes at the end of the tick.
    Dead,
}

/// An attached client: its view's id, its terminal if the server took it,
/// and what its paints are made from.
struct Attached {
    client: ClientId,
    tty: Option<TakenTerminal>,
    /// What the client's terminal shows, for diffing, while `screen` is
    /// `Shown`.
    shown: Grid,
    screen: Screen,
    /// The grid the next paint is composed into, then swapped with `shown`:
    /// the two are reused by every paint, as is the placement of its panes.
    spare: Grid,
    placement: Placement,
    /// When the next paint may be made.
    clock: PaintClock,
}

/// What is known of what an attached client's terminal shows.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Screen {
    /// `Attached::shown`: the next paint is a diff against it.
    Shown,
    /// Not known: the next paint is a full one.
    Unknown,
    /// Not known, and its output was full: nothing is painted until what
    /// waits is all written, then all of it is.
    Starved,
}

impl Screen {
    /// The terminal was resized: the next paint is a full one.
    fn forget(&mut self) {
        if *self == Screen::Shown {
            *self = Screen::Unknown;
        }
    }
}

/// When a client may be painted next: `PAINT` after its last paint, as a
/// pane that keeps the screen changing is painted; at once for a
/// keystroke's echo; and once the panes are quiet, if that is sooner.
struct PaintClock {
    /// `PAINT` after the last paint.
    next: Instant,
    /// When the next paint may be made: `next`, or once the panes are
    /// quiet for `QUIET` if output came before it.
    due: Instant,
    /// The pane the client last typed into, until it writes: what it writes
    /// then, a keystroke's echo, is painted at once, not held to `PAINT`
    /// behind a pane that keeps the screen changing (tmux paints it at
    /// once too). One paint a keystroke at most.
    echo: Option<PaneId>,
}

impl PaintClock {
    /// A paint was made at `now`.
    fn painted(&mut self, now: Instant) {
        self.next = crate::after(now, PAINT);
        self.due = self.next;
    }
    /// Pane `pane` wrote, read at `now`: the echo of a keystroke into it is
    /// painted at once, other output held to `PAINT` once the panes are
    /// quiet for `QUIET`.
    fn output(&mut self, pane: PaneId, now: Instant) {
        if self.echo == Some(pane) {
            self.echo = None;
            (self.next, self.due) = (now, now);
        } else if now < self.next {
            self.due = crate::after(now, QUIET).min(self.next);
        }
    }
}

/// The client's terminal, which it sent with its `Attach` and the server
/// reopened (`fuxix::terminal::reopen`): its keys are read and its paints
/// written here, not relayed in frames. tmux's server writes to its
/// client's terminal as well. It is held by its client's attachment alone,
/// and given back as that ends (`Conn::enter`).
struct TakenTerminal {
    fd: OwnedFd,
    /// What waits to be written to it.
    out: ByteQueue,
}

impl TakenTerminal {
    fn new(fd: OwnedFd) -> TakenTerminal {
        TakenTerminal {
            fd,
            out: ByteQueue::default(),
        }
    }
    /// Writes what waits, as far as the terminal takes it; false if it
    /// failed, and is to be given up.
    fn flush(&mut self) -> bool {
        write_out(&self.fd, &mut self.out)
    }
    /// Hands the terminal back: what waits for it written as far as it
    /// takes it at once, the rest to `out` for the client to write in paint
    /// frames, before the `Exit` that follows, and the server's descriptor
    /// for it closed.
    fn give_back(mut self, out: &mut ByteQueue) {
        if self.flush() && !self.out.is_empty() {
            ServerFrame::split_into(self.out.as_slice(), ServerFrame::Paint, out);
        }
    }
}

/// Writes `out` to `fd`, nonblocking, as far as it takes it; false if the
/// write failed. The memory a large write took is given back.
fn write_out(fd: impl std::os::fd::AsFd, out: &mut ByteQueue) -> bool {
    while !out.is_empty() {
        match fuxix::io::write(&fd, out.as_slice()) {
            Ok(0) | Err(fuxix::Errno::AGAIN) => break,
            Ok(n) => out.take(n),
            Err(fuxix::Errno::INTR) => continue,
            Err(_) => return false,
        }
    }
    out.shrink(CONN_KEEP);
    true
}

impl Attached {
    fn new(client: ClientId, tty: Option<TakenTerminal>) -> Attached {
        let now = Instant::now();
        Attached {
            client,
            tty,
            shown: Grid::new(0, 0),
            screen: Screen::Unknown,
            spare: Grid::new(0, 0),
            placement: Placement::default(),
            clock: PaintClock {
                next: now,
                due: now,
                echo: None,
            },
        }
    }

    /// Input from the client's terminal, read at `now`, to the session; the
    /// pane it went to, the client's focus, is the one whose echo is
    /// painted at once.
    fn input(&mut self, session: &mut Session, bytes: &[u8], now: Instant) {
        session.input_at(self.client, bytes, now);
        self.clock.echo = session.focused(self.client);
    }

    /// Bytes for the client's terminal, a paint's or what the session sends
    /// it outside one: written there if the server took it, else framed in
    /// `out` for the client.
    fn paint(&mut self, out: &mut ByteQueue, bytes: &[u8]) {
        match &mut self.tty {
            Some(tty) => tty.out.push(bytes),
            None => ServerFrame::split_into(bytes, ServerFrame::Paint, out),
        }
    }
}

impl Conn {
    fn new(stream: UnixStream) -> Conn {
        Conn {
            stream,
            decoder: Decoder::default(),
            out: ByteQueue::default(),
            stage: Stage::Hello(None),
        }
    }

    /// Bytes waiting for the client, in frames or for its terminal.
    fn pending(&self) -> usize {
        let tty = if let Stage::Attached(attached) = &self.stage {
            attached.tty.as_ref().map_or(0, |tty| tty.out.len())
        } else {
            0
        };
        self.out.len().saturating_add(tty)
    }

    /// When the client attached here may be painted, if its view changed
    /// and its terminal is not starved.
    fn paint_due(&self, session: &Session) -> Option<Instant> {
        let Stage::Attached(attached) = &self.stage else {
            return None;
        };
        let dirty = session.views.get(&attached.client)?.dirty;
        // A screen not known is painted whole, whether or not the view
        // changed: forgetting it is asking for that paint.
        let unknown = attached.screen != Screen::Shown;
        let starved = attached.screen == Screen::Starved && self.pending() != 0;
        ((dirty || unknown) && !starved).then_some(attached.clock.due)
    }

    /// Whether `client` is attached here.
    fn serves(&self, client: ClientId) -> bool {
        matches!(&self.stage, Stage::Attached(attached) if attached.client == client)
    }

    fn attached(&mut self) -> Option<&mut Attached> {
        if let Stage::Attached(attached) = &mut self.stage {
            Some(attached)
        } else {
            None
        }
    }

    /// Moves the connection on to `next`. One that was attached has its
    /// client detached and its terminal given back; one that is dead stays
    /// so.
    fn enter(&mut self, session: &mut Session, next: Stage) {
        if matches!(self.stage, Stage::Dead) {
            return;
        }
        if let Stage::Attached(attached) = std::mem::replace(&mut self.stage, next) {
            session.detach(attached.client);
            if let Some(tty) = attached.tty {
                tty.give_back(&mut self.out);
            }
        }
    }

    /// Encodes a frame straight into the output; a connection whose frame
    /// cannot be encoded fails.
    fn send(&mut self, session: &mut Session, frame: &ServerFrame) {
        if self.out.push_with(|out| frame.encode_into(out)).is_err() {
            self.enter(session, Stage::Dead);
        }
    }

    /// Sends the connection's last frame, its client detached and its
    /// terminal given back first: it closes once what waits is sent.
    fn end(&mut self, session: &mut Session, frame: &ServerFrame) {
        self.enter(session, Stage::Closing);
        self.send(session, frame);
    }

    /// Writes what waits for the client's terminal, as far as it takes it;
    /// a terminal that fails is given up, and its client detached, as one
    /// that can no longer be read is (`Server::serve_tty`): no one would
    /// read its keys.
    fn flush_tty(&mut self, session: &mut Session) {
        if let Some(attached) = self.attached()
            && let Some(tty) = &mut attached.tty
            && !tty.flush()
        {
            attached.tty = None;
            self.end(session, &ServerFrame::Exit("detached: the terminal closed"));
        }
    }

    /// Writes what waits for the client until it is all written or the
    /// socket takes no more; a connection that fails is dead. What waits
    /// for its terminal is written too.
    fn flush(&mut self, session: &mut Session) {
        self.flush_tty(session);
        if !write_out(&self.stream, &mut self.out) {
            self.enter(session, Stage::Dead);
        }
    }
}

pub struct Server {
    session: Session,
    phase: Phase,
    conns: Vec<Conn>,
    children: UnixStream,
    /// Closed panes' programs killed but not yet exited, reaped once SIGCHLD
    /// says they have.
    killed: Vec<Leader>,
    stops: UnixStream,
    /// Where bytes read from a client's connection or terminal land before
    /// they are taken; one for the server, reused by every read.
    read_buffer: Vec<u8>,
    /// Where a paint is made before it is sent; reused by every paint.
    paint_buffer: Vec<u8>,
    /// What each descriptor polled is, and then what is ready; reused by
    /// every tick.
    slots: Vec<Slot>,
    ready: Vec<(Slot, PollFlags)>,
    /// A descriptor held in reserve, so that a connection that arrives when
    /// all others are taken can still be accepted and told why it is
    /// refused (bevy-final findings 006 and 012).
    spare: Option<std::fs::File>,
    shortage: Option<Shortage>,
    /// When an `accept` failure other than a shortage was last logged.
    accept_logged: Option<Instant>,
}

/// Writes to the server's log, its stderr. A log that cannot be written,
/// a full disk, say, loses the line and stops nothing.
fn log(message: &str) {
    let _ = writeln!(std::io::stderr(), "fux server: {message}");
}

/// Why a server could not start.
#[derive(Debug)]
pub enum Error {
    Socket(crate::socket::Error),
    /// The listener or the signal pipes could not be set up.
    Setup(std::io::Error),
    /// The first workspace could not be made.
    Start(crate::session::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Socket(error) => error.fmt(f),
            Error::Setup(error) => error.fmt(f),
            Error::Start(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Socket(error) => Some(error),
            Error::Setup(error) => Some(error),
            Error::Start(error) => Some(error),
        }
    }
}

/// Runs a server on `socket` until it is told to stop or its last pane
/// closes.
pub fn serve(socket: &SocketPath, config_path: Option<PathBuf>) -> Result<(), Error> {
    let (config, error) = match &config_path {
        Some(path) => match Config::from_file(path) {
            Ok(config) => (config, None),
            Err(error) => {
                log(&format!("{error}; running on the defaults"));
                (Config::default(), Some(error))
            }
        },
        None => (Config::default(), None),
    };
    let (endpoint, listener) = crate::socket::bind_socket(socket).map_err(Error::Socket)?;
    listener.set_nonblocking(true).map_err(Error::Setup)?;
    let children = crate::signal_pipe(&[SIGCHLD]).map_err(Error::Setup)?;
    let stops = crate::signal_pipe(&[SIGTERM, SIGINT, SIGHUP]).map_err(Error::Setup)?;
    let mut session = Session::new(config, socket.path().to_owned(), true);
    session.config_path = config_path;
    session.config_error = error;
    session.start().map_err(Error::Start)?;
    let mut server = Server {
        session,
        phase: Phase::Serving {
            listening: Listening {
                listener,
                _endpoint: endpoint,
            },
            resting: None,
        },
        conns: Vec::new(),
        children,
        killed: Vec::new(),
        stops,
        read_buffer: vec![0u8; 64 * 1024],
        paint_buffer: Vec::new(),
        slots: Vec::new(),
        ready: Vec::new(),
        spare: std::fs::File::open("/dev/null").ok(),
        shortage: None,
        accept_logged: None,
    };
    server.run();
    Ok(())
}

/// What a polled descriptor is. A connection's index into `conns` holds
/// for the whole tick: connections are only appended during it (`accept`)
/// and removed at its end (`close_conns`). Handlers still look each one up
/// with `get_mut`, as the lints ask.
#[derive(Clone, Copy)]
enum Slot {
    Listener,
    Children,
    Stops,
    Conn(usize),
    /// The terminal a client sent, which the server reads and writes.
    Tty(usize),
    Pane(PaneId),
}

impl Server {
    fn run(&mut self) {
        loop {
            let now = Instant::now();
            let due: Vec<Wake> = (self.wakes())
                .filter_map(|(at, wake)| (at <= now).then_some(wake))
                .collect();
            for wake in due {
                if self.wake(wake, now).is_break() {
                    return;
                }
            }
            // Input given to panes since the last poll is written now rather
            // than when the next poll says their terminals can take it.
            self.session.panes.values_mut().for_each(Pane::write_input);
            self.close_conns();
            // Each change settles as it is made; this catches output that
            // dropped rows a copy mode held.
            self.session.settle_if_needed();
            self.flush_outbox();
            if matches!(self.phase, Phase::Stopping(_))
                && self.session.dying.is_empty()
                && self.killed.is_empty()
                && self.conns.iter().all(|c| c.out.is_empty())
            {
                self.finish();
                return;
            }
            let timeout = (self.wakes().map(|(at, _)| at).min())
                .map(|at| at.saturating_duration_since(Instant::now()));
            let mut ready = std::mem::take(&mut self.ready);
            self.poll(timeout, &mut ready);
            let now = Instant::now();
            for &(slot, flags) in &ready {
                match slot {
                    Slot::Listener => self.accept(),
                    Slot::Children => {
                        crate::drain(&mut self.children);
                        self.reap();
                    }
                    Slot::Stops => {
                        crate::drain(&mut self.stops);
                        self.stop("stopped by a signal".into());
                    }
                    Slot::Conn(i) => self.serve_conn(i, flags, now),
                    Slot::Tty(i) => self.serve_tty(i, flags, now),
                    Slot::Pane(id) => self.serve_pane(id, flags),
                }
            }
            self.ready = ready;
        }
    }

    /// Everything the loop waits for, each with when it is due. Paints come
    /// last, after what may change what they show.
    fn wakes(&self) -> impl Iterator<Item = (Instant, Wake)> + '_ {
        let session = (self.session.timers()).map(|(at, timer)| (at, Wake::Session(timer)));
        let grace = (self.session.dying.iter()).map(|(pane, d)| (d.deadline, Wake::Grace(*pane)));
        let phase = match self.phase {
            Phase::Serving { resting, .. } => resting.map(|at| (at, Wake::Listen)),
            Phase::Stopping(by) => Some((by, Wake::Stop)),
        };
        let paints = (self.conns.iter().enumerate())
            .filter_map(|(i, conn)| Some((conn.paint_due(&self.session)?, Wake::Paint(i))));
        session.chain(grace).chain(phase).chain(paints)
    }

    /// What `wake` waited for is done, now that it is due at `now`; a break
    /// once the server has stopped.
    fn wake(&mut self, wake: Wake, now: Instant) -> ControlFlow<()> {
        match wake {
            Wake::Session(timer) => self.session.fire(timer),
            Wake::Grace(pane) => {
                let dying = self.session.dying.remove(&pane);
                self.killed.extend(dying.and_then(Dying::kill));
            }
            Wake::Listen => {
                if let Phase::Serving { resting, .. } = &mut self.phase {
                    *resting = None;
                }
            }
            Wake::Stop => {
                self.finish();
                return ControlFlow::Break(());
            }
            Wake::Paint(index) => {
                self.session.settle_if_needed();
                self.flush_outbox();
                self.paint(index, now);
            }
        }
        ControlFlow::Continue(())
    }

    /// Drops every connection that is dead, or closing with nothing left to
    /// send. Neither has a client attached: it was detached as it ended.
    fn close_conns(&mut self) {
        self.conns.retain(|conn| match conn.stage {
            Stage::Dead => false,
            Stage::Closing => !conn.out.is_empty(),
            Stage::Hello(_) | Stage::Attaching(_) | Stage::Attached(_) | Stage::Command => true,
        });
    }

    /// Waits for descriptors to be ready, or `timeout`; which are, and for
    /// what, go into `ready`.
    fn poll(&mut self, timeout: Option<Duration>, ready: &mut Vec<(Slot, PollFlags)>) {
        ready.clear();
        let slots = &mut self.slots;
        slots.clear();
        // Built afresh, as it borrows the descriptors.
        let mut fds = Vec::new();
        if let Phase::Serving {
            listening,
            resting: None,
        } = &self.phase
        {
            fds.push(PollFd::new(&listening.listener, PollFlags::IN));
            slots.push(Slot::Listener);
        }
        fds.push(PollFd::new(&self.children, PollFlags::IN));
        slots.push(Slot::Children);
        fds.push(PollFd::new(&self.stops, PollFlags::IN));
        slots.push(Slot::Stops);
        for (i, conn) in self.conns.iter().enumerate() {
            let mut flags = PollFlags::IN;
            if !conn.out.is_empty() {
                flags |= PollFlags::OUT;
            }
            fds.push(PollFd::new(&conn.stream, flags));
            slots.push(Slot::Conn(i));
            if let Stage::Attached(attached) = &conn.stage
                && let Some(tty) = &attached.tty
            {
                let mut flags = PollFlags::IN;
                if !tty.out.is_empty() {
                    flags |= PollFlags::OUT;
                }
                fds.push(PollFd::new(&tty.fd, flags));
                slots.push(Slot::Tty(i));
            }
        }
        for (id, pane) in &self.session.panes {
            if let Process::Reading(child) = &pane.process {
                let mut flags = PollFlags::IN;
                if !pane.input.is_empty() {
                    flags |= PollFlags::OUT;
                }
                fds.push(PollFd::new(&child.master, flags));
                slots.push(Slot::Pane(*id));
            }
        }
        match fuxix::poll::poll(&mut fds, timeout) {
            Ok(_) | Err(fuxix::Errno::INTR) => {}
            Err(error) => log(&format!("poll: {error}")),
        }
        ready.extend(
            fds.iter()
                .map(PollFd::revents)
                .zip(slots.iter().copied())
                .filter(|(flags, _)| !flags.is_empty())
                .map(|(flags, slot)| (slot, flags)),
        );
    }

    fn stop(&mut self, reason: String) {
        if let Phase::Stopping(_) = self.phase {
            return;
        }
        log(&reason);
        // The socket goes first: from now on a client finds no server.
        self.phase = Phase::Stopping(crate::after(Instant::now(), STOP_WAIT));
        self.session.shutdown();
        // Every connection still open ends, so no command runs on what is
        // left of the session.
        let exit = format!("the fux server stopped: {reason}");
        for conn in &mut self.conns {
            match conn.stage {
                Stage::Hello(_) | Stage::Attaching(_) | Stage::Attached(_) | Stage::Command => {
                    conn.end(&mut self.session, &ServerFrame::Exit(&exit));
                }
                Stage::Closing | Stage::Dead => {}
            }
        }
    }

    fn flush_outbox(&mut self) {
        for outgoing in std::mem::take(&mut self.session.outbox) {
            match outgoing {
                Outgoing::Bytes(client, bytes) => {
                    if let Some(conn) = self.conns.iter_mut().find(|c| c.serves(client))
                        && let Stage::Attached(attached) = &mut conn.stage
                    {
                        attached.paint(&mut conn.out, &bytes);
                    }
                }
                Outgoing::Exit(client, reason) => {
                    if let Some(conn) = self.conns.iter_mut().find(|c| c.serves(client)) {
                        conn.end(&mut self.session, &ServerFrame::Exit(&reason));
                    }
                }
                Outgoing::Shutdown(reason) => self.stop(reason),
            }
        }
    }

    /// Paints the client attached on connection `index`, whose paint is due.
    fn paint(&mut self, index: usize, now: Instant) {
        let session = &mut self.session;
        let Some(conn) = self.conns.get_mut(index) else {
            return;
        };
        let pending = conn.pending();
        let Stage::Attached(attached) = &mut conn.stage else {
            return;
        };
        if pending > OUTPUT_CAP {
            // A client that stops reading gets nothing more queued; once it
            // drains, one full repaint.
            attached.screen = Screen::Starved;
            return;
        }
        if attached.screen == Screen::Starved {
            attached.screen = Screen::Unknown;
        }
        let client = attached.client;
        // The title and bell marks first: the bar shows the marks.
        let before = session.before_paint(client);
        if !render::compose_into(
            session,
            client,
            &mut attached.spare,
            &mut attached.placement,
        ) {
            return;
        }
        if !before.is_empty() {
            attached.paint(&mut conn.out, &before);
        }
        let shown = (attached.screen == Screen::Shown).then_some(&attached.shown);
        // The same screen as the client shows: nothing to send, not even
        // the envelope, whose cursor hide and show would restart a blinking
        // cursor.
        if shown.is_none_or(|shown| !attached.spare.same_as(shown)) {
            self.paint_buffer.clear();
            render::paint_into(shown, &attached.spare, &mut self.paint_buffer);
            attached.paint(&mut conn.out, &self.paint_buffer);
            std::mem::swap(&mut attached.shown, &mut attached.spare);
            attached.screen = Screen::Shown;
            attached.clock.painted(now);
            // Written now rather than when the next poll says it can be: a
            // keystroke's echo goes out a round sooner.
            conn.flush(session);
        }
        if let Some(view) = session.views.get_mut(&client) {
            view.dirty = false;
        }
    }

    fn accept(&mut self) {
        // Accept until the backlog is empty, so one tick drains it.
        loop {
            // The spare makes room for this accept, and is taken back after
            // it: if it cannot be, the connection took the last descriptor.
            self.spare = None;
            let Phase::Serving { listening, .. } = &self.phase else {
                return;
            };
            let accepted = listening.listener.accept();
            let room = match std::fs::File::open("/dev/null") {
                Ok(spare) => {
                    self.spare = Some(spare);
                    true
                }
                Err(e) => !out_of_descriptors(&e),
            };
            match accepted {
                Ok((stream, _)) if !room => {
                    self.refuse(stream);
                    // Closing it freed a descriptor for the spare.
                    self.spare = std::fs::File::open("/dev/null").ok();
                }
                Ok((stream, _)) => {
                    if let Some(shortage) = self.shortage.take() {
                        log(&format!(
                            "file descriptors available again; {} connections were refused while they were short",
                            shortage.refused
                        ));
                    }
                    let euid = fuxix::process::geteuid();
                    match fuxix::socket::peer_uid(&stream) {
                        Ok(uid) if uid == euid => {}
                        Ok(uid) => {
                            log(&format!(
                                "refused a connection from uid {uid}; only uid {euid} may use this server"
                            ));
                            continue;
                        }
                        Err(error) => {
                            log(&format!(
                                "refused a connection whose peer could not be identified: {error}"
                            ));
                            continue;
                        }
                    }
                    if stream.set_nonblocking(true).is_err() {
                        continue;
                    }
                    self.conns.push(Conn::new(stream));
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => return,
                // A peer that gave up before it was accepted: the next may
                // be fine.
                Err(e)
                    if matches!(
                        e.kind(),
                        ErrorKind::Interrupted | ErrorKind::ConnectionAborted
                    ) =>
                {
                    continue;
                }
                Err(e) => {
                    // Even the spare's room was not enough, or something
                    // else persists. The listener stays readable, so it
                    // rests rather than being polled again at once.
                    let now = Instant::now();
                    if let Phase::Serving { resting, .. } = &mut self.phase {
                        *resting = Some(crate::after(now, ACCEPT_REST));
                    }
                    if self
                        .accept_logged
                        .is_none_or(|at| now.duration_since(at) >= REPORT_EVERY)
                    {
                        self.accept_logged = Some(now);
                        log(&format!(
                            "accept: {e}; listening again in {} ms",
                            ACCEPT_REST.as_millis()
                        ));
                    }
                    return;
                }
            }
        }
    }

    /// Tells a connection that there is no room for it, and closes it. The
    /// shortage is logged when it starts, then at most every `REPORT_EVERY`.
    fn refuse(&mut self, stream: UnixStream) {
        let euid = fuxix::process::geteuid();
        if fuxix::socket::peer_uid(&stream).is_ok_and(|uid| uid == euid)
            && stream.set_nonblocking(true).is_ok()
            && let Ok(bytes) = ServerFrame::Exit(NO_DESCRIPTORS).encode()
        {
            // A fresh socket's buffer holds the frame; if not, the peer
            // sees the connection close.
            let _ = (&stream).write_all(&bytes);
        }
        drop(stream);
        let now = Instant::now();
        let shortage = self.shortage.get_or_insert_with(|| {
            log("out of file descriptors: new connections are refused, each told why, until some close");
            Shortage {
                reported: (now, 0),
                refused: 0,
            }
        });
        shortage.refused = shortage.refused.saturating_add(1);
        let (at, then) = shortage.reported;
        if now.duration_since(at) >= REPORT_EVERY {
            log(&format!(
                "still out of file descriptors: {} connections refused in the last {} s",
                shortage.refused.saturating_sub(then),
                now.duration_since(at).as_secs()
            ));
            shortage.reported = (now, shortage.refused);
        }
    }

    /// The terminal a client sent: room to write what waits for it, or
    /// keys to read. A terminal that closed detaches its client, as a
    /// client whose terminal closes detaches itself.
    fn serve_tty(&mut self, index: usize, flags: PollFlags, now: Instant) {
        let session = &mut self.session;
        let Some(conn) = self.conns.get_mut(index) else {
            return;
        };
        if flags.contains(PollFlags::OUT) {
            conn.flush_tty(session);
        }
        // A client detached this tick, or a connection that died: its keys
        // are not taken.
        let Some(attached) = conn.attached() else {
            return;
        };
        if !flags.intersects(PollFlags::IN | PollFlags::HUP | PollFlags::ERR) {
            return;
        }
        // A terminal resized and then typed into: the keys come here, the
        // size by the client's `Resize`, which may come after them. The
        // size is read here first, so that a program sees the keys at the
        // size they were typed at, as when both came by the client.
        let Some(tty) = &attached.tty else { return };
        if let Some(size) = fuxix::terminal::window_size(&tty.fd)
            && session.resize(attached.client, size.rows.get(), size.cols.get())
        {
            attached.screen.forget();
        }
        let mut read = 0usize;
        let mut gone = false;
        while read < CONN_READ {
            let Some(tty) = &attached.tty else { return };
            let n = match fuxix::io::read(&tty.fd, &mut self.read_buffer) {
                Ok(0) => {
                    gone = true;
                    break;
                }
                Ok(n) => n,
                Err(fuxix::Errno::AGAIN) => break,
                Err(fuxix::Errno::INTR) => continue,
                Err(_) => {
                    gone = true;
                    break;
                }
            };
            read = read.saturating_add(n);
            // Less than the buffer holds: that was all there was, and the
            // poll says when there is more, without a read to find none.
            let all = n < self.read_buffer.len();
            attached.input(session, self.read_buffer.get(..n).unwrap_or_default(), now);
            if all {
                break;
            }
        }
        if gone {
            conn.end(session, &ServerFrame::Exit("detached: the terminal closed"));
        }
    }

    /// A client's connection is ready, as found at `now`.
    fn serve_conn(&mut self, index: usize, flags: PollFlags, now: Instant) {
        if flags.contains(PollFlags::OUT)
            && let Some(conn) = self.conns.get_mut(index)
        {
            conn.flush(&mut self.session);
        }
        if flags.intersects(PollFlags::IN | PollFlags::HUP | PollFlags::ERR) {
            self.read_conn(index, now);
        }
    }

    /// Reads what a client sent until it has sent no more, or `CONN_READ`
    /// bytes, or more than 256 whole frames, or a bad one; each whole frame
    /// is handled as it arrives, in order, and nothing after a bad one.
    fn read_conn(&mut self, index: usize, now: Instant) {
        let Some(conn) = self.conns.get_mut(index) else {
            return;
        };
        // Held apart while the frames it lends are handled.
        let mut decoder = std::mem::take(&mut conn.decoder);
        let (mut frames, mut read) = (0usize, 0usize);
        let closed = 'read: loop {
            let Some(conn) = self.conns.get_mut(index) else {
                break false;
            };
            // A client may send its terminal with its `Attach`.
            match fuxix::socket::recv_with_fd(&conn.stream, &mut self.read_buffer) {
                Ok((0, _)) => break true,
                Ok((n, fd)) => {
                    if let (Some(fd), Stage::Hello(passed) | Stage::Attaching(passed)) =
                        (fd, &mut conn.stage)
                    {
                        *passed = Some(fd);
                    }
                    read = read.saturating_add(n);
                    decoder.push(self.read_buffer.get(..n).unwrap_or_default());
                    loop {
                        match self.take_frame(index, &mut decoder, now) {
                            Ok(true) => frames = frames.saturating_add(1),
                            Ok(false) => break,
                            Err(error) => {
                                log(&format!("a client sent a bad frame: {error}"));
                                break 'read true;
                            }
                        }
                    }
                    if frames > 256 || read >= CONN_READ {
                        break false;
                    }
                }
                Err(fuxix::Errno::AGAIN) => break false,
                Err(fuxix::Errno::INTR) => {}
                Err(_) => break true,
            }
        };
        if let Some(conn) = self.conns.get_mut(index) {
            decoder.shrink(CONN_KEEP);
            conn.decoder = decoder;
            if closed {
                conn.enter(&mut self.session, Stage::Dead);
            }
        }
    }

    /// Takes the next whole frame `decoder` holds, decoded as the frames
    /// the connection's stage allows, and handles it; whether there was
    /// one. Any other frame is an error.
    fn take_frame(
        &mut self,
        index: usize,
        decoder: &mut Decoder,
        now: Instant,
    ) -> Result<bool, crate::protocol::Error> {
        let session = &mut self.session;
        let Some(conn) = self.conns.get_mut(index) else {
            return Ok(false);
        };
        match &mut conn.stage {
            Stage::Hello(passed) => {
                let Some(hello) = decoder.frame::<Hello>()? else {
                    return Ok(false);
                };
                let passed = passed.take();
                let ours = Hello {
                    protocol: PROTOCOL,
                    version: env!("CARGO_PKG_VERSION"),
                    role: hello.role,
                };
                conn.send(session, &ServerFrame::Hello(ours));
                match (Mismatch::between(&hello, &ours), hello.role) {
                    (Err(mismatch), _) => {
                        conn.end(session, &ServerFrame::Exit(&mismatch.to_string()));
                    }
                    (Ok(()), Role::Kill) => {
                        conn.end(session, &ServerFrame::Done { status: 0 });
                        self.stop("stopped by fux kill-server".into());
                    }
                    (Ok(()), Role::Attach) => conn.enter(session, Stage::Attaching(passed)),
                    (Ok(()), Role::Command) => conn.enter(session, Stage::Command),
                }
            }
            Stage::Attaching(passed) => {
                let Some(Attach {
                    rows,
                    cols,
                    workspace,
                }) = decoder.frame()?
                else {
                    return Ok(false);
                };
                let passed = passed.take();
                match session.attach(rows.get(), cols.get(), workspace) {
                    Ok(client) => {
                        // The client's terminal, if it sent it: taken, the
                        // client is told before anything is painted.
                        let tty = passed.map(|fd| fuxix::terminal::reopen(&fd).ok());
                        let taken = tty.as_ref().map(Option::is_some);
                        let tty = tty.flatten().map(TakenTerminal::new);
                        conn.enter(
                            session,
                            Stage::Attached(Box::new(Attached::new(client, tty))),
                        );
                        if let Some(taken) = taken {
                            conn.send(session, &ServerFrame::Terminal { taken });
                        }
                    }
                    Err(error) => conn.end(session, &ServerFrame::Exit(&error.to_string())),
                }
            }
            Stage::Attached(attached) => match decoder.frame()? {
                // Input goes to the session as the decoder lends it.
                Some(AttachedFrame::Input(bytes)) => attached.input(session, bytes, now),
                // The client's terminal was resized, which may have moved
                // what it shows even if the size it ends at is the same:
                // forgotten, it is repainted in full either way.
                Some(AttachedFrame::Resize { rows, cols }) => {
                    attached.screen.forget();
                    session.resize(attached.client, rows.get(), cols.get());
                }
                Some(AttachedFrame::Detach) => conn.end(session, &ServerFrame::Exit("detached")),
                None => return Ok(false),
            },
            Stage::Command => {
                let Some(Command { argv, cwd, pane }) = decoder.frame()? else {
                    return Ok(false);
                };
                let origin = Origin::Cli {
                    pane: pane.and_then(|p| crate::command::parse_pane(&p).ok()),
                    cwd: Some(PathBuf::from(cwd)).filter(|p| p.is_dir()),
                };
                let outcome = session.run(&argv, &origin);
                // Nothing is sent for no output.
                let stdout = outcome.stdout.as_bytes();
                ServerFrame::split_into(stdout, ServerFrame::Stdout, &mut conn.out);
                if !outcome.stderr.is_empty() {
                    let mut stderr = outcome.stderr;
                    if !stderr.ends_with('\n') {
                        stderr.push('\n');
                    }
                    ServerFrame::split_into(stderr.as_bytes(), ServerFrame::Stderr, &mut conn.out);
                }
                conn.end(
                    session,
                    &ServerFrame::Done {
                        status: outcome.status,
                    },
                );
            }
            // A connection that is ending has had its last word: what it
            // sends after a Detach, a Command, a refused Hello or a bad
            // frame is dropped unread.
            Stage::Closing | Stage::Dead => {
                *decoder = Decoder::default();
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn serve_pane(&mut self, id: PaneId, flags: PollFlags) {
        if flags.contains(PollFlags::OUT)
            && let Some(pane) = self.session.panes.get_mut(&id)
        {
            pane.write_input();
        }
        if flags.intersects(PollFlags::IN | PollFlags::HUP | PollFlags::ERR) {
            self.read_pane(id);
        }
    }

    fn read_pane(&mut self, id: PaneId) {
        let mut total = 0;
        let mut buffer = [0u8; 16384];
        let mut ended = false;
        while total < PANE_READ {
            let Some(Process::Reading(child)) = self.session.panes.get(&id).map(|p| &p.process)
            else {
                return;
            };
            match fuxix::io::read(&child.master, &mut buffer) {
                Ok(0) => {
                    ended = true;
                    break;
                }
                Ok(n) => {
                    // Past PANE_READ by at most one buffer, when the loop ends.
                    total = total.saturating_add(n);
                    self.session.output(id, buffer.get(..n).unwrap_or_default());
                    // A little, as an echo or a prompt is: that was all,
                    // and the poll says when there is more, without a read
                    // to find none. A flood's reads are larger, and go on.
                    if n < SHORT_READ {
                        break;
                    }
                }
                Err(fuxix::Errno::AGAIN) => break,
                Err(fuxix::Errno::INTR) => continue,
                // EIO: the slave side is closed; the program is gone.
                Err(_) => {
                    ended = true;
                    break;
                }
            }
        }
        if total > 0 {
            let now = Instant::now();
            for attached in self.conns.iter_mut().filter_map(Conn::attached) {
                attached.clock.output(id, now);
            }
        }
        if ended {
            self.reap();
            if let Some(pane) = self.session.panes.get_mut(&id) {
                pane.process.hang_up();
            }
        }
    }

    /// Checks every killed program and every pane's, since signals
    /// coalesce.
    fn reap(&mut self) {
        self.reap_killed();
        let exited: Vec<(PaneId, i32)> = self
            .session
            .panes
            .iter()
            .filter_map(|(id, pane)| Some((*id, pane.process.child()?.leader.exited()?)))
            .collect();
        for (id, status) in exited {
            self.session.exited(id, status);
        }
    }

    fn reap_killed(&mut self) {
        let killed = std::mem::take(&mut self.killed);
        self.killed = killed
            .into_iter()
            .filter_map(|l| l.finish().err())
            .collect();
    }

    /// Kills every closed pane's program, and waits for them, at most a
    /// second: a process stuck in the kernel is left to init rather than
    /// hold the server's exit.
    fn finish(&mut self) {
        let dying = std::mem::take(&mut self.session.dying);
        self.killed
            .extend(dying.into_values().filter_map(Dying::kill));
        for _ in 0..500 {
            self.reap_killed();
            if self.killed.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

/// Whether an error is a shortage of descriptors, the process's or the
/// system's.
fn out_of_descriptors(error: &std::io::Error) -> bool {
    matches!(
        fuxix::Errno::from_io_error(error),
        Some(fuxix::Errno::MFILE | fuxix::Errno::NFILE)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A session with a client attached on a connection whose terminal the
    /// server took, the other end of a socket pair standing in for it: the
    /// session, the connection, the client's end of its socket, and the
    /// terminal's other end.
    fn with_terminal() -> Result<(Session, Conn, UnixStream, UnixStream), String> {
        let (session, client) = crate::session::testing::attached(10, 40)?;
        let (stream, peer) = UnixStream::pair().map_err(|e| e.to_string())?;
        let (tty, far) = UnixStream::pair().map_err(|e| e.to_string())?;
        tty.set_nonblocking(true).map_err(|e| e.to_string())?;
        let mut conn = Conn::new(stream);
        let tty = TakenTerminal::new(OwnedFd::from(tty));
        conn.stage = Stage::Attached(Box::new(Attached::new(client, Some(tty))));
        Ok((session, conn, peer, far))
    }

    /// The frames waiting for the client: what is painted in them, and the
    /// `Exit`'s reason.
    fn sent(conn: &Conn) -> (Vec<u8>, Option<String>) {
        let mut decoder = Decoder::default();
        decoder.push(conn.out.as_slice());
        let mut painted = Vec::new();
        let mut exit = None;
        while let Ok(Some(frame)) = decoder.frame() {
            if let ServerFrame::Paint(bytes) = frame {
                painted.extend_from_slice(bytes);
            } else if let ServerFrame::Exit(reason) = frame {
                exit = Some(reason.to_owned());
            }
        }
        (painted, exit)
    }

    /// A client whose screen is forgotten, as on a resize that ends at the
    /// same size, is due a full paint though nothing it views changed.
    #[test]
    fn a_forgotten_screen_is_due_a_paint() -> Result<(), String> {
        let (mut session, mut conn, _peer, _far) = with_terminal()?;
        session.views.values_mut().for_each(|v| v.dirty = false);
        if let Some(attached) = conn.attached() {
            attached.screen = Screen::Shown;
        }
        assert_eq!(conn.paint_due(&session), None, "nothing to paint");
        if let Some(attached) = conn.attached() {
            attached.screen.forget();
        }
        assert!(conn.paint_due(&session).is_some());
        Ok(())
    }

    /// A terminal given back that takes no more at once (a slow link) has
    /// what it did not take sent to the client in paint frames before the
    /// `Exit`, rather than dropped.
    #[test]
    fn what_a_terminal_given_back_did_not_take_goes_to_the_client() -> Result<(), String> {
        let (mut session, mut conn, _peer, _far) = with_terminal()?;
        // Fill the stand-in until it takes no more.
        if let Some(tty) = conn.attached().and_then(|a| a.tty.as_mut()) {
            tty.out.push(&vec![b'x'; 4 << 20]);
            tty.out.push(b"end");
        }
        conn.flush_tty(&mut session);
        conn.end(&mut session, &ServerFrame::Exit("detached"));
        let (painted, exit) = sent(&conn);
        assert!(painted.ends_with(b"end"), "the last of what waited");
        assert_eq!(exit.as_deref(), Some("detached"));
        assert!(session.views.is_empty(), "the client is detached");
        Ok(())
    }

    /// A terminal that can no longer be written is given up and its client
    /// detached, as one that can no longer be read is: no one would read
    /// its keys.
    #[test]
    fn a_terminal_that_cannot_be_written_detaches_its_client() -> Result<(), String> {
        let (mut session, mut conn, _peer, far) = with_terminal()?;
        drop(far);
        if let Some(tty) = conn.attached().and_then(|a| a.tty.as_mut()) {
            tty.out.push(b"paint");
        }
        conn.flush_tty(&mut session);
        assert!(matches!(conn.stage, Stage::Closing), "the client is let go");
        assert!(session.views.is_empty(), "and detached");
        let (_, exit) = sent(&conn);
        assert_eq!(exit.as_deref(), Some("detached: the terminal closed"));
        Ok(())
    }
}
