//! Two real terminal viewers of shared processes, including captured pointer gestures.

use serde_json::{Value, json};

use super::brp::Descriptor;
use super::pty::Terminal;
use super::{Fixture, Outcome, Result, TICK, WAIT, capture, err, nonce, pane_row, until};

const SMALL: (u16, u16) = (20, 100);
const LARGE: (u16, u16) = (28, 140);
const RESIZED: (u16, u16) = (24, 120);

fn number(value: &Value, field: &str) -> Result<u64> {
    value[field]
        .as_u64()
        .ok_or_else(|| err(format!("missing {field}: {value}")))
}

fn generation(fux: &Descriptor, workspace: &str, root: u64) -> Result<u64> {
    let state = fux.call("fux/workspace.list", json!({}))?;
    state["workspaces"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|row| row["name"].as_str() == Some(workspace))
        .flat_map(|row| row["roots"].as_array().into_iter().flatten())
        .find(|row| row["id"].as_u64() == Some(root))
        .and_then(|row| row["generation"].as_u64())
        .ok_or_else(|| err("layout root disappeared"))
}

fn cells(terminal: &Terminal, size: (u16, u16), text: &str) -> Result<Vec<String>> {
    until(WAIT, &format!("rendered cells contain {text}"), || {
        let screen = terminal.screen(size.0, size.1);
        Ok(screen
            .iter()
            .any(|line| line.contains(text))
            .then_some(screen))
    })
}

fn position(screen: &[String], text: &str) -> Result<(usize, usize)> {
    screen
        .iter()
        .enumerate()
        .find_map(|(row, line)| {
            line.find(text)
                .map(|byte| (line[..byte].chars().count() + 1, row + 1))
        })
        .ok_or_else(|| err(format!("no painted cell for {text}")))
}

fn click(terminal: &Terminal, screen: &[String], text: &str) -> Result<()> {
    let (col, row) = position(screen, text)?;
    terminal.send(format!("\x1b[<0;{col};{row}M\x1b[<0;{col};{row}m").as_bytes())
}

fn prefix(terminal: &Terminal, key: u8) -> Result<()> {
    terminal.send(b"\x02")?;
    std::thread::sleep(TICK * 4);
    terminal.send(&[key])
}

fn viewer(fux: &Descriptor, workspace: &str, terminal_size: (u16, u16)) -> Result<Value> {
    // viewer.list reports the content viewport, excluding the physical status row.
    // Keep PTY creation, SIGWINCH and screen decoding at the full terminal size.
    let content_rows = terminal_size.0.saturating_sub(1).max(1);
    let content_cols = terminal_size.1.max(1);
    until(WAIT, "viewer identity and content viewport", || {
        let rows = fux.call("fux/viewer.list", json!({}))?;
        Ok(rows["viewers"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|row| {
                row["workspace"].as_str() == Some(workspace)
                    && row["rows"].as_u64() == Some(u64::from(content_rows))
                    && row["cols"].as_u64() == Some(u64::from(content_cols))
            })
            .cloned())
    })
}

fn targets(fux: &Descriptor, a: u64, left: u64, b: u64, right: u64) -> Result<Value> {
    until(WAIT, "independent authoritative viewer targets", || {
        let rows = fux.call("fux/viewer.list", json!({}))?;
        let has =
            |id, pane| {
                rows["viewers"].as_array().into_iter().flatten().any(|row| {
                    row["id"].as_u64() == Some(id) && row["target"].as_u64() == Some(pane)
                })
            };
        Ok((has(a, left) && has(b, right)).then_some(rows))
    })
}

fn identity(fux: &Descriptor, pane: u64, pid: u64, node: u64) -> Result<Value> {
    let row = pane_row(fux, pane)?;
    if row["state"].as_str() != Some("live")
        || row["pid"].as_u64() != Some(pid)
        || row["node"].as_u64() != Some(node)
    {
        return Err(err(format!("shared process identity changed: {row}")));
    }
    // The kernel must still know this process, not just a stale retained server row.
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(i32::try_from(pid)?), None)?;
    Ok(row)
}

