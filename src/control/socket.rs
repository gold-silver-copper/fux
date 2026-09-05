use std::fs;
use std::io;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct BoundControlSocket {
    listener: UnixListener,
    path: PathBuf,
    device: u64,
    inode: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeerAuthorization {
    /// Kernel peer credentials and private filesystem paths must both authorize access.
    OperatingSystemCredentials,
}

impl BoundControlSocket {
    pub fn listener(&self) -> &UnixListener {
        &self.listener
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn peer_authorization(&self) -> PeerAuthorization {
        PeerAuthorization::OperatingSystemCredentials
    }
    pub fn accept(&self) -> io::Result<UnixStream> {
        let (stream, _) = self.listener.accept()?;
        authorize_peer(&stream)?;
        Ok(stream)
    }
}

impl Drop for BoundControlSocket {
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

pub fn control_socket_path(runtime_root: &Path, workspace: &str) -> io::Result<PathBuf> {
    if !runtime_root.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "XDG runtime directory must be absolute",
        ));
    }
    if workspace.is_empty()
        || workspace.len() > 64
        || !workspace
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        || workspace == "."
        || workspace == ".."
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unsafe workspace name",
        ));
    }
    Ok(runtime_root.join("fux").join(format!("{workspace}.sock")))
}

pub fn bind_control_socket(runtime_root: &Path, workspace: &str) -> io::Result<BoundControlSocket> {
    bind_local_socket(&control_socket_path(runtime_root, workspace)?)
}

/// Bind an owned local service socket. The caller serializes startup using its server lock.
pub fn bind_local_socket(path: &Path) -> io::Result<BoundControlSocket> {
    let path = path.to_owned();
    let directory = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "socket has no parent"))?;
    ensure_private_directory(directory)?;
    remove_stale_socket(&path)?;
    let listener = UnixListener::bind(&path)?;
    let metadata = fs::symlink_metadata(&path)?;
    let bound = BoundControlSocket {
        listener,
        path,
        device: metadata.dev(),
        inode: metadata.ino(),
    };
    // Establish inode-aware cleanup before setting socket permissions.
    fs::set_permissions(&bound.path, fs::Permissions::from_mode(0o600))?;
    Ok(bound)
}

fn ensure_private_directory(directory: &Path) -> io::Result<()> {
    match fs::symlink_metadata(directory) {
        Ok(metadata) => {
            if !metadata.is_dir()
                || metadata.file_type().is_symlink()
                || metadata.permissions().mode() & 0o077 != 0
                || metadata.uid() != nix::unistd::geteuid().as_raw()
            {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "fux runtime directory must be a private real directory",
                ));
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            use std::os::unix::fs::DirBuilderExt as _;
            fs::DirBuilder::new().mode(0o700).create(directory)?;
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
            "refusing to replace a non-socket control path",
        ));
    }
    match UnixStream::connect(path) {
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                "control socket is already accepting connections",
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
    let parent_metadata = fs::metadata(parent)?;
    if metadata.uid() != parent_metadata.uid() {
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
            "control socket changed during stale recovery",
        ));
    }
    fs::remove_file(path)
}

/// Authenticate a connected socket using credentials supplied by the kernel.
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
}
