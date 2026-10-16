//! The server: one thread (but for fuxix's watchdog while a PTY opens on
//! macOS), one `poll` loop over the listening socket, a signal pipe, every
//! client and every pane's PTY.
use crate::bytes::ByteQueue;
use crate::code::Code;
use crate::events::{Event, Snapshot, Subscription};
use crate::id::{ClientId, PaneId};
use crate::json::Json;
use crate::layout::Placement;
use crate::pane::{Pane, Process};
use crate::process::{Leader, Pid};
use crate::protocol::{
    Attach, AttachedFrame, Decoder, End, Frame, Hello, MAX_PAYLOAD, Mismatch, Reply, Request, Role,
    ServerFrame,
};
use crate::render::{self, Grid};
use crate::session::{Begun, Caller, Dying, Origin, Outgoing, Session, Timer};
use crate::socket::{Endpoint, SocketPath};
use crate::view::ClientKind;
use crate::wait::Waiting;
use fuxix::poll::{Events as PollFlags, PollFd};
use signal_hook::consts::{SIGCHLD, SIGHUP, SIGINT, SIGTERM};
use std::io::{ErrorKind, Write};
use std::ops::ControlFlow;
use std::os::fd::OwnedFd;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
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
    /// None where the system denies sockets: the door alone serves.
    listener: Option<UnixListener>,
    /// The door beside the socket, for clients in a sandbox that denies
    /// them sockets (`crate::door`).
    door: Option<crate::door::Door>,
    /// Holds the lock; removes the socket once dropped, after the listener
    /// closes.
    _endpoint: Endpoint,
}

