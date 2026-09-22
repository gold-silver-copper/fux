//! Hostile inputs from hunt 5, kept in the harness as currently-failing
//! assertions. This scenario is registered but excluded from `all`, so the
//! default smoke stays green; run it explicitly with `--scenario hostile`.
//! Each check asserts the fix, so a vulnerable binary FAILS here and a
//! hardened one passes. See fux-fuzz/BREAKS.md for the analysis.
use super::*;
use crate::runtime;

const TAB: &str = "fux::model::Tab";
const WORKSPACE: &str = "fux::model::Workspace";

/// Finding 001 (class 1). A `Viewer` component on a layout entity makes
/// navigation::repair treat that entity as a viewer; the next viewer-relationship
/// insert queues repair, which never converges and overflows the stack, aborting
/// the process. The fix: repair must ignore Viewer components on layout entities
/// (or be depth-bounded). Passing means the server is still healthy afterwards.
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
    Ok(())
}

pub(super) fn run(s: &mut Server) -> Result<()> {
    viewer_on_layout_entity_repair_recursion(s)
}
