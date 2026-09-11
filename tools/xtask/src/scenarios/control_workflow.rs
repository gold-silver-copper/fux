//! Preserve main's complete headless control workflow while checking retained final evidence.
use crate::support::{
    control::Peer,
    local::{Root, completed, rpc, stop_servers, until},
    process,
};
use anyhow::{Context, Result, ensure};
use serde_json::json;
use std::{path::Path, time::Duration};

pub(super) fn run(binary: &Path) -> Result<()> {
    let root = Root::new("fcontrol-rs-", &["/bin/sh".into()])?;
    let socket = root.path().join("fux/agent.sock");
    let manager = root.path().join("fux/manager.sock");
    let result = (|| {
        let mut command = root.command(binary);
        command.args(["workspace", "new", "agent"]);
        let created = process::output(command, Duration::from_secs(20), 1024 * 1024)?;
        ensure!(
            created.status.success(),
            "workspace startup: {}",
            String::from_utf8_lossy(&created.stderr)
        );
        until(Duration::from_secs(10), || {
            Ok(socket.exists().then_some(()))
        })?;
        let info = completed(&socket, json!({"command":"info","id":1}))?["info"].clone();
        ensure!(
            info["limits"]["capture_bytes"] == 131072
                && info["limits"]["key_bytes"] == 65536
                && info["limits"]["frame_bytes"] == 1_048_576
                && info["limits"]["scrollback_lines"].is_u64(),
            "info request bounds changed: {info}"
        );
        let pane = completed(&socket, json!({"command":"split","id":2,"axis":"horizontal",
            "argv":["/bin/sh","-c","printf \"%s\\n\" \"$ROLE\"; read x; printf \"got:%s\\n\" \"$x\"; exit 7"],
            "env":[["ROLE","agent-pane"]],"rows":12,"columns":50}))?["pane"].clone();
        // The cells capture carries the environment the command printed, row by row.
        let seen = completed(&socket, json!({"command":"list","id":3}))?["workspaces"][0]["tabs"]
            [0]["panes"]
            .as_array()
            .context("panes")?
            .iter()
            .find(|summary| summary["id"] == pane)
            .map(|summary| summary["seq"].clone())
            .context("split pane listed")?;
        ensure!(seen.is_u64(), "listed pane carries an output sequence");
        until(Duration::from_secs(10), || {
            let cells = completed(
                &socket,
                json!({"command":"capture","id":5,"pane":pane,"max_bytes":65536,"format":"cells"}),
            )?;
            let text = cells["lines"]
                .as_array()
                .context("capture lines")?
                .iter()
                .map(|line| {
                    line["cells"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|cell| cell["text"].as_str())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n");
            Ok(text.contains("agent-pane").then_some(()))
        })?;
        // rows/columns specify the initial PTY size; main's layout may resize it after spawn.
        // The ECS spawn regression verifies the initial dimensions independently.
        let mut events = Peer::connect(&socket)?;
        events.send(&json!({"command":"subscribe","id":7}))?;
        ensure!(events.read()?["status"] == "accepted", "subscription");
        completed(
            &socket,
            json!({"command":"send-keys","id":6,"pane":pane,"keys":"o k Enter","notation":"keys"}),
        )?;
        loop {
            let event = events.read()?;
            if event["event"] == "pane.closed" && event["pane"] == pane {
                ensure!(event["exit_status"] == 7, "observed exit status");
                break;
            }
        }
        drop(events);
        let mut command = root.command(binary);
        command.args(["workspace", "kill", "agent"]);
        ensure!(
            process::output(command, Duration::from_secs(10), 1024 * 1024)?
                .status
                .success(),
            "workspace kill"
        );
        until(Duration::from_secs(5), || {
            Ok((!socket.exists()).then_some(()))
        })?;
        ensure!(
            rpc(&manager, json!({"request":"list"}))?["names"] == json!([]),
            "retired workspace listed"
        );
        let final_reply = rpc(
            &manager,
            json!({"request":"final","instance":info["instance_nonce"],"pane":pane}),
        )?;
        let record = &final_reply["result"]["result"]["value"]["record"];
        ensure!(
            record["exit_status"] == 7
                && record["capture"]["text"]
                    .as_str()
                    .context("final text")?
                    .contains("got:ok"),
            "final output and status after socket retirement"
        );
        ensure!(manager.exists(), "manager did not retain evidence");
        Ok(())
    })();
    let cleanup = stop_servers(root.path());
    result.and(cleanup)?;
    ensure!(
        !manager.exists(),
        "explicit shutdown left the manager socket"
    );
    println!(
        "PASS workspace startup, info, env/size, seq wait, cells capture, key notation, exit event, final evidence and shutdown"
    );
    Ok(())
}
