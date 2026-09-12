//! Private local sockets: owned directories, mode 0600 inodes, inode-aware cleanup and kernel peer
//! credential checks and deadline-bounded connect and write over `local_ipc`. The
//! operating-system user is the authorization boundary; stale-socket recovery and the control
//! preface are fux's own.

use std::fs;
use std::io;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

use super::control::CONTROL_PREFACE;

const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(2);

pub use local_ipc::BoundSocket;

/// Binds an owned local service socket below a private directory. Startup must be serialized by
/// the caller's lock; stale sockets are replaced only when refused and owner-matching.
pub fn bind_local_socket(path: &Path) -> io::Result<BoundSocket> {
    let path = path.to_owned();
    let directory = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "socket has no parent"))?;
    ensure_private_directory(directory)?;
    remove_stale_socket(&path)?;
    BoundSocket::bind(&path)
}

/// Requires (creating it when absent) a real directory owned by this user with no group or
/// world access.
pub fn ensure_private_directory(directory: &Path) -> io::Result<()> {
    local_ipc::ensure_private_directory(directory).map_err(|error| match error {
        local_ipc::DirectoryError::Unsafe => io::Error::new(
            io::ErrorKind::PermissionDenied,
            "fux runtime directory must be a private real directory owned by this user",
        ),
        local_ipc::DirectoryError::Io(error) => error,
    })
}

fn remove_stale_socket(path: &Path) -> io::Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if !metadata.file_type().is_socket() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "refusing to replace a non-socket path",
        ));
    }
    match UnixStream::connect(path) {
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                "socket is already accepting connections",
            ));
        }
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
            ) => {}
        Err(error) => return Err(error),
    }
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "socket has no parent"))?;
    if metadata.uid() != fs::metadata(parent)?.uid() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "stale socket owner differs from runtime directory owner",
        ));
    }
    let current = fs::symlink_metadata(path)?;
    if !current.file_type().is_socket()
        || current.dev() != metadata.dev()
        || current.ino() != metadata.ino()
    {
        return Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            "socket changed during stale recovery",
        ));
    }
    fs::remove_file(path)
}

/// Authenticates a connected peer through kernel-supplied credentials.
pub fn authorize_peer(stream: &UnixStream) -> io::Result<()> {
    authorize_uid(
        local_ipc::peer_uid(stream)?,
        nix::unistd::geteuid().as_raw(),
    )
}

fn authorize_uid(peer: u32, owner: u32) -> io::Result<()> {
    if peer != owner {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "local peer belongs to another user",
        ));
    }
    Ok(())
}

/// Validates that a client-side socket path lives in a private, owner-matching directory.
pub fn check_private_socket_path(path: &Path) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "socket has no parent"))?;
    let directory = fs::symlink_metadata(parent)?;
    let socket = fs::symlink_metadata(path)?;
    let owner = nix::unistd::geteuid().as_raw();
    if !directory.is_dir()
        || directory.uid() != owner
        || directory.permissions().mode() & 0o077 != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "unsafe local socket directory",
        ));
    }
    if !socket.file_type().is_socket()
        || socket.uid() != owner
        || socket.permissions().mode() & 0o077 != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "unsafe local socket path",
        ));
    }
    Ok(())
}

pub use local_ipc::{connect_until as connect_local, write_all_until};

/// Client half of control negotiation: authorize the peer, send the preface, expect it back
/// (the server half lives with the async socket tasks).
pub fn negotiate_client(stream: &mut UnixStream) -> io::Result<()> {
    negotiate_client_with_timeout(stream, HANDSHAKE_DEADLINE)
}

pub fn negotiate_client_with_timeout(stream: &mut UnixStream, timeout: Duration) -> io::Result<()> {
    let timeout = timeout.min(HANDSHAKE_DEADLINE);
    if timeout.is_zero() {
        return Err(io::ErrorKind::TimedOut.into());
    }
    authorize_peer(stream)?;
    let deadline = Instant::now() + timeout;
    write_all_until(stream, CONTROL_PREFACE, deadline)?;
    let mut received = [0; CONTROL_PREFACE.len()];
    local_ipc::read_exact_until(stream, &mut received, deadline)?;
    if &received != CONTROL_PREFACE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "not a fux control socket; restart the session server if it is older than this fux",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn slow_partial_socket_writes_obey_one_deadline_and_restore_flags() -> io::Result<()> {
        use nix::fcntl::{FcntlArg, OFlag, fcntl};
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        let (mut writer, mut reader) = UnixStream::pair()?;
        nix::sys::socket::setsockopt(&writer, nix::sys::socket::sockopt::SndBuf, &4096)?;
        reader.set_read_timeout(Some(Duration::from_secs(1)))?;
        let finished = Arc::new(AtomicBool::new(false));
        let done = Arc::clone(&finished);
        let (ready, started) = std::sync::mpsc::channel();
        let peer = std::thread::spawn(move || {
            let _ = ready.send(());
            let mut buffer = [0_u8; 4096];
            while !done.load(Ordering::Acquire) {
                match reader.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => std::thread::sleep(Duration::from_millis(5)),
                }
            }
        });
        started
            .recv_timeout(Duration::from_secs(1))
            .map_err(io::Error::other)?;
        let start = Instant::now();
        let result = write_all_until(
            &mut writer,
            &vec![b'x'; 1024 * 1024],
            start + Duration::from_millis(100),
        );
        finished.store(true, Ordering::Release);
        let flags = OFlag::from_bits_truncate(fcntl(&writer, FcntlArg::F_GETFL)?);
        drop(writer);
        peer.join().map_err(|_| io::Error::other("peer panicked"))?;
        assert!(matches!(result, Err(error) if error.kind() == io::ErrorKind::TimedOut));
        assert!(start.elapsed() < Duration::from_secs(1));
        assert!(!flags.contains(OFlag::O_NONBLOCK));
        Ok(())
    }

    #[test]
    fn foreign_uid_is_rejected_and_current_kernel_peer_is_accepted() -> io::Result<()> {
        assert!(authorize_uid(501, 502).is_err());
        assert!(authorize_uid(0, 502).is_err());
        let (first, second) = UnixStream::pair()?;
        authorize_peer(&first)?;
        authorize_peer(&second)?;
        Ok(())
    }

    #[test]
    fn negotiation_requires_the_exact_preface_from_the_server() -> io::Result<()> {
        for (answer, accepted) in [(&b"FUX\n"[..], true), (b"FUZ\n", false)] {
            let (mut client, mut server) = UnixStream::pair()?;
            let handle = std::thread::spawn(move || {
                let mut preface = [0; 4];
                server.read_exact(&mut preface)?;
                server.write_all(answer)?;
                Ok::<_, io::Error>(preface)
            });
            let result = negotiate_client(&mut client);
            assert_eq!(result.is_ok(), accepted, "{answer:?}: {result:?}");
            assert!(
                handle
                    .join()
                    .is_ok_and(|sent| sent.is_ok_and(|preface| &preface == CONTROL_PREFACE))
            );
        }
        Ok(())
    }
}
