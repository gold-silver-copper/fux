//! Attach-side bootstrap: find the running server or start one.

use super::{
    DaemonPaths, Descriptor, ManagerIdentity, ManagerReply, ManagerRequest, STARTUP_TIMEOUT,
    ServerChild, manager_request, manager_request_until, read_descriptor,
};
use anyhow::{Result, bail};

/// The descriptor of an existing workspace from the running session server, `None` when no
/// server is listening.
pub fn resolve(paths: &DaemonPaths, name: Option<&str>) -> Result<Option<Descriptor>> {
    match manager_request(
        &paths.manager_socket,
        &ManagerRequest::Resolve {
            name: name.map(str::to_owned),
        },
    ) {
        Ok(reply) => reply.into_descriptor().map(Some),
        Err(error) if no_server(&error) => Ok(None),
        Err(error) => Err(error.context(
            "cannot use the existing session server; if it is older than this fux, save your work in it and restart it",
        )),
    }
}

/// No session server is listening (as opposed to one that answered badly).
pub fn no_server(error: &anyhow::Error) -> bool {
    error.downcast_ref::<std::io::Error>().is_some_and(|error| {
        matches!(
            error.kind(),
            std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
        )
    })
}

/// Starts a background session server owning `name` and waits for its descriptor.
pub fn start_server(paths: &DaemonPaths, name: &str) -> Result<Descriptor> {
    let executable = std::env::current_exe()?;
    let mut child = ServerChild::spawn(&paths.runtime_dir, &executable, name)?;
    let deadline = std::time::Instant::now() + STARTUP_TIMEOUT;
    loop {
        // READY may arrive before the manager answers; keep polling until the deadline either way.
        child.poll()?;
        if let Ok(ManagerReply::Info { info }) =
            manager_request_until(&paths.manager_socket, &ManagerRequest::Info, deadline)
        {
            anyhow::ensure!(
                info.pid == child.pid(),
                "another server won startup; owned workspace was not created"
            );
            let identity = ManagerIdentity {
                pid: info.pid,
                instance_nonce: info.instance_nonce,
            };
            if let Ok(descriptor) = read_descriptor(&paths.descriptor(name)?, name, &identity) {
                anyhow::ensure!(
                    child.confirm(descriptor.pid),
                    "workspace belongs to another server"
                );
                return Ok(descriptor);
            }
        }
        if std::time::Instant::now() >= deadline {
            bail!("session server startup timed out");
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}
