//! Explicit OpenCode native-session recreation, never replay of a launch or prompt.
//!
//! The managed lifecycle remains the sole launch/reconciliation owner. A resume is a Launch
//! operation carrying an immutable, journaled ResumeIntent; it uses the same marker, receipt
//! uncertainty and intent-before-effect boundary. Exclusive World access is necessary to check
//! historical authority and allocate the new attempt atomically with the operation identity.
//!
//! Eligibility is deliberately evidence-based: both the old pane's concrete exit and its
//! sidecar's retired producer lifetime must be retained. Lost/missing processes are not proof
//! of absence. OpenCode's direct interactive executable accepts `--session`; Codex's current
//! sidecar always starts a new thread and Claude has no native authority, so neither is eligible.
//! Arbitrary arguments, shell wrappers and inherited storage namespaces are not replay policy.

use std::path::Path;

use bevy_ecs::prelude::*;
use bevy_reflect::Reflect;
use serde::{Deserialize, Serialize};

use super::{Integration, LaunchSpec, LaunchTemplate, LifecycleError, Link};
use crate::model::*;
use crate::providers::{Provider, ProviderKind, ProviderSession, SessionState};

/// Public IDs survive scene entity remapping. The enclosing OperationOf owns the new attempt;
/// previous_attempt is historical authority, not a lifetime-coupled relationship.
#[derive(Component, Reflect, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[reflect(Component)]
#[component(immutable)]
pub struct ResumeIntent {
    pub previous_attempt: u64,
    pub fux_instance: String,
    pub native_session: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeSpec {
    pub operation: String,
    pub fux_instance: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Eligibility {
    pub previous_attempt: u64,
    pub native_session: String,
    pub provider: ProviderKind,
}

struct Plan<'a> {
    previous_attempt: u64,
    session: &'a str,
    executable: &'a str,
    template: &'a LaunchTemplate,
    provider: &'a Provider,
}

fn refuse<T>(reason: &str) -> Result<T, LifecycleError> {
    Err(LifecycleError::Refused(reason.into()))
}

/// These are the OpenCode namespace inputs retained by the old integration contract. Requiring
/// explicit values prevents a new zor process's inherited HOME/XDG defaults selecting another
/// provider database under an otherwise identical native session ID.
const STORAGE_KEYS: &[&str] = &[
    "HOME",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_STATE_HOME",
    "XDG_CACHE_HOME",
];

/// Pin the effective OpenCode storage namespace in both terminal and sidecar launch intent.
/// XDG treats an absent/empty setting as its HOME-relative default. A stable launch retry uses
/// its original snapshot for omitted keys; it must not acquire the restarted daemon's HOME.
pub(super) fn capture_storage_env(world: &World, spec: &mut LaunchSpec) {
    if !spec
        .integration
        .as_ref()
        .is_some_and(|provider| provider.kind == ProviderKind::OpenCode)
    {
        return;
    }
    let retained = world
        .resource::<Ids>()
        .operation(&spec.operation)
        .and_then(|operation| world.get::<LaunchTemplate>(operation));
    if let Some(template) = retained {
        for key in STORAGE_KEYS {
            if let Some(pair) = template.env.iter().find(|(name, _)| name == key) {
                match spec.env.iter_mut().rev().find(|(name, _)| name == key) {
                    Some((_, value)) if *key != "HOME" && value.is_empty() => {
                        *value = pair.1.clone();
                    }
                    None => spec.env.push(pair.clone()),
                    _ => {}
                }
            }
        }
        return;
    }
    let value = |key: &str| {
        spec.env
            .iter()
            .rev()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.clone())
            .or_else(|| std::env::var(key).ok())
    };
    let Some(home) = value("HOME").filter(|home| Path::new(home).is_absolute()) else {
        // Launch remains supported; without proof of the namespace resume is unavailable.
        return;
    };
    let mut effective = Vec::with_capacity(STORAGE_KEYS.len());
    effective.push(("HOME", home.clone()));
    for (key, suffix) in [
        ("XDG_CONFIG_HOME", ".config"),
        ("XDG_DATA_HOME", ".local/share"),
        ("XDG_STATE_HOME", ".local/state"),
        ("XDG_CACHE_HOME", ".cache"),
    ] {
        let directory = value(key)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| Path::new(&home).join(suffix).display().to_string());
        effective.push((key, directory));
    }
    for (key, value) in effective {
        if let Some((_, original)) = spec.env.iter_mut().rev().find(|(name, _)| name == key) {
            *original = value;
        } else {
            spec.env.push((key.into(), value));
        }
    }
}

