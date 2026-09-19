//! The default dashboard CLI, real terminal input, viewer-local scrolling and exact handoff.

use serde_json::{Value, json};

use super::brp::Descriptor;
use super::pty::Terminal;
use super::{
    Fixture, MACHINE, Outcome, Result, TARGET, WAIT, capture, err, exact_viewers, first_pane,
    live_attempt, nonce, require_target, type_and_observe, until,
};

const ROWS: u16 = 10;
const COLS: u16 = 120;

fn ready(zor: &Descriptor, id: &Value) -> Result<Value> {
    until(WAIT, "dashboard acknowledged projection", || {
        let state = zor.call("zor/dashboard.state", json!({"id": id}))?;
        Ok((state["status"].as_str() == Some("open")
            && state["revision"]
                .as_u64()
                .is_some_and(|revision| revision > 0))
        .then_some(state))
    })
}

fn painted(terminal: &Terminal, task: &str, selected: bool) -> Result<Vec<String>> {
    until(WAIT, "dashboard row rendered in terminal cells", || {
        let screen = terminal.screen(ROWS, COLS);
        Ok(screen
            .iter()
            .any(|line| line.contains(task) && (!selected || line.trim_start().starts_with('>')))
            .then_some(screen))
    })
}

fn task_lines(terminal: &Terminal) -> Vec<String> {
    terminal
        .screen(ROWS, COLS)
        .into_iter()
        .filter(|line| line.contains("scn-"))
        .collect()
}

