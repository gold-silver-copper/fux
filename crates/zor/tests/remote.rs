//! zor's BRP surface over a real `RemoteHttpPlugin` (prompt 4.3): allowlist, token
//! enforcement without side effects, `zor/server.info`, the 0600 descriptor, narrowed tokens,
//! projection-only reads and `zor/events+watch` delivering a zor event.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "integration-test helpers; clippy.toml only relaxes #[test] bodies"
)]
mod common;

use std::collections::BTreeSet;
use std::os::unix::fs::PermissionsExt;
use std::sync::mpsc;
use std::time::Duration;

use bevy_remote::RemoteMethods;
use common::{Server, build, code, spawn_graph};
use serde_json::{Value, json};
use zor::model::*;
use zor::remote::client;
use zor::remote::events::TaskOpened;
use zor::remote::methods::{self, codes};
use zor::remote::projection::ALLOWED_TYPE_PATHS;
use zor::remote::watch::EVENTS_WATCH_METHOD;

const DENYLIST: [&str; 11] = [
    "world.spawn_entity",
    "world.insert_components",
    "world.remove_components",
    "world.despawn_entity",
    "world.reparent_entities",
    "world.mutate_components",
    "world.insert_resources",
    "world.remove_resources",
    "world.mutate_resources",
    "world.trigger_event",
    "world.write_message",
];

#[test]
fn method_table_equals_allowlist_and_hides_authoritative_components() {
    let dir = tempfile::tempdir().unwrap();
    let app = build(&dir.path().join("run"), &dir.path().join("state"), "allow");
    let registered: BTreeSet<String> = app
        .world()
        .resource::<RemoteMethods>()
        .methods()
        .into_iter()
        .collect();
    let expected: BTreeSet<String> = methods::allowlist()
        .into_iter()
        .map(str::to_owned)
        .collect();
    assert_eq!(registered, expected);
    for denied in DENYLIST {
        assert!(
            !registered.contains(denied),
            "{denied} must not be registered"
        );
    }
    for path in ALLOWED_TYPE_PATHS {
        assert!(!path.contains("model::components::TaskState"));
    }
}

