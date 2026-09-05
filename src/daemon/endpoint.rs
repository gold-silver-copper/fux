use super::{EndpointHandle, MAX_CONNECTIONS_PER_WORKSPACE, ManagerError};
use koh::identity::Identity;
use koh::server::SessionHost;
use std::collections::BTreeSet;
use std::net::SocketAddrV4;
use std::path::Path;

pub use koh::embed::NetworkProfile;

pub struct ProductionEndpoint(koh::embed::Server);

pub async fn bind_workspace_endpoint<H, F>(
    key_path: &Path,
    allow: &BTreeSet<String>,
    protocol: &'static [u8],
    profile: NetworkProfile,
    make_host: F,
) -> Result<Box<dyn EndpointHandle>, ManagerError>
where
    H: SessionHost,
    F: Fn() -> anyhow::Result<H> + Send + Sync + 'static,
{
    let identity = koh::identity::load(key_path).map_err(manager_error)?;
    bind_workspace_endpoint_with_secret(identity, allow, protocol, profile, make_host).await
}

pub async fn bind_workspace_endpoint_with_secret<H, F>(
    identity: Identity,
    allow: &BTreeSet<String>,
    protocol: &'static [u8],
    profile: NetworkProfile,
    make_host: F,
) -> Result<Box<dyn EndpointHandle>, ManagerError>
where
    H: SessionHost,
    F: Fn() -> anyhow::Result<H> + Send + Sync + 'static,
{
    let server = koh::embed::Server::bind(
        identity,
        allow,
        protocol,
        profile,
        MAX_CONNECTIONS_PER_WORKSPACE,
        make_host,
    )
    .await
    .map_err(manager_error)?;
    Ok(Box::new(ProductionEndpoint(server)))
}

fn manager_error(error: anyhow::Error) -> ManagerError {
    ManagerError::Io(std::io::Error::other(format!("{error:#}")))
}

impl EndpointHandle for ProductionEndpoint {
    fn endpoint_id(&self) -> &str {
        self.0.endpoint_id()
    }
    fn direct_addr(&self) -> SocketAddrV4 {
        self.0.direct_addr()
    }
    fn close(&mut self) {
        self.0.close();
    }
    fn reap_terminal_sessions(&mut self, _now_ms: u64, _ttl_ms: u64) {}
    fn active_tasks(&self) -> usize {
        self.0.active_tasks()
    }
}
