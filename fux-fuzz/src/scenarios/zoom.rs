use super::*;

fn locate(frame: &str, marker: &str) -> Option<(u16, u16)> {
    frame.lines().enumerate().find_map(|(y, line)| {
        line.find(marker)
            .map(|i| (line[..i].chars().count() as u16, y as u16))
    })
}
fn widths(frame: &str, cols: u16) -> Result<(u16, u16)> {
    let (ax, ay) = locate(frame, "PANE-1").ok_or("PANE-1 not painted")?;
    let (bx, by) = locate(frame, "PANE-2").ok_or("PANE-2 not painted")?;
    ensure(
        (ax, ay) == (0, 0) && by == 0 && bx > 1,
        "unexpected side-by-side layout",
    )?;
    Ok((bx - 1, cols - bx))
}
fn dims(s: &mut Server, pids: &[i32]) -> Result<Vec<(u16, u16)>> {
    let states = states(s)?;
    pids.iter()
        .map(|p| {
            let state = states
                .iter()
                .map(|(_, state)| state)
                .find(|state| pid(state).ok() == Some(*p))
                .ok_or("pane process disappeared")?;
            Ok((
                state.get("rows").and_then(Value::as_u64).unwrap_or(0) as u16,
                state.get("cols").and_then(Value::as_u64).unwrap_or(0) as u16,
            ))
        })
        .collect()
}
fn expect_dims(s: &mut Server, pids: &[i32], label: &str, wanted: &[(u16, u16)]) -> Result<()> {
    let mut actual = Vec::new();
    let result = s.wait(label, |s| {
        actual = dims(s, pids)?;
        Ok(actual == wanted)
    });
    s.journal.record(
        "zoom_dims",
        json!({"label":label,"wanted":wanted,"actual":actual}),
    )?;
    result.map_err(|e| {
        format!("application: {label}: pane sizes should be {wanted:?} but are {actual:?}: {e}")
            .into()
    })
}

