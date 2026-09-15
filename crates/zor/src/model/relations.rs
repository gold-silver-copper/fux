//! Relationships (`docs/model.md`). The relationship side is the source of truth; the reverse
//! side is an index and is never edited directly. Children of a task/attempt/check are
//! `linked_spawn`: despawning the owner despawns its subgraph, which the archive relies on.

use bevy_ecs::prelude::*;
use bevy_reflect::prelude::*;

macro_rules! relation {
    ($(#[$m:meta])* $rel:ident => $target:ident $(, $linked:ident)?) => {
        $(#[$m])*
        #[derive(Component, Reflect, Clone, Copy, Debug, PartialEq, Eq)]
        #[reflect(Component)]
        #[relationship(relationship_target = $target)]
        pub struct $rel(pub Entity);

        #[derive(Component, Reflect, Debug)]
        #[reflect(Component)]
        #[relationship_target(relationship = $rel $(, $linked)?)]
        pub struct $target(Vec<Entity>);
    };
}

relation!(
    /// An attempt associates one task with one session/pane (TASKS.md:66-67).
    AttemptOf => Attempts, linked_spawn
);
relation!(
    /// A prepared prompt operation targets one attempt (TASKS.md:176).
    PromptOf => Prompts, linked_spawn
);
relation!(
    /// A durable intent record (launch, stop, resume, handoff, group step) of one attempt.
    OperationOf => Operations, linked_spawn
);
relation!(
    /// Retained artifact bytes of one attempt (ARTIFACTS.md:10-12).
    ArtifactOf => Artifacts, linked_spawn
);
relation!(
    /// A check execution belongs to one task (CHECKS.md:14).
    CheckOf => TaskChecks, linked_spawn
);
relation!(
    /// A source-bound check runs against one retained snapshot (CHECKS.md:82).
    CheckOn => Checks
);
relation!(
    /// A retained tree snapshot of one task (SOURCES.md:10).
    SourceOf => Sources, linked_spawn
);
relation!(
    /// Evidence of a finished or uncertain check.
    ResultOf => Results, linked_spawn
);
relation!(
    /// Group members are operations (GROUPS.md:102).
    MemberOf => Members
);
relation!(
    /// A worktree zor created for one task (WORKTREES.md:23-24).
    OwnedWorktree => Worktrees, linked_spawn
);
relation!(
    /// An observed agent or a service known through one machine's control binding.
    Bound => BoundAgents
);
relation!(
    /// A manifest action of one plugin.
    ActionOf => Actions, linked_spawn
);
relation!(
    /// From a reflected projection entity to the model entity it mirrors; `linked_spawn`
    /// despawns the projection with its model.
    Mirrors => Projections, linked_spawn
);

/// Prerequisite members (`--after`) of a group member; must name members of the same group
/// (GROUPS.md:102-105).
#[derive(Component, Reflect, Clone, Debug, Default, PartialEq, Eq)]
#[reflect(Component)]
pub struct After(#[entities] pub Vec<Entity>);

/// The verification seal on a task (RESULTS.md:69-70): the selected source, checks and artifacts.
#[derive(Component, Reflect, Clone, Debug, PartialEq, Eq)]
#[reflect(Component)]
pub struct Seal {
    #[entities]
    pub source: Entity,
    #[entities]
    pub checks: Vec<Entity>,
    #[entities]
    pub artifacts: Vec<Entity>,
    pub sealed_ms: u64,
    pub generation: u64,
}
