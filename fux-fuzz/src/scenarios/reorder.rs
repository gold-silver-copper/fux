use super::*;

const TAB: &str = "fux::model::Tab";
const WORKSPACE: &str = "fux::model::Workspace";
const ORDER: &str = "fux::model::WorkspaceOrder";
const CHILDREN: &str = "bevy_ecs::hierarchy::Children";

fn children(s: &mut Server, parent: u64) -> Result<Vec<u64>> {
    s.query(CHILDREN)?
        .iter()
        .find(|r| id(r).ok() == Some(parent))
        .and_then(|r| {
            r.pointer("/components/bevy_ecs::hierarchy::Children")?
                .as_array()
                .cloned()
        })
        .map(|list| list.iter().filter_map(Value::as_u64).collect())
        .ok_or_else(|| format!("{parent} has no children").into())
}
fn tabs(s: &mut Server, workspace: u64) -> Result<Vec<u64>> {
    let tab_ids: Vec<u64> = s.query(TAB)?.iter().map(id).collect::<Result<_>>()?;
    Ok(children(s, workspace)?
        .into_iter()
        .filter(|e| tab_ids.contains(e))
        .collect())
}
fn workspace_orders(s: &mut Server) -> Result<Vec<(u64, i64)>> {
    let mut list: Vec<(u64, i64)> = s
        .query(ORDER)?
        .iter()
        .filter_map(|r| {
            Some((
                id(r).ok()?,
                r.pointer("/components/fux::model::WorkspaceOrder")?
                    .as_i64()?,
            ))
        })
        .collect();
    list.sort_by_key(|(entity, order)| (*order, *entity));
    Ok(list)
}
fn bar(s: &mut Server, v: u64) -> Result<String> {
    Ok(s.frame(v, 24, 80)?
        .lines()
        .last()
        .unwrap_or_default()
        .to_owned())
}
fn bar_positions(bar: &str, names: &[&str]) -> Vec<Option<usize>> {
    names.iter().map(|n| bar.find(n)).collect()
}
fn expect_tabs(s: &mut Server, label: &str, workspace: u64, want: &[u64]) -> Result<()> {
    let mut seen = Vec::new();
    let result = s.wait(label, |s| {
        seen = tabs(s, workspace)?;
        Ok(seen == want)
    });
    s.journal
        .record("tab_order", json!({"label":label,"seen":seen,"want":want}))?;
    result.map_err(|e| {
        format!("application: {label}: tabs should be {want:?} but are {seen:?}: {e}").into()
    })
}
fn locate(frame: &str, marker: &str) -> Option<usize> {
    frame.lines().next()?.find(marker)
}

