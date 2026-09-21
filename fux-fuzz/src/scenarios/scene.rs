use super::*;

const PATH: &str = "layout.scn.ron";

fn notice(s: &mut Server, viewer: u64) -> Result<Value> {
    let rows = s.query(VIEWER)?;
    let row = rows
        .iter()
        .find(|row| id(row).ok() == Some(viewer))
        .ok_or("scene viewer disappeared")?;
    Ok(component(row, VIEWER)?
        .get("notice")
        .cloned()
        .unwrap_or(Value::Null))
}
fn settled(s: &mut Server, viewer: u64, label: &str) -> Result<Value> {
    let mut last = Value::Null;
    s.wait(label, |s| {
        last = notice(s, viewer)?;
        let text = last.get("text").and_then(Value::as_str).unwrap_or_default();
        Ok(!text.ends_with("..."))
    })?;
    Ok(last)
}
fn leaves(s: &mut Server) -> Result<Vec<u64>> {
    let mut ids: Vec<u64> = s
        .query("fux::model::PaneView")?
        .iter()
        .map(id)
        .collect::<Result<_>>()?;
    ids.sort_unstable();
    Ok(ids)
}

pub(super) fn run(s: &mut Server) -> Result<()> {
    let f = s.attach(24, 80)?;
    let v = s.frontend(f)?.viewer;
    s.wait("first shell output", |s| {
        Ok(s.frame(v, 24, 80)?.contains("DEFAULT-SHELL"))
    })?;
    let workspace = s.relation(v, "fux::model::Viewing")?;
    s.control(v, json!({"kind":"split","axis":"horizontal","program":"stty raw -echo; printf '\\033[2J\\033[HSCENE-B'; exec cat > /dev/null"}))?;
    s.wait("second pane painted", |s| {
        Ok(s.frame(v, 24, 80)?.contains("SCENE-B"))
    })?;
    let pids = running(s)?;
    ensure(pids.len() == 2, "expected two processes")?;
    let before = leaves(s)?;
    ensure(before.len() == 2, "expected two pane views")?;

    // Save, then confirm the file exists and the save reported success.
    s.control(
        v,
        json!({"kind":"save_layout","workspace":workspace,"path":PATH}),
    )?;
    let saved = settled(s, v, "save completed")?;
    s.journal.record("save_layout", json!({"notice":saved}))?;
    ensure(
        saved.get("error") != Some(&json!(true)),
        &format!("application: saving a layout reported an error: {saved}"),
    )?;
    s.wait("layout file written", |s| {
        Ok(s.directory.join(PATH).is_file())
    })?;
    let text = fs::read_to_string(s.directory.join(PATH))?;
    s.journal
        .record("scene_bytes", json!({"bytes":text.len()}))?;
    ensure(
        !text.contains("Terminal") && !text.contains("Launch"),
        "application: a saved layout must not contain runtimes or process recipes",
    )?;

    // Loading the layout it just saved must succeed and keep both processes.
    s.control(
        v,
        json!({"kind":"load_layout","workspace":workspace,"path":PATH,"mapping":[]}),
    )?;
    let loaded = settled(s, v, "load completed")?;
    s.journal.record("load_layout", json!({"notice":loaded}))?;
    ensure(
        loaded.get("error") != Some(&json!(true)),
        &format!("application: loading a layout that was just saved failed: {loaded}"),
    )?;
    ensure(
        pids.iter().all(|p| alive(*p)),
        "application: loading a layout terminated a live process",
    )?;
    s.wait("layout still shows both panes", |s| {
        let frame = s.frame(v, 24, 80)?;
        Ok(frame.contains("SCENE-B") && frame.contains("DEFAULT-SHELL"))
    })
    .map_err(|e| format!("application: reloading the saved layout lost a pane: {e}"))?;
    // Loading never launches a process.
    let after_load = running(s)?;
    ensure(
        after_load.len() == pids.len(),
        &format!(
            "application: loading a layout changed the process count from {} to {}",
            pids.len(),
            after_load.len()
        ),
    )?;

    // A layout whose referenced process no longer exists must fail without
    // replacing the current layout.
    let focused = s.relation(v, "fux::model::Focused")?;
    s.control(v, json!({"kind":"close","subject":{"pane":focused}}))?;
    s.wait("one pane closed", |s| Ok(leaves(s)?.len() == 1))?;
    let survivors = leaves(s)?;
    s.control(
        v,
        json!({"kind":"load_layout","workspace":workspace,"path":PATH,"mapping":[]}),
    )?;
    let failed = settled(s, v, "stale load completed")?;
    s.journal.record("stale_load", json!({"notice":failed}))?;
    ensure(
        failed.get("error") == Some(&json!(true)),
        &format!(
            "application: loading a layout that references a terminated process must fail: {failed}"
        ),
    )?;
    ensure(
        leaves(s)? == survivors,
        "application: a failed layout load must not replace the current layout",
    )?;

    // A missing file fails cleanly and leaves the layout alone.
    s.control(
        v,
        json!({"kind":"load_layout","workspace":workspace,"path":"no-such-layout.scn.ron","mapping":[]}),
    )?;
    let missing = settled(s, v, "missing load completed")?;
    ensure(
        missing.get("error") == Some(&json!(true)),
        &format!("application: loading a missing layout file must report an error: {missing}"),
    )?;
    ensure(
        leaves(s)? == survivors,
        "application: a missing layout load must not replace the current layout",
    )?;
    Ok(())
}
