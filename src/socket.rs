//! Where the server's socket lives, and the rules that keep it private.
//!
//! - The path is `str PATH socket`'s, or `str NAME server`'s
//!   `NAME.sock` in fux's directory, else `FUX_SOCKET`, else `server.sock`
//!   there: fux's directory is `$XDG_RUNTIME_DIR/fux`, else `$TMPDIR/fux`.
//! - Its directory is owned by the user, mode 0700, reached only through
//!   directories no other user can rewrite. The socket is mode 0600, set
//!   before `listen`.
//! - A lock file beside the socket makes one server its only owner.
//! - A leftover socket is replaced only when nothing answers on it.
//! - At exit the socket is removed only if it is still the one this server
//!   bound. On Linux an `O_PATH` descriptor keeps its inode allocated so the
//!   number cannot be reused by a replacement (bevy-final finding 014).
//! - Every accepted peer must run as the server's own user.
use fuxix::process::geteuid;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};

const DEFAULT_NAME: &str = "server.sock";

/// Why a socket path is refused, or its directory, or the socket itself.
#[derive(Debug)]
pub enum Error {
    /// No socket at the path: no server runs there.
    NoServer(PathBuf),
    /// A call about `path` that failed: what it was doing, as the message
    /// begins ("locking "), and why.
    Io {
        doing: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    /// Anything else refused: why.
    Refused(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NoServer(path) => {
                write!(
                    f,
                    "no fux server is running at {}; {}",
                    path.display(),
                    start_hint(path)
                )
            }
            Error::Io {
                doing,
                path,
                source,
            } => write!(f, "{doing}{}: {source}", path.display()),
            Error::Refused(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for Error {}

/// An I/O error about `path`.
fn io<'a>(path: &'a Path) -> impl FnOnce(io::Error) -> Error + 'a {
    failed("", path)
}

/// An error about `path`, while `doing` what the message begins with.
fn failed<'a, E: Into<io::Error>>(
    doing: &'static str,
    path: &'a Path,
) -> impl FnOnce(E) -> Error + 'a {
    move |source| Error::Io {
        doing,
        path: path.to_owned(),
        source: source.into(),
    }
}

/// A socket's path, checked: absolute, short enough for `sockaddr_un`, and
/// naming a file in a directory that has a parent, the directory's own
/// privacy being checked against that parent's.
#[derive(Clone, Debug)]
pub struct SocketPath {
    path: PathBuf,
    directory: PathBuf,
    above: PathBuf,
}

impl SocketPath {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The directory holding the socket, and its log.
    pub fn directory(&self) -> &Path {
        &self.directory
    }
}

/// Which server a command names, before the command: `str PATH socket`, `str NAME server`,
/// or neither.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Choice<'a> {
    #[default]
    Default,
    /// A socket's path, and the flag that gave it.
    Path(&'a str, &'static str),
    /// A server's name: its socket is `NAME.sock` in fux's directory.
    Name(&'a str),
}

impl Choice<'_> {
    /// Whether the command named its server, rather than taking it from
    /// the environment.
    pub fn named(&self) -> bool {
        !matches!(self, Choice::Default)
    }
}

/// The socket a command uses: the one `choice` names, else `FUX_SOCKET`,
/// else `server.sock` in fux's directory.
pub fn socket_path(choice: Choice<'_>) -> Result<SocketPath, Error> {
    match choice {
        Choice::Path(path, from) => checked(path, from),
        Choice::Name(name) => {
            let valid = (1..=64).contains(&name.len())
                && !name.starts_with(['.', '-'])
                && (name.chars()).all(|c| c.is_ascii_alphanumeric() || "._-".contains(c));
            if !valid {
                return Err(Error::Refused(format!(
                    "server {name:?}: a server's name is 1 to 64 letters, digits, '.', '_' or '-', \
                     not starting with '.' or '-'"
                )));
            }
            in_directory(&format!("{name}.sock"))
        }
        Choice::Default => match std::env::var_os("FUX_SOCKET") {
            Some(value) => {
                let value = value.into_string().map_err(|_| not_utf8("FUX_SOCKET"))?;
                checked(&value, "FUX_SOCKET")
            }
            None => in_directory(DEFAULT_NAME),
        },
    }
}

/// What starts the server at `path`: `fux start` the default's (or
/// `FUX_SOCKET`'s), the command that names its socket another's.
pub fn start_hint(path: &Path) -> String {
    let default = socket_path(Choice::Default).is_ok_and(|s| s.path == path);
    if default {
        return "`fux` in a terminal starts one, or `fux start` one with no terminal".into();
    }
    let name = (path.parent().zip(directory().ok()))
        .filter(|(parent, directory)| parent == directory)
        .and_then(|_| path.file_name()?.to_str()?.strip_suffix(".sock"));
    // Quoted for a shell, as the hint is a command to run.
    let words = match name {
        Some(name) => format!("str {} server", crate::words::quote(name)),
        None => format!(
            "str {} socket",
            crate::words::quote(&path.to_string_lossy())
        ),
    };
    format!("`fux {words} start` starts one")
}

