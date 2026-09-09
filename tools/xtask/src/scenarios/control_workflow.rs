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
            info["limits"]["panes"] == 128 && info["limits"]["viewers"] == 64,
            "info limits changed"
        );
        let pane = completed(&socket, json!({"command":"split","id":2,"axis":"horizontal",
            "argv":["/bin/sh","-c","printf \"%s\\n\" \"$ROLE\"; read x; printf \"got:%s\\n\" \"$x\"; exit 7"],
            "env":[["ROLE","agent-pane"]],"rows":12,"columns":50}))?["pane"].clone();
        let waited = completed(
            &socket,
            json!({"command":"wait","id":3,"pane":pane,
            "until":{"kind":"pattern","regex":"agent-pane"},"timeout_ms":10000}),
        )?;
        ensure!(waited["fired"] == "pattern", "wait pattern");
        let rows = completed(
            &socket,
            json!({"command":"capture","id":4,"pane":pane,
            "max_bytes":65536,"format":"rows","since":0}),
        )?;
        let text = rows["rows"]
            .as_array()
            .context("capture rows")?
            .iter()
            .map(|row| row["text"].as_str().unwrap_or_default())
            .collect::<Vec<_>>()
            .join("\n");
        ensure!(
            text.contains("agent-pane") && rows["since_applied"] == true,
            "incremental row capture"
        );
        // rows/columns specify the initial PTY size; main's layout may resize it after spawn.
        // The ECS spawn regression verifies the initial dimensions independently.
        let mut events = Peer::connect(&socket)?;
        events.send(&json!({"command":"subscribe","id":7,"events":["pane.closed"]}))?;
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
        "PASS workspace startup, info, env/size, pattern wait, changed rows, key notation, exit event, final evidence and shutdown"
    );
    Ok(())
}
