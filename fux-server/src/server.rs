//! The server: one thread, one `poll` loop over the listening socket, a
//! signal pipe, every client and every pane's link to its own process.
use crate::after;
use crate::journal::Detach;
use crate::link::{Link, Read};
use crate::render;
use crate::session::{Begun, Caller, Origin, Outgoing, Session, Timer, Within};
use crate::socket::{Endpoint, SocketPath};
use crate::wait::Waiting;
use fux_vt::bytes::ByteQueue;
use fux_wire::callback::Data;
use fux_wire::code::Code;
use fux_wire::events::Actor;
use fux_wire::id::{ClientId, PaneId};
use fux_wire::protocol::{
    Asked, Attach, AttachedFrame, Body, Context, Decoder, Frame, Greeting, Hello, Mismatch, Reply,
    Role, ToAttached, ToCommand, split_into,
};
use fuxix::Errno;
use fuxix::poll::{Events as PollFlags, PollFd};
use std::fs;
use std::io::{ErrorKind, Write};
use std::mem;
use std::ops::ControlFlow;

use std::os::unix::net::{UnixListener, UnixStream};

use std::time::{Duration, Instant};

/// Shared storage for client and pane link reads.
const READ_BUFFER: usize = 64 * 1024;
/// Bytes read from one client per tick, so one client cannot hold up the
/// others: a frame larger than this is read over several ticks.
const CONN_READ: usize = 256 * 1024;
/// Frames handled from one client per tick, so one that sends many small
/// ones cannot hold up the others: the rest wait in its decoder for its
/// next turn (`Conn::frame_waits`).
const CONN_FRAMES: usize = 256;
/// Connections the server accepts itself per tick (`Server::accept`), so a
/// storm of them cannot hold up the clients already served: the rest wait
/// in the listener's backlog, which keeps it readable.
const ACCEPTS: usize = 64;
/// What a connection's buffers keep once they empty; beyond it, the memory
/// a large frame or paint took is given back.
const CONN_KEEP: usize = 64 * 1024;
/// How long a stopping server waits for its clients and processes.
const STOP_WAIT: Duration = Duration::from_millis(500);
/// A condition that persists is logged once, then at most this often.
const REPORT_EVERY: Duration = Duration::from_secs(10);
/// How long the server waits to start another connection's process after
/// one could not start, or ended before a client came: retrying at once
/// would only repeat.
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
#[derive(Clone, Copy)]
enum Wake {
    Session(Timer),
    /// A program adopted from a pane whose own process ended has had its
    /// grace: it is killed.
    Grace,
    /// The listener's rest is over.
    Listen,
    /// A stopping server waits no more.
    Stop,
    /// The loop runs again, which after every wake asks the waits for
    /// their answers (`Waiting::due`).
    Look,
}

/// A client's connection: its link to its own process (`conn`), which
/// decodes what its client sends and holds what is sent it (D-123); and
/// where it is in its life.
struct Conn {
    link: crate::link::End,
    /// The frames its client sent, whole, as its process sent them on.
    decoder: Decoder,
    /// Frames for its client, till they go to its process.
    out: ByteQueue,
    /// Its process has bytes its client has not taken yet.
    pending: bool,
    /// Its client sent what is no frame: it is read no more.
    bad: bool,
    /// Its process closed its link: it is done, whatever its stage.
    gone: bool,
    /// The body each frame its process sent has, which its process keeps
    /// (`FromConn::Frame`), in order: taken as each frame is; and its
    /// request's, till it is asked.
    bodies: std::collections::VecDeque<Option<u64>>,
    body: Option<u64>,
    /// What panes, or other clients' connections, wait for or hold for it,
    /// each a token: let go once used up (`Retired`) or typed, dropped once
    /// it is gone (`ToPane::Drop`, `ToConn::Drop`).
    tokens: std::collections::BTreeSet<(Holder, u64)>,
    /// Keys for its client its process was told to wait for the body of
    /// (`ToConn::Keys`), till they are typed or let go.
    keys: std::collections::BTreeSet<u64>,
    stage: Stage,
}

/// What holds a body or capture for a connection: a pane, or another
/// client's connection, keys for which are taken once the client answers
/// the read numbered `1` sent it after them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Holder {
    Pane(PaneId),
    Client(ClientId, u64),
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
    /// It said hello as `fux opts attach`: its `Attach` is next.
    Attaching,
    /// Its client is attached: its input, resizes and detach come next.
    Attached(Box<Attached>),
    /// It said hello as a command client: its requests come next, each
    /// answered, and the answer written, before the next is read
    /// (`Conn::reads`): a client that never reads its answers is read no
    /// more, and holds no more memory than one answer.
    Command,
    /// One decoded request awaiting its source attachment's sent prefix.
    /// Bounded like a wait: one request per command connection.
    Blocked(Box<Asked>),
    /// Its request waits for its pane (`wait`, or a respawn under way): it
    /// is answered once the wait has an answer. It is read only to notice
    /// the client go.
    Waiting(Box<Waiting>),
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
            | Stage::Attaching
            | Stage::Attached(_)
            | Stage::Command
            | Stage::Waiting(_)
            | Stage::Blocked(_) => true,
            Stage::Closing | Stage::Dead => false,
        }
    }

    /// Whether a request of its is under way, blocked or waiting.
    fn asked(&self) -> bool {
        match self {
            Stage::Blocked(_) | Stage::Waiting(_) => true,
            Stage::Hello
            | Stage::Attaching
            | Stage::Attached(_)
            | Stage::Command
            | Stage::Closing
            | Stage::Dead => false,
        }
    }
}

/// An attached client: its view's id. Its process composes its screen, from
/// the scene its view makes (`render::Scene`), which it is sent as it
/// changes, and the screens of the panes it shows, which it gets from them.
struct Attached {
    client: ClientId,
    /// Attached frames handled since Attach, refused ones too: what a
    /// job's fence waits for.
    processed: u64,
}

impl Attached {
    fn new(client: ClientId) -> Attached {
        Attached {
            client,
            processed: 0,
        }
    }

    /// Actions from the client, to the session.
    fn act(&mut self, session: &mut Session, act: fux_wire::act::Act, now: Instant) {
        session.acts(self.client, vec![act], now);
    }

    /// Bytes for the client's terminal, framed in `out` for the client.
    fn paint(out: &mut ByteQueue, bytes: &[u8]) {
        split_into(bytes, ToAttached::Paint, out);
    }
}

impl Conn {
    /// A connection whose process is linked on `link`.
    fn new(link: UnixStream) -> std::io::Result<Conn> {
        Ok(Conn {
            link: crate::link::End::new(link)?,
            decoder: Decoder::default(),
            out: ByteQueue::default(),
            pending: false,
            bad: false,
            gone: false,
            bodies: std::collections::VecDeque::new(),
            body: None,
            tokens: std::collections::BTreeSet::new(),
            keys: std::collections::BTreeSet::new(),
            stage: Stage::Hello,
        })
    }

    /// Starts the process of the next connection, from `exe`, before its
    /// client comes: `fux __conn`, its link on its stdin, and the server's
    /// socket, `listener`, on its stdout, where it takes its client itself
    /// (`conn::wait`). A command's connection is then served at once, with
    /// no process to start first.
    fn spare(listener: &UnixListener, exe: &std::path::Path) -> std::io::Result<Conn> {
        Conn::launch(exe, listener.try_clone()?.into(), &[])
    }

    /// Starts the process of a connection the server accepted itself on
    /// `stream`, from `exe`: `fux __conn taken`, its client on stdout.
    fn start(stream: UnixStream, exe: &std::path::Path) -> std::io::Result<Conn> {
        Conn::launch(exe, stream.into(), &[crate::link::TAKEN])
    }

    /// `fux __conn` with `words`, its link on stdin and `out` on stdout.
    fn launch(
        exe: &std::path::Path,
        out: std::os::fd::OwnedFd,
        words: &[&str],
    ) -> std::io::Result<Conn> {
        let (ours, theirs) = UnixStream::pair()?;
        let child = std::process::Command::new(exe)
            .arg(crate::link::CONN)
            .args(words)
            .stdin(std::process::Stdio::from(std::os::fd::OwnedFd::from(
                theirs,
            )))
            .stdout(std::process::Stdio::from(out))
            .spawn()?;
        // Reaped as the server's other children are (`reap`); std never
        // waits on a dropped child.
        drop(child);
        Conn::new(ours)
    }

    /// Whether all it sent its client was taken.
    fn idle(&self) -> bool {
        self.out.is_empty() && !self.pending
    }

    /// Whether what the client sends is read: by its stage, and for a
    /// command client only once its last answer is all written.
    fn reads(&self) -> bool {
        self.stage.reads() && (self.idle() || !matches!(self.stage, Stage::Command))
    }

    /// Whether a frame it sent waits, read but not handled, for its next
    /// turn (`CONN_FRAMES`), or an error does: it is served as if readable,
    /// though its link may have nothing more.
    fn frame_waits(&self) -> bool {
        self.reads() && self.decoder.ready().unwrap_or(true)
    }

    /// Tells its process `message`, after the frames queued before it.
    fn tell_process(&mut self, message: &crate::link::ToConn) {
        self.queue_out();
        self.link.send(message);
    }

    /// Passes its process the frames queued for its client.
    fn queue_out(&mut self) {
        if !self.out.is_empty() {
            let bytes = self.out.as_slice().to_vec();
            self.out.take(bytes.len());
            self.out.shrink(CONN_KEEP);
            self.link.send(&crate::link::ToConn::Bytes(bytes));
            self.pending = true;
        }
    }

    /// What its process said: its client's frames, into `decoder`, and how
    /// its client is; whether its client is gone, or shut its side.
    fn hear(&mut self, buffer: &mut [u8]) -> bool {
        use crate::link::FromConn;
        self.gone |= self.link.read_up_to(buffer, CONN_READ) == crate::link::Read::Closed;
        let mut closed = self.gone;
        loop {
            match self.link.next_message::<FromConn>() {
                Ok(Some(FromConn::Frame { frame, body })) => {
                    self.decoder.push(&frame);
                    self.bodies.push_back(body);
                }
                Ok(Some(FromConn::Flushed)) => self.pending = false,
                // A spare's first words, which made it a connection
                // (`spare_heard`).
                Ok(Some(FromConn::Waiting | FromConn::Accepted)) => {}
                Ok(Some(FromConn::Shut)) => closed = true,
                Ok(Some(FromConn::Bad(why))) => {
                    log!("a client sent a bad frame: {why}");
                    self.bad = true;
                    return true;
                }
                Ok(None) => break,
                Err(error) => {
                    log!("a connection's process sent a bad frame: {error}");
                    self.bad = true;
                    return true;
                }
            }
        }
        closed
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
    /// client detached, as disconnected unless it was detached for another
    /// reason first (`exit`), and its terminal given its own cursor shape
    /// and configured colour back; one that is dead stays so.
    fn enter(&mut self, session: &mut Session, next: Stage, now: Instant) {
        if matches!(self.stage, Stage::Dead) {
            return;
        }
        let next_expects = match next {
            Stage::Attaching => Some(crate::link::Expect::Attach),
            Stage::Command => Some(crate::link::Expect::Asked),
            Stage::Attached(_) => Some(crate::link::Expect::Attached),
            Stage::Hello | Stage::Blocked(_) | Stage::Waiting(_) | Stage::Closing | Stage::Dead => {
                None
            }
        };
        let was_command = matches!(
            self.stage,
            Stage::Command | Stage::Blocked(_) | Stage::Waiting(_)
        );
        match mem::replace(&mut self.stage, next) {
            Stage::Attached(attached) => {
                session.detach(attached.client, Detach::Disconnected, now);
                // Its terminal's cursor overrides are given back as its own.
                self.tell_process(&crate::link::ToConn::Detached);
            }
            // What it asked of panes and no one answered is let go, however
            // it leaves its wait: one answered took its answers already.
            Stage::Waiting(waiting) => {
                if let Waiting::Asked(pending) = *waiting {
                    session.abandon(&pending.asks, &[], now);
                }
            }
            Stage::Hello
            | Stage::Attaching
            | Stage::Command
            | Stage::Blocked(_)
            | Stage::Closing
            | Stage::Dead => {}
        }
        match (&self.stage, next_expects) {
            // A command client's next request is read once its answer is
            // written (`answer`).
            (Stage::Command, Some(_)) if was_command => {}
            (_, Some(expect)) => self.tell_process(&crate::link::ToConn::Expect(expect)),
            (_, None) => {}
        }
    }

    /// Encodes a frame straight into the output.
    fn send<'a>(&mut self, frame: &impl Frame<'a>) {
        self.out.push_with(|out| frame.encode_into(out));
    }