fn plan(world: &World, task: Entity) -> Result<Plan<'_>, LifecycleError> {
    if super::task_state(world, task)?.is_closed() || world.get::<StopRequested>(task).is_some() {
        return refuse("resume requires an open task without stop intent");
    }
    if super::attempt_of(world, task).is_some() {
        return refuse("original process absence is unproven: task has a current attempt");
    }
    let old = world
        .get::<Attempts>(task)
        .and_then(|attempts| {
            attempts
                .iter()
                .filter_map(|entity| world.get::<AttemptId>(entity).map(|id| (id.0, entity)))
                .max_by_key(|(id, _)| *id)
        })
        .ok_or_else(|| LifecycleError::Refused("resume requires a retained attempt".into()))?;
    let (previous_attempt, attempt) = old;
    if world.get::<Ownership>(attempt) != Some(&Ownership::Managed)
        || world.get::<AttemptState>(attempt) != Some(&AttemptState::Finished)
        || world
            .get::<FinalEvidence>(attempt)
            .is_none_or(|e| e.exit_code.is_none())
        || world.get::<Uncertain>(attempt).is_some()
        || world.get::<Lost>(attempt).is_some()
    {
        return refuse("resume requires a managed attempt with a verified process exit");
    }
    if world.get::<TaskChecks>(task).is_some_and(|checks| {
        checks.iter().any(|check| {
            matches!(
                world.get::<CheckState>(check),
                Some(CheckState::Queued | CheckState::Running | CheckState::Uncertain)
            )
        })
    }) {
        return refuse("unresolved checks prevent resume");
    }
    // Any attempt of the task can still be pinned by a group or retain uncertain delivery.
    if let Some(attempts) = world.get::<Attempts>(task) {
        for entity in attempts.iter() {
            if world.get::<Operations>(entity).is_some_and(|operations| {
                operations.iter().any(|operation| {
                    world.get::<MemberOf>(operation).is_some()
                        || matches!(
                            world.get::<OperationPhase>(operation),
                            Some(
                                OperationPhase::Prepared
                                    | OperationPhase::Submitting
                                    | OperationPhase::Uncertain
                            )
                        )
                })
            }) {
                return refuse("group membership or unresolved operations prevent resume");
            }
            if world.get::<Prompts>(entity).is_some_and(|prompts| {
                prompts.iter().any(|prompt| {
                    world
                        .get::<Delivery>(prompt)
                        .is_some_and(|d| d.is_pending())
                })
            }) {
                return refuse("unresolved prompt delivery prevents resume");
            }
        }
    }
    let provider = world
        .get::<Provider>(attempt)
        .ok_or_else(|| LifecycleError::Refused("no retained native provider authority".into()))?;
    if provider.kind != ProviderKind::OpenCode {
        return refuse(
            "only OpenCode has a supported native session resume command; Codex starts a new thread and Claude is passive",
        );
    }
    if world
        .get::<ProviderSession>(attempt)
        .is_some_and(|s| !matches!(s.state, SessionState::Exited { .. }))
    {
        return refuse("old provider sidecar absence is unproven");
    }
    let lifetime = world
        .get::<ProducerLifetime>(attempt)
        .ok_or_else(|| LifecycleError::Refused("provider exit authority is missing".into()))?;
    if !lifetime.producer.is_empty() {
        return refuse("old provider sidecar is not retired");
    }
    let handle = world
        .get::<PaneHandle>(attempt)
        .ok_or_else(|| LifecycleError::Refused("old pane identity is missing".into()))?;
    let mut session = None;
    if let Some(prompts) = world.get::<Prompts>(attempt) {
        for prompt in prompts.iter() {
            let Some(binding) = world.get::<Binding>(prompt) else {
                continue;
            };
            let receipt_matches = world.get::<Receipt>(prompt).is_some_and(|receipt| {
                receipt.instance == handle.instance
                    && receipt.pane == handle.pane
                    && receipt.operation == binding.input_operation
            });
            let retired = lifetime.retired.iter().any(|(producer, start, end)| {
                !producer.is_empty()
                    && *producer == binding.producer
                    && *start <= binding.bound_ms
                    && binding.bound_ms <= *end
            });
            if !receipt_matches
                || !retired
                || binding.session.is_empty()
                || binding.session.len() > 256
                || binding.session.chars().any(char::is_control)
            {
                return refuse(
                    "native session binding does not match retained receipt and retired producer authority",
                );
            }
            if session.is_some_and(|old| old != binding.session.as_str()) {
                return refuse("ambiguous native session identities in the old attempt");
            }
            session = Some(binding.session.as_str());
        }
    }
    let session = session.ok_or_else(|| {
        LifecycleError::Refused("no receipt-correlated native session binding".into())
    })?;
    let operation = super::launch_of(world, attempt)
        .ok_or_else(|| LifecycleError::Refused("managed launch authority is missing".into()))?;
    let template = world
        .get::<LaunchTemplate>(operation)
        .ok_or_else(|| LifecycleError::Refused("managed launch template is missing".into()))?;
    if template.integration.as_deref() != Some("opencode")
        || provider.argv.is_empty()
        || provider.cwd.as_deref() != Some(template.cwd.as_str())
        || provider.env != template.env
    {
        return refuse("provider and launch storage context disagree");
    }
    for key in STORAGE_KEYS {
        let mut values = template.env.iter().filter(|(name, _)| name == key);
        let Some((_, value)) = values.next() else {
            return refuse(
                "resume requires explicitly retained HOME and all XDG storage directories",
            );
        };
        if !Path::new(value).is_absolute() || value.contains('\0') || values.next().is_some() {
            return refuse("resume storage directories must be unique absolute paths");
        }
    }
    let argv = template.argv.get(3..).unwrap_or_default();
    let Some(executable) = argv.first() else {
        return refuse("original provider executable is missing");
    };
    if Path::new(executable)
        .file_name()
        .and_then(|name| name.to_str())
        != Some("opencode")
    {
        return refuse("resume requires a direct opencode executable, not a command wrapper");
    }
    if let Some(previous) = world.get::<ResumeIntent>(operation) {
        if argv.len() != 3
            || argv.get(1).map(String::as_str) != Some("--session")
            || argv.get(2) != Some(&previous.native_session)
            || session != previous.native_session
        {
            return refuse("retained resumed command or native session changed");
        }
    } else if argv.len() != 1 {
        return refuse("original command arguments are not a supported resume policy");
    }
    let cwd = match world.get::<Location>(task) {
        Some(Location::Cwd(path)) => Some(path.clone()),
        Some(Location::Worktree(id)) => world
            .resource::<Ids>()
            .worktree(id)
            .and_then(|entity| crate::worktrees::path_of(world, entity))
            .map(|path| path.display().to_string()),
        None => None,
    };
    if cwd.as_deref() != Some(template.cwd.as_str()) {
        return refuse("task location no longer matches the native session launch");
    }
    Ok(Plan {
        previous_attempt,
        session,
        executable,
        template,
        provider,
    })
}

