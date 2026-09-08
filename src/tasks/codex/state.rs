//! Native submission intent and evidence in zor's existing atomic journal.
use super::protocol::{Phase, Turn};
use crate::tasks::{
    model::{self, Journal, LaunchPhase, Ownership, TaskOutcome},
    store::Store,
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ControlKind {
    Read,
    Interrupt,
    Recreate,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ControlPhase {
    Queued,
    Sent,
    Observed,
    Expired,
    Obsolete,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Control {
    pub kind: ControlKind,
    pub producer: String,
    pub created_ms: u64,
    pub timeout_ms: u64,
    pub phase: ControlPhase,
    #[serde(default)]
    pub replacement: Option<String>,
}
impl Control {
    fn pending(&self) -> bool {
        matches!(self.phase, ControlPhase::Queued | ControlPhase::Sent)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Worker {
    pub task: String,
    pub attempt: String,
    pub marker: String,
    pub producer: String,
    pub thread: String,
    #[serde(default)]
    pub storage: Option<super::storage::Storage>,
    pub ready: bool,
    pub retired: Vec<String>,
}
impl Worker {
    /// Retired producer identities consume their admitted lifetime allowance.
    /// Keep it reserved while the provider is stopped for recreation.
    pub fn remaining_reservation(&self) -> usize {
        let retained = serde_json::to_vec(&self.retired).map_or(2048, |bytes| bytes.len());
        2048usize.saturating_sub(retained + self.producer.len() + usize::from(!self.ready))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub task: String,
    pub attempt: String,
    pub marker: String,
    pub producer: String,
    pub observer: Option<String>,
    #[serde(default)]
    pub observed_ms: Option<u64>,
    pub created_ms: u64,
    pub timeout_ms: u64,
    pub evidence: Turn,
    #[serde(default, deserialize_with = "model::unique_records")]
    pub controls: BTreeMap<String, Control>,
}
impl Entry {
    pub fn remaining_reservation(&self) -> usize {
        if self.evidence.phase.terminal() && !self.controls.values().any(Control::pending) {
            return 0;
        }
        let sizes = (|| -> Result<(usize, usize)> {
            let mut initial = self.clone();
            initial.observer = None;
            initial.observed_ms = None;
            initial.controls.clear();
            initial.evidence = Turn::prepare(
                &self.evidence.operation,
                &self.evidence.thread,
                &self.evidence.text,
                self.evidence.deadline_ms,
            )?;
            Ok((
                serde_json::to_vec(&initial)?.len(),
                serde_json::to_vec(self)?.len(),
            ))
        })();
        match sizes {
            Ok((initial, retained)) => 262144usize.saturating_sub(retained.saturating_sub(initial)),
            Err(_) => crate::tasks::store::MAX_BYTES,
        }
    }
}

fn worker<'a>(
    journal: &'a Journal,
    task: &str,
    producer: Option<&str>,
    input: bool,
) -> Result<&'a Worker> {
    let worker = journal
        .native_workers
        .get(task)
        .context("native worker is not registered")?;
    let current = journal.tasks.get(task).context("native task missing")?;
    let launch = journal
        .launches
        .get(task)
        .context("native managed launch missing")?;
    ensure!(
        worker.ready
            && worker.attempt == current.attempt
            && worker.marker == launch.marker
            && producer.is_none_or(|producer| producer == worker.producer)
            && launch.phase == LaunchPhase::Attached
            && !launch.stop_requested,
        "native worker lifetime is unavailable or changed"
    );
    if input {
        ensure!(
            !journal
                .native_turns
                .values()
                .filter(|entry| entry.task == task)
                .flat_map(|entry| entry.controls.values())
                .any(|control| control.kind == ControlKind::Recreate && control.pending()),
            "native recreation is pending; input is disabled"
        );
        ensure!(
            current.outcome == TaskOutcome::Open,
            "task coordination is closed; native input is disabled"
        );
    }
    Ok(worker)
}

/// The managed worker invokes registration only after verifying its live fux
/// identity and app-server handshake/thread. Registration cannot replace a live
/// producer; retirement is explicit and invalidates its observations first.
pub fn register(root: &Path, mut candidate: Worker) -> Result<()> {
    let mut store = Store::open(root)?;
    let launch = store
        .journal()
        .launches
        .get(&candidate.task)
        .context("managed native launch missing")?;
    let task = store
        .journal()
        .tasks
        .get(&candidate.task)
        .context("native task missing")?;
    ensure!(
        candidate.ready
            && launch.phase == LaunchPhase::Attached
            && !launch.stop_requested
            && launch.agent.as_deref() == Some("codex")
            && launch.marker == candidate.marker
            && task.attempt == candidate.attempt
            && (task.outcome == TaskOutcome::Open
                || store.journal().native_workers.contains_key(&candidate.task)),
        "native registration does not match an active managed Codex launch"
    );
    if let Some(old) = store.journal().native_workers.get(&candidate.task) {
        ensure!(
            old.thread == candidate.thread && old.storage == candidate.storage,
            "native thread or retained storage identity changed during producer replacement"
        );
        // Retirement history is maintained here, not part of caller launch intent.
        candidate.retired = old.retired.clone();
        if old == &candidate {
            return Ok(());
        }
        ensure!(
            !old.ready && old.producer != candidate.producer,
            "native producer replacement requires prior retirement and a new identity"
        );
        ensure!(
            old.retired.len() < 16 && !old.retired.contains(&candidate.producer),
            "native producer was retired or lifetime limit reached"
        );
        candidate.retired = old.retired.clone();
        candidate.retired.push(old.producer.clone());
        ensure!(
            !store
                .journal()
                .native_turns
                .values()
                .any(|entry| entry.producer == candidate.producer),
            "native producer identity was previously used"
        );
    } else {
        ensure!(
            candidate.retired.is_empty(),
            "initial native producer has fabricated retirement history"
        );
    }
    store.transaction(|journal| {
        journal
            .native_workers
            .insert(candidate.task.clone(), candidate);
        Ok(())
    })
}

pub fn prepare(
    root: &Path,
    task: &str,
    operation: &str,
    text: &str,
    timeout_ms: u64,
) -> Result<Value> {
    ensure!(
        (1..=300_000).contains(&timeout_ms),
        "native input timeout must be 1..300000 ms"
    );
    let mut store = Store::open(root)?;
    if let Some(entry) = store.journal().native_turns.get(operation) {
        ensure!(
            entry.task == task && entry.evidence.text == text && entry.timeout_ms == timeout_ms,
            "native operation ID already has different intent"
        );
        return Ok(serde_json::to_value(entry)?);
    }
    let owner = worker(store.journal(), task, None, true)?.clone();
    ensure!(
        !store
            .journal()
            .native_turns
            .values()
            .any(|entry| entry.task == task && !entry.evidence.phase.terminal()),
        "native worker already has an unresolved input operation"
    );
    let created_ms = super::super::now_ms()?;
    let entry = Entry {
        controls: BTreeMap::new(),
        task: task.into(),
        attempt: owner.attempt,
        marker: owner.marker,
        producer: owner.producer,
        observer: None,
        observed_ms: None,
        created_ms,
        timeout_ms,
        evidence: Turn::prepare(
            operation,
            &owner.thread,
            text,
            created_ms
                .checked_add(timeout_ms)
                .context("native deadline overflow")?,
        )?,
    };
    let result = serde_json::to_value(&entry)?;
    store.transaction(|journal| {
        journal.native_turns.insert(operation.into(), entry);
        Ok(())
    })?;
    Ok(result)
}

fn owner_of<'a>(
    journal: &'a Journal,
    operation: &str,
    producer: &str,
    input: bool,
) -> Result<&'a Entry> {
    let entry = journal
        .native_turns
        .get(operation)
        .context("native operation missing")?;
    let owner = worker(journal, &entry.task, Some(producer), input)?;
    ensure!(
        (!input || entry.producer == owner.producer)
            && entry.attempt == owner.attempt
            && entry.marker == owner.marker
            && entry.evidence.thread == owner.thread,
        "native operation belongs to a different worker lifetime; no replay authorized"
    );
    Ok(entry)
}

/// Only the owning worker uses this boundary. Successful return means submitted
/// intent was synced, not that the provider accepted input. None never grants replay.
pub fn take_submission(root: &Path, operation: &str, producer: &str) -> Result<Option<Value>> {
    let mut store = Store::open(root)?;
    let entry = owner_of(store.journal(), operation, producer, true)?;
    if entry.evidence.phase != Phase::Prepared {
        return Ok(None);
    }
    let now = super::super::now_ms()?;
    ensure!(
        now >= entry.created_ms,
        "native input clock moved backwards"
    );
    store.transaction(|journal| {
        let entry = journal
            .native_turns
            .get_mut(operation)
            .context("native operation missing")?;
        Ok(Some(entry.evidence.submission(now)?))
    })
}

pub fn take_interrupt(
    root: &Path,
    operation: &str,
    producer: &str,
    interrupt: &str,
) -> Result<Option<Value>> {
    let mut store = Store::open(root)?;
    let entry = owner_of(store.journal(), operation, producer, false)?;
    if let Some(previous) = &entry.evidence.interrupt_operation {
        ensure!(
            previous == interrupt,
            "native interrupt already has different intent"
        );
        return Ok(None);
    }
    store.transaction(|journal| {
        journal
            .native_turns
            .get_mut(operation)
            .context("native operation missing")?
            .evidence
            .interrupt(interrupt)
    })
}

/// Queue one explicit native control operation. Its original deadline and owner
/// survive caller loss. A repeated request ID never grants another send.
pub fn request_control(
    root: &Path,
    operation: &str,
    request: &str,
    kind: ControlKind,
    timeout_ms: u64,
) -> Result<Value> {
    ensure!(
        model::id(request) && (1..=300_000).contains(&timeout_ms),
        "invalid native control ID/timeout"
    );
    let mut store = Store::open(root)?;
    let entry = store
        .journal()
        .native_turns
        .get(operation)
        .context("native operation missing")?;
    if let Some(previous) = entry.controls.get(request) {
        ensure!(
            previous.kind == kind && previous.timeout_ms == timeout_ms,
            "native control intent changed"
        );
        return Ok(serde_json::to_value(previous)?);
    }
    ensure!(
        !store
            .journal()
            .native_turns
            .values()
            .any(|entry| entry.controls.contains_key(request)
                || entry.evidence.interrupt_operation.as_deref() == Some(request)),
        "native control ID already used"
    );
    let owner = worker(store.journal(), &entry.task, None, false)?;
    owner_of(store.journal(), operation, &owner.producer, false)?;
    ensure!(
        entry.evidence.phase != Phase::Prepared && entry.controls.len() < 16,
        "native input is not submitted or control limit reached"
    );
    if kind == ControlKind::Interrupt {
        ensure!(
            !entry
                .controls
                .values()
                .any(|control| control.kind == ControlKind::Interrupt),
            "native interrupt already requested"
        );
        // Validate current native identity without consuming send authority.
        entry.evidence.clone().interrupt(request)?;
    }
    if kind == ControlKind::Recreate {
        ensure!(
            owner.retired.len() < 16,
            "native producer lifetime limit reached"
        );
        owner
            .storage
            .as_ref()
            .context("native storage identity unavailable; recreation is unsupported")?
            .require_materialized()?;
        ensure!(
            !store
                .journal()
                .native_turns
                .values()
                .filter(|other| other.task == entry.task)
                .any(|other| other.evidence.phase == Phase::Prepared
                    || other
                        .controls
                        .values()
                        .any(|control| control.kind == ControlKind::Recreate && control.pending())),
            "native recreation or unsubmitted input is already pending"
        );
    }
    let replacement = if kind == ControlKind::Recreate {
        Some(super::super::store::nonce()?)
    } else {
        None
    };
    let control = Control {
        kind,
        producer: owner.producer.clone(),
        created_ms: crate::tasks::now_ms()?,
        timeout_ms,
        phase: ControlPhase::Queued,
        replacement,
    };
    let value = serde_json::to_value(&control)?;
    store.transaction(|journal| {
        journal
            .native_turns
            .get_mut(operation)
            .context("native operation missing")?
            .controls
            .insert(request.into(), control);
        Ok(())
    })?;
    Ok(value)
}

#[derive(Clone)]
pub struct Recreation {
    pub task: String,
    pub operation: String,
    pub request: String,
    pub producer: String,
    pub replacement: String,
    pub thread: String,
    pub storage: super::storage::Storage,
    pub deadline_ms: u64,
}

pub fn take_recreation(
    root: &Path,
    operation: &str,
    producer: &str,
    request: &str,
) -> Result<Option<Recreation>> {
    let mut store = Store::open(root)?;
    let entry = owner_of(store.journal(), operation, producer, false)?;
    let control = entry
        .controls
        .get(request)
        .context("native recreation missing")?;
    ensure!(
        control.kind == ControlKind::Recreate,
        "native control is not recreation"
    );
    if control.phase != ControlPhase::Queued {
        return Ok(None);
    }
    ensure!(
        control.producer == producer,
        "native recreation producer changed"
    );
    let now = crate::tasks::now_ms()?;
    ensure!(
        now >= control.created_ms,
        "native recreation clock moved backwards"
    );
    let deadline_ms = control
        .created_ms
        .checked_add(control.timeout_ms)
        .context("native recreation deadline overflow")?;
    if now >= deadline_ms {
        store.transaction(|journal| {
            journal
                .native_turns
                .get_mut(operation)
                .context("entry")?
                .controls
                .get_mut(request)
                .context("control")?
                .phase = ControlPhase::Expired;
            Ok(())
        })?;
        return Ok(None);
    }
    let storage = worker(store.journal(), &entry.task, Some(producer), false)?
        .storage
        .clone()
        .context("native storage identity missing")?;
    storage.require_materialized()?;
    let action = Recreation {
        task: entry.task.clone(),
        operation: operation.into(),
        request: request.into(),
        producer: producer.into(),
        replacement: control
            .replacement
            .clone()
            .context("replacement producer missing")?,
        thread: entry.evidence.thread.clone(),
        storage,
        deadline_ms,
    };
    store.transaction(|journal| {
        journal
            .native_turns
            .get_mut(operation)
            .context("entry")?
            .controls
            .get_mut(request)
            .context("control")?
            .phase = ControlPhase::Sent;
        Ok(())
    })?;
    Ok(Some(action))
}

/// The owner has already stopped the original child and verified the replacement
/// handshake, thread, storage namespace and live fux identity. Registration is
/// retryable; pending recreation continues to block input until final publication.
pub fn finish_recreation(root: &Path, action: &Recreation, candidate: Worker) -> Result<()> {
    ensure!(
        candidate.task == action.task
            && candidate.producer == action.replacement
            && candidate.thread == action.thread
            && candidate.storage.as_ref() == Some(&action.storage),
        "native recreation result identity mismatch"
    );
    {
        let store = Store::open(root)?;
        let control = store
            .journal()
            .native_turns
            .get(&action.operation)
            .context("entry")?
            .controls
            .get(&action.request)
            .context("control")?;
        ensure!(
            control.kind == ControlKind::Recreate
                && control.producer == action.producer
                && control.replacement.as_deref() == Some(action.replacement.as_str()),
            "native recreation intent changed"
        );
        if control.phase == ControlPhase::Observed {
            return Ok(());
        }
        ensure!(
            control.phase == ControlPhase::Sent && crate::tasks::now_ms()? < action.deadline_ms,
            "native recreation is not pending or deadline expired"
        );
    }
    register(root, candidate)?;
    let mut store = Store::open(root)?;
    worker(
        store.journal(),
        &action.task,
        Some(&action.replacement),
        false,
    )?;
    store.transaction(|journal| {
        let control = journal
            .native_turns
            .get_mut(&action.operation)
            .context("entry")?
            .controls
            .get_mut(&action.request)
            .context("control")?;
        ensure!(
            control.phase == ControlPhase::Sent,
            "native recreation no longer pending"
        );
        let now = crate::tasks::now_ms()?;
        let deadline = control
            .created_ms
            .checked_add(control.timeout_ms)
            .context("native recreation deadline overflow")?;
        ensure!(
            now >= control.created_ms && now < deadline && now < action.deadline_ms,
            "native recreation deadline expired before publication"
        );
        control.phase = ControlPhase::Observed;
        Ok(())
    })
}

pub fn fail_recreation(root: &Path, action: &Recreation) -> Result<()> {
    let mut store = Store::open(root)?;
    store.transaction(|journal| {
        let control = journal
            .native_turns
            .get_mut(&action.operation)
            .context("entry")?
            .controls
            .get_mut(&action.request)
            .context("control")?;
        ensure!(
            control.producer == action.producer
                && control.replacement.as_deref() == Some(action.replacement.as_str()),
            "native recreation identity changed"
        );
        if control.phase == ControlPhase::Sent {
            control.phase = ControlPhase::Obsolete;
        }
        if let Some(worker) = journal.native_workers.get_mut(&action.task)
            && worker.producer == action.replacement
        {
            worker.ready = false;
        }
        for entry in journal
            .native_turns
            .values_mut()
            .filter(|entry| entry.task == action.task)
        {
            for control in entry
                .controls
                .values_mut()
                .filter(|control| control.producer == action.replacement && control.pending())
            {
                control.phase = ControlPhase::Obsolete;
            }
        }
        Ok(())
    })
}

pub fn take_control(
    root: &Path,
    operation: &str,
    producer: &str,
    request: &str,
) -> Result<Option<Value>> {
    let mut store = Store::open(root)?;
    let entry = owner_of(store.journal(), operation, producer, false)?;
    let control = entry
        .controls
        .get(request)
        .context("native control missing")?;
    if !control.pending() {
        return Ok(None);
    }
    ensure!(
        control.producer == producer,
        "native control belongs to an old producer"
    );
    let now = crate::tasks::now_ms()?;
    ensure!(
        now >= control.created_ms,
        "native control clock moved backwards"
    );
    let expired = now
        >= control
            .created_ms
            .checked_add(control.timeout_ms)
            .context("native control deadline overflow")?;
    if !expired && control.phase != ControlPhase::Queued {
        return Ok(None);
    }
    store.transaction(|journal| {
        let entry = journal
            .native_turns
            .get_mut(operation)
            .context("native operation missing")?;
        let control = entry
            .controls
            .get_mut(request)
            .context("native control missing")?;
        if expired {
            control.phase = ControlPhase::Expired;
            return Ok(None);
        }
        let frame = match control.kind {
            ControlKind::Read => {
                let mut frame = entry.evidence.read_request();
                *frame.get_mut("id").context("native read ID")? =
                    json!(format!("read:{operation}:{request}"));
                Some(frame)
            }
            ControlKind::Interrupt if entry.evidence.phase.terminal() => {
                control.phase = ControlPhase::Obsolete;
                return Ok(None);
            }
            ControlKind::Interrupt => entry.evidence.interrupt(request)?,
            ControlKind::Recreate => anyhow::bail!("recreation requires the managed process owner"),
        };
        control.phase = ControlPhase::Sent;
        Ok(frame)
    })
}

/// A rejected native frame persists invalidation before returning its error.
/// Exact duplicates and unrelated events do not rewrite the journal.
pub fn observe(root: &Path, operation: &str, producer: &str, frame: &Value) -> Result<bool> {
    let mut store = Store::open(root)?;
    let entry = owner_of(store.journal(), operation, producer, false)?;
    let mut normalized = frame.clone();
    let control_id = if frame.get("method").is_none() {
        entry.controls.iter().find_map(|(id, control)| {
            let expected = match control.kind {
                ControlKind::Read => format!("read:{operation}:{id}"),
                ControlKind::Interrupt => format!("interrupt:{id}"),
                ControlKind::Recreate => return None,
            };
            (frame.get("id").and_then(Value::as_str) == Some(expected.as_str()))
                .then_some(id.clone())
        })
    } else {
        None
    };
    if let Some(id) = &control_id {
        let control = entry.controls.get(id).context("native control missing")?;
        if control.phase != ControlPhase::Sent || control.producer != producer {
            return Ok(false);
        }
        let now = crate::tasks::now_ms()?;
        if now < control.created_ms || now >= control.created_ms.saturating_add(control.timeout_ms)
        {
            return Ok(false);
        }
        if control.kind == ControlKind::Read {
            *normalized
                .get_mut("id")
                .context("native read response ID")? = json!(format!("read:{operation}"));
        }
    }
    let mut evidence = entry.evidence.clone();
    let result = evidence.observe(&normalized);
    let observed_ms = if result.as_ref().is_ok_and(|matched| *matched)
        && evidence.fresh
        && evidence.user_item.is_some()
        && (evidence != entry.evidence
            || control_id.as_ref().is_some_and(|id| {
                entry
                    .controls
                    .get(id)
                    .is_some_and(|control| control.kind == ControlKind::Read)
            }))
        && !control_id.as_ref().is_some_and(|id| {
            entry
                .controls
                .get(id)
                .is_some_and(|control| control.kind == ControlKind::Interrupt)
        }) {
        Some(crate::tasks::now_ms()?)
    } else {
        entry.observed_ms
    };
    let observer_changed = result.as_ref().is_ok_and(|matched| *matched)
        && entry.observer.as_deref() != Some(producer);
    if evidence != entry.evidence || observer_changed || control_id.is_some() {
        store.transaction(|journal| {
            let entry = journal
                .native_turns
                .get_mut(operation)
                .context("native operation missing")?;
            entry.evidence = evidence;
            entry.observed_ms = observed_ms;
            entry.observer = Some(producer.into());
            if let Some(id) = control_id {
                entry
                    .controls
                    .get_mut(&id)
                    .context("native control missing")?
                    .phase = ControlPhase::Observed;
            }
            Ok(())
        })?;
    }
    result
}

pub fn retire(root: &Path, task: &str, producer: &str, reason: &str) -> Result<()> {
    let mut store = Store::open(root)?;
    let old = store
        .journal()
        .native_workers
        .get(task)
        .context("native worker missing")?;
    ensure!(
        old.producer == producer,
        "native retirement producer mismatch"
    );
    if !old.ready {
        return Ok(());
    }
    store.transaction(|journal| {
        journal
            .native_workers
            .get_mut(task)
            .context("native worker missing")?
            .ready = false;
        for entry in journal
            .native_turns
            .values_mut()
            .filter(|entry| entry.task == task)
        {
            if (entry.producer == producer || entry.observer.as_deref() == Some(producer))
                && !entry.evidence.phase.terminal()
            {
                entry.evidence.invalidate(reason);
            }
            for control in entry
                .controls
                .values_mut()
                .filter(|control| control.producer == producer && control.pending())
            {
                if !(control.kind == ControlKind::Recreate && control.phase == ControlPhase::Sent) {
                    control.phase = ControlPhase::Obsolete;
                }
            }
        }
        Ok(())
    })
}

pub fn inspect(root: &Path, operation: &str) -> Result<Value> {
    let store = Store::open(root)?;
    inspect_retained(store.journal(), operation, crate::tasks::now_ms()?)
}

/// Evidence age is independent of live provider availability. No probing or writes.
pub(crate) fn inspect_retained(journal: &Journal, operation: &str, now: u64) -> Result<Value> {
    let mut entry = journal
        .native_turns
        .get(operation)
        .context("native operation missing")?
        .clone();
    if owner_of(
        journal,
        operation,
        entry.observer.as_deref().unwrap_or(&entry.producer),
        false,
    )
    .is_err()
    {
        entry.evidence.invalidate(
            "native producer is not the current ready worker; retained evidence is historical",
        );
    }
    let age = entry
        .observed_ms
        .and_then(|observed| now.checked_sub(observed));
    if entry.evidence.fresh && age.is_none_or(|age| age >= 5000) {
        entry.evidence.invalidate(
            "native evidence is absent, expired or clock-invalid; reconcile explicitly",
        );
    }
    Ok(
        json!({"operation":entry,"evidence_age_ms":age,"availability":"not-probed","verified_task_completion":false}),
    )
}

pub fn validate(journal: &Journal) -> Result<()> {
    ensure!(
        journal.native_workers.len() <= model::MAX_TASKS && journal.native_turns.len() <= 128,
        "native journal record limit exceeded"
    );
    for (task, worker) in &journal.native_workers {
        if let Some(storage) = &worker.storage {
            storage.validate()?;
        }
        ensure!(
            task == &worker.task
                && model::id(task)
                && model::id(&worker.marker)
                && model::id(&worker.producer)
                && !worker.thread.is_empty()
                && worker.thread.len() <= 256
                && !worker.thread.chars().any(char::is_control),
            "invalid native worker identity"
        );
        ensure!(
            worker.retired.len() <= 16
                && worker
                    .retired
                    .iter()
                    .all(|producer| model::id(producer) && producer != &worker.producer)
                && worker
                    .retired
                    .iter()
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
                    == worker.retired.len(),
            "invalid native retirement history"
        );
        let attempt = journal
            .attempts
            .get(&worker.attempt)
            .context("native worker attempt missing")?;
        let session = journal
            .sessions
            .get(&attempt.session)
            .context("native worker session missing")?;
        ensure!(
            attempt.task == *task
                && session.ownership == Ownership::Managed
                && session.agent.as_deref() == Some("codex"),
            "native worker is not a managed Codex attempt"
        );
    }
    let mut control_ids = std::collections::BTreeSet::new();
    for (operation, entry) in &journal.native_turns {
        ensure!(
            operation == &entry.evidence.operation
                && model::id(operation)
                && model::id(&entry.marker)
                && model::id(&entry.producer)
                && entry
                    .observer
                    .as_ref()
                    .is_none_or(|observer| model::id(observer))
                && (1..=300_000).contains(&entry.timeout_ms)
                && entry
                    .observed_ms
                    .is_none_or(|observed| observed >= entry.created_ms)
                && entry.created_ms.checked_add(entry.timeout_ms)
                    == Some(entry.evidence.deadline_ms)
                && journal
                    .attempts
                    .get(&entry.attempt)
                    .is_some_and(|attempt| attempt.task == entry.task),
            "invalid native operation identity/deadline"
        );
        entry.evidence.validate()?;
        ensure!(entry.controls.len() <= 16, "native control limit exceeded");
        for (id, control) in &entry.controls {
            ensure!(
                (control.kind == ControlKind::Recreate) == control.replacement.is_some()
                    && control
                        .replacement
                        .as_ref()
                        .is_none_or(|replacement| model::id(replacement)
                            && replacement != &control.producer),
                "invalid native recreation producer"
            );
            ensure!(
                model::id(id)
                    && control_ids.insert(id)
                    && model::id(&control.producer)
                    && (1..=300_000).contains(&control.timeout_ms)
                    && control.created_ms.checked_add(control.timeout_ms).is_some(),
                "invalid native control identity/deadline"
            );
            if control.kind == ControlKind::Interrupt
                && matches!(control.phase, ControlPhase::Sent | ControlPhase::Observed)
            {
                ensure!(
                    entry.evidence.interrupt_operation.as_deref() == Some(id),
                    "native interrupt intent mismatch"
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn fixture() -> Result<tempfile::TempDir> {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir()?;
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))?;
        let path = root.path().to_owned();
        let marker = "a".repeat(32);
        let mut store = Store::open(root.path())?;
        store.transaction(|journal| {
            journal.tasks.insert(
                "task".into(),
                model::Task {
                    id: "task".into(),
                    requested_runtime: path.clone(),
                    title: "native task".into(),
                    created_ms: 1,
                    outcome: TaskOutcome::Open,
                    attempt: "attempt".into(),
                    required_checks: BTreeMap::new(),
                    required_artifacts: BTreeMap::new(),
                },
            );
            journal.attempts.insert(
                "attempt".into(),
                model::Attempt {
                    id: "attempt".into(),
                    task: "task".into(),
                    session: "session".into(),
                    state: model::AttemptState::Active,
                },
            );
            journal.sessions.insert(
                "session".into(),
                model::Session {
                    id: "session".into(),
                    agent: Some("codex".into()),
                    ownership: Ownership::Managed,
                    launch: Some("task".into()),
                    created_ms: 1,
                    target: model::Target {
                        runtime: path.clone(),
                        instance: "instance".into(),
                        workspace: "workspace".into(),
                        stream: 1,
                        pane: 1,
                        pid: Some(std::process::id()),
                    },
                },
            );
            journal.launches.insert(
                "task".into(),
                model::Launch {
                    id: "task".into(),
                    task: None,
                    resume: None,
                    title: "native task".into(),
                    agent: Some("codex".into()),
                    requested_runtime: path.clone(),
                    runtime: path.clone(),
                    requested_cwd: path.clone(),
                    cwd: path.clone(),
                    worktree: None,
                    instance: "instance".into(),
                    workspace: "workspace".into(),
                    stream: 1,
                    event_sequence: 1,
                    argv: vec!["zor".into(), "codex-worker".into()],
                    marker: marker.clone(),
                    integration: None,
                    final_evidence: None,
                    stop_requested: false,
                    created_ms: 1,
                    phase: LaunchPhase::Attached,
                    pane: Some(1),
                    session: Some("session".into()),
                    problem: None,
                },
            );
            Ok(())
        })?;
        drop(store);
        register(
            root.path(),
            Worker {
                task: "task".into(),
                attempt: "attempt".into(),
                marker,
                producer: "producer-1".into(),
                thread: "thread-1".into(),
                storage: None,
                ready: true,
                retired: Vec::new(),
            },
        )?;
        Ok(root)
    }

    fn history() -> Value {
        json!({"id":"read:operation-1","result":{"thread":{"id":"thread-1","turns":[{
            "id":"turn-1","status":"completed","items":[
                {"type":"userMessage","id":"user-1","clientId":"operation-1","content":[{"type":"text","text":"literal\ntext"}]},
                {"type":"agentMessage","id":"response-1","text":"native answer"}]}]}}})
    }

    #[test]
    fn retained_freshness_expires_without_mutating_history_or_duplicate_refresh() -> Result<()> {
        let root = fixture()?;
        prepare(root.path(), "task", "operation-1", "literal\ntext", 300000)?;
        take_submission(root.path(), "operation-1", "producer-1")?;
        let mut frame = history();
        *frame
            .pointer_mut("/result/thread/turns/0/status")
            .context("status")? = json!("inProgress");
        observe(root.path(), "operation-1", "producer-1", &frame)?;
        let observed = Store::open(root.path())?
            .journal()
            .native_turns
            .get("operation-1")
            .context("entry")?
            .observed_ms
            .context("observed")?;
        let generation = Store::open(root.path())?.journal().generation;
        observe(root.path(), "operation-1", "producer-1", &frame)?;
        let store = Store::open(root.path())?;
        assert_eq!(store.journal().generation, generation);
        for now in [observed + 5000, observed - 1] {
            let view = inspect_retained(store.journal(), "operation-1", now)?;
            assert_eq!(
                view.pointer("/operation/evidence/phase"),
                Some(&json!("uncertain"))
            );
            assert_eq!(
                view.pointer("/operation/evidence/fresh"),
                Some(&json!(false))
            );
        }
        assert_eq!(
            inspect_retained(store.journal(), "operation-1", observed + 4999)?
                .pointer("/operation/evidence/phase"),
            Some(&json!("working"))
        );
        assert_eq!(
            store
                .journal()
                .native_turns
                .get("operation-1")
                .context("entry")?
                .evidence
                .phase,
            Phase::Working
        );
        Ok(())
    }

    #[test]
    fn recreation_survives_coordination_cancel_without_replaying_input() -> Result<()> {
        let root = fixture()?;
        let rollout = root.path().join("rollout.json");
        std::fs::write(&rollout, b"fixture storage")?;
        let storage = super::super::storage::Storage::capture(&rollout)?;
        Store::open(root.path())?.transaction(|journal| {
            journal
                .native_workers
                .get_mut("task")
                .context("worker")?
                .storage = Some(storage);
            Ok(())
        })?;
        prepare(root.path(), "task", "operation-1", "literal\ntext", 300000)?;
        take_submission(root.path(), "operation-1", "producer-1")?;
        observe(root.path(), "operation-1", "producer-1", &history())?;
        let intent = request_control(
            root.path(),
            "operation-1",
            "restart-1",
            ControlKind::Recreate,
            300000,
        )?;
        assert!(prepare(root.path(), "task", "operation-2", "second", 300000).is_err());
        Store::open(root.path())?.transaction(|journal| {
            journal.tasks.get_mut("task").context("task")?.outcome = TaskOutcome::Cancelled;
            Ok(())
        })?;
        let action = take_recreation(root.path(), "operation-1", "producer-1", "restart-1")?
            .context("recreation")?;
        assert!(take_recreation(root.path(), "operation-1", "producer-1", "restart-1")?.is_none());
        assert_eq!(
            action.deadline_ms,
            intent
                .get("created_ms")
                .context("created_ms")?
                .as_u64()
                .context("created")?
                + 300000
        );
        retire(root.path(), "task", "producer-1", "recreation")?;
        let mut replacement = Store::open(root.path())?
            .journal()
            .native_workers
            .get("task")
            .context("worker")?
            .clone();
        replacement.producer = action.replacement.clone();
        replacement.ready = true;
        finish_recreation(root.path(), &action, replacement.clone())?;
        finish_recreation(root.path(), &action, replacement)?;
        let retry = request_control(
            root.path(),
            "operation-1",
            "restart-1",
            ControlKind::Recreate,
            300000,
        )?;
        assert_eq!(
            retry.get("created_ms").context("created_ms")?,
            intent.get("created_ms").context("created_ms")?
        );
        assert_eq!(
            retry.get("replacement").context("replacement")?,
            intent.get("replacement").context("replacement")?
        );
        assert_eq!(retry.get("phase").context("phase")?, &json!("observed"));
        assert!(take_submission(root.path(), "operation-1", &action.replacement).is_err());
        assert!(prepare(root.path(), "task", "operation-2", "second", 300000).is_err());
        let store = Store::open(root.path())?;
        assert!(
            store
                .journal()
                .native_workers
                .get("task")
                .context("worker")?
                .ready
        );
        assert_eq!(
            store
                .journal()
                .native_workers
                .get("task")
                .context("worker")?
                .retired,
            vec!["producer-1"]
        );
        assert_eq!(
            store.journal().tasks.get("task").context("task")?.outcome,
            TaskOutcome::Cancelled
        );
        Ok(())
    }

    #[test]
    fn recreation_expiry_and_failed_publication_do_not_grant_another_restart() -> Result<()> {
        let root = fixture()?;
        let rollout = root.path().join("rollout.json");
        std::fs::write(&rollout, b"fixture storage")?;
        let storage = super::super::storage::Storage::capture(&rollout)?;
        Store::open(root.path())?.transaction(|journal| {
            journal
                .native_workers
                .get_mut("task")
                .context("worker")?
                .storage = Some(storage);
            Ok(())
        })?;
        prepare(root.path(), "task", "operation-1", "literal\ntext", 300000)?;
        take_submission(root.path(), "operation-1", "producer-1")?;
        request_control(
            root.path(),
            "operation-1",
            "expired-restart",
            ControlKind::Recreate,
            1,
        )?;
        std::thread::sleep(std::time::Duration::from_millis(5));
        assert!(
            take_recreation(root.path(), "operation-1", "producer-1", "expired-restart")?.is_none()
        );
        let expired = request_control(
            root.path(),
            "operation-1",
            "expired-restart",
            ControlKind::Recreate,
            1,
        )?;
        assert_eq!(expired.get("phase").context("phase")?, &json!("expired"));
        request_control(
            root.path(),
            "operation-1",
            "failed-restart",
            ControlKind::Recreate,
            300000,
        )?;
        let action = take_recreation(root.path(), "operation-1", "producer-1", "failed-restart")?
            .context("recreation")?;
        retire(root.path(), "task", "producer-1", "recreation")?;
        let mut replacement = Store::open(root.path())?
            .journal()
            .native_workers
            .get("task")
            .context("worker")?
            .clone();
        replacement.producer = action.replacement.clone();
        replacement.ready = true;
        // Registration can succeed before final control publication is interrupted.
        register(root.path(), replacement.clone())?;
        // An in-memory action cannot extend the persisted publication deadline.
        Store::open(root.path())?.transaction(|journal| {
            journal
                .native_turns
                .get_mut("operation-1")
                .context("entry")?
                .controls
                .get_mut("failed-restart")
                .context("control")?
                .created_ms = 1;
            Ok(())
        })?;
        assert!(finish_recreation(root.path(), &action, replacement.clone()).is_err());
        fail_recreation(root.path(), &action)?;
        assert!(finish_recreation(root.path(), &action, replacement).is_err());
        let failed = request_control(
            root.path(),
            "operation-1",
            "failed-restart",
            ControlKind::Recreate,
            300000,
        )?;
        assert_eq!(failed.get("phase").context("phase")?, &json!("obsolete"));
        assert!(
            !Store::open(root.path())?
                .journal()
                .native_workers
                .get("task")
                .context("worker")?
                .ready
        );
        assert!(take_submission(root.path(), "operation-1", &action.replacement).is_err());
        Ok(())
    }

    #[test]
    fn durable_controls_preserve_deadlines_and_ignore_unissued_or_duplicate_reads() -> Result<()> {
        let root = fixture()?;
        prepare(root.path(), "task", "operation-1", "literal\ntext", 300000)?;
        assert!(
            request_control(
                root.path(),
                "operation-1",
                "read-1",
                ControlKind::Read,
                300000
            )
            .is_err()
        );
        take_submission(root.path(), "operation-1", "producer-1")?;
        let original = request_control(
            root.path(),
            "operation-1",
            "read-1",
            ControlKind::Read,
            300000,
        )?;
        let mut frame = history();
        *frame.get_mut("id").context("read ID")? = json!("read:operation-1:read-1");
        assert!(!observe(root.path(), "operation-1", "producer-1", &frame)?);
        assert!(take_control(root.path(), "operation-1", "producer-1", "read-1")?.is_some());
        assert!(take_control(root.path(), "operation-1", "producer-1", "read-1")?.is_none());
        assert!(observe(root.path(), "operation-1", "producer-1", &frame)?);
        let generation = Store::open(root.path())?.journal().generation;
        assert!(!observe(root.path(), "operation-1", "producer-1", &frame)?);
        assert_eq!(generation, Store::open(root.path())?.journal().generation);
        let retry = request_control(
            root.path(),
            "operation-1",
            "read-1",
            ControlKind::Read,
            300000,
        )?;
        assert_eq!(original.get("created_ms"), retry.get("created_ms"));
        assert!(
            request_control(root.path(), "operation-1", "read-1", ControlKind::Read, 10).is_err()
        );
        assert!(take_submission(root.path(), "operation-1", "producer-1")?.is_none());
        Ok(())
    }

    #[test]
    fn queued_interrupt_consumes_authority_once_and_expired_read_never_sends() -> Result<()> {
        let root = fixture()?;
        prepare(root.path(), "task", "operation-1", "literal\ntext", 300000)?;
        take_submission(root.path(), "operation-1", "producer-1")?;
        let mut active = history();
        *active
            .pointer_mut("/result/thread/turns/0/status")
            .context("status")? = json!("inProgress");
        observe(root.path(), "operation-1", "producer-1", &active)?;
        request_control(
            root.path(),
            "operation-1",
            "stop-1",
            ControlKind::Interrupt,
            300000,
        )?;
        assert!(take_control(root.path(), "operation-1", "producer-1", "stop-1")?.is_some());
        assert!(take_control(root.path(), "operation-1", "producer-1", "stop-1")?.is_none());
        assert!(
            request_control(
                root.path(),
                "operation-1",
                "stop-2",
                ControlKind::Interrupt,
                300000
            )
            .is_err()
        );
        request_control(
            root.path(),
            "operation-1",
            "expired-read",
            ControlKind::Read,
            1,
        )?;
        std::thread::sleep(std::time::Duration::from_millis(5));
        assert!(take_control(root.path(), "operation-1", "producer-1", "expired-read")?.is_none());
        let journal = Store::open(root.path())?;
        assert_eq!(
            journal
                .journal()
                .native_turns
                .get("operation-1")
                .context("entry")?
                .controls
                .get("expired-read")
                .context("control")?
                .phase,
            ControlPhase::Expired
        );
        Ok(())
    }

    #[test]
    fn persisted_submission_is_never_reissued_after_reopen_and_history_is_not_task_success()
    -> Result<()> {
        let root = fixture()?;
        let first = prepare(root.path(), "task", "operation-1", "literal\ntext", 300000)?;
        assert!(take_submission(root.path(), "operation-1", "producer-1")?.is_some());
        assert!(take_submission(root.path(), "operation-1", "producer-1")?.is_none());
        assert!(prepare(root.path(), "task", "operation-1", "changed", 300000).is_err());
        let retry = prepare(root.path(), "task", "operation-1", "literal\ntext", 300000)?;
        assert_eq!(first.get("created_ms"), retry.get("created_ms"));
        assert_eq!(
            first.pointer("/evidence/deadline_ms"),
            retry.pointer("/evidence/deadline_ms")
        );
        assert!(observe(
            root.path(),
            "operation-1",
            "producer-1",
            &history()
        )?);
        let before = Store::open(root.path())?.journal().generation;
        observe(root.path(), "operation-1", "producer-1", &history())?;
        let store = Store::open(root.path())?;
        assert_eq!(before, store.journal().generation);
        assert_eq!(
            store.journal().tasks.get("task").context("task")?.outcome,
            TaskOutcome::Open
        );
        assert_eq!(store.journal().check_reserve_bytes(), 2036);
        Ok(())
    }

    #[test]
    fn invalid_native_evidence_is_persisted_as_uncertain_before_returning_error() -> Result<()> {
        let root = fixture()?;
        prepare(root.path(), "task", "operation-1", "literal\ntext", 300000)?;
        take_submission(root.path(), "operation-1", "producer-1")?;
        let mut frame = history();
        *frame
            .pointer_mut("/result/thread/id")
            .context("fixture thread")? = json!("other-thread");
        assert!(observe(root.path(), "operation-1", "producer-1", &frame).is_err());
        let record = inspect(root.path(), "operation-1")?;
        assert_eq!(
            record.pointer("/operation/evidence/phase"),
            Some(&json!("uncertain"))
        );
        assert_eq!(record.get("verified_task_completion"), Some(&json!(false)));
        assert!(take_submission(root.path(), "operation-1", "producer-1")?.is_none());
        Ok(())
    }

    #[test]
    fn retirement_rejects_old_events_and_replacement_can_only_reconcile_old_input() -> Result<()> {
        let root = fixture()?;
        prepare(root.path(), "task", "operation-1", "literal\ntext", 300000)?;
        take_submission(root.path(), "operation-1", "producer-1")?;
        let mut replacement = Store::open(root.path())?
            .journal()
            .native_workers
            .get("task")
            .context("worker")?
            .clone();
        replacement.producer = "producer-2".into();
        assert!(register(root.path(), replacement.clone()).is_err());
        retire(root.path(), "task", "producer-1", "owned app-server exited")?;
        assert!(observe(root.path(), "operation-1", "producer-1", &history()).is_err());
        register(root.path(), replacement)?;
        assert!(take_submission(root.path(), "operation-1", "producer-2").is_err());
        observe(root.path(), "operation-1", "producer-2", &history())?;
        assert_eq!(
            inspect(root.path(), "operation-1")?.pointer("/operation/evidence/phase"),
            Some(&json!("completed"))
        );
        Ok(())
    }

    #[test]
    fn outstanding_native_output_reserves_capacity_and_coordination_cancel_blocks_new_input()
    -> Result<()> {
        let root = fixture()?;
        prepare(root.path(), "task", "operation-1", "literal\ntext", 300000)?;
        assert!(prepare(root.path(), "task", "operation-2", "second", 300000).is_err());
        let mut store = Store::open(root.path())?;
        assert_eq!(store.journal().check_reserve_bytes(), 262144 + 2036);
        store.transaction(|journal| {
            journal.tasks.get_mut("task").context("task")?.outcome = TaskOutcome::Cancelled;
            Ok(())
        })?;
        drop(store);
        assert!(take_submission(root.path(), "operation-1", "producer-1").is_err());
        Ok(())
    }

    #[test]
    fn producers_without_turns_still_cannot_reuse_retired_lifetime_identities() -> Result<()> {
        let root = fixture()?;
        let mut replacement = Store::open(root.path())?
            .journal()
            .native_workers
            .get("task")
            .context("worker")?
            .clone();
        retire(root.path(), "task", "producer-1", "closed")?;
        replacement.producer = "producer-2".into();
        register(root.path(), replacement.clone())?;
        let generation = Store::open(root.path())?.journal().generation;
        register(root.path(), replacement.clone())?;
        assert_eq!(generation, Store::open(root.path())?.journal().generation);
        retire(root.path(), "task", "producer-2", "closed")?;
        replacement.producer = "producer-1".into();
        assert!(register(root.path(), replacement).is_err());
        assert!(retire(root.path(), "task", "producer-1", "delayed old retirement").is_err());
        Ok(())
    }

    #[test]
    fn interrupt_intent_is_durable_and_independent_of_coordination_cancellation() -> Result<()> {
        let root = fixture()?;
        prepare(root.path(), "task", "operation-1", "literal\ntext", 300000)?;
        take_submission(root.path(), "operation-1", "producer-1")?;
        let mut frame = history();
        *frame
            .pointer_mut("/result/thread/turns/0/status")
            .context("status")? = json!("inProgress");
        observe(root.path(), "operation-1", "producer-1", &frame)?;
        Store::open(root.path())?.transaction(|journal| {
            journal.tasks.get_mut("task").context("task")?.outcome = TaskOutcome::Cancelled;
            Ok(())
        })?;
        assert!(take_interrupt(root.path(), "operation-1", "producer-1", "interrupt-1")?.is_some());
        assert!(take_interrupt(root.path(), "operation-1", "producer-1", "interrupt-1")?.is_none());
        assert_eq!(
            inspect(root.path(), "operation-1")?.pointer("/operation/evidence/phase"),
            Some(&json!("working"))
        );
        assert!(take_submission(root.path(), "operation-1", "producer-1").is_err());
        Ok(())
    }

    #[test]
    fn journal_rejects_native_completion_without_correlated_identity() -> Result<()> {
        let root = fixture()?;
        prepare(root.path(), "task", "operation-1", "literal\ntext", 300000)?;
        assert!(
            Store::open(root.path())?
                .transaction(|journal| {
                    journal
                        .native_turns
                        .get_mut("operation-1")
                        .context("entry")?
                        .evidence
                        .phase = Phase::Completed;
                    Ok(())
                })
                .is_err()
        );
        assert_eq!(
            inspect(root.path(), "operation-1")?.pointer("/operation/evidence/phase"),
            Some(&json!("prepared"))
        );
        Ok(())
    }

    #[test]
    fn admitted_reservation_funds_intermediate_evidence_at_the_journal_capacity_limit() -> Result<()>
    {
        let root = fixture()?;
        prepare(root.path(), "task", "operation-1", "literal\ntext", 300000)?;
        take_submission(root.path(), "operation-1", "producer-1")?;
        let mut store = Store::open(root.path())?;
        let mut filler = store
            .journal()
            .native_turns
            .get("operation-1")
            .context("native entry")?
            .clone();
        filler.evidence.phase = Phase::Completed;
        filler.evidence.turn = Some("filler-turn".into());
        filler.evidence.user_item = Some("filler-user".into());
        filler.evidence.text = "x".repeat(32768);
        store.transaction(|journal| {
            for index in 0..127 {
                let operation = format!("filler-{index}");
                let mut entry = filler.clone();
                entry.evidence.operation = operation.clone();
                let before = serde_json::to_vec(journal)?.len() + journal.check_reserve_bytes();
                let overhead = serde_json::to_vec(&entry)?.len() + operation.len() + 4;
                if before + overhead + 64 <= crate::tasks::store::MAX_BYTES {
                    journal.native_turns.insert(operation, entry);
                    continue;
                }
                let shortage = before + overhead + 64 - crate::tasks::store::MAX_BYTES;
                ensure!(shortage < entry.evidence.text.len(), "fixture padding room");
                entry
                    .evidence
                    .text
                    .truncate(entry.evidence.text.len() - shortage);
                journal.native_turns.insert(operation, entry);
                break;
            }
            Ok(())
        })?;
        let occupied =
            serde_json::to_vec(store.journal())?.len() + store.journal().check_reserve_bytes();
        ensure!(
            crate::tasks::store::MAX_BYTES - occupied < 128,
            "fixture is not near the admission limit"
        );
        drop(store);
        request_control(
            root.path(),
            "operation-1",
            "capacity-read",
            ControlKind::Read,
            1,
        )?;
        std::thread::sleep(std::time::Duration::from_millis(5));
        assert!(take_control(root.path(), "operation-1", "producer-1", "capacity-read")?.is_none());
        let mut frame = history();
        *frame
            .pointer_mut("/result/thread/turns/0/status")
            .context("status")? = json!("inProgress");
        observe(root.path(), "operation-1", "producer-1", &frame)?;
        assert!(take_interrupt(root.path(), "operation-1", "producer-1", "interrupt-1")?.is_some());
        request_control(
            root.path(),
            "operation-1",
            "capacity-retirement",
            ControlKind::Read,
            300000,
        )?;
        retire(root.path(), "task", "producer-1", "capacity retirement")?;
        let mut replacement = Store::open(root.path())?
            .journal()
            .native_workers
            .get("task")
            .context("worker")?
            .clone();
        replacement.producer = "producer-2".into();
        replacement.ready = true;
        register(root.path(), replacement)?;
        observe(root.path(), "operation-1", "producer-2", &history())?;
        assert_eq!(
            inspect(root.path(), "operation-1")?.pointer("/operation/evidence/phase"),
            Some(&json!("completed"))
        );
        Ok(())
    }
}
