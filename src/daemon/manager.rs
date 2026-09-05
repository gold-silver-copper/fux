use super::{
    DaemonPaths, Descriptor, DescriptorError, IDLE_WORKSPACE_TTL_MS, INITIAL_REQUEST_GRACE_MS,
    MAX_CONNECTIONS_PER_WORKSPACE, MAX_WORKSPACES, ManagerIdentity, PathError,
    recover_stale_descriptors, write_descriptor,
};
use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::future::Future;
use std::io::{self, Read};
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

pub trait EndpointHandle: Send {
    fn socket_path(&self) -> &Path;
    fn close(&mut self);
    fn reap_terminal_sessions(&mut self, now_ms: u64, ttl_ms: u64);
    fn active_tasks(&self) -> usize {
        0
    }
}

pub trait EndpointFactory {
    fn create(
        &mut self,
        name: &str,
        socket_path: &Path,
    ) -> Result<Box<dyn EndpointHandle>, ManagerError>;
}

pub struct WorkspaceEndpoint {
    pub descriptor: Descriptor,
    endpoint: Box<dyn EndpointHandle>,
    viewers: usize,
}

impl WorkspaceEndpoint {
    pub fn attach(&mut self) -> Result<(), ManagerError> {
        if self.viewers >= MAX_CONNECTIONS_PER_WORKSPACE {
            return Err(ManagerError::ConnectionLimit);
        }
        self.viewers += 1;
        Ok(())
    }
    pub fn detach(&mut self) {
        self.viewers = self.viewers.saturating_sub(1);
    }
    pub const fn viewers(&self) -> usize {
        self.viewers
    }
}

