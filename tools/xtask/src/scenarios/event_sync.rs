//! Snapshot/replay/live synchronization against a disposable real server. Every workspace
//! event is delivered (there is no subscription filter); `tab.opened` carries a name, so it is
//! the marker the checks look for.
use crate::support::{
    control::Peer,
    local::{Root, until},
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{path::Path, time::Duration};

pub(super) fn run(binary: &Path) -> Result<()> {
    let argv = [
        std::env::current_exe()?.to_string_lossy().into_owned(),
        "fixture-worker".into(),
        "event-titles".into(),
    ];
    let root = Root::new("fev-rs-", &argv)?;
    let mut server = root.server(binary)?;
    let path = root.control();
    let mut control = Peer::connect(&path)?;
    fn rpc(peer: &mut Peer, instance: &Value, command: &str, mut fields: Value) -> Result<Value> {
        fields["command"] = command.into();
        fields["instance"] = instance.clone();
        fields["id"] = 7.into();
        peer.send(&fields)?;
        peer.read()
    }
    fn value(peer: &mut Peer, instance: &Value, command: &str, fields: Value) -> Result<Value> {
        let reply = rpc(peer, instance, command, fields)?;
        ensure!(reply["status"] == "completed", "control failed: {reply}");
        Ok(reply["result"]["value"].clone())
    }
    /// Reads live events until a `tab.opened` arrives, checking that every frame is a sequenced
    /// event of the subscription with a cursor strictly after `after`.
    fn next_tab(peer: &mut Peer, after: &mut u64) -> Result<Value> {
        for _ in 0..64 {
            let event = peer.read()?;
            let sequence = event["cursor"]["sequence"]
                .as_u64()
                .with_context(|| format!("unsequenced frame: {event}"))?;
            ensure!(
                sequence > *after && event["id"] == 7,
                "duplicated, reordered or foreign event: {event}"
            );
            *after = sequence;
            if event["event"] == "tab.opened" {
                return Ok(event);
            }
        }
        anyhow::bail!("no tab.opened event within 64 frames")
    }
    let instance = value(&mut control, &Value::Null, "list", json!({}))?["instance"].clone();
    until(Duration::from_secs(10), || {
        Ok(value(
            &mut control,
            &instance,
            "capture",
            json!({"pane":1,"max_bytes":131072}),
        )?["text"]
            .as_str()
            .context("capture text")?
            .contains("READY")
            .then_some(()))
    })?;
    let snapshot = value(&mut control, &instance, "list", json!({}))?["workspaces"][0].clone();
    let cursor = &snapshot["event_cursor"];
    let rejected = rpc(
        &mut control,
        &instance,
        "split",
        json!({"axis":"horizontal","stream":cursor["stream"].as_u64().context("stream")?+1,"argv":["/bin/sh","-c","touch \"$HOME/unexpected-launch\""]}),
    )?;
    ensure!(
        rejected["status"] == "failed" && rejected["error"]["code"] == "conflict",
        "stale stream accepted: {rejected}"
    );
    ensure!(
        !root.path().join("unexpected-launch").exists(),
        "stale launch executed"
    );
    ensure!(
        value(&mut control, &instance, "list", json!({}))?["workspaces"][0] == snapshot,
        "stale launch changed snapshot"
    );
    value(
        &mut control,
        &instance,
        "tab",
        json!({"action":{"new":{"name":"title-a"}}}),
    )?;
    let replay = until(Duration::from_secs(10), || {
        let replay = value(&mut control, &instance, "events", json!({"after":cursor}))?;
        let names: Vec<_> = replay["events"]
            .as_array()
            .context("replay events")?
            .iter()
            .filter(|event| event["event"] == "tab.opened")
            .map(|event| event["name"].clone())
            .collect();
        ensure!(names.len() <= 1, "duplicated replay: {replay}");
        Ok((names == vec![json!("title-a")]).then_some(replay))
    })?;
    let events = replay["events"].as_array().context("replay events")?;
    ensure!(
        events
            .iter()
            .all(|event| event["cursor"]["stream"] == cursor["stream"]),
        "foreign stream in replay"
    );
    ensure!(
        value(
            &mut control,
            &instance,
            "events",
            json!({"after":replay["cursor"]})
        )?["events"]
            == json!([]),
        "replay duplicated events"
    );
    let mut subscriber = Peer::connect(&path)?;
    subscriber.send(&json!({"id":7,"command":"subscribe","instance":instance,"after":cursor}))?;
    ensure!(
        subscriber.read()?["status"] == "accepted",
        "subscription rejected"
    );
    let mut seen = cursor["sequence"].as_u64().context("cursor sequence")?;
    let first = next_tab(&mut subscriber, &mut seen)?;
    ensure!(first["name"] == "title-a", "wrong first: {first}");
    value(
        &mut control,
        &instance,
        "tab",
        json!({"action":{"new":{"name":"title-b"}}}),
    )?;
    let second = next_tab(&mut subscriber, &mut seen)?;
    ensure!(second["name"] == "title-b", "wrong second: {second}");
    let mut racing = Peer::connect(&path)?;
    let current =
        value(&mut control, &instance, "list", json!({}))?["workspaces"][0]["event_cursor"].clone();
    racing.send(&json!({"id":7,"command":"subscribe","instance":instance,"after":current}))?;
    value(
        &mut control,
        &instance,
        "tab",
        json!({"action":{"new":{"name":"title-c"}}}),
    )?;
    ensure!(
        racing.read()?["status"] == "accepted",
        "racing subscription rejected"
    );
    let mut raced = current["sequence"].as_u64().context("current sequence")?;
    ensure!(
        next_tab(&mut racing, &mut raced)?["name"] == "title-c",
        "race lost or duplicated title-c"
    );
    value(
        &mut control,
        &instance,
        "tab",
        json!({"action":{"new":{"name":"title-d"}}}),
    )?;
    ensure!(
        next_tab(&mut racing, &mut raced)?["name"] == "title-d",
        "race lost or duplicated title-d"
    );
    for _ in 0..1030 {
        value(
            &mut control,
            &instance,
            "focus",
            json!({"target":{"pane":1}}),
        )?;
    }
    ensure!(
        rpc(&mut control, &instance, "events", json!({"after":cursor}))?["error"]["code"] == "gap",
        "old cursor accepted"
    );
    let mut gap = Peer::connect(&path)?;
    gap.send(&json!({"id":7,"command":"subscribe","instance":instance,"after":cursor}))?;
    ensure!(
        gap.read()?["error"]["code"] == "gap",
        "old subscription accepted"
    );
    let fresh =
        value(&mut control, &instance, "list", json!({}))?["workspaces"][0]["event_cursor"].clone();
    ensure!(
        value(&mut control, &instance, "events", json!({"after":fresh}))?["events"] == json!([]),
        "fresh snapshot replay not empty"
    );
    for (field, increment) in [("stream", 1), ("sequence", 100)] {
        let mut bad = fresh.clone();
        bad[field] = json!(bad[field].as_u64().context("cursor number")? + increment);
        ensure!(
            rpc(&mut control, &instance, "events", json!({"after":bad}))?["error"]["code"] == "gap",
            "foreign or future cursor accepted"
        );
    }
    drop((control, subscriber, racing, gap));
    server.finish()?;
    println!("PASS snapshot replay, live delivery, race deduplication, gap and resync");
    Ok(())
}
