//! The `zor plugin hook NAME` child consumes two bounded event streams. Each cursor claim
//! is fsynced by the host BEFORE running matching commands. Reconnects skip claimed events;
//! a crash between claim and completion can omit work, never authorizes automatic replay.
//! This is at-most-once dispatch, not exactly-once side effects. An incarnation change starts
//! at the present; a retention gap is reported on stderr and explicitly claimed before resume.
//! Hook commands inherit this child's process group so host cancellation includes descendants.
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;
use fux::remote::client::{self, Descriptor};
use serde_json::{Value, json};
use super::{HookCursors, manifest::{EventHook, pattern_matches}};

/// Internal CLI entrypoint. The host provides all authority through scoped descriptors;
/// invoking this without the host's environment is rejected.
pub fn run(name: &str) -> Result<(), String> {
    if std::env::var("ZOR_PLUGIN_NAME").as_deref() != Ok(name) {
        return Err("hook name does not match host invocation".into());
    }
    let zor = PathBuf::from(std::env::var_os("ZOR_BRP").ok_or("missing ZOR_BRP")?);
    let authority = client::read_descriptor(&zor).map_err(|e| e.to_string())?;
    let hooks: Vec<EventHook> = serde_json::from_str(&std::env::var("ZOR_PLUGIN_HOOKS").map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let cursors: HookCursors = serde_json::from_str(&std::env::var("ZOR_PLUGIN_CURSORS").map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let (done, receive) = mpsc::sync_channel(2);
    for (source, path, cursor, instance) in [
        ("zor", Some(zor.clone()), cursors.zor, cursors.zor_instance),
        ("fux", std::env::var_os("FUX_BRP").map(PathBuf::from), cursors.fux, cursors.fux_instance),
    ] {
        let Some(path) = path else { continue };
        let hooks = hooks.clone();
        let name = name.to_owned();
        let zor = authority.clone();
        let done = done.clone();
        std::thread::Builder::new().name(format!("plugin-hook-{source}")).spawn(move || {
            let result = consume(&name, source, &path, &zor, hooks, cursor, instance);
            let _ = done.send(result);
        }).map_err(|e| e.to_string())?;
    }
    drop(done);
    receive.recv().map_err(|e| e.to_string())?
}

fn claim(zor: &Descriptor, name: &str, source: &str, cursor: u64, instance: &str) -> Result<(), String> {
    let mut params = json!({"name": name});
    params[source] = json!(cursor);
    params[format!("{source}_instance")] = json!(instance);
    client::call_with(zor, "zor/plugin.cursor", params).map(|_| ()).map_err(|e| e.to_string())
}

fn consume(name: &str, source: &str, path: &Path, zor: &Descriptor, hooks: Vec<EventHook>, mut cursor: u64, mut instance: String) -> Result<(), String> {
    let descriptor = client::read_descriptor(path).map_err(|e| e.to_string())?;
    let mut backoff = Duration::from_millis(250);
    loop {
        if instance != descriptor.instance {
            eprintln!("zor: {source} hook stream incarnation changed; beginning at present");
            // A null watch emits no baseline. Persist the finite snapshot before subscribing,
            // so a disconnect before the first event cannot resume from retained history.
            let baseline = client::call_with(&descriptor, &format!("{source}/events.poll"), json!({"cursor": null}))
                .map_err(|e| e.to_string())?;
            let activation = baseline.get("cursor").and_then(Value::as_u64).ok_or("invalid hook activation cursor")?;
            claim(zor, name, source, activation, &descriptor.instance)?;
            instance.clone_from(&descriptor.instance);
            cursor = activation;
        }
        let mut failure = None;
        let result = client::stream(&descriptor, &format!("{source}/events+watch"), json!({"cursor": cursor}), |value| {
            match handle_item(name, source, zor, &descriptor, &hooks, &mut cursor, value) {
                Ok(()) => { backoff = Duration::from_millis(250); true }
                Err(e) => { failure = Some(e); false }
            }
        });
        if let Some(failure) = failure { return Err(failure); }
        if let Err(e) = result { eprintln!("zor: {source} hook stream disconnected: {e}"); }
        // A reconnect only resumes observation. It never re-runs a claimed command.
        std::thread::sleep(backoff);
        backoff = (backoff * 2).min(Duration::from_secs(5));
    }
}

fn handle_item(name: &str, source: &str, zor: &Descriptor, descriptor: &Descriptor, hooks: &[EventHook], cursor: &mut u64, value: Value) -> Result<(), String> {
    let value = if value.get("jsonrpc").is_some() { client::unwrap_reply(value).map_err(|e| e.to_string())? } else { value };
    if let Some(gap) = value.get("gap").filter(|g| !g.is_null()) {
        let resume = gap.get("resume").and_then(Value::as_u64).ok_or("invalid hook gap")?;
        eprintln!("zor: {source} hook event gap {gap}; missing events are not replayable");
        claim(zor, name, source, resume, &descriptor.instance)?;
        *cursor = resume;
    }
    let events = value.get("events").and_then(Value::as_array).ok_or("invalid hook stream item")?;
    for event in events {
        let next = event.get("cursor").and_then(Value::as_u64).ok_or("invalid event cursor")?;
        if next <= *cursor { continue; }
        let event_name = event.get("name").and_then(Value::as_str).ok_or("invalid event name")?;
        let qualified = if event_name.starts_with(&format!("{source}/")) { event_name.to_owned() } else { format!("{source}/{event_name}") };
        // One durable claim covers every matching hook. A failed action is logged and is not
        // retried; retries require an application-level idempotency/reconciliation protocol.
        claim(zor, name, source, next, &descriptor.instance)?;
        *cursor = next;
        for hook in hooks.iter().filter(|h| pattern_matches(&h.pattern, &qualified)) {
            let Some((program, args)) = hook.command.split_first() else { return Err("empty hook command".into()) };
            let payload = serde_json::to_string(event.get("event").unwrap_or(&Value::Null)).map_err(|e| e.to_string())?;
            let mut command = Command::new(program);
            command.args(args).stdin(Stdio::null()).env("ZOR_PLUGIN_KIND", "event")
                .env("ZOR_PLUGIN_EVENT_NAME", &qualified).env("ZOR_PLUGIN_EVENT", &payload)
                .env("ZOR_PLUGIN_EVENT_CURSOR", next.to_string()).env("ZOR_PLUGIN_EVENT_INSTANCE", &descriptor.instance);
            let status = command.status().map_err(|e| format!("hook {qualified}: {e}"))?;
            if !status.success() { eprintln!("zor: hook {qualified} exited {status}; claim retained, not retried"); }
        }
    }
    Ok(())
}
