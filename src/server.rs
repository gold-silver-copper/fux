//! The server: one thread (but for fuxix's watchdog while a PTY opens on
//! macOS), one `poll` loop over the listening socket, a signal pipe, every
//! client and every pane's PTY.
use crate::after;
use crate::bytes::ByteQueue;
use crate::code::Code;
use crate::events::{BACKLOG, Ending, Subscription};
use crate::id::{ClientId, PaneId};
use crate::layout::Placement;
use crate::pane::{Pane, Process};
use crate::process::Pid;
use crate::protocol::{
    Asked, Attach, AttachedFrame, Context, Decoder, Frame, Greeting, Hello, Mismatch, Reply, Role,
    ToAttached, ToCommand, split_into,
};
use crate::render::{self, Grid};
use crate::session::{Begun, Caller, Dying, Origin, Outgoing, Session, Timer, Within};
use crate::socket::{Endpoint, SocketPath};
use crate::view::ClientKind;
use crate::wait::Waiting;
use fuxix::Errno;
use fuxix::poll::{Events as PollFlags, PollFd};
use signal_hook::consts::SIGCHLD;
use std::fs;
use std::io::{ErrorKind, Write};
use std::mem;
use std::ops::ControlFlow;

use std::os::unix::net::{UnixListener, UnixStream};

use std::time::{Duration, Instant};

/// Writes to the server's log, its stderr: `log!("…", args)` expands to
/// `writeln!(stderr(), "fux server: {}", format_args!("…", args))`. A log
/// that cannot be written, a full disk, say, loses the line and stops nothing.
macro_rules! log {
    ($($arg:tt)*) => {{
        let _ = writeln!(std::io::stderr(), "fux server: {}", format_args!($($arg)*));
    }};
}

/// The least time between two paints for a client, as a flood is painted.
/// A keystroke's echo (`PaintClock::echo`) and panes gone quiet (`QUIET`) are
/// painted sooner.
const PAINT: Duration = Duration::from_millis(16);
/// Panes quiet for this after their output: painted then, not held to
/// `PAINT`. A program's output in a few bursts, as a screen redrawn is,
/// shows when it ends; a flood, never quiet so long, keeps to `PAINT`.
const QUIET: Duration = Duration::from_millis(1);
/// A pane's read shorter than this is taken for all its terminal had: the
/// next is left to the poll.
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

/// The socket the server is found by: held only while it serves, so the
/// moment it stops, its listener closes and the socket is gone, and a
/// client finds no server rather than one that never answers.
struct Listening {
    listener: UnixListener,
    /// Holds the lock; removes the socket once dropped, after the listener
    /// closes.
    _endpoint: Endpoint,
}

impl Listening {
    /// The socket at `socket` bound and listened on.
    fn open(socket: &SocketPath) -> Result<Listening, Error> {
        let (endpoint, listener) = crate::socket::bind_socket(socket).map_err(Error::Socket)?;
        listener.set_nonblocking(true).map_err(Error::Setup)?;
        Ok(Listening {
            listener,
            _endpoint: endpoint,
        })
    }
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
    Grace,
    /// The listener's rest is over.
    Listen,
    /// A stopping server waits no more.
    Stop,
    /// A connection's client may be painted (`Conn::paint_due`).
    Paint(usize),
    /// The loop runs again, which after every wake asks the waits for
    /// their answers (`Waiting::due`).
    Look,
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
    /// answer to send its `Attach`.
    Hello,
    /// It said hello as `fux attach`: its `Attach` is next, and the kind
    /// of client it is.
    Attaching(ClientKind),
    /// Its client is attached: its input, resizes and detach come next.
    Attached(Box<Attached>),
    /// It said hello as a command client: its requests come next, each
    /// answered, and the answer written, before the next is read
    /// (`Conn::reads`): a client that never reads its answers is read no
    /// more, and holds no more memory than one answer.
    Command,
    /// Its request waits for its pane (`wait`): it is answered once the
    /// wait has an answer. It is read only to notice the client go.
    Waiting(Box<Waiting>),
    /// Its request streams events (`events`) until the client goes, or the
    /// server stops. It is read only to notice the client go.
    Events(Box<Subscription>),
    /// Its last frame is sent: it closes once what waits is written, and
    /// is read no more, so a client that has stopped sending, or shut its
    /// side, still gets it all.
    Closing,
    /// It failed or hung up: it closes at the end of the tick.
    Dead,
}

impl Stage {
    /// Whether what the client sends is read.
    fn reads(&self) -> bool {
        match self {
            Stage::Hello
            | Stage::Attaching(_)
            | Stage::Attached(_)
            | Stage::Command
            | Stage::Waiting(_)
            | Stage::Events(_) => true,
            Stage::Closing | Stage::Dead => false,
        }
    }

    /// Whether a request of its is under way: waiting or streaming.
    fn asked(&self) -> bool {
        match self {
            Stage::Waiting(_) | Stage::Events(_) => true,
            Stage::Hello
            | Stage::Attaching(_)
            | Stage::Attached(_)
            | Stage::Command
            | Stage::Closing
            | Stage::Dead => false,
        }
    }
}

