use super::*;

const TAB: &str = "fux::model::Tab";
const WORKSPACE: &str = "fux::model::Workspace";
const PANE_VIEW: &str = "fux::model::PaneView";

fn rel(s: &mut Server, v: u64, which: &str) -> Result<u64> {
    s.relation(v, &format!("fux::model::{which}"))
}
fn expect_rel(s: &mut Server, v: u64, which: &str, want: u64, label: &str) -> Result<()> {
    let mut got = 0;
    s.wait(label, |s| {
        got = rel(s, v, which)?;
        Ok(got == want)
    })
    .map_err(|e| format!("application: {label}: {which} should be {want}, is {got}: {e}").into())
}
fn pane_of(s: &mut Server, leaf: u64) -> Result<u64> {
    s.query(PANE_VIEW)?
        .iter()
        .find(|r| id(r).ok() == Some(leaf))
        .and_then(|r| r.pointer("/components/fux::model::PaneView/pane")?.as_u64())
        .ok_or("leaf has no pane".into())
}
fn pid_of(s: &mut Server, pane: u64) -> Result<i32> {
    states(s)?
        .into_iter()
        .find(|(e, _)| *e == pane)
        .map(|(_, st)| pid(&st))
        .ok_or("pane state missing")?
}
fn despawn(s: &mut Server, entity: u64) -> Result<()> {
    s.rpc("world.despawn_entity", json!({"entity":entity}))?;
    Ok(())
}

pub(super) fn run(s: &mut Server) -> Result<()> {
    let fa = s.attach(24, 80)?;
    let a = s.frontend(fa)?.viewer;
    s.wait("first shell output", |s| {
        Ok(s.frame(a, 24, 80)?.contains("DEFAULT-SHELL"))
    })?;
    let ws = rel(s, a, "Viewing")?;
    let tab1 = rel(s, a, "OnTab")?;
    let p1 = rel(s, a, "Focused")?;
    s.control(a, json!({"kind":"split","axis":"horizontal","program":"stty raw -echo; printf '\\033[2J\\033[HREP-2'; exec cat > /dev/null"}))?;
    let p2 = rel(s, a, "Focused")?;
    s.wait("pane 2", |s| Ok(s.frame(a, 24, 80)?.contains("REP-2")))?;
    s.control(a, json!({"kind":"tab_new","name":"two"}))?;
    let tab2 = rel(s, a, "OnTab")?;
    let p3 = rel(s, a, "Focused")?;
    s.wait("tab two painted", |s| Ok(s.query(TAB)?.len() == 2))?;
    // A returns to tab one focused on pane 2; B attaches and views tab two.
    s.control(a, json!({"kind":"select","scope":"tab","entity":tab1}))?;
    s.control(a, json!({"kind":"focus","pane":p2}))?;
    expect_rel(s, a, "Focused", p2, "A focused on pane 2")?;
    let fb = s.attach(24, 80)?;
    let b = s.frontend(fb)?.viewer;
    s.control(b, json!({"kind":"select","scope":"tab","entity":tab2}))?;
    expect_rel(s, b, "OnTab", tab2, "B on tab two")?;
    expect_rel(s, b, "Focused", p3, "B focused on pane 3")?;
    let (pane1, pane2, pane3) = (pane_of(s, p1)?, pane_of(s, p2)?, pane_of(s, p3)?);
    let (pid1, pid2, pid3) = (pid_of(s, pane1)?, pid_of(s, pane2)?, pid_of(s, pane3)?);

    // Raw despawn of A's focused pane view: A lands on the first surviving
    // leaf of its tab, and the process is not terminated, because native
    // hierarchy removal does not own shared processes.
    despawn(s, p2)?;
    expect_rel(s, a, "Focused", p1, "A repaired to the surviving pane")?;
    expect_rel(s, a, "OnTab", tab1, "A stays on its tab")?;
    ensure(
        alive(pid2),
        "application: a raw despawn of a pane view terminated its process; only a close owns processes",
    )?;
    expect_rel(s, b, "Focused", p3, "B unaffected by A's pane loss")?;

    // Raw despawn of B's tab: B falls back to the first tab and its first
    // leaf; pane 3's process survives; A is untouched.
    despawn(s, tab2)?;
    expect_rel(s, b, "OnTab", tab1, "B repaired to the first tab")?;
    expect_rel(
        s,
        b,
        "Focused",
        p1,
        "B focused the first leaf of the first tab",
    )?;
    ensure(
        alive(pid3),
        "application: a raw despawn of a tab terminated a process",
    )?;
    expect_rel(s, a, "OnTab", tab1, "A unaffected by B's tab loss")?;
    expect_rel(s, a, "Focused", p1, "A's focus unchanged by B's tab loss")?;
    ensure(s.query(TAB)?.len() == 1, "a despawned tab lingered")?;

    // Raw despawn of the only workspace: no workspace remains, so both viewers
    // are detached and their frontends exit gracefully; no process is
    // terminated; a new attach recreates the initial workspace.
    s.frontend(fa)?.expected_exit = true;
    s.frontend(fb)?.expected_exit = true;
    despawn(s, ws)?;
    s.wait("both frontends exit after their workspace vanished", |s| {
        Ok(s.frontend(fa)?.exited && s.frontend(fb)?.exited)
    })
    .map_err(|e| {
        format!("application: viewers of a raw-despawned workspace were not detached: {e}")
    })?;
    for (fi, label) in [(fa, "A"), (fb, "B")] {
        ensure(
            s.frontend(fi)?.exit_success,
            &format!(
                "application: frontend {label} exited abnormally after its workspace vanished"
            ),
        )?;
        ensure(
            s.frontend(fi)?.terminal_restored()?,
            &format!("application: frontend {label} did not restore its terminal"),
        )?;
    }
    ensure(
        alive(pid1) && alive(pid2) && alive(pid3),
        "application: a raw despawn of a workspace terminated a process",
    )?;
    ensure(
        s.query(VIEWER)?.is_empty(),
        "application: a viewer survived the loss of every workspace",
    )?;
    let fc = s.attach(24, 80)?;
    let c = s.frontend(fc)?.viewer;
    s.wait("a new attach recreates a workspace and shell", |s| {
        Ok(s.frame(c, 24, 80)?.contains("DEFAULT-SHELL"))
    })?;
    ensure(
        s.query(WORKSPACE)?.len() == 1,
        "application: recreation produced the wrong workspace count",
    )?;
    // The orphaned processes are still alive and untracked by any view.
    let views = s.query(PANE_VIEW)?.len();
    s.journal.record(
        "orphans",
        json!({"views":views,"orphaned_pids":[pid1,pid2,pid3]}),
    )?;
    Ok(())
}
