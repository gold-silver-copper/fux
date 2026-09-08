//! Navigation through generic fux focus; no change to task ownership or outcome.
use super::{model::Target, submit};
use anyhow::Result;
use serde_json::{Value, json};
use std::time::{Duration, Instant};

pub fn target(target: &Target) -> Result<Value> {
    anyhow::ensure!(
        super::model::workspace(&target.workspace)
            && target.runtime.is_absolute()
            && target.pid.is_some_and(|pid| pid > 0)
            && target.stream > 0,
        "invalid focus handle"
    );
    let deadline = Instant::now() + Duration::from_secs(3);
    submit::verify_target(target, deadline)?;
    crate::fux::completed_until(
        &target.runtime.join(format!("{}.sock", target.workspace)),
        json!({"command":"focus","id":1,"instance":target.instance,"target":{"pane":target.pane}}),
        deadline,
    )
}
