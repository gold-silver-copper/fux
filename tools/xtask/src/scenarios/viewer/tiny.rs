use super::*;

pub(super) fn run(binary: &Path) -> Result<()> {
    let mut h = Harness {
        root: Root::new(
            "ftiny-rs-",
            &[
                "/bin/sh".into(),
                "-c".into(),
                "stty raw -echo; printf READY; exec cat".into(),
            ],
        )?,
        binary: binary.into(),
        viewers: Vec::new(),
        finished: false,
    };
    h.add(&[])?;
    h.wait(|h| Ok(h.text(0).contains("READY")), "tiny fixture ready", 8)?;
    h.send(0, b"\x01|")?;
    h.state(|s| panes(&s[0]) == 2, "tiny side split", 8)?;
    h.send(0, b"\x01-")?;
    h.state(|s| panes(&s[0]) == 3, "tiny nested split", 8)?;
    h.wait(
        |h| Ok(h.text(0).matches("READY").count() == 3),
        "tiny children ready",
        8,
    )?;
    let before = h.tabs("default")?;
    let members = before[0]["panes"].as_array().context("tiny members")?;
    let ids: Vec<_> = members.iter().map(|pane| pane["id"].clone()).collect();
    let pids: Vec<_> = members.iter().map(|pane| pane["pid"].clone()).collect();
    let mut index = members
        .iter()
        .position(|pane| pane["focused"] == true)
        .context("tiny focus")?;
    let inspect = |h: &Harness, pane: &Value| -> Result<Value> {
        crate::support::local::until(Duration::from_secs(8), || {
            let reply = crate::support::local::rpc(
                &h.root.control(),
                serde_json::json!({
                    "command": "layout", "id": 1, "tab": 1,
                    "action": {"operation": "inspect", "pane": pane}
                }),
            )?;
            if reply["status"] == "failed"
                && reply["error"]["code"] == "conflict"
                && reply["error"]["message"] == "viewer area changed; wait for the next layout"
            {
                return Ok(None);
            }
            ensure!(
                reply["status"] == "completed",
                "tiny inspect failed: {reply}"
            );
            Ok(Some(reply["result"]["value"]["geometry"].clone()))
        })
    };
    let document = inspect(&h, &ids[0])?["document"].clone();
    for (rows, cols) in [(2, 2), (1, 1)] {
        h.resize(0, rows, cols)?;
        h.wait(
            |h| {
                let geometry = inspect(h, &ids[0])?;
                Ok(geometry["area"]["width"] == cols && geometry["area"]["height"] == rows - 1)
            },
            "tiny area applied",
            8,
        )?;
        ensure!(
            ids.iter()
                .any(|pane| inspect(&h, pane).is_ok_and(|g| g["visible_rect"].is_null())),
            "tiny layout did not collapse any pane"
        );
        for _ in 0..ids.len() {
            index = (index + 1) % ids.len();
            h.send(0, b"\x01o\x01z")?;
            h.wait(
                |h| Ok(inspect(h, &ids[index])?["zoomed"] == ids[index]),
                "tiny keyboard focus and zoom",
                8,
            )?;
            let geometry = inspect(&h, &ids[index])?;
            ensure!(
                geometry["document"] == document,
                "tiny zoom changed split tree"
            );
            if rows > 1 {
                ensure!(
                    geometry["visible_rect"] == geometry["area"],
                    "zoom did not reveal tiny pane"
                );
                let marker = b'1' + u8::try_from(index)?;
                h.send(0, &[marker])?;
                h.wait(
                    |h| Ok(h.rows(0)[0].contains(char::from(marker))),
                    "tiny pane accepts input",
                    8,
                )?;
            } else {
                ensure!(
                    geometry["visible_rect"].is_null(),
                    "zero-height area displayed content"
                );
            }
            h.send(0, b"\x01z")?;
            h.wait(
                |h| Ok(inspect(h, &ids[index])?["zoomed"].is_null()),
                "tiny unzoom",
                8,
            )?;
        }
        ensure!(!h.exited(0)?, "tiny viewer exited");
    }
    h.resize(0, 24, 80)?;
    h.wait(
        |h| Ok(inspect(h, &ids[0])?["area"]["width"] == 80),
        "restore tiny layout",
        8,
    )?;
    for (pane, pid) in ids.iter().zip(&pids) {
        let geometry = inspect(&h, pane)?;
        ensure!(
            geometry["document"] == document && !geometry["visible_rect"].is_null(),
            "restored layout lost a pane"
        );
        let state = h.tabs("default")?;
        ensure!(
            state[0]["panes"]
                .as_array()
                .context("restored members")?
                .iter()
                .any(|entry| entry["id"] == *pane && entry["pid"] == *pid),
            "tiny resize replaced process"
        );
    }
    h.send(0, b"RESTORED_OUTPUT")?;
    h.wait(
        |h| Ok(h.text(0).contains("RESTORED_OUTPUT")),
        "output after tiny restore",
        8,
    )?;
    ensure!(
        h.viewers[0]
            .screen
            .screen()
            .cell(1, 1)
            .is_some_and(|cell| cell.bgcolor() == vt100::Color::Default),
        "tiny resize left the status-bar background in pane content"
    );
    for confirm in [false, true] {
        h.send(0, b"\x1b[<2;2;2M\x1b[<2;2;2m")?;
        click_popup_entry(&mut h, "close pane")?;
        click_popup_entry(&mut h, if confirm { "Confirm close" } else { "Cancel" })?;
        h.wait(
            |h| Ok(!h.text(0).contains("Close pane")),
            "mouse pane close dialog finished",
            8,
        )?;
        h.state(
            |s| panes(&s[0]) == if confirm { 2 } else { 3 },
            "mouse pane close membership",
            8,
        )?;
    }
    for confirm in [false, true] {
        let column = h.bar(0).find("main").context("close tab label")? + 2;
        h.send(
            0,
            format!("\x1b[<2;{column};24M\x1b[<2;{column};24m").as_bytes(),
        )?;
        click_popup_entry(&mut h, "close tab")?;
        click_popup_entry(&mut h, if confirm { "Confirm close" } else { "Cancel" })?;
        if confirm {
            h.wait(|h| h.exited(0), "mouse last-tab close detaches viewer", 8)?;
            ensure!(h.success(0)?, "mouse tab close viewer failed");
        } else {
            h.wait(
                |h| Ok(!h.text(0).contains("Close tab")),
                "mouse tab close cancelled",
                8,
            )?;
            ensure!(panes(&h.tabs("default")?[0]) == 2, "cancel closed tab");
        }
    }
    h.finish()
}
