//! The fux side of the lifecycle: `Effect::FuxCall`s correlated by a runtime [`Calls`] table,
//! their `Inbound::FuxReply`s, the `fux/events+watch` items and link changes, and the
//! reconciliation of every attempt against `fux/workspace.list`. Nothing here is persisted:
//! after a restart no call is pending, so restored records are reconciled, never resent.

use std::collections::HashMap;

use bevy_ecs::prelude::*;
use bevy_time::Time;
use fux::remote::input_methods::{InputReceipt, PaneFinal, final_codes};
use fux::remote::methods::{PaneEntry, WorkspaceList};
use serde_json::{Value, json};

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallKind {
    /// `fux/root.new` or `fux/workspace.new` for a Launch operation.
    Launch,
    /// `fux/input.reserve` for a prompt.
    Reserve,
    /// `fux/input.submit` for a prompt.
    Submit,
    /// `fux/input.status` for a prompt.
    Status,
    /// `fux/pane.capture` for an attempt.
    Capture,
    /// `fux/pane.final` for an attempt.
    Final,
    /// `fux/pane.close` for an attempt.
    Close,
    /// `fux/workspace.list`, shared by every attempt.
    List,
    /// `fux/workspace.kill` of an ephemeral workspace.
    Kill,
}

#[derive(Debug, Clone, Copy)]
pub struct Call {
    pub kind: CallKind,
    pub entity: Entity,
}

/// Calls in flight, by id. `listing` coalesces listing requests: one at a time.
#[derive(Resource, Debug, Default)]
pub struct Calls {
    next: u64,
    pending: HashMap<u64, Call>,
    listing: bool,
}

impl Calls {
    pub fn pending(&self) -> usize {
        self.pending.len()
    }
}

const TAG_MASK: u64 = 0xff << 56;

/// Emits one `fux/*` call for `entity` and returns its id.
pub fn call(world: &mut World, kind: CallKind, entity: Entity, method: &str, params: Value) -> u64 {
    let id = {
        let mut calls = world.resource_mut::<Calls>();
        calls.next += 1;
        let id = CALL_TAG | (calls.next & !TAG_MASK);
        calls.pending.insert(id, Call { kind, entity });
        id
    };
    effect(
        world,
        Effect::FuxCall {
            call: id,
            method: method.to_owned(),
            params,
        },
    );
    id
}

/// One `fux/workspace.list`, unless one is already in flight.
pub fn request_listing(world: &mut World) {
    if world.resource::<Calls>().listing {
        return;
    }
    world.resource_mut::<Calls>().listing = true;
    call(
        world,
        CallKind::List,
        Entity::PLACEHOLDER,
        "fux/workspace.list",
        json!({}),
    );
}

fn request_final(world: &mut World, attempt: Entity) {
    if world.get::<PendingCall>(attempt).is_some() {
        return;
    }
    let pane = pane_handle(world, attempt).map_or(0, |h| h.pane);
    if pane == 0 {
        return;
    }
    let id = call(
        world,
        CallKind::Final,
        attempt,
        "fux/pane.final",
        json!({ "pane": pane }),
    );
    world.entity_mut(attempt).insert(PendingCall(id));
}

fn request_close(world: &mut World, attempt: Entity) {
    if world.get::<PendingCall>(attempt).is_some() {
        return;
    }
    let pane = pane_handle(world, attempt).map_or(0, |h| h.pane);
    let id = call(
        world,
        CallKind::Close,
        attempt,
        "fux/pane.close",
        json!({ "pane": pane }),
    );
    world.entity_mut(attempt).insert(PendingCall(id));
}

fn request_capture(world: &mut World, attempt: Entity) {
    if world.get::<PendingCall>(attempt).is_some() {
        return;
    }
    let pane = pane_handle(world, attempt).map_or(0, |h| h.pane);
    let id = call(
        world,
        CallKind::Capture,
        attempt,
        "fux/pane.capture",
        json!({ "pane": pane, "scrollback": 0 }),
    );
    world.entity_mut(attempt).insert(PendingCall(id));
}

