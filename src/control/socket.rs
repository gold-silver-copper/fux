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
    /// Stable `std` does not expose peer credentials on all supported Unix platforms. A socket
    /// inside the same-user 0700 runtime directory, with mode 0600, is the portable boundary.
    FilesystemPermissions,
}

impl BoundControlSocket {
    pub fn listener(&self) -> &UnixListener {
        &self.listener
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn peer_authorization(&self) -> PeerAuthorization {
        PeerAuthorization::FilesystemPermissions
    }
    pub fn accept(&self) -> io::Result<UnixStream> {
        self.listener.accept().map(|(stream, _)| stream)
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
    let path = control_socket_path(runtime_root, workspace)?;
    let directory = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "socket has no parent"))?;
    ensure_private_directory(directory)?;
    remove_stale_socket(&path)?;
    let listener = UnixListener::bind(&path)?;
    if let Err(error) = fs::set_permissions(&path, fs::Permissions::from_mode(0o600)) {
        let _ = fs::remove_file(&path);
        return Err(error);
    }
    let metadata = fs::symlink_metadata(&path)?;
    Ok(BoundControlSocket {
        listener,
        path,
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

fn ensure_private_directory(directory: &Path) -> io::Result<()> {
    match fs::symlink_metadata(directory) {
        Ok(metadata) => {
            if !metadata.is_dir()
                || metadata.file_type().is_symlink()
                || metadata.permissions().mode() & 0o077 != 0
            {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "fux runtime directory must be a private real directory",
                ));
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir(directory)?;
            fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
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
    if UnixStream::connect(path).is_ok() {
        return Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            "control socket is already accepting connections",
        ));
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
