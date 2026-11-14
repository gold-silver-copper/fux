//! The server's endpoint on Windows: a named pipe the socket's path names
//! (D-138), the files beside the path, and how fux's own processes are
//! started and linked.
//!
//! - The path is `FUX_SOCKET`, else `server.sock` in `%LOCALAPPDATA%\fux`,
//!   `%LOCALAPPDATA%\fux-elevated` for an elevated process, so an elevated
//!   and a normal fux of one user keep apart their folders, locks, records
//!   and pipes (D-137): each folder's owner is its own mode's default owner,
//!   and an elevated one is closed to the user's normal processes. No file
//!   is at the path: it names the pipe
//!   (`\\.\pipe\fux-<path>-<SID>[-elevated]`, `fuxix::socket::name`), the
//!   lock files beside it and the run's folder.
//! - Its folder is made with a protected owner-only descriptor, which what
//!   is made in it inherits; one there already, and a run's folder left
//!   there, must be a real folder owned as this user's own objects are and
//!   closed to everyone else, before anything in them is trusted or run.
//! - `.starting` then `.lock` make one server its only owner: opened with
//!   no delete sharing, so neither can be replaced while held, and held by
//!   a live server alone (no child inherits them: every start hands only
//!   the handles it names). `.server` records the server's pid and creation
//!   time, which tell a live server from a pid since reused.
//! - A pipe name some process holds while the lock is ours is a crashed
//!   server's `__conn` process's (it ends as its link to the server breaks)
//!   or a squatter's: waited for a while, then refused. A first instance is
//!   never joined (`FILE_FLAG_FIRST_PIPE_INSTANCE`), and every client checks
//!   its server's user and the pipe's owner (`fuxix::socket::connect`).
use super::{Error, SocketPath, failed, io};
use fuxix::process::Pid;
use std::fs::{self, File, OpenOptions};
use std::os::windows::io::{AsHandle, BorrowedHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// fux's folder's name: its own for an elevated process, whose default
/// owner is not its normal processes' (D-137).
pub fn folder() -> Result<&'static str, Error> {
    let user = fuxix::socket::User::current()
        .map_err(|e| Error::Refused(format!("this process's user can't be read: {e}")))?;
    Ok(match user.elevated() {
        true => "fux-elevated",
        false => "fux",
    })
}

/// Where fux's folder is: in `%LOCALAPPDATA%`.
pub(super) const BASES: [&str; 1] = ["LOCALAPPDATA"];

/// Why there is no default socket when no base is set.
pub(super) const NO_BASE: &str =
    "no socket location: LOCALAPPDATA is not set; set FUX_SOCKET to an absolute socket path";

/// The longest pipe name Windows takes.
const PIPE_NAME_MAX: usize = 256;

/// A path whose pipe name is too long for Windows is refused, saying `from`.
pub(super) fn check_length(path: &Path, from: &'static str) -> Result<(), Error> {
    let name = pipe_name(path)?;
    let length = name.encode_utf16().count();
    if length > PIPE_NAME_MAX {
        return Err(Error::Refused(format!(
            "the socket path names a pipe {length} characters long, longer than Windows' \
             {PIPE_NAME_MAX} (from {from}): {}",
            path.display()
        )));
    }
    Ok(())
}

/// The pipe the path `path` names: its text, `\` as `/` (a pipe's name has
/// no `\` of its own), after `fux-`, then this user's SID.
pub fn pipe_name(path: &Path) -> Result<String, Error> {
    let text = path
        .to_str()
        .ok_or_else(|| Error::Refused(format!("{}: a socket path must be text", path.display())))?;
    let base = format!("fux-{}", text.replace('\\', "/"));
    fuxix::socket::name(&base).map_err(|e| failed("naming the pipe for ", path)(e))
}

/// The folder holding the socket's files must be a real one, ours alone,
/// and so must a run's folder left beside the socket, which a server takes
/// a record and a program from. `create` makes the first, owner-only, when
/// it is missing.
pub fn private_directory(socket: &SocketPath, create: bool) -> Result<(), Error> {
    let directory = socket.directory();
    if create {
        make_private(directory)?;
    }
    check_private(directory)?;
    let panes = super::panes(socket.path());
    match fs::symlink_metadata(&panes) {
        Ok(_) => check_private(&panes),
        Err(_) => Ok(()),
    }
}