pub(crate) fn request_status(world: &mut World, prompt: Entity, operation: u64) {
    let id = call(
        world,
        CallKind::Status,
        prompt,
        "fux/input.status",
        json!({ "operation": operation }),
    );
    world.entity_mut(prompt).insert(PendingCall(id));
}

/// `Submitting` is committed, then `fux/input.submit` goes out (lifecycle-transitions.md:21).
pub fn submit_input(world: &mut World, prompt: Entity, operation: u64) {
    let text = world
        .get::<PromptText>(prompt)
        .map(|t| t.0.clone())
        .unwrap_or_default();
    set_delivery(world, prompt, Delivery::Submitting);
    let id = call(
        world,
        CallKind::Submit,
        prompt,
        "fux/input.submit",
        json!({ "operation": operation, "keys": keys_for(&text) }),
    );
    world.entity_mut(prompt).insert(PendingCall(id));
}

/// An ephemeral workspace is killed once its attempt finished (`zor run`).
pub fn after_finished(world: &mut World, attempt: Entity) {
    let Some(op) = launch_of(world, attempt) else {
        return;
    };
    let Some(template) = world.get::<LaunchTemplate>(op) else {
        return;
    };
    if template.ephemeral {
        let name = template.workspace.clone();
        call(
            world,
            CallKind::Kill,
            attempt,
            "fux/workspace.kill",
            json!({ "name": name }),
        );
    }
}

/// The JSON-RPC code of an adapter error string (`error <code>: ...`); `None` for transport
/// failures, whose outcome is unknown.
pub fn rpc_code(error: &str) -> Option<i64> {
    let rest = error.strip_prefix("error ")?;
    let end = rest.find(':')?;
    rest.get(..end)?.trim().parse().ok()
}

// ---------------------------------------------------------------------------------------------
// Ingest
// ---------------------------------------------------------------------------------------------

enum Item {
    Link(Option<String>),
    Gap,
    Event(String, Value),
    Reply(u64, Result<Value, String>),
}

/// `PreUpdate`/`Completions`: applies every fux message of this update.
pub fn ingest(world: &mut World) {
    let batch: Vec<Item> = world
        .resource::<Messages<Inbound>>()
        .iter_current_update_messages()
        .filter_map(|m| match m {
            Inbound::FuxLink { instance } => Some(Item::Link(instance.clone())),
            Inbound::FuxGap { .. } => Some(Item::Gap),
            Inbound::FuxEvent { name, body, .. } => Some(Item::Event(name.clone(), body.clone())),
            Inbound::FuxReply { call, result } if call & TAG_MASK == CALL_TAG => {
                Some(Item::Reply(*call, result.clone()))
            }
            _ => None,
        })
        .collect();
    for item in batch {
        match item {
            Item::Link(instance) => link(world, instance),
            Item::Gap => request_listing(world),
            Item::Event(name, body) => event(world, &name, &body),
            Item::Reply(id, result) => reply(world, id, result),
        }
    }
}

/// Attempts that still describe a pane: not `Finished`, not `Lost`.
fn open_attempts(world: &mut World) -> Vec<(Entity, PaneHandle, AttemptState)> {
    world
        .query_filtered::<(Entity, &PaneHandle, &AttemptState), (With<Attempt>, Without<Lost>)>()
        .iter(world)
        .filter(|(_, _, s)| **s != AttemptState::Finished)
        .map(|(e, h, s)| (e, h.clone(), *s))
        .collect()
}

fn link(world: &mut World, instance: Option<String>) {
    let attempts = open_attempts(world);
    match instance {
        Some(instance) => {
            world.resource_mut::<Link>().instance = Some(instance.clone());
            let mut reconcile = world.resource::<Link>().sweep_due;
            for (attempt, handle, _) in attempts {
                if handle.instance.is_empty() {
                    continue;
                }
                if handle.instance != instance {
                    observe(
                        world,
                        attempt,
                        Observed::Replaced(format!(
                            "fux instance {instance} replaced {}",
                            handle.instance
                        )),
                    );
                } else {
                    reconcile = true;
                }
            }
            if reconcile {
                request_listing(world);
            }
        }
        None => {
            world.resource_mut::<Link>().instance = None;
            for (attempt, _, state) in attempts {
                if state != AttemptState::Pending {
                    observe(world, attempt, Observed::Missing("fux link dropped".into()));
                }
            }
        }
    }
}

