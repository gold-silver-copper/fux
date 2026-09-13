//! Resolve routing for an exact retained process. Discovery never replaces task identity.
use super::model::{Origin, Target};
use crate::fux::manager::Location;
use anyhow::{Context, Result, ensure};
use std::time::Instant;

impl Location {
    pub fn origin(&self) -> Origin {
        Origin {
            workspace: self.origin_workspace.clone(),
            stream: self.origin_stream,
        }
    }
}

pub(super) fn locate(target: &Target, deadline: Instant) -> Result<Location> {
    let location =
        crate::fux::manager::locate(&target.runtime, &target.instance, target.pane, deadline)?;
    validate(target, location)
}

/// Called only after durable managed identity exists. Lost replies retry the same exact process.
pub(super) fn release_pin(target: &Target, deadline: Instant) -> Result<()> {
    let pid = target
        .pid
        .filter(|pid| *pid != 0)
        .context("live process identity missing")?;
    crate::fux::manager::release_pin(
        &target.runtime,
        &target.instance,
        target.pane,
        pid,
        deadline,
    )
    .context("workspace pin release incomplete; reconcile the existing launch")
}

fn validate(target: &Target, location: Location) -> Result<Location> {
    ensure!(
        location.instance == target.instance
            && location.pane == target.pane
            && target.pane != 0
            && target.pid == Some(location.pid)
            && location.pid != 0
            && location.accepts_input,
        "pane process identity changed"
    );
    ensure!(
        super::model::workspace(&location.workspace)
            && location.stream != 0
            && super::model::workspace(&location.origin_workspace)
            && location.origin_stream != 0
            && location.tab != 0,
        "invalid pane route"
    );
    // The revision is inspected as a numeric field but is not authority for later mutations.
    let _ = location.layout_generation;
    ensure!(
        target
            .origin
            .as_ref()
            .is_none_or(|origin| *origin == location.origin()),
        "pane launch identity changed"
    );
    Ok(location)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    #[test]
    fn location_follows_routes_but_never_replaces_process_or_origin() -> Result<()> {
        let target = Target {
            runtime: "/tmp/fixture".into(),
            instance: "owner".into(),
            workspace: "original".into(),
            stream: 1,
            pane: 4,
            pid: Some(5),
            origin: Some(Origin {
                workspace: "original".into(),
                stream: 1,
            }),
        };
        let location = json!({"instance":"owner","pane":4,"pid":5,"accepts_input":true,"workspace":"moved","stream":9,
            "origin_workspace":"original","origin_stream":1,"tab":8,"layout_generation":3});
        let reply = |location: Value| -> Result<Location> { Ok(serde_json::from_value(location)?) };
        let found = validate(&target, reply(location.clone())?)?;
        assert_eq!(found.workspace, "moved");
        assert_eq!(found.stream, 9);
        for (key, value) in [
            ("instance", json!("replacement")),
            ("pane", json!(40)),
            ("pid", json!(50)),
            ("accepts_input", json!(false)),
            ("origin_workspace", json!("other")),
            ("origin_stream", json!(10)),
            ("workspace", json!("../escape")),
            ("stream", json!(0)),
            ("tab", json!(0)),
        ] {
            let mut invalid = location.clone();
            invalid
                .as_object_mut()
                .context("location object")?
                .insert(key.into(), value);
            assert!(
                validate(&target, reply(invalid)?).is_err(),
                "accepted {key}"
            );
        }
        let mut relocated = target.clone();
        relocated.workspace = "moved".into();
        relocated.stream = 9;
        assert_eq!(target.identity(), relocated.identity());
        Ok(())
    }
}