fn answer(
    fux: &Descriptor,
    terminal: &Terminal,
    pane: u64,
    other: u64,
    label: &str,
    input: &str,
    pid: u64,
) -> Result<String> {
    terminal.send(format!("{input}\r").as_bytes())?;
    let expected = format!("{label}:{input} pid={pid}");
    until(WAIT, "real process answered terminal input", || {
        let output = capture(fux, pane)?;
        if capture(fux, other)?.replace('\n', "").contains(input) {
            return Err(err(format!("input leaked into another process: {input}")));
        }
        // Capture exposes physical rows; a narrow shared pane can soft-wrap the PID.
        Ok(output
            .replace('\n', "")
            .contains(&expected)
            .then_some(output))
    })
}

pub(super) fn interaction(fixture: &mut Fixture) -> Result<Outcome> {
    let fux = fixture.local.fux()?;
    let workspace = format!("scn-layout-{}", nonce());
    let created = fux.call(
        "fux/workspace.new",
        json!({"name": workspace, "empty": true}),
    )?;
    let root = number(&created, "root")?;
    let mut terminals: Vec<Terminal> = Vec::new();
    let mut evidence = json!({"workspace": workspace, "instance": fux.instance});
    let exercised = (|| -> Result<()> {
        fux.call(
            "fux/node.patch",
            json!({"node": root, "generation": generation(&fux, &workspace, root)?,
            "patch": {"flex_direction": "row"}}),
        )?;
        let mut panes = Vec::new();
        for label in ["LEFT", "RIGHT"] {
            let script = format!(
                "stty -echo; printf '{label} pid=%s\\n' \"$$\"; while IFS= read -r line; do printf '{label}:%s pid=%s\\n' \"$line\" \"$$\"; done"
            );
            let node = fux.call("fux/node.spawn", json!({"parent": root,
                "generation": generation(&fux, &workspace, root)?,
                "patch": {"flex_grow": 1.0, "flex_basis": "0px", "min_width": "12px", "min_height": "4px", "border": {"all": "1px"}},
                "template": {"argv": ["/bin/sh", "-c", script]}}))?;
            let pane = number(&node, "pane")?;
            let row = until(WAIT, "layout process running", || {
                let row = pane_row(&fux, pane)?;
                Ok(
                    (row["state"].as_str() == Some("live") && row["pid"].as_u64().is_some())
                        .then_some(row),
                )
            })?;
            let pid = number(&row, "pid")?;
            until(WAIT, "process reports its actual PID", || {
                Ok(capture(&fux, pane)?
                    .contains(&format!("{label} pid={pid}"))
                    .then_some(()))
            })?;
            panes.push((pane, pid, number(&node, "node")?));
        }
        let (left, left_pid, left_node) = panes[0];
        let (right, right_pid, right_node) = panes[1];
        evidence["processes_before"] = json!([
            identity(&fux, left, left_pid, left_node)?,
            identity(&fux, right, right_pid, right_node)?
        ]);
        for size in [SMALL, LARGE] {
            let mut command = fixture.local.command(fixture.local.fux_binary());
            command.args([
                "attach",
                "--brp",
                fixture
                    .local
                    .fux_descriptor_path()
                    .to_str()
                    .ok_or_else(|| err("layout descriptor path"))?,
                "--workspace",
                &workspace,
            ]);
            terminals.push(Terminal::spawn(command, size.0, size.1)?);
        }
        let a = &terminals[0];
        let b = &terminals[1];
        let aid = number(&viewer(&fux, &workspace, SMALL)?, "id")?;
        let bid = number(&viewer(&fux, &workspace, LARGE)?, "id")?;
        if aid == bid {
            return Err(err("two terminal viewers share an identity"));
        }
        let screen_a = cells(a, SMALL, "RIGHT pid=")?;
        let screen_b = cells(b, LARGE, "RIGHT pid=")?;
        evidence["initial_screens"] = json!([screen_a, screen_b]);
        click(a, &screen_a, "LEFT pid=")?;
        click(b, &screen_b, "RIGHT pid=")?;
        evidence["independent_focus"] = targets(&fux, aid, left, bid, right)?;
        evidence["left_input"] = json!(answer(&fux, a, left, right, "LEFT", "FIRST-A", left_pid)?);
        evidence["right_input"] =
            json!(answer(&fux, b, right, left, "RIGHT", "FIRST-B", right_pid)?);
        // Both terminals display output from the SAME left process, although their focus differs.
        cells(a, SMALL, "LEFT:FIRST-A")?;
        cells(b, LARGE, "LEFT:FIRST-A")?;

        // Real border capture: move twice, waiting for each new generation and repaint.
        // The second movement must work after the original instance has been replaced.
        let before = cells(a, SMALL, "RIGHT pid=")?;
        let (right_col, row) = position(&before, "RIGHT pid=")?;
        let border = right_col - 1;
        let gen0 = generation(&fux, &workspace, root)?;
        a.send(format!("\x1b[<0;{border};{row}M").as_bytes())?;
        std::thread::sleep(TICK * 4);
        a.send(format!("\x1b[<32;{};{row}M", border + 4).as_bytes())?;
        let gen1 = until(WAIT, "first captured border resize", || {
            let next = generation(&fux, &workspace, root)?;
            Ok((next > gen0).then_some(next))
        })?;
        let dragged_once = until(WAIT, "first border repaint", || {
            let screen = a.screen(SMALL.0, SMALL.1);
            Ok(position(&screen, "RIGHT pid=")
                .ok()
                .is_some_and(|pos| pos.0 > right_col)
                .then_some(screen))
        })?;
        let once_col = position(&dragged_once, "RIGHT pid=")?.0;
        a.send(format!("\x1b[<32;{};{row}M", border + 9).as_bytes())?;
        until(WAIT, "captured resize survives re-instancing", || {
            Ok((generation(&fux, &workspace, root)? > gen1).then_some(()))
        })?;
        let dragged_twice = until(WAIT, "second border repaint", || {
            let screen = a.screen(SMALL.0, SMALL.1);
            Ok(position(&screen, "RIGHT pid=")
                .ok()
                .is_some_and(|pos| pos.0 > once_col)
                .then_some(screen))
        })?;
        a.send(format!("\x1b[<0;{};{row}m", border + 9).as_bytes())?;
        std::thread::sleep(TICK * 4);
        let released = generation(&fux, &workspace, root)?;
        a.send(format!("\x1b[<35;{};{row}M", border + 15).as_bytes())?;
        let settled = std::time::Instant::now();
        until(WAIT, "released gesture no longer mutates layout", || {
            if generation(&fux, &workspace, root)? != released {
                return Err(err("border capture survived mouse release"));
            }
            Ok((settled.elapsed() >= TICK * 12).then_some(()))
        })?;
        evidence["border_drag"] = json!({"before": before, "first": dragged_once, "second": dragged_twice, "generations": [gen0, gen1, released]});
        click(a, &dragged_twice, "LEFT pid=")?;
        targets(&fux, aid, left, bid, right)?;

        // A template move/reparent keeps the same leaf, PTY and PID in both viewers.
        let container = fux.call(
            "fux/node.spawn",
            json!({"parent": root,
            "generation": generation(&fux, &workspace, root)?,
            "patch": {"flex_grow": 1.0, "flex_basis": "0px", "flex_direction": "row"}}),
        )?;
        let parent = number(&container, "node")?;
        for node in [left_node, right_node] {
            fux.call(
                "fux/node.reparent",
                json!({"node": node, "parent": parent,
                "generation": generation(&fux, &workspace, root)?}),
            )?;
        }
        fux.call(
            "fux/node.reorder",
            json!({"node": left_node, "index": 1,
            "generation": generation(&fux, &workspace, root)?}),
        )?;
        for (terminal, size) in [(a, SMALL), (b, LARGE)] {
            until(WAIT, "both viewers paint reordered process leaves", || {
                let screen = terminal.screen(size.0, size.1);
                let left_pos = position(&screen, "LEFT pid=").ok();
                let right_pos = position(&screen, "RIGHT pid=").ok();
                Ok(left_pos
                    .zip(right_pos)
                    .is_some_and(|(left, right)| left.0 > right.0)
                    .then_some(()))
            })?;
        }
        evidence["after_reparent"] = json!([
            identity(&fux, left, left_pid, left_node)?,
            identity(&fux, right, right_pid, right_node)?
        ]);
        targets(&fux, aid, left, bid, right)?;
        a.resize(RESIZED.0, RESIZED.1)?;
        if number(&viewer(&fux, &workspace, RESIZED)?, "id")? != aid {
            return Err(err("terminal resize replaced viewer"));
        }
        viewer(&fux, &workspace, LARGE)?;
        // SIGWINCH paints the local viewport before the server's resized scene arrives.
        // Persistent pane text is therefore not a paint barrier: a key read against that
        // old revision may correctly be refused after the authoritative scene replaces it.
        // A new title, committed after viewer.list observed the resize, proves both
        // terminals have painted a subsequent server frame before we type the shortcut.
        let resized_title = "resize-painted";
        fux.call(
            "fux/root.rename",
            json!({"root": root, "name": resized_title}),
        )?;
        cells(a, RESIZED, resized_title)?;
        cells(b, LARGE, resized_title)?;
        evidence["after_resize"] = json!([
            cells(a, RESIZED, "LEFT pid=")?,
            cells(b, LARGE, "LEFT pid=")?
        ]);
        prefix(a, b'z')?;
        let zoomed = until(WAIT, "zoom is local to first viewer", || {
            let screen = a.screen(RESIZED.0, RESIZED.1);
            Ok((screen.iter().any(|line| line.contains("LEFT pid="))
                && !screen.iter().any(|line| line.contains("RIGHT pid=")))
            .then_some(screen))
        })?;
        let other = cells(b, LARGE, "RIGHT pid=")?;
        if !other.iter().any(|line| line.contains("LEFT pid=")) {
            return Err(err("first viewer's zoom changed the other viewer's layout"));
        }
        evidence["zoom"] = json!({"first": zoomed, "other": other, "targets": targets(&fux, aid, left, bid, right)?});
        answer(&fux, a, left, right, "LEFT", "ZOOM-A", left_pid)?;
        prefix(a, b'z')?;
        cells(a, RESIZED, "RIGHT pid=")?;

        // A modal consumes its own keys. q and the process input deliberately share one
        // terminal write: dismissal must immediately hand focus back, not use a stale popup.
        prefix(a, b'?')?;
        evidence["modal"] = json!(cells(a, RESIZED, "bindings")?);
        a.send(b"j")?;
        std::thread::sleep(TICK * 4);
        a.send(b"qAFTER-MODAL\r")?;
        until(
            WAIT,
            "modal dismissal restores live pane in same input batch",
            || {
                Ok(capture(&fux, left)?
                    .contains(&format!("LEFT:AFTER-MODAL pid={left_pid}"))
                    .then_some(()))
            },
        )?;
        if capture(&fux, right)?.contains("AFTER-MODAL") || capture(&fux, left)?.contains("LEFT:j")
        {
            return Err(err(
                "modal keys reached a process or dismissal selected wrong pane",
            ));
        }
        targets(&fux, aid, left, bid, right)?;

        // The public generic Surface contract, not an alternate renderer or a fake owner.
        // This is the minimal allowed RON scene used by the product's BRP fixture.
        let surface_node = fux.call(
            "fux/node.spawn",
            json!({"parent": root,
            "generation": generation(&fux, &workspace, root)?,
            "patch": {"width": "24px", "flex_shrink": 0.0, "border": {"all": "1px"}}}),
        )?;
        let surface = number(&surface_node, "node")?;
        fux.call(
            "fux/surface.open",
            json!({"workspace": workspace, "node": surface,
            "provider": "scenario-layout", "generation": generation(&fux, &workspace, root)?}),
        )?;
        let scene = "(resources: {}, entities: {4294967294: (components: {\"bevy_ui::ui_node::Node\": (flex_direction: Column, width: Percent(100.0), height: Percent(100.0)), \"fux::surface::Text\": (\"SURFACE-CELLS\")})})";
        fux.call("fux/surface.update", json!({"surface": surface, "expected_provider": "scenario-layout", "revision": 1, "full": true, "delta": scene}))?;
        let surface_screen = cells(a, RESIZED, "SURFACE-CELLS")?;
        cells(b, LARGE, "SURFACE-CELLS")?;
        let cursor = fux.call("fux/events.poll", json!({}))?["cursor"].clone();
        click(a, &surface_screen, "SURFACE-CELLS")?;
        std::thread::sleep(TICK * 4);
        a.send(b"g")?;
        let events = until(WAIT, "real surface mouse and keyboard owner events", || {
            let poll = fux.call("fux/events.poll", json!({"cursor": cursor}))?;
            let owned: Vec<_> = poll["events"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|entry| {
                    entry["workspace"].as_str() == Some(&workspace)
                        && entry["event"]["surface"].as_u64() == Some(surface)
                        && entry["event"]["viewer"].as_u64() == Some(aid)
                        && entry["event"]["provider"].as_str() == Some("scenario-layout")
                        && entry["event"]["revision"].as_u64() == Some(1)
                })
                .map(|entry| entry["event"].clone())
                .collect();
            let key = owned.iter().any(|event| {
                event["kind"].as_str() == Some("Key") && event["bytes"] == json!([103])
            });
            let mouse = owned
                .iter()
                .any(|event| event["kind"].as_str() == Some("Press"));
            Ok((key && mouse).then_some(owned))
        })?;
        evidence["surface"] = json!({"screen": surface_screen, "events": events});
        // The Surface key must not be buffered in either PTY. A later newline exposes leaks.
        let pane_screen = cells(a, RESIZED, "LEFT pid=")?;
        click(a, &pane_screen, "LEFT pid=")?;
        targets(&fux, aid, left, bid, right)?;
        evidence["final_left_input"] = json!(answer(
            &fux,
            a,
            left,
            right,
            "LEFT",
            "AFTER-SURFACE",
            left_pid
        )?);
        evidence["final_right_input"] = json!(answer(
            &fux,
            b,
            right,
            left,
            "RIGHT",
            "OTHER-VIEWER",
            right_pid
        )?);
        evidence["processes_after"] = json!([
            identity(&fux, left, left_pid, left_node)?,
            identity(&fux, right, right_pid, right_node)?
        ]);
        evidence["scene_after"] = fux.call("fux/scene.export", json!({"workspace": workspace}))?;
        evidence["final_screens"] = json!([
            cells(a, RESIZED, "LEFT:AFTER-SURFACE")?,
            cells(b, LARGE, "RIGHT:OTHER-VIEWER")?
        ]);
        Ok(())
    })();
    if let Err(error) = &exercised {
        evidence["error"] = json!(error.to_string());
    }
    evidence["viewers_at_cleanup"] = fux
        .call("fux/viewer.list", json!({}))
        .unwrap_or(Value::Null);
    evidence["workspace_at_cleanup"] = fux
        .call("fux/workspace.list", json!({}))
        .unwrap_or(Value::Null);
    let saved = (|| -> Result<()> {
        for (index, terminal) in terminals.iter().enumerate() {
            std::fs::write(
                fixture.artifacts.join(format!("layouts-{index}.ansi")),
                terminal.output(),
            )?;
            let sizes: &[(u16, u16)] = if index == 0 {
                &[SMALL, RESIZED]
            } else {
                &[LARGE]
            };
            for size in sizes {
                std::fs::write(
                    fixture
                        .artifacts
                        .join(format!("layouts-{index}-{}x{}-screen.txt", size.0, size.1)),
                    terminal.screen(size.0, size.1).join("\n"),
                )?;
            }
        }
        std::fs::write(
            fixture
                .artifacts
                .join("layouts-identities-and-screens.json"),
            serde_json::to_vec_pretty(&evidence)?,
        )?;
        Ok(())
    })();
    drop(terminals);
    let cleanup = fux.call("fux/workspace.kill", json!({"name": workspace}));
    exercised?;
    cleanup?;
    saved?;
    Ok(Outcome::Pass("two distinct-size terminal viewers shared live PTY/PID identities through captured two-step border dragging, reparent/reorder, SIGWINCH resize and viewer-local zoom; independent focus, modal dismissal and generic Surface mouse/key routing preserved process ownership".to_owned()))
}
