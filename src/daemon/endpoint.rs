use super::{EndpointHandle, MAX_CONNECTIONS_PER_WORKSPACE, ManagerError};
use koh::server::{Hosts, SessionHost, SharedHost};
use koh::transport_iroh::{
    bind_endpoint_alpns, bind_endpoint_local_alpns, bind_endpoint_with_relay_alpns,
    format_endpoint_id, load_or_create_secret_key, parse_endpoint_id, parse_relay_url,
};
use std::collections::{BTreeSet, HashSet};
use std::net::{Ipv4Addr, SocketAddrV4};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const ENDPOINT_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NetworkProfile {
    Default,
    Local,
    Relay(String),
}

pub struct ProductionEndpoint {
    _endpoint: iroh::Endpoint,
    endpoint_id: String,
    direct_addr: SocketAddrV4,
    shutdown: CancellationToken,
    accept_task: Option<std::thread::JoinHandle<()>>,
    active_tasks: Arc<AtomicUsize>,
}

struct ActiveTask(Arc<AtomicUsize>);

fn build_worker_runtime(
    build: impl FnOnce() -> std::io::Result<tokio::runtime::Runtime>,
) -> Result<tokio::runtime::Runtime, ManagerError> {
    build().map_err(ManagerError::Io)
}

impl ActiveTask {
    fn new(count: Arc<AtomicUsize>) -> Self {
        count.fetch_add(1, Ordering::AcqRel);
        Self(count)
    }
}

