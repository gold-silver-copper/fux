//! Hostile inputs from hunt 5, kept as regression coverage in the default
//! smoke (`--scenario all`) and runnable alone with `--scenario hostile`.
//! Each check asserts the fix, so a vulnerable binary FAILS here. See
//! fux-fuzz/BREAKS.md for the analysis and the fixing commits.
use super::*;
use crate::runtime;

const TAB: &str = "fux::model::Tab";
const WORKSPACE: &str = "fux::model::Workspace";

/// Finding 001 (class 1). A `Viewer` component on a layout entity made
/// navigation::repair treat that entity as a viewer; the next viewer-relationship
/// insert queued repair, which never converged and overflowed the stack,
/// aborting the process. Fixed: a `Viewer` inserted onto a layout node is
/// removed, and repair selects only genuine viewers. Passing means the server
/// still answers, the tab carries no `Viewer`, and the real viewer's workspace,
/// tab and focused pane still belong together.
fn viewer_on_layout_entity_repair_recursion(s: &mut Server) -> Result<()> {
    let viewer = s
        .rpc("fux.attach", json!({"rows":24,"cols":80}))?
        .get("viewer")
        .and_then(Value::as_u64)
        .ok_or("attach returned no viewer")?;
    s.wait("first shell output", |s| {
        Ok(s.frame(viewer, 24, 80)?.contains("DEFAULT-SHELL"))
    })?;
    let tab = runtime::id(s.query(TAB)?.first().ok_or("no tab")?)?;
    let workspace = runtime::id(s.query(WORKSPACE)?.first().ok_or("no workspace")?)?;

    // 1) Put a Viewer component on the tab entity.
    let _ = s.rpc(
        "world.insert_components",
        json!({"entity":tab,"components":{
            "fux::model::Viewer":{"rows":24,"cols":80,"zoom":false,"scrollback":0,"notice":null}}}),
    );
    // 2) Any viewer relationship on the real viewer queues repair.
    let _ = s.rpc(
        "world.insert_components",
        json!({"entity":viewer,"components":{"fux::model::Viewing":workspace}}),
    );

    // The server must survive and keep answering. On a vulnerable binary this
    // request fails at the transport because the process has already aborted.
    s.wait("server survives a Viewer on a layout entity", |s| {
        Ok(!s.frame(viewer, 24, 80)?.is_empty())
    })
    .map_err(|e| {
        format!(
            "application: a Viewer component on a tab entity crashed the server (finding 001): {e}"
        )
    })?;
    let kept = s
        .rpc(
            "world.get_components",
            json!({"entity":tab,"components":["fux::model::Viewer"]}),
        )?
        .pointer("/components/fux::model::Viewer")
        .is_some();
    ensure(
        !kept,
        "application: a Viewer component stayed on a tab entity (finding 001)",
    )?;
    let viewing = s.relation(viewer, "fux::model::Viewing")?;
    let on_tab = s.relation(viewer, "fux::model::OnTab")?;
    let focused = s.relation(viewer, "fux::model::Focused")?;
    let parent = |s: &mut Server, entity: u64| -> Result<Option<u64>> {
        Ok(s.rpc(
            "world.get_components",
            json!({"entity":entity,"components":["bevy_ecs::hierarchy::ChildOf"]}),
        )?
        .pointer("/components/bevy_ecs::hierarchy::ChildOf")
        .and_then(Value::as_u64))
    };
    ensure(
        viewing == workspace && parent(s, on_tab)? == Some(workspace),
        "application: the viewer's tab is not in its workspace after finding 001's inputs",
    )?;
    let mut cursor = focused;
    for _ in 0..64 {
        if cursor == on_tab {
            return Ok(());
        }
        match parent(s, cursor)? {
            Some(next) => cursor = next,
            None => break,
        }
    }
    Err(
        "application: the viewer's focused pane is not in its tab after finding 001's inputs"
            .into(),
    )
}

pub(super) fn run(s: &mut Server) -> Result<()> {
    viewer_on_layout_entity_repair_recursion(s)
}
