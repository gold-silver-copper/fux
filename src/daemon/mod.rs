//! Per-user session-server lifecycle: private paths, workspace descriptors, election locks,
//! on-demand background startup and the manager RPC contract.

mod descriptor;
mod migration;
mod paths;
mod rpc;
mod startup;

pub use descriptor::{
    Descriptor, DescriptorError, MAX_DESCRIPTOR_BYTES, ManagerIdentity, read_descriptor,
    recorded_servers, recover_stale_descriptors, remove_descriptor, write_descriptor,
};
pub use migration::{
    MigrationChoice, incompatible_server, migration_dialog, print_alongside_instructions,
    stop_old_server,
};
pub use paths::{DaemonPaths, PathError};
pub use rpc::{
    MANAGER_DEADLINE, ManagerReply, ManagerRequest, manager_request, read_json_frame,
    workspace_names,
};
pub use startup::{
    ManagerLock, STARTUP_TIMEOUT, ServerChild, StartupLock, report_startup, sanitized_environment,
};
