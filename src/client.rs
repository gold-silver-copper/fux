//! The clients: an attached one, which reads its terminal's keys, runs
//! its own bindings, and sends the server typed actions, writing the
//! paints the server relays to its terminal;
//! a headless one; and the connection a program's requests go over.
use crate::act::Act;
use crate::client_state::ClientState;
use crate::code::Code;
#[cfg(test)]
use crate::command::{Command, Request};
use crate::decode::Input;
use crate::keys::{Key, KeyPress, Modifiers};
use crate::protocol::{
    Asked, Attach, AttachedFrame, Context, Decoder, Frame, Greeting, Hello, Mismatch, PROTOCOL,
    Reply, Role, ToAttached, ToCommand,
};
use crate::screens::{Dispatch, Display, Job, Screen};
use crate::script;
use crate::socket::{self, SocketPath};
use fuxix::Errno;
use fuxix::poll::{Events as PollFlags, PollFd};
use fuxix::terminal::Termios;
use std::collections::VecDeque;
use std::io::{self, BufRead, ErrorKind, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// What the attach client sets on its terminal: its title saved
/// (`outer::TITLE_PUSH`), the alternate screen, normal cursor and keypad
/// keys, bracketed paste, focus events, no autowrap. The pane's modes are
/// the server's to set (`render::Modes`). Public for fux-vt-compare.
pub const ENTER: &str =
    "\x1b[22;0t\x1b[?1049h\x1b[?1l\x1b>\x1b[?2004h\x1b[?1004h\x1b[?7l\x1b[H\x1b[2J";
/// And puts it back, however the attachment ends, with what the server may
/// have turned on: the pane's modes (`render::Modes`; the server gives the
/// cursor's shape back itself, as DECSCUSR has no "the terminal's own"),
/// scheme reports (mode 2031), and the kitty flags, popped on the alternate
/// screen they were pushed on.
const LEAVE: &str = "\x1b[23;0t\x1b[?2026l\x1b[?1003l\x1b[?1002l\x1b[?1000l\x1b[?1006l\x1b[?1004l\x1b[?2004l\x1b[?2031l\x1b[<u\x1b[?7h\x1b[0m\x1b[?25h\x1b[?1049l";

/// Why a client could not reach the server, start one, or go on.
#[derive(Debug)]
pub enum Error {
    Socket(socket::Error),
    Connect {
        path: PathBuf,
        source: io::Error,
    },
    /// The server ended the connection, saying why.
    Refused(String),
    /// The server speaks another protocol.
    Mismatch(Mismatch),
    Protocol(crate::protocol::Error),
    NotATerminal,
    /// The terminal says no size (`TIOCGWINSZ`), or 0 rows or columns.
    NoSize,
    /// What a request streamed could not be written out.
    Output(io::Error),
    /// Anything else that failed: why.
    Failed(String),
}

impl Error {
    /// What kind of failure it is: its code.
    pub fn code(&self) -> Code {
        match self {
            Error::Socket(socket::Error::NoServer(_)) | Error::Connect { .. } => Code::NoServer,
            Error::Mismatch(_) => Code::Mismatch,
            Error::Output(_) => Code::Output,
            Error::Socket(_)
            | Error::Refused(_)
            | Error::Protocol(_)
            | Error::NotATerminal
            | Error::NoSize
            | Error::Failed(_) => Code::Client,
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Socket(error) => error.fmt(f),
            // A socket left by a server that is gone.
            Error::Connect { path, source } if source.kind() == ErrorKind::ConnectionRefused => {
                write!(
                    f,
                    "no fux server is running at {} (one that stopped left it); {}",
                    path.display(),
                    socket::start_hint(path)
                )
            }
            Error::Connect { path, source } => {
                write!(f, "connecting to {}: {source}", path.display())
            }
            Error::Refused(reason) => f.write_str(reason),
            Error::Mismatch(mismatch) => mismatch.fmt(f),
            Error::Protocol(error) => error.fmt(f),
            Error::NotATerminal => {
                f.write_str("attaching needs a terminal on stdin and stdout, the same one")
            }
            Error::NoSize => f.write_str(
                "the terminal on stdin says no size (0 rows or columns), which fux would have to make up; \
                 give it one (`stty rows 24 cols 80`)",
            ),
            Error::Output(error) => write!(f, "writing the output: {error}"),
            Error::Failed(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for Error {}

/// A failure while `doing` what its message begins with.
fn failed<E: std::fmt::Display>(doing: &'static str) -> impl FnOnce(E) -> Error {
    move |e| Error::Failed(format!("{doing}: {e}"))
}

/// The server closed the connection before it answered.
fn no_answer() -> Error {
    Error::Failed("the server did not answer".to_owned())
}

/// A pipe or a thread of the client's own could not be made.
impl From<io::Error> for Error {
    fn from(error: io::Error) -> Error {
        Error::Failed(error.to_string())
    }
}

impl From<socket::Error> for Error {
    fn from(error: socket::Error) -> Error {
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
        let mut out = io::stdout();
        let _ = out.write_all(LEAVE.as_bytes());
        let _ = out.flush();
        let _ = fuxix::terminal::set_attributes(io::stdin(), &termios);
    }
}

/// How long a client waits for a server's answer: its Hello, or its last
/// frames once the client could not write.
const ANSWER: Duration = Duration::from_secs(5);

/// The socket of the server at `socket`, once it is checked to be served by
/// this user. fux is reached through its socket alone: where the system
/// denies `connect` (a sandbox), it cannot be.
fn open(socket: &SocketPath) -> Result<UnixStream, Error> {
    // Said of the socket chosen, which a hint starts again.
    let failed = |source| Error::Connect {
        path: socket.path().to_owned(),
        source,
    };
    socket::check_client_socket(socket)?;
    let stream = UnixStream::connect(socket.path()).map_err(failed)?;
    // Its server is this user's, whoever made the path's directories.
    let euid = fuxix::process::geteuid();
    match fuxix::socket::peer_uid(&stream) {
        Ok(uid) if uid == euid => Ok(stream),
        Ok(uid) => Err(io::Error::other(format!(
            "its server runs as uid {uid}, not this user's ({euid})"
        ))),
        Err(errno) => Err(errno.into()),
    }
    .map_err(failed)
}

/// A connection to the server at `socket` as `role`, greeted, with the
/// server's version.
fn connect(socket: &SocketPath, role: Role) -> Result<(UnixStream, Decoder, String), Error> {
    let mut stream = open(socket)?;
    let ours = Hello::ours(role);
    let sent = send(&mut stream, &ours);
    let mut decoder = Decoder::default();
    // A refusal may close the connection before the Hello is written: its
    // reason is still there to read.
    let answer = read_frame(&mut stream, &mut decoder, Some(ANSWER));
    let version = greeted(&ours, answer, sent)?;
    Ok((stream, decoder, version))
}

/// The server's version, if its answer to our Hello, `ours`, and sending
/// it let the client go on: a refusal, which may come before the Hello is
/// read, wins; a server of another protocol is refused before anything it
/// would not understand is sent.
fn greeted(
    ours: &Hello,
    answer: Result<Option<Greeting>, Error>,
    sent: Result<(), Error>,
) -> Result<String, Error> {
    match (answer, sent) {
        (Ok(Some(Greeting::Exit(reason))), _) => Err(Error::Refused(reason.to_owned())),
        (Ok(Some(Greeting::Hello(theirs))), sent) => {
            Mismatch::between(ours, &theirs).map_err(Error::Mismatch)?;
            sent.map(|()| theirs.version.to_owned())
        }
        (_, Err(error)) | (Err(error), Ok(())) => Err(error),
        (Ok(Some(Greeting::Done(_)) | None), Ok(())) => Err(no_answer()),
    }
}

/// Reads on `stream` wait `timeout` at most from now on (`None`, for
/// ever). macOS refuses a timeout on a socket its peer has shut: a read
/// there cannot wait, as what the server left is read at once and then its
/// end, so the refusal changes nothing.
fn wait_at_most(stream: &UnixStream, timeout: Option<Duration>) {
    let _ = stream.set_read_timeout(timeout);
}

fn send<'a>(stream: &mut UnixStream, frame: &impl Frame<'a>) -> Result<(), Error> {
    let bytes = frame.encode()?;
    stream
        .write_all(&bytes)
        .map_err(failed("writing to the server"))
}

/// The next frame the server sends, waiting `timeout` at most for each
/// read; none once it has closed.
fn read_frame<'d, F: Frame<'d>>(
    stream: &mut UnixStream,
    decoder: &'d mut Decoder,
    timeout: Option<Duration>,
) -> Result<Option<F>, Error> {
    wait_at_most(stream, timeout);
    while !decoder.ready()? {
        if !read_more(stream, decoder)? {
            return Ok(None);
        }
    }
    Ok(decoder.frame()?)
}

/// What the server sent next, read into `decoder`: false once it has
/// closed.
fn read_more(stream: &mut UnixStream, decoder: &mut Decoder) -> Result<bool, Error> {
    let mut buffer = [0u8; 16 * 1024];
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => return Ok(false),
            Ok(n) => {
                decoder.push(buffer.get(..n).unwrap_or_default());
                return Ok(true);
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                let why = "the server did not answer in time";
                return Err(Error::Failed(why.to_owned()));
            }
            Err(e) => return Err(failed("reading from the server")(e)),
        }
    }
}

/// A connection to the server for requests: each sent whole, and answered
/// before the next. One per run of a program, made when its first request
/// is.
pub struct Requests {
    stream: UnixStream,
    decoder: Decoder,
}

impl Requests {
    pub fn open(socket: &SocketPath) -> Result<Requests, Error> {
        let (stream, decoder, _) = connect(socket, Role::Command)?;
        Ok(Requests { stream, decoder })
    }

    /// Sends `asked`, and waits for its reply, which may come in pieces:
    /// what the server streams before it (`events`) goes to `out` as it
    /// comes. Launch requests already contain their selected environment patch.
    pub fn ask(&mut self, asked: &Asked, out: &mut dyn Write) -> Result<Reply, Error> {
        wait_at_most(&self.stream, None);

        send(&mut self.stream, asked)?;
        let mut parts = Vec::new();
        loop {
            while let Some(frame) = self.decoder.frame()? {
                match frame {
                    ToCommand::Stdout(bytes) => {
                        (out.write_all(&bytes).and_then(|()| out.flush()))
                            .map_err(Error::Output)?;
                    }
                    ToCommand::Part(bytes) => {
                        crate::wire::append(&mut parts, &bytes)
                            .map_err(crate::protocol::Error::Wire)?;
                    }
                    ToCommand::PartsEnd => {
                        let reply = crate::wire::Reader::new(&parts).whole::<Reply>();
                        return Ok(reply.map_err(crate::protocol::Error::Wire)?);
                    }
                    ToCommand::Exit(reason) => return Err(Error::Refused(reason.to_owned())),
                }
            }
            if !read_more(&mut self.stream, &mut self.decoder)? {
                let closed = "the server closed the connection";
                return Err(Error::Failed(closed.to_owned()));
            }
        }
    }

    /// What stops it from another thread: shut down, the request under way
    /// ends as if the server had closed the connection.
    pub fn stopper(&self) -> Option<UnixStream> {
        self.stream.try_clone().ok()
    }
}

/// Where requests from this process come from: its directory, its pane
/// and that pane's server. Launch environment is carried in the request.
pub fn context() -> Context {
    let var = |name| std::env::var(name).ok().filter(|v: &String| !v.is_empty());
    Context {
        cwd: std::env::current_dir().unwrap_or_default(),
        pane: var("FUX_PANE").and_then(|p| crate::id::program(&p)),
        server: var("FUX_SERVER"),
        client: None,
        of: None,
    }
}

/// Stops the server, whatever fux version it is.
pub fn kill_server(socket: &SocketPath) -> Result<(), Error> {
    let (mut stream, mut decoder, _) = connect(socket, Role::Kill)?;
    loop {
        match read_frame(&mut stream, &mut decoder, Some(ANSWER))? {
            Some(Greeting::Done(_)) | None => return Ok(()),
            Some(Greeting::Hello(_) | Greeting::Exit(_)) => {}
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
    match connect(socket, Role::Command) {
        Ok((.., version)) => Ok(Probe::Server {
            protocol: PROTOCOL,
            version,
        }),
        Err(Error::Mismatch(Mismatch { protocol, version })) => {
            Ok(Probe::Server { protocol, version })
        }
        Err(Error::Socket(socket::Error::NoServer(_))) => Ok(Probe::Absent),
        // A socket left by a server that is gone.
        Err(Error::Connect { source, .. })
            if matches!(
                source.kind(),
                ErrorKind::ConnectionRefused | ErrorKind::TimedOut
            ) =>
        {
            Ok(Probe::Left)
        }
        Err(error) => Err(error),
    }
}

/// Whether a server answers on `socket`.
pub fn answers(socket: &SocketPath) -> bool {
    UnixStream::connect(socket.path()).is_ok()
}

/// Starts a server in the background, in a new session with its log in
/// `fux.log` beside the socket, and waits for the line it writes once it
/// listens (`server::READY`). Its stdout closing first means it failed, or
/// found another server on the socket: one that answers, as a server that
/// starts waits for the one starting before it (`socket::bind_socket`). A
/// server that hangs before it listens holds the client too.
pub fn start_server(socket: &SocketPath) -> Result<(), Error> {
    let exe = std::env::current_exe().map_err(failed("finding the fux binary"))?;
    socket::private_directory(socket, true)?;
    let log = socket.directory().join("fux.log");
    let output = open_log(&log).map_err(|e| Error::Failed(format!("{}: {e}", log.display())))?;
    let mut command = std::process::Command::new(exe);
    command
        .arg(crate::SERVE)
        .arg(socket.path())
        .current_dir("/")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(output);
    // A daemon (daemon(7)): a session of its own, no descriptors of the
    // client's but stdio, no directory kept busy.
    fuxix::process::lead_session(&mut command, false);
    let mut child = command.spawn().map_err(failed("starting a server"))?;
    let mut said = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        let line = io::BufReader::new(stdout.take(64)).read_until(b'\n', &mut said);
        line.map_err(failed("reading the server"))?;
    }
    // Dropping a `Child` neither waits for nor kills it.
    if said == crate::server::READY || answers(socket) {
        return Ok(());
    }
    let status = child.wait().map_err(failed("waiting for the server"))?;
    let why = format!("the server exited ({status}); see {}", log.display());
    Err(Error::Failed(why))
}

/// The server's log at `path`, opened to append: a regular file alone. A
/// link is not followed, a FIFO with no reader refused at once (`ENXIO`),
/// not waited on, and a terminal there never becomes this process's.
fn open_log(path: &std::path::Path) -> io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    let flags = fuxix::file::NOFOLLOW | fuxix::file::NONBLOCK | fuxix::file::NOCTTY;
    let file = (std::fs::OpenOptions::new().create(true).append(true))
        .custom_flags(flags)
        .open(path)?;
    // Nonblocking changes nothing for a regular file's writes.
    match file.metadata()?.is_file() {
        true => Ok(file),
        false => Err(io::Error::other("not a regular file")),
    }
}

/// The rows and columns of the terminal on stdin, which the client reads
/// and the server paints, if it says them: one that says none (or 0 of
/// either) has no size fux could lay panes out on.
pub(crate) fn window_size() -> Option<fux_vt::Size> {
    fuxix::terminal::window_size(io::stdin()).map(|(s, _)| (s.rows, s.cols).into())
}

/// Whether stdin and stdout are one terminal: the same device, or both the
/// session's controlling terminal (`fux </dev/tty`, whose device is
/// `/dev/tty`'s). The client reads keys from stdin, which the server
/// paints, and sets the terminal's modes through stdout.
pub(crate) fn on_one_terminal() -> bool {
    use io::IsTerminal as _;
    use std::os::fd::AsFd as _;
    use std::os::unix::fs::MetadataExt as _;
    let device = |fd: std::os::fd::BorrowedFd<'_>| {
        let file = std::fs::File::from(fd.try_clone_to_owned().ok()?);
        file.metadata().ok().map(|m| m.rdev())
    };
    io::stdin().is_terminal()
        && io::stdout().is_terminal()
        && (device(io::stdin().as_fd()).is_some_and(|d| device(io::stdout().as_fd()) == Some(d))
            || fuxix::terminal::controls_session(io::stdin())
                && fuxix::terminal::controls_session(io::stdout()))
}

/// How a client attaches: to which workspace, whether it only watches, its
/// keys (a typing terminal's config run on them already, else the bundled
/// ones), and what was wrong with that config.
pub struct Attaching {
    pub of: Option<crate::command::Run>,
    pub target: Option<crate::command::AttachTarget>,
    pub read_only: bool,
    pub creation: crate::script::Opts,
    pub state: ClientState,
    pub problem: Option<String>,
}

impl Attaching {
    fn initial(&self) -> Result<crate::command::InitialWorkspace, Error> {
        let creation = &self.creation;
        let (start, env) = creation
            .launch(
                &self.state.preferences.shell,
                &self.state.preferences.copy_environment,
            )
            .map_err(Error::Failed)?;
        Ok(crate::command::InitialWorkspace {
            name: creation.name.clone(),
            start,
            options: creation.pane_options(self.state.preferences.pane_options()),
            cwd: {
                let invocation = context().cwd;
                match creation.cwd.as_deref() {
                    Some(dir) if dir.is_absolute() => dir.to_path_buf(),
                    Some(dir) if invocation.is_absolute() => invocation.join(dir),
                    Some(_) => PathBuf::new(),
                    None => invocation,
                }
            },
            env,
        })
    }
}

/// What a program on the worker came to: what to say of it, and the
/// screen it opened.
struct Ran {
    notice: Option<(String, bool)>,
    opened: Option<Screen>,
    /// It changed the keys of fux's screens.
    rebound: bool,
}

enum Update {
    Presentation(crate::view::Presentation, std::sync::mpsc::SyncSender<u64>),
    Finished(Ran),
}

/// The longest a program's notice may be: what it prints past this fails
/// its write, and a notice longer, made of what it left, is not shown but
/// said not to be. Well within a frame (`protocol::MAX_FRAME`).
pub(crate) const NOTICE_MAX: usize = 64 << 10;

/// What a key's program prints, for its notice: `NOTICE_MAX` at most.
#[derive(Default)]
struct Said(Vec<u8>);

impl Write for Said {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.0.len().saturating_add(bytes.len()) > NOTICE_MAX {
            let why = format!("a key's program prints {NOTICE_MAX} bytes at most");
            return Err(io::Error::other(why));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Programs that may wait for the worker, and the bytes of their code:
/// one that would take them past either is not run, and said not to be.
pub(crate) const JOBS: usize = 64;
pub(crate) const JOB_BYTES: usize = 4 << 20;
/// Inputs that may wait for a program, and the bytes of their pastes: those
/// past either are not taken, and said not to be (`Keyboard::holds`).
pub(crate) const HELD: usize = 1024;
pub(crate) const HELD_BYTES: usize = 1 << 20;

/// The bytes `input` holds beyond itself.
fn pasted(input: &Input) -> usize {
    match input {
        Input::Paste(text) => text.len(),
        Input::Key(_)
        | Input::PasteTooLong
        | Input::FocusIn
        | Input::FocusOut
        | Input::Mouse(_)
        | Input::Reply(_) => 0,
    }
}

/// The state the worker and the client share. A program changes a copy,
/// put in its place whole (`run_job`), so one that panicked holding it
/// left no change half made.
fn lock(state: &Mutex<ClientState>) -> std::sync::MutexGuard<'_, ClientState> {
    state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// An attached client's own keys: its state, the screen it has open or
/// what its keys are in the middle of, the decoder its terminal's bytes go
/// through, and the worker that runs the programs its bindings and screens
/// give, one after another, as the client. The worker and the client share
/// the state: a program changes it as it ends. What each takes, it turns
/// into the actions to send the server.
///
/// Every mode is the client's (`crate::screens`), changed here at the key
/// that changes it. A program it runs may open a screen, so the keys typed
/// while any runs wait for it, and go to the screen it opened, if any.
///
/// The transport must handle every returned effect in order: send each
/// action, count its frame, acknowledge `Dispatch::Fence` with `Self::fence`,
/// and pass each admitted job to `Self::dispatch`.
/// Dropping an admitted job leaves its reservation and held keys waiting.
pub struct Keyboard {
    state: std::sync::Arc<Mutex<ClientState>>,
    screen: Display,
    read_only: bool,
    decoder: crate::decode::Decoder,
    jobs: std::sync::mpsc::Sender<(Job, u64)>,
    /// Bounded committed presentation updates and the dispatched program's result.
    told: std::sync::mpsc::Receiver<Update>,
    receipt: Option<std::sync::mpsc::SyncSender<u64>>,
    wake: io::PipeReader,
    /// The weights of admitted programs not yet done, in order:
    /// `JOBS` and `JOB_BYTES` at most.
    running: VecDeque<usize>,
    /// Admitted jobs dispatched after the running job's screen effects.
    pending: VecDeque<Job>,
    /// What stops the program the worker runs (the prefix, then Esc).
    stop: script::Stop,
    /// What was typed while a program ran, in order, and the bytes of its
    /// pastes: `HELD` and `HELD_BYTES` at most.
    waiting: VecDeque<Input>,
    held: usize,
}

/// Ending an attachment stops its outstanding program rather than leaving
/// its worker running.
impl Drop for Keyboard {
    fn drop(&mut self) {
        self.stop.stop();
    }
}

impl Keyboard {
    /// The keyboard of the client attached as `me` in `run` on `socket`, with
    /// `state`; `read_only` if it only watches.
    pub fn new(
        socket: &SocketPath,
        me: crate::id::ClientId,
        run: crate::command::Run,
        state: ClientState,
        read_only: bool,
    ) -> Result<Keyboard, Error> {
        let (jobs, todo) = std::sync::mpsc::channel::<(Job, u64)>();
        let (tell, told) = std::sync::mpsc::sync_channel::<Update>(1);
        let (wake, poke) = io::pipe()?;
        fuxix::io::set_nonblocking(&wake, true).map_err(io::Error::from)?;
        fuxix::io::set_nonblocking(&poke, true).map_err(io::Error::from)?;
        let path = socket.path().to_owned();
        let stop = script::Stop::default();
        let stops = stop.clone();
        let state = std::sync::Arc::new(Mutex::new(state));
        let shared = state.clone();
        let spawned = std::thread::Builder::new()
            .name("keys".into())
            .spawn(move || {
                let chosen = socket::Choice::Path(path);
                let mut server = script::Server::stoppable(chosen, stops.clone());
                for (job, frames) in todo {
                    let after = std::rc::Rc::new(std::cell::Cell::new(crate::protocol::Fence {
                        run,
                        client: me,
                        frames,
                    }));
                    server.after = Some(after.clone());
                    let mut present = |presentation| {
                        let (sent, received) = std::sync::mpsc::sync_channel(1);
                        if tell.send(Update::Presentation(presentation, sent)).is_err() {
                            stops.stop();
                            return;
                        }
                        let _ = fuxix::io::write(&poke, b"!");
                        // Receipt follows transmission, not mailbox consumption.
                        // Dropping the keyboard drops the receipt sender too.
                        match received.recv() {
                            Ok(frames) => after.set(crate::protocol::Fence {
                                frames,
                                ..after.get()
                            }),
                            Err(_) => stops.stop(),
                        }
                    };
                    let ran = run_job(&mut server, (me, run), &shared, &job, &stops, &mut present);
                    if tell.send(Update::Finished(ran)).is_err() {
                        break;
                    }
                    // A pipe too full for it is readable: the client wakes
                    // all the same.
                    let _ = fuxix::io::write(&poke, b"!");
                }
            });
        spawned?;
        let mut decoder = crate::decode::Decoder::default();
        // The server asks the terminal about itself as the client attaches.
        decoder.expect(Instant::now());
        Ok(Keyboard {
            state,
            screen: Display::default(),
            read_only,
            decoder,
            jobs,
            told,
            receipt: None,
            wake,
            running: VecDeque::new(),
            pending: VecDeque::new(),
            stop,
            waiting: VecDeque::new(),
            held: 0,
        })
    }

    /// Whether nothing waits: no program running, no keys held for one.
    pub fn idle(&self) -> bool {
        self.running.is_empty() && self.waiting.is_empty()
    }

    /// Readable when a program it ran is done (`Keyboard::ran`).
    pub fn wake(&self) -> &io::PipeReader {
        &self.wake
    }

    /// When what its terminal sent last is to be taken as it is: a lone
    /// Escape, or an answer cut short (`Keyboard::timeout`).
    pub fn deadline(&self) -> Option<Instant> {
        self.decoder.deadline()
    }

    /// Bytes from its terminal, read at `now`. Handle returned effects in
    /// order, as `Keyboard` requires.
    pub fn bytes(&mut self, bytes: &[u8], now: Instant) -> Vec<Dispatch> {
        let mut inputs = Vec::new();
        self.decoder.expire(now);
        self.decoder.bytes(bytes, &mut inputs);
        self.decoder.mark(now);
        self.inputs(inputs)
    }

    /// A deadline passed (`Keyboard::deadline`): what waited is taken.
    /// Handle returned effects in order, as `Keyboard` requires.
    pub fn timeout(&mut self) -> Vec<Dispatch> {
        let mut inputs = Vec::new();
        self.decoder.timeout(&mut inputs);
        self.inputs(inputs)
    }

    /// Inputs taken one at a time, as if each came alone: however the
    /// terminal's reads split them, they come to the same.
    fn inputs(&mut self, inputs: impl IntoIterator<Item = Input>) -> Vec<Dispatch> {
        let mut acts = Vec::new();
        let mut refused = 0usize;
        for input in inputs {
            let running = !self.running.is_empty();
            // The prefix, then Esc, typed while a program runs, stops it.
            if running && self.stops(&input) {
                self.waiting.pop_back();
                self.stop.stop();
                let text = "stopping…".to_owned();
                acts.push(Dispatch::Send(Act::Notice { text, error: false }));
            } else if !running || self.holds(&input) {
                self.held = self.held.saturating_add(pasted(&input));
                self.waiting.push_back(input);
            } else {
                refused = refused.saturating_add(1);
            }
            acts.extend(self.release());
        }
        if refused > 0 {
            let (held, bytes) = (self.waiting.len(), self.held);
            let text = format!(
                "{refused} inputs not taken: {held} already wait for a program ({bytes} bytes pasted)"
            );
            acts.push(Dispatch::Send(Act::Notice { text, error: true }));
        }
        acts
    }

    /// Whether `input` is Esc after the prefix, the last key waiting.
    fn stops(&self, input: &Input) -> bool {
        let is = |input: Option<&Input>, key: KeyPress| matches!(input, Some(Input::Key(stroke)) if stroke.press == key);
        let escape = KeyPress::new(Key::Escape, Modifiers::NONE);
        is(Some(input), escape)
            && lock(&self.state)
                .keys
                .prefix
                .is_some_and(|prefix| is(self.waiting.back(), prefix))
    }

    /// Whether `input` may wait for a program: the prefix has one place
    /// more, so that Esc after it still stops it.
    fn holds(&self, input: &Input) -> bool {
        let prefix = matches!(input, Input::Key(stroke) if Some(stroke.press) == lock(&self.state).keys.prefix);
        let room = HELD.saturating_add(usize::from(prefix));
        self.waiting.len() < room && self.held.saturating_add(pasted(input)) <= HELD_BYTES
    }

    /// What waits, taken unless a program runs.
    fn release(&mut self) -> Vec<Dispatch> {
        let mut acts = Vec::new();
        while self.running.is_empty() {
            let Some(input) = self.waiting.pop_front() else {
                break;
            };
            self.held = self.held.saturating_sub(pasted(&input));
            self.input(input, &mut acts);
        }
        acts
    }

    /// A frame from the server: copy mode it ended, a program to run (a
    /// screen asked of it from elsewhere), keys to take as typed
    /// (`send-keys` to it), the prefix then Esc among them stopping a
    /// program as typed ones do. Handle returned effects in order, as
    /// `Keyboard` requires.
    pub fn frame(&mut self, frame: ToAttached<'_>) -> Vec<Dispatch> {
        let mut acts = Vec::new();
        match frame {
            ToAttached::Ended(opening) => self.screen.ended(opening),
            ToAttached::Run(code) => self.run(code.into(), &mut acts),
            ToAttached::Keys(typed) => return self.inputs(typed.into_iter().flat_map(decoded)),
            // The attachment's own: `attached` reads them.
            ToAttached::NeedsInitial
            | ToAttached::Welcome(..)
            | ToAttached::Paint(_)
            | ToAttached::Exit(_) => {}
        }
        acts.extend(self.release());
        acts
    }

    /// What the programs it ran came to, those done: a screen one opened
    /// opens, what one said shows, and the keys that waited go on.
    /// Handle returned effects in order, as `Keyboard` requires.
    pub fn ran(&mut self) -> Vec<Dispatch> {
        crate::drain(&self.wake);
        let mut acts = Vec::new();
        for _ in 0..JOBS {
            let Ok(update) = self.told.try_recv() else {
                break;
            };
            let ran = match update {
                Update::Presentation(presentation, sent) => {
                    self.receipt = Some(sent);
                    acts.push(Dispatch::Send(Act::Presentation(presentation)));
                    acts.push(Dispatch::Fence);
                    continue;
                }
                Update::Finished(ran) => ran,
            };
            self.running.pop_front();
            let mut notice = ran.notice;
            // The screen showing hints at keys that changed: it closes,
            // rather than say what its keys no longer do, and says so with
            // what the program said.
            if ran.opened.is_none() && ran.rebound && self.screen.screen.is_open() {
                acts.extend(self.screen.close().map(Dispatch::Send));
                let closed = "the keys of fux's screens changed: the one open closed";
                notice = Some(match notice {
                    Some((text, error)) => (format!("{text}  ({closed})"), error),
                    None => (closed.to_owned(), false),
                });
            }
            if let Some(screen) = ran.opened {
                let at = acts.len();
                self.open(screen, &mut acts);
                if let Some(Dispatch::Send(Act::Notice { text, error })) = acts.get_mut(at)
                    && let Some((said, failed)) = notice.take()
                {
                    text.push_str("  ");
                    text.push_str(&said);
                    *error |= failed;
                }
            }
            if let Some((text, error)) = notice {
                acts.push(Dispatch::Send(Act::Notice { text, error }));
            }
            // A completed job's screen effects precede the next job's
            // fence, just as typed acts do.
            if let Some(job) = self.pending.pop_front() {
                acts.push(Dispatch::Run(job));
            }
        }
        acts.extend(self.release());
        acts
    }

    /// Opens `screen`, in place of what was open. A client that only
    /// watches opens none: it says why.
    fn open(&mut self, screen: Screen, acts: &mut Vec<Dispatch>) {
        if self.read_only {
            let text = "watching only: no choosers, prompts or copy mode here".to_owned();
            acts.push(Dispatch::Send(Act::Notice { text, error: true }));
            return;
        }
        acts.extend(
            self.screen
                .open(screen, &lock(&self.state).keys.modes)
                .into_iter()
                .map(Dispatch::Send),
        );
    }

    /// What a key or a paste led to: actions for the server, programs to run.
    fn apply(&mut self, effects: Vec<Dispatch>, acts: &mut Vec<Dispatch>) {
        for effect in effects {
            match effect {
                Dispatch::Send(act) => acts.push(Dispatch::Send(act)),
                Dispatch::Run(job) => self.run(job, acts),
                Dispatch::Fence => acts.push(Dispatch::Fence),
            }
        }
    }

    /// Reserves `job` in local output order, unless as many programs, or as much
    /// code, as it may hold wait for it. Each emitted `Dispatch::Run` owns
    /// that reservation and must reach `Self::dispatch` in transport order.
    fn run(&mut self, job: Job, acts: &mut Vec<Dispatch>) {
        // A stop stops the programs queued with the one it was typed for.
        if self.running.is_empty() {
            self.stop.reset();
        }
        let (waiting, bytes) = (self.running.len(), self.running.iter().sum::<usize>());
        let len = (job.code.len()).saturating_add(job.given.as_ref().map_or(0, String::len));
        if waiting >= JOBS || bytes.saturating_add(len) > JOB_BYTES {
            let text = format!(
                "a program was not run: {waiting} already wait to run ({bytes} bytes of code)"
            );
            return acts.push(Dispatch::Send(Act::Notice { text, error: true }));
        }
        self.running.push_back(len);
        if waiting == 0 {
            acts.push(Dispatch::Run(job));
        } else {
            self.pending.push_back(job);
        }
    }

    /// What the terminal sent, decoded (`Screen::input`).
    fn input(&mut self, input: Input, acts: &mut Vec<Dispatch>) {
        let state = lock(&self.state);
        let effects = self.screen.input(&state.keys, input, self.read_only);
        drop(state);
        self.apply(effects, acts);
    }

    /// A transport completed every send preceding `Dispatch::Fence`.
    pub fn fence(&mut self, frames: u64) {
        if let Some(sent) = self.receipt.take() {
            let _ = sent.send(frames);
        }
    }

    /// Dispatches an admitted job after its preceding attached frames were
    /// sent. Every request carries that prefix and its source identity.
    pub fn dispatch(&mut self, job: Job, frames: u64) -> Result<(), Error> {
        self.jobs
            .send((job, frames))
            .map_err(|_| Error::Failed("the keys worker stopped".into()))
    }
}

/// A key the server says was typed here (`send-keys` to this client), as
/// its terminal would have sent it.
fn decoded(input: crate::command::Input) -> Vec<Input> {
    match input {
        crate::command::Input::Key(press) => vec![Input::Key(press.into())],
        crate::command::Input::Text(text) => text
            .chars()
            .map(|c| Input::Key(KeyPress::char(c).into()))
            .collect(),
        crate::command::Input::Paste(text) => vec![Input::Paste(text)],
        // A pane's alone: `send-keys` refuses them for a client.
        crate::command::Input::Bytes(_) => Vec::new(),
    }
}

/// Runs a key's or a screen's program as client `me`, on the shared
/// `state`. Each checkpoint commits whole; a failure discards pending changes.
fn run_job(
    server: &mut dyn script::Asks,
    me: (crate::id::ClientId, crate::command::Run),
    state: &Mutex<ClientState>,
    job: &Job,
    stop: &script::Stop,
    present: &mut dyn FnMut(crate::view::Presentation),
) -> Ran {
    let mut changed = lock(state).draft();
    let config = changed.config.clone();
    let mut out = Said::default();
    let mut context = context();
    context.client = Some(me.0);
    context.of = Some(me.1);
    let mut rebound = false;
    let mut publish = |committed: &ClientState| {
        let mut live = lock(state);
        rebound |= live.keys.modes != committed.keys.modes;
        let changed = live.preferences.presentation() != committed.preferences.presentation();
        *live = committed.clone();
        drop(live);
        if changed {
            present(committed.preferences.presentation());
        }
    };
    let mut asks = script::AsClient {
        asks: server,
        context: context.clone(),
        opened: None,
        on_commit: Some(&mut publish),
    };
    let mut result = (|| {
        let tokens = job.tokens()?;
        let mut machine = script::compiled(config.as_ref(), &tokens)?;
        let mut host = script::Host::new(context.clone(), Some(&mut changed), &mut out);
        script::run_capped(
            &mut machine,
            &mut host,
            &mut asks,
            Some(script::PROGRAM_STEPS),
            Some(stop),
        )
    })();
    if result.is_ok()
        && let Err(failed) = script::Asks::commit(&mut asks, &mut changed, context)
    {
        result = Err(failed);
    }
    let opened = result.is_ok().then_some(asks.opened).flatten();
    let notice = match result {
        // Stopped as asked (the prefix, then Esc): no failure of its own.
        Err(_) if stop.stopped() => Some(("stopped".to_owned(), false)),
        Err(failed) => Some((failed.notice(), true)),
        Ok(stack) => said(String::from_utf8_lossy(&out.0).into_owned(), &stack),
    };
    let notice = notice.map(|(text, error)| match text.len() {
        0..=NOTICE_MAX => (text, error),
        n => (
            format!("a notice of {n} bytes, longer than {NOTICE_MAX}, was not shown"),
            true,
        ),
    });
    Ran {
        notice,
        opened,
        rebound,
    }
}

/// The notice a program that ended makes: what it printed, then what it
/// left, each line that is not blank. Made only as far as `NOTICE_MAX`:
/// one longer is said not to be shown.
fn said(mut said: String, stack: &[fuxth::Value<script::Fux>]) -> Option<(String, bool)> {
    for value in stack {
        let room = NOTICE_MAX.saturating_sub(said.len());
        match value.text_upto(room) {
            Ok(Some(text)) if text.len() < room => {
                said.push_str(&text);
                said.push('\n');
            }
            Err(failure) => return Some((failure.message, true)),
            Ok(_) => {
                let text = format!(
                    "what the program left is longer than a notice ({NOTICE_MAX} bytes): not shown"
                );
                return Some((text, true));
            }
        }
    }
    let said: Vec<&str> = said.lines().filter(|l| !l.trim().is_empty()).collect();
    (!said.is_empty()).then(|| (said.join("  "), false))
}

/// Attaches this terminal (stdin, a terminal), or a client with no
/// terminal of `size`, until detach or the server's end, and says why it
/// detached on stdout: on a terminal that may be gone, where a hung-up one
/// fails the write, and the detach is no less done. A client with no
/// terminal says its id as it attaches; it reads no keys, but takes those
/// sent it (`send-keys` to a client) as typed.
pub fn attach(
    socket: &SocketPath,
    attaching: Attaching,
    size: Option<fux_vt::Size>,
) -> Result<(), Error> {
    let stdin = io::stdin();
    let terminal = size.is_none();
    // A terminal's signals are caught once connected, so that ^C while
    // connecting still ends fux, and before it goes raw.
    let early = (!terminal).then(Signals::new).transpose()?;
    let role = if terminal {
        Role::Attach
    } else {
        Role::Headless
    };
    let (mut stream, mut decoder, _) = connect(socket, role)?;
    let signals = early.map_or_else(Signals::new, Ok)?;
    let size = size.or_else(window_size).ok_or(Error::NoSize)?;
    let raw = terminal.then(|| Raw::enter(&stdin)).transpose()?;
    let attach = Attach {
        of: attaching.of,
        size,
        target: attaching.target.clone(),
        read_only: attaching.read_only,
        presentation: attaching.state.preferences.presentation(),
        initial: None,
    };
    // The server relays every paint, which the client writes to its
    // terminal; the keys are the client's to read.
    let mut output = terminal.then(Output::open).transpose()?;
    let result = send(&mut stream, &attach).and_then(|()| {
        attached(
            socket,
            &mut stream,
            &mut decoder,
            &signals,
            &attaching,
            &attach,
            output.as_mut(),
        )
    });
    // What the server painted until it let go is written before the
    // terminal is put back, not after, on the shell.
    let finished = output.as_mut().map_or(Ok(()), Output::finish);
    drop(raw);
    let reason = result.and_then(|reason| finished.map(|()| reason))?;
    let _ = writeln!(io::stdout(), "[{reason}]");
    Ok(())
}

/// Why a client detached when a signal told it to.
const STOPPED: &str = "detached by a signal";

/// The signals an attached client answers: a new size, and those that
/// detach it.
struct Signals {
    winch: fuxix::signal::Pipe,
    stops: fuxix::signal::Pipe,
}

impl Signals {
    fn new() -> Result<Signals, Error> {
        Ok(Signals {
            winch: fuxix::signal::Pipe::new(&[fuxix::process::Signal::Winch])
                .map_err(io::Error::from)?,
            stops: fuxix::signal::Pipe::new(&crate::STOPS).map_err(io::Error::from)?,
        })
    }
}

/// The terminal in raw mode on the alternate screen, which puts the
/// terminal back as it was once dropped, however the attachment ends.
struct Raw;

impl Raw {
    fn enter(stdin: &io::Stdin) -> Result<Raw, Error> {
        Self::enter_with(stdin, &mut io::stdout())
    }

    fn enter_with(stdin: &io::Stdin, stdout: &mut impl Write) -> Result<Raw, Error> {
        let original =
            fuxix::terminal::attributes(stdin).map_err(failed("reading terminal modes"))?;
        let mut raw = original.clone();
        raw.make_raw();
        if let Ok(mut saved) = SAVED.lock() {
            *saved = Some(original);
        }
        // Wrapped once per process, however often it attaches.
        static HOOKED: std::sync::Once = std::sync::Once::new();
        HOOKED.call_once(|| {
            let previous = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                restore();
                previous(info);
            }));
        });
        // Made before raw mode, so that failing to set it restores too.
        let entered = Raw;
        fuxix::terminal::set_attributes(stdin, &raw).map_err(failed("setting raw mode"))?;
        stdout.write_all(ENTER.as_bytes()).map_err(Error::Output)?;
        stdout.flush().map_err(Error::Output)?;
        Ok(entered)
    }
}

