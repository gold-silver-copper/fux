//! Application quiescence guard for the thin koh identity-reset delegate.
use anyhow::Context as _;
use std::path::Path;

pub fn reset(paths: &crate::daemon::DaemonPaths, path: &Path) -> anyhow::Result<()> {
    paths.prepare()?;
    let _startup = crate::daemon::StartupLock::acquire(&paths.runtime_dir)?;
    let _manager = crate::daemon::ManagerLock::exclude_for_key_reset(paths)
        .context("cannot reset while a fux manager is running or its socket is unsafe; stop all workspaces with `fux workspace kill NAME` first (this ends their panes)")?;
    koh::identity::reset(path)
}