/// An attached client: its view's id, and what its paints are made from.
/// Every paint is relayed to it in `Paint` frames, which it writes to its
/// terminal.
struct Attached {
    client: ClientId,
    /// What the client's terminal shows, for diffing, while `known`.
    shown: Grid,
    /// Whether `shown` is what the terminal shows: if not (a new client, a
    /// resize), the next paint is a full one.
    known: bool,
    /// The grid the next paint is composed into, then swapped with `shown`:
    /// the two are reused by every paint, as is the placement of its panes.
    spare: Grid,
    placement: Placement,
    /// When the next paint may be made.
    clock: PaintClock,
    /// The pictures its terminal was last sent, as they show there, and
    /// their sixel data.
    pictures: Vec<render::Visible>,
    sixels: render::Sixels,
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
        self.next = after(now, PAINT);
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
            self.due = after(now, QUIET).min(self.next);
        }
    }
}

/// Writes `out` to `fd`, nonblocking, as far as it takes it; false if the
/// write failed. The memory a large write took is given back.
fn write_out(fd: impl std::os::fd::AsFd, out: &mut ByteQueue) -> bool {
    while !out.is_empty() {
        match fuxix::io::write(&fd, out.as_slice()) {
            Ok(0) | Err(Errno::AGAIN) => break,
            Ok(n) => out.take(n),
            Err(Errno::INTR) => continue,
            Err(_) => return false,
        }
    }
    out.shrink(CONN_KEEP);
    true
}

impl Attached {
    fn new(client: ClientId) -> Attached {
        let now = Instant::now();
        Attached {
            client,
            shown: Grid::new(0, 0),
            known: false,
            spare: Grid::new(0, 0),
            placement: Placement::default(),
            pictures: Vec::new(),
            sixels: render::Sixels::default(),
            clock: PaintClock {
                next: now,
                due: now,
                echo: None,
            },
        }
    }

    /// Actions from the client, to the session: a key's pane, the client's
    /// focus, is the one whose echo is painted at once.
    fn act(&mut self, session: &mut Session, act: crate::act::Act) {
        let typed = matches!(act, crate::act::Act::Key(_) | crate::act::Act::Paste(_));
        session.acts(self.client, vec![act]);
        if typed {
            self.clock.echo = session.focused(self.client);
        }
    }

    /// Bytes for the client's terminal, a paint's or what the session sends
    /// it outside one, framed in `out` for the client.
    fn paint(out: &mut ByteQueue, bytes: &[u8]) {
        split_into(bytes, ToAttached::Paint, out);
    }
}

impl Conn {
    fn new(stream: UnixStream) -> Conn {
        Conn {
            stream,
            decoder: Decoder::default(),
            out: ByteQueue::default(),
            stage: Stage::Hello,
        }
    }

    /// Whether what the client sends is read: by its stage, and for a
    /// command client only once its last answer is all written.
    fn reads(&self) -> bool {
        self.stage.reads() && (self.out.is_empty() || !matches!(self.stage, Stage::Command))
    }

    /// When the client attached here may be painted: if its view changed,
    /// and once what was sent it before is all written. One frame is in
    /// flight at most, so a client that reads slowly gets fewer, newer
    /// frames, and one that stops reading has nothing more queued.
    fn paint_due(&self, session: &Session) -> Option<Instant> {
        let Stage::Attached(attached) = &self.stage else {
            return None;
        };
        let dirty = session.views.get(&attached.client)?.dirty;
        // A screen not known is painted whole, whether or not the view
        // changed: forgetting it is asking for that paint.
        ((dirty || !attached.known) && self.out.is_empty()).then_some(attached.clock.due)
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
    /// client detached and its terminal given its own cursor shape back;
    /// one that is dead stays so.
    fn enter(&mut self, session: &mut Session, next: Stage) {
        if matches!(self.stage, Stage::Dead) {
            return;
        }
        if let Stage::Attached(attached) = mem::replace(&mut self.stage, next) {
            session.detach(attached.client);
            // A cursor shape a pane set is given back as the terminal's own
            // (`render::cursor_shape`); one never set is left as it was.
            let mut shape = Vec::new();
            let initial = fux_vt::CursorShape::Initial;
            render::cursor_shape(attached.shown.modes.cursor_shape, initial, &mut shape);
            if !shape.is_empty() {
                Attached::paint(&mut self.out, &shape);
            }
        }
    }

    /// Encodes a frame straight into the output; a connection whose frame
    /// cannot be encoded fails.
    fn send<'a>(&mut self, session: &mut Session, frame: &impl Frame<'a>) {
        if self.out.push_with(|out| frame.encode_into(out)).is_err() {
            self.enter(session, Stage::Dead);
        }
    }

    /// Its client attaches as `attach` says: it is told its id before
    /// anything is painted; or why it cannot attach.
    fn attach(&mut self, session: &mut Session, kind: ClientKind, attach: Attach) {
        match session.attach_as(kind, attach) {
            Ok(client) => {
                let attached = Attached::new(client);
                self.enter(session, Stage::Attached(Box::new(attached)));
                self.send(session, &ToAttached::Welcome(client));
            }
            Err(error) => self.end(session, &ToAttached::Exit(&error.to_string())),
        }
    }

    /// Answers a request with its outcome, encoded once and sent in pieces
    /// as long as fit, then their end; and takes the next.
    fn answer(&mut self, session: &mut Session, reply: Reply) {
        let bytes = crate::wire::encode(&reply);
        split_into(&bytes, ToCommand::Part, &mut self.out);
        self.send(session, &ToCommand::PartsEnd);
        if self.stage.asked() {
            self.enter(session, Stage::Command);
        }
    }

