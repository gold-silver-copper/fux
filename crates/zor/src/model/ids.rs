//! Immutable public identities and the `Ids` index maintained by component hooks (fux's pattern:
//! hooks only record; uniqueness and syntax are validated by the typed transition before
//! insertion). Caller-chosen ids are strings (`docs/model.md`, TASKS.md:169); zor-allocated ids
//! are counters.

use bevy_ecs::lifecycle::HookContext;
use bevy_ecs::prelude::*;
use bevy_ecs::world::DeferredWorld;
use bevy_platform::collections::HashMap;
use bevy_reflect::prelude::*;
use serde::{Deserialize, Serialize};

/// Caller-chosen ids: `1..=64` ASCII `[A-Za-z0-9_-]` (TASKS.md:169).
pub const MAX_ID_LEN: usize = 64;

pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID_LEN
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

macro_rules! id_impls {
    ($name:ident, $field:ident) => {
        impl $name {
            fn on_insert(world: DeferredWorld, ctx: HookContext) {
                on_insert_id::<Self>(world, ctx);
            }
            fn on_discard(world: DeferredWorld, ctx: HookContext) {
                on_discard_id::<Self>(world, ctx);
            }
        }

        impl Indexed for $name {
            fn map(ids: &mut Ids) -> &mut HashMap<Self, Entity> {
                &mut ids.$field
            }
        }

        impl core::fmt::Display for $name {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}

macro_rules! numeric_id {
    ($(#[$m:meta])* $name:ident, $field:ident) => {
        $(#[$m])*
        #[derive(Component, Reflect, Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[reflect(Component, Serialize, Deserialize)]
        #[component(immutable, on_insert = Self::on_insert, on_discard = Self::on_discard)]
        #[serde(transparent)]
        pub struct $name(pub u64);
        id_impls!($name, $field);
    };
}

macro_rules! string_id {
    ($(#[$m:meta])* $name:ident, $field:ident) => {
        $(#[$m])*
        #[derive(Component, Reflect, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[reflect(Component, Serialize, Deserialize)]
        #[component(immutable, on_insert = Self::on_insert, on_discard = Self::on_discard)]
        #[serde(transparent)]
        pub struct $name(pub String);
        id_impls!($name, $field);

        impl $name {
            /// A validated id (`valid_id`).
            pub fn new(id: &str) -> Option<Self> {
                valid_id(id).then(|| Self(id.to_owned()))
            }
        }

        impl core::borrow::Borrow<str> for $name {
            fn borrow(&self) -> &str {
                &self.0
            }
        }
    };
}

string_id!(
    /// Caller-chosen task id (TASKS.md:169).
    TaskId,
    tasks
);
numeric_id!(
    /// zor-allocated attempt number.
    AttemptId,
    attempts
);
string_id!(
    /// A prompt's operation id (TASKS.md:176); shares the operation namespace with `OperationId`.
    PromptId,
    prompts
);
string_id!(
    /// Operation id of a durable intent record (launch, stop, resume, handoff, group step).
    OperationId,
    operations
);
string_id!(
    /// Check execution id (CHECKS.md:17).
    CheckId,
    checks
);
numeric_id!(
    /// zor-allocated result number.
    ResultId,
    results
);
string_id!(SourceId, sources);
string_id!(ArtifactId, artifacts);
string_id!(GroupId, groups);
string_id!(WorktreeId, worktrees);
string_id!(
    /// Stable machine id, unchanged by rename (multi-machine-supervision.md:50).
    MachineId,
    machines
);
string_id!(
    /// A fux server name as published by its descriptor.
    ServiceId,
    services
);
numeric_id!(ObservedAgentId, agents);
string_id!(PluginId, plugins);
string_id!(PluginActionId, plugin_actions);

pub trait Indexed: Component + Clone + Eq + core::hash::Hash + Sized {
    fn map(ids: &mut Ids) -> &mut HashMap<Self, Entity>;
}

/// Index from public ids to entities; equals the set of live id components (invariant 1).
#[derive(Resource, Default, Clone, Debug)]
pub struct Ids {
    pub tasks: HashMap<TaskId, Entity>,
    pub attempts: HashMap<AttemptId, Entity>,
    pub prompts: HashMap<PromptId, Entity>,
    pub operations: HashMap<OperationId, Entity>,
    pub checks: HashMap<CheckId, Entity>,
    pub results: HashMap<ResultId, Entity>,
    pub sources: HashMap<SourceId, Entity>,
    pub artifacts: HashMap<ArtifactId, Entity>,
    pub groups: HashMap<GroupId, Entity>,
    pub worktrees: HashMap<WorktreeId, Entity>,
    pub machines: HashMap<MachineId, Entity>,
    pub services: HashMap<ServiceId, Entity>,
    pub agents: HashMap<ObservedAgentId, Entity>,
    pub plugins: HashMap<PluginId, Entity>,
    pub plugin_actions: HashMap<PluginActionId, Entity>,
    next_attempt: u64,
    next_result: u64,
    next_agent: u64,
}

impl Ids {
    pub fn get<T: Indexed>(&mut self, id: &T) -> Option<Entity> {
        T::map(self).get(id).copied()
    }
    pub fn task(&self, id: &str) -> Option<Entity> {
        self.tasks.get(id).copied()
    }
    pub fn attempt(&self, id: AttemptId) -> Option<Entity> {
        self.attempts.get(&id).copied()
    }
    pub fn prompt(&self, id: &str) -> Option<Entity> {
        self.prompts.get(id).copied()
    }
    pub fn operation(&self, id: &str) -> Option<Entity> {
        self.operations.get(id).copied()
    }
    pub fn check(&self, id: &str) -> Option<Entity> {
        self.checks.get(id).copied()
    }
    pub fn source(&self, id: &str) -> Option<Entity> {
        self.sources.get(id).copied()
    }
    pub fn artifact(&self, id: &str) -> Option<Entity> {
        self.artifacts.get(id).copied()
    }
    pub fn group(&self, id: &str) -> Option<Entity> {
        self.groups.get(id).copied()
    }
    pub fn worktree(&self, id: &str) -> Option<Entity> {
        self.worktrees.get(id).copied()
    }
    pub fn machine(&self, id: &str) -> Option<Entity> {
        self.machines.get(id).copied()
    }
    /// Whether an operation id is taken in either namespace (prompt or operation records).
    pub fn operation_taken(&self, id: &str) -> bool {
        self.prompts.contains_key(id) || self.operations.contains_key(id)
    }
    pub fn allocate_attempt(&mut self) -> AttemptId {
        self.next_attempt += 1;
        AttemptId(self.next_attempt)
    }
    pub fn allocate_result(&mut self) -> ResultId {
        self.next_result += 1;
        ResultId(self.next_result)
    }
    pub fn allocate_agent(&mut self) -> ObservedAgentId {
        self.next_agent += 1;
        ObservedAgentId(self.next_agent)
    }
    /// After a restore: counters continue above every id the document carried.
    pub fn bump_counters(&mut self) {
        self.next_attempt = self
            .attempts
            .keys()
            .map(|a| a.0)
            .max()
            .unwrap_or(0)
            .max(self.next_attempt);
        self.next_result = self
            .results
            .keys()
            .map(|r| r.0)
            .max()
            .unwrap_or(0)
            .max(self.next_result);
    }
}

/// Records the id. An id already mapped to a *different live* entity keeps its mapping, so an
/// accidental duplicate is visible to `check_invariants` instead of silently displacing the first.
fn on_insert_id<T: Indexed>(mut world: DeferredWorld, ctx: HookContext) {
    let Some(id) = world.get::<T>(ctx.entity).cloned() else {
        return;
    };
    let existing = {
        let mut ids = world.resource_mut::<Ids>();
        T::map(&mut ids).get(&id).copied()
    };
    if let Some(other) = existing
        && other != ctx.entity
        && world.get_entity(other).is_ok()
    {
        return;
    }
    let mut ids = world.resource_mut::<Ids>();
    T::map(&mut ids).insert(id, ctx.entity);
}

fn on_discard_id<T: Indexed>(mut world: DeferredWorld, ctx: HookContext) {
    let Some(id) = world.get::<T>(ctx.entity).cloned() else {
        return;
    };
    let mut ids = world.resource_mut::<Ids>();
    let map = T::map(&mut ids);
    if map.get(&id) == Some(&ctx.entity) {
        map.remove(&id);
    }
}
