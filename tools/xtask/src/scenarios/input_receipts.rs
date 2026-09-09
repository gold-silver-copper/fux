//! Disconnected local replies, duplicate suppression and stalled PTY backpressure.
use crate::support::{
    control::Peer,
    local::{Root, rpc, until},
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{path::Path, time::Duration};

pub(super) fn run(binary: &Path) -> Result<()> {
    let argv = [
        std::env::current_exe()?.to_string_lossy().into_owned(),
        "fixture-worker".into(),
        "input-bytes".into(),
    ];
    let root = Root::new("fi-rs-", &argv)?;
    let mut server = root.server(binary)?;
    let path = root.control();
    let instance =
        rpc(&path, json!({"id":1,"command":"list"}))?["result"]["value"]["instance"].clone();
    let request = |command: &str, mut fields: Value| {
        fields["command"] = command.into();
        fields["id"] = 1.into();
        fields["instance"] = instance.clone();
        fields
    };
    let call = |command: &str, fields: Value| rpc(&path, request(command, fields));
    let value = |command: &str, fields: Value| -> Result<Value> {
        let reply = call(command, fields)?;
        ensure!(reply["status"] == "completed", "control failed: {reply}");
        Ok(reply["result"]["value"].clone())
    };
    let capture = || -> Result<String> {
        Ok(
            value("capture", json!({"pane":1,"max_bytes":131072}))?["text"]
                .as_str()
                .context("capture")?
                .into(),
        )
    };
    let reserve = || -> Result<Value> {
        Ok(value("input-reserve", json!({"pane":1}))?["receipt"]["operation"].clone())
    };
    let status = |op: &Value| -> Result<Value> {
        Ok(value("input-status", json!({"operation":op}))?["receipt"].clone())
    };
    until(Duration::from_secs(10), || {
        Ok(capture()?.contains("READY").then_some(()))
    })?;
    let operation = reserve()?;
    ensure!(
        status(&operation)?["state"] == "reserved",
        "new receipt not reserved"
    );
    let mut discarded = Peer::connect(&path)?;
    discarded.send(&request(
        "input-submit",
        json!({"operation":operation,"keys":"abc"}),
    ))?;
    drop(discarded);
    value("input-submit", json!({"operation":operation,"keys":"abc"}))?;
    let delivered = until(Duration::from_secs(10), || {
        let r = status(&operation)?;
        Ok((r["state"] == "delivered").then_some(r))
    })?;
    ensure!(
        delivered["bytes_written"] == 3,
        "wrong receipt: {delivered}"
    );
    until(Duration::from_secs(10), || {
        Ok(capture()?.contains("BYTE 63").then_some(()))
    })?;
    for _ in 0..3 {
        ensure!(
            value("input-submit", json!({"operation":operation,"keys":"abc"}))?["receipt"]
                == delivered,
            "duplicate changed receipt"
        );
    }
    let text = capture()?;
    ensure!(
        ["61", "62", "63"]
            .iter()
            .all(|code| text.matches(&format!("BYTE {code}")).count() == 1),
        "input duplication: {text}"
    );
    ensure!(
        call(
            "input-submit",
            json!({"operation":operation,"keys":"different"})
        )?["error"]["code"]
            == "conflict",
        "changed replay accepted"
    );
    let stale = reserve()?;
    value("send-keys", json!({"pane":1,"keys":"h"}))?;
    ensure!(
        call(
            "input-submit",
            json!({"operation":stale,"keys":"must-not-arrive"})
        )?["error"]["code"]
            == "conflict",
        "interference accepted"
    );
    ensure!(
        call("input-status", json!({"operation":999999}))?["error"]["code"] == "expired",
        "unknown receipt accepted"
    );
    let stop = reserve()?;
    value("input-submit", json!({"operation":stop,"keys":"!"}))?;
    until(Duration::from_secs(10), || {
        Ok(capture()?.contains("STOPPED").then_some(()))
    })?;
    let mut operations = Vec::new();
    for _ in 0..70 {
        let op = reserve()?;
        value(
            "input-submit",
            json!({"operation":op,"keys":"q".repeat(65536)}),
        )?;
        operations.push(op);
    }
    until(Duration::from_secs(10), || {
        for op in &operations {
            let r = status(op)?;
            if r["state"] == "failed" && r["bytes_written"] == 0 {
                return Ok(Some(()));
            }
        }
        Ok(None)
    })?;
    ensure!(
        value("list", json!({}))?["instance"] == instance,
        "server lost responsiveness"
    );
    server.finish()?;
    ensure!(
        !server.diagnostic()?.contains("panicked"),
        "server panicked"
    );
    println!(
        "PASS reconnect deduplication, PTY receipts, writer interference and bounded stalled input"
    );
    Ok(())
}
