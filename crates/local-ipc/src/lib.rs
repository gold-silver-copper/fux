//! Same-user local socket discipline shared by programs that publish a private Unix socket:
//! an owned 0700 directory, a 0600 socket inode that is only ever removed by the process that
//! bound it, kernel-supplied peer credentials, random instance tokens and non-blocking
//! connect initiation. Wait policies, framing and error wording stay with the caller.

use std::fmt;
use std::fs;
use std::io::{self, Read};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

/// Why a directory cannot hold private sockets.
#[derive(Debug)]
pub enum DirectoryError {
    /// The path exists but is not a real directory owned by this user with mode `0?00`.
    Unsafe,
    /// Inspecting or creating the directory failed.
    Io(io::Error),
}

impl fmt::Display for DirectoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsafe => {
                f.write_str("directory must be a private real directory owned by this user")
            }
            Self::Io(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for DirectoryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Unsafe => None,
            Self::Io(error) => Some(error),
        }
    }
}

impl From<io::Error> for DirectoryError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Requires (creating it with mode 0700 when absent) a real directory owned by this user with
/// no group or world access. Symlinks are refused even when they point at a private directory.
pub fn ensure_private_directory(directory: &Path) -> Result<(), DirectoryError> {
    match fs::symlink_metadata(directory) {
        Ok(metadata) => {
            if !metadata.is_dir()
                || metadata.file_type().is_symlink()
                || metadata.permissions().mode() & 0o077 != 0
                || metadata.uid() != nix::unistd::geteuid().as_raw()
            {
                return Err(DirectoryError::Unsafe);
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            use std::os::unix::fs::DirBuilderExt as _;
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(directory)?;
        }
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

/// A listening socket whose inode is removed on drop only while it is still the one bound here.
/// The caller decides whether a pre-existing path may be replaced before binding.
#[derive(Debug)]
pub struct BoundSocket {
    listener: UnixListener,
    path: PathBuf,
    device: u64,
    inode: u64,
}

impl BoundSocket {
    /// Binds `path`, records the new inode and restricts it to mode 0600.
    pub fn bind(path: &Path) -> io::Result<Self> {
        let listener = UnixListener::bind(path)?;
        let metadata = fs::symlink_metadata(path)?;
        let bound = Self {
            listener,
            path: path.to_owned(),
            device: metadata.dev(),
            inode: metadata.ino(),
        };
        fs::set_permissions(&bound.path, fs::Permissions::from_mode(0o600))?;
        Ok(bound)
    }

    pub fn listener(&self) -> &UnixListener {
        &self.listener
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for BoundSocket {
    fn drop(&mut self) {
        if fs::symlink_metadata(&self.path).is_ok_and(|metadata| {
            metadata.file_type().is_socket()
                && metadata.dev() == self.device
                && metadata.ino() == self.inode
        }) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// The effective uid of a connected peer, from kernel-supplied credentials.
pub fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    let uid =
        nix::sys::socket::getsockopt(stream, nix::sys::socket::sockopt::PeerCredentials)?.uid();
    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    ))]
    let uid = nix::unistd::getpeereid(stream)?.0.as_raw();
    #[cfg(not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    )))]
    let uid = {
        let _ = stream;
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "OS peer credentials unavailable",
        ));
    };
    Ok(uid)
}

/// Whether the connected peer runs as this process's effective user.
pub fn peer_is_current_user(stream: &UnixStream) -> io::Result<bool> {
    Ok(peer_uid(stream)? == nix::unistd::geteuid().as_raw())
}

/// 16 random bytes from `/dev/urandom` as 32 lowercase hexadecimal characters.
pub fn random_token() -> io::Result<String> {
    let mut bytes = [0_u8; 16];
    fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// A close-on-exec, non-blocking stream socket with `connect` initiated. When `pending`, the
/// caller waits for writability under its own deadline policy, then calls [`Connecting::confirm`].
#[derive(Debug)]
pub struct Connecting {
    fd: OwnedFd,
    pending: bool,
}

impl Connecting {
    pub fn start(path: &Path) -> io::Result<Self> {
        use nix::fcntl::{FcntlArg, FdFlag, OFlag, fcntl};
        use nix::sys::socket::{AddressFamily, SockFlag, SockType, UnixAddr};
        let fd = nix::sys::socket::socket(
            AddressFamily::Unix,
            SockType::Stream,
            SockFlag::empty(),
            None,
        )?;
        fcntl(&fd, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC))?;
        fcntl(&fd, FcntlArg::F_SETFL(OFlag::O_NONBLOCK))?;
        let pending = match nix::sys::socket::connect(fd.as_raw_fd(), &UnixAddr::new(path)?) {
            Ok(()) => false,
            Err(nix::errno::Errno::EINPROGRESS) => true,
            Err(error) => return Err(error.into()),
        };
        Ok(Self { fd, pending })
    }

    /// True until the kernel completes the connection asynchronously.
    pub fn pending(&self) -> bool {
        self.pending
    }

    /// The pending connection's outcome once the descriptor polled writable.
    pub fn confirm(&self) -> io::Result<()> {
        let error = nix::sys::socket::getsockopt(&self.fd, nix::sys::socket::sockopt::SocketError)?;
        if error != 0 {
            return Err(io::Error::from_raw_os_error(error));
        }
        Ok(())
    }

    /// The connected descriptor as a blocking stream.
    pub fn finish(self) -> io::Result<UnixStream> {
        let stream = UnixStream::from(self.fd);
        stream.set_nonblocking(false)?;
        Ok(stream)
    }
}

impl AsFd for Connecting {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Short enough for a socket path below the platform temporary directory.
    fn scratch(name: &str) -> io::Result<PathBuf> {
        let token = random_token()?;
        let short = token.get(..8).unwrap_or(&token);
        Ok(std::env::temp_dir().join(format!("li-{name}-{}-{short}", std::process::id())))
    }

    #[test]
    fn private_directory_is_created_0700_and_unsafe_modes_are_refused() -> io::Result<()> {
        let root = scratch("dir")?;
        let nested = root.join("a").join("b");
        ensure_private_directory(&nested).map_err(io::Error::other)?;
        assert_eq!(fs::metadata(&nested)?.permissions().mode() & 0o777, 0o700);
        ensure_private_directory(&nested).map_err(io::Error::other)?;
        fs::set_permissions(&nested, fs::Permissions::from_mode(0o750))?;
        assert!(matches!(
            ensure_private_directory(&nested),
            Err(DirectoryError::Unsafe)
        ));
        let file = root.join("file");
        fs::write(&file, b"x")?;
        assert!(matches!(
            ensure_private_directory(&file),
            Err(DirectoryError::Unsafe)
        ));
        fs::set_permissions(&nested, fs::Permissions::from_mode(0o700))?;
        let link = root.join("link");
        std::os::unix::fs::symlink(&nested, &link)?;
        assert!(matches!(
            ensure_private_directory(&link),
            Err(DirectoryError::Unsafe)
        ));
        fs::remove_dir_all(root)
    }

    #[test]
    fn bound_socket_is_0600_and_only_its_own_inode_is_removed_on_drop() -> io::Result<()> {
        let root = scratch("sock")?;
        ensure_private_directory(&root).map_err(io::Error::other)?;
        let path = root.join("s.sock");
        let bound = BoundSocket::bind(&path)?;
        assert_eq!(fs::metadata(&path)?.permissions().mode() & 0o777, 0o600);
        assert_eq!(bound.path(), path);
        drop(bound);
        assert!(!path.exists());
        let first = BoundSocket::bind(&path)?;
        fs::remove_file(&path)?;
        let second = BoundSocket::bind(&path)?;
        drop(first);
        assert!(
            path.exists(),
            "a replacement inode survives the old owner's drop"
        );
        drop(second);
        assert!(!path.exists());
        assert!(BoundSocket::bind(&root.join("missing").join("s.sock")).is_err());
        fs::remove_dir_all(root)
    }

    #[test]
    fn peer_credentials_identify_the_current_user() -> io::Result<()> {
        let (first, second) = UnixStream::pair()?;
        assert_eq!(peer_uid(&first)?, nix::unistd::geteuid().as_raw());
        assert!(peer_is_current_user(&second)?);
        Ok(())
    }

    #[test]
    fn tokens_are_32_hex_characters_and_distinct() -> io::Result<()> {
        let first = random_token()?;
        let second = random_token()?;
        assert_eq!(first.len(), 32);
        assert!(first.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_ne!(first, second);
        Ok(())
    }

    #[test]
    fn connect_initiation_reaches_a_listener_and_reports_missing_paths() -> io::Result<()> {
        let root = scratch("connect")?;
        ensure_private_directory(&root).map_err(io::Error::other)?;
        let path = root.join("s.sock");
        let bound = BoundSocket::bind(&path)?;
        let connecting = Connecting::start(&path)?;
        if connecting.pending() {
            connecting.confirm()?;
        }
        let mut client = connecting.finish()?;
        let (mut server, _) = bound.listener().accept()?;
        client.write_all(b"hi")?;
        let mut received = [0; 2];
        server.read_exact(&mut received)?;
        assert_eq!(&received, b"hi");
        let missing = Connecting::start(&root.join("absent.sock"));
        assert!(matches!(
            missing.map(|_| ()),
            Err(error) if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            )
        ));
        drop(bound);
        fs::remove_dir_all(root)
    }
}