    /// Ends a stream of events so, `json` or not: its last line, then its
    /// reply.
    fn ends(&mut self, session: &mut Session, json: bool, ending: &Ending) {
        let line = ending.line(json);
        split_into(line.as_bytes(), ToCommand::Stdout, &mut self.out);
        self.answer(session, ending.reply());
    }

    /// Sends the connection's last frame, its client detached and its
    /// terminal given back first: it closes once what waits is sent.
    fn end<'a>(&mut self, session: &mut Session, frame: &impl Frame<'a>) {
        self.enter(session, Stage::Closing);
        self.send(session, frame);
    }

    /// Sends the client attached here `frame`; an `Exit` as its last
    /// frame.
    fn tell(&mut self, session: &mut Session, frame: &ToAttached) {
        match frame {
            ToAttached::Paint(bytes) => {
                if let Stage::Attached(_) = &self.stage {
                    Attached::paint(&mut self.out, bytes);
                }
            }
            ToAttached::Exit(_) => self.end(session, frame),
            ToAttached::Welcome(..)
            | ToAttached::Ended(_)
            | ToAttached::Run(_)
            | ToAttached::Keys(_) => self.send(session, frame),
        }
    }

    /// Writes what waits for the client until it is all written or the
    /// socket takes no more; a connection that fails is dead.
    fn flush(&mut self, session: &mut Session) {
        if !write_out(&self.stream, &mut self.out) {
            self.enter(session, Stage::Dead);
        }
    }
}

pub struct Server {
    session: Session,
    phase: Phase,
    conns: Vec<Conn>,
    children: crate::SignalPipe,
    /// Closed panes' programs killed but not yet exited, reaped once SIGCHLD
    /// says they have.
    killed: Vec<Pid>,
    stops: crate::SignalPipe,
    /// Where bytes read from a client's connection or terminal land before
    /// they are taken; one for the server, reused by every read.
    read_buffer: Vec<u8>,
    /// Where a paint is made before it is sent; reused by every paint.
    paint_buffer: Vec<u8>,
    /// What each descriptor polled is, and then what is ready; reused by
    /// every tick.
    slots: Vec<Slot>,
    ready: Vec<(Slot, PollFlags)>,
    /// When an `accept` failure was last logged.
    accept_logged: Option<Instant>,
}

