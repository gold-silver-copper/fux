//! Private local sockets: owned directories, mode 0600 inodes, inode-aware cleanup and kernel peer
//! credential checks. The operating-system user is the authorization boundary.

use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::control::CONTROL_PREFACE;

const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(2);

#[derive(Debug)]
pub struct BoundSocket {
    listener: UnixListener,
    path: PathBuf,
    device: u64,
    inode: u64,
}

impl BoundSocket {
    pub fn listener(&self) -> &UnixListener {
        &self.listener
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    /// Accepts and authenticates one peer.
    pub fn accept(&self) -> io::Result<UnixStream> {
        let (stream, _) = self.listener.accept()?;
        authorize_peer(&stream)?;
        Ok(stream)
    }
}

impl Drop for BoundSocket {
    fn drop(&mut self) {
        if let Ok(metadata) = fs::symlink_metadata(&self.path)
            && metadata.file_type().is_socket()
            && metadata.dev() == self.device
            && metadata.ino() == self.inode
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Binds an owned local service socket below a private directory. Startup must be serialized by
/// the caller's lock; stale sockets are replaced only when refused and owner-matching.
pub fn bind_local_socket(path: &Path) -> io::Result<BoundSocket> {
    let path = path.to_owned();
    let directory = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "socket has no parent"))?;
    ensure_private_directory(directory)?;
    remove_stale_socket(&path)?;
    let listener = UnixListener::bind(&path)?;
    let metadata = fs::symlink_metadata(&path)?;
    let bound = BoundSocket {
        listener,
        path,
        device: metadata.dev(),
        inode: metadata.ino(),
    };
    fs::set_permissions(&bound.path, fs::Permissions::from_mode(0o600))?;
    Ok(bound)
}

pub fn ensure_private_directory(directory: &Path) -> io::Result<()> {
    match fs::symlink_metadata(directory) {
        Ok(metadata) => {
            if !metadata.is_dir()
                || metadata.file_type().is_symlink()
                || metadata.permissions().mode() & 0o077 != 0
                || metadata.uid() != nix::unistd::geteuid().as_raw()
            {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "fux runtime directory must be a private real directory owned by this user",
                ));
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            use std::os::unix::fs::DirBuilderExt as _;
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(directory)?;
        }
        Err(error) => return Err(error),
    }
    Ok(())
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
    authorize_uid(uid, nix::unistd::geteuid().as_raw())
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

/// Client half of control negotiation: authorize the peer, send the preface, expect it back.
pub fn negotiate_client(stream: &mut UnixStream) -> io::Result<()> {
    authorize_peer(stream)?;
    negotiate(stream, true)
}

/// Server half of control negotiation: read the preface, answer with ours, compare.
pub fn negotiate_server(stream: &mut UnixStream) -> io::Result<()> {
    negotiate(stream, false)
}

fn negotiate(stream: &mut UnixStream, client: bool) -> io::Result<()> {
    let read_timeout = stream.read_timeout()?;
    let write_timeout = stream.write_timeout()?;
    let result = (|| {
        stream.set_write_timeout(Some(HANDSHAKE_DEADLINE))?;
        if client {
            stream.write_all(CONTROL_PREFACE)?;
        }
        let deadline = Instant::now() + HANDSHAKE_DEADLINE;
        let mut received = [0; 8];
        let mut used = 0;
        while used < received.len() {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::TimedOut, "control negotiation timed out")
                })?;
            stream.set_read_timeout(Some(remaining))?;
            let target = received
                .get_mut(used..)
                .ok_or_else(|| io::Error::other("invalid preface offset"))?;
            let length = stream.read(target)?;
            if length == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "peer closed during version negotiation",
                ));
            }
            used += length;
        }
        if !client {
            stream.write_all(CONTROL_PREFACE)?;
        }
        if &received != CONTROL_PREFACE {
            // `Unsupported` lets callers recognise a version mismatch (as opposed to a broken or
            // foreign peer) and offer the operator a way out.
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "incompatible fux control protocol; expected FUXCTL2; use matching versions or restart the session server after saving your work",
            ));
        }
        Ok(())
    })();
    // Best effort: macOS rejects timeout changes on a socket whose peer already closed, and the
    // negotiation outcome above is what matters.
    let _ = stream.set_read_timeout(read_timeout);
    let _ = stream.set_write_timeout(write_timeout);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn negotiation_requires_the_exact_preface_on_both_sides() -> io::Result<()> {
        let (mut client, mut server) = UnixStream::pair()?;
        let handle = std::thread::spawn(move || negotiate_server(&mut server));
        negotiate_client(&mut client)?;
        assert!(handle.join().is_ok_and(|result| result.is_ok()));

        let (mut client, mut server) = UnixStream::pair()?;
        let handle = std::thread::spawn(move || negotiate_server(&mut server));
        client.write_all(b"FUXCTL1\n")?;
        assert!(handle.join().is_ok_and(|result| result.is_err()));
        Ok(())
    }
}
