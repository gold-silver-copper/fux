//! Prompt section 5, BRP bullet: allowlist, token enforcement without side effects, typed
//! methods over a real `RemoteHttpPlugin`, projection-only reads, narrowed tokens, fixtures.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp,
    reason = "integration-test helpers; clippy.toml only relaxes #[test] bodies"
)]
mod common;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use bevy_remote::RemoteMethods;
use common::{Server, build, code};
use fux::remote::client;
use fux::remote::methods::{self, codes};
use fux::remote::projection::ALLOWED_TYPE_PATHS;
use serde_json::{Value, json};

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

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/brp")
        .join(name)
}

#[test]
fn method_table_equals_allowlist() {
    let dir = tempfile::tempdir().unwrap();
    let app = build(dir.path(), "allow");
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
}

#[test]
fn end_to_end_over_http() {
    let server = Server::start();
    let nonce = server.descriptor.instance.clone();
    assert_eq!(server.workspace_names(), ["default"]);

    // No token / wrong token: an error and no side effect.
    assert_eq!(
        code(server.raw(
            "fux/workspace.new",
            json!({ "name": "ghost", "instance": nonce })
        )),
        codes::UNAUTHORIZED
    );
    assert_eq!(
        code(server.raw(
            "fux/workspace.new",
            json!({ "name": "ghost", "instance": nonce, "token": "deadbeef" })
        )),
        codes::UNAUTHORIZED
    );
    assert_eq!(server.workspace_names(), ["default"]);

    // An unknown method is answered and does not stall the mailbox.
    assert_eq!(
        code(server.call("fux/nope", json!({}))),
        bevy_remote::error_codes::METHOD_NOT_FOUND
    );

    let info = server.call("fux/server.info", json!({})).unwrap();
    assert_eq!(info["nonce"], nonce);
    assert_eq!(info["workspaces"], 1);

    // Mutations need the instance nonce.
    let mut wrong = server.descriptor.clone();
    wrong.instance = "stale".into();
    assert_eq!(
        code(client::call_with(
            &wrong,
            "fux/workspace.new",
            json!({ "name": "ghost" })
        )),
        codes::INVALID
    );

    let created = server
        .call("fux/workspace.new", json!({ "name": "alpha" }))
        .unwrap();
    assert_eq!(created["name"], "alpha");
    let root = created["root"].as_u64().unwrap();
    let pane = created["pane"].as_u64().unwrap();
    assert_eq!(server.workspace_names(), ["alpha", "default"]);

    // Stale generation is refused before anything changes.
    let list = server.call("fux/workspace.list", json!({})).unwrap();
    let alpha = &list["workspaces"][0];
    let generation = alpha["roots"][0]["generation"].as_u64().unwrap();
    assert_eq!(alpha["roots"][0]["panes"][0]["id"], pane);
    assert_eq!(alpha["roots"][0]["panes"][0]["state"], "starting");
    assert_eq!(
        code(server.call(
            "fux/node.spawn",
            json!({ "generation": generation + 7, "parent": root, "template": {} })
        )),
        codes::STALE_GENERATION
    );
    assert_eq!(
        code(server.call(
            "fux/node.despawn",
            json!({ "generation": generation, "node": 999_999 })
        )),
        codes::NOT_FOUND
    );

    // pane.new returns ids immediately and the pane is visible through the projection.
    let split = server
        .call(
            "fux/pane.new",
            json!({ "split": pane, "direction": "Below", "template": { "argv": ["/bin/sh"] } }),
        )
        .unwrap();
    let new_pane = split["pane"].as_u64().unwrap();
    assert_ne!(new_pane, pane);
    assert!(split["generation"].as_u64().unwrap() > generation);
    let rows = server
        .call(
            "world.query",
            json!({ "data": { "components": ["fux::remote::projection::PaneView"] } }),
        )
        .unwrap();
    let ids: Vec<u64> = rows
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            r["components"]["fux::remote::projection::PaneView"]["id"]
                .as_u64()
                .unwrap()
        })
        .collect();
    assert!(ids.contains(&new_pane), "{ids:?}");
    let view = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["components"]["fux::remote::projection::PaneView"]["id"] == new_pane)
        .unwrap();
    assert_eq!(
        view["components"]["fux::remote::projection::PaneView"]["workspace"],
        "alpha"
    );
    assert_eq!(
        view["components"]["fux::remote::projection::PaneView"]["state"],
        "starting"
    );

    // Authoritative components are not readable even though they are registered for scenes.
    assert_eq!(
        code(server.call(
            "world.query",
            json!({ "data": { "components": ["fux::model::components::Process"] } }),
        )),
        codes::UNAUTHORIZED
    );
    assert_eq!(
        code(server.call(
            "world.query",
            json!({ "data": { "components": ["fux::remote::projection::PaneView"], "option": "all" } }),
        )),
        codes::UNAUTHORIZED
    );
    let listed = server.call("world.list_components", json!({})).unwrap();
    let listed: BTreeSet<&str> = listed
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(listed, ALLOWED_TYPE_PATHS.iter().copied().collect());
    // registry.schema is cut to the projection vocabulary (plus the types those schemas
    // reference); neither the default filter nor a widening one reaches authoritative types.
    for params in [json!({}), json!({ "with_crates": ["fux"] })] {
        let schema = server.call("registry.schema", params).unwrap();
        let keys: BTreeSet<&str> = schema
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert!(ALLOWED_TYPE_PATHS.iter().all(|path| keys.contains(path)));
        assert!(keys.iter().all(|key| !key.starts_with("fux::model::")));
        assert!(!keys.contains("fux::model::components::Process"));
    }
    let narrowed = server
        .call("registry.schema", json!({ "with_crates": ["bevy_ui"] }))
        .unwrap();
    assert!(narrowed.as_object().unwrap().is_empty());

    // A token narrowed to alpha cannot touch default.
    let minted = server
        .call(
            "fux/token.mint",
            json!({ "workspace": "alpha", "capabilities": ["read", "mutate"] }),
        )
        .unwrap();
    let mut narrow = server.descriptor.clone();
    narrow.token = minted["token"].as_str().unwrap().to_owned();
    assert_eq!(
        code(client::call_with(
            &narrow,
            "fux/root.new",
            json!({ "workspace": "default", "name": "x" })
        )),
        codes::UNAUTHORIZED
    );
    assert_eq!(
        code(client::call_with(
            &narrow,
            "fux/workspace.new",
            json!({ "name": "beta" })
        )),
        codes::UNAUTHORIZED
    );
    assert_eq!(
        code(client::call_with(
            &narrow,
            "fux/token.mint",
            json!({ "workspace": "alpha", "capabilities": ["read"] })
        )),
        codes::UNAUTHORIZED
    );
    let names = client::call_with(&narrow, "fux/workspace.list", json!({})).unwrap();
    assert_eq!(names["workspaces"].as_array().unwrap().len(), 1);
    assert_eq!(names["workspaces"][0]["name"], "alpha");
    let extra = client::call_with(
        &narrow,
        "fux/root.new",
        json!({ "workspace": "alpha", "name": "x" }),
    )
    .unwrap();
    assert!(extra["root"].as_u64().unwrap() > root);
    assert_eq!(server.workspace_names(), ["alpha", "default"]);

    // Revocation is immediate.
    let revoked = server
        .call("fux/token.revoke", json!({ "revoke": narrow.token }))
        .unwrap();
    assert_eq!(revoked["revoked"], true);
    assert_eq!(
        code(client::call_with(&narrow, "fux/workspace.list", json!({}))),
        codes::UNAUTHORIZED
    );

    // rpc.discover advertises exactly the allowlist.
    let discovered = server.call("rpc.discover", json!({})).unwrap();
    let advertised: BTreeSet<String> = discovered["methods"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["name"].as_str().unwrap().to_owned())
        .collect();
    let expected: BTreeSet<String> = methods::allowlist()
        .into_iter()
        .map(str::to_owned)
        .collect();
    assert_eq!(advertised, expected);

    // Unknown fields are refused (deny_unknown_fields on every params struct).
    assert_eq!(
        code(server.call(
            "fux/workspace.kill",
            json!({ "name": "alpha", "force": true })
        )),
        bevy_remote::error_codes::INVALID_PARAMS
    );
    server
        .call("fux/workspace.kill", json!({ "name": "alpha" }))
        .unwrap();

    // fux/schema matches the pinned table.
    let schema = server.call("fux/schema", json!({})).unwrap();
    assert_eq!(
        schema,
        serde_json::to_value(methods::schema_table()).unwrap()
    );

    let brp = server.brp.clone();
    drop(server);
    assert!(!brp.exists(), "brp.json must be removed when the App drops");
}