pub(super) fn interaction(fixture: &mut Fixture) -> Result<Outcome> {
    let target = require_target(fixture)?;
    let fux = fixture.local.fux()?;
    let zor = fixture.local.zor()?;
    let remote_fux = fixture.remote.fux()?;
    let remote_zor = fixture.remote.zor()?;
    let local_pane = first_pane(&fux)?;
    // More real task rows than fit in either terminal. No extra processes are launched.
    let cwd = fixture.remote.path().join("home");
    for index in 0..16 {
        fixture.remote.zor_run(&[
            "task",
            "create",
            &format!("scn-dashboard-{index:02}"),
            "--title",
            "dashboard scroll row",
            "--cwd",
            cwd.to_str().ok_or_else(|| err("dashboard cwd"))?,
        ])?;
    }
    until(WAIT, "Remote dashboard rows observed by Local", || {
        let rows = zor.call("zor/dashboard.rows", json!({"machine": MACHINE}))?;
        Ok(rows["rows"]
            .as_array()
            .is_some_and(|rows| {
                rows.iter()
                    .filter(|row| {
                        row["target"]["task"]
                            .as_str()
                            .is_some_and(|task| task.starts_with("scn-dashboard-"))
                    })
                    .count()
                    == 16
            })
            .then_some(()))
    })?;
    let cursor = fux.call("fux/events.poll", json!({}))?["cursor"].clone();
    let mut command = fixture.local.command(fixture.local.zor_binary());
    command.args(["--machine", MACHINE, "dashboard"]);
    let mut terminal = Terminal::spawn(command, ROWS, COLS)?;
    let mut dashboard_id = None;
    let mut observer = None;
    let mut evidence = json!({
        "local_fux_instance": fux.instance, "local_zor_instance": zor.instance,
        "remote_fux_instance": remote_fux.instance, "remote_zor_instance": remote_zor.instance,
        "remote_target": target, "local_pane": local_pane,
    });
    let exercised = (|| -> Result<()> {
        let (viewer, workspace) = until(WAIT, "default dashboard CLI viewer", || {
            if !terminal.running()? {
                return Err(err("default dashboard CLI exited"));
            }
            let viewers = fux.call("fux/viewer.list", json!({}))?;
            Ok(viewers["viewers"]
                .as_array()
                .into_iter()
                .flatten()
                .find_map(|viewer| {
                    let workspace = viewer["workspace"].as_str()?;
                    workspace
                        .starts_with("dashboard-")
                        .then(|| (viewer["id"].as_u64(), workspace.to_owned()))
                        .and_then(|(id, workspace)| id.map(|id| (id, workspace)))
                }))
        })?;
        let workspaces = fux.call("fux/workspace.list", json!({}))?;
        let created = workspaces["workspaces"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|row| row["name"].as_str() == Some(&workspace))
            .ok_or_else(|| err("default dashboard workspace"))?;
        if created["roots"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|root| {
                root["panes"]
                    .as_array()
                    .is_some_and(|panes| !panes.is_empty())
            })
        {
            return Err(err("default dashboard unexpectedly spawned a shell pane"));
        }
        let first_screen = painted(&terminal, "scn-dashboard-", false)?;
        // Discover the session with the first meaningful selection, not a no-op k: even
        // Previous at row zero queues a guarded scroll, so a following j could arrive while
        // that unacknowledged input is pending. No input is retried.
        terminal.send(b"j")?;
        let event = until(WAIT, "painted keyboard SurfaceInput event", || {
            let events = fux.call("fux/events.poll", json!({"cursor": cursor}))?;
            Ok(events["events"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|entry| {
                    entry["workspace"].as_str() == Some(&workspace)
                        && entry["event"]["viewer"].as_u64() == Some(viewer)
                        && entry["event"]["kind"].as_str() == Some("Key")
                })
                .map(|entry| entry["event"].clone()))
        })?;
        let id: u64 = event["provider"]
            .as_str()
            .and_then(|provider| provider.rsplit(':').next())
            .ok_or_else(|| err("dashboard provider identity"))?
            .parse()?;
        let id = json!(id);
        dashboard_id = Some(id.clone());
        evidence["keyboard_event"] = event;
        evidence["workspace"] = json!(workspace);
        evidence["viewer"] = json!(viewer);
        let initial = ready(&zor, &id)?;
        let rows = initial["rows"]
            .as_array()
            .ok_or_else(|| err("dashboard rows"))?;
        let first = rows.first().ok_or_else(|| err("first dashboard row"))?;
        let second = rows.get(1).ok_or_else(|| err("second dashboard row"))?;
        let first_task = first["target"]["task"]
            .as_str()
            .ok_or_else(|| err("first row task"))?;
        let second_task = second["target"]["task"]
            .as_str()
            .ok_or_else(|| err("second row task"))?;
        if !first_screen
            .iter()
            .any(|line| line.contains(first_task) && line.trim_start().starts_with('>'))
        {
            return Err(err(
                "dashboard did not initially paint its first selected row",
            ));
        }
        until(WAIT, "keyboard selects next dashboard row", || {
            let state = ready(&zor, &id)?;
            Ok((state["selected"] == second["key"]).then_some(()))
        })?;
        painted(&terminal, second_task, true)?;
        evidence["keyboard_selected_screen"] = json!(terminal.screen(ROWS, COLS));
        // k returns the viewport to its first row. Then click the observed second row's
        // actual terminal cell, proving pointer routing independently of keyboard selection.
        terminal.send(b"k")?;
        until(WAIT, "keyboard returns to first dashboard row", || {
            let state = ready(&zor, &id)?;
            Ok((state["selected"] == first["key"]).then_some(()))
        })?;
        let screen = painted(&terminal, first_task, true)?;
        let y = screen
            .iter()
            .position(|line| line.contains(second_task))
            .ok_or_else(|| err("visible mouse target"))?
            + 1;
        terminal.send(format!("\x1b[<0;5;{y}M\x1b[<0;5;{y}m").as_bytes())?;
        until(WAIT, "mouse selects rendered dashboard row", || {
            let state = ready(&zor, &id)?;
            Ok((state["selected"] == second["key"]).then_some(()))
        })?;
        evidence["mouse_selected_screen"] = json!(painted(&terminal, second_task, true)?);
        let mut command = fixture.local.command(fixture.local.fux_binary());
        command.args([
            "attach",
            "--brp",
            fixture
                .local
                .fux_descriptor_path()
                .to_str()
                .ok_or_else(|| err("descriptor path"))?,
            "--workspace",
            &workspace,
        ]);
        observer = Some(Terminal::spawn(command, ROWS, COLS)?);
        let other = observer.as_ref().unwrap();
        painted(other, first_task, false)?;
        evidence["independent_viewers"] = fux.call("fux/viewer.list", json!({}))?;
        let other_before = task_lines(other);
        let before = task_lines(&terminal);
        // Wheel input is local to the fux viewer; it must not move the other viewer's list.
        terminal.send(b"\x1b[<65;5;3M")?;
        let after = until(
            WAIT,
            "first viewer locally scrolls its rendered rows",
            || {
                let after = task_lines(&terminal);
                Ok((!after.is_empty()
                    && after != before
                    && !after.iter().any(|line| line.contains(first_task)))
                .then_some(after))
            },
        )?;
        let settled = std::time::Instant::now();
        until(
            WAIT,
            "independent viewer keeps its own scroll offset",
            || {
                if task_lines(other) != other_before {
                    return Err(err("scroll moved the independent dashboard viewer"));
                }
                Ok((settled.elapsed() >= super::TICK * 12).then_some(()))
            },
        )?;
        let scrolled = ready(&zor, &id)?;
        if scrolled["selected"] != second["key"]
            || !scrolled["handoff"].is_null()
            || live_attempt(fixture, TARGET)? != Some(target)
        {
            return Err(err(
                "viewer-local scroll changed selection, handoff or Remote ownership",
            ));
        }
        evidence["scroll"] = json!({"before": before, "after": after, "other_before": other_before, "other_after": task_lines(other)});
        let target_index = rows
            .iter()
            .position(|row| row["target"]["task"].as_str() == Some(TARGET))
            .ok_or_else(|| err("dashboard target row"))?;
        let mut current = 1;
        while current != target_index {
            let next = if current < target_index {
                current + 1
            } else {
                current - 1
            };
            terminal.send(if current < target_index { b"j" } else { b"k" })?;
            until(WAIT, "terminal keyboard selects exact task key", || {
                let state = ready(&zor, &id)?;
                Ok((state["selected"] == rows[next]["key"]).then_some(()))
            })?;
            painted(
                &terminal,
                rows[next]["target"]["task"]
                    .as_str()
                    .ok_or_else(|| err("task row"))?,
                true,
            )?;
            current = next;
        }
        let selected = ready(&zor, &id)?;
        evidence["selected_screen"] = json!(painted(&terminal, TARGET, true)?);
        if zor
            .call(
                "zor/dashboard.input",
                json!({"id": id, "viewer": viewer,
            "revision": 0, "input": {"kind": "attach"}}),
            )
            .is_ok()
        {
            return Err(err("stale dashboard activation was accepted"));
        }
        let before = exact_viewers(&remote_fux)?;
        terminal.send(b"\r")?;
        let handoff = until(WAIT, "terminal Enter creates guarded exact handoff", || {
            let state = zor.call("zor/dashboard.state", json!({"id": id}))?;
            Ok((!state["handoff"].is_null()).then_some(state["handoff"].clone()))
        })?;
        let exact = &handoff["target"];
        if exact != &rows[target_index]["target"]
            || exact["pane"]["pane"].as_u64() != Some(target.0)
            || exact["pane"]["pid"].as_u64() != Some(u64::from(target.1))
            || exact["instance"].as_str() != Some(remote_zor.instance.as_str())
            || exact["pane"]["instance"].as_str() != Some(remote_fux.instance.as_str())
        {
            return Err(format!("dashboard handed off wrong identity: {handoff}").into());
        }
        evidence["selected"] = selected;
        evidence["handoff"] = handoff;
        until(WAIT, "default CLI consumes exact handoff", || {
            if !terminal.running()? {
                return Err(err("dashboard CLI exited during handoff"));
            }
            Ok((exact_viewers(&remote_fux)? > before).then_some(()))
        })?;
        let marker = format!("DASHBOARD-{}", nonce());
        type_and_observe(fixture, &terminal, target.0, &marker)?;
        evidence["attached_screen"] = json!(painted(&terminal, &marker, false)?);
        if capture(&fux, local_pane)?.contains(&marker) {
            return Err(err("dashboard handoff input reached Local pane"));
        }
        if let Some(pane) = fixture.decoy_pane
            && capture(&remote_fux, pane)?.contains(&marker)
        {
            return Err(err("dashboard handoff input reached Remote decoy pane"));
        }
        terminal.send(b"\x02")?;
        // Prefix and binding are separate terminal events, with a frame between them.
        std::thread::sleep(super::TICK * 4);
        terminal.send(b"d")?;
        until(
            WAIT,
            "default CLI retires handoff and returns to dashboard",
            || {
                let state = ready(&zor, &id)?;
                Ok((state["handoff"].is_null()
                    && state["selected"] == rows[target_index]["key"]
                    && exact_viewers(&remote_fux)? == before)
                    .then_some(()))
            },
        )?;
        // A newly attached viewer starts with its own unscrolled viewport, while the
        // provider retains the exact selected task key.
        evidence["returned_screen"] = json!(painted(&terminal, first_task, false)?);
        terminal.send(b"q")?;
        until(WAIT, "terminal q closes dashboard CLI", || {
            Ok((!terminal.running()?).then_some(()))
        })?;
        until(WAIT, "hosted dashboard surface closed", || {
            let state = zor.call("zor/dashboard.state", json!({"id": id}))?;
            Ok((state["status"].as_str() == Some("closed")).then_some(()))
        })?;
        if live_attempt(fixture, TARGET)? != Some(target) {
            return Err(err("closing dashboard changed Remote ownership"));
        }
        evidence["target_after_close"] = json!(live_attempt(fixture, TARGET)?);
        Ok(())
    })();
    // Capture diagnostics before any cleanup; close is attempted even after failed input.
    let saved = std::fs::write(fixture.artifacts.join("dashboard.ansi"), terminal.output());
    let cells = std::fs::write(
        fixture.artifacts.join("dashboard-screen.txt"),
        terminal.screen(ROWS, COLS).join("\n"),
    );
    let saved_evidence = std::fs::write(
        fixture.artifacts.join("dashboard-handoff.json"),
        serde_json::to_vec_pretty(&evidence)?,
    );
    let closed = dashboard_id
        .map(|id| zor.call("zor/dashboard.close", json!({"id": id})))
        .transpose();
    if terminal.running().unwrap_or(false) {
        // The CLI owns Session's close-on-drop. Give its installed signal handler a chance
        // to run even if a broken input bridge prevented discovery of the session id.
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(terminal.pid() as i32),
            nix::sys::signal::Signal::SIGTERM,
        );
        let _ = until(WAIT, "dashboard parent cleanup", || {
            Ok((!terminal.running()?).then_some(()))
        });
    }
    drop(observer);
    drop(terminal);
    exercised?;
    closed?;
    saved?;
    cells?;
    saved_evidence?;
    Ok(Outcome::Pass(format!(
        "default dashboard CLI created no shell; terminal keyboard and mouse selected painted rows; wheel scrolled one of two viewers only; stale activation refused; Enter attached only {TARGET} pane {} pid {}, prefix-d returned and q closed without changing Remote ownership",
        target.0, target.1,
    )))
}
