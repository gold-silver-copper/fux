//! Durable terminal input coordination. Receipts establish PTY delivery only.
use super::{model::*, store::Store};
use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    path::Path,
    time::{Duration, Instant},
};

/// One reconcile round after the prompt window: `wait::check_until` gives the reconcile
/// (`input-status`) up to 6 s and the evaluation that follows up to 4 s.
const RECONCILE_GRACE_MS: u64 = 10_000;

/// The receipt retention zor asks fux for. zor reads the receipt through `input-status` for as
/// long as the prompt is open: every reconcile until delivery is `delivered`/`failed`
/// (`run_until`, `wait::check_until`, the service's 1 s recovery loop), a late binding
/// (`binding.rs`) and the arm retirement (`integration.rs`). So the receipt must outlive the
/// prompt's remaining window plus one reconcile round; anything longer is waste, anything
/// beyond fux's ceiling would be clamped there anyway, so zor clamps first.
pub(super) fn input_retain_ms(prompt: &Prompt, now_ms: u64) -> u64 {
    prompt
        .deadline_ms
        .saturating_sub(now_ms)
        .saturating_add(RECONCILE_GRACE_MS)
        .min(crate::fux::MAX_INPUT_RETENTION_MS)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Reserve,
    Submit,
    Reconcile,
}

