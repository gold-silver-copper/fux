use super::{ManagerIdentity, STARTUP_RETRY_MS, STARTUP_TIMEOUT_MS};
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StdioPolicy {
    Null,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpawnRequest {
    pub executable: PathBuf,
    pub args: Vec<OsString>,
    pub environment: BTreeMap<OsString, OsString>,
    pub stdin: StdioPolicy,
    pub stdout: StdioPolicy,
    pub stderr: StdioPolicy,
    /// The child reports pre-readiness failures over a private channel owned by the spawner.
    pub error_channel: bool,
}

pub trait SpawnTicket {
    fn try_error(&mut self) -> Option<String>;
}

pub trait DaemonSpawner {
    type Ticket: SpawnTicket;
    fn spawn(&mut self, request: SpawnRequest) -> Result<Self::Ticket, SpawnError>;
}

pub trait DaemonConnector {
    fn connect(&mut self) -> Result<Option<ManagerIdentity>, SpawnError>;
}

pub trait Clock {
    fn now_ms(&self) -> u64;
    fn sleep_ms(&mut self, milliseconds: u64);
}

pub struct SystemClock;
impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |value| {
                value.as_millis().min(u128::from(u64::MAX)) as u64
            })
    }
    fn sleep_ms(&mut self, milliseconds: u64) {
        std::thread::sleep(std::time::Duration::from_millis(milliseconds));
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum SpawnError {
    Spawn(String),
    Child(String),
    Timeout,
}

pub struct ProcessDaemonSpawner {
    runtime_dir: PathBuf,
}

/// Serializes client-side daemon startup. The lock is advisory, process-owned, and automatically
/// released on crash; the persistent file avoids unsafe unlink/recreate lock races.
pub struct StartupLock {
    _lock: nix::fcntl::Flock<std::fs::File>,
}

impl StartupLock {
    pub fn acquire(runtime_dir: &std::path::Path) -> Result<Self, SpawnError> {
        use nix::fcntl::{Flock, FlockArg};
        use std::os::unix::fs::OpenOptionsExt as _;
        let path = runtime_dir.join("startup.lock");
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(nix::libc::O_NOFOLLOW)
            .open(&path)
            .map_err(|error| SpawnError::Spawn(error.to_string()))?;
        let metadata = file
            .metadata()
            .map_err(|error| SpawnError::Spawn(error.to_string()))?;
        let directory =
            std::fs::metadata(runtime_dir).map_err(|error| SpawnError::Spawn(error.to_string()))?;
        if !metadata.file_type().is_file()
            || metadata.uid() != directory.uid()
            || metadata.permissions().mode() & 0o077 != 0
        {
            return Err(SpawnError::Spawn("unsafe daemon startup lock".into()));
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        let mut file = file;
        loop {
            match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
                Ok(lock) => return Ok(Self { _lock: lock }),
                Err((returned, nix::errno::Errno::EWOULDBLOCK)) => file = returned,
                Err((_, error)) => return Err(SpawnError::Spawn(error.to_string())),
            }
            if std::time::Instant::now() >= deadline {
                return Err(SpawnError::Timeout);
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}

impl ProcessDaemonSpawner {
    pub fn new(runtime_dir: PathBuf) -> Self {
        Self { runtime_dir }
    }
}

pub struct ProcessTicket {
    child: Child,
    listener: UnixListener,
    channel_path: PathBuf,
    complete: bool,
    armed: bool,
    ready: bool,
}

impl DaemonSpawner for ProcessDaemonSpawner {
    type Ticket = ProcessTicket;

    fn spawn(&mut self, mut request: SpawnRequest) -> Result<Self::Ticket, SpawnError> {
        let metadata = fs::symlink_metadata(&self.runtime_dir)
            .map_err(|error| SpawnError::Spawn(error.to_string()))?;
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || metadata.permissions().mode() & 0o077 != 0
        {
            return Err(SpawnError::Spawn(
                "startup channel directory is not private".to_owned(),
            ));
        }
        let nonce = random_token().map_err(|error| SpawnError::Spawn(error.to_string()))?;
        let short_nonce = nonce.get(..16).unwrap_or(&nonce);
        let file_name = format!("s-{short_nonce}");
        let mut channel_path = self.runtime_dir.join(&file_name);
        if channel_path.as_os_str().len() >= 96 {
            let short_dir = PathBuf::from(format!("/tmp/fux-start-{}", metadata.uid()));
            match fs::create_dir(&short_dir) {
                Ok(()) => fs::set_permissions(&short_dir, fs::Permissions::from_mode(0o700))
                    .map_err(|error| SpawnError::Spawn(error.to_string()))?,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(SpawnError::Spawn(error.to_string())),
            }
            let short = fs::symlink_metadata(&short_dir)
                .map_err(|error| SpawnError::Spawn(error.to_string()))?;
            if !short.is_dir()
                || short.file_type().is_symlink()
                || short.uid() != metadata.uid()
                || short.permissions().mode() & 0o077 != 0
            {
                return Err(SpawnError::Spawn("unsafe short startup directory".into()));
            }
            channel_path = short_dir.join(file_name);
        }
        let listener = UnixListener::bind(&channel_path)
            .map_err(|error| SpawnError::Spawn(error.to_string()))?;
        fs::set_permissions(&channel_path, fs::Permissions::from_mode(0o600))
            .map_err(|error| SpawnError::Spawn(error.to_string()))?;
        let channel = channel_path.as_os_str().to_owned();
        listener
            .set_nonblocking(true)
            .map_err(|error| SpawnError::Spawn(error.to_string()))?;
        request.args.push(OsString::from("--startup-channel"));
        request.args.push(channel);
        let mut command = Command::new(&request.executable);
        command
            .args(&request.args)
            .env_clear()
            .envs(&request.environment)
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let child = command
            .spawn()
            .map_err(|error| SpawnError::Spawn(error.to_string()))?;
        Ok(ProcessTicket {
            child,
            listener,
            channel_path,
            complete: false,
            armed: true,
            ready: false,
        })
    }
}

impl SpawnTicket for ProcessTicket {
    fn try_error(&mut self) -> Option<String> {
        if self.complete {
            return None;
        }
        if let Ok(Some(status)) = self.child.try_wait() {
            self.complete = true;
            self.armed = false;
            return Some(format!("daemon exited before readiness: {status}"));
        }
        let Ok((stream, _)) = self.listener.accept() else {
            return None;
        };
        if !same_user(&stream, channel_owner(&self.channel_path)) {
            return None;
        }
        // The one-shot reporter writes before closing. On macOS, changing flags or timeouts on an
        // already-closed peer can return EINVAL even though its queued frame remains readable.
        let _ = stream.set_nonblocking(false);
        let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(100)));
        let mut bytes = Vec::new();
        if stream.take(4097).read_to_end(&mut bytes).is_err() || bytes.len() > 4096 {
            return None;
        }
        let text = String::from_utf8_lossy(&bytes);
        if text.trim_end() == "READY" {
            self.complete = true;
            self.armed = false;
            self.ready = true;
            None
        } else {
            text.strip_prefix("ERROR ")
                .map_or_else(|| None, |message| Some(message.trim_end().to_owned()))
        }
    }
}

impl ProcessTicket {
    pub fn channel_path(&self) -> &std::path::Path {
        &self.channel_path
    }

    pub fn process_id(&self) -> u32 {
        self.child.id()
    }

    pub fn is_ready(&self) -> bool {
        self.ready
    }

    pub fn is_armed(&self) -> bool {
        self.armed
    }

    /// A successful reply from this exact daemon's manager loop proves startup completed even if
    /// the redundant one-shot READY frame raced with peer close on the local socket.
    pub fn confirm_manager_ready(&mut self, pid: u32) -> bool {
        if self.child.id() != pid {
            return false;
        }
        self.complete = true;
        self.armed = false;
        self.ready = true;
        true
    }

    /// Sends bounded secret material only after the child connects over the same-user channel.
    pub fn send_secret(&mut self, secret: &[u8]) -> Result<(), SpawnError> {
        if secret.is_empty() || secret.len() > 4096 {
            return Err(SpawnError::Spawn("invalid startup secret size".into()));
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            match self.listener.accept() {
                Ok((mut stream, _)) if same_user(&stream, channel_owner(&self.channel_path)) => {
                    if stream
                        .set_nonblocking(false)
                        .and_then(|()| {
                            stream.set_read_timeout(Some(std::time::Duration::from_secs(1)))
                        })
                        .and_then(|()| {
                            stream.set_write_timeout(Some(std::time::Duration::from_secs(1)))
                        })
                        .is_err()
                    {
                        continue;
                    }
                    let mut hello = [0_u8; 6];
                    if stream.read_exact(&mut hello).is_err() {
                        continue;
                    }
                    if &hello != b"SECRET" {
                        continue;
                    }
                    let length = u32::try_from(secret.len())
                        .map_err(|_| SpawnError::Spawn("startup secret too large".into()))?;
                    stream
                        .write_all(&length.to_be_bytes())
                        .and_then(|()| stream.write_all(secret))
                        .map_err(|error| SpawnError::Spawn(error.to_string()))?;
                    return Ok(());
                }
                Ok(_) => continue,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if let Ok(Some(status)) = self.child.try_wait() {
                        self.complete = true;
                        self.armed = false;
                        return Err(SpawnError::Child(format!(
                            "daemon exited before secret transfer: {status}"
                        )));
                    }
                    if std::time::Instant::now() >= deadline {
                        return Err(SpawnError::Timeout);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(error) => return Err(SpawnError::Spawn(error.to_string())),
            }
        }
    }
}

pub fn receive_startup_secret(address: &str) -> std::io::Result<Vec<u8>> {
    validate_channel(address)?;
    let mut stream = UnixStream::connect(address)?;
    stream.write_all(b"SECRET")?;
    let mut length = [0_u8; 4];
    stream.read_exact(&mut length)?;
    let length = usize::try_from(u32::from_be_bytes(length)).map_err(std::io::Error::other)?;
    if length == 0 || length > 4096 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "invalid startup secret size",
        ));
    }
    let mut secret = vec![0_u8; length];
    stream.read_exact(&mut secret)?;
    Ok(secret)
}

