//! The clients: an attached one, which reads its terminal's keys, runs
//! its own bindings, and sends the server typed actions, the server
//! painting its terminal (or relaying the paints, if it did not take it);
//! a headless one; and the connection a program's requests go over.
use crate::act::Act;
use crate::decode::Input;
use crate::door::Link;
use crate::keymap::{Held, Keymap, Step};
use crate::keys::KeyPress;
use crate::protocol::{
    Attach, AttachedFrame, Context, Decoder, Frame, Hello, Mismatch, PROTOCOL, Reply, Request,
    Role, ServerFrame,
};
use crate::screens::{Effect, Screen};
use crate::socket::SocketPath;
use fuxix::poll::{Events as PollFlags, PollFd};
use fuxix::terminal::Termios;
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM, SIGWINCH};
use std::io::{ErrorKind, Read, Write};
use std::num::NonZeroU16;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Modes the attach client sets on the outer terminal: the alternate
/// screen, normal cursor and keypad keys, bracketed paste, focus events, no
/// autowrap; and it saves the terminal's title (xterm's title stack,
/// `outer::TITLE_PUSH`), which fux may set. Mouse reporting is the server's to turn on and off in its
/// paints, while a program asks for it (`render::mouse_level`). Public for fux-vt-compare's
/// `transparency`, which writes it to its terminal as this client does.
pub const ENTER: &str =
    "\x1b[22;0t\x1b[?1049h\x1b[?1l\x1b>\x1b[?2004h\x1b[?1004h\x1b[?7l\x1b[H\x1b[2J";
/// And turns them off again, the title saved restored
/// (`outer::TITLE_POP`), with what the server may have turned on in
/// its paints: mouse reporting (`render::MOUSE_OFF`), colour-scheme
/// reports (mode 2031, `outer`), and the kitty
/// keyboard flags it pushed, popped on the alternate screen they were
/// pushed on, as the kitty spec says to leave. The client sends this
/// however the attachment ends, the server gone or not; a terminal that
/// never had them ignores both, and a pop of a stack the server pushed
/// nothing on, the alternate screen's own, empties it.
const LEAVE: &str = "\x1b[23;0t\x1b[?2026l\x1b[?1003l\x1b[?1002l\x1b[?1000l\x1b[?1006l\x1b[?1004l\x1b[?2004l\x1b[?2031l\x1b[<u\x1b[?7h\x1b[0m\x1b[0 q\x1b[?25h\x1b[?1049l";

/// Why a client could not reach the server, start one, or go on.
#[derive(Debug)]
pub enum Error {
    Socket(crate::socket::Error),
    Connect {
        path: PathBuf,
        source: std::io::Error,
    },
    /// The server ended the connection, saying why.
    Refused(String),
    /// The server speaks another protocol.
    Mismatch(crate::protocol::Mismatch),
    /// The server closed the connection before it answered.
    NoAnswer,
    Timeout,
    /// The server closed the connection before the command was done.
    Closed,
    Protocol(crate::protocol::Error),
    Write(std::io::Error),
    Read(std::io::Error),
    // Starting a server.
    Exe(std::io::Error),
    Log(std::io::Error),
    Spawn(std::io::Error),
    Exited {
        status: std::process::ExitStatus,
        log: PathBuf,
    },
    NotStarted {
        log: PathBuf,
    },
    // Attaching this terminal.
    NotATerminal,
    Modes(fuxix::Errno),
    RawMode(fuxix::Errno),
    /// The pipes that carry signals could not be set up.
    Signals(std::io::Error),
    Poll(fuxix::Errno),
    ReadTerminal(fuxix::Errno),
    WriteTerminal(std::io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Socket(error) => error.fmt(f),
            // A socket left by a server that is gone.
            Error::Connect { path, source }
                if source.kind() == std::io::ErrorKind::ConnectionRefused =>
            {
                write!(
                    f,
                    "no fux server is running at {} (its socket is left from one that stopped); {}",
                    path.display(),
                    crate::socket::start_hint(path)
                )
            }
            Error::Connect { path, source } => {
                write!(f, "connecting to {}: {source}", path.display())
            }
            Error::Refused(reason) => f.write_str(reason),
            Error::Mismatch(mismatch) => mismatch.fmt(f),
            Error::NoAnswer => f.write_str("the server did not answer"),
            Error::Timeout => f.write_str("the server did not answer in time"),
            Error::Closed => f.write_str("the server closed the connection"),
            Error::Protocol(error) => error.fmt(f),
            Error::Write(error) => write!(f, "writing to the server: {error}"),
            Error::Read(error) => write!(f, "reading from the server: {error}"),
            Error::Exe(error) => write!(f, "finding the fux binary: {error}"),
            Error::Log(error) => write!(f, "the log file: {error}"),
            Error::Spawn(error) => write!(f, "starting a server: {error}"),
            Error::Exited { status, log } => {
                write!(f, "the server exited ({status}); see {}", log.display())
            }
            Error::NotStarted { log } => write!(
                f,
                "the server did not start within 2 s; see {}",
                log.display()
            ),
            Error::NotATerminal => f.write_str("fux attach needs a terminal on stdin"),
            Error::Modes(errno) => write!(f, "reading terminal modes: {errno}"),
            Error::RawMode(errno) => write!(f, "setting raw mode: {errno}"),
            Error::Signals(error) => error.fmt(f),
            Error::Poll(errno) => write!(f, "poll: {errno}"),
            Error::ReadTerminal(errno) => write!(f, "reading the terminal: {errno}"),
            Error::WriteTerminal(error) => write!(f, "writing the terminal: {error}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Socket(error) => Some(error),
            Error::Protocol(error) => Some(error),
            Error::Mismatch(mismatch) => Some(mismatch),
            Error::Connect { source: error, .. }
            | Error::Write(error)
            | Error::Read(error)
            | Error::Exe(error)
            | Error::Log(error)
            | Error::Spawn(error)
            | Error::Signals(error)
            | Error::WriteTerminal(error) => Some(error),
            Error::Modes(errno)
            | Error::RawMode(errno)
            | Error::Poll(errno)
            | Error::ReadTerminal(errno) => Some(errno),
            Error::Refused(_)
            | Error::NoAnswer
            | Error::Timeout
            | Error::Closed
            | Error::Exited { .. }
            | Error::NotStarted { .. }
            | Error::NotATerminal => None,
        }
    }
}

