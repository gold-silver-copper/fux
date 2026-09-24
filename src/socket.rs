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
#[cfg(target_os = "linux")]
use rustix::fs::Mode;
use rustix::fs::{FlockOperation, OFlags};
use rustix::process::geteuid;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io;
#[cfg(target_os = "linux")]
use std::os::fd::OwnedFd;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

const DEFAULT_NAME: &str = "server.sock";

/// The socket a command uses: `flag` (a `--socket`), else `FUX_SOCKET`, else
/// the default under `XDG_RUNTIME_DIR` or `TMPDIR`.
pub fn socket_path(flag: Option<&str>) -> Result<PathBuf, String> {
    if let Some(flag) = flag {
        return checked(flag, "--socket");
    }
    if let Some(value) = std::env::var_os("FUX_SOCKET") {
        let value = value
            .into_string()
            .map_err(|_| "FUX_SOCKET is not valid UTF-8".to_string())?;
        return checked(&value, "FUX_SOCKET");
    }
    for base in ["XDG_RUNTIME_DIR", "TMPDIR"] {
        if let Some(value) = std::env::var_os(base) {
            let value = value
                .into_string()
                .map_err(|_| format!("{base} is not valid UTF-8"))?;
            return socket_path_from(base, &value);
        }
    }
    Err(
        "no socket location: neither XDG_RUNTIME_DIR nor TMPDIR is set; \
         set FUX_SOCKET to an absolute socket path"
            .into(),
    )
}

/// Longest socket path, in bytes, that `sockaddr_un` holds with its NUL.
pub fn max_path_bytes() -> usize {
    if cfg!(target_os = "linux") { 107 } else { 103 }
}

fn socket_path_from(base: &str, value: &str) -> Result<PathBuf, String> {
    if value.is_empty() {
        return Err(format!(
            "{base} is set but empty; set it to a directory or set FUX_SOCKET"
        ));
    }
    if !Path::new(value).is_absolute() {
        return Err(format!(
            "{base} is {value:?}, not an absolute path; fix it or set FUX_SOCKET"
        ));
    }
    let path = Path::new(value).join("fux").join(DEFAULT_NAME);
    checked(&path.to_string_lossy(), base)
}

fn checked(value: &str, source: &str) -> Result<PathBuf, String> {
    if value.is_empty() {
        return Err(format!(
            "{source} is empty; it must be an absolute socket path"
        ));
    }
    if value.contains('\0') {
        return Err(format!("{source} contains a NUL byte"));
    }
    let path = PathBuf::from(value);
    if !path.is_absolute() {
        return Err(format!(
            "{source} is {value:?}; it must be an absolute path"
        ));
    }
    if path.file_name().is_none() || path.parent().is_none() {
        return Err(format!("{source} is {value:?}; it must name a socket file"));
    }
    let limit = max_path_bytes();
    if value.len() > limit {
        return Err(format!(
            "the socket path is {} bytes, longer than the {limit}-byte limit for a Unix \
             domain socket on this platform (from {source}): {value}",
            value.len()
        ));
    }
    Ok(path)
}

fn octal(mode: u32) -> String {
    format!("{:04o}", mode & 0o7777)
}

/// The directory holding the socket must be ours, mode 0700, reached only
/// through directories no other user can rewrite. `create` makes it when it
/// is missing; nothing that already exists is modified.
fn private_directory(directory: &Path, create: bool) -> Result<(), String> {
    let euid = geteuid().as_raw();
    let shown = directory.display();
    let above = directory
        .parent()
        .ok_or_else(|| format!("{shown} has no parent directory"))?;
    let canonical = above
        .canonicalize()
        .map_err(|error| format!("socket directory parent {}: {error}", above.display()))?;
    for ancestor in canonical.ancestors() {
        let meta =
            fs::metadata(ancestor).map_err(|error| format!("{}: {error}", ancestor.display()))?;
        let mode = meta.permissions().mode();
        let owned = meta.uid() == 0 || meta.uid() == euid;
        let shared = mode & 0o022 != 0;
        if !owned || (shared && mode & 0o1000 == 0) {
            return Err(format!(
                "{} (mode {}, owner uid {}) could be changed by another user, so the socket \
                 beneath it would not be private; choose another location with FUX_SOCKET",
                ancestor.display(),
                octal(mode),
                meta.uid()
            ));
        }
    }
    match fs::symlink_metadata(directory) {
        Err(error) if error.kind() == io::ErrorKind::NotFound && create => {
            match DirBuilder::new().mode(0o700).create(directory) {
                // Set explicitly: the umask must not decide who reaches the socket.
                Ok(()) => fs::set_permissions(directory, fs::Permissions::from_mode(0o700))
                    .map_err(|error| format!("{shown}: {error}"))?,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(format!("creating {shown}: {error}")),
            }
        }
        Err(error) => return Err(format!("{shown}: {error}")),
        Ok(_) => {}
    }
    let meta = fs::symlink_metadata(directory).map_err(|error| format!("{shown}: {error}"))?;
    let mode = meta.permissions().mode();
    if meta.file_type().is_symlink() {
        return Err(format!(
            "{shown} is a symbolic link; the socket directory must be a real directory"
        ));
    }
    if !meta.is_dir() || meta.uid() != euid || mode & 0o077 != 0 {
        return Err(format!(
            "{shown} must be a directory owned by you (uid {euid}) with mode 0700; it is {} \
             with mode {} owned by uid {}. fux does not change it",
            if meta.is_dir() {
                "a directory"
            } else {
                "not a directory"
            },
            octal(mode),
            meta.uid()
        ));
    }
    Ok(())
}

