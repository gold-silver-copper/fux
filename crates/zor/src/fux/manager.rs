//! Typed manager operations over the existing authenticated, bounded transport.
//! Process authority and retry decisions remain with the task policy layer.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{path::Path, time::Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Operation {
    PaneLocation,
    ReleasePanePin,
    Final,
    InputStatus,
}

#[derive(Serialize)]
#[serde(tag = "request", rename_all = "kebab-case")]
enum Request<'a> {
    PaneLocation {
        instance: &'a str,
        pane: u32,
    },
    InputStatus {
        instance: &'a str,
        pane: u32,
        operation: u64,
    },
    Final {
        instance: &'a str,
        pane: u32,
    },
    ReleasePanePin {
        instance: &'a str,
        pane: u32,
        pid: u32,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Location {
    pub instance: String,
    pub pane: u32,
    pub pid: u32,
    pub accepts_input: bool,
    pub workspace: String,
    pub stream: u64,
    pub origin_workspace: String,
    pub origin_stream: u64,
    pub tab: u32,
    pub layout_generation: u64,
}

#[derive(Debug, Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "kebab-case",
    deny_unknown_fields
)]
enum Payload {
    Unit,
    Input { receipt: super::input::Receipt },
    Final { record: FinalRecord },
    PaneLocation { location: Location },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FinalRecord {
    pub pane: u32,
    pub workspace: String,
    pub stream: u64,
    pub command: Vec<String>,
    pub cwd: std::path::PathBuf,
    #[serde(deserialize_with = "Option::deserialize")]
    pub exit_status: Option<u32>,
    pub input_sequence: u64,
    pub capture: Capture,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Capture {
    pub text: String,
    pub revision: u64,
    pub rows: u16,
    pub columns: u16,
    pub scrollback_offset: u32,
    pub title: String,
    pub progress: Option<(u8, u8)>,
    pub unchanged: bool,
    pub truncated: bool,
}

#[derive(Debug)]
pub(crate) enum FinalOutcome {
    Record(FinalRecord),
    Pending,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoteError {
    code: String,
    message: String,
}
impl std::fmt::Display for RemoteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for RemoteError {}

/// Only validated remote failures expose a code; malformed envelopes never do.
pub(crate) fn remote_code(error: &anyhow::Error) -> Option<&str> {
    error
        .downcast_ref::<RemoteError>()
        .map(|error| error.code.as_str())
}

#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case", deny_unknown_fields)]
enum ControlReply {
    Completed { id: u64, result: Payload },
    Failed { id: u64, error: RemoteError },
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    reply: Operation,
    result: ControlReply,
}

fn decode(value: Value, expected: Operation) -> Result<Payload> {
    let envelope: Envelope = serde_json::from_value(value).context("invalid manager reply")?;
    ensure!(envelope.reply == expected, "unexpected manager operation");
    match envelope.result {
        ControlReply::Completed { id, result } => {
            ensure!(id == 0, "unexpected manager request identity");
            ensure!(
                matches!(
                    (&result, expected),
                    (Payload::Unit, Operation::ReleasePanePin)
                        | (Payload::Input { .. }, Operation::InputStatus)
                        | (Payload::Final { .. }, Operation::Final)
                        | (Payload::PaneLocation { .. }, Operation::PaneLocation)
                ),
                "unexpected manager result kind"
            );
            Ok(result)
        }
        ControlReply::Failed { id, error } => {
            ensure!(id == 0, "unexpected manager request identity");
            Err(error.into())
        }
    }
}

pub(crate) fn locate(
    runtime: &Path,
    instance: &str,
    pane: u32,
    deadline: Instant,
) -> Result<Location> {
    let request = serde_json::to_value(Request::PaneLocation { instance, pane })?;
    let reply = super::request_until(&runtime.join("manager.sock"), request, deadline)?;
    match decode(reply, Operation::PaneLocation)? {
        Payload::PaneLocation { location } => Ok(location),
        _ => anyhow::bail!("unexpected location result"),
    }
}

pub(crate) fn release_pin(
    runtime: &Path,
    instance: &str,
    pane: u32,
    pid: u32,
    deadline: Instant,
) -> Result<()> {
    let request = serde_json::to_value(Request::ReleasePanePin {
        instance,
        pane,
        pid,
    })?;
    let reply = super::request_until(&runtime.join("manager.sock"), request, deadline)?;
    decode(reply, Operation::ReleasePanePin)?;
    Ok(())
}

pub(crate) fn final_record(
    runtime: &Path,
    instance: &str,
    pane: u32,
    deadline: Instant,
) -> Result<FinalOutcome> {
    let request = serde_json::to_value(Request::Final { instance, pane })?;
    let reply = super::request_until(&runtime.join("manager.sock"), request, deadline)?;
    decode_final(reply)
}

fn decode_final(reply: Value) -> Result<FinalOutcome> {
    match decode(reply, Operation::Final) {
        Ok(Payload::Final { record }) => Ok(FinalOutcome::Record(record)),
        Ok(_) => anyhow::bail!("unexpected final result"),
        Err(error) => {
            match error
                .downcast_ref::<RemoteError>()
                .map(|error| error.code.as_str())
            {
                Some("pending") => Ok(FinalOutcome::Pending),
                Some("evicted") => anyhow::bail!(
                    "fux dropped the final record under load before its retention elapsed; the exit evidence is lost and cannot be retried ({error})"
                ),
                _ => Err(error.context("final evidence unavailable")),
            }
        }
    }
}

pub(crate) fn input_status(
    runtime: &Path,
    instance: &str,
    pane: u32,
    operation: u64,
    deadline: Instant,
) -> Result<super::input::Receipt> {
    let request = serde_json::to_value(Request::InputStatus {
        instance,
        pane,
        operation,
    })?;
    let reply = super::request_until(&runtime.join("manager.sock"), request, deadline)?;
    match decode(reply, Operation::InputStatus)? {
        Payload::Input { receipt } => Ok(receipt),
        _ => anyhow::bail!("unexpected input status result"),
    }
}

pub(super) fn decode_input_control(reply: Value) -> Result<super::input::Receipt> {
    match serde_json::from_value::<ControlReply>(reply).context("invalid input reply")? {
        ControlReply::Completed {
            id,
            result: Payload::Input { receipt },
        } => {
            ensure!(id == 1, "unexpected input request identity");
            Ok(receipt)
        }
        ControlReply::Failed { id, error } => {
            ensure!(id == 1, "unexpected input request identity");
            Err(error.into())
        }
        _ => anyhow::bail!("unexpected input result kind"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn producer_fixtures_decode_and_requests_match() -> Result<()> {
        let fixture = |name: &str| -> Result<Value> {
            Ok(serde_json::from_slice(&std::fs::read(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fixtures/manager")
                    .join(format!("{name}_zor.json")),
            )?)?)
        };
        for (name, request, operation) in [
            (
                "pane_location",
                Request::PaneLocation {
                    instance: "owner",
                    pane: 2,
                },
                Operation::PaneLocation,
            ),
            (
                "release_pane_pin",
                Request::ReleasePanePin {
                    instance: "owner",
                    pane: 2,
                    pid: 3,
                },
                Operation::ReleasePanePin,
            ),
            (
                "input_status",
                Request::InputStatus {
                    instance: "owner",
                    pane: 2,
                    operation: 9,
                },
                Operation::InputStatus,
            ),
            (
                "final",
                Request::Final {
                    instance: "owner",
                    pane: 2,
                },
                Operation::Final,
            ),
        ] {
            assert_eq!(
                serde_json::to_value(request)?,
                fixture(&format!("request_{name}"))?
            );
            decode(fixture(&format!("reply_{name}"))?, operation)?;
        }
        assert!(matches!(
            decode_final(fixture("reply_final_pending")?)?,
            FinalOutcome::Pending
        ));
        for code in ["conflict", "expired", "unknown", "evicted"] {
            assert!(decode_final(fixture(&format!("reply_final_{code}"))?).is_err());
        }
        Ok(())
    }

    #[test]
    fn final_evidence_requires_correlated_complete_record_and_classifies_failures() -> Result<()> {
        let record = json!({"pane":2,"workspace":"origin","stream":7,"command":["cat"],
            "cwd":"/tmp","exit_status":0,"input_sequence":3,
            "capture":{"text":"done","revision":4,"rows":24,"columns":80,
                "scrollback_offset":0,"title":"","progress":null,"unchanged":false,"truncated":false}});
        let good = json!({"reply":"final","result":{"id":0,"status":"completed",
            "result":{"kind":"final","value":{"record":record}}}});
        let FinalOutcome::Record(decoded) = decode_final(good.clone())? else {
            anyhow::bail!("completed final was pending");
        };
        assert_eq!(serde_json::to_value(decoded)?, record);
        for (pointer, value) in [
            ("/result/id", json!(1)),
            ("/reply", json!("pane-location")),
            ("/result/result/kind", json!("input")),
            ("/result/result/value/record/exit_status", json!(-1)),
            ("/result/result/value/record/input_sequence", json!("3")),
        ] {
            let mut invalid = good.clone();
            *invalid.pointer_mut(pointer).context("fixture pointer")? = value;
            assert!(decode_final(invalid).is_err(), "accepted {pointer}");
        }
        let mut missing = good;
        missing
            .pointer_mut("/result/result/value/record")
            .and_then(Value::as_object_mut)
            .context("record")?
            .remove("exit_status");
        assert!(
            decode_final(missing).is_err(),
            "missing exit status became null"
        );
        let failed = |code| {
            json!({"reply":"final","result":{"id":0,"status":"failed",
            "error":{"code":code,"message":"m"}}})
        };
        assert!(matches!(
            decode_final(failed("pending"))?,
            FinalOutcome::Pending
        ));
        let error = decode_final(failed("evicted"))
            .err()
            .context("eviction accepted")?
            .to_string();
        assert!(
            error.contains("dropped the final record under load")
                && error.contains("cannot be retried")
        );
        for code in ["expired", "unknown", "conflict"] {
            let error = decode_final(failed(code))
                .err()
                .context("unavailable evidence accepted")?;
            assert!(format!("{error:#}").contains(code));
        }
        let mut stale_pending = failed("pending");
        *stale_pending.pointer_mut("/result/id").context("id")? = json!(4);
        assert!(
            decode_final(stale_pending).is_err(),
            "stale pending became retryable"
        );
        assert_eq!(
            serde_json::to_value(Request::Final {
                instance: "owner",
                pane: 2
            })?,
            json!({"request":"final","instance":"owner","pane":2})
        );
        Ok(())
    }

    #[test]
    fn release_requires_exact_operation_id_and_payload() -> Result<()> {
        let good = json!({"reply":"release-pane-pin","result":{"id":0,"status":"completed","result":{"kind":"unit"}}});
        assert!(decode(good.clone(), Operation::ReleasePanePin).is_ok());
        assert!(decode(good.clone(), Operation::PaneLocation).is_err());
        for (pointer, value) in [
            ("/result/id", json!(1)),
            ("/result/status", json!("accepted")),
            ("/result/result/kind", json!("final")),
            ("/reply", json!("names")),
        ] {
            let mut invalid = good.clone();
            *invalid.pointer_mut(pointer).context("fixture pointer")? = value;
            assert!(decode(invalid, Operation::ReleasePanePin).is_err());
        }
        assert!(decode(json!({"reply":"release-pane-pin","result":{"id":0,"status":"failed","error":{"code":"pending","message":"still live"}}}), Operation::ReleasePanePin).is_err());
        assert_eq!(
            serde_json::to_value(Request::ReleasePanePin {
                instance: "owner",
                pane: 2,
                pid: 3
            })?,
            json!({"request":"release-pane-pin","instance":"owner","pane":2,"pid":3})
        );
        assert_eq!(
            serde_json::to_value(Request::PaneLocation {
                instance: "owner",
                pane: 2
            })?,
            json!({"request":"pane-location","instance":"owner","pane":2})
        );
        Ok(())
    }
}
