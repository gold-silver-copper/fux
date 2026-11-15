//! The server's socket on Unix, and the rules that keep it private.
//!
//! - The path is `FUX_SOCKET`, else `server.sock` in fux's directory:
//!   fux's directory is `$XDG_RUNTIME_DIR/fux` whenever that is
//!   set (even empty, which is refused), else `$TMPDIR/fux`.
//! - Its directory is owned by the user, mode 0700; the directories above
//!   it are not checked (`private_directory`). The socket is mode 0600, set
//!   before `listen`.
//! - A lock on the run's own directory (`panes`) makes one server its only
//!   owner, for as long as it lives: the lock is on the directory the
//!   run's record is in, so whoever finds that record finds the lock, and
//!   nothing that removes files can take it away (`lock_run`).
//! - A leftover socket is replaced only when nothing answers on it.
//! - At exit the socket is removed only if it is still the one this server
//!   bound. On Linux an `O_PATH` descriptor keeps its inode allocated so the
//!   number cannot be reused by a replacement.
//! - Every accepted peer must run as the server's own user.
use super::{Error, SocketPath, failed, io, panes};
use fuxix::process::geteuid;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

pub use fux_pane::socket::{Accepted, PaneListener, Stream, accept, nonblocking, peer, serve};

/// fux's directory's name.
pub fn folder() -> Result<&'static str, Error> {
    Ok("fux")
}

/// Where fux's directory is, the first set: `XDG_RUNTIME_DIR`, else `TMPDIR`.
pub(super) const BASES: [&str; 2] = ["XDG_RUNTIME_DIR", "TMPDIR"];

/// Why there is no default socket when no base is set.
pub(super) const NO_BASE: &str = "no socket location: neither XDG_RUNTIME_DIR nor TMPDIR is set; \
     set FUX_SOCKET to an absolute socket path";

/// A path too long for `sockaddr_un` is refused, saying `from`.
pub(super) fn check_length(path: &Path, from: &'static str) -> Result<(), Error> {
    let limit = fuxix::socket::max_path_len();
    if path.as_os_str().len() > limit {
        return Err(Error::Refused(format!(
            "the socket path is {} bytes, longer than the {limit}-byte limit for a Unix \
             domain socket on this platform (from {from}): {}",
            path.as_os_str().len(),
            path.display()
        )));
    }
    Ok(())
}

/// The directory holding the socket must be ours, mode 0700. The
/// directories above it are not checked: inside a sandbox's user namespace
/// root's show as another uid (65534), and what keeps a connection private
/// is that each side checks the other's user (the server its clients', a
/// client its server's), whoever could rename what lies above. `create`
/// makes it when it is missing (as a server does, or a client about to
/// start one); nothing that already exists is modified.
pub fn private_directory(socket: &SocketPath, create: bool) -> Result<(), Error> {
    let directory = socket.directory();
    let euid = geteuid();
    if create {
        match DirBuilder::new().mode(0o700).create(directory) {
            // Set explicitly: the umask must not decide who reaches the socket.
            Ok(()) => fs::set_permissions(directory, fs::Permissions::from_mode(0o700))
                .map_err(io(directory))?,
            // Whatever is there (a symbolic link too) is checked below.
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(failed("creating ", directory)(error)),
        }
    }
    let meta = fs::symlink_metadata(directory).map_err(io(directory))?;
    let mode = meta.permissions().mode();
    if meta.file_type().is_symlink() {
        return Err(Error::Refused(format!(
            "{} is a symbolic link; the socket directory must be a real directory",
            directory.display()
        )));
    }
    if !meta.is_dir() || meta.uid() != euid || mode & 0o077 != 0 {
        return Err(Error::Refused(format!(
            "{} must be a directory owned by you (uid {euid}) with mode 0700; it is {} \
             with mode {:04o} owned by uid {}. fux does not change it",
            directory.display(),
            if meta.is_dir() {
                "a directory"
            } else {
                "not a directory"
            },
            mode & 0o7777,
            meta.uid(),
        )));
    }
    Ok(())
}

/// A client refuses a socket that is not in a private directory of its own
/// user, or not a socket of its own user.
pub fn check_client_socket(socket: &SocketPath) -> Result<(), Error> {
    let path = socket.path();
    let meta = match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(Error::NoServer(path.to_owned()));
        }
        Err(error) => return Err(io(path)(error)),
        Ok(meta) => meta,
    };
    private_directory(socket, false)?;
    if !meta.file_type().is_socket() || meta.uid() != geteuid() {
        return Err(Error::Refused(format!(
            "{} is not a socket owned by you; refusing to connect",
            path.display()
        )));
    }
    Ok(())
}

fn identity(meta: &fs::Metadata) -> (u64, u64) {
    (meta.dev(), meta.ino())
}

/// A socket file's identity, held so that it keeps meaning that file.
struct Pinned {
    identity: (u64, u64),
    #[cfg(any(target_os = "linux", target_os = "android"))]
    _inode: File,
}

impl Pinned {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn new(path: &Path) -> io::Result<Self> {
        let inode = fuxix::file::pin(path)?;
        let meta = inode.metadata()?;
        Ok(Self {
            identity: identity(&meta),
            _inode: inode,
        })
    }

    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    fn new(path: &Path) -> io::Result<Self> {
        fs::symlink_metadata(path).map(|meta| Self {
            identity: identity(&meta),
        })
    }

