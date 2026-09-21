use super::*;

const TAB: &str = "fux::model::Tab";

fn focused(s: &mut Server, v: u64) -> Result<u64> {
    s.relation(v, "fux::model::Focused")
}
fn on_tab(s: &mut Server, v: u64) -> Result<u64> {
    s.relation(v, "fux::model::OnTab")
}
fn expect(
    s: &mut Server,
    label: &str,
    mut check: impl FnMut(&mut Server) -> Result<(u64, u64)>,
) -> Result<()> {
    let mut seen = (0, 0);
    let result = s.wait(label, |s| {
        let (got, expected) = check(s)?;
        seen = (got, expected);
        Ok(got == expected)
    });
    s.journal.record(
        "memory_check",
        json!({"label":label,"seen":seen.0,"expected":seen.1}),
    )?;
    result.map_err(|e| {
        format!(
            "application: {label}: expected {} but saw {}: {e}",
            seen.1, seen.0
        )
        .into()
    })
}
fn split(s: &mut Server, v: u64, marker: &str) -> Result<u64> {
    let before = focused(s, v).ok();
    s.control(v, json!({"kind":"split","axis":"horizontal","program":format!("stty raw -echo; printf '\\033[2J\\033[H{marker}'; exec cat > /dev/null")}))?;
    let mut leaf = 0;
    s.wait("split focused the new pane", |s| {
        leaf = focused(s, v)?;
        Ok(Some(leaf) != before && s.frame(v, 24, 80)?.contains(marker))
    })?;
    Ok(leaf)
}

pub(super) fn run(s: &mut Server) -> Result<()> {
    let fa = s.attach(24, 80)?;
    let a = s.frontend(fa)?.viewer;
    s.wait("first shell output", |s| {
        Ok(s.frame(a, 24, 80)?.contains("DEFAULT-SHELL"))
    })?;
    let t1 = on_tab(s, a)?;
    let p1 = focused(s, a)?;
    let p2 = split(s, a, "MEM-2")?;
    let fb = s.attach(24, 80)?;
    let b = s.frontend(fb)?.viewer;
    ensure(
        on_tab(s, b)? == t1,
        "second viewer did not join the first tab",
    )?;
    let b_initial = focused(s, b)?;
    let pids = running(s)?;

    // A opens a new tab; B stays where it is.
    s.control(a, json!({"kind":"tab_new","name":"two"}))?;
    s.wait("second tab", |s| Ok(s.query(TAB)?.len() == 2))?;
    let t2 = on_tab(s, a)?;
    ensure(t2 != t1, "tab_new did not select the new tab")?;
    let p3 = focused(s, a)?;
    expect(s, "B is unaffected by A's new tab", |s| {
        Ok((on_tab(s, b)?, t1))
    })?;
    expect(s, "B's focus is unaffected by A's new tab", |s| {
        Ok((focused(s, b)?, b_initial))
    })?;

    // A returns to tab one and gets back the pane it last focused there.
    s.control(a, json!({"kind":"select","scope":"tab","entity":t1}))?;
    expect(s, "A back on tab one", |s| Ok((on_tab(s, a)?, t1)))?;
    expect(s, "A's remembered focus on tab one", |s| {
        Ok((focused(s, a)?, p2))
    })?;

    // A changes its tab-one focus; the memory follows it across a round trip.
    s.control(a, json!({"kind":"focus","pane":p1}))?;
    expect(s, "A focused pane one", |s| Ok((focused(s, a)?, p1)))?;
    s.control(a, json!({"kind":"select","scope":"tab","entity":t2}))?;
    expect(s, "A on tab two again", |s| Ok((on_tab(s, a)?, t2)))?;
    expect(s, "A's tab-two focus remembered", |s| {
        Ok((focused(s, a)?, p3))
    })?;
    s.control(a, json!({"kind":"select","scope":"tab","entity":t1}))?;
    expect(s, "A's updated tab-one focus remembered", |s| {
        Ok((focused(s, a)?, p1))
    })?;

    // focus_last on tab one returns to the pane A left there.
    s.control(a, json!({"kind":"focus_last"}))?;
    expect(
        s,
        "focus_last returns to the previously focused pane",
        |s| Ok((focused(s, a)?, p2)),
    )?;

    // B switches tabs independently; A is untouched.
    let a_tab = on_tab(s, a)?;
    let a_focus = focused(s, a)?;
    s.control(b, json!({"kind":"select","scope":"tab","entity":t2}))?;
    expect(s, "B on tab two", |s| Ok((on_tab(s, b)?, t2)))?;
    expect(s, "B focused the only pane of tab two", |s| {
        Ok((focused(s, b)?, p3))
    })?;
    expect(s, "A's tab unchanged by B", |s| Ok((on_tab(s, a)?, a_tab)))?;
    expect(s, "A's focus unchanged by B", |s| {
        Ok((focused(s, a)?, a_focus))
    })?;

    // next/previous cycle within the workspace and land on real tabs.
    s.control(a, json!({"kind":"next","scope":"tab"}))?;
    let next = on_tab(s, a)?;
    s.journal
        .record("tab_next", json!({"from":a_tab,"to":next}))?;
    ensure(
        next != a_tab && (next == t1 || next == t2),
        "next did not land on another tab of this workspace",
    )?;
    s.control(a, json!({"kind":"previous","scope":"tab"}))?;
    expect(s, "previous returns to the original tab", |s| {
        Ok((on_tab(s, a)?, a_tab))
    })?;

    // A moves its focused pane to a new tab and follows it; B, on tab two,
    // is untouched, and B's memory for tab one never names the moved pane.
    s.control(a, json!({"kind":"focus","pane":p2}))?;
    expect(s, "A focused pane two before the move", |s| {
        Ok((focused(s, a)?, p2))
    })?;
    s.control(
        a,
        json!({"kind":"move","to":{"kind":"new_tab","name":"three"}}),
    )?;
    s.wait("third tab", |s| Ok(s.query(TAB)?.len() == 3))?;
    let t3 = on_tab(s, a)?;
    ensure(
        t3 != t1 && t3 != t2,
        "A did not follow the moved pane to its new tab",
    )?;
    expect(s, "A focuses the moved pane", |s| Ok((focused(s, a)?, p2)))?;
    expect(s, "B is unaffected by A's move", |s| {
        Ok((on_tab(s, b)?, t2))
    })?;
    s.control(b, json!({"kind":"select","scope":"tab","entity":t1}))?;
    expect(s, "B returns to tab one", |s| Ok((on_tab(s, b)?, t1)))?;
    let b_focus = focused(s, b)?;
    s.journal.record(
        "b_focus_after_move",
        json!({"focus":b_focus,"moved":p2,"remaining":p1}),
    )?;
    ensure(
        b_focus == p1,
        &format!(
            "application: after pane {p2} moved out of tab one, B focused {b_focus} instead of the remaining pane {p1}"
        ),
    )?;

    ensure(
        pids.iter().all(|p| alive(*p)),
        "a tab switch or move terminated a process",
    )?;
    Ok(())
}
