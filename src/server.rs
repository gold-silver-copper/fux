//! The server: one thread (but for fuxix's watchdog while a PTY opens on
//! macOS), one `poll` loop over the listening socket, a signal pipe, every
//! client and every pane's PTY.
use crate::after;
use crate::bytes::ByteQueue;
use crate::code::Code;
use crate::events::{BACKLOG, Ending, Snapshot, Subscription};
use crate::id::{ClientId, PaneId};
use crate::layout::Placement;
use crate::pane::{Pane, Process};
use crate::process::Pid;
use crate::protocol::{
    Asked, Attach, AttachedFrame, Context, Decoder, Frame, Greeting, Hello, Mismatch, Reply, Role,
    ToAttached, ToCommand, split_into,
};
use crate::render::{self, Grid};
use crate::session::{Begun, Caller, Came, Dying, Origin, Outgoing, Session, Timer, Within};
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
use std::os::fd::OwnedFd;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Component, PathBuf};
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
        let (endpoint, bound) = crate::socket::bind_socket(socket).map_err(Error::Socket)?;
        let door = match crate::door::Door::open(socket.path()) {
            Ok(door) => Some(door),
            Err(error) if bound.is_none() => return Err(Error::Setup(error)),
            Err(error) => {
                log!("no door beside the socket: {error}");
                None
            }
        };
        // Listened on only once the door is there: a client that can
        // connect finds the door too, as one in a sandbox looks for it as
        // soon as `fux start` says the server answers.
        let listener = bound
            .map(crate::socket::Bound::listen)
            .transpose()
            .map_err(Error::Socket)?;
        if let Some(listener) = &listener {
            listener.set_nonblocking(true).map_err(Error::Setup)?;
        }
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
        /// The server's own socket first, then those `fux grant` made, each
        /// with its path, which its connections reach through.
        listening: Vec<(Option<PathBuf>, Listening)>,
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
    /// their answers (`Waiting::due`) and saves the layout if it changed.
    Look,
}

/// What a connection comes over: a socket, or a door's two FIFOs, which
/// carry no descriptor (`crate::door`).
enum Wire {
    Socket(UnixStream),
    Door(crate::door::Fifos),
}

/// A client's connection: its wire, and where it is in its life.
struct Conn {
    wire: Wire,
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
    /// answered, and the answer written, before the next is read
    /// (`Conn::reads`): a client that never reads its answers is read no
    /// more, and holds no more memory than one answer.
    Command,
    /// Its request waits for the restore that runs to end, so that it takes
    /// no id the restore asks for (`Server::request`). It is read only to
    /// notice the client go.
    Held(Box<Asked>),
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