fn event(world: &mut World, name: &str, body: &Value) {
    let Some(pane) = body.get("pane").and_then(Value::as_u64) else {
        return;
    };
    let Some(instance) = world.resource::<Link>().instance.clone() else {
        return;
    };
    let targets: Vec<Entity> = open_attempts(world)
        .into_iter()
        .filter(|(_, h, _)| h.pane == pane && h.instance == instance)
        .map(|(e, _, _)| e)
        .collect();
    for attempt in targets {
        match name {
            "PaneSpawned" => {
                let pid = body
                    .get("pid")
                    .and_then(Value::as_u64)
                    .and_then(|p| u32::try_from(p).ok());
                observe(world, attempt, Observed::Live { pid });
            }
            "PaneExited" | "PaneClosed" => {
                world.entity_mut(attempt).insert(AwaitFinal);
                request_final(world, attempt);
            }
            _ => {}
        }
    }
}

fn reply(world: &mut World, id: u64, result: Result<Value, String>) {
    let Some(Call { kind, entity }) = world.resource_mut::<Calls>().pending.remove(&id) else {
        return;
    };
    if kind != CallKind::List && world.get::<PendingCall>(entity) == Some(&PendingCall(id)) {
        world.entity_mut(entity).remove::<PendingCall>();
    }
    match kind {
        CallKind::Launch => launch_reply(world, entity, result),
        CallKind::Reserve => reserve_reply(world, entity, result),
        CallKind::Submit => submit_reply(world, entity, result),
        CallKind::Status => status_reply(world, entity, result),
        CallKind::Capture => {
            if let Ok(value) = result
                && let Some(seq) = value.get("seq").and_then(Value::as_u64)
                && world.get::<Attempt>(entity).is_some()
            {
                world.entity_mut(entity).insert(LastCapture { seq });
            }
        }
        CallKind::Final => final_reply(world, entity, result),
        CallKind::Close => close_reply(world, entity, result),
        CallKind::List => {
            world.resource_mut::<Calls>().listing = false;
            if let Ok(value) = result
                && let Ok(list) = serde_json::from_value::<WorkspaceList>(value)
            {
                reconcile(world, &list);
            }
        }
        CallKind::Kill => {}
    }
}

fn launch_reply(world: &mut World, operation: Entity, result: Result<Value, String>) {
    let Some(attempt) = world.get::<OperationOf>(operation).map(|o| o.0) else {
        return;
    };
    match result {
        Ok(value) => {
            let Some(pane) = value.get("pane").and_then(Value::as_u64) else {
                set_phase(world, operation, OperationPhase::Uncertain);
                observe(
                    world,
                    attempt,
                    Observed::Missing("creation reply without a pane id".into()),
                );
                return;
            };
            observe(world, attempt, Observed::Created { pane });
            request_listing(world);
        }
        Err(error) => {
            // A refusal created nothing, a lost reply may have: both leave the operation to
            // reconciliation by marker (TASKS.md:129-133); neither is resent.
            set_phase(world, operation, OperationPhase::Uncertain);
            world
                .entity_mut(operation)
                .insert(Problem(clip(format!("creation: {error}"))));
            observe(
                world,
                attempt,
                Observed::Missing(format!("creation: {error}")),
            );
            if rpc_code(&error).is_none() {
                request_listing(world);
            }
        }
    }
}

fn receipt_from(world: &World, prompt: Entity, receipt: &InputReceipt) -> Receipt {
    let attempt = world.get::<PromptOf>(prompt).map(|p| p.0);
    let handle = attempt
        .and_then(|a| pane_handle(world, a))
        .unwrap_or_default();
    Receipt {
        instance: handle.instance,
        pane: receipt.pane.unwrap_or(handle.pane),
        operation: receipt.operation,
        state: receipt.state.clone(),
        bytes_written: receipt.bytes_written,
        seq: receipt.seq,
        expires_ms: receipt.expires_ms,
    }
}