impl Drop for Raw {
    fn drop(&mut self) {
        restore();
    }
}

/// What may wait unwritten for the terminal before the server's frames
/// are read no more: what the server sends meanwhile waits there, within
/// its bound.
pub(crate) const OUTPUT_MAX: usize = 1 << 20;

/// The paints the server relays, for the terminal: handed to a thread
/// that writes them to stdout, waiting on the terminal as any program does,
/// so that the client reads keys meanwhile. stdout stays as the shell left
/// it, shared with it and its jobs.
struct Output {
    to: Option<std::sync::mpsc::Sender<Vec<u8>>>,
    writer: Option<std::thread::JoinHandle<io::Result<()>>>,
    /// The bytes handed over and not yet written.
    unwritten: std::sync::Arc<AtomicUsize>,
    /// Whether a write failed, which ends the writer: set before its wake.
    failed: std::sync::Arc<AtomicBool>,
    /// Readable once the writer wrote some, or failed.
    wake: io::PipeReader,
}

/// Writes all of `bytes` to stdout's descriptor, not through `Stdout`,
/// whose lock a terminal that stopped reading would hold from `restore`.
fn write_stdout(mut bytes: &[u8]) -> io::Result<()> {
    while !bytes.is_empty() {
        match fuxix::io::write(io::stdout(), bytes) {
            Ok(0) => return Err(ErrorKind::WriteZero.into()),
            Ok(n) => bytes = bytes.get(n..).unwrap_or_default(),
            Err(Errno::INTR) => {}
            Err(errno) => return Err(errno.into()),
        }
    }
    Ok(())
}