/// Makes the socket's private directory if it is missing, checking it as a
/// server does; for a client about to start one.
pub fn prepare_directory(directory: &Path) -> Result<(), String> {
    private_directory(directory, true)
}

/// Whether a socket file exists at `path` at all.
pub fn exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

/// A client refuses a socket that is not in a private directory of its own
/// user, or not a socket of its own user.
pub fn check_client_socket(path: &Path) -> Result<(), String> {
    let meta = match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(format!("no fux server is running at {}", path.display()));
        }
        Err(error) => return Err(format!("{}: {error}", path.display())),
        Ok(meta) => meta,
    };
    let directory = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    private_directory(directory, false)?;
    if !meta.file_type().is_socket() || meta.uid() != geteuid().as_raw() {
        return Err(format!(
            "{} is not a socket owned by you; refusing to connect",
            path.display()
        ));
    }
    Ok(())
}

fn identity(meta: &fs::Metadata) -> (u64, u64) {
    (meta.dev(), meta.ino())
}

/// A socket file's identity, held so that it keeps meaning that file.
struct Pinned {
    identity: (u64, u64),
    #[cfg(target_os = "linux")]
    _inode: OwnedFd,
}

impl Pinned {
    #[cfg(target_os = "linux")]
    fn new(path: &Path) -> io::Result<Self> {
        let inode = rustix::fs::open(
            path,
            OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        let stat = rustix::fs::fstat(&inode)?;
        Ok(Self {
            identity: (stat.st_dev, stat.st_ino),
            _inode: inode,
        })
    }

    #[cfg(not(target_os = "linux"))]
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

impl Endpoint {
    pub fn path(&self) -> &Path {
        &self.path
    }
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
    let fd = rustix::net::socket(
        rustix::net::AddressFamily::UNIX,
        rustix::net::SocketType::STREAM,
        None,
    )?;
    rustix::io::fcntl_setfd(&fd, rustix::io::FdFlags::CLOEXEC)?;
    rustix::io::ioctl_fionbio(&fd, true)?;
    let address = rustix::net::SocketAddrUnix::new(path)?;
    match rustix::net::connect(&fd, &address) {
        Ok(()) | Err(rustix::io::Errno::AGAIN | rustix::io::Errno::INPROGRESS) => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// Takes ownership of `path` and binds it. Refuses a live server's socket and
/// anything that is not a socket; replaces only a socket proven stale.
pub fn bind_socket(path: &Path) -> Result<(Endpoint, UnixListener), String> {
    let shown = path.display().to_string();
    let directory = path
        .parent()
        .ok_or_else(|| format!("{shown} has no parent directory"))?;
    private_directory(directory, true)?;
    let name = path
        .file_name()
        .ok_or_else(|| format!("{shown} names no file"))?;
    let euid = geteuid().as_raw();
    let foreign = |meta: &fs::Metadata| !meta.file_type().is_socket() || meta.uid() != euid;
    let refused =
        || format!("{shown} exists and is not a socket owned by you; fux will not replace it");
    if fs::symlink_metadata(path).is_ok_and(|meta| foreign(&meta)) {
        return Err(refused());
    }
    let lock_path = directory.join(format!("{}.lock", name.to_string_lossy()));
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags((OFlags::NOFOLLOW | OFlags::CLOEXEC).bits() as i32)
        .open(&lock_path)
        .map_err(|error| format!("{}: {error}", lock_path.display()))?;
    rustix::fs::flock(&lock, FlockOperation::NonBlockingLockExclusive).map_err(|errno| {
        if errno == rustix::io::Errno::WOULDBLOCK {
            format!("another fux server is already using {shown}")
        } else {
            format!("locking {}: {errno}", lock_path.display())
        }
    })?;
    // Pinned before the probe, so the file found dead is the one removed.
    let stale = Pinned::new(path);
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("{shown}: {error}")),
        Ok(meta) if foreign(&meta) => return Err(refused()),
        Ok(_) => match probe(path) {
            Ok(()) => {
                return Err(format!(
                    "another fux server is already listening on {shown}"
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => {
                if stale.as_ref().is_ok_and(|stale| stale.still_at(path)) {
                    fs::remove_file(path)
                        .map_err(|error| format!("removing stale socket {shown}: {error}"))?;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "cannot tell whether {shown} is in use ({error}); it was left in place"
                ));
            }
        },
    }
    let fd = rustix::net::socket(
        rustix::net::AddressFamily::UNIX,
        rustix::net::SocketType::STREAM,
        None,
    )
    .map_err(|error| format!("socket: {error}"))?;
    rustix::io::fcntl_setfd(&fd, rustix::io::FdFlags::CLOEXEC)
        .map_err(|error| format!("socket: {error}"))?;
    let address =
        rustix::net::SocketAddrUnix::new(path).map_err(|error| format!("{shown}: {error}"))?;
    rustix::net::bind(&fd, &address).map_err(|error| format!("binding {shown}: {error}"))?;
    let endpoint = Endpoint {
        path: path.to_owned(),
        socket: Pinned::new(path).map_err(|error| format!("{shown}: {error}"))?,
        _lock: lock,
    };
    // From here a failure drops `endpoint`, which removes the socket. The mode
    // is set before `listen`, so no connection is accepted on an open socket.
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("{shown}: {error}"))?;
    rustix::net::listen(&fd, 128).map_err(|error| format!("listening on {shown}: {error}"))?;
    Ok((endpoint, UnixListener::from(fd)))
}

/// The effective user ID of the process at the other end of `stream`.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    rustix::net::sockopt::socket_peercred(stream)
        .map(|credentials| credentials.uid.as_raw())
        .map_err(io::Error::from)
}

/// The effective user ID of the process at the other end of `stream`.
#[cfg(target_os = "macos")]
pub fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    use std::os::fd::AsRawFd;
    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    // SAFETY: the descriptor is a live socket for the call's duration and both
    // out-pointers point at initialised locals.
    let result = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) };
    if result == 0 {
        Ok(uid)
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let long = format!("/{}", "a".repeat(max_path_bytes()));
        assert!(
            checked(&long, "X")
                .err()
                .unwrap_or_default()
                .contains("limit")
        );
        assert!(checked("/tmp/fux/s.sock", "X").is_ok());
        assert!(socket_path_from("TMPDIR", "").is_err());
        assert!(socket_path_from("TMPDIR", "rel").is_err());
        assert_eq!(
            socket_path_from("TMPDIR", "/t").ok(),
            Some(PathBuf::from("/t/fux/server.sock"))
        );
    }

    #[test]
    fn bind_is_private_single_owner_and_cleans_up_only_its_own_socket() -> Result<(), String> {
        let root = scratch("bind");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).map_err(|e| e.to_string())?;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
        let path = root.join("fux").join("s.sock");
        let (endpoint, listener) = bind_socket(&path)?;
        let mode = |p: &Path| fs::metadata(p).map(|m| m.permissions().mode() & 0o777).ok();
        assert_eq!(mode(&path), Some(0o600));
        assert_eq!(mode(&root.join("fux")), Some(0o700));
        // A second server on the same path is refused while the first lives.
        assert!(
            bind_socket(&path)
                .err()
                .unwrap_or_default()
                .contains("already")
        );
        // A client connects, and its peer is this user.
        let client = UnixStream::connect(&path).map_err(|e| e.to_string())?;
        let (served, _) = listener.accept().map_err(|e| e.to_string())?;
        assert_eq!(peer_uid(&served).ok(), Some(geteuid().as_raw()));
        drop((client, served, listener, endpoint));
        assert!(!exists(&path), "the socket is removed at exit");
        // A stale socket (nothing listening) is replaced.
        let stale = UnixListener::bind(&path).map_err(|e| e.to_string())?;
        drop(stale);
        let (endpoint, _listener) = bind_socket(&path)?;
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

    #[test]
    fn a_shared_directory_is_refused() -> Result<(), String> {
        let root = scratch("shared");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("fux")).map_err(|e| e.to_string())?;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
        fs::set_permissions(root.join("fux"), fs::Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;
        let error = bind_socket(&root.join("fux").join("s.sock"))
            .err()
            .unwrap_or_default();
        assert!(error.contains("mode 0700"), "{error}");
        let _ = fs::remove_dir_all(&root);
        Ok(())
    }
}
