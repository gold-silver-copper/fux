//! Where the server's socket lives, and the rules that keep it private.
//!
//! - The path is `--socket`, else `FUX_SOCKET`, else
//!   `$XDG_RUNTIME_DIR/fux/server.sock`, else `$TMPDIR/fux/server.sock`.
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
    /// An environment variable that is not UTF-8.
    NotUtf8 {
        variable: &'static str,
    },
    /// Nowhere to put the socket.
    NoLocation,
    /// A base directory's variable, set but empty.
    EmptyVariable {
        variable: &'static str,
    },
    /// A base directory that is not absolute.
    RelativeVariable {
        variable: &'static str,
        value: String,
    },
    // A socket path, given by `from`, that is empty, has a NUL, is
    // relative, names no file in a directory below /, or is too long.
    EmptyPath {
        from: &'static str,
    },
    Nul {
        from: &'static str,
    },
    Relative {
        from: &'static str,
        value: String,
    },
    NoFile {
        from: &'static str,
        value: String,
    },
    TooLong {
        from: &'static str,
        value: String,
        limit: usize,
    },
    /// A directory above the socket's that another user could change.
    Shared {
        path: PathBuf,
        mode: u32,
        uid: u32,
    },
    Symlink(PathBuf),
    /// The socket's directory, not a directory of ours with mode 0700.
    NotPrivate {
        path: PathBuf,
        euid: u32,
        directory: bool,
        mode: u32,
        uid: u32,
    },
    NoServer(PathBuf),
    /// A socket not ours to connect to.
    NotOurs(PathBuf),
    /// Something at the socket's path that is not ours to replace.
    Foreign(PathBuf),
    /// Another server holds the lock.
    InUse(PathBuf),
    /// Another server answers on the socket.
    Listening(PathBuf),
    /// A call about `path` that failed: what it was doing, as the message
    /// begins ("locking "), and why.
    Io {
        doing: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    /// Whether a server listens could not be told.
    Probe {
        path: PathBuf,
        source: io::Error,
    },
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NotUtf8 { variable } => write!(f, "{variable} is not valid UTF-8"),
            Error::NoLocation => f.write_str(
                "no socket location: neither XDG_RUNTIME_DIR nor TMPDIR is set; \
                 set FUX_SOCKET to an absolute socket path",
            ),
            Error::EmptyVariable { variable } => write!(
                f,
                "{variable} is set but empty; set it to a directory or set FUX_SOCKET"
            ),
            Error::RelativeVariable { variable, value } => write!(
                f,
                "{variable} is {value:?}, not an absolute path; fix it or set FUX_SOCKET"
            ),
            Error::EmptyPath { from } => {
                write!(f, "{from} is empty; it must be an absolute socket path")
            }
            Error::Nul { from } => write!(f, "{from} contains a NUL byte"),
            Error::Relative { from, value } => {
                write!(f, "{from} is {value:?}; it must be an absolute path")
            }
            Error::NoFile { from, value } => {
                write!(
                    f,
                    "{from} is {value:?}; it must name a socket file in a directory below /"
                )
            }
            Error::TooLong { from, value, limit } => write!(
                f,
                "the socket path is {} bytes, longer than the {limit}-byte limit for a Unix \
                 domain socket on this platform (from {from}): {value}",
                value.len()
            ),
            Error::Shared { path, mode, uid } => write!(
                f,
                "{} (mode {mode:04o}, owner uid {uid}) could be changed by another user, so the socket \
                 beneath it would not be private; choose another location with FUX_SOCKET",
                path.display()
            ),
            Error::Symlink(path) => write!(
                f,
                "{} is a symbolic link; the socket directory must be a real directory",
                path.display()
            ),
            Error::NotPrivate {
                path,
                euid,
                directory,
                mode,
                uid,
            } => write!(
                f,
                "{} must be a directory owned by you (uid {euid}) with mode 0700; it is {} \
                 with mode {mode:04o} owned by uid {uid}. fux does not change it",
                path.display(),
                if *directory {
                    "a directory"
                } else {
                    "not a directory"
                },
            ),
            Error::NoServer(path) => {
                write!(f, "no fux server is running at {}", path.display())
            }
            Error::NotOurs(path) => write!(
                f,
                "{} is not a socket owned by you; refusing to connect",
                path.display()
            ),
            Error::Foreign(path) => write!(
                f,
                "{} exists and is not a socket owned by you; fux will not replace it",
                path.display()
            ),
            Error::InUse(path) => {
                write!(f, "another fux server is already using {}", path.display())
            }
            Error::Listening(path) => write!(
                f,
                "another fux server is already listening on {}",
                path.display()
            ),
            Error::Io {
                doing,
                path,
                source,
            } => write!(f, "{doing}{}: {source}", path.display()),
            Error::Probe { path, source } => write!(
                f,
                "cannot tell whether {} is in use ({source}); it was left in place",
                path.display()
            ),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io { source, .. } | Error::Probe { source, .. } => Some(source),
            Error::NotUtf8 { .. }
            | Error::NoLocation
            | Error::EmptyVariable { .. }
            | Error::RelativeVariable { .. }
            | Error::EmptyPath { .. }
            | Error::Nul { .. }
            | Error::Relative { .. }
            | Error::NoFile { .. }
            | Error::TooLong { .. }
            | Error::Shared { .. }
            | Error::Symlink(_)
            | Error::NotPrivate { .. }
            | Error::NoServer(_)
            | Error::NotOurs(_)
            | Error::Foreign(_)
            | Error::InUse(_)
            | Error::Listening(_) => None,
        }
    }
}

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

