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
    List,
    Create {
        name: &'a str,
    },
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
pub(crate) struct Descriptor {
    pub stream: u64,
    pub name: String,
    pub pid: u32,
    pub instance_nonce: String,
    pub socket_path: std::path::PathBuf,
}
#[derive(Deserialize)]
#[serde(tag = "reply", rename_all = "kebab-case", deny_unknown_fields)]
enum CreationReply {
    Attach { descriptor: Descriptor },
    Failed { message: String },
}
use super::error::Refused as CreationRefused;

fn decode_creation(value: Value, name: &str) -> Result<Descriptor> {
    super::error::reply(|| {
        match serde_json::from_value(value).context("invalid workspace creation reply")? {
            CreationReply::Attach { descriptor } => {
                ensure!(
                    descriptor.name == name && super::endpoint::valid_name(name),
                    "created workspace name mismatch"
                );
                ensure!(
                    descriptor.stream != 0 && descriptor.pid != 0,
                    "invalid workspace descriptor identity"
                );
                ensure!(
                    !descriptor.instance_nonce.is_empty()
                        && descriptor.instance_nonce.len() <= 128
                        && !descriptor
                            .instance_nonce
                            .chars()
                            .any(|c| c.is_whitespace() || c == '\0'),
                    "invalid workspace instance"
                );
                ensure!(
                    descriptor.socket_path.is_absolute(),
                    "invalid workspace attachment path"
                );
                Ok(descriptor)
            }
            CreationReply::Failed { message } => Err(CreationRefused(message).into()),
        }
    })
}

/// Decodes the manager envelope printed by the fux CLI; startup authority stays in the caller.
pub(crate) fn created_from_cli(bytes: &[u8], name: &str) -> Result<Descriptor> {
    super::error::reply(|| {
        ensure!(
            bytes.len() <= super::MAX_REPLY,
            "workspace creation reply exceeds limit"
        );
        decode_creation(
            serde_json::from_slice(bytes).context("invalid fux CLI reply")?,
            name,
        )
    })
}

pub(crate) fn create(runtime: &Path, name: &str, deadline: Instant) -> Result<Descriptor> {
    ensure!(super::endpoint::valid_name(name), "invalid workspace name");
    decode_creation(
        super::request_until(
            &super::endpoint::Endpoint::new(runtime).manager(),
            serde_json::to_value(Request::Create { name })?,
            deadline,
        )?,
        name,
    )
}

#[derive(Debug, Deserialize)]
#[serde(tag = "reply", rename_all = "kebab-case", deny_unknown_fields)]
enum DiscoveryReply {
    Names { names: Vec<String> },
    Failed { message: String },
}

fn decode_names(value: Value) -> Result<Vec<String>> {
    super::error::reply(|| {
        match serde_json::from_value(value).context("invalid workspace discovery reply")? {
            DiscoveryReply::Names { names } => {
                ensure!(names.len() <= 64, "workspace discovery limit exceeded");
                let mut seen = std::collections::BTreeSet::new();
                for name in &names {
                    ensure!(super::endpoint::valid_name(name), "invalid workspace name");
                    ensure!(seen.insert(name), "duplicate workspace discovery");
                }
                Ok(names)
            }
            DiscoveryReply::Failed { message } => Err(super::error::Refused(message).into()),
        }
    })
}