    /// Whether a request of its is under way: held, waiting or streaming.
    fn asked(&self) -> bool {
        match self {
            Stage::Held(_) | Stage::Waiting(_) | Stage::Events(_) => true,
            Stage::Hello(_)
            | Stage::Attaching(..)
            | Stage::Attached(_)
            | Stage::Command
            | Stage::Closing
            | Stage::Dead => false,
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
            split_into(self.out.as_slice(), ToAttached::Paint, out);
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
    /// it outside one: written there if the server took it, else framed in
    /// `out` for the client.
    fn paint(&mut self, out: &mut ByteQueue, bytes: &[u8]) {
        match &mut self.tty {
            Some(tty) => tty.out.push(bytes),
            None => split_into(bytes, ToAttached::Paint, out),
        }
    }
}

impl Conn {
    fn new(wire: Wire, grant: Option<PathBuf>) -> Conn {
        Conn {
            wire,
            decoder: Decoder::default(),
            out: ByteQueue::default(),
            stage: Stage::Hello(None),
            grant,
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
    /// client detached and its terminal given back, with its own cursor
    /// shape; one that is dead stays so.
    fn enter(&mut self, session: &mut Session, next: Stage) {
        if matches!(self.stage, Stage::Dead) {
            return;
        }
        if let Stage::Attached(mut attached) = mem::replace(&mut self.stage, next) {
            session.detach(attached.client);
            // A cursor shape a pane set is given back as the terminal's own
            // (`render::cursor_shape`); one never set is left as it was.
            let mut shape = Vec::new();
            let initial = fux_vt::CursorShape::Initial;
            render::cursor_shape(attached.shown.modes.cursor_shape, initial, &mut shape);
            if !shape.is_empty() {
                attached.paint(&mut self.out, &shape);
            }
            if let Some(tty) = attached.tty {
                tty.give_back(&mut self.out);
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

    /// Sends the client attached here `frame`: bytes for its terminal
    /// written there if the server took it, else framed; an `Exit` as its
    /// last frame.
    fn tell(&mut self, session: &mut Session, frame: &ToAttached) {
        match frame {
            ToAttached::Paint(bytes) => {
                if let Stage::Attached(attached) = &mut self.stage {
                    attached.paint(&mut self.out, bytes);
                }
            }
            ToAttached::Exit(_) => self.end(session, frame),
            ToAttached::Welcome(..)
            | ToAttached::Ended(_)
            | ToAttached::Run(_)
            | ToAttached::Keys(_) => self.send(session, frame),
        }
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
            self.end(session, &ToAttached::Exit("detached: the terminal closed"));
        }
    }

    /// Writes what waits for the client until it is all written or the
    /// socket takes no more; a connection that fails is dead. What waits
    /// for its terminal is written too.
    fn flush(&mut self, session: &mut Session) {
        self.flush_tty(session);
        let written = match &self.wire {
            Wire::Socket(stream) => write_out(stream, &mut self.out),
            Wire::Door(fifos) => write_out(&fifos.down, &mut self.out),
        };
        if !written {
            self.enter(session, Stage::Dead);
        }
    }
}

pub struct Server {
    session: Session,
    phase: Phase,
    conns: Vec<Conn>,
    /// Where the layout is saved, for a server after a crash to bring
    /// back; none if there is nowhere to.
    saved: Option<Saved>,
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
    /// Something changed since it was last saved.
    pending: bool,
    /// The workspaces, tabs and panes it was last saved with.
    shape: Shape,
    /// The restore under way, if one is: the other end of a socket its
    /// thread closes as it ends (`Server::restored`). Meanwhile what there
    /// is is not saved over the file.
    restore: Option<UnixStream>,
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
        // Each part of the path, `%` in it written `%25`, the parts joined
        // by `%`: no two sockets share one, and one path spelt two ways
        // (`/run//x.sock`, `/run/./x.sock`) has one.
        let parts: Vec<String> = (socket.path().components())
            .filter_map(|part| match part {
                Component::Normal(_) | Component::ParentDir => {
                    Some(part.as_os_str().to_string_lossy().replace('%', "%25"))
                }
                Component::RootDir | Component::CurDir | Component::Prefix(_) => None,
            })
            .collect();
        let name = format!("{}.layout", parts.join("%"));
        Some(Saved {
            path: base.join("fux").join(name),
            written: String::new(),
            at: Instant::now(),
            pending: false,
            shape: Shape::default(),
            restore: None,
        })
    }

    /// Brings back the session saved, if there is one, by running it, a
    /// fuxth program, as a client of the server on `socket` does: on a
    /// thread of its own, as the server serves it. The socket that thread
    /// closes as it ends, if it started.
    fn restore(&self, socket: &SocketPath) -> Option<UnixStream> {
        let text = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(e) if e.kind() == ErrorKind::NotFound => return None,
            Err(e) => {
                log!("{}: {e}; starting afresh", self.path.display());
                return None;
            }
        };
        let (path, socket) = (self.path.clone(), socket.clone());
        let (ends, end) = UnixStream::pair()
            .inspect_err(|error| log!("could not bring back {}: {error}", self.path.display()))
            .ok()?;
        let spawned = std::thread::Builder::new()
            .name("restore".into())
            .spawn(move || {
                let _ends = ends;
                match crate::script::restore(&socket, &path, &text) {
                    Ok(()) => log!("brought back the session in {}", path.display()),
                    Err(error) => {
                        // Kept aside, not saved over, for a person to look at.
                        let aside = path.with_extension("layout.failed");
                        let _ = fs::rename(&path, &aside);
                        log!(
                            "could not bring back {}: {error}; the file kept as {}",
                            path.display(),
                            aside.display()
                        );
                    }
                }
            });
        match spawned {
            Ok(_) => Some(end),
            Err(error) => {
                log!("could not bring back {}: {error}", self.path.display());
                None
            }
        }
    }

    /// Saves the layout if it may have changed and `SAVE_EVERY` has passed
    /// since it was last saved; else it is saved then. A workspace, tab or
    /// pane made or closed is saved at once: a crash must not lose a pane.
    fn save_by(&mut self, session: &mut Session, now: Instant) {
        self.pending |= mem::take(&mut session.edited);
        if !self.pending {
            return;
        }
        let shape = Shape::of(session);
        if now < self.at && shape == self.shape {
            return;
        }
        self.save(session);
        self.shape = shape;
        self.at = after(now, SAVE_EVERY);
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
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)?;
        }
        let temporary = self.path.with_extension("layout.new");
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        fs::rename(&temporary, &self.path)
    }

    /// Whether a restore is under way.
    fn restoring(&self) -> bool {
        self.restore.is_some()
    }
}

/// `fux grant`: a socket made at `socket` (from the caller's directory
/// `cwd`), whose commands reach `workspace` alone; or `fux revoke`, the one
/// made there closed (and what came through it ends): the socket of the
/// grant it revoked.
fn grant(
    listening: &mut Vec<(Option<PathBuf>, Listening)>,
    session: &mut Session,
    (socket, workspace): (String, Option<crate::command::WsRef>),
    cwd: Option<PathBuf>,
) -> (Reply, Option<PathBuf>) {
    let failed = |stderr: String| (Reply::failed(Code::Grant, stderr), None);
    let path = cwd.map_or_else(|| PathBuf::from(&socket), |cwd| cwd.join(&socket));
    let Some(workspace) = workspace else {
        let before = listening.len();
        listening.retain(|(socket, _)| socket.as_ref() != Some(&path));
        session.grants.retain(|g| g.socket != path);
        return match listening.len() < before {
            true => (Reply::default(), Some(path)),
            false => failed(format!("no grant made {}", path.display())),
        };
    };
    let name = match session.grantable(&path, &workspace) {
        Ok(name) => name,
        Err(error) => return failed(error.to_string()),
    };
    let checked = crate::socket::checked(&path, "grant");
    let opened = match checked
        .map_err(Error::Socket)
        .and_then(|s| Listening::open(&s))
    {
        Ok(opened) => opened,
        Err(error) => return failed(error.to_string()),
    };
    session.grant(path.clone(), name);
    listening.push((Some(path.clone()), opened));
    (format!("{}\n", path.display()).into(), None)
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
    let children = crate::SignalPipe::new(&[SIGCHLD]).map_err(Error::Setup)?;
    // Orphans the panes' programs leave stay the server's to read and reap.
    if let Err(errno) = fuxix::process::adopt_orphans() {
        log!("adopting orphans: {errno}");
    }
    let stops = crate::SignalPipe::new(&crate::STOPS).map_err(Error::Setup)?;
    // Settings come from clients (`set`), each running its config.
    let mut session = Session::new(
        crate::config::Config::default(),
        socket.path().to_owned(),
        true,
    );
    let mut saved = Saved::of(socket);
    let restoring = match &mut saved {
        Some(saved) if fresh => {
            let _ = fs::remove_file(&saved.path);
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
            listening: vec![(None, listening)],
            resting: None,
        },
        conns: Vec::new(),
        saved,
        children,
        killed: Vec::new(),
        stops,
        read_buffer: vec![0u8; 64 * 1024],
        paint_buffer: Vec::new(),
        slots: Vec::new(),
        ready: Vec::new(),
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
    /// A socket listened on, by its place among them (`Phase::Serving`).
    Listener(usize),
    /// The door beside it: knocks waiting.
    Door(usize),
    Children,
    Stops,
    /// The restore under way has ended.
    Restore,
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
            self.answer_waits(now);
            self.tell_events();
            if let Some(saved) = &mut self.saved {
                saved.save_by(&mut self.session, now);
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
            let mut ready = mem::take(&mut self.ready);
            self.poll(timeout, &mut ready);
            for &(slot, flags) in &ready {
                match slot {
                    Slot::Listener(grant) => self.accept(grant),
                    Slot::Door(grant) => self.open_door(grant),
                    Slot::Children => {
                        crate::drain(self.children.pipe());
                        self.reap();
                    }
                    Slot::Restore => self.restored(),
                    Slot::Stops => {
                        crate::drain(self.stops.pipe());
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
        let grace =
            (self.session.dying.iter().map(|d| d.deadline).min()).map(|at| (at, Wake::Grace));
        let phase = match self.phase {
            Phase::Serving { resting, .. } => resting.map(|at| (at, Wake::Listen)),
            Phase::Stopping(by) => Some((by, Wake::Stop)),
        };
        let save = (self.saved.as_ref()).and_then(|s| s.pending.then_some((s.at, Wake::Look)));
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
            .chain(save)
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
            // The loop answers waits and saves after every wake: these only
            // set when it wakes.
            Wake::Look => {}
        }
        ControlFlow::Continue(())
    }

    /// A restore is over: the requests held for it are answered; and if it
    /// brought nothing back, there is a first workspace, as a fresh server
    /// has.
    fn restored(&mut self) {
        if let Some(saved) = &mut self.saved {
            saved.restore = None;
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
                && let Stage::Held(held) = mem::replace(&mut conn.stage, Stage::Command)
            {
                self.request(index, held.request, held.context);
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
            let (lines, gone) = subscription.told(&now, &self.session);
            let json = subscription.json;
            // Behind with lines to tell: they are lost, and so is the stream.
            let lost = gone.is_none() && !lines.is_empty() && conn.out.len() > BACKLOG;
            if !lost {
                split_into(lines.as_bytes(), ToCommand::Stdout, &mut conn.out);
            }
            if let Some(ending) = gone.or(lost.then_some(Ending::Lost)) {
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
            for (grant, (_, listening)) in listening.iter().enumerate() {
                if let Some(listener) = &listening.listener {
                    add(PollFd::new(listener, PollFlags::IN), Slot::Listener(grant));
                }
                if let Some(door) = &listening.door {
                    add(PollFd::new(door.fd(), PollFlags::IN), Slot::Door(grant));
                }
            }
        }
        add(
            PollFd::new(self.children.pipe(), PollFlags::IN),
            Slot::Children,
        );
        add(PollFd::new(self.stops.pipe(), PollFlags::IN), Slot::Stops);
        if let Some(end) = self.saved.as_ref().and_then(|s| s.restore.as_ref()) {
            add(PollFd::new(end, PollFlags::IN), Slot::Restore);
        }
        for (i, conn) in self.conns.iter().enumerate() {
            // A connection that is not read is open only while it has
            // something to write.
            let mut flags = if conn.reads() {
                PollFlags::IN
            } else {
                PollFlags::OUT
            };
            if !conn.out.is_empty() {
                flags |= PollFlags::OUT;
            }
            match &conn.wire {
                Wire::Socket(stream) => add(PollFd::new(stream, flags), Slot::Conn(i)),
                // Each FIFO for its own way, and for its end.
                Wire::Door(fifos) => {
                    if flags.intersects(PollFlags::IN) {
                        add(PollFd::new(&fifos.up, PollFlags::IN), Slot::Conn(i));
                    }
                    if flags.intersects(PollFlags::OUT) {
                        add(PollFd::new(&fifos.down, PollFlags::OUT), Slot::Conn(i));
                    }
                }
            }
            // Polled for its hangup, and for room to write what waits.
            if let Stage::Attached(attached) = &conn.stage
                && let Some(tty) = &attached.tty
            {
                let flags = match tty.out.is_empty() {
                    true => PollFlags::HUP,
                    false => PollFlags::OUT,
                };
                add(PollFd::new(&tty.fd, flags), Slot::Tty(i));
            }
        }
        for (id, pane) in &self.session.panes {
            if let Process::Reading(child) = &pane.process {
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

    /// Stops the server, saying why; with `keep`, its layout is saved for
    /// the next server to bring back, else forgotten.
    fn stop(&mut self, reason: String, keep: bool) {
        if let Phase::Stopping(_) = self.phase {
            return;
        }
        log!("{reason}");
        // Nothing more is saved (or what was, forgotten). The last save:
        // with `keep`, the layout as it is, before its panes end, is what
        // the next server brings back; else none is (the server was stopped
        // on purpose, or its last pane closed).
        if let Some(mut saved) = self.saved.take() {
            match keep {
                true => saved.save(&self.session),
                false => {
                    let _ = fs::remove_file(&saved.path);
                }
            }
        }
        // The sockets go first: from now on a client finds no server.
        self.phase = Phase::Stopping(after(Instant::now(), STOP_WAIT));
        self.session.grants.clear();
        self.session.shutdown();
        // Every connection still open ends, so no command runs on what is
        // left of the session.
        let exit = format!("the fux server stopped: {reason}");
        for conn in &mut self.conns {
            // A stream says why it ends, as one of its lines, and ends as a
            // command that is done.
            if let Stage::Events(subscription) = &conn.stage {
                let json = subscription.json;
                conn.ends(&mut self.session, json, &Ending::Stopped(reason.clone()));
            }
            let session = &mut self.session;
            match conn.stage {
                Stage::Hello(_) => conn.end(session, &Greeting::Exit(&exit)),
                Stage::Attaching(..) | Stage::Attached(_) => {
                    conn.end(session, &ToAttached::Exit(&exit));
                }
                Stage::Command | Stage::Held(_) | Stage::Waiting(_) | Stage::Events(_) => {
                    conn.end(session, &ToCommand::Exit(&exit));
                }
                Stage::Closing | Stage::Dead => {}
            }
        }
    }

    fn flush_outbox(&mut self) {
        for outgoing in mem::take(&mut self.session.outbox) {
            let (client, frame) = match outgoing {
                Outgoing::Shutdown { reason, keep } => {
                    self.stop(reason, keep);
                    continue;
                }
                Outgoing::To(client, frame) => (client, frame),
            };
            if let Some(conn) = self.conns.iter_mut().find(|c| c.serves(client)) {
                conn.tell(&mut self.session, &frame);
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
        let placed = attached.placement.clone();
        let painted = (attached.known).then_some(render::Painted {
            grid: &attached.shown,
            placement: &placed,
        });
        render::compose_held(
            session,
            view,
            &mut attached.spare,
            &mut attached.placement,
            painted,
        );
        if !before.is_empty() {
            attached.paint(&mut conn.out, &before);
        }
        // A picture gone or moved has its cells painted again, blank, which
        // takes its pixels away; one whose cells this paint paints over, or
        // new, is sent after the text.
        let mut pictures = render::pictures(session, client, &attached.placement);
        let known = attached.known;
        for &(id, rect) in &attached.placement.panes {
            if session.panes.get(&id).is_some_and(Pane::mid_frame) {
                pictures.retain(|p| p.pane != id);
                let (rows, cols) = (attached.spare.rows, attached.spare.cols);
                let painted = render::Painted {
                    grid: &attached.shown,
                    placement: &placed,
                };
                if known && painted.kept(id, rect, rows, cols, &attached.placement) {
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
            attached.paint(&mut conn.out, &self.paint_buffer);
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
            attached.paint(&mut conn.out, &drawn);
        }
        attached.sixels.keep(&pictures);
        attached.pictures = pictures;
        if let Some(view) = session.views.get_mut(&client) {
            view.dirty = false;
        }
    }

    /// The `at`th socket the server listens on while it serves, and the
    /// grant's socket, if a grant made it, which its connections reach
    /// through.
    fn listening(&mut self, at: usize) -> Option<(&mut Listening, Option<PathBuf>)> {
        let Phase::Serving { listening, .. } = &mut self.phase else {
            return None;
        };
        (listening.get_mut(at)).map(|(socket, l)| (l, socket.clone()))
    }

    /// Accepts the connections waiting on the server's socket, or with
    /// `grant` on that socket `fux grant` made.
    fn accept(&mut self, grant: usize) {
        // Accept until the backlog is empty, so one tick drains it.
        loop {
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
            self.conns.push(Conn::new(Wire::Socket(stream), through));
        }
    }

    /// Takes the connections knocks at the door bring: each a client's two
    /// FIFOs, whose owner the door checked, as the socket's peer check
    /// does. With `grant`, the door beside that grant's socket.
    fn open_door(&mut self, grant: usize) {
        let Some((
            Listening {
                door: Some(door), ..
            },
            through,
        )) = self.listening(grant)
        else {
            return;
        };
        for fifos in door.knocks() {
            self.conns
                .push(Conn::new(Wire::Door(fifos), through.clone()));
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
        if flags.intersects(PollFlags::OUT) {
            conn.flush_tty(session);
        }
        if flags.intersects(PollFlags::HUP | PollFlags::ERR) && conn.attached().is_some() {
            conn.end(session, &ToAttached::Exit("detached: the terminal closed"));
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
        if flags.intersects(PollFlags::OUT) || (gone && !reads) {
            conn.flush(&mut self.session);
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
            // A client may send its terminal with its `Attach`.
            let got = match &conn.wire {
                Wire::Socket(stream) => fuxix::socket::recv_with_fd(stream, &mut self.read_buffer),
                Wire::Door(fifos) => {
                    fuxix::io::read(&fifos.up, &mut self.read_buffer).map(|n| (n, None))
                }
            };
            match got {
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
                    conn.end(session, &Greeting::Exit(refused));
                    return Ok(true);
                }
                let between = Mismatch::between(&hello, &ours);
                conn.send(session, &Greeting::Hello(ours));
                match (between, hello.role) {
                    (Err(mismatch), _) => {
                        conn.end(session, &Greeting::Exit(&mismatch.to_string()));
                    }
                    (Ok(()), Role::Kill) => {
                        conn.end(session, &Greeting::Done(0));
                        self.stop("stopped by fux force-kill-server".into(), false);
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
                    size,
                    workspace,
                    read_only,
                    env,
                }) = decoder.frame()?
                else {
                    return Ok(false);
                };
                let (passed, kind) = (passed.take(), *kind);
                match session.attach_as(kind, size.rows(), size.cols(), workspace.as_ref()) {
                    Ok(client) => {
                        if let Some(view) = session.views.get_mut(&client) {
                            view.read_only = read_only;
                            view.env = env;
                        }
                        // The client's terminal, if it sent it, taken or
                        // not: the client is told so, and its id, before
                        // anything is painted.
                        let tty = passed.map(|fd| fuxix::terminal::reopen(&fd).ok());
                        let taken = tty.as_ref().map(Option::is_some);
                        let tty = tty.flatten().map(TakenTerminal::new);
                        conn.enter(
                            session,
                            Stage::Attached(Box::new(Attached::new(client, tty))),
                        );
                        conn.send(session, &ToAttached::Welcome(client, taken));
                    }
                    Err(error) => conn.end(session, &ToAttached::Exit(&error.to_string())),
                }
            }
            Stage::Attached(attached) => match decoder.frame()? {
                Some(AttachedFrame::Act(act)) => attached.act(session, *act),
                // The client's terminal was resized, which may have moved
                // what it shows even if the size it ends at is the same:
                // forgotten, it is repainted in full either way.
                Some(AttachedFrame::Resize(size)) => {
                    attached.known = false;
                    session.resize(attached.client, size.rows(), size.cols());
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
    fn request(&mut self, index: usize, request: crate::command::Request, context: Context) {
        let restore_runs = self.saved.as_ref().is_some_and(Saved::restoring);
        let session = &mut self.session;
        let Some(conn) = self.conns.get_mut(index) else {
            return;
        };
        // A restore is the server's own, which no grant's socket makes.
        let came = match conn.grant.clone() {
            Some(socket) => Came::Grant(socket),
            None if context.restoring && restore_runs => Came::Restore,
            None => Came::Socket,
        };
        // While a saved session comes back, other requests wait for it,
        // so that none takes an id it asks for.
        let held = match came {
            Came::Restore => false,
            Came::Socket | Came::Grant(_) => restore_runs,
        };
        if held {
            conn.stage = Stage::Held(Box::new(Asked { request, context }));
            return;
        }
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
            Some(s) if s == session.server_id => Within::This,
            Some(_) => Within::Other,
        };
        let cwd = Some(cwd).filter(|p| p.is_dir());
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
                gave: pane,
                within,
                cwd: cwd.clone(),
                env,
                came,
            }),
        };
        match session.begin(request, &origin) {
            Begun::Done(outcome) => conn.answer(session, outcome),
            Begun::Grant(socket, workspace) => {
                let (outcome, revoked) = match &mut self.phase {
                    Phase::Serving { listening, .. } => {
                        grant(listening, session, (socket, workspace), cwd)
                    }
                    Phase::Stopping(_) => {
                        (Reply::failed(Code::Grant, "the server is stopping"), None)
                    }
                };
                conn.answer(session, outcome);
                // Every connection through a grant revoked ends, idle or
                // not: one kept would reach what the path is granted next.
                for conn in
                    (self.conns.iter_mut()).filter(|c| revoked.is_some() && c.grant == revoked)
                {
                    if conn.stage.reads() {
                        let why = "the grant this came through was revoked";
                        conn.end(session, &ToCommand::Exit(why));
                    }
                }
            }
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
        if flags.intersects(PollFlags::OUT)
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
            let Some(pane) = self.session.panes.get(&id) else {
                return;
            };
            // Output already waits: it is read before any more is taken.
            if pane.has_unread() {
                break false;
            }
            let Process::Reading(child) = &pane.process else {
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
        let leaders: Vec<Pid> = (panes.chain(session.dying.iter().map(|d| &d.child)))
            .map(|child| child.leader.pid())
            .chain(self.killed.iter().copied())
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

    /// A saved layout's file is one per socket: an escape in a path never
    /// makes it another's, and one path spelt two ways has one.
    #[test]
    fn a_layout_file_is_one_per_socket() -> Result<(), String> {
        let file = |path: &str| {
            let socket = crate::socket::checked(path, "test").map_err(|e| e.to_string())?;
            Saved::of(&socket)
                .map(|s| s.path)
                .ok_or_else(|| "no state directory".to_owned())
        };
        assert_ne!(file("/%41/x.sock")?, file("/2541/x.sock")?);
        assert_eq!(file("/run//x.sock")?, file("/run/x.sock")?);
        assert_eq!(file("/run/./x.sock")?, file("/run/x.sock")?);
        Ok(())
    }

    /// A session with a client attached on a connection whose terminal the
    /// server took, the other end of a socket pair standing in for it: the
    /// session, the connection, the client's end of its socket, and the
    /// terminal's other end.
    fn with_terminal() -> Result<(Session, Conn, UnixStream, UnixStream), String> {
        let (session, client) = crate::session::testing::attached(10, 40)?;
        let (stream, peer) = UnixStream::pair().map_err(|e| e.to_string())?;
        let (tty, far) = UnixStream::pair().map_err(|e| e.to_string())?;
        tty.set_nonblocking(true).map_err(|e| e.to_string())?;
        let mut conn = Conn::new(Wire::Socket(stream), None);
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
        let socket = crate::socket::checked(&path, "test").map_err(|x| e(&x))?;
        let listening = Listening::open(&socket).map_err(|x| e(&x))?;
        assert!(listening.listener.is_some(), "the socket serves");
        assert!(listening.door.is_none(), "with no door");
        drop(listening);
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    /// A grant from a directory whose name is not UTF-8 is bound at the
    /// path it names and records, not at a lossy spelling of it. Linux
    /// only: macOS's file systems refuse a name that is not UTF-8.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_grant_from_a_directory_not_utf8_binds_its_own_path() -> Result<(), String> {
        use std::os::unix::ffi::OsStrExt;
        let e = |e: &dyn std::fmt::Display| e.to_string();
        let dir = (fuxix::file::scratch().map_err(|x| e(&x))?)
            .join(format!("fux-grant-{}", std::process::id()))
            .join(std::ffi::OsStr::from_bytes(b"caf\xe9"));
        let _ = dir.parent().map(std::fs::remove_dir_all);
        let _ = dir.parent().map(std::fs::create_dir_all);
        let mut session = crate::session::testing::started(crate::config::Config::default())?;
        let main = crate::command::WsRef::Name("main".into());
        let mut listening = Vec::new();
        let asked = ("g.sock".to_owned(), Some(main));
        let (reply, _) = grant(&mut listening, &mut session, asked, Some(dir.clone()));
        assert_eq!(reply.end, crate::protocol::End::Done, "{reply:?}");
        let bound = std::fs::symlink_metadata(dir.join("g.sock")).map_err(|x| e(&x))?;
        assert!(std::os::unix::fs::FileTypeExt::is_socket(
            &bound.file_type()
        ));
        drop(listening);
        let _ = dir.parent().map(std::fs::remove_dir_all);
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
        conn.end(&mut session, &ToAttached::Exit("detached"));
        let (painted, exit) = sent(&conn);
        assert!(painted.ends_with(b"end"), "the last of what waited");
        assert_eq!(exit.as_deref(), Some("detached"));
        assert!(session.views.is_empty(), "the client is detached");
        Ok(())
    }

    /// A client let go gives its terminal the cursor's shape back as the
    /// terminal's own (DECSCUSR 0, then xterm's 7) only if a pane set one:
    /// by ctlseqs, 0 is a blinking block, which a terminal fux never told
    /// otherwise was not asked to be.
    #[test]
    fn a_cursor_shape_is_given_back_only_once_one_was_set() -> Result<(), String> {
        use std::io::Read;
        for (shape, given) in [
            (fux_vt::CursorShape::Initial, ""),
            (fux_vt::CursorShape::SteadyBar, "\x1b[0 q\x1b[7 q"),
        ] {
            let (mut session, mut conn, _peer, mut far) = with_terminal()?;
            if let Some(attached) = conn.attached() {
                attached.shown.modes.cursor_shape = shape;
            }
            conn.end(&mut session, &ToAttached::Exit("detached"));
            far.set_nonblocking(true).map_err(|e| e.to_string())?;
            let mut written = Vec::new();
            let _ = far.read_to_end(&mut written);
            assert_eq!(String::from_utf8_lossy(&written), given, "{shape:?}");
        }
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
