//! Resolve a freshly observed agent through the service's own fux runtime.
use crate::{
    tasks::model::{Origin, Target},
    watch::Handle,
};
use anyhow::{Result, ensure};
use std::{
    path::Path,
    time::{Duration, Instant},
};

pub(crate) fn resolve(runtime: &Path, handle: &Handle) -> Result<serde_json::Value> {
    let location = crate::fux::manager::locate(
        runtime,
        &handle.instance,
        handle.pane,
        Instant::now() + Duration::from_secs(2),
    )?;
    ensure!(
        location.instance == handle.instance
            && location.pane == handle.pane
            && handle.pid == Some(location.pid)
            && location.pid > 0
            && location.accepts_input,
        "observed process exited or was replaced"
    );
    ensure!(
        crate::fux::endpoint::valid_name(&location.workspace)
            && location.stream > 0
            && crate::fux::endpoint::valid_name(&location.origin_workspace)
            && location.origin_stream > 0,
        "invalid observed process route"
    );
    Ok(serde_json::to_value(Target {
        runtime: runtime.into(),
        instance: location.instance,
        workspace: location.workspace,
        stream: location.stream,
        pane: location.pane,
        pid: Some(location.pid),
        origin: Some(Origin {
            workspace: location.origin_workspace,
            stream: location.origin_stream,
        }),
    })?)
}
