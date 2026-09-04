use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub sequence: u64,
    pub source: String,
    pub event: Event,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Event {
    Input { client: String, bytes_hex: String },
    PtyWrite { pane: String, bytes_hex: String },
    Command { name: String },
    Clock { milliseconds: u64 },
    Cleanup { owned_resources: usize },
}

pub fn encode_jsonl(entries: &[Entry]) -> Result<String, serde_json::Error> {
    let mut output = String::new();
    for entry in entries {
        output.push_str(&serde_json::to_string(entry)?);
        output.push('\n');
    }
    Ok(output)
}

pub fn hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        output.push_str(&format!("{byte:02x}"));
    }
    output
}

pub fn assert_fixture_safe(value: &str) -> Result<(), String> {
    let lower = value.to_ascii_lowercase();
    let suspicious = [
        "authorization: bearer",
        "github_pat_",
        "ghp_",
        "begin openssh private key",
        "begin rsa private key",
        "set-cookie:",
        "/users/",
        "\\users\\",
    ];
    if let Some(marker) = suspicious.iter().find(|marker| lower.contains(**marker)) {
        return Err(format!("fixture contains forbidden marker {marker:?}"));
    }
    Ok(())
}
