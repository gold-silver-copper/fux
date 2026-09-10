//! Identity-checked native turn evidence, independent of PTY delivery receipts.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

const MAX_TEXT: usize = 65_536;
const MAX_RESPONSES: usize = 8;
const MAX_RESPONSE_TEXT: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Phase {
    Prepared,
    Submitted,
    Working,
    InputRequired,
    Completed,
    Interrupted,
    Failed,
    Uncertain,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn replace(value: &mut Value, pointer: &str, replacement: Value) -> Result<()> {
        *value
            .pointer_mut(pointer)
            .with_context(|| format!("fixture field {pointer}"))? = replacement;
        Ok(())
    }

    fn pending() -> Result<Turn> {
        let mut turn = Turn::prepare(
            "operation-1",
            "thread-1",
            "literal\n\t$(never execute)\\é",
            5000,
        )?;
        let request = turn.submission(1000)?;
        assert_eq!(
            request.pointer("/params/clientUserMessageId"),
            Some(&json!("operation-1"))
        );
        assert_eq!(
            request.pointer("/params/input/0/text"),
            Some(&json!(turn.text))
        );
        Ok(turn)
    }
    fn user(turn: &Turn) -> Value {
        json!({"type":"userMessage","id":"user-1","clientId":turn.operation,
            "content":[{"type":"text","text":turn.text}]})
    }
    fn snapshot(turn: &Turn, status: &str) -> Value {
        json!({"id":"turn-1","status":status,"itemsView":"full","items":[user(turn),
            {"type":"agentMessage","id":"assistant-1","text":"answer"}]})
    }
    fn read(turn: &Turn, status: &str) -> Value {
        json!({"id":format!("read:{}", turn.operation),"result":{"thread":{"id":turn.thread,"turns":[snapshot(turn,status)]}}})
    }

    #[test]
    fn lost_ack_and_events_reconcile_from_native_identity_without_replay() -> Result<()> {
        let mut turn = pending()?;
        turn.invalidate("caller disappeared after submission");
        assert!(turn.submission(1001).is_err());
        let restored: Turn = serde_json::from_slice(&serde_json::to_vec(&turn)?)?;
        assert_eq!(restored, turn);
        assert!(turn.observe(&read(&turn, "completed"))?);
        assert_eq!(turn.phase, Phase::Completed);
        assert_eq!(turn.turn.as_deref(), Some("turn-1"));
        assert_eq!(turn.user_item.as_deref(), Some("user-1"));
        assert_eq!(
            turn.responses.get("assistant-1").context("response")?.text,
            "answer"
        );
        assert!(turn.fresh);
        assert!(turn.submission(1002).is_err());
        Ok(())
    }

    #[test]
    fn ack_without_native_user_identity_is_not_a_correlated_response() -> Result<()> {
        let mut turn = pending()?;
        turn.observe(&json!({"id":"start:operation-1","result":{"turn":{"id":"turn-1","status":"inProgress","items":[]}}}))?;
        assert_eq!(turn.phase, Phase::Submitted);
        assert!(turn.user_item.is_none());
        assert!(!turn.observe(
            &json!({"method":"item/completed","params":{"threadId":"thread-1","turnId":"turn-1",
            "item":{"type":"agentMessage","id":"stale","text":"must not bind"}}})
        )?);
        assert!(turn.responses.is_empty());
        assert!(turn.interrupt("interrupt-1").is_err());
        Ok(())
    }

    #[test]
    fn stale_threads_turns_and_operations_cannot_bind_or_refresh() -> Result<()> {
        let mut turn = pending()?;
        let mut event = json!({"method":"item/started","params":{"threadId":"other-thread","turnId":"turn-1","item":user(&turn)}});
        assert!(!turn.observe(&event)?);
        replace(&mut event, "/params/threadId", json!("thread-1"))?;
        replace(
            &mut event,
            "/params/item/clientId",
            json!("other-operation"),
        )?;
        assert!(!turn.observe(&event)?);
        assert!(!turn.observe(&json!({"id":"start:other-operation","result":{}}))?);
        assert!(!turn.fresh);
        assert!(turn.turn.is_none());
        turn.observe(&read(&turn, "inProgress"))?;
        let before = turn.clone();
        assert!(!turn.observe(
            &json!({"method":"turn/completed","params":{"threadId":"thread-1",
            "turn":{"id":"other-turn","status":"completed","items":[]}}})
        )?);
        assert_eq!(turn, before);
        Ok(())
    }

    #[test]
    fn duplicate_items_do_not_clear_blockers_or_regress_terminal_evidence() -> Result<()> {
        let mut turn = pending()?;
        turn.observe(&read(&turn, "inProgress"))?;
        let blocked = json!({"id":"question-1","method":"item/tool/requestUserInput","params":{
            "threadId":"thread-1","turnId":"turn-1","itemId":"question-item","isBlocking":true,
            "questions":[{"id":"question","question":"Choose a value"}]}});
        turn.observe(&blocked)?;
        assert_eq!(turn.phase, Phase::InputRequired);
        let mut nonblocking = blocked.clone();
        replace(&mut nonblocking, "/params/isBlocking", json!(false))?;
        replace(
            &mut nonblocking,
            "/params/itemId",
            json!("nonblocking-item"),
        )?;
        turn.observe(&nonblocking)?;
        assert_eq!(
            turn.blocker.as_ref().and_then(|b| b.get("itemId")),
            Some(&json!("question-item"))
        );
        let duplicate = json!({"method":"item/started","params":{"threadId":"thread-1","turnId":"turn-1","item":user(&turn)}});
        turn.observe(&duplicate)?;
        assert_eq!(turn.phase, Phase::InputRequired);
        turn.observe(&read(&turn, "inProgress"))?;
        assert_eq!(turn.phase, Phase::InputRequired);
        turn.observe(&read(&turn, "completed"))?;
        let completed = turn.clone();
        turn.observe(&duplicate)?;
        turn.observe(&read(&turn, "completed"))?;
        assert_eq!(turn, completed);
        assert!(!turn.observe(&blocked)?);
        assert_eq!(turn.phase, Phase::Completed);
        Ok(())
    }

    #[test]
    fn conflicting_identity_or_content_invalidates_without_partial_binding() -> Result<()> {
        for field in ["thread", "turn", "user", "text"] {
            let mut turn = pending()?;
            turn.observe(&read(&turn, "inProgress"))?;
            let mut reply = read(&turn, "completed");
            match field {
                "thread" => replace(&mut reply, "/result/thread/id", json!("other-thread"))?,
                "turn" => replace(&mut reply, "/result/thread/turns/0/id", json!("other-turn"))?,
                "user" => replace(
                    &mut reply,
                    "/result/thread/turns/0/items/0/id",
                    json!("other-user"),
                )?,
                _ => replace(
                    &mut reply,
                    "/result/thread/turns/0/items/0/content/0/text",
                    json!("changed literal"),
                )?,
            }
            assert!(turn.observe(&reply).is_err());
            assert_eq!(turn.turn.as_deref(), Some("turn-1"));
            assert_eq!(turn.user_item.as_deref(), Some("user-1"));
            assert_eq!(turn.phase, Phase::Uncertain);
            assert!(!turn.fresh);
            assert!(turn.submission(1003).is_err());
        }
        Ok(())
    }

    #[test]
    fn absent_partial_or_duplicate_history_is_not_proof_of_non_submission() -> Result<()> {
        for kind in ["absent", "partial", "duplicate"] {
            let mut turn = pending()?;
            let mut history = read(&turn, "completed");
            match kind {
                "absent" => replace(&mut history, "/result/thread/turns", json!([]))?,
                "partial" => replace(
                    &mut history,
                    "/result/thread/turns/0/itemsView",
                    json!("summary"),
                )?,
                _ => replace(
                    &mut history,
                    "/result/thread/turns",
                    json!([snapshot(&turn, "completed"), snapshot(&turn, "completed")]),
                )?,
            }
            let result = turn.observe(&history);
            if kind == "absent" {
                assert!(result.is_ok());
            } else {
                assert!(result.is_err());
            }
            assert_eq!(turn.phase, Phase::Uncertain);
            assert!(turn.submission(1003).is_err());
        }
        Ok(())
    }

    #[test]
    fn native_interrupt_is_a_distinct_at_most_once_intent_with_observed_completion() -> Result<()> {
        let mut turn = pending()?;
        turn.observe(&read(&turn, "inProgress"))?;
        let request = turn
            .interrupt("interrupt-1")?
            .context("interrupt request")?;
        assert_eq!(
            request.pointer("/params/threadId"),
            Some(&json!("thread-1"))
        );
        assert_eq!(request.pointer("/params/turnId"), Some(&json!("turn-1")));
        assert!(turn.interrupt("interrupt-1")?.is_none());
        assert!(turn.interrupt("interrupt-2").is_err());
        turn.observe(&json!({"id":"interrupt:interrupt-1","result":{}}))?;
        assert!(turn.interrupt_acknowledged);
        assert_eq!(turn.phase, Phase::Working);
        turn.observe(&read(&turn, "interrupted"))?;
        assert_eq!(turn.phase, Phase::Interrupted);
        assert!(turn.interrupt("interrupt-1")?.is_none());
        Ok(())
    }

    #[test]
    fn original_deadline_and_output_bounds_survive_native_recovery() -> Result<()> {
        let mut expired = Turn::prepare("expired", "thread-1", "text", 1000)?;
        assert!(expired.submission(1000).is_err());
        assert_eq!(expired.phase, Phase::Prepared);
        let mut turn = pending()?;
        let mut history = read(&turn, "completed");
        let long = "é".repeat(3000);
        replace(
            &mut history,
            "/result/thread/turns/0/items/1/text",
            json!(long),
        )?;
        turn.observe(&history)?;
        let response = turn.responses.get("assistant-1").context("response")?;
        assert!(response.truncated && response.text.len() <= MAX_RESPONSE_TEXT);
        assert_eq!(turn.deadline_ms, 5000);
        replace(
            &mut history,
            "/result/thread/turns/0/items/1/text",
            json!(format!("{long}changed tail")),
        )?;
        assert!(turn.observe(&history).is_err());
        Ok(())
    }

    #[test]
    fn in_progress_history_does_not_freeze_partial_native_response_text() -> Result<()> {
        let mut turn = pending()?;
        let mut partial = read(&turn, "inProgress");
        replace(
            &mut partial,
            "/result/thread/turns/0/items/1/text",
            json!("part"),
        )?;
        turn.observe(&partial)?;
        assert!(turn.responses.is_empty());
        turn.observe(
            &json!({"method":"item/completed","params":{"threadId":"thread-1","turnId":"turn-1",
            "item":{"type":"agentMessage","id":"assistant-1","text":"answer"}}}),
        )?;
        turn.observe(&read(&turn, "completed"))?;
        assert_eq!(turn.phase, Phase::Completed);
        assert_eq!(
            turn.responses.get("assistant-1").context("response")?.text,
            "answer"
        );
        Ok(())
    }

    #[test]
    fn server_request_id_namespace_is_independent_of_client_response_ids() -> Result<()> {
        let mut turn = pending()?;
        turn.observe(&read(&turn, "inProgress"))?;
        turn.observe(&json!({"id":"start:operation-1","method":"item/tool/requestUserInput","params":{
            "threadId":"thread-1","turnId":"turn-1","itemId":"question-1","isBlocking":true,"questions":[]}}))?;
        assert_eq!(turn.phase, Phase::InputRequired);
        Ok(())
    }

    #[test]
    fn retained_blockers_must_match_the_native_turn_and_live_input_scope() -> Result<()> {
        let mut turn = pending()?;
        turn.observe(&read(&turn, "inProgress"))?;
        turn.observe(&json!({"method":"item/tool/requestUserInput","params":{
            "threadId":"thread-1","turnId":"turn-1","itemId":"question-1","isBlocking":true,"questions":[]}}))?;
        turn.validate()?;
        for key in ["threadId", "turnId", "itemId", "questions", "phase"] {
            let mut invalid = turn.clone();
            if key == "phase" {
                invalid.phase = Phase::Prepared;
            } else {
                let object = invalid
                    .blocker
                    .as_mut()
                    .and_then(Value::as_object_mut)
                    .context("blocker")?;
                if matches!(key, "threadId" | "turnId") {
                    object.insert(key.into(), json!("wrong-identity"));
                } else {
                    object.remove(key);
                }
            }
            assert!(
                invalid.validate().is_err(),
                "accepted malformed blocker field {key}"
            );
        }
        Ok(())
    }
}
impl Phase {
    pub fn terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Interrupted | Self::Failed)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    pub text: String,
    pub truncated: bool,
    pub sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Turn {
    pub operation: String,
    pub text: String,
    pub thread: String,
    pub deadline_ms: u64,
    pub phase: Phase,
    pub turn: Option<String>,
    pub user_item: Option<String>,
    pub responses: BTreeMap<String, Response>,
    pub blocker: Option<Value>,
    pub interrupt_operation: Option<String>,
    pub interrupt_acknowledged: bool,
    pub fresh: bool,
    pub problem: Option<String>,
}

