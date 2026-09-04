//! Secure, testable lifecycle for the named-workspace daemon.

mod descriptor;
mod endpoint;
mod manager;
mod paths;
mod spawn;

pub use descriptor::{
    Descriptor, DescriptorError, ManagerIdentity, read_descriptor, recover_stale_descriptors,
    write_descriptor,
};
pub use endpoint::{
    NetworkProfile, ProductionEndpoint, bind_workspace_endpoint,
    bind_workspace_endpoint_with_secret,
};
pub use manager::{
    Daemon, DaemonAction, EndpointFactory, EndpointHandle, ManagerError, ManagerLock, Resolution,
    WorkspaceEndpoint,
};
pub use paths::{DaemonPaths, PathError, validate_workspace_name};
pub use spawn::{
    Clock, DaemonConnector, DaemonSpawner, ProcessDaemonSpawner, ProcessTicket, SpawnError,
    SpawnRequest, SpawnTicket, StartupLock, StdioPolicy, SystemClock, receive_startup_secret,
    receive_startup_secret_async, report_startup, sanitized_environment, start_or_connect,
};

pub const MAX_DESCRIPTOR_BYTES: u64 = 64 * 1024;
pub const STARTUP_TIMEOUT_MS: u64 = 10_000;
pub const STARTUP_RETRY_MS: u64 = 25;
pub const INITIAL_REQUEST_GRACE_MS: u64 = 5_000;
pub const IDLE_WORKSPACE_TTL_MS: u64 = 30 * 60 * 1_000;
pub const MAX_WORKSPACES: usize = 64;
pub const MAX_CONNECTIONS_PER_WORKSPACE: usize = 64;
