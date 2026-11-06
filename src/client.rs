//! The clients: an attached one, which reads its terminal's keys, runs
//! its own bindings, and sends the server typed actions, writing the
//! paints the server relays to its terminal;
//! a headless one; and the connection a program's requests go over.
use crate::client_state::ClientState;
use crate::screens::{Dispatch, Display, Job, Screen};
use crate::script;
use fux_server::socket::{self, SocketPath};
use fux_vt::keys::decode::Input;
use fux_vt::keys::{Key, KeyPress, Modifiers};
use fux_wire::act::Act;
use fux_wire::code::Code;
use fux_wire::command::ClientRead;
#[cfg(all(test, unix))]
use fux_wire::command::Command;
use fux_wire::command::Request;
use fux_wire::protocol::{
    Asked, Attach, AttachedFrame, Context, Decoder, Frame, Greeting, Hello, Mismatch, PROTOCOL,
    Reply, Role, ToAttached, ToCommand,
};
use fuxix::Errno;
use fuxix::poll::{Events as PollFlags, PollFd};
use socket::Client;
use std::cell::Cell;
use std::collections::VecDeque;
use std::io::{self, BufRead, ErrorKind, Read, Write};
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
    /// The server speaks another protocol: what it said, and its process,
    /// where the system says it (`fuxix::socket::peer`).
    Mismatch {
        said: Mismatch,
        server: Option<fuxix::process::Pid>,
    },
    /// And this command could not run with the server's own binary
    /// instead (`follow`): why.
    Unfollowed {
        said: Mismatch,
        why: String,
    },
    Protocol(fux_wire::protocol::Error),
    NotATerminal,
    /// The terminal on stdin can't be waited on (`waitable`).
    Unwaitable,
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
            Error::Mismatch { .. } | Error::Unfollowed { .. } => Code::Mismatch,
            Error::Output(_) => Code::Output,
            Error::Socket(_)
            | Error::Refused(_)
            | Error::Protocol(_)
            | Error::NotATerminal
            | Error::Unwaitable
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
            Error::Mismatch { said, .. } => said.fmt(f),
            Error::Unfollowed { said, why } => {
                write!(f, "{said}. Running the server's own binary instead cannot be: {why}")
            }
            Error::Protocol(error) => error.fmt(f),
            Error::NotATerminal => write!(
                f,
                "attaching needs a terminal on stdin and stdout, the same one{}",
                socket::NOT_A_TERMINAL
            ),
            Error::Unwaitable => f.write_str(
                "the terminal on stdin can't be waited on for keys (macOS's poll says POLLNVAL \
                 of /dev/tty): give stdin as the terminal's own device, /dev/ and what \
                 `ps -o tty= -p $$` says",
            ),
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

impl From<fux_wire::protocol::Error> for Error {
    fn from(error: fux_wire::protocol::Error) -> Error {
        Error::Protocol(error)
    }
}

/// Whether the terminal is on the alternate screen, which the panic hook
/// leaves too.
static ENTERED: AtomicBool = AtomicBool::new(false);
fn leave() {
    if ENTERED.swap(false, Ordering::SeqCst) {
        let mut out = io::stdout();
        let _ = out.write_all(LEAVE.as_bytes());
        let _ = out.flush();
    }
}

/// How long a client waits for a server's answer: its Hello, or its last
/// frames once the client could not write.
const ANSWER: Duration = Duration::from_secs(5);

/// The socket of the server at `socket`, once it is checked to be served by
/// this user, and the server's process, where the system says it. fux is
/// reached through its socket alone: where the system denies `connect` (a
/// sandbox), it cannot be.
fn open(socket: &SocketPath) -> Result<(Client, Option<fuxix::process::Pid>), Error> {
    // Said of the socket chosen, which a hint starts again.
    let failed = |source| Error::Connect {
        path: socket.path().to_owned(),
        source,
    };
    socket::check_client_socket(socket)?;
    let stream = socket::dial(socket).map_err(failed)?;
    // Its server is this user's, whoever made the path's directories.
    match socket::client_peer(&stream) {
        Ok(peer) if peer.user.is_current() => Ok((stream, peer.pid)),
        Ok(peer) => Err(io::Error::other(format!(
            "its server runs as {}, not this user's ({})",
            peer.user,
            socket::this_user(),
        ))),
        Err(errno) => Err(errno.into()),
    }
    .map_err(failed)
}

/// A connection to the server at `socket` as `role`, greeted, with the
/// server's version.
fn connect(socket: &SocketPath, role: Role) -> Result<(Client, Decoder, String), Error> {
    let (mut stream, server) = open(socket)?;
    let ours = Hello::ours(role);
    let sent = send(&mut stream, &ours);
    let mut decoder = Decoder::default();
    // A refusal may close the connection before the Hello is written: its
    // reason is still there to read.
    let answer = read_frame(&mut stream, &mut decoder, Some(ANSWER));
    let version = greeted(&ours, server, answer, sent)?;
    Ok((stream, decoder, version))
}

/// The server's version, if its answer to our Hello, `ours`, and sending
/// it let the client go on: a refusal, which may come before the Hello is
/// read, wins; a server of another protocol, its process `server`, is
/// refused before anything it would not understand is sent.
fn greeted(
    ours: &Hello,
    server: Option<fuxix::process::Pid>,
    answer: Result<Option<Greeting>, Error>,
    sent: Result<(), Error>,
) -> Result<String, Error> {
    match (answer, sent) {
        (Ok(Some(Greeting::Exit(reason))), _) => Err(Error::Refused(reason.to_owned())),
        (Ok(Some(Greeting::Hello(theirs))), sent) => {
            Mismatch::between(ours, &theirs).map_err(|said| Error::Mismatch { said, server })?;
            sent.map(|()| theirs.version.to_owned())
        }
        (_, Err(error)) | (Err(error), Ok(())) => Err(error),
        (Ok(None), Ok(())) => Err(no_answer()),
    }
}

/// Reads on `stream` wait `timeout` at most from now on (`None`, for
/// ever). macOS refuses a timeout on a socket its peer has shut: a read
/// there cannot wait, as what the server left is read at once and then its
/// end, so the refusal changes nothing.
fn wait_at_most(stream: &Client, timeout: Option<Duration>) {
    let _ = stream.set_read_timeout(timeout);
}

fn send<'a>(stream: &mut Client, frame: &impl Frame<'a>) -> Result<(), Error> {
    stream
        .write_all(&frame.encode())
        .map_err(failed("writing to the server"))
}

