//! Multiplexer-owned local workspace attachment.
use super::{EndpointHandle, ManagerError};
use crate::host::session::SessionHost;
pub use crate::local::server::LocalEndpoint as ProductionEndpoint;
use std::path::Path;

pub async fn bind_workspace_endpoint<H: SessionHost<State = crate::state::WorkspaceState>>(
    socket: &Path,
    host: H,
) -> Result<Box<dyn EndpointHandle>, ManagerError> {
    ProductionEndpoint::bind(socket, host)
        .map(|endpoint| Box::new(endpoint) as Box<dyn EndpointHandle>)
        .map_err(|error| ManagerError::Io(std::io::Error::other(format!("{error:#}"))))
}
impl EndpointHandle for ProductionEndpoint {
    fn socket_path(&self) -> &Path {
        self.path()
    }
    fn close(&mut self) {
        Self::close(self);
    }
    fn reap_terminal_sessions(&mut self, _now_ms: u64, _ttl_ms: u64) {}
    fn active_tasks(&self) -> usize {
        Self::active_tasks(self)
    }
}