pub(crate) fn names(runtime: &Path, deadline: Instant) -> Result<Vec<String>> {
    decode_names(super::request_until(
        &super::endpoint::Endpoint::new(runtime).manager(),
        serde_json::to_value(Request::List)?,
        deadline,
    )?)
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

use super::error::RemoteFailure as RemoteError;

/// Only validated remote failures expose a code; malformed envelopes never do.
pub(crate) fn remote_code(error: &anyhow::Error) -> Option<&str> {
    super::error::remote_code(error)
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
    super::error::reply(|| {
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
    })
}

pub(crate) fn locate(
    runtime: &Path,
    instance: &str,
    pane: u32,
    deadline: Instant,
) -> Result<Location> {
    let request = serde_json::to_value(Request::PaneLocation { instance, pane })?;
    let reply = super::request_until(
        &super::endpoint::Endpoint::new(runtime).manager(),
        request,
        deadline,
    )?;
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
    let reply = super::request_until(
        &super::endpoint::Endpoint::new(runtime).manager(),
        request,
        deadline,
    )?;
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
    let reply = super::request_until(
        &super::endpoint::Endpoint::new(runtime).manager(),
        request,
        deadline,
    )?;
    decode_final(reply)
}

fn decode_final(reply: Value) -> Result<FinalOutcome> {
    super::error::reply(|| match decode(reply, Operation::Final) {
        Ok(Payload::Final { record }) => Ok(FinalOutcome::Record(record)),
        Ok(_) => anyhow::bail!("unexpected final result"),
        Err(error) => {
            match error
                .downcast_ref::<RemoteError>()
                .map(|error| error.code.as_str())
            {
                Some("pending") => Ok(FinalOutcome::Pending),
                Some("evicted") => Err(error.context(
                    "fux dropped the final record under load before its retention elapsed; the exit evidence is lost and cannot be retried"
                )),
                _ => Err(error.context("final evidence unavailable")),
            }
        }
    })
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
    let reply = super::request_until(
        &super::endpoint::Endpoint::new(runtime).manager(),
        request,
        deadline,
    )?;
    match decode(reply, Operation::InputStatus)? {
        Payload::Input { receipt } => Ok(receipt),
        _ => anyhow::bail!("unexpected input status result"),
    }
}

pub(super) fn decode_input_control(reply: Value) -> Result<super::input::Receipt> {
    super::error::reply(|| {
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
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creation_requires_complete_descriptor_and_matching_name() -> Result<()> {
        use serde_json::json;
        let request: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/manager/request_create_client.json"
        ))?;
        assert_eq!(
            serde_json::to_value(Request::Create { name: "run" })?,
            request
        );
        let bytes = include_bytes!("../../tests/fixtures/manager/reply_create_client.json");
        let descriptor = created_from_cli(bytes, "run")?;
        assert_eq!(descriptor.stream, 1);
        assert_eq!(descriptor.instance_nonce, "owner");
        assert!(created_from_cli(bytes, "other").is_err());
        let valid: Value = serde_json::from_slice(bytes)?;
        for (path, value) in [
            ("/reply", json!("names")),
            ("/descriptor/stream", json!(0)),
            ("/descriptor/pid", json!(0)),
            ("/descriptor/instance_nonce", json!("")),
            ("/descriptor/instance_nonce", json!("with space")),
            ("/descriptor/socket_path", json!("relative")),
            ("/descriptor/name", json!("other")),
        ] {
            let mut invalid = valid.clone();
            *invalid.pointer_mut(path).context("fixture field")? = value;
            assert!(decode_creation(invalid, "run").is_err(), "accepted {path}");
        }
        for field in ["stream", "pid", "instance_nonce", "socket_path", "name"] {
            let mut invalid = valid.clone();
            invalid
                .get_mut("descriptor")
                .and_then(Value::as_object_mut)
                .context("descriptor")?
                .remove(field);
            assert!(decode_creation(invalid, "run").is_err(), "missing {field}");
        }
        let error = decode_creation(json!({"reply":"failed","message":"exists"}), "run")
            .err()
            .context("expected refusal")?;
        assert!(error.downcast_ref::<CreationRefused>().is_some());
        assert!(created_from_cli(b"not json", "run").is_err());
        Ok(())
    }

    #[test]
    fn discovery_requires_exact_reply_and_safe_unique_names() -> Result<()> {
        assert_eq!(
            decode_names(serde_json::json!({"reply":"names","names":["one","two"]}))?,
            vec!["one", "two"]
        );
        for invalid in [
            serde_json::json!({"names":[]}),
            serde_json::json!({"reply":"info","names":[]}),
            serde_json::json!({"reply":"names","names":[],"extra":true}),
            serde_json::json!({"reply":"names","names":["../escape"]}),
            serde_json::json!({"reply":"names","names":["one","one"]}),
            serde_json::json!({"reply":"names","names":[null]}),
            serde_json::json!({"reply":"failed","message":"unavailable"}),
        ] {
            assert!(decode_names(invalid).is_err());
        }
        Ok(())
    }
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
