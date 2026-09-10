//! Prompt-scoped explicit reports. Reports are attributed claims, never verification results.
use super::{model::*, store::Store, submit};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::{
    path::Path,
    time::{Duration, Instant},
};

/// One immutable terminal-response report per prompt, with exact retry deduplication.
pub struct Report {
    pub producer: String,
    pub sequence: u64,
    pub input_operation: u64,
    pub kind: ResponseKind,
    pub message: Option<AgentMessage>,
}

pub fn report(root: &Path, id: &str, token: &str, request: Report) -> Result<Value> {
    let Report {
        producer,
        sequence,
        input_operation,
        kind,
        message,
    } = request;
    anyhow::ensure!(
        super::model::id(&producer) && sequence != 0,
        "invalid report producer/sequence"
    );
    let mut store = Store::open(root)?;
    let prompt = store
        .journal()
        .prompts
        .get(id)
        .context("prompt not found")?;
    anyhow::ensure!(
        prompt.report_token.as_deref() == Some(token),
        "prompt report token mismatch"
    );
    super::binding::matches_report(
        prompt.report_binding.as_ref(),
        &producer,
        sequence,
        input_operation,
        message.as_ref(),
    )?;
    if let Some(existing) = &prompt.response {
        anyhow::ensure!(
            existing.producer == producer
                && existing.sequence == sequence
                && existing.input_operation == input_operation
                && existing.kind == kind
                && existing.message == message,
            "prompt already has a different response report"
        );
        return Ok(serde_json::to_value(existing)?);
    }
    anyhow::ensure!(
        !super::integration::retired(store.journal(), prompt),
        "prompt adapter producer is retired"
    );
    if prompt.report_binding.is_some() {
        super::binding::newer_than_retained(store.journal(), &producer, sequence)?;
    }
    anyhow::ensure!(
        !prompt.released
            && matches!(prompt.delivery, Delivery::Queued | Delivery::Delivered)
            && prompt
                .receipt
                .as_ref()
                .is_some_and(|receipt| receipt.operation == input_operation),
        "report requires this prompt's accepted input operation"
    );
    let now = super::now_ms()?;
    anyhow::ensure!(
        now >= prompt.created_ms && now < prompt.deadline_ms,
        "response arrived outside the prompt deadline"
    );
    let response = ResponseReport {
        producer,
        sequence,
        input_operation,
        kind,
        received_ms: now,
        message,
    };
    store.transaction(|journal| {
        journal
            .prompts
            .get_mut(id)
            .context("prompt missing")?
            .response = Some(response.clone());
        Ok(())
    })?;
    Ok(serde_json::to_value(response)?)
}

pub(super) fn terminal(wait: &WaitOutcome) -> bool {
    matches!(
        wait,
        WaitOutcome::ResponseObserved
            | WaitOutcome::NeedsInput
            | WaitOutcome::ProcessExited
            | WaitOutcome::TimedOut
            | WaitOutcome::Cancelled
    )
}

/// A bounded, single evaluation. Pending callers may retry; this never holds a sleeping waiter.
/// Screen states do not satisfy this contract. A report must name the actual prompt operation.
pub fn check(root: &Path, id: &str) -> Result<Value> {
    check_until(root, id, Instant::now() + Duration::from_secs(10), None)
}

#[derive(Debug)]
pub(super) struct CallerDeadline;
impl std::fmt::Display for CallerDeadline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("caller wait deadline reached")
    }
}
impl std::error::Error for CallerDeadline {}

#[derive(Clone, PartialEq, Eq)]
struct Identity {
    attempt: String,
    created_ms: u64,
    report_token: Option<String>,
}
fn identity(prompt: &Prompt) -> Identity {
    Identity {
        attempt: prompt.attempt.clone(),
        created_ms: prompt.created_ms,
        report_token: prompt.report_token.clone(),
    }
}

fn check_until(
    root: &Path,
    id: &str,
    limit: Instant,
    expected: Option<&Identity>,
) -> Result<Value> {
    let initial = {
        let store = Store::open(root)?;
        let prompt = store
            .journal()
            .prompts
            .get(id)
            .context("prompt not found")?;
        anyhow::ensure!(
            expected.is_none_or(|key| *key == identity(prompt)),
            "prompt identity changed while waiting"
        );
        if terminal(&prompt.wait) {
            return Ok(serde_json::to_value(prompt)?);
        }
        if prompt.response.is_none() && super::integration::retired(store.journal(), prompt) {
            return Ok(serde_json::to_value(prompt)?);
        }
        anyhow::ensure!(
            !matches!(prompt.delivery, Delivery::Prepared | Delivery::Reserved),
            "prompt has not been submitted"
        );
        identity(prompt)
    };
    // Reconciliation never types. Its failure does not turn missing evidence into completion.
    let reconciliation = match submit::run_until(
        root,
        id,
        submit::Action::Reconcile,
        limit.min(Instant::now() + Duration::from_secs(6)),
        expected.map(|_| limit),
    ) {
        Err(error) if error.is::<super::store::Busy>() || error.is::<CallerDeadline>() => {
            return Err(error);
        }
        result => result,
    };
    let mut store = Store::open(root)?;
    let prompt = store
        .journal()
        .prompts
        .get(id)
        .context("prompt missing")?
        .clone();
    anyhow::ensure!(
        identity(&prompt) == initial,
        "prompt identity changed while checking"
    );
    if terminal(&prompt.wait) {
        return Ok(serde_json::to_value(prompt)?);
    }
    if prompt.response.is_none() && super::integration::retired(store.journal(), &prompt) {
        return Ok(serde_json::to_value(prompt)?);
    }
    anyhow::ensure!(
        !matches!(prompt.delivery, Delivery::Prepared | Delivery::Reserved),
        "reconciliation proves this prompt has not been submitted"
    );
    let attempt = store
        .journal()
        .attempts
        .get(&prompt.attempt)
        .context("attempt missing")?;
    let target = &store
        .journal()
        .sessions
        .get(&attempt.session)
        .context("session missing")?
        .target;
    let outcome = evaluate(&prompt, target, reconciliation, limit);
    if expected.is_some() && Instant::now() >= limit {
        return Err(CallerDeadline.into());
    }
    let (wait, problem, exit_status) = match outcome {
        Ok((wait, status)) => (wait, None, status),
        Err(error) => (
            WaitOutcome::Uncertain,
            Some(error.to_string().chars().take(128).collect::<String>()),
            None,
        ),
    };
    if prompt.wait != wait
        || prompt.wait_problem != problem
        || prompt.wait_exit_status != exit_status
    {
        store.transaction(|journal| {
            let prompt = journal.prompts.get_mut(id).context("prompt missing")?;
            prompt.wait = wait;
            prompt.wait_problem = problem;
            prompt.wait_exit_status = exit_status;
            Ok(())
        })?;
    }
    Ok(serde_json::to_value(store.journal().prompts.get(id))?)
}

