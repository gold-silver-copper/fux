use serde::{Deserialize, Serialize};

pub const MAX_STEPS: usize = 512;
pub const MAX_NAME_BYTES: usize = 128;
pub const MAX_PAYLOAD_BYTES: usize = 1024 * 1024;
pub const MAX_DIMENSION: u16 = 512;
pub const MAX_CLOCK_ADVANCE_MS: u64 = 60_000;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    pub schema_version: u16,
    pub name: String,
    pub applicability: Applicability,
    pub initial_size: Size,
    pub steps: Vec<Step>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Applicability {
    All,
    Production,
    Binary,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Size {
    pub rows: u16,
    pub columns: u16,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Step {
    StartDaemon,
    Attach {
        client: String,
    },
    Detach {
        client: String,
    },
    Reconnect {
        client: String,
    },
    Shutdown,
    CreateWorkspace {
        workspace: String,
    },
    SelectWorkspace {
        workspace: String,
    },
    DeleteWorkspace {
        workspace: String,
    },
    Resize {
        client: String,
        size: Size,
    },
    Disconnect {
        client: String,
    },
    ChildOutput {
        pane: u32,
        bytes: Vec<u8>,
    },
    ExpectInput {
        pane: u32,
        bytes: Vec<u8>,
    },
    TerminalReply {
        pane: u32,
        bytes: Vec<u8>,
    },
    Signal {
        pane: u32,
        signal: Signal,
    },
    ChildExit {
        pane: u32,
        status: i32,
    },
    Input {
        client: String,
        bytes: Vec<u8>,
    },
    Prefix {
        client: String,
        key: u8,
    },
    Paste {
        client: String,
        bytes: Vec<u8>,
    },
    CopyInput {
        client: String,
        bytes: Vec<u8>,
    },
    MouseInput {
        client: String,
        bytes: Vec<u8>,
    },
    Control {
        request: serde_json::Value,
    },
    Subscribe {
        request_id: u64,
        events: Vec<String>,
    },
    AdvanceClock {
        milliseconds: u64,
    },
    Transport {
        fault: TransportFault,
    },
    Expect {
        expected: Expected,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Signal {
    Hup,
    Int,
    Term,
    Kill,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportFault {
    Lose,
    Duplicate,
    Reorder,
    Reconnect,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expected {
    #[serde(default)]
    pub forwarded: Vec<Vec<u8>>,
    #[serde(default)]
    pub commands: Vec<String>,
    #[serde(default)]
    pub exit_status: Option<i32>,
    #[serde(default)]
    pub owned_resources: usize,
}

impl Scenario {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != 1 {
            return Err("unsupported scenario schema_version".into());
        }
        bounded_text("scenario name", &self.name)?;
        validate_size(self.initial_size)?;
        if self.steps.len() > MAX_STEPS {
            return Err(format!("scenario has more than {MAX_STEPS} steps"));
        }
        for step in &self.steps {
            validate_step(step)?;
        }
        Ok(())
    }
}

fn validate_step(step: &Step) -> Result<(), String> {
    match step {
        Step::Attach { client }
        | Step::Detach { client }
        | Step::Reconnect { client }
        | Step::Disconnect { client }
        | Step::Prefix { client, .. } => bounded_text("client", client)?,
        Step::CreateWorkspace { workspace }
        | Step::SelectWorkspace { workspace }
        | Step::DeleteWorkspace { workspace } => bounded_text("workspace", workspace)?,
        Step::Resize { size, .. } => validate_size(*size)?,
        Step::ChildOutput { bytes, .. }
        | Step::ExpectInput { bytes, .. }
        | Step::TerminalReply { bytes, .. } => bounded_bytes(bytes)?,
        Step::Input { client, bytes }
        | Step::Paste { client, bytes }
        | Step::CopyInput { client, bytes }
        | Step::MouseInput { client, bytes } => {
            bounded_text("client", client)?;
            bounded_bytes(bytes)?;
        }
        Step::Subscribe { events, .. } => {
            if events.len() > 32 {
                return Err("subscription has more than 32 event filters".into());
            }
            for event in events {
                bounded_text("event", event)?;
            }
        }
        Step::AdvanceClock { milliseconds } if *milliseconds > MAX_CLOCK_ADVANCE_MS => {
            return Err("clock advance exceeds 60 seconds".into());
        }
        Step::Control { request } => {
            let encoded = serde_json::to_vec(request).map_err(|error| error.to_string())?;
            bounded_bytes(&encoded)?;
        }
        Step::Expect { expected } => {
            for bytes in &expected.forwarded {
                bounded_bytes(bytes)?;
            }
            for command in &expected.commands {
                bounded_text("command", command)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn validate_size(size: Size) -> Result<(), String> {
    if size.rows == 0
        || size.columns == 0
        || size.rows > MAX_DIMENSION
        || size.columns > MAX_DIMENSION
    {
        return Err(format!("dimensions must be in 1..={MAX_DIMENSION}"));
    }
    Ok(())
}

fn bounded_text(kind: &str, value: &str) -> Result<(), String> {
    if value.is_empty() || value.len() > MAX_NAME_BYTES || value.contains('\0') {
        return Err(format!(
            "{kind} must contain 1-{MAX_NAME_BYTES} non-NUL bytes"
        ));
    }
    Ok(())
}

fn bounded_bytes(value: &[u8]) -> Result<(), String> {
    if value.len() > MAX_PAYLOAD_BYTES {
        return Err(format!("payload exceeds {MAX_PAYLOAD_BYTES} bytes"));
    }
    Ok(())
}
