//! A controller's checked attempt must still be current when its mutation executes.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "integration-test assertions and helpers"
)]
mod common;

use common::{Server, code, handle};
use serde_json::json;
use zor::model::*;
use zor::remote::methods::codes;

#[test]
fn cancellation_cannot_follow_a_replaced_attempt_or_pane() {
    let server = Server::start();
    server
        .call(
            "zor/task.create",
            json!({"task":"guarded", "title":"guarded", "cwd":"/tmp"}),
        )
        .unwrap();
    let attempt = server.with_world(|world| {
        let task = world.resource::<Ids>().task("guarded").unwrap();
        let attempt = spawn_attempt(
            world,
            AttemptSpec {
                task,
                ownership: Ownership::Managed,
                handle: handle("fux-a", 7, Some(4242)),
            },
        )
        .unwrap();
        world.get::<AttemptId>(attempt).unwrap().0
    });
    let before = server
        .call("zor/task.inspect", json!({"task":"guarded"}))
        .unwrap();
    let pane = json!({"instance":"fux-a", "workspace":"default", "pane":7, "pid":4242});
    assert_eq!(
        code(server.call(
            "zor/task.cancel",
            json!({
                "task":"guarded", "guard":{"attempt":attempt + 1, "pane":pane}
            })
        )),
        codes::INVALID
    );
    assert_eq!(
        code(server.call(
            "zor/task.stop",
            json!({
                "task":"guarded", "guard":{"attempt":attempt, "pane":{
                    "instance":"fux-a", "workspace":"default", "pane":7, "pid":4243
                }}
            })
        )),
        codes::INVALID
    );
    assert_eq!(
        code(server.call(
            "zor/task.cancel",
            json!({
                "task":"guarded", "guard":{"attempt":null, "pane":null}
            })
        )),
        codes::INVALID
    );
    let after = server
        .call("zor/task.inspect", json!({"task":"guarded"}))
        .unwrap();
    assert_eq!(after["state"], before["state"]);
    assert_eq!(after["attempts"], before["attempts"]);
    let cancelled = server
        .call(
            "zor/task.cancel",
            json!({
                "task":"guarded", "guard":{"attempt":attempt, "pane":pane}
            }),
        )
        .unwrap();
    assert_ne!(cancelled["state"], before["state"]);
    assert_eq!(cancelled["attempts"][0]["pane"], 7);
    assert_eq!(cancelled["attempts"][0]["pid"], 4242);
}

#[test]
fn absent_attempt_guard_refuses_an_attempt_created_since_observation() {
    let server = Server::start();
    server
        .call(
            "zor/task.create",
            json!({"task":"empty", "title":"empty", "cwd":"/tmp"}),
        )
        .unwrap();
    let observation = server
        .call("zor/task.inspect", json!({"task":"empty"}))
        .unwrap();
    assert_eq!(observation["attempts"], json!([]));
    server.with_world(|world| {
        let task = world.resource::<Ids>().task("empty").unwrap();
        spawn_attempt(
            world,
            AttemptSpec {
                task,
                ownership: Ownership::Adopted,
                handle: handle("fux-a", 8, Some(4244)),
            },
        )
        .unwrap();
    });
    assert_eq!(
        code(server.call(
            "zor/task.cancel",
            json!({
                "task":"empty", "guard":{"attempt":null, "pane":null}
            })
        )),
        codes::INVALID
    );
    let still_open = server
        .call("zor/task.inspect", json!({"task":"empty"}))
        .unwrap();
    assert_eq!(still_open["state"], observation["state"]);
    assert_eq!(still_open["attempts"][0]["pane"], 8);
}
