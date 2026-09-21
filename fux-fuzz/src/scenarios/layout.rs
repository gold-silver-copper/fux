use super::*;

const TAB: &str = "fux::model::Tab";
const WORKSPACE: &str = "fux::model::Workspace";
const PANE_VIEW: &str = "fux::model::PaneView";
const CHILD_OF: &str = "bevy_ecs::hierarchy::ChildOf";

fn entities(s: &mut Server, component: &str) -> Result<Vec<u64>> {
    s.query(component)?.iter().map(id).collect()
}
fn viewer(s: &mut Server, viewer: u64) -> Result<Value> {
    let rows = s.query(VIEWER)?;
    let row = rows
        .iter()
        .find(|row| id(row).ok() == Some(viewer))
        .ok_or("layout viewer disappeared")?;
    component(row, VIEWER)
}
fn notice_text(s: &mut Server, id: u64) -> Result<String> {
    Ok(viewer(s, id)?
        .pointer("/notice/text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned())
}
fn pane_of(s: &mut Server, leaf: u64) -> Result<u64> {
    s.query(PANE_VIEW)?
        .iter()
        .find(|r| id(r).ok() == Some(leaf))
        .and_then(|r| r.pointer("/components/fux::model::PaneView/pane")?.as_u64())
        .ok_or_else(|| format!("leaf {leaf} has no pane").into())
}
fn pid_of(s: &mut Server, pane: u64) -> Result<i32> {
    let state = states(s)?
        .into_iter()
        .find(|(entity, _)| *entity == pane)
        .ok_or("pane process missing")?
        .1;
    pid(&state)
}
fn split(s: &mut Server, v: u64, marker: &str) -> Result<(u64, u64, i32)> {
    // An empty tab has no focused pane yet.
    let before = s.relation(v, "fux::model::Focused").ok();
    s.control(v, json!({"kind":"split","axis":"horizontal","program":format!("stty raw -echo; printf '\\033[2J\\033[H{marker}'; exec cat > /dev/null")}))?;
    let mut leaf = 0;
    s.wait("split focuses the new pane", |s| {
        leaf = s.relation(v, "fux::model::Focused")?;
        Ok(Some(leaf) != before && s.frame(v, 24, 80)?.contains(marker))
    })?;
    let pane = pane_of(s, leaf)?;
    let mut pid = 0;
    s.wait("split process running", |s| {
        pid = pid_of(s, pane).unwrap_or(0);
        Ok(pid > 0)
    })?;
    Ok((leaf, pane, pid))
}
fn expect_dead(s: &mut Server, label: &str, pid: i32) -> Result<()> {
    s.wait(label, |_| Ok(!alive(pid))).map_err(|e| {
        format!("application: {label}: process {pid} should have been terminated: {e}").into()
    })
}
fn expect_count(s: &mut Server, label: &str, component: &str, count: usize) -> Result<()> {
    let mut seen = 0;
    s.wait(label, |s| {
        seen = entities(s, component)?.len();
        Ok(seen == count)
    })
    .map_err(|e| {
        format!("application: {label}: expected {count} {component} entities, saw {seen}: {e}")
            .into()
    })
}

pub(super) fn run(s: &mut Server) -> Result<()> {
    let f = s.attach(24, 80)?;
    let v = s.frontend(f)?.viewer;
    s.wait("first shell output", |s| {
        Ok(s.frame(v, 24, 80)?.contains("DEFAULT-SHELL"))
    })?;
    let shell_pid = *running(s)?.first().ok_or("no shell")?;
    let leaf1 = s.relation(v, "fux::model::Focused")?;
    let pane1 = pane_of(s, leaf1)?;
    let workspace1 = s.relation(v, "fux::model::Viewing")?;
    let tab1 = s.relation(v, "fux::model::OnTab")?;

    // Move a pane to a new tab: the viewer follows and the process survives.
    let (leaf2, _pane2, pid2) = split(s, v, "PANE-2")?;
    s.control(
        v,
        json!({"kind":"move","to":{"kind":"new_tab","name":"second"}}),
    )?;
    expect_count(s, "second tab created", TAB, 2)?;
    let tab2 = s.relation(v, "fux::model::OnTab")?;
    ensure(
        tab2 != tab1,
        "viewer did not follow the moved pane to the new tab",
    )?;
    ensure(
        s.relation(v, "fux::model::Focused")? == leaf2,
        "moved pane lost focus",
    )?;
    s.wait("moved pane painted alone", |s| {
        let frame = s.frame(v, 24, 80)?;
        Ok(frame.starts_with("PANE-2") && !frame.contains("DEFAULT-SHELL"))
    })?;
    ensure(alive(pid2) && alive(shell_pid), "move terminated a process")?;
    // Closing that tab terminates its only-referenced process, not the shell.
    s.control(v, json!({"kind":"close","subject":{"tab":tab2}}))?;
    expect_count(s, "closed tab removed", TAB, 1)?;
    expect_dead(s, "tab close terminates the unreferenced process", pid2)?;
    ensure(
        alive(shell_pid),
        "tab close killed a process in another tab",
    )?;
    s.wait("viewer returned to the first tab", |s| {
        Ok(s.relation(v, "fux::model::OnTab")? == tab1
            && s.relation(v, "fux::model::Focused")? == leaf1)
    })?;

    // The same through a new workspace.
    let (_leaf3, _pane3, pid3) = split(s, v, "PANE-3")?;
    s.control(
        v,
        json!({"kind":"move","to":{"kind":"new_workspace","name":"scratch"}}),
    )?;
    expect_count(s, "second workspace created", WORKSPACE, 2)?;
    let workspace2 = s.relation(v, "fux::model::Viewing")?;
    ensure(
        workspace2 != workspace1,
        "viewer did not follow to the new workspace",
    )?;
    s.control(
        v,
        json!({"kind":"close","subject":{"workspace":workspace2}}),
    )?;
    expect_count(s, "closed workspace removed", WORKSPACE, 1)?;
    expect_dead(
        s,
        "workspace close terminates the unreferenced process",
        pid3,
    )?;
    ensure(alive(shell_pid), "workspace close killed the shell")?;
    s.wait("viewer returned to the surviving workspace", |s| {
        Ok(s.relation(v, "fux::model::Viewing")? == workspace1)
    })?;

    // A second view of the shell's process: closing either view alone keeps
    // the process; closing the last reference terminates it.
    let extra = s
        .rpc(
            "world.spawn_entity",
            json!({"components":{PANE_VIEW:{"pane":pane1}}}),
        )?
        .get("entity")
        .and_then(Value::as_u64)
        .ok_or("spawn returned no entity")?;
    s.rpc(
        "world.reparent_entities",
        json!({"entities":[extra],"parent":tab1}),
    )?;
    s.wait("second view painted", |s| {
        // Both views show the same shell output side by side.
        Ok(s.frame(v, 24, 80)?.matches("DEFAULT-SHELL").count() >= 2)
    })?;
    ensure(
        s.query(CHILD_OF)?.iter().any(|r| {
            id(r).ok() == Some(extra)
                && r.pointer("/components/bevy_ecs::hierarchy::ChildOf")
                    .and_then(Value::as_u64)
                    == Some(tab1)
        }),
        "second view is not a child of the tab",
    )?;
    s.control(v, json!({"kind":"close","subject":{"pane":extra}}))?;
    s.wait("second view removed", |s| {
        Ok(!entities(s, PANE_VIEW)?.contains(&extra))
    })?;
    ensure(
        alive(shell_pid),
        "closing one of two views terminated the shared process",
    )?;
    s.wait("original view still focused", |s| {
        Ok(s.relation(v, "fux::model::Focused")? == leaf1)
    })?;

    // An interactive confirmation whose target disappears must not retarget.
    let (leaf4, _pane4, pid4) = split(s, v, "PANE-4")?;
    s.send(f, b"\x02x")?; // confirm closing pane 4 (focused)
    s.wait("close confirmation open", |s| {
        Ok(s.frame(v, 24, 80)?.to_lowercase().contains("close"))
    })?;
    s.control(v, json!({"kind":"close","subject":{"pane":leaf4}}))?;
    expect_dead(s, "API close terminates pane 4", pid4)?;
    s.wait("focus fell back to the shell", |s| {
        Ok(s.relation(v, "fux::model::Focused")? == leaf1)
    })?;
    s.send(f, b"y")?;
    s.wait("confirmation reports the vanished target", |s| {
        let text = notice_text(s, v)?;
        Ok(text.contains("no longer exists") || text.contains("target changed"))
    })
    .map_err(|e| format!("application: confirming a close whose target vanished: {e}"))?;
    ensure(
        alive(shell_pid) && entities(s, PANE_VIEW)?.contains(&leaf1),
        "a stale confirmation closed the newly focused pane",
    )?;

    // Closing the last pane leaves an empty tab; a split opens a pane in it.
    s.control(v, json!({"kind":"close","subject":{"pane":leaf1}}))?;
    expect_dead(s, "closing the last pane terminates its process", shell_pid)?;
    expect_count(s, "empty tab retained", TAB, 1)?;
    ensure(
        entities(s, PANE_VIEW)?.is_empty(),
        "pane view survived close",
    )?;
    let (leaf5, _pane5, pid5) = split(s, v, "PANE-5")?;
    ensure(
        entities(s, PANE_VIEW)? == vec![leaf5],
        "split into an empty tab",
    )?;

    // Closing the only workspace detaches its viewers gracefully.
    s.frontend(f)?.expected_exit = true;
    s.control(
        v,
        json!({"kind":"close","subject":{"workspace":workspace1}}),
    )?;
    s.wait("frontend exits when its last workspace closes", |s| {
        Ok(s.frontend(f)?.exited)
    })?;
    ensure(
        s.frontend(f)?.exit_success,
        "application: frontend exited abnormally after its workspace closed",
    )?;
    ensure(
        s.frontend(f)?.terminal_restored()?,
        "frontend did not restore the terminal after its workspace closed",
    )?;
    expect_dead(s, "closing the last workspace terminates its process", pid5)?;
    s.wait("viewer removed", |s| Ok(!entities(s, VIEWER)?.contains(&v)))?;
    // The server keeps running and can still accept a new attachment.
    let g = s.attach(24, 80)?;
    let w = s.frontend(g)?.viewer;
    s.wait("new attachment gets a workspace and a shell", |s| {
        Ok(s.frame(w, 24, 80)?.contains("DEFAULT-SHELL"))
    })
    .map_err(|e| format!("application: attaching after the last workspace closed: {e}").into())
}
