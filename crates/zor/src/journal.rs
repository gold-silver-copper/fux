//! Durability (prompt 4.1, `docs/model.md` "Journal and archive"): the journal is the reflected
//! snapshot of the allowlisted subgraph, extracted with `DynamicWorldBuilder::deny_all()` plus
//! explicit allows, bounded at extraction (bytes and record counts), written atomically to
//! `<state_dir>/journal.scn.ron` (temp + fsync + rename + directory sync, TASKS.md:427-433),
//! restored at startup into an inert World, validated, then rebuilt with fresh entities.
//!
//! Archive lifecycle: closed tasks older than `Limits.archive_after_ms` move with their
//! subgraph into `<state_dir>/archive/<date>.scn.ron` (read-only, merged when the date's file
//! exists). A task is never archived while any of its operations, attempts or prompts is
//! `Uncertain` or any of its checks is unresolved.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use bevy_app::prelude::*;
use bevy_asset::AssetServer;
use bevy_ecs::entity::EntityHashMap;
use bevy_ecs::error::BevyError;
use bevy_ecs::prelude::*;
use bevy_ecs::reflect::AppTypeRegistry;
use bevy_ecs::relationship::RelationshipTarget;
use bevy_log::{error, info, warn};
use bevy_reflect::prelude::*;
use bevy_world_serialization::serde::WorldDeserializer;
use bevy_world_serialization::{DynamicWorld, DynamicWorldBuilder};
use serde::de::DeserializeSeed as _;

use crate::model::invariants::check_invariants;
use crate::model::*;

pub const JOURNAL_FILE: &str = "journal.scn.ron";
pub const ARCHIVE_DIR: &str = "archive";

#[derive(Debug)]
pub enum JournalError {
    Io(PathBuf, std::io::Error),
    /// Serialized size or a record count exceeds the bound; the previous generation stays.
    OverBound {
        what: &'static str,
        count: usize,
        limit: usize,
    },
    Parse(String),
    /// A component outside the journal vocabulary or a resource.
    NotAllowed(String),
    /// The restored graph violates a structural invariant `(number, description)`.
    Invariant(u8, String),
    NoAssetServer,
}

impl core::fmt::Display for JournalError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Io(path, error) => write!(f, "{}: {error}", path.display()),
            Self::OverBound { what, count, limit } => {
                write!(f, "journal {what}: {count} exceeds {limit}")
            }
            Self::Parse(reason) => write!(f, "journal parse: {reason}"),
            Self::NotAllowed(what) => write!(f, "journal: {what} is not allowed"),
            Self::Invariant(n, reason) => write!(f, "journal invariant {n}: {reason}"),
            Self::NoAssetServer => f.write_str("journal: no AssetServer to deserialize with"),
        }
    }
}

impl core::error::Error for JournalError {}

/// Journal location, bounds and write state.
#[derive(Resource, Debug)]
pub struct Journal {
    pub path: PathBuf,
    pub archive_dir: PathBuf,
    /// Something in the allowlisted subgraph changed since the last commit.
    dirty: bool,
    /// Set when the committed file could not be restored: nothing is ever written over it
    /// (TASKS.md:437-442 "corrupt committed state preserved").
    frozen: bool,
    /// Last archive sweep, `Clock` milliseconds.
    last_sweep_ms: u64,
}

/// The journal generation the live document was written as; the only resource in the
/// document. Restored into `Generation`, so generations (and `CreatedGeneration` ordering,
/// CHECKS.md:41) never regress across incarnations.
#[derive(Resource, Reflect, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[reflect(Resource)]
pub struct GenerationCounter(pub u64);

/// Between two sweeps for archivable tasks.
const SWEEP_INTERVAL_MS: u64 = 60_000;

