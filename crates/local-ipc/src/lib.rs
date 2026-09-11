//! Same-user local socket discipline shared by programs that publish a private Unix socket:
//! an owned 0700 directory, a 0600 socket inode that is only ever removed by the process that
//! bound it, kernel-supplied peer credentials, random instance tokens and non-blocking
//! connect initiation, plus the deadline discipline built on it: a bounded connect, a bounded
//! full write, a newline-delimited frame reader and per-application runtime-directory
//! discovery. Limits, prefaces and error wording stay with the caller.

use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Instant;

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

/// Time left before `deadline`, or `TimedOut` once it has passed.
fn remaining(deadline: Instant) -> io::Result<std::time::Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|duration| !duration.is_zero())
        .ok_or_else(|| io::Error::from(io::ErrorKind::TimedOut))
}

/// Waits for `flags` on `fd` for at most the time left before `deadline` (capped at two seconds
/// per call so callers observe the deadline promptly). `Ok(false)` means nothing happened yet;
/// `EINTR` is retried by the caller's loop.
fn poll_until(
    fd: BorrowedFd<'_>,
    flags: nix::poll::PollFlags,
    deadline: Instant,
) -> io::Result<bool> {
    let timeout =
        u16::try_from(remaining(deadline)?.as_millis().clamp(1, 2000)).map_err(io::Error::other)?;
    let mut polls = [nix::poll::PollFd::new(fd, flags)];
    match nix::poll::poll(&mut polls, timeout) {
        Ok(0) | Err(nix::errno::Errno::EINTR) => Ok(false),
        Ok(_) => Ok(true),
        Err(error) => Err(error.into()),
    }
}

/// Connects to `path` without letting a saturated listener outlive `deadline`: initiates the
/// connection non-blocking and polls for writability until it completes or the deadline passes
/// (`TimedOut`), retrying interrupted polls. The returned stream is blocking.
pub fn connect_until(path: &Path, deadline: Instant) -> io::Result<UnixStream> {
    remaining(deadline)?;
    let connecting = Connecting::start(path)?;
    if connecting.pending() {
        while !poll_until(connecting.as_fd(), nix::poll::PollFlags::POLLOUT, deadline)? {}
        connecting.confirm()?;
    }
    remaining(deadline)?;
    connecting.finish()
}

/// Writes all of `bytes` under one wall-clock deadline, even when the peer drains only a few
/// bytes at a time. The descriptor's status flags are restored before returning.
pub fn write_all_until(
    stream: &mut UnixStream,
    mut bytes: &[u8],
    deadline: Instant,
) -> io::Result<()> {
    use nix::fcntl::{FcntlArg, OFlag, fcntl};
    let flags = OFlag::from_bits_truncate(fcntl(&*stream, FcntlArg::F_GETFL)?);
    fcntl(&*stream, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK))?;
    let result = (|| {
        while !bytes.is_empty() {
            remaining(deadline)?;
            match stream.write(bytes) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(count) => {
                    bytes = bytes
                        .get(count..)
                        .ok_or_else(|| io::Error::other("invalid socket write count"))?
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    poll_until(stream.as_fd(), nix::poll::PollFlags::POLLOUT, deadline)?;
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    })();
    let restored = fcntl(&*stream, FcntlArg::F_SETFL(flags))
        .map(|_| ())
        .map_err(io::Error::from);
    result.and(restored)
}

/// Why a newline-delimited frame did not arrive.
#[derive(Debug)]
pub enum FrameError {
    /// The deadline passed before a newline arrived.
    TimedOut,
    /// The peer closed before a newline arrived.
    Closed,
    /// More than the reader's maximum frame size arrived without a newline.
    Oversize,
    /// Polling or reading the socket failed.
    Io(io::Error),
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TimedOut => f.write_str("frame deadline passed"),
            Self::Closed => f.write_str("peer closed before a complete frame"),
            Self::Oversize => f.write_str("frame exceeds the size limit"),
            Self::Io(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for FrameError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::TimedOut | Self::Closed | Self::Oversize => None,
        }
    }
}

impl From<io::Error> for FrameError {
    fn from(error: io::Error) -> Self {
        if error.kind() == io::ErrorKind::TimedOut {
            Self::TimedOut
        } else {
            Self::Io(error)
        }
    }
}

/// Reads newline-delimited frames of at most `max_frame` payload bytes (the newline excluded),
/// keeping partial data across calls. Works on blocking and non-blocking streams alike: every
/// read is preceded by a poll bounded by the call's deadline. Decoding stays with the caller.
#[derive(Debug)]
pub struct FrameReader {
    buffer: Vec<u8>,
    max_frame: usize,
    read_size: usize,
}

