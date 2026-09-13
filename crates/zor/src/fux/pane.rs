//! Generic pane mutations; task authority and retry policy remain in callers.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{path::Path, time::Instant};

#[derive(Clone, Copy)]
pub(crate) enum Action {
    Focus,
    Kill,
}

#[derive(Serialize)]
#[serde(tag = "command", rename_all = "kebab-case")]
enum Request<'a> {
    Workspace {
        id: u64,
        instance: &'a str,
        stream: u64,
        action: WorkspaceAction<'a>,
    },
    Split {
        id: u64,
        instance: &'a str,
        axis: Axis,
        #[serde(flatten)]
        spawn: Spawn<'a>,
    },
    Focus {
        id: u64,
        instance: &'a str,
        target: FocusTarget,
    },
    Kill {
        id: u64,
        instance: &'a str,
        pane: u32,
    },
}
#[derive(Serialize)]
#[serde(rename_all = "kebab-case")]
enum Axis {
    Horizontal,
}

#[derive(Serialize)]
pub(crate) struct Spawn<'a> {
    pub stream: u64,
    pub cwd: Option<&'a Path>,
    pub argv: &'a [String],
    pub env: &'a [(String, String)],
    pub rows: Option<u16>,
    pub columns: Option<u16>,
    pub fixed_workspace: bool,
    pub final_retain_ms: u64,
}
#[derive(Serialize)]
#[serde(rename_all = "kebab-case")]
enum WorkspaceAction<'a> {
    Kill { name: &'a str },
}

/// Only a validated failure envelope exposes a retry-relevant remote code.
pub(crate) fn remote_code(error: &anyhow::Error) -> Option<&str> {
    super::error::remote_code(error)
}

pub(crate) fn kill_workspace(
    socket: &Path,
    instance: &str,
    stream: u64,
    name: &str,
    deadline: Instant,
) -> Result<()> {
    ensure!(
        !instance.is_empty() && stream != 0,
        "missing workspace identity"
    );
    ensure!(super::endpoint::valid_name(name), "invalid workspace name");
    let request = Request::Workspace {
        id: 1,
        instance,
        stream,
        action: WorkspaceAction::Kill { name },
    };
    decode_workspace(
        super::request_until(socket, serde_json::to_value(request)?, deadline)?,
        name,
    )
}

fn decode_workspace(reply: serde_json::Value, expected: &str) -> Result<()> {
    super::error::reply(|| {
        match serde_json::from_value(reply).context("invalid workspace mutation reply")? {
            Reply::Completed {
                id,
                result: Payload::Workspace { name },
            } => {
                ensure!(
                    id == 1 && name == expected,
                    "workspace cleanup identity mismatch"
                );
                Ok(())
            }
            Reply::Failed { id, error } => {
                ensure!(id == 1, "workspace cleanup failure ID mismatch");
                Err(error.into())
            }
            Reply::Completed { .. } => anyhow::bail!("expected workspace cleanup result"),
        }
    })
}

#[derive(Serialize)]
struct FocusTarget {
    pane: u32,
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
    Unit,
    Pane { pane: u32 },
    Workspace { name: String },
}
use super::error::RemoteFailure as Failure;

fn decode(reply: serde_json::Value) -> Result<()> {
    super::error::reply(|| {
        match serde_json::from_value(reply).context("invalid pane mutation reply")? {
            Reply::Completed {
                id,
                result: Payload::Unit,
            } => {
                ensure!(id == 1, "pane mutation response ID mismatch");
                Ok(())
            }
            Reply::Failed { id, error } => {
                ensure!(id == 1, "pane mutation failure ID mismatch");
                Err(error.into())
            }
            Reply::Completed { .. } => anyhow::bail!("expected unit pane mutation result"),
        }
    })
}

pub(crate) fn split(
    socket: &Path,
    instance: &str,
    spawn: Spawn<'_>,
    deadline: Instant,
) -> Result<u32> {
    ensure!(
        !instance.is_empty() && spawn.stream != 0,
        "missing workspace identity"
    );
    let request = Request::Split {
        id: 1,
        instance,
        axis: Axis::Horizontal,
        spawn,
    };
    decode_pane(super::request_until(
        socket,
        serde_json::to_value(request)?,
        deadline,
    )?)
}

fn decode_pane(reply: serde_json::Value) -> Result<u32> {
    super::error::reply(|| {
        match serde_json::from_value(reply).context("invalid pane identity reply")? {
            Reply::Completed {
                id,
                result: Payload::Pane { pane },
            } => {
                ensure!(id == 1 && pane != 0, "invalid request or pane identity");
                Ok(pane)
            }
            Reply::Completed { .. } => anyhow::bail!("expected pane identity result"),
            Reply::Failed { id, error } => {
                ensure!(id == 1, "pane failure ID mismatch");
                Err(error.into())
            }
        }
    })
}