pub(super) fn run(s: &mut Server) -> Result<()> {
    let f = s.attach(24, 80)?;
    let v = s.frontend(f)?.viewer;
    s.wait("first shell output", |s| {
        Ok(s.frame(v, 24, 80)?.contains("DEFAULT-SHELL"))
    })?;
    let ws1 = s.relation(v, "fux::model::Viewing")?;
    let t1 = s.relation(v, "fux::model::OnTab")?;
    s.control(
        v,
        json!({"kind":"rename","subject":{"tab":t1},"name":"one"}),
    )?;
    s.control(v, json!({"kind":"tab_new","name":"two"}))?;
    s.control(v, json!({"kind":"tab_new","name":"three"}))?;
    s.wait("three tabs", |s| Ok(s.query(TAB)?.len() == 3))?;
    let order = tabs(s, ws1)?;
    ensure(
        order.len() == 3 && order.first() == Some(&t1),
        "unexpected initial tab order",
    )?;
    let (t2, t3) = (
        *order.get(1).ok_or("second tab")?,
        *order.get(2).ok_or("third tab")?,
    );

    // Tab reorder from the viewer's current tab (one): next moves it right,
    // twice more pins it at the end, previous brings it back one place. The
    // bar lists tabs in the same order, and the viewer stays on its tab.
    s.control(v, json!({"kind":"select","scope":"tab","entity":t1}))?;
    s.wait("on tab one", |s| {
        Ok(s.relation(v, "fux::model::OnTab")? == t1)
    })?;
    s.control(v, json!({"kind":"reorder","scope":"tab","order":"next"}))?;
    expect_tabs(s, "tab one moved right", ws1, &[t2, t1, t3])?;
    s.control(v, json!({"kind":"reorder","scope":"tab","order":"next"}))?;
    expect_tabs(s, "tab one moved to the end", ws1, &[t2, t3, t1])?;
    s.control(v, json!({"kind":"reorder","scope":"tab","order":"next"}))?;
    expect_tabs(s, "reorder past the end is a no-op", ws1, &[t2, t3, t1])?;
    let bar_text = bar(s, v)?;
    let positions = bar_positions(&bar_text, &["two", "three", "one"]);
    s.journal.record(
        "bar_after_reorder",
        json!({"bar":bar_text,"positions":positions}),
    )?;
    let increasing = positions
        .iter()
        .zip(positions.iter().skip(1))
        .all(|(a, b)| a.is_some() && b.is_some() && a < b);
    ensure(
        positions.iter().all(Option::is_some) && increasing,
        &format!("application: the bar does not list tabs in their order: {bar_text:?}"),
    )?;
    ensure(
        s.relation(v, "fux::model::OnTab")? == t1,
        "reordering changed the viewer's tab",
    )?;
    s.control(
        v,
        json!({"kind":"reorder","scope":"tab","order":"previous"}),
    )?;
    expect_tabs(s, "tab one moved back left", ws1, &[t2, t1, t3])?;

    // Workspace reorder keeps WorkspaceOrder a duplicate-free sequence and
    // leaves every viewer on its workspace.
    s.control(v, json!({"kind":"workspace_new","name":"beta"}))?;
    s.control(v, json!({"kind":"workspace_new","name":"gamma"}))?;
    s.wait("three workspaces", |s| Ok(s.query(WORKSPACE)?.len() == 3))?;
    let ws3 = s.relation(v, "fux::model::Viewing")?;
    let before = workspace_orders(s)?;
    ensure(
        before.len() == 3 && before.last().map(|(e, _)| *e) == Some(ws3),
        "new workspace is not last",
    )?;
    let ws2 = before.get(1).ok_or("second workspace")?.0;
    s.control(
        v,
        json!({"kind":"reorder","scope":"workspace","order":"previous"}),
    )?;
    let mut after = Vec::new();
    s.wait("workspace moved earlier", |s| {
        after = workspace_orders(s)?;
        Ok(after.iter().map(|(e, _)| *e).collect::<Vec<_>>() == vec![ws1, ws3, ws2])
    })
    .map_err(|e| format!("application: workspace reorder previous: expected [{ws1}, {ws3}, {ws2}], saw {after:?}: {e}"))?;
    let orders: Vec<i64> = after.iter().map(|(_, o)| *o).collect();
    let mut unique = orders.clone();
    unique.sort_unstable();
    unique.dedup();
    ensure(
        unique.len() == orders.len(),
        &format!("application: WorkspaceOrder has duplicates after reorder: {after:?}"),
    )?;
    ensure(
        s.relation(v, "fux::model::Viewing")? == ws3,
        "reordering changed the viewer's workspace",
    )?;
    s.control(
        v,
        json!({"kind":"reorder","scope":"workspace","order":"previous"}),
    )?;
    s.wait("workspace moved first", |s| {
        Ok(workspace_orders(s)?.first().map(|(e, _)| *e) == Some(ws3))
    })?;
    s.control(
        v,
        json!({"kind":"reorder","scope":"workspace","order":"previous"}),
    )?;
    let pinned = workspace_orders(s)?;
    ensure(
        pinned.first().map(|(e, _)| *e) == Some(ws3),
        "reorder before the start moved the workspace",
    )?;

    // Closing a middle workspace leaves the survivors in their order, without
    // duplicates, and the viewer where it was.
    s.control(v, json!({"kind":"close","subject":{"workspace":ws1}}))?;
    s.wait("middle workspace closed", |s| {
        Ok(s.query(WORKSPACE)?.len() == 2)
    })?;
    let survivors = workspace_orders(s)?;
    s.journal.record("orders_after_close", json!(survivors))?;
    ensure(
        survivors.iter().map(|(e, _)| *e).collect::<Vec<_>>() == vec![ws3, ws2],
        &format!(
            "application: closing a middle workspace changed the survivors' order: {survivors:?}"
        ),
    )?;
    ensure(
        s.relation(v, "fux::model::Viewing")? == ws3,
        "closing another workspace moved the viewer",
    )?;
    // The closed workspace's three shells were terminated on purpose, and
    // their states must leave the world rather than linger as running with a
    // dead pid. Judge liveness only for the processes that exist from here on.
    let mut remaining = Vec::new();
    s.wait("closed workspace's processes leave the world", |s| {
        remaining = states(s)?;
        Ok(remaining.len() == 2)
    })
    .map_err(|e| {
        let listed: Vec<Value> = remaining.iter().map(|(_, st)| st.clone()).collect();
        format!(
            "application: after closing a workspace with three shells, {} process states remain instead of 2: {}: {e}",
            remaining.len(),
            serde_json::to_string(&listed).unwrap_or_default()
        )
    })?;
    let pids = running(s)?;
    s.journal.record("pids_after_close", json!(pids))?;

    // reorder_pane swaps a pane with its sibling; the frame shows the swap,
    // and at an edge it is a no-op.
    let leaf_a = s.relation(v, "fux::model::Focused")?;
    s.control(v, json!({"kind":"split","axis":"horizontal","program":"stty raw -echo; printf '\\033[2J\\033[HORD-B'; exec cat > /dev/null"}))?;
    let leaf_b = s.relation(v, "fux::model::Focused")?;
    ensure(leaf_b != leaf_a, "split did not focus the new pane")?;
    s.wait("both panes painted", |s| {
        Ok(s.frame(v, 24, 80)?.contains("ORD-B"))
    })?;
    let parent = s
        .query("bevy_ecs::hierarchy::ChildOf")?
        .iter()
        .find(|r| id(r).ok() == Some(leaf_b))
        .and_then(|r| {
            r.pointer("/components/bevy_ecs::hierarchy::ChildOf")?
                .as_u64()
        })
        .ok_or("pane has no parent")?;
    ensure(
        children(s, parent)? == vec![leaf_a, leaf_b],
        "unexpected sibling order after split",
    )?;
    let marker_before = locate(&s.frame(v, 24, 80)?, "ORD-B");
    s.control(v, json!({"kind":"reorder_pane","order":"previous"}))?;
    s.wait("pane moved before its sibling", |s| {
        Ok(children(s, parent)? == vec![leaf_b, leaf_a])
    })
    .map_err(|e| format!("application: reorder_pane previous: {e}"))?;
    let mut marker_after = None;
    s.wait("frame reflects the swap", |s| {
        marker_after = locate(&s.frame(v, 24, 80)?, "ORD-B");
        Ok(marker_after == Some(0))
    })
    .map_err(|e| format!("application: after reorder_pane, ORD-B should paint at column 0, was {marker_before:?} then {marker_after:?}: {e}"))?;
    s.control(v, json!({"kind":"reorder_pane","order":"previous"}))?;
    std::thread::sleep(std::time::Duration::from_millis(100));
    ensure(
        children(s, parent)? == vec![leaf_b, leaf_a],
        "application: reorder_pane at the first position moved the pane",
    )?;
    ensure(
        s.relation(v, "fux::model::Focused")? == leaf_b,
        "reorder_pane changed focus",
    )?;
    let dead: Vec<i32> = pids.iter().copied().filter(|p| !alive(*p)).collect();
    if !dead.is_empty() {
        let listed: Vec<Value> = states(s)?.into_iter().map(|(_, st)| st).collect();
        return Err(format!(
            "application: reordering terminated a process: dead {dead:?} of {pids:?}; states now {}",
            serde_json::to_string(&listed)?
        )
        .into());
    }
    Ok(())
}
