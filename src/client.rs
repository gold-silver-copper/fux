//! The clients: an attached one, which reads its terminal's keys, runs
//! its own bindings, and sends the server typed actions, the server
//! painting its terminal (or relaying the paints, if it did not take it);
//! a headless one; and the connection a program's requests go over.
use crate::act::Act;
use crate::code::Code;
use crate::command::{Command, Request};
use crate::decode::Input;
use crate::keymap::Keymap;
use crate::keys::{Key, KeyPress, Modifiers};
use crate::protocol::{
    Asked, Attach, AttachedFrame, Context, Decoder, Frame, Greeting, Hello, Mismatch, PROTOCOL,
    Reply, Role, ToAttached, ToCommand,
};
use crate::screens::{Effect, Screen};
use crate::script;
use crate::socket::{self, SocketPath};
use fuxix::Errno;
use fuxix::poll::{Events as PollFlags, PollFd};
use fuxix::terminal::Termios;
use signal_hook::consts::SIGWINCH;
use std::collections::VecDeque;
use std::io::{self, ErrorKind, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::Mutex;
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
                f.write_str("fux attach needs a terminal on stdin and stdout, the same one")
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
    /// comes. A request that may start programs carries this process's
    /// environment, of which they take what `update-environment` names.
    pub fn ask(&mut self, asked: &Asked, out: &mut dyn Write) -> Result<Reply, Error> {
        wait_at_most(&self.stream, None);
        if starts_programs(&asked.request) && asked.context.env.is_empty() {
            let mut asked = asked.clone();
            asked.context.env = environment();
            send(&mut self.stream, &asked)?;
        } else {
            send(&mut self.stream, asked)?;
        }
        let mut parts = Vec::new();
        loop {
            while let Some(frame) = self.decoder.frame()? {
                match frame {
                    ToCommand::Stdout(bytes) => {
                        (out.write_all(&bytes).and_then(|()| out.flush()))
                            .map_err(Error::Output)?;
                    }
                    ToCommand::Part(bytes) => parts.extend_from_slice(&bytes),
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
/// and that pane's server. Its environment goes only with the requests
/// that may start programs (`Requests::ask`).
pub fn context() -> Context {
    let var = |name| std::env::var(name).ok().filter(|v: &String| !v.is_empty());
    Context {
        cwd: std::env::current_dir().unwrap_or_default(),
        pane: var("FUX_PANE").and_then(|p| p.parse().ok()),
        server: var("FUX_SERVER"),
        env: Vec::new(),
        client: None,
    }
}

/// Whether `request` may start programs: the only requests whose caller's
/// environment the server reads.
fn starts_programs(request: &Request) -> bool {
    let Request::Do(command) = request else {
        return false;
    };
    matches!(
        command,
        Command::NewWorkspace { .. }
            | Command::NewTab { .. }
            | Command::Run { .. }
            | Command::Respawn { .. }
            | Command::Split { .. }
    )
}

/// This process's environment, its values as the bytes they are, its
/// names as far as they are UTF-8 (as every name `update-environment`
/// takes is): the server gives panes started for it what that names of it.
pub fn environment() -> Vec<(String, std::ffi::OsString)> {
    std::env::vars_os()
        .filter_map(|(k, v)| Some((k.into_string().ok()?, v)))
        .collect()
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

/// Starts a server in the background, in a new session with its output in
/// `fux.log` beside the socket, and waits for it to answer.
pub fn start_server(socket: &SocketPath) -> Result<(), Error> {
    let exe = std::env::current_exe().map_err(failed("finding the fux binary"))?;
    socket::private_directory(socket, true)?;
    let log = socket.directory().join("fux.log");
    let output = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)
        .map_err(failed("the log file"))?;
    let mut command = std::process::Command::new(exe);
    command
        .arg(crate::SERVE)
        .arg(socket.path())
        .current_dir("/")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(output);
    // A daemon (daemon(7)): a session of its own, no descriptors of the
    // client's but stdio, no directory kept busy.
    fuxix::process::lead_session(&mut command, false);
    let mut child = command.spawn().map_err(failed("starting a server"))?;
    let deadline = crate::after(Instant::now(), Duration::from_secs(2));
    let see_log = |what: String| Error::Failed(format!("{what}; see {}", log.display()));
    loop {
        if answers(socket) {
            // Dropping a `Child` neither waits for nor kills it.
            drop(child);
            return Ok(());
        }
        if let Ok(Some(status)) = child.try_wait() {
            // It lost a race to another server, or failed: connect to the
            // winner if there is one.
            if answers(socket) {
                return Ok(());
            }
            return Err(see_log(format!("the server exited ({status})")));
        }
        if Instant::now() > deadline {
            return Err(see_log("the server did not start within 2 s".to_owned()));
        }
        std::thread::sleep(Duration::from_millis(20));
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
/// keys, its config run already, and what was wrong with that config.
pub struct Attaching {
    pub workspace: Option<crate::command::WsRef>,
    pub read_only: bool,
    pub keymap: Keymap,
    pub problem: Option<String>,
}

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

/// An attached client's own keys: its keymap, the screen it has open or
/// what its keys are in the middle of, the decoder its terminal's bytes go
/// through, and the worker that runs the programs its bindings and screens
/// give, one after another, as the client. What each takes, it turns into
/// the actions to send the server.
///
/// Every mode is the client's (`crate::screens`), changed here at the key
/// that changes it. A program it runs may open a screen, so the keys typed
/// while any runs wait for it, and go to the screen it opened, if any.
pub struct Keyboard {
    keymap: Keymap,
    screen: Screen,
    read_only: bool,
    decoder: crate::decode::Decoder,
    jobs: std::sync::mpsc::Sender<Job>,
    told: std::sync::mpsc::Receiver<Ran>,
    wake: io::PipeReader,
    /// Programs sent to the worker and not yet done.
    running: usize,
    /// What stops the program the worker runs (the prefix, then Esc).
    stop: script::Stop,
    /// What was typed while a program ran, in order.
    waiting: VecDeque<Input>,
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
        let (wake, mut poke) = io::pipe()?;
        fuxix::io::set_nonblocking(&wake, true).map_err(io::Error::from)?;
        let path = socket.path().to_string_lossy().into_owned();
        let stop = script::Stop::default();
        let stops = stop.clone();
        let spawned = std::thread::Builder::new()
            .name("keys".into())
            .spawn(move || {
                let chosen = socket::Choice::Path(path);
                let mut server = script::Server::stoppable(chosen, stops.clone());
                for job in todo {
                    let ran = run_job(&mut server, me, job, &stops);
                    if tell.send(ran).is_err() {
                        break;
                    }
                    let _ = poke.write_all(b"!");
                }
            });
        spawned?;
        let mut decoder = crate::decode::Decoder::default();
        // The server asks the terminal about itself as the client attaches.
        decoder.expect(Instant::now());
        Ok(Keyboard {
            keymap,
            screen: Screen::default(),
            read_only,
            decoder,
            jobs,
            told,
            wake,
            running: 0,
            stop,
            waiting: VecDeque::new(),
        })
    }

    /// Whether nothing waits: no program running, no keys held for one.
    pub fn idle(&self) -> bool {
        self.running == 0 && self.waiting.is_empty()
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
        self.decoder.timeout(&mut inputs);
        self.inputs(inputs)
    }

    fn inputs(&mut self, inputs: Vec<Input>) -> Vec<Act> {
        self.waiting.extend(inputs);
        let mut acts = Vec::new();
        // The prefix, then Esc, typed while a program runs, stops it.
        if self.running > 0 && self.held_stop() {
            self.waiting.pop_back();
            self.waiting.pop_back();
            self.stop.stop();
            let text = "stopping…".to_owned();
            acts.push(Act::Notice { text, error: false });
        }
        acts.extend(self.release());
        acts
    }

    /// Whether the keys waiting end with the prefix then Esc.
    fn held_stop(&self) -> bool {
        let mut last = self.waiting.iter().rev();
        let is = |input: Option<&Input>, key: KeyPress| matches!(input, Some(Input::Key(stroke)) if stroke.press == key);
        let escape = KeyPress::new(Key::Escape, Modifiers::NONE);
        is(last.next(), escape) && is(last.next(), self.keymap.prefix)
    }

    /// What waits, taken unless a program runs.
    fn release(&mut self) -> Vec<Act> {
        let mut acts = Vec::new();
        while self.running == 0 {
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
    pub fn frame(&mut self, frame: ToAttached<'_>) -> Vec<Act> {
        match frame {
            ToAttached::Ended(kind) => self.screen.ended(kind),
            ToAttached::Run(code) => self.run(code),
            ToAttached::Keys(typed) => self.typed(typed),
            // The attachment's own: `attached` reads them.
            ToAttached::Welcome(..) | ToAttached::Paint(_) | ToAttached::Exit(_) => {}
        }
        self.release()
    }

    /// What the programs it ran came to, those done: a screen one opened
    /// opens, what one said shows, and the keys that waited go on.
    pub fn ran(&mut self) -> Vec<Act> {
        crate::drain(&self.wake);
        let mut acts = Vec::new();
        while let Ok(ran) = self.told.try_recv() {
            self.running = self.running.saturating_sub(1);
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
            let text = "watching only: no choosers, prompts or copy mode here".to_owned();
            acts.push(Act::Notice { text, error: true });
            return;
        }
        if let Screen::Copy(_) = self.screen {
            acts.push(Act::Copy(crate::act::CopyAct::Leave));
        }
        acts.extend(screen.opened());
        self.screen = screen;
    }

    /// What a key or a paste led to: actions for the server, programs to run.
    fn apply(&mut self, effects: Vec<Effect>, acts: &mut Vec<Act>) {
        for effect in effects {
            match effect {
                Effect::Send(act) => acts.push(act),
                Effect::Run(code) => self.run(code),
            }
        }
    }

    fn run(&mut self, code: String) {
        let keymap = self.keymap.clone();
        // A stop stops the programs queued with the one it was typed for.
        if self.running == 0 {
            self.stop.reset();
        }
        if self.jobs.send(Job { code, keymap }).is_ok() {
            self.running = self.running.saturating_add(1);
        }
    }

    /// What the terminal sent, decoded (`Screen::input`).
    fn input(&mut self, input: Input, acts: &mut Vec<Act>) {
        let effects = self.screen.input(&self.keymap, input, self.read_only);
        self.apply(effects, acts);
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
                    let keys = text.chars().map(|c| Input::Key(KeyPress::char(c).into()));
                    self.waiting.extend(keys);
                }
                crate::command::Input::Paste(text) => self.waiting.push_back(Input::Paste(text)),
                // A pane's alone: `send-keys` refuses them for a client.
                crate::command::Input::Bytes(_) => {}
            }
        }
    }
}

/// Runs a key's or a screen's program as client `me`.
fn run_job(
    server: &mut script::Server,
    me: crate::id::ClientId,
    job: Job,
    stop: &script::Stop,
) -> Ran {
    let Job { code, keymap } = job;
    let mut changed = keymap.clone();
    let mut out = Vec::new();
    let mut context = context();
    context.client = Some(me);
    let mut asks = script::AsClient {
        asks: server,
        context: context.clone(),
        opened: None,
    };
    let result = (|| {
        let tokens = fuxth::token::source("key", &code)?;
        let mut machine = script::compiled(keymap.config.as_ref(), &tokens)?;
        let mut host = script::Host::new(context, Some(&mut changed), &mut out);
        script::run_capped(
            &mut machine,
            &mut host,
            &mut asks,
            Some(script::PROGRAM_STEPS),
            Some(stop),
        )
    })();
    let notice = match result {
        // Stopped as asked (the prefix, then Esc): no failure of its own.
        Err(_) if stop.stopped() => Some(("stopped".to_owned(), false)),
        Err(failed) => Some((failed.to_string(), true)),
        Ok(stack) => {
            let mut said = String::from_utf8_lossy(&out).into_owned();
            for value in &stack {
                said.push_str(&value.text());
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
        size,
        workspace: attaching.workspace.clone(),
        read_only: attaching.read_only,
        env: environment(),
    };
    // The terminal goes with the `Attach`: if the server takes it, it
    // writes the paints there itself. The keys are the client's either way.
    let sent = match terminal {
        true => send_with_terminal(&mut stream, &attach, &stdin),
        false => send(&mut stream, &attach),
    };
    let result = sent.and_then(|()| {
        attached(
            socket,
            &mut stream,
            &mut decoder,
            &signals,
            &attaching,
            terminal,
        )
    });
    drop(raw);
    let _ = writeln!(io::stdout(), "[{}]", result?);
    Ok(())
}

/// Why a client detached when a signal told it to.
const STOPPED: &str = "detached by a signal";

/// The signals an attached client answers: a new size, and those that
/// detach it.
struct Signals {
    winch: crate::SignalPipe,
    stops: crate::SignalPipe,
}

impl Signals {
    fn new() -> Result<Signals, Error> {
        Ok(Signals {
            winch: crate::SignalPipe::new(&[SIGWINCH])?,
            stops: crate::SignalPipe::new(&crate::STOPS)?,
        })
    }
}

/// The terminal in raw mode on the alternate screen, which puts the
/// terminal back as it was once dropped, however the attachment ends.
struct Raw;

impl Raw {
    fn enter(stdin: &io::Stdin) -> Result<Raw, Error> {
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
        let mut stdout = io::stdout();
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
    stream: &mut UnixStream,
    frame: &impl Frame<'a>,
    terminal: impl std::os::fd::AsFd,
) -> Result<(), Error> {
    let bytes = frame.encode()?;
    let sent = loop {
        match fuxix::socket::send_with_fd(&*stream, &bytes, &terminal) {
            Ok(n) => break n,
            Err(Errno::INTR) => continue,
            Err(errno) => return Err(failed("writing to the server")(errno)),
        }
    };
    stream
        .write_all(bytes.get(sent..).unwrap_or_default())
        .map_err(failed("writing to the server"))
}

/// Holds the attachment until the server ends it, and says why: reads the
/// terminal's keys, with `terminal`, and runs them through the client's
/// keys; writes the paints the server relays; passes on the terminal's new
/// size, and a signal that detaches. Until the server says the client's id
/// (its `Welcome`), an `Exit` is a refusal.
fn attached(
    socket: &SocketPath,
    stream: &mut UnixStream,
    decoder: &mut Decoder,
    signals: &Signals,
    attaching: &Attaching,
    terminal: bool,
) -> Result<String, Error> {
    let Signals { winch, stops } = signals;
    wait_at_most(stream, None);
    let stdin = io::stdin();
    let mut stdout = io::stdout();
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
                (ToAttached::Paint(bytes), _) => {
                    if terminal {
                        stdout
                            .write_all(&bytes)
                            .map_err(failed("writing the terminal"))?;
                    }
                }
                (ToAttached::Exit(reason), None) => return Err(Error::Refused(reason.to_owned())),
                (ToAttached::Exit(reason), Some(_)) => return Ok(reason.to_owned()),
                (ToAttached::Welcome(me, _), None) => {
                    if !terminal {
                        (writeln!(stdout, "{me}").and_then(|()| stdout.flush()))
                            .map_err(Error::Output)?;
                    }
                    let keymap = attaching.keymap.clone();
                    keyboard = Some(Keyboard::new(socket, me, keymap, attaching.read_only)?);
                    if let Some(text) = attaching.problem.clone() {
                        acts.push(Act::Notice { text, error: true });
                    }
                }
                (frame, Some(keyboard)) => acts.extend(keyboard.frame(frame)),
                (_, None) => {}
            }
        }
        stdout.flush().map_err(failed("writing the terminal"))?;
        for act in std::mem::take(&mut acts) {
            let act = AttachedFrame::Act(Box::new(act));
            if let Err(ended) = tell(stream, decoder, keyboard.is_some(), terminal, &act) {
                return ended;
            }
        }
        let mut fds = vec![
            PollFd::new(&*stream, PollFlags::IN),
            PollFd::new(winch.pipe(), PollFlags::IN),
            PollFd::new(stops.pipe(), PollFlags::IN),
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
            Ok(_) | Err(Errno::INTR) => {}
            Err(e) => return Err(failed("poll")(e)),
        }
        let ready: Vec<bool> = fds.iter().map(|fd| !fd.revents().is_empty()).collect();
        drop(fds);
        let is = |i: usize| ready.get(i).copied().unwrap_or(false);
        if is(STOPS) {
            // What the server paints until it lets go is written before the
            // terminal is put back, not after, on the shell.
            if send(stream, &AttachedFrame::Detach).is_ok() && keyboard.is_some() {
                let _ = last_frames(stream, decoder, true, terminal);
            }
            return Ok(STOPPED.into());
        }
        if is(WINCH) {
            crate::drain(winch.pipe());
            // A terminal that says no size now keeps the last it said.
            if terminal && let Some(size) = window_size() {
                let resize = AttachedFrame::Resize(size);
                if let Err(ended) = tell(stream, decoder, keyboard.is_some(), terminal, &resize) {
                    return ended;
                }
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
                    Err(Errno::INTR | Errno::AGAIN) => {}
                    Err(e) => return Err(failed("reading the terminal")(e)),
                }
            }
        }
        if is(STREAM) && !read_more(stream, decoder)? {
            return Ok("the server closed the connection".into());
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
    terminal: bool,
    frame: &AttachedFrame,
) -> Result<(), Result<String, Error>> {
    let Err(error) = send(stream, frame) else {
        return Ok(());
    };
    Err(last_frames(stream, decoder, welcomed, terminal).unwrap_or(Err(error)))
}

/// Reads the server's frames up to its `Exit`, waiting `ANSWER` at
/// most for each read, and writes their paints if the client has a
/// `terminal`: how the attachment ended, the `Exit`'s reason (a refusal,
/// before its `Welcome`), or `None` if the server said nothing more.
fn last_frames(
    stream: &mut UnixStream,
    decoder: &mut Decoder,
    welcomed: bool,
    terminal: bool,
) -> Option<Result<String, Error>> {
    wait_at_most(stream, Some(ANSWER));
    let mut stdout = io::stdout();
    loop {
        while let Some(frame) = decoder.frame().ok()? {
            match frame {
                ToAttached::Paint(bytes) if terminal => {
                    let _ = stdout.write_all(&bytes).and_then(|()| stdout.flush());
                }
                ToAttached::Exit(reason) if welcomed => return Some(Ok(reason.to_owned())),
                ToAttached::Exit(reason) => return Some(Err(Error::Refused(reason.to_owned()))),
                ToAttached::Paint(_)
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

    /// Keys typed while any program the client ran is running wait for it,
    /// whatever its words (a config's word may open a chooser); the prefix,
    /// then Esc, stops it, and the keys go on once it is done.
    #[test]
    fn keys_wait_for_a_running_program_till_it_is_done() -> Result<(), Box<dyn std::error::Error>> {
        let socket = socket::checked("/nowhere/fux/s.sock", "test")?;
        let mut keyboard = Keyboard::new(&socket, "c1".parse()?, Keymap::empty(), false)?;
        let now = Instant::now();
        let run = ToAttached::Run("begin false until".into());
        assert!(keyboard.frame(run).is_empty());
        assert!(keyboard.bytes(b"x", now).is_empty(), "a key went past it");
        let mut stopping = keyboard.bytes(b"\x02\x1b", now);
        stopping.extend(keyboard.timeout());
        assert!(
            matches!(stopping.as_slice(), [Act::Notice { error: false, .. }]),
            "{stopping:?}"
        );
        let mut done = [PollFd::new(keyboard.wake(), PollFlags::IN)];
        fuxix::poll::poll(&mut done, Some(ANSWER))?;
        let acts = keyboard.ran();
        assert!(matches!(acts.last(), Some(Act::Key(_))), "{acts:?}");
        assert!(keyboard.idle());
        Ok(())
    }

    /// A client that only watches is told of each key and paste it takes
    /// no further: a paste, and a key after the prefix but `d`, as a key.
    #[test]
    fn a_watcher_is_told_of_what_it_takes_no_further() -> Result<(), Box<dyn std::error::Error>> {
        let socket = socket::checked("/nowhere/fux/s.sock", "test")?;
        let mut keyboard = Keyboard::new(&socket, "c1".parse()?, Keymap::empty(), true)?;
        for typed in [&b"x"[..], b"\x1b[200~p\x1b[201~", b"\x02c"] {
            let acts = keyboard.bytes(typed, Instant::now());
            let notice = acts.first().filter(|_| acts.len() == 1);
            let told =
                matches!(notice, Some(Act::Notice { text, .. }) if text.contains("d detaches"));
            assert!(told, "{typed:?}: {acts:?}");
        }
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