impl Output {
    fn open() -> Result<Output, Error> {
        let (to, todo) = std::sync::mpsc::channel::<Vec<u8>>();
        let (wake, poke) = io::pipe()?;
        fuxix::io::set_nonblocking(&wake, true).map_err(io::Error::from)?;
        fuxix::io::set_nonblocking(&poke, true).map_err(io::Error::from)?;
        let unwritten = std::sync::Arc::new(AtomicUsize::new(0));
        let failed = std::sync::Arc::new(AtomicBool::new(false));
        let (left, failing) = (unwritten.clone(), failed.clone());
        let writer = std::thread::Builder::new()
            .name("terminal".into())
            .spawn(move || {
                for bytes in todo {
                    let wrote = write_stdout(&bytes);
                    left.fetch_sub(bytes.len(), Ordering::Relaxed);
                    failing.store(wrote.is_err(), Ordering::Release);
                    // A pipe too full for it is readable: the client wakes
                    // all the same.
                    let _ = fuxix::io::write(&poke, b"!");
                    wrote?;
                }
                Ok(())
            })?;
        Ok(Output {
            to: Some(to),
            writer: Some(writer),
            unwritten,
            failed,
            wake,
        })
    }

    /// Whether the server's frames may be read: less than `OUTPUT_MAX` is
    /// unwritten, so a paint frame more, `MAX_FRAME` at most, may follow.
    fn room(&self) -> bool {
        self.unwritten.load(Ordering::Relaxed) < OUTPUT_MAX
    }