// Line submission is deliberately explicit about its framing. Multiline prompts
// need an adapter-selected paste mechanism, not literal newlines that execute early.
pub(super) fn keys(text: &str) -> Result<String> {
    anyhow::ensure!(
        !text.is_empty() && !text.chars().any(char::is_control),
        "line submission requires nonempty text without control characters"
    );
    let encoded = text.replace('\\', "\\\\") + "\\r";
    anyhow::ensure!(
        encoded.len() <= 65536,
        "encoded prompt exceeds fux input limit"
    );
    Ok(encoded)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireReceipt {
    operation: u64,
    pane: u32,
    state: String,
    revision: u64,
    input_sequence: u64,
    expires_ms: u64,
    bytes_written: usize,
    error: Option<String>,
}

pub(super) fn receipt(
    response: Value,
    target: &Target,
    previous: Option<&Receipt>,
    bytes: usize,
) -> Result<(Delivery, Receipt)> {
    let wire: WireReceipt = serde_json::from_value(
        response
            .pointer("/result/value/receipt")
            .cloned()
            .context("missing input receipt")?,
    )?;
    anyhow::ensure!(
        wire.operation != 0
            && wire.pane == target.pane
            && previous
                .is_none_or(|old| old.operation == wire.operation
                    && old.expires_server_ms == wire.expires_ms)
            && wire.bytes_written <= bytes,
        "invalid input receipt identity/length"
    );
    let phase = match wire.state.as_str() {
        "reserved" => Delivery::Reserved,
        "queued" => Delivery::Queued,
        "delivered" => Delivery::Delivered,
        "failed" => Delivery::Failed,
        _ => anyhow::bail!("unknown input receipt state"),
    };
    anyhow::ensure!(
        (phase != Delivery::Reserved || wire.bytes_written == 0)
            && (phase != Delivery::Delivered || wire.bytes_written == bytes)
            && wire.error.as_ref().is_none_or(|error| error.len() <= 4096),
        "invalid input receipt evidence"
    );
    Ok((
        phase,
        Receipt {
            operation: wire.operation,
            revision: wire.revision,
            input_sequence: wire.input_sequence,
            expires_server_ms: wire.expires_ms,
            bytes_written: wire.bytes_written,
        },
    ))
}

pub(super) fn request(
    target: &Target,
    command: &str,
    fields: Value,
    deadline: Instant,
) -> Result<Value> {
    let mut value = json!({"command":command,"id":1,"instance":target.instance});
    value
        .as_object_mut()
        .context("request object")?
        .extend(fields.as_object().context("fields object")?.clone());
    let response = crate::fux::completed_until(
        &target.runtime.join(format!("{}.sock", target.workspace)),
        value,
        deadline,
    )?;
    anyhow::ensure!(
        response.get("id").and_then(Value::as_u64) == Some(1),
        "fux response ID mismatch"
    );
    Ok(response)
}

pub(super) fn verify_target(target: &Target, deadline: Instant) -> Result<()> {
    target_pane(target, deadline).map(|_| ())
}

pub(super) fn target_pane(target: &Target, deadline: Instant) -> Result<Value> {
    let response = request(target, "list", json!({}), deadline)?;
    validate_target_reply(target, &response)
}

// Pure identity validation; the effectful caller owns socket negotiation and its deadline.
fn validate_target_reply(target: &Target, response: &Value) -> Result<Value> {
    let listing = response
        .pointer("/result/value")
        .context("missing listing")?;
    anyhow::ensure!(
        listing.get("instance").and_then(Value::as_str) == Some(&target.instance),
        "server changed"
    );
    let workspace = listing
        .get("workspaces")
        .and_then(Value::as_array)
        .and_then(|spaces| {
            spaces
                .iter()
                .find(|space| space.get("name").and_then(Value::as_str) == Some(&target.workspace))
        })
        .context("workspace lost")?;
    anyhow::ensure!(
        workspace
            .pointer("/event_cursor/stream")
            .and_then(Value::as_u64)
            == Some(target.stream),
        "workspace replaced"
    );
    let pane = workspace
        .get("tabs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|tab| tab.get("panes").and_then(Value::as_array))
        .flatten()
        .find(|pane| pane.get("id").and_then(Value::as_u64) == Some(u64::from(target.pane)))
        .context("pane lost")?;
    anyhow::ensure!(
        target.pid.is_some()
            && pane.get("pid").and_then(Value::as_u64) == target.pid.map(u64::from),
        "pane process replaced"
    );
    Ok(pane.clone())
}

fn persist(store: &mut Store, id: &str, phase: Delivery, receipt: Receipt) -> Result<()> {
    store.transaction(|journal| {
        let prompt = journal.prompts.get_mut(id).context("prompt missing")?;
        prompt.wait = if super::wait::terminal(&prompt.wait) {
            prompt.wait.clone()
        } else if prompt.released {
            WaitOutcome::Cancelled
        } else if phase == Delivery::Failed {
            WaitOutcome::Uncertain
        } else {
            WaitOutcome::Pending
        };
        if phase == Delivery::Submitting
            && let Some(arm) = &mut prompt.arm
        {
            arm.input_started = true;
        }
        prompt.delivery = phase;
        prompt.receipt = Some(receipt);
        Ok(())
    })
}

/// One bounded controller transaction. The journal lock serializes zor callers.
/// A failed reservation cannot have typed bytes. Once a reservation is durable,
/// every retry uses that operation, including after an ambiguous submission.
pub fn run(root: &Path, id: &str, action: Action) -> Result<Value> {
    run_until(
        root,
        id,
        action,
        Instant::now() + Duration::from_secs(6),
        None,
    )
}

pub(super) fn run_until(
    root: &Path,
    id: &str,
    action: Action,
    deadline: Instant,
    caller_deadline: Option<Instant>,
) -> Result<Value> {
    let mut store = Store::open(root)?;
    let prompt = store
        .journal()
        .prompts
        .get(id)
        .context("prompt not found")?
        .clone();
    if action != Action::Reconcile {
        anyhow::ensure!(
            !super::integration::retired(store.journal(), &prompt),
            "prompt adapter producer is retired; input will not be replayed"
        );
        super::group::authorize(store.journal(), &prompt)?;
    }
    anyhow::ensure!(
        action == Action::Reconcile || (!prompt.released && prompt.wait != WaitOutcome::Cancelled),
        "prompt coordination was cancelled; submission is disabled"
    );
    if prompt.arm.as_ref().is_some_and(|arm| arm.disarm_requested) {
        // Retirement retains the original unsent proof, not a live receipt view.
        // Released coordination already rejects reserve/submit above.
        return Ok(serde_json::to_value(prompt)?);
    }
    if matches!(prompt.delivery, Delivery::Delivered | Delivery::Failed) {
        return Ok(serde_json::to_value(prompt)?);
    }
    if prompt.delivery == Delivery::Prepared && action == Action::Reconcile {
        return Ok(serde_json::to_value(prompt)?);
    }
    let encoded = keys(&prompt.text)?;
    let bytes = prompt.text.len() + 1;
    let attempt = store
        .journal()
        .attempts
        .get(&prompt.attempt)
        .context("attempt missing")?;
    let target = store
        .journal()
        .sessions
        .get(&attempt.session)
        .context("session missing")?
        .target
        .clone();
    let result = (|| -> Result<()> {
        if prompt.receipt.is_none() {
            anyhow::ensure!(
                prompt.delivery == Delivery::Prepared,
                "uncertain operation has no receipt; cannot reserve again"
            );
            check_deadline(&prompt)?;
            verify_target(&target, deadline)?;
            let retain_ms = input_retain_ms(&prompt, super::now_ms()?);
            let response = request(
                &target,
                "input-reserve",
                json!({"pane":target.pane,"retain_ms":retain_ms}),
                deadline,
            )?;
            let (phase, receipt) = receipt(response, &target, None, bytes)?;
            anyhow::ensure!(phase == Delivery::Reserved, "reservation is not unsent");
            persist(&mut store, id, phase, receipt)?;
        } else {
            let old = prompt.receipt.as_ref().context("receipt missing")?;
            let response = request(
                &target,
                "input-status",
                json!({"operation":old.operation}),
                deadline,
            )?;
            let (phase, receipt) = receipt(response, &target, Some(old), bytes)?;
            persist(&mut store, id, phase, receipt)?;
        }
        let current = store
            .journal()
            .prompts
            .get(id)
            .context("prompt missing")?
            .clone();
        if action != Action::Submit || current.delivery != Delivery::Reserved {
            return Ok(());
        }
        check_deadline(&current)?;
        verify_target(&target, deadline)?;
        super::integration::arm(&mut store, id, deadline)?;
        let old = current.receipt.clone().context("receipt missing")?;
        persist(&mut store, id, Delivery::Submitting, old.clone())?;
        let submission_deadline = deadline.min(input_deadline(&current)?);
        let response = request(
            &target,
            "input-submit",
            json!({"operation":old.operation,"keys":encoded}),
            submission_deadline,
        )?;
        let (phase, receipt) = receipt(response, &target, Some(&old), bytes)?;
        anyhow::ensure!(
            phase != Delivery::Reserved,
            "submission returned an unsent reservation"
        );
        persist(&mut store, id, phase, receipt)
    })();
    if let Err(error) = result {
        if action == Action::Reconcile
            && caller_deadline.is_some_and(|limit| Instant::now() >= limit)
        {
            return Err(super::wait::CallerDeadline.into());
        }
        // Never reserve a replacement after any potentially submitted operation.
        // Even an uncertain fsync/rename is resolved by reopening the committed journal.
        if store.journal().prompts.get(id).is_some_and(|prompt| {
            prompt.receipt.is_some()
                && !matches!(prompt.delivery, Delivery::Delivered | Delivery::Failed)
        }) {
            store.transaction(|journal| {
                let prompt = journal.prompts.get_mut(id).context("prompt missing")?;
                prompt.delivery = Delivery::Uncertain;
                if !prompt.released && !super::wait::terminal(&prompt.wait) {
                    prompt.wait = WaitOutcome::Uncertain;
                }
                Ok(())
            })?;
        }
        return Err(error.context("prompt operation incomplete; inspect/reconcile before retrying"));
    }
    Ok(serde_json::to_value(store.journal().prompts.get(id))?)
}

fn input_deadline(prompt: &Prompt) -> Result<Instant> {
    let started = Instant::now();
    let now = super::now_ms()?;
    anyhow::ensure!(
        now >= prompt.created_ms && now < prompt.deadline_ms,
        "prompt deadline expired or clock moved backwards; no new input submitted"
    );
    Ok(started + Duration::from_millis(prompt.deadline_ms - now))
}

fn check_deadline(prompt: &Prompt) -> Result<()> {
    let now = super::now_ms()?;
    anyhow::ensure!(
        now >= prompt.created_ms && now < prompt.deadline_ms,
        "prompt deadline expired or clock moved backwards; no new input submitted"
    );
    Ok(())
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    #[test]
    fn captured_reply_cannot_transfer_authority_to_replaced_targets() -> Result<()> {
        let target = Target {
            runtime: "/tmp/fixture".into(),
            instance: "owner".into(),
            workspace: "default".into(),
            stream: 3,
            pane: 4,
            pid: Some(5),
        };
        let reply = json!({"result":{"value":{"instance":"owner","workspaces":[{
            "name":"default","event_cursor":{"stream":3},"tabs":[{"panes":[{"id":4,"pid":5}]}]
        }]}}});
        assert!(validate_target_reply(&target, &reply).is_ok());
        for (pointer, replacement) in [
            ("/result/value/instance", json!("replacement")),
            ("/result/value/workspaces/0/name", json!("foreign")),
            ("/result/value/workspaces/0/event_cursor/stream", json!(9)),
            ("/result/value/workspaces/0/tabs/0/panes/0/id", json!(9)),
            ("/result/value/workspaces/0/tabs/0/panes/0/pid", json!(9)),
            ("/result/value/workspaces/0/tabs/0/panes/0/pid", Value::Null),
        ] {
            let mut changed = reply.clone();
            *changed.pointer_mut(pointer).context("fixture pointer")? = replacement;
            assert!(
                validate_target_reply(&target, &changed).is_err(),
                "{pointer}"
            );
        }
        Ok(())
    }

    #[test]
    fn partial_failure_keeps_writer_exclusion() {
        let prompt = Prompt {
            handoff: None,
            id: "partial".into(),
            attempt: "attempt".into(),
            text: "hello".into(),
            created_ms: 1,
            deadline_ms: 2,
            delivery: Delivery::Failed,
            receipt: Some(Receipt {
                operation: 1,
                revision: 1,
                input_sequence: 1,
                expires_server_ms: 60000,
                bytes_written: 2,
            }),
            wait: WaitOutcome::Uncertain,
            released: false,
            report_token: None,
            response: None,
            report_binding: None,
            arm: None,
            wait_problem: None,
            wait_exit_status: None,
        };
        assert!(super::super::active(&prompt));
    }

    #[test]
    fn receipt_retention_covers_the_prompt_window_and_stays_under_fux_ceiling() {
        let mut prompt = Prompt {
            handoff: None,
            id: "window".into(),
            attempt: "attempt".into(),
            text: "hello".into(),
            created_ms: 1_000,
            deadline_ms: 31_000,
            delivery: Delivery::Prepared,
            receipt: None,
            wait: WaitOutcome::Pending,
            released: false,
            report_token: None,
            response: None,
            report_binding: None,
            arm: None,
            wait_problem: None,
            wait_exit_status: None,
        };
        // The remaining window plus one reconcile round.
        assert_eq!(input_retain_ms(&prompt, 6_000), 25_000 + RECONCILE_GRACE_MS);
        const {
            assert!(RECONCILE_GRACE_MS <= crate::fux::MAX_INPUT_RETENTION_MS);
        }
        // A day-long prompt (the longest zor accepts) is clamped to what fux will grant.
        prompt.deadline_ms = prompt.created_ms + 86_400_000;
        assert_eq!(
            input_retain_ms(&prompt, prompt.created_ms),
            crate::fux::MAX_INPUT_RETENTION_MS
        );
        // Never zero, which fux refuses, even after the window closed.
        assert_eq!(
            input_retain_ms(&prompt, prompt.deadline_ms + 5),
            RECONCILE_GRACE_MS
        );
    }

    #[test]
    fn line_encoding_does_not_interpret_user_escapes_or_allow_extra_commands() {
        assert_eq!(keys(r"literal\n").expect("encode"), "literal\\\\n\\r");
        for text in ["", "first\nsecond", "tab\t", "\x1b[201~"] {
            assert!(keys(text).is_err());
        }
        assert!(keys(&"\\".repeat(32768)).is_err());
        assert!(keys(&"x".repeat(65534)).is_ok());
    }
}
