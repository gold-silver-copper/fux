//! Typed workspace input operations. Routing and delivery policy belong to callers.
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::{path::Path, time::Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum State {
    Reserved,
    Queued,
    Delivered,
    Failed,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Receipt {
    pub operation: u64,
    pub pane: u32,
    pub state: State,
    pub revision: u64,
    pub input_sequence: u64,
    pub expires_ms: u64,
    pub bytes_written: usize,
    pub error: Option<String>,
}

#[derive(Serialize)]
#[serde(tag = "command", rename_all = "kebab-case")]
enum Request<'a> {
    InputReserve {
        id: u64,
        instance: &'a str,
        pane: u32,
        retain_ms: u64,
    },
    InputSubmit {
        id: u64,
        instance: &'a str,
        operation: u64,
        keys: &'a str,
    },
}

pub(crate) fn reserve(
    socket: &Path,
    instance: &str,
    pane: u32,
    retain_ms: u64,
    deadline: Instant,
) -> Result<Receipt> {
    let value = serde_json::to_value(Request::InputReserve {
        id: 1,
        instance,
        pane,
        retain_ms,
    })?;
    super::manager::decode_input_control(super::request_until(socket, value, deadline)?)
}

pub(crate) fn submit(
    socket: &Path,
    instance: &str,
    operation: u64,
    keys: &str,
    deadline: Instant,
) -> Result<Receipt> {
    let value = serde_json::to_value(Request::InputSubmit {
        id: 1,
        instance,
        operation,
        keys,
    })?;
    super::manager::decode_input_control(super::request_until(socket, value, deadline)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context;
    use serde_json::{Value, json};

    #[test]
    fn producer_input_fixtures_match_typed_requests_and_receipt() -> Result<()> {
        let fixture = |name: &str| -> Result<Value> {
            Ok(serde_json::from_slice(&std::fs::read(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fixtures/control")
                    .join(format!("{name}_zor.json")),
            )?)?)
        };
        assert_eq!(
            serde_json::to_value(Request::InputReserve {
                id: 1,
                instance: "owner",
                pane: 2,
                retain_ms: 1000
            })?,
            fixture("request_input_reserve")?
        );
        assert_eq!(
            serde_json::to_value(Request::InputSubmit {
                id: 1,
                instance: "owner",
                operation: 9,
                keys: "literal\\\\界\\r"
            })?,
            fixture("request_input_submit")?
        );
        let receipt = super::super::manager::decode_input_control(fixture("reply_input")?)?;
        assert_eq!(receipt.state, State::Delivered);
        assert_eq!(receipt.bytes_written, 5);
        Ok(())
    }

    #[test]
    fn requests_preserve_literal_keys_and_receipts_require_exact_envelopes() -> Result<()> {
        let keys = "literal\\\\界\\r";
        assert_eq!(
            serde_json::to_value(Request::InputSubmit {
                id: 1,
                instance: "owner",
                operation: 9,
                keys
            })?,
            json!({"command":"input-submit","id":1,"instance":"owner","operation":9,"keys":keys})
        );
        assert_eq!(
            serde_json::to_value(Request::InputReserve {
                id: 1,
                instance: "owner",
                pane: 2,
                retain_ms: 1000
            })?,
            json!({"command":"input-reserve","id":1,"instance":"owner","pane":2,"retain_ms":1000})
        );
        let value = json!({"id":1,"status":"completed","result":{"kind":"input","value":{"receipt":{
            "operation":9,"pane":2,"state":"delivered","revision":3,"input_sequence":4,
            "expires_ms":1000,"bytes_written":5,"error":null}}}});
        let receipt = super::super::manager::decode_input_control(value.clone())?;
        assert_eq!(receipt.state, State::Delivered);
        assert_eq!(receipt.operation, 9);
        for (pointer, replacement) in [
            ("/id", json!(0)),
            ("/result/kind", json!("final")),
            ("/result/value/receipt/state", json!("unknown")),
            ("/result/value/receipt/bytes_written", json!(-1)),
            ("/result/value/receipt/input_sequence", json!("4")),
        ] {
            let mut invalid = value.clone();
            *invalid.pointer_mut(pointer).context("fixture pointer")? = replacement;
            assert!(
                super::super::manager::decode_input_control(invalid).is_err(),
                "accepted {pointer}"
            );
        }
        let mut missing = value;
        missing
            .pointer_mut("/result/value/receipt")
            .and_then(Value::as_object_mut)
            .context("receipt")?
            .remove("operation");
        assert!(super::super::manager::decode_input_control(missing).is_err());
        Ok(())
    }
}