    /// Its client attaches as `attach` says: it is told its id before
    /// anything is painted; or why it cannot attach.
    fn attach(&mut self, session: &mut Session, attach: Attach, now: Instant) {
        match session.attach_as(attach, now) {
            Ok(client) => {
                let attached = Attached::new(client);
                self.enter(session, Stage::Attached(Box::new(attached)), now);
                self.send(&ToAttached::Welcome(client, session.run));
                // Its process takes what other connections send its client.
                if session.panes().is_some() {
                    self.tell_process(&crate::link::ToConn::Listen { client });
                }
            }
            Err(error) => self.end(session, &ToAttached::Exit(&error.to_string()), now),
        }
    }

    /// Answers a request with its outcome, encoded once and sent in pieces
    /// as long as fit, then their end; and takes the next.
    fn answer(&mut self, session: &mut Session, mut reply: Reply, now: Instant) {
        // One its process makes itself: told how (`link::Making`).
        if let Some(making) = session.making.take() {
            match &making {
                crate::link::Making::Capture { pane, token, .. } => {
                    self.tokens.insert((Holder::Pane(*pane), *token));
                }
                crate::link::Making::Keys {
                    client,
                    token,
                    asked,
                    ..
                } => {
                    self.tokens
                        .insert((Holder::Client(*client, *asked), *token));
                }
                crate::link::Making::Client { .. } => {}
            }
            self.tell_process(&crate::link::ToConn::Make(Box::new(making)));
            if self.stage.asked() {
                self.enter(session, Stage::Command, now);
            }
            self.tell_process(&crate::link::ToConn::Answered);
            return;
        }
        reply.run = Some(session.run);
        const REFUSED: &str = "the OS refused memory for the reply";
        // Keep the fixed failure frame available before trying the reply.
        if self
            .out
            .push_with(|out| out.try_reserve_exact(REFUSED.len().saturating_add(5)))
            .is_err()
        {
            self.enter(session, Stage::Dead, now);
            return;
        }
        let bytes = fux_wire::wire::encode(&reply);
        // Include every Part header and PartsEnd, before queuing any of it.
        let headers = bytes
            .len()
            .div_ceil(fux_wire::protocol::MAX_PAYLOAD)
            .checked_mul(5);
        let reserve = headers
            .and_then(|n| n.checked_add(bytes.len()))
            .and_then(|n| n.checked_add(5));
        if reserve.is_none_or(|n| self.out.push_with(|out| out.try_reserve_exact(n)).is_err()) {
            self.end(session, &ToCommand::Exit(REFUSED), now);
            return;
        }
        split_into(&bytes, ToCommand::Part, &mut self.out);
        self.send(&ToCommand::PartsEnd);
        if self.stage.asked() {
            self.enter(session, Stage::Command, now);
        }
        // Its client's next request is read once it has this answer.
        self.tell_process(&crate::link::ToConn::Answered);
    }

    /// Sends the connection's last frame, its client detached and its
    /// terminal given back first: it closes once what waits is sent.
    fn end<'a>(&mut self, session: &mut Session, frame: &impl Frame<'a>, now: Instant) {
        self.enter(session, Stage::Closing, now);
        self.send(frame);
        self.tell_process(&crate::link::ToConn::Close);
    }

    /// Detaches the client attached here for `why`, and ends it, telling
    /// it `text`.
    fn exit(&mut self, session: &mut Session, why: Detach, text: &str, now: Instant) {
        if let Some(attached) = self.attached() {
            session.detach(attached.client, why, now);
        }
        self.end(session, &ToAttached::Exit(text), now);
    }

    /// Sends the client attached here `frame`, an `Exit` as its last
    /// frame: the session's, which asked it to detach. A paint is made
    /// only once the queue empties.
    fn tell(&mut self, session: &mut Session, frame: &ToAttached, now: Instant) {
        if self.attached().is_none() {
            return;
        }
        match frame {
            ToAttached::Paint(bytes) => Attached::paint(&mut self.out, bytes),
            ToAttached::Exit(text) => self.exit(session, Detach::Detached, text, now),
            ToAttached::Welcome(..)
            | ToAttached::Ended(_)
            | ToAttached::Open(_)
            | ToAttached::Keys(_)
            | ToAttached::Read(..) => self.send(frame),
        }
    }

    /// Passes its process what waits for its client, as far as its link
    /// takes it; one whose process is gone is dead.
    fn flush(&mut self, session: &mut Session, now: Instant) {
        self.queue_out();
        if self.link.flush().is_err() {
            self.enter(session, Stage::Dead, now);
        }
    }
}

pub struct Server {
    session: Session,
    phase: Phase,
    conns: Vec<Conn>,
    children: fuxix::signal::Pipe,
    stops: fuxix::signal::Pipe,
    /// Where bytes read from a client's connection or terminal land before
    /// they are taken; one for the server, reused by every read.
    read_buffer: Box<[u8; READ_BUFFER]>,
    /// Where each client's screen is composed, reused by every paint.
    /// What each descriptor polled is, and then what is ready; reused by
    /// every tick.
    slots: Vec<Slot>,
    pollfds: fuxix::poll::Poll,
    ready: Vec<(Slot, PollFlags)>,
    /// The next connection's process, waiting for its client (`spare`),
    /// and whether it said it waits at the socket: till then the server
    /// accepts on it itself (`accept`).
    spare: Option<Conn>,
    spare_waits: bool,
    /// When a failure to keep one was last logged.
    accept_logged: Option<Instant>,
}

/// Allocate the shared buffer without putting a large array on the stack.
fn read_buffer() -> Result<Box<[u8; READ_BUFFER]>, Error> {
    vec![0; READ_BUFFER]
        .into_boxed_slice()
        .try_into()
        .map_err(|_| {
            Error::Setup(std::io::Error::other(
                "could not size the server read buffer",
            ))
        })
}

