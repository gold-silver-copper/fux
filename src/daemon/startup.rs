//! Server election and on-demand startup: the manager socket lock, the client-side startup lock,
//! the background `fux serve --daemon` spawn and its private readiness channel.

use super::DaemonPaths;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);

/// Exclusive ownership of the manager socket for one runtime directory.
#[derive(Debug)]
pub struct ManagerLock {
    listener: UnixListener,
    path: PathBuf,
    device: u64,
    inode: u64,
}

impl ManagerLock {
    /// Stale-socket inspection and replacement are one election transaction under the bind lock,
    /// so two contenders cannot both unlink and rebind.
    pub fn bind(paths: &DaemonPaths) -> io::Result<Self> {
        paths.prepare().map_err(io::Error::other)?;
        let _bind_lock = acquire_lock(&paths.runtime_dir, "manager.bind.lock")?;
        remove_stale_manager_socket(&paths.manager_socket, &paths.runtime_dir)?;
        let listener = UnixListener::bind(&paths.manager_socket)?;
        let metadata = fs::symlink_metadata(&paths.manager_socket)?;
        let bound = Self {
            listener,
            path: paths.manager_socket.clone(),
            device: metadata.dev(),
            inode: metadata.ino(),
        };
        fs::set_permissions(&bound.path, fs::Permissions::from_mode(0o600))?;
        Ok(bound)
    }
    pub fn listener(&self) -> &UnixListener {
        &self.listener
    }
}

impl Drop for ManagerLock {
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

/// Serializes client-side startup so first launches elect exactly one server.
pub struct StartupLock {
    _lock: nix::fcntl::Flock<fs::File>,
}

impl StartupLock {
    pub fn acquire(runtime_dir: &Path) -> io::Result<Self> {
        Ok(Self {
            _lock: acquire_lock(runtime_dir, "startup.lock")?,
        })
    }
}

fn acquire_lock(runtime_dir: &Path, name: &str) -> io::Result<nix::fcntl::Flock<fs::File>> {
    use nix::fcntl::{Flock, FlockArg};
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(runtime_dir.join(name))?;
    let metadata = file.metadata()?;
    let directory = fs::metadata(runtime_dir)?;
    if !metadata.file_type().is_file()
        || metadata.uid() != directory.uid()
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "unsafe startup lock file",
        ));
    }
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut file = file;
    loop {
        match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
            Ok(lock) => return Ok(lock),
            Err((returned, nix::errno::Errno::EWOULDBLOCK)) => file = returned,
            Err((_, error)) => return Err(io::Error::other(error)),
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "another fux startup holds the lock",
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn remove_stale_manager_socket(path: &Path, owner_dir: &Path) -> io::Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(value) => value,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if !metadata.file_type().is_socket() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "manager path is not a socket",
        ));
    }
    // A concurrently elected listener can be visible just before connect succeeds; retry briefly.
    for _ in 0..3 {
        if UnixStream::connect(path).is_ok() {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                "a fux session server is already running",
            ));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    if metadata.uid() != fs::metadata(owner_dir)?.uid() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "stale manager socket owner mismatch",
        ));
    }
    let current = match fs::symlink_metadata(path) {
        Ok(current) => current,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if !current.file_type().is_socket()
        || current.dev() != metadata.dev()
        || current.ino() != metadata.ino()
    {
        return Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            "manager socket changed during stale recovery",
        ));
    }
    fs::remove_file(path)
}

/// Environment handed to a background server: application and credential keys removed.
pub fn sanitized_environment(
    inherited: impl IntoIterator<Item = (OsString, OsString)>,
) -> BTreeMap<OsString, OsString> {
    inherited
        .into_iter()
        .filter(|(key, _)| !crate::os::pty::is_private_env_key(key))
        .collect()
}

/// A background server started by a viewer, with a private same-user readiness channel.
pub struct ServerChild {
    child: Child,
    listener: UnixListener,
    channel_path: PathBuf,
    ready: bool,
    armed: bool,
}

