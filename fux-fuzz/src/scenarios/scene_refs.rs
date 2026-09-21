use super::*;
use nix::sys::stat::Mode;

const PANE_VIEW: &str = "fux::model::PaneView";
const WORKSPACE: &str = "fux::model::Workspace";

fn notice(s: &mut Server, viewer: u64) -> Result<Value> {
    let rows = s.query(VIEWER)?;
    let row = rows
        .iter()
        .find(|row| id(row).ok() == Some(viewer))
        .ok_or("refs viewer disappeared")?;
    Ok(component(row, VIEWER)?
        .get("notice")
        .cloned()
        .unwrap_or(Value::Null))
}
fn text_of(n: &Value) -> String {
    n.get("text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}
fn settled(s: &mut Server, viewer: u64, label: &str) -> Result<Value> {
    let mut last = Value::Null;
    s.wait(label, |s| {
        last = notice(s, viewer)?;
        let text = text_of(&last);
        Ok(!text.is_empty() && !text.ends_with("..."))
    })?;
    Ok(last)
}
fn leaves(s: &mut Server) -> Result<Vec<u64>> {
    let mut ids: Vec<u64> = s.query(PANE_VIEW)?.iter().map(id).collect::<Result<_>>()?;
    ids.sort_unstable();
    Ok(ids)
}
fn pane_of(s: &mut Server, leaf: u64) -> Result<u64> {
    s.query(PANE_VIEW)?
        .iter()
        .find(|r| id(r).ok() == Some(leaf))
        .and_then(|r| r.pointer("/components/fux::model::PaneView/pane")?.as_u64())
        .ok_or("leaf has no pane".into())
}
fn fifo(s: &mut Server, name: &str) -> Result<std::path::PathBuf> {
    let path = s.directory.join(name);
    nix::unistd::mkfifo(&path, Mode::S_IRWXU)?;
    Ok(path)
}
fn spawn_launch(s: &mut Server, program: &str) -> Result<(u64, i32)> {
    let cwd = s.directory.clone();
    let entity = s
        .rpc(
            "world.spawn_entity",
            json!({"components":{LAUNCH:{"argv":["/bin/sh","-c",program],"cwd":cwd,"history_lines":100}}}),
        )?
        .get("entity")
        .and_then(Value::as_u64)
        .ok_or("spawn returned no entity")?;
    let mut found = 0;
    s.wait("launched process running", |s| {
        found = states(s)?
            .iter()
            .find(|(e, _)| *e == entity)
            .and_then(|(_, st)| pid(st).ok())
            .unwrap_or(0);
        Ok(found > 0)
    })?;
    Ok((entity, found))
}

pub(super) fn run(s: &mut Server) -> Result<()> {
    let f = s.attach(24, 80)?;
    let v = s.frontend(f)?.viewer;
    s.wait("first shell output", |s| {
        Ok(s.frame(v, 24, 80)?.contains("DEFAULT-SHELL"))
    })?;
    let workspace = s.relation(v, "fux::model::Viewing")?;
    let shell_pid = *running(s)?.first().ok_or("no shell")?;
    s.control(v, json!({"kind":"split","axis":"horizontal","program":"stty raw -echo; printf '\\033[2J\\033[HREF-B'; exec cat > /dev/null"}))?;
    let leaf_b = s.relation(v, "fux::model::Focused")?;
    s.wait("two panes", |s| Ok(s.frame(v, 24, 80)?.contains("REF-B")))?;
    let pid_b = running(s)?
        .into_iter()
        .find(|p| *p != shell_pid)
        .ok_or("pane B pid")?;
    let pane_b = pane_of(s, leaf_b)?;
    s.control(
        v,
        json!({"kind":"save_layout","workspace":workspace,"path":"refs.scn.ron"}),
    )?;
    let saved = settled(s, v, "save")?;
    ensure(
        saved.get("error") != Some(&json!(true)),
        &format!("save failed: {saved}"),
    )?;
    let ron = fs::read_to_string(s.directory.join("refs.scn.ron"))?;

    // Case A: the referenced process is terminated after the request was
    // validated but before the file finished reading. Completion must fail
    // and must not replace the layout.
    let pipe = fifo(s, "refs-a.fifo")?;
    s.control(
        v,
        json!({"kind":"load_layout","workspace":workspace,"path":pipe,"mapping":[]}),
    )?;
    s.wait("load in flight", |s| {
        Ok(text_of(&notice(s, v)?).ends_with("..."))
    })?;
    s.control(v, json!({"kind":"close","subject":{"pane":leaf_b}}))?;
    s.wait("pane B closed and terminated", |s| {
        Ok(!leaves(s)?.contains(&leaf_b) && !alive(pid_b))
    })?;
    let before = leaves(s)?;
    let ws_before = s.query(WORKSPACE)?.len();
    fs::write(&pipe, &ron)?;
    let done = settled(s, v, "load completed after its process died")?;
    s.journal
        .record("load_after_process_closed", json!({"notice":done}))?;
    ensure(
        done.get("error") == Some(&json!(true)),
        &format!(
            "application: a load whose referenced process was terminated mid-read reported success: {done}"
        ),
    )?;
    ensure(
        leaves(s)? == before && s.query(WORKSPACE)?.len() == ws_before,
        "application: a load whose referenced process died mid-read still replaced the layout",
    )?;
    ensure(alive(shell_pid), "the shell died during case A")?;

    // Case B: a mapping whose target exits naturally during the read. The
    // target is validated as live when the request arrives, then exits on its
    // own; completion must fail without replacing the layout.
    let (short, short_pid) = spawn_launch(s, "printf 'GONE'; sleep 0.3; exit 0")?;
    let pipe = fifo(s, "refs-b.fifo")?;
    s.control(
        v,
        json!({"kind":"load_layout","workspace":workspace,"path":pipe,"mapping":[[pane_b, short]]}),
    )?;
    s.wait("mapped load in flight", |s| {
        Ok(text_of(&notice(s, v)?).ends_with("..."))
    })?;
    s.wait("mapping target exited on its own", |s| {
        Ok(!alive(short_pid)
            && states(s)?
                .iter()
                .any(|(e, st)| *e == short && st.pointer("/status/kind") == Some(&json!("exited"))))
    })?;
    let before = leaves(s)?;
    fs::write(&pipe, &ron)?;
    let done = settled(s, v, "mapped load completed after its target exited")?;
    s.journal
        .record("load_after_target_exited", json!({"notice":done}))?;
    // An exited process entity still exists with a Launch and a final status;
    // the README retains exited panes. Either outcome must be consistent: a
    // success must show the exited pane retained in the layout, a failure
    // must leave the layout untouched.
    if done.get("error") == Some(&json!(true)) {
        ensure(
            leaves(s)? == before,
            "application: a failed mapped load still replaced the layout",
        )?;
    } else {
        let now = leaves(s)?;
        ensure(
            now.len() == 2,
            &format!(
                "application: a mapped load onto an exited process produced {} views",
                now.len()
            ),
        )?;
        s.wait("exited pane retained in the loaded layout", |s| {
            Ok(s.frame(v, 24, 80)?.contains("[exit:0]"))
        })
        .map_err(|e| format!("application: a load mapped onto an exited process did not show its retained status: {e}"))?;
    }
    ensure(alive(shell_pid), "the shell died during case B")?;

    // Case C: two loads of the same workspace in flight at once, released in
    // reverse order. The first release replaces the workspace; the second
    // then names a workspace that no longer exists and must fail. Exactly one
    // replacement happens and no workspace is added.
    let workspace = s.relation(v, "fux::model::Viewing")?;
    // The earlier save still names pane B's process, terminated in case A;
    // save the current layout so both loads reference only live processes.
    s.control(
        v,
        json!({"kind":"save_layout","workspace":workspace,"path":"refs-c.scn.ron"}),
    )?;
    let saved = settled(s, v, "save for case C")?;
    ensure(
        saved.get("error") != Some(&json!(true)),
        &format!("case C save failed: {saved}"),
    )?;
    let ron = fs::read_to_string(s.directory.join("refs-c.scn.ron"))?;
    let ws_count = s.query(WORKSPACE)?.len();
    let first = fifo(s, "refs-c1.fifo")?;
    let second = fifo(s, "refs-c2.fifo")?;
    let w = s
        .rpc("fux.attach", json!({"rows":24,"cols":80}))?
        .get("viewer")
        .and_then(Value::as_u64)
        .ok_or("attach returned no viewer")?;
    s.control(
        v,
        json!({"kind":"load_layout","workspace":workspace,"path":first,"mapping":[]}),
    )?;
    s.control(
        w,
        json!({"kind":"load_layout","workspace":workspace,"path":second,"mapping":[]}),
    )?;
    s.wait("both loads in flight", |s| {
        Ok(text_of(&notice(s, v)?).ends_with("...") && text_of(&notice(s, w)?).ends_with("..."))
    })?;
    fs::write(&second, &ron)?;
    let done_second = settled(s, w, "second-issued load completed first")?;
    ensure(
        done_second.get("error") != Some(&json!(true)),
        &format!("application: the first load to complete should succeed: {done_second}"),
    )?;
    let replaced = s.relation(v, "fux::model::Viewing")?;
    ensure(
        replaced != workspace,
        "the first completed load did not replace the workspace",
    )?;
    fs::write(&first, &ron)?;
    let done_first = settled(s, v, "first-issued load completed last")?;
    s.journal.record(
        "double_load",
        json!({"first_completed":done_second,"second_completed":done_first}),
    )?;
    ensure(
        done_first.get("error") == Some(&json!(true)),
        &format!(
            "application: a load completing after another replaced its workspace reported success: {done_first}"
        ),
    )?;
    ensure(
        s.query(WORKSPACE)?.len() == ws_count,
        "application: two racing loads changed the workspace count",
    )?;
    ensure(
        s.relation(v, "fux::model::Viewing")? == replaced,
        "application: the late load replaced the workspace a second time",
    )?;
    s.control(w, json!({"kind":"detach"}))?;
    ensure(alive(shell_pid), "the shell died during case C")?;
    Ok(())
}