fn parse_receipt(value: Value) -> Option<InputReceipt> {
    serde_json::from_value(value).ok()
}

fn reserve_reply(world: &mut World, prompt: Entity, result: Result<Value, String>) {
    if world.get::<Delivery>(prompt) != Some(&Delivery::Prepared) {
        return;
    }
    match result.map(parse_receipt) {
        Ok(Some(receipt)) => {
            let record = receipt_from(world, prompt, &receipt);
            world.entity_mut(prompt).insert(record);
            set_delivery(world, prompt, Delivery::Reserved);
            if world.get::<WaitState>(prompt) == Some(&WaitState::Cancelled) {
                set_delivery(world, prompt, Delivery::Released);
                return;
            }
            match crate::providers::arm(world, prompt) {
                Ok(()) => submit_input(world, prompt, receipt.operation),
                Err(e) => {
                    world
                        .entity_mut(prompt)
                        .insert(Problem(clip(format!("arm: {e}; retry submit"))));
                }
            }
        }
        Ok(None) => {
            world
                .entity_mut(prompt)
                .insert(Problem("reserve: malformed receipt; retry submit".into()));
        }
        Err(error) => {
            // Reservation alone cannot type bytes: a lost or refused reply is safe to retry
            // (TASKS.md:211).
            world
                .entity_mut(prompt)
                .insert(Problem(clip(format!("reserve: {error}; retry submit"))));
        }
    }
}

fn apply_receipt_state(world: &mut World, prompt: Entity, state: &str) {
    let Some(delivery) = world.get::<Delivery>(prompt).copied() else {
        return;
    };
    if !matches!(delivery, Delivery::Submitting | Delivery::Uncertain) {
        return;
    }
    match state {
        "submitted" => set_delivery(world, prompt, Delivery::Delivered),
        "reserved" => {
            // fux never wrote: the submit was lost before the write (lifecycle-transitions.md:21).
            set_delivery(world, prompt, Delivery::Uncertain);
            set_delivery(world, prompt, Delivery::Reserved);
        }
        other => {
            set_delivery(world, prompt, Delivery::Uncertain);
            if other == "expired" {
                world
                    .entity_mut(prompt)
                    .insert(Problem("receipt expired before delivery was known".into()));
            }
        }
    }
}

fn settle_released(world: &mut World, prompt: Entity) {
    if world.get::<WaitState>(prompt) == Some(&WaitState::Cancelled)
        && let Some(delivery) = world.get::<Delivery>(prompt).copied()
        && delivery != Delivery::Released
        && delivery.may_become(Delivery::Released)
    {
        set_delivery(world, prompt, Delivery::Released);
    }
}

fn submit_reply(world: &mut World, prompt: Entity, result: Result<Value, String>) {
    if world.get::<Delivery>(prompt) != Some(&Delivery::Submitting) {
        return;
    }
    match result {
        Ok(value) => match parse_receipt(value) {
            Some(receipt) => {
                let record = receipt_from(world, prompt, &receipt);
                world.entity_mut(prompt).insert(record);
                apply_receipt_state(world, prompt, &receipt.state);
            }
            None => {
                set_delivery(world, prompt, Delivery::Uncertain);
                world
                    .entity_mut(prompt)
                    .insert(Problem("submit: malformed receipt".into()));
            }
        },
        Err(error) => {
            if rpc_code(&error).is_some() {
                // fux refused (human input took the reservation, pane gone): nothing was
                // written under this operation and it is not retried as a fresh one.
                set_delivery(world, prompt, Delivery::Failed);
            } else {
                set_delivery(world, prompt, Delivery::Uncertain);
            }
            world
                .entity_mut(prompt)
                .insert(Problem(clip(format!("submit: {error}"))));
        }
    }
    settle_released(world, prompt);
}