impl Journal {
    pub fn new(state_dir: &Path) -> Self {
        Self {
            path: state_dir.join(JOURNAL_FILE),
            archive_dir: state_dir.join(ARCHIVE_DIR),
            dirty: false,
            frozen: false,
            last_sweep_ms: 0,
        }
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn is_frozen(&self) -> bool {
        self.frozen
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }
}

// ---------------------------------------------------------------------------------------------
// Vocabulary
// ---------------------------------------------------------------------------------------------

macro_rules! vocabulary {
    ($($ty:ty),* $(,)?) => {
        fn allow(builder: DynamicWorldBuilder<'_>) -> DynamicWorldBuilder<'_> {
            builder $(.allow_component::<$ty>())*
        }

        fn allowed(type_id: core::any::TypeId) -> bool {
            [$(core::any::TypeId::of::<$ty>()),*].contains(&type_id)
        }
    };
}

vocabulary!(
    Task,
    Attempt,
    Prompt,
    Operation,
    Check,
    CheckResult,
    Source,
    Artifact,
    Group,
    Worktree,
    Machine,
    TaskId,
    AttemptId,
    PromptId,
    OperationId,
    CheckId,
    ResultId,
    SourceId,
    ArtifactId,
    GroupId,
    WorktreeId,
    MachineId,
    AttemptOf,
    Attempts,
    PromptOf,
    Prompts,
    OperationOf,
    Operations,
    ArtifactOf,
    Artifacts,
    CheckOf,
    TaskChecks,
    CheckOn,
    Checks,
    SourceOf,
    Sources,
    ResultOf,
    Results,
    MemberOf,
    Members,
    OwnedWorktree,
    Worktrees,
    After,
    Seal,
    Title,
    CreatedMs,
    ClosedMs,
    Location,
    TaskState,
    StopRequested,
    Ownership,
    PaneHandle,
    LaunchMarker,
    AttemptState,
    FinalEvidence,
    Problem,
    Uncertain,
    Lost,
    NeedsInput,
    PromptText,
    Deadline,
    ReportToken,
    Delivery,
    WaitState,
    Receipt,
    ResponseEvent,
    Binding,
    ArmInputStarted,
    OperationKind,
    OperationPhase,
    CheckCommand,
    Requirement,
    Required,
    CreatedGeneration,
    CheckState,
    Verdict,
    OutputTail,
    SourceRevision,
    ArtifactPath,
    ArtifactState,
    GroupIntent,
    Concurrency,
    WorktreeSpec,
    WorktreeState,
    MachineName,
    ControlBinding,
    ProducerLifetime,
    crate::groups::Cursor,
    crate::groups::MemberPrompt,
    crate::worktrees::ForceRemoval,
    crate::checks::CheckPolicy,
    crate::checks::ArtifactPolicy,
    crate::checks::CaptureRequests,
    crate::checks::CheckCwd,
    crate::checks::CapturedBy,
    crate::checks::ArtifactBytes,
    crate::checks::SourceState,
    crate::lifecycle::LaunchTemplate,
    crate::providers::Provider,
);

/// Every entity the journal persists.
fn persisted(world: &mut World) -> Vec<Entity> {
    world
        .query_filtered::<Entity, Or<(
            With<Task>,
            With<Attempt>,
            With<Prompt>,
            With<Operation>,
            With<Check>,
            With<CheckResult>,
            With<Source>,
            With<Artifact>,
            With<Group>,
            With<Worktree>,
            With<Machine>,
        )>>()
        .iter(world)
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Snapshot
// ---------------------------------------------------------------------------------------------

/// The allowlisted subgraph of `entities` as a RON document, refused over any bound. The live
/// journal (`counter`) also carries [`GenerationCounter`]; archives never do.
pub fn snapshot(
    world: &mut World,
    entities: &[Entity],
    counter: bool,
) -> Result<String, JournalError> {
    let limits = world.resource::<Limits>().clone();
    check_counts(world, &limits)?;
    let registry = world.resource::<AppTypeRegistry>().clone();
    let registry = registry.read();
    let mut builder = allow(DynamicWorldBuilder::from_world(world, &registry).deny_all())
        .extract_entities(entities.iter().copied())
        .remove_empty_entities();
    if counter {
        builder = builder
            .allow_resource::<GenerationCounter>()
            .extract_resources();
    }
    let dynamic = builder.build();
    let document = dynamic
        .serialize(&registry)
        .map_err(|e| JournalError::Parse(e.to_string()))?;
    if document.len() > limits.journal_bytes {
        return Err(JournalError::OverBound {
            what: "bytes",
            count: document.len(),
            limit: limits.journal_bytes,
        });
    }
    Ok(document)
}

fn check_counts(world: &mut World, limits: &Limits) -> Result<(), JournalError> {
    macro_rules! within {
        ($marker:ty, $limit:expr, $what:literal) => {
            let count = world
                .query_filtered::<(), With<$marker>>()
                .iter(world)
                .count();
            if count > $limit {
                return Err(JournalError::OverBound {
                    what: $what,
                    count,
                    limit: $limit,
                });
            }
        };
    }
    within!(Task, limits.tasks, "tasks");
    within!(Attempt, limits.attempts, "attempts");
    within!(Prompt, limits.prompts, "prompts");
    within!(Operation, limits.operations, "operations");
    within!(Check, limits.checks, "checks");
    within!(Source, limits.sources, "sources");
    within!(Artifact, limits.artifacts, "artifacts");
    within!(Group, limits.groups, "groups");
    within!(Worktree, limits.worktrees, "worktrees");
    within!(Machine, limits.machines, "machines");
    Ok(())
}

/// Writes atomically: temp file (0600) in the same directory, fsync, rename, directory fsync.
pub fn write_atomic(path: &Path, document: &str, mode: u32) -> Result<(), JournalError> {
    let io = |error| JournalError::Io(path.to_path_buf(), error);
    let parent = path
        .parent()
        .ok_or_else(|| io(std::io::Error::other("journal path has no parent")))?;
    fs::create_dir_all(parent).map_err(io)?;
    let temporary = parent.join(format!(
        ".{}-{}.tmp",
        path.file_name()
            .map_or("journal".into(), |n| n.to_string_lossy()),
        std::process::id()
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(io)?;
    let written = (|| {
        file.write_all(document.as_bytes())?;
        file.sync_all()?;
        fs::set_permissions(&temporary, fs::Permissions::from_mode(mode))?;
        fs::rename(&temporary, path)?;
        fs::File::open(parent)?.sync_all()
    })();
    if let Err(error) = written {
        let _ = fs::remove_file(&temporary);
        return Err(io(error));
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Restore
// ---------------------------------------------------------------------------------------------

/// Parses a document and validates its vocabulary in an inert World; returns the parsed
/// document and the scratch World (whose `Ids` were filled by the id hooks).
fn parse(world: &World, document: &str) -> Result<(DynamicWorld, World), JournalError> {
    let registry = world.resource::<AppTypeRegistry>().clone();
    let mut assets = world
        .get_resource::<AssetServer>()
        .cloned()
        .ok_or(JournalError::NoAssetServer)?;
    let mut de = ron::de::Deserializer::from_str(document)
        .map_err(|e| JournalError::Parse(e.to_string()))?;
    let dynamic = WorldDeserializer {
        type_registry: &registry.read(),
        load_from_path: &mut assets,
    }
    .deserialize(&mut de)
    .map_err(|e| JournalError::Parse(de.span_error(e).to_string()))?;
    for resource in &dynamic.resources {
        let info = resource
            .get_represented_type_info()
            .ok_or_else(|| JournalError::NotAllowed(resource.reflect_type_path().to_owned()))?;
        if info.type_id() != core::any::TypeId::of::<GenerationCounter>() {
            return Err(JournalError::NotAllowed(info.type_path().to_owned()));
        }
    }
    for entity in &dynamic.entities {
        for component in &entity.components {
            let info = component.get_represented_type_info().ok_or_else(|| {
                JournalError::NotAllowed(component.reflect_type_path().to_owned())
            })?;
            if !allowed(info.type_id()) {
                return Err(JournalError::NotAllowed(info.type_path().to_owned()));
            }
        }
    }
    let mut scratch = World::new();
    scratch.insert_resource(registry.clone());
    scratch.init_resource::<GenerationCounter>();
    scratch.init_resource::<Ids>();
    scratch.insert_resource(world.resource::<Limits>().clone());
    let mut map = EntityHashMap::<Entity>::default();
    dynamic
        .write_to_world_with(&mut scratch, &mut map, &registry.read())
        .map_err(|e| JournalError::Parse(e.to_string()))?;
    check_invariants(&mut scratch).map_err(|(n, reason)| JournalError::Invariant(n, reason))?;
    Ok((dynamic, scratch))
}

/// Reads a document of at most the journal byte bound (+1 to detect overflow).
fn read_bounded(path: &Path, limit: usize) -> Result<Option<String>, JournalError> {
    use std::io::Read;
    let io = |error| JournalError::Io(path.to_path_buf(), error);
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io(error)),
    };
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(io)?;
    if bytes.len() > limit {
        return Err(JournalError::OverBound {
            what: "bytes",
            count: bytes.len(),
            limit,
        });
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|e| JournalError::Parse(e.to_string()))
}

/// Restores `path` into `world` with fresh entities and continues the journal generation from
/// the document's counter. Returns how many entities were rebuilt; `0` when there is no file.
/// The World is untouched on any error.
pub fn restore(world: &mut World, path: &Path) -> Result<usize, JournalError> {
    let limit = world.resource::<Limits>().journal_bytes;
    let Some(document) = read_bounded(path, limit)? else {
        return Ok(0);
    };
    let (dynamic, scratch) = parse(world, &document)?;
    let registry = world.resource::<AppTypeRegistry>().clone();
    let mut map = EntityHashMap::<Entity>::default();
    dynamic
        .write_to_world_with(world, &mut map, &registry.read())
        .map_err(|e| JournalError::Parse(e.to_string()))?;
    world.resource_mut::<Ids>().bump_counters();
    check_invariants(world).map_err(|(n, reason)| JournalError::Invariant(n, reason))?;
    let counter = scratch.resource::<GenerationCounter>().0;
    world.resource_mut::<Generation>().0 = counter;
    world.insert_resource(GenerationCounter(counter));
    Ok(map.len())
}

// ---------------------------------------------------------------------------------------------
// Archive
// ---------------------------------------------------------------------------------------------

/// The task's whole persisted subgraph: attempts, prompts, operations, artifacts, checks,
/// results, sources, worktrees.
pub fn subgraph(world: &World, task: Entity) -> Vec<Entity> {
    fn targets<T: RelationshipTarget>(world: &World, entity: Entity, out: &mut Vec<Entity>) {
        if let Some(targets) = world.get::<T>(entity) {
            out.extend(targets.iter());
        }
    }
    let mut out = vec![task];
    let mut attempts = Vec::new();
    targets::<Attempts>(world, task, &mut attempts);
    for attempt in &attempts {
        targets::<Prompts>(world, *attempt, &mut out);
        targets::<Operations>(world, *attempt, &mut out);
        targets::<Artifacts>(world, *attempt, &mut out);
    }
    out.append(&mut attempts);
    let mut checks = Vec::new();
    targets::<TaskChecks>(world, task, &mut checks);
    for check in &checks {
        targets::<Results>(world, *check, &mut out);
    }
    out.append(&mut checks);
    targets::<Sources>(world, task, &mut out);
    targets::<Worktrees>(world, task, &mut out);
    out.sort_unstable();
    out.dedup();
    out
}

/// Whether a closed task may leave the live journal: nothing uncertain, no unresolved check.
pub fn archivable(world: &World, task: Entity) -> bool {
    subgraph(world, task).into_iter().all(|entity| {
        world.get::<Uncertain>(entity).is_none()
            && world
                .get::<OperationPhase>(entity)
                .is_none_or(|p| *p != OperationPhase::Uncertain)
            && world
                .get::<Delivery>(entity)
                .is_none_or(|d| *d != Delivery::Uncertain)
            && world
                .get::<CheckState>(entity)
                .is_none_or(|s| matches!(s, CheckState::Passed | CheckState::Failed))
    })
}

/// Closed tasks whose `ClosedMs` is older than `archive_after` and that are archivable.
pub fn archive_candidates(world: &mut World, now_ms: u64, archive_after: u64) -> Vec<Entity> {
    let closed: Vec<Entity> = world
        .query_filtered::<(Entity, &TaskState, &ClosedMs), With<Task>>()
        .iter(world)
        .filter(|(_, state, closed)| {
            state.is_closed() && closed.0.saturating_add(archive_after) <= now_ms
        })
        .map(|(e, _, _)| e)
        .collect();
    closed
        .into_iter()
        .filter(|task| archivable(world, *task))
        .collect()
}

/// Moves `tasks` with their subgraphs into `<archive_dir>/<date>.scn.ron`, merging into the
/// date's file when it exists, then despawns them. Returns the archive path.
pub fn archive(
    world: &mut World,
    archive_dir: &Path,
    tasks: &[Entity],
    now_ms: u64,
) -> Result<PathBuf, JournalError> {
    let path = archive_dir.join(format!("{}.scn.ron", civil_date(now_ms)));
    let mut entities: Vec<Entity> = tasks
        .iter()
        .flat_map(|task| subgraph(world, *task))
        .collect();
    entities.sort_unstable();
    entities.dedup();
    let limit = world.resource::<Limits>().journal_bytes;
    let document = match read_bounded(&path, limit)? {
        None => snapshot(world, &entities, false)?,
        Some(existing) => {
            // Merge: the existing archive plus the new subgraph, both rebuilt into one inert
            // World so entity ids never collide.
            let (dynamic, _) = parse(world, &existing)?;
            let registry = world.resource::<AppTypeRegistry>().clone();
            let fresh = allow(DynamicWorldBuilder::from_world(world, &registry.read()).deny_all())
                .extract_entities(entities.iter().copied())
                .remove_empty_entities()
                .build();
            let mut merged = World::new();
            merged.insert_resource(registry.clone());
            merged.init_resource::<Ids>();
            merged.insert_resource(world.resource::<Limits>().clone());
            let mut map = EntityHashMap::<Entity>::default();
            dynamic
                .write_to_world_with(&mut merged, &mut map, &registry.read())
                .map_err(|e| JournalError::Parse(e.to_string()))?;
            let mut map = EntityHashMap::<Entity>::default();
            fresh
                .write_to_world_with(&mut merged, &mut map, &registry.read())
                .map_err(|e| JournalError::Parse(e.to_string()))?;
            let all = persisted(&mut merged);
            snapshot(&mut merged, &all, false)?
        }
    };
    if path.exists() {
        // Archives are read-only; the merge rewrites them through a fresh temp file.
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .map_err(|e| JournalError::Io(path.clone(), e))?;
    }
    write_atomic(&path, &document, 0o400)?;
    for entity in entities {
        world.despawn(entity);
    }
    Ok(path)
}

/// Looks a task up in the archives read-only: the archive file name and the task's state.
pub fn find_archived(
    world: &World,
    archive_dir: &Path,
    task: &str,
) -> Result<Option<(String, Option<String>)>, JournalError> {
    let io = |error| JournalError::Io(archive_dir.to_path_buf(), error);
    let entries = match fs::read_dir(archive_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io(error)),
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.to_string_lossy().ends_with(".scn.ron"))
        .collect();
    files.sort_unstable();
    let limit = world.resource::<Limits>().journal_bytes;
    for file in files.into_iter().rev() {
        let Some(document) = read_bounded(&file, limit)? else {
            continue;
        };
        let (_, scratch) = parse(world, &document)?;
        if let Some(entity) = scratch.resource::<Ids>().task(task) {
            let state = scratch.get::<TaskState>(entity).map(|s| format!("{s:?}"));
            let name = file
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            return Ok(Some((name, state)));
        }
    }
    Ok(None)
}

/// `YYYY-MM-DD` of a Unix millisecond timestamp (proleptic Gregorian, UTC).
pub fn civil_date(ms: u64) -> String {
    let days = i64::try_from(ms / 86_400_000).unwrap_or(0);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

// ---------------------------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------------------------

/// `Startup`: restore; `PostUpdate`/`Phase::Journal`: mark dirty, archive, commit.
pub struct JournalPlugin {
    pub state_dir: PathBuf,
}

impl Plugin for JournalPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(Journal::new(&self.state_dir))
            .init_resource::<GenerationCounter>()
            .register_type::<GenerationCounter>()
            .add_systems(Startup, restore_at_startup)
            .add_systems(
                PostUpdate,
                (mark_dirty, sweep, commit).chain().in_set(Phase::Journal),
            );
    }
}

fn restore_at_startup(world: &mut World) -> Result<(), BevyError> {
    let path = world.resource::<Journal>().path.clone();
    match restore(world, &path) {
        Ok(0) => Ok(()),
        Ok(entities) => {
            info!(
                "journal: restored {entities} entities from {}",
                path.display()
            );
            Ok(())
        }
        Err(e) => {
            world.resource_mut::<Journal>().frozen = true;
            error!(
                "journal: {} could not be restored ({e}); it is preserved and never overwritten",
                path.display()
            );
            Err(BevyError::from(e))
        }
    }
}

type Lifecycle = (
    Changed<TaskState>,
    Changed<AttemptState>,
    Changed<Delivery>,
    Changed<WaitState>,
    Changed<OperationPhase>,
    Changed<CheckState>,
    Changed<WorktreeState>,
    Changed<ArtifactState>,
    Changed<GroupIntent>,
    Changed<FinalEvidence>,
    Changed<Receipt>,
    Changed<Problem>,
    Changed<OutputTail>,
    Changed<Observation>,
    // `Or` tuples hold at most 15 filters: later owners nest theirs.
    Or<(
        Changed<crate::groups::Cursor>,
        Changed<crate::checks::CheckPolicy>,
        Changed<crate::checks::ArtifactPolicy>,
        Changed<crate::checks::SourceState>,
        Changed<PaneHandle>,
    )>,
);
type Presence = (
    Added<Task>,
    Added<Attempt>,
    Added<Prompt>,
    Added<Operation>,
    Added<Check>,
    Added<CheckResult>,
    Added<Source>,
    Added<Artifact>,
    Added<Group>,
    Added<Worktree>,
    Added<Machine>,
    Added<Seal>,
    Added<Uncertain>,
    Added<Lost>,
    Or<(Added<StopRequested>, Added<crate::worktrees::ForceRemoval>)>,
);

/// Anything in the allowlisted subgraph changed, was added or was removed.
fn mark_dirty(
    mut journal: ResMut<Journal>,
    lifecycle: Query<(), Or<Lifecycle>>,
    presence: Query<(), Or<Presence>>,
    (
        mut removed_task,
        mut removed_attempt,
        mut removed_prompt,
        mut removed_uncertain,
        mut removed_lost,
        mut removed_needs,
        mut removed_stop,
    ): (
        RemovedComponents<Task>,
        RemovedComponents<Attempt>,
        RemovedComponents<Prompt>,
        RemovedComponents<Uncertain>,
        RemovedComponents<Lost>,
        RemovedComponents<NeedsInput>,
        RemovedComponents<StopRequested>,
    ),
) {
    let removed = removed_task.read().next().is_some()
        | removed_attempt.read().next().is_some()
        | removed_prompt.read().next().is_some()
        | removed_uncertain.read().next().is_some()
        | removed_lost.read().next().is_some()
        | removed_needs.read().next().is_some()
        | removed_stop.read().next().is_some();
    if removed || !lifecycle.is_empty() || !presence.is_empty() {
        journal.dirty = true;
    }
}

/// Once a minute: archive closed tasks past `archive_after_ms`.
fn sweep(world: &mut World) {
    let now = world.resource::<Clock>().now_ms;
    {
        let journal = world.resource::<Journal>();
        if journal.frozen || now < journal.last_sweep_ms.saturating_add(SWEEP_INTERVAL_MS) {
            return;
        }
    }
    world.resource_mut::<Journal>().last_sweep_ms = now;
    let after = world.resource::<Limits>().archive_after_ms;
    let candidates = archive_candidates(world, now, after);
    if candidates.is_empty() {
        return;
    }
    let dir = world.resource::<Journal>().archive_dir.clone();
    match archive(world, &dir, &candidates, now) {
        Ok(path) => {
            info!(
                "journal: archived {} tasks into {}",
                candidates.len(),
                path.display()
            );
            world.resource_mut::<Journal>().dirty = true;
        }
        Err(e) => warn!("journal: archive refused: {e}"),
    }
}

/// Writes the journal when dirty; a violated invariant or an exceeded bound keeps the previous
/// generation (TASKS.md:427-433).
fn commit(world: &mut World) {
    {
        let journal = world.resource::<Journal>();
        if !journal.dirty || journal.frozen {
            return;
        }
    }
    if let Err((n, reason)) = check_invariants(world) {
        error!("journal: invariant {n} violated, not committing: {reason}");
        return;
    }
    let entities = persisted(world);
    let path = world.resource::<Journal>().path.clone();
    let next = world.resource::<Generation>().0 + 1;
    world.resource_mut::<GenerationCounter>().0 = next;
    let result =
        snapshot(world, &entities, true).and_then(|document| write_atomic(&path, &document, 0o600));
    match result {
        Ok(()) => {
            world.resource_mut::<Generation>().0 = next;
            world.resource_mut::<Journal>().dirty = false;
        }
        Err(e) => error!("journal: commit refused: {e}"),
    }
}

impl Journal {
    /// Looks a task up in the archives (read-only).
    pub fn find_archived(
        &self,
        world: &World,
        task: &str,
    ) -> Result<Option<(String, Option<String>)>, JournalError> {
        find_archived(world, &self.archive_dir, task)
    }
}