/// The next frame the server sends, waiting `timeout` at most for each
/// read; none once it has closed.
fn read_frame<'d, F: Frame<'d>>(
    stream: &mut Client,
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
fn read_more(stream: &mut Client, decoder: &mut Decoder) -> Result<bool, Error> {
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
pub(crate) struct Requests {
    stream: Client,
    decoder: Decoder,
}

impl Requests {
    pub fn open(socket: &SocketPath) -> Result<Requests, Error> {
        let (stream, decoder, _) = connect(socket, Role::Command)?;
        Ok(Requests { stream, decoder })
    }

    /// Sends one request and reads its reply in pieces. With `out`, a
    /// command line's: if stdout's reader goes while a wait or an event
    /// read waits (a closed pipe: `fux all str '' events | head -1`), it is
    /// set, and the wait fails. An action is never stopped so: it acts, and
    /// a closed pipe is met as its answer is written, as `cat` meets one.
    pub fn ask(&mut self, asked: &Asked, out: Option<&Cell<bool>>) -> Result<Reply, Error> {
        let waits = matches!(asked.request, Request::Wait(_) | Request::NextEvents { .. });
        let out = out.filter(|_| waits);
        wait_at_most(&self.stream, None);

        send(&mut self.stream, asked)?;
        let mut parts = Vec::new();
        loop {
            while let Some(frame) = self.decoder.frame()? {
                match frame {
                    ToCommand::Part(bytes) => parts.extend_from_slice(&bytes),
                    ToCommand::PartsEnd => {
                        let reply = fux_wire::wire::Reader::new(&parts).whole::<Reply>();
                        return Ok(reply.map_err(fux_wire::protocol::Error::Wire)?);
                    }
                    ToCommand::Exit(reason) => return Err(Error::Refused(reason.to_owned())),
                }
            }
            if let Some(out) = out
                && reader_gone(&self.stream)?
            {
                out.set(true);
                return Err(Error::Output(ErrorKind::BrokenPipe.into()));
            }
            if !read_more(&mut self.stream, &mut self.decoder)? {
                let closed = "the server closed the connection";
                return Err(Error::Failed(closed.to_owned()));
            }
        }
    }

    /// What stops it from another thread: shut down, the request under way
    /// ends as if the server had closed the connection.
    pub fn stopper(&self) -> Option<Client> {
        self.stream.try_clone().ok()
    }
}

/// Waits until the server sends more on `stream`, or stdout's reader is
/// gone, and says which. Only a pipe's is watched: its reader gone, Linux
/// says `POLLERR`, and macOS, asked for `POLLHUP`, says that; Windows has
/// no such word, so its pipe is asked at each `STDOUT_LOOK`.
fn reader_gone(stream: &Client) -> Result<bool, Error> {
    if !fuxix::io::stdout_is_fifo() {
        return Ok(false);
    }
    let out = io::stdout();
    loop {
        let mut fds = [
            PollFd::new(stream, PollFlags::IN),
            PollFd::new(&out, PollFlags::HUP),
        ];
        match fuxix::poll::poll(&mut fds, fuxix::io::STDOUT_LOOK) {
            Ok(0) => {}
            Ok(_) => {
                let [_, out] = fds;
                return Ok(out.revents().intersects(PollFlags::ERR | PollFlags::HUP));
            }
            Err(Errno::INTR) => {}
            Err(e) => return Err(failed("poll")(e)),
        }
    }
}

/// Where requests from this process come from: its directory, its pane
/// and that pane's server. Launch environment is carried in the request.
pub fn context() -> Context {
    let var = |name| std::env::var(name).ok().filter(|v: &String| !v.is_empty());
    Context {
        cwd: std::env::current_dir().unwrap_or_default(),
        pane: var("FUX_PANE").and_then(|p| fux_wire::id::program(&p)),
        server: var("FUX_SERVER"),
        client: None,
        of: None,
    }
}

/// What answers on a socket: nothing, or a server, its protocol and
/// version, and its process, where the system says it.
pub enum Probe {
    Absent,
    /// A socket left by a server that stopped.
    Left,
    Server {
        protocol: u32,
        version: String,
        pid: Option<fuxix::process::Pid>,
    },
}

/// Who answers on `socket`, asking only for its `Hello`, which every
/// version answers.
pub fn probe(socket: &SocketPath) -> Result<Probe, Error> {
    match connect(socket, Role::Command) {
        Ok((stream, _, version)) => Ok(Probe::Server {
            protocol: PROTOCOL,
            version,
            pid: socket::client_peer(&stream).ok().and_then(|peer| peer.pid),
        }),
        Err(Error::Mismatch {
            said: Mismatch { protocol, version },
            server,
        }) => Ok(Probe::Server {
            protocol,
            version,
            pid: server,
        }),
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

/// A command line as it runs again with another binary (`follow`): the
/// process's arguments, its first among them, and the program it read
/// from stdin, if it did.
pub struct Rerun {
    pub argv: Vec<std::ffi::OsString>,
    pub program: Option<String>,
}

/// Runs a command line, `rerun`, again in this process, with the binary of
/// the server that answered it in another protocol (`Error::Mismatch`),
/// which speaks the server's: its process's `/proc/PID/exe`, which is the
/// file it runs even once that was replaced or removed. The same
/// arguments, environment, stdout and stderr, and the same stdin, or the
/// program as stdin where the command line read it from there; the
/// binary's exit status is this command's. Said on stderr first. A
/// command follows once: the binary runs with `FUX_FOLLOWED` set to this
/// process's ID, which an exec keeps, so a process that finds its own ID
/// there followed already; a process it starts has another. Returns only
/// if it cannot be, saying why.
pub fn follow(said: Mismatch, server: Option<fuxix::process::Pid>, rerun: &Rerun) -> Error {
    let Err(why) = followed(&said, server, rerun);
    Error::Unfollowed { said, why }
}

fn followed(
    said: &Mismatch,
    server: Option<fuxix::process::Pid>,
    rerun: &Rerun,
) -> Result<std::convert::Infallible, String> {
    once(
        std::env::var_os(FOLLOWED).as_deref(),
        &socket::follower_id().to_string(),
    )?;
    let pid = server.ok_or("the system does not say which process the server is")?;
    let (exe, path, same) = socket::server_binary(pid)?;
    // Run again, it would meet the same server, and follow it again.
    if same {
        return Err(format!(
            "the server runs this same binary, {}",
            path.display()
        ));
    }
    let (arg0, args) = rerun.argv.split_first().ok_or("no command line to run")?;
    let mut command = std::process::Command::new(&exe);
    command
        .args(args)
        .env(FOLLOWED, std::process::id().to_string());
    if let Some(program) = &rerun.program {
        let file = program_file(program).map_err(|e| format!("keeping the program: {e}"))?;
        command.stdin(file);
    }
    let theirs = fux_wire::protocol::Build {
        version: &said.version,
        protocol: said.protocol,
    };
    let ours = fux_wire::protocol::Build::OURS;
    let _ = writeln!(
        io::stderr(),
        "fux: the server runs {theirs} and this client {ours}: running the server's own binary, {}",
        path.display()
    );
    let error = socket::rerun(command, arg0);
    Err(format!("running {}: {error}", path.display()))
}

/// The variable a followed command runs with: the ID of its process.
const FOLLOWED: &str = "FUX_FOLLOWED";

/// Whether this process, of ID `us`, may follow, `FOLLOWED` being `marked`:
/// not once it has.
fn once(marked: Option<&std::ffi::OsStr>, us: &str) -> Result<(), String> {
    match marked.is_some_and(|marked| marked == us) {
        true => Err(format!(
            "this command already runs with another server's binary ({FOLLOWED}), and follows once"
        )),
        false => Ok(()),
    }
}

/// A file holding `program`, open to read from its start, for this
/// process alone (`fuxix::file::anonymous`).
fn program_file(program: &str) -> io::Result<std::fs::File> {
    use std::io::Seek;
    let mut file = fuxix::file::anonymous("fux-program")?;
    file.write_all(program.as_bytes())?;
    file.rewind()?;
    Ok(file)
}

/// Whether the server at `socket` speaks this protocol: one Hello, then
/// nothing; `Error::Mismatch` where it does not.
pub fn greet(socket: &SocketPath) -> Result<(), Error> {
    connect(socket, Role::Command).map(drop)
}

/// Whether a server answers on `socket` (`socket::owned`).
pub fn answers(socket: &SocketPath) -> bool {
    socket::owned(socket)
}

/// Starts a server in the background, its journal keeping `journal`
/// events, with its log in `fux.log` beside the socket (`socket::start_server`
/// says how each OS does), and waits for the line it writes once it listens
/// (`server::READY`), showing each notice it says first as it comes, for a
/// download's wait, and giving them back, as the alternate screen would hide
/// them. Its stdout closing first means it failed, or found another server
/// on the socket: one that answers, as a server that starts waits for the
/// one starting before it (`socket::bind_socket`). A server that hangs
/// before it listens holds the client too.
pub fn start_server(
    socket: &SocketPath,
    journal: fux_wire::command::JournalEntries,
) -> Result<Vec<String>, Error> {
    let exe = std::env::current_exe().map_err(failed("finding the fux binary"))?;
    socket::private_directory(socket, true)?;
    let log = socket.directory().join("fux.log");
    let output =
        socket::open_log(&log).map_err(|e| Error::Failed(format!("{}: {e}", log.display())))?;
    let entries = journal.get().to_string();
    let args = [
        std::ffi::OsStr::new(crate::SERVE),
        socket.path().as_os_str(),
        std::ffi::OsStr::new(&entries),
    ];
    // A follow's mark is this command's, not its panes' (`follow`).
    let mut child = socket::start_server(&exe, &args, socket, output, FOLLOWED)
        .map_err(failed("starting a server"))?;
    let mut said = Vec::new();
    let mut notices = Vec::new();
    if let Some(stdout) = child.output() {
        let mut lines = io::BufReader::new(stdout.take(64 << 10));
        loop {
            said.clear();
            let line = lines.read_until(b'\n', &mut said);
            line.map_err(failed("reading the server"))?;
            let text = String::from_utf8_lossy(&said);
            let Some(notice) = text.trim_end().strip_prefix(fux_server::server::NOTICE) else {
                break;
            };
            let _ = writeln!(io::stderr(), "fux: {notice}");
            notices.push(notice.to_owned());
        }
    }
    // Dropping what started it neither waits for nor kills it.
    if said == fux_server::server::READY || answers(socket) {
        return Ok(notices);
    }
    let status = child.ended().map_err(failed("waiting for the server"))?;
    // A server that refused to start says why.
    let refused = String::from_utf8_lossy(&said);
    let why = match refused.trim_end().strip_prefix("fux server: ") {
        Some(why) => why.to_owned(),
        None => format!("the server exited ({status}); see {}", log.display()),
    };
    Err(Error::Failed(why))
}

/// The rows and columns of the terminal on stdin, which the client reads
/// and the server paints, if it says them: one that says none (or 0 of
/// either) has no size fux could lay panes out on.
pub(crate) fn window_size() -> Option<fux_vt::Size> {
    fuxix::terminal::stdin_size().map(|(s, _)| (s.rows, s.cols).into())
}

/// Whether stdin and stdout are one terminal: the client reads keys from
/// stdin, which the server paints, and sets the terminal's modes through
/// stdout.
pub(crate) fn on_one_terminal() -> bool {
    fuxix::terminal::on_one_terminal()
}

/// Whether the terminal on stdin can be waited on for keys: macOS's poll
/// says POLLNVAL of `/dev/tty`, never that a key is there, so the client,
/// reading each time it is told, would wait in the read for a key.
pub(crate) fn waitable() -> bool {
    let stdin = io::stdin();
    let mut fds = [PollFd::new(&stdin, PollFlags::IN)];
    fuxix::poll::poll(&mut fds, Some(Duration::ZERO)).is_ok()
        && fds
            .iter()
            .all(|fd| !fd.revents().intersects(PollFlags::NVAL))
}

/// How a client attaches: to which workspace, whether it only watches, its
/// keys (a typing terminal's config run on them already, else the bundled
/// ones), and what was wrong with that config.
pub struct Attaching {
    pub of: Option<fux_wire::command::Run>,
    pub target: Option<fux_wire::command::AttachTarget>,
    pub read_only: bool,
    pub creation: crate::script::Opts,
    pub state: ClientState,
    pub problem: Option<String>,
    /// What a server started for this attach said (`start_server`).
    pub notices: Vec<String>,
}

impl Attaching {
    fn initial(&self) -> Result<fux_wire::command::InitialWorkspace, String> {
        let creation = &self.creation;
        let start = creation.launch(&self.state.preferences.shell)?;
        Ok(fux_wire::command::InitialWorkspace {
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
    Presentation(
        fux_wire::presentation::Presentation,
        std::sync::mpsc::SyncSender<u64>,
    ),
    Finished(Ran),
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
    decoder: fux_vt::keys::decode::Decoder,
    jobs: std::sync::mpsc::Sender<(Job, u64)>,
    /// Bounded committed presentation updates and the dispatched program's result.
    told: std::sync::mpsc::Receiver<Update>,
    receipt: Option<std::sync::mpsc::SyncSender<u64>>,
    wake: fuxix::poll::Wake,
    /// How many admitted programs are not yet done.
    running: usize,
    /// Admitted jobs dispatched after the running job's screen effects.
    pending: VecDeque<Job>,
    /// What stops the program the worker runs (the prefix, then Esc).
    stop: script::Stop,
    /// What was typed while a program ran, in order.
    waiting: VecDeque<Input>,
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
        me: fux_wire::id::ClientId,
        run: fux_wire::command::Run,
        state: ClientState,
        read_only: bool,
    ) -> Result<Keyboard, Error> {
        let (jobs, todo) = std::sync::mpsc::channel::<(Job, u64)>();
        let (tell, told) = std::sync::mpsc::sync_channel::<Update>(1);
        let (wake, poke) = fuxix::poll::waker()?;
        let path = socket.path().to_owned();
        let stop = script::Stop::default();
        let stops = stop.clone();
        let timing = state.preferences.decode_timing();
        let state = std::sync::Arc::new(Mutex::new(state));
        let shared = state.clone();
        let spawned = std::thread::Builder::new()
            .name("keys".into())
            .spawn(move || {
                let mut server = script::Server::stoppable(path, stops.clone());
                let after = std::rc::Rc::new(std::cell::Cell::new(fux_wire::protocol::Fence {
                    run,
                    client: me,
                    frames: 0,
                }));
                server.after = Some(after.clone());
                let mut present = |presentation| {
                    let (sent, received) = std::sync::mpsc::sync_channel(1);
                    if tell.send(Update::Presentation(presentation, sent)).is_err() {
                        stops.stop();
                        return;
                    }
                    poke.poke();
                    // Receipt follows transmission, not mailbox consumption.
                    // Dropping the keyboard drops the receipt sender too.
                    match received.recv() {
                        Ok(frames) => after.set(fux_wire::protocol::Fence {
                            frames,
                            ..after.get()
                        }),
                        Err(_) => stops.stop(),
                    }
                };
                let fence = |frames| {
                    after.set(fux_wire::protocol::Fence {
                        frames,
                        ..after.get()
                    })
                };
                let done = |ran| {
                    let told = tell.send(Update::Finished(ran)).is_ok();
                    poke.poke();
                    told
                };
                work(
                    &mut server,
                    (me, run),
                    &shared,
                    &stops,
                    &mut present,
                    todo,
                    fence,
                    done,
                );
            });
        spawned?;
        // A paste of any length is kept: the user's terminal and memory
        // bound it, not fux.
        let mut decoder = fux_vt::keys::decode::Decoder::with_paste_limit(timing, usize::MAX);
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
            running: 0,
            pending: VecDeque::new(),
            stop,
            waiting: VecDeque::new(),
        })
    }

    /// Whether nothing waits: no program running, no keys held for one.
    pub fn idle(&self) -> bool {
        self.running == 0 && self.waiting.is_empty()
    }

    /// Readable when a program it ran is done (`Keyboard::ran`).
    pub fn wake(&self) -> &fuxix::poll::Wake {
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
        self.decoder
            .set_timing(lock(&self.state).preferences.decode_timing());
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
        for input in inputs {
            // The prefix, then Esc, typed while a program runs, stops it.
            if self.running > 0 && self.stops(&input) {
                self.waiting.pop_back();
                self.stop.stop();
                let text = "stopping…".to_owned();
                acts.push(Dispatch::Send(Act::Notice { text, error: false }));
            } else {
                self.waiting.push_back(input);
            }
            acts.extend(self.release());
        }
        acts
    }

    /// Whether `input` is Esc after the prefix (a key of the root that
    /// holds a map), the last key waiting.
    fn stops(&self, input: &Input) -> bool {
        let escape = KeyPress::new(Key::Escape, Modifiers::NONE);
        matches!(input, Input::Key(stroke) if stroke.press == escape)
            && matches!(self.waiting.back(), Some(Input::Key(stroke)) if lock(&self.state).keys.leads(stroke.press))
    }

    /// What waits, taken unless a program runs.
    fn release(&mut self) -> Vec<Dispatch> {
        let mut acts = Vec::new();
        while self.running == 0 {
            let Some(input) = self.waiting.pop_front() else {
                break;
            };
            self.input(input, &mut acts);
        }
        acts
    }

    /// A frame from the server: a screen it ended, a program to run (a
    /// screen asked of it from elsewhere), keys to take as typed
    /// (`send-keys` to it), the prefix then Esc among them stopping a
    /// program as typed ones do. Handle returned effects in order, as
    /// `Keyboard` requires.
    pub fn frame(&mut self, frame: ToAttached<'_>) -> Vec<Dispatch> {
        let mut acts = Vec::new();
        match frame {
            ToAttached::Ended(opening) => self.screen.ended(opening),
            ToAttached::Open(open) => self.run(Job::open(open), &mut acts),
            ToAttached::Keys(typed) => return self.inputs(typed.into_iter().flat_map(decoded)),
            ToAttached::Read(asked, read) => {
                // The keys held, none while a screen takes them.
                let held = match &self.screen.screen {
                    Screen::Keys(held) => held.at(),
                    Screen::List(_) | Screen::Prompt(_) => &[],
                };
                let value = match read {
                    ClientRead::Keys => lock(&self.state).keys.record(held),
                    ClientRead::Preferences => lock(&self.state).preferences.record(),
                };
                acts.push(Dispatch::Answer(asked, value));
            }
            // The attachment's own: `attached` reads them.
            ToAttached::Welcome(..) | ToAttached::Paint(_) | ToAttached::Exit(_) => {}
        }
        acts.extend(self.release());
        acts
    }

    /// What the programs it ran came to, those done: a screen one opened
    /// opens, what one said shows, and the keys that waited go on.
    /// Handle returned effects in order, as `Keyboard` requires.
    pub fn ran(&mut self) -> Vec<Dispatch> {
        self.wake.drain();
        let mut acts = Vec::new();
        while let Ok(update) = self.told.try_recv() {
            let ran = match update {
                Update::Presentation(presentation, sent) => {
                    self.receipt = Some(sent);
                    acts.push(Dispatch::Send(Act::Presentation(presentation)));
                    acts.push(Dispatch::Fence);
                    continue;
                }
                Update::Finished(ran) => ran,
            };
            self.running = self.running.saturating_sub(1);
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
            let text = "watching only: no choosers or prompts here".to_owned();
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

    /// Queues `job` in local output order. Each emitted `Dispatch::Run`
    /// must reach `Self::dispatch` in transport order.
    fn run(&mut self, job: Job, acts: &mut Vec<Dispatch>) {
        // A stop stops the programs queued with the one it was typed for.
        if self.running == 0 {
            self.stop.reset();
        }
        let waiting = self.running;
        self.running = self.running.saturating_add(1);
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
        for effect in effects {
            match effect {
                Dispatch::Run(job) => self.run(job, acts),
                effect @ (Dispatch::Send(_) | Dispatch::Fence | Dispatch::Answer(..)) => {
                    acts.push(effect)
                }
            }
        }
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
fn decoded(input: fux_wire::command::Input) -> Vec<Input> {
    match input {
        fux_wire::command::Input::Key(press) => vec![Input::Key(press.into())],
        fux_wire::command::Input::Text(text) => text
            .chars()
            .map(|c| Input::Key(KeyPress::char(c).into()))
            .collect(),
        fux_wire::command::Input::Paste(text) => vec![Input::Paste(text)],
        // A pane's alone: `send-keys` refuses them for a client.
        fux_wire::command::Input::Bytes(_) => Vec::new(),
    }
}

/// What a program printed, which a client's worker reads once it ended.
#[derive(Clone, Default)]
struct Said(std::rc::Rc<std::cell::RefCell<Vec<u8>>>);

impl std::io::Write for Said {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The interpreter a client's next job runs in, or why none is.
type Prepared<'a, 'b, 'c> =
    Result<script::Interpreter<'a, 'b, script::AsClient<'c>>, script::Failed>;

/// A worker's loop: runs each of `jobs` as client `me`, on the shared
/// `state`. Each job has an interpreter nothing has run in, so a run's
/// definitions, its failure's leftovers and what it held go with it; the
/// next is built once `done` has told of this one, off the next key's path.
#[expect(
    clippy::too_many_arguments,
    reason = "the worker and its tests share this loop"
)]
fn work(
    server: &mut dyn script::Asks,
    me: (fux_wire::id::ClientId, fux_wire::command::Run),
    state: &Mutex<ClientState>,
    stop: &script::Stop,
    present: &mut dyn FnMut(fux_wire::presentation::Presentation),
    jobs: impl IntoIterator<Item = (Job, u64)>,
    mut fence: impl FnMut(u64),
    mut done: impl FnMut(Ran) -> bool,
) {
    let rebound = std::cell::Cell::new(false);
    let mut publish = |committed: &ClientState| {
        let mut live = lock(state);
        rebound.set(rebound.get() || live.keys.modes != committed.keys.modes);
        let changed = live.preferences.presentation() != committed.preferences.presentation();
        *live = committed.clone();
        drop(live);
        if changed {
            present(committed.preferences.presentation());
        }
    };
    let job_context = || {
        let mut context = context();
        context.client = Some(me.0);
        context.of = Some(me.1);
        context
    };
    let mut asks = script::AsClient {
        asks: server,
        context: job_context(),
        opened: None,
        on_commit: Some(&mut publish),
        stop: Some(stop),
    };
    let mut draft = Default::default();
    let said = Said::default();
    let mut out = said.clone();
    let mut host = script::Host::job(job_context(), &mut draft, &mut out);
    let mut runtime = script::Runtime::new(&mut host, &mut asks, Some(stop));
    let mut defaults = None;
    let mut next = prepare(&mut runtime, state, &mut defaults);
    for (job, frames) in jobs {
        fence(frames);
        rebound.set(false);
        said.0.borrow_mut().clear();
        let mut ran = run_job(next, &mut runtime, state, &job, stop, job_context(), &said);
        ran.rebound = rebound.get();
        if !done(ran) {
            break;
        }
        next = prepare(&mut runtime, state, &mut defaults);
    }
}

/// An interpreter for the next job: the libraries loaded, and the config's
/// definitions evaluated in it.
fn prepare<'a, 'b, 'c>(
    runtime: &mut script::Runtime<'a, 'b, script::AsClient<'c>>,
    state: &Mutex<ClientState>,
    defaults: &mut Option<ClientState>,
) -> Prepared<'a, 'b, 'c> {
    let config = lock(state).config.clone();
    let mut g = script::interpreter(runtime)?;
    script::load_libraries(&mut g, runtime)?;
    if let Some(config) = &config {
        let defaults = match defaults {
            Some(defaults) => defaults,
            None => defaults.insert(script::default_state()?),
        };
        script::replay_config(&mut g, runtime, config, defaults)?;
    }
    Ok(g)
}

/// Runs a key's or a screen's program in `prepared`. Each checkpoint
/// commits whole; a failure discards pending changes.
fn run_job<'a, 'b, 'c>(
    prepared: Prepared<'a, 'b, 'c>,
    runtime: &mut script::Runtime<'a, 'b, script::AsClient<'c>>,
    state: &Mutex<ClientState>,
    job: &Job,
    stop: &script::Stop,
    context: fux_wire::protocol::Context,
    said: &Said,
) -> Ran {
    let draft = lock(state).draft();
    runtime.host.next_job(draft, context.clone());
    runtime.asks.context = context.clone();
    runtime.asks.opened = None;
    let mut result = (|| {
        stop.check()?;
        if let Some(open) = &job.screen {
            let draft = runtime.host.draft().map(|draft| &*draft);
            runtime.asks.opened = Some(crate::script::admit(
                open.clone(),
                draft,
                &context,
                Some(stop),
            )?);
            return Ok(Vec::new());
        }
        let mut g = prepared?;
        let machine = job.machine(None, Some(stop))?;
        let values = script::evaluate_job(&mut g, runtime, &machine)?;
        script::refuse_unfinished(&values, runtime.host)?;
        Ok(values)
    })();
    if result.is_ok()
        && let Err(failed) = stop.check().and_then(|()| match runtime.host.draft() {
            Some(draft) => script::Asks::commit(runtime.asks, draft, context),
            None => Ok(()),
        })
    {
        result = Err(failed);
    }
    let opened = result.is_ok().then(|| runtime.asks.opened.take()).flatten();
    let notice = match result {
        // Stopped as asked (the prefix, then Esc): no failure of its own.
        Err(_) if stop.stopped() => Some(("stopped".to_owned(), false)),
        Err(failed) => Some((failed.notice(), true)),
        Ok(stack) => said_notice(
            String::from_utf8_lossy(&said.0.borrow()).into_owned(),
            &stack,
        ),
    };
    Ran {
        notice,
        opened,
        rebound: false,
    }
}

/// The notice a program that ended makes: what it printed, then what it
/// left, each line that is not blank.
fn said_notice(mut said: String, stack: &[script::Value<script::Fux>]) -> Option<(String, bool)> {
    for value in stack {
        if let Err(failure) = value.write_text(&mut said) {
            return Some((failure.message, true));
        }
        said.push('\n');
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
    let terminal = size.is_none();
    // A terminal's signals are caught once connected, so that ^C while
    // connecting still ends fux, and before it goes raw.
    let early = (!terminal).then(Signals::new).transpose()?;
    // An untargeted writable attach carries the launch an empty server starts.
    // One unmade is left out, said only on a refusal.
    let creates = attaching.target.is_none() && !attaching.read_only;
    let (initial, unusable) = match creates.then(|| attaching.initial()).transpose() {
        Ok(initial) => (initial, None),
        Err(why) => (None, Some(why)),
    };
    let (mut stream, mut decoder, _) = connect(socket, Role::Attach)?;
    let signals = early.map_or_else(Signals::new, Ok)?;
    let size = size.or_else(window_size).ok_or(Error::NoSize)?;
    let attach = Attach {
        of: attaching.of,
        headless: !terminal,
        size,
        target: attaching.target.clone(),
        read_only: attaching.read_only,
        presentation: attaching.state.preferences.presentation(),
        initial,
        // A pane's program among them, it runs in that pane.
        ancestry: if terminal {
            fuxix::process::ancestry()
        } else {
            Vec::new()
        },
    };
    let raw = terminal
        .then(|| Raw::enter_with(&mut io::stdout()))
        .transpose()?;
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
            output.as_mut(),
        )
    });
    // Context for a refusal, not its cause: the server says why.
    let result = match (result, unusable) {
        (Err(Error::Refused(reason)), Some(why)) => Err(Error::Refused(format!(
            "{reason}; local initial launch unavailable: {why}"
        ))),
        (result, _) => result,
    };
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
            stops: fuxix::signal::Pipe::new(&socket::CLIENT_STOPS).map_err(io::Error::from)?,
        })
    }
}