impl From<crate::socket::Error> for Error {
    fn from(error: crate::socket::Error) -> Error {
        Error::Socket(error)
    }
}

impl From<crate::protocol::Error> for Error {
    fn from(error: crate::protocol::Error) -> Error {
        Error::Protocol(error)
    }
}

/// The terminal state to restore, shared with the panic hook.
static SAVED: Mutex<Option<Termios>> = Mutex::new(None);
fn restore() {
    let saved = SAVED.lock().ok().and_then(|mut s| s.take());
    if let Some(termios) = saved {
        let mut out = std::io::stdout();
        let _ = out.write_all(LEAVE.as_bytes());
        let _ = out.flush();
        let _ = fuxix::terminal::set_attributes(std::io::stdin(), &termios);
    }
}

/// The stream to the server at `socket`: its socket, once it is checked to
/// be served by this user; or its door (`crate::door`), where the system
/// denies sockets (a sandbox) or the server serves its door alone.
fn open(socket: &SocketPath) -> Result<Link, Error> {
    let door = crate::door::path(socket.path());
    let by_door = || {
        crate::door::knock(socket.path()).map_err(|source| Error::Connect {
            path: door.clone(),
            source,
        })
    };
    match crate::socket::check_client_socket(socket) {
        Err(crate::socket::Error::NoServer(_)) if door.exists() => return by_door(),
        Err(error) => return Err(error.into()),
        Ok(()) => {}
    }
    let failed = |source| Error::Connect {
        path: socket.path().to_owned(),
        source,
    };
    let stream = match UnixStream::connect(socket.path()) {
        Ok(stream) => stream,
        Err(e) if e.kind() == ErrorKind::PermissionDenied && door.exists() => return by_door(),
        Err(source) => return Err(failed(source)),
    };
    // Its server is this user's, whoever made the path's directories.
    let euid = fuxix::process::geteuid();
    match fuxix::socket::peer_uid(&stream) {
        Ok(uid) if uid == euid => Link::socket(stream),
        Ok(uid) => Err(std::io::Error::other(format!(
            "its server runs as uid {uid}, not this user's ({euid})"
        ))),
        Err(errno) => Err(errno.into()),
    }
    .map_err(failed)
}

fn connect(socket: &SocketPath, role: Role) -> Result<(Link, Decoder), Error> {
    let mut stream = open(socket)?;
    let ours = Hello {
        protocol: PROTOCOL,
        version: env!("CARGO_PKG_VERSION"),
        role,
    };
    let sent = send(&mut stream, &ours);
    let mut decoder = Decoder::default();
    let mut buffer = vec![0u8; 64 * 1024];
    // A server that refuses a connection says why and closes it, maybe
    // before the Hello is written: its reason is still there to read.
    let answer = read_frame(
        &mut stream,
        &mut decoder,
        &mut buffer,
        Some(Duration::from_secs(5)),
    );
    greeted(&ours, answer, sent)?;
    Ok((stream, decoder))
}

/// Whether the server's answer to our Hello, `ours`, and sending it, let
/// the client go on: a refusal, which a server may make before reading the
/// Hello, wins, and a server of another protocol is refused here, before
/// anything it would not understand is sent.
fn greeted(
    ours: &Hello,
    answer: Result<Option<ServerFrame>, Error>,
    sent: Result<(), Error>,
) -> Result<(), Error> {
    match (answer, sent) {
        (Ok(Some(ServerFrame::Exit(reason))), _) => Err(Error::Refused(reason.to_owned())),
        // A server of another protocol says so, though it hung up before
        // reading the Hello.
        (Ok(Some(ServerFrame::Hello(theirs))), sent) => {
            Mismatch::between(ours, &theirs).map_err(Error::Mismatch)?;
            sent
        }
        (_, Err(error)) => Err(error),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Ok(())) => Err(Error::NoAnswer),
    }
}

fn send<'a>(stream: &mut Link, frame: &impl Frame<'a>) -> Result<(), Error> {
    let bytes = frame.encode()?;
    stream.write_all(&bytes).map_err(Error::Write)
}

fn read_frame<'d>(
    stream: &mut Link,
    decoder: &'d mut Decoder,
    buffer: &mut [u8],
    timeout: Option<Duration>,
) -> Result<Option<ServerFrame<'d>>, Error> {
    stream.set_read_timeout(timeout);
    while !decoder.ready()? {
        match stream.read(buffer) {
            Ok(0) => return Ok(None),
            Ok(n) => decoder.push(buffer.get(..n).unwrap_or_default()),
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                return Err(Error::Timeout);
            }
            Err(e) => return Err(Error::Read(e)),
        }
    }
    Ok(decoder.frame()?)
}

/// A connection to the server for requests: each sent whole, and answered
/// before the next. One per run of a program, made when its first request
/// is.
pub struct Requests {
    stream: Link,
    decoder: Decoder,
    buffer: Vec<u8>,
    /// The pieces of a reply too large for one frame, so far.
    parts: Vec<u8>,
}

impl Requests {
    pub fn open(socket: &SocketPath) -> Result<Requests, Error> {
        let (stream, decoder) = connect(socket, Role::Command)?;
        Ok(Requests {
            stream,
            decoder,
            buffer: vec![0u8; 64 * 1024],
            parts: Vec::new(),
        })
    }

