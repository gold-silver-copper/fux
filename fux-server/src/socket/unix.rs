//! The server's socket on Unix, and the rules that keep it private.
//!
//! - The path is `FUX_SOCKET`, else `server.sock` in fux's directory:
//!   fux's directory is `$XDG_RUNTIME_DIR/fux` whenever that is
//!   set (even empty, which is refused), else `$TMPDIR/fux`.
//! - Its directory is owned by the user, mode 0700; the directories above
//!   it are not checked (`private_directory`). The socket is mode 0600, set
//!   before `listen`.
//! - A lock file beside the socket makes one server its only owner.
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

/// The lock that makes this server its only owner, and the socket it bound.
/// Dropping it removes the socket if it is still the one this server bound.
pub struct Endpoint {
    path: PathBuf,
    socket: Pinned,
    _lock: File,
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        if self.socket.still_at(&self.path) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Whether a server listens on `path`, without waiting: a nonblocking
/// connect answers at once. A listener with a full backlog refuses with
/// `EAGAIN`, and counts as alive. One whose maker, as the system names it
/// (`SO_PEERCRED`: the process that listened), is gone is no server's: a
/// crashed server's socket, still held by a process of its own that has
/// yet to see it go (its connection's, waiting for a client, `conn::wait`,
/// which never listens itself).
pub fn listened(path: &Path) -> bool {
    let Ok(fd) = fuxix::socket::stream() else {
        return false;
    };
    if fuxix::io::set_nonblocking(&fd, true).is_err() {
        return false;
    }
    match fuxix::socket::connect(&fd, path) {
        Ok(()) => !fuxix::socket::peer(&fd)
            .ok()
            .and_then(|peer| peer.pid)
            .is_some_and(gone),
        Err(fuxix::Errno::AGAIN | fuxix::Errno::INPROGRESS) => true,
        Err(_) => false,
    }
}

/// Whether a server answers on `socket`: something listens there
/// (`listened`), and it is not a crashed server's, held by a process of its
/// with a backlog full or not.
pub fn owned(socket: &SocketPath) -> bool {
    listened(socket.path()) && !crashed(socket.path())
}

/// Whether the run on the socket at `path` lost its server: its record is
/// left, which a stop removes, and no server holds the socket's lock, asked
/// as a starting server does, once none is starting there.
fn crashed(path: &Path) -> bool {
    if !panes(path).join("run").exists() {
        return false;
    }
    let Ok((starting, _)) = lock_file(path, ".starting") else {
        return false;
    };
    starting.lock().is_ok()
        && matches!(
            lock_file(path, ".lock").map(|(lock, _)| lock.try_lock()),
            Ok(Ok(()))
        )
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

/// The lock file beside the socket at `path`, its name ending `end`, made
/// if it is not there, and its path.
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
        // With the lock held, one that listens is no live fux server's,
        // but still refused, unless its run's record is left: then it is a
        // crashed one's, held by a process of its (`listened`, full or not).
        Ok(_) if listened(path) && !panes(path).join("run").exists() => {
            return Err(Error::Refused(format!(
                "another fux server is already listening on {}",
                path.display()
            )));
        }
        // No answer, or a crashed server's socket a process of its still
        // holds: with the lock held, no server owns it.
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
    };
    // From here a failure drops `endpoint`, which removes the socket. The mode
    // is set before `listen`, so no connection is accepted on an open socket.
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(io(path))?;
    fuxix::socket::listen(&fd, 128).map_err(failed("listening on ", path))?;
    Ok((endpoint, UnixListener::from(fd)))
}

/// One end of a link between fux's own processes, or a client's.
pub type Stream = UnixStream;
/// The server's listening socket.
pub type Listener = UnixListener;
/// A client's connection, as accepted: to serve, or to hand to its
/// connection's process.
pub type Accepted = UnixStream;
/// What a child is handed as a standard stream.
pub type Handed = std::os::fd::OwnedFd;
/// A pane's process's socket, where its server and connections reach it.
pub type PaneListener = UnixListener;

/// Connects to the socket at `path`.
pub fn connect(path: &Path) -> std::io::Result<Stream> {
    UnixStream::connect(path)
}

/// `stream` read and written without waiting.
pub fn nonblocking(stream: &Stream) -> std::io::Result<()> {
    stream.set_nonblocking(true)
}

/// The next client waiting at `listener`.
pub fn accept(listener: &mut Listener) -> std::io::Result<Accepted> {
    listener.accept().map(|(stream, _)| stream)
}

/// Who connected on `accepted`, as the system says.
pub fn peer(accepted: &Accepted) -> fuxix::Result<fuxix::socket::Peer> {
    fuxix::socket::peer(accepted)
}

/// `accepted` as a stream to serve here.
pub fn serve(accepted: Accepted) -> std::io::Result<Stream> {
    Ok(accepted)
}

