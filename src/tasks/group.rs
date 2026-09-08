//! Bounded durable fan-out over prepared prompts for existing managed tasks.
use super::{model::*, store::Store, submit};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeSet, path::Path};

pub(crate) const MAX_GROUPS: usize = 32;

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RequestMember {
    pub operation: String,
    #[serde(default)]
    pub after: Vec<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Member {
    pub request: RequestMember,
    pub task: String,
    pub attempt: String,
    pub admitted: bool,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Group {
    pub id: String,
    pub concurrency: usize,
    pub members: Vec<Member>,
    pub cancelled: bool,
    pub cursor: Option<String>,
    pub retirement_cursor: Option<String>,
    pub created_generation: u64,
    #[serde(default)]
    pub automatic: bool,
    #[serde(default)]
    pub run_generation: u64,
    #[serde(default)]
    pub run_problem: Option<String>,
}

pub fn cli_members(operations: Vec<String>, after: Vec<String>) -> Result<Vec<RequestMember>> {
    let mut members: Vec<_> = operations
        .into_iter()
        .map(|operation| RequestMember {
            operation,
            after: vec![],
        })
        .collect();
    anyhow::ensure!(after.len() <= 64, "too many group dependencies");
    for edge in after {
        let (operation, task) = edge
            .split_once('=')
            .context("dependency must be OPERATION=TASK")?;
        let member = members
            .iter_mut()
            .find(|member| member.operation == operation)
            .context("dependency operation is not a member")?;
        member.after.push(task.into());
    }
    Ok(members)
}
fn normalize(members: &mut [RequestMember]) -> Result<()> {
    anyhow::ensure!(
        (1..=8).contains(&members.len()),
        "group requires 1..8 members"
    );
    members.sort_by(|a, b| a.operation.cmp(&b.operation));
    let mut operations = BTreeSet::new();
    for member in members {
        anyhow::ensure!(
            id(&member.operation) && operations.insert(&member.operation),
            "invalid or duplicate group operation"
        );
        member.after.sort();
        anyhow::ensure!(
            member.after.len() <= 8
                && member.after.iter().all(|task| id(task))
                && member
                    .after
                    .iter()
                    .zip(member.after.iter().skip(1))
                    .all(|(a, b)| a < b),
            "invalid or duplicate dependency"
        );
    }
    Ok(())
}
fn records<'a>(
    journal: &'a Journal,
    member: &Member,
) -> Result<(&'a Task, &'a Prompt, &'a Session)> {
    let task = journal
        .tasks
        .get(&member.task)
        .context("group task missing")?;
    let prompt = journal
        .prompts
        .get(&member.request.operation)
        .context("group prompt missing")?;
    let attempt = journal
        .attempts
        .get(&member.attempt)
        .context("group attempt missing")?;
    let session = journal
        .sessions
        .get(&attempt.session)
        .context("group session missing")?;
    anyhow::ensure!(
        task.attempt == member.attempt
            && prompt.attempt == member.attempt
            && attempt.task == task.id
            && session.ownership == Ownership::Managed,
        "group member identity changed"
    );
    Ok((task, prompt, session))
}
fn dependencies_ready(journal: &Journal, member: &Member) -> bool {
    member
        .request
        .after
        .iter()
        .all(|task| journal.verifications.contains_key(task))
}
fn complete(journal: &Journal, member: &Member) -> bool {
    member.admitted
        && journal.verifications.contains_key(&member.task)
        && journal
            .prompts
            .get(&member.request.operation)
            .is_some_and(|prompt| prompt.delivery == Delivery::Delivered)
}
fn active_count(journal: &Journal, group: &Group) -> usize {
    group
        .members
        .iter()
        .filter(|member| member.admitted && !complete(journal, member))
        .count()
}
pub(super) fn validate(journal: &Journal) -> Result<()> {
    anyhow::ensure!(
        journal.groups.len() <= MAX_GROUPS,
        "group record limit exceeded"
    );
    let mut operations = BTreeSet::new();
    let mut active_targets = BTreeSet::new();
    for (key, group) in &journal.groups {
        anyhow::ensure!(
            id(key)
                && key == &group.id
                && (1..=8).contains(&group.members.len())
                && (1..=group.members.len()).contains(&group.concurrency)
                && group.created_generation > 0
                && group.created_generation <= journal.generation,
            "invalid group identity or limits"
        );
        anyhow::ensure!(
            (!group.automatic || (!group.cancelled && group.run_generation > 0))
                && group.run_generation <= journal.generation
                && group
                    .run_problem
                    .as_ref()
                    .is_none_or(|problem| problem.len() <= 512),
            "invalid group scheduling state"
        );
        let requests: Vec<_> = group
            .members
            .iter()
            .map(|member| member.request.clone())
            .collect();
        let mut normalized = requests.clone();
        normalize(&mut normalized)?;
        anyhow::ensure!(requests == normalized, "group members are not canonical");
        anyhow::ensure!(
            group
                .retirement_cursor
                .as_ref()
                .is_none_or(|operation| group.cancelled
                    && journal.prompts.get(operation).is_some_and(|prompt| group
                        .members
                        .iter()
                        .any(|member| member.attempt == prompt.attempt))),
            "invalid group retirement cursor"
        );
        anyhow::ensure!(
            group.cursor.as_ref().is_none_or(|cursor| group
                .members
                .iter()
                .any(|member| &member.request.operation == cursor)),
            "invalid group cursor"
        );
        let mut tasks = BTreeSet::new();
        for member in &group.members {
            let (_, prompt, session) = records(journal, member)?;
            anyhow::ensure!(
                operations.insert(&member.request.operation) && tasks.insert(&member.task),
                "group operation or task reused"
            );
            anyhow::ensure!(
                member
                    .request
                    .after
                    .iter()
                    .all(|task| task != &member.task && journal.tasks.contains_key(task)),
                "group dependency missing or self-referential"
            );
            if member.admitted {
                anyhow::ensure!(
                    dependencies_ready(journal, member),
                    "group admitted before dependencies were verified"
                );
            } else {
                anyhow::ensure!(
                    prompt.delivery == Delivery::Prepared
                        && prompt.receipt.is_none()
                        && prompt.arm.is_none()
                        && prompt.response.is_none()
                        && prompt.report_binding.is_none(),
                    "unadmitted group prompt has delivery evidence"
                );
            }
            if !group.cancelled && !complete(journal, member) {
                anyhow::ensure!(
                    active_targets.insert(session.target.identity()),
                    "target belongs to another active group member"
                );
            }
            let mut todo = if group.cancelled {
                vec![]
            } else {
                member.request.after.clone()
            };
            let mut visited = BTreeSet::new();
            while let Some(task) = todo.pop() {
                anyhow::ensure!(task != member.task, "group dependency cycle");
                if visited.insert(task.clone())
                    && let Some(other) = journal
                        .groups
                        .values()
                        .filter(|other| !other.cancelled)
                        .flat_map(|other| &other.members)
                        .find(|other| other.task == task)
                {
                    todo.extend(other.request.after.iter().cloned());
                }
            }
        }
        anyhow::ensure!(
            active_count(journal, group) <= group.concurrency,
            "group admission exceeds concurrency"
        );
    }
    Ok(())
}

/// Applies to repair prompts and aliases of the same target as well as the planned operation.
pub(super) fn authorize(journal: &Journal, prompt: &Prompt) -> Result<()> {
    let target = &journal
        .attempts
        .get(&prompt.attempt)
        .and_then(|attempt| journal.sessions.get(&attempt.session))
        .context("prompt session missing")?
        .target;
    for group in journal.groups.values() {
        for member in &group.members {
            anyhow::ensure!(
                !(group.cancelled && member.request.operation == prompt.id),
                "group was cancelled; submission is disabled"
            );
            if !group.cancelled
                && !complete(journal, member)
                && records(journal, member)?.2.target.identity() == target.identity()
            {
                anyhow::ensure!(
                    member.admitted,
                    "group member has not been admitted; use group-step"
                );
            }
        }
    }
    Ok(())
}

pub fn create(
    root: &Path,
    name: &str,
    concurrency: usize,
    mut requests: Vec<RequestMember>,
) -> Result<Value> {
    anyhow::ensure!(id(name), "invalid group ID");
    normalize(&mut requests)?;
    anyhow::ensure!(
        (1..=requests.len()).contains(&concurrency),
        "invalid group concurrency"
    );
    let mut store = Store::open(root)?;
    if let Some(group) = store.journal().groups.get(name) {
        anyhow::ensure!(
            group.concurrency == concurrency
                && group
                    .members
                    .iter()
                    .map(|member| member.request.clone())
                    .collect::<Vec<_>>()
                    == requests,
            "group ID already has a different plan"
        );
        return view(store.journal(), name);
    }
    let mut members = Vec::new();
    for request in requests {
        let prompt = store
            .journal()
            .prompts
            .get(&request.operation)
            .context("group prompt missing")?;
        let attempt = store
            .journal()
            .attempts
            .get(&prompt.attempt)
            .context("group attempt missing")?;
        let member = Member {
            request: request.clone(),
            task: attempt.task.clone(),
            attempt: attempt.id.clone(),
            admitted: false,
        };
        let (task, prompt, session) = records(store.journal(), &member)?;
        let launch = session
            .launch
            .as_ref()
            .and_then(|id| store.journal().launches.get(id))
            .context("managed launch missing")?;
        anyhow::ensure!(
            task.outcome == TaskOutcome::Open
                && prompt.delivery == Delivery::Prepared
                && !prompt.released
                && prompt.deadline_ms > super::now_ms()?
                && launch.phase == LaunchPhase::Attached
                && !launch.stop_requested,
            "group requires open managed tasks with live, unsent prompt coordination"
        );
        members.push(member);
    }
    store.transaction(|journal| {
        let group = Group {
            id: name.into(),
            concurrency,
            members,
            cancelled: false,
            cursor: None,
            retirement_cursor: None,
            automatic: false,
            run_generation: 0,
            run_problem: None,
            created_generation: journal
                .generation
                .checked_add(1)
                .context("generation exhausted")?,
        };
        journal.groups.insert(name.into(), group);
        Ok(())
    })?;
    view(store.journal(), name)
}

pub(crate) fn view(journal: &Journal, name: &str) -> Result<Value> {
    let group = journal.groups.get(name).context("group not found")?;
    let now = super::now_ms()?;
    let mut members = Vec::new();
    for member in &group.members {
        let (task, prompt, _) = records(journal, member)?;
        let state = if complete(journal, member) {
            "verified"
        } else if task.outcome != TaskOutcome::Open
            || prompt.released
            || matches!(
                prompt.wait,
                WaitOutcome::NeedsInput
                    | WaitOutcome::TimedOut
                    | WaitOutcome::Uncertain
                    | WaitOutcome::ProcessExited
            )
            || matches!(prompt.delivery, Delivery::Failed | Delivery::Uncertain)
            || (prompt.delivery != Delivery::Delivered && now >= prompt.deadline_ms)
        {
            "needs-attention"
        } else if prompt.wait == WaitOutcome::ResponseObserved {
            "verification-required"
        } else if !member.admitted && !dependencies_ready(journal, member) {
            "dependencies-pending"
        } else if !member.admitted {
            "queued"
        } else {
            "active"
        };
        members.push(json!({"operation":member.request.operation,"task":member.task,"attempt":member.attempt,
            "after":member.request.after,"admitted":member.admitted,"state":state,
            "task_outcome":task.outcome,"delivery":prompt.delivery,"wait":prompt.wait,"released":prompt.released}));
    }
    let state = if group.cancelled {
        "cancelled"
    } else if group.members.iter().all(|member| complete(journal, member)) {
        "complete"
    } else if group.run_problem.is_some()
        || members
            .iter()
            .any(|member| member.get("state").and_then(Value::as_str) == Some("needs-attention"))
    {
        "needs-attention"
    } else {
        "active"
    };
    let retirement_pending = retirements(journal, group);
    Ok(
        json!({"v":1,"generation":journal.generation,"group":group,"state":state,"members":members,
        "retirement_pending_count":retirement_pending.len(),"retirement_pending":retirement_pending.iter().take(8).collect::<Vec<_>>(),
        "active_count":active_count(journal, group),"scope":"admitted-unverified-tasks; existing-processes-not-counted"}),
    )
}
pub fn inspect(root: &Path, name: &str) -> Result<Value> {
    view(Store::open(root)?.journal(), name)
}
pub fn list(root: &Path) -> Result<Value> {
    let store = Store::open(root)?;
    let mut groups = Vec::new();
    for (name, group) in &store.journal().groups {
        let summary = view(store.journal(), name)?;
        groups.push(
            json!({"id":name,"state":summary.get("state"),"concurrency":group.concurrency,
            "active_count":summary.get("active_count"),"member_count":group.members.len(),
            "automatic":group.automatic,"run_problem":group.run_problem}),
        );
    }
    Ok(json!({"generation":store.journal().generation,"groups":groups}))
}

pub fn step(root: &Path, name: &str) -> Result<Value> {
    step_mode(root, name, false)
}

pub fn configure(root: &Path, name: &str, automatic: bool) -> Result<Value> {
    let mut store = Store::open(root)?;
    let group = store
        .journal()
        .groups
        .get(name)
        .context("group not found")?;
    anyhow::ensure!(!automatic || !group.cancelled, "cancelled group cannot run");
    if group.automatic != automatic {
        store.transaction(|journal| {
            let group = journal.groups.get_mut(name).context("group missing")?;
            group.automatic = automatic;
            group.run_generation = journal
                .generation
                .checked_add(1)
                .context("generation exhausted")?;
            group.run_problem = None;
            Ok(())
        })?;
    }
    view(store.journal(), name)
}

fn candidates(journal: &Journal, group: &Group, now: u64) -> Result<Vec<String>> {
    let capacity = active_count(journal, group) < group.concurrency;
    let mut candidates = Vec::new();
    for member in &group.members {
        let (task, prompt, _) = records(journal, member)?;
        if task.outcome == TaskOutcome::Open
            && !prompt.released
            && now < prompt.deadline_ms
            && !matches!(prompt.delivery, Delivery::Delivered | Delivery::Failed)
            && dependencies_ready(journal, member)
            && (member.admitted || capacity)
        {
            candidates.push(member.request.operation.clone());
        }
    }
    Ok(candidates)
}

/// One eligible operation per tick, rotating across groups. No absent journal is created.
#[derive(Clone, Debug)]
pub(crate) struct Pause {
    group: String,
    generation: u64,
    problem: String,
}
impl std::fmt::Display for Pause {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.problem)
    }
}
impl std::error::Error for Pause {}
fn record_pause(root: &Path, pause: &Pause) -> Result<()> {
    let mut store = Store::open(root)?;
    let group = store
        .journal()
        .groups
        .get(&pause.group)
        .context("group missing")?;
    if group.automatic && group.run_generation == pause.generation {
        store.transaction(|journal| {
            let group = journal
                .groups
                .get_mut(&pause.group)
                .context("group missing")?;
            group.automatic = false;
            group.run_problem = Some(pause.problem.clone());
            Ok(())
        })?;
    }
    Ok(())
}
pub(crate) fn advance(
    root: &Path,
    after: Option<&str>,
    pending: &mut Option<Pause>,
) -> Result<Option<String>> {
    if let Some(pause) = pending.as_ref() {
        record_pause(root, pause)?;
        let selected = pause.group.clone();
        *pending = None;
        return Ok(Some(selected));
    }
    if !root.join("journal.json").try_exists()? {
        return Ok(None);
    }
    let store = Store::open(root)?;
    let now = super::now_ms()?;
    let mut ready = Vec::new();
    for (id, group) in &store.journal().groups {
        if group.automatic
            && !group.cancelled
            && !candidates(store.journal(), group, now)?.is_empty()
        {
            ready.push(id.clone());
        }
    }
    let selected = ready
        .iter()
        .find(|id| after.is_none_or(|after| id.as_str() > after))
        .or_else(|| ready.first())
        .cloned();
    drop(store);
    if let Some(id) = &selected {
        // The durable admission and receipt protocol handles service loss. A transient
        // busy journal retries on a later tick; rotation still gives other groups a turn.
        if let Err(error) = step_mode(root, id, true)
            && let Some(pause) = error.downcast_ref::<Pause>()
        {
            *pending = Some(pause.clone());
        }
    }
    Ok(selected)
}