pub(super) fn run(s: &mut Server) -> Result<()> {
    let a = s.attach(24, 80)?;
    let b = s.attach(40, 120)?;
    let va = s.frontend(a)?.viewer;
    let vb = s.frontend(b)?.viewer;
    child_command(
        s,
        a,
        "stty raw -echo; printf '\\033[2J\\033[HPANE-1'; exec cat > /dev/null",
    )?;
    s.wait("pane 1 ready", |s| {
        Ok(s.frame(va, 24, 80)?.starts_with("PANE-1"))
    })?;
    let leaf1 = s.relation(va, "fux::model::Focused")?;
    s.control(va, json!({"kind":"split","axis":"horizontal","program":"stty raw -echo; printf '\\033[2J\\033[HPANE-2'; exec cat > /dev/null"}))?;
    let leaf2 = s.relation(va, "fux::model::Focused")?;
    ensure(leaf1 != leaf2, "split did not create a second pane")?;
    s.wait("pane 2 ready in both viewers", |s| {
        Ok(s.frame(va, 24, 80)?.contains("PANE-2") && s.frame(vb, 40, 120)?.contains("PANE-2"))
    })?;
    let pids = running(s)?;
    ensure(pids.len() == 2, "expected exactly two processes")?;
    // Map processes to panes through the order of PaneView entities.
    let views = s.query("fux::model::PaneView")?;
    let pane_of = |leaf: u64| -> Result<u64> {
        views
            .iter()
            .find(|r| id(r).ok() == Some(leaf))
            .and_then(|r| {
                r.pointer("/components/fux::model::PaneView/pane")
                    .and_then(Value::as_u64)
            })
            .ok_or("pane view missing".into())
    };
    let (p1, p2) = (pane_of(leaf1)?, pane_of(leaf2)?);
    let all = states(s)?;
    let pid_of = |pane: u64| -> Result<i32> {
        all.iter()
            .find(|(entity, _)| *entity == pane)
            .map(|(_, state)| pid(state))
            .ok_or("pane process missing")?
    };
    let pids = [pid_of(p1)?, pid_of(p2)?];

    let fa = s.frame(va, 24, 80)?;
    let fb = s.frame(vb, 40, 120)?;
    let (a1, a2) = widths(&fa, 80)?;
    let (b1, b2) = widths(&fb, 120)?;
    s.journal.record(
        "zoom_layout",
        json!({"a":[a1,a2],"b":[b1,b2],"leaves":[leaf1,leaf2]}),
    )?;
    // Shared size is the minimum over both viewers; A is the smaller one.
    expect_dims(
        s,
        &pids,
        "shared size before zoom",
        &[(23, a1.min(b1)), (23, a2.min(b2))],
    )?;

    // Zoom pane 2 (focused) in A only: A shows pane 2 alone at full width, so
    // pane 2's width is now bounded by B, and pane 1 is bounded by B alone.
    s.send(a, b"\x02z")?;
    s.wait("A zoomed", |s| {
        let frame = s.frame(va, 24, 80)?;
        Ok(frame.starts_with("PANE-2") && !frame.contains("PANE-1"))
    })?;
    ensure(
        s.frame(vb, 40, 120)?.starts_with("PANE-1") && s.frame(vb, 40, 120)?.contains("PANE-2"),
        "zoom in one viewer changed another viewer's layout",
    )?;
    expect_dims(
        s,
        &pids,
        "shared size while A zooms pane 2",
        &[(39, b1), (23, 80.min(b2))],
    )?;

    // Hidden panes are unreachable while zoomed, by design: the focus command
    // reports a notice and the zoomed frame does not change.
    s.control(va, json!({"kind":"focus","pane":leaf1}))?;
    s.wait("hidden focus target refused", |s| {
        let rows = s.query(VIEWER)?;
        let row = rows
            .iter()
            .find(|row| id(row).ok() == Some(va))
            .ok_or("viewer A disappeared")?;
        Ok(component(row, VIEWER)?.pointer("/notice/error") == Some(&json!(true)))
    })?;
    ensure(
        s.frame(va, 24, 80)?.starts_with("PANE-2"),
        "focusing a hidden pane changed the zoomed frame",
    )?;

    // Unzoom, focus pane 1, zoom again: zoom follows the focused pane.
    s.send(a, b"\x02z")?;
    s.wait("A unzoomed for refocus", |s| {
        Ok(s.frame(va, 24, 80)?.contains("PANE-1"))
    })?;
    s.control(va, json!({"kind":"focus","pane":leaf1}))?;
    s.send(a, b"\x02z")?;
    s.wait("A zoomed on pane 1", |s| {
        let frame = s.frame(va, 24, 80)?;
        Ok(frame.starts_with("PANE-1") && !frame.contains("PANE-2"))
    })?;
    expect_dims(
        s,
        &pids,
        "shared size while A zooms pane 1",
        &[(23, 80.min(b1)), (39, b2)],
    )?;

    // Unzoom restores the split and the original shared sizes.
    s.send(a, b"\x02z")?;
    s.wait("A unzoomed", |s| {
        let frame = s.frame(va, 24, 80)?;
        Ok(frame.starts_with("PANE-1") && frame.contains("PANE-2"))
    })?;
    expect_dims(
        s,
        &pids,
        "shared size after unzoom",
        &[(23, a1.min(b1)), (23, a2.min(b2))],
    )?;

    // Zoom in A, then detach B: the zoomed pane's size is A's alone and the
    // hidden pane keeps its last size rather than collapsing.
    s.send(a, b"\x02z")?;
    s.wait("A zoomed again", |s| {
        Ok(!s.frame(va, 24, 80)?.contains("PANE-2"))
    })?;
    expect_dims(
        s,
        &pids,
        "zoomed with B present",
        &[(23, 80.min(b1)), (39, b2)],
    )?;
    detach(s, b)?;
    expect_dims(s, &pids, "zoomed after B detached", &[(23, 80), (39, b2)])?;
    ensure(pids.iter().all(|p| alive(*p)), "detach killed a pane")?;
    Ok(())
}
