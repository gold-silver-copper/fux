//! Live pane transfer, source-workspace cleanup and fixed zor routing over public endpoints.
use crate::support::{
    local::{Root, completed, rpc, until},
    process,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{path::Path, time::Duration};

fn capture(socket: &Path, pane: &Value) -> Result<String> {
    let value = completed(
        socket,
        json!({"command":"capture","id":1,"pane":pane,"max_bytes":65536}),
    )?;
    Ok(value["text"].as_str().context("capture text")?.to_owned())
}

pub(super) fn run(fux: &Path, zor: Option<&Path>) -> Result<()> {
    if zor.is_none() {
        verify_transfer_ratios(fux)?;
    }
    let root = Root::new(
        "layout-rs-",
        &[
            "/bin/sh".into(),
            "-c".into(),
            "stty raw -echo; printf 'LAYOUT_PID:%s PTY:%s\n' \"$$\" \"$(tty)\"; cat".into(),
        ],
    )?;
    let mut server = root.server(fux)?;
    let manager = root.path().join("fux/manager.sock");
    let other = root.path().join("fux/other.sock");
    let result = (|| -> Result<()> {
        let listing = completed(&root.control(), json!({"command":"list","id":1}))?;
        let instance = listing["instance"].as_str().context("instance")?;
        let pane = listing["workspaces"][0]["tabs"][0]["panes"][0]["id"].clone();
        let pid = listing["workspaces"][0]["tabs"][0]["panes"][0]["pid"].clone();
        completed(
            &root.control(),
            json!({"command":"rename-pane","id":1,
            "instance":instance,"pane":pane,"name":"portable label"}),
        )?;
        let before = until(Duration::from_secs(5), || {
            let text = capture(&root.control(), &pane)?;
            Ok((text.contains("LAYOUT_PID:") && text.contains("PTY:/dev/")).then_some(text))
        })?;
        let reserved = completed(
            &root.control(),
            json!({"command":"input-reserve","id":1,"instance":instance,"pane":pane,"retain_ms":60000}),
        )?;
        let operation = reserved["receipt"]["operation"].clone();
        completed(
            &root.control(),
            json!({"command":"input-submit","id":1,"instance":instance,"operation":operation,"keys":"PROMPT_BEFORE_MOVE"}),
        )?;
        let delivered = until(Duration::from_secs(5), || {
            let status = completed(
                &root.control(),
                json!({"command":"input-status","id":1,"instance":instance,"operation":operation}),
            )?;
            Ok((status["receipt"]["state"] == "delivered").then_some(status["receipt"].clone()))
        })?;
        let unused = completed(
            &root.control(),
            json!({"command":"input-reserve","id":1,"instance":instance,"pane":pane,"retain_ms":60000}),
        )?;
        let initial_layout = completed(
            &root.control(),
            json!({"command":"layout","id":1,"tab":1,"action":{"operation":"export"}}),
        )?;
        let mut inspect = root.command(fux);
        inspect.args(["layout", "1", "inspect", &pane.to_string()]);
        let inspected = process::output(inspect, Duration::from_secs(8), 1048576)?;
        ensure!(
            inspected.status.success(),
            "inspect CLI: {}",
            String::from_utf8_lossy(&inspected.stderr)
        );
        let inspected: Value = serde_json::from_slice(&inspected.stdout)?;
        let geometry = &inspected["result"]["value"]["geometry"];
        ensure!(
            inspected["result"]["kind"] == "pane-geometry"
                && geometry["pane"] == pane
                && geometry["instance"] == instance
                && geometry["generation"] == initial_layout["generation"]
                && geometry["document"] == initial_layout["document"]
                && geometry["neighbors"] == json!({"left":null,"right":null,"up":null,"down":null})
                && geometry["edges"] == json!({"left":true,"right":true,"up":true,"down":true}),
            "unexpected pane geometry: {inspected}"
        );
        let after_inspect = completed(&root.control(), json!({"command":"list","id":1}))?;
        ensure!(
            after_inspect["workspaces"][0]["tabs"][0]["panes"][0]["pid"] == pid,
            "inspection replaced the process"
        );
        let mut unknown = root.command(fux);
        unknown.args(["layout", "1", "inspect", "4294967295"]);
        ensure!(
            !process::output(unknown, Duration::from_secs(8), 1048576)?
                .status
                .success(),
            "unknown pane inspection succeeded"
        );
        let saved = rpc(
            &root.control(),
            json!({"command":"layout","id":2,"instance":instance,
            "tab":1,"generation":initial_layout["generation"],"action":{"operation":"zoom","pane":pane}}),
        )?;
        let layout_file = root.path().join("saved-layout.json");
        std::fs::write(&layout_file, serde_json::to_vec(&saved)?)?;
        completed(
            &root.control(),
            json!({"command":"layout","id":3,"instance":instance,
            "tab":1,"generation":saved["result"]["value"]["generation"],"action":{"operation":"zoom","pane":null}}),
        )?;
        completed(
            &root.control(),
            json!({"command":"rename-pane","id":1,
            "instance":instance,"pane":pane,"name":"temporary label"}),
        )?;
        let unzoomed = completed(
            &root.control(),
            json!({"command":"layout","id":1,
            "tab":1,"action":{"operation":"export"}}),
        )?;
        let mut restore = root.command(fux);
        restore
            .args([
                "layout",
                "1",
                "--instance",
                instance,
                "--generation",
                &unzoomed["generation"].to_string(),
                "apply",
            ])
            .arg(&layout_file);
        let restored = process::output(restore, Duration::from_secs(8), 1048576)?;
        ensure!(
            restored.status.success(),
            "layout restore CLI: {}",
            String::from_utf8_lossy(&restored.stderr)
        );
        let restored: Value = serde_json::from_slice(&restored.stdout)?;
        ensure!(
            restored["result"]["value"]["zoomed"] == pane,
            "complete export lost zoom: {restored}"
        );
        ensure!(
            restored["result"]["value"]["labels"] == json!([[pane, "portable label"]]),
            "complete export lost labels: {restored}"
        );
        let mut command = root.command(fux);
        let mut export = root.command(fux);
        export.args(["workspace", "export-layout"]);
        let archive = process::output(export, Duration::from_secs(8), 1048576)?;
        ensure!(
            archive.status.success(),
            "archive CLI: {}",
            String::from_utf8_lossy(&archive.stderr)
        );
        let archive: Value = serde_json::from_slice(&archive.stdout)?;
        ensure!(
            archive["archive"]["version"] == 1
                && archive["archive"]["instance"] == instance
                && archive["archive"]["workspaces"][0]["name"] == "default"
                && archive["archive"]["workspaces"][0]["tabs"][0]["zoomed"] == pane,
            "archive omitted current layout state: {archive}"
        );
        let expected_file = root.path().join("expected-archive.json");
        let desired_file = root.path().join("desired-archive.json");
        std::fs::write(&expected_file, serde_json::to_vec(&archive)?)?;
        let mut desired = archive["archive"].clone();
        desired["workspaces"][0]["tabs"][0]["label"] = json!("restored-label");
        desired["workspaces"][0]["tabs"][0]["zoomed"] = Value::Null;
        desired["workspaces"][0]["tabs"][0]["labels"] = json!([[pane, "restored pane label"]]);
        std::fs::write(&desired_file, serde_json::to_vec(&desired)?)?;
        let mut apply = root.command(fux);
        apply
            .args(["workspace", "apply-layout"])
            .arg(&desired_file)
            .arg("--against")
            .arg(&expected_file);
        let applied = process::output(apply, Duration::from_secs(8), 1048576)?;
        ensure!(
            applied.status.success(),
            "archive apply CLI: {}",
            String::from_utf8_lossy(&applied.stderr)
        );
        let applied: Value = serde_json::from_slice(&applied.stdout)?;
        ensure!(
            applied["archive"]["workspaces"][0]["tabs"][0]["label"] == "restored-label"
                && applied["archive"]["workspaces"][0]["tabs"][0]["zoomed"].is_null(),
            "archive state not restored: {applied}"
        );
        ensure!(
            applied["archive"]["workspaces"][0]["tabs"][0]["labels"]
                == json!([[pane, "restored pane label"]]),
            "archive lost pane labels"
        );
        let layout = completed(
            &root.control(),
            json!({"command":"layout","id":4,"tab":1,"action":{"operation":"export"}}),
        )?;
        command.args([
            "transfer-pane",
            &pane.to_string(),
            "--instance",
            instance,
            "--source-tab",
            "1",
            "--generation",
            &layout["generation"].to_string(),
            "--workspace",
            "other",
            "--label",
            "moved",
        ]);
        if zor.is_some() {
            let created = rpc(&manager, json!({"request":"create","name":"other"}))?;
            ensure!(
                created["reply"] == "attach",
                "create destination: {created}"
            );
            command.args(["--stream", &created["descriptor"]["stream"].to_string()]);
        } else {
            command.arg("--new-workspace");
        }
        let moved = process::output(command, Duration::from_secs(8), 1048576)?;
        ensure!(
            moved.status.success(),
            "transfer CLI: {} {}",
            String::from_utf8_lossy(&moved.stdout),
            String::from_utf8_lossy(&moved.stderr)
        );
        let moved: Value = serde_json::from_slice(&moved.stdout)?;
        let tab = moved["result"]["result"]["value"]["tab"].clone();
        ensure!(
            tab.is_u64(),
            "transfer reply lacks destination tab: {moved}"
        );
        until(Duration::from_secs(5), || {
            Ok((!root.control().exists()).then_some(()))
        })?;
        let after = completed(&other, json!({"command":"list","id":1}))?;
        if zor.is_none() {
            ensure!(
                after["workspaces"][0]["tabs"]
                    .as_array()
                    .is_some_and(|tabs| tabs.len() == 1)
                    && after["workspaces"][0]["tabs"][0]["panes"]
                        .as_array()
                        .is_some_and(|panes| panes.len() == 1),
                "new workspace contains an unexpected replacement pane"
            );
        }
        let moved_pane = after["workspaces"][0]["tabs"]
            .as_array()
            .context("tabs")?
            .iter()
            .flat_map(|tab| tab["panes"].as_array().into_iter().flatten())
            .find(|entry| entry["id"] == pane)
            .context("moved pane missing after source cleanup")?;
        ensure!(moved_pane["pid"] == pid, "transfer replaced the process");
        ensure!(
            moved_pane["input_sequence"] == delivered["input_sequence"],
            "move invalidated delivered prompt sequence"
        );
        let status = rpc(
            &manager,
            json!({"request":"input-status","instance":instance,"pane":pane,"operation":operation}),
        )?;
        ensure!(
            status["result"]["result"]["value"]["receipt"] == delivered,
            "receipt lost after source retirement: {status}"
        );
        let failed = rpc(
            &manager,
            json!({"request":"input-status","instance":instance,"pane":pane,"operation":unused["receipt"]["operation"]}),
        )?;
        ensure!(
            failed["result"]["result"]["value"]["receipt"]["state"] == "failed"
                && failed["result"]["result"]["value"]["receipt"]["bytes_written"] == 0,
            "unused reservation survived move: {failed}"
        );

        let mut locate = root.command(fux);
        locate.args(["locate-pane", &pane.to_string(), "--instance", instance]);
        let located = process::output(locate, Duration::from_secs(8), 1048576)?;
        ensure!(
            located.status.success(),
            "locate-pane failed after source retirement"
        );
        let located: Value = serde_json::from_slice(&located.stdout)?;
        let location = &located["result"]["result"]["value"]["location"];
        ensure!(
            location["instance"] == instance
                && location["pane"] == pane
                && location["pid"] == pid
                && location["workspace"] == "other"
                && location["stream"] == after["workspaces"][0]["event_cursor"]["stream"]
                && location["origin_workspace"] == listing["workspaces"][0]["name"]
                && location["origin_stream"] == listing["workspaces"][0]["event_cursor"]["stream"],
            "locate-pane lost current routing or launch attribution: {location}"
        );
        ensure!(
            moved_pane["label"] == "restored pane label",
            "layout restore or transfer lost manual label"
        );
        ensure!(
            capture(&other, &pane)?.contains(before.trim()),
            "transfer lost PID/PTY output"
        );
        completed(
            &other,
            json!({"command":"send-keys","id":1,"pane":pane,"keys":"OUTPUT_AFTER_MOVE"}),
        )?;
        until(Duration::from_secs(5), || {
            Ok(capture(&other, &pane)?
                .contains("OUTPUT_AFTER_MOVE")
                .then_some(()))
        })?;
        if let Some(zor) = zor {
            let task = |args: &[&str]| -> Result<Value> {
                let mut command = root.command(zor);
                command.arg("task").args(args);
                let output = process::output(command, Duration::from_secs(8), 1048576)?;
                ensure!(
                    output.status.success(),
                    "zor {args:?}: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
                Ok(serde_json::from_slice(&output.stdout)?)
            };
            let adopted = task(&[
                "adopt",
                "layout-adopted",
                "--title",
                "layout fixture",
                "--instance",
                instance,
                "--workspace",
                "other",
                "--pane",
                &pane.to_string(),
            ])?;
            ensure!(
                adopted["session"]["target"]["pane"] == pane,
                "adoption changed pane"
            );
            let managed = task(&[
                "start",
                "layout-managed",
                "--title",
                "managed layout fixture",
                "--instance",
                instance,
                "--workspace",
                "other",
                "--cwd",
                root.path().to_str().context("fixture path")?,
                "--",
                "/bin/cat",
            ])?;
            let managed_pane = managed["session"]["target"]["pane"].clone();
            ensure!(managed_pane.is_u64(), "managed target missing: {managed}");
            let return_ws = rpc(&manager, json!({"request":"create","name":"return"}))?;
            let listing = completed(&other, json!({"command":"list","id":1}))?;
            {
                let protected = &managed_pane;
                let owner = listing["workspaces"][0]["tabs"]
                    .as_array()
                    .context("tabs")?
                    .iter()
                    .find(|tab| {
                        tab["panes"].as_array().is_some_and(|panes| {
                            panes.iter().any(|entry| &entry["id"] == protected)
                        })
                    })
                    .context("protected pane tab")?;
                let entry = owner["panes"]
                    .as_array()
                    .context("panes")?
                    .iter()
                    .find(|entry| &entry["id"] == protected)
                    .context("pane")?;
                ensure!(
                    entry["fixed_workspace"] == false,
                    "durable managed launch remained pinned"
                );
                let layout = completed(
                    &other,
                    json!({"command":"layout","id":1,"tab":owner["id"],"action":{"operation":"export"}}),
                )?;
                let moved_managed = rpc(
                    &manager,
                    json!({"request":"transfer","transfer":{
                        "instance":instance,"source":owner["id"],"generation":layout["generation"],"pane":protected,
                        "workspace":{"kind":"existing","name":"return","stream":return_ws["descriptor"]["stream"]},
                        "destination":{"kind":"new-tab","label":null},"side":"right"
                    }}),
                )?;
                ensure!(
                    moved_managed["result"]["status"] == "completed",
                    "managed pane could not move: {moved_managed}"
                );
            }
            let owner = listing["workspaces"][0]["tabs"]
                .as_array()
                .context("tabs")?
                .iter()
                .find(|tab| {
                    tab["panes"]
                        .as_array()
                        .is_some_and(|panes| panes.iter().any(|entry| entry["id"] == pane))
                })
                .context("adopted pane tab")?;
            let layout = completed(
                &other,
                json!({"command":"layout","id":1,"tab":owner["id"],"action":{"operation":"export"}}),
            )?;
            let moved = rpc(
                &manager,
                json!({"request":"transfer","transfer":{
                    "instance":instance,"source":owner["id"],"generation":layout["generation"],"pane":pane,
                    "workspace":{"kind":"existing","name":"return","stream":return_ws["descriptor"]["stream"]},
                    "destination":{"kind":"new-tab","label":null},"side":"right"
                }}),
            )?;
            ensure!(
                moved["result"]["status"] == "completed",
                "adopted pane could not move: {moved}"
            );
            rpc(&manager, json!({"request":"kill","name":"other"}))?;
            until(Duration::from_secs(5), || {
                Ok((!other.exists()).then_some(()))
            })?;
            task(&[
                "prepare",
                "layout-managed",
                "--operation",
                "managed-after-move",
                "--text",
                "MANAGED_AFTER_MOVE",
            ])?;
            task(&["submit", "managed-after-move"])?;
            until(Duration::from_secs(5), || {
                Ok(
                    (task(&["reconcile", "managed-after-move"])?["delivery"] == "delivered")
                        .then_some(()),
                )
            })?;
            let managed_destination = root.path().join("fux/return.sock");
            until(Duration::from_secs(5), || {
                Ok(capture(&managed_destination, &managed_pane)?
                    .contains("MANAGED_AFTER_MOVE")
                    .then_some(()))
            })?;
            task(&["launch-reconcile", "layout-managed"])?;
            let mut stop = root.command(zor);
            stop.args(["task", "stop", "layout-managed"]);
            let stopped = process::output(stop, Duration::from_secs(8), 1048576)?;
            ensure!(
                stopped.status.success()
                    || String::from_utf8_lossy(&stopped.stderr)
                        .contains("stop requested; pane release is not yet confirmed"),
                "moved managed stop failed: {}",
                String::from_utf8_lossy(&stopped.stderr)
            );
            until(Duration::from_secs(5), || {
                let mut reconcile = root.command(zor);
                reconcile.args(["task", "launch-reconcile", "layout-managed"]);
                let output = process::output(reconcile, Duration::from_secs(8), 1048576)?;
                if !output.status.success() {
                    ensure!(
                        String::from_utf8_lossy(&output.stderr).contains("pane is still live"),
                        "moved launch reconciliation failed: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                    return Ok(None);
                }
                let observed: Value = serde_json::from_slice(&output.stdout)?;
                ensure!(
                    observed["session"]["target"] == managed["session"]["target"],
                    "stop changed process identity"
                );
                Ok((observed["launch"]["phase"] == "closed").then_some(()))
            })?;
            task(&[
                "prepare",
                "layout-adopted",
                "--operation",
                "after-move",
                "--text",
                "ADOPTED_AFTER_MOVE",
            ])?;
            task(&["submit", "after-move"])?;
            until(Duration::from_secs(5), || {
                Ok((task(&["reconcile", "after-move"])?["delivery"] == "delivered").then_some(()))
            })?;
            let destination = root.path().join("fux/return.sock");
            until(Duration::from_secs(5), || {
                Ok(capture(&destination, &pane)?
                    .contains("ADOPTED_AFTER_MOVE")
                    .then_some(()))
            })?;
            completed(
                &destination,
                json!({"command":"kill","id":1,"instance":instance,"pane":pane}),
            )?;
            until(Duration::from_secs(5), || {
                let waited = task(&["wait", "after-move"])?;
                Ok((waited["wait"] == "process-exited").then_some(()))
            })?;
            let inspected = task(&["inspect", "layout-adopted"])?;
            ensure!(
                inspected["session"]["target"] == adopted["session"]["target"],
                "movement rewrote immutable adoption identity"
            );
            ensure!(
                task(&["inspect", "layout-managed"])?["session"]["target"]
                    == managed["session"]["target"],
                "managed route changed"
            );
        }
        Ok(())
    })();
    let stopped = server.finish();
    result?;
    stopped
}

fn verify_transfer_ratios(fux: &Path) -> Result<()> {
    let root = Root::new("transfer-ratio-", &["/bin/sh".into()])?;
    let mut server = root.server(fux)?;
    let result = (|| -> Result<()> {
        let before = completed(&root.control(), json!({"command":"list","id":1}))?;
        let instance = before["instance"].as_str().context("instance")?;
        let pid = before["workspaces"][0]["tabs"][0]["panes"][0]["pid"].clone();
        let source = completed(
            &root.control(),
            json!({"command":"layout","id":1,"tab":1,"action":{"operation":"export"}}),
        )?;
        let created = rpc(
            &root.path().join("fux/manager.sock"),
            json!({"request":"create","name":"other"}),
        )?;
        let other = root.path().join("fux/other.sock");
        let dest = completed(
            &other,
            json!({"command":"layout","id":1,"tab":2,"action":{"operation":"export"}}),
        )?;
        completed(
            &other,
            json!({"command":"layout","id":1,"instance":instance,"tab":2,"generation":dest["generation"],"action":{"operation":"zoom","pane":2}}),
        )?;
        let dest = completed(
            &other,
            json!({"command":"layout","id":1,"tab":2,"action":{"operation":"export"}}),
        )?;
        let mut command = root.command(fux);
        command.args([
            "transfer-pane",
            "1",
            "--instance",
            instance,
            "--source-tab",
            "1",
            "--generation",
            &source["generation"].to_string(),
            "--workspace",
            "other",
            "--stream",
            &created["descriptor"]["stream"].to_string(),
            "--destination-tab",
            "2",
            "--destination-generation",
            &dest["generation"].to_string(),
            "--target",
            "2",
            "--side",
            "left",
            "--ratio",
            "7000",
            "--no-focus",
        ]);
        let reply = process::output(command, Duration::from_secs(8), 1048576)?;
        ensure!(
            reply.status.success(),
            "ratio transfer CLI: {}",
            String::from_utf8_lossy(&reply.stderr)
        );
        let reply: Value = serde_json::from_slice(&reply.stdout)?;
        let value = &reply["result"]["result"]["value"];
        ensure!(
            value["document"]["nodes"][0]["ratio"] == 3000,
            "left insertion ratio: {reply}"
        );
        ensure!(
            value["zoomed"] == 2,
            "no-focus transfer cleared destination zoom: {reply}"
        );
        let tab = completed(
            &other,
            json!({"command":"tab","id":1,"action":{"new":{"name":"target"}}}),
        )?;
        let dest = completed(
            &other,
            json!({"command":"layout","id":1,"tab":tab["tab"],"action":{"operation":"export"}}),
        )?;
        let source = completed(
            &other,
            json!({"command":"layout","id":1,"tab":2,"action":{"operation":"export"}}),
        )?;
        let mut command = root.command(fux);
        command.args([
            "other",
            "layout",
            "2",
            "--instance",
            instance,
            "--generation",
            &source["generation"].to_string(),
            "to-tab",
            "1",
            "3",
            &dest["generation"].to_string(),
            "3",
            "down",
            "--ratio",
            "6500",
            "--focus",
        ]);
        let reply = process::output(command, Duration::from_secs(8), 1048576)?;
        ensure!(
            reply.status.success(),
            "tab ratio CLI: {}",
            String::from_utf8_lossy(&reply.stderr)
        );
        let reply: Value = serde_json::from_slice(&reply.stdout)?;
        ensure!(
            reply["result"]["value"]["document"]["nodes"][0]["ratio"] == 6500,
            "down insertion ratio: {reply}"
        );
        let listing = completed(&other, json!({"command":"list","id":1}))?;
        let selected = listing["workspaces"][0]["tabs"]
            .as_array()
            .context("tabs")?
            .iter()
            .find(|tab| tab["focused"] == true)
            .context("selected tab")?;
        ensure!(
            selected["id"] == 3
                && selected["panes"]
                    .as_array()
                    .context("panes")?
                    .iter()
                    .any(|pane| pane["id"] == 1 && pane["focused"] == true),
            "focused transfer did not select destination pane: {listing}"
        );
        let moved = listing["workspaces"][0]["tabs"]
            .as_array()
            .context("tabs")?
            .iter()
            .flat_map(|tab| tab["panes"].as_array().into_iter().flatten())
            .find(|pane| pane["id"] == 1)
            .context("moved pane")?;
        ensure!(moved["pid"] == pid, "ratio transfer changed PID");
        Ok(())
    })();
    let stopped = server.finish();
    result?;
    stopped
}