    /// Hands `bytes` to the writer: an error once it has ended.
    fn push(&mut self, bytes: Vec<u8>) -> Result<(), Error> {
        self.unwritten.fetch_add(bytes.len(), Ordering::Relaxed);
        match self.to.as_ref().map(|to| to.send(bytes)) {
            Some(Ok(())) => Ok(()),
            Some(Err(_)) | None => Err((self.ended().err())
                .unwrap_or_else(|| Error::Failed("the terminal's writer ended".into()))),
        }
    }

    /// The writer wrote some: an error if a write failed.
    fn woke(&mut self) -> Result<(), Error> {
        crate::drain(&self.wake);
        match self.failed.load(Ordering::Acquire) {
            true => self.ended(),
            false => Ok(()),
        }
    }

    /// Waits for the writer to end: the write that failed, if one did.
    fn ended(&mut self) -> Result<(), Error> {
        match self.writer.take().map(std::thread::JoinHandle::join) {
            Some(Ok(Err(error))) => Err(failed("writing the terminal")(error)),
            Some(Err(_)) => Err(Error::Failed("the terminal's writer panicked".into())),
            Some(Ok(Ok(()))) | None => Ok(()),
        }
    }

    /// Waits for all handed over to be written, as the attachment ends:
    /// a terminal that has stopped reading holds the client, as it would
    /// any program that writes to it. A write that failed is said.
    fn finish(&mut self) -> Result<(), Error> {
        self.to = None;
        self.ended()
    }
}

