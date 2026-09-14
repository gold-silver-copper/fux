//! Revalidate a selected attempt and resolve its live workspace route without changing focus.
use super::{model::Target, store::Store, supervise::Expected};
use anyhow::{Context, Result};
use std::{
    path::Path,
    time::{Duration, Instant},
};

pub(crate) fn resolve(root: &Path, expected: &Expected) -> Result<Target> {
    let store = Store::open(root)?;
    expected.validate(store.journal())?;
    // Read the authoritative retained target, never a runtime/socket path supplied by the client.
    let session = store
        .journal()
        .sessions
        .get(&expected.session)
        .context("selected session missing")?;
    let location = super::route::locate(&session.target, Instant::now() + Duration::from_secs(2))?;
    let origin = location.origin();
    let mut target = session.target.clone();
    target.workspace = location.workspace;
    target.stream = location.stream;
    target.origin = Some(origin);
    Ok(target)
}