    fn still_at(&self, path: &Path) -> bool {
        fs::symlink_metadata(path).is_ok_and(|meta| identity(&meta) == self.identity)
    }
}

/// The lock that makes this server its run's only one, and the socket it
/// bound. Dropping it removes the socket if it is still the one this
/// server bound, and lets the lock go.
pub struct Endpoint {
    path: PathBuf,
    socket: Pinned,
    _lock: File,
    /// Its turn among the servers starting, taken as it stops (`stop`).
    starting: Option<File>,
}

impl Endpoint {
    /// The server stops: its socket goes now, so no client finds it, and
    /// its run's lock stays its own till this is dropped, as it ends. A
    /// run is its server's while that server lives, its panes' closing
    /// too: a start meanwhile waits its turn behind it, as behind one
    /// starting, and finds what it left, not panes it still holds. That
    /// turn is taken first, while `listener` still answers: a client that
    /// finds no server from then on, and starts one, finds the turn
    /// taken, and its start waits; none is refused for the run's lock.
    pub fn stop(&mut self, listener: UnixListener) {
        self.starting = turn(&self.path).ok().and_then(Result::ok);
        drop(listener);
        self.unlink();
    }

    fn unlink(&self) {
        if self.socket.still_at(&self.path) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        self.unlink();
    }
}

/// Whether a server listens on `path`, without waiting: a nonblocking
/// connect answers at once. A listener with a full backlog refuses with
/// `EAGAIN`, and counts as alive. A crashed server's socket refuses: no
/// process of its holds its listener (each is close-on-exec).
pub fn listened(path: &Path) -> bool {
    let Ok(fd) = fuxix::socket::stream() else {
        return false;
    };
    if fuxix::io::set_nonblocking(&fd, true).is_err() {
        return false;
    }
    match fuxix::socket::connect(&fd, path) {
        Ok(()) | Err(fuxix::Errno::AGAIN | fuxix::Errno::INPROGRESS) => true,
        Err(_) => false,
    }
}

/// Whether a server answers on `socket` (`listened`), with a backlog full
/// or not.
pub fn owned(socket: &SocketPath) -> bool {
    listened(socket.path())
}

/// Whether process `pid` is gone: it has no session, or, on Linux, it is a
/// zombie, whose descriptors are closed.
fn gone(pid: fuxix::process::Pid) -> bool {
    let zombie = || {
        let stat = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
        (stat.rsplit_once(") ")).is_some_and(|(_, rest)| rest.starts_with(['Z', 'X']))
    };
    fuxix::process::session(pid).is_none() || cfg!(target_os = "linux") && zombie()
}

/// What locking the run on a socket found (`lock_run`).
enum Run {
    /// No server has it: the lock, this process's while it is kept.
    Ours(File),
    /// A server that lives has it: its process, where the system says
    /// which (`locker`).
    Theirs(Option<i32>),
    /// No run is there: it has no directory, and none was to be made.
    None,
}

/// How often a run's directory replaced as it was locked is locked again
/// before the start is refused: it takes a run ending and one beginning
/// there between two calls, each time.
const LOCK_TRIES: usize = 8;

/// Locks the run on the socket at `path`, without waiting: the lock is on
/// the run's directory itself (`panes`), made if `make` and it is not
/// there. That is what no cleaner of old files can undo while its server
/// lives: the run's record is in that directory, so a server that finds
/// the record finds this lock, and a directory removed (it was empty,
/// then: no record, no run to take) and made again is another run's. A
/// lock file beside the socket, as this once was, could be removed alone,
/// and a second server then started on the live one's run. systemd's
/// cleaner leaves a locked directory and all in it alone, too
/// (tmpfiles.d(5)).
///
/// The lock is the one the directory at the path has: one removed, or
/// replaced, between its opening and its locking is locked again.
fn lock_run(path: &Path, make: bool) -> Result<Run, Error> {
    let directory = panes(path);
    for _ in 0..LOCK_TRIES {
        if make {
            super::make_run_directory(path)?;
        }
        let opened = OpenOptions::new()
            .read(true)
            // std opens close-on-exec.
            .custom_flags(fuxix::file::NOFOLLOW)
            .open(&directory);
        let lock = match opened {
            Ok(lock) => lock,
            Err(error) if error.kind() == io::ErrorKind::NotFound && make => continue,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Run::None),
            Err(error) => return Err(io(&directory)(error)),
        };
        let held = lock.metadata().map_err(io(&directory))?;
        if !held.is_dir() {
            return Err(Error::Refused(format!(
                "{} is not a directory: fux keeps a run there, and will not replace it",
                directory.display()
            )));
        }
        match lock.try_lock() {
            Ok(()) => {}
            Err(fs::TryLockError::WouldBlock) => return Ok(Run::Theirs(locker(&held))),
            // No guess at another way to be the only one: a filesystem
            // that locks no directory (NFS, for one that must be written
            // to be locked) has no fux server.
            Err(fs::TryLockError::Error(error)) => {
                return Err(Error::Refused(format!(
                    "{} could not be locked ({error}): the lock on a run's directory is what \
                     makes its server the only one, so fux starts none there; FUX_SOCKET \
                     names a socket on another filesystem",
                    directory.display()
                )));
            }
        }
        let there = fs::symlink_metadata(&directory).map(|meta| identity(&meta));
        if there.is_ok_and(|there| there == identity(&held)) {
            return Ok(Run::Ours(lock));
        }
    }
    Err(Error::Refused(format!(
        "{} was replaced each of the {LOCK_TRIES} times it was locked; fux starts no server there",
        directory.display()
    )))
}