/// Read-only eligibility, not permission to dispatch later without repeating these checks.
pub fn eligibility(world: &World, task: Entity) -> Result<Eligibility, LifecycleError> {
    let plan = plan(world, task)?;
    Ok(Eligibility {
        previous_attempt: plan.previous_attempt,
        native_session: plan.session.into(),
        provider: plan.provider.kind,
    })
}

/// Returns the original attempt on a stable retry, including after a lost reply or restart.
/// Never calls launch's Prepared retry path: even an undispatched restored resume is reconciled,
/// not authority to repeat external effects after its eligibility may have changed.
pub fn retained(
    world: &World,
    task: Entity,
    spec: &ResumeSpec,
) -> Result<Option<Entity>, LifecycleError> {
    let Some(operation) = world.resource::<Ids>().operation(&spec.operation) else {
        if world.resource::<Ids>().prompt(&spec.operation).is_some() {
            return Err(LifecycleError::Conflict(
                "resume operation ID belongs to a prompt".into(),
            ));
        }
        return Ok(None);
    };
    let intent = world.get::<ResumeIntent>(operation);
    let attempt = world.get::<OperationOf>(operation).map(|owner| owner.0);
    if world.get::<OperationKind>(operation) != Some(&OperationKind::Launch)
        || intent.is_none_or(|intent| intent.fux_instance != spec.fux_instance)
        || attempt.is_none_or(|attempt| super::task_of(world, attempt) != Some(task))
    {
        return Err(LifecycleError::Conflict(
            "resume operation was recorded with different intent".into(),
        ));
    }
    Ok(attempt)
}

pub fn resume(world: &mut World, task: Entity, spec: ResumeSpec) -> Result<Entity, LifecycleError> {
    if let Some(attempt) = retained(world, task, &spec)? {
        return Ok(attempt);
    }
    if world.resource::<Link>().instance.as_deref() != Some(spec.fux_instance.as_str())
        || spec.fux_instance.is_empty()
    {
        return refuse("requested fux incarnation is not the connected instance");
    }
    let plan = plan(world, task)?;
    let intent = ResumeIntent {
        previous_attempt: plan.previous_attempt,
        fux_instance: spec.fux_instance,
        native_session: plan.session.into(),
    };
    let launch = LaunchSpec {
        operation: spec.operation,
        argv: vec![
            plan.executable.into(),
            "--session".into(),
            plan.session.into(),
        ],
        env: plan.template.env.clone(),
        // A late cleanup of the old ephemeral workspace must never target the new process.
        workspace: if plan.template.ephemeral {
            format!("resume-{}", super::random_hex(32)?)
        } else {
            plan.template.workspace.clone()
        },
        ephemeral: plan.template.ephemeral,
        integration: Some(Integration {
            kind: plan.provider.kind,
            argv: plan.provider.argv.clone(),
        }),
    };
    let attempt = super::launch(world, task, launch)?;
    // launch has only enqueued an Effect: PostUpdate journal sees both intents before Last
    // can hand that effect to the adapter. No intermediate World is externally observable.
    let operation = super::launch_of(world, attempt)
        .ok_or_else(|| LifecycleError::NotFound("new resume launch operation is missing".into()))?;
    world.entity_mut(operation).insert(intent);
    Ok(attempt)
}