/// The socket a command uses: `flag` (a `--socket`), else `FUX_SOCKET`, else
/// the default under `XDG_RUNTIME_DIR` or `TMPDIR`.
pub fn socket_path(flag: Option<&str>) -> Result<SocketPath, Error> {
    if let Some(flag) = flag {
        return checked(flag, "--socket");
    }
    if let Some(value) = std::env::var_os("FUX_SOCKET") {
        let value = value.into_string().map_err(|_| Error::NotUtf8 {
            variable: "FUX_SOCKET",
        })?;
        return checked(&value, "FUX_SOCKET");
    }
    for base in ["XDG_RUNTIME_DIR", "TMPDIR"] {
        if let Some(value) = std::env::var_os(base) {
            let value = value
                .into_string()
                .map_err(|_| Error::NotUtf8 { variable: base })?;
            return socket_path_from(base, &value);
        }
    }
    Err(Error::NoLocation)
}

/// Longest socket path, in bytes, that `sockaddr_un` holds with its NUL.
pub fn max_path_bytes() -> usize {
    if cfg!(target_os = "linux") { 107 } else { 103 }
}

fn socket_path_from(base: &'static str, value: &str) -> Result<SocketPath, Error> {
    if value.is_empty() {
        return Err(Error::EmptyVariable { variable: base });
    }
    if !Path::new(value).is_absolute() {
        return Err(Error::RelativeVariable {
            variable: base,
            value: value.to_owned(),
        });
    }
    let path = Path::new(value).join("fux").join(DEFAULT_NAME);
    checked(&path.to_string_lossy(), base)
}

fn checked(value: &str, from: &'static str) -> Result<SocketPath, Error> {
    if value.is_empty() {
        return Err(Error::EmptyPath { from });
    }
    if value.contains('\0') {
        return Err(Error::Nul { from });
    }
    let path = PathBuf::from(value);
    let value = || value.to_owned();
    if !path.is_absolute() {
        return Err(Error::Relative {
            from,
            value: value(),
        });
    }
    let directories = path
        .file_name()
        .and(path.parent())
        .and_then(|directory| Some((directory.to_owned(), directory.parent()?.to_owned())));
    let Some((directory, above)) = directories else {
        return Err(Error::NoFile {
            from,
            value: value(),
        });
    };
    let limit = max_path_bytes();
    if path.as_os_str().len() > limit {
        return Err(Error::TooLong {
            from,
            value: value(),
            limit,
        });
    }
    Ok(SocketPath {
        path,
        directory,
        above,
    })
}