impl FrameReader {
    /// A reader that pulls up to 8 KiB per read; bytes after a newline stay buffered for the
    /// next call, so one reader must serve the stream for its lifetime.
    pub fn new(max_frame: usize) -> Self {
        Self {
            buffer: Vec::new(),
            max_frame,
            read_size: 8192,
        }
    }

    /// A reader that pulls one byte per read, so nothing past the newline leaves the socket and
    /// the reader may be dropped between frames.
    pub fn bytewise(max_frame: usize) -> Self {
        Self {
            read_size: 1,
            ..Self::new(max_frame)
        }
    }

    /// The next frame's payload, without its newline, before `deadline`.
    pub fn next_frame(
        &mut self,
        stream: &mut UnixStream,
        deadline: Instant,
    ) -> Result<Vec<u8>, FrameError> {
        loop {
            if let Some(newline) = self.buffer.iter().position(|byte| *byte == b'\n') {
                // The payload is judged by its own length, whatever chunk size delivered it.
                if newline > self.max_frame {
                    return Err(FrameError::Oversize);
                }
                let mut frame = self.buffer.split_off(newline);
                std::mem::swap(&mut frame, &mut self.buffer);
                self.buffer.drain(..1);
                return Ok(frame);
            }
            if self.buffer.len() > self.max_frame {
                return Err(FrameError::Oversize);
            }
            if !poll_until(stream.as_fd(), nix::poll::PollFlags::POLLIN, deadline)? {
                continue;
            }
            let start = self.buffer.len();
            self.buffer.resize(start + self.read_size, 0);
            let target = self
                .buffer
                .get_mut(start..)
                .ok_or_else(|| io::Error::other("invalid frame buffer offset"))?;
            let outcome = stream.read(target);
            self.buffer
                .truncate(start + outcome.as_ref().copied().unwrap_or(0));
            match outcome {
                Ok(0) => return Err(FrameError::Closed),
                Ok(_) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
}

/// The private runtime directory for the application `name`: `$XDG_RUNTIME_DIR/<name>` when
/// that variable holds an absolute path; on macOS otherwise
/// `$HOME/Library/Caches/<name>-runtime/<name>`. `None` when neither applies.
pub fn runtime_directory(name: &str) -> Option<PathBuf> {
    runtime_directory_from(
        name,
        std::env::var_os("XDG_RUNTIME_DIR"),
        std::env::var_os("HOME"),
    )
}

/// [`runtime_directory`] over explicit `XDG_RUNTIME_DIR` and `HOME` values.
pub fn runtime_directory_from(
    name: &str,
    runtime: Option<OsString>,
    home: Option<OsString>,
) -> Option<PathBuf> {
    let absolute = |value: Option<OsString>| {
        value
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    };
    if let Some(root) = absolute(runtime) {
        return Some(root.join(name));
    }
    macos_fallback(name, absolute(home))
}

#[cfg(target_os = "macos")]
fn macos_fallback(name: &str, home: Option<PathBuf>) -> Option<PathBuf> {
    home.map(|home| {
        home.join("Library/Caches")
            .join(format!("{name}-runtime"))
            .join(name)
    })
}

#[cfg(not(target_os = "macos"))]
fn macos_fallback(_: &str, _: Option<PathBuf>) -> Option<PathBuf> {
    None
}

#[cfg(test)]
mod tests {

    use super::*;
    use std::time::Duration;

    #[test]
    fn chunked_reader_rejects_an_oversize_payload_that_arrives_with_its_newline()
    -> Result<(), Box<dyn std::error::Error>> {
        let (mut client, mut server) = UnixStream::pair()?;
        let mut reader = FrameReader::new(16);
        let mut payload = vec![b'x'; 17];
        payload.push(b'\n');
        client.write_all(&payload)?;
        let deadline = Instant::now() + Duration::from_secs(2);
        assert!(matches!(
            reader.next_frame(&mut server, deadline),
            Err(FrameError::Oversize)
        ));
        let mut exact = vec![b'y'; 16];
        exact.push(b'\n');
        let mut reader = FrameReader::new(16);
        client.write_all(&exact)?;
        assert_eq!(reader.next_frame(&mut server, deadline)?, vec![b'y'; 16]);
        Ok(())
    }

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

    #[test]
    fn frames_survive_partial_reads_and_reject_oversize_and_deadlines() -> io::Result<()> {
        let (mut client, mut server) = UnixStream::pair()?;
        let mut reader = FrameReader::new(8);
        let far = || Instant::now() + Duration::from_secs(2);
        server.write_all(b"ab")?;
        std::thread::sleep(Duration::from_millis(10));
        server.write_all(b"c\nde\nf")?;
        let frame = reader
            .next_frame(&mut client, far())
            .map_err(io::Error::other)?;
        assert_eq!(frame, b"abc");
        let frame = reader
            .next_frame(&mut client, far())
            .map_err(io::Error::other)?;
        assert_eq!(frame, b"de");
        let started = Instant::now();
        assert!(matches!(
            reader.next_frame(&mut client, Instant::now() + Duration::from_millis(50)),
            Err(FrameError::TimedOut)
        ));
        assert!(started.elapsed() < Duration::from_secs(1));
        server.write_all(b"gh\n")?;
        let frame = reader
            .next_frame(&mut client, far())
            .map_err(io::Error::other)?;
        assert_eq!(frame, b"fgh", "partial data is retained across a deadline");
        server.write_all(b"123456789")?;
        assert!(matches!(
            reader.next_frame(&mut client, far()),
            Err(FrameError::Oversize)
        ));
        let (mut client, mut server) = UnixStream::pair()?;
        server.write_all(b"12345678\n")?;
        let frame = FrameReader::new(8)
            .next_frame(&mut client, far())
            .map_err(io::Error::other)?;
        assert_eq!(frame.len(), 8, "a frame of exactly the limit is accepted");
        server.write_all(b"partial")?;
        drop(server);
        assert!(matches!(
            FrameReader::new(8).next_frame(&mut client, far()),
            Err(FrameError::Closed)
        ));
        Ok(())
    }

    #[test]
    fn bytewise_reader_leaves_the_next_frame_on_the_socket() -> io::Result<()> {
        let (mut client, mut server) = UnixStream::pair()?;
        server.write_all(b"one\ntwo\n")?;
        let far = Instant::now() + Duration::from_secs(2);
        let first = FrameReader::bytewise(16)
            .next_frame(&mut client, far)
            .map_err(io::Error::other)?;
        let second = FrameReader::bytewise(16)
            .next_frame(&mut client, far)
            .map_err(io::Error::other)?;
        assert_eq!(
            (first.as_slice(), second.as_slice()),
            (&b"one"[..], &b"two"[..])
        );
        Ok(())
    }

    #[test]
    fn connect_until_honors_the_deadline_against_a_saturated_listener() -> io::Result<()> {
        let root = scratch("deadline")?;
        ensure_private_directory(&root).map_err(io::Error::other)?;
        let path = root.join("s.sock");
        let bound = BoundSocket::bind(&path)?;
        let mut client = connect_until(&path, Instant::now() + Duration::from_secs(2))?;
        let (mut server, _) = bound.listener().accept()?;
        client.write_all(b"hi")?;
        let mut received = [0; 2];
        server.read_exact(&mut received)?;
        assert!(matches!(
            connect_until(&path, Instant::now()),
            Err(error) if error.kind() == io::ErrorKind::TimedOut
        ));
        assert!(matches!(
            connect_until(&root.join("absent.sock"), Instant::now() + Duration::from_secs(1)),
            Err(error) if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            )
        ));
        drop(bound);
        fs::remove_dir_all(root)
    }

    #[test]
    fn slow_partial_writes_obey_one_deadline_and_restore_flags() -> io::Result<()> {
        use nix::fcntl::{FcntlArg, OFlag, fcntl};
        let (mut writer, mut reader) = UnixStream::pair()?;
        nix::sys::socket::setsockopt(&writer, nix::sys::socket::sockopt::SndBuf, &4096)?;
        reader.set_read_timeout(Some(Duration::from_secs(1)))?;
        let peer = std::thread::spawn(move || {
            let mut buffer = [0_u8; 4096];
            while let Ok(count) = reader.read(&mut buffer) {
                if count == 0 {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        let start = Instant::now();
        let result = write_all_until(
            &mut writer,
            &vec![b'x'; 1024 * 1024],
            start + Duration::from_millis(100),
        );
        let flags = OFlag::from_bits_truncate(fcntl(&writer, FcntlArg::F_GETFL)?);
        drop(writer);
        peer.join().map_err(|_| io::Error::other("peer panicked"))?;
        assert!(matches!(result, Err(error) if error.kind() == io::ErrorKind::TimedOut));
        assert!(start.elapsed() < Duration::from_secs(1));
        assert!(!flags.contains(OFlag::O_NONBLOCK));
        Ok(())
    }

    #[test]
    fn runtime_directory_prefers_xdg_and_falls_back_only_on_macos() {
        assert_eq!(
            runtime_directory_from("app", Some("/run/user/1".into()), Some("/home/u".into())),
            Some(PathBuf::from("/run/user/1/app"))
        );
        let fallback = runtime_directory_from("app", Some("".into()), Some("/Users/u".into()));
        if cfg!(target_os = "macos") {
            assert_eq!(
                fallback,
                Some(PathBuf::from("/Users/u/Library/Caches/app-runtime/app"))
            );
        } else {
            assert_eq!(fallback, None);
        }
        assert_eq!(
            runtime_directory_from("app", Some("relative".into()), Some("relative".into())),
            None
        );
    }
}