/// The process that holds a lock on the file or directory `held`, as the
/// system lists them: on Linux alone (`/proc/locks`, each named there
/// checked to have that very file open), and only to name it.
fn locker(held: &fs::Metadata) -> Option<i32> {
    let locks = fs::read_to_string("/proc/locks").ok()?;
    let inode = format!(":{}", held.ino());
    (locks.lines())
        // `1: FLOCK ADVISORY WRITE 4242 00:2e:1234 0 EOF`
        .filter_map(|line| {
            let mut words = line.split_whitespace().skip(1);
            let (kind, pid, file) = (words.next()?, words.nth(2)?, words.next()?);
            (kind == "FLOCK" && file.ends_with(&inode)).then(|| pid.parse::<i32>().ok())?
        })
        .find(|pid| {
            fs::read_dir(format!("/proc/{pid}/fd")).is_ok_and(|open| {
                (open.flatten())
                    .filter_map(|entry| fs::metadata(entry.path()).ok())
                    .any(|meta| identity(&meta) == identity(held))
            })
        })
}

/// Why a start on the socket at `path` is refused for the server that
/// holds its run (`server`, where the system named it). One whose socket
/// file is gone, removed under it, is said so: no client finds it there.
fn held_by(path: &Path, server: Option<i32>) -> String {
    let who = server.map_or_else(String::new, |pid| format!(" (process {pid})"));
    let gone = match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => format!(
            ": it lives, and holds {}, but its socket file is gone, so no client reaches it",
            panes(path).display()
        ),
        Ok(_) | Err(_) => String::new(),
    };
    format!(
        "another fux server{who} is already using {}{gone}",
        path.display()
    )
}

/// A turn among the servers starting or stopping on the socket at `path`:
/// the lock of its `.starting` file, waited for at most `TURN_WAIT`. Or
/// why it was not had by then: who keeps it, where the system says which
/// process, a server stopped as it starts or stops. Nothing is changed
/// for a turn not had, that server's run least of all.
fn turn(path: &Path) -> Result<Result<File, String>, Error> {
    let (starting, starting_path) = lock_file(path, ".starting")?;
    let held = starting.metadata().map_err(io(&starting_path))?;
    let locked = super::lock_within(starting, super::TURN_WAIT)
        .map_err(failed("locking ", &starting_path))?;
    Ok(locked.ok_or_else(|| {
        let who = locker(&held).map_or_else(String::new, |pid| format!(" (process {pid})"));
        format!(
            "another fux server{who} has been starting or stopping on {} for {} seconds, \
             holding {}: this start waits no longer, and has changed nothing",
            path.display(),
            super::TURN_WAIT.as_secs(),
            starting_path.display()
        )
    }))
}

/// A start's turn among the servers starting or stopping on the socket
/// at `path` (`turn`), its while it is kept: refused, saying why, where
/// another keeps the turn too long.
pub fn start_turn(path: &Path) -> Result<File, Error> {
    turn(path)?.map_err(Error::Refused)
}

/// The lock file beside the socket at `path`, its name ending `end`, made
/// if it is not there, and its path. It orders the servers starting there
/// (`.starting`), and only that: which one has the run is `lock_run`'s to
/// say, so this file removed under a start costs that start's turn alone.
fn lock_file(path: &Path, end: &str) -> Result<(File, PathBuf), Error> {
    let mut lock_path = path.as_os_str().to_owned();
    lock_path.push(end);
    let lock_path = PathBuf::from(lock_path);
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        // std opens close-on-exec.
        .custom_flags(fuxix::file::NOFOLLOW)
        .open(&lock_path)
        .map_err(io(&lock_path))?;
    Ok((lock, lock_path))
}

/// Takes ownership of `path`, binds it and listens there. Refuses a live
/// server's socket and anything that is not a socket; replaces only a
/// socket proven stale.
pub fn bind_socket(socket: &SocketPath) -> Result<(Endpoint, UnixListener), Error> {
    let path = socket.path();
    private_directory(socket, true)?;
    // One server starts at a time, held here till the one before it
    // listens or fails (this returns, or it exits): one refused below is
    // refused once the socket answers, which its client then finds
    // (`client::start_server`).
    let _starting = turn(path)?.map_err(Error::Refused)?;
    // Before anything there is touched: a server refused here has changed
    // nothing, its socket least of all.
    let lock = match lock_run(path, true)? {
        Run::Ours(lock) => lock,
        Run::Theirs(server) => return Err(Error::Refused(held_by(path, server))),
        Run::None => return Err(io(&panes(path))(io::ErrorKind::NotFound.into())),
    };
    // Pinned before the probe, so the file found dead is the one removed.
    let stale = Pinned::new(path);
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(io(path)(error)),
        Ok(meta) if !meta.file_type().is_socket() || meta.uid() != geteuid() => {
            return Err(Error::Refused(format!(
                "{} exists and is not a socket owned by you; fux will not replace it",
                path.display()
            )));
        }
        // With the run's lock held, one that listens is no live fux
        // server's of this run, but still refused (`listened`, full or not).
        Ok(_) if listened(path) => {
            return Err(Error::Refused(format!(
                "another fux server is already listening on {}",
                path.display()
            )));
        }
        // No answer: with the lock held, no server owns it.
        Ok(_) => {
            if stale.as_ref().is_ok_and(|stale| stale.still_at(path)) {
                fs::remove_file(path).map_err(failed("removing stale socket ", path))?;
            }
        }
    }
    let fd = fuxix::socket::stream().map_err(failed("a socket for ", path))?;
    fuxix::socket::bind(&fd, path).map_err(failed("binding ", path))?;
    let endpoint = Endpoint {
        path: path.to_owned(),
        socket: Pinned::new(path).map_err(io(path))?,
        _lock: lock,
        starting: None,
    };
    // From here a failure drops `endpoint`, which removes the socket. The mode
    // is set before `listen`, so no connection is accepted on an open socket.
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(io(path))?;
    fuxix::socket::listen(&fd, 128).map_err(failed("listening on ", path))?;
    Ok((endpoint, UnixListener::from(fd)))
}