fn identity(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control),
        "invalid native identity"
    );
    Ok(())
}
fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .with_context(|| format!("native {key} missing"))
}
fn clipped(text: &str, limit: usize) -> String {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.get(..end).unwrap_or_default().into()
}

impl Turn {
    /// Internal consistency of a persisted record is not provider acceptance.
    pub fn validate(&self) -> Result<()> {
        Self::prepare(&self.operation, &self.thread, &self.text, self.deadline_ms)?;
        for value in self
            .turn
            .iter()
            .chain(self.user_item.iter())
            .chain(self.responses.keys())
        {
            identity(value)?;
        }
        ensure!(
            self.user_item.is_none() || self.turn.is_some(),
            "native user identity lacks a turn"
        );
        ensure!(
            self.responses.is_empty() || self.user_item.is_some(),
            "native responses lack correlated input"
        );
        ensure!(
            self.responses.len() <= MAX_RESPONSES
                && self
                    .responses
                    .values()
                    .all(|response| response.text.len() <= MAX_RESPONSE_TEXT
                        && response.sha256.len() == 64
                        && response.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())),
            "invalid retained native response"
        );
        ensure!(
            self.problem
                .as_ref()
                .is_none_or(|problem| problem.len() <= 512),
            "native problem exceeds bound"
        );
        ensure!(
            self.blocker
                .as_ref()
                .map(serde_json::to_vec)
                .transpose()?
                .is_none_or(|bytes| bytes.len() <= 8192),
            "native blocker exceeds bound"
        );
        if let Some(blocker) = &self.blocker {
            ensure!(
                matches!(self.phase, Phase::InputRequired | Phase::Uncertain)
                    && self.user_item.is_some()
                    && string(blocker, "threadId")? == self.thread
                    && Some(string(blocker, "turnId")?) == self.turn.as_deref()
                    && blocker.get("isBlocking").and_then(Value::as_bool) == Some(true)
                    && blocker.get("questions").and_then(Value::as_array).is_some(),
                "invalid retained native blocker identity/state"
            );
            identity(string(blocker, "itemId")?)?;
        }
        ensure!(
            self.interrupt_operation
                .as_ref()
                .is_none_or(
                    |operation| super::super::model::id(operation) && self.user_item.is_some()
                )
                && (!self.interrupt_acknowledged || self.interrupt_operation.is_some()),
            "invalid native interrupt identity"
        );
        if self.phase == Phase::Prepared {
            ensure!(
                self.turn.is_none()
                    && self.user_item.is_none()
                    && self.interrupt_operation.is_none()
                    && !self.fresh,
                "prepared native input has execution evidence"
            );
        }
        if matches!(self.phase, Phase::Working | Phase::InputRequired) || self.phase.terminal() {
            ensure!(
                self.user_item.is_some(),
                "native phase has no correlated input"
            );
        }
        if self.phase == Phase::InputRequired {
            ensure!(
                self.blocker
                    .as_ref()
                    .and_then(|blocker| blocker.get("isBlocking"))
                    .and_then(Value::as_bool)
                    == Some(true),
                "native input-required phase lacks a blocker"
            );
        }
        ensure!(
            !self.phase.terminal() || self.blocker.is_none(),
            "terminal native turn retains a live blocker"
        );
        Ok(())
    }

    pub fn prepare(operation: &str, thread: &str, text: &str, deadline_ms: u64) -> Result<Self> {
        ensure!(
            super::super::model::id(operation),
            "invalid native operation ID"
        );
        identity(thread)?;
        ensure!(
            !text.is_empty() && text.len() <= MAX_TEXT && !text.contains('\0'),
            "native input exceeds text bounds"
        );
        ensure!(deadline_ms != 0, "native input deadline is missing");
        Ok(Self {
            operation: operation.into(),
            text: text.into(),
            thread: thread.into(),
            deadline_ms,
            phase: Phase::Prepared,
            turn: None,
            user_item: None,
            responses: BTreeMap::new(),
            blocker: None,
            interrupt_operation: None,
            interrupt_acknowledged: false,
            fresh: false,
            problem: None,
        })
    }

    /// The owner must durably persist this transition BEFORE writing the returned
    /// request. Reopening any submitted/uncertain record never grants replay.
    pub fn submission(&mut self, now_ms: u64) -> Result<Value> {
        ensure!(
            self.phase == Phase::Prepared,
            "native input already attempted; reconcile without replay"
        );
        ensure!(now_ms < self.deadline_ms, "native input deadline expired");
        self.phase = Phase::Submitted;
        Ok(
            json!({"id":format!("start:{}", self.operation),"method":"turn/start",
            "params":{"threadId":self.thread,"clientUserMessageId":self.operation,
                "input":[{"type":"text","text":self.text}],"approvalPolicy":"never"}}),
        )
    }

    /// A read is safe after caller/event loss. History absence is uncertainty,
    /// never evidence that the original submission had no effect.
    pub fn read_request(&self) -> Value {
        json!({"id":format!("read:{}", self.operation),"method":"thread/read",
            "params":{"threadId":self.thread,"includeTurns":true}})
    }

    /// Persist interrupt intent before writing, exactly as for submission. An
    /// acknowledgement alone does not prove that the native turn was interrupted.
    pub fn interrupt(&mut self, operation: &str) -> Result<Option<Value>> {
        ensure!(
            super::super::model::id(operation),
            "invalid native interrupt operation"
        );
        if let Some(existing) = &self.interrupt_operation {
            ensure!(
                existing == operation,
                "native interrupt already has a different operation ID"
            );
            return Ok(None);
        }
        ensure!(
            !self.phase.terminal() && self.user_item.is_some(),
            "native turn identity is not active and correlated"
        );
        let turn = self.turn.as_ref().context("native turn identity missing")?;
        let request = json!({"id":format!("interrupt:{operation}"),"method":"turn/interrupt",
            "params":{"threadId":self.thread,"turnId":turn}});
        self.interrupt_operation = Some(operation.into());
        Ok(Some(request))
    }

    pub fn invalidate(&mut self, reason: &str) {
        self.fresh = false;
        self.problem = Some(clipped(reason, 512));
        if !self.phase.terminal() {
            self.phase = Phase::Uncertain;
        }
    }

    /// Apply atomically: malformed or contradictory identity cannot leave a
    /// partially accepted binding. Unrelated frames do not refresh evidence.
    pub fn observe(&mut self, frame: &Value) -> Result<bool> {
        let mut next = self.clone();
        match next.apply(frame) {
            Ok(true) => {
                *self = next;
                Ok(true)
            }
            Ok(false) => Ok(false),
            Err(error) => {
                self.invalidate(&error.to_string());
                Err(error)
            }
        }
    }

    fn bind_turn(&mut self, id: &str) -> Result<()> {
        identity(id)?;
        ensure!(
            self.turn.as_deref().is_none_or(|old| old == id),
            "native turn identity changed"
        );
        self.turn = Some(id.into());
        Ok(())
    }

    fn user(&mut self, turn: &str, item: &Value) -> Result<bool> {
        if item.get("type").and_then(Value::as_str) != Some("userMessage")
            || item.get("clientId").and_then(Value::as_str) != Some(&self.operation)
        {
            return Ok(false);
        }
        let content = item
            .get("content")
            .and_then(Value::as_array)
            .context("native user content missing")?;
        ensure!(
            content.len() == 1
                && content
                    .first()
                    .is_some_and(
                        |entry| entry.get("type").and_then(Value::as_str) == Some("text")
                            && entry.get("text").and_then(Value::as_str) == Some(&self.text)
                    ),
            "native input content differs from the retained literal intent"
        );
        let id = string(item, "id")?;
        identity(id)?;
        self.bind_turn(turn)?;
        ensure!(
            self.user_item.as_deref().is_none_or(|old| old == id),
            "native user item identity changed"
        );
        let new_binding = self.user_item.is_none();
        self.user_item = Some(id.into());
        if new_binding && !self.phase.terminal() {
            self.phase = Phase::Working;
        }
        if new_binding {
            self.fresh = true;
            self.problem = None;
        }
        Ok(true)
    }

    fn response(&mut self, item: &Value) -> Result<()> {
        if item.get("type").and_then(Value::as_str) != Some("agentMessage") {
            return Ok(());
        }
        ensure!(
            self.user_item.is_some(),
            "native response has no correlated input"
        );
        let id = string(item, "id")?;
        identity(id)?;
        let text = string(item, "text")?;
        let response = Response {
            text: clipped(text, MAX_RESPONSE_TEXT),
            truncated: text.len() > MAX_RESPONSE_TEXT,
            sha256: format!("{:x}", Sha256::digest(text.as_bytes())),
        };
        if let Some(previous) = self.responses.get(id) {
            ensure!(previous == &response, "completed native response changed");
        } else {
            ensure!(
                self.responses.len() < MAX_RESPONSES,
                "native response item limit exceeded"
            );
            self.responses.insert(id.into(), response);
        }
        Ok(())
    }

    fn turn_snapshot(&mut self, turn: &Value, authoritative: bool) -> Result<bool> {
        let id = string(turn, "id")?;
        if let Some(view) = turn.get("itemsView") {
            ensure!(
                view.as_str() == Some("full"),
                "native turn history is incomplete"
            );
        }
        let items = turn
            .get("items")
            .and_then(Value::as_array)
            .context("native turn items missing")?;
        ensure!(items.len() <= 256, "native turn item limit exceeded");
        let mut matched = false;
        for item in items {
            matched |= self.user(id, item)?;
        }
        if !matched && self.turn.as_deref() != Some(id) {
            return Ok(false);
        }
        if self.user_item.is_none() {
            return Ok(false);
        }
        // In-progress history may contain partial agent text. Only completed
        // turns or explicit item/completed events freeze immutable responses.
        if string(turn, "status")? == "completed" {
            for item in items {
                self.response(item)?;
            }
        }
        let phase = match string(turn, "status")? {
            "inProgress"
                if self
                    .blocker
                    .as_ref()
                    .and_then(|blocker| blocker.get("isBlocking"))
                    .and_then(Value::as_bool)
                    == Some(true) =>
            {
                Phase::InputRequired
            }
            "inProgress" => Phase::Working,
            "completed" => Phase::Completed,
            "interrupted" => Phase::Interrupted,
            "failed" => Phase::Failed,
            _ => anyhow::bail!("unknown native turn status"),
        };
        if self.phase.terminal() {
            if phase.terminal() {
                ensure!(self.phase == phase, "native terminal status changed");
            }
        } else if authoritative || phase.terminal() {
            self.phase = phase;
        }
        if self.phase.terminal() {
            self.blocker = None;
        }
        self.fresh = true;
        self.problem = None;
        Ok(true)
    }

    fn apply(&mut self, frame: &Value) -> Result<bool> {
        ensure!(
            self.phase != Phase::Prepared,
            "native evidence precedes submission intent"
        );
        if frame.get("method").is_none()
            && let Some(id) = frame.get("id").and_then(Value::as_str)
        {
            if id == format!("start:{}", self.operation) {
                ensure!(
                    frame.get("error").is_none(),
                    "native submission returned an error; input is not replayable"
                );
                let turn = frame
                    .pointer("/result/turn")
                    .context("native submission response missing turn")?;
                self.bind_turn(string(turn, "id")?)?;
                self.turn_snapshot(turn, false)?;
                return Ok(true);
            }
            if id == format!("read:{}", self.operation) {
                ensure!(frame.get("error").is_none(), "native history query failed");
                let thread = frame
                    .pointer("/result/thread")
                    .context("native history missing thread")?;
                ensure!(
                    string(thread, "id")? == self.thread,
                    "native history thread identity changed"
                );
                let turns = thread
                    .get("turns")
                    .and_then(Value::as_array)
                    .context("native thread history missing turns")?;
                ensure!(
                    turns.len() <= 256,
                    "native history exceeds bounded recovery"
                );
                let mut found = false;
                for turn in turns {
                    if self.turn_snapshot(turn, true)? {
                        ensure!(!found, "native operation occurs in multiple turns");
                        found = true;
                    }
                }
                if !found {
                    self.invalidate(
                        "native input absent from bounded history; no replay authorized",
                    );
                }
                return Ok(true);
            }
            if self
                .interrupt_operation
                .as_ref()
                .is_some_and(|operation| id == format!("interrupt:{operation}"))
            {
                ensure!(
                    frame.get("error").is_none() && frame.get("result").is_some(),
                    "native interrupt acknowledgement missing"
                );
                self.interrupt_acknowledged = true;
                return Ok(true);
            }
        }
        let Some(method) = frame.get("method").and_then(Value::as_str) else {
            return Ok(false);
        };
        let params = frame
            .get("params")
            .context("native event parameters missing")?;
        if params.get("threadId").and_then(Value::as_str) != Some(&self.thread) {
            return Ok(false);
        }
        match method {
            "turn/started" | "turn/completed" => {
                let turn = params.get("turn").context("native event turn missing")?;
                self.turn_snapshot(turn, method == "turn/completed")
            }
            "item/started" | "item/completed" => {
                let turn = string(params, "turnId")?;
                let item = params.get("item").context("native event item missing")?;
                if self.user(turn, item)? {
                    return Ok(true);
                }
                if self.turn.as_deref() != Some(turn) || self.user_item.is_none() {
                    return Ok(false);
                }
                if method == "item/completed" {
                    self.response(item)?;
                }
                Ok(true)
            }
            "item/tool/requestUserInput" => {
                if self.turn.as_deref() != Some(string(params, "turnId")?)
                    || self.user_item.is_none()
                    || self.phase.terminal()
                {
                    return Ok(false);
                }
                identity(string(params, "itemId")?)?;
                ensure!(
                    params.get("isBlocking").and_then(Value::as_bool).is_some(),
                    "native blocker scope missing"
                );
                ensure!(
                    params.get("questions").and_then(Value::as_array).is_some(),
                    "native blocker questions missing"
                );
                ensure!(
                    serde_json::to_vec(params)?.len() <= 8192,
                    "native blocker exceeds evidence bound"
                );
                if params.get("isBlocking").and_then(Value::as_bool) == Some(true) {
                    self.blocker = Some(params.clone());
                    self.phase = Phase::InputRequired;
                }
                self.fresh = true;
                Ok(true)
            }
            _ => Ok(false),
        }
    }
}
