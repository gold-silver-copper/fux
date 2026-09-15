//! Native-channel frames → normalized [`Claim`]s. Decoders are pure: identity checks against
//! the World (report token, receipt, binding) happen in `providers::apply_claim`.
//!
//! **Codex app-server** (JSON-RPC over stdio; the old `tasks/codex/protocol.rs` is the oracle):
//! zor sends `initialize`, `thread/start` and, per armed prompt, `turn/start` whose
//! `clientUserMessageId` is the prompt's report token. The `userMessage` item echoes it as
//! `clientId`; `turn/started|completed`, `item/*` and `item/tool/requestUserInput` carry the
//! thread and turn ids from which bindings, reports and state claims follow.
//!
//! **OpenCode sidecar** (zor's line protocol, v1): each line is one JSON object with `t`:
//! `hello {producer, session?}`, `bound {operation, token, session, message}`,
//! `report {operation, token, kind: response|needs-input}`, `state {state}`. zor writes
//! `arm {operation, token, text}`.

use serde_json::{Value, json};

use crate::model::AgentState;

use super::{ProviderKind, ProviderSession};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReportKind {
    Response,
    NeedsInput,
}

impl ReportKind {
    /// The `ResponseEvent.kind` string the lifecycle reads.
    pub fn name(self) -> &'static str {
        match self {
            Self::Response => "response",
            Self::NeedsInput => "needs-input",
        }
    }
}

/// One fact a provider frame asserts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Claim {
    /// The producer identified itself and/or its native session.
    Hello {
        producer: Option<String>,
        session: Option<String>,
    },
    /// The producer consumed the prompt armed under `token`: native ancestry.
    Bound {
        token: String,
        operation: Option<String>,
        session: String,
        message: String,
    },
    /// A report about the prompt armed under `token`.
    Report {
        token: String,
        operation: Option<String>,
        kind: ReportKind,
    },
    /// The producer's current state (bare, uncorrelated to a prompt).
    State(AgentState),
    /// The frame was malformed for its protocol.
    Malformed(String),
}

pub fn codex_initialize() -> Value {
    json!({
        "id": "zor:initialize",
        "method": "initialize",
        "params": { "clientInfo": { "name": "zor", "version": env!("CARGO_PKG_VERSION") } }
    })
}

pub fn codex_thread_start() -> Value {
    json!({ "id": "zor:thread", "method": "thread/start", "params": {} })
}

pub fn codex_turn_start(thread: &str, token: &str, text: &str) -> Value {
    json!({
        "id": format!("zor:turn:{token}"),
        "method": "turn/start",
        "params": {
            "threadId": thread,
            "clientUserMessageId": token,
            "input": [{ "type": "text", "text": text }],
            "approvalPolicy": "never"
        }
    })
}

pub fn opencode_arm(operation: &str, token: &str, text: &str) -> Value {
    json!({ "t": "arm", "operation": operation, "token": token, "text": text })
}

fn str_of<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn identity(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}

/// Decodes one line for `kind`, updating the session's protocol memory (Codex thread and
/// turn → token table). An unrelated frame yields no claims.
pub fn decode(kind: ProviderKind, session: &mut ProviderSession, line: &[u8]) -> Vec<Claim> {
    let frame: Value = match serde_json::from_slice(line) {
        Ok(Value::Object(map)) => Value::Object(map),
        Ok(_) => return vec![Claim::Malformed("frame is not an object".into())],
        Err(e) => return vec![Claim::Malformed(format!("frame is not JSON: {e}"))],
    };
    match kind {
        ProviderKind::Codex => codex(session, &frame),
        ProviderKind::OpenCode => opencode(&frame),
        ProviderKind::Claude => Vec::new(),
    }
}

fn opencode(frame: &Value) -> Vec<Claim> {
    let Some(t) = str_of(frame, "t") else {
        return vec![Claim::Malformed("missing `t`".into())];
    };
    let required = |key: &str| -> Result<String, Claim> {
        str_of(frame, key)
            .filter(|v| identity(v))
            .map(str::to_owned)
            .ok_or_else(|| Claim::Malformed(format!("{t}: missing or invalid `{key}`")))
    };
    let claim = match t {
        "hello" => Ok(Claim::Hello {
            producer: str_of(frame, "producer")
                .filter(|v| identity(v))
                .map(str::to_owned),
            session: str_of(frame, "session")
                .filter(|v| identity(v))
                .map(str::to_owned),
        }),
        "bound" => (|| {
            Ok(Claim::Bound {
                token: required("token")?,
                operation: Some(required("operation")?),
                session: required("session")?,
                message: required("message")?,
            })
        })(),
        "report" => (|| {
            let kind = match str_of(frame, "kind") {
                Some("response") => ReportKind::Response,
                Some("needs-input") => ReportKind::NeedsInput,
                _ => return Err(Claim::Malformed("report: unknown `kind`".into())),
            };
            Ok(Claim::Report {
                token: required("token")?,
                operation: Some(required("operation")?),
                kind,
            })
        })(),
        "state" => match str_of(frame, "state") {
            Some("working") => Ok(Claim::State(AgentState::Working)),
            Some("blocked") => Ok(Claim::State(AgentState::Blocked)),
            Some("idle") => Ok(Claim::State(AgentState::Idle)),
            Some("unknown") => Ok(Claim::State(AgentState::Unknown)),
            _ => Err(Claim::Malformed("state: unknown `state`".into())),
        },
        _ => return Vec::new(),
    };
    vec![claim.unwrap_or_else(|malformed| malformed)]
}