/// The server's listening socket.
pub type Listener = UnixListener;
/// What a child is handed as a standard stream.
pub type Handed = std::os::fd::OwnedFd;
/// Connects to the socket at `path`.
pub fn connect(path: &Path) -> std::io::Result<Stream> {
    UnixStream::connect(path)
}

/// A second hold on `accepted`'s connection, which outlives handing the
/// first to a child.
pub fn kept(accepted: &Accepted) -> std::io::Result<Accepted> {
    accepted.try_clone()
}

/// `accepted`, to hand to a child.
pub fn hand(accepted: Accepted) -> Handed {
    accepted.into()
}

/// Starts `fux __conn` from `exe`, its link on stdin and `out` on stdout:
/// the server's end of its link, and the process, which the server reaps
/// by name once it ends (`Server::reap`).
pub fn launch_conn(
    exe: &Path,
    out: Handed,
) -> std::io::Result<(Stream, Option<fuxix::process::Pid>)> {
    let (ours, theirs) = UnixStream::pair()?;
    let child = std::process::Command::new(exe)
        .arg(crate::link::CONN)
        .stdin(std::process::Stdio::from(std::os::fd::OwnedFd::from(
            theirs,
        )))
        .stdout(std::process::Stdio::from(out))
        .spawn()?;
    // std never waits on a dropped child.
    Ok((ours, fuxix::process::Pid::from_u32(child.id())))
}

/// Starts pane process `fux __pane` from `exe`, listening at `socket` on
/// its stdin, in a session of its own: its pid and the server's link to
/// it. Why it could not, if it could not.
pub fn start_pane(exe: &Path, socket: &Path) -> Result<(fuxix::process::Pid, Stream), String> {
    let at = |doing: &str| {
        let socket = socket.display().to_string();
        let doing = doing.to_owned();
        move |e: std::io::Error| format!("{doing} {socket}: {e}")
    };
    // One a process still listens at is a pane's, whatever the record
    // says: never let go as debris. One no one does was left by a pane
    // that ended.
    // Asked without waiting: one that listens and takes no one (stopped,
    // its backlog full) listens all the same.
    match reach(socket).map_err(at("reaching for"))? {
        super::Reach::Listening(_) | super::Reach::Busy => {
            return Err(format!(
                "a pane's process listens at {} already",
                socket.display()
            ));
        }
        super::Reach::NoListener => {}
    }
    let _ = std::fs::remove_file(socket);
    let listener = UnixListener::bind(socket).map_err(at("binding its socket"))?;
    std::fs::set_permissions(socket, PermissionsExt::from_mode(0o600)).map_err(at("its socket"))?;
    // The file just bound, as the pane's process is told it: what it may
    // remove as it ends, whatever is at the path by then (`own`).
    let (device, inode) = fs::symlink_metadata(socket)
        .map(|meta| identity(&meta))
        .map_err(at("its socket"))?;
    let mut command = std::process::Command::new(exe);
    command
        .arg(crate::link::PANE)
        .arg(device.to_string())
        .arg(inode.to_string())
        .stdin(std::process::Stdio::from(std::os::fd::OwnedFd::from(
            listener,
        )))
        .stdout(std::process::Stdio::null());
    fuxix::process::lead_session(&mut command, false, fuxix::process::Dispositions::Default);
    let child = command
        .spawn()
        .map_err(|e| format!("starting its process, {}: {e}", exe.display()))?;
    let pid = fuxix::process::Pid::of(&child);
    // fux reaps it by its pid; std never waits on a dropped child.
    drop(child);
    let pid = pid.ok_or("its process has no pid")?;
    let stream = UnixStream::connect(socket).map_err(at("reaching its process at"))?;
    Ok((pid, stream))
}

/// A pane process's socket, on its stdin, the path it was bound at, and
/// that file as its server bound it (`args`: its device and inode, as
/// `start_pane` says them), which it alone may remove (`Own`).
pub fn pane_listener(args: &[String]) -> Result<(PaneListener, Option<PathBuf>, Own), String> {
    use std::os::fd::AsFd;
    let bound = match args {
        [device, inode] => device.parse::<u64>().ok().zip(inode.parse::<u64>().ok()),
        _ => None,
    };
    let bound = bound.ok_or("it takes its socket's device and inode")?;
    let listener = std::io::stdin()
        .as_fd()
        .try_clone_to_owned()
        .map(UnixListener::from)
        .map_err(|e| format!("its socket: {e}"))?;
    let socket =
        (listener.local_addr().ok()).and_then(|address| address.as_pathname().map(Path::to_owned));
    // The file at its path now is its own only if it is the one bound: a
    // path replaced since (its run's directory made anew before this
    // process ran) is another's, and never removed by it.
    let own = (socket.as_deref())
        .and_then(|path| Pinned::new(path).ok())
        .filter(|pinned| pinned.identity == bound);
    Ok((listener, socket, Own(own)))
}