/// Makes the folder `directory`, owner-only, unless one is there.
fn make_private(directory: &Path) -> Result<(), Error> {
    match fuxix::file::private_dir(directory) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(failed("creating ", directory)(error)),
    }
}

/// Refuses `directory` unless it is a real folder, ours alone.
fn check_private(directory: &Path) -> Result<(), Error> {
    match fuxix::file::is_private_dir(directory) {
        Ok(true) => Ok(()),
        Ok(false) => Err(Error::Refused(format!(
            "{} must be a folder owned by you that no one else can use, not a link; \
             fux does not change it",
            directory.display()
        ))),
        Err(error) => Err(io(directory)(error)),
    }
}

/// The file beside the socket at `path`, its name ending `end`, made if it
/// is not there (owner-only, as its folder is), and its path: opened with
/// no delete sharing, so no one replaces it while it is open.
fn lock_file(path: &Path, end: &str) -> Result<(File, PathBuf), Error> {
    let mut lock_path = path.as_os_str().to_owned();
    lock_path.push(end);
    let lock_path = PathBuf::from(lock_path);
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    let lock = fuxix::file::private_file(&mut options)
        .open(&lock_path)
        .map_err(io(&lock_path))?;
    Ok((lock, lock_path))
}

/// The server's record beside the socket: who it is, which a server that
/// finds the pipe's name held reads, to tell a live one from a pid reused.
fn record_path(path: &Path) -> PathBuf {
    let mut record = path.as_os_str().to_owned();
    record.push(".server");
    PathBuf::from(record)
}

/// Whether the server `.server` records still runs: its pid is a process
/// made when the record says.
fn recorded_runs(path: &Path) -> bool {
    let Ok(text) = fs::read_to_string(record_path(path)) else {
        return false;
    };
    let mut words = text.split_whitespace();
    let (Some(pid), Some(created)) = (words.next(), words.next()) else {
        return false;
    };
    let (Some(pid), Ok(created)) = (pid.parse().ok().and_then(Pid::from_raw), created.parse())
    else {
        return false;
    };
    fuxix::process::identity(pid).is_ok_and(|now| now.created == created)
}

/// The lock that makes this server its only owner, and its record, which
/// is removed when it is dropped.
pub struct Endpoint {
    path: PathBuf,
    _lock: File,
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        let _ = fs::remove_file(record_path(&self.path));
    }
}

/// How long a server that holds the lock waits for a pipe name another
/// process holds: a crashed server's connection processes let it go as
/// their links break, within milliseconds.
const HELD_WAIT: Duration = Duration::from_secs(2);

/// Takes ownership of the socket at `socket`: its lock, then its pipe's
/// first instance. Refuses while another server runs; waits a while for a
/// crashed one's processes to let the name go, then refuses whoever holds
/// it.
pub fn bind_socket(socket: &SocketPath) -> Result<(Endpoint, Listener), Error> {
    let path = socket.path();
    private_directory(socket, true)?;
    let (starting, starting_path) = lock_file(path, ".starting")?;
    starting
        .lock()
        .map_err(failed("locking ", &starting_path))?;
    let (lock, lock_path) = lock_file(path, ".lock")?;
    lock.try_lock().map_err(|error| match error {
        fs::TryLockError::WouldBlock => Error::Refused(format!(
            "another fux server is already using {}",
            path.display()
        )),
        fs::TryLockError::Error(error) => failed("locking ", &lock_path)(error),
    })?;
    let name = pipe_name(path)?;
    let start = Instant::now();
    let listener = loop {
        match fuxix::socket::Listener::bind(&name) {
            Ok(listener) => break listener,
            Err(fuxix::Errno::ADDRINUSE) if recorded_runs(path) => {
                return Err(Error::Refused(format!(
                    "a fux server runs on {} without its lock; fux will not start beside it",
                    path.display()
                )));
            }
            Err(fuxix::Errno::ADDRINUSE) if start.elapsed() < HELD_WAIT => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(fuxix::Errno::ADDRINUSE) => {
                return Err(Error::Refused(format!(
                    "the pipe {name} is held by another process; fux will not join it"
                )));
            }
            Err(error) => return Err(failed("binding ", path)(error)),
        }
    };
    let me = fuxix::process::own_identity().map_err(|e| io(path)(e.into()))?;
    let record = format!("{} {}\n", me.pid, me.created);
    crate::record::write(&record_path(path), record.as_bytes()).map_err(io(path))?;
    drop(starting);
    let endpoint = Endpoint {
        path: path.to_owned(),
        _lock: lock,
    };
    Ok((endpoint, listener))
}