    /// Sends `request`, and waits for its reply: what the server streams
    /// before it (`events`) goes to `out` as it comes.
    pub fn ask(&mut self, request: &Request, out: &mut dyn Write) -> Result<Reply, Error> {
        self.send(request)?;
        loop {
            if let Some(reply) = self.take(out)? {
                return Ok(reply);
            }
            match self.stream.read(&mut self.buffer) {
                Ok(0) => return Err(Error::Closed),
                Ok(n) => self.decoder.push(self.buffer.get(..n).unwrap_or_default()),
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => return Err(Error::Read(e)),
            }
        }
    }

    /// Sends `request`, not waiting for its reply.
    pub fn send(&mut self, request: &Request) -> Result<(), Error> {
        self.stream.set_read_timeout(None);
        send(&mut self.stream, request)
    }

    /// The reply read already, if it is whole: what comes before it goes to
    /// `out`.
    pub fn take(&mut self, out: &mut dyn Write) -> Result<Option<Reply>, Error> {
        while let Some(frame) = self.decoder.frame()? {
            match frame {
                ServerFrame::Stdout(bytes) | ServerFrame::Stderr(bytes) => {
                    let _ = out.write_all(bytes).and_then(|()| out.flush());
                }
                ServerFrame::Reply(reply) => return Ok(Some(*reply)),
                ServerFrame::Part(bytes) => self.parts.extend_from_slice(bytes),
                ServerFrame::PartsEnd => {
                    let parts = std::mem::take(&mut self.parts);
                    let reply = crate::wire::Reader::new(&parts).whole::<Reply>();
                    return reply
                        .map(Some)
                        .map_err(|e| Error::Protocol(crate::protocol::Error::Wire(e)));
                }
                ServerFrame::Exit(reason) => return Err(Error::Refused(reason.to_owned())),
                ServerFrame::Hello(_)
                | ServerFrame::Paint(_)
                | ServerFrame::Done { .. }
                | ServerFrame::Terminal { .. }
                | ServerFrame::Ended
                | ServerFrame::Run(_)
                | ServerFrame::Keys(_) => {}
            }
        }
        Ok(None)
    }

    /// A copy of its socket, to shut down from another thread.
    pub fn socket_copy(&self) -> Option<UnixStream> {
        self.stream.socket_copy()
    }
}

/// Where requests from this process come from: its directory, its pane
/// and that pane's server, its environment.
pub fn context() -> Context {
    let var = |name| std::env::var(name).ok().filter(|v: &String| !v.is_empty());
    Context {
        cwd: std::env::current_dir()
            .map(|d| d.to_string_lossy().into_owned())
            .unwrap_or_default(),
        pane: var("FUX_PANE"),
        server: var("FUX_SERVER"),
        env: environment(),
        client: None,
        restoring: false,
    }
}

/// This process's environment, as far as it is UTF-8: the server gives
/// panes started for it what `update-environment` names of it.
pub fn environment() -> Vec<(String, String)> {
    std::env::vars_os()
        .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
        .collect()
}

/// Stops the server, whatever fux version it is.
pub fn kill_server(socket: &SocketPath) -> Result<(), Error> {
    let (mut stream, mut decoder) = connect(socket, Role::Kill)?;
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        match read_frame(
            &mut stream,
            &mut decoder,
            &mut buffer,
            Some(Duration::from_secs(5)),
        ) {
            Ok(Some(ServerFrame::Done { .. })) | Ok(None) => return Ok(()),
            Ok(Some(_)) => {}
            Err(error) => return Err(error),
        }
    }
}

/// What answers on a socket: nothing, or a server, its protocol and
/// version.
pub enum Probe {
    Absent,
    /// A socket left by a server that stopped.
    Left,
    Server {
        protocol: u32,
        version: String,
    },
}

/// Who answers on `socket`, asking only for its `Hello`, which every
/// version answers.
pub fn probe(socket: &SocketPath) -> Result<Probe, Error> {
    let door = crate::door::path(socket.path());
    match crate::socket::check_client_socket(socket) {
        Err(crate::socket::Error::NoServer(_)) if !door.exists() => return Ok(Probe::Absent),
        Err(crate::socket::Error::NoServer(_)) | Ok(()) => {}
        Err(error) => return Err(error.into()),
    }
    let mut stream = match open(socket) {
        Ok(stream) => stream,
        // A socket or a door left by a server that is gone.
        Err(Error::Connect { source, .. })
            if matches!(
                source.kind(),
                ErrorKind::ConnectionRefused | ErrorKind::TimedOut
            ) =>
        {
            return Ok(Probe::Left);
        }
        Err(error) => return Err(error),
    };
    let ours = Hello {
        protocol: PROTOCOL,
        version: env!("CARGO_PKG_VERSION"),
        role: Role::Command,
    };
    send(&mut stream, &ours)?;
    let mut decoder = Decoder::default();
    let mut buffer = vec![0u8; 4096];
    let wait = Some(Duration::from_secs(5));
    match read_frame(&mut stream, &mut decoder, &mut buffer, wait)? {
        Some(ServerFrame::Hello(theirs)) => Ok(Probe::Server {
            protocol: theirs.protocol,
            version: theirs.version.to_owned(),
        }),
        Some(ServerFrame::Exit(reason)) => Err(Error::Refused(reason.to_owned())),
        Some(_) | None => Err(Error::NoAnswer),
    }
}

/// Whether a server answers on `socket`: its socket, or, where sockets are
/// denied, its door.
pub fn answers(socket: &SocketPath) -> bool {
    UnixStream::connect(socket.path()).is_ok() || crate::door::served(socket.path())
}