/// Reaches for what listens at `socket`, without waiting: a link to it; or
/// that nothing listens there (no such file, or one no process listens
/// at); or that one does and has no room for another now (its backlog is
/// full: `EAGAIN`). Any other failure says nothing of what is there, and
/// is the error. macOS and the BSDs refuse a connect to a full backlog as
/// they do one to no listener, so a pane that full is found gone there:
/// it takes more connections than a pane's backlog holds, each left
/// untaken by a pane that stays stopped.
pub fn reach(socket: &Path) -> io::Result<super::Reach> {
    let fd = fuxix::socket::stream()?;
    fuxix::io::set_nonblocking(&fd, true)?;
    match fuxix::socket::connect(&fd, socket) {
        Ok(()) => Ok(super::Reach::Listening(UnixStream::from(fd))),
        Err(fuxix::Errno::AGAIN) => Ok(super::Reach::Busy),
        Err(errno) => {
            let error = io::Error::from(errno);
            let none = matches!(
                error.kind(),
                io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
            );
            if none {
                Ok(super::Reach::NoListener)
            } else {
                Err(error)
            }
        }
    }
}

/// The process at the other end of `stream`, as the system names it:
/// the one that listens there (`SO_PEERCRED`), where it names one (not on
/// macOS).
pub fn peer_pid(stream: &Stream) -> Option<fuxix::process::Pid> {
    fuxix::socket::peer(stream).ok()?.pid
}

/// A pane's process's own socket file, the one its server bound for it
/// (`start_pane`, `pane_listener`): what it may remove as it ends. A file
/// put at the same path since is another's. None where the file at its
/// path was not that one as the process began.
pub struct Own(Option<Pinned>);

/// Whether the socket file at `path` is still the one `own` names: what
/// a server taking its run back finds its pane by.
pub fn own_at(own: &Own, path: &Path) -> bool {
    own.0.as_ref().is_some_and(|own| own.still_at(path))
}

/// Removes the socket file at `path` if it is still the one `own` names;
/// one that could not be named is left.
pub fn unlink_own(own: &Own, path: &Path) {
    if own_at(own, path) {
        let _ = fs::remove_file(path);
    }
}

/// Whether the process `pid` still runs (`gone`).
pub fn runs(pid: fuxix::process::Pid) -> bool {
    !gone(pid)
}

/// Whether `error` says this process has every descriptor its limit
/// allows (`ulimit -n`), not that the whole system has none left.
pub fn out_of_descriptors(error: &std::io::Error) -> bool {
    error.raw_os_error() == Some(fuxix::Errno::MFILE.raw())
}

/// The server's listener, made to accept without waiting.
pub fn listening(listener: &Listener) -> std::io::Result<()> {
    listener.set_nonblocking(true)
}

/// The file a process done with its stdout points it at.
pub const NULL: &str = "/dev/null";

/// Keeps the fux this process runs at `exe`: a link to it where one can be
/// made, else a copy of it, there whole or not at all.
pub fn keep_executable(exe: &Path) -> std::io::Result<()> {
    let running = std::env::current_exe()?;
    if fs::hard_link(&running, exe).is_ok() {
        return Ok(());
    }
    // The running binary itself, though the file was replaced since.
    let source = match Path::new("/proc/self/exe").exists() {
        true => PathBuf::from("/proc/self/exe"),
        false => running,
    };
    super::copy_whole(&source, exe)
}

/// Runs `exe` in this process's place, with this process's arguments: it
/// returns only with why it could not.
pub fn run_pinned(exe: &Path) -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;
    Err(std::process::Command::new(exe)
        .args(std::env::args_os().skip(1))
        .exec())
}

/// The pid of the process listening at the other end of `stream`, where
/// a takeover must check it: never on Unix, where a pane's greeting is
/// all a server goes by.
pub fn listener_pid(_stream: &Stream) -> Option<fuxix::process::Pid> {
    None
}

/// A socket of this process's at `path`, any file there replaced, read
/// without waiting: where an attached client's connection takes bodies.
pub fn listen_at(path: &Path) -> std::io::Result<Listener> {
    let _ = fs::remove_file(path);
    let listener = UnixListener::bind(path)?;
    listener.set_nonblocking(true)?;
    Ok(listener)
}

/// A connection's process's client and its link to its server, as `fux
/// __conn` is given them: on stdout and stdin.
pub fn taken_stdio() -> std::io::Result<(Stream, Stream)> {
    use std::os::fd::AsFd;
    let server = std::io::stdin().as_fd().try_clone_to_owned()?;
    let client = std::io::stdout().as_fd().try_clone_to_owned()?;
    Ok((client.into(), server.into()))
}

/// Unix's terminals need nothing got ready: nothing to say.
pub fn prepare_terminals(_notice: impl Fn(&str)) -> Option<String> {
    None
}

/// Unix's terminals need nothing chosen.
pub fn load_terminals() {}

/// A client's connection to its server: the same methods on every OS.
pub type Client = UnixStream;

/// The signals that detach an attached client.
pub const CLIENT_STOPS: [fuxix::process::Signal; 3] = crate::STOPS;