/// Why a server could not start.
#[derive(Debug)]
pub enum Error {
    Socket(crate::socket::Error),
    /// The listener or the signal pipes could not be set up.
    Setup(std::io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Socket(error) => error.fmt(f),
            Error::Setup(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for Error {}

/// Runs a server on `socket` until it is told to stop or its last pane
/// closes.
pub fn serve(socket: &SocketPath) -> Result<(), Error> {
    let children = crate::SignalPipe::new(&[SIGCHLD]).map_err(Error::Setup)?;
    // Orphans the panes' programs leave stay the server's to read and reap.
    if let Err(errno) = fuxix::process::adopt_orphans() {
        log!("adopting orphans: {errno}");
    }
    let stops = crate::SignalPipe::new(&crate::STOPS).map_err(Error::Setup)?;
    // Settings come from clients (`set`), each running its config.
    let run = crate::command::Run::now().ok_or_else(|| {
        Error::Setup(std::io::Error::other(
            "the system clock is before 1970 or past 2554: this run cannot be told from another",
        ))
    })?;
    let session = Session::new(
        crate::config::Config::default(),
        socket.path().to_owned(),
        true,
        run,
    );
    // Last of what can fail: once it listens, a server that started
    // with it finds it there (`socket::bind_socket`).
    let listening = Listening::open(socket)?;
    // A server starts with nothing in it: the first client to attach
    // starts its first shell, on its screen, and a request makes a pane
    // only at a size it knows.
    let mut server = Server {
        session,
        phase: Phase::Serving {
            listening,
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
        accept_logged: None,
    };
    ready();
    server.run();
    Ok(())
}

/// The line that says the server listens, which the client that started
/// it waits for (`client::start_server`): written on stdout, which then
/// points at `/dev/null`, so the client's pipe closes and no descriptor
/// opened later is 1. Neither can fail in a way that would stop a server
/// that already serves: a client that gave up reading has gone.
fn ready() {
    let mut stdout = std::io::stdout();
    let _ = stdout.write_all(READY).and_then(|()| stdout.flush());
    match fs::File::open("/dev/null") {
        Ok(null) => {
            if let Err(errno) = fuxix::io::redirect_stdout(&null) {
                log!("stdout to /dev/null: {errno}");
            }
        }
        Err(error) => log!("/dev/null: {error}"),
    }
}

/// What a server says on stdout once it listens.
pub(crate) const READY: &[u8] = b"fux server: listening\n";

/// What a polled descriptor is. A connection's index into `conns` holds
/// for the whole tick: connections are only appended during it (`accept`)
/// and removed at its end (`close_conns`). Handlers still look each one up
/// with `get_mut`, as the lints ask.
#[derive(Clone, Copy)]
enum Slot {
    /// The socket listened on.
    Listener,
    Children,
    Stops,
    Conn(usize),
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
            // What the last round read or changed may be what a wait waits
            // for, or an event to tell.
            self.answer_waits(now);
            self.tell_events(false);
            // Input given to panes since the last poll is written now rather
            // than when the next poll says their terminals can take it.
            let mut wrote = false;
            self.session
                .parts
                .each_pane_mut(|p| wrote |= p.write_input());
            self.wrote_input(wrote);
            self.close_conns();
            // Each change settles as it is made; this catches output that
            // dropped rows a copy mode held.
            self.session.hold_copies();
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
            let mut ready = mem::take(&mut self.ready);
            self.poll(timeout, &mut ready);
            for &(slot, flags) in &ready {
                match slot {
                    Slot::Listener => self.accept(),
                    Slot::Children => {
                        crate::drain(self.children.pipe());
                        self.reap();
                    }
                    Slot::Stops => {
                        crate::drain(self.stops.pipe());
                        self.stop("stopped by a signal".into());
                    }
                    Slot::Conn(i) => self.serve_conn(i, flags),
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
        let grace =
            (self.session.dying.iter().map(|d| d.deadline).min()).map(|at| (at, Wake::Grace));
        let phase = match self.phase {
            Phase::Serving { resting, .. } => resting.map(|at| (at, Wake::Listen)),
            Phase::Stopping(by) => Some((by, Wake::Stop)),
        };
        let paints = (self.conns.iter().enumerate())
            .filter_map(|(i, conn)| Some((conn.paint_due(&self.session)?, Wake::Paint(i))));
        let answers = self.conns.iter().filter_map(|conn| {
            let Stage::Waiting(waiting) = &conn.stage else {
                return None;
            };
            Some((waiting.due(), Wake::Look))
        });
        session
            .chain(grace)
            .chain(phase)
            .chain(answers)
            .chain(paints)
    }

    /// What `wake` waited for is done, now that it is due at `now`; a break
    /// once the server has stopped.
    fn wake(&mut self, wake: Wake, now: Instant) -> ControlFlow<()> {
        match wake {
            Wake::Session(timer) => self.session.fire(timer),
            Wake::Grace => {
                let (due, dying) = mem::take(&mut self.session.dying)
                    .into_iter()
                    .partition(|d| d.deadline <= now);
                self.session.dying = dying;
                self.killed.extend(due.into_iter().filter_map(Dying::kill));
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
                self.session.hold_copies();
                self.flush_outbox();
                self.paint(index, now);
            }
            // The loop answers waits after every wake: these only set when
            // it wakes.
            Wake::Look => {}
        }
        ControlFlow::Continue(())
    }

    /// Answers each wait that has its answer.
    fn answer_waits(&mut self, now: Instant) {
        for conn in &mut self.conns {
            let Stage::Waiting(waiting) = &mut conn.stage else {
                continue;
            };
            if let Some(outcome) = waiting.answer(&self.session, now) {
                conn.answer(&mut self.session, outcome);
            }
        }
    }

    /// Tells each stream of events what the journal holds that it has not
    /// read, as far as its output has room (`all`: room or none): a reader
    /// too slow is told the rest as it catches up, or how much of it the
    /// journal let go.
    fn tell_events(&mut self, all: bool) {
        for conn in &mut self.conns {
            let Stage::Events(subscription) = &mut conn.stage else {
                continue;
            };
            let room = match all {
                true => usize::MAX,
                false => BACKLOG.saturating_sub(conn.out.len()),
            };
            let session = &self.session;
            let holder = |target| session.holder(target);
            let (lines, gone) = subscription.told(session.parts.journal(), room, holder);
            let json = subscription.json;
            split_into(lines.as_bytes(), ToCommand::Stdout, &mut conn.out);
            if let Some(ending) = gone {
                conn.ends(&mut self.session, json, &ending);
            }
        }
    }

    /// Drops every connection that is dead, or closing with nothing left to
    /// send. Neither has a client attached: it was detached as it ended.
    fn close_conns(&mut self) {
        self.conns.retain(|conn| {
            conn.stage.reads() || matches!(conn.stage, Stage::Closing) && !conn.out.is_empty()
        });
    }

    /// Waits for descriptors to be ready, or `timeout`; which are, and for
    /// what, go into `ready`.
    fn poll(&mut self, timeout: Option<Duration>, ready: &mut Vec<(Slot, PollFlags)>) {
        ready.clear();
        let slots = &mut self.slots;
        slots.clear();
        // Built afresh, as it borrows the descriptors; each with its slot.
        let mut fds = Vec::new();
        let mut add = |fd, slot| {
            fds.push(fd);
            slots.push(slot);
        };
        if let Phase::Serving {
            listening,
            resting: None,
        } = &self.phase
        {
            add(
                PollFd::new(&listening.listener, PollFlags::IN),
                Slot::Listener,
            );
        }
        add(
            PollFd::new(self.children.pipe(), PollFlags::IN),
            Slot::Children,
        );
        add(PollFd::new(self.stops.pipe(), PollFlags::IN), Slot::Stops);
        for (i, conn) in self.conns.iter().enumerate() {
            // A connection that is not read is polled only for what it
            // has to write. One with nothing to write either (an attach
            // held) asks for nothing: a hang-up is told unasked (POSIX),
            // where the system tells one at all; else it is found as the
            // connection is read again. Asking for a hang-up would not do:
            // macOS then wakes for each byte waiting unread, and tells a
            // peer that shut only its side as hung up.
            let mut flags = match conn.reads() {
                true => PollFlags::IN,
                false => PollFlags::NONE,
            };
            if !conn.out.is_empty() {
                flags |= PollFlags::OUT;
            }
            add(PollFd::new(&conn.stream, flags), Slot::Conn(i));
        }
        for (id, pane) in self.session.parts.panes() {
            if let Process::Reading(child) = &pane.process() {
                // Output waiting to be read (`Pane::has_unread`) is read
                // first: until it is, no more is taken from the program.
                let mut flags = match pane.has_unread() {
                    true => PollFlags::HUP,
                    false => PollFlags::IN,
                };
                if !pane.input.is_empty() {
                    flags |= PollFlags::OUT;
                }
                add(PollFd::new(&child.master, flags), Slot::Pane(*id));
            }
        }
        match fuxix::poll::poll(&mut fds, timeout) {
            Ok(_) | Err(Errno::INTR) => {}
            Err(error) => log!("poll: {error}"),
        }
        ready.extend(
            fds.iter()
                .map(PollFd::revents)
                .zip(slots.iter().copied())
                .filter(|(flags, _)| !flags.is_empty())
                .map(|(flags, slot)| (slot, flags)),
        );
    }

    /// Stops the server, saying why.
    fn stop(&mut self, reason: String) {
        if let Phase::Stopping(_) = self.phase {
            return;
        }
        log!("{reason}");
        // The sockets go first: from now on a client finds no server.
        self.phase = Phase::Stopping(after(Instant::now(), STOP_WAIT));
        // A stream is told all the journal holds for it, and says why it
        // ends, as one of its lines, and ends as a command that is done.
        self.tell_events(true);
        for conn in &mut self.conns {
            if let Stage::Events(subscription) = &conn.stage {
                let json = subscription.json;
                conn.ends(&mut self.session, json, &Ending::Stopped(reason.clone()));
            }
        }
        self.session.shutdown();
        // Every connection still open ends, so no command runs on what is
        // left of the session.
        let exit = format!("the fux server stopped: {reason}");
        for conn in &mut self.conns {
            let session = &mut self.session;
            match conn.stage {
                Stage::Hello => conn.end(session, &Greeting::Exit(&exit)),
                Stage::Attaching(_) | Stage::Attached(_) => {
                    conn.end(session, &ToAttached::Exit(&exit));
                }
                Stage::Command | Stage::Waiting(_) | Stage::Events(_) => {
                    conn.end(session, &ToCommand::Exit(&exit));
                }
                Stage::Closing | Stage::Dead => {}
            }
        }
    }

    fn flush_outbox(&mut self) {
        for outgoing in mem::take(&mut self.session.outbox) {
            match outgoing {
                Outgoing::Shutdown(reason) => self.stop(reason),
                Outgoing::To(client, frame) => {
                    if let Some(conn) = self.conns.iter_mut().find(|c| c.serves(client)) {
                        conn.tell(&mut self.session, &frame);
                    }
                }
            }
        }
    }

    /// Paints the client attached on connection `index`, whose paint is due.
    fn paint(&mut self, index: usize, now: Instant) {
        let session = &mut self.session;
        let Some(conn) = self.conns.get_mut(index) else {
            return;
        };
        let Stage::Attached(attached) = &mut conn.stage else {
            return;
        };
        let client = attached.client;
        // The title and bell marks first: the bar shows the marks.
        let before = session.before_paint(client);
        let Some(view) = session.views.get(&client) else {
            return;
        };
        // A pane part way through a frame shows as it was last painted
        // (render::compose_held), its pictures too.
        let placed = attached.placement.panes.clone();
        let painted = (attached.known).then_some(render::Painted {
            grid: &attached.shown,
            panes: &placed,
        });
        render::compose_held(
            session,
            view,
            &mut attached.spare,
            &mut attached.placement,
            painted,
        );
        if !before.is_empty() {
            Attached::paint(&mut conn.out, &before);
        }
        // A picture gone or moved has its cells painted again, blank, which
        // takes its pixels away; one whose cells this paint paints over, or
        // new, is sent after the text.
        let mut pictures = render::pictures(session, client, &attached.placement);
        let known = attached.known;
        for &(id, rect) in &attached.placement.panes {
            if session.parts.panes().get(&id).is_some_and(Pane::mid_frame) {
                pictures.retain(|p| p.pane != id);
                let (rows, cols) = (attached.spare.rows, attached.spare.cols);
                let painted = render::Painted {
                    grid: &attached.shown,
                    panes: &placed,
                };
                if known && painted.kept(id, rect, rows, cols) {
                    pictures.extend(attached.pictures.iter().filter(|p| p.pane == id).cloned());
                }
            }
        }
        for gone in (attached.pictures.iter()).filter(|p| known && !pictures.contains(p)) {
            attached.shown.forget_area(gone.area);
        }
        let send: Vec<bool> = (pictures.iter())
            .map(|p| {
                !known
                    || !attached.pictures.contains(p)
                    || !attached.spare.same_in(&attached.shown, p.area)
            })
            .collect();
        // The same screen as the client shows: nothing to send, not even
        // the envelope, whose cursor hide and show would restart a blinking
        // cursor.
        self.paint_buffer.clear();
        let shown = &attached.shown;
        if render::paint_into(shown, known, &attached.spare, &mut self.paint_buffer) {
            Attached::paint(&mut conn.out, &self.paint_buffer);
            mem::swap(&mut attached.shown, &mut attached.spare);
            attached.known = true;
            attached.clock.painted(now);
            // Written now rather than when the next poll says it can be: a
            // keystroke's echo goes out a round sooner.
            conn.flush(session);
        }
        let Stage::Attached(attached) = &mut conn.stage else {
            return;
        };
        let mut drawn = Vec::new();
        for (picture, _) in pictures.iter().zip(&send).filter(|(_, send)| **send) {
            attached.sixels.picture_into(picture, &mut drawn);
        }
        if !drawn.is_empty() {
            Attached::paint(&mut conn.out, &drawn);
        }
        attached.sixels.keep(&pictures);
        attached.pictures = pictures;
        if let Some(view) = session.views.get_mut(&client) {
            view.dirty = false;
        }
    }

    /// The socket the server listens on while it serves.
    fn listening(&mut self) -> Option<&mut Listening> {
        match &mut self.phase {
            Phase::Serving { listening, .. } => Some(listening),
            Phase::Stopping(_) => None,
        }
    }

    /// Accepts the connections waiting on the server's socket.
    fn accept(&mut self) {
        // Accept until the backlog is empty, so one tick drains it.
        loop {
            let Some(Listening { listener, .. }) = self.listening() else {
                return;
            };
            let stream = match listener.accept() {
                Ok((stream, _)) => stream,
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
                    // Out of descriptors, or something else that persists.
                    // The listener stays readable, so it rests rather than
                    // being polled again at once.
                    let now = Instant::now();
                    if let Phase::Serving { resting, .. } = &mut self.phase {
                        *resting = Some(after(now, ACCEPT_REST));
                    }
                    if self
                        .accept_logged
                        .is_none_or(|at| now.duration_since(at) >= REPORT_EVERY)
                    {
                        self.accept_logged = Some(now);
                        log!(
                            "accept: {e}; listening again in {} ms",
                            ACCEPT_REST.as_millis()
                        );
                    }
                    return;
                }
            };
            let euid = fuxix::process::geteuid();
            match fuxix::socket::peer_uid(&stream) {
                Ok(uid) if uid == euid => {}
                Ok(uid) => {
                    log!(
                        "refused a connection from uid {uid}; only uid {euid} may use this server"
                    );
                    continue;
                }
                Err(error) => {
                    log!("refused a connection whose peer could not be identified: {error}");
                    continue;
                }
            }
            if stream.set_nonblocking(true).is_err() {
                continue;
            }
            self.conns.push(Conn::new(stream));
        }
    }

    /// A client's connection is ready, as `poll` found it (`flags`).
    fn serve_conn(&mut self, index: usize, flags: PollFlags) {
        let Some(conn) = self.conns.get_mut(index) else {
            return;
        };
        let gone = flags.intersects(PollFlags::HUP | PollFlags::ERR);
        let reads = conn.reads();
        // One that is not read learns that its peer is gone by writing.
        // A hang-up says only that nothing more comes (macOS says it of a
        // peer that shut its side): one that is read once its answer is
        // written takes what it sent first; one that is still not read,
        // with nothing to write, is dead.
        if flags.intersects(PollFlags::OUT) || (gone && !reads) {
            conn.flush(&mut self.session);
        }
        if gone && !conn.reads() && conn.out.is_empty() {
            conn.enter(&mut self.session, Stage::Dead);
        }
        // A command client whose answer is now written: its next request
        // may already wait, read before.
        if flags.intersects(PollFlags::IN) || gone || (!reads && conn.reads()) {
            self.read_conn(index);
        }
    }

    /// Handles the whole frames a client sent, then reads more, until it
    /// has sent no more, or `CONN_READ` bytes, or more than 256 whole
    /// frames, or a bad one, or it is to be read no more (`Conn::reads`);
    /// each whole frame is handled in order, and nothing after a bad one.
    fn read_conn(&mut self, index: usize) {
        let Some(conn) = self.conns.get_mut(index) else {
            return;
        };
        // Held apart while the frames it lends are handled.
        let mut decoder = mem::take(&mut conn.decoder);
        let (mut frames, mut read) = (0usize, 0usize);
        let closed = 'read: loop {
            loop {
                match self.take_frame(index, &mut decoder) {
                    Ok(true) => frames = frames.saturating_add(1),
                    Ok(false) => break,
                    Err(error) => {
                        log!("a client sent a bad frame: {error}");
                        break 'read true;
                    }
                }
            }
            // A frame it sent may have ended it, or be answered yet unread.
            let Some(conn) = self.conns.get_mut(index).filter(|c| c.reads()) else {
                break false;
            };
            if frames > 256 || read >= CONN_READ {
                break false;
            }
            match fuxix::io::read(&conn.stream, &mut self.read_buffer) {
                Ok(0) => break true,
                Ok(n) => {
                    read = read.saturating_add(n);
                    decoder.push(self.read_buffer.get(..n).unwrap_or_default());
                }
                Err(Errno::AGAIN) => break false,
                Err(Errno::INTR) => {}
                Err(_) => break true,
            }
        };
        if let Some(conn) = self.conns.get_mut(index) {
            decoder.shrink(CONN_KEEP);
            conn.decoder = decoder;
            // A command client that shut its side still gets the answers
            // to what it asked.
            if closed {
                let next = if matches!(conn.stage, Stage::Command) {
                    Stage::Closing
                } else {
                    Stage::Dead
                };
                conn.enter(&mut self.session, next);
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
    ) -> Result<bool, crate::protocol::Error> {
        let session = &mut self.session;
        let Some(conn) = self.conns.get_mut(index) else {
            return Ok(false);
        };
        match &mut conn.stage {
            Stage::Hello => {
                let Some(hello) = decoder.frame::<Hello>()? else {
                    return Ok(false);
                };
                let ours = Hello::ours(hello.role);
                let between = Mismatch::between(&hello, &ours);
                conn.send(session, &Greeting::Hello(ours));
                match (between, hello.role) {
                    (Err(mismatch), _) => {
                        conn.end(session, &Greeting::Exit(&mismatch.to_string()));
                    }
                    (Ok(()), Role::Kill) => {
                        conn.end(session, &Greeting::Done(0));
                        self.stop("stopped by fux force-kill-server".into());
                    }
                    (Ok(()), Role::Attach) => {
                        conn.enter(session, Stage::Attaching(ClientKind::Terminal));
                    }
                    (Ok(()), Role::Headless) => {
                        conn.enter(session, Stage::Attaching(ClientKind::Headless));
                    }
                    (Ok(()), Role::Command) => conn.enter(session, Stage::Command),
                }
            }
            &mut Stage::Attaching(kind) => {
                let Some(attach) = decoder.frame::<Attach>()? else {
                    return Ok(false);
                };
                conn.attach(session, kind, attach);
            }
            Stage::Attached(attached) => match decoder.frame()? {
                Some(AttachedFrame::Act(act)) => attached.act(session, *act),
                // The client's terminal was resized, which may have moved
                // what it shows even if the size it ends at is the same:
                // forgotten, it is repainted in full either way, and its
                // cell size, which a font zoom changes, is asked again.
                Some(AttachedFrame::Resize(size)) => {
                    attached.known = false;
                    session.terminal_resized(attached.client);
                    let resized = session.resize(attached.client, size.rows(), size.cols());
                    if let Err(refused) = resized {
                        session.notify_to(attached.client, refused.to_string(), true);
                    }
                }
                Some(AttachedFrame::Detach) => conn.end(session, &ToAttached::Exit("detached")),
                None => return Ok(false),
            },
            // The last answer is still being written: the next request waits.
            Stage::Command if !conn.out.is_empty() => return Ok(false),
            Stage::Command => {
                let Some(Asked { request, context }) = decoder.frame()? else {
                    return Ok(false);
                };
                self.request(index, request, context);
            }
            // Nothing more is asked of a waiting command, or a stream. A
            // connection that is ending has had its last word: what came
            // with the Detach, Command or refused Hello that ended it is
            // dropped unread.
            Stage::Waiting(_) | Stage::Events(_) | Stage::Closing | Stage::Dead => {
                *decoder = Decoder::default();
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// A request from connection `index`: answered now, or once its wait
    /// has its answer, or streamed.
    fn request(&mut self, index: usize, request: crate::command::Request, context: Context) {
        let session = &mut self.session;
        let Some(conn) = self.conns.get_mut(index) else {
            return;
        };
        let Context {
            cwd,
            pane,
            server,
            env,
            client,
            ..
        } = context;
        // A pane of another server's (FUX_PANE kept as FUX_SOCKET chose
        // this one) is not the caller's here.
        let within = match server {
            None => Within::Unsaid,
            Some(s) if s.parse() == Ok(session.run) => Within::This,
            Some(_) => Within::Other,
        };
        let cwd = Some(cwd).filter(|p| p.is_dir());
        // A program run as a client (`str c2 client focused`) names one
        // that must be there.
        // A key's or the prompt's program acts where its client is.
        let origin = match client {
            Some(c) if !session.views.contains_key(&c) => {
                let outcome = Reply::failed(Code::NoClient, format!("no client {c}"));
                conn.answer(session, outcome);
                return;
            }
            Some(client) => Origin::Client(client),
            None => Origin::Cli(Caller {
                gave: pane,
                within,
                cwd,
                env,
            }),
        };
        match session.begin(request, &origin) {
            Begun::Done(outcome) => conn.answer(session, outcome),
            Begun::Waiting(waiting) => conn.enter(session, Stage::Waiting(waiting)),
            Begun::Events(subscription) => {
                // Told it is armed: from here on, nothing is missed.
                let ready = crate::events::ready(subscription.json);
                conn.send(session, &ToCommand::Stdout(ready.as_bytes().into()));
                conn.enter(session, Stage::Events(subscription));
            }
        }
    }

    fn serve_pane(&mut self, id: PaneId, flags: PollFlags) {
        let wrote = flags.intersects(PollFlags::OUT)
            && (self.session.parts.pane_mut(id)).is_some_and(|mut p| p.write_input());
        self.wrote_input(wrote);
        if flags.intersects(PollFlags::IN | PollFlags::HUP | PollFlags::ERR) {
            self.read_pane(id);
        }
    }

    /// A program read input if `wrote`, making room in its queue: a scheme
    /// report refused for want of it is sent now (`Session::tell_schemes`),
    /// not at the next change that settles the session.
    fn wrote_input(&mut self, wrote: bool) {
        if wrote {
            self.session.tell_schemes();
        }
    }

    fn read_pane(&mut self, id: PaneId) {
        let mut total = 0;
        let mut buffer = [0u8; 16384];
        let ended = loop {
            if total >= PANE_READ {
                break false;
            }
            let Some(pane) = self.session.parts.panes().get(&id) else {
                return;
            };
            // Output already waits: it is read before any more is taken.
            if pane.has_unread() {
                break false;
            }
            let Process::Reading(child) = &pane.process() else {
                return;
            };
            match fuxix::io::read(&child.master, &mut buffer) {
                Err(Errno::AGAIN) => break false,
                Err(Errno::INTR) => continue,
                // EIO: the slave side is closed; the program is gone.
                Ok(0) | Err(_) => break true,
                Ok(n) => {
                    // Past PANE_READ by at most one buffer, when the loop ends.
                    total = total.saturating_add(n);
                    self.session.output(id, buffer.get(..n).unwrap_or_default());
                    // A short read: that was all, and the poll says when
                    // there is more, without a read to find none. Full
                    // reads go on.
                    if n < SHORT_READ {
                        break false;
                    }
                }
            }
        };
        if total > 0 {
            let now = Instant::now();
            for attached in self.conns.iter_mut().filter_map(Conn::attached) {
                attached.clock.output(id, now);
            }
        }
        if ended {
            self.reap();
            // Hung up, unless its exit is being told (the rest of its
            // output still to read): then it exited, and did not hang up.
            if let Some(mut pane) = self.session.parts.pane_mut(id)
                && pane.exiting.is_none()
            {
                pane.stop_reading();
            }
        }
    }

    /// Checks every killed program and every pane's, since signals
    /// coalesce, and reaps the orphans the server adopted, every child
    /// that is no pane's leader.
    fn reap(&mut self) {
        self.reap_killed();
        let session = &self.session;
        let panes = (session.parts.panes().values()).filter_map(|p| p.process().child());
        let leaders: Vec<Pid> = (panes.chain(session.dying.iter().map(|d| &d.child)))
            .map(|child| child.leader.pid())
            .chain(self.killed.iter().copied())
            .collect();
        fuxix::process::reap_orphans(|pid| leaders.contains(&pid));
        let exited: Vec<(PaneId, crate::process::Ended)> = (self.session.parts.panes().iter())
            .filter_map(|(id, pane)| Some((*id, pane.process().child()?.leader.exited()?)))
            .collect();
        for (id, status) in exited {
            self.session.exited(id, status);
        }
    }

    fn reap_killed(&mut self) {
        self.killed.retain(|pid| !crate::process::reaped(*pid));
    }

    /// Kills every closed pane's program, and waits for them, at most a
    /// second: a process stuck in the kernel is left to init rather than
    /// hold the server's exit.
    fn finish(&mut self) {
        let dying = mem::take(&mut self.session.dying);
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
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A session with a client attached on a connection: the session, the
    /// connection, and the client's end of its socket.
    fn attached() -> Result<(Session, Conn, UnixStream), String> {
        let (session, client) = crate::session::testing::attached(10, 40)?;
        let (stream, peer) = UnixStream::pair().map_err(|e| e.to_string())?;
        let mut conn = Conn::new(stream);
        conn.stage = Stage::Attached(Box::new(Attached::new(client)));
        Ok((session, conn, peer))
    }

    /// The frames waiting for the client: what is painted in them, and the
    /// `Exit`'s reason.
    fn sent(conn: &Conn) -> (Vec<u8>, Option<String>) {
        let mut decoder = Decoder::default();
        decoder.push(conn.out.as_slice());
        let mut painted = Vec::new();
        let mut exit = None;
        while let Ok(Some(frame)) = decoder.frame() {
            match frame {
                ToAttached::Paint(bytes) => painted.extend_from_slice(&bytes),
                ToAttached::Exit(reason) => exit = Some(reason.to_owned()),
                ToAttached::Welcome(..)
                | ToAttached::Ended(_)
                | ToAttached::Run(_)
                | ToAttached::Keys(_) => {}
            }
        }
        (painted, exit)
    }

    /// A client whose screen is forgotten, as on a resize that ends at the
    /// same size, is due a full paint though nothing it views changed; but
    /// not while its last frame is still being written.
    #[test]
    fn a_forgotten_screen_is_due_a_paint_once_the_last_is_sent() -> Result<(), String> {
        let (mut session, mut conn, _peer) = attached()?;
        session.views.values_mut().for_each(|v| v.dirty = false);
        if let Some(attached) = conn.attached() {
            attached.known = true;
        }
        assert_eq!(conn.paint_due(&session), None, "nothing to paint");
        if let Some(attached) = conn.attached() {
            attached.known = false;
        }
        assert!(conn.paint_due(&session).is_some());
        conn.out.push(b"the last frame");
        assert_eq!(conn.paint_due(&session), None, "one frame in flight");
        Ok(())
    }

    /// A client let go gives its terminal the cursor's shape back as the
    /// terminal's own (DECSCUSR 0, then xterm's 7) only if a pane set one:
    /// by ctlseqs, 0 is a blinking block, which a terminal fux never told
    /// otherwise was not asked to be.
    #[test]
    fn a_cursor_shape_is_given_back_only_once_one_was_set() -> Result<(), String> {
        for (shape, given) in [
            (fux_vt::CursorShape::Initial, ""),
            (fux_vt::CursorShape::SteadyBar, "\x1b[0 q\x1b[7 q"),
        ] {
            let (mut session, mut conn, _peer) = attached()?;
            if let Some(attached) = conn.attached() {
                attached.shown.modes.cursor_shape = shape;
            }
            conn.end(&mut session, &ToAttached::Exit("detached"));
            let (painted, exit) = sent(&conn);
            assert_eq!(String::from_utf8_lossy(&painted), given, "{shape:?}");
            assert_eq!(exit.as_deref(), Some("detached"));
        }
        Ok(())
    }
}