pub struct Daemon {
    paths: DaemonPaths,
    identity: ManagerIdentity,
    workspaces: BTreeMap<String, WorkspaceEndpoint>,
    started_ms: u64,
    received_initial_request: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Resolution {
    Attach(Descriptor),
    Create(String),
    Pick(Vec<String>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DaemonAction {
    Continue,
    ReplyThenExit,
}

impl Daemon {
    pub fn new(paths: DaemonPaths, pid: u32, started_ms: u64) -> Result<Self, ManagerError> {
        paths.prepare()?;
        if pid == 0 {
            return Err(ManagerError::Invalid);
        }
        let identity = ManagerIdentity {
            pid,
            instance_nonce: random_nonce()?,
        };
        recover_stale_descriptors(&paths, &identity)?;
        Ok(Self {
            paths,
            identity,
            workspaces: BTreeMap::new(),
            started_ms,
            received_initial_request: false,
        })
    }

    pub fn identity(&self) -> &ManagerIdentity {
        &self.identity
    }
    pub fn workspace(&self, name: &str) -> Option<&WorkspaceEndpoint> {
        self.workspaces.get(name)
    }
    pub fn workspace_mut(&mut self, name: &str) -> Option<&mut WorkspaceEndpoint> {
        self.workspaces.get_mut(name)
    }
    pub fn names(&self) -> Vec<String> {
        self.workspaces.keys().cloned().collect()
    }

    pub fn create_or_find(
        &mut self,
        name: &str,
        factory: &mut impl EndpointFactory,
    ) -> Result<Descriptor, ManagerError> {
        super::validate_workspace_name(name)?;
        self.received_initial_request = true;
        if let Some(existing) = self.workspaces.get(name) {
            return Ok(existing.descriptor.clone());
        }
        if self.workspaces.len() >= MAX_WORKSPACES {
            return Err(ManagerError::WorkspaceLimit);
        }
        let socket = self.paths.runtime_dir.join(format!("{name}.attach.sock"));
        let endpoint = factory.create(name, &socket)?;
        self.insert_created(name, endpoint)
    }

    pub async fn create_or_find_async<F, Fut>(
        &mut self,
        name: &str,
        factory: F,
    ) -> Result<Descriptor, ManagerError>
    where
        F: FnOnce(PathBuf) -> Fut,
        Fut: Future<Output = Result<Box<dyn EndpointHandle>, ManagerError>>,
    {
        super::validate_workspace_name(name)?;
        self.received_initial_request = true;
        if let Some(existing) = self.workspaces.get(name) {
            return Ok(existing.descriptor.clone());
        }
        if self.workspaces.len() >= MAX_WORKSPACES {
            return Err(ManagerError::WorkspaceLimit);
        }
        let socket = self.paths.runtime_dir.join(format!("{name}.attach.sock"));
        let endpoint = factory(socket).await?;
        self.insert_created(name, endpoint)
    }

    fn insert_created(
        &mut self,
        name: &str,
        endpoint: Box<dyn EndpointHandle>,
    ) -> Result<Descriptor, ManagerError> {
        if self
            .workspaces
            .values()
            .any(|workspace| workspace.endpoint.socket_path() == endpoint.socket_path())
        {
            return Err(ManagerError::DuplicateEndpoint);
        }
        let descriptor = Descriptor {
            name: name.to_owned(),
            pid: self.identity.pid,
            instance_nonce: self.identity.instance_nonce.clone(),
            socket_path: endpoint.socket_path().to_owned(),
            protocol: crate::local::VERSION,
        };
        write_descriptor(&self.paths, &descriptor)?;
        self.workspaces.insert(
            name.to_owned(),
            WorkspaceEndpoint {
                descriptor: descriptor.clone(),
                endpoint,
                viewers: 0,
            },
        );
        Ok(descriptor)
    }

    pub fn resolve(&self, name: Option<&str>) -> Result<Resolution, ManagerError> {
        match name {
            Some(name) => {
                super::validate_workspace_name(name)?;
                Ok(self.workspaces.get(name).map_or_else(
                    || Resolution::Create(name.to_owned()),
                    |workspace| Resolution::Attach(workspace.descriptor.clone()),
                ))
            }
            None => match self.workspaces.len() {
                0 => Ok(Resolution::Create("default".to_owned())),
                1 => self
                    .workspaces
                    .values()
                    .next()
                    .map(|workspace| Resolution::Attach(workspace.descriptor.clone()))
                    .ok_or(ManagerError::Invalid),
                _ => Ok(Resolution::Pick(self.names())),
            },
        }
    }

    pub fn kill(&mut self, name: &str) -> Result<DaemonAction, ManagerError> {
        let mut workspace = self.workspaces.remove(name).ok_or(ManagerError::NotFound)?;
        workspace.endpoint.close();
        let descriptor = self.paths.descriptor(name)?;
        remove_regular_owned_file(&descriptor, &self.paths.descriptors_dir)?;
        Ok(if self.workspaces.is_empty() {
            DaemonAction::ReplyThenExit
        } else {
            DaemonAction::Continue
        })
    }

    pub fn tick(&mut self, now_ms: u64) -> DaemonAction {
        for workspace in self.workspaces.values_mut() {
            workspace
                .endpoint
                .reap_terminal_sessions(now_ms, IDLE_WORKSPACE_TTL_MS);
        }
        if self.workspaces.is_empty()
            && !self.received_initial_request
            && now_ms.saturating_sub(self.started_ms) >= INITIAL_REQUEST_GRACE_MS
        {
            DaemonAction::ReplyThenExit
        } else {
            DaemonAction::Continue
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        for workspace in self.workspaces.values_mut() {
            workspace.endpoint.close();
        }
        for name in self.workspaces.keys() {
            if let Ok(path) = self.paths.descriptor(name) {
                let _ = remove_regular_owned_file(&path, &self.paths.descriptors_dir);
            }
        }
    }
}

#[derive(Debug)]
pub struct ManagerLock {
    listener: UnixListener,
    path: PathBuf,
    device: u64,
    inode: u64,
}
impl ManagerLock {
    /// Exclude manager election for a key reset without connecting to or replacing its socket.
    /// A stale socket is deliberately refused too: normal startup owns stale-state recovery.
    pub fn exclude_for_key_reset(
        paths: &DaemonPaths,
    ) -> Result<nix::fcntl::Flock<fs::File>, ManagerError> {
        let lock = acquire_manager_bind_lock(&paths.runtime_dir)?;
        match fs::symlink_metadata(&paths.manager_socket) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(lock),
            Err(error) => Err(ManagerError::Io(error)),
            Ok(_) => Err(ManagerError::Io(io::Error::new(
                io::ErrorKind::AddrInUse,
                "manager socket exists; stop the manager, or start and stop fux to recover a stale socket",
            ))),
        }
    }

    pub fn bind(paths: &DaemonPaths) -> Result<Self, ManagerError> {
        paths.prepare()?;
        // Stale-socket inspection and replacement must be one election transaction. Without the
        // process-owned startup lock, two contenders can both observe the old socket, then the
        // loser can unlink the winner's newly bound socket and bind a second listener.
        let _bind_lock = acquire_manager_bind_lock(&paths.runtime_dir)?;
        remove_stale_socket(&paths.manager_socket, &paths.runtime_dir)?;
        let listener = UnixListener::bind(&paths.manager_socket).map_err(ManagerError::Io)?;
        let metadata = fs::symlink_metadata(&paths.manager_socket).map_err(ManagerError::Io)?;
        let bound = Self {
            listener,
            path: paths.manager_socket.clone(),
            device: metadata.dev(),
            inode: metadata.ino(),
        };
        fs::set_permissions(&bound.path, fs::Permissions::from_mode(0o600))
            .map_err(ManagerError::Io)?;
        Ok(bound)
    }
    pub fn listener(&self) -> &UnixListener {
        &self.listener
    }
}

fn acquire_manager_bind_lock(
    runtime_dir: &Path,
) -> Result<nix::fcntl::Flock<fs::File>, ManagerError> {
    use nix::fcntl::{Flock, FlockArg};
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(runtime_dir.join("manager.bind.lock"))
        .map_err(ManagerError::Io)?;
    let metadata = file.metadata().map_err(ManagerError::Io)?;
    let directory = fs::metadata(runtime_dir).map_err(ManagerError::Io)?;
    if !metadata.file_type().is_file()
        || metadata.uid() != directory.uid()
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(ManagerError::Invalid);
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    let mut file = file;
    loop {
        match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
            Ok(lock) => return Ok(lock),
            Err((returned, nix::errno::Errno::EWOULDBLOCK)) => file = returned,
            Err((_, error)) => return Err(ManagerError::Io(error.into())),
        }
        if std::time::Instant::now() >= deadline {
            return Err(ManagerError::Invalid);
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
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

#[derive(Debug)]
pub enum ManagerError {
    Path(PathError),
    Descriptor(DescriptorError),
    Io(io::Error),
    Invalid,
    NotFound,
    Unauthorized,
    ConnectionLimit,
    WorkspaceLimit,
    DuplicateEndpoint,
}
impl fmt::Display for ManagerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for ManagerError {}
impl From<PathError> for ManagerError {
    fn from(value: PathError) -> Self {
        Self::Path(value)
    }
}
impl From<DescriptorError> for ManagerError {
    fn from(value: DescriptorError) -> Self {
        Self::Descriptor(value)
    }
}

fn random_nonce() -> Result<String, ManagerError> {
    let mut bytes = [0_u8; 16];
    fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(ManagerError::Io)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn remove_stale_socket(path: &Path, owner_dir: &Path) -> Result<(), ManagerError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(value) => value,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(ManagerError::Io(error)),
    };
    if !metadata.file_type().is_socket() {
        return Err(ManagerError::Io(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "manager path is not a socket",
        )));
    }
    // A concurrently elected listener can be visible just before connect becomes observable on
    // all supported Unix kernels. Retry briefly before classifying it as stale and unlinking it.
    for _ in 0..3 {
        if UnixStream::connect(path).is_ok() {
            return Err(ManagerError::Io(io::Error::new(
                io::ErrorKind::AddrInUse,
                "daemon already running",
            )));
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let owner = fs::metadata(owner_dir).map_err(ManagerError::Io)?;
    if metadata.uid() != owner.uid() {
        return Err(ManagerError::Io(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "stale manager socket owner mismatch",
        )));
    }
    let current = match fs::symlink_metadata(path) {
        Ok(current) => current,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(ManagerError::Io(error)),
    };
    if !current.file_type().is_socket()
        || current.dev() != metadata.dev()
        || current.ino() != metadata.ino()
    {
        return Err(ManagerError::Io(io::Error::new(
            io::ErrorKind::AddrInUse,
            "manager socket changed during stale recovery",
        )));
    }
    fs::remove_file(path).map_err(ManagerError::Io)
}

fn remove_regular_owned_file(path: &Path, owner_dir: &Path) -> Result<(), ManagerError> {
    match fs::symlink_metadata(path) {
        Ok(metadata)
            if metadata.file_type().is_file()
                && !metadata.file_type().is_symlink()
                && metadata.uid() == fs::metadata(owner_dir).map_err(ManagerError::Io)?.uid() =>
        {
            fs::remove_file(path).map_err(ManagerError::Io)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Ok(_) => Err(ManagerError::Io(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "refusing unsafe descriptor cleanup",
        ))),
        Err(error) => Err(ManagerError::Io(error)),
    }
}