/// What starts a server at `path`, quoted for a shell.
pub(super) fn start_another(path: &str) -> String {
    format!("`FUX_SOCKET={path} fux '(start)'` starts one")
}

/// Connects to the server at `socket`.
pub fn dial(socket: &SocketPath) -> std::io::Result<Client> {
    UnixStream::connect(socket.path())
}

/// What a refusal to attach adds.
pub const NOT_A_TERMINAL: &str = "";

/// This process's user, as a refusal says it.
pub fn this_user() -> String {
    geteuid().to_string()
}

/// Who serves `client`, as the system says.
pub fn client_peer(client: &Client) -> fuxix::Result<fuxix::socket::Peer> {
    fuxix::socket::peer(client)
}

/// The server's log at `path`, opened to append: a regular file alone. A
/// link is not followed, a FIFO with no reader refused at once (`ENXIO`),
/// not waited on, and a terminal there never becomes this process's.
pub fn open_log(path: &Path) -> std::io::Result<File> {
    let flags = fuxix::file::NOFOLLOW | fuxix::file::NONBLOCK | fuxix::file::NOCTTY;
    let file = (OpenOptions::new().create(true).append(true))
        .custom_flags(flags)
        .open(path)?;
    // Nonblocking changes nothing for a regular file's writes.
    match file.metadata()?.is_file() {
        true => Ok(file),
        false => Err(std::io::Error::other("not a regular file")),
    }
}

/// A server being started: what it says on stdout, then its end.
pub struct Starting {
    child: std::process::Child,
}

/// Starts `exe` with `args` in the background, its stderr `log`, its stdout
/// a pipe to read. A daemon (daemon(7)): a
/// session of its own, no descriptors of the client's but stdio, no
/// directory kept busy.
pub fn start_server(
    exe: &Path,
    args: &[&std::ffi::OsStr],
    _socket: &SocketPath,
    log: File,
) -> std::io::Result<Starting> {
    let mut command = std::process::Command::new(exe);
    command
        .args(args)
        .current_dir("/")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(log);
    fuxix::process::lead_session(&mut command, false, fuxix::process::Dispositions::Default);
    Ok(Starting {
        child: command.spawn()?,
    })
}

impl Starting {
    /// What it writes on stdout, to read until it closes.
    pub fn output(&mut self) -> Option<impl std::io::Read + use<>> {
        self.child.stdout.take()
    }

    /// Waits for it to end: how, as words. Dropped instead, it runs on.
    pub fn ended(mut self) -> std::io::Result<String> {
        self.child.wait().map(|status| status.to_string())
    }
}

/// Where `fux help` says a server is found.
pub const SOCKET_HELP: &str = "(`FUX_SOCKET=/abs/path fux …`); else\n\
     server.sock in $XDG_RUNTIME_DIR/fux, or with XDG_RUNTIME_DIR unset\n\
     $TMPDIR/fux.";

#[cfg(test)]
mod tests {
    use super::super::{base_directory, checked};
    use super::*;

    /// Whether a file exists at `path` at all, a link not followed.
    fn exists(path: &Path) -> bool {
        fs::symlink_metadata(path).is_ok()
    }

    fn bind(path: &Path) -> Result<(Endpoint, UnixListener), Error> {
        bind_socket(&checked(path, "test")?)
    }

    fn scratch(name: &str) -> std::io::Result<PathBuf> {
        Ok(fuxix::file::scratch()?.join(format!("fux-socket-{name}-{}", std::process::id())))
    }

