//! The server: one thread, one `poll` loop over the listening socket, a
//! signal pipe, every client and every pane's PTY.
use crate::command::ClientId;
use crate::config::Config;
use crate::layout::PaneId;
use crate::protocol::{Decoder, Frame, PROTOCOL, Role};
use crate::render::{self, Grid};
use crate::session::{Ctx, Outgoing, Session};
use rustix::event::{PollFd, PollFlags};
use std::io::{ErrorKind, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Paints are coalesced to at most one per client per this.
const PAINT: Duration = Duration::from_millis(16);
/// A client's unsent output may grow to this before paints for it stop.
const OUTPUT_CAP: usize = 4 << 20;
/// Output read from one pane per tick, so one busy pane cannot starve others.
const PANE_READ: usize = 64 * 1024;
/// How long a stopping server waits for its clients and processes.
const STOP_WAIT: Duration = Duration::from_millis(500);

struct Conn {
    stream: UnixStream,
    decoder: Decoder,
    out: Vec<u8>,
    role: Option<Role>,
    client: Option<ClientId>,
    /// What the client's terminal shows, for diffing.
    shown: Option<Grid>,
    next_paint: Instant,
    /// Paints were skipped while its output was full: repaint all once drained.
    starved: bool,
    /// Close once `out` is flushed.
    closing: bool,
    dead: bool,
}

impl Conn {
    fn send(&mut self, frame: &Frame) {
        match frame.encode() {
            Ok(bytes) => self.out.extend_from_slice(&bytes),
            Err(_) => self.dead = true,
        }
    }
    fn send_bytes(&mut self, make: fn(Vec<u8>) -> Frame, bytes: &[u8]) {
        for frame in Frame::chunked(make, bytes) {
            self.send(&frame);
        }
    }
}

pub struct Server {
    session: Session,
    listener: UnixListener,
    conns: Vec<Conn>,
    children: UnixStream,
    stops: UnixStream,
    stopping: Option<(Instant, String)>,
    /// When each client's decoder began waiting on a lone Escape.
    escapes: std::collections::HashMap<ClientId, Instant>,
}

fn log(message: &str) {
    eprintln!("fux server: {message}");
}

/// Runs a server on `socket` until it is told to stop or its last pane
/// closes.
pub fn serve(socket: &Path, config_path: Option<PathBuf>) -> Result<(), String> {
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
    let (endpoint, listener) = crate::socket::bind_socket(socket)?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let (children, children_in) = UnixStream::pair().map_err(|e| e.to_string())?;
    let (stops, stops_in) = UnixStream::pair().map_err(|e| e.to_string())?;
    children.set_nonblocking(true).map_err(|e| e.to_string())?;
    stops.set_nonblocking(true).map_err(|e| e.to_string())?;
    signal_hook::low_level::pipe::register(signal_hook::consts::SIGCHLD, children_in)
        .map_err(|e| e.to_string())?;
    for signal in [
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGINT,
        signal_hook::consts::SIGHUP,
    ] {
        let stop = stops_in.try_clone().map_err(|e| e.to_string())?;
        signal_hook::low_level::pipe::register(signal, stop).map_err(|e| e.to_string())?;
    }
    let mut session = Session::new(config, endpoint.path().to_owned(), true);
    session.config_path = config_path;
    session.config_error = error;
    session.start()?;
    let mut server = Server {
        session,
        listener,
        conns: Vec::new(),
        children,
        stops,
        stopping: None,
        escapes: std::collections::HashMap::new(),
    };
    server.run();
    drop(endpoint);
    Ok(())
}

enum Slot {
    Listener,
    Children,
    Stops,
    Conn(usize),
    Pane(PaneId),
}

impl Server {
    fn run(&mut self) {
        loop {
            self.session.settle();
            self.flush_outbox();
            let now = Instant::now();
            if let Some((since, _)) = &self.stopping
                && (now.duration_since(*since) >= STOP_WAIT
                    || (self.session.dying.is_empty()
                        && self.conns.iter().all(|c| c.out.is_empty())))
            {
                self.finish_dying(true);
                return;
            }
            self.paint(now);
            let timeout = self.timeout(Instant::now());
            let ready = self.poll(timeout);
            let now = Instant::now();
            for (slot, flags) in ready {
                match slot {
                    Slot::Listener => self.accept(),
                    Slot::Children => {
                        drain(&mut self.children);
                        self.reap();
                    }
                    Slot::Stops => {
                        drain(&mut self.stops);
                        self.stop("stopped by a signal".into());
                    }
                    Slot::Conn(i) => self.serve_conn(i, flags),
                    Slot::Pane(id) => self.serve_pane(id, flags),
                }
            }
            self.escapes(now);
            self.session.type_due(now);
            self.finish_dying(false);
            self.conns
                .retain(|c| !(c.dead || c.closing && c.out.is_empty()));
        }
    }

    /// A lone Escape becomes a key once `ESCAPE_DELAY` passes with no byte
    /// after it.
    fn escapes(&mut self, now: Instant) {
        let waiting: Vec<ClientId> = self
            .session
            .views
            .keys()
            .copied()
            .filter(|c| self.session.waiting(*c))
            .collect();
        self.escapes.retain(|c, _| waiting.contains(c));
        for client in waiting {
            let since = *self.escapes.entry(client).or_insert(now);
            if now.duration_since(since) >= crate::decode::ESCAPE_DELAY {
                self.escapes.remove(&client);
                self.session.escape(client);
            }
        }
    }

    fn timeout(&self, now: Instant) -> Option<Duration> {
        let mut deadline: Option<Instant> = None;
        let mut sooner = |at: Instant| {
            deadline = Some(deadline.map_or(at, |d| d.min(at)));
        };
        for conn in &self.conns {
            if let Some(client) = conn.client
                && self.session.views.get(&client).is_some_and(|v| v.dirty)
                && !conn.starved
            {
                sooner(conn.next_paint);
            }
        }
        for client in self
            .session
            .views
            .keys()
            .filter(|c| self.session.waiting(**c))
        {
            let since = self.escapes.get(client).copied().unwrap_or(now);
            sooner(since + crate::decode::ESCAPE_DELAY);
        }
        for dying in &self.session.dying {
            sooner(dying.deadline);
        }
        for pane in self.session.panes.values() {
            if let Some((_, at)) = pane.typed {
                sooner(at);
            }
        }
        if let Some((since, _)) = &self.stopping {
            sooner(*since + STOP_WAIT);
        }
        deadline.map(|d| d.saturating_duration_since(now))
    }

    fn poll(&mut self, timeout: Option<Duration>) -> Vec<(Slot, PollFlags)> {
        let mut slots = Vec::new();
        let mut fds = Vec::new();
        if self.stopping.is_none() {
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
        }
        for (id, pane) in &self.session.panes {
            if let Some(child) = &pane.child {
                let mut flags = PollFlags::IN;
                if !pane.input.is_empty() {
                    flags |= PollFlags::OUT;
                }
                fds.push(PollFd::new(&child.master, flags));
                slots.push(Slot::Pane(*id));
            }
        }
        let timespec = timeout.map(|t| rustix::event::Timespec {
            tv_sec: i64::try_from(t.as_secs()).unwrap_or(i64::MAX),
            tv_nsec: i64::from(t.subsec_nanos()),
        });
        match rustix::event::poll(&mut fds, timespec.as_ref()) {
            Ok(_) | Err(rustix::io::Errno::INTR) => {}
            Err(error) => log(&format!("poll: {error}")),
        }
        fds.iter()
            .map(PollFd::revents)
            .zip(slots)
            .filter(|(flags, _)| !flags.is_empty())
            .map(|(flags, slot)| (slot, flags))
            .collect()
    }

    fn stop(&mut self, reason: String) {
        if self.stopping.is_some() {
            return;
        }
        log(&reason);
        self.session.shutdown();
        for conn in &mut self.conns {
            if conn.role == Some(Role::Attach) {
                conn.send(&Frame::Exit(format!("the fux server stopped: {reason}")));
                conn.closing = true;
            }
        }
        self.stopping = Some((Instant::now(), reason));
    }

    fn flush_outbox(&mut self) {
        for outgoing in std::mem::take(&mut self.session.outbox) {
            match outgoing {
                Outgoing::Bytes(client, bytes) => {
                    if let Some(conn) = self.conns.iter_mut().find(|c| c.client == Some(client)) {
                        conn.send_bytes(Frame::Paint, &bytes);
                    }
                }
                Outgoing::Exit(client, reason) => {
                    if let Some(conn) = self.conns.iter_mut().find(|c| c.client == Some(client)) {
                        conn.send(&Frame::Exit(reason));
                        conn.closing = true;
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
            if conn.out.len() > OUTPUT_CAP {
                // A client that stops reading gets nothing more queued; once
                // it drains, one full repaint.
                conn.starved = true;
                conn.shown = None;
                continue;
            }
            if conn.starved {
                if !conn.out.is_empty() {
                    continue;
                }
                conn.starved = false;
            }
            let dirty = self.session.views.get(&client).is_some_and(|v| v.dirty);
            if !dirty || now < conn.next_paint {
                continue;
            }
            let Some(grid) = render::compose(&self.session, client) else {
                continue;
            };
            let bytes = render::paint(conn.shown.as_ref(), &grid);
            conn.send_bytes(Frame::Paint, &bytes);
            conn.shown = Some(grid);
            conn.next_paint = now + PAINT;
            if let Some(view) = self.session.views.get_mut(&client) {
                view.dirty = false;
            }
        }
    }

    fn accept(&mut self) {
        // Accept until the backlog is empty, so one tick drains it.
        loop {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    let euid = rustix::process::geteuid().as_raw();
                    match crate::socket::peer_uid(&stream) {
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
                    self.conns.push(Conn {
                        stream,
                        decoder: Decoder::default(),
                        out: Vec::new(),
                        role: None,
                        client: None,
                        shown: None,
                        next_paint: Instant::now(),
                        starved: false,
                        closing: false,
                        dead: false,
                    });
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => return,
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) => {
                    // Out of descriptors or similar: try again next tick.
                    log(&format!("accept: {e}"));
                    return;
                }
            }
        }
    }

    fn serve_conn(&mut self, index: usize, flags: PollFlags) {
        if flags.contains(PollFlags::OUT) {
            self.write_conn(index);
        }
        if flags.intersects(PollFlags::IN | PollFlags::HUP | PollFlags::ERR) {
            self.read_conn(index);
        }
    }

    fn write_conn(&mut self, index: usize) {
        let Some(conn) = self.conns.get_mut(index) else {
            return;
        };
        while !conn.out.is_empty() {
            match conn.stream.write(&conn.out) {
                Ok(0) => {
                    conn.dead = true;
                    break;
                }
                Ok(n) => {
                    conn.out.drain(..n);
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(_) => {
                    conn.dead = true;
                    break;
                }
            }
        }
        if conn.dead
            && let Some(client) = conn.client.take()
        {
            self.session.detach(client);
        }
    }

    fn read_conn(&mut self, index: usize) {
        let mut buffer = [0u8; 65536];
        let mut frames = Vec::new();
        let mut closed = false;
        {
            let Some(conn) = self.conns.get_mut(index) else {
                return;
            };
            loop {
                match conn.stream.read(&mut buffer) {
                    Ok(0) => {
                        closed = true;
                        break;
                    }
                    Ok(n) => {
                        conn.decoder.push(buffer.get(..n).unwrap_or_default());
                        loop {
                            match conn.decoder.frame() {
                                Ok(Some(frame)) => frames.push(frame),
                                Ok(None) => break,
                                Err(error) => {
                                    log(&format!("a client sent a bad frame: {error}"));
                                    closed = true;
                                    break;
                                }
                            }
                        }
                        if closed || frames.len() > 256 {
                            break;
                        }
                    }
                    Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                    Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                    Err(_) => {
                        closed = true;
                        break;
                    }
                }
            }
        }
        for frame in frames {
            self.frame(index, frame);
        }
        if closed && let Some(conn) = self.conns.get_mut(index) {
            conn.dead = true;
            if let Some(client) = conn.client.take() {
                self.session.detach(client);
            }
        }
    }

    fn frame(&mut self, index: usize, frame: Frame) {
        let Some(conn) = self.conns.get_mut(index) else {
            return;
        };
        let Some(role) = conn.role else {
            let Frame::Hello { protocol, role, .. } = frame else {
                conn.dead = true;
                return;
            };
            conn.send(&Frame::Hello {
                protocol: PROTOCOL,
                version: env!("CARGO_PKG_VERSION").into(),
                role,
            });
            if role == Role::Kill {
                conn.send(&Frame::Done { status: 0 });
                conn.closing = true;
                self.stop("stopped by fux kill-server".into());
                return;
            }
            if protocol != PROTOCOL {
                conn.send(&Frame::Exit(format!(
                    "the server speaks protocol {PROTOCOL} (fux {}); restart it with `fux kill-server`",
                    env!("CARGO_PKG_VERSION")
                )));
                conn.closing = true;
                return;
            }
            conn.role = Some(role);
            return;
        };
        match (role, frame) {
            (
                Role::Attach,
                Frame::Attach {
                    rows,
                    cols,
                    workspace,
                },
            ) if conn.client.is_none() => {
                match self.session.attach(rows, cols, workspace.as_deref()) {
                    Ok(client) => {
                        conn.client = Some(client);
                        conn.next_paint = Instant::now();
                    }
                    Err(error) => {
                        conn.send(&Frame::Exit(error));
                        conn.closing = true;
                    }
                }
            }
            (Role::Attach, Frame::Input(bytes)) => {
                if let Some(client) = conn.client {
                    self.session.input(client, &bytes);
                }
            }
            (Role::Attach, Frame::Resize { rows, cols }) => {
                if let Some(client) = conn.client {
                    conn.shown = None;
                    self.session.resize(client, rows, cols);
                }
            }
            (Role::Attach, Frame::Detach) => {
                if let Some(client) = conn.client.take() {
                    conn.send(&Frame::Exit("detached".into()));
                    conn.closing = true;
                    self.session.detach(client);
                }
            }
            (Role::Command, Frame::Command { argv, cwd, pane }) => {
                let ctx = Ctx {
                    client: None,
                    pane: pane.and_then(|p| crate::command::parse_pane(&p).ok()),
                    cwd: Some(PathBuf::from(cwd)).filter(|p| p.is_dir()),
                };
                let outcome = self.session.run(&argv, &ctx);
                let Some(conn) = self.conns.get_mut(index) else {
                    return;
                };
                if !outcome.stdout.is_empty() {
                    conn.send_bytes(Frame::Stdout, outcome.stdout.as_bytes());
                }
                if !outcome.stderr.is_empty() {
                    let mut stderr = outcome.stderr;
                    if !stderr.ends_with('\n') {
                        stderr.push('\n');
                    }
                    conn.send_bytes(Frame::Stderr, stderr.as_bytes());
                }
                conn.send(&Frame::Done {
                    status: outcome.status,
                });
                conn.closing = true;
            }
            _ => conn.dead = true,
        }
    }

    fn serve_pane(&mut self, id: PaneId, flags: PollFlags) {
        if flags.contains(PollFlags::OUT) {
            self.write_pane(id);
        }
        if flags.intersects(PollFlags::IN | PollFlags::HUP | PollFlags::ERR) {
            self.read_pane(id);
        }
    }

    fn write_pane(&mut self, id: PaneId) {
        let Some(pane) = self.session.panes.get_mut(&id) else {
            return;
        };
        let Some(child) = &pane.child else { return };
        while let Some(bytes) = pane.input.front() {
            match rustix::io::write(&child.master, bytes) {
                Ok(0) => break,
                Ok(n) => pane.input.advance(n),
                Err(rustix::io::Errno::INTR) => continue,
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
            match rustix::io::read(&child.master, &mut buffer) {
                Ok(0) => {
                    ended = true;
                    break;
                }
                Ok(n) => {
                    total += n;
                    self.session.output(id, buffer.get(..n).unwrap_or_default());
                }
                Err(rustix::io::Errno::AGAIN) => break,
                Err(rustix::io::Errno::INTR) => continue,
                // EIO: the slave side is closed; the program is gone.
                Err(_) => {
                    ended = true;
                    break;
                }
            }
        }
        if ended {
            self.reap();
            // A pane whose master reports the end but whose leader has not
            // exited stays; its hangup will come with the exit.
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
        for dying in due {
            let pid = dying.pid;
            // The master closes first: a dying writer can hold on to it.
            drop(dying.master);
            crate::process::finish(pid);
        }
    }
}

fn drain(stream: &mut UnixStream) {
    let mut buffer = [0u8; 256];
    while let Ok(n) = stream.read(&mut buffer) {
        if n == 0 {
            break;
        }
    }
}