fn codex(session: &mut ProviderSession, frame: &Value) -> Vec<Claim> {
    let mut claims = Vec::new();
    // Responses to zor's own requests.
    if frame.get("method").is_none() {
        let Some(id) = str_of(frame, "id") else {
            return claims;
        };
        if let Some(error) = frame.get("error") {
            claims.push(Claim::Malformed(format!("{id}: error {error}")));
            return claims;
        }
        if id == "zor:thread" {
            match frame
                .pointer("/result/thread/id")
                .and_then(Value::as_str)
                .filter(|v| identity(v))
            {
                Some(thread) => claims.push(Claim::Hello {
                    producer: None,
                    session: Some(thread.to_owned()),
                }),
                None => claims.push(Claim::Malformed("thread/start: missing thread id".into())),
            }
        } else if let Some(token) = id.strip_prefix("zor:turn:")
            && let Some(turn) = frame.pointer("/result/turn")
        {
            turn_snapshot(session, turn, Some(token), &mut claims);
        }
        return claims;
    }
    let (Some(method), Some(params)) = (str_of(frame, "method"), frame.get("params")) else {
        return claims;
    };
    if let Some(thread) = str_of(params, "threadId")
        && session.session.as_deref().is_some_and(|s| s != thread)
    {
        return claims;
    }
    match method {
        "turn/started" | "turn/completed" => {
            if let Some(turn) = params.get("turn") {
                turn_snapshot(session, turn, None, &mut claims);
            } else {
                claims.push(Claim::Malformed(format!("{method}: missing turn")));
            }
        }
        "item/started" | "item/completed" => {
            let (Some(turn), Some(item)) = (str_of(params, "turnId"), params.get("item")) else {
                claims.push(Claim::Malformed(format!("{method}: missing turnId/item")));
                return claims;
            };
            let thread = str_of(params, "threadId").unwrap_or_default();
            user_item(session, thread, turn, item, &mut claims);
        }
        "item/tool/requestUserInput" => {
            let Some(turn) = str_of(params, "turnId") else {
                claims.push(Claim::Malformed("requestUserInput: missing turnId".into()));
                return claims;
            };
            if params.get("isBlocking").and_then(Value::as_bool) == Some(true) {
                if let Some(token) = session.turns.get(turn).cloned() {
                    claims.push(Claim::Report {
                        token,
                        operation: None,
                        kind: ReportKind::NeedsInput,
                    });
                }
                claims.push(Claim::State(AgentState::Blocked));
            }
        }
        _ => {}
    }
    claims
}

/// A `turn` object: binds its `userMessage` item, records the turn → token table entry, and
/// reports completion.
fn turn_snapshot(
    session: &mut ProviderSession,
    turn: &Value,
    armed_token: Option<&str>,
    claims: &mut Vec<Claim>,
) {
    let Some(id) = str_of(turn, "id").filter(|v| identity(v)) else {
        claims.push(Claim::Malformed("turn without id".into()));
        return;
    };
    if let Some(token) = armed_token
        && !session.turns.contains_key(id)
    {
        session.turns.insert(id.to_owned(), token.to_owned());
    }
    let thread = str_of(turn, "threadId")
        .or(session.session.as_deref())
        .unwrap_or_default()
        .to_owned();
    if let Some(items) = turn.get("items").and_then(Value::as_array) {
        for item in items.iter().take(256) {
            user_item(session, &thread, id, item, claims);
        }
    }
    match str_of(turn, "status") {
        Some("inProgress") => claims.push(Claim::State(AgentState::Working)),
        Some("completed") => {
            if let Some(token) = session.turns.get(id).cloned() {
                claims.push(Claim::Report {
                    token,
                    operation: None,
                    kind: ReportKind::Response,
                });
            }
            claims.push(Claim::State(AgentState::Idle));
        }
        Some("interrupted" | "failed") => claims.push(Claim::State(AgentState::Unknown)),
        _ => {}
    }
}

fn user_item(
    session: &mut ProviderSession,
    thread: &str,
    turn: &str,
    item: &Value,
    claims: &mut Vec<Claim>,
) {
    if str_of(item, "type") != Some("userMessage") {
        return;
    }
    let (Some(client), Some(message)) = (str_of(item, "clientId"), str_of(item, "id")) else {
        claims.push(Claim::Malformed("userMessage without clientId/id".into()));
        return;
    };
    if !identity(client) || !identity(message) {
        claims.push(Claim::Malformed("userMessage with invalid identity".into()));
        return;
    }
    if !session.turns.contains_key(turn) && session.turns.len() < 1024 {
        session.turns.insert(turn.to_owned(), client.to_owned());
    }
    claims.push(Claim::Bound {
        token: client.to_owned(),
        operation: None,
        session: thread.to_owned(),
        message: message.to_owned(),
    });
}