fn status_reply(world: &mut World, prompt: Entity, result: Result<Value, String>) {
    match result {
        Ok(value) => {
            if let Some(receipt) = parse_receipt(value) {
                let record = receipt_from(world, prompt, &receipt);
                world.entity_mut(prompt).insert(record);
                apply_receipt_state(world, prompt, &receipt.state);
            }
        }
        Err(error) => {
            if world.get::<Delivery>(prompt) == Some(&Delivery::Submitting) {
                set_delivery(world, prompt, Delivery::Uncertain);
            }
            world
                .entity_mut(prompt)
                .insert(Problem(clip(format!("status: {error}"))));
        }
    }
    settle_released(world, prompt);
}

fn final_reply(world: &mut World, attempt: Entity, result: Result<Value, String>) {
    match result {
        Ok(value) => match serde_json::from_value::<PaneFinal>(value) {
            Ok(record) => {
                world.entity_mut(attempt).remove::<AwaitFinal>();
                let evidence = evidence_from(&record);
                let mut handle = pane_handle(world, attempt).unwrap_or_default();
                if handle.stream.is_empty() {
                    handle.stream = record.stream;
                    world.entity_mut(attempt).insert(handle);
                }
                observe(world, attempt, Observed::Final(evidence));
            }
            Err(e) => observe(
                world,
                attempt,
                Observed::Missing(format!("final: malformed record: {e}")),
            ),
        },
        Err(error) => match rpc_code(&error) {
            Some(code) if code == i64::from(final_codes::PENDING) => {
                // Still running: the heartbeat asks again.
            }
            Some(_) => {
                world.entity_mut(attempt).remove::<AwaitFinal>();
                observe(world, attempt, Observed::Missing(format!("final: {error}")));
            }
            None => observe(world, attempt, Observed::Missing(format!("final: {error}"))),
        },
    }
}

/// `FinalEvidence` from fux's record: the screen joined by newlines, bounded to 4096 bytes.
pub fn evidence_from(record: &PaneFinal) -> FinalEvidence {
    let mut output = record.screen.join("\n");
    let mut truncated = false;
    if output.len() > MAX_FINAL_OUTPUT_BYTES {
        let mut cut = MAX_FINAL_OUTPUT_BYTES;
        while !output.is_char_boundary(cut) {
            cut -= 1;
        }
        output.truncate(cut);
        truncated = true;
    }
    FinalEvidence {
        exit_code: Some(record.exit_code),
        seq: record.last_seq,
        output,
        truncated,
    }
}