/// A frame count must never wrap into a different prefix.
fn next_frame(frames: u64) -> Result<u64, Error> {
    frames
        .checked_add(1)
        .ok_or_else(|| Error::Failed("attached frame count exhausted".into()))
}

/// Holds the attachment until the server ends it, and says why: reads the
/// terminal's keys, with an `output` for it, and runs them through the
/// client's keys; writes the paints the server relays; passes on the
/// terminal's new size, and a signal that detaches. Until the server says
/// the client's id (its `Welcome`), an `Exit` is a refusal.
fn attached(
    socket: &SocketPath,
    stream: &mut UnixStream,
    decoder: &mut Decoder,
    signals: &Signals,
    attaching: &Attaching,
    attach: &Attach,
    mut output: Option<&mut Output>,
) -> Result<String, Error> {
    let Signals { winch, stops } = signals;
    wait_at_most(stream, None);
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let terminal = output.is_some();
    let mut buffer = vec![0u8; 64 * 1024];
    let mut keyboard: Option<Keyboard> = None;
    let mut retried = false;
    let mut viewport = attach.size;
    let mut pending_resize = None;
    let mut acts = Vec::new();
    let mut frames = 0u64;
    const STREAM: Option<usize> = Some(0);
    const WINCH: Option<usize> = Some(1);
    const STOPS: Option<usize> = Some(2);
    loop {
        // Every whole frame the server sent, while the terminal has room
        // for their paints.
        while output.as_ref().is_none_or(|o| o.room())
            && let Some(frame) = decoder.frame()?
        {
            match (frame, &mut keyboard) {
                (ToAttached::Paint(bytes), _) => {
                    if let Some(output) = &mut output {
                        output.push(bytes.into_owned())?;
                    }
                }
                (ToAttached::NeedsInitial, None) => {
                    if retried || attach.target.is_some() || attach.read_only {
                        return Err(Error::Failed("unexpected needs-initial reply".into()));
                    }
                    let initial = attaching.initial()?;
                    let role = if terminal {
                        Role::Attach
                    } else {
                        Role::Headless
                    };
                    let (mut retry, fresh, _) = connect(socket, role)?;
                    let mut request = attach.clone();
                    // WINCH may still be pending when the reply arrives.
                    if terminal && let Some(size) = window_size() {
                        viewport = size;
                    }
                    request.size = viewport;
                    request.initial = Some(initial);
                    send(&mut retry, &request)?;
                    wait_at_most(&retry, None);
                    *stream = retry;
                    *decoder = fresh;
                    frames = 0;
                    pending_resize = None;
                    retried = true;
                }
                (ToAttached::Exit(reason), None) => return Err(Error::Refused(reason.to_owned())),
                (ToAttached::Exit(reason), Some(_)) => return Ok(reason.to_owned()),
                (ToAttached::Welcome(me, run), None) => {
                    if !terminal {
                        (writeln!(stdout, "{me}").and_then(|()| stdout.flush()))
                            .map_err(Error::Output)?;
                    }
                    let state = attaching.state.clone();
                    keyboard = Some(Keyboard::new(socket, me, run, state, attaching.read_only)?);
                    if let Some(text) = attaching.problem.clone() {
                        acts.push(Dispatch::Send(Act::Notice { text, error: true }));
                    }
                }
                (frame, Some(keyboard)) => acts.extend(keyboard.frame(frame)),
                (_, None) => {}
            }
        }
        // Before admission, resizes stay local. Once admitted, send the
        // latest one before dispatching any job that fences this connection.
        if keyboard.is_some()
            && let Some(size) = pending_resize.take()
        {
            if let Err(ended) = tell(
                stream,
                decoder,
                true,
                output.as_deref_mut(),
                &AttachedFrame::Resize(size),
            ) {
                return ended;
            }
            frames = next_frame(frames)?;
        }
        for out in std::mem::take(&mut acts) {
            let act = match out {
                Dispatch::Run(job) => {
                    keyboard
                        .as_mut()
                        .ok_or_else(|| Error::Failed("a program has no attached keyboard".into()))?
                        .dispatch(job, frames)?;
                    continue;
                }
                Dispatch::Fence => {
                    if let Some(keyboard) = &mut keyboard {
                        keyboard.fence(frames);
                    }
                    continue;
                }
                Dispatch::Send(act) => AttachedFrame::Act(Box::new(act)),
            };
            if let Err(ended) = tell(
                stream,
                decoder,
                keyboard.is_some(),
                output.as_deref_mut(),
                &act,
            ) {
                return ended;
            }
            frames = next_frame(frames)?;
        }
        // The server's frames only while the terminal has room for them; a
        // hang-up is told all the same.
        let reads = match output.as_ref().is_none_or(|o| o.room()) {
            true => PollFlags::IN,
            false => PollFlags::NONE,
        };
        let mut fds = vec![
            PollFd::new(&*stream, reads),
            PollFd::new(winch.reader(), PollFlags::IN),
            PollFd::new(stops.reader(), PollFlags::IN),
        ];
        let mut add = |fd| {
            fds.push(fd);
            Some(fds.len().saturating_sub(1))
        };
        // The terminal's writer, as it writes.
        let written = output
            .as_ref()
            .and_then(|o| add(PollFd::new(&o.wake, PollFlags::IN)));
        // The keys only once the client knows itself, to run them.
        let (wake, typed) = match &keyboard {
            Some(keyboard) => (
                add(PollFd::new(keyboard.wake(), PollFlags::IN)),
                terminal
                    .then(|| add(PollFd::new(&stdin, PollFlags::IN)))
                    .flatten(),
            ),
            None => (None, None),
        };
        // A lone Escape, or an answer cut short, waits until its deadline;
        // whole frames read already, held for room, not at all.
        let deadline = keyboard.as_ref().and_then(Keyboard::deadline);
        let held = output.as_ref().is_none_or(|o| o.room()) && decoder.ready()?;
        let timeout = match held {
            true => Some(Duration::ZERO),
            false => deadline.map(|at| at.saturating_duration_since(Instant::now())),
        };
        match fuxix::poll::poll(&mut fds, timeout) {
            Ok(_) | Err(Errno::INTR) => {}
            Err(e) => return Err(failed("poll")(e)),
        }
        let ready: Vec<bool> = fds.iter().map(|fd| !fd.revents().is_empty()).collect();
        drop(fds);
        let is = |i: Option<usize>| i.and_then(|i| ready.get(i)).copied().unwrap_or(false);
        if is(STOPS) {
            // What the server paints until it lets go is written before the
            // terminal is put back, not after, on the shell.
            // Welcomed, its only error is the terminal's writer's.
            if send(stream, &AttachedFrame::Detach).is_ok()
                && keyboard.is_some()
                && let Some(Err(error)) = last_frames(stream, decoder, true, output.as_deref_mut())
            {
                return Err(error);
            }
            return Ok(STOPPED.into());
        }
        if is(written)
            && let Some(output) = &mut output
        {
            output.woke()?;
        }
        if is(WINCH) {
            crate::drain(winch.reader());
            // A terminal that says no size now keeps the last it said.
            if terminal && let Some(size) = window_size() {
                viewport = size;
                pending_resize = Some(size);
            }
        }
        if let Some(keyboard) = &mut keyboard {
            if is(wake) {
                acts.extend(keyboard.ran());
            }
            let now = Instant::now();
            if keyboard.deadline().is_some_and(|at| at <= now) {
                acts.extend(keyboard.timeout());
            }
            if is(typed) {
                match fuxix::io::read(&stdin, &mut buffer) {
                    Ok(0) => {
                        let _ = send(stream, &AttachedFrame::Detach);
                        return Ok("detached: the terminal closed".into());
                    }
                    Ok(n) => acts.extend(keyboard.bytes(buffer.get(..n).unwrap_or_default(), now)),
                    Err(Errno::INTR | Errno::AGAIN) => {}
                    Err(e) => return Err(failed("reading the terminal")(e)),
                }
            }
        }
        // The frames read before it closed still count: its `Exit` says why.
        if is(STREAM) && !read_more(stream, decoder)? {
            let welcomed = keyboard.is_some();
            let ended = last_frames(stream, decoder, welcomed, output.as_deref_mut());
            return ended.unwrap_or_else(|| Ok("the server closed the connection".into()));
        }
    }
}