/// Removes `null` members so `Option` fields absent from a fixture compare equal after the
/// typed round trip.
fn without_nulls(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .filter(|(_, v)| !v.is_null())
                .map(|(k, v)| (k, without_nulls(v)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.into_iter().map(without_nulls).collect()),
        other => other,
    }
}

#[test]
fn fixtures_round_trip_through_typed_shapes() {
    let raw = std::fs::read(fixture("methods.json")).unwrap();
    let cases: Vec<Value> = serde_json::from_slice(&raw).unwrap();
    let mut covered = BTreeSet::new();
    for case in cases {
        let method = case["method"].as_str().unwrap().to_owned();
        let method = method.as_str();
        let spec = methods::all_specs()
            .find(|s| s.name == method)
            .unwrap_or_else(|| panic!("{method} is not in the table"));
        covered.insert(spec.name);
        let params = (spec.roundtrip_params)(case["params"].clone())
            .unwrap_or_else(|e| panic!("{method} params: {e}"));
        assert_eq!(
            without_nulls(params),
            without_nulls(case["params"].clone()),
            "{method} params"
        );
        let result = (spec.roundtrip_result)(case["result"].clone())
            .unwrap_or_else(|e| panic!("{method} result: {e}"));
        assert_eq!(
            without_nulls(result),
            without_nulls(case["result"].clone()),
            "{method} result"
        );
    }
    let all: BTreeSet<&str> = methods::all_specs().map(|s| s.name).collect();
    assert_eq!(covered, all, "every fux/* method needs a fixture");
    let spec = methods::all_specs()
        .find(|s| s.name == "fux/pane.close")
        .unwrap();
    assert!((spec.roundtrip_params)(json!({ "pane": 1, "extra": true })).is_err());
}

