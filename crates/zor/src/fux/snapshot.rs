//! Typed coherent workspace snapshots; these are observations, not task policy.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    time::Instant,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceSummary {
    pub event_cursor: EventCursor,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub focused: bool,
    pub viewers: u32,
    pub tabs: Vec<TabSummary>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TabSummary {
    pub id: u32,
    pub index: u32,
    pub name: String,
    pub focused: bool,
    pub panes: Vec<PaneSummary>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaneSummary {
    #[serde(default)]
    pub right_click: RightClickPolicy,
    pub id: u32,
    pub command: Vec<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub pid: Option<u32>,
    pub cwd: PathBuf,
    pub title: String,
    /// Manual label, independent of the application title; absent when cleared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The output sequence: advances whenever the visible screen, cursor, modes, title or exit
    /// status changed; `capture` and `pane.output` report the same counter.
    pub seq: u64,
    /// Terminal capture revision; independent of the grid sequence.
    pub revision: u64,
    pub input_sequence: u64,
    pub fixed_workspace: bool,
    pub geometry: Rect,
    pub focused: bool,
    pub cursor: Cursor,
    pub modes: PaneModes,
    #[serde(deserialize_with = "Option::deserialize")]
    pub exit_status: Option<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventCursor {
    pub stream: u64,
    pub sequence: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cursor {
    pub row: u16,
    pub column: u16,
    pub hidden: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaneModes {
    pub alternate_screen: bool,
    pub application_keypad: bool,
    pub application_cursor: bool,
    pub bracketed_paste: bool,
    pub mouse_mode: MouseMode,
    pub mouse_encoding: MouseEncoding,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MouseMode {
    #[default]
    None,
    Press,
    PressRelease,
    ButtonMotion,
    AnyMotion,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MouseEncoding {
    #[default]
    Default,
    Utf8,
    Sgr,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RightClickPolicy {
    #[default]
    Auto,
    Fux,
    Pane,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Listing {
    pub instance: String,
    pub workspaces: Vec<WorkspaceSummary>,
}
#[derive(Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "kebab-case",
    deny_unknown_fields
)]
enum Payload {
    Listing(Listing),
}
#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case", deny_unknown_fields)]
enum Reply {
    Completed { id: u64, result: Payload },
    Failed { id: u64, error: Failure },
}
use super::error::RemoteFailure as Failure;

#[derive(Serialize)]
#[serde(tag = "command", rename_all = "kebab-case")]
enum Request<'a> {
    List { id: u64, instance: Option<&'a str> },
}

fn decode(value: serde_json::Value, expected: Option<&str>) -> Result<Listing> {
    super::error::reply(|| {
        let listing = match serde_json::from_value(value).context("invalid list reply")? {
            Reply::Completed {
                id,
                result: Payload::Listing(listing),
            } => {
                ensure!(id == 1, "list response ID mismatch");
                listing
            }
            Reply::Failed { id, error } => {
                ensure!(id == 1, "list failure ID mismatch");
                return Err(error.into());
            }
        };
        ensure!(
            !listing.instance.is_empty() && listing.instance.len() <= 128,
            "invalid server instance"
        );
        if expected.is_some_and(|expected| expected != listing.instance) {
            return Err(super::error::stale("server instance changed"));
        }
        ensure!(listing.workspaces.len() == 1, "expected one workspace");
        let mut panes = std::collections::BTreeSet::new();
        let mut tabs = std::collections::BTreeSet::new();
        for workspace in &listing.workspaces {
            ensure!(
                super::endpoint::valid_name(&workspace.name),
                "invalid workspace name"
            );
            ensure!(
                workspace.event_cursor.stream != 0,
                "invalid workspace lifetime"
            );
            ensure!(workspace.tabs.len() <= 32, "tab limit exceeded");
            for tab in &workspace.tabs {
                ensure!(
                    tab.id != 0 && tabs.insert(tab.id),
                    "invalid or duplicate tab identity"
                );
                for pane in &tab.panes {
                    ensure!(
                        pane.id != 0 && panes.insert(pane.id),
                        "invalid or duplicate pane identity"
                    );
                    ensure!(panes.len() <= 128, "pane limit exceeded");
                    ensure!(
                        pane.pid.is_none_or(|pid| pid > 0),
                        "invalid pane process identity"
                    );
                }
            }
        }
        Ok(listing)
    })
}
pub(crate) fn list(socket: &Path, expected: Option<&str>, deadline: Instant) -> Result<Listing> {
    decode(
        super::request_until(
            socket,
            serde_json::to_value(Request::List {
                id: 1,
                instance: expected,
            })?,
            deadline,
        )?,
        expected,
    )
}

#[cfg(test)]
pub(crate) fn fixture_listing() -> Result<Listing> {
    decode(tests::reply(), Some("owner"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    pub(super) fn reply() -> serde_json::Value {
        let modes = json!({"alternate_screen":false,"application_keypad":false,"application_cursor":false,
            "bracketed_paste":false,"mouse_mode":"none","mouse_encoding":"default"});
        let pane = json!({"id":1,"command":["sh"],"pid":42,"cwd":"/tmp","title":"shell","seq":4,"revision":5,
            "input_sequence":6,"fixed_workspace":false,"geometry":{"x":0,"y":0,"width":80,"height":24},
            "focused":true,"cursor":{"row":0,"column":0,"hidden":false},"exit_status":null,"modes":modes});
        let workspace = json!({"name":"main","event_cursor":{"stream":2,"sequence":3},
            "focused":true,"viewers":0,"tabs":[{"id":1,"index":0,"name":"main","focused":true,"panes":[pane]}]});
        json!({"status":"completed","id":1,"result":{"kind":"listing","value":{
            "instance":"owner","workspaces":[workspace]}}})
    }

    #[test]
    fn listing_validates_envelope_identity_and_required_observations() -> Result<()> {
        assert_eq!(decode(reply(), Some("owner"))?.instance, "owner");
        assert!(decode(reply(), Some("replacement")).is_err());
        for (path, replacement) in [
            ("/id", json!(2)),
            ("/status", json!("accepted")),
            ("/result/kind", json!("cells")),
            ("/result/value/instance", json!("")),
            ("/result/value/workspaces/0/name", json!("../escape")),
            ("/result/value/workspaces/0/event_cursor/stream", json!(0)),
            ("/result/value/workspaces/0/tabs/0/panes/0/id", json!(0)),
            ("/result/value/workspaces/0/tabs/0/panes/0/pid", json!(0)),
        ] {
            let mut invalid = reply();
            *invalid.pointer_mut(path).context("test field missing")? = replacement;
            assert!(decode(invalid, None).is_err(), "accepted {path}");
        }
        for field in ["pid", "exit_status", "revision", "input_sequence"] {
            let mut invalid = reply();
            invalid
                .pointer_mut("/result/value/workspaces/0/tabs/0/panes/0")
                .and_then(serde_json::Value::as_object_mut)
                .context("pane missing")?
                .remove(field);
            assert!(decode(invalid, None).is_err(), "accepted missing {field}");
        }
        Ok(())
    }
}