fn step_mode(root: &Path, name: &str, automatic: bool) -> Result<Value> {
    let mut store = Store::open(root)?;
    let journal = store.journal();
    let group = journal.groups.get(name).context("group not found")?;
    if group.cancelled || (automatic && !group.automatic) {
        return view(journal, name);
    }
    let now = super::now_ms()?;
    let candidates = candidates(journal, group, now)?;
    let run_generation = group.run_generation;
    let selected = candidates
        .iter()
        .find(|operation| {
            group
                .cursor
                .as_ref()
                .is_none_or(|cursor| *operation > cursor)
        })
        .or_else(|| candidates.first())
        .cloned();
    let Some(operation) = selected else {
        return view(journal, name);
    };
    store.transaction(|journal| {
        let group = journal.groups.get_mut(name).context("group missing")?;
        let member = group
            .members
            .iter_mut()
            .find(|member| member.request.operation == operation)
            .context("member missing")?;
        member.admitted = true;
        group.cursor = Some(operation.clone());
        Ok(())
    })?;
    drop(store);
    let outcome = submit::run(root, &operation, submit::Action::Submit);
    let problem = match &outcome {
        Err(error) if !error.is::<super::store::Busy>() => Some(format!("{error:#}")),
        Ok(value) if !matches!(value.get("delivery").and_then(Value::as_str), Some("queued" | "delivered")) =>
            Some("submission has no queued or delivered receipt; inspect the retained operation before resuming".into()),
        _ => None,
    };
    if automatic && let Some(problem) = problem {
        let pause = Pause {
            group: name.into(),
            generation: run_generation,
            problem: problem.chars().take(128).collect(),
        };
        record_pause(root, &pause).map_err(|_| anyhow::Error::new(pause))?;
    }
    let mut value = inspect(root, name)?;
    let object = value
        .as_object_mut()
        .context("group view is not an object")?;
    object.insert("selected".into(), json!(operation));
    let submission = match outcome {
        Ok(_) => json!({"status":"recorded"}),
        Err(error) => {
            json!({"status":"unresolved","problem":format!("{error:#}").chars().take(512).collect::<String>()})
        }
    };
    object.insert("submission".into(), submission);
    Ok(value)
}