/// fux's directory: under `XDG_RUNTIME_DIR`, else `TMPDIR`.
pub fn directory() -> Result<PathBuf, Error> {
    for base in ["XDG_RUNTIME_DIR", "TMPDIR"] {
        if let Some(value) = std::env::var_os(base) {
            let value = value.into_string().map_err(|_| not_utf8(base))?;
            return base_directory(base, &value);
        }
    }
    Err(Error::Refused(
        "no socket location: neither XDG_RUNTIME_DIR nor TMPDIR is set; \
         set FUX_SOCKET to an absolute socket path"
            .to_owned(),
    ))
}

fn not_utf8(variable: &str) -> Error {
    Error::Refused(format!("{variable} is not valid UTF-8"))
}

/// The socket `name` in fux's directory.
fn in_directory(name: &str) -> Result<SocketPath, Error> {
    let path = directory()?.join(name);
    checked(&path.to_string_lossy(), "the socket's directory")
}

fn base_directory(base: &'static str, value: &str) -> Result<PathBuf, Error> {
    if value.is_empty() {
        return Err(Error::Refused(format!(
            "{base} is set but empty; set it to a directory or set FUX_SOCKET"
        )));
    }
    if !Path::new(value).is_absolute() {
        return Err(Error::Refused(format!(
            "{base} is {value:?}, not an absolute path; fix it or set FUX_SOCKET"
        )));
    }
    Ok(Path::new(value).join("fux"))
}

fn checked(value: &str, from: &'static str) -> Result<SocketPath, Error> {
    let refused = |why: String| Err(Error::Refused(why));
    if value.is_empty() {
        return refused(format!(
            "{from} is empty; it must be an absolute socket path"
        ));
    }
    if value.contains('\0') {
        return refused(format!("{from} contains a NUL byte"));
    }
    let path = PathBuf::from(value);
    if !path.is_absolute() {
        return refused(format!("{from} is {value:?}; it must be an absolute path"));
    }
    let directories = path
        .file_name()
        .and(path.parent())
        .and_then(|directory| Some((directory.to_owned(), directory.parent()?.to_owned())));
    let Some((directory, above)) = directories else {
        return refused(format!(
            "{from} is {value:?}; it must name a socket file in a directory below /"
        ));
    };
    let limit = fuxix::socket::max_path_len();
    if path.as_os_str().len() > limit {
        return refused(format!(
            "the socket path is {} bytes, longer than the {limit}-byte limit for a Unix \
             domain socket on this platform (from {from}): {value}",
            value.len()
        ));
    }
    Ok(SocketPath {
        path,
        directory,
        above,
    })
}

