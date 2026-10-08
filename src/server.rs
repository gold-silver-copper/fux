//! The server: one thread (but for fuxix's watchdog while a PTY opens on
//! macOS), one `poll` loop over the listening socket, a signal pipe, every
//! client and every pane's PTY.
use crate::bytes::ByteQueue;
use crate::command::ClientId;
use crate::config::Config;
use crate::layout::{PaneId, Placement};
use crate::protocol::{
    AttachFrame, Command, Decoder, Frame, Hello, PROTOCOL, Role, ServerFrame, Stream,
};
use crate::render::{self, Grid};
use crate::session::{Ctx, Outgoing, Session};
use crate::socket::SocketPath;
use fuxix::poll::{Events as PollFlags, PollFd};
use signal_hook::consts::{SIGCHLD, SIGHUP, SIGINT, SIGTERM};
use std::io::{ErrorKind, Write};
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
    reported: Instant,
    /// Refused since the last report, and in all.
    refused: u64,
    total: u64,
}

struct Conn {
    stream: UnixStream,
    decoder: Decoder,
    out: ByteQueue,
    role: Option<Role>,
    client: Option<ClientId>,
    /// What the client's terminal shows, for diffing, once `painted`.
    shown: Grid,
    /// Whether `shown` is on the client's terminal; if not, the next paint
    /// is a full one.
    painted: bool,
    /// The grid the next paint is composed into, then swapped with `shown`:
    /// the two are reused by every paint, as is the placement of its panes.
    spare: Grid,
    placement: Placement,
    /// When the next paint may be made.
    clock: PaintClock,
    /// Paints were skipped while its output was full: repaint all once drained.
    starved: bool,
    /// Close once `out` is flushed.
    closing: bool,
    dead: bool,
    /// The client's terminal, if the server took it.
    tty: Option<TakenTerminal>,
    /// A descriptor the client sent, until the `Attach` it came with.
    passed: Option<std::os::fd::OwnedFd>,
}

/// When a client may be painted next: `PAINT` after its last paint, as a
/// pane that keeps the screen changing is painted; at once for a
/// keystroke's echo; and once the panes are quiet, if that is sooner.
struct PaintClock {
    /// `PAINT` after the last paint.
    next: Instant,
    /// When the panes will have been quiet for `QUIET`, if output came
    /// before `next`.
    settled: Option<Instant>,
    /// The pane the client last typed into, until it writes: what it writes
    /// then, a keystroke's echo, is painted at once, not held to `PAINT`
    /// behind a pane that keeps the screen changing (tmux paints it at
    /// once too). One paint a keystroke at most.
    echo: Option<PaneId>,
}

impl PaintClock {
    fn new() -> PaintClock {
        PaintClock {
            next: Instant::now(),
            settled: None,
            echo: None,
        }
    }
    /// When the next paint may be made.
    fn due(&self) -> Instant {
        self.settled
            .map_or(self.next, |settled| settled.min(self.next))
    }
    /// The next paint may be made now.
    fn paint_now(&mut self) {
        self.next = Instant::now();
    }
    /// A paint was made at `now`.
    fn painted(&mut self, now: Instant) {
        self.next = crate::after(now, PAINT);
        self.settled = None;
    }
    /// The client typed into `pane`, its focus, if any.
    fn typed_into(&mut self, pane: Option<PaneId>) {
        self.echo = pane;
    }
    /// Pane `pane` wrote: if it is the pane typed into, its echo is painted
    /// at once.
    fn wrote(&mut self, pane: PaneId) {
        if self.echo == Some(pane) {
            self.echo = None;
            self.paint_now();
        }
    }
    /// Panes wrote, read at `now`: output held to `PAINT` is painted once
    /// the panes are quiet for `QUIET`.
    fn output(&mut self, now: Instant) {
        if now < self.next {
            self.settled = Some(crate::after(now, QUIET));
        }
    }
}