fn retirements<'a>(journal: &'a Journal, group: &Group) -> Vec<&'a str> {
    journal
        .prompts
        .values()
        .filter(|prompt| {
            prompt.released
                && !super::integration::retired(journal, prompt)
                && group
                    .members
                    .iter()
                    .any(|member| member.attempt == prompt.attempt)
                && prompt
                    .arm
                    .as_ref()
                    .is_some_and(|arm| !arm.input_started && !arm.disarmed)
        })
        .map(|prompt| prompt.id.as_str())
        .collect()
}

pub fn cancel(root: &Path, name: &str) -> Result<Value> {
    let mut store = Store::open(root)?;
    let group = store
        .journal()
        .groups
        .get(name)
        .context("group not found")?;
    let cancelled = group.cancelled;
    let attempts: BTreeSet<_> = group
        .members
        .iter()
        .map(|member| member.attempt.clone())
        .collect();
    if !cancelled {
        store.transaction(|journal| {
            let group = journal.groups.get_mut(name).context("group missing")?;
            group.cancelled = true;
            group.automatic = false;
            for prompt in journal
                .prompts
                .values_mut()
                .filter(|prompt| attempts.contains(&prompt.attempt))
            {
                prompt.released = true;
                let verified = journal
                    .attempts
                    .get(&prompt.attempt)
                    .is_some_and(|attempt| journal.verifications.contains_key(&attempt.task));
                if !verified
                    || !matches!(
                        prompt.wait,
                        WaitOutcome::ResponseObserved | WaitOutcome::ProcessExited
                    )
                {
                    prompt.wait = WaitOutcome::Cancelled;
                }
            }
            Ok(())
        })?;
    }
    let group = store.journal().groups.get(name).context("group missing")?;
    let pending = retirements(store.journal(), group);
    let selected = pending
        .iter()
        .find(|operation| {
            group
                .retirement_cursor
                .as_ref()
                .is_none_or(|cursor| **operation > cursor.as_str())
        })
        .or_else(|| pending.first())
        .map(|operation| (*operation).to_owned());
    let Some(operation) = selected else {
        return view(store.journal(), name);
    };
    store.transaction(|journal| {
        journal
            .groups
            .get_mut(name)
            .context("group missing")?
            .retirement_cursor = Some(operation.clone());
        Ok(())
    })?;
    let outcome = super::integration::disarm(&mut store, &operation);
    let mut value = view(store.journal(), name)?;
    let retirement = match outcome {
        Ok(()) => json!({"operation":operation,"status":"retired"}),
        Err(error) => {
            json!({"operation":operation,"status":"unresolved","problem":format!("{error:#}").chars().take(512).collect::<String>()})
        }
    };
    value
        .as_object_mut()
        .context("group view is not an object")?
        .insert("retirement".into(), retirement);
    Ok(value)
}