/// Cancellation-safe async variant used while the daemon is still starting.
pub async fn receive_startup_secret_async(address: &str) -> std::io::Result<Vec<u8>> {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    validate_channel(address)?;
    let mut stream = tokio::net::UnixStream::connect(address).await?;
    stream.write_all(b"SECRET").await?;
    let mut length = [0_u8; 4];
    stream.read_exact(&mut length).await?;
    let length = usize::try_from(u32::from_be_bytes(length)).map_err(std::io::Error::other)?;
    if length == 0 || length > 4096 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "invalid startup secret size",
        ));
    }
    let mut secret = vec![0_u8; length];
    stream.read_exact(&mut secret).await?;
    Ok(secret)
}

/// Child-side half of the private startup channel. Call once after manager bind succeeds or fails.
pub fn report_startup(address: &str, error: Option<&str>) -> std::io::Result<()> {
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

impl Drop for ProcessTicket {
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

fn validate_channel(address: &str) -> std::io::Result<()> {
    let metadata = fs::symlink_metadata(address)?;
    let parent = PathBuf::from(address)
        .parent()
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "startup channel has no parent",
            )
        })?
        .to_owned();
    let parent_metadata = fs::symlink_metadata(parent)?;
    if !metadata.file_type().is_socket()
        || metadata.uid() != parent_metadata.uid()
        || metadata.permissions().mode() & 0o077 != 0
        || !parent_metadata.is_dir()
        || parent_metadata.file_type().is_symlink()
        || parent_metadata.permissions().mode() & 0o077 != 0
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "startup channel is not a private same-user socket",
        ));
    }
    Ok(())
}

