use super::*;

const TAB: &str = "fux::model::Tab";
const WORKSPACE: &str = "fux::model::Workspace";
const PANE_VIEW: &str = "fux::model::PaneView";
const CHILD_OF: &str = "bevy_ecs::hierarchy::ChildOf";
const ORDER: &str = "fux::model::WorkspaceOrder";

fn notice_text(s: &mut Server, viewer: u64) -> Result<String> {
    let rows = s.query(VIEWER)?;
    let row = rows
        .iter()
        .find(|row| id(row).ok() == Some(viewer))
        .ok_or("soak viewer disappeared")?;
    Ok(component(row, VIEWER)?
        .pointer("/notice/text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned())
}
fn settled(s: &mut Server, v: u64, label: &str) -> Result<String> {
    let mut text = String::new();
    s.wait(label, |s| {
        text = notice_text(s, v)?;
        Ok(!text.is_empty() && !text.ends_with("..."))
    })?;
    Ok(text)
}

/// The invariants every other scenario checks once, checked after each cycle.
fn invariants(s: &mut Server, v: u64, cycle: usize) -> Result<()> {
    let ws: Vec<u64> = s.query(WORKSPACE)?.iter().map(id).collect::<Result<_>>()?;
    let tabs: Vec<u64> = s.query(TAB)?.iter().map(id).collect::<Result<_>>()?;
    let parents: Vec<(u64, u64)> = s
        .query(CHILD_OF)?
        .iter()
        .filter_map(|r| {
            Some((
                id(r).ok()?,
                r.pointer("/components/bevy_ecs::hierarchy::ChildOf")?
                    .as_u64()?,
            ))
        })
        .collect();
    for t in &tabs {
        let p = parents.iter().find(|(e, _)| e == t).map(|(_, p)| *p);
        ensure(
            p.is_some_and(|p| ws.contains(&p)),
            &format!("application: cycle {cycle}: tab {t} has parent {p:?}, not a workspace"),
        )?;
    }
    let views = s.query(PANE_VIEW)?;
    let states = states(s)?;
    for r in &views {
        let leaf = id(r)?;
        let pane = r
            .pointer("/components/fux::model::PaneView/pane")
            .and_then(Value::as_u64)
            .ok_or("view without pane")?;
        let state = states.iter().find(|(e, _)| *e == pane).map(|(_, st)| st);
        ensure(
            state.is_some(),
            &format!(
                "application: cycle {cycle}: view {leaf} refers to pane {pane} with no process state"
            ),
        )?;
        if let Some(st) = state
            && st.pointer("/status/kind") == Some(&json!("running"))
            && let Ok(p) = pid(st)
        {
            ensure(
                alive(p),
                &format!(
                    "application: cycle {cycle}: pane {pane} reports running pid {p} which is dead"
                ),
            )?;
        }
    }
    let mut orders: Vec<i64> = s
        .query(ORDER)?
        .iter()
        .filter_map(|r| {
            r.pointer("/components/fux::model::WorkspaceOrder")?
                .as_i64()
        })
        .collect();
    let n = orders.len();
    orders.sort_unstable();
    orders.dedup();
    ensure(
        orders.len() == n,
        &format!("application: cycle {cycle}: duplicate WorkspaceOrder values"),
    )?;
    let leaves: Vec<u64> = views.iter().map(id).collect::<Result<_>>()?;
    let viewers: Vec<u64> = s.query(VIEWER)?.iter().map(id).collect::<Result<_>>()?;
    ensure(
        viewers.contains(&v),
        &format!("application: cycle {cycle}: the driving viewer vanished"),
    )?;
    for viewer in viewers {
        let viewing = s.relation(viewer, "fux::model::Viewing")?;
        let on_tab = s.relation(viewer, "fux::model::OnTab")?;
        let focused = s.relation(viewer, "fux::model::Focused")?;
        ensure(
            ws.contains(&viewing),
            &format!("application: cycle {cycle}: viewer {viewer} Viewing dangles"),
        )?;
        ensure(
            tabs.contains(&on_tab),
            &format!("application: cycle {cycle}: viewer {viewer} OnTab dangles"),
        )?;
        ensure(
            leaves.contains(&focused),
            &format!("application: cycle {cycle}: viewer {viewer} Focused dangles"),
        )?;
        ensure(
            parents.iter().any(|(e, p)| *e == on_tab && *p == viewing),
            &format!("application: cycle {cycle}: viewer {viewer}'s tab is not in its workspace"),
        )?;
    }
    ensure(
        !s.frame(v, 24, 80)?.is_empty(),
        &format!("application: cycle {cycle}: fux.frame returned an empty paint"),
    )?;
    Ok(())
}

pub(super) fn run(s: &mut Server, cycles: usize) -> Result<()> {
    let f = s.attach(24, 80)?;
    let v = s.frontend(f)?.viewer;
    s.wait("first shell output", |s| {
        Ok(s.frame(v, 24, 80)?.contains("DEFAULT-SHELL"))
    })?;
    let shell_pid = *running(s)?.first().ok_or("no shell")?;
    // A second viewer that never acts: its relationships must be repaired
    // through every replacement and close the driving viewer causes.
    let _bystander = s.attach(24, 80)?;
    let mut home_ws = s.relation(v, "fux::model::Viewing")?;
    let mut home_tab = s.relation(v, "fux::model::OnTab")?;
    let mut home_leaf = s.relation(v, "fux::model::Focused")?;
    invariants(s, v, 0)?;
    let mut processes_after = Vec::new();
    for cycle in 1..=cycles {
        // Split a scratch pane, move it through a tab and a workspace, close
        // that workspace, save and reload the home workspace, resize and
        // zoom, then return home.
        s.control(v, json!({"kind":"focus","pane":home_leaf}))?;
        s.wait("home focused", |s| {
            Ok(s.relation(v, "fux::model::Focused")? == home_leaf)
        })?;
        s.control(v, json!({"kind":"split","axis":if cycle % 2 == 0 {"horizontal"} else {"vertical"},"program":format!("stty raw -echo; printf '\\033[2J\\033[HSOAK-{cycle}'; exec cat > /dev/null")}))?;
        let scratch = s.relation(v, "fux::model::Focused")?;
        ensure(scratch != home_leaf, "split did not focus the scratch pane")?;
        s.wait("scratch painted", |s| {
            Ok(s.frame(v, 24, 80)?.contains(&format!("SOAK-{cycle}")))
        })?;
        s.control(
            v,
            json!({"kind":"move","to":{"kind":"new_tab","name":format!("t{cycle}")}}),
        )?;
        s.wait("moved to a new tab", |s| {
            Ok(s.relation(v, "fux::model::OnTab")? != home_tab)
        })?;
        s.control(v, json!({"kind":"resize","axis":"horizontal","grow":true}))?;
        s.control(v, json!({"kind":"zoom"}))?;
        s.control(
            v,
            json!({"kind":"move","to":{"kind":"new_workspace","name":format!("w{cycle}")}}),
        )?;
        let scratch_ws = s.relation(v, "fux::model::Viewing")?;
        ensure(scratch_ws != home_ws, "move did not create a new workspace")?;
        s.control(v, json!({"kind":"zoom"}))?;
        s.control(
            v,
            json!({"kind":"save_layout","workspace":home_ws,"path":"soak.scn.ron"}),
        )?;
        let saved = settled(s, v, "soak save")?;
        ensure(
            saved.starts_with("saved"),
            &format!("application: cycle {cycle}: save failed: {saved}"),
        )?;
        s.control(
            v,
            json!({"kind":"close","subject":{"workspace":scratch_ws}}),
        )?;
        s.wait("scratch workspace closed", |s| {
            Ok(s.relation(v, "fux::model::Viewing")? == home_ws)
        })?;
        s.control(
            v,
            json!({"kind":"load_layout","workspace":home_ws,"path":"soak.scn.ron","mapping":[]}),
        )?;
        let loaded = settled(s, v, "soak load")?;
        ensure(
            loaded.starts_with("loaded"),
            &format!("application: cycle {cycle}: load failed: {loaded}"),
        )?;
        // The load replaced the home workspace; it keeps its name and the
        // viewer follows, so refresh the home identities.
        let new_home = s.relation(v, "fux::model::Viewing")?;
        ensure(
            new_home != home_ws,
            &format!("application: cycle {cycle}: load did not replace the workspace"),
        )?;
        home_ws = new_home;
        home_tab = s.relation(v, "fux::model::OnTab")?;
        home_leaf = s.relation(v, "fux::model::Focused")?;
        s.control(v, json!({"kind":"resize","axis":"horizontal","grow":false}))?;
        invariants(s, v, cycle)?;
        ensure(
            alive(shell_pid),
            &format!("application: cycle {cycle}: the home shell died"),
        )?;
        return_home(s, v, cycle)?;
        processes_after.push(states(s)?.len());
    }
    s.journal.record("soak_processes", json!(processes_after))?;
    let max = processes_after.iter().copied().max().unwrap_or(0);
    ensure(
        max <= 2,
        &format!("application: process count grew across cycles: {processes_after:?}"),
    )?;
    Ok(())
}

/// After each cycle exactly one process should remain: the home shell. The
/// scratch pane lived in the closed workspace.
fn return_home(s: &mut Server, v: u64, cycle: usize) -> Result<()> {
    let mut count = 0;
    s.wait("only the home shell remains", |s| {
        count = states(s)?.len();
        Ok(count == 1)
    })
    .map_err(|e| format!("application: cycle {cycle}: {count} processes remain after closing the scratch workspace: {e}"))?;
    let _ = v;
    Ok(())
}
