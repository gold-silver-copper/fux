//! Golden protocol fixtures shared with koh and zor. Each file under tests/verify/fixtures/
//! must decode into its schema type (strict, `deny_unknown_fields`) and survive a round trip, so
//! a schema change shows up here and in the integrators that load the same files.
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use serde::Serialize;
use serde::de::DeserializeOwned;
use std::path::{Path, PathBuf};

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/verify/fixtures")
}

fn round_trip<T: DeserializeOwned + Serialize + PartialEq + std::fmt::Debug>(path: &Path) {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let value: T = serde_json::from_slice(&bytes).unwrap_or_else(|e| {
        panic!(
            "decode {} as {}: {e}",
            path.display(),
            std::any::type_name::<T>()
        )
    });
    let encoded = serde_json::to_vec(&value).expect("re-encode");
    let again: T = serde_json::from_slice(&encoded)
        .unwrap_or_else(|e| panic!("re-decode {}: {e}", path.display()));
    assert_eq!(value, again, "round trip changed {}", path.display());
}

fn each(subdir: &str, prefix: &str, run: impl Fn(&Path)) -> usize {
    let dir = fixtures_dir().join(subdir);
    let mut count = 0;
    for entry in
        std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()))
    {
        let path = entry.expect("entry").path();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if name.starts_with(prefix) && name.ends_with(".json") {
            run(&path);
            count += 1;
        }
    }
    count
}

#[test]
fn every_protocol_fixture_round_trips_through_its_type() {
    use fux::daemon::{ManagerReply, ManagerRequest};
    use fux::proto::attach::{ClientMessage, ServerMessage};
    use fux::proto::control::{Event, Reply, Request};
    let mut total = 0;
    total += each("control", "request_", round_trip::<Request>);
    total += each("control", "reply_", round_trip::<Reply>);
    total += each("control", "event_", round_trip::<Event>);
    total += each("manager", "request_", round_trip::<ManagerRequest>);
    total += each("manager", "reply_", round_trip::<ManagerReply>);
    total += each("attach", "client_", round_trip::<ClientMessage>);
    total += each("attach", "server_", round_trip::<ServerMessage>);
    assert!(total >= 20, "expected the fixture set, found {total}");
}

#[test]
fn zor_capture_decoders_use_the_producer_fixtures() {
    for name in ["reply_completed_capture.json", "reply_completed_cells.json"] {
        let producer = fixtures_dir().join("control").join(name);
        let consumer = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../zor/tests/fixtures/capture")
            .join(name);
        assert_eq!(
            std::fs::read(&producer).expect("producer capture"),
            std::fs::read(&consumer).expect("consumer capture"),
            "capture fixture drift: {name}"
        );
        round_trip::<fux::proto::control::Reply>(&producer);
    }
}

#[test]
fn zor_typed_client_fixtures_match_the_producer_schema() {
    for name in [
        "request_focus_client.json",
        "reply_focus_client.json",
        "request_split_client.json",
        "reply_split_client.json",
        "request_events_client.json",
        "reply_events_client.json",
    ] {
        let producer = fixtures_dir().join("control").join(name);
        let consumer = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../zor/tests/fixtures/control")
            .join(name);
        assert_eq!(
            std::fs::read(&producer).expect("producer fixture"),
            std::fs::read(&consumer).expect("client fixture")
        );
        if name.starts_with("request") {
            round_trip::<fux::proto::control::Request>(&producer);
        } else {
            round_trip::<fux::proto::control::Reply>(&producer);
        }
    }
}

#[test]
fn zor_creation_fixtures_match_the_manager_producer() {
    for name in ["request_create_client.json", "reply_create_client.json"] {
        let producer = fixtures_dir().join("manager").join(name);
        let consumer = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../zor/tests/fixtures/manager")
            .join(name);
        assert_eq!(
            std::fs::read(&producer).expect("producer"),
            std::fs::read(consumer).expect("consumer")
        );
        if name.starts_with("request") {
            round_trip::<fux::daemon::ManagerRequest>(&producer);
        } else {
            round_trip::<fux::daemon::ManagerReply>(&producer);
        }
    }
}

/// The `info` fixture publishes the live retention ceilings, and zor's copy of it (which pins
/// zor's retention policy in `crates/zor/src/fux.rs`) is byte-identical to fux's.
#[test]
fn info_fixture_carries_the_retention_ceilings_and_zor_reads_the_same_bytes() {
    use fux::proto::control::{CommandResult, Reply};
    let path = fixtures_dir().join("control/reply_completed_info.json");
    let bytes = std::fs::read(&path).expect("info fixture");
    let Reply::Completed {
        result: CommandResult::Info { info },
        ..
    } = serde_json::from_slice(&bytes).expect("decode info reply")
    else {
        panic!("info fixture is not a completed info reply");
    };
    assert_eq!(
        info.limits.input_retention_ms,
        fux::proto::control::MAX_INPUT_RETENTION_MS
    );
    assert_eq!(
        info.limits.final_retention_ms,
        fux::proto::control::MAX_FINAL_RETENTION_MS
    );
    let zor = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../zor/tests/fixtures/control/reply_completed_info.json");
    assert_eq!(
        std::fs::read(&zor).expect("zor's copy of the info fixture"),
        bytes,
        "{} differs from fux's fixture",
        zor.display()
    );
}

#[test]
fn zor_typed_operation_fixtures_match_the_producer_and_round_trip_exactly() {
    use fux::daemon::{ManagerReply, ManagerRequest};
    use fux::proto::control::{Reply, Request};
    let consumer = Path::new(env!("CARGO_MANIFEST_DIR")).join("../zor/tests/fixtures");
    let mut count = 0;
    for dir in ["manager", "control"] {
        for entry in std::fs::read_dir(fixtures_dir().join(dir)).expect("fixtures") {
            let path = entry.expect("entry").path();
            let name = path.file_name().unwrap().to_str().unwrap();
            if !name.ends_with("_zor.json") {
                continue;
            }
            let bytes = std::fs::read(&path).expect("producer fixture");
            assert_eq!(
                bytes,
                std::fs::read(consumer.join(dir).join(name)).expect("consumer fixture")
            );
            let value: serde_json::Value = serde_json::from_slice(&bytes).expect("JSON");
            let encoded = match (dir, name.starts_with("request_")) {
                ("manager", true) => serde_json::to_value(
                    serde_json::from_slice::<ManagerRequest>(&bytes).expect("manager request"),
                ),
                ("manager", false) => serde_json::to_value(
                    serde_json::from_slice::<ManagerReply>(&bytes).expect("manager reply"),
                ),
                (_, true) => serde_json::to_value(
                    serde_json::from_slice::<Request>(&bytes).expect("request"),
                ),
                (_, false) => {
                    serde_json::to_value(serde_json::from_slice::<Reply>(&bytes).expect("reply"))
                }
            }
            .expect("serialize producer");
            assert_eq!(encoded, value, "producer changed {name}");
            count += 1;
        }
    }
    assert_eq!(count, 16);
}
