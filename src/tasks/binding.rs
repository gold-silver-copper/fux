//! Immutable native-message ancestry for an explicitly named managed prompt.
//! Never discovers a "current prompt" from text or turns application claims into verification.
use super::{model::*, store::Store, submit};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::Path,
    time::{Duration, Instant},
};

pub struct Bind {
    pub producer: String,
    pub sequence: u64,
    pub input_operation: u64,
    pub message: AgentMessage,
}

fn valid_message(message: &AgentMessage) -> bool {
    super::model::id(&message.session) && super::model::id(&message.id)
}

pub fn run(root: &Path, operation: &str, token: &str, request: Bind) -> Result<Value> {
    anyhow::ensure!(
        super::model::id(&request.producer)
            && valid_message(&request.message)
            && request.sequence > 0
            && request.input_operation > 0,
        "invalid binding identity/sequence"
    );
    let mut store = Store::open(root)?;
    let prompt = store
        .journal()
        .prompts
        .get(operation)
        .context("prompt not found")?
        .clone();
    anyhow::ensure!(
        prompt.report_token.as_deref() == Some(token),
        "prompt report token mismatch"
    );
    if let Some(existing) = &prompt.report_binding {
        anyhow::ensure!(
            existing.producer == request.producer
                && existing.sequence == request.sequence
                && existing.input_operation == request.input_operation
                && existing.message == request.message,
            "prompt already has a different message binding"
        );
        return Ok(serde_json::to_value(existing)?);
    }
    anyhow::ensure!(
        !super::integration::retired(store.journal(), &prompt),
        "prompt adapter producer is retired"
    );
    anyhow::ensure!(
        !prompt.released
            && !super::wait::terminal(&prompt.wait)
            && prompt.response.is_none()
            && matches!(
                prompt.delivery,
                Delivery::Submitting | Delivery::Queued | Delivery::Delivered | Delivery::Uncertain
            ),
        "binding requires unresolved possibly submitted prompt input"
    );
    let attempt = store
        .journal()
        .attempts
        .get(&prompt.attempt)
        .context("attempt missing")?;
    let task = store
        .journal()
        .tasks
        .get(&attempt.task)
        .context("task missing")?;
    let session = store
        .journal()
        .sessions
        .get(&attempt.session)
        .context("session missing")?;
    let launch = session
        .launch
        .as_ref()
        .and_then(|id| store.journal().launches.get(id))
        .context("binding requires a managed launch")?;
    anyhow::ensure!(
        session.ownership == Ownership::Managed
            && launch.phase == LaunchPhase::Attached
            && !launch.stop_requested
            && task.outcome == TaskOutcome::Open,
        "binding requires an open task and live managed launch"
    );
    let target = session.target.clone();
    let old = prompt.receipt.as_ref().context("input receipt missing")?;
    anyhow::ensure!(
        old.operation == request.input_operation,
        "binding input operation mismatch"
    );
    newer_than_retained(store.journal(), &request.producer, request.sequence)?;
    // Status only: recover acceptance after a lost submit reply without ever typing.
    // Keep the journal lock through identity, receipt, and binding publication.
    let deadline = Instant::now() + Duration::from_secs(4);
    let response = submit::request(
        &target,
        "input-status",
        json!({"operation":old.operation}),
        deadline,
    )?;
    let (delivery, receipt) = submit::receipt(response, &target, Some(old), prompt.text.len() + 1)?;
    anyhow::ensure!(
        matches!(delivery, Delivery::Queued | Delivery::Delivered),
        "binding requires an accepted input receipt"
    );
    let pane = submit::target_pane(&target, deadline)?;
    anyhow::ensure!(
        pane.get("input_sequence").and_then(Value::as_u64) == Some(receipt.input_sequence),
        "intervening input weakened message binding"
    );
    let bound_ms = super::now_ms()?;
    anyhow::ensure!(
        bound_ms >= prompt.created_ms && bound_ms < prompt.deadline_ms,
        "binding arrived outside the prompt deadline"
    );
    let binding = ReportBinding {
        producer: request.producer,
        sequence: request.sequence,
        input_operation: request.input_operation,
        message: request.message,
        bound_ms,
    };
    store.transaction(|journal| {
        let prompt = journal
            .prompts
            .get_mut(operation)
            .context("prompt missing")?;
        prompt.delivery = delivery;
        prompt.receipt = Some(receipt);
        prompt.report_binding = Some(binding.clone());
        Ok(())
    })?;
    Ok(serde_json::to_value(binding)?)
}