/// Why a server could not start.
#[derive(Debug)]
pub enum Error {
    Socket(crate::socket::Error),
    /// The listener or the signal pipes could not be set up.
    Setup(std::io::Error),
    /// The run on the socket cannot be taken back, or started beside, so.
    Refused(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Socket(error) => error.fmt(f),
            Error::Setup(error) => error.fmt(f),
            Error::Refused(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for Error {}

/// Runs a server on `socket` until it is told to stop or its last pane
/// closes.
pub fn serve(socket: &SocketPath, journal: fux_wire::command::JournalEntries) -> Result<(), Error> {
    let children = fuxix::signal::Pipe::new(&[fuxix::process::Signal::Chld])
        .map_err(|e| Error::Setup(e.into()))?;
    // Orphans the panes' programs leave stay the server's to read and reap.
    if let Err(errno) = fuxix::process::adopt_orphans() {
        log!("adopting orphans: {errno}");
    }
    let stops = fuxix::signal::Pipe::new(&crate::STOPS).map_err(|e| Error::Setup(e.into()))?;
    // Pane creation and observation requests supply their own policy.
    let run = fux_wire::command::Run::now().ok_or_else(|| {
        Error::Setup(std::io::Error::other(
            "the system clock is before 1970 or past 2554: this run cannot be told from another",
        ))
    })?;
    crate::socket::private_directory(socket, true).map_err(Error::Socket)?;
    let panes = crate::socket::panes(socket.path());
    let exe = panes.join("exe");
    // A run to take back runs the fux it started with (D-124 (7)), chosen
    // by the run's file alone: its record is that fux's to read, in its
    // own format.
    let recorded = panes.join("run").exists();
    match recorded {
        true if !exe.exists() => {
            let why = format!(
                "the fux the run on {} runs, {}, is gone",
                socket.path().display(),
                exe.display()
            );
            return Err(Error::Refused(stranded(&panes, &why)));
        }
        true if std::env::current_exe().ok().as_deref() != Some(exe.as_path()) => {
            use std::os::unix::process::CommandExt;
            let error = std::process::Command::new(&exe)
                .args(std::env::args_os().skip(1))
                .exec();
            return Err(Error::Refused(format!(
                "running {}: {error}",
                exe.display()
            )));
        }
        true => {}
        false if !waiting(&panes).is_empty() => {
            let why = format!(
                "the record of the run on {} is gone",
                socket.path().display()
            );
            return Err(Error::Refused(stranded(&panes, &why)));
        }
        false => {}
    }
    let mut session = Session::new(socket.path().to_owned(), true, run, journal);
    session.exe = Some(exe.clone());
    let read_buffer = read_buffer()?;
    // Last of what can fail: once it listens, a server that started
    // with it finds it there (`socket::bind_socket`).
    let listening = Listening::open(socket)?;
    // Under the socket's lock, the one server of the socket: a run whose
    // server crashed is taken back before anyone is served; else a new one
    // begins.
    match crate::record::read(&panes.join("run"))
        .map_err(|e| Error::Refused(stranded(&panes, &e)))?
    {
        Some(record) => session
            .resume(record, Instant::now())
            .map_err(|e| Error::Refused(stranded(&panes, &format!("taking the run back: {e}"))))?,
        None => pin(&panes, &exe).map_err(Error::Setup)?,
    }
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
        stops,
        read_buffer,
        slots: Vec::new(),
        pollfds: fuxix::poll::Poll::default(),
        ready: Vec::new(),
        spare: None,
        spare_waits: false,
        accept_logged: None,
    };
    ready();
    server.run();
    Ok(())
}

/// Makes `panes`, where a fresh run keeps what outlives its server, and
/// pins the fux it runs there, `exe`, which its panes and programs run and
/// a server taking it back runs too: installing fux neither upgrades nor
/// strands it (D-124 (7)). A link to this fux where one can be made, else a
/// copy of it.
fn pin(panes: &std::path::Path, exe: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    // What a run that stopped, or never began, left there is no one's.
    match fs::remove_dir_all(panes) {
        Err(error) if error.kind() != ErrorKind::NotFound => return Err(error),
        Ok(()) | Err(_) => {}
    }
    fs::DirBuilder::new().mode(0o700).create(panes)?;
    let running = std::env::current_exe()?;
    if fs::hard_link(&running, exe).is_ok() {
        return Ok(());
    }
    // The running binary itself, though the file was replaced since.
    let source = match std::path::Path::new("/proc/self/exe").exists() {
        true => std::path::PathBuf::from("/proc/self/exe"),
        false => running,
    };
    fs::copy(source, exe).map(drop)
}

/// The pane processes waiting at `panes` for a server, by their sockets'
/// names and their pids, as each answers.
fn waiting(panes: &std::path::Path) -> Vec<(String, Option<fuxix::process::Pid>)> {
    let Ok(entries) = fs::read_dir(panes) else {
        return Vec::new();
    };
    let mut waiting = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.parse::<u32>().is_err() {
            continue;
        }
        if let Some(pid) = crate::link::Link::who(&entry.path()) {
            waiting.push((name, fuxix::process::Pid::from_raw(pid)));
        }
    }
    waiting
}

/// Why a run cannot be taken back, `why`, and its panes that wait.
fn stranded(panes: &std::path::Path, why: &str) -> String {
    let waiting: Vec<String> = (waiting(panes).into_iter())
        .map(|(name, pid)| match pid {
            Some(pid) => format!("%{name} (pid {pid})"),
            None => format!("%{name}"),
        })
        .collect();
    format!(
        "{why}: its run cannot be taken back, and no other starts beside it; \
         its panes wait: {}; `kill` on each pid ends it",
        waiting.join(", ")
    )
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
pub const READY: &[u8] = b"fux server: listening\n";

/// What a polled descriptor is. A connection's index into `conns` holds
/// for the whole tick: connections are only appended during it (`accept`)
/// and removed at its end (`close_conns`). Handlers still look each one up
/// with `get_mut`, as the lints ask.
#[derive(Clone, Copy)]
enum Slot {
    /// The socket listened on, while no spare waits there.
    Listener,
    /// The link of the next connection's process (`Server::spare`).
    Spare,
    Children,
    Stops,
    Conn(usize),
    /// A pane's link, open or closing (`Session::ending`).
    Pane(PaneId),
}

impl Server {
    fn run(&mut self) {
        let mut wakes = Vec::new();
        loop {
            let ControlFlow::Continue(next) = self.turn(Instant::now(), &mut wakes) else {
                return;
            };
            if matches!(self.phase, Phase::Stopping(_))
                && self.session.links.is_empty()
                && self.session.ending.is_empty()
                && self.conns.is_empty()
            {
                self.finish();
                return;
            }
            // Long work in the turn must not extend the poll's deadline.
            let timeout = next.map(|at| at.saturating_duration_since(Instant::now()));
            let mut ready = mem::take(&mut self.ready);
            self.poll(timeout, &mut ready);
            for &(slot, flags) in &ready {
                let now = Instant::now();
                match slot {
                    Slot::Listener => self.accept(now),
                    Slot::Spare => self.spare_heard(now),
                    Slot::Children => {
                        crate::drain(self.children.reader());
                        self.reap(now);
                    }
                    Slot::Stops => {
                        crate::drain(self.stops.reader());
                        self.stop("stopped by a signal".into(), Actor::Server, now);
                    }
                    Slot::Conn(i) => self.serve_conn(i, flags, now),
                    Slot::Pane(id) => self.serve_pane(id, flags, now),
                }
            }
            self.ready = ready;
        }
    }

    /// Finish the preceding descriptor events before polling again. Session
    /// timers precede answers, and an echo already due paints in this turn.
    fn turn(
        &mut self,
        now: Instant,
        wakes: &mut Vec<(Instant, Wake)>,
    ) -> ControlFlow<(), Option<Instant>> {
        wakes.clear();
        wakes.extend(self.wakes());
        let mut changed = false;
        let mut next = None;
        for &(at, wake) in wakes.iter() {
            if at <= now {
                changed = true;
                self.wake(wake, now)?;
            } else {
                next = Some(next.map_or(at, |next: Instant| next.min(at)));
            }
        }
        // A stop asked for since the last turn goes first: its entry is the
        // last that the reads it ends must read.
        if (self.session.outbox.iter()).any(|o| matches!(o, Outgoing::Shutdown(..))) {
            changed = true;
            self.deliver(now);
        }
        changed |= self.answer_waits(now);
        let answered_at = self.session.parts.journal().end(self.session.journal_run);
        self.flush_links();
        // Only the deadline survives compaction, never connection indices.
        self.close_conns(now);
        // Delivery can detach a client, resize its panes or start shutdown.
        changed |= !self.session.outbox.is_empty();
        self.deliver(now);
        if self.session.parts.journal().end(self.session.journal_run) != answered_at {
            changed |= self.answer_waits(now);
        }
        self.retire();
        // Each client's view as it ends the turn.
        self.scenes(now);
        self.flush_links();
        // The next connection's process starts once what this turn did has
        // gone out, so a client waits for no start; one that can't is tried
        // again when the turn says.
        changed |= self.keep_spare(now);
        if changed {
            next = self.wakes().map(|(at, _)| at).min();
        }
        ControlFlow::Continue(next)
    }

    /// Everything the loop waits for, each with when it is due.
    fn wakes(&self) -> impl Iterator<Item = (Instant, Wake)> + '_ {
        let session = (self.session.timers()).map(|(at, timer)| (at, Wake::Session(timer)));
        let grace =
            (self.session.orphans.iter().map(|o| o.deadline).min()).map(|at| (at, Wake::Grace));
        let phase = match self.phase {
            Phase::Serving { resting, .. } => resting.map(|at| (at, Wake::Listen)),
            Phase::Stopping(by) => Some((by, Wake::Stop)),
        };
        let answers = self.conns.iter().filter_map(|conn| {
            let Stage::Waiting(waiting) = &conn.stage else {
                return None;
            };
            Some((waiting.due()?, Wake::Look))
        });
        session.chain(grace).chain(phase).chain(answers)
    }

    /// What `wake` waited for is done, now that it is due at `now`; a break
    /// once the server has stopped.
    fn wake(&mut self, wake: Wake, now: Instant) -> ControlFlow<()> {
        match wake {
            Wake::Session(timer) => self.session.fire(timer, now),
            Wake::Grace => {
                for orphan in self
                    .session
                    .orphans
                    .iter_mut()
                    .filter(|o| o.deadline <= now)
                {
                    // The server's child, unreaped: its pid is its own.
                    let _ = fuxix::process::kill(orphan.pid, fuxix::process::Signal::Kill);
                    orphan.deadline = after(now, STOP_WAIT);
                }
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
            // The loop answers waits after every wake: these only set when
            // it wakes.
            Wake::Look => {}
        }
        ControlFlow::Continue(())
    }

    /// Answers each wait that has its answer.
    fn answer_waits(&mut self, now: Instant) -> bool {
        let mut answered = false;
        for conn in &mut self.conns {
            let Stage::Waiting(waiting) = &mut conn.stage else {
                continue;
            };
            if let Some(outcome) = waiting.answer(&mut self.session, now) {
                conn.answer(&mut self.session, outcome, now);
                answered = true;
            }
        }
        answered
    }

    /// Drops every connection that is dead, or closing with nothing left to
    /// send. Neither has a client attached: it was detached as it ended.
    fn close_conns(&mut self, now: Instant) {
        // One closing goes once its process, done with its client, closes
        // its link (`Conn::hear`).
        let session = &mut self.session;
        let mut dropped = Vec::new();
        self.conns.retain_mut(|conn| {
            let dead = matches!(conn.stage, Stage::Dead) || conn.gone;
            // What it was told before it died still goes, as far as it can;
            // what it asked of panes is no one's (`Session::abandon`).
            if dead {
                conn.enter(session, Stage::Dead, now);
                let _ = conn.link.flush();
                let mut tokens = Vec::new();
                for (holder, token) in mem::take(&mut conn.tokens) {
                    match holder {
                        Holder::Pane(pane) => tokens.push((pane, token)),
                        Holder::Client(client, _) => dropped.push((client, token)),
                    }
                }
                session.abandon(&[], &tokens, now);
            }
            !dead
        });
        for (_, token) in dropped {
            self.let_go(token);
        }
    }

    /// Keys `token` go to their client no more: its connection waits for
    /// their body no more, and lets what follows them go on.
    fn let_go(&mut self, token: u64) {
        if let Some(conn) = self.conns.iter_mut().find(|c| c.keys.contains(&token)) {
            conn.keys.remove(&token);
            conn.tell_process(&crate::link::ToConn::Drop { token });
        }
    }

    /// Waits for descriptors to be ready, or `timeout`; which are, and for
    /// what, go into `ready`.
    fn poll(&mut self, timeout: Option<Duration>, ready: &mut Vec<(Slot, PollFlags)>) {
        ready.clear();
        let held: Vec<bool> = (self.conns.iter()).map(|conn| self.held(conn)).collect();
        let runnable: Vec<bool> = (self.conns.iter().zip(&held))
            .map(|(conn, held)| {
                !held && conn.frame_waits()
                    || matches!(&conn.stage, Stage::Blocked(asked)
                if !matches!(self.unprocessed(asked), Some(1..)))
            })
            .collect();
        let slots = &mut self.slots;
        slots.clear();
        let mut fds = self.pollfds.list();
        let mut add = |fd, slot| {
            fds.push(fd);
            slots.push(slot);
        };
        if let Phase::Serving {
            listening,
            resting: None,
        } = &self.phase
            && !self.spare_waits
        {
            add(
                PollFd::new(&listening.listener, PollFlags::IN),
                Slot::Listener,
            );
        }
        if let Some(spare) = &self.spare {
            add(PollFd::new(&spare.link.stream, PollFlags::IN), Slot::Spare);
        }
        add(
            PollFd::new(self.children.reader(), PollFlags::IN),
            Slot::Children,
        );
        add(PollFd::new(self.stops.reader(), PollFlags::IN), Slot::Stops);
        for (i, conn) in self.conns.iter().enumerate() {
            // Its process's link is read, what its client sent and whether
            // the client took what it was sent, but while what its client
            // types waits for a pane (`Server::held`).
            let mut flags = match held.get(i).copied().unwrap_or(false) {
                true => PollFlags::NONE,
                false => PollFlags::IN,
            };
            if !conn.out.is_empty() || !conn.link.out.is_empty() {
                flags |= PollFlags::OUT;
            }
            add(PollFd::new(&conn.link.stream, flags), Slot::Conn(i));
        }
        let links = (self.session.links.iter())
            .chain(self.session.ending.iter().map(|(id, link)| (id, link)));
        for (id, link) in links {
            if let Link::Remote(remote) = link
                && !remote.closed
            {
                let mut flags = PollFlags::IN;
                if !remote.end.out.is_empty() {
                    flags |= PollFlags::OUT;
                }
                add(PollFd::new(&remote.end.stream, flags), Slot::Pane(*id));
            }
        }
        // A connection with frames waiting is served this round: the poll
        // does not wait.
        let waits = runnable.iter().any(|&ready| ready);
        let timeout = if waits { Some(Duration::ZERO) } else { timeout };
        match fds.poll(timeout) {
            Ok(_) | Err(Errno::INTR) => {}
            Err(error) => log!("poll: {error}"),
        }
        ready.extend(
            fds.revents()
                .zip(slots.iter().copied())
                .map(|(flags, slot)| {
                    let waits = matches!(slot, Slot::Conn(i)
                        if runnable.get(i).copied().unwrap_or(false));
                    (if waits { flags | PollFlags::IN } else { flags }, slot)
                })
                .filter(|(flags, _)| !flags.is_empty())
                .map(|(flags, slot)| (slot, flags)),
        );
    }

    /// Whether connection `conn`'s client's frames wait, unread: what it
    /// types waits for a pane that reads nothing more of what it is sent
    /// (`Session::typing_waits`), and stays with its connection's process.
    fn held(&self, conn: &Conn) -> bool {
        matches!(&conn.stage, Stage::Attached(attached) if self.session.typing_waits(attached.client))
    }

    /// Stops the server, saying why, as `actor`.
    fn stop(&mut self, reason: String, actor: Actor, now: Instant) {
        if let Phase::Stopping(_) = self.phase {
            return;
        }
        log!("{reason}");
        // The run ends: no server takes it back, nor a part of it.
        self.session.unrecord();
        // The sockets go first: from now on a client finds no server. The
        // next connection's process, its link closed, ends.
        self.phase = Phase::Stopping(after(now, STOP_WAIT));
        (self.spare, self.spare_waits) = (None, false);
        // Every client attached is detached for it before its entry, which
        // is the journal's last.
        let exit = format!("the fux server stopped: {reason}");
        let was = self.session.parts.actor(actor);
        for conn in &mut self.conns {
            if conn.attached().is_some() {
                conn.exit(&mut self.session, Detach::ServerStopped, &exit, now);
            }
        }
        self.session.parts.actor(was);
        // The journal's last entry, which ends every pending read.
        let stopped = crate::journal::Event::Stopped(reason.clone());
        self.session.parts.journal_mut().push_from(stopped, actor);
        self.answer_waits(now);
        self.session.shutdown(now);
        // Every connection still open ends, so no command runs on what is
        // left of the session.
        for conn in &mut self.conns {
            let session = &mut self.session;
            match conn.stage {
                Stage::Hello => conn.end(session, &Greeting::Exit(&exit), now),
                Stage::Attaching => conn.end(session, &ToAttached::Exit(&exit), now),
                Stage::Attached(_) => {}
                Stage::Command | Stage::Blocked(_) | Stage::Waiting(_) => {
                    conn.end(session, &ToCommand::Exit(&exit), now);
                }
                Stage::Closing | Stage::Dead => {}
            }
        }
    }

    /// Does what the session asks outside itself: sends its frames,
    /// then stops if it asks.
    fn deliver(&mut self, now: Instant) {
        self.send_frames(now);
        let (stops, rest) = (mem::take(&mut self.session.outbox).into_iter())
            .partition(|o| matches!(o, Outgoing::Shutdown(..)));
        self.session.outbox = rest;
        for stop in stops {
            if let Outgoing::Shutdown(reason, actor) = stop {
                self.stop(reason, actor, now);
            }
        }
    }

    /// Sends the session's frames. A stop stays for `deliver`: the answer
    /// that asked for it goes first.
    fn send_frames(&mut self, now: Instant) {
        for outgoing in mem::take(&mut self.session.outbox) {
            let (client, frame) = match outgoing {
                Outgoing::Shutdown(..) => {
                    self.session.outbox.push(outgoing);
                    continue;
                }
                Outgoing::To(client, frame) => (client, frame),
                Outgoing::Keys(client, token) => {
                    if let Some(conn) = self.conns.iter_mut().find(|c| c.serves(client)) {
                        conn.keys.insert(token);
                        conn.tell_process(&crate::link::ToConn::Keys { token });
                    }
                    continue;
                }
            };
            if let Some(conn) = self.conns.iter_mut().find(|c| c.serves(client)) {
                conn.tell(&mut self.session, &frame, now);
            }
        }
    }

    /// Paints the client attached on connection `index`, whose paint is due.
    fn scenes(&mut self, now: Instant) {
        let session = &mut self.session;
        for conn in &mut self.conns {
            let Stage::Attached(attached) = &conn.stage else {
                continue;
            };
            let client = attached.client;
            if !session.views.get(&client).is_some_and(|view| view.dirty) {
                continue;
            }
            // The title and bell marks first: the bar shows the marks.
            let before = session.before_paint(client);
            if !before.is_empty() {
                Attached::paint(&mut conn.out, &before);
            }
            if let Some(scene) = render::scene(session, client) {
                conn.tell_process(&crate::link::ToConn::Scene(Box::new(scene)));
            }
            if let Some(view) = session.views.get_mut(&client) {
                view.dirty = false;
            }
            conn.flush(session, now);
        }
    }

    /// Starts the next connection's process (`Conn::spare`) while the
    /// server serves and has none; one that cannot start is tried again
    /// after `ACCEPT_REST`, as an `accept` that fails is: whether it rests
    /// now.
    fn keep_spare(&mut self, now: Instant) -> bool {
        if self.spare.is_some() {
            return false;
        }
        let Phase::Serving {
            listening,
            resting: None,
        } = &self.phase
        else {
            return false;
        };
        let Some(exe) = self.session.exe.as_deref() else {
            return false;
        };
        match Conn::spare(&listening.listener, exe) {
            Ok(mut conn) => {
                if let Some(panes) = self.session.panes() {
                    let run = self.session.run;
                    conn.tell_process(&crate::link::ToConn::Hello { panes, run });
                }
                let _ = conn.link.flush();
                self.spare = Some(conn);
                false
            }
            // Out of descriptors or processes: what waits at the socket is
            // taken, started or refused there and then (`accept`).
            Err(e) => {
                self.rest(&format!("starting a connection's process: {e}"), now);
                self.accept(now);
                true
            }
        }
    }

    /// Accepts the connections waiting on the server's socket, `ACCEPTS`
    /// at most, while no spare waits there: each is given a process of its
    /// own (`Conn::start`), so that a burst of clients waits for no spare
    /// to start one at a time. One that can't be given one is closed, and
    /// so its client fails at once, as are the others waiting then.
    fn accept(&mut self, now: Instant) {
        let mut refusing = false;
        for _ in 0..ACCEPTS {
            let Phase::Serving { listening, .. } = &self.phase else {
                return;
            };
            let stream = match listening.listener.accept() {
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
                // Out of descriptors, or something else that persists: the
                // listener, readable still, rests.
                Err(e) => return self.rest(&format!("accept: {e}"), now),
            };
            if let Some(why) = crate::socket::stranger(&stream) {
                log!("{why}");
                continue;
            }
            let Some(exe) = self.session.exe.as_deref().filter(|_| !refusing) else {
                continue;
            };
            match Conn::start(stream, exe) {
                Ok(mut conn) => {
                    if let Some(panes) = self.session.panes() {
                        let run = self.session.run;
                        conn.tell_process(&crate::link::ToConn::Hello { panes, run });
                    }
                    self.conns.push(conn);
                }
                Err(e) => {
                    self.rest(&format!("starting a connection's process: {e}"), now);
                    refusing = true;
                }
            }
        }
    }

    /// The next connection's process spoke: it waits at the socket; or it
    /// took a client, and is one of the server's connections from now on,
    /// and the next is started at once; or it ended first.
    fn spare_heard(&mut self, now: Instant) {
        use crate::link::FromConn;
        let Some(mut spare) = self.spare.take() else {
            return;
        };
        let closed = spare
            .link
            .read_up_to(self.read_buffer.as_mut_slice(), CONN_READ)
            == crate::link::Read::Closed;
        loop {
            match spare.link.next_message::<FromConn>() {
                Ok(Some(FromConn::Waiting)) => self.spare_waits = true,
                Ok(Some(FromConn::Accepted)) => {
                    self.spare_waits = false;
                    self.conns.push(spare);
                    let index = self.conns.len().saturating_sub(1);
                    self.read_conn(index, now);
                    if let Some(conn) = self.conns.get_mut(index) {
                        conn.flush(&mut self.session, now);
                    }
                    self.keep_spare(now);
                    return;
                }
                Ok(None) if !closed => return self.spare = Some(spare),
                Ok(_) | Err(_) => {
                    self.spare_waits = false;
                    let why = "a connection's process ended before its client came";
                    return self.rest(why, now);
                }
            }
        }
    }

    /// The next connection's process waits `ACCEPT_REST`, `why` said at
    /// most so often.
    fn rest(&mut self, why: &str, now: Instant) {
        if let Phase::Serving { resting, .. } = &mut self.phase {
            *resting = Some(after(now, ACCEPT_REST));
        }
        if self
            .accept_logged
            .is_none_or(|at| now.duration_since(at) >= REPORT_EVERY)
        {
            self.accept_logged = Some(now);
            log!("{why}; another in {} ms", ACCEPT_REST.as_millis());
        }
    }

    /// A client's connection is ready, as `poll` found it (`flags`).
    fn serve_conn(&mut self, index: usize, flags: PollFlags, now: Instant) {
        let Some(conn) = self.conns.get_mut(index) else {
            return;
        };
        if flags.intersects(PollFlags::OUT) {
            conn.flush(&mut self.session, now);
        }
        if flags.intersects(PollFlags::IN | PollFlags::HUP | PollFlags::ERR) {
            self.read_conn(index, now);
        }
        let eligible = self.conns.get(index).is_some_and(|conn| {
            matches!(&conn.stage, Stage::Blocked(asked) if !matches!(self.unprocessed(asked), Some(1..)))
        });
        if eligible && let Some(conn) = self.conns.get_mut(index) {
            let stage = mem::replace(&mut conn.stage, Stage::Command);
            if let Stage::Blocked(asked) = stage {
                self.ordered_request(index, *asked, now);
            }
        }
    }

    /// Handles the whole frames a client sent, then reads more, until it
    /// has sent no more, or `CONN_READ` bytes, or `CONN_FRAMES` frames are
    /// handled, or a bad one, or it is to be read no more (`Conn::reads`);
    /// each whole frame is handled in order, and nothing after a bad one.
    fn read_conn(&mut self, index: usize, now: Instant) {
        let Some(conn) = self.conns.get_mut(index) else {
            return;
        };
        let mut closed = conn.hear(self.read_buffer.as_mut_slice());
        // Held apart while the frames it lends are handled.
        let mut decoder = mem::take(&mut conn.decoder);
        let mut frames = 0usize;
        while frames < CONN_FRAMES
            && self
                .conns
                .get(index)
                .is_some_and(|conn| conn.reads() && !self.held(conn))
        {
            // Each frame is a new event: a previous request may have taken
            // long enough to pass another request's deadline.
            match self.take_frame(index, &mut decoder, Instant::now()) {
                Ok(true) => frames = frames.saturating_add(1),
                Ok(false) => break,
                Err(error) => {
                    log!("a client sent a bad frame: {error}");
                    closed = true;
                    break;
                }
            }
        }
        if let Some(conn) = self.conns.get_mut(index) {
            decoder.shrink(CONN_KEEP);
            conn.decoder = decoder;
            // A command client that shut its side still gets the answers
            // to what it asked.
            if closed {
                match conn.stage {
                    Stage::Command if !conn.bad => {
                        conn.enter(&mut self.session, Stage::Closing, now);
                        conn.tell_process(&crate::link::ToConn::Close);
                    }
                    // Its last word goes on to its client: its process closes
                    // its link once it is written.
                    Stage::Dead | Stage::Closing => {}
                    Stage::Command
                    | Stage::Hello
                    | Stage::Attaching
                    | Stage::Attached(_)
                    | Stage::Blocked(_)
                    | Stage::Waiting(_) => conn.enter(&mut self.session, Stage::Dead, now),
                }
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
    ) -> Result<bool, fux_wire::protocol::Error> {
        let session = &mut self.session;
        let Some(conn) = self.conns.get_mut(index) else {
            return Ok(false);
        };
        let mut answered = None;
        match &mut conn.stage {
            Stage::Hello => {
                let Some(hello) = decoder.frame::<Hello>()? else {
                    return Ok(false);
                };
                conn.bodies.pop_front();
                // An older client's kill: refused before any Hello, which it
                // would take as the server stopped.
                if hello.role == Role::RETIRED_KILL {
                    let why = format!(
                        "the server runs {}, which takes no kill: `kill` on its process stops it \
                         (`fux status` gives its `server_pid`, where the system says it)",
                        fux_wire::protocol::Build::OURS
                    );
                    conn.end(session, &Greeting::Exit(&why), now);
                    return Ok(true);
                }
                let ours = Hello::ours(hello.role);
                let between = Mismatch::between(&hello, &ours);
                conn.send(&Greeting::Hello(ours));
                // A mismatch is answered whatever the role: a client of
                // another protocol may say one this protocol has not.
                match (between, hello.role) {
                    (Err(mismatch), _) => {
                        conn.end(session, &Greeting::Exit(&mismatch.to_string()), now);
                    }
                    (Ok(()), Role::Attach) => conn.enter(session, Stage::Attaching, now),
                    (Ok(()), Role::Command) => conn.enter(session, Stage::Command, now),
                    (Ok(()), Role::Unknown(byte)) => {
                        let why = format!("role {byte} is none of this protocol's");
                        conn.end(session, &Greeting::Exit(&why), now);
                    }
                }
            }
            Stage::Attaching => {
                let Some(attach) = decoder.frame::<Attach>()? else {
                    return Ok(false);
                };
                conn.bodies.pop_front();
                conn.attach(session, attach, now);
            }
            Stage::Attached(attached) => {
                let frame = decoder.frame()?;
                let body = frame
                    .as_ref()
                    .and_then(|_| conn.bodies.pop_front())
                    .flatten();
                match frame {
                    Some(AttachedFrame::Act(act)) => {
                        session.give_body(body);
                        attached.act(session, *act, now);
                        session.give_body(None);
                    }
                    // The client's terminal was resized, which may have moved
                    // what it shows even if the size it ends at is the same:
                    // forgotten, it is repainted in full either way, and its
                    // cell size, which a font zoom changes, is asked again.
                    Some(AttachedFrame::Resize(size)) => {
                        session.terminal_resized(attached.client);
                        let actor = session.parts.actor(Actor::Client(attached.client));
                        let resized =
                            session.resize(attached.client, size.rows(), size.cols(), now);
                        session.parts.actor(actor);
                        if let Err(refused) = resized {
                            session.notify_to(attached.client, refused.to_string(), true);
                        }
                    }
                    Some(AttachedFrame::Detach) => {
                        let actor = session.parts.actor(Actor::Client(attached.client));
                        conn.exit(session, Detach::Detached, "detached", now);
                        session.parts.actor(actor);
                    }
                    Some(AttachedFrame::Answer(asked, value)) => {
                        answered = Some((attached.client, asked, value));
                    }
                    None => return Ok(false),
                }
                if let Some(attached) = conn.attached() {
                    if let Some(next) = attached.processed.checked_add(1) {
                        attached.processed = next;
                    } else {
                        let text = "attached frame count exhausted";
                        conn.exit(session, Detach::Failed, text, now);
                    }
                }
            }
            // The last answer is still being written: the next request waits.
            Stage::Command if !conn.out.is_empty() => return Ok(false),
            Stage::Command => {
                let Some(asked) = decoder.frame()? else {
                    return Ok(false);
                };
                conn.body = conn.bodies.pop_front().flatten();
                self.ordered_request(index, asked, now);
            }
            // Nothing more is asked of a waiting command. A
            // connection that is ending has had its last word: what came
            // with the Detach, Command or refused Hello that ended it is
            // dropped unread.
            Stage::Blocked(_) | Stage::Waiting(_) | Stage::Closing | Stage::Dead => {
                *decoder = Decoder::default();
                return Ok(false);
            }
        }
        if let Some((client, asked, value)) = answered {
            self.answered(client, asked, value, now);
        }
        self.deliver_bodies(index);
        Ok(true)
    }

    /// What panes used up (`Retired`), their askers' connections hold no
    /// more; and keys for a client gone, which their askers' are told.
    fn retire(&mut self) {
        let retired: Vec<u64> = (mem::take(&mut self.session.retired).into_iter())
            .map(|(_, token)| token)
            .collect();
        let views = &self.session.views;
        let mut lost = Vec::new();
        for conn in &mut self.conns {
            let mut theirs = Vec::new();
            conn.tokens.retain(|&(holder, token)| match holder {
                Holder::Client(client, _) if !views.contains_key(&client) => {
                    theirs.push(token);
                    false
                }
                Holder::Client(..) => true,
                Holder::Pane(_) => !retired.contains(&token),
            });
            for &token in &theirs {
                conn.tell_process(&crate::link::ToConn::Drop { token });
            }
            lost.append(&mut theirs);
        }
        // Its client gone, its connection, detaching it, is held up by
        // none of them.
        for token in lost {
            self.let_go(token);
        }
    }

    /// The bodies connection `index` keeps that its request or act sent
    /// panes asking for (`Session::deliveries`): it sends them the panes.
    fn deliver_bodies(&mut self, index: usize) {
        let deliveries = mem::take(&mut self.session.deliveries);
        let Some(conn) = self.conns.get_mut(index) else {
            return;
        };
        for (body, token, panes) in deliveries {
            (conn.tokens).extend(panes.iter().map(|pane| (Holder::Pane(*pane), token)));
            conn.tell_process(&crate::link::ToConn::Deliver { body, token, panes });
        }
    }

    /// A client's answer to the question numbered `asked` it was relayed
    /// (`Waiting::Read`): the reply of the request that asked it, if that
    /// still waits; else nobody's.
    fn answered(&mut self, client: ClientId, asked: u64, value: Data, now: Instant) {
        // Keys sent it before the question: taken, so typed.
        for conn in &mut self.conns {
            let held = |(holder, _): &&(Holder, u64)| *holder == Holder::Client(client, asked);
            if let Some(&typed) = conn.tokens.iter().find(held) {
                conn.tokens.remove(&typed);
                conn.tell_process(&crate::link::ToConn::Typed { token: typed.1 });
                for conn in &mut self.conns {
                    conn.keys.remove(&typed.1);
                }
                return;
            }
        }
        let asks = |w: &Waiting| matches!(*w, Waiting::Read { client: c, asked: a } if (c, a) == (client, asked));
        let asker = (self.conns.iter_mut())
            .find(|conn| matches!(&conn.stage, Stage::Waiting(w) if asks(w)));
        if let Some(conn) = asker {
            conn.answer(&mut self.session, Body::Read(value).into(), now);
        }
    }

    /// How many attached frames the source has yet to process: zero with
    /// no fence, or None if the source is not attached in this server run.
    fn unprocessed(&self, asked: &Asked) -> Option<u64> {
        let Some(fence) = &asked.after else {
            return Some(0);
        };
        if fence.run != self.session.run {
            return None;
        }
        self.conns.iter().find_map(|conn| {
            let Stage::Attached(attached) = &conn.stage else {
                return None;
            };
            (attached.client == fence.client)
                .then(|| fence.frames.saturating_sub(attached.processed))
        })
    }

    /// Begins only after the source's earlier attached frames were
    /// processed. No resolution, mutation or wait baseline precedes this.
    fn ordered_request(&mut self, index: usize, asked: Asked, now: Instant) {
        // Send older output and any mode endings its detaches produce first.
        // Each repeat requires another client to detach.
        while self
            .session
            .outbox
            .iter()
            .any(|o| matches!(o, Outgoing::To(..)))
        {
            self.send_frames(now);
        }
        match self.unprocessed(&asked) {
            Some(0) => self.request(
                index,
                asked.request,
                asked.context,
                asked.after.map(|fence| fence.client),
                now,
            ),
            Some(_) => {
                if let Some(conn) = self.conns.get_mut(index) {
                    conn.enter(&mut self.session, Stage::Blocked(Box::new(asked)), now);
                }
            }
            None => {
                if let Some(conn) = self.conns.get_mut(index) {
                    let reply = Reply::failed(
                        Code::NoClient,
                        "the job's source is not attached in this server run",
                    );
                    conn.answer(&mut self.session, reply, now);
                }
            }
        }
    }

    /// A request from connection `index`: answered now, or once its wait has an answer.
    fn request(
        &mut self,
        index: usize,
        request: fux_wire::command::Request,
        context: Context,
        source: Option<ClientId>,
        now: Instant,
    ) {
        let session = &mut self.session;
        let Some(conn) = self.conns.get_mut(index) else {
            return;
        };
        if let Err(error) = session.check_run(context.of) {
            conn.answer(session, Reply::failed(error.code(), error.to_string()), now);
            return;
        }
        let Context {
            cwd,
            pane,
            server,
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
        // A program run as a client (`str c2 client focused`) names one
        // that must be there.
        // A key's or the prompt's program acts where its client is.
        let cli = Origin::Cli(Caller {
            gave: pane,
            within,
            cwd: Some(cwd),
        });
        let actor = source.map_or_else(|| session.request_actor(&cli), Actor::Client);
        let origin = match client {
            Some(c) if !session.views.contains_key(&c) => {
                let outcome = Reply::failed(Code::NoClient, format!("no client {c}"));
                conn.answer(session, outcome, now);
                return;
            }
            Some(client) => Origin::ClientContext { client, actor },
            None => cli,
        };
        let body = self.conns.get_mut(index).and_then(|conn| conn.body.take());
        self.session.give_body(body);
        let begun = self.session.begin(request, &origin, now);
        self.session.give_body(None);
        self.deliver_bodies(index);
        self.send_frames(now);
        let session = &mut self.session;
        let Some(conn) = self.conns.get_mut(index) else {
            return;
        };
        match begun {
            Begun::Done(outcome) => conn.answer(session, outcome, now),
            Begun::Waiting(waiting) => conn.enter(session, Stage::Waiting(waiting), now),
        }
    }

    /// Pane `id`'s link is ready: what its process said is taken in, in
    /// order, and what is queued for it written.
    fn serve_pane(&mut self, id: PaneId, flags: PollFlags, now: Instant) {
        let session = &mut self.session;
        let link = (session.links.get_mut(&id))
            .or_else(|| (session.ending.iter_mut()).find_map(|(e, l)| (*e == id).then_some(l)));
        let Some(Link::Remote(remote)) = link else {
            return;
        };
        let mut closed = false;
        if flags.intersects(PollFlags::OUT) && remote.end.flush().is_err() {
            closed = true;
        }
        if flags.intersects(PollFlags::IN | PollFlags::HUP | PollFlags::ERR)
            && remote.end.read(self.read_buffer.as_mut_slice()) == Read::Closed
        {
            closed = true;
        }
        let mut heard = Vec::new();
        loop {
            match remote.end.next_message() {
                Ok(Some(report)) => heard.push(report),
                Ok(None) => break,
                Err(error) => {
                    log!("{id}'s own process sent a bad frame: {error}");
                    closed = true;
                    break;
                }
            }
        }
        for report in heard {
            self.session.heard(id, report);
        }
        self.session.pump(now);
        if closed {
            self.session.link_closed(id, now);
        }
    }

    /// Writes what is queued for each pane's process, as far as its link
    /// takes it now: the rest as the link takes more.
    fn flush_links(&mut self) {
        let session = &mut self.session;
        let links = (session.links.values_mut()).chain(session.ending.iter_mut().map(|(_, l)| l));
        for link in links {
            if let Link::Remote(remote) = link
                && !remote.closed
            {
                remote.flush();
            }
        }
    }

    /// Reaps each pane's own process and each program adopted from one
    /// that ended (`Session::children`), since signals coalesce, and the
    /// other orphans the server adopted.
    fn reap(&mut self, now: Instant) {
        let children = self.session.children();
        for pid in &children {
            if let Ok(Some(status)) = fuxix::process::reap(*pid) {
                self.session.reaped(*pid, status, now);
            }
        }
        let children = self.session.children();
        fuxix::process::reap_orphans(|pid| children.contains(&pid));
    }

    /// Waits for the panes' own processes, told to end, at most a second: one
    /// stuck in the kernel is left to init rather than hold the server's
    /// exit. Each ends its program as it ends.
    fn finish(&mut self) {
        for _ in 0..500 {
            self.flush_links();
            self.reap(Instant::now());
            if self.session.children().is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        // The run is over, and its panes' sockets gone with them: so is
        // where it kept them, unless the next run's server made it again.
        if let Some(socket) = self.session.socket() {
            let _ = fs::remove_dir(crate::socket::panes(socket));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delivery_departures_answer_event_reads_in_the_same_turn()
    -> Result<(), Box<dyn std::error::Error>> {
        let (mut server, _peers, client) = ordered_fixture()?;
        let now = Instant::now();
        server.phase = Phase::Stopping(after(now, Duration::from_secs(60)));
        server.session.outbox.clear();
        let reader = crate::journal::Reader::new(
            fux_wire::inspect::Scope::Client(client),
            server.session.parts.journal(),
            server.session.run,
            None,
        )?;
        server.conns.first_mut().ok_or("worker")?.stage =
            Stage::Waiting(Box::new(Waiting::Events(crate::journal::Read::new(
                reader,
                after(now, Duration::from_secs(30)),
            ))));
        server
            .session
            .outbox
            .push(Outgoing::To(client, ToAttached::Exit("test")));
        let _ = server.turn(now, &mut Vec::new());
        assert!(!server.session.views.contains_key(&client));
        assert!(
            matches!(
                server.conns.first().map(|conn| &conn.stage),
                Some(Stage::Command)
            ),
            "delivery left the event read waiting"
        );
        Ok(())
    }

    /// A stop that a request itself asks for, killing the last pane, is
    /// journaled before any read is answered: a read of everything, armed
    /// at the end, gets the closes and the numbered `stopped` entry in one
    /// batch, which ends it. The turn then wakes for the stop's deadline,
    /// with a read armed or none.
    #[test]
    fn a_stop_a_request_asks_for_ends_armed_reads_with_its_entry()
    -> Result<(), Box<dyn std::error::Error>> {
        for armed in [true, false] {
            let (mut server, _peers, _) = ordered_fixture()?;
            let now = Instant::now();
            let name = format!("fux-stop-{}-{armed}", std::process::id());
            let dir = fuxix::file::scratch()?.join(name);
            let socket = crate::socket::checked(dir.join("s"), "test")?;
            server.phase = Phase::Serving {
                listening: Listening::open(&socket)?,
                resting: None,
            };
            // No client attached, whose paint would deliver the stop first.
            let source = server.conns.get_mut(1).ok_or("source")?;
            source.enter(&mut server.session, Stage::Dead, now);
            server.session.outbox.clear();
            let journal = server.session.parts.journal();
            let end = journal.end(server.session.run);
            if armed {
                let reader = crate::journal::Reader::new(
                    fux_wire::inspect::Scope::All,
                    journal,
                    server.session.run,
                    None,
                )?;
                let read = crate::journal::Read::new(reader, after(now, Duration::from_secs(30)));
                server.conns.first_mut().ok_or("worker")?.stage =
                    Stage::Waiting(Box::new(Waiting::Events(read)));
            }
            let kill = fux_wire::command::Command::Kill {
                target: Some(fux_wire::command::AnyRef::Pane(PaneId::of(1))),
            };
            let cli = Origin::Cli(Caller::default());
            let Begun::Done(_) =
                server
                    .session
                    .begin(fux_wire::command::Request::Do(kill), &cli, now)
            else {
                return Err("the kill waited".into());
            };
            let flow = server.turn(now, &mut Vec::new());
            let due = server.wakes().map(|(at, _)| at).min();
            assert_eq!(flow, ControlFlow::Continue(due), "armed {armed}");
            assert!(due.is_some_and(|at| at <= after(now, STOP_WAIT)));
            let mut decoder = Decoder::default();
            decoder.push(&client_bytes(server.conns.first().ok_or("worker")?));
            let part = match decoder.frame()? {
                Some(ToCommand::Part(part)) => Some(part),
                _ => None,
            };
            drop(server);
            fs::remove_dir_all(dir)?;
            if !armed {
                continue;
            }
            let part = part.ok_or("the read was not answered")?;
            let reply = fux_wire::wire::Reader::new(&part).whole::<Reply>()?;
            let fux_wire::protocol::Response::Returned(fux_wire::protocol::Body::Events(batch)) =
                reply.response
            else {
                return Err(format!("{reply:?}").into());
            };
            let names: Vec<_> = (batch.events.iter())
                .filter_map(|item| item.data.get("event"))
                .collect();
            let named = |name: &str| fux_wire::json::Json::str(name);
            assert_eq!(names.first(), Some(&&named("closed")), "{names:?}");
            assert_eq!(names.last(), Some(&&named("stopped")), "{names:?}");
            let stopped = batch.events.last().ok_or("stopped")?;
            assert_eq!(stopped.origin, fux_wire::events::Actor::Outside);
            assert_eq!(batch.next, stopped.next);
            assert!(stopped.next.seq > end.seq);
            assert_eq!(
                batch.status,
                fux_wire::events::Status::Stopped("the last pane closed".into())
            );
        }
        Ok(())
    }

    #[test]
    fn an_answered_wait_leaves_no_cached_deadline() -> Result<(), Box<dyn std::error::Error>> {
        let (mut server, _peers, _) = ordered_fixture()?;
        let now = Instant::now();
        let stop = after(now, Duration::from_secs(10));
        server.phase = Phase::Stopping(stop);
        server
            .conns
            .get_mut(1)
            .ok_or("source")?
            .enter(&mut server.session, Stage::Dead, now);
        server.session.outbox.clear();
        let pane = PaneId::of(1);
        let counts = server
            .session
            .parts
            .panes()
            .get(&pane)
            .ok_or("pane")?
            .counts();
        let waiting = Waiting::new(
            crate::wait::Watches::Pane((pane, counts)),
            server.session.run,
            now,
            Some(Duration::from_secs(1)),
        );
        server.conns.first_mut().ok_or("worker")?.stage = Stage::Waiting(Box::new(waiting));
        server.session.output(pane, b"changed", now);
        assert_eq!(
            server.turn(now, &mut Vec::new()),
            ControlFlow::Continue(Some(stop))
        );
        Ok(())
    }

    #[test]
    fn due_frames_precede_expired_wait_answers() -> Result<(), Box<dyn std::error::Error>> {
        let (mut server, _peers, _) = ordered_fixture()?;
        let now = Instant::now();
        server.phase = Phase::Stopping(after(now, Duration::from_secs(10)));
        let pane = PaneId::of(1);
        server.session.output(pane, b"\x1b[?2026hheld", now);
        let counts = server
            .session
            .parts
            .panes()
            .get(&pane)
            .ok_or("pane")?
            .counts();
        let waiting = Waiting::new(
            crate::wait::Watches::Pane((pane, counts)),
            server.session.run,
            after(now, Duration::from_millis(1)),
            Some(crate::pane::FRAME_TIMEOUT),
        );
        let late = after(waiting.due().ok_or("deadline")?, Duration::from_millis(1));
        server.conns.first_mut().ok_or("worker")?.stage = Stage::Waiting(Box::new(waiting));
        assert!(server.turn(late, &mut Vec::new()).is_continue());
        let mut decoder = Decoder::default();
        decoder.push(&client_bytes(server.conns.first().ok_or("worker")?));
        let Some(ToCommand::Part(part)) = decoder.frame()? else {
            return Err("wait was not answered".into());
        };
        let reply = fux_wire::wire::Reader::new(&part).whole::<Reply>()?;
        assert!(
            matches!(
                reply.response,
                fux_wire::protocol::Response::Answered(fux_wire::answer::PaneResults::One(
                    fux_wire::answer::PaneAnswer::Changed { .. }
                ))
            ),
            "{reply:?}"
        );
        Ok(())
    }

    /// A session with a client attached on a connection: the session, the
    /// connection, and the client's end of its socket.
    fn attached() -> Result<(Session, Conn, UnixStream), String> {
        let (session, client) = crate::session::testing::attached(10, 40)?;
        let (stream, peer) = UnixStream::pair().map_err(|e| e.to_string())?;
        let mut conn =
            linked(stream, Some(crate::link::Expect::Attached)).map_err(|e| e.to_string())?;
        conn.stage = Stage::Attached(Box::new(Attached::new(client)));
        Ok((session, conn, peer))
    }

    /// What `conn` was given for its client, not yet passed on, and passed
    /// to its process and not yet sent it.
    fn client_bytes(conn: &Conn) -> Vec<u8> {
        let mut bytes = Vec::new();
        for message in conn.link.queued::<crate::link::ToConn>() {
            if let crate::link::ToConn::Bytes(sent) = message {
                bytes.extend_from_slice(&sent);
            }
        }
        bytes.extend_from_slice(conn.out.as_slice());
        bytes
    }

    /// Waits for connection `index`'s process to say what it has to, and
    /// takes it in, as the server's next poll would.
    fn heard(server: &mut Server, index: usize) -> Result<(), String> {
        let conn = server.conns.get(index).ok_or("no connection")?;
        let mut fds = [PollFd::new(&conn.link.stream, PollFlags::IN)];
        fuxix::poll::poll(&mut fds, Some(Duration::from_secs(3))).map_err(|e| e.to_string())?;
        server.read_conn(index, Instant::now());
        Ok(())
    }

    /// A connection whose process runs in a thread of the test's, its
    /// client the far end of `client`, expecting `expect`'s frames.
    fn linked(client: UnixStream, expect: Option<crate::link::Expect>) -> std::io::Result<Conn> {
        let (link, theirs) = UnixStream::pair()?;
        let theirs = crate::link::End::new(theirs)?;
        std::thread::spawn(move || crate::conn::serve(client, theirs, Vec::new()));
        let mut conn = Conn::new(link)?;
        if let Some(expect) = expect {
            conn.tell_process(&crate::link::ToConn::Expect(expect));
        }
        Ok(conn)
    }

    fn server() -> Result<(Server, UnixStream), Box<dyn std::error::Error>> {
        let (session, conn, peer) = attached()?;
        Ok((
            Server {
                session,
                phase: Phase::Stopping(Instant::now()),
                conns: vec![conn],
                children: fuxix::signal::Pipe::new(&[])?,
                stops: fuxix::signal::Pipe::new(&[])?,
                read_buffer: read_buffer()?,
                slots: Vec::new(),
                pollfds: fuxix::poll::Poll::default(),
                ready: Vec::new(),
                spare: None,
                spare_waits: false,
                accept_logged: None,
            },
            peer,
        ))
    }

    /// A command's connection gone while its question waits: the question
    /// is waited for no more, and its answer, come after, is let go
    /// (D-123), however the connection goes.
    #[test]
    fn a_connection_gone_waiting_lets_its_question_go() -> Result<(), Box<dyn std::error::Error>> {
        let (mut server, _peer) = server()?;
        let (stream, client) = UnixStream::pair()?;
        let mut conn = linked(stream, Some(crate::link::Expect::Asked))?;
        conn.stage = Stage::Command;
        server.conns.push(conn);
        // Asked, and its answer not yet heard.
        let pane = PaneId::of(1);
        let tab = (server.session.locate(pane).map(|(_, tab)| tab)).ok_or("its tab")?;
        let what = crate::link::Ask::Capture {
            history: None,
            view: fux_wire::command::CaptureView::default(),
            tab,
            hold: false,
        };
        let ask = &server.session.ask_pane(pane, what, Instant::now())?;
        let pending = crate::wait::Pending::new(vec![(pane, *ask)], crate::wait::Form::Capture);
        let waiting = Box::new(Waiting::Asked(pending));
        let conn = server.conns.last_mut().ok_or("its connection")?;
        conn.enter(&mut server.session, Stage::Waiting(waiting), Instant::now());
        drop(client);
        let index = server.conns.len().saturating_sub(1);
        let deadline = after(Instant::now(), std::time::Duration::from_secs(10));
        while server.conns.len() > 1 {
            if Instant::now() > deadline {
                return Err("its connection was kept".into());
            }
            server.serve_conn(index, PollFlags::IN | PollFlags::OUT, Instant::now());
            server.close_conns(Instant::now());
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        server.session.pump(Instant::now());
        assert!(server.session.answered_yet(*ask), "still waited for");
        assert!(server.session.answer_to(*ask).is_none(), "its answer kept");
        Ok(())
    }

    /// What a pane used up of what a connection held for it is let go of
    /// it: a connection that lives on keeps only what is still to come.
    #[test]
    fn what_a_pane_used_up_is_let_go() -> Result<(), Box<dyn std::error::Error>> {
        let (mut server, _peer) = server()?;
        let pane = PaneId::of(1);
        let conn = server.conns.first_mut().ok_or("its connection")?;
        conn.tokens.insert((Holder::Pane(pane), 7));
        conn.tokens.insert((Holder::Pane(pane), 8));
        server.session.retired.push((pane, 7));
        server.retire();
        let conn = server.conns.first().ok_or("its connection")?;
        assert_eq!(
            conn.tokens.iter().copied().collect::<Vec<_>>(),
            [(Holder::Pane(pane), 8)]
        );
        Ok(())
    }

    /// A command client gone, its connection is dropped once its process
    /// ends: none is kept polled on a closed link, which spun an idle
    /// server.
    #[test]
    fn a_finished_commands_connection_is_dropped() -> Result<(), Box<dyn std::error::Error>> {
        let (mut server, _peer) = server()?;
        let (stream, client) = UnixStream::pair()?;
        let mut conn = linked(stream, Some(crate::link::Expect::Asked))?;
        conn.stage = Stage::Command;
        server.conns.push(conn);
        drop(client);
        let deadline = after(Instant::now(), std::time::Duration::from_secs(10));
        while server.conns.len() > 1 {
            if Instant::now() > deadline {
                return Err("its connection was kept".into());
            }
            server.serve_conn(1, PollFlags::IN | PollFlags::OUT, Instant::now());
            server.close_conns(Instant::now());
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        Ok(())
    }

    /// The frames waiting for the client: what is painted in them, and the
    /// `Exit`'s reason.
    /// Worker first, source second, unrelated command third. No process or
    /// listener is started; each turn is chosen by the test.
    fn ordered_fixture() -> Result<(Server, Vec<UnixStream>, ClientId), Box<dyn std::error::Error>>
    {
        let (session, source, peer) = attached()?;
        let client = session.views.keys().next().copied().ok_or("no client")?;
        let command = || -> Result<(Conn, UnixStream), std::io::Error> {
            let (stream, peer) = UnixStream::pair()?;
            let mut conn = linked(stream, Some(crate::link::Expect::Asked))?;
            conn.stage = Stage::Command;
            Ok((conn, peer))
        };
        let (worker, worker_peer) = command()?;
        let (unrelated, unrelated_peer) = command()?;
        let conns = vec![worker, source, unrelated];
        let peers = vec![worker_peer, peer, unrelated_peer];
        let server = Server {
            session,
            phase: Phase::Stopping(Instant::now()),
            conns,
            children: fuxix::signal::Pipe::new(&[])?,
            stops: fuxix::signal::Pipe::new(&[])?,
            read_buffer: read_buffer()?,
            slots: Vec::new(),
            pollfds: fuxix::poll::Poll::default(),
            ready: Vec::new(),
            spare: None,
            spare_waits: false,
            accept_logged: None,
        };
        Ok((server, peers, client))
    }

    fn key_request(run: fux_wire::command::Run, client: ClientId, frames: u64) -> Asked {
        Asked {
            after: Some(fux_wire::protocol::Fence {
                run,
                client,
                frames,
            }),
            context: Context {
                client: Some(client),
                ..Context::default()
            },
            request: fux_wire::command::Command::SendKeys {
                to: fux_wire::command::To::Now(fux_wire::command::AnyRef::Pane(PaneId::of(1))),
                inputs: vec![fux_wire::command::Input::Text("Z".into())],
            }
            .into(),
        }
    }

    /// A queued prefix larger than a fair turn always precedes the job,
    /// even with the worker served first and frames split across reads.
    #[test]
    fn a_job_cannot_overtake_its_attached_prefix() -> Result<(), Box<dyn std::error::Error>> {
        const KEYS: usize = 3000;
        for partition in [1, 257, usize::MAX] {
            let (mut server, _peers, client) = ordered_fixture()?;
            let key = AttachedFrame::Act(Box::new(fux_wire::act::Act::Key(
                fux_vt::keys::KeyPress::char('a').into(),
            )))
            .encode();
            let resize = AttachedFrame::Resize(fux_vt::Size::new(10, 40)?).encode();
            let mut prefix = Vec::new();
            for _ in 0..KEYS {
                prefix.extend_from_slice(&key);
            }
            prefix.extend_from_slice(&resize);
            let frames = u64::try_from(KEYS + 1)?;
            let asked = key_request(server.session.run, client, frames);
            server
                .conns
                .get_mut(0)
                .ok_or("worker")?
                .decoder
                .push(&asked.encode());
            server.serve_conn(0, PollFlags::IN, Instant::now());
            assert!(matches!(
                server.conns.first().map(|c| &c.stage),
                Some(Stage::Blocked(_))
            ));
            assert!(!server.conns.first().ok_or("worker")?.frame_waits());
            // A blocked worker must not force zero-time polls.
            let mut ready = Vec::new();
            server.poll(Some(Duration::ZERO), &mut ready);
            assert!(
                !ready
                    .iter()
                    .any(|(slot, flags)| matches!(slot, Slot::Conn(0))
                        && flags.intersects(PollFlags::IN))
            );

            let unrelated = Asked {
                after: None,
                request: fux_wire::command::Command::Inspect {
                    scope: fux_wire::inspect::Scope::All,
                    projection: fux_wire::inspect::Projection::Facts,
                }
                .into(),
                context: Context::default(),
            };
            server
                .conns
                .get_mut(2)
                .ok_or("unrelated")?
                .decoder
                .push(&unrelated.encode());
            let mut left = prefix.as_slice();
            while !left.is_empty() {
                let (chunk, rest) = left
                    .split_at_checked(partition.min(left.len()))
                    .ok_or("partition")?;
                left = rest;
                server.conns.get_mut(1).ok_or("source")?.decoder.push(chunk);
                server.serve_conn(0, PollFlags::IN, Instant::now());
                let before = server
                    .conns
                    .get_mut(1)
                    .and_then(Conn::attached)
                    .ok_or("source")?
                    .processed;
                server.serve_conn(1, PollFlags::IN | PollFlags::OUT, Instant::now());
                let after = server
                    .conns
                    .get_mut(1)
                    .and_then(Conn::attached)
                    .ok_or("source")?
                    .processed;
                assert!(after - before <= u64::try_from(CONN_FRAMES)?);
                if before == 0 && after > 0 {
                    assert!(after < frames);
                    if partition == usize::MAX {
                        assert!(server.conns.get(1).ok_or("source")?.frame_waits());
                    }
                    server.serve_conn(2, PollFlags::IN, Instant::now());
                    assert!(!client_bytes(server.conns.get(2).ok_or("unrelated")?).is_empty());
                }
            }
            while matches!(server.unprocessed(&asked), Some(1..)) {
                server.serve_conn(0, PollFlags::IN, Instant::now());
                let before = server
                    .conns
                    .get_mut(1)
                    .and_then(Conn::attached)
                    .ok_or("source")?
                    .processed;
                assert!(
                    server
                        .session
                        .host_mut(PaneId::of(1), |h| h.pane().input().len(), Instant::now())
                        .ok_or("pane")?
                        <= KEYS
                );
                server.serve_conn(1, PollFlags::IN | PollFlags::OUT, Instant::now());
                let after = server
                    .conns
                    .get_mut(1)
                    .and_then(Conn::attached)
                    .ok_or("source")?
                    .processed;
                assert!(after > before && after - before <= u64::try_from(CONN_FRAMES)?);
            }
            server.poll(Some(Duration::ZERO), &mut ready);
            assert!(ready.iter().any(|(slot, _)| matches!(slot, Slot::Conn(0))));
            server.serve_conn(0, PollFlags::IN, Instant::now());
            let got = server
                .session
                .host_mut(
                    PaneId::of(1),
                    |h| h.pane_mut().drain_input(),
                    Instant::now(),
                )
                .ok_or("pane")?;
            let mut expected = vec![b'a'; KEYS];
            expected.push(b'Z');
            assert_eq!(got, expected, "partition {partition}");
        }
        Ok(())
    }

    /// Client ids reused by another server run cannot satisfy an old job.
    #[test]
    fn a_fence_only_names_its_original_server() -> Result<(), Box<dyn std::error::Error>> {
        let (mut server, _peers, client) = ordered_fixture()?;
        server.ordered_request(
            0,
            key_request("1-2".parse().map_err(|()| "test run")?, client, 0),
            Instant::now(),
        );
        let conn = server.conns.first().ok_or("worker")?;
        assert!(matches!(conn.stage, Stage::Command));
        assert!(!client_bytes(conn).is_empty());
        assert!(
            server
                .session
                .host_mut(
                    PaneId::of(1),
                    |h| h.pane().input().is_empty(),
                    Instant::now()
                )
                .ok_or("pane")?
        );
        Ok(())
    }

    /// Cancelling a blocked request drops it; losing its source refuses it.
    /// Neither path leaves a held request behind after connections move.
    #[test]
    fn a_blocked_job_ends_with_its_command_or_source() -> Result<(), Box<dyn std::error::Error>> {
        for ending in ["cancel", "eof", "detach"] {
            let (mut server, peers, client) = ordered_fixture()?;
            server.ordered_request(
                0,
                key_request(server.session.run, client, 1000),
                Instant::now(),
            );
            assert!(matches!(
                server.conns.first().map(|c| &c.stage),
                Some(Stage::Blocked(_))
            ));
            if ending == "cancel" {
                peers
                    .first()
                    .ok_or("worker")?
                    .shutdown(std::net::Shutdown::Both)?;
                heard(&mut server, 0)?;
                assert!(matches!(
                    server.conns.first().map(|c| &c.stage),
                    Some(Stage::Dead)
                ));
            } else {
                if ending == "eof" {
                    peers
                        .get(1)
                        .ok_or("source")?
                        .shutdown(std::net::Shutdown::Both)?;
                } else {
                    server
                        .conns
                        .get_mut(1)
                        .ok_or("source")?
                        .decoder
                        .push(&AttachedFrame::Detach.encode());
                }
                match ending == "eof" {
                    true => heard(&mut server, 1)?,
                    false => server.serve_conn(1, PollFlags::IN, Instant::now()),
                }
                if let Some(source) = server.conns.get_mut(1) {
                    source.out = ByteQueue::default();
                }
                server.close_conns(Instant::now());
                server.serve_conn(0, PollFlags::IN, Instant::now());
                let conn = server.conns.first().ok_or("worker")?;
                assert!(matches!(conn.stage, Stage::Command));
                let mut decoder = Decoder::default();
                decoder.push(&client_bytes(conn));
                let Some(ToCommand::Part(part)) = decoder.frame()? else {
                    return Err("no refusal".into());
                };
                let reply = fux_wire::wire::Reader::new(&part).whole::<Reply>()?;
                assert!(matches!(
                    reply.end(),
                    fux_wire::protocol::End::Failed {
                        code: Code::NoClient,
                        ..
                    }
                ));
            }
            assert!(
                !server
                    .conns
                    .iter()
                    .any(|c| matches!(c.stage, Stage::Blocked(_)))
            );
        }
        Ok(())
    }

    /// What only a client knows is asked of it, and its answer with the
    /// question's number replies to the request that asked; a client that
    /// detaches first fails the request, `no_client`.
    #[test]
    fn a_read_is_answered_by_its_client_or_fails_as_it_detaches()
    -> Result<(), Box<dyn std::error::Error>> {
        use fux_wire::command::{ClientRead, Request};
        use fux_wire::protocol::{End, Response};
        for answers in [true, false] {
            let (mut server, _peers, client) = ordered_fixture()?;
            let read = Request::Read {
                client,
                read: ClientRead::Keys,
            };
            let now = Instant::now();
            server.request(0, read, Context::default(), None, now);
            let source = server.conns.get_mut(1).ok_or("source")?;
            let mut decoder = Decoder::default();
            decoder.push(&client_bytes(source));
            let mut asked = None;
            while let Some(frame) = decoder.frame::<ToAttached>()? {
                if let ToAttached::Read(n, ClientRead::Keys) = frame {
                    asked = Some(n);
                }
            }
            let asked = asked.ok_or("the client was not asked")?;
            let frame = match answers {
                true => AttachedFrame::Answer(asked, Data::Text("its keys".into())),
                false => AttachedFrame::Detach,
            };
            source.decoder.push(&frame.encode());
            server.serve_conn(1, PollFlags::IN, now);
            server.answer_waits(now);
            let conn = server.conns.first().ok_or("worker")?;
            assert!(matches!(conn.stage, Stage::Command));
            let mut decoder = Decoder::default();
            decoder.push(&client_bytes(conn));
            let Some(ToCommand::Part(part)) = decoder.frame()? else {
                return Err("no reply".into());
            };
            let reply = fux_wire::wire::Reader::new(&part).whole::<Reply>()?;
            match answers {
                true => assert_eq!(
                    reply.response,
                    Response::Returned(Body::Read(Data::Text("its keys".into())))
                ),
                false => assert!(matches!(
                    reply.end(),
                    End::Failed {
                        code: Code::NoClient,
                        ..
                    }
                )),
            }
        }
        Ok(())
    }

    /// Waits only for the worker socket: the test chooses every server turn.
    fn command_readable(server: &Server) -> Result<(), String> {
        let conn = server.conns.first().ok_or("worker")?;
        let mut fds = [PollFd::new(&conn.link.stream, PollFlags::IN)];
        match fuxix::poll::poll(&mut fds, Some(Duration::from_secs(3))) {
            Ok(0) => Err("the worker did not ask".into()),
            Ok(_) => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    }

    /// Handles and writes one greeting or reply without reading ahead into
    /// the next request, so a transport failure has an exact position.
    fn command_reply(server: &mut Server) -> Result<(), String> {
        while client_bytes(server.conns.first().ok_or("worker")?).is_empty() {
            command_readable(server)?;
            server.serve_conn(0, PollFlags::IN, Instant::now());
            if matches!(
                server.conns.first().ok_or("worker")?.stage,
                Stage::Closing | Stage::Dead
            ) {
                return Err("the worker closed before its request".into());
            }
        }
        server
            .conns
            .first_mut()
            .ok_or("worker")?
            .flush(&mut server.session, Instant::now());
        Ok(())
    }

    fn command_connection(server: &mut Server, listener: &UnixListener) -> Result<(), String> {
        let mut fds = [PollFd::new(listener, PollFlags::IN)];
        if fuxix::poll::poll(&mut fds, Some(Duration::from_secs(3))).map_err(|e| e.to_string())?
            == 0
        {
            return Err("the worker did not connect".into());
        }
        let (stream, _) = listener.accept().map_err(|e| e.to_string())?;
        let conn = linked(stream, None).map_err(|e| e.to_string())?;
        *server.conns.first_mut().ok_or("worker")? = conn;
        command_reply(server)
    }

    /// A failed fence and a successful prefix both remain the job's source
    /// identity: catching a refusal or reconnecting cannot bypass either.
    #[test]
    fn every_request_keeps_its_jobs_source_fence() -> Result<(), Box<dyn std::error::Error>> {
        use fux::script::Asks;
        use std::os::unix::fs::PermissionsExt;
        for rejected in [false, true] {
            let dir = std::env::temp_dir().join(format!("ff-{}-{rejected}", std::process::id()));
            fs::create_dir_all(&dir)?;
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
            let path = dir.join("s.sock");
            let listener = UnixListener::bind(&path)?;
            listener.set_nonblocking(true)?;
            let original = "1-2".parse().map_err(|()| "original run")?;
            let replacement = "2-3".parse().map_err(|()| "replacement run")?;
            let peer = std::thread::spawn(move || -> Result<Vec<u8>, String> {
                let (mut server, _peers, client) = ordered_fixture().map_err(|e| e.to_string())?;
                server.session.run = if rejected { replacement } else { original };
                command_connection(&mut server, &listener)?;
                command_reply(&mut server)?; // The first request.
                if !rejected {
                    command_readable(&server)?;
                    server
                        .conns
                        .first()
                        .ok_or("worker")?
                        .link
                        .stream
                        .shutdown(std::net::Shutdown::Both)
                        .map_err(|e| e.to_string())?;
                    // The same socket path now belongs to a different run
                    // that reused the original source's client id.
                    let (mut next, _next_peers, reused) =
                        ordered_fixture().map_err(|e| e.to_string())?;
                    assert_eq!(reused, client);
                    next.session.run = replacement;
                    command_connection(&mut next, &listener)?;
                    server = next;
                    // Keep the replacement attachment's peers alive until
                    // its last request has been served.
                    command_reply(&mut server)?;
                } else {
                    command_reply(&mut server)?;
                }
                Ok(server
                    .session
                    .host_mut(
                        PaneId::of(1),
                        |h| h.pane_mut().drain_input(),
                        Instant::now(),
                    )
                    .ok_or("pane")?)
            });
            let result = (|| -> Result<Reply, String> {
                let mut asks = fux::script::Server::new(path);
                let client = "c1".parse().map_err(|e| format!("client: {e}"))?;
                let mut mutation = key_request(original, client, 0);
                asks.after = mutation
                    .after
                    .take()
                    .map(|after| std::rc::Rc::new(std::cell::Cell::new(after)));
                let probe = Asked {
                    after: None,
                    context: mutation.context.clone(),
                    request: fux_wire::command::Command::Inspect {
                        scope: fux_wire::inspect::Scope::All,
                        projection: fux_wire::inspect::Projection::Facts,
                    }
                    .into(),
                };
                let first = asks.ask(&probe).map_err(|e| e.to_string())?;
                if rejected {
                    assert!(matches!(
                        first.end(),
                        fux_wire::protocol::End::Failed {
                            code: Code::NoClient,
                            ..
                        }
                    ));
                } else {
                    assert!(matches!(first.end(), fux_wire::protocol::End::Done));
                    assert!(asks.ask(&probe).is_err());
                }
                asks.ask(&mutation).map_err(|e| e.to_string())
            })();
            let input = peer.join().map_err(|_| "server thread panicked")?;
            fs::remove_dir_all(&dir)?;
            let reply = result
                .map_err(|e| format!("client rejected={rejected}: {e}; server: {input:?}"))?;
            let input = input?;
            assert!(
                matches!(
                    reply.end(),
                    fux_wire::protocol::End::Failed {
                        code: Code::NoClient,
                        ..
                    }
                ),
                "rejected={rejected}: {reply:?}"
            );
            assert!(input.is_empty(), "rejected={rejected}: {input:?}");
        }
        Ok(())
    }

    /// A client detaches saying why: disconnected as its connection
    /// ends, detached when it asks, and server_stopped as the server
    /// stops, before the stop's entry, which is the journal's last.
    #[test]
    fn a_detach_says_why() -> Result<(), Box<dyn std::error::Error>> {
        // Each event read from `from`, by name, and why if it says.
        let read = |session: &Session, from| -> Result<Vec<String>, String> {
            let journal = session.parts.journal();
            let scope = fux_wire::inspect::Scope::All;
            let mut reader = crate::journal::Reader::new(scope, journal, session.run, Some(from))?;
            let said =
                |e: &fux_wire::events::Item, key| e.data.get(key).map(fux_wire::json::Json::render);
            Ok((reader.batch(journal, true)?.events.iter())
                .map(|e| {
                    [said(e, "event"), said(e, "why")]
                        .into_iter()
                        .flatten()
                        .collect()
                })
                .collect())
        };
        let now = Instant::now();
        let (mut session, mut conn, _peer) = attached()?;
        let from = session.parts.journal().end(session.run);
        conn.enter(&mut session, Stage::Dead, now);
        assert_eq!(
            read(&session, from)?,
            [r#""detached""disconnected""#, r#""laid_out""#]
        );
        let (mut session, mut conn, _peer) = attached()?;
        let from = session.parts.journal().end(session.run);
        conn.exit(&mut session, Detach::Detached, "detached", now);
        assert_eq!(
            read(&session, from)?,
            [r#""detached""detached""#, r#""laid_out""#]
        );
        let (mut server, _peer) = server()?;
        let dir = fuxix::file::scratch()?.join(format!("fux-detach-{}", std::process::id()));
        let socket = crate::socket::checked(dir.join("s"), "test")?;
        server.phase = Phase::Serving {
            listening: Listening::open(&socket)?,
            resting: None,
        };
        let from = server.session.parts.journal().end(server.session.run);
        server.stop("asked".into(), Actor::Outside, now);
        assert_eq!(
            read(&server.session, from)?,
            [
                r#""detached""server_stopped""#,
                r#""laid_out""#,
                r#""stopped""#,
            ]
        );
        Ok(())
    }

    /// A connection's process that can't be started is tried again when
    /// the turn says to wake: the server waits on nothing else for it.
    #[test]
    fn a_spare_that_cannot_start_is_tried_again_by_the_turns_deadline()
    -> Result<(), Box<dyn std::error::Error>> {
        let (mut server, _peer) = server()?;
        let dir = fuxix::file::scratch()?.join(format!("fux-spare-{}", std::process::id()));
        let socket = crate::socket::checked(dir.join("s"), "test")?;
        server.phase = Phase::Serving {
            listening: Listening::open(&socket)?,
            resting: None,
        };
        // A turn with nothing to start settles what the session had to do,
        // so that the next does nothing else.
        server.session.exe = None;
        let now = Instant::now();
        let mut wakes = Vec::new();
        let ControlFlow::Continue(_) = server.turn(now, &mut wakes) else {
            return Err("the turn stopped the server".into());
        };
        server.session.exe = Some(dir.join("no-such-fux"));
        let ControlFlow::Continue(next) = server.turn(now, &mut wakes) else {
            return Err("the turn stopped the server".into());
        };
        let Phase::Serving {
            resting: Some(retry),
            ..
        } = server.phase
        else {
            return Err("no retry".into());
        };
        assert!(server.spare.is_none());
        assert_eq!(next, Some(retry));
        Ok(())
    }
}