fn evaluate(
    prompt: &Prompt,
    target: &Target,
    reconciliation: Result<Value>,
    limit: Instant,
) -> Result<(WaitOutcome, Option<u32>)> {
    anyhow::ensure!(
        prompt.delivery != Delivery::Failed,
        "PTY delivery failed or was partial"
    );
    let receipt = prompt
        .receipt
        .as_ref()
        .context("delivery receipt unavailable")?;
    let deadline = limit.min(Instant::now() + Duration::from_secs(4));
    let live = (|| -> Result<Value> {
        submit::verify_target(target, deadline)?;
        submit::request(
            target,
            "capture",
            json!({"pane":target.pane,"max_bytes":1}),
            deadline,
        )
    })();
    match live {
        Ok(capture) => {
            let sequence = capture
                .pointer("/result/value/input_sequence")
                .and_then(Value::as_u64)
                .context("capture input sequence missing")?;
            anyhow::ensure!(
                sequence == receipt.input_sequence,
                "intervening input weakened prompt correlation"
            );
        }
        Err(live_error) => {
            let response = crate::fux::request_until(
                &target.runtime.join("manager.sock"),
                json!({"request":"final","instance":target.instance,"pane":target.pane}),
                deadline,
            )
            .with_context(|| format!("live/final evidence unavailable: {live_error}"))?;
            anyhow::ensure!(
                response.get("reply").and_then(Value::as_str) == Some("final")
                    && response.pointer("/result/status").and_then(Value::as_str)
                        == Some("completed"),
                "final evidence unavailable or expired"
            );
            let record = response
                .pointer("/result/result/value/record")
                .context("final evidence unavailable or expired")?;
            anyhow::ensure!(
                record.get("pane").and_then(Value::as_u64) == Some(u64::from(target.pane))
                    && record.get("workspace").and_then(Value::as_str) == Some(&target.workspace)
                    && record.get("stream").and_then(Value::as_u64) == Some(target.stream)
                    && record.get("input_sequence").and_then(Value::as_u64)
                        == Some(receipt.input_sequence),
                "final evidence identity/input sequence mismatch"
            );
            let status = record
                .get("exit_status")
                .and_then(Value::as_u64)
                .and_then(|status| u32::try_from(status).ok())
                .context("pane closed without valid observed process exit")?;
            return Ok((WaitOutcome::ProcessExited, Some(status)));
        }
    }
    reconciliation?;
    let now = super::now_ms()?;
    anyhow::ensure!(now >= prompt.created_ms, "clock moved backwards");
    if prompt.delivery == Delivery::Delivered
        && let Some(report) = &prompt.response
    {
        return Ok((
            match report.kind {
                ResponseKind::NeedsInput => WaitOutcome::NeedsInput,
                ResponseKind::ResponseObserved => WaitOutcome::ResponseObserved,
            },
            None,
        ));
    }
    if now >= prompt.deadline_ms {
        return Ok((WaitOutcome::TimedOut, None));
    }
    Ok((WaitOutcome::Pending, None))
}

/// Bounded local blocking wait. Poll sleeps never retain the journal lock.
pub fn follow(root: &Path, id: &str, timeout_ms: u64) -> Result<Value> {
    anyhow::ensure!(
        (1..=86_400_000).contains(&timeout_ms),
        "waiter timeout must be 1 ms through 24 hours"
    );
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let (guard, expected, mut last) = {
        let store = Store::open(root)?;
        let prompt = store
            .journal()
            .prompts
            .get(id)
            .context("prompt not found")?;
        (
            store.waiter()?,
            identity(prompt),
            serde_json::to_value(prompt)?,
        )
    };
    let _guard = guard;
    loop {
        if Instant::now() >= deadline {
            return Ok(json!({"stop":"caller-deadline","prompt":last}));
        }
        match check_until(root, id, deadline, Some(&expected)) {
            Ok(prompt) => {
                let wait = prompt
                    .get("wait")
                    .and_then(Value::as_str)
                    .context("wait outcome missing")?;
                let stop = match wait {
                    "pending" => None,
                    "uncertain" => Some("uncertain"),
                    _ => Some("terminal"),
                };
                last = prompt;
                if let Some(stop) = stop {
                    return Ok(json!({"stop":stop,"prompt":last}));
                }
            }
            Err(error) if error.is::<CallerDeadline>() => {
                return Ok(json!({"stop":"caller-deadline","prompt":last}));
            }
            Err(error) if error.is::<super::store::Busy>() => {}
            Err(error) => return Err(error),
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        std::thread::sleep(remaining.min(Duration::from_millis(250)));
    }
}
