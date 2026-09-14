//! Exact initial selection is admitted by the owner before hello and cannot fall back on exit.
use crate::support::{
    attachment,
    local::{Root, completed, until},
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{os::unix::net::UnixStream, path::Path, time::Duration};

fn attach(path: &Path, initial: Value) -> Result<(UnixStream, Value)> {
    let mut peer = attachment::connect(path)?;
    attachment::send(
        &mut peer,
        &json!({"type":"hello","rows":24,"columns":80,"initial":initial}),
    )?;
    ensure!(
        attachment::receive(&mut peer)? == json!({"hello":{}}),
        "exact hello not accepted"
    );
    ensure!(
        attachment::receive(&mut peer)?.get("bindings").is_some(),
        "bindings missing"
    );
    let state = attachment::receive(&mut peer)?;
    ensure!(
        state.pointer("/state/state").is_some(),
        "first state missing"
    );
    Ok((peer, state))
}
pub(super) fn run(binary: &Path) -> Result<()> {
    let root = Root::new(
        "fexact-",
        &["/bin/sh".into(), "-c".into(), "stty raw -echo; cat".into()],
    )?;
    let mut server = root.server(binary)?;
    let first = completed(&root.control(), json!({"command":"list","id":1}))?;
    let pane = first
        .pointer("/workspaces/0/tabs/0/panes/0")
        .context("initial pane")?;
    let initial = json!({"instance":first["instance"],"workspace":"default",
        "stream":first["workspaces"][0]["event_cursor"]["stream"],"pane":pane["id"],"pid":pane["pid"]});
    let sibling = completed(
        &root.control(),
        json!({"command":"split","id":2,"axis":"horizontal","final_retain_ms":60000}),
    )?["pane"]
        .clone();
    let path = root.path().join("fux/default.attach.sock");
    let (_ordinary, ordinary) = attach(&path, Value::Null)?;
    ensure!(
        ordinary.pointer("/state/state/focused") == Some(&sibling),
        "workspace default focus missing"
    );
    let (mut exact, exact_state) = attach(&path, initial.clone())?;
    ensure!(
        exact_state.pointer("/state/state/focused") == initial.get("pane"),
        "exact attachment inherited wrong focus"
    );
    let (_another, another) = attach(&path, Value::Null)?;
    ensure!(
        another.pointer("/state/state/focused") == Some(&sibling),
        "exact attachment changed default focus"
    );
    let mut invalid = initial.clone();
    invalid["pid"] = json!(initial["pid"].as_u64().context("pid")? + 1);
    let mut denied = attachment::connect(&path)?;
    attachment::send(
        &mut denied,
        &json!({"type":"hello","rows":24,"columns":80,"initial":invalid}),
    )?;
    ensure!(
        attachment::receive(&mut denied)?.get("error").is_some(),
        "stale target received an accepted hello"
    );
    attachment::send(
        &mut exact,
        &json!({"type":"input","bytes":b"EXACT_SELECTION\n".to_vec()}),
    )?;
    until(Duration::from_secs(3), || {
        let capture = completed(
            &root.control(),
            json!({"command":"capture","max_bytes":65536,"id":3,"pane":initial["pane"]}),
        )?;
        Ok(capture["text"]
            .as_str()
            .is_some_and(|text| text.contains("EXACT_SELECTION"))
            .then_some(()))
    })?;
    completed(
        &root.control(),
        json!({"command":"kill","id":4,"pane":initial["pane"]}),
    )?;
    until(Duration::from_secs(5), || {
        Ok(attachment::receive(&mut exact)?
            .get("error")
            .is_some()
            .then_some(()))
    })?;
    let _ = attachment::send(
        &mut exact,
        &json!({"type":"input","bytes":b"NOT_TO_SIBLING\n".to_vec()}),
    );
    let capture = completed(
        &root.control(),
        json!({"command":"capture","max_bytes":65536,"id":5,"pane":sibling}),
    )?;
    ensure!(
        capture["input_sequence"] == 0,
        "exact attachment redirected input after process exit"
    );
    drop(exact);
    server.finish()?;
    println!(
        "PASS: exact initial pane, private focus/defaults, stale hello refusal, target-only input and close without sibling fallback"
    );
    Ok(())
}