/// The directory holding the socket must be ours, mode 0700, reached only
/// through directories no other user can rewrite. `create` makes it when it
/// is missing; nothing that already exists is modified.
fn private_directory(socket: &SocketPath, create: bool) -> Result<(), Error> {
    let (directory, above) = (socket.directory(), socket.above.as_path());
    let euid = geteuid();
    let canonical = above
        .canonicalize()
        .map_err(failed("socket directory parent ", above))?;
    for ancestor in canonical.ancestors() {
        let meta = fs::metadata(ancestor).map_err(io(ancestor))?;
        let mode = meta.permissions().mode();
        let owned = meta.uid() == 0 || meta.uid() == euid;
        let shared = mode & 0o022 != 0;
        if !owned || (shared && mode & 0o1000 == 0) {
            return Err(Error::Shared {
                path: ancestor.to_owned(),
                mode: mode & 0o7777,
                uid: meta.uid(),
            });
        }
    }
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
        return Err(Error::Symlink(directory.to_owned()));
    }
    if !meta.is_dir() || meta.uid() != euid || mode & 0o077 != 0 {
        return Err(Error::NotPrivate {
            path: directory.to_owned(),
            euid,
            directory: meta.is_dir(),
            mode: mode & 0o7777,
            uid: meta.uid(),
        });
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
        return Err(Error::NotOurs(path.to_owned()));
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

/// The bound socket and the lock that makes this server its only owner.
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
/// anything that is not a socket; replaces only a socket proven stale.
pub fn bind_socket(socket: &SocketPath) -> Result<(Endpoint, UnixListener), Error> {
    let path = socket.path();
    private_directory(socket, true)?;
    let euid = geteuid();
    let foreign = |meta: &fs::Metadata| !meta.file_type().is_socket() || meta.uid() != euid;
    let refused = || Error::Foreign(path.to_owned());
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
        fs::TryLockError::WouldBlock => Error::InUse(path.to_owned()),
        fs::TryLockError::Error(error) => failed("locking ", &lock_path)(error),
    })?;
    // Pinned before the probe, so the file found dead is the one removed.
    let stale = Pinned::new(path);
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(io(path)(error)),
        Ok(meta) if foreign(&meta) => return Err(refused()),
        Ok(_) => match probe(path) {
            Ok(()) => return Err(Error::Listening(path.to_owned())),
            Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => {
                if stale.as_ref().is_ok_and(|stale| stale.still_at(path)) {
                    fs::remove_file(path).map_err(failed("removing stale socket ", path))?;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(Error::Probe {
                    path: path.to_owned(),
                    source,
                });
            }
        },
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;

    /// Whether a file exists at `path` at all, a link not followed.
    fn exists(path: &Path) -> bool {
        fs::symlink_metadata(path).is_ok()
    }

    fn bind(path: &Path) -> Result<(Endpoint, UnixListener), Error> {
        bind_socket(&checked(&path.to_string_lossy(), "test")?)
    }

    fn scratch(name: &str) -> PathBuf {
        let base = std::env::temp_dir()
            .canonicalize()
            .unwrap_or_else(|_| std::env::temp_dir());
        base.join(format!("fux-socket-{name}-{}", std::process::id()))
    }

    #[test]
    fn paths_are_checked_for_shape_and_length() {
        assert!(checked("", "X").is_err());
        assert!(checked("relative/s.sock", "X").is_err());
        assert!(checked("/", "X").is_err());
        assert!(checked("/s.sock", "X").is_err());
        let long: String = "/d/"
            .chars()
            .chain(std::iter::repeat_n('a', max_path_bytes()))
            .collect();
        let too_long = checked(&long, "X");
        assert!(matches!(too_long, Err(Error::TooLong { .. })));
        assert!(too_long.is_err_and(|e| e.to_string().contains("limit")));
        assert!(checked("/tmp/fux/s.sock", "X").is_ok());
        assert!(socket_path_from("TMPDIR", "").is_err());
        assert!(socket_path_from("TMPDIR", "rel").is_err());
        assert_eq!(
            socket_path_from("TMPDIR", "/t")
                .map(|s| s.path().to_owned())
                .ok(),
            Some(PathBuf::from("/t/fux/server.sock"))
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
                socket_path_from("TMPDIR", ""),
                "TMPDIR is set but empty; set it to a directory or set FUX_SOCKET",
            ),
            (
                socket_path_from("TMPDIR", "rel"),
                "TMPDIR is \"rel\", not an absolute path; fix it or set FUX_SOCKET",
            ),
        ] {
            assert_eq!(message(result).as_deref(), Some(expected));
        }
        assert_eq!(
            Error::NoLocation.to_string(),
            "no socket location: neither XDG_RUNTIME_DIR nor TMPDIR is set; \
             set FUX_SOCKET to an absolute socket path"
        );
    }

    #[test]
    fn bind_is_private_single_owner_and_cleans_up_only_its_own_socket()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = scratch("bind");
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
        assert!(matches!(second, Err(Error::InUse(_))));
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
        // A stale socket (nothing listening) is replaced.
        let stale = UnixListener::bind(&path).map_err(|e| e.to_string())?;
        drop(stale);
        let (endpoint, _listener) = bind(&path)?;
        // A replacement bound by someone else at the same path is left alone.
        fs::remove_file(&path).map_err(|e| e.to_string())?;
        let replacement = UnixListener::bind(&path).map_err(|e| e.to_string())?;
        drop(endpoint);
        assert!(
            exists(&path),
            "another program's socket survives our cleanup"
        );
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
        let root = scratch("reuse");
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
        let root = scratch("shared");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("fux")).map_err(|e| e.to_string())?;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
        fs::set_permissions(root.join("fux"), fs::Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;
        let error = bind(&root.join("fux").join("s.sock"));
        assert!(matches!(error, Err(Error::NotPrivate { mode: 0o755, .. })));
        let error = error.err().map(|e| e.to_string()).unwrap_or_default();
        assert!(
            error.contains("mode 0700") && error.contains("mode 0755"),
            "{error}"
        );
        let _ = fs::remove_dir_all(&root);
        Ok(())
    }
}