/// Starts a server in the background, in a new session with its output in
/// `fux.log` beside the socket, and waits for it to answer.
pub fn start_server(socket: &SocketPath) -> Result<(), Error> {
    let exe = std::env::current_exe().map_err(Error::Exe)?;
    crate::socket::prepare_directory(socket)?;
    let log = socket.directory().join("fux.log");
    let output = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)
        .map_err(Error::Log)?;
    let mut command = std::process::Command::new(exe);
    command
        .arg(crate::SERVE)
        .arg(socket.path())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(output);
    let mut child = command.spawn().map_err(Error::Spawn)?;
    let deadline = crate::after(Instant::now(), Duration::from_secs(2));
    loop {
        if answers(socket) {
            // The child is left to run; it is not ours to wait for, and
            // dropping a `Child` neither waits for nor kills it.
            drop(child);
            return Ok(());
        }
        if let Ok(Some(status)) = child.try_wait() {
            // It lost a race to another server, or failed: connect to the
            // winner if there is one.
            if answers(socket) {
                return Ok(());
            }
            return Err(Error::Exited { status, log });
        }
        if Instant::now() > deadline {
            return Err(Error::NotStarted { log });
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The terminal's rows and columns; 24 by 80 if it does not say.
fn window_size() -> (NonZeroU16, NonZeroU16) {
    let fallback = |n: u16| NonZeroU16::MIN.saturating_add(n.saturating_sub(1));
    fuxix::terminal::window_size(std::io::stdout())
        .map_or((fallback(24), fallback(80)), |s| (s.rows, s.cols))
}

/// How a client attaches: to which workspace, whether it only watches, its
/// keys, its config run already, and what was wrong with that config.
pub struct Attaching {
    pub workspace: Option<String>,
    pub read_only: bool,
    pub keymap: Keymap,
    pub problem: Option<String>,
}

/// The words that open one of a client's screens: a program that has one
/// may open a screen the keys typed after its key go to, so they wait for
/// it; any other runs while the keys go on.
const OPENS: [&str; 10] = [
    "menu",
    "choose-tab",
    "choose-workspace",
    "choose-tab-for",
    "choose-workspace-for",
    "choose-pane",
    "rename-prompt",
    "confirm-close",
    "command-prompt",
    "copy-mode",
];

/// Whether a key's program may open a screen: a word that opens one, other
/// than the text `str` takes.
fn may_open(code: &str) -> bool {
    let Ok(tokens) = fuxth::token::source("key", code) else {
        return false;
    };
    let mut quoted = false;
    tokens.iter().any(|token| {
        let opens = !quoted && OPENS.contains(&&*token.text);
        quoted = &*token.text == "str" && !quoted;
        opens
    })
}

/// The most steps a key's or a screen's program takes: one that loops
/// without end fails rather than spinning the client.
const KEY_STEPS: u64 = 10_000_000;

/// A program a client's key or prompt runs, on its worker, in order.
struct Job {
    code: String,
    keymap: Keymap,
}

/// What a job came to: the keymap, if its program changed it (`bind`
/// typed at the prompt), what to say of it, and the screen it opened.
struct Ran {
    keymap: Option<Keymap>,
    notice: Option<(String, bool)>,
    opened: Option<Screen>,
}

/// An attached client's own keys: its keymap, what its keys are in the
/// middle of, the screen it has open, the decoder its terminal's bytes go
/// through, and the worker that runs the programs its bindings and screens
/// give, one after another, as the client. What each takes, it turns into
/// the actions to send the server.
///
/// Every mode is the client's (`crate::screens`), changed here at the key
/// that changes it. A program it runs may open a screen, so the keys typed
/// while one runs wait for it, and go to the screen it opened, if any.
pub struct Keyboard {
    keymap: Keymap,
    held: Held,
    screen: Screen,
    read_only: bool,
    decoder: crate::decode::Decoder,
    jobs: std::sync::mpsc::Sender<Job>,
    told: std::sync::mpsc::Receiver<Ran>,
    wake: std::io::PipeReader,
    /// Programs sent to the worker and not yet done, in order, each with
    /// whether it may open a screen.
    running: std::collections::VecDeque<bool>,
    /// What stops the program the worker runs (the prefix, then Esc).
    stop: crate::script::Stop,
    /// What was typed while a program ran, in order.
    waiting: std::collections::VecDeque<Input>,
}

impl Keyboard {
    /// The keyboard of the client attached as `me` on `socket`, with
    /// `keymap`; `read_only` if it only watches.
    pub fn new(
        socket: &SocketPath,
        me: crate::id::ClientId,
        keymap: Keymap,
        read_only: bool,
    ) -> Result<Keyboard, Error> {
        let (jobs, todo) = std::sync::mpsc::channel::<Job>();
        let (tell, told) = std::sync::mpsc::channel::<Ran>();
        let (wake, mut poke) = std::io::pipe().map_err(Error::Signals)?;
        fuxix::io::set_nonblocking(&wake, true).map_err(|e| Error::Signals(e.into()))?;
        let path = socket.path().to_string_lossy().into_owned();
        let stop = crate::script::Stop::default();
        let stops = stop.clone();
        let spawned = std::thread::Builder::new()
            .name("keys".into())
            .spawn(move || {
                let chosen = crate::script::Chosen::Path(path);
                let mut server = crate::script::Server::stoppable(chosen, stops.clone());
                for job in todo {
                    stops.reset();
                    let ran = run_job(&mut server, me, job, &stops);
                    if tell.send(ran).is_err() {
                        break;
                    }
                    let _ = poke.write_all(b"!");
                }
            });
        spawned.map_err(Error::Signals)?;
        let mut decoder = crate::decode::Decoder::default();
        // The server asks the terminal about itself as the client attaches.
        decoder.expect(Instant::now());
        Ok(Keyboard {
            keymap,
            held: Held::default(),
            screen: Screen::None,
            read_only,
            decoder,
            jobs,
            told,
            wake,
            running: std::collections::VecDeque::new(),
            stop,
            waiting: std::collections::VecDeque::new(),
        })
    }

    /// Whether nothing waits: no program running, no keys held for one.
    pub fn idle(&self) -> bool {
        self.running.is_empty() && self.waiting.is_empty()
    }

    /// Readable when a program it ran is done (`Keyboard::ran`).
    pub fn wake(&self) -> &std::io::PipeReader {
        &self.wake
    }

    /// When what its terminal sent last is to be taken as it is: a lone
    /// Escape, or an answer cut short (`Keyboard::timeout`).
    pub fn deadline(&self) -> Option<Instant> {
        self.decoder.deadline()
    }

    /// Whether the keys typed wait: a program that may open a screen they
    /// go to runs, until it opens one or ends.
    fn holding(&self) -> bool {
        self.running.iter().any(|opens| *opens)
    }

    /// Stops the program that runs, if one does, saying so.
    fn stop(&mut self, acts: &mut Vec<Act>) {
        if self.running.is_empty() {
            return;
        }
        self.stop.stop();
        let text = "stopping…".to_owned();
        acts.push(Act::Notice { text, error: false });
    }

    /// Bytes from its terminal, read at `now`.
    pub fn bytes(&mut self, bytes: &[u8], now: Instant) -> Vec<Act> {
        let mut inputs = Vec::new();
        self.decoder.expire(now);
        self.decoder.bytes(bytes, &mut inputs);
        self.decoder.mark(now);
        self.inputs(inputs)
    }

    /// A deadline passed (`Keyboard::deadline`): what waited is taken.
    pub fn timeout(&mut self) -> Vec<Act> {
        let mut inputs = Vec::new();
        if self
            .decoder
            .deadline()
            .is_some_and(|at| at <= Instant::now())
        {
            self.decoder.timeout(&mut inputs);
        }
        self.inputs(inputs)
    }

    fn inputs(&mut self, inputs: Vec<Input>) -> Vec<Act> {
        self.waiting.extend(inputs);
        let mut acts = Vec::new();
        // The prefix, then Esc, typed while keys wait for a program, stops
        // it, the two keys taken for that.
        if self.holding() && self.held_stop() {
            self.waiting.pop_back();
            self.waiting.pop_back();
            self.stop(&mut acts);
        }
        acts.extend(self.release());
        acts
    }

    /// Whether the keys waiting end with the prefix then Esc.
    fn held_stop(&self) -> bool {
        let mut last = self.waiting.iter().rev();
        let is = |input: Option<&Input>, key: KeyPress| matches!(input, Some(Input::Key(stroke)) if stroke.press == key);
        let escape = KeyPress::new(crate::keys::Key::Escape, crate::keys::Modifiers::NONE);
        is(last.next(), escape) && is(last.next(), self.keymap.prefix)
    }

    /// What waits, taken unless a program holds it.
    fn release(&mut self) -> Vec<Act> {
        let mut acts = Vec::new();
        while !self.holding() {
            let Some(input) = self.waiting.pop_front() else {
                break;
            };
            self.input(input, &mut acts);
        }
        acts
    }

    /// A frame from the server: copy mode it ended, a program to run (a
    /// screen asked of it from elsewhere), keys to take as typed
    /// (`send-keys` to it).
    pub fn frame(&mut self, frame: ServerFrame<'_>) -> Vec<Act> {
        match frame {
            ServerFrame::Ended => self.screen.ended(),
            ServerFrame::Run(code) => self.run(code.to_owned()),
            ServerFrame::Keys(typed) => self.typed(typed),
            ServerFrame::Hello(_)
            | ServerFrame::Paint(_)
            | ServerFrame::Exit(_)
            | ServerFrame::Stdout(_)
            | ServerFrame::Stderr(_)
            | ServerFrame::Done { .. }
            | ServerFrame::Terminal { .. }
            | ServerFrame::Reply(_)
            | ServerFrame::Part(_)
            | ServerFrame::PartsEnd => {}
        }
        self.release()
    }

    /// What the programs it ran came to, those done: a screen one opened
    /// opens, what one said shows, and the keys that waited go on.
    pub fn ran(&mut self) -> Vec<Act> {
        crate::drain(&self.wake);
        let mut acts = Vec::new();
        while let Ok(ran) = self.told.try_recv() {
            self.running.pop_front();
            if let Some(keymap) = ran.keymap {
                self.keymap = keymap;
            }
            if let Some((text, error)) = ran.notice {
                acts.push(Act::Notice { text, error });
            }
            if let Some(screen) = ran.opened {
                self.open(screen, &mut acts);
            }
        }
        acts.extend(self.release());
        acts
    }

    /// Opens `screen`, in place of what was open. A client that only
    /// watches opens none: it says why.
    fn open(&mut self, screen: Screen, acts: &mut Vec<Act>) {
        if self.read_only {
            let text = "watching only: no menus, prompts or copy mode here".to_owned();
            acts.push(Act::Notice { text, error: true });
            return;
        }
        self.held = Held::Normal;
        if let Screen::Copy(_) = self.screen {
            acts.push(Act::Copy(crate::act::CopyAct::Leave));
        }
        acts.extend(screen.opened());
        self.screen = screen;
    }

    /// A key typed: to the screen open, else through the bindings. Esc
    /// right after the prefix also stops a program its key ran, if one runs.
    fn press(&mut self, stroke: crate::keys::Keystroke, acts: &mut Vec<Act>) {
        let escape = KeyPress::new(crate::keys::Key::Escape, crate::keys::Modifiers::NONE);
        let column = matches!(&self.held, Held::Column { path, .. } if path.is_empty());
        if column && stroke.press == escape {
            self.stop(acts);
        }
        if self.screen != Screen::None {
            for effect in self.screen.key(stroke.press) {
                match effect {
                    Effect::Send(act) => acts.push(act),
                    Effect::Run(code) => self.run(code),
                }
            }
            return;
        }
        for step in self.held.press(&self.keymap, stroke, self.read_only) {
            match step {
                Step::Send(act) => acts.push(act),
                Step::Run(code) => self.run(code),
            }
        }
    }

    fn run(&mut self, code: String) {
        let keymap = self.keymap.clone();
        let opens = may_open(&code);
        if self.jobs.send(Job { code, keymap }).is_ok() {
            self.running.push_back(opens);
        }
    }

    /// What the terminal sent, decoded.
    fn input(&mut self, input: Input, acts: &mut Vec<Act>) {
        match input {
            Input::Key(stroke) => self.press(stroke, acts),
            Input::Reply(reply) => acts.push(Act::Reply(reply)),
            Input::Paste(_)
            | Input::PasteTooLong
            | Input::FocusIn
            | Input::FocusOut
            | Input::Mouse(_)
                if self.read_only => {}
            Input::Paste(text) if self.screen != Screen::None => {
                for effect in self.screen.paste(&text) {
                    match effect {
                        Effect::Send(act) => acts.push(act),
                        Effect::Run(code) => self.run(code),
                    }
                }
            }
            Input::Paste(text) => {
                // A paste is the pane's: the column or a repeat mode ends.
                if self.held != Held::Normal {
                    self.held = Held::Normal;
                    acts.push(Act::Hide);
                }
                acts.push(Act::Paste(text));
            }
            Input::PasteTooLong => {
                let limit = crate::decode::PASTE_LIMIT / 1024;
                let text = format!("paste exceeds {limit} KiB; discarded");
                acts.push(Act::Notice { text, error: true });
            }
            Input::FocusIn => acts.push(Act::Focus(true)),
            Input::FocusOut => acts.push(Act::Focus(false)),
            // The mouse is a program's, while the client shows none of its
            // own screens.
            Input::Mouse(_) if self.screen != Screen::None => {}
            Input::Mouse(event) => acts.push(Act::Mouse(event)),
        }
    }

    /// Keys the server says were typed here (`send-keys` to this client),
    /// in turn with what the terminal sent.
    fn typed(&mut self, inputs: Vec<crate::command::Input>) {
        for input in inputs {
            match input {
                crate::command::Input::Key(press) => {
                    self.waiting.push_back(Input::Key(press.into()))
                }
                crate::command::Input::Text(text) => {
                    let keys = text
                        .chars()
                        .map(|c| Input::Key(crate::keys::KeyPress::char(c).into()));
                    self.waiting.extend(keys);
                }
                crate::command::Input::Paste(text) => self.waiting.push_back(Input::Paste(text)),
            }
        }
    }
}

/// Runs a key's or a screen's program as client `me`.
fn run_job(
    server: &mut crate::script::Server,
    me: crate::id::ClientId,
    job: Job,
    stop: &crate::script::Stop,
) -> Ran {
    let Job { code, keymap } = job;
    let mut changed = keymap.clone();
    let mut out = Vec::new();
    let mut context = context();
    context.client = Some(me);
    let mut asks = crate::script::AsClient {
        asks: server,
        context: context.clone(),
        opened: None,
    };
    let result = (|| {
        let tokens = fuxth::token::source("key", &code)?;
        let mut machine = crate::script::compile(&tokens)?;
        let mut host = crate::script::Host {
            context,
            keymap: Some(&mut changed),
            last_bound: None,
            out: &mut out,
            settle: false,
        };
        crate::script::run_capped(
            &mut machine,
            &mut host,
            &mut asks,
            Some(KEY_STEPS),
            Some(stop),
        )
    })();
    let notice = match result {
        // Stopped as asked (the prefix, then Esc): no failure of its own.
        Err(_) if stop.stopped() => Some(("stopped".to_owned(), false)),
        Err(failed) => Some((failed.to_string(), true)),
        Ok(ended) => {
            let mut said = String::from_utf8_lossy(&out).into_owned();
            for value in &ended.stack {
                said.push_str(&crate::script::display(value));
                said.push('\n');
            }
            let said: Vec<&str> = said.lines().filter(|l| !l.trim().is_empty()).collect();
            (!said.is_empty()).then(|| (said.join("  "), false))
        }
    };
    Ran {
        keymap: (changed != keymap).then_some(changed),
        notice,
        opened: asks.opened,
    }
}

/// Attaches this terminal to the server until detach or the server's end.
pub fn attach(socket: &SocketPath, attaching: Attaching) -> Result<(), Error> {
    let stdin = std::io::stdin();
    if !std::io::IsTerminal::is_terminal(&stdin) {
        return Err(Error::NotATerminal);
    }
    let (mut stream, mut decoder) = connect(socket, Role::Attach)?;
    let (rows, cols) = window_size();
    let raw = Raw::enter(Signals::new()?, &stdin)?;
    // The terminal goes with the `Attach`, now that it is set up: if the
    // server takes it, it writes the paints there itself. The keys are
    // this client's to read either way.
    let attach = Attach {
        rows,
        cols,
        workspace: attaching.workspace.as_deref(),
        read_only: attaching.read_only,
        env: environment(),
    };
    let result = send_with_terminal(&mut stream, &attach, &stdin).and_then(|()| {
        attached(
            socket,
            &mut stream,
            &mut decoder,
            &raw.signals,
            &attaching,
            true,
        )
    });
    drop(raw);
    // Why the client detached, on a terminal that may be gone: a hung-up
    // one fails the write, and the detach is no less done.
    let _ = writeln!(std::io::stdout(), "[{}]", result?);
    Ok(())
}

/// Attaches a client with no terminal, of `size`, until the server ends it
/// or a signal detaches it: its id goes to stdout as it attaches. It reads
/// no keys, but takes those sent it (`send-keys` to a client) as typed.
pub fn headless(
    socket: &SocketPath,
    attaching: Attaching,
    size: fux_vt::Size,
) -> Result<(), Error> {
    let signals = Signals::new()?;
    let (mut stream, mut decoder) = connect(socket, Role::Headless)?;
    let fallback = NonZeroU16::MIN;
    let attach = Attach {
        rows: NonZeroU16::new(size.rows()).unwrap_or(fallback),
        cols: NonZeroU16::new(size.cols()).unwrap_or(fallback),
        workspace: None,
        read_only: false,
        env: environment(),
    };
    send(&mut stream, &attach)?;
    let reason = attached(
        socket,
        &mut stream,
        &mut decoder,
        &signals,
        &attaching,
        false,
    )?;
    say(&reason)
}

/// Says why a client detached, on its stdout.
fn say(reason: &str) -> Result<(), Error> {
    let _ = writeln!(std::io::stdout(), "[{reason}]");
    Ok(())
}

/// Why a client detached when a signal told it to.
const STOPPED: &str = "detached by a signal";

/// The signals an attached client answers: a new size, and those that
/// detach it. Made before the terminal goes raw, so that no wait in raw
/// mode can miss one.
struct Signals {
    winch: std::io::PipeReader,
    stops: std::io::PipeReader,
}

impl Signals {
    fn new() -> Result<Signals, Error> {
        Ok(Signals {
            winch: crate::signal_pipe(&[SIGWINCH]).map_err(Error::Signals)?,
            stops: crate::signal_pipe(&[SIGTERM, SIGHUP, SIGINT]).map_err(Error::Signals)?,
        })
    }
}

/// The terminal in raw mode on the alternate screen, which only the
/// signals that end it can make, and which puts the terminal back as it
/// was once dropped, however the attachment ends.
struct Raw {
    signals: Signals,
}

impl Raw {
    fn enter(signals: Signals, stdin: &std::io::Stdin) -> Result<Raw, Error> {
        let original = fuxix::terminal::attributes(stdin).map_err(Error::Modes)?;
        let mut raw = original.clone();
        raw.make_raw();
        if let Ok(mut saved) = SAVED.lock() {
            *saved = Some(original);
        }
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore();
            previous(info);
        }));
        // Made before raw mode, so that failing to set it restores too.
        let entered = Raw { signals };
        fuxix::terminal::set_attributes(stdin, &raw).map_err(Error::RawMode)?;
        let mut stdout = std::io::stdout();
        let _ = stdout.write_all(ENTER.as_bytes());
        let _ = stdout.flush();
        Ok(entered)
    }
}