/// `accepted`, to hand to a child.
pub fn hand(accepted: Accepted) -> Handed {
    accepted.into()
}

/// The server's socket, for the next connection's process to wait at.
pub fn spare(listener: &Listener) -> std::io::Result<Handed> {
    Ok(listener.try_clone()?.into())
}

/// Starts `fux __conn` from `exe` with `words`, its link on stdin and
/// `out` on stdout: the server's end of its link.
pub fn launch_conn(exe: &Path, words: &[&str], out: Handed) -> std::io::Result<Stream> {
    let (ours, theirs) = UnixStream::pair()?;
    let child = std::process::Command::new(exe)
        .arg(crate::link::CONN)
        .args(words)
        .stdin(std::process::Stdio::from(std::os::fd::OwnedFd::from(
            theirs,
        )))
        .stdout(std::process::Stdio::from(out))
        .spawn()?;
    // Reaped as the server's other children are (`reap`); std never
    // waits on a dropped child.
    drop(child);
    Ok(ours)
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
    if UnixStream::connect(socket).is_ok() {
        return Err(format!(
            "a pane's process listens at {} already",
            socket.display()
        ));
    }
    let _ = std::fs::remove_file(socket);
    let listener = UnixListener::bind(socket).map_err(at("binding its socket"))?;
    std::fs::set_permissions(socket, PermissionsExt::from_mode(0o600)).map_err(at("its socket"))?;
    let mut command = std::process::Command::new(exe);
    command
        .arg(crate::link::PANE)
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

/// A pane process's socket, on its stdin, and the path it was bound at
/// (`args`: none on Unix).
pub fn pane_listener(args: &[String]) -> Result<(PaneListener, Option<PathBuf>), String> {
    use std::os::fd::AsFd;
    if !args.is_empty() {
        return Err("it takes no words".to_owned());
    }
    let listener = std::io::stdin()
        .as_fd()
        .try_clone_to_owned()
        .map(UnixListener::from)
        .map_err(|e| format!("its socket: {e}"))?;
    let socket =
        (listener.local_addr().ok()).and_then(|address| address.as_pathname().map(Path::to_owned));
    Ok((listener, socket))
}

/// Its server, the first to reach a pane's process at `listener`, waited
/// for; `listener` read without waiting from then on.
pub fn pane_first(listener: &mut PaneListener) -> Result<Stream, String> {
    // Listened on again, by it: one that reaches it is told its pid, not
    // the server's that made the socket (`SO_PEERCRED`).
    fuxix::socket::listen(&*listener, 16).map_err(|e| format!("its socket: {e}"))?;
    let (stream, _) = listener.accept().map_err(|e| format!("its server: {e}"))?;
    listener
        .set_nonblocking(true)
        .map_err(|e| format!("its socket: {e}"))?;
    Ok(stream)
}

/// The pid of what listens at `socket`, as the system says (`SO_PEERCRED`)
/// if it does, and a link to it; none if nothing does.
pub fn reach_named(socket: &Path) -> Option<(Option<fuxix::process::Pid>, Stream)> {
    let stream = UnixStream::connect(socket).ok()?;
    let pid = fuxix::socket::peer(&stream).ok().and_then(|peer| peer.pid);
    Some((pid, stream))
}

/// The server's listener, made to accept without waiting.
pub fn listening(listener: &Listener) -> std::io::Result<()> {
    listener.set_nonblocking(true)
}

/// The file a process done with its stdout points it at.
pub const NULL: &str = "/dev/null";

/// Keeps the fux this process runs at `exe`: a link to it where one can be
/// made, else a copy of it.
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
    fs::copy(source, exe).map(drop)
}

/// Runs `exe` in this process's place, with this process's arguments: it
/// returns only with why it could not.
pub fn run_pinned(exe: &Path) -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;
    Err(std::process::Command::new(exe)
        .args(std::env::args_os().skip(1))
        .exec())
}

/// Whether a connect that failed so says nothing listens there: on Unix,
/// every failure does.
pub fn none_listens(_error: &std::io::Error) -> bool {
    true
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
/// __conn taken` is given them: on stdout and stdin.
pub fn taken_stdio() -> std::io::Result<(Stream, Stream)> {
    use std::os::fd::AsFd;
    let server = std::io::stdin().as_fd().try_clone_to_owned()?;
    let client = std::io::stdout().as_fd().try_clone_to_owned()?;
    Ok((client.into(), server.into()))
}

/// Unix's terminals need nothing got ready: nothing to say.
pub fn prepare_terminals() -> Option<String> {
    None
}

/// Unix's terminals need nothing chosen.
pub fn load_terminals() {}

#[cfg(all(test, unix))]
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
        // A second server on the same path is refused while the first lives.
        let second = bind(&path);
        assert_eq!(
            second.err().map(|e| e.to_string()),
            Some(format!(
                "another fux server is already using {}",
                path.display()
            ))
        );
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
