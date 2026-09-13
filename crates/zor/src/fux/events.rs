//! Typed retained multiplexer events. Consumers decide what the evidence means for a task.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{path::Path, time::Instant};

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Cursor {
    pub stream: u64,
    pub sequence: u64,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Event {
    #[serde(rename = "workspace.changed")]
    WorkspaceChanged { id: u64, cursor: Cursor },
    #[serde(rename = "pane.opened")]
    PaneOpened {
        id: u64,
        cursor: Cursor,
        pane: u32,
        tab: u32,
        command: Vec<String>,
    },
    #[serde(rename = "pane.closed")]
    PaneClosed {
        id: u64,
        cursor: Cursor,
        pane: u32,
        #[serde(deserialize_with = "Option::deserialize")]
        exit_status: Option<i32>,
    },
    #[serde(rename = "pane.output")]
    PaneOutput {
        id: u64,
        cursor: Cursor,
        pane: u32,
        /// The output sequence after the change that produced the event.
        seq: u64,
    },
    #[serde(rename = "tab.opened")]
    TabOpened {
        id: u64,
        cursor: Cursor,
        tab: u32,
        name: String,
    },
    #[serde(rename = "tab.closed")]
    TabClosed { id: u64, tab: u32, cursor: Cursor },
}
impl Event {
    fn cursor(&self) -> Cursor {
        match self {
            Self::WorkspaceChanged { cursor, .. }
            | Self::PaneOpened { cursor, .. }
            | Self::PaneClosed { cursor, .. }
            | Self::PaneOutput { cursor, .. }
            | Self::TabOpened { cursor, .. }
            | Self::TabClosed { cursor, .. } => *cursor,
        }
    }
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Replay {
    pub cursor: Cursor,
    pub events: Vec<Event>,
}
#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case", deny_unknown_fields)]
enum Reply {
    Completed { id: u64, result: Payload },
    Failed { id: u64, error: Failure },
}
#[derive(Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "kebab-case",
    deny_unknown_fields
)]
enum Payload {
    Events(Replay),
}
use super::error::RemoteFailure as Failure;

#[derive(Serialize)]
#[serde(tag = "command", rename_all = "kebab-case")]
enum Request<'a> {
    Events {
        id: u64,
        instance: &'a str,
        after: Cursor,
    },
}
fn decode(value: serde_json::Value, after: Cursor) -> Result<Replay> {
    super::error::reply(|| {
        let replay = match serde_json::from_value(value).context("invalid retained-event reply")? {
            Reply::Completed {
                id,
                result: Payload::Events(replay),
            } => {
                ensure!(id == 1, "event reply ID mismatch");
                replay
            }
            Reply::Failed { id, error } => {
                ensure!(id == 1, "event failure ID mismatch");
                return Err(error.into());
            }
        };
        ensure!(
            after.stream != 0 && replay.cursor.stream != 0,
            "invalid event stream"
        );
        if replay.cursor.stream != after.stream {
            return Err(super::error::stale("event stream changed"));
        }
        ensure!(replay.events.len() <= 1024, "event replay limit exceeded");
        let mut previous = after.sequence;
        for event in &replay.events {
            let cursor = event.cursor();
            ensure!(
                cursor.stream == after.stream && previous.checked_add(1) == Some(cursor.sequence),
                "event gap, duplicate or reordered evidence"
            );
            previous = cursor.sequence;
            match event {
                Event::PaneOpened { pane, tab, .. } => {
                    ensure!(*pane != 0 && *tab != 0, "invalid opened pane identity")
                }
                Event::PaneClosed { pane, .. } | Event::PaneOutput { pane, .. } => {
                    ensure!(*pane != 0, "invalid pane identity")
                }
                Event::TabOpened { tab, .. } | Event::TabClosed { tab, .. } => {
                    ensure!(*tab != 0, "invalid tab identity")
                }
                Event::WorkspaceChanged { .. } => {}
            }
        }
        ensure!(
            previous == replay.cursor.sequence,
            "incomplete event replay"
        );
        Ok(replay)
    })
}
pub(crate) fn replay(
    socket: &Path,
    instance: &str,
    after: Cursor,
    deadline: Instant,
) -> Result<Replay> {
    let request = serde_json::to_value(Request::Events {
        id: 1,
        instance,
        after,
    })?;
    decode(super::request_until(socket, request, deadline)?, after)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    #[test]
    fn replay_requires_complete_ordered_evidence_in_one_stream() -> Result<()> {
        let after = Cursor {
            stream: 2,
            sequence: 10,
        };
        let expected: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/control/request_events_client.json"
        ))?;
        assert_eq!(
            serde_json::to_value(Request::Events {
                id: 1,
                instance: "owner",
                after
            })?,
            expected
        );
        let reply: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/control/reply_events_client.json"
        ))?;
        assert_eq!(decode(reply.clone(), after)?.events.len(), 2);
        for (path, value) in [
            ("/id", json!(2)),
            ("/result/kind", json!("listing")),
            ("/result/value/cursor/stream", json!(3)),
            ("/result/value/cursor/sequence", json!(13)),
            ("/result/value/events/0/cursor/sequence", json!(12)),
            ("/result/value/events/1/cursor/sequence", json!(11)),
            ("/result/value/events/1/cursor/stream", json!(3)),
            ("/result/value/events/0/pane", json!(0)),
            ("/result/value/events/1/event", json!("unknown.event")),
        ] {
            let mut invalid = reply.clone();
            *invalid.pointer_mut(path).context("fixture field")? = value;
            assert!(decode(invalid, after).is_err(), "accepted {path}");
        }
        let empty = json!({"status":"completed","id":1,"result":{"kind":"events","value":{"cursor":{"stream":2,"sequence":10},"events":[]}}});
        assert!(decode(empty, after)?.events.is_empty());
        assert!(
            decode(
                json!({"status":"failed","id":1,"error":{"code":"gap","message":"expired"}}),
                after
            )
            .is_err()
        );
        Ok(())
    }
}
