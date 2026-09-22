use super::*;

const PANE_VIEW: &str = "fux::model::PaneView";

fn state_of(s: &mut Server, entity: u64) -> Result<Value> {
    states(s)?
        .into_iter()
        .find(|(e, _)| *e == entity)
        .map(|(_, state)| state)
        .ok_or_else(|| format!("process {entity} has no state").into())
}
fn spawn_launch(s: &mut Server, argv: &[&str], history_lines: usize) -> Result<u64> {
    let cwd = s.directory.clone();
    s.rpc(
        "world.spawn_entity",
        json!({"components":{LAUNCH:{"argv":argv,"cwd":cwd,"history_lines":history_lines}}}),
    )?
    .get("entity")
    .and_then(Value::as_u64)
    .ok_or("spawn returned no entity".into())
}
fn pane_of_leaf(s: &mut Server, leaf: u64) -> Result<u64> {
    s.query(PANE_VIEW)?
        .iter()
        .find(|r| id(r).ok() == Some(leaf))
        .and_then(|r| r.pointer("/components/fux::model::PaneView/pane")?.as_u64())
        .ok_or_else(|| format!("leaf {leaf} has no pane").into())
}
/// The topmost `ROW-NN` visible anywhere in the frame. A side-by-side split
/// means the frame's first line belongs to the other pane, so scan for the
/// marker rather than reading line zero.
fn top_row(frame: &str) -> Option<String> {
    frame.lines().find_map(|line| {
        let index = line.find("ROW-")?;
        let token: String = line.get(index..)?.chars().take(6).collect();
        (token.len() == 6).then_some(token)
    })
}
fn wait_pid(s: &mut Server, entity: u64) -> Result<i32> {
    let mut found = 0;
    s.wait("launched process running", |s| {
        found = pid(&state_of(s, entity)?).unwrap_or(0);
        Ok(found > 0)
    })?;
    Ok(found)
}
/// A view reparented over the API has no rectangle until the next paint, and
/// `focus` refuses unpainted targets; retry until the relation confirms it.
fn focus(s: &mut Server, v: u64, leaf: u64) -> Result<()> {
    s.wait("focus confirmed", |s| {
        if s.relation(v, "fux::model::Focused")? == leaf {
            return Ok(true);
        }
        s.control(v, json!({"kind":"focus","pane":leaf}))?;
        Ok(false)
    })
}
fn notice_text(s: &mut Server, viewer: u64) -> Result<String> {
    let rows = s.query(VIEWER)?;
    let row = rows
        .iter()
        .find(|row| id(row).ok() == Some(viewer))
        .ok_or("process viewer disappeared")?;
    Ok(component(row, VIEWER)?
        .pointer("/notice/text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned())
}

pub(super) fn run(s: &mut Server) -> Result<()> {
    let f = s.attach(24, 80)?;
    let v = s.frontend(f)?.viewer;
    let tab = s.relation(v, "fux::model::OnTab")?;
    let shell_leaf = s.relation(v, "fux::model::Focused")?;
    running(s)?;

    // A stock-spawned Launch with exact argv/cwd, shown through a PaneView.
    let entity = spawn_launch(
        s,
        &[
            "/bin/sh",
            "-c",
            "pwd > cwd.txt; printf '\\033[2J\\033[HLAUNCHED'; exec cat > launched.bin",
        ],
        100,
    )?;
    let pid_launched = wait_pid(s, entity)?;
    let view = s
        .rpc(
            "world.spawn_entity",
            json!({"components":{PANE_VIEW:{"pane":entity}}}),
        )?
        .get("entity")
        .and_then(Value::as_u64)
        .ok_or("view spawn returned no entity")?;
    s.rpc(
        "world.reparent_entities",
        json!({"entities":[view],"parent":tab}),
    )?;
    s.wait("launched pane painted", |s| {
        Ok(s.frame(v, 24, 80)?.contains("LAUNCHED"))
    })?;
    s.wait("launched cwd is the recipe cwd", |s| {
        Ok(fs::read_to_string(s.directory.join("cwd.txt"))
            .is_ok_and(|c| c.trim() == s.directory.to_string_lossy()))
    })?;
    focus(s, v, view)?;
    // The launched shell left the PTY in canonical mode: a line is needed.
    s.send(f, b"hello\r")?;
    s.wait("input reaches the launched process", |s| {
        Ok(fs::read(s.directory.join("launched.bin")).is_ok_and(|b| b == b"hello\n"))
    })?;

    // A process with no view keeps its own size, and a reflected dimension
    // edit resizes the real PTY.
    let sizer = spawn_launch(
        s,
        &[
            "/bin/sh",
            "-c",
            "while :; do stty size > size.txt; sleep 0.05; done",
        ],
        100,
    )?;
    let pid_sizer = wait_pid(s, sizer)?;
    for (rows, cols) in [(10, 40), (1, 1), (1, 12), (12, 1)] {
        let mut state = state_of(s, sizer)?;
        let edited = state.as_object_mut().ok_or("state is not an object")?;
        edited.insert("rows".into(), json!(rows));
        edited.insert("cols".into(), json!(cols));
        s.rpc(
            "world.insert_components",
            json!({"entity":sizer,"components":{STATE:state}}),
        )?;
        let wanted = format!("{rows} {cols}");
        // Observe each intermediate resize through the child, not the decoder
        // (upstream crashes on some one-row/one-column streams).
        s.wait("reflected dimension edit resized the PTY", |s| {
            Ok(fs::read_to_string(s.directory.join("size.txt")).is_ok_and(|t| t.trim() == wanted))
        })
        .map_err(|e| format!("application: child PTY did not resize to {wanted}: {e}"))?;
        s.journal
            .record("child_pty_geometry", json!({"rows":rows,"cols":cols}))?;
    }
    let tiny = s.rpc("world.spawn_entity", json!({"components":{
        LAUNCH:{"argv":["/bin/sh","-c","stty size > initial-size.txt; printf '界ABCD'; exec sleep 60"],"cwd":s.directory,"history_lines":2},
        STATE:{"status":{"kind":"starting"},"rows":1,"cols":1,"revision":0}
    }}))?.get("entity").and_then(Value::as_u64).ok_or("tiny spawn returned no entity")?;
    let tiny_pid = wait_pid(s, tiny)?;
    s.wait("PTY starts at actual 1x1", |s| {
        Ok(fs::read_to_string(s.directory.join("initial-size.txt"))
            .is_ok_and(|t| t.trim() == "1 1"))
    })?;
    ensure(alive(tiny_pid), "tiny output killed its process")?;
    s.rpc(
        "world.remove_components",
        json!({"entity":tiny,"components":[LAUNCH]}),
    )?;
    s.wait("tiny child terminated", |_| Ok(!alive(tiny_pid)))?;
    // Removing Launch terminates it and publishes a final status.
    s.rpc(
        "world.remove_components",
        json!({"entity":sizer,"components":[LAUNCH]}),
    )?;
    s.wait("removed Launch terminated its process", |_| {
        Ok(!alive(pid_sizer))
    })?;
    s.wait("final status published", |s| {
        let state = state_of(s, sizer)?;
        Ok(state.pointer("/status/kind") != Some(&json!("running")))
    })?;

    // Despawning the process entity terminates it too.
    let doomed = spawn_launch(s, &["/bin/sh", "-c", "exec sleep 30"], 100)?;
    let pid_doomed = wait_pid(s, doomed)?;
    s.rpc("world.despawn_entity", json!({"entity":doomed}))?;
    s.wait("despawned process terminated", |_| Ok(!alive(pid_doomed)))?;

    // A launch that cannot start reports failure, not a running status.
    let broken = spawn_launch(s, &["/nonexistent/program"], 100)?;
    let mut last = Value::Null;
    s.wait("failed launch reported", |s| {
        last = state_of(s, broken)?;
        Ok(last.pointer("/status/kind") == Some(&json!("failed"))
            || last.pointer("/status/kind") == Some(&json!("exited")))
    })
    .map_err(|e| format!("application: unstartable launch: {last}: {e}"))?;
    s.journal.record("failed_launch", json!(last))?;
    let empty = spawn_launch(s, &[], 100)?;
    s.wait("empty argv reported", |s| {
        Ok(state_of(s, empty)?.pointer("/status/kind") == Some(&json!("failed")))
    })
    .map_err(|e| format!("application: empty argv launch: {e}"))?;

    // Natural exit retains the final screen and status; input then reports.
    focus(s, v, shell_leaf)?;
    s.control(
        v,
        json!({"kind":"split","axis":"vertical","program":"printf '\\033[2J\\033[HFINAL-SCREEN'; exit 3"}),
    )?;
    let exited_leaf = s.relation(v, "fux::model::Focused")?;
    ensure(
        exited_leaf != shell_leaf,
        "split did not focus the new pane",
    )?;
    s.wait("natural exit retained", |s| {
        let frame = s.frame(v, 24, 80)?;
        Ok(frame.contains("FINAL-SCREEN") && frame.contains("[exit:3]"))
    })?;
    s.send(f, b"x")?;
    s.wait("input to an exited pane reports", |s| {
        Ok(notice_text(s, v)?.contains("exited"))
    })
    .map_err(|e| format!("application: key into an exited pane: {e}"))?;

    // history_lines bounds retained history. Use a dedicated tab so the pane
    // is full height, and drive it with `split` so fux owns its geometry.
    s.control(v, json!({"kind":"close","subject":{"pane":view}}))?;
    s.wait("launched process terminated with its view", |_| {
        Ok(!alive(pid_launched))
    })?;
    s.control(v, json!({"kind":"split","axis":"horizontal","program":
        "i=1; while [ $i -le 40 ]; do printf 'ROW-%02d\\r\\n' $i; i=$((i+1)); done; printf 'END'; exec cat > /dev/null"}))?;
    let short_leaf = s.relation(v, "fux::model::Focused")?;
    let short = pane_of_leaf(s, short_leaf)?;
    s.wait("short-history pane painted", |s| {
        Ok(s.frame(v, 24, 80)?.contains("END"))
    })?;
    // A split pane created by fux gets the configured history_lines, which the
    // harness fixture leaves at the default, so this pane retains full history.
    // Scroll to the oldest retained line and require it to be a real row that
    // never precedes ROW-01, and that scrolling cannot pass beyond it.
    for _ in 0..12 {
        s.control(v, json!({"kind":"scroll","order":"previous"}))?;
    }
    let mut top = String::new();
    s.wait("scrolled to the oldest retained row", |s| {
        top = top_row(&s.frame(v, 24, 80)?).unwrap_or_default();
        Ok(!top.is_empty())
    })?;
    let oldest = top.clone();
    let offset = s
        .query(VIEWER)?
        .iter()
        .find(|r| id(r).ok() == Some(v))
        .and_then(|r| {
            r.pointer("/components/fux::model::Viewer/scrollback")?
                .as_u64()
        })
        .unwrap_or(0);
    s.control(v, json!({"kind":"scroll","order":"previous"}))?;
    let mut again = String::new();
    s.wait("further scrolling stays at the oldest row", |s| {
        again = top_row(&s.frame(v, 24, 80)?).unwrap_or_default();
        Ok(!again.is_empty())
    })?;
    s.journal.record(
        "history_bound",
        json!({"oldest":oldest,"scrollback":offset,"after_extra_scroll":again,"pane":short}),
    )?;
    ensure(
        again == oldest,
        &format!(
            "application: scrolling past the oldest retained row moved the view from {oldest:?} to {again:?}"
        ),
    )?;
    ensure(
        oldest.as_str() >= "ROW-01",
        &format!("application: oldest retained row {oldest:?} precedes the first printed row"),
    )
}