fn channel_owner(path: &PathBuf) -> Option<u32> {
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
impl fmt::Display for SpawnError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for SpawnError {}

/// Connects to the elected manager, or starts one process and waits for bounded readiness.
pub fn start_or_connect<S: DaemonSpawner, C: DaemonConnector>(
    executable: PathBuf,
    inherited_environment: impl IntoIterator<Item = (OsString, OsString)>,
    spawner: &mut S,
    connector: &mut C,
    clock: &mut impl Clock,
) -> Result<ManagerIdentity, SpawnError> {
    if let Some(identity) = connector.connect()? {
        return Ok(identity);
    }
    let request = SpawnRequest {
        executable,
        args: vec![OsString::from("serve"), OsString::from("--daemon")],
        environment: sanitized_environment(inherited_environment),
        stdin: StdioPolicy::Null,
        stdout: StdioPolicy::Null,
        stderr: StdioPolicy::Null,
        error_channel: true,
    };
    let mut ticket = spawner.spawn(request)?;
    let deadline = clock.now_ms().saturating_add(STARTUP_TIMEOUT_MS);
    loop {
        if let Some(identity) = connector.connect()? {
            return Ok(identity);
        }
        if let Some(error) = ticket.try_error() {
            return Err(SpawnError::Child(error));
        }
        if clock.now_ms() >= deadline {
            return Err(SpawnError::Timeout);
        }
        clock.sleep_ms(STARTUP_RETRY_MS);
    }
}

pub fn sanitized_environment(
    inherited: impl IntoIterator<Item = (OsString, OsString)>,
) -> BTreeMap<OsString, OsString> {
    inherited
        .into_iter()
        .filter(|(key, _)| !secret_key(key))
        .collect()
}

fn secret_key(key: &OsStr) -> bool {
    let key = key.to_string_lossy();
    key.starts_with("FUX_") || key.starts_with("KOH_")
}

fn random_token() -> std::io::Result<String> {
    let mut bytes = [0_u8; 16];
    fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}
