//! Typed server information. Learning an incarnation does not authorize adopting its panes.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    time::Instant,
};

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Info {
    pub pid: u32,
    pub instance_nonce: String,
    pub version: String,
    pub runtime_dir: PathBuf,
    #[serde(deserialize_with = "Option::deserialize")]
    pub workspace: Option<String>,
    pub limits: Limits,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Limits {
    pub scrollback_lines: usize,
    pub frame_bytes: usize,
    pub capture_bytes: usize,
    pub key_bytes: usize,
    pub input_retention_ms: u64,
    pub final_retention_ms: u64,
}
#[derive(Deserialize)]
#[serde(tag = "reply", rename_all = "kebab-case", deny_unknown_fields)]
enum Reply {
    Info { info: Info },
    Failed { message: String },
}

fn decode(value: serde_json::Value) -> Result<Info> {
    super::error::reply(|| {
        match serde_json::from_value(value).context("invalid manager info reply")? {
            Reply::Info { info } => {
                ensure!(
                    info.pid > 0
                        && !info.instance_nonce.is_empty()
                        && info.instance_nonce.len() <= 128,
                    "invalid server identity"
                );
                ensure!(info.workspace.is_none(), "manager info names a workspace");
                ensure!(
                    info.runtime_dir.is_absolute(),
                    "invalid server runtime path"
                );
                Ok(info)
            }
            Reply::Failed { message } => Err(super::error::Refused(message).into()),
        }
    })
}
pub(crate) fn manager(runtime: &Path, deadline: Instant) -> Result<Info> {
    decode(super::request_until(
        &super::endpoint::Endpoint::new(runtime).manager(),
        serde_json::json!({"request":"info"}),
        deadline,
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    #[test]
    fn manager_identity_requires_complete_info_and_correct_endpoint() -> Result<()> {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/control/reply_completed_info.json"
        ))?;
        let mut info = fixture
            .pointer("/result/value/info")
            .context("info fixture")?
            .clone();
        assert!(decode(json!({"reply":"info","info":info})).is_err());
        *info.get_mut("workspace").context("workspace field")? = Value::Null;
        assert_eq!(
            decode(json!({"reply":"info","info":info}))?.instance_nonce,
            "a1b2c3"
        );
        for field in ["pid", "instance_nonce", "workspace", "limits"] {
            let mut invalid = info.clone();
            invalid
                .as_object_mut()
                .context("info object")?
                .remove(field);
            assert!(decode(json!({"reply":"info","info":invalid})).is_err());
        }
        assert!(decode(json!({"reply":"names","info":info})).is_err());
        Ok(())
    }
}