impl Drop for ActiveTask {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Binds and starts one endpoint serving exactly one shared workspace over the supplied ALPN.
pub async fn bind_workspace_endpoint<H, F>(
    key_path: &Path,
    allow: &BTreeSet<String>,
    alpn: &'static [u8],
    profile: NetworkProfile,
    make_host: F,
) -> Result<Box<dyn EndpointHandle>, ManagerError>
where
    H: SessionHost,
    F: Fn() -> anyhow::Result<H> + Send + Sync + 'static,
{
    let secret = load_or_create_secret_key(key_path)
        .map_err(|error| ManagerError::Io(std::io::Error::other(error.to_string())))?;
    bind_workspace_endpoint_with_secret(secret, allow, alpn, profile, make_host).await
}

/// Binds a workspace using caller-supplied key material, useful when key storage is external.
pub async fn bind_workspace_endpoint_with_secret<H, F>(
    secret: iroh::SecretKey,
    allow: &BTreeSet<String>,
    alpn: &'static [u8],
    profile: NetworkProfile,
    make_host: F,
) -> Result<Box<dyn EndpointHandle>, ManagerError>
where
    H: SessionHost,
    F: Fn() -> anyhow::Result<H> + Send + Sync + 'static,
{
    let mut allowed = HashSet::new();
    for value in allow {
        allowed.insert(parse_endpoint_id(value).map_err(|_| ManagerError::Invalid)?);
    }
    if allowed.is_empty() {
        return Err(ManagerError::Unauthorized);
    }
    let hosts = Arc::new(Hosts::new().with(alpn, SharedHost::new(make_host)));
    let alpns = hosts.alpns();
    let endpoint = match profile {
        NetworkProfile::Default => bind_endpoint_alpns(secret, alpns).await,
        NetworkProfile::Local => bind_endpoint_local_alpns(secret, alpns).await,
        NetworkProfile::Relay(url) => {
            let relay = parse_relay_url(&url).map_err(|_| ManagerError::Invalid)?;
            bind_endpoint_with_relay_alpns(secret, alpns, relay).await
        }
    }
    .map_err(|error| ManagerError::Io(std::io::Error::other(error.to_string())))?;
    let direct_addr = endpoint
        .bound_sockets()
        .into_iter()
        .find(|address| address.is_ipv4())
        .map(|address| SocketAddrV4::new(Ipv4Addr::LOCALHOST, address.port()))
        .ok_or_else(|| {
            ManagerError::Io(std::io::Error::new(
                std::io::ErrorKind::AddrNotAvailable,
                "iroh endpoint has no IPv4 socket",
            ))
        })?;
    let endpoint_id = format_endpoint_id(&endpoint.id());
    let shutdown = CancellationToken::new();
    let accept_endpoint = endpoint.clone();
    let accept_shutdown = shutdown.clone();
    let allowed = Arc::new(allowed);
    let limit = Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS_PER_WORKSPACE));
    let active_tasks = Arc::new(AtomicUsize::new(0));
    let worker_active = Arc::clone(&active_tasks);
    let worker_runtime = match build_worker_runtime(|| {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
    }) {
        Ok(runtime) => runtime,
        Err(error) => {
            endpoint.close().await;
            return Err(error);
        }
    };
    let accept_task = match std::thread::Builder::new()
        .name("fux-iroh-endpoint".into())
        .spawn(move || {
            worker_runtime.block_on(async move {
                let mut connections = tokio::task::JoinSet::new();
                loop {
                    tokio::select! {
                        biased;
                        () = accept_shutdown.cancelled() => break,
                        completed = connections.join_next(), if !connections.is_empty() => {
                            let _ = completed;
                            continue;
                        }
                        incoming = accept_endpoint.accept() => {
                            let Some(incoming) = incoming else { break };
                            let Ok(permit) = Arc::clone(&limit).try_acquire_owned() else {
                                incoming.refuse();
                                continue;
                            };
                            let hosts = Arc::clone(&hosts);
                            let allowed = Arc::clone(&allowed);
                            let active = Arc::clone(&worker_active);
                            connections.spawn(async move {
                                let _active = ActiveTask::new(active);
                                let _permit = permit;
                                let connection = match tokio::time::timeout(Duration::from_secs(10), incoming).await {
                                    Ok(Ok(value)) => value,
                                    Ok(Err(_)) | Err(_) => return,
                                };
                                if !allowed.contains(&connection.remote_id()) {
                                    connection.close(1_u32.into(), b"not authorized");
                                    return;
                                }
                                hosts.serve_connection(connection).await;
                            });
                        }
                    }
                }
                let _ = tokio::time::timeout(ENDPOINT_SHUTDOWN_TIMEOUT, accept_endpoint.close()).await;
                let deadline = tokio::time::sleep(ENDPOINT_SHUTDOWN_TIMEOUT);
                tokio::pin!(deadline);
                while !connections.is_empty() {
                    tokio::select! {
                        _ = &mut deadline => {
                            connections.abort_all();
                            while connections.join_next().await.is_some() {}
                            break;
                        }
                        completed = connections.join_next() => {
                            if completed.is_none() { break; }
                        }
                    }
                }
            });
        }) {
        Ok(task) => task,
        Err(error) => {
            endpoint.close().await;
            return Err(ManagerError::Io(error));
        }
    };
    Ok(Box::new(ProductionEndpoint {
        _endpoint: endpoint,
        endpoint_id,
        direct_addr,
        shutdown,
        accept_task: Some(accept_task),
        active_tasks,
    }))
}

impl EndpointHandle for ProductionEndpoint {
    fn endpoint_id(&self) -> &str {
        &self.endpoint_id
    }
    fn direct_addr(&self) -> SocketAddrV4 {
        self.direct_addr
    }
    fn close(&mut self) {
        self.shutdown.cancel();
        if let Some(task) = self.accept_task.take() {
            let _ = task.join();
        }
    }
    fn reap_terminal_sessions(&mut self, _now_ms: u64, _ttl_ms: u64) {}
    fn active_tasks(&self) -> usize {
        self.active_tasks.load(Ordering::Acquire)
    }
}

impl Drop for ProductionEndpoint {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_runtime_build_failure_is_returned_to_the_factory() {
        let result = build_worker_runtime(|| {
            Err(std::io::Error::other(
                "injected runtime construction failure",
            ))
        });
        assert!(matches!(&result, Err(ManagerError::Io(_))));
        assert!(result.is_err_and(|error| {
            error
                .to_string()
                .contains("injected runtime construction failure")
        }));
    }
}
