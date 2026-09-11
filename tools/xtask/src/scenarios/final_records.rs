//! Final evidence remains addressable after the last workspace retires.
use crate::support::{
    local::{Root, completed, rpc, stop_servers, until},
    process,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{fs, path::Path, time::Duration};

pub(super) fn run(binary: &Path) -> Result<()> {
    let argv = [
        "/bin/sh",
        "-c",
        "printf READY; read line; printf \"\\nFINAL_OUTPUT\\n\"; exit 17",
    ]
    .map(str::to_owned);
    let root = Root::new("ffinal-rs-", &argv)?;
    let mut server = root.server(binary)?;
    let workspace = root.control();
    let manager = root.path().join("fux/manager.sock");
    let listing = completed(&workspace, json!({"command":"list","id":1}))?;
    let instance = &listing["instance"];
    let pane = &listing["workspaces"][0]["tabs"][0]["panes"][0]["id"];
    let final_record = || -> Result<Value> {
        Ok(rpc(
            &manager,
            json!({"request":"final","instance":instance,"pane":pane}),
        )?["result"]
            .clone())
    };
    ensure!(
        final_record()?["error"]["code"] == "pending",
        "live pane final was accepted"
    );
    until(Duration::from_secs(10), || {
        let capture = completed(
            &workspace,
            json!({"command":"capture","id":1,"pane":pane,"max_bytes":131072}),
        )?;
        Ok(capture["text"]
            .as_str()
            .context("capture")?
            .contains("READY")
            .then_some(()))
    })?;
    completed(
        &workspace,
        json!({"command":"send-keys","id":1,"instance":instance,"pane":pane,"keys":"go\\n"}),
    )?;
    let result = until(Duration::from_secs(10), || {
        let r = final_record()?;
        Ok((r["status"] == "completed").then_some(r))
    })?;
    let record = &result["result"]["value"]["record"];
    ensure!(
        record["exit_status"] == 17
            && record["capture"]["text"]
                .as_str()
                .context("final capture")?
                .contains("FINAL_OUTPUT"),
        "wrong final: {record}"
    );
    ensure!(record["capture"]["truncated"] == false, "truncated final");
    ensure!(!workspace.exists(), "retired workspace socket remains");
    ensure!(
        rpc(&manager, json!({"request":"list"}))?["names"] == json!([]),
        "workspace remained listed"
    );
    ensure!(
        server.child.try_wait()?.is_none(),
        "manager exited before final retention"
    );
    // `final` is a manager primitive with no CLI subcommand; a second read is the same record.
    ensure!(
        final_record()? == result,
        "final record changed between reads"
    );
    ensure!(
        rpc(
            &manager,
            json!({"request":"final","instance":"replacement","pane":pane})
        )?["result"]["error"]["code"]
            == "conflict",
        "wrong instance accepted"
    );
    ensure!(
        rpc(
            &manager,
            json!({"request":"final","instance":instance,"pane":pane.as_u64().context("pane")? + 100})
        )?["result"]["error"]["code"]
            == "unknown",
        "never-recorded pane was not reported unknown"
    );
    let mut command = root.command(binary);
    command.args(["workspace", "new", "default"]);
    ensure!(
        process::output(command, Duration::from_secs(10), 1024 * 1024)?
            .status
            .success(),
        "workspace recreation failed"
    );
    let current = completed(&workspace, json!({"command":"list","id":1}))?["workspaces"][0].clone();
    ensure!(
        current["event_cursor"]["stream"] != record["stream"],
        "workspace reused stream"
    );
    ensure!(
        final_record()? == result,
        "workspace recreation changed old final"
    );
    completed(
        &workspace,
        json!({"command":"send-keys","id":1,"instance":instance,"pane":current["tabs"][0]["panes"][0]["id"],"keys":"go\\n"}),
    )?;
    until(Duration::from_secs(10), || {
        Ok((rpc(&manager, json!({"request":"list"}))?["names"] == json!([])).then_some(()))
    })?;
    for entry in fs::read_dir(root.path().join("fux/workspaces"))? {
        ensure!(
            entry?.path().extension() != Some(std::ffi::OsStr::new("json")),
            "retired workspace metadata remains"
        );
    }
    stop_servers(root.path())?;
    server.finish()?;
    println!(
        "PASS final screen and exit after last workspace closes, CLI, authority, recreated name"
    );
    Ok(())
}