impl ServerChild {
    pub fn spawn(runtime_dir: &Path, executable: &Path, name: &str) -> io::Result<Self> {
        let metadata = fs::symlink_metadata(runtime_dir)?;
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || metadata.permissions().mode() & 0o077 != 0
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "startup channel directory is not private",
            ));
        }
        let nonce = super::descriptor::random_token()?;
        let short = nonce.get(..16).unwrap_or(&nonce);
        let file_name = format!("s-{short}");
        let mut channel_path = runtime_dir.join(&file_name);
        if channel_path.as_os_str().len() >= 96 {
            // Unix socket paths are short; fall back to a private per-user /tmp directory.
            let short_dir = PathBuf::from(format!("/tmp/fux-start-{}", metadata.uid()));
            match fs::create_dir(&short_dir) {
                Ok(()) => fs::set_permissions(&short_dir, fs::Permissions::from_mode(0o700))?,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
            let short = fs::symlink_metadata(&short_dir)?;
            if !short.is_dir()
                || short.file_type().is_symlink()
                || short.uid() != metadata.uid()
                || short.permissions().mode() & 0o077 != 0
            {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "unsafe short startup directory",
                ));
            }
            channel_path = short_dir.join(file_name);
        }
        let listener = UnixListener::bind(&channel_path)?;
        fs::set_permissions(&channel_path, fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        let child = Command::new(executable)
            .args(["serve", "--daemon", "--name", name, "--startup-channel"])
            .arg(&channel_path)
            .env_clear()
            .envs(sanitized_environment(std::env::vars_os()))
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        Ok(Self {
            child,
            listener,
            channel_path,
            ready: false,
            armed: true,
        })
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Polls the readiness channel. `Ok(true)` once the server reported READY; `Err` when it
    /// exited or reported a startup error.
    pub fn poll(&mut self) -> io::Result<bool> {
        if self.ready {
            return Ok(true);
        }
        if let Ok(Some(status)) = self.child.try_wait() {
            self.armed = false;
            return Err(io::Error::other(format!(
                "session server exited before readiness: {status}"
            )));
        }
        let Ok((stream, _)) = self.listener.accept() else {
            return Ok(false);
        };
        if !same_user(&stream, channel_owner(&self.channel_path)) {
            return Ok(false);
        }
        let _ = stream.set_nonblocking(false);
        let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
        let mut bytes = Vec::new();
        if stream.take(4097).read_to_end(&mut bytes).is_err() || bytes.len() > 4096 {
            return Ok(false);
        }
        let text = String::from_utf8_lossy(&bytes);
        if text.trim_end() == "READY" {
            self.ready = true;
            self.armed = false;
            return Ok(true);
        }
        if let Some(message) = text.strip_prefix("ERROR ") {
            self.armed = false;
            return Err(io::Error::other(message.trim_end().to_owned()));
        }
        Ok(false)
    }

    /// A manager reply from this exact child proves readiness even if the READY frame raced.
    pub fn confirm(&mut self, pid: u32) -> bool {
        if self.child.id() != pid {
            return false;
        }
        self.ready = true;
        self.armed = false;
        true
    }
}

impl Drop for ServerChild {
    fn drop(&mut self) {
        if self.armed {
            if let Ok(pid) = i32::try_from(self.child.id()) {
                let _ = nix::sys::signal::killpg(
                    nix::unistd::Pid::from_raw(pid),
                    nix::sys::signal::Signal::SIGKILL,
                );
            }
            let _ = self.child.wait();
        }
        let _ = fs::remove_file(&self.channel_path);
    }
}

/// Server side: report readiness or a startup error over the private channel exactly once.
pub fn report_startup(address: &Path, error: Option<&str>) -> io::Result<()> {
    validate_channel(address)?;
    let mut stream = UnixStream::connect(address)?;
    let message = match error {
        Some(error) => format!(
            "ERROR {}",
            error
                .chars()
                .filter(|character| !character.is_control())
                .take(2048)
                .collect::<String>()
        ),
        None => "READY".to_owned(),
    };
    stream.write_all(message.as_bytes())
}

fn validate_channel(address: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(address)?;
    let parent = address.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "startup channel has no parent")
    })?;
    let parent_metadata = fs::symlink_metadata(parent)?;
    if !metadata.file_type().is_socket()
        || metadata.uid() != parent_metadata.uid()
        || metadata.permissions().mode() & 0o077 != 0
        || !parent_metadata.is_dir()
        || parent_metadata.file_type().is_symlink()
        || parent_metadata.permissions().mode() & 0o077 != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "startup channel is not a private same-user socket",
        ));
    }
    Ok(())
}

fn channel_owner(path: &Path) -> Option<u32> {
    fs::symlink_metadata(path)
        .ok()
        .map(|metadata| metadata.uid())
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn same_user(stream: &UnixStream, expected: Option<u32>) -> bool {
    nix::sys::socket::getsockopt(stream, nix::sys::socket::sockopt::PeerCredentials)
        .ok()
        .zip(expected)
        .is_some_and(|(credentials, uid)| credentials.uid() == uid)
}

#[cfg(target_os = "macos")]
fn same_user(stream: &UnixStream, expected: Option<u32>) -> bool {
    nix::sys::socket::getsockopt(stream, nix::sys::socket::sockopt::LocalPeerCred)
        .ok()
        .zip(expected)
        .is_some_and(|(credentials, uid)| credentials.uid() == uid)
}

#[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
fn same_user(stream: &UnixStream, expected: Option<u32>) -> bool {
    crate::proto::socket::authorize_peer(stream).is_ok() && expected.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn sanitized_environment_drops_private_keys() {
        let environment = sanitized_environment([
            (OsString::from("PATH"), OsString::from("/bin")),
            (OsString::from("FUX_SOCKET"), OsString::from("/x")),
            (OsString::from("KOH_KEY_PASSPHRASE"), OsString::from("s")),
        ]);
        assert_eq!(environment.len(), 1);
        assert!(environment.contains_key(OsStr::new("PATH")));
    }
}