/// One end of a link between fux's own processes, or a client's.
pub type Stream = fuxix::io::Stream;
/// The server's pipe.
pub type Listener = fuxix::socket::Listener;
/// A client's connection, as accepted: to serve, or to hand to its
/// connection's process.
pub type Accepted = fuxix::socket::Connection;
/// What a child is handed as a standard handle.
pub type Handed = OwnedHandle;
/// A pane's process's pipe, where its server and connections reach it.
pub type PaneListener = fuxix::socket::Listener;

/// Connects to the pipe the path `path` names.
pub fn connect(path: &Path) -> std::io::Result<Stream> {
    let name = pipe_name(path).map_err(std::io::Error::other)?;
    Ok(fuxix::socket::connect(&name)?)
}

/// Streams are always read and written without waiting here.
pub fn nonblocking(_stream: &Stream) -> std::io::Result<()> {
    Ok(())
}

/// The listener accepts without waiting already.
pub fn listening(_listener: &Listener) -> std::io::Result<()> {
    Ok(())
}

/// The next client connected at `listener`: `WouldBlock` while none has.
pub fn accept(listener: &mut Listener) -> std::io::Result<Accepted> {
    Ok(listener.accept()?)
}

/// Who connected on `accepted`, as the system says.
pub fn peer(accepted: &Accepted) -> fuxix::Result<fuxix::socket::Peer> {
    fuxix::socket::peer(accepted, accepted.connected()?)
}

/// `accepted` as a stream to serve here.
pub fn serve(accepted: Accepted) -> std::io::Result<Stream> {
    Ok(accepted.stream()?)
}

/// A second hold on `accepted`'s connection, which outlives handing the
/// first to a child.
pub fn kept(accepted: &Accepted) -> std::io::Result<Accepted> {
    Ok(accepted.as_handle().try_clone_to_owned()?.into())
}

/// `accepted`, to hand to a child.
pub fn hand(accepted: Accepted) -> Handed {
    accepted.into()
}

/// Windows starts no connection's process before its client comes
/// (`Server::keep_spare`): the server takes every client itself.
pub fn spare(_listener: &Listener) -> std::io::Result<Handed> {
    Err(std::io::ErrorKind::Unsupported.into())
}

/// Windows gives a process no limit on its handles to run into.
pub fn out_of_descriptors(_error: &std::io::Error) -> bool {
    false
}

/// The file a process done with its stdout points it at.
pub const NULL: &str = "NUL";