/// The terminal in raw mode on the alternate screen, which puts the
/// terminal back as it was once dropped, however the attachment ends: it
/// leaves the alternate screen, then its modes come back.
struct Raw(#[expect(dead_code, reason = "held for its drop")] fuxix::terminal::Raw<io::Stdin>);

impl Raw {
    fn enter_with(stdout: &mut impl Write) -> Result<Raw, Error> {
        let modes = fuxix::terminal::Raw::enter(io::stdin()).map_err(failed("setting raw mode"))?;
        // Set after fuxix's, so it runs first: the alternate screen is left
        // before the modes come back and the message is printed. Once per
        // process, however often it attaches.
        static HOOKED: std::sync::Once = std::sync::Once::new();
        HOOKED.call_once(|| {
            let previous = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                fuxix::terminal::leaving(leave);
                previous(info);
            }));
        });
        // Made before the screen is entered, so that failing to enter it
        // leaves it too.
        let entered = Raw(modes);
        ENTERED.store(true, Ordering::SeqCst);
        stdout.write_all(ENTER.as_bytes()).map_err(Error::Output)?;
        stdout.flush().map_err(Error::Output)?;
        Ok(entered)
    }
}

impl Drop for Raw {
    fn drop(&mut self) {
        leave();
    }
}

/// What may wait unwritten for the terminal before the server's frames
/// are read no more, till it takes some: flow control, which refuses and
/// drops nothing. What the server sends meanwhile waits there, and the
/// server paints again only once its queue empties.
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
    wake: fuxix::poll::Wake,
}