pub(crate) fn act(
    socket: &Path,
    instance: &str,
    pane: u32,
    action: Action,
    deadline: Instant,
) -> Result<()> {
    ensure!(pane != 0 && !instance.is_empty(), "missing pane identity");
    let request = match action {
        Action::Focus => Request::Focus {
            id: 1,
            instance,
            target: FocusTarget { pane },
        },
        Action::Kill => Request::Kill {
            id: 1,
            instance,
            pane,
        },
    };
    let reply = super::request_until(socket, serde_json::to_value(request)?, deadline)?;
    match action {
        Action::Kill => decode(reply),
        Action::Focus => decode_focus(reply, pane),
    }
}

fn decode_focus(reply: serde_json::Value, expected: u32) -> Result<()> {
    super::error::reply(|| {
        ensure!(
            decode_pane(reply)? == expected,
            "focused pane identity mismatch"
        );
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn focus_requires_the_requested_pane_and_matching_envelope() -> Result<()> {
        let request: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/control/request_focus_client.json"
        ))?;
        assert_eq!(
            serde_json::to_value(Request::Focus {
                id: 1,
                instance: "owner",
                target: FocusTarget { pane: 4 }
            })?,
            request
        );
        let reply: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/control/reply_focus_client.json"
        ))?;
        decode_focus(reply.clone(), 4)?;
        assert!(decode_focus(reply.clone(), 5).is_err());
        let mut wrong_id = reply;
        *wrong_id.get_mut("id").context("reply id")? = json!(2);
        assert!(decode_focus(wrong_id, 4).is_err());
        assert!(
            decode_focus(
                json!({"status":"completed","id":1,"result":{"kind":"unit"}}),
                4
            )
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn split_fixtures_match_the_request_and_require_a_nonzero_pane() -> Result<()> {
        let expected: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/control/request_split_client.json"
        ))?;
        let argv = vec!["/bin/sh".into(), "-c".into(), "printf literal".into()];
        let env = vec![("ROLE".into(), "fixture".into())];
        let spawn = Spawn {
            stream: 2,
            cwd: Some(Path::new("/tmp")),
            argv: &argv,
            env: &env,
            rows: Some(24),
            columns: Some(80),
            fixed_workspace: true,
            final_retain_ms: 60000,
        };
        assert_eq!(
            serde_json::to_value(Request::Split {
                id: 1,
                instance: "owner",
                axis: Axis::Horizontal,
                spawn
            })?,
            expected
        );
        let reply: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/control/reply_split_client.json"
        ))?;
        assert_eq!(decode_pane(reply.clone())?, 4);
        for (path, value) in [
            ("/id", json!(2)),
            ("/result/value/pane", json!(0)),
            ("/result/value/pane", json!(null)),
            ("/result/kind", json!("unit")),
        ] {
            let mut invalid = reply.clone();
            *invalid.pointer_mut(path).context("fixture field")? = value;
            assert!(decode_pane(invalid).is_err(), "accepted {path}");
        }
        Ok(())
    }
    #[test]
    fn workspace_cleanup_is_scoped_and_remote_codes_require_valid_envelopes() -> Result<()> {
        assert_eq!(
            serde_json::to_value(Request::Workspace {
                id: 1,
                instance: "owner",
                stream: 2,
                action: WorkspaceAction::Kill { name: "run" },
            })?,
            json!({"command":"workspace","id":1,"instance":"owner","stream":2,
                    "action":{"kill":{"name":"run"}}})
        );
        let completed = json!({"status":"completed","id":1,"result":{"kind":"workspace","value":{"name":"run"}}});
        decode_workspace(completed.clone(), "run")?;
        assert!(decode_workspace(completed, "replacement").is_err());
        assert!(
            decode_workspace(
                json!({"status":"completed","id":1,"result":{"kind":"unit"}}),
                "run"
            )
            .is_err()
        );
        let valid = json!({"status":"failed","id":1,"error":{"code":"not-found","message":"gone"}});
        let error = decode(valid).err().context("expected remote failure")?;
        assert_eq!(remote_code(&error), Some("not-found"));
        for invalid in [
            json!({"status":"completed","id":1,"error":{"code":"not-found","message":"gone"}}),
            json!({"status":"failed","id":2,"error":{"code":"not-found","message":"gone"}}),
            json!({"status":"failed","id":1,"error":{"code":"not-found"}}),
        ] {
            let error = decode(invalid).err().context("expected invalid envelope")?;
            assert_eq!(remote_code(&error), None);
        }
        Ok(())
    }

    #[test]
    fn mutation_acknowledgement_requires_unit_and_matching_id() {
        assert!(decode(json!({"status":"completed","id":1,"result":{"kind":"unit"}})).is_ok());
        for reply in [
            json!({"status":"completed","id":2,"result":{"kind":"unit"}}),
            json!({"status":"completed","id":1,"result":{"kind":"pane","value":{"pane":1}}}),
            json!({"status":"accepted","id":1}),
            json!({"status":"failed","id":1,"error":{"code":"not-found","message":"gone"}}),
        ] {
            assert!(decode(reply).is_err());
        }
    }
}