/// The directory holding the socket must be ours, mode 0700. The
/// directories above it are not checked: inside a sandbox's user namespace
/// root's show as another uid (65534), and what keeps a connection private
/// is that each side checks the other's user (the server its clients', a
/// client its server's), whoever could rename what lies above. `create`
/// makes it when it is missing; nothing that already exists is modified.
fn private_directory(socket: &SocketPath, create: bool) -> Result<(), Error> {
    let (directory, above) = (socket.directory(), socket.above.as_path());
    let euid = geteuid();
    above
        .canonicalize()
        .map_err(failed("socket directory parent ", above))?;
    match fs::symlink_metadata(directory) {
        Err(error) if error.kind() == io::ErrorKind::NotFound && create => {
            match DirBuilder::new().mode(0o700).create(directory) {
                // Set explicitly: the umask must not decide who reaches the socket.
                Ok(()) => fs::set_permissions(directory, fs::Permissions::from_mode(0o700))
                    .map_err(io(directory))?,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(failed("creating ", directory)(error)),
            }
        }
        Err(error) => return Err(io(directory)(error)),
        Ok(_) => {}
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

/// Makes the socket's private directory if it is missing, checking it as a
/// server does; for a client about to start one.
pub fn prepare_directory(socket: &SocketPath) -> Result<(), Error> {
    private_directory(socket, true)
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

/// The lock that makes this server its only owner, and the socket it bound
/// if the system let it (a sandbox may deny sockets: a door serves instead,
/// `crate::door`). Dropping it removes the socket if it is still the one
/// this server bound.
pub struct Endpoint {
    path: PathBuf,
    socket: Option<Pinned>,
    _lock: File,
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        if self.socket.as_ref().is_some_and(|s| s.still_at(&self.path)) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Whether something listens on `path`, without waiting: a nonblocking
/// connect answers at once. A listener with a full backlog refuses with
/// `EAGAIN`, and counts as alive.
fn probe(path: &Path) -> io::Result<()> {
    let fd = fuxix::socket::stream()?;
    fuxix::io::set_nonblocking(&fd, true)?;
    match fuxix::socket::connect(&fd, path) {
        Ok(()) | Err(fuxix::Errno::AGAIN | fuxix::Errno::INPROGRESS) => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// Takes ownership of `path` and binds it. Refuses a live server's socket and
/// anything that is not a socket; replaces only a socket proven stale. Where
/// the system denies sockets the lock is taken all the same, with no
/// listener: its owner serves through its door alone.
pub fn bind_socket(socket: &SocketPath) -> Result<(Endpoint, Option<UnixListener>), Error> {
    let path = socket.path();
    private_directory(socket, true)?;
    let euid = geteuid();
    let foreign = |meta: &fs::Metadata| !meta.file_type().is_socket() || meta.uid() != euid;
    let refused = || {
        Error::Refused(format!(
            "{} exists and is not a socket owned by you; fux will not replace it",
            path.display()
        ))
    };
    if fs::symlink_metadata(path).is_ok_and(|meta| foreign(&meta)) {
        return Err(refused());
    }
    let mut lock_path = path.as_os_str().to_owned();
    lock_path.push(".lock");
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
        Ok(meta) if foreign(&meta) => return Err(refused()),
        Ok(_) if probe(path).is_ok() => {
            return Err(Error::Refused(format!(
                "another fux server is already listening on {}",
                path.display()
            )));
        }
        // No answer: with the lock held, no server owns it, whether connect
        // was refused or denied (a sandbox that forbids sockets).
        Ok(_) => {
            if stale.as_ref().is_ok_and(|stale| stale.still_at(path)) {
                fs::remove_file(path).map_err(failed("removing stale socket ", path))?;
            }
        }
    }
    let mut endpoint = Endpoint {
        path: path.to_owned(),
        socket: None,
        _lock: lock,
    };
    let bound = fuxix::socket::stream()
        .map_err(failed("a socket for ", path))
        .and_then(|fd| {
            fuxix::socket::bind(&fd, path).map_err(failed("binding ", path))?;
            Ok(fd)
        });
    let fd = match bound {
        Ok(fd) => fd,
        // The system denies sockets at all (a sandbox that forbids them): a
        // door may serve instead (`crate::door`).
        Err(Error::Io { source, .. }) if source.kind() == io::ErrorKind::PermissionDenied => {
            return Ok((endpoint, None));
        }
        Err(error) => return Err(error),
    };
    endpoint.socket = Some(Pinned::new(path).map_err(io(path))?);
    // From here a failure drops `endpoint`, which removes the socket. The mode
    // is set before `listen`, so no connection is accepted on an open socket.
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(io(path))?;
    fuxix::socket::listen(&fd, 128).map_err(failed("listening on ", path))?;
    Ok((endpoint, Some(UnixListener::from(fd))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;

    /// Whether a file exists at `path` at all, a link not followed.
    fn exists(path: &Path) -> bool {
        fs::symlink_metadata(path).is_ok()
    }

    fn bind(path: &Path) -> Result<(Endpoint, UnixListener), Error> {
        match bind_socket(&checked(&path.to_string_lossy(), "test")?)? {
            (endpoint, Some(listener)) => Ok((endpoint, listener)),
            (_, None) => Err(io(path)(io::ErrorKind::PermissionDenied.into())),
        }
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
                base_directory("TMPDIR", "").and_then(|d| checked(&d.to_string_lossy(), "X")),
                "TMPDIR is set but empty; set it to a directory or set FUX_SOCKET",
            ),
            (
                base_directory("TMPDIR", "rel").and_then(|d| checked(&d.to_string_lossy(), "X")),
                "TMPDIR is \"rel\", not an absolute path; fix it or set FUX_SOCKET",
            ),
        ] {
            assert_eq!(message(result).as_deref(), Some(expected));
        }
        // No server at a socket chosen by its path: what starts that one.
        assert!(
            Error::NoServer(PathBuf::from("/x/y/absent.sock"))
                .to_string()
                .ends_with("`fux str /x/y/absent.sock socket start` starts one")
        );
        // A path with a blank is quoted, so the command can be pasted.
        assert!(
            Error::NoServer(PathBuf::from("/x/my dir/absent.sock"))
                .to_string()
                .ends_with("`fux str '/x/my dir/absent.sock' socket start` starts one")
        );
        // As is one a shell would read otherwise: `;`, a quote.
        assert!(
            Error::NoServer(PathBuf::from("/x/a;b'c.sock"))
                .to_string()
                .ends_with(r"`fux str '/x/a;b'\''c.sock' socket start` starts one")
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
        assert_eq!(fuxix::socket::peer_uid(&served).ok(), Some(geteuid()));
        drop((client, served, listener, endpoint));
        assert!(!exists(&path), "the socket is removed at exit");
        // A stale socket (nothing listening) is replaced, even one whose
        // connect is denied, as a sandbox denies sockets.
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
    /// replacement gets the freed inode number (bevy-final finding 014).
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
        let _ = fs::remove_dir_all(&root);
        Ok(())
    }
}