impl Drop for Raw {
    fn drop(&mut self) {
        restore();
    }
}

/// Sends `frame` with this process's terminal attached, its first bytes
/// carrying it.
fn send_with_terminal<'a>(
    stream: &mut Link,
    frame: &impl Frame<'a>,
    terminal: impl std::os::fd::AsFd,
) -> Result<(), Error> {
    // Through a door, a FIFO, no descriptor can go: the server relays
    // the paints instead.
    if !stream.is_socket() {
        return send(stream, frame);
    }
    let bytes = frame.encode()?;
    let sent = loop {
        match fuxix::socket::send_with_fd(stream.output(), &bytes, &terminal) {
            Ok(n) => break n,
            Err(fuxix::Errno::INTR) => continue,
            Err(errno) => return Err(Error::Write(errno.into())),
        }
    };
    stream
        .write_all(bytes.get(sent..).unwrap_or_default())
        .map_err(Error::Write)
}

/// Holds the attachment until the server ends it, and says why: reads the
/// terminal's keys, with `terminal`, and runs them through the client's
/// keys; writes the paints the server relays; passes on the terminal's new
/// size, and a signal that detaches. Until the server says the client's id
/// (its first `Stdout`), an `Exit` is a refusal.
fn attached(
    socket: &SocketPath,
    stream: &mut Link,
    decoder: &mut Decoder,
    signals: &Signals,
    attaching: &Attaching,
    terminal: bool,
) -> Result<String, Error> {
    let Signals { winch, stops } = signals;
    stream.set_read_timeout(None);
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let mut buffer = vec![0u8; 64 * 1024];
    let mut keyboard: Option<Keyboard> = None;
    let mut acts = Vec::new();
    const STREAM: usize = 0;
    const WINCH: usize = 1;
    const STOPS: usize = 2;
    const WAKE: usize = 3;
    const TERMINAL: usize = 4;
    loop {
        // Every whole frame the server sent.
        while let Some(frame) = decoder.frame()? {
            match (frame, &mut keyboard) {
                (ServerFrame::Paint(bytes), _) => {
                    if terminal {
                        stdout.write_all(bytes).map_err(Error::WriteTerminal)?;
                    }
                }
                (ServerFrame::Exit(reason), None) => return Err(Error::Refused(reason.to_owned())),
                (ServerFrame::Exit(reason), Some(_)) => return Ok(reason.to_owned()),
                (ServerFrame::Stdout(said), None) => {
                    let said = String::from_utf8_lossy(said);
                    let me = said.trim().parse().map_err(|_| Error::NoAnswer)?;
                    if !terminal {
                        let _ = stdout
                            .write_all(said.as_bytes())
                            .and_then(|()| stdout.flush());
                    }
                    let keymap = attaching.keymap.clone();
                    keyboard = Some(Keyboard::new(socket, me, keymap, attaching.read_only)?);
                    if let Some(problem) = &attaching.problem {
                        acts.push(Act::Notice {
                            text: problem.clone(),
                            error: true,
                        });
                    }
                }
                (frame, Some(keyboard)) => acts.extend(keyboard.frame(frame)),
                (_, None) => {}
            }
        }
        let _ = stdout.flush();
        for act in std::mem::take(&mut acts) {
            send(stream, &AttachedFrame::Act(Box::new(act)))?;
        }
        let mut fds = vec![
            PollFd::new(&*stream, PollFlags::IN),
            PollFd::new(winch, PollFlags::IN),
            PollFd::new(stops, PollFlags::IN),
        ];
        // The keys only once the client knows itself, to run them.
        if let Some(keyboard) = &keyboard {
            fds.push(PollFd::new(keyboard.wake(), PollFlags::IN));
            if terminal {
                fds.push(PollFd::new(&stdin, PollFlags::IN));
            }
        }
        // A lone Escape, or an answer cut short, waits until its deadline.
        let deadline = keyboard.as_ref().and_then(Keyboard::deadline);
        let timeout = deadline.map(|at| at.saturating_duration_since(Instant::now()));
        match fuxix::poll::poll(&mut fds, timeout) {
            Ok(_) | Err(fuxix::Errno::INTR) => {}
            Err(e) => return Err(Error::Poll(e)),
        }
        let ready: Vec<bool> = fds.iter().map(|fd| !fd.revents().is_empty()).collect();
        drop(fds);
        let is = |i: usize| ready.get(i).copied().unwrap_or(false);
        if is(STOPS) {
            let _ = send(stream, &AttachedFrame::Detach);
            return Ok(STOPPED.into());
        }
        if is(WINCH) {
            crate::drain(winch);
            if terminal {
                let (rows, cols) = window_size();
                send(stream, &AttachedFrame::Resize { rows, cols })?;
            }
        }
        if let Some(keyboard) = &mut keyboard {
            if is(WAKE) {
                acts.extend(keyboard.ran());
            }
            let now = Instant::now();
            if keyboard.deadline().is_some_and(|at| at <= now) {
                acts.extend(keyboard.timeout());
            }
            if is(TERMINAL) {
                match fuxix::io::read(&stdin, &mut buffer) {
                    Ok(0) => {
                        let _ = send(stream, &AttachedFrame::Detach);
                        return Ok("detached: the terminal closed".into());
                    }
                    Ok(n) => acts.extend(keyboard.bytes(buffer.get(..n).unwrap_or_default(), now)),
                    Err(fuxix::Errno::INTR | fuxix::Errno::AGAIN) => {}
                    Err(e) => return Err(Error::ReadTerminal(e)),
                }
            }
        }
        if is(STREAM) {
            match stream.read(&mut buffer) {
                Ok(0) => return Ok("the server closed the connection".into()),
                Ok(n) => decoder.push(buffer.get(..n).unwrap_or_default()),
                Err(e) if matches!(e.kind(), ErrorKind::Interrupted | ErrorKind::WouldBlock) => {}
                Err(e) => return Err(Error::Read(e)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Keys wait only for a program that may open a screen: one with a
    /// word that opens one, not as `str`'s text.
    #[test]
    fn keys_wait_only_for_a_program_that_may_open_a_screen() {
        assert!(may_open("me me focused menu"));
        assert!(may_open("me command-prompt"));
        assert!(!may_open("me focused opts horizontal split drop"));
        assert!(!may_open("str menu rename"));
    }

    /// What the client makes of the server's answer to Hello, as read from
    /// a socket in `wait`.
    fn answer(server: impl FnOnce(&mut UnixStream), wait: Duration) -> Result<(), Error> {
        let (client, mut peer) = UnixStream::pair().map_err(Error::Signals)?;
        let mut client = Link::socket(client).map_err(Error::Signals)?;
        server(&mut peer);
        let mut decoder = Decoder::default();
        let mut buffer = [0u8; 256];
        let answer = read_frame(&mut client, &mut decoder, &mut buffer, Some(wait));
        greeted(&OURS, answer, Ok(()))
    }

    const OURS: Hello = Hello {
        protocol: PROTOCOL,
        version: "0",
        role: Role::Command,
    };

    fn write(peer: &mut UnixStream, frame: &ServerFrame) {
        let _ = frame.encode().map(|bytes| peer.write_all(&bytes));
    }

    /// A server that refuses says why; one that says nothing times out;
    /// one that hangs up did not answer.
    #[test]
    fn the_client_tells_a_refusal_from_a_timeout() {
        let wait = Duration::from_millis(50);
        assert!(answer(|peer| write(peer, &ServerFrame::Hello(OURS)), wait).is_ok());
        // A server of another protocol is refused before anything else is
        // sent to it.
        let other = ServerFrame::Hello(Hello {
            protocol: PROTOCOL + 1,
            ..OURS
        });
        let mismatched = answer(|peer| write(peer, &other), wait);
        assert!(matches!(&mismatched, Err(Error::Mismatch(m)) if m.protocol == PROTOCOL + 1));
        let refused = answer(|peer| write(peer, &ServerFrame::Exit("busy")), wait);
        assert!(matches!(&refused, Err(Error::Refused(reason)) if reason == "busy"));
        let silent = answer(|_| {}, wait);
        assert!(matches!(silent, Err(Error::Timeout)));
        assert_eq!(
            silent.err().map(|e| e.to_string()).as_deref(),
            Some("the server did not answer in time")
        );
        let gone = answer(|peer| drop(peer.shutdown(std::net::Shutdown::Write)), wait);
        assert!(matches!(gone, Err(Error::NoAnswer)));
        let garbled = answer(|peer| drop(peer.write_all(&[0, 0, 0, 1, 99])), wait);
        assert!(matches!(
            garbled,
            Err(Error::Protocol(crate::protocol::Error::UnexpectedKind(99)))
        ));
        // A refusal wins over a failure to send the Hello.
        let both = greeted(&OURS, Ok(Some(ServerFrame::Exit("no"))), Err(Error::Closed));
        assert!(matches!(both, Err(Error::Refused(_))));
    }
}