#[test]
fn end_to_end_over_http() {
    let server = Server::start();
    let nonce = server.descriptor.instance.clone();

    // The descriptor is private.
    let mode = std::fs::metadata(&server.brp).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    assert!(server.descriptor.attach.is_none());

    // No token / wrong token: an error and no handler side effect.
    assert_eq!(
        code(server.raw(
            "zor/token.mint",
            json!({ "capabilities": ["read"], "instance": nonce })
        )),
        codes::UNAUTHORIZED
    );
    assert_eq!(
        code(server.raw(
            "zor/token.mint",
            json!({ "capabilities": ["read"], "instance": nonce, "token": "deadbeef" })
        )),
        codes::UNAUTHORIZED
    );
    let info = server.call("zor/server.info", json!({})).unwrap();
    assert_eq!(info["nonce"], nonce);
    assert_eq!(info["tokens_minted"], 0);
    assert_eq!(info["tasks"], 0);
    assert_eq!(info["limits"]["tasks"], 128);

    // An unknown method is answered and does not stall the mailbox.
    assert_eq!(
        code(server.call("zor/nope", json!({}))),
        bevy_remote::error_codes::METHOD_NOT_FOUND
    );

    // A narrowed token reads but cannot mint or revoke.
    let minted = server
        .call("zor/token.mint", json!({ "capabilities": ["read"] }))
        .unwrap();
    let mut narrow = server.descriptor.clone();
    narrow.token = minted["token"].as_str().unwrap().to_owned();
    assert!(client::call_with(&narrow, "zor/server.info", json!({})).is_ok());
    assert_eq!(
        code(client::call_with(
            &narrow,
            "zor/token.mint",
            json!({ "capabilities": ["read"] })
        )),
        codes::UNAUTHORIZED
    );
    let revoked = server
        .call("zor/token.revoke", json!({ "revoke": narrow.token }))
        .unwrap();
    assert_eq!(revoked["revoked"], true);
    assert_eq!(
        code(client::call_with(&narrow, "zor/server.info", json!({}))),
        codes::UNAUTHORIZED
    );

    // Projections: the graph appears through `world.query`; authoritative components are refused.
    server.with_world(|world| {
        spawn_graph(world);
    });
    std::thread::sleep(Duration::from_millis(50));
    let rows = server
        .call(
            "world.query",
            json!({ "data": { "components": ["zor::remote::projection::TaskView"] } }),
        )
        .unwrap();
    let mut ids: Vec<String> = rows
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            r["components"]["zor::remote::projection::TaskView"]["id"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    ids.sort();
    assert_eq!(ids, ["t1", "t2"]);
    assert_eq!(
        code(server.call(
            "world.query",
            json!({ "data": { "components": ["zor::model::components::TaskState"] } }),
        )),
        codes::UNAUTHORIZED
    );
    let checks = server
        .call(
            "world.query",
            json!({ "data": { "components": ["zor::remote::projection::CheckView"] } }),
        )
        .unwrap();
    assert_eq!(
        checks[0]["components"]["zor::remote::projection::CheckView"]["state"],
        "Passed"
    );
    let listed = server.call("world.list_components", json!({})).unwrap();
    for name in listed.as_array().unwrap() {
        assert!(ALLOWED_TYPE_PATHS.contains(&name.as_str().unwrap()));
    }
    let schema = server.call("registry.schema", json!({})).unwrap();
    assert!(schema.get("zor::remote::projection::TaskView").is_some());
    assert!(schema.get("zor::model::components::TaskState").is_none());

    // The schema and task.inspect answer.
    let table = server.call("zor/schema", json!({})).unwrap();
    assert_eq!(table["envelope"], json!(["token", "instance"]));
    assert!(table["methods"].get("zor/task.inspect").is_some());
    let inspect = server
        .call("zor/task.inspect", json!({ "task": "t1" }))
        .unwrap();
    assert_eq!(inspect["live"], true);
    assert_eq!(inspect["state"], "Open");
    assert_eq!(
        code(server.call("zor/task.inspect", json!({ "task": "nope" }))),
        codes::NOT_FOUND
    );
}

#[test]
fn events_watch_delivers_a_zor_event_and_resumes_by_cursor() {
    let server = Server::start();
    let descriptor = server.descriptor.clone();
    let (tx, rx) = mpsc::channel::<Value>();
    let stream = std::thread::spawn(move || {
        client::stream(&descriptor, EVENTS_WATCH_METHOD, json!({}), |item| {
            let more = item["events"].as_array().is_none_or(Vec::is_empty);
            let _ = tx.send(item);
            more
        })
    });
    // Let the stream open before the fact it must carry.
    std::thread::sleep(Duration::from_millis(100));
    server.with_world(|world| {
        let task = world
            .spawn((Task, TaskId("evt".into()), TaskState::Open))
            .id();
        world.trigger(TaskOpened {
            entity: task,
            task: TaskId("evt".into()),
        });
    });
    let item = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(item["gap"].is_null());
    let event = &item["events"][0];
    assert_eq!(event["name"], "TaskOpened");
    assert_eq!(event["event"]["task"], "evt");
    let cursor = event["cursor"].as_u64().unwrap();
    assert!(cursor >= 1);
    stream.join().unwrap().unwrap();

    // Resuming from an older cursor replays the retained event; a foreign incarnation's
    // cursor is answered with a gap notice first.
    let (tx, rx) = mpsc::channel::<Value>();
    let descriptor = server.descriptor.clone();
    std::thread::spawn(move || {
        client::stream(
            &descriptor,
            EVENTS_WATCH_METHOD,
            json!({ "cursor": cursor - 1 }),
            |item| tx.send(item).is_err(),
        )
    });
    let replay = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(replay["events"][0]["cursor"], cursor);
    let mut foreign = server.descriptor.clone();
    foreign.instance = "other".into();
    let (tx, rx) = mpsc::channel::<Value>();
    std::thread::spawn(move || {
        client::stream(
            &foreign,
            EVENTS_WATCH_METHOD,
            json!({ "cursor": 40 }),
            |item| tx.send(item).is_err(),
        )
    });
    let gap = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(gap["gap"]["since"], 40);
    assert_eq!(gap["gap"]["resume"], 0);
}