/// The client's terminal, which it sent with its `Attach` and the server
/// reopened (`fuxix::terminal::reopen`): its keys are read and its paints
/// written here, not relayed in frames. tmux's server writes to its
/// client's terminal as well.
///
/// One is held only while its client is attached: every `Exit` gives it
/// back first (`Conn::send`). So a connection being stopped or closed has
/// nothing waiting for its terminal, and what waits on those is `out`'s.
struct TakenTerminal {
    fd: std::os::fd::OwnedFd,
    /// What waits to be written to it.
    out: ByteQueue,
    /// Whether the paints written to it saved its title
    /// (`outer::TITLE_PUSH`) and have not restored it: restored as it is
    /// given back, as the client restores it after frames.
    title_saved: bool,
}

impl TakenTerminal {
    fn new(fd: std::os::fd::OwnedFd) -> TakenTerminal {
        TakenTerminal {
            fd,
            out: ByteQueue::default(),
            title_saved: false,
        }
    }
    /// Writes what waits, as far as the terminal takes it; false if it
    /// failed, and is to be given up.
    fn flush(&mut self) -> bool {
        write_out(&self.fd, &mut self.out)
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

/// Input from a client's terminal, read at `now`, to the session; the pane
/// it went to, the client's focus, is the one whose echo is painted at once.
fn take_input(
    session: &mut Session,
    clock: &mut PaintClock,
    client: ClientId,
    bytes: &[u8],
    now: Instant,
) {
    session.input_at(client, bytes, now);
    clock.typed_into(
        session
            .views
            .get(&client)
            .and_then(crate::view::View::focus),
    );
}

impl Conn {
    fn new(stream: UnixStream) -> Conn {
        Conn {
            stream,
            decoder: Decoder::default(),
            out: ByteQueue::default(),
            role: None,
            client: None,
            shown: Grid::new(0, 0),
            painted: false,
            spare: Grid::new(0, 0),
            placement: Placement::default(),
            clock: PaintClock::new(),
            starved: false,
            closing: false,
            dead: false,
            tty: None,
            passed: None,
        }
    }

    /// Encodes a frame straight into the output. An `Exit` to a client
    /// whose terminal the server writes to comes after what waits for the
    /// terminal, as much as it takes at once, and its title restored:
    /// then the terminal is the client's again.
    fn send(&mut self, frame: &ServerFrame) {
        if matches!(frame, ServerFrame::Exit(_)) {
            self.give_back_tty();
        }
        if self.out.push_with(|out| frame.encode_into(out)).is_err() {
            self.dead = true;
        }
    }

    /// Sends the connection's last frame: it closes once what waits is
    /// sent.
    fn end(&mut self, frame: &ServerFrame) {
        self.send(frame);
        self.closing = true;
    }

    /// Bytes for a client in `stream`. The paint stream, which carries what
    /// the session sends its terminal outside a paint too, goes to the
    /// terminal if the server writes to it; the rest, and all of it for a
    /// client that kept its terminal, is framed for the client.
    fn send_stream(&mut self, stream: Stream, bytes: &[u8]) {
        if let Some(tty) = &mut self.tty
            && stream == Stream::Paint
        {
            note_title(&mut tty.title_saved, bytes);
            tty.out.push(bytes);
            return;
        }
        stream.encode_into(bytes, &mut self.out);
    }

    /// Bytes waiting for the client, in frames or for its terminal.
    fn pending(&self) -> usize {
        let tty = self.tty.as_ref().map_or(0, |tty| tty.out.len());
        self.out.len().saturating_add(tty)
    }

    /// Writes what waits for the client's terminal, as far as it takes it;
    /// a terminal that fails is given up.
    fn flush_tty(&mut self) {
        if let Some(tty) = &mut self.tty
            && !tty.flush()
        {
            // Given up, and its client detached, as one that can no longer
            // be read is (`serve_tty`): no one would read its keys.
            self.tty = None;
            if self.client.is_some() && !self.closing {
                self.end(&ServerFrame::Exit("detached: the terminal closed"));
            }
        }
    }

    /// Hands the terminal back: its title restored if the paints saved it,
    /// what waits for it written as far as it takes it at once and the
    /// rest sent to the client to write, and the server's descriptor for
    /// it closed.
    fn give_back_tty(&mut self) {
        if let Some(mut tty) = self.tty.take() {
            if tty.title_saved {
                tty.out.push(crate::outer::TITLE_POP);
            }
            // What it does not take at once, the title's restore with it,
            // goes to the client in paint frames, before the `Exit` that
            // follows: it writes them to the terminal as it writes paints.
            if tty.flush() && !tty.out.is_empty() {
                Stream::Paint.encode_into(tty.out.as_slice(), &mut self.out);
            }
        }
    }

    /// Writes what waits for the client until it is all written or the
    /// socket takes no more; a connection that fails is marked dead. What
    /// waits for its terminal is written too.
    fn flush(&mut self) {
        self.flush_tty();
        self.dead |= !write_out(&self.stream, &mut self.out);
    }
}

pub struct Server {
    session: Session,
    listener: UnixListener,
    conns: Vec<Conn>,
    children: UnixStream,
    stops: UnixStream,
    /// When a server that is stopping stops waiting for its clients and
    /// processes.
    stop_by: Option<Instant>,
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
    /// While set, the listener is not polled: an `accept` failed in a way
    /// that retrying at once would repeat, and it stays readable meanwhile.
    listen_after: Option<Instant>,
    /// When an `accept` failure other than a shortage was last logged.
    accept_logged: Option<Instant>,
}

/// Notes a title saved or restored in a paint for a client's terminal: the
/// later of the two wins (`client::note_title` does the same for paints
/// it relays).
fn note_title(saved: &mut bool, paint: &[u8]) {
    if let Some(now) = crate::outer::title_saved_by(paint) {
        *saved = now;
    }
}

fn log(message: &str) {
    eprintln!("fux server: {message}");
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
        listener,
        conns: Vec::new(),
        children,
        stops,
        stop_by: None,
        read_buffer: vec![0u8; 64 * 1024],
        paint_buffer: Vec::new(),
        slots: Vec::new(),
        ready: Vec::new(),
        spare: std::fs::File::open("/dev/null").ok(),
        shortage: None,
        listen_after: None,
        accept_logged: None,
    };
    server.run();
    drop(endpoint);
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
            // Each change settles as it is made; this catches output that
            // dropped rows a copy mode held.
            self.session.settle_if_needed();
            self.flush_outbox();
            let now = Instant::now();
            if let Some(by) = self.stop_by
                && (now >= by
                    || (self.session.dying.is_empty()
                        && self.conns.iter().all(|c| c.out.is_empty())))
            {
                self.finish_dying(true);
                return;
            }
            self.paint(now);
            let timeout = self.timeout(Instant::now());
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
            self.escapes(now);
            self.session.type_due(now);
            // Input given to panes this tick is written now rather than
            // when the next poll says their terminals can take it.
            self.write_waiting_panes();
            self.session.release_frames(now);
            self.finish_dying(false);
            self.close_conns();
        }
    }

    /// Drops every connection that is dead, or closing with nothing left to
    /// send, detaching its client first: however a connection ends, its
    /// view goes with it.
    fn close_conns(&mut self) {
        let session = &mut self.session;
        self.conns.retain_mut(|conn| {
            let gone = conn.dead || conn.closing && conn.out.is_empty();
            if gone && let Some(client) = conn.client.take() {
                session.detach(client);
            }
            !gone
        });
    }

    /// What a client's decoder waits on is taken as it is once its deadline
    /// passes: a lone Escape becomes a key, an answer cut short is dropped
    /// (`Decoder::deadline`).
    fn escapes(&mut self, now: Instant) {
        // One at a time, in the clients' order: an Escape runs whatever it
        // completes.
        while let Some(client) = self.session.escape_due(now) {
            self.session.escape(client);
        }
    }

    fn timeout(&self, now: Instant) -> Option<Duration> {
        let session = &self.session;
        let paints = self.conns.iter().filter_map(|conn| {
            let dirty = session.views.get(&conn.client?).is_some_and(|v| v.dirty);
            (dirty && !conn.starved).then_some(conn.clock.due())
        });
        let escapes = session.views.values().filter_map(|v| v.decoder.deadline());
        paints
            .chain(escapes)
            .chain(session.dying.iter().map(|d| d.deadline))
            .chain(session.next_typing())
            .chain(session.next_frame_release())
            .chain(self.stop_by)
            .chain(self.listen_after)
            .min()
            .map(|d| d.saturating_duration_since(now))
    }

    /// Waits for descriptors to be ready, or `timeout`; which are, and for
    /// what, go into `ready`.
    fn poll(&mut self, timeout: Option<Duration>, ready: &mut Vec<(Slot, PollFlags)>) {
        ready.clear();
        let slots = &mut self.slots;
        slots.clear();
        // Built afresh, as it borrows the descriptors.
        let mut fds = Vec::new();
        if self.listen_after.is_some_and(|at| Instant::now() >= at) {
            self.listen_after = None;
        }
        if self.stop_by.is_none() && self.listen_after.is_none() {
            fds.push(PollFd::new(&self.listener, PollFlags::IN));
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
            if let Some(tty) = &conn.tty {
                let mut flags = PollFlags::IN;
                if !tty.out.is_empty() {
                    flags |= PollFlags::OUT;
                }
                fds.push(PollFd::new(&tty.fd, flags));
                slots.push(Slot::Tty(i));
            }
        }
        for (id, pane) in &self.session.panes {
            if pane.hung_up {
                continue;
            }
            if let Some(child) = &pane.child {
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
        if self.stop_by.is_some() {
            return;
        }
        log(&reason);
        self.session.shutdown();
        for conn in &mut self.conns {
            if conn.role == Some(Role::Attach) {
                conn.end(&ServerFrame::Exit(&format!(
                    "the fux server stopped: {reason}"
                )));
            }
        }
        self.stop_by = Some(crate::after(Instant::now(), STOP_WAIT));
    }

    fn flush_outbox(&mut self) {
        for outgoing in std::mem::take(&mut self.session.outbox) {
            match outgoing {
                Outgoing::Bytes(client, bytes) => {
                    if let Some(conn) = self.conns.iter_mut().find(|c| c.client == Some(client)) {
                        conn.send_stream(Stream::Paint, &bytes);
                    }
                }
                Outgoing::Exit(client, reason) => {
                    if let Some(conn) = self.conns.iter_mut().find(|c| c.client == Some(client)) {
                        conn.end(&ServerFrame::Exit(&reason));
                        conn.client = None;
                    }
                }
                Outgoing::Shutdown(reason) => self.stop(reason),
            }
        }
    }

    fn paint(&mut self, now: Instant) {
        for conn in &mut self.conns {
            let Some(client) = conn.client else { continue };
            if conn.pending() > OUTPUT_CAP {
                // A client that stops reading gets nothing more queued; once
                // it drains, one full repaint.
                conn.starved = true;
                conn.painted = false;
                continue;
            }
            if conn.starved {
                if conn.pending() != 0 {
                    continue;
                }
                conn.starved = false;
            }
            let dirty = self.session.views.get(&client).is_some_and(|v| v.dirty);
            if !dirty || now < conn.clock.due() {
                continue;
            }
            // The title and bell marks first: the bar shows the marks.
            let before = self.session.before_paint(client);
            if !render::compose_into(&self.session, client, &mut conn.spare, &mut conn.placement) {
                continue;
            }
            if !before.is_empty() {
                conn.send_stream(Stream::Paint, &before);
            }
            // The same screen as the client shows: nothing to send, not even
            // the envelope, whose cursor hide and show would restart a
            // blinking cursor.
            if conn.painted && conn.spare.same_as(&conn.shown) {
                if let Some(view) = self.session.views.get_mut(&client) {
                    view.dirty = false;
                }
                continue;
            }
            self.paint_buffer.clear();
            let shown = conn.painted.then_some(&conn.shown);
            render::paint_into(shown, &conn.spare, &mut self.paint_buffer);
            conn.send_stream(Stream::Paint, &self.paint_buffer);
            // Written now rather than when the next poll says it can be: a
            // keystroke's echo goes out a round sooner.
            conn.flush();
            std::mem::swap(&mut conn.shown, &mut conn.spare);
            conn.painted = true;
            conn.clock.painted(now);
            if let Some(view) = self.session.views.get_mut(&client) {
                view.dirty = false;
            }
        }
    }

    fn accept(&mut self) {
        // Accept until the backlog is empty, so one tick drains it.
        loop {
            // The spare makes room for this accept, and is taken back after
            // it: if it cannot be, the connection took the last descriptor.
            self.spare = None;
            let accepted = self.listener.accept();
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
                            shortage.total
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
                    self.listen_after = Some(crate::after(now, ACCEPT_REST));
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
        match &mut self.shortage {
            None => {
                log(
                    "out of file descriptors: new connections are refused, each told why, until some close",
                );
                self.shortage = Some(Shortage {
                    reported: now,
                    refused: 1,
                    total: 1,
                });
            }
            Some(shortage) => {
                shortage.refused = shortage.refused.saturating_add(1);
                shortage.total = shortage.total.saturating_add(1);
                if now.duration_since(shortage.reported) >= REPORT_EVERY {
                    log(&format!(
                        "still out of file descriptors: {} connections refused in the last {} s",
                        shortage.refused,
                        now.duration_since(shortage.reported).as_secs()
                    ));
                    shortage.reported = now;
                    shortage.refused = 0;
                }
            }
        }
    }

    /// The terminal a client sent: room to write what waits for it, or
    /// keys to read. A terminal that closed detaches its client, as a
    /// client whose terminal closes detaches itself.
    fn serve_tty(&mut self, index: usize, flags: PollFlags, now: Instant) {
        // A connection that died this tick is closed at its end, its client
        // detached: its keys are not taken.
        let Some(conn) = self.conns.get_mut(index).filter(|c| !c.dead) else {
            return;
        };
        if flags.contains(PollFlags::OUT) {
            conn.flush_tty();
        }
        if !flags.intersects(PollFlags::IN | PollFlags::HUP | PollFlags::ERR) {
            return;
        }
        // A terminal resized and then typed into: the keys come here, the
        // size by the client's `Resize`, which may come after them. The
        // size is read here first, so that a program sees the keys at the
        // size they were typed at, as when both came by the client.
        if let Some(tty) = &conn.tty
            && let Some(client) = conn.client
            && let Ok(size) = fuxix::terminal::window_size(&tty.fd)
            && size.0 > 0
            && size.1 > 0
            && self.session.views.get(&client).map(|v| (v.rows, v.cols)) != Some(size)
        {
            conn.painted = false;
            self.session.resize(client, size.0, size.1);
        }
        let mut read = 0usize;
        let mut gone = false;
        while read < CONN_READ {
            let Some(conn) = self.conns.get_mut(index) else {
                return;
            };
            let Some(tty) = &conn.tty else { return };
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
            let Some(client) = conn.client else { return };
            let bytes = self.read_buffer.get(..n).unwrap_or_default();
            take_input(&mut self.session, &mut conn.clock, client, bytes, now);
            if all {
                break;
            }
        }
        if gone
            && let Some(conn) = self.conns.get_mut(index)
            && let Some(client) = conn.client.take()
        {
            conn.end(&ServerFrame::Exit("detached: the terminal closed"));
            self.session.detach(client);
        }
    }

    /// A client's connection is ready, as found at `now`.
    fn serve_conn(&mut self, index: usize, flags: PollFlags, now: Instant) {
        if flags.contains(PollFlags::OUT) {
            self.write_conn(index);
        }
        if flags.intersects(PollFlags::IN | PollFlags::HUP | PollFlags::ERR) {
            self.read_conn(index, now);
        }
    }

    fn write_conn(&mut self, index: usize) {
        if let Some(conn) = self.conns.get_mut(index) {
            conn.flush();
        }
    }

    /// Reads what a client sent until it has sent no more, or `CONN_READ`
    /// bytes, or more than 256 whole frames, or a bad one; each whole frame
    /// is decoded and handled as it arrives, in order, and nothing after a
    /// bad one. So no whole frame is left in the decoder for a poll that
    /// would not come.
    fn read_conn(&mut self, index: usize, now: Instant) {
        let Some(conn) = self.conns.get_mut(index) else {
            return;
        };
        // Held apart while its frames, which borrow it, are handled.
        let mut decoder = std::mem::take(&mut conn.decoder);
        let (mut frames, mut read) = (0usize, 0usize);
        let closed = loop {
            let Some(conn) = self.conns.get_mut(index) else {
                break false;
            };
            // A client may send its terminal with its `Attach`.
            match fuxix::socket::recv_with_fd(&conn.stream, &mut self.read_buffer) {
                Ok((0, _)) => break true,
                Ok((n, fd)) => {
                    if fd.is_some() {
                        conn.passed = fd;
                    }
                    read = read.saturating_add(n);
                    decoder.push(self.read_buffer.get(..n).unwrap_or_default());
                    match self.take_frames(index, &mut decoder, now) {
                        Ok(taken) => frames = frames.saturating_add(taken),
                        Err(error) => {
                            log(&format!("a client sent a bad frame: {error}"));
                            break true;
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
            conn.dead |= closed;
        }
    }

    /// Handles each whole frame `decoder` holds, decoded as what the
    /// connection may send at its point: a `Hello`, then an attaching
    /// client's frames or a command. How many there were.
    fn take_frames(
        &mut self,
        index: usize,
        decoder: &mut Decoder,
        now: Instant,
    ) -> Result<usize, crate::protocol::Error> {
        let mut frames = 0usize;
        loop {
            let Some(conn) = self.conns.get_mut(index) else {
                return Ok(frames);
            };
            let ending = conn.closing || conn.dead;
            let handled = match conn.role {
                None if !ending => decoder.frame()?.map(|hello| self.hello(index, hello)),
                Some(Role::Attach) if !ending => decoder
                    .frame()?
                    .map(|frame| self.attaching(index, frame, now)),
                Some(Role::Command) if !ending => {
                    decoder.frame()?.map(|command| self.command(index, command))
                }
                // A connection that is ending has had its last word: what
                // it sends after a Detach, a Command, a refused Hello or a
                // bad frame is dropped unread. A Kill ends at its Hello,
                // before its role is taken.
                None | Some(Role::Attach | Role::Command | Role::Kill) => {
                    *decoder = Decoder::default();
                    None
                }
            };
            if handled.is_none() {
                return Ok(frames);
            }
            frames = frames.saturating_add(1);
        }
    }

    /// A client's first frame: answered with the server's, and its role
    /// taken unless it is to stop the server or speaks another protocol.
    fn hello(&mut self, index: usize, hello: Hello) {
        let Some(conn) = self.conns.get_mut(index) else {
            return;
        };
        conn.send(&ServerFrame::Hello(Hello {
            protocol: PROTOCOL,
            version: env!("CARGO_PKG_VERSION"),
            role: hello.role,
        }));
        if hello.role == Role::Kill {
            conn.end(&ServerFrame::Done { status: 0 });
            self.stop("stopped by fux kill-server".into());
        } else if hello.protocol != PROTOCOL {
            conn.end(&ServerFrame::Exit(&format!(
                "the server speaks protocol {PROTOCOL} (fux {}); restart it with `fux kill-server`",
                env!("CARGO_PKG_VERSION")
            )));
        } else {
            conn.role = Some(hello.role);
        }
    }

    /// An attaching client's frame. Its input goes to the session as it
    /// was lent from the decoder.
    fn attaching(&mut self, index: usize, frame: AttachFrame, now: Instant) {
        let Some(conn) = self.conns.get_mut(index) else {
            return;
        };
        match frame {
            AttachFrame::Attach {
                rows,
                cols,
                workspace,
            } if conn.client.is_none() => {
                match self.session.attach(rows.get(), cols.get(), workspace) {
                    Ok(client) => {
                        conn.client = Some(client);
                        conn.clock.paint_now();
                        // The client's terminal, if it sent it: taken, the
                        // client is told before anything is painted.
                        if let Some(passed) = conn.passed.take() {
                            conn.tty = fuxix::terminal::reopen(&passed)
                                .ok()
                                .map(TakenTerminal::new);
                            conn.send(&ServerFrame::Terminal {
                                taken: conn.tty.is_some(),
                            });
                        }
                    }
                    Err(error) => conn.end(&ServerFrame::Exit(&error.to_string())),
                }
            }
            // Attached already.
            AttachFrame::Attach { .. } => conn.dead = true,
            AttachFrame::Input(bytes) => {
                if let Some(client) = conn.client {
                    take_input(&mut self.session, &mut conn.clock, client, bytes, now);
                }
            }
            AttachFrame::Resize { rows, cols } => {
                if let Some(client) = conn.client {
                    conn.painted = false;
                    self.session.resize(client, rows.get(), cols.get());
                }
            }
            AttachFrame::Detach => {
                if let Some(client) = conn.client.take() {
                    conn.end(&ServerFrame::Exit("detached"));
                    self.session.detach(client);
                }
            }
        }
    }

    /// A command client's command: run, and its output and status sent.
    fn command(&mut self, index: usize, Command { argv, cwd, pane }: Command) {
        let ctx = Ctx {
            client: None,
            pane: pane.and_then(|p| crate::command::parse_pane(&p).ok()),
            cwd: Some(PathBuf::from(cwd)).filter(|p| p.is_dir()),
        };
        let outcome = self.session.run(&argv, &ctx);
        let Some(conn) = self.conns.get_mut(index) else {
            return;
        };
        // Nothing is sent for no output.
        conn.send_stream(Stream::Stdout, outcome.stdout.as_bytes());
        if !outcome.stderr.is_empty() {
            let mut stderr = outcome.stderr;
            if !stderr.ends_with('\n') {
                stderr.push('\n');
            }
            conn.send_stream(Stream::Stderr, stderr.as_bytes());
        }
        conn.end(&ServerFrame::Done {
            status: outcome.status,
        });
    }

    fn serve_pane(&mut self, id: PaneId, flags: PollFlags) {
        if flags.contains(PollFlags::OUT) {
            self.write_pane(id);
        }
        if flags.intersects(PollFlags::IN | PollFlags::HUP | PollFlags::ERR) {
            self.read_pane(id);
        }
    }

    /// Writes each pane's waiting input, as far as its terminal takes it.
    fn write_waiting_panes(&mut self) {
        let waiting: Vec<PaneId> = self
            .session
            .panes
            .iter()
            .filter(|(_, p)| !p.input.is_empty() && !p.hung_up && p.child.is_some())
            .map(|(id, _)| *id)
            .collect();
        for id in waiting {
            self.write_pane(id);
        }
    }

    fn write_pane(&mut self, id: PaneId) {
        let Some(pane) = self.session.panes.get_mut(&id) else {
            return;
        };
        let Some(child) = &pane.child else { return };
        while let Some(bytes) = pane.input.front() {
            match fuxix::io::write(&child.master, bytes) {
                Ok(0) => break,
                Ok(n) => pane.input.advance(n),
                Err(fuxix::Errno::INTR) => continue,
                Err(_) => break,
            }
        }
    }

    fn read_pane(&mut self, id: PaneId) {
        let mut total = 0;
        let mut buffer = [0u8; 16384];
        let mut ended = false;
        while total < PANE_READ {
            let Some(pane) = self.session.panes.get(&id) else {
                return;
            };
            let Some(child) = &pane.child else { return };
            match fuxix::io::read(&child.master, &mut buffer) {
                Ok(0) => {
                    ended = true;
                    break;
                }
                Ok(n) => {
                    // Past PANE_READ by at most one buffer, when the loop ends.
                    total = total.saturating_add(n);
                    self.session.output(id, buffer.get(..n).unwrap_or_default());
                    // A keystroke's echo is painted at once.
                    for conn in &mut self.conns {
                        conn.clock.wrote(id);
                    }
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
        // Output held to `PAINT` is painted once the panes are quiet.
        if total > 0 {
            let now = Instant::now();
            for conn in &mut self.conns {
                conn.clock.output(now);
            }
        }
        if ended {
            self.reap();
            // A pane whose master reports the end but whose program has not
            // exited stays until it does, unpolled: its master would report
            // the end on every poll, and the loop would spin.
            if let Some(pane) = self.session.panes.get_mut(&id) {
                pane.hung_up = true;
            }
        }
    }

    /// Checks every pane's process, since signals coalesce.
    fn reap(&mut self) {
        let exited: Vec<(PaneId, i32)> = self
            .session
            .panes
            .iter()
            .filter_map(|(id, pane)| {
                let child = pane.child.as_ref()?;
                crate::process::exited(child.pid).map(|status| (*id, status))
            })
            .collect();
        for (id, status) in exited {
            self.session.exited(id, status);
        }
    }

    fn finish_dying(&mut self, all: bool) {
        let now = Instant::now();
        let (due, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.session.dying)
            .into_iter()
            .partition(|d| all || d.deadline <= now);
        self.session.dying = rest;
        for mut dying in due {
            // The master closes first: a dying writer can hold on to it.
            dying.master = None;
            if !crate::process::finish(dying.pid) {
                // Killed but not yet exited: reaped on a later tick. When the
                // server itself is stopping, it waits for it here instead.
                if all {
                    // At most a second: a process stuck in the kernel is
                    // left to init rather than hold the exit.
                    for _ in 0..500 {
                        if crate::process::finish(dying.pid) {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(2));
                    }
                } else {
                    dying.deadline = crate::after(now, Duration::from_millis(10));
                    self.session.dying.push(dying);
                }
            }
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
    use std::os::fd::OwnedFd;

    /// A connection with a taken terminal, the other end of a socket pair
    /// standing in for it: one end of the client's socket, the
    /// terminal's, and the terminal's other end.
    fn with_terminal() -> std::io::Result<(Conn, UnixStream, UnixStream)> {
        let (stream, client) = UnixStream::pair()?;
        let (tty, far) = UnixStream::pair()?;
        tty.set_nonblocking(true)?;
        let mut conn = Conn::new(stream);
        conn.client = Some(ClientId(1));
        conn.tty = Some(TakenTerminal::new(OwnedFd::from(tty)));
        Ok((conn, client, far))
    }

    /// A terminal given back that takes no more at once (a slow link) has
    /// what it did not take, its title's restore last, sent to the client
    /// in paint frames before the `Exit`, rather than dropped.
    #[test]
    fn what_a_terminal_given_back_did_not_take_goes_to_the_client() -> std::io::Result<()> {
        let (mut conn, _client, _far) = with_terminal()?;
        // Fill the stand-in until it takes no more.
        if let Some(tty) = &mut conn.tty {
            tty.out.push(&vec![b'x'; 4 << 20]);
            tty.title_saved = true;
        }
        conn.flush_tty();
        conn.send(&ServerFrame::Exit("detached"));
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
        assert!(
            painted.ends_with(crate::outer::TITLE_POP),
            "the title's restore"
        );
        assert_eq!(exit.as_deref(), Some("detached"));
        Ok(())
    }

    /// A terminal that can no longer be written is given up and its client
    /// detached, as one that can no longer be read is: no one would read
    /// its keys.
    #[test]
    fn a_terminal_that_cannot_be_written_detaches_its_client() -> std::io::Result<()> {
        let (mut conn, _client, far) = with_terminal()?;
        drop(far);
        if let Some(tty) = &mut conn.tty {
            tty.out.push(b"paint");
        }
        conn.flush_tty();
        assert!(conn.tty.is_none());
        assert!(conn.closing, "the client is let go");
        let mut decoder = Decoder::default();
        decoder.push(conn.out.as_slice());
        assert_eq!(
            decoder.frame(),
            Ok(Some(ServerFrame::Exit("detached: the terminal closed")))
        );
        Ok(())
    }
}
