//! Prompt 3.9 streams over a real `RemoteHttpPlugin`: `fux/events+watch` with cursors and gaps,
//! `world.observe+watch` over the public events only, the built-in `+watch` wrappers, batch
//! refusal, revocation closing streams, unknown names never stalling the mailbox.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "integration-test helpers; clippy.toml only relaxes #[test] bodies"
)]
mod common;

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use common::{Server, code};
use fux::model::{Ids, Inbound, PaneId};
use fux::remote::client;
use fux::remote::methods::codes;
use serde_json::{Value, json};

const WAIT: Duration = Duration::from_secs(5);

/// One HTTP/1.1 POST whose reply is read incrementally: a `text/event-stream` reply yields
/// JSON-RPC responses one `data:` frame at a time; a complete reply is read to its end.
struct Http {
    stream: TcpStream,
    raw: Vec<u8>,
    body: Vec<u8>,
    chunked: bool,
    content_length: Option<usize>,
    chunk_left: usize,
    trailer_pending: bool,
    ended: bool,
}

impl Http {
    fn post(server: &Server, payload: &Value) -> Self {
        let body = serde_json::to_vec(payload).unwrap();
        let host = &server.descriptor.http.host;
        let port = server.descriptor.http.port;
        let mut stream = TcpStream::connect((host.as_str(), port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_millis(50)))
            .unwrap();
        let header = format!(
            "POST / HTTP/1.1\r\nHost: {host}:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        stream.write_all(header.as_bytes()).unwrap();
        stream.write_all(&body).unwrap();
        let mut http = Self {
            stream,
            raw: Vec::new(),
            body: Vec::new(),
            chunked: false,
            content_length: None,
            chunk_left: 0,
            trailer_pending: false,
            ended: false,
        };
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(end) = find(&http.raw, b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&http.raw[..end]).to_ascii_lowercase();
                assert!(head.starts_with("http/1.1 200"), "{head}");
                http.chunked = head.contains("transfer-encoding: chunked");
                http.content_length = head
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:"))
                    .map(|v| v.trim().parse().unwrap());
                http.raw.drain(..end + 4);
                http.pump();
                return http;
            }
            assert!(
                http.read_more() || Instant::now() < deadline,
                "no response head"
            );
        }
    }

    /// Reads whatever the socket has; `false` on EOF or timeout.
    fn read_more(&mut self) -> bool {
        let mut chunk = [0u8; 4096];
        match self.stream.read(&mut chunk) {
            Ok(0) => {
                self.ended = true;
                false
            }
            Ok(n) => {
                self.raw.extend_from_slice(&chunk[..n]);
                true
            }
            Err(_) => false,
        }
    }

    /// Moves decoded body bytes from `raw` to `body`.
    fn pump(&mut self) {
        if !self.chunked {
            self.body.append(&mut self.raw);
            if let Some(len) = self.content_length
                && self.body.len() >= len
            {
                self.ended = true;
            }
            return;
        }
        loop {
            if self.trailer_pending {
                if self.raw.len() < 2 {
                    return;
                }
                self.raw.drain(..2);
                self.trailer_pending = false;
            }
            if self.chunk_left == 0 {
                let Some(end) = find(&self.raw, b"\r\n") else {
                    return;
                };
                let line = String::from_utf8_lossy(&self.raw[..end]).to_string();
                let size =
                    usize::from_str_radix(line.split(';').next().unwrap().trim(), 16).unwrap();
                self.raw.drain(..end + 2);
                if size == 0 {
                    self.ended = true;
                    return;
                }
                self.chunk_left = size;
            }
            let take = self.chunk_left.min(self.raw.len());
            if take == 0 {
                return;
            }
            self.body.extend(self.raw.drain(..take));
            self.chunk_left -= take;
            if self.chunk_left == 0 {
                self.trailer_pending = true;
            }
        }
    }

    /// The next SSE frame's JSON-RPC response, `None` once the stream ended or `wait` passed.
    fn next(&mut self, wait: Duration) -> Option<Value> {
        let deadline = Instant::now() + wait;
        loop {
            if let Some(end) = find(&self.body, b"\n\n") {
                let frame = String::from_utf8(self.body.drain(..end + 2).collect()).unwrap();
                let data: String = frame
                    .lines()
                    .filter_map(|l| l.strip_prefix("data: "))
                    .collect();
                return Some(serde_json::from_str(&data).unwrap());
            }
            if self.ended || Instant::now() > deadline {
                return None;
            }
            self.read_more();
            self.pump();
        }
    }

    /// Whether the server ended the stream (terminating chunk or EOF) within `wait`.
    fn ended(&mut self, wait: Duration) -> bool {
        let deadline = Instant::now() + wait;
        while !self.ended && Instant::now() < deadline {
            self.read_more();
            self.pump();
        }
        self.ended
    }

    /// A complete (non-streaming) reply body.
    fn complete(mut self) -> Value {
        assert!(self.ended(WAIT), "reply never completed");
        serde_json::from_slice(&self.body).unwrap()
    }

    /// The first item of a stream that is expected to end right after it.
    fn next_owned(mut self) -> Option<Value> {
        self.next(WAIT)
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn rpc(method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": 7, "method": method, "params": params })
}

/// Opens a stream with the descriptor's envelope injected.
fn watch(server: &Server, method: &str, params: Value) -> Http {
    watch_as(server, &server.descriptor, method, params)
}

fn watch_as(server: &Server, descriptor: &client::Descriptor, method: &str, params: Value) -> Http {
    let Value::Object(mut fields) = params else {
        panic!("params must be an object");
    };
    fields.insert("token".into(), Value::String(descriptor.token.clone()));
    fields.insert(
        "instance".into(),
        Value::String(descriptor.instance.clone()),
    );
    Http::post(server, &rpc(method, Value::Object(fields)))
}

fn result(item: Option<Value>) -> Value {
    let item = item.expect("stream item");
    assert!(item.get("error").is_none(), "{item}");
    item["result"].clone()
}

fn error_code(item: Option<Value>) -> i64 {
    item.expect("stream item")["error"]["code"]
        .as_i64()
        .unwrap()
}

fn spawn_pane(server: &Server, id: u64) {
    server.with_world(move |world| {
        let pane = world.resource::<Ids>().pane(PaneId(id)).unwrap();
        world.write_message(Inbound::PaneSpawned { pane, pid: 4242 });
    });
}

fn exit_pane(server: &Server, id: u64, code: i32) {
    server.with_world(move |world| {
        let pane = world.resource::<Ids>().pane(PaneId(id)).unwrap();
        world.write_message(Inbound::PaneExited { pane, code });
    });
}

fn new_workspace(server: &Server, name: &str) -> u64 {
    let created = server
        .call("fux/workspace.new", json!({ "name": name }))
        .unwrap();
    created["pane"].as_u64().unwrap()
}

/// A second pane next to `pane`, spawned: one more `PaneSpawned` in its workspace.
fn spawn_sibling(server: &Server, pane: u64) -> u64 {
    let split = server
        .call(
            "fux/pane.new",
            json!({ "split": pane, "direction": "Below", "template": { "argv": ["/bin/sh"] } }),
        )
        .unwrap();
    let id = split["pane"].as_u64().unwrap();
    spawn_pane(server, id);
    id
}

/// Items until the stream is silent for a while; the events flattened, in order.
fn drain(stream: &mut Http) -> Vec<Value> {
    let mut events = Vec::new();
    while let Some(item) = stream.next(Duration::from_millis(300)) {
        let item = result(Some(item));
        events.extend(item["events"].as_array().unwrap().iter().cloned());
    }
    events
}

/// Reads items until an event called `name` arrives.
fn named(stream: &mut Http, name: &str) -> Value {
    let deadline = Instant::now() + WAIT;
    loop {
        let item = result(stream.next(WAIT));
        if let Some(event) = item["events"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["name"] == name)
        {
            return event.clone();
        }
        assert!(Instant::now() < deadline, "no {name} event");
    }
}

#[test]
fn events_watch_streams_resumes_and_reports_gaps() {
    let server = Server::start();
    let mut live = watch(&server, "fux/events+watch", json!({}));

    let pane = new_workspace(&server, "alpha");
    spawn_pane(&server, pane);
    let first = result(live.next(WAIT));
    assert!(first["gap"].is_null(), "{first}");
    let spawned = first["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "PaneSpawned")
        .unwrap_or_else(|| panic!("{first}"));
    assert_eq!(spawned["workspace"], "alpha");
    assert_eq!(spawned["event"]["pane"], pane);
    assert_eq!(spawned["event"]["pid"], 4242);
    let after_spawn = spawned["cursor"].as_u64().unwrap();

    // A second pane keeps the root (and the workspace) alive when the first exits.
    let sibling = spawn_sibling(&server, pane);
    let second = named(&mut live, "PaneSpawned");
    assert_eq!(second["event"]["pane"], sibling);

    exit_pane(&server, pane, 3);
    let exited = named(&mut live, "PaneExited");
    assert_eq!(exited["event"]["pane"], pane);
    assert_eq!(exited["event"]["code"], 3);
    let after_exit = exited["cursor"].as_u64().unwrap();
    assert!(after_exit > after_spawn);

    // Resuming after the spawn cursor replays only what came later.
    let mut resumed = watch(
        &server,
        "fux/events+watch",
        json!({ "cursor": after_spawn }),
    );
    let replay = result(resumed.next(WAIT));
    assert!(replay["gap"].is_null(), "{replay}");
    let cursors: Vec<u64> = replay["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["cursor"].as_u64().unwrap())
        .collect();
    assert!(cursors.iter().all(|c| *c > after_spawn), "{cursors:?}");
    assert!(cursors.contains(&after_exit), "{cursors:?}");
    // Resuming at the latest cursor is silent until something happens.
    let latest = drain(&mut live)
        .last()
        .map_or(after_exit, |e| e["cursor"].as_u64().unwrap());
    let mut quiet = watch(&server, "fux/events+watch", json!({ "cursor": latest }));
    let extra = quiet.next(Duration::from_millis(300));
    assert!(extra.is_none(), "{extra:?}");

    // A cursor without `instance` is refused; another incarnation's cursor is a gap first,
    // then everything retained.
    let no_instance = Http::post(
        &server,
        &rpc(
            "fux/events+watch",
            json!({ "token": server.descriptor.token, "cursor": after_spawn }),
        ),
    )
    .next(WAIT);
    assert_eq!(error_code(no_instance), i64::from(codes::INVALID));
    let mut stale = server.descriptor.clone();
    stale.instance = "previous-incarnation".into();
    let mut gapped = watch_as(
        &server,
        &stale,
        "fux/events+watch",
        json!({ "cursor": 999 }),
    );
    let gap = result(gapped.next(WAIT));
    assert_eq!(gap["gap"]["since"], 999);
    assert!(gap["events"].as_array().unwrap().is_empty());
    let resume = gap["gap"]["resume"].as_u64().unwrap();
    let retained = result(gapped.next(WAIT));
    let cursors: Vec<u64> = retained["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["cursor"].as_u64().unwrap())
        .collect();
    assert!(cursors.iter().all(|c| *c > resume), "{cursors:?}");
    assert!(cursors.contains(&after_spawn) && cursors.contains(&after_exit));

    // A scoped token streams its workspace only; naming another is refused.
    let minted = server
        .call(
            "fux/token.mint",
            json!({ "workspace": "default", "capabilities": ["read"] }),
        )
        .unwrap();
    let mut narrow = server.descriptor.clone();
    narrow.token = minted["token"].as_str().unwrap().to_owned();
    let refused = watch_as(
        &server,
        &narrow,
        "fux/events+watch",
        json!({ "workspace": "alpha" }),
    );
    assert_eq!(
        error_code(refused.next_owned()),
        i64::from(codes::UNAUTHORIZED)
    );
    let mut scoped = watch_as(&server, &narrow, "fux/events+watch", json!({}));
    spawn_sibling(&server, sibling);
    assert!(scoped.next(Duration::from_millis(300)).is_none());
    let default_pane = server.call("fux/workspace.list", json!({})).unwrap()["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["name"] == "default")
        .unwrap()["roots"][0]["panes"][0]["id"]
        .as_u64()
        .unwrap();
    spawn_pane(&server, default_pane);
    let item = result(scoped.next(WAIT));
    assert_eq!(item["events"][0]["workspace"], "default");

    // Revocation ends the token's streams.
    let info = server.call("fux/server.info", json!({})).unwrap();
    assert!(info["watches"].as_u64().unwrap() >= 1, "{info}");
    assert!(info["diagnostics"]["events_retained"].is_u64(), "{info}");
    server
        .call("fux/token.revoke", json!({ "revoke": narrow.token }))
        .unwrap();
    assert!(scoped.ended(WAIT), "revoked stream still open");
    assert!(!live.ended(Duration::from_millis(200)));
}

#[test]
fn events_watch_reports_a_gap_past_the_retained_window() {
    let server = Server::start_with(|world| {
        let limits = fux::model::Limits {
            event_log_entries: 2,
            ..world.resource::<fux::model::Limits>().clone()
        };
        world.insert_resource(fux::events::EventLog::new(&limits));
    });
    let pane = new_workspace(&server, "alpha");
    let mut live = watch(&server, "fux/events+watch", json!({}));
    spawn_pane(&server, pane);
    let first = result(live.next(WAIT));
    let after_spawn = first["events"][0]["cursor"].as_u64().unwrap();
    for _ in 0..3 {
        spawn_sibling(&server, pane);
        result(live.next(WAIT));
    }
    let mut behind = watch(
        &server,
        "fux/events+watch",
        json!({ "cursor": after_spawn }),
    );
    let gap = result(behind.next(WAIT));
    assert_eq!(gap["gap"]["since"], after_spawn, "{gap}");
    let resume = gap["gap"]["resume"].as_u64().unwrap();
    assert!(resume > after_spawn, "{gap}");
    assert!(gap["events"].as_array().unwrap().is_empty());
    let rest = result(behind.next(WAIT));
    let cursors: Vec<u64> = rest["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["cursor"].as_u64().unwrap())
        .collect();
    assert_eq!(cursors.len(), 2, "{rest}");
    assert!(cursors.iter().all(|c| *c > resume), "{cursors:?}");
}

#[test]
fn observe_watch_serves_public_events_only() {
    let server = Server::start();
    let pane = new_workspace(&server, "alpha");
    spawn_pane(&server, pane);

    let refused = watch(
        &server,
        "world.observe+watch",
        json!({ "event": "fux::model::components::Process" }),
    );
    assert_eq!(
        error_code(refused.next_owned()),
        i64::from(codes::UNAUTHORIZED)
    );
    let entity_scoped = watch(
        &server,
        "world.observe+watch",
        json!({ "event": "fux::events::PaneExited", "entity": 8 }),
    );
    assert_eq!(
        error_code(entity_scoped.next_owned()),
        i64::from(codes::INVALID)
    );

    let mut exits = watch(
        &server,
        "world.observe+watch",
        json!({ "event": "fux::events::PaneExited" }),
    );
    assert!(exits.next(Duration::from_millis(200)).is_none());
    exit_pane(&server, pane, 9);
    let item = result(exits.next(WAIT));
    assert_eq!(item["dropped"], 0);
    assert_eq!(item["events"][0]["pane"], pane, "{item}");
    assert_eq!(item["events"][0]["code"], 9);
    assert!(item["events"][0].get("entity").is_none(), "{item}");

    // Closing the stream despawns its observer: the next exit reaches nobody's buffer.
    let observers_open = server.with_world(|world| {
        world
            .query::<&fux::remote::watch::ObserveBuffer>()
            .iter(world)
            .count()
    });
    assert_eq!(observers_open, 1);
    drop(exits);
    let deadline = Instant::now() + WAIT;
    loop {
        let open = server.with_world(|world| {
            world
                .query::<&fux::remote::watch::ObserveBuffer>()
                .iter(world)
                .count()
        });
        if open == 0 {
            break;
        }
        assert!(Instant::now() < deadline, "observer never despawned");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn scoped_tokens_cannot_read_global_world_state() {
    let server = Server::start();
    let pane = new_workspace(&server, "beta");
    let rows = server
        .call(
            "world.query",
            json!({ "data": { "components": ["fux::remote::projection::PaneView"] } }),
        )
        .unwrap();
    let entity = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["components"]["fux::remote::projection::PaneView"]["id"] == pane)
        .unwrap()["entity"]
        .clone();
    let minted = server
        .call(
            "fux/token.mint",
            json!({ "workspace": "default", "capabilities": ["read"] }),
        )
        .unwrap();
    let mut narrow = server.descriptor.clone();
    narrow.token = minted["token"].as_str().unwrap().to_owned();

    for (method, params) in [
        (
            "world.query",
            json!({ "data": { "components": ["fux::remote::projection::PaneView"] } }),
        ),
        (
            "world.get_components",
            json!({ "entity": entity, "components": ["fux::remote::projection::PaneView"] }),
        ),
        ("world.list_components", json!({ "entity": entity })),
    ] {
        assert_eq!(
            code(client::call_with(&narrow, method, params)),
            codes::UNAUTHORIZED,
            "{method}"
        );
    }
    for (method, params) in [
        (
            "world.observe+watch",
            json!({ "event": "fux::events::SurfaceInput" }),
        ),
        (
            "world.get_components+watch",
            json!({ "entity": entity, "components": ["fux::remote::projection::PaneView"] }),
        ),
        ("world.list_components+watch", json!({ "entity": entity })),
    ] {
        let mut refused = watch_as(&server, &narrow, method, params);
        assert_eq!(
            error_code(refused.next(WAIT)),
            i64::from(codes::UNAUTHORIZED),
            "{method}"
        );
        assert!(refused.ended(WAIT), "{method} remained open");
    }
    assert_eq!(
        server.call("fux/server.info", json!({})).unwrap()["watches"],
        0
    );
}

#[test]
fn surface_input_streams_preserve_workspace_scope_and_revocation() {
    use fux::events::{SurfaceInput, SurfaceInputKind};
    use fux::model::{NodeId, ViewerId};

    let server = Server::start();
    new_workspace(&server, "beta");
    let mut narrow = server.descriptor.clone();
    narrow.token = server
        .call(
            "fux/token.mint",
            json!({ "workspace": "default", "capabilities": ["read"] }),
        )
        .unwrap()["token"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut global = server.descriptor.clone();
    global.token = server
        .call("fux/token.mint", json!({ "capabilities": ["read"] }))
        .unwrap()["token"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut scoped = watch_as(&server, &narrow, "fux/events+watch", json!({}));
    let mut observed = watch_as(
        &server,
        &global,
        "world.observe+watch",
        json!({ "event": "fux::events::SurfaceInput" }),
    );
    // A round trip through the dispatcher ensures both streams have opened.
    assert_eq!(
        server.call("fux/server.info", json!({})).unwrap()["watches"],
        2
    );
    server.with_world(|world| {
        for (workspace, surface, bytes) in [
            ("beta", 200, b"beta-secret".to_vec()),
            ("default", 100, b"default-key".to_vec()),
        ] {
            let scope = world.resource::<Ids>().workspace(workspace).unwrap();
            world.trigger(SurfaceInput {
                entity: scope,
                scope,
                surface: NodeId(surface),
                node: NodeId(surface),
                provider: "test".into(),
                provider_node: None,
                revision: 0,
                viewer: ViewerId(1),
                kind: SurfaceInputKind::Key,
                col: 0,
                row: 0,
                bytes,
            });
        }
    });
    let item = result(scoped.next(WAIT));
    let events = item["events"].as_array().unwrap();
    assert_eq!(events.len(), 1, "{item}");
    assert_eq!(events[0]["workspace"], "default");
    assert_eq!(events[0]["event"]["bytes"], json!(b"default-key".to_vec()));
    assert!(scoped.next(Duration::from_millis(200)).is_none());

    let public = result(observed.next(WAIT));
    let events = public["events"].as_array().unwrap();
    assert_eq!(events.len(), 2, "{public}");
    assert_eq!(events[0]["bytes"], json!(b"beta-secret".to_vec()));
    assert_eq!(events[1]["bytes"], json!(b"default-key".to_vec()));

    server
        .call("fux/token.revoke", json!({ "revoke": global.token }))
        .unwrap();
    assert!(observed.ended(WAIT), "revoked observer still open");
    assert!(!scoped.ended(Duration::from_millis(200)));
    server
        .call("fux/token.revoke", json!({ "revoke": narrow.token }))
        .unwrap();
    assert!(scoped.ended(WAIT), "revoked workspace stream still open");
}

#[test]
fn builtin_watches_are_wrapped_over_projections() {
    let server = Server::start();
    let pane = new_workspace(&server, "alpha");
    let rows = server
        .call(
            "world.query",
            json!({ "data": { "components": ["fux::remote::projection::PaneView"] } }),
        )
        .unwrap();
    let row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["components"]["fux::remote::projection::PaneView"]["id"] == pane)
        .unwrap();
    let entity = row["entity"].clone();

    let refused = watch(
        &server,
        "world.get_components+watch",
        json!({ "entity": entity, "components": ["fux::model::components::Process"] }),
    );
    assert_eq!(
        error_code(refused.next_owned()),
        i64::from(codes::UNAUTHORIZED)
    );

    let mut reader = server.descriptor.clone();
    reader.token = server
        .call("fux/token.mint", json!({ "capabilities": ["read"] }))
        .unwrap()["token"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut view = watch_as(
        &server,
        &reader,
        "world.get_components+watch",
        json!({ "entity": entity, "components": ["fux::remote::projection::PaneView"] }),
    );
    let mut listed = watch_as(
        &server,
        &reader,
        "world.list_components+watch",
        json!({ "entity": entity }),
    );
    // Let both streams open before the change so neither misses it.
    assert!(view.next(Duration::from_millis(200)).is_none());
    spawn_pane(&server, pane);
    let changed = result(view.next(WAIT));
    assert_eq!(
        changed["components"]["fux::remote::projection::PaneView"]["state"], "live",
        "{changed}"
    );
    // Nothing was added or removed on the projection entity: silence, not a leak of the
    // relationship components it also carries.
    assert!(listed.next(Duration::from_millis(300)).is_none());
    server
        .call("fux/token.revoke", json!({ "revoke": reader.token }))
        .unwrap();
    assert!(view.ended(WAIT), "revoked component watch still open");
    assert!(
        listed.ended(WAIT),
        "revoked component-list watch still open"
    );
}

#[test]
fn transport_rules_hold() {
    let server = Server::start();

    // `+watch` in a batch is refused by the transport; the other member is served.
    let batch = Http::post(
        &server,
        &json!([
            rpc(
                "fux/events+watch",
                json!({ "token": server.descriptor.token })
            ),
            rpc(
                "fux/server.info",
                json!({ "token": server.descriptor.token })
            ),
        ]),
    )
    .complete();
    let batch = batch.as_array().unwrap();
    assert_eq!(batch.len(), 2);
    assert_eq!(
        batch[0]["error"]["code"],
        i64::from(bevy_remote::error_codes::INVALID_REQUEST)
    );
    assert_eq!(batch[1]["result"]["name"], "test");

    // Unknown names, streaming or not, are answered and never stall the next request.
    let unknown = watch(&server, "fux/nope+watch", json!({}));
    assert_eq!(
        error_code(unknown.next_owned()),
        i64::from(bevy_remote::error_codes::METHOD_NOT_FOUND)
    );
    assert_eq!(
        code(server.call("fux/nope", json!({}))),
        bevy_remote::error_codes::METHOD_NOT_FOUND
    );
    let info = server.call("fux/server.info", json!({})).unwrap();
    assert_eq!(info["watches"], 0);

    // A stream without a token is refused before any state exists.
    let anonymous = Http::post(&server, &rpc("fux/events+watch", json!({})));
    assert_eq!(
        error_code(anonymous.next_owned()),
        i64::from(codes::UNAUTHORIZED)
    );
}