/// Both consumption and response events share a producer's sequence domain. Retained
/// exact retries bypass this check; new events must advance it. Forgetting history is
/// an explicit retention boundary, not durable replay protection for arbitrary lifetimes.
pub(super) fn newer_than_retained(journal: &Journal, producer: &str, sequence: u64) -> Result<()> {
    for prompt in journal.prompts.values() {
        if let Some(binding) = &prompt.report_binding
            && binding.producer == producer
        {
            anyhow::ensure!(sequence > binding.sequence, "producer sequence is stale");
        }
        if let Some(response) = &prompt.response
            && response.producer == producer
        {
            anyhow::ensure!(sequence > response.sequence, "producer sequence is stale");
        }
    }
    Ok(())
}

pub(super) fn matches_report(
    binding: Option<&ReportBinding>,
    producer: &str,
    sequence: u64,
    input_operation: u64,
    message: Option<&AgentMessage>,
) -> Result<()> {
    match (binding, message) {
        (None, None) => Ok(()),
        (Some(binding), Some(message)) => {
            anyhow::ensure!(
                binding.producer == producer
                    && binding.input_operation == input_operation
                    && binding.message == *message
                    && sequence > binding.sequence,
                "response does not match this prompt's native message binding"
            );
            Ok(())
        }
        _ => anyhow::bail!("bound reports require the recorded native message identity"),
    }
}

/// Revalidate retained relationships on every journal load and before every commit.
pub(super) fn validate(journal: &Journal) -> Result<()> {
    let mut messages = std::collections::BTreeSet::new();
    let mut producers = BTreeMap::new();
    let mut sequences = std::collections::BTreeSet::new();
    for prompt in journal.prompts.values() {
        if let Some(binding) = &prompt.report_binding {
            let session = journal
                .attempts
                .get(&prompt.attempt)
                .and_then(|a| journal.sessions.get(&a.session))
                .context("binding session missing")?;
            anyhow::ensure!(
                session.ownership == Ownership::Managed
                    && super::model::id(&binding.producer)
                    && valid_message(&binding.message)
                    && binding.sequence > 0
                    && prompt.report_token.is_some()
                    && matches!(
                        prompt.delivery,
                        Delivery::Queued
                            | Delivery::Delivered
                            | Delivery::Failed
                            | Delivery::Uncertain
                    )
                    && prompt
                        .receipt
                        .as_ref()
                        .is_some_and(|r| r.operation == binding.input_operation)
                    && binding.bound_ms >= prompt.created_ms
                    && binding.bound_ms < prompt.deadline_ms,
                "invalid retained message binding"
            );
            anyhow::ensure!(
                messages.insert((&session.id, &binding.message.session, &binding.message.id)),
                "native message already bound to another prompt"
            );
            let scope = (&session.id, &binding.message.session);
            if let Some(old) = producers.insert(&binding.producer, scope) {
                anyhow::ensure!(
                    old == scope,
                    "producer lifetime changed managed/native session"
                );
            }
            anyhow::ensure!(
                sequences.insert((&binding.producer, binding.sequence)),
                "producer event sequence reused"
            );
        }
        if let Some(response) = &prompt.response {
            matches_report(
                prompt.report_binding.as_ref(),
                &response.producer,
                response.sequence,
                response.input_operation,
                response.message.as_ref(),
            )?;
            if let Some(binding) = &prompt.report_binding {
                anyhow::ensure!(
                    response.received_ms >= binding.bound_ms,
                    "response predates message binding"
                );
            }
        }
    }
    for prompt in journal.prompts.values() {
        if let Some(response) = &prompt.response
            && producers.contains_key(&response.producer)
        {
            anyhow::ensure!(
                response.message.is_some(),
                "bound producer cannot supply an unbound response"
            );
            anyhow::ensure!(
                sequences.insert((&response.producer, response.sequence)),
                "producer event sequence reused"
            );
        }
    }
    Ok(())
}
