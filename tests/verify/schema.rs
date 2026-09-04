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
    SwitchWorkspace {
        client: String,
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
        query: Vec<u8>,
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
    KillPane {
        pane: u32,
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
    pub snapshots: Vec<ExpectedSnapshot>,
    #[serde(default)]
    pub control_events: Vec<ExpectedControlEvent>,
    #[serde(default)]
    pub subscriptions: Vec<ExpectedSubscription>,
    #[serde(default)]
    pub control_replies: Vec<ExpectedControlReply>,
    #[serde(default)]
    pub terminal_frames: Vec<ExpectedTerminalFrame>,
    #[serde(default)]
    pub pty_resizes: Vec<ExpectedResize>,
    #[serde(default)]
    pub signals: Vec<ExpectedSignal>,
    #[serde(default)]
    pub exit_status: Option<i32>,
    #[serde(default)]
    pub owned_resources: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedSnapshot {
    pub workspace: String,
    pub generation: u64,
    pub stable_hash: String,
    #[serde(default)]
    pub focused_pane: Option<u32>,
    #[serde(default)]
    pub pane_count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedControlEvent {
    pub name: String,
    pub request_id: u64,
    #[serde(default)]
    pub subscription_id: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedSubscription {
    pub request_id: u64,
    pub events: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedControlReply {
    pub request_id: u64,
    pub status: String,
    pub result_kind: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedTerminalFrame {
    pub rows: u16,
    pub columns: u16,
    pub cells: Vec<String>,
    #[serde(default)]
    pub cursor: Option<(u16, u16)>,
    #[serde(default)]
    pub synchronized: Option<bool>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedResize {
    pub pane: u32,
    pub rows: u16,
    pub columns: u16,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedSignal {
    pub process: String,
    pub signal: Signal,
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
        let mut inner_columns = self.initial_size.columns.saturating_sub(2);
        let mut child_output_columns = std::collections::BTreeMap::<u32, usize>::new();
        let mut copy_mode = false;
        let mut attached_clients = std::collections::BTreeSet::<String>::new();
        let mut client_workspaces = std::collections::BTreeMap::<String, String>::new();
        let mut workspaces = std::collections::BTreeSet::<String>::new();
        let mut transport_lost = false;
        let mut child_exited = false;
        #[derive(Clone, Copy, Eq, PartialEq)]
        enum DaemonPhase {
            NotStarted,
            Running,
            Stopped,
        }
        let mut daemon_phase = DaemonPhase::NotStarted;
        for (index, step) in self.steps.iter().enumerate() {
            match step {
                Step::StartDaemon if daemon_phase == DaemonPhase::NotStarted => {
                    daemon_phase = DaemonPhase::Running;
                    workspaces.insert("binary".into());
                }
                Step::Shutdown if daemon_phase == DaemonPhase::Running => {
                    daemon_phase = DaemonPhase::Stopped;
                }
                Step::Expect { .. }
                    if daemon_phase == DaemonPhase::Stopped
                        && index.checked_add(1) == Some(self.steps.len()) => {}
                Step::StartDaemon => return Err("serialized daemon start is one-shot".into()),
                Step::Shutdown => return Err("serialized shutdown requires running daemon".into()),
                Step::Expect { .. } => {
                    return Err("serialized expect must be final after shutdown".into());
                }
                _ if daemon_phase != DaemonPhase::Running => {
                    return Err("serialized operation requires running daemon".into());
                }
                _ => {}
            }
            if child_exited && !matches!(step, Step::Shutdown | Step::Expect { .. }) {
                return Err("only shutdown and expect may follow serialized child_exit".into());
            }
            validate_step(step)?;
            match step {
                Step::Attach { client } if workspaces.contains("binary") => {
                    attached_clients.insert(client.clone());
                    client_workspaces.insert(client.clone(), "binary".into());
                }
                Step::Attach { .. } => {
                    return Err("serialized attach requires the binary workspace".into());
                }
                Step::Reconnect { client }
                    if !attached_clients.contains(client)
                        && client_workspaces
                            .get(client)
                            .is_some_and(|workspace| workspaces.contains(workspace)) =>
                {
                    attached_clients.insert(client.clone());
                }
                Step::Reconnect { client } => {
                    return Err(format!(
                        "serialized client {client:?} has no live workspace to reconnect"
                    ));
                }
                Step::Detach { client } | Step::Disconnect { client } => {
                    attached_clients.remove(client);
                }
                Step::CreateWorkspace { workspace } if workspaces.insert(workspace.clone()) => {}
                Step::CreateWorkspace { workspace } => {
                    return Err(format!("serialized workspace {workspace:?} already exists"));
                }
                Step::SelectWorkspace { workspace } if workspaces.contains(workspace) => {}
                Step::SelectWorkspace { workspace } => {
                    return Err(format!("serialized workspace {workspace:?} does not exist"));
                }
                Step::SwitchWorkspace { client, workspace }
                    if attached_clients.contains(client) && workspaces.contains(workspace) =>
                {
                    client_workspaces.insert(client.clone(), workspace.clone());
                }
                Step::SwitchWorkspace { .. } => {
                    return Err(
                        "serialized switch requires an attached client and workspace".into(),
                    );
                }
                Step::Transport {
                    fault: TransportFault::Lose,
                } if !transport_lost => transport_lost = true,
                Step::Transport {
                    fault: TransportFault::Reconnect,
                } if transport_lost => transport_lost = false,
                Step::Transport {
                    fault: TransportFault::Duplicate | TransportFault::Reorder,
                } if !transport_lost => {}
                Step::Transport { .. } => {
                    return Err("serialized transport fault violates link lifecycle".into());
                }
                Step::DeleteWorkspace { workspace }
                    if !client_workspaces.iter().any(|(client, current)| {
                        attached_clients.contains(client) && current == workspace
                    }) && workspaces.remove(workspace) => {}
                Step::DeleteWorkspace { workspace } => {
                    return Err(format!("serialized workspace {workspace:?} does not exist"));
                }
                Step::Resize { size, .. } => {
                    inner_columns = size.columns.saturating_sub(2);
                    if child_output_columns
                        .values()
                        .any(|columns| *columns > usize::from(inner_columns))
                    {
                        return Err("resize would wrap serialized child_output".into());
                    }
                }
                Step::ChildOutput { pane, bytes } => {
                    let columns = child_output_columns.entry(*pane).or_default();
                    *columns = columns.saturating_add(bytes.len());
                    if *columns > usize::from(inner_columns) {
                        return Err("serialized child_output must fit one visible pane row".into());
                    }
                }
                Step::TerminalReply { pane, query, bytes }
                    if *pane != 1
                        || query != b"\x1b[6n"
                        || bytes != b"\x1b[1;1R"
                        || child_output_columns.contains_key(pane) =>
                {
                    return Err(
                        "serialized terminal_reply supports pane-1 DSR at the initial cursor only"
                            .into(),
                    );
                }
                Step::Prefix { key: b'[', .. } => copy_mode = true,
                Step::CopyInput { client, bytes }
                    if attached_clients.contains(client) && copy_mode && bytes == b"q" =>
                {
                    copy_mode = false;
                }
                Step::CopyInput { .. } => {
                    return Err(
                        "serialized copy_input currently supports q after prefix-[ only".into(),
                    );
                }
                Step::ChildExit { pane, status } if *pane == 1 && (0..=125).contains(status) => {
                    child_exited = true;
                }
                Step::ChildExit { .. } => {
                    return Err(
                        "serialized child_exit supports pane 1 and status 0-125 only".into(),
                    );
                }
                Step::Signal { pane: 1, .. } => child_exited = true,
                Step::Signal { .. } => {
                    return Err("serialized signal supports pane 1 only".into());
                }
                Step::KillPane { pane: 1 } => child_exited = true,
                Step::KillPane { .. } => {
                    return Err("serialized kill_pane supports pane 1 only".into());
                }
                _ => {}
            }
        }
        if daemon_phase != DaemonPhase::Stopped
            || self
                .steps
                .iter()
                .filter(|step| matches!(step, Step::Expect { .. }))
                .count()
                != 1
        {
            return Err("serialized scenario must end with shutdown and one final expect".into());
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
        | Step::DeleteWorkspace { workspace } => validate_workspace(workspace)?,
        Step::SwitchWorkspace { client, workspace } => {
            bounded_text("client", client)?;
            validate_workspace(workspace)?;
        }
        Step::Resize { client, size } => {
            bounded_text("client", client)?;
            validate_size(*size)?;
        }
        Step::ChildOutput { bytes, .. } => {
            bounded_bytes(bytes)?;
            if bytes.is_empty() || bytes.iter().any(|byte| !(0x21..=0x7e).contains(byte)) {
                return Err(
                    "serialized child_output currently supports nonempty ASCII graphic bytes only; use a terminal cassette for whitespace and control sequences"
                        .into(),
                );
            }
        }
        Step::ExpectInput { bytes, .. } => bounded_bytes(bytes)?,
        Step::TerminalReply { query, bytes, .. } => {
            bounded_bytes(query)?;
            bounded_bytes(bytes)?;
        }
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
            let expected_items = expected.forwarded.len()
                + expected.commands.len()
                + expected.snapshots.len()
                + expected.control_events.len()
                + expected.subscriptions.len()
                + expected.control_replies.len()
                + expected.terminal_frames.len()
                + expected.pty_resizes.len()
                + expected.signals.len();
            if expected_items > MAX_STEPS {
                return Err("expectation contains too many items".into());
            }
            for bytes in &expected.forwarded {
                bounded_bytes(bytes)?;
            }
            for command in &expected.commands {
                bounded_text("command", command)?;
            }
            for snapshot in &expected.snapshots {
                bounded_text("workspace", &snapshot.workspace)?;
                bounded_text("stable hash", &snapshot.stable_hash)?;
            }
            for event in &expected.control_events {
                bounded_text("control event", &event.name)?;
            }
            for subscription in &expected.subscriptions {
                if subscription.events.len() > 32 {
                    return Err("expected subscription has more than 32 filters".into());
                }
                for event in &subscription.events {
                    bounded_text("subscription event", event)?;
                }
            }
            for reply in &expected.control_replies {
                bounded_text("control reply status", &reply.status)?;
                bounded_text("control reply result kind", &reply.result_kind)?;
            }
            for frame in &expected.terminal_frames {
                validate_size(Size {
                    rows: frame.rows,
                    columns: frame.columns,
                })?;
                if frame.cells.len() > usize::from(MAX_DIMENSION) * usize::from(MAX_DIMENSION) {
                    return Err("terminal frame has too many cells".into());
                }
                for cell in &frame.cells {
                    if cell.len() > MAX_NAME_BYTES || cell.contains('\0') {
                        return Err("terminal cell exceeds its text bound".into());
                    }
                }
            }
            for resize in &expected.pty_resizes {
                validate_size(Size {
                    rows: resize.rows,
                    columns: resize.columns,
                })?;
            }
            for signal in &expected.signals {
                bounded_text("process", &signal.process)?;
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

fn validate_workspace(workspace: &str) -> Result<(), String> {
    if workspace.is_empty()
        || workspace.len() > 64
        || matches!(workspace, "." | "..")
        || !workspace
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err("workspace name is unsafe".into());
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
