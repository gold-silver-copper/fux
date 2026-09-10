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