/// `handle`, a standard handle of this process's, if it is one: a
/// process started without one has none (null, or invalid).
fn usable(handle: BorrowedHandle<'_>) -> Option<BorrowedHandle<'_>> {
    use std::os::windows::io::AsRawHandle;
    let raw = handle.as_raw_handle();
    let invalid: std::os::windows::io::RawHandle = std::ptr::without_provenance_mut(usize::MAX);
    (!raw.is_null() && raw != invalid).then_some(handle)
}

/// Starts `fux __conn` from `exe` with `words`, its link on stdin and
/// `out` on stdout: the server's end of its link.
pub fn launch_conn(exe: &Path, words: &[&str], out: Handed) -> std::io::Result<Stream> {
    let (ours, theirs) = fuxix::io::pipe_for_fuxix_child()?;
    let mut args = vec![std::ffi::OsStr::new(crate::link::CONN)];
    args.extend(words.iter().map(std::ffi::OsStr::new));
    let log = std::io::stderr();
    let stdio = [
        Some(theirs.as_handle()),
        Some(out.as_handle()),
        usable(log.as_handle()),
    ];
    fuxix::process::start(exe, &args, stdio)?;
    Ok(ours)
}

/// How long a pane's process has to say it listens.
const PANE_START: Duration = Duration::from_secs(10);

/// Starts pane process `fux __pane SOCKET` from `exe`, which makes the
/// pipe `socket` names itself: the system names a pipe's server by the
/// process that made its first instance, whatever made the instance
/// reached, so a pipe the server made would name the server for as long
/// as the pane lives, a dead one after a crash (CI, run 42117571201). It says
/// it listens on its stdout. A marker at `socket` itself, which the pane
/// removes as it ends, lets a server find the panes waiting by their files
/// (`server::waiting`). Its pid, as it started, which its program's handle
/// is copied from (D-134), and the server's link to it.
pub fn start_pane(exe: &Path, socket: &Path) -> Result<(Pid, Stream), String> {
    let name = pipe_name(socket).map_err(|e| e.to_string())?;
    // One a process still listens at is a pane's: never let go as debris.
    if fuxix::socket::connect(&name).is_ok() {
        return Err(format!(
            "a pane's process listens at {} already",
            socket.display()
        ));
    }
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    fuxix::file::private_file(&mut options)
        .open(socket)
        .map_err(|e| format!("its marker {}: {e}", socket.display()))?;
    let (said, theirs) = fuxix::io::pipe_for_child().map_err(|e| format!("its pipe: {e}"))?;
    let args = [std::ffi::OsStr::new(crate::link::PANE), socket.as_os_str()];
    let log = std::io::stderr();
    let stdio = [None, Some(theirs.as_handle()), usable(log.as_handle())];
    let started = fuxix::process::start(exe, &args, stdio)
        .map_err(|e| format!("starting its process, {}: {e}", exe.display()))?;
    drop(theirs);
    let stream = listens(&said).and_then(|()| {
        fuxix::socket::connect(&name)
            .map_err(|e| format!("reaching its process at {}: {e}", socket.display()))
    });
    // One that can't be reached serves no one, and takes its marker along.
    if stream.is_err() {
        let _ = fuxix::process::kill(started.pid, fuxix::process::Signal::Kill);
        let _ = fs::remove_file(socket);
    }
    Ok((started.pid, stream?))
}

/// Waits for a pane's process to say it listens, on `said`, its stdout.
fn listens(said: &Stream) -> Result<(), String> {
    use fuxix::poll::{Events, PollFd};
    let start = Instant::now();
    loop {
        let left = PANE_START.saturating_sub(start.elapsed());
        if left.is_zero() {
            return Err("its process did not listen in time".to_owned());
        }
        let mut fds = [PollFd::new(said, Events::IN)];
        fuxix::poll::poll(&mut fds, Some(left)).map_err(|e| format!("poll: {e}"))?;
        match fuxix::io::read(said, &mut [0u8; 1]) {
            Ok(0) => return Err("its process ended before it listened".to_owned()),
            Ok(_) => return Ok(()),
            Err(fuxix::Errno::AGAIN) => {}
            Err(error) => return Err(format!("its process: {error}")),
        }
    }
}

/// A pane process's pipe, made here, and the marker path `args` names,
/// which also names the pipe; its server told, on stdout, that it listens.
/// A name any process holds already is refused.
pub fn pane_listener(args: &[String]) -> Result<(PaneListener, Option<PathBuf>), String> {
    use std::io::Write;
    let [socket] = args else {
        return Err("it takes its socket's path".to_owned());
    };
    let socket = PathBuf::from(socket);
    let name = pipe_name(&socket).map_err(|e| e.to_string())?;
    let listener = fuxix::socket::Listener::bind(&name).map_err(|e| format!("its pipe: {e}"))?;
    let mut stdout = std::io::stdout();
    (stdout.write_all(b"\n").and_then(|()| stdout.flush()))
        .map_err(|e| format!("telling its server: {e}"))?;
    Ok((listener, Some(socket)))
}

/// A pane's process made its pipe itself (`pane_listener`): nothing more
/// to listen on.
pub fn pane_listen(_listener: &PaneListener) -> Result<(), String> {
    Ok(())
}

/// A pane's process has begun: its pipe is read without waiting already.
pub fn pane_begun(_listener: &PaneListener) -> Result<(), String> {
    Ok(())
}

/// The next to reach a pane's process at `listener`, waited for: its
/// server, till it has begun.
pub fn pane_next(listener: &mut PaneListener) -> Result<Stream, String> {
    use fuxix::poll::{Events, PollFd};
    loop {
        match listener.accept() {
            Ok(connection) => return connection.stream().map_err(|e| format!("its server: {e}")),
            Err(fuxix::Errno::AGAIN) => {}
            Err(error) => return Err(format!("its server: {error}")),
        }
        let mut fds = [PollFd::new(&*listener, Events::IN)];
        fuxix::poll::poll(&mut fds, None).map_err(|e| format!("poll: {e}"))?;
    }
}

/// Reaches for what listens on the pipe the path `socket` names: a link
/// to it; or that nothing does (no such pipe); or that one does and has no
/// instance free (`fuxix::socket::connect` waited a second for one). Any
/// other failure says nothing of what is there, and is the error.
pub fn reach(socket: &Path) -> std::io::Result<super::Reach> {
    let name = pipe_name(socket).map_err(std::io::Error::other)?;
    match fuxix::socket::connect(&name) {
        Ok(stream) => Ok(super::Reach::Listening(stream)),
        Err(fuxix::Errno::AGAIN) => Ok(super::Reach::Busy),
        Err(errno) => {
            let error = std::io::Error::from(errno);
            if error.kind() == std::io::ErrorKind::NotFound {
                Ok(super::Reach::NoListener)
            } else {
                Err(error)
            }
        }
    }
}

/// Whether the process `pid` still runs.
pub fn runs(pid: Pid) -> bool {
    fuxix::process::identity(pid).is_ok()
}

/// The pid of the process that made the pipe `stream` is connected to: a
/// pane's own, as it makes its own pipe (`start_pane`).
pub fn listener_pid(stream: &Stream) -> Option<Pid> {
    let connected = std::time::SystemTime::now();
    fuxix::socket::peer(stream, connected).ok()?.pid
}

/// Keeps the fux this process runs at `exe`: always a copy, as a running
/// executable can't be replaced on Windows, and a link to it would be
/// the installed one (WINDOWS.md).
pub fn keep_executable(exe: &Path) -> std::io::Result<()> {
    super::copy_whole(&std::env::current_exe()?, exe)
}

/// Starts `exe` with this process's arguments and standard handles,
/// Windows having no `exec`: this process then ends, before it takes any
/// lock, and the copy is the server.
pub fn run_pinned(exe: &Path) -> std::io::Result<()> {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let args: Vec<&std::ffi::OsStr> = args.iter().map(std::ffi::OsString::as_os_str).collect();
    let (stdin, stdout, log) = (std::io::stdin(), std::io::stdout(), std::io::stderr());
    let stdio = [stdin.as_handle(), stdout.as_handle(), log.as_handle()].map(usable);
    fuxix::process::start(exe, &args, stdio).map(drop)
}

/// A pipe of this process's that the path `path` names, waiting for
/// clients: where an attached client's connection takes bodies.
pub fn listen_at(path: &Path) -> std::io::Result<Listener> {
    let name = pipe_name(path).map_err(std::io::Error::other)?;
    Ok(fuxix::socket::Listener::bind(&name)?)
}

/// A connection's process's client and its link to its server, as `fux
/// __conn taken` is given them: the client's pipe instance on stdout, the
/// link on stdin, each a handle it inherited.
pub fn taken_stdio() -> std::io::Result<(Stream, Stream)> {
    let server = std::io::stdin().as_handle().try_clone_to_owned()?;
    let client = std::io::stdout().as_handle().try_clone_to_owned()?;
    let client = fuxix::socket::Connection::from(client).stream()?;
    Ok((client, Stream::try_from(server)?))
}

/// Where the pinned ConPTY is kept: `conpty` in fux's folder, which must
/// be ours alone, as what is loaded from it runs as us.
fn conpty() -> Result<PathBuf, String> {
    let folder = super::directory().map_err(|e| e.to_string())?;
    make_private(&folder)
        .and_then(|()| check_private(&folder))
        .map_err(|e| e.to_string())?;
    Ok(folder.join("conpty"))
}

/// Gets the pinned ConPTY ready before the server serves (D-130): fetched
/// on first use (`fuxix::pty::fetch`), `notice` saying so before the wait
/// only if a download will happen, then chosen as each pane will choose
/// it. What the built-in loses, if it is the one, said once, here.
pub fn prepare_terminals(notice: impl Fn(&str)) -> Option<String> {
    let dir = match conpty() {
        Ok(dir) => dir,
        Err(why) => return Some(format!("no folder for the pinned ConPTY: {why}")),
    };
    if !fuxix::pty::fetched(&dir) {
        notice("fetching Windows' ConPTY, once");
    }
    let fetched = fuxix::pty::fetch(&dir).err();
    let loaded = fuxix::pty::load(&dir).err()?;
    let why = fetched.map_or_else(|| loaded.to_string(), |e| e.to_string());
    Some(format!(
        "Windows' built-in ConPTY is used ({why}): it drops sixel pictures and \
         synchronized output, and rewrites titles, links and colours"
    ))
}

/// Chooses a pane process's ConPTY, as its server did (`prepare_terminals`,
/// which said what the built-in loses): before its first terminal.
pub fn load_terminals() {
    if let Ok(dir) = conpty() {
        let _ = fuxix::pty::load(&dir);
    }
}

/// A client's connection to its server, off the process's completion port:
/// its worker waits on requests while the main thread polls.
pub type Client = fuxix::socket::Client;

/// The signals that detach an attached client, with Ctrl-Break, which a
/// console sends whatever its mode.
pub const CLIENT_STOPS: [fuxix::process::Signal; 4] = [
    fuxix::process::Signal::Term,
    fuxix::process::Signal::Int,
    fuxix::process::Signal::Hup,
    fuxix::process::Signal::Quit,
];

/// What starts a server at `path`.
pub(super) fn start_another(path: &str) -> String {
    format!("`set FUX_SOCKET={path}`, then `fux '(start)'`, starts one")
}

/// A client's refusal: no folder or no record is no server; a folder not
/// ours alone is refused before any record there is read, as a bad
/// directory is on Unix. No file is at the path to look at.
pub fn check_client_socket(socket: &SocketPath) -> Result<(), Error> {
    let path = socket.path();
    if fs::symlink_metadata(socket.directory()).is_err() {
        return Err(Error::NoServer(path.to_owned()));
    }
    private_directory(socket, false)?;
    match record_path(path).exists() {
        true => Ok(()),
        false => Err(Error::NoServer(path.to_owned())),
    }
}

/// Whether a server runs on `socket`: its folder is ours, `.server` names
/// a live process, and `.lock` is held, asked as a starting server asks,
/// once none is starting there. Every instance being busy says nothing: a
/// squatter's are busy too. A client that finds no record makes no file.
pub fn owned(socket: &SocketPath) -> bool {
    let path = socket.path();
    private_directory(socket, false).is_ok() && recorded_runs(path) && lock_held(path)
}

/// Whether a server holds the lock of the socket at `path`.
fn lock_held(path: &Path) -> bool {
    let Ok((starting, _)) = lock_file(path, ".starting") else {
        return false;
    };
    if starting.lock().is_err() {
        return false;
    }
    let Ok((lock, _)) = lock_file(path, ".lock") else {
        return false;
    };
    matches!(lock.try_lock(), Err(fs::TryLockError::WouldBlock))
}

/// How long a client waits on a server whose every instance is taken, as
/// a full backlog holds a Unix client: a loaded server's next is coming.
const BUSY_WAIT: Duration = Duration::from_secs(5);

/// Connects to the server at `socket`. A pipe that is gone while its record
/// is left is what Unix's refused connect says: one that stopped left it.
/// Every instance busy is waited on only while `owned` says a server runs:
/// a squatter's busy instances are no server, and time out.
pub fn dial(socket: &SocketPath) -> std::io::Result<Client> {
    let name = pipe_name(socket.path()).map_err(std::io::Error::other)?;
    let start = Instant::now();
    loop {
        match Client::connect(&name) {
            Ok(client) => return Ok(client),
            Err(fuxix::Errno::AGAIN) if start.elapsed() < BUSY_WAIT && owned(socket) => {}
            Err(fuxix::Errno::AGAIN) => return Err(std::io::ErrorKind::TimedOut.into()),
            Err(errno) => {
                let error = std::io::Error::from(errno);
                return Err(match error.kind() == std::io::ErrorKind::NotFound {
                    true => std::io::ErrorKind::ConnectionRefused.into(),
                    false => error,
                });
            }
        }
    }
}

/// What a refusal to attach adds: mintty's pipes are no console.
pub const NOT_A_TERMINAL: &str = " (Git Bash's mintty is none: use Windows Terminal, or `winpty`)";

/// This process's user, as a refusal says it.
pub fn this_user() -> String {
    fuxix::socket::User::current().map_or_else(|e| e.to_string(), |user| user.to_string())
}

/// Who serves `client`, as the system says.
pub fn client_peer(client: &Client) -> fuxix::Result<fuxix::socket::Peer> {
    client.peer()
}

/// The server's log at `path`, opened to append, never through a link.
pub fn open_log(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    let file = fuxix::file::private_file(&mut options).open(path)?;
    match file.metadata()?.is_file() {
        true => Ok(file),
        false => Err(std::io::Error::other("not a regular file")),
    }
}

/// A server being started: what it says on stdout, then its end.
pub struct Starting {
    pid: Pid,
    output: Option<std::io::PipeReader>,
}

/// How long a server whose stdout closed has to end.
const ENDING: Duration = Duration::from_secs(2);

/// Starts `exe` with `args` in the background, its stderr `log`, its stdout
/// a pipe to read; without the variable `unset`. Through
/// `fuxix::process::start`, whose handle list hands it these alone (D-140):
/// with std's, it would inherit what the client inherited, and `fux start |
/// findstr x` would never end. It runs in its socket's folder: a process's
/// directory can't be removed or renamed, so the user's would stay pinned.
pub fn start_server(
    exe: &Path,
    args: &[&std::ffi::OsStr],
    socket: &SocketPath,
    log: File,
    unset: &str,
) -> std::io::Result<Starting> {
    let (output, theirs) = std::io::pipe()?;
    let stdio = [None, Some(theirs.as_handle()), Some(log.as_handle())];
    let directory = Some(socket.directory());
    let started = fuxix::process::start_in(exe, args, stdio, directory, &[unset])?;
    Ok(Starting {
        pid: started.pid,
        output: Some(output),
    })
}

impl Starting {
    /// What it writes on stdout, to read until it closes.
    pub fn output(&mut self) -> Option<impl std::io::Read + use<>> {
        self.output.take()
    }

    /// Waits for it to end: how, as words. Dropped instead, it runs on.
    pub fn ended(self) -> std::io::Result<String> {
        let start = Instant::now();
        loop {
            match fuxix::process::reap(self.pid) {
                Ok(Some(status)) => return Ok(format!("{status:?}")),
                Ok(None) if start.elapsed() < ENDING => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Ok(None) => return Ok("it runs on, saying nothing".to_owned()),
                Err(errno) => return Err(errno.into()),
            }
        }
    }
}

/// The program process `pid` runs, to run it; its path, to say; and whether
/// it is this very binary, by path, as `follow` asks.
pub fn server_binary(pid: Pid) -> Result<(PathBuf, PathBuf, bool), String> {
    let path = fuxix::process::image(pid).map_err(|e| format!("process {pid}: {e}"))?;
    let ours = std::env::current_exe().map_err(|e| format!("this binary: {e}"))?;
    let real = |path: &Path| fs::canonicalize(path).map_err(|e| format!("{}: {e}", path.display()));
    let same = real(&path)? == real(&ours)?;
    Ok((path.clone(), path, same))
}

/// Runs `command`, having no `exec`: waits for it, and this process ends
/// with its code. Returns only if it can't start. `arg0` has no place here:
/// std names a program by its path.
pub fn rerun(mut command: std::process::Command, _arg0: &std::ffi::OsStr) -> std::io::Error {
    match command.status() {
        // No terminal is raw here: a command line, ending as what took its
        // place did.
        Ok(status) => fuxix::process::end(status.code().unwrap_or(1)),
        Err(error) => error,
    }
}

/// The id a command that followed finds in its mark: its parent's, as there
/// is no `exec` to keep one.
pub fn follower_id() -> u32 {
    fuxix::process::ancestry().get(1).copied().unwrap_or(0)
}

/// Windows starts no connection's process before its client.
pub fn waiting_stdio() -> Result<(), String> {
    Err("Windows starts no connection's process before its client".to_owned())
}

/// Where `fux help` says a server is found.
pub const SOCKET_HELP: &str = "(`set FUX_SOCKET=C:\\abs\\path`, then `fux …`); else\n\
     server.sock in %LOCALAPPDATA%\\fux.";