/// Writes all of `bytes` to stdout's descriptor, not through `Stdout`,
/// whose lock a terminal that stopped reading would hold from `restore`.
fn write_stdout(mut bytes: &[u8]) -> io::Result<()> {
    let stdout = io::stdout();
    while !bytes.is_empty() {
        match fuxix::io::write(&stdout, bytes) {
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
        let (wake, poke) = fuxix::poll::waker()?;
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
                    poke.poke();
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
        self.wake.drain();
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
    stream: &mut Client,
    decoder: &mut Decoder,
    signals: &Signals,
    attaching: &Attaching,
    mut output: Option<&mut Output>,
) -> Result<String, Error> {
    let Signals { winch, stops } = signals;
    wait_at_most(stream, None);
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let terminal = output.is_some();
    let mut buffer = vec![0u8; 64 * 1024];
    let mut keyboard: Option<Keyboard> = None;
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
                    for text in attaching.notices.iter().cloned() {
                        acts.push(Dispatch::Send(Act::Notice { text, error: false }));
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
        let mut runs = Vec::new();
        for out in std::mem::take(&mut acts) {
            let act = match out {
                Dispatch::Run(job) => {
                    runs.push(job);
                    continue;
                }
                Dispatch::Fence => {
                    if let Some(keyboard) = &mut keyboard {
                        keyboard.fence(frames);
                    }
                    continue;
                }
                Dispatch::Send(act) => AttachedFrame::Act(Box::new(act)),
                Dispatch::Answer(asked, value) => AttachedFrame::Answer(asked, value),
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
        // A job runs after every frame read with the keys that made it is
        // answered: what the server sends after keys it sent here says they
        // were taken, before what they do (a detach, say).
        for job in runs {
            keyboard
                .as_mut()
                .ok_or_else(|| Error::Failed("a program has no attached keyboard".into()))?
                .dispatch(job, frames)?;
        }
        // The server's frames only while the terminal has room for them; a
        // hang-up is told all the same.
        let reads = match output.as_ref().is_none_or(|o| o.room()) {
            true => PollFlags::IN,
            false => PollFlags::WATCH,
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
            while winch.next().is_some() {}
            // A terminal that says no size now keeps the last it said.
            if terminal && let Some(size) = window_size() {
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
            // A server that stops says so first (`Exit`): without a word,
            // it crashed, its run waiting for the next, or this
            // connection's own process ended.
            let crashed = "the connection to the fux server ended without a word: it, or the \
                           server, crashed; `fux` attaches again, and takes a crashed server's \
                           panes back";
            return ended.unwrap_or_else(|| Ok(crashed.into()));
        }
    }
}

/// Sends an attached client's `frame`. A write the server hung up on
/// ends the attachment as the server's last frames say: once it has sent
/// its `Exit` (a detach, say) it closes, while the client, not yet having
/// read it, may still be writing. Only a server that said nothing makes
/// the failed write the end.
fn tell(
    stream: &mut Client,
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
    stream: &mut Client,
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
                ToAttached::Welcome(..)
                | ToAttached::Ended(_)
                | ToAttached::Open(_)
                | ToAttached::Keys(_)
                | ToAttached::Read(..) => {}
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

    /// A socket no server is at, absolute on each OS.
    const NOWHERE: &str = if cfg!(windows) {
        r"C:\nowhere\fux\s.sock"
    } else {
        "/nowhere/fux/s.sock"
    };

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

    fn keyboard(read_only: bool) -> Result<Keyboard, Box<dyn std::error::Error>> {
        new_keyboard(
            &socket::checked(NOWHERE, "test")?,
            script::default_state().map_err(|e| e.to_string())?,
            read_only,
        )
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
                Dispatch::Answer(..) => None,
            })
            .collect()
    }

    fn admitted(keyboard: &mut Keyboard, job: Job) -> Vec<Dispatch> {
        let mut outputs = Vec::new();
        keyboard.run(job, &mut outputs);
        outputs
    }

    fn program(keyboard: &mut Keyboard, job: Job) -> Vec<Act> {
        let outputs = admitted(keyboard, job);
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
    fn decoder_timing_belongs_to_each_client_and_waits_for_idle()
    -> Result<(), Box<dyn std::error::Error>> {
        let socket = socket::checked(NOWHERE, "test")?;
        let mut a = script::default_state().map_err(|e| e.to_string())?;
        let b = a.clone();
        a.preferences.escape_delay = fux_wire::command::DecodeMillis::new(100).ok_or("delay")?;
        let mut a = new_keyboard(&socket, a, false)?;
        let mut b = new_keyboard(&socket, b, false)?;
        let now = Instant::now();
        assert!(a.bytes(b"\x1b", now).is_empty());
        assert!(b.bytes(b"\x1b", now).is_empty());
        assert_eq!(a.deadline(), now.checked_add(Duration::from_millis(100)));
        assert_eq!(b.deadline(), now.checked_add(Duration::from_millis(35)));
        lock(&a.state).preferences.escape_delay =
            fux_wire::command::DecodeMillis::new(200).ok_or("delay")?;
        assert!(a.bytes(b"[", now).is_empty());
        assert_eq!(a.deadline(), now.checked_add(Duration::from_millis(100)));
        a.timeout();
        assert!(a.bytes(b"\x1b", now).is_empty());
        assert_eq!(a.deadline(), now.checked_add(Duration::from_millis(200)));
        Ok(())
    }

    // Unix alone: a socket pair stands for the server.
    #[cfg(unix)]
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
        let effects = admitted(
            &mut keyboard,
            Job::from(
                "(sequence! (define! variables-2 (context)) (define! value-3 \"terminal-clipboard\") (define! value-4 (set value-3 #false)) (define! value-5 \"styles\") (define! value-8 \"bar\") (define! value-9 (list value-8 7 8)) (define! value-10 (list value-9)) (define! value-11 (set value-5 value-10)) (define! value-12 \"bar-slots\") (define! value-15 \"inert\") (define! value-16 (list 0 value-15)) (define! value-17 (list value-16)) (define! value-18 (set value-12 value-17)) (define! value-19 \"show-column\") (define! value-20 (set value-19 #false)) (define! value-21 (pane 1)) (define! value-22 \"Enter\") (define! value-23 (key value-21 value-22)) (define! value-24 (send-keys value-23)) #inert)",
            ),
        );
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
        let (mut attachment, mut view) = Client::pair()?;
        let mut frames = 7;
        for effect in pending {
            match effect {
                Dispatch::Send(act) => {
                    send(&mut attachment, &AttachedFrame::Act(Box::new(act)))?;
                    frames = next_frame(frames)?;
                }
                Dispatch::Fence => keyboard.fence(frames),
                Dispatch::Run(_) | Dispatch::Answer(..) => return Err("another job".into()),
            }
        }
        let sent: AttachedFrame =
            read_frame(&mut view, &mut Decoder::default(), Some(ANSWER))?.ok_or("presentation")?;
        let AttachedFrame::Act(act) = sent else {
            return Err("expected attached act".into());
        };
        let Act::Presentation(p) = *act else {
            return Err("expected presentation".into());
        };
        assert!(!p.terminal_clipboard && !p.show_column);
        assert_eq!(p.styles.bar.foreground, fux_vt::Color::Idx(7));
        assert_eq!(p.slots.text(0), Some("inert"));
        assert_ne!(fuxix::poll::poll(&mut requests, Some(ANSWER))?, 0);
        let (mut peer, _) = listener.accept()?;
        let mut decoder = Decoder::default();
        let _: Hello = read_frame(&mut peer, &mut decoder, Some(ANSWER))?.ok_or("hello")?;
        send(&mut peer, &Greeting::Hello(ours()))?;
        let asked: Asked = read_frame(&mut peer, &mut decoder, Some(ANSWER))?.ok_or("request")?;
        assert_eq!(
            asked.after,
            Some(fux_wire::protocol::Fence {
                run: "1-2".parse().map_err(|()| "run")?,
                client: "c1".parse()?,
                frames: 8
            })
        );
        assert!(matches!(
            asked.request,
            Request::Do(Command::SendKeys { .. })
        ));
        drop(peer);
        all_ran(&mut keyboard)?;
        Ok(())
    }

    #[test]
    fn dropping_the_keyboard_releases_a_worker_waiting_for_presentation_transmission()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut keyboard = keyboard(false)?;
        program(
            &mut keyboard,
            Job::from(
                "(sequence! (define! variables-25 (context)) (define! value-26 \"titles\") (define! value-27 (set value-26 #true)) (define! value-28 (pane 1)) (define! value-29 (timeout value-28 30)) (define! value-30 (wait value-29)) value-30)",
            ),
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
        let socket = socket::checked(NOWHERE, "test")?;
        let mut state = script::default_state().map_err(|e| e.to_string())?;
        state.keys.bind(
            &[KeyPress::char('y')],
            crate::keymap::Binding::new(crate::testing::code(
                "(lambda! () (sequence! (define! variables-31 (context)) (define! value-32 \"done\") value-32))",
            )),
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
            assert_eq!(keyboard.running, 1);
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
        let mut keyboard = keyboard(false)?;
        let first = admitted(&mut keyboard, Job::from("(command-prompt (me)) \"ready\""));
        assert!(admitted(&mut keyboard, Job::from("(sequence! (define! variables-34 (context)) (define! value-35 \"next\") value-35)")).is_empty());
        assert!(transport(&mut keyboard, first).is_empty());
        let mut done = [PollFd::new(keyboard.wake(), PollFlags::IN)];
        assert_ne!(fuxix::poll::poll(&mut done, Some(ANSWER))?, 0);
        let next = keyboard.ran();
        assert!(
            matches!(next.as_slice(), [Dispatch::Send(Act::Screen { act: fux_wire::act::ScreenAct::Show(_), .. }), Dispatch::Send(Act::Notice { text, .. }), Dispatch::Run(_)] if text == "ready")
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
        let mut keyboard = keyboard(false)?;
        let now = Instant::now();
        let run = Job::from("(let! ((spin (lambda! () (spin)))) (spin))");
        assert!(program(&mut keyboard, run).is_empty());
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

    #[test]
    fn stopping_cancels_queued_screen_openings_and_releases_keys()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut keyboard = keyboard(false)?;
        assert!(
            program(
                &mut keyboard,
                "(command-prompt (me)) (let! ((spin (lambda! () (spin)))) (spin))".into()
            )
            .is_empty()
        );
        let queued = keyboard.frame(ToAttached::Open(fux_wire::callback::Open::Prompt(
            fux_wire::callback::Prompt {
                title: ":".into(),
                line: fux_wire::act::Line::new(String::new()),
                action: None,
            },
        )));
        assert!(queued.is_empty());
        assert_eq!(keyboard.pending.len(), 1);
        let mut acts = bytes(&mut keyboard, b"x\x02\x1b", Instant::now());
        acts.extend(timeout(&mut keyboard));
        assert!(keyboard.stop.stopped());
        acts.extend(all_ran(&mut keyboard)?);
        assert!(
            !acts.iter().any(|act| matches!(act, Act::Screen { .. })),
            "a cancelled job published a screen: {acts:?}"
        );
        let keys: Vec<_> = acts
            .iter()
            .filter_map(|act| {
                if let Act::Key(stroke) = act {
                    Some(stroke.press)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(keys, [KeyPress::char('x')]);
        assert!(keyboard.idle());
        assert!(keyboard.pending.is_empty());
        assert_eq!(keyboard.screen.screen, Screen::default());
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

    /// What `job` came to, alone on a worker.
    fn one_job(
        server: &mut dyn script::Asks,
        me: (fux_wire::id::ClientId, fux_wire::command::Run),
        state: &Mutex<ClientState>,
        job: Job,
        stop: &script::Stop,
        present: &mut dyn FnMut(fux_wire::presentation::Presentation),
    ) -> Ran {
        let mut ended = None;
        let done = |ran| {
            ended = Some(ran);
            false
        };
        work(server, me, state, stop, present, [(job, 0)], |_| {}, done);
        ended.unwrap_or(Ran {
            notice: None,
            opened: None,
            rebound: false,
        })
    }

    /// What each job in turn came to on one worker.
    fn jobs_on_a_worker(
        state: &Mutex<ClientState>,
        jobs: &[&str],
    ) -> Result<Vec<Option<String>>, Box<dyn std::error::Error>> {
        struct Refuse;
        impl script::Asks for Refuse {
            fn ask(&mut self, _: &Asked) -> Result<Reply, script::Failed> {
                Err(script::Failed::new(Code::NotConfig, "operation refused"))
            }
        }
        let me = (
            fux_wire::id::ClientId::of(1),
            fux_wire::command::Run::now().ok_or("clock")?,
        );
        let mut said = Vec::new();
        let done = |ran: Ran| {
            said.push(ran.notice.map(|(text, _)| text));
            true
        };
        let stop = script::Stop::default();
        let jobs = jobs.iter().map(|job| (Job::from(*job), 0));
        work(
            &mut Refuse,
            me,
            state,
            &stop,
            &mut |_| {},
            jobs,
            |_| {},
            done,
        );
        Ok(said)
    }

    /// A job leaves nothing for the next on the same worker: not what it
    /// defined, not what it put in a context its config holds, not what a
    /// failure stopped it in the middle of.
    #[test]
    fn a_job_leaves_nothing_for_the_next() -> Result<(), Box<dyn std::error::Error>> {
        let dir = std::env::temp_dir().join(format!("fux-isolated-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("config.grift");
        std::fs::write(&path, "(define! store (context))\n")?;
        let state = Mutex::new(script::run_file(&path).map_err(|e| e.to_string())?);
        let said = jobs_on_a_worker(
            &state,
            &[
                "(define! mine 1) (bind store (quote! left) 2) (print (names store))",
                "(print (names store))",
                "(print mine)",
                "(bind store (quote! failed) 3) (first 1)",
                "(print (names store))",
            ],
        )?;
        std::fs::remove_dir_all(dir)?;
        let said: Vec<String> = said.into_iter().map(Option::unwrap_or_default).collect();
        assert_eq!(said.first().map(String::as_str), Some("(left)"), "{said:?}");
        assert_eq!(said.get(1).map(String::as_str), Some("()"), "{said:?}");
        assert!(
            said.get(2).is_some_and(|s| s.contains("unbound-symbol")),
            "{said:?}"
        );
        assert!(
            said.get(3).is_some_and(|s| s.contains("type-error")),
            "{said:?}"
        );
        assert_eq!(said.get(4).map(String::as_str), Some("()"), "{said:?}");
        Ok(())
    }

    #[test]
    fn presentation_commits_whole_and_survives_a_later_failure()
    -> Result<(), Box<dyn std::error::Error>> {
        struct Refuse;
        impl script::Asks for Refuse {
            fn ask(&mut self, _: &Asked) -> Result<Reply, script::Failed> {
                Err(script::Failed::new(Code::NotConfig, "operation refused"))
            }
        }
        let state = Mutex::new(script::default_state().map_err(|e| e.to_string())?);
        let me = (
            fux_wire::id::ClientId::of(1),
            "1-2".parse().map_err(|()| "run")?,
        );
        let stop = script::Stop::default();
        let mut updates = Vec::new();
        let code = "(sequence! (define! variables-37 (context)) (define! value-38 \"history-bytes\") (define! value-39 (set value-38 0)) (define! value-40 \"program-clipboard\") (define! value-41 (set value-40 #false)) (define! value-42 \"terminal-clipboard\") (define! value-43 (set value-42 #false)) (define! value-44 \"bell\") (define! value-45 (set value-44 #false)) (define! value-46 \"titles\") (define! value-47 (set value-46 #true)) #inert)";
        let ran = one_job(&mut Refuse, me, &state, code.into(), &stop, &mut |p| {
            updates.push(p)
        });
        assert!(ran.notice.is_none());
        assert_eq!(
            updates,
            [fux_wire::presentation::Presentation {
                bell: false,
                titles: true,
                terminal_clipboard: false,
                cell_placement: false,
                ..script::default_state()
                    .map_err(|e| e.to_string())?
                    .preferences
                    .presentation()
            }]
        );
        updates.clear();
        let committed = lock(&state).clone();
        let failed = "(sequence! (define! variables-48 (context)) (define! value-49 \"titles\") (define! value-50 (set value-49 #false)) (define! value-51 \"Nope\") (define! value-52 (unbind value-51)) #inert)";
        let ran = one_job(&mut Refuse, me, &state, failed.into(), &stop, &mut |p| {
            updates.push(p)
        });
        assert!(ran.notice.is_some());
        assert!(updates.is_empty());
        assert_eq!(*lock(&state), committed);
        let checkpoint = "(sequence! (define! variables-53 (context)) (define! value-54 \"titles\") (define! value-55 (set value-54 #false)) (define! value-56 (pane 1)) (define! value-57 \"horizontal\") (define! value-58 (axis value-56 value-57)) (define! value-59 (split value-58)) #inert)";
        let ran = one_job(
            &mut Refuse,
            me,
            &state,
            checkpoint.into(),
            &stop,
            &mut |p| updates.push(p),
        );
        let said = ran.notice.map(|(text, _)| text).unwrap_or_default();
        assert!(said.contains("already took effect"), "{said}");
        assert!(updates.last().is_some_and(|p| !p.titles));
        assert!(!lock(&state).preferences.titles);
        Ok(())
    }

    #[test]
    fn queued_programs_commit_the_whole_local_state() -> Result<(), Box<dyn std::error::Error>> {
        let mut keyboard = keyboard(false)?;
        for code in [
            "(sequence! (define! variables-60 (context)) (define! value-61 \"C-b\") (define! value-62 \"C-a\") (define! value-63 (move-keys value-61 value-62)) (define! value-64 \"history-bytes\") (define! value-65 (set value-64 2)) #inert)",
            "(sequence! (define! variables-66 (context)) (define! value-67 \"F5\") (define! value-70 (code! (lambda! () (sequence! (define! variables-68 (context)) (define! value-69 (list-session)) value-69)))) (define! value-71 (bind-key value-67 value-70)) (define! value-72 \"titles\") (define! value-73 (set value-72 #true)) #inert)",
            "(sequence! (define! variables-74 (context)) (define! value-75 \"F6\") (define! value-78 (code! (lambda! () (sequence! (define! variables-76 (context)) (define! value-77 (list-session)) value-77)))) (define! value-79 (bind-key value-75 value-78)) (define! value-80 \"history-bytes\") (define! value-81 (set value-80 0)) (define! value-82 \"bell\") (define! value-83 (set value-82 #false)) (define! value-84 \"Nope\") (define! value-85 (unbind value-84)) #inert)",
        ] {
            assert!(program(&mut keyboard, Job::from(code)).is_empty());
        }
        all_ran(&mut keyboard)?;
        let state = lock(&keyboard.state);
        let bound = |name: &str| -> Result<bool, String> {
            Ok(state.keys.root.get(crate::keymap::key(name)?).is_some())
        };
        assert_eq!(
            [bound("C-a")?, bound("F5")?, bound("C-b")?, bound("F6")?],
            [true, true, false, false]
        );
        assert_eq!(state.preferences.history_bytes.get(), 2);
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
        let mut keyboard = keyboard(false)?;
        let prompt = Screen::Prompt(fux_wire::callback::Prompt {
            title: "rename".into(),
            line: fux_wire::act::Line::new(String::new()),
            action: None,
        });
        keyboard.open(prompt.clone(), &mut Vec::new());
        program(
            &mut keyboard,
            Job::from(
                "(sequence! (define! variables-86 (context)) (define! value-87 \"F5\") (define! value-90 (code! (lambda! () (sequence! (define! variables-88 (context)) (define! value-89 (list-session)) value-89)))) (define! value-91 (bind-key value-87 value-90)) #inert)",
            ),
        );
        assert_eq!(all_ran(&mut keyboard)?, []);
        assert_eq!(keyboard.screen.screen, prompt);
        program(
            &mut keyboard,
            Job::from(
                "(sequence! (define! variables-92 (context)) (define! value-93 \"line\") (define! value-94 \"F2\") (define! value-95 \"home\") (define! value-96 (bind-in value-93 value-94 value-95)) #inert)",
            ),
        );
        let acts = all_ran(&mut keyboard)?;
        assert!(
            matches!(
                acts.as_slice(),
                [
                    Act::Screen {
                        act: fux_wire::act::ScreenAct::Hide,
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
        let mut keyboard = keyboard(false)?;
        let (mut session, client) = crate::script::testing::attached(24, 80)?;
        for (suffix, failed) in [
            (
                "(sequence! (define! variables-97 (context)) (define! value-98 \"hello\") value-98)",
                false,
            ),
            (
                "(sequence! (define! variables-99 (context)) (define! value-100 \"boom\") (define! value-101 (raise (error (quote! fail) value-100))) #inert)",
                true,
            ),
        ] {
            program(
                &mut keyboard,
                Job::from(format!("(command-prompt (me)) {suffix}")),
            );
            let acts = all_ran(&mut keyboard)?;
            assert_eq!(
                acts.iter()
                    .filter(|act| matches!(act, Act::Screen { .. }))
                    .count(),
                usize::from(!failed)
            );
            session.acts(client, acts, std::time::Instant::now());
            let view = session.views.get(&client).ok_or("view")?;
            let notice = view.notice.as_ref().ok_or("notice")?;
            assert_eq!(notice.error, failed);
            assert!(notice.text.contains(if failed { "boom" } else { "hello" }));
            if let Some(act) = keyboard.screen.close() {
                session.acts(client, vec![act], std::time::Instant::now());
            }
        }
        keyboard.read_only = true;
        program(&mut keyboard, Job::from("(command-prompt (me)) \"hello\""));
        let acts = all_ran(&mut keyboard)?;
        assert!(
            matches!(acts.as_slice(), [Act::Notice { text, error: true }] if text.contains("watching only") && text.contains("hello"))
        );
        Ok(())
    }

    /// A binding's program, then the prefix, Esc (as `CSI 27 u`) and a key,
    /// stop the program and leave the key for after it, however the reads
    /// split them: the cancel is checked at each key, not at a read's end.
    #[test]
    fn a_stop_is_the_same_however_the_reads_split() -> Result<(), Box<dyn std::error::Error>> {
        let socket = socket::checked(NOWHERE, "test")?;
        let mut state = script::default_state().map_err(|e| e.to_string())?;
        let forever = crate::keymap::Binding::new(crate::testing::code(
            "(lambda! () (let! ((spin (lambda! () (spin)))) (spin)))",
        ));
        state.keys.bind(&[KeyPress::char('y')], forever)?;
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

    /// What a program prints and leaves is its notice, however long.
    #[test]
    fn what_a_program_prints_is_its_notice() -> Result<(), Box<dyn std::error::Error>> {
        let mut keyboard = keyboard(false)?;
        let line: String = std::iter::repeat_n('x', 1000).collect();
        let code = format!(
            "(define! output-line {}) (define! repeat-output (lambda! (count) (when! (less-than? count 1100) (print output-line) (repeat-output (add count 1))))) (repeat-output 0)",
            crate::script::quoted(&line)
        );
        assert!(program(&mut keyboard, Job::from(code)).is_empty());
        let acts = all_ran(&mut keyboard)?;
        let said = matches!(acts.as_slice(),
            [Act::Notice { text, error: false }] if text.matches('x').count() == 1_100_000);
        assert!(said, "{} acts", acts.len());
        assert!(program(&mut keyboard, Job::from(crate::script::quoted(&line))).is_empty());
        let acts = all_ran(&mut keyboard)?;
        let shown =
            matches!(acts.as_slice(), [Act::Notice { text, error: false }] if *text == line);
        assert!(shown, "{acts:?}");
        Ok(())
    }

    /// A client that only watches is told of each key and paste it takes
    /// no further: a paste, and a key after the prefix but `d`, as a key.
    #[test]
    fn a_watcher_is_told_of_what_it_takes_no_further() -> Result<(), Box<dyn std::error::Error>> {
        let mut keyboard = keyboard(true)?;
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
    // Unix alone: FIFOs and symbolic links.
    #[cfg(unix)]
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
        let refused = [
            socket::open_log(&fifo).is_err(),
            socket::open_log(&link).is_err(),
        ];
        let opened = socket::open_log(&dir.join("file.log")).is_ok();
        std::fs::remove_dir_all(&dir)?;
        assert_eq!((refused, opened), ([true, true], true));
        Ok(())
    }

    // Unix alone: a pty.
    #[cfg(unix)]
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
            // Every setting of the terminal, as `stty -a` lists them, but
            // `pendin`: a state of its input, not a setting, which macOS's
            // kernel sets itself whenever line editing comes back on.
            let modes = || {
                let out = std::process::Command::new("stty")
                    .arg("-a")
                    .stdin(std::process::Stdio::inherit())
                    .output()?;
                let listed = String::from_utf8(out.stdout).map_err(io::Error::other)?;
                let settings = listed.split_whitespace();
                let settings = settings.filter(|s| s.trim_start_matches('-') != "pendin");
                let settings: Vec<_> = settings.map(str::to_owned).collect();
                match out.status.success() {
                    true => Ok(settings),
                    false => Err(io::Error::other("stty -a failed")),
                }
            };
            let before = modes()?;
            for flush in [false, true] {
                let result = Raw::enter_with(&mut Refused { flush });
                let error = result.err().ok_or("entry succeeded")?;
                let expected = if flush {
                    "entry flush refused"
                } else {
                    "entry write refused"
                };
                assert!(matches!(&error, Error::Output(e) if e.to_string() == expected));
                assert_eq!(modes()?, before);
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
    #[cfg(unix)]
    fn answer(server: impl FnOnce(&mut Client), wait: Duration) -> Result<String, Error> {
        let (client, mut peer) = Client::pair()?;
        let mut client = client;
        server(&mut peer);
        let mut decoder = Decoder::default();
        let answer = read_frame(&mut client, &mut decoder, Some(wait));
        greeted(&ours(), None, answer, Ok(()))
    }

    #[cfg(unix)]
    fn ours() -> Hello {
        Hello {
            protocol: PROTOCOL,
            version: "0".into(),
            role: Role::Command,
        }
    }

    #[cfg(unix)]
    fn write(peer: &mut Client, frame: &Greeting) {
        let _ = peer.write_all(&frame.encode());
    }

    /// A process follows once: not one that finds its own ID marked, which
    /// an exec kept; a process it started, of another ID, still may.
    #[test]
    fn a_command_follows_once() {
        let marked = std::ffi::OsStr::new("42");
        assert!(once(None, "42").is_ok());
        assert!(once(Some(marked), "43").is_ok());
        let again = once(Some(marked), "42").err().unwrap_or_default();
        assert!(again.contains("follows once"), "{again}");
    }

    /// A server whose process is not known, or which runs this same
    /// binary, is not followed: it is said why, and nothing runs.
    // Unix alone: /proc.
    #[cfg(unix)]
    #[test]
    fn a_server_that_cannot_be_followed_is_refused() {
        let said = Mismatch {
            protocol: PROTOCOL + 1,
            version: "0".into(),
        };
        let rerun = Rerun {
            argv: vec!["fux".into(), "ls".into()],
            program: None,
        };
        let unknown = followed(&said, None, &rerun).err();
        let why = "the system does not say which process the server is";
        assert_eq!(unknown.as_deref(), Some(why));
        if cfg!(target_os = "macos") {
            return;
        }
        let us = fuxix::process::Pid::from_raw(std::process::id().cast_signed());
        let same = followed(&said, us, &rerun).err().unwrap_or_default();
        assert!(
            same.starts_with("the server runs this same binary, "),
            "{same}"
        );
    }

    /// A server that refuses says why; one that says nothing times out;
    /// one that hangs up did not answer.
    // Unix alone: a socket pair stands for the server.
    #[cfg(unix)]
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
        assert!(
            matches!(&mismatched, Err(Error::Mismatch { said, .. }) if said.protocol == PROTOCOL + 1)
        );
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
            Err(Error::Protocol(fux_wire::protocol::Error::UnexpectedKind(
                99
            )))
        ));
        // A refusal wins over a failure to send the Hello.
        let both = greeted(
            &ours(),
            None,
            Ok(Some(Greeting::Exit("no"))),
            Err(no_answer()),
        );
        assert!(matches!(both, Err(Error::Refused(_))));
    }
}