    #[test]
    fn paths_are_checked_for_shape_and_length() {
        assert!(checked("", "X").is_err());
        assert!(checked("relative/s.sock", "X").is_err());
        assert!(checked("/", "X").is_err());
        assert!(checked("/s.sock", "X").is_err());
        let long: String = "/d/"
            .chars()
            .chain(std::iter::repeat_n('a', fuxix::socket::max_path_len()))
            .collect();
        let too_long = checked(&long, "X");
        assert!(too_long.is_err_and(|e| e.to_string().contains("limit")));
        assert!(checked("/tmp/fux/s.sock", "X").is_ok());
        assert!(base_directory("TMPDIR", "").is_err());
        assert!(base_directory("TMPDIR", "rel").is_err());
        assert_eq!(
            base_directory("TMPDIR", "/t").ok(),
            Some(PathBuf::from("/t/fux"))
        );
        let message = |r: Result<SocketPath, Error>| r.err().map(|e| e.to_string());
        for (result, expected) in [
            (
                checked("", "X"),
                "X is empty; it must be an absolute socket path",
            ),
            (checked("a\0b", "X"), "X contains a NUL byte"),
            (
                checked("relative/s.sock", "X"),
                "X is \"relative/s.sock\"; it must be an absolute path",
            ),
            (
                checked("/s", "X"),
                "X is \"/s\"; it must name a socket file in a directory below /",
            ),
            (
                base_directory("TMPDIR", "").and_then(|d| checked(&d, "X")),
                "TMPDIR is set but empty; set it to a directory or set FUX_SOCKET",
            ),
            (
                base_directory("TMPDIR", "rel").and_then(|d| checked(&d, "X")),
                "TMPDIR is \"rel\", not an absolute path; fix it or set FUX_SOCKET",
            ),
        ] {
            assert_eq!(message(result).as_deref(), Some(expected));
        }
        // No server at a socket chosen by its path: what starts that one.
        assert!(
            Error::NoServer(PathBuf::from("/x/y/absent.sock"))
                .to_string()
                .ends_with("`FUX_SOCKET=/x/y/absent.sock fux '(start)'` starts one")
        );
        // A path with a blank is quoted, so the command can be pasted.
        assert!(
            Error::NoServer(PathBuf::from("/x/my dir/absent.sock"))
                .to_string()
                .ends_with("`FUX_SOCKET='/x/my dir/absent.sock' fux '(start)'` starts one")
        );
        // As is one a shell would read otherwise: `;`, a quote.
        assert!(
            Error::NoServer(PathBuf::from("/x/a;b'c.sock"))
                .to_string()
                .ends_with(r###"`FUX_SOCKET='/x/a;b'"'"'c.sock' fux '(start)'` starts one"###)
        );
    }

    #[test]
    fn bind_is_private_single_owner_and_cleans_up_only_its_own_socket()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = scratch("bind")?;
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).map_err(|e| e.to_string())?;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
        let path = root.join("fux").join("s.sock");
        let (endpoint, listener) = bind(&path)?;
        let mode = |p: &Path| fs::metadata(p).map(|m| m.permissions().mode() & 0o777).ok();
        assert_eq!(mode(&path), Some(0o600));
        assert_eq!(mode(&root.join("fux")), Some(0o700));
        // A second server on the same path is refused while the first
        // lives, naming it where the system says which process it is; and
        // still with the one lock file there is removed, as a cleaner of
        // old files would: the run's lock is on its directory.
        let using = format!("is already using {}", path.display());
        for removed in [false, true] {
            // Every file beside the socket named after it: on a build
            // whose lock is one of them, the next bind is not refused so.
            for entry in fs::read_dir(root.join("fux"))?.flatten() {
                let beside = entry.file_type()?.is_file()
                    && entry.file_name().to_string_lossy().starts_with("s.sock");
                if removed && beside {
                    fs::remove_file(entry.path())?;
                }
            }
            let second = bind(&path).err().map(|e| e.to_string());
            let second = second.ok_or("a second server took the socket")?;
            assert!(
                second.starts_with("another fux server") && second.contains(&using),
                "{second}"
            );
            #[cfg(target_os = "linux")]
            assert!(
                second.contains(&format!("(process {})", std::process::id())),
                "{second}"
            );
        }
        // A client connects, and its peer is this user.
        let client = UnixStream::connect(&path).map_err(|e| e.to_string())?;
        let (served, _) = listener.accept().map_err(|e| e.to_string())?;
        assert!(fuxix::socket::peer(&served).is_ok_and(|p| p.user.is_current()));
        drop((client, served, listener, endpoint));
        assert!(!exists(&path), "the socket is removed at exit");
        // A stale socket (nothing listening) is replaced, even one whose
        // connect is refused (mode 0000).
        let stale = UnixListener::bind(&path).map_err(|e| e.to_string())?;
        drop(stale);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).map_err(|e| e.to_string())?;
        let (endpoint, _listener) = bind(&path)?;
        // A replacement bound by someone else at the same path is left alone.
        fs::remove_file(&path).map_err(|e| e.to_string())?;
        let replacement = UnixListener::bind(&path).map_err(|e| e.to_string())?;
        drop(endpoint);
        assert!(
            exists(&path),
            "another program's socket survives our cleanup"
        );
        // A socket something listens on, though it holds no lock, is never
        // taken over.
        assert!(bind(&path).is_err_and(|e| e.to_string().contains("already listening")));
        assert!(exists(&path), "the live socket is kept");
        drop(replacement);
        let _ = fs::remove_dir_all(&root);
        Ok(())
    }

    /// A private directory of the test `name`'s own, and a socket's path
    /// in it.
    fn private(name: &str) -> Result<(PathBuf, PathBuf), Box<dyn std::error::Error>> {
        let root = scratch(name)?;
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root)?;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
        let path = root.join("fux").join("s.sock");
        Ok((root, path))
    }

    /// The lock that orders the servers starting on `path`, held as a
    /// server starting or stopping there holds it.
    fn ahead(path: &Path) -> Result<File, Box<dyn std::error::Error>> {
        let (starting, _) = lock_file(path, ".starting")?;
        starting.lock()?;
        Ok(starting)
    }

    /// A server that stops takes its turn among those starting before its
    /// socket stops answering: with the turn held by another, its socket
    /// is still there, and takes a client; it goes once the turn is had.
    /// (The socket went first: a client that then found no server started
    /// one, which found no turn taken and was refused for the run's lock,
    /// not held up.)
    #[test]
    fn a_stopping_server_takes_its_turn_before_its_socket_goes()
    -> Result<(), Box<dyn std::error::Error>> {
        let (root, path) = private("order")?;
        let (mut endpoint, listener) = bind(&path)?;
        let turn = ahead(&path)?;
        let stopping = std::thread::spawn(move || {
            endpoint.stop(listener);
            endpoint
        });
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(!stopping.is_finished(), "it stopped without its turn");
        assert!(
            UnixStream::connect(&path).is_ok(),
            "its socket went before its turn was had"
        );
        drop(turn);
        let endpoint = stopping.join().map_err(|_| "the stop failed")?;
        assert!(!exists(&path), "its socket is gone once it stopped");
        // The turn is its own now, and the run's lock still.
        assert!(endpoint.starting.is_some(), "it holds no turn");
        assert!(matches!(lock_run(&path, false), Ok(Run::Theirs(_))));
        drop(endpoint);
        let _ = fs::remove_dir_all(&root);
        Ok(())
    }

    /// A start waits its turn no longer than `TURN_WAIT`: with the turn
    /// kept by another all that while (a server stopped as it stops), it
    /// is refused, saying who keeps it, and has made no socket and taken
    /// no run's lock; once the turn is free, a start is made as ever.
    /// (It waited without end, and its client with it.)
    #[test]
    fn a_start_waits_its_turn_only_so_long() -> Result<(), Box<dyn std::error::Error>> {
        let (root, path) = private("turn")?;
        private_directory(&checked(&path, "test")?, true)?;
        let turn = ahead(&path)?;
        let began = std::time::Instant::now();
        let refused = bind(&path).err().map(|e| e.to_string());
        let refused = refused.ok_or("a start took the socket out of turn")?;
        assert!(began.elapsed() >= super::super::TURN_WAIT, "{refused}");
        assert!(refused.contains("s.sock.starting"), "{refused}");
        #[cfg(target_os = "linux")]
        assert!(
            refused.contains(&format!("(process {})", std::process::id())),
            "{refused}"
        );
        assert!(!exists(&path), "the refused start made a socket");
        assert!(matches!(lock_run(&path, false), Ok(Run::None)));
        drop(turn);
        let (endpoint, _listener) = bind(&path)?;
        drop(endpoint);
        let _ = fs::remove_dir_all(&root);
        Ok(())
    }

    fn inode(path: &Path) -> Result<u64, String> {
        fs::symlink_metadata(path)
            .map(|m| m.ino())
            .map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Whether the filesystem under `dir` gives a freed inode number to
    /// the next file it makes, as ext4 does and APFS does not.
    fn reuses_inode_numbers(dir: &Path) -> Result<bool, String> {
        let probe = dir.join("probe.sock");
        for _ in 0..5 {
            let first = UnixListener::bind(&probe).map_err(|e| e.to_string())?;
            let number = inode(&probe)?;
            drop(first);
            fs::remove_file(&probe).map_err(|e| e.to_string())?;
            let second = UnixListener::bind(&probe).map_err(|e| e.to_string())?;
            let again = inode(&probe)?;
            drop(second);
            fs::remove_file(&probe).map_err(|e| e.to_string())?;
            if number == again {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// A socket that replaces fux's own after fux's listener closed keeps
    /// its place when fux's endpoint is dropped, and a stale socket's pin
    /// tells a replacement from the file it pinned, even where the
    /// replacement gets the freed inode number.
    /// The test says whether its filesystem reuses numbers, since only
    /// there does it test anything a plain identity check would not.
    #[test]
    fn cleanup_leaves_a_socket_that_replaced_ours_where_inodes_are_reused()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = scratch("reuse")?;
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("fux")).map_err(|e| e.to_string())?;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
        fs::set_permissions(root.join("fux"), fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;
        let reuse = reuses_inode_numbers(&root.join("fux"))?;
        eprintln!(
            "inode numbers under {} are {}",
            root.display(),
            if reuse {
                "reused: the guarantee is exercised"
            } else {
                "not reused: the guarantee holds here without the pin"
            }
        );
        let path = root.join("fux").join("s.sock");
        // At exit: fux's listener closes, someone replaces the socket, then
        // fux's endpoint is dropped.
        let (endpoint, listener) = bind(&path)?;
        let ours = inode(&path)?;
        drop(listener);
        fs::remove_file(&path).map_err(|e| e.to_string())?;
        let replacement = UnixListener::bind(&path).map_err(|e| e.to_string())?;
        let theirs = inode(&path)?;
        eprintln!("fux's socket was inode {ours}, the replacement's {theirs}");
        drop(endpoint);
        assert!(exists(&path), "the replacement survives fux's cleanup");
        drop(replacement);
        fs::remove_file(&path).map_err(|e| e.to_string())?;
        // Binding: a stale socket is pinned, then replaced before it could be
        // removed. The pin must not mistake the replacement for it.
        drop(UnixListener::bind(&path).map_err(|e| e.to_string())?);
        let pin = Pinned::new(&path).map_err(|e| e.to_string())?;
        fs::remove_file(&path).map_err(|e| e.to_string())?;
        let replacement = UnixListener::bind(&path).map_err(|e| e.to_string())?;
        assert!(
            !pin.still_at(&path),
            "the pin took a replacement for the stale socket"
        );
        drop(replacement);
        let _ = fs::remove_dir_all(&root);
        Ok(())
    }

    #[test]
    fn a_shared_directory_is_refused() -> Result<(), Box<dyn std::error::Error>> {
        let root = scratch("shared")?;
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("fux")).map_err(|e| e.to_string())?;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
        fs::set_permissions(root.join("fux"), fs::Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;
        let error = bind(&root.join("fux").join("s.sock"));
        let error = error.err().map(|e| e.to_string()).unwrap_or_default();
        assert!(
            error.contains("mode 0700") && error.contains("mode 0755"),
            "{error}"
        );
        // A client is refused there too.
        let socket = root.join("fux").join("s.sock");
        std::os::unix::net::UnixListener::bind(&socket)?;
        let socket = checked(&socket, "socket")?;
        let error = check_client_socket(&socket).err().map(|e| e.to_string());
        let error = error.unwrap_or_default();
        assert!(error.contains("mode 0755"), "{error}");
        let _ = fs::remove_dir_all(&root);
        Ok(())
    }
}