#[test]
fn schema_matches_fixture() {
    let path = fixture("schema.json");
    let table = serde_json::to_value(methods::schema_table()).unwrap();
    if std::env::var_os("FUX_BLESS").is_some() {
        std::fs::write(&path, serde_json::to_string_pretty(&table).unwrap() + "\n").unwrap();
    }
    let pinned: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(
        table, pinned,
        "run with FUX_BLESS=1 after an intentional schema change"
    );
}

#[test]
fn empty_workspace_cleanup_cannot_retire_a_live_process_workspace() {
    let server = Server::start();
    let before = server.call("fux/workspace.list", json!({})).unwrap();
    assert_eq!(
        code(server.call("fux/workspace.kill", json!({"name":"default","empty_only":true}))),
        codes::INVALID,
    );
    assert_eq!(server.call("fux/workspace.list", json!({})).unwrap(), before);
    server.call("fux/workspace.new", json!({"name":"owned-empty","empty":true})).unwrap();
    server.call("fux/workspace.kill", json!({"name":"owned-empty","empty_only":true})).unwrap();
    let after = server.call("fux/workspace.list", json!({})).unwrap();
    assert!(!after["workspaces"].as_array().unwrap().iter().any(|workspace| {
        workspace["name"] == "owned-empty" && workspace["open"] == true
    }));
    assert!(after["workspaces"].as_array().unwrap().iter().any(|workspace| {
        workspace["name"] == "default" && workspace["open"] == true
    }));
}