fn close_reply(world: &mut World, attempt: Entity, result: Result<Value, String>) {
    match result {
        Ok(_) => {
            world.entity_mut(attempt).insert((CloseSent, AwaitFinal));
            request_final(world, attempt);
        }
        Err(error) => {
            // Kill acceptance was not obtained; the pane may already be gone, so final
            // evidence is still sought (invariant 19). A retry of `stop` re-validates.
            world
                .entity_mut(attempt)
                .insert((AwaitFinal, Problem(clip(format!("close: {error}")))));
            request_final(world, attempt);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Reconciliation against the listing
// ---------------------------------------------------------------------------------------------

fn panes_of(list: &WorkspaceList) -> Vec<(&str, &PaneEntry)> {
    list.workspaces
        .iter()
        .flat_map(|w| {
            w.roots
                .iter()
                .flat_map(move |r| r.panes.iter().map(move |p| (w.name.as_str(), p)))
        })
        .collect()
}

/// Matches every open attempt of the connected instance against the listing: by pane id, or
/// by launch marker when creation's reply was lost. Live entries go `Live` (and are closed
/// when a stop is pending), exited entries ask for final evidence, missing panes ask for a
/// retained final record; ambiguity stays `Uncertain`. Completes the startup sweep (27).
fn reconcile(world: &mut World, list: &WorkspaceList) {
    let Some(instance) = world.resource::<Link>().instance.clone() else {
        return;
    };
    let panes = panes_of(list);
    let attempts = open_attempts(world);
    for (attempt, handle, state) in attempts {
        if state == AttemptState::Pending
            || (!handle.instance.is_empty() && handle.instance != instance)
        {
            continue;
        }
        let entry = if handle.pane == 0 {
            let marker = world
                .get::<LaunchMarker>(attempt)
                .map(|m| format!("{LAUNCH_ID_VAR}={}", m.0));
            let Some(marker) = marker else {
                continue;
            };
            let matches: Vec<&PaneEntry> = panes
                .iter()
                .filter(|(_, p)| p.argv.contains(&marker))
                .map(|(_, p)| *p)
                .collect();
            match matches.as_slice() {
                [one] => {
                    observe(world, attempt, Observed::Created { pane: one.id });
                    Some(*one)
                }
                [] => {
                    if world.get::<PendingCall>(attempt).is_none() {
                        observe(
                            world,
                            attempt,
                            Observed::Missing("no pane carries the launch marker".into()),
                        );
                    }
                    None
                }
                _ => {
                    observe(
                        world,
                        attempt,
                        Observed::Missing(
                            "ambiguous creation: several panes carry the marker".into(),
                        ),
                    );
                    None
                }
            }
        } else {
            let found = panes
                .iter()
                .find(|(_, p)| p.id == handle.pane)
                .map(|(_, p)| *p);
            if found.is_none() {
                world.entity_mut(attempt).insert(AwaitFinal);
                request_final(world, attempt);
            }
            found
        };
        let Some(entry) = entry else {
            continue;
        };
        if let (Some(expected), Some(actual)) = (handle.pid, entry.pid)
            && expected != actual
        {
            observe(
                world,
                attempt,
                Observed::Missing(format!(
                    "pane {} runs pid {actual}, recorded {expected}",
                    entry.id
                )),
            );
            continue;
        }
        match entry.state.as_str() {
            "live" => {
                observe(world, attempt, Observed::Live { pid: entry.pid });
                if state == AttemptState::Finishing && world.get::<CloseSent>(attempt).is_none() {
                    request_close(world, attempt);
                }
            }
            "starting" => {}
            _ => {
                world.entity_mut(attempt).insert(AwaitFinal);
                request_final(world, attempt);
            }
        }
    }
    world.resource_mut::<Link>().sweep_due = false;
}

// ---------------------------------------------------------------------------------------------
// Heartbeat
// ---------------------------------------------------------------------------------------------

/// `Update`/`Lifecycle`: per attempt with a [`Heartbeat`], once per period: receipt status
/// for prompts whose delivery or wait is unsettled, a capture while a delivered prompt waits,
/// and final evidence while it is due.
pub fn heartbeat(world: &mut World) {
    let delta = world.resource::<Time<bevy_time::Real>>().delta();
    let due: Vec<Entity> = world
        .query::<(Entity, &mut Heartbeat)>()
        .iter_mut(world)
        .filter_map(|(e, mut h)| h.0.tick(delta).just_finished().then_some(e))
        .collect();
    for attempt in due {
        if world.get::<AwaitFinal>(attempt).is_some() {
            request_final(world, attempt);
        }
        let prompts: Vec<Entity> = world
            .get::<Prompts>(attempt)
            .map(|p| p.iter().collect())
            .unwrap_or_default();
        let mut capture = false;
        for prompt in prompts {
            if world.get::<PendingCall>(prompt).is_some() {
                continue;
            }
            let Some(receipt) = world.get::<Receipt>(prompt).cloned() else {
                continue;
            };
            let delivery = world.get::<Delivery>(prompt).copied().unwrap_or_default();
            let waiting = world
                .get::<WaitState>(prompt)
                .is_some_and(|w| !w.is_terminal());
            match delivery {
                Delivery::Submitting | Delivery::Uncertain => {
                    request_status(world, prompt, receipt.operation);
                }
                Delivery::Delivered if waiting => {
                    request_status(world, prompt, receipt.operation);
                    capture = true;
                }
                _ => {}
            }
        }
        if capture && world.get::<AttemptState>(attempt) == Some(&AttemptState::Live) {
            request_capture(world, attempt);
        }
    }
}
