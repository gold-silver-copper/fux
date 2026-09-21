use super::*;

const PATH: &str = "map.scn.ron";
const PANE_VIEW: &str = "fux::model::PaneView";
const WORKSPACE: &str = "fux::model::Workspace";

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
        Ok(!text.is_empty() && !text.ends_with("..."))
    })?;
    Ok(last)
}
fn leaves(s: &mut Server) -> Result<Vec<u64>> {
    let mut ids: Vec<u64> = s.query(PANE_VIEW)?.iter().map(id).collect::<Result<_>>()?;
    ids.sort_unstable();
    Ok(ids)
}
fn count(s: &mut Server, component: &str) -> Result<usize> {
    Ok(s.query(component)?.len())
}
/// Saved process references, as `pane: <id>` inside PaneView blocks.
fn saved_panes(ron: &str) -> Vec<u64> {
    let mut out = Vec::new();
    for (i, _) in ron.match_indices("pane:") {
        let rest = ron.get(i + 5..).unwrap_or_default();
        let digits: String = rest
            .trim_start()
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        if let Ok(n) = digits.parse() {
            out.push(n);
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}
/// The saved workspace root: the top-level entity block that holds Workspace.
fn saved_root(ron: &str) -> Option<u64> {
    let i = ron.find("\"fux::model::Workspace\"")?;
    let head = ron.get(..i)?;
    let mut last = None;
    for line in head.lines() {
        let t = line.trim_start();
        if line.len() - t.len() == 4 && t.ends_with(": (") {
            last = t.trim_end_matches(": (").parse().ok();
        }
    }
    last
}
fn spawn_launch(s: &mut Server, marker: &str) -> Result<u64> {
    let cwd = s.directory.clone();
    let argv = [
        "/bin/sh".to_owned(),
        "-c".to_owned(),
        format!("printf '\\033[2J\\033[H{marker}'; exec cat > /dev/null"),
    ];
    let entity = s
        .rpc(
            "world.spawn_entity",
            json!({"components":{LAUNCH:{"argv":argv,"cwd":cwd,"history_lines":100}}}),
        )?
        .get("entity")
        .and_then(Value::as_u64)
        .ok_or("spawn returned no entity")?;
    s.wait("launched process running", |s| {
        Ok(states(s)?
            .iter()
            .any(|(e, st)| *e == entity && pid(st).is_ok()))
    })?;
    Ok(entity)
}
fn load(s: &mut Server, v: u64, workspace: u64, mapping: Value, label: &str) -> Result<Value> {
    s.control(
        v,
        json!({"kind":"load_layout","workspace":workspace,"path":PATH,"mapping":mapping}),
    )?;
    let done = settled(s, v, label)?;
    s.journal
        .record("load_layout", json!({"label":label,"notice":done}))?;
    Ok(done)
}
fn expect_error(done: &Value, label: &str, fragment: &str) -> Result<()> {
    let text = done.get("text").and_then(Value::as_str).unwrap_or_default();
    ensure(
        done.get("error") == Some(&json!(true)) && text.contains(fragment),
        &format!("application: {label} should fail mentioning {fragment:?}, got {done}"),
    )
}

pub(super) fn run(s: &mut Server) -> Result<()> {
    let f = s.attach(24, 80)?;
    let v = s.frontend(f)?.viewer;
    s.wait("first shell output", |s| {
        Ok(s.frame(v, 24, 80)?.contains("DEFAULT-SHELL"))
    })?;
    let workspace = s.relation(v, "fux::model::Viewing")?;
    let shell_pid = *running(s)?.first().ok_or("no shell")?;
    s.control(v, json!({"kind":"split","axis":"horizontal","program":"stty raw -echo; printf '\\033[2J\\033[HMAP-B'; exec cat > /dev/null"}))?;
    let leaf_b = s.relation(v, "fux::model::Focused")?;
    s.wait("second pane painted", |s| {
        Ok(s.frame(v, 24, 80)?.contains("MAP-B"))
    })?;
    let pid_b = running(s)?
        .into_iter()
        .find(|p| *p != shell_pid)
        .ok_or("second process")?;
    let pane_b = s
        .query(PANE_VIEW)?
        .iter()
        .find(|r| id(r).ok() == Some(leaf_b))
        .and_then(|r| r.pointer("/components/fux::model::PaneView/pane")?.as_u64())
        .ok_or("pane B")?;

    s.control(
        v,
        json!({"kind":"save_layout","workspace":workspace,"path":PATH}),
    )?;
    let saved = settled(s, v, "save completed")?;
    ensure(
        saved.get("error") != Some(&json!(true)),
        &format!("save failed: {saved}"),
    )?;
    let ron = fs::read_to_string(s.directory.join(PATH))?;
    let saved_ids = saved_panes(&ron);
    let root = saved_root(&ron).ok_or("saved workspace root not found")?;
    s.journal.record(
        "saved_layout",
        json!({"panes":saved_ids,"root":root,"bytes":ron.len()}),
    )?;
    ensure(
        saved_ids.len() == 2 && saved_ids.contains(&pane_b),
        &format!(
            "saved layout should reference both live processes including {pane_b}: {saved_ids:?}"
        ),
    )?;

    // Terminate B, replace it with a fresh process, and map the saved
    // reference onto the replacement: the load must succeed, show the new
    // process, and launch nothing itself.
    s.control(v, json!({"kind":"close","subject":{"pane":leaf_b}}))?;
    s.wait("pane B terminated", |_| Ok(!alive(pid_b)))?;
    let fresh = spawn_launch(s, "MAP-NEW")?;
    let processes = states(s)?.len();
    let done = load(
        s,
        v,
        workspace,
        json!([[pane_b, fresh]]),
        "mapped load completed",
    )?;
    ensure(
        done.get("error") != Some(&json!(true)),
        &format!("application: loading with a valid old-to-existing mapping failed: {done}"),
    )?;
    s.wait("mapped layout painted", |s| {
        let frame = s.frame(v, 24, 80)?;
        Ok(frame.contains("MAP-NEW") && frame.contains("DEFAULT-SHELL"))
    })
    .map_err(|e| format!("application: mapped load did not show the mapped process: {e}"))?;
    ensure(
        states(s)?.len() == processes,
        "application: loading a layout launched a process",
    )?;
    ensure(
        alive(shell_pid),
        "application: loading a layout killed the shell",
    )?;
    let restored = leaves(s)?;
    ensure(
        restored.len() == 2,
        "restored layout should have two pane views",
    )?;
    // A successful load replaces the workspace entity, so later requests must
    // name the workspace the viewer is on now, not the one that was saved.
    let workspace = s.relation(v, "fux::model::Viewing")?;

    // Every invalid mapping fails without replacing the layout.
    let done = load(
        s,
        v,
        workspace,
        json!([[pane_b, fresh], [pane_b, fresh]]),
        "duplicate mapping load",
    )?;
    expect_error(&done, "a duplicate pane mapping", "duplicate")?;
    ensure(
        leaves(s)? == restored,
        "application: a rejected duplicate mapping replaced the layout",
    )?;
    let done = load(
        s,
        v,
        workspace,
        json!([[root, fresh]]),
        "layout-entity mapping load",
    )?;
    expect_error(
        &done,
        "a mapping that names a layout entity",
        "layout entit",
    )?;
    ensure(
        leaves(s)? == restored,
        "application: a rejected layout-entity mapping replaced the layout",
    )?;
    let done = load(
        s,
        v,
        workspace,
        json!([[pane_b, 4_294_967_295u64]]),
        "missing target load",
    )?;
    expect_error(
        &done,
        "a mapping to a nonexistent process",
        "missing live pane",
    )?;
    ensure(
        leaves(s)? == restored,
        "application: a rejected missing-target mapping replaced the layout",
    )?;
    ensure(
        states(s)?.len() == processes,
        "application: a rejected mapping launched or terminated a process",
    )?;

    // Re-save so the file names only live processes, then let the watched
    // configuration load it: a saved workspace with the same name replaces
    // the current one; a renamed copy is added beside it.
    s.control(
        v,
        json!({"kind":"save_layout","workspace":workspace,"path":PATH}),
    )?;
    let saved = settled(s, v, "re-save completed")?;
    ensure(
        saved.get("error") != Some(&json!(true)),
        &format!("re-save failed: {saved}"),
    )?;
    let before_ws = count(s, WORKSPACE)?;
    let before_leaves = leaves(s)?;
    fs::write(
        s.directory.join("fux.json"),
        serde_json::to_vec(&json!({"layout":PATH}))?,
    )?;
    s.journal.record("write_config", json!({"layout":PATH}))?;
    let mut now = Vec::new();
    s.wait("configured layout replaced the same-named workspace", |s| {
        now = leaves(s)?;
        Ok(now != before_leaves && now.len() == 2)
    })
    .map_err(|e| format!("application: `layout` in fux.json did not replace the same-named workspace: leaves {before_leaves:?} -> {now:?}: {e}"))?;
    ensure(
        count(s, WORKSPACE)? == before_ws,
        "application: a same-named configured layout added a workspace instead of replacing",
    )?;
    ensure(
        states(s)?.len() == processes && alive(shell_pid),
        "application: the configured layout changed processes",
    )?;
    s.wait("viewer follows the replaced workspace", |s| {
        let frame = s.frame(v, 24, 80)?;
        Ok(frame.contains("MAP-NEW") && frame.contains("DEFAULT-SHELL"))
    })?;

    // Both the workspace and its tab are named "main"; rename every one so the
    // saved workspace name matches no existing workspace.
    let renamed = fs::read_to_string(s.directory.join(PATH))?.replace("\"main\"", "\"loaded\"");
    ensure(
        renamed.contains("\"loaded\""),
        "saved layout did not carry the workspace name",
    )?;
    fs::write(s.directory.join(PATH), renamed)?;
    s.journal
        .record("rewrite_layout", json!({"name":"loaded"}))?;
    let mut ws_now = 0;
    s.wait("renamed configured layout was added", |s| {
        ws_now = count(s, WORKSPACE)?;
        Ok(ws_now == before_ws + 1)
    })
    .map_err(|e| format!("application: a configured layout with a new name was not added ({before_ws} -> {ws_now}): {e}"))?;
    ensure(
        states(s)?.len() == processes,
        "application: adding a configured layout launched a process",
    )?;
    Ok(())
}