/// Sends an attached client's `frame`. A write the server hung up on
/// ends the attachment as the server's last frames say: once it has sent
/// its `Exit` (a detach, say) it closes, while the client, not yet having
/// read it, may still be writing. Only a server that said nothing makes
/// the failed write the end.
fn tell(
    stream: &mut UnixStream,
    decoder: &mut Decoder,
    welcomed: bool,
    output: Option<&mut Output>,
    frame: &AttachedFrame,
) -> Result<(), Result<String, Error>> {
    match send(stream, frame) {
        Ok(()) => Ok(()),
        // Not sent at all: the server goes on as it was.
        Err(error @ Error::Protocol(_)) => Err(Err(error)),
        Err(error) => Err(last_frames(stream, decoder, welcomed, output).unwrap_or(Err(error))),
    }
}

/// Reads the server's frames up to its `Exit`, waiting `ANSWER` at
/// most for each read, and hands their paints to the terminal, if there
/// is an `output` for one: how the attachment ended, the `Exit`'s reason (a refusal, before its
/// `Welcome`), or `None` if the server said nothing more.
fn last_frames(
    stream: &mut UnixStream,
    decoder: &mut Decoder,
    welcomed: bool,
    mut output: Option<&mut Output>,
) -> Option<Result<String, Error>> {
    wait_at_most(stream, Some(ANSWER));
    loop {
        while let Some(frame) = decoder.frame().ok()? {
            match frame {
                // What the server has left to send, within its bound.
                ToAttached::Paint(bytes) => {
                    if let Some(output) = &mut output
                        && let Err(error) = output.push(bytes.into_owned())
                    {
                        return Some(Err(error));
                    }
                }
                ToAttached::Exit(reason) if welcomed => return Some(Ok(reason.to_owned())),
                ToAttached::Exit(reason) => return Some(Err(Error::Refused(reason.to_owned()))),
                ToAttached::NeedsInitial
                | ToAttached::Welcome(..)
                | ToAttached::Ended(_)
                | ToAttached::Run(_)
                | ToAttached::Keys(_) => {}
            }
        }
        if !matches!(read_more(stream, decoder), Ok(true)) {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No end marker is needed to detect an oversized reply. Its owner
    /// drops the failed request connection, as script::Server does.
    #[test]
    fn oversized_unfinished_reply_is_refused() -> Result<(), Box<dyn std::error::Error>> {
        let (stream, mut peer) = UnixStream::pair()?;
        peer.set_read_timeout(Some(Duration::from_secs(5)))?;
        peer.set_write_timeout(Some(Duration::from_secs(5)))?;
        let writer = std::thread::spawn(move || -> io::Result<bool> {
            let mut header = [0; 4];
            peer.read_exact(&mut header)?;
            let mut request = vec![0; u32::from_be_bytes(header) as usize];
            peer.read_exact(&mut request)?;
            let part = vec![0; crate::protocol::MAX_PAYLOAD];
            let frame = ToCommand::Part(std::borrow::Cow::Borrowed(&part))
                .encode()
                .map_err(io::Error::other)?;
            for _ in 0..=crate::wire::MAX_REPLY.div_ceil(part.len()) {
                if peer.write_all(&frame).is_err() {
                    break;
                }
            }
            Ok(match peer.read(&mut [0]) {
                Ok(0) => true,
                Err(error) => matches!(
                    error.kind(),
                    ErrorKind::ConnectionReset
                        | ErrorKind::ConnectionAborted
                        | ErrorKind::BrokenPipe
                ),
                Ok(_) => false,
            })
        });
        let mut requests = Requests {
            stream,
            decoder: Decoder::default(),
        };
        let asked = Asked {
            after: None,
            context: Context::default(),
            request: Request::Do(Command::Ls {
                target: None,
                get: None,
            }),
        };
        let result = requests.ask(&asked, &mut io::sink());
        assert!(matches!(
            result,
            Err(Error::Protocol(crate::protocol::Error::Wire(
                crate::wire::Error::Limit
            )))
        ));
        drop(requests);
        assert!(writer.join().map_err(|_| "writer stopped")??);
        Ok(())
    }

    fn new_keyboard(
        socket: &SocketPath,
        state: ClientState,
        read_only: bool,
    ) -> Result<Keyboard, Box<dyn std::error::Error>> {
        Ok(Keyboard::new(
            socket,
            "c1".parse()?,
            "1-2".parse().map_err(|()| "test run")?,
            state,
            read_only,
        )?)
    }

    fn transport(keyboard: &mut Keyboard, outputs: Vec<Dispatch>) -> Vec<Act> {
        outputs
            .into_iter()
            .filter_map(|out| match out {
                Dispatch::Send(act) => Some(act),
                Dispatch::Fence => {
                    keyboard.fence(0);
                    None
                }
                Dispatch::Run(job) => {
                    assert!(keyboard.dispatch(job, 0).is_ok());
                    None
                }
            })
            .collect()
    }

    fn frame(keyboard: &mut Keyboard, frame: ToAttached<'_>) -> Vec<Act> {
        let outputs = keyboard.frame(frame);
        transport(keyboard, outputs)
    }

    fn bytes(keyboard: &mut Keyboard, bytes: &[u8], now: Instant) -> Vec<Act> {
        let outputs = keyboard.bytes(bytes, now);
        transport(keyboard, outputs)
    }

    fn timeout(keyboard: &mut Keyboard) -> Vec<Act> {
        let outputs = keyboard.timeout();
        transport(keyboard, outputs)
    }

    fn ran(keyboard: &mut Keyboard) -> Vec<Act> {
        let outputs = keyboard.ran();
        transport(keyboard, outputs)
    }

    #[test]
    fn committed_presentation_precedes_the_following_request()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = fuxix::file::scratch()?.join(format!("f5-fence-{}/s", std::process::id()));
        let socket = socket::checked(&path, "test")?;
        let (_endpoint, listener) = socket::bind_socket(&socket)?;
        let mut keyboard = new_keyboard(
            &socket,
            script::default_state().map_err(|e| e.to_string())?,
            false,
        )?;
        let effects = keyboard.frame(ToAttached::Run(
            "str terminal-clipboard false set %1 str Enter key send-keys drop".into(),
        ));
        let [Dispatch::Run(job)] = effects.as_slice() else {
            return Err("job".into());
        };
        keyboard.dispatch(job.clone(), 7)?;
        let mut wake = [PollFd::new(keyboard.wake(), PollFlags::IN)];
        assert_ne!(fuxix::poll::poll(&mut wake, Some(ANSWER))?, 0);
        let pending = keyboard.ran();
        assert!(
            matches!(pending.first(), Some(Dispatch::Send(Act::Presentation(p))) if !p.terminal_clipboard)
        );
        // Receiving an update is not transmission: hold every dispatch here.
        let mut requests = [PollFd::new(&listener, PollFlags::IN)];
        assert_eq!(
            fuxix::poll::poll(&mut requests, Some(Duration::from_millis(100)))?,
            0,
            "the request overtook the unsent presentation"
        );
        let (mut attachment, mut view) = UnixStream::pair()?;
        let mut frames = 7;
        for effect in pending {
            match effect {
                Dispatch::Send(act) => {
                    send(&mut attachment, &AttachedFrame::Act(Box::new(act)))?;
                    frames = next_frame(frames)?;
                }
                Dispatch::Fence => keyboard.fence(frames),
                Dispatch::Run(_) => return Err("another job".into()),
            }
        }
        let sent: AttachedFrame =
            read_frame(&mut view, &mut Decoder::default(), Some(ANSWER))?.ok_or("presentation")?;
        assert!(
            matches!(sent, AttachedFrame::Act(act) if matches!(*act, Act::Presentation(p) if !p.terminal_clipboard))
        );
        assert_ne!(fuxix::poll::poll(&mut requests, Some(ANSWER))?, 0);
        let (mut peer, _) = listener.accept()?;
        let mut decoder = Decoder::default();
        let _: Hello = read_frame(&mut peer, &mut decoder, Some(ANSWER))?.ok_or("hello")?;
        send(&mut peer, &Greeting::Hello(ours()))?;
        let asked: Asked = read_frame(&mut peer, &mut decoder, Some(ANSWER))?.ok_or("request")?;
        assert_eq!(
            asked.after,
            Some(crate::protocol::Fence {
                run: "1-2".parse().map_err(|()| "run")?,
                client: "c1".parse()?,
                frames: 8
            })
        );
        assert!(matches!(
            asked.request,
            Request::Do(Command::Resolve { .. })
        ));
        let reply = Reply {
            run: asked.after.map(|f| f.run),
            ..crate::protocol::Body::Found(crate::command::AnyRef::Pane("%1".parse()?)).into()
        };
        let bytes = crate::wire::encode(&reply)?;
        send(&mut peer, &ToCommand::Part(bytes.into()))?;
        send(&mut peer, &ToCommand::PartsEnd)?;
        let next: Asked = read_frame(&mut peer, &mut decoder, Some(ANSWER))?.ok_or("send-keys")?;
        assert_eq!(next.after, asked.after);
        assert!(matches!(
            next.request,
            Request::Do(Command::SendKeys { .. })
        ));
        drop(peer);
        all_ran(&mut keyboard)?;
        Ok(())
    }

    #[test]
    fn dropping_the_keyboard_releases_a_worker_waiting_for_presentation_transmission()
    -> Result<(), Box<dyn std::error::Error>> {
        let socket = socket::checked("/nowhere/fux/s.sock", "test")?;
        let mut keyboard = new_keyboard(
            &socket,
            script::default_state().map_err(|e| e.to_string())?,
            false,
        )?;
        frame(
            &mut keyboard,
            ToAttached::Run("str titles true set %1 30 timeout wait".into()),
        );
        let mut wake = [PollFd::new(keyboard.wake(), PollFlags::IN)];
        assert_ne!(fuxix::poll::poll(&mut wake, Some(ANSWER))?, 0);
        let pending = keyboard.ran();
        assert!(
            pending
                .iter()
                .any(|effect| matches!(effect, Dispatch::Fence))
        );
        let state = std::sync::Arc::downgrade(&keyboard.state);
        drop(keyboard);
        drop(pending);
        let deadline = Instant::now().checked_add(ANSWER).ok_or("deadline")?;
        while state.strong_count() != 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(
            state.strong_count(),
            0,
            "worker still holds the client state"
        );
        Ok(())
    }

    /// A binding reserves its job immediately but cannot start it while
    /// preceding acts still belong to the local transport.
    #[test]
    fn a_binding_emits_its_job_after_prior_keys() -> Result<(), Box<dyn std::error::Error>> {
        let socket = socket::checked("/nowhere/fux/s.sock", "test")?;
        let mut state = script::default_state().map_err(|e| e.to_string())?;
        state.keys.bind_root(
            KeyPress::char('y'),
            crate::keymap::Binding::new("str done".into(), true),
        )?;
        let mut typed = vec![b'a'; 3000];
        typed.extend_from_slice(b"yx");
        for split in [0, 1, 255, 256, 257, 3000, 3001, 3002] {
            let mut keyboard = new_keyboard(&socket, state.clone(), false)?;
            let (first, last) = typed.split_at_checked(split).ok_or("split")?;
            let mut outputs = keyboard.bytes(first, Instant::now());
            outputs.extend(keyboard.bytes(last, Instant::now()));
            assert_eq!(outputs.len(), 3001);
            assert!(
                outputs
                    .iter()
                    .take(3000)
                    .all(|out| matches!(out, Dispatch::Send(Act::Key(_))))
            );
            assert!(matches!(outputs.last(), Some(Dispatch::Run(_))));
            assert_eq!(keyboard.running.len(), 1);
            assert_eq!(keyboard.waiting.len(), 1);
            assert!(matches!(
                keyboard.told.try_recv(),
                Err(std::sync::mpsc::TryRecvError::Empty)
            ));
        }
        Ok(())
    }

    /// A queued job reaches the transport after its predecessor's screen
    /// changes, so its first request also fences those attached acts.
    #[test]
    fn a_completed_jobs_screen_precedes_the_next_job() -> Result<(), Box<dyn std::error::Error>> {
        let socket = socket::checked("/nowhere/fux/s.sock", "test")?;
        let mut keyboard = new_keyboard(
            &socket,
            script::default_state().map_err(|e| e.to_string())?,
            false,
        )?;
        let first = keyboard.frame(ToAttached::Run("me command-prompt str ready".into()));
        assert!(
            keyboard
                .frame(ToAttached::Run("str next".into()))
                .is_empty()
        );
        assert!(transport(&mut keyboard, first).is_empty());
        let mut done = [PollFd::new(keyboard.wake(), PollFlags::IN)];
        assert_ne!(fuxix::poll::poll(&mut done, Some(ANSWER))?, 0);
        let next = keyboard.ran();
        assert!(
            matches!(next.as_slice(), [Dispatch::Send(Act::Screen { act: crate::act::ScreenAct::Show(_), .. }), Dispatch::Send(Act::Notice { text, .. }), Dispatch::Run(_)] if text == "ready")
        );
        transport(&mut keyboard, next);
        all_ran(&mut keyboard)?;
        Ok(())
    }

    /// Keys typed while any program the client ran is running wait for it,
    /// whatever its words (a config's word may open a chooser); the prefix,
    /// then Esc, stops it, and the keys go on once it is done.
    #[test]
    fn keys_wait_for_a_running_program_till_it_is_done() -> Result<(), Box<dyn std::error::Error>> {
        let socket = socket::checked("/nowhere/fux/s.sock", "test")?;
        let mut keyboard = new_keyboard(
            &socket,
            script::default_state().map_err(|e| e.to_string())?,
            false,
        )?;
        let now = Instant::now();
        let run = ToAttached::Run("begin false until".into());
        assert!(frame(&mut keyboard, run).is_empty());
        assert!(
            bytes(&mut keyboard, b"x", now).is_empty(),
            "a key went past it"
        );
        let mut stopping = bytes(&mut keyboard, b"\x02\x1b", now);
        stopping.extend(timeout(&mut keyboard));
        assert!(
            matches!(stopping.as_slice(), [Act::Notice { error: false, .. }]),
            "{stopping:?}"
        );
        let mut done = [PollFd::new(keyboard.wake(), PollFlags::IN)];
        fuxix::poll::poll(&mut done, Some(ANSWER))?;
        let acts = ran(&mut keyboard);
        assert!(matches!(acts.last(), Some(Act::Key(_))), "{acts:?}");
        assert!(keyboard.idle());
        Ok(())
    }

    /// What the worker's programs came to, once all are done.
    fn all_ran(keyboard: &mut Keyboard) -> Result<Vec<Act>, Box<dyn std::error::Error>> {
        let mut acts = Vec::new();
        while !keyboard.idle() {
            let mut done = [PollFd::new(keyboard.wake(), PollFlags::IN)];
            if fuxix::poll::poll(&mut done, Some(ANSWER))? == 0 {
                return Err("a program did not end".into());
            }
            acts.extend(ran(keyboard));
        }
        Ok(acts)
    }

    #[test]
    fn presentation_commits_whole_and_survives_a_later_failure()
    -> Result<(), Box<dyn std::error::Error>> {
        struct Refuse;
        impl script::Asks for Refuse {
            fn ask(&mut self, _: &Asked, _: &mut dyn Write) -> Result<Reply, script::Failed> {
                Err(script::Failed::new(Code::NotConfig, "operation refused"))
            }
        }
        let state = Mutex::new(script::default_state().map_err(|e| e.to_string())?);
        let me = (
            crate::id::ClientId::of(1),
            "1-2".parse().map_err(|()| "run")?,
        );
        let stop = script::Stop::default();
        let mut updates = Vec::new();
        let code = "str history-bytes 0 set str program-clipboard false set str terminal-clipboard false set str bell false set str titles true set";
        let ran = run_job(&mut Refuse, me, &state, &code.into(), &stop, &mut |p| {
            updates.push(p)
        });
        assert!(ran.notice.is_none());
        assert_eq!(
            updates,
            [crate::view::Presentation {
                bell: false,
                titles: true,
                terminal_clipboard: false
            }]
        );
        updates.clear();
        let committed = lock(&state).clone();
        let failed = "str titles false set str Nope prefix";
        let ran = run_job(&mut Refuse, me, &state, &failed.into(), &stop, &mut |p| {
            updates.push(p)
        });
        assert!(ran.notice.is_some());
        assert!(updates.is_empty());
        assert_eq!(*lock(&state), committed);
        let checkpoint = "str titles false set %1 horizontal split drop";
        let ran = run_job(
            &mut Refuse,
            me,
            &state,
            &checkpoint.into(),
            &stop,
            &mut |p| updates.push(p),
        );
        assert!(ran.notice.is_some());
        assert!(updates.last().is_some_and(|p| !p.titles));
        assert!(!lock(&state).preferences.titles);
        Ok(())
    }

    #[test]
    fn queued_programs_commit_the_whole_local_state() -> Result<(), Box<dyn std::error::Error>> {
        let socket = socket::checked("/nowhere/fux/s.sock", "test")?;
        let mut keyboard = new_keyboard(
            &socket,
            script::default_state().map_err(|e| e.to_string())?,
            false,
        )?;
        for code in [
            "str C-a prefix str wait-seconds 2 set",
            "str F5 str ls bind-root str titles true set",
            "str F6 str ls bind-root str wait-seconds 0 set str bell false set str Nope prefix",
        ] {
            assert!(frame(&mut keyboard, ToAttached::Run(code.into())).is_empty());
        }
        all_ran(&mut keyboard)?;
        let state = lock(&keyboard.state);
        let root: Vec<String> = state
            .keys
            .root
            .iter()
            .map(|(key, _)| key.to_string())
            .collect();
        assert_eq!(
            (
                state.keys.prefix.map(|p| p.to_string()).unwrap_or_default(),
                root
            ),
            ("C-a".into(), vec!["F5".into()])
        );
        assert_eq!(state.preferences.wait.seconds(), 2);
        assert!(state.preferences.titles);
        assert!(state.preferences.bell);
        drop(state);
        Ok(())
    }

    /// A program that changes the screens' keys while one is open closes
    /// it, so its hints never name keys that do otherwise; one that changes
    /// none leaves it open.
    #[test]
    fn a_screen_closes_when_its_keys_change() -> Result<(), Box<dyn std::error::Error>> {
        let socket = socket::checked("/nowhere/fux/s.sock", "test")?;
        let mut keyboard = new_keyboard(
            &socket,
            script::default_state().map_err(|e| e.to_string())?,
            false,
        )?;
        keyboard.open(Screen::Copy(None), &mut Vec::new());
        frame(
            &mut keyboard,
            ToAttached::Run("str F5 str ls bind-root".into()),
        );
        assert_eq!(all_ran(&mut keyboard)?, []);
        assert_eq!(keyboard.screen.screen, Screen::Copy(None));
        frame(
            &mut keyboard,
            ToAttached::Run("str copy str g str top bind-in".into()),
        );
        let acts = all_ran(&mut keyboard)?;
        assert!(
            matches!(
                acts.as_slice(),
                [
                    Act::Screen {
                        act: crate::act::ScreenAct::Hide,
                        ..
                    },
                    Act::Notice { .. }
                ]
            ),
            "{acts:?}"
        );
        assert_eq!(keyboard.screen.screen, Screen::default());
        Ok(())
    }

    /// The actual worker and keyboard send a successful screen before its
    /// notice; a failed program discards its staged screen and keeps its error.
    #[test]
    fn a_jobs_notice_survives_its_screen() -> Result<(), Box<dyn std::error::Error>> {
        let socket = socket::checked("/nowhere/fux/s.sock", "test")?;
        let mut keyboard = new_keyboard(
            &socket,
            crate::script::default_state().map_err(|e| e.to_string())?,
            false,
        )?;
        let (mut session, client) = crate::session::testing::attached(24, 80)?;
        for (suffix, failed) in [("str hello", false), ("str boom fail", true)] {
            frame(
                &mut keyboard,
                ToAttached::Run(format!("me command-prompt {suffix}")),
            );
            let acts = all_ran(&mut keyboard)?;
            assert_eq!(
                acts.iter()
                    .filter(|act| matches!(act, Act::Screen { .. }))
                    .count(),
                usize::from(!failed)
            );
            session.apply(client, acts);
            let view = session.views.get(&client).ok_or("view")?;
            let notice = view.notice.as_ref().ok_or("notice")?;
            assert_eq!(notice.error, failed);
            assert!(notice.text.contains(if failed { "boom" } else { "hello" }));
            if let Some(act) = keyboard.screen.close() {
                session.apply(client, vec![act]);
            }
        }
        keyboard.read_only = true;
        frame(
            &mut keyboard,
            ToAttached::Run("me command-prompt str hello".into()),
        );
        let acts = all_ran(&mut keyboard)?;
        assert!(
            matches!(acts.as_slice(), [Act::Notice { text, error: true }] if text.contains("watching only") && text.contains("hello"))
        );
        Ok(())
    }

    /// A screen opens only if it fits a frame at its largest with the keys
    /// as they are when it opens; else it says so and opens nothing.
    #[test]
    fn a_screen_too_large_to_show_is_refused_as_it_opens() -> Result<(), Box<dyn std::error::Error>>
    {
        let socket = socket::checked("/nowhere/fux/s.sock", "test")?;
        let mut keyboard = new_keyboard(
            &socket,
            script::default_state().map_err(|e| e.to_string())?,
            false,
        )?;
        let prompt = crate::screens::Prompt {
            title: std::iter::repeat_n('t', crate::protocol::MAX_FRAME - 2048).collect(),
            line: crate::view::Line::new(String::new()),
            action: None,
        };
        let mut acts = Vec::new();
        keyboard.open(Screen::Prompt(prompt), &mut acts);
        assert!(
            matches!(
                acts.as_slice(),
                [Dispatch::Send(Act::Notice { error: true, .. })]
            ),
            "{acts:?}"
        );
        assert_eq!(keyboard.screen.screen, Screen::default());
        Ok(())
    }

    /// A binding's program, then the prefix, Esc (as `CSI 27 u`) and a key,
    /// stop the program and leave the key for after it, however the reads
    /// split them: the cancel is checked at each key, not at a read's end.
    #[test]
    fn a_stop_is_the_same_however_the_reads_split() -> Result<(), Box<dyn std::error::Error>> {
        let socket = socket::checked("/nowhere/fux/s.sock", "test")?;
        let mut state = script::default_state().map_err(|e| e.to_string())?;
        let forever = crate::keymap::Binding::new("begin false until".into(), true);
        state.keys.bind_root(KeyPress::char('y'), forever)?;
        let typed = b"y\x02\x1b[27ux";
        for split in 0..=typed.len() {
            let mut keyboard = new_keyboard(&socket, state.clone(), false)?;
            let now = Instant::now();
            let (first, then) = typed.split_at_checked(split).ok_or("past the end")?;
            let mut acts = bytes(&mut keyboard, first, now);
            acts.extend(bytes(&mut keyboard, then, now));
            let stopping = matches!(acts.as_slice(),
                [Act::Notice { text, error: false }] if text == "stopping…");
            let after = all_ran(&mut keyboard)?;
            let x = KeyPress::char('x');
            let stopped = matches!(after.as_slice(),
                [Act::Notice { text, error: false }, Act::Key(key)] if text == "stopped" && key.press == x);
            assert!(
                stopping && stopped,
                "split at {split}: {acts:?} then {after:?}"
            );
        }
        Ok(())
    }

    /// While a program runs, what waits for it is bounded: the inputs past
    /// `HELD` and the programs past `JOBS` are refused, and said to be;
    /// the prefix, then Esc, still stops it.
    #[test]
    fn what_waits_for_a_program_is_bounded() -> Result<(), Box<dyn std::error::Error>> {
        let socket = socket::checked("/nowhere/fux/s.sock", "test")?;
        let mut keyboard = new_keyboard(
            &socket,
            script::default_state().map_err(|e| e.to_string())?,
            false,
        )?;
        let forever = || ToAttached::Run("begin false until".into());
        for _ in 0..JOBS {
            assert!(frame(&mut keyboard, forever()).is_empty());
        }
        let notice = |acts: &[Act], says: &str| matches!(acts, [Act::Notice { text, error: true }] if text.starts_with(says));
        let more = frame(&mut keyboard, forever());
        assert!(
            notice(&more, "a program was not run: 64 already"),
            "{more:?}"
        );
        let typed = vec![b'x'; HELD + 2];
        let held = bytes(&mut keyboard, &typed, Instant::now());
        assert!(
            notice(&held, "2 inputs not taken: 1024 already"),
            "{held:?}"
        );
        let mut stopping = bytes(&mut keyboard, b"\x02\x1b", Instant::now());
        stopping.extend(timeout(&mut keyboard));
        assert!(
            matches!(stopping.as_slice(), [Act::Notice { text, error: false }] if text == "stopping…"),
            "{stopping:?}"
        );
        let after = all_ran(&mut keyboard)?;
        let keys = after.iter().filter(|a| matches!(a, Act::Key(_))).count();
        assert_eq!(keys, HELD);
        Ok(())
    }

    /// A program that prints more than a notice holds fails, said; one
    /// that leaves more is said not to be shown. Neither is kept.
    #[test]
    fn a_notice_is_bounded() -> Result<(), Box<dyn std::error::Error>> {
        let socket = socket::checked("/nowhere/fux/s.sock", "test")?;
        let mut keyboard = new_keyboard(
            &socket,
            script::default_state().map_err(|e| e.to_string())?,
            false,
        )?;
        let line: String = std::iter::repeat_n('x', 1000).collect();
        let code = format!("0 begin str {line} print 1 add 0 pick 100 eq until drop");
        assert!(frame(&mut keyboard, ToAttached::Run(code)).is_empty());
        let acts = all_ran(&mut keyboard)?;
        let failed = matches!(acts.as_slice(),
            [Act::Notice { text, error: true }] if text.contains("prints 65536 bytes at most"));
        assert!(failed, "{acts:?}");
        // What it leaves, the same string thrice, is made no further than
        // a notice holds.
        let long: String = std::iter::repeat_n('x', 30_000).collect();
        let left = format!("str {long} 0 pick 0 pick");
        assert!(frame(&mut keyboard, ToAttached::Run(left)).is_empty());
        let acts = all_ran(&mut keyboard)?;
        let refused = matches!(acts.as_slice(),
            [Act::Notice { text, error: true }] if text.starts_with("what the program left"));
        assert!(refused, "{acts:?}");
        Ok(())
    }

    /// A client that only watches is told of each key and paste it takes
    /// no further: a paste, and a key after the prefix but `d`, as a key.
    #[test]
    fn a_watcher_is_told_of_what_it_takes_no_further() -> Result<(), Box<dyn std::error::Error>> {
        let socket = socket::checked("/nowhere/fux/s.sock", "test")?;
        let mut keyboard = new_keyboard(
            &socket,
            script::default_state().map_err(|e| e.to_string())?,
            true,
        )?;
        for typed in [&b"x"[..], b"\x1b[200~p\x1b[201~", b"\x02c"] {
            let acts = bytes(&mut keyboard, typed, Instant::now());
            let notice = acts.first().filter(|_| acts.len() == 1);
            let told =
                matches!(notice, Some(Act::Notice { text, .. }) if text.contains("d detaches"));
            assert!(told, "{typed:?}: {acts:?}");
        }
        Ok(())
    }

    /// The server's log is a regular file alone: a FIFO no one reads is
    /// refused at once, not waited on, and a link is not followed.
    #[test]
    fn the_server_log_is_a_regular_file() -> Result<(), Box<dyn std::error::Error>> {
        let dir = fuxix::file::scratch()?.join(format!("fux-log-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;
        let fifo = dir.join("fifo.log");
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&fifo)
                .status()?
                .success()
        );
        let link = dir.join("link.log");
        std::os::unix::fs::symlink(dir.join("file.log"), &link)?;
        let refused = [open_log(&fifo).is_err(), open_log(&link).is_err()];
        let opened = open_log(&dir.join("file.log")).is_ok();
        std::fs::remove_dir_all(&dir)?;
        assert_eq!((refused, opened), ([true, true], true));
        Ok(())
    }

    #[test]
    fn terminal_entry_failures_restore_modes() -> Result<(), Box<dyn std::error::Error>> {
        const CHILD: &str = "FUX_TEST_ENTRY_FAILURE";
        const REPORT: &str = "fux-entry-restored\n";
        if std::env::var_os(CHILD).is_some() {
            struct Refused {
                flush: bool,
            }
            impl Write for Refused {
                fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                    if self.flush {
                        Ok(bytes.len())
                    } else {
                        Err(io::Error::other("entry write refused"))
                    }
                }
                fn flush(&mut self) -> io::Result<()> {
                    Err(io::Error::other("entry flush refused"))
                }
            }
            let stdin = io::stdin();
            let before = fuxix::terminal::attributes(&stdin)?.modes();
            for flush in [false, true] {
                let result = Raw::enter_with(&stdin, &mut Refused { flush });
                let error = result.err().ok_or("entry succeeded")?;
                let expected = if flush {
                    "entry flush refused"
                } else {
                    "entry write refused"
                };
                assert!(matches!(&error, Error::Output(e) if e.to_string() == expected));
                assert_eq!(fuxix::terminal::attributes(&stdin)?.modes(), before);
            }
            io::stdout().write_all(REPORT.as_bytes())?;
            return Ok(());
        }
        let size =
            fuxix::terminal::Size::from((std::num::NonZeroU16::MIN, std::num::NonZeroU16::MIN));
        let (_master, slave) = fuxix::pty::open(size)?;
        let output = std::process::Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "client::tests::terminal_entry_failures_restore_modes",
                "--test-threads",
                "1",
            ])
            .env(CHILD, "1")
            .stdin(std::process::Stdio::from(
                std::os::fd::AsFd::as_fd(&slave).try_clone_to_owned()?,
            ))
            .output()?;
        assert!(
            output.status.success() && String::from_utf8_lossy(&output.stdout).contains(REPORT),
            "child failed or did not run the restoration checks: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        Ok(())
    }

    /// Leaving sets no cursor shape: DECSCUSR 0 is a blinking block (ctlseqs),
    /// not the terminal's own, and the server gives back one a pane set.
    #[test]
    fn leaving_leaves_the_cursor_shape_alone() {
        assert!(!LEAVE.contains(" q"), "{LEAVE:?}");
    }

    /// What the client makes of the server's answer to Hello, as read from
    /// a socket in `wait`.
    fn answer(server: impl FnOnce(&mut UnixStream), wait: Duration) -> Result<String, Error> {
        let (client, mut peer) = UnixStream::pair()?;
        let mut client = client;
        server(&mut peer);
        let mut decoder = Decoder::default();
        let answer = read_frame(&mut client, &mut decoder, Some(wait));
        greeted(&ours(), answer, Ok(()))
    }

    fn ours() -> Hello {
        Hello {
            protocol: PROTOCOL,
            version: "0".into(),
            role: Role::Command,
        }
    }

    fn write(peer: &mut UnixStream, frame: &Greeting) {
        let _ = frame.encode().map(|bytes| peer.write_all(&bytes));
    }

    /// A server that refuses says why; one that says nothing times out;
    /// one that hangs up did not answer.
    #[test]
    fn the_client_tells_a_refusal_from_a_timeout() {
        let wait = Duration::from_millis(50);
        assert!(answer(|peer| write(peer, &Greeting::Hello(ours())), wait).is_ok());
        // A server of another protocol is refused before anything else is
        // sent to it.
        let other = Greeting::Hello(Hello {
            protocol: PROTOCOL + 1,
            ..ours()
        });
        let mismatched = answer(|peer| write(peer, &other), wait);
        assert!(matches!(&mismatched, Err(Error::Mismatch(m)) if m.protocol == PROTOCOL + 1));
        let refused = answer(|peer| write(peer, &Greeting::Exit("busy")), wait);
        assert!(matches!(&refused, Err(Error::Refused(reason)) if reason == "busy"));
        let silent = answer(|_| {}, wait);
        assert_eq!(
            silent.err().map(|e| e.to_string()).as_deref(),
            Some("the server did not answer in time")
        );
        let gone = answer(|peer| drop(peer.shutdown(std::net::Shutdown::Write)), wait);
        assert_eq!(
            gone.err().map(|e| e.to_string()).as_deref(),
            Some("the server did not answer")
        );
        let garbled = answer(|peer| drop(peer.write_all(&[0, 0, 0, 1, 99])), wait);
        assert!(matches!(
            garbled,
            Err(Error::Protocol(crate::protocol::Error::UnexpectedKind(99)))
        ));
        // A refusal wins over a failure to send the Hello.
        let both = greeted(&ours(), Ok(Some(Greeting::Exit("no"))), Err(no_answer()));
        assert!(matches!(both, Err(Error::Refused(_))));
    }
}