impl Listening {
    /// The socket at `socket` bound and its door opened; where the system
    /// denies sockets, the door alone, the lock held all the same.
    fn open(socket: &SocketPath) -> Result<Listening, Error> {
        let (endpoint, listener) = crate::socket::bind_socket(socket).map_err(Error::Socket)?;
        if let Some(listener) = &listener {
            listener.set_nonblocking(true).map_err(Error::Setup)?;
        }
        let door = match crate::door::Door::open(socket.path()) {
            Ok(door) => Some(door),
            Err(error) if listener.is_none() => return Err(Error::Setup(error)),
            Err(error) => {
                log!("no door beside the socket: {error}");
                None
            }
        };
        Ok(Listening {
            listener,
            door,
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
    Grace(PaneId),
    /// The listener's rest is over.
    Listen,
    /// A stopping server waits no more.
    Stop,
    /// A connection's client may be painted (`Conn::paint_due`).
    Paint(usize),
    /// A connection's wait is to be asked for its answer (`Waiting::due`).
    Answer,
    /// The layout is saved, if it changed.
    Save,
}

/// A client's connection: its socket, and where it is in its life.
struct Conn {
    stream: UnixStream,
    decoder: Decoder,
    out: ByteQueue,
    stage: Stage,
    /// The socket `fux grant` made that it came through, if one did: its
    /// commands reach that grant's workspace alone, while it lasts.
    grant: Option<PathBuf>,
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
    /// descriptor it sent, and the kind of client it is.
    Attaching(Option<OwnedFd>, ClientKind),
    /// Its client is attached: its input, resizes and detach come next.
    Attached(Box<Attached>),
    /// It said hello as a command client: its requests come next, each
    /// answered before the next is read.
    Command,
    /// Its request waits for the restore that runs to end, so that it takes
    /// no id the restore asks for (`Server::request`). It is read only to
    /// notice the client go.
    Held(Box<Request>),
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
            Stage::Hello(_)
            | Stage::Attaching(..)
            | Stage::Attached(_)
            | Stage::Command
            | Stage::Held(_)
            | Stage::Waiting(_)
            | Stage::Events(_) => true,
            Stage::Closing | Stage::Dead => false,
        }
    }
}

/// An attached client: its view's id, its terminal if the server took it,
/// and what its paints are made from.
struct Attached {
    client: ClientId,
    tty: Option<TakenTerminal>,
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
    /// The pictures its terminal was last sent, as they show there.
    pictures: Vec<render::Visible>,
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
/// reopened (`fuxix::terminal::reopen`): its paints are written here, not
/// relayed in frames, as tmux's server writes to its client's terminal.
/// Its keys are the client's, which reads them itself. It is held by its client's attachment alone,
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
            known: false,
            spare: Grid::new(0, 0),
            placement: Placement::default(),
            pictures: Vec::new(),
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
    fn new(stream: UnixStream, grant: Option<PathBuf>) -> Conn {
        Conn {
            stream,
            decoder: Decoder::default(),
            out: ByteQueue::default(),
            stage: Stage::Hello(None),
            grant,
        }
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
        let sent = self.out.is_empty() && attached.tty.as_ref().is_none_or(|t| t.out.is_empty());
        // A screen not known is painted whole, whether or not the view
        // changed: forgetting it is asking for that paint.
        ((dirty || !attached.known) && sent).then_some(attached.clock.due)
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

    /// Answers a request with its outcome, and takes the next.
    fn answer(&mut self, session: &mut Session, reply: Reply) {
        let bytes = crate::wire::encode(&reply);
        if bytes.len() < MAX_PAYLOAD {
            self.send(session, &ServerFrame::Reply(Box::new(reply)));
        } else {
            // Too large for one frame: in pieces, then the end.
            ServerFrame::split_into(&bytes, ServerFrame::Part, &mut self.out);
            self.send(session, &ServerFrame::PartsEnd);
        }
        if matches!(self.stage, Stage::Waiting(_) | Stage::Events(_)) {
            self.enter(session, Stage::Command);
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
    /// The sockets `fux grant` made, which the session's grants reach
    /// through.
    grants: Vec<(PathBuf, Listening)>,
    /// A grant was revoked: what waits or streams through it ends.
    revoked: bool,
    /// Where the layout is saved, for a server after a crash to bring
    /// back; none if there is nowhere to.
    saved: Option<Saved>,
    children: std::io::PipeReader,
    /// Closed panes' programs killed but not yet exited, reaped once SIGCHLD
    /// says they have.
    killed: Vec<Leader>,
    stops: std::io::PipeReader,
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

/// How long after a change the layout is saved at the latest.
const SAVE_EVERY: Duration = Duration::from_secs(1);

/// The layout a server saves as it changes, for the next server on its
/// socket to bring back after a crash or a reboot: in
/// `$XDG_STATE_HOME/fux` (`~/.local/state/fux`), which outlives a reboot,
/// written whole to a file of the user's alone, then moved in place.
struct Saved {
    path: PathBuf,
    /// What it holds.
    written: String,
    /// When it may be saved next.
    at: Instant,
    /// Something happened since it was last saved.
    pending: bool,
    /// The workspaces, tabs and panes it was last saved with.
    shape: Shape,
    /// A restore was started, and its end is yet to be seen to
    /// (`Server::after_restore`); while its thread runs, what there is is
    /// not saved over the file.
    restore: Option<std::thread::JoinHandle<()>>,
    /// The server stops: nothing more is saved (or what was, forgotten).
    done: bool,
}

/// Which workspaces, tabs and panes there are.
#[derive(Default, PartialEq)]
struct Shape(Vec<crate::id::WsId>, Vec<crate::id::TabId>, Vec<PaneId>);

impl Shape {
    fn of(session: &Session) -> Shape {
        let workspaces = &session.workspaces;
        Shape(
            workspaces.iter().map(|w| w.id).collect(),
            (workspaces.iter())
                .flat_map(|w| w.tabs().iter().map(|t| t.id))
                .collect(),
            session.panes.keys().copied().collect(),
        )
    }
}

impl Saved {
    /// Where the server on `socket` saves its layout, if anywhere.
    fn of(socket: &SocketPath) -> Option<Saved> {
        let base = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| {
                let home = std::env::var_os("HOME").map(PathBuf::from)?;
                Some(home.join(".local").join("state"))
            })?;
        // `/` read as `%`, and `%` itself as `%25`: no two sockets share one.
        let name = (socket.path().to_string_lossy())
            .replace('%', "%25")
            .replace('/', "%");
        let name = format!("{}.layout", name.trim_start_matches('%'));
        Some(Saved {
            path: base.join("fux").join(name),
            written: String::new(),
            at: Instant::now(),
            pending: false,
            shape: Shape::default(),
            restore: None,
            done: false,
        })
    }

    /// Brings back the session saved, if there is one, by running it, a
    /// fuxth program, as a client of the server on `socket` does: on a
    /// thread of its own, as the server serves it; that thread, if there is
    /// one: it is not saved over while it comes back.
    fn restore(&self, socket: &SocketPath) -> Option<std::thread::JoinHandle<()>> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(e) if e.kind() == ErrorKind::NotFound => return None,
            Err(e) => {
                log!("{}: {e}; starting afresh", self.path.display());
                return None;
            }
        };
        let (path, socket) = (self.path.clone(), socket.clone());
        let spawned = std::thread::Builder::new()
            .name("restore".into())
            .spawn(move || {
                match crate::script::restore(&socket, &path, &text) {
                    Ok(()) => log!("brought back the session in {}", path.display()),
                    Err(error) => {
                        // Kept aside, not saved over, for a person to look at.
                        let aside = path.with_extension("layout.failed");
                        let _ = std::fs::rename(&path, &aside);
                        log!(
                            "could not bring back {}: {error}; the file kept as {}",
                            path.display(),
                            aside.display()
                        );
                    }
                }
            });
        spawned
            .inspect_err(|error| log!("could not bring back {}: {error}", self.path.display()))
            .ok()
    }

    /// Saves the layout if it changed and `SAVE_EVERY` has passed since it
    /// was last saved; else it is saved then. A workspace, tab or pane made
    /// or closed is saved at once: a crash must not lose a pane.
    fn save_by(&mut self, session: &Session, now: Instant) {
        if self.done {
            return;
        }
        let shape = Shape::of(session);
        if now < self.at && shape == self.shape {
            self.pending = true;
            return;
        }
        self.save(session);
        self.shape = shape;
        self.at = crate::after(now, SAVE_EVERY);
    }

    /// Saves the layout as it is, if it changed and no restore is under
    /// way.
    fn save(&mut self, session: &Session) {
        if self.restoring() {
            self.pending = true;
            return;
        }
        self.pending = false;
        // Everything, as it is now, its ids kept.
        let text = session.dump_layout(None, true);
        if text == self.written {
            return;
        }
        match self.write(&text) {
            Ok(()) => self.written = text,
            Err(e) => log!("saving the layout to {}: {e}", self.path.display()),
        }
    }

    /// Writes `text` to a file of the user's alone beside it, and moves
    /// that in place: a crash leaves the old layout or the new, never half.
    fn write(&self, text: &str) -> std::io::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)?;
        }
        let temporary = self.path.with_extension("layout.new");
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&temporary, &self.path)
    }

    /// Whether a restore is under way: its thread has not ended, however
    /// it ended.
    fn restoring(&self) -> bool {
        (self.restore.as_ref()).is_some_and(|thread| !thread.is_finished())
    }

    /// Saves nothing more: the server stops. With `keep`, the layout as it
    /// is, before its panes end, is what the next brings back; else none
    /// is (the server was stopped on purpose, or its last pane closed).
    fn stop(&mut self, session: &Session, keep: bool) {
        match keep {
            true => self.save(session),
            false => self.forget_file(),
        }
        self.pending = false;
        self.done = true;
    }

    fn forget_file(&self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// `fux grant`: a socket made at `socket` (from the caller's directory
/// `cwd`), whose commands reach `workspace` alone; or `fux revoke`, the one
/// made there closed (and what came through it ends): whether it was.
fn grant(
    grants: &mut Vec<(PathBuf, Listening)>,
    session: &mut Session,
    (socket, workspace): (String, Option<crate::command::WsRef>),
    cwd: Option<PathBuf>,
) -> (Reply, bool) {
    let failed = |stderr: String| (Reply::failed(Code::Grant, stderr), false);
    let path = cwd.map_or_else(|| PathBuf::from(&socket), |cwd| cwd.join(&socket));
    let Some(workspace) = workspace else {
        let before = grants.len();
        grants.retain(|(socket, _)| *socket != path);
        session.grants.retain(|g| g.socket != path);
        return match grants.len() < before {
            true => (Reply::default(), true),
            false => failed(format!("no grant made {}", path.display())),
        };
    };
    let name = match session.grantable(&path, &workspace) {
        Ok(name) => name,
        Err(error) => return failed(error.to_string()),
    };
    let checked = crate::socket::checked(&path.to_string_lossy(), "grant");
    let listening = match checked
        .map_err(Error::Socket)
        .and_then(|s| Listening::open(&s))
    {
        Ok(listening) => listening,
        Err(error) => return failed(error.to_string()),
    };
    session.grant(path.clone(), name);
    grants.push((path.clone(), listening));
    (format!("{}\n", path.display()).into(), false)
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

impl std::error::Error for Error {}

/// Runs a server on `socket` until it is told to stop or its last pane
/// closes.
pub fn serve(socket: &SocketPath, fresh: bool) -> Result<(), Error> {
    let listening = Listening::open(socket)?;
    let children = crate::signal_pipe(&[SIGCHLD]).map_err(Error::Setup)?;
    // Orphans the panes' programs leave stay the server's to read and reap.
    if let Err(errno) = fuxix::process::adopt_orphans() {
        log!("adopting orphans: {errno}");
    }
    let stops = crate::signal_pipe(&[SIGTERM, SIGINT, SIGHUP]).map_err(Error::Setup)?;
    // Settings come from clients (`set`), each running its config.
    let mut session = Session::new(
        crate::config::Config::default(),
        socket.path().to_owned(),
        true,
    );
    let mut saved = Saved::of(socket);
    let restoring = match &mut saved {
        Some(saved) if fresh => {
            saved.forget_file();
            false
        }
        Some(saved) => {
            saved.restore = saved.restore(socket);
            saved.restore.is_some()
        }
        None => false,
    };
    if !restoring {
        session.start().map_err(Error::Start)?;
    }
    let mut server = Server {
        session,
        phase: Phase::Serving {
            listening,
            resting: None,
        },
        conns: Vec::new(),
        grants: Vec::new(),
        revoked: false,
        saved,
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
    /// The server's socket, or with an index into `grants` one `fux grant`
    /// made.
    Listener(Option<usize>),
    /// The door beside it: knocks waiting.
    Door(Option<usize>),
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
            // What the last round read or changed may be what a wait waits
            // for, or an event to tell.
            self.after_restore();
            self.answer_waits(now);
            self.end_revoked();
            self.tell_events();
            if let Some(saved) = &mut self.saved {
                saved.save_by(&self.session, now);
            }
            // Input given to panes since the last poll is written now rather
            // than when the next poll says their terminals can take it.
            self.session.panes.values_mut().for_each(Pane::write_input);
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
            let mut ready = std::mem::take(&mut self.ready);
            self.poll(timeout, &mut ready);
            for &(slot, flags) in &ready {
                match slot {
                    Slot::Listener(grant) => self.accept(grant),
                    Slot::Door(grant) => self.open_door(grant),
                    Slot::Children => {
                        crate::drain(&self.children);
                        self.reap();
                    }
                    Slot::Stops => {
                        crate::drain(&self.stops);
                        // A signal (a shutdown, a logout) keeps the layout
                        // for the next server to bring back.
                        self.stop("stopped by a signal".into(), true);
                    }
                    Slot::Conn(i) => self.serve_conn(i, flags),
                    Slot::Tty(i) => self.serve_tty(i, flags),
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
        let save = (self.saved.as_ref()).and_then(|s| s.pending.then_some((s.at, Wake::Save)));
        let paints = (self.conns.iter().enumerate())
            .filter_map(|(i, conn)| Some((conn.paint_due(&self.session)?, Wake::Paint(i))));
        let answers = self.conns.iter().filter_map(|conn| {
            let Stage::Waiting(waiting) = &conn.stage else {
                return None;
            };
            Some((waiting.due(), Wake::Answer))
        });
        session
            .chain(grace)
            .chain(phase)
            .chain(save)
            .chain(answers)
            .chain(paints)
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
                self.session.hold_copies();
                self.flush_outbox();
                self.paint(index, now);
            }
            // The loop answers waits and saves after every wake: these only
            // set when it wakes.
            Wake::Answer | Wake::Save => {}
        }
        ControlFlow::Continue(())
    }

    /// Once a restore is over: the requests held for it, answered; and if
    /// it brought nothing back, a first workspace, as a fresh server has.
    fn after_restore(&mut self) {
        let Some(saved) = &mut self.saved else {
            return;
        };
        if saved.restoring() || saved.restore.take().is_none() {
            return;
        }
        if self.session.workspaces.is_empty()
            && let Err(error) = self.session.start()
        {
            log!("starting afresh: {error}");
        }
        for index in 0..self.conns.len() {
            let Some(conn) = self.conns.get_mut(index) else {
                continue;
            };
            if let Stage::Held(_) = conn.stage
                && let Stage::Held(held) = std::mem::replace(&mut conn.stage, Stage::Command)
            {
                self.request(index, held.command, held.context);
            }
        }
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

    /// Ends what waits or streams through a grant that was revoked.
    fn end_revoked(&mut self) {
        if !std::mem::take(&mut self.revoked) {
            return;
        }
        let live: Vec<&PathBuf> = self.grants.iter().map(|(socket, _)| socket).collect();
        for conn in &mut self.conns {
            let gone = conn.grant.as_ref().is_some_and(|g| !live.contains(&g));
            if gone && matches!(conn.stage, Stage::Waiting(_) | Stage::Events(_)) {
                let why = "the grant this came through was revoked";
                conn.end(&mut self.session, &ServerFrame::Exit(why));
            }
        }
    }

    /// Tells each stream of events what changed since it last looked; one
    /// whose reader has fallen too far behind is told it lost events, and
    /// ends.
    fn tell_events(&mut self) {
        let streams = self
            .conns
            .iter()
            .any(|c| matches!(c.stage, Stage::Events(_)));
        if !streams {
            return;
        }
        let now = Snapshot::of(&self.session);
        for conn in &mut self.conns {
            let Stage::Events(subscription) = &mut conn.stage else {
                continue;
            };
            let (lines, over) = subscription.told(&now, &self.session);
            if over {
                ServerFrame::split_into(lines.as_bytes(), ServerFrame::Stdout, &mut conn.out);
                let gone = Reply {
                    end: End::Failed {
                        status: std::num::NonZeroU8::new(crate::wait::GONE)
                            .unwrap_or(std::num::NonZeroU8::MIN),
                        code: Code::Gone,
                        message: "everything it watched has closed".into(),
                    },
                    ..Reply::default()
                };
                conn.answer(&mut self.session, gone);
                continue;
            }
            if lines.is_empty() {
                continue;
            }
            if conn.out.len() > crate::events::BACKLOG {
                let lost = Event::stream("lost", Vec::new()).line(subscription.json);
                ServerFrame::split_into(lost.as_bytes(), ServerFrame::Stdout, &mut conn.out);
                let lost = Reply::failed(Code::Lost, "events: the reader fell too far behind");
                conn.answer(&mut self.session, lost);
                continue;
            }
            ServerFrame::split_into(lines.as_bytes(), ServerFrame::Stdout, &mut conn.out);
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
        // Built afresh, as it borrows the descriptors.
        let mut fds = Vec::new();
        if let Phase::Serving {
            listening,
            resting: None,
        } = &self.phase
        {
            let grants = (self.grants.iter().enumerate()).map(|(i, (_, l))| (Some(i), l));
            for (grant, listening) in std::iter::once((None, listening)).chain(grants) {
                if let Some(listener) = &listening.listener {
                    fds.push(PollFd::new(listener, PollFlags::IN));
                    slots.push(Slot::Listener(grant));
                }
                if let Some(door) = &listening.door {
                    fds.push(PollFd::new(door.fd(), PollFlags::IN));
                    slots.push(Slot::Door(grant));
                }
            }
        }
        fds.push(PollFd::new(&self.children, PollFlags::IN));
        slots.push(Slot::Children);
        fds.push(PollFd::new(&self.stops, PollFlags::IN));
        slots.push(Slot::Stops);
        for (i, conn) in self.conns.iter().enumerate() {
            // A connection that is not read is open only while it has
            // something to write.
            let mut flags = if conn.stage.reads() {
                PollFlags::IN
            } else {
                PollFlags::OUT
            };
            if !conn.out.is_empty() {
                flags |= PollFlags::OUT;
            }
            fds.push(PollFd::new(&conn.stream, flags));
            slots.push(Slot::Conn(i));
            // Polled for its hangup, and for room to write what waits.
            if let Stage::Attached(attached) = &conn.stage
                && let Some(tty) = &attached.tty
            {
                let flags = match tty.out.is_empty() {
                    true => PollFlags::HUP,
                    false => PollFlags::OUT,
                };
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

    /// Stops the server, saying why; with `keep`, its layout is saved for
    /// the next server to bring back, else forgotten.
    fn stop(&mut self, reason: String, keep: bool) {
        if let Phase::Stopping(_) = self.phase {
            return;
        }
        log!("{reason}");
        if let Some(saved) = &mut self.saved {
            saved.stop(&self.session, keep);
        }
        // The sockets go first: from now on a client finds no server.
        self.phase = Phase::Stopping(crate::after(Instant::now(), STOP_WAIT));
        self.grants.clear();
        self.session.grants.clear();
        self.session.shutdown();
        // Every connection still open ends, so no command runs on what is
        // left of the session.
        let exit = format!("the fux server stopped: {reason}");
        for conn in &mut self.conns {
            // A stream says why it ends, as one of its lines, and ends as a
            // command that is done.
            if let Stage::Events(subscription) = &conn.stage {
                let reason = Json::str(reason.clone());
                let stopped = Event::stream("stopped", vec![("reason", reason)]);
                let line = stopped.line(subscription.json);
                ServerFrame::split_into(line.as_bytes(), ServerFrame::Stdout, &mut conn.out);
                conn.answer(&mut self.session, Reply::default());
            }
            if conn.stage.reads() {
                conn.end(&mut self.session, &ServerFrame::Exit(&exit));
            }
        }
    }

    fn flush_outbox(&mut self) {
        for outgoing in std::mem::take(&mut self.session.outbox) {
            let client = match outgoing {
                Outgoing::Shutdown { reason, keep } => {
                    self.stop(reason, keep);
                    continue;
                }
                Outgoing::Bytes(client, _)
                | Outgoing::Exit(client, _)
                | Outgoing::Run(client, _)
                | Outgoing::Keys(client, _)
                | Outgoing::Ended(client) => client,
            };
            let Some(conn) = self.conns.iter_mut().find(|c| c.serves(client)) else {
                continue;
            };
            let session = &mut self.session;
            match outgoing {
                Outgoing::Bytes(_, bytes) => {
                    if let Stage::Attached(attached) = &mut conn.stage {
                        attached.paint(&mut conn.out, &bytes);
                    }
                }
                Outgoing::Exit(_, reason) => conn.end(session, &ServerFrame::Exit(&reason)),
                Outgoing::Run(_, code) => conn.send(session, &ServerFrame::Run(&code)),
                Outgoing::Keys(_, keys) => conn.send(session, &ServerFrame::Keys(keys)),
                Outgoing::Ended(_) => conn.send(session, &ServerFrame::Ended),
                // Stopped above.
                Outgoing::Shutdown { .. } => {}
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
        // A picture gone or moved has its cells painted again, blank, which
        // takes its pixels away; one whose cells this paint paints over, or
        // new, is sent after the text.
        let pictures = render::pictures(session, client, &attached.placement);
        let known = attached.known;
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
        let shown = known.then_some(&attached.shown);
        // The same screen as the client shows: nothing to send, not even
        // the envelope, whose cursor hide and show would restart a blinking
        // cursor.
        if shown.is_none_or(|shown| !attached.spare.same_as(shown)) {
            self.paint_buffer.clear();
            render::paint_into(shown, &attached.spare, &mut self.paint_buffer);
            attached.paint(&mut conn.out, &self.paint_buffer);
            std::mem::swap(&mut attached.shown, &mut attached.spare);
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
            render::picture_into(picture, &mut drawn);
        }
        if !drawn.is_empty() {
            attached.paint(&mut conn.out, &drawn);
        }
        attached.pictures = pictures;
        if let Some(view) = session.views.get_mut(&client) {
            view.dirty = false;
        }
    }

    /// What the server listens on while it serves, or with `grant` what
    /// that grant does, and the grant's socket, which its connections
    /// reach through.
    fn listening(&mut self, grant: Option<usize>) -> Option<(&mut Listening, Option<PathBuf>)> {
        let Phase::Serving { listening, .. } = &mut self.phase else {
            return None;
        };
        match grant {
            Some(i) => (self.grants.get_mut(i)).map(|(socket, l)| (l, Some(socket.clone()))),
            None => Some((listening, None)),
        }
    }

    /// Accepts the connections waiting on the server's socket, or with
    /// `grant` on that socket `fux grant` made.
    fn accept(&mut self, grant: Option<usize>) {
        // Accept until the backlog is empty, so one tick drains it.
        loop {
            // The spare makes room for this accept, and is taken back after
            // it: if it cannot be, the connection took the last descriptor.
            self.spare = None;
            let Some((
                Listening {
                    listener: Some(listener),
                    ..
                },
                through,
            )) = self.listening(grant)
            else {
                return;
            };
            let accepted = listener.accept();
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
                        log!(
                            "file descriptors available again; {} connections were refused while they were short",
                            shortage.refused
                        );
                    }
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
                            log!(
                                "refused a connection whose peer could not be identified: {error}"
                            );
                            continue;
                        }
                    }
                    if stream.set_nonblocking(true).is_err() {
                        continue;
                    }
                    self.conns.push(Conn::new(stream, through));
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
                        log!(
                            "accept: {e}; listening again in {} ms",
                            ACCEPT_REST.as_millis()
                        );
                    }
                    return;
                }
            }
        }
    }

    /// Takes the connections knocks at the door bring: each is the server's
    /// end of a pair relayed to a client's FIFOs, whose owner the door
    /// checked, as the socket's peer check does. With `grant`, the door
    /// beside that grant's socket.
    fn open_door(&mut self, grant: Option<usize>) {
        let Some((
            Listening {
                door: Some(door), ..
            },
            through,
        )) = self.listening(grant)
        else {
            return;
        };
        for stream in door.knocks() {
            if stream.set_nonblocking(true).is_ok() {
                self.conns.push(Conn::new(stream, through.clone()));
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
            log!("out of file descriptors: new connections are refused, each told why, until some close");
            Shortage {
                reported: (now, 0),
                refused: 0,
            }
        });
        shortage.refused = shortage.refused.saturating_add(1);
        let (at, then) = shortage.reported;
        if now.duration_since(at) >= REPORT_EVERY {
            log!(
                "still out of file descriptors: {} connections refused in the last {} s",
                shortage.refused.saturating_sub(then),
                now.duration_since(at).as_secs()
            );
            shortage.reported = (now, shortage.refused);
        }
    }

    /// The terminal a client sent: room to write what waits for it. One
    /// that closed detaches its client, as a client whose terminal closes
    /// detaches itself.
    fn serve_tty(&mut self, index: usize, flags: PollFlags) {
        let session = &mut self.session;
        let Some(conn) = self.conns.get_mut(index) else {
            return;
        };
        if flags.contains(PollFlags::OUT) {
            conn.flush_tty(session);
        }
        if flags.intersects(PollFlags::HUP | PollFlags::ERR) && conn.attached().is_some() {
            conn.end(session, &ServerFrame::Exit("detached: the terminal closed"));
        }
    }

    /// A client's connection is ready, as found at `now`.
    fn serve_conn(&mut self, index: usize, flags: PollFlags) {
        let Some(conn) = self.conns.get_mut(index) else {
            return;
        };
        let gone = flags.intersects(PollFlags::HUP | PollFlags::ERR);
        // One that is not read learns that its peer is gone by writing.
        if flags.contains(PollFlags::OUT) || (gone && !conn.stage.reads()) {
            conn.flush(&mut self.session);
        }
        if flags.contains(PollFlags::IN) || gone {
            self.read_conn(index);
        }
    }

    /// Reads what a client sent until it has sent no more, or `CONN_READ`
    /// bytes, or more than 256 whole frames, or a bad one; each whole frame
    /// is handled as it arrives, in order, and nothing after a bad one.
    fn read_conn(&mut self, index: usize) {
        let Some(conn) = self.conns.get_mut(index) else {
            return;
        };
        // Held apart while the frames it lends are handled.
        let mut decoder = std::mem::take(&mut conn.decoder);
        let (mut frames, mut read) = (0usize, 0usize);
        let closed = 'read: loop {
            // A frame it sent may have ended it: it is read no more.
            let Some(conn) = self.conns.get_mut(index).filter(|c| c.stage.reads()) else {
                break false;
            };
            // A client may send its terminal with its `Attach`.
            match fuxix::socket::recv_with_fd(&conn.stream, &mut self.read_buffer) {
                Ok((0, _)) => break true,
                Ok((n, fd)) => {
                    if let (
                        Some(fd),
                        Stage::Hello(passed) | Stage::Attaching(passed, ClientKind::Terminal),
                    ) = (fd, &mut conn.stage)
                    {
                        *passed = Some(fd);
                    }
                    read = read.saturating_add(n);
                    decoder.push(self.read_buffer.get(..n).unwrap_or_default());
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
            Stage::Hello(passed) => {
                let Some(hello) = decoder.frame::<Hello>()? else {
                    return Ok(false);
                };
                let passed = passed.take();
                let ours = Hello::ours(hello.role);
                // Refused before the answer, which a client reads as why.
                if conn.grant.is_some() && hello.role != Role::Command {
                    let refused = "a socket `fux grant` made takes commands only: no client \
                                   attaches through it, and it stops no server";
                    conn.end(session, &ServerFrame::Exit(refused));
                    return Ok(true);
                }
                let between = Mismatch::between(&hello, &ours);
                conn.send(session, &ServerFrame::Hello(ours));
                match (between, hello.role) {
                    (Err(mismatch), _) => {
                        conn.end(session, &ServerFrame::Exit(&mismatch.to_string()));
                    }
                    (Ok(()), Role::Kill) => {
                        conn.end(session, &ServerFrame::Done { status: 0 });
                        self.stop("stopped by fux kill-server".into(), false);
                    }
                    (Ok(()), Role::Attach) => {
                        conn.enter(session, Stage::Attaching(passed, ClientKind::Terminal));
                    }
                    // A headless client has no terminal to pass.
                    (Ok(()), Role::Headless) => {
                        conn.enter(session, Stage::Attaching(None, ClientKind::Headless));
                    }
                    (Ok(()), Role::Command) => conn.enter(session, Stage::Command),
                }
            }
            Stage::Attaching(passed, kind) => {
                let Some(Attach {
                    rows,
                    cols,
                    workspace,
                    read_only,
                    env,
                }) = decoder.frame()?
                else {
                    return Ok(false);
                };
                let (passed, kind) = (passed.take(), *kind);
                match session.attach_as(kind, rows.get(), cols.get(), workspace.as_deref()) {
                    Ok(client) => {
                        if let Some(view) = session.views.get_mut(&client) {
                            view.read_only = read_only;
                            view.env = env;
                        }
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
                        // Told its id: its bindings' `me`, a headless
                        // client's to print.
                        let id = format!("{client}\n");
                        conn.send(session, &ServerFrame::Stdout(id.as_bytes()));
                    }
                    Err(error) => conn.end(session, &ServerFrame::Exit(&error.to_string())),
                }
            }
            Stage::Attached(attached) => match decoder.frame()? {
                Some(AttachedFrame::Act(act)) => attached.act(session, *act),
                // The client's terminal was resized, which may have moved
                // what it shows even if the size it ends at is the same:
                // forgotten, it is repainted in full either way.
                Some(AttachedFrame::Resize { rows, cols }) => {
                    attached.known = false;
                    session.resize(attached.client, rows.get(), cols.get());
                }
                Some(AttachedFrame::Detach) => conn.end(session, &ServerFrame::Exit("detached")),
                None => return Ok(false),
            },
            Stage::Command => {
                let Some(Request { command, context }) = decoder.frame()? else {
                    return Ok(false);
                };
                self.request(index, command, context);
            }
            // Nothing more is asked of a waiting command, or a stream. A
            // connection that is ending has had its last word: what came
            // with the Detach, Command or refused Hello that ended it is
            // dropped unread.
            Stage::Held(_)
            | Stage::Waiting(_)
            | Stage::Events(_)
            | Stage::Closing
            | Stage::Dead => {
                *decoder = Decoder::default();
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// A request from connection `index`: answered now, or once its wait
    /// has its answer, or streamed.
    fn request(
        &mut self,
        index: usize,
        command: crate::command::Command,
        context: crate::protocol::Context,
    ) {
        let restore_runs = self.saved.as_ref().is_some_and(Saved::restoring);
        let session = &mut self.session;
        let Some(conn) = self.conns.get_mut(index) else {
            return;
        };
        // A restore is the server's own, which no grant's socket makes.
        let restoring = context.restoring && restore_runs && conn.grant.is_none();
        // While a saved session comes back, other requests wait for it,
        // so that none takes an id it asks for.
        if restore_runs && !restoring {
            conn.stage = Stage::Held(Box::new(Request { command, context }));
            return;
        }
        let crate::protocol::Context {
            cwd,
            pane,
            server,
            env,
            client,
            ..
        } = context;
        // A pane of another server's (FUX_PANE kept as FUX_SOCKET chose
        // this one) is not the caller's here.
        let ours = server.as_deref().is_none_or(|s| s == session.server_id);
        let inside = server.as_deref() == Some(session.server_id.as_str());
        let (pane, stray) = if ours { (pane, None) } else { (None, pane) };
        let cwd = Some(PathBuf::from(cwd)).filter(|p| p.is_dir());
        use crate::command::Command;
        let granting = if let Command::Grant { workspace, socket } = &command {
            Some((socket.clone(), Some(workspace.clone())))
        } else if let Command::Revoke { socket } = &command {
            Some((socket.clone(), None))
        } else {
            None
        };
        if let Some(granting) = granting {
            let outcome = match conn.grant {
                // `fux grant` from a granted socket: refused.
                Some(_) => Reply::failed(
                    Code::NotGranted,
                    "a socket `grant` made cannot grant another",
                ),
                None => {
                    let (outcome, revoked) = grant(&mut self.grants, session, granting, cwd);
                    // What came through a revoked grant ends.
                    self.revoked |= revoked;
                    outcome
                }
            };
            conn.answer(session, outcome);
            return;
        }
        // A program run as a client (`str c2 client focused`) names one
        // that must be there.
        // A key's or the prompt's program acts where its client is; one
        // from a grant never does.
        let origin = match client.filter(|_| conn.grant.is_none()) {
            Some(c) if !session.views.contains_key(&c) => {
                let outcome = Reply::failed(Code::NoClient, format!("no client {c}"));
                conn.answer(session, outcome);
                return;
            }
            Some(client) => Origin::Client(client),
            None => Origin::Cli(Caller {
                pane: pane.and_then(|p| crate::command::parse_pane(&p).ok()),
                cwd,
                stray,
                env,
                inside,
                grant: conn.grant.clone(),
                restoring,
            }),
        };
        match session.begin(command, &origin) {
            Begun::Done(outcome) => conn.answer(session, outcome),
            Begun::Waiting(waiting) => conn.enter(session, Stage::Waiting(waiting)),
            Begun::Events(subscription) => {
                // Told it is armed: from here on, nothing is missed.
                let ready = Event::stream("ready", Vec::new()).line(subscription.json);
                conn.send(session, &ServerFrame::Stdout(ready.as_bytes()));
                conn.enter(session, Stage::Events(subscription));
            }
        }
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
        let ended = loop {
            if total >= PANE_READ {
                break false;
            }
            let Some(Process::Reading(child)) = self.session.panes.get(&id).map(|p| &p.process)
            else {
                return;
            };
            match fuxix::io::read(&child.master, &mut buffer) {
                Err(fuxix::Errno::AGAIN) => break false,
                Err(fuxix::Errno::INTR) => continue,
                // EIO: the slave side is closed; the program is gone.
                Ok(0) | Err(_) => break true,
                Ok(n) => {
                    // Past PANE_READ by at most one buffer, when the loop ends.
                    total = total.saturating_add(n);
                    self.session.output(id, buffer.get(..n).unwrap_or_default());
                    // A little, as an echo or a prompt is: that was all,
                    // and the poll says when there is more, without a read
                    // to find none. A flood's reads are larger, and go on.
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
            if let Some(pane) = self.session.panes.get_mut(&id) {
                pane.process.stop_reading();
            }
        }
    }

    /// Checks every killed program and every pane's, since signals
    /// coalesce, and reaps the orphans the server adopted, every child
    /// that is no pane's leader.
    fn reap(&mut self) {
        self.reap_killed();
        let session = &self.session;
        let panes = session.panes.values().filter_map(|p| p.process.child());
        let leaders: Vec<Pid> = (panes.chain(session.dying.values().map(|d| &d.child)))
            .map(|child| child.leader.pid())
            .chain(self.killed.iter().map(Leader::pid))
            .collect();
        fuxix::process::reap_orphans(|pid| leaders.contains(&pid));
        let exited: Vec<(PaneId, crate::process::Ended)> = self
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
        let mut conn = Conn::new(stream, None);
        let tty = TakenTerminal::new(OwnedFd::from(tty));
        conn.stage = Stage::Attached(Box::new(Attached::new(client, Some(tty))));
        Ok((session, conn, peer, far))
    }

    /// Something not a door where the door goes (a file left there): the
    /// server serves on its socket all the same, with no door.
    #[test]
    fn a_door_that_cannot_open_leaves_the_socket_serving() -> Result<(), String> {
        use std::os::unix::fs::PermissionsExt;
        let e = |e: &dyn std::fmt::Display| e.to_string();
        let dir = (fuxix::file::scratch().map_err(|x| e(&x))?)
            .join(format!("fux-listening-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("fux")).map_err(|x| e(&x))?;
        for private in [dir.clone(), dir.join("fux")] {
            std::fs::set_permissions(private, std::fs::Permissions::from_mode(0o700))
                .map_err(|x| e(&x))?;
        }
        let path = dir.join("fux").join("s.sock");
        std::fs::write(crate::door::path(&path), "").map_err(|x| e(&x))?;
        let socket = crate::socket::checked(&path.to_string_lossy(), "test").map_err(|x| e(&x))?;
        let listening = Listening::open(&socket).map_err(|x| e(&x))?;
        assert!(listening.listener.is_some(), "the socket serves");
        assert!(listening.door.is_none(), "with no door");
        drop(listening);
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
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
    /// same size, is due a full paint though nothing it views changed; but
    /// not while its last frame is still being written.
    #[test]
    fn a_forgotten_screen_is_due_a_paint_once_the_last_is_sent() -> Result<(), String> {
        let (mut session, mut conn, _peer, _far) = with_terminal()?;
        session.views.values_mut().for_each(|v| v.dirty = false);
        if let Some(attached) = conn.attached() {
            attached.known = true;
        }
        assert_eq!(conn.paint_due(&session), None, "nothing to paint");
        if let Some(attached) = conn.attached() {
            attached.known = false;
        }
        assert!(conn.paint_due(&session).is_some());
        if let Some(tty) = conn.attached().and_then(|a| a.tty.as_mut()) {
            tty.out.push(b"the last frame");
        }
        assert_eq!(conn.paint_due(&session), None, "one frame in flight");
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
