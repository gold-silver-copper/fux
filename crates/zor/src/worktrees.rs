//! Owned worktrees (WORKTREES.md; `docs/model.md` invariants 9, 25): intent-before-git creation
//! `Allocating → Prepared → Creating → Ready | Uncertain` and removal `Ready → Removing →
//! Removed`.
//!
//! Journal-first is structural: every state is written by a request or by [`advance`] in
//! `Phase::Lifecycle`, journaled in `Phase::Journal`, and only acted on in a *later* update
//! (filesystem steps) or drained in `Phase::Effects` after the journal (git steps). The
//! in-flight table [`InFlight`] is runtime-only: after a restart nothing is in flight, so a
//! restored `Creating` becomes `Uncertain` (its add may have run), restored `Allocating`/
//! `Prepared` wait for an explicit [`reconcile`], and a restored `Removing` never re-runs
//! `git worktree remove`. `Removed` is written only once the checkout path and the repository
//! registration are both absent.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use bevy_app::prelude::*;
use bevy_ecs::prelude::*;
use bevy_log::warn;
use bevy_reflect::prelude::*;
use serde::{Deserialize, Serialize};

use crate::git::{self, GitDone};
use crate::model::*;

/// Where every worktree parent lives: `<state_dir>/worktrees/<random>` (WORKTREES.md:35-36).
pub const WORKTREES_DIR: &str = "worktrees";
/// The checkout inside the reserved parent (WORKTREES.md:36).
pub const TREE: &str = "tree";

/// Retained `--force` policy of a removal (WORKTREES.md:110, 115): once committed it cannot be
/// silently replaced.
#[derive(
    Component, Reflect, Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize,
)]
#[reflect(Component, Default)]
pub struct ForceRemoval;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeError {
    Model(ModelError),
    NotFound(String),
    /// A different intent under a retained id.
    Conflict(String),
    /// The request is well-formed but not applicable in the current state.
    Refused(String),
    /// A read-only git pre-check failed or could not run.
    Git(String),
}

impl core::fmt::Display for WorktreeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Model(e) => write!(f, "{e}"),
            Self::NotFound(what) => write!(f, "{what} not found"),
            Self::Conflict(why) => write!(f, "conflict: {why}"),
            Self::Refused(why) => write!(f, "refused: {why}"),
            Self::Git(why) => write!(f, "git: {why}"),
        }
    }
}

impl core::error::Error for WorktreeError {}

impl From<ModelError> for WorktreeError {
    fn from(e: ModelError) -> Self {
        Self::Model(e)
    }
}

/// `zor/worktree.allocate` intent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeRequest {
    pub id: String,
    /// Absolute path inside a repository.
    pub repo: String,
    /// A new literal branch name (WORKTREES.md:14-15).
    pub branch: String,
    /// A revision expression resolved to a commit before any mutation (WORKTREES.md:14).
    pub base: String,
}

/// The retained record, for inspection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeRecord {
    pub id: String,
    pub task: String,
    pub state: WorktreeState,
    pub repo: String,
    pub branch: String,
    /// The pinned base commit.
    pub base: String,
    pub path: String,
    pub force: bool,
    pub problem: Option<String>,
    /// A git command or a commit wait is in progress in this incarnation.
    pub in_flight: bool,
}

// ---------------------------------------------------------------------------------------------
// Runtime state
// ---------------------------------------------------------------------------------------------

/// Root of every reserved parent, canonical.
#[derive(Resource, Debug, Clone)]
pub struct WorktreeRoot(pub PathBuf);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flight {
    /// The state written at journal `generation` must be committed before acting on it.
    Committed { after: Option<u64> },
    /// `git worktree add` is running.
    Adding { op: u64 },
    /// `git worktree list --porcelain` after a successful add.
    Listing { op: u64 },
    /// `git status --porcelain` in the new checkout.
    Status { op: u64 },
    /// `git worktree remove` is running.
    Removing { op: u64 },
    /// `git worktree list --porcelain` for an explicit or post-removal reconciliation.
    Reconciling { op: u64 },
}

/// What this incarnation is doing per worktree; never persisted.
#[derive(Resource, Default, Debug)]
pub struct InFlight(HashMap<Entity, Flight>);

pub struct WorktreesPlugin {
    pub state_dir: PathBuf,
}

impl Plugin for WorktreesPlugin {
    fn build(&self, app: &mut App) {
        let root = self.state_dir.join(WORKTREES_DIR);
        app.register_type::<ForceRemoval>()
            .insert_resource(WorktreeRoot(root))
            .init_resource::<InFlight>()
            .init_resource::<git::GitOps>()
            .add_systems(Update, advance.in_set(Phase::Lifecycle));
    }
}

// ---------------------------------------------------------------------------------------------
// Requests
// ---------------------------------------------------------------------------------------------

/// The checkout path of a worktree that still has one.
pub fn path_of(world: &World, worktree: Entity) -> Option<PathBuf> {
    let spec = world.get::<WorktreeSpec>(worktree)?;
    let state = world.get::<WorktreeState>(worktree)?;
    (*state != WorktreeState::Removed).then(|| tree_path(spec))
}

fn tree_path(spec: &WorktreeSpec) -> PathBuf {
    Path::new(&spec.parent).join(TREE)
}

pub fn inspect(world: &World, worktree: Entity) -> Option<WorktreeRecord> {
    let id = world.get::<WorktreeId>(worktree)?;
    let spec = world.get::<WorktreeSpec>(worktree)?;
    let state = *world.get::<WorktreeState>(worktree)?;
    let task = world
        .get::<OwnedWorktree>(worktree)
        .and_then(|of| world.get::<TaskId>(of.0))
        .map(|t| t.0.clone())
        .unwrap_or_default();
    Some(WorktreeRecord {
        id: id.0.clone(),
        task,
        state,
        repo: spec.repo.clone(),
        branch: spec.branch.clone(),
        base: spec.base.clone(),
        path: tree_path(spec).display().to_string(),
        force: world.get::<ForceRemoval>(worktree).is_some(),
        problem: world.get::<Problem>(worktree).map(|p| p.0.clone()),
        in_flight: world
            .get_resource::<InFlight>()
            .is_some_and(|f| f.0.contains_key(&worktree)),
    })
}

pub fn list(world: &mut World) -> Vec<WorktreeRecord> {
    let entities: Vec<Entity> = world
        .query_filtered::<Entity, With<Worktree>>()
        .iter(world)
        .collect();
    entities
        .into_iter()
        .filter_map(|e| inspect(world, e))
        .collect()
}

/// Records creation intent for `task` (WORKTREES.md:42-47). Read-only git pre-checks run here
/// (repository identity, branch absence, base resolution); the mutation is committed as
/// `Allocating` first and every later step runs in a later update. An identical retry returns
/// the record and, from `Allocating`/`Prepared`, lets creation proceed; a different plan under
/// the same id fails.
pub fn allocate(
    world: &mut World,
    task: Entity,
    request: WorktreeRequest,
) -> Result<Entity, WorktreeError> {
    if !valid_id(&request.id) {
        return Err(ModelError::InvalidId(request.id).into());
    }
    if world.get::<Task>(task).is_none() {
        return Err(ModelError::WrongKind {
            expected: "task",
            entity: task,
        }
        .into());
    }
    valid_branch(&request.branch)?;
    valid_base(&request.base)?;
    let repo = Path::new(&request.repo);
    if !repo.is_absolute() {
        return Err(WorktreeError::Refused(
            "repo must be an absolute path".into(),
        ));
    }
    let toplevel = git_line(&["rev-parse", "--show-toplevel"], repo)
        .map_err(|e| WorktreeError::Git(format!("{}: {e}", request.repo)))?;
    let toplevel = std::fs::canonicalize(&toplevel)
        .map(|p| p.display().to_string())
        .unwrap_or(toplevel);
    let base = git_line(
        &[
            "rev-parse",
            "--verify",
            &format!("{}^{{commit}}", request.base),
        ],
        Path::new(&toplevel),
    )
    .map_err(|e| WorktreeError::Refused(format!("base {:?}: {e}", request.base)))?;

    if let Some(existing) = world.resource::<Ids>().worktree(&request.id) {
        let spec = world
            .get::<WorktreeSpec>(existing)
            .ok_or_else(|| WorktreeError::NotFound(request.id.clone()))?;
        let owner = world.get::<OwnedWorktree>(existing).map(|o| o.0);
        if spec.repo != toplevel
            || spec.branch != request.branch
            || spec.base != base
            || owner != Some(task)
        {
            return Err(WorktreeError::Conflict(format!(
                "worktree {} exists with a different plan",
                request.id
            )));
        }
        let state = world
            .get::<WorktreeState>(existing)
            .copied()
            .unwrap_or_default();
        if matches!(state, WorktreeState::Allocating | WorktreeState::Prepared)
            && !world.resource::<InFlight>().0.contains_key(&existing)
        {
            // A restored intent is already committed: act on the next update.
            set_flight(world, existing, Flight::Committed { after: None });
        }
        return Ok(existing);
    }

    let branch_exists = git::run(
        0,
        &[
            "rev-parse".into(),
            "--verify".into(),
            "--quiet".into(),
            format!("refs/heads/{}", request.branch),
        ],
        Path::new(&toplevel),
    );
    if branch_exists.succeeded() {
        return Err(WorktreeError::Refused(format!(
            "branch {} already exists",
            request.branch
        )));
    }
    let root = world.resource::<WorktreeRoot>().0.clone();
    let parent = root.join(format!("wt-{}", random_suffix()?));
    let entity = spawn_worktree(
        world,
        &request.id,
        task,
        WorktreeSpec {
            repo: toplevel,
            branch: request.branch,
            base,
            parent: parent.display().to_string(),
        },
    )?;
    let generation = world.resource::<Generation>().0;
    world.resource_mut::<InFlight>().0.insert(
        entity,
        Flight::Committed {
            after: Some(generation),
        },
    );
    Ok(entity)
}

/// Records removal intent (WORKTREES.md:77-81, 110): a ready checkout whose task has no
/// unresolved check and no attempt outside `Finished` (invariant 9). A retry with the same
/// force policy returns; a different policy fails.
pub fn remove(world: &mut World, worktree: Entity, force: bool) -> Result<(), WorktreeError> {
    let state = *world
        .get::<WorktreeState>(worktree)
        .ok_or_else(|| WorktreeError::NotFound(format!("worktree {worktree}")))?;
    match state {
        WorktreeState::Removed => return Ok(()),
        WorktreeState::Removing => {
            let retained = world.get::<ForceRemoval>(worktree).is_some();
            return if retained == force {
                Ok(())
            } else {
                Err(WorktreeError::Conflict(
                    "removal is retained with a different force policy".into(),
                ))
            };
        }
        WorktreeState::Ready => {}
        other => {
            return Err(WorktreeError::Refused(format!(
                "worktree is {other:?}, not ready"
            )));
        }
    }
    let task = world
        .get::<OwnedWorktree>(worktree)
        .map(|o| o.0)
        .ok_or_else(|| WorktreeError::NotFound("owning task".into()))?;
    let path = tree_path(
        world
            .get::<WorktreeSpec>(worktree)
            .ok_or_else(|| WorktreeError::NotFound("worktree spec".into()))?,
    );
    if let Some(check) = unresolved_check(world, task) {
        return Err(WorktreeError::Refused(format!(
            "check {check} of the owning task is unresolved"
        )));
    }
    if let Some(attempt) = live_attempt_inside(world, task, &path) {
        return Err(WorktreeError::Refused(format!(
            "attempt {attempt} is not finished"
        )));
    }
    transition(world, worktree, WorktreeState::Removing)?;
    let mut entity = world.entity_mut(worktree);
    entity.remove::<Problem>();
    if force {
        entity.insert(ForceRemoval);
    }
    let generation = world.resource::<Generation>().0;
    world.resource_mut::<InFlight>().0.insert(
        worktree,
        Flight::Committed {
            after: Some(generation),
        },
    );
    Ok(())
}

/// Explicit reconciliation (WORKTREES.md:44-54, 111-114, 121): inspects the filesystem and the
/// repository registration and advances only where the evidence is unambiguous. Never runs
/// `git worktree add`/`remove`.
pub fn reconcile(world: &mut World, worktree: Entity) -> Result<(), WorktreeError> {
    let state = *world
        .get::<WorktreeState>(worktree)
        .ok_or_else(|| WorktreeError::NotFound(format!("worktree {worktree}")))?;
    if world.resource::<InFlight>().0.contains_key(&worktree) {
        return Err(WorktreeError::Refused(
            "a git command or commit is in progress; retry later".into(),
        ));
    }
    let spec = world
        .get::<WorktreeSpec>(worktree)
        .cloned()
        .ok_or_else(|| WorktreeError::NotFound("worktree spec".into()))?;
    match state {
        WorktreeState::Removed => Ok(()),
        WorktreeState::Allocating | WorktreeState::Prepared => {
            let parent = Path::new(&spec.parent);
            if parent.exists() && !dir_is_empty(parent) {
                let reason = "reserved parent is not empty".to_owned();
                world.entity_mut(worktree).insert(Problem(reason.clone()));
                return Err(WorktreeError::Refused(reason));
            }
            resume_committed(world, worktree);
            Ok(())
        }
        WorktreeState::Creating => {
            uncertain(
                world,
                worktree,
                "creation was submitted; its outcome is unknown",
            );
            list_registration(world, worktree, &spec, Flight::Reconciling { op: 0 });
            Ok(())
        }
        WorktreeState::Uncertain | WorktreeState::Ready | WorktreeState::Removing => {
            list_registration(world, worktree, &spec, Flight::Reconciling { op: 0 });
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Advancement
// ---------------------------------------------------------------------------------------------

/// One step per worktree per update, only past the commit that recorded the current state.
fn advance(world: &mut World) {
    let generation = world.resource::<Generation>().0;
    let worktrees: Vec<(Entity, WorktreeState, WorktreeSpec)> = world
        .query_filtered::<(Entity, &WorktreeState, &WorktreeSpec), With<Worktree>>()
        .iter(world)
        .map(|(e, s, spec)| (e, *s, spec.clone()))
        .collect();
    for (entity, state, spec) in worktrees {
        let flight = world.resource::<InFlight>().0.get(&entity).copied();
        match (state, flight) {
            (WorktreeState::Allocating, Some(Flight::Committed { after }))
                if due(after, generation) =>
            {
                prepare(world, entity, &spec, generation);
            }
            (WorktreeState::Prepared, Some(Flight::Committed { after }))
                if due(after, generation)
                    && transition(world, entity, WorktreeState::Creating).is_ok() =>
            {
                let op = git::request(
                    world,
                    vec![
                        "worktree".into(),
                        "add".into(),
                        "-b".into(),
                        spec.branch.clone(),
                        tree_path(&spec).display().to_string(),
                        spec.base.clone(),
                    ],
                    spec.repo.clone(),
                );
                set_flight(world, entity, Flight::Adding { op });
            }
            (WorktreeState::Creating, None) => {
                uncertain(
                    world,
                    entity,
                    "creation reply lost (restart); reconcile to inspect registration",
                );
            }
            (WorktreeState::Creating, Some(Flight::Adding { op })) => {
                if let Some(done) = git::take_reply(world, op) {
                    if done.succeeded() {
                        list_registration(world, entity, &spec, Flight::Listing { op: 0 });
                    } else {
                        uncertain(world, entity, &describe(&done, "git worktree add"));
                    }
                }
            }
            (WorktreeState::Creating, Some(Flight::Listing { op })) => {
                if let Some(done) = git::take_reply(world, op) {
                    match ready_registration(&done, &spec, true) {
                        Ok(()) => {
                            let op = git::request(
                                world,
                                vec!["status".into(), "--porcelain".into()],
                                tree_path(&spec).display().to_string(),
                            );
                            set_flight(world, entity, Flight::Status { op });
                        }
                        Err(reason) => uncertain(world, entity, &reason),
                    }
                }
            }
            (WorktreeState::Creating, Some(Flight::Status { op })) => {
                if let Some(done) = git::take_reply(world, op) {
                    if done.succeeded() && done.stdout.trim().is_empty() {
                        ready(world, entity);
                    } else if done.succeeded() {
                        uncertain(world, entity, "new checkout is not clean against HEAD");
                    } else {
                        uncertain(world, entity, &describe(&done, "git status"));
                    }
                }
            }
            (WorktreeState::Uncertain | WorktreeState::Ready, Some(Flight::Reconciling { op })) => {
                if let Some(done) = git::take_reply(world, op) {
                    match ready_registration(&done, &spec, false) {
                        Ok(()) if tree_path(&spec).is_dir() => ready(world, entity),
                        Ok(()) => problem(world, entity, "registered but the checkout is missing"),
                        Err(reason) => problem(world, entity, &reason),
                    }
                }
            }
            (WorktreeState::Removing, Some(Flight::Committed { after }))
                if due(after, generation) =>
            {
                let mut argv = vec!["worktree".to_owned(), "remove".to_owned()];
                if world.get::<ForceRemoval>(entity).is_some() {
                    argv.push("--force".into());
                }
                argv.push(tree_path(&spec).display().to_string());
                let op = git::request(world, argv, spec.repo.clone());
                set_flight(world, entity, Flight::Removing { op });
            }
            (WorktreeState::Removing, Some(Flight::Removing { op })) => {
                if let Some(done) = git::take_reply(world, op) {
                    if !done.succeeded() {
                        problem(world, entity, &describe(&done, "git worktree remove"));
                    }
                    list_registration(world, entity, &spec, Flight::Reconciling { op: 0 });
                }
            }
            (WorktreeState::Removing, Some(Flight::Reconciling { op })) => {
                if let Some(done) = git::take_reply(world, op) {
                    let path = tree_path(&spec);
                    let path_present = std::fs::symlink_metadata(&path).is_ok();
                    let registered =
                        done.succeeded() && registration(&done.stdout, &path).is_some();
                    if !done.succeeded() {
                        problem(world, entity, &describe(&done, "git worktree list"));
                    } else if path_present || registered {
                        problem(
                            world,
                            entity,
                            &format!(
                                "removal incomplete: path present={path_present}, registered={registered}; \
                                 resolve manually, then reconcile"
                            ),
                        );
                    } else {
                        let _ = transition(world, entity, WorktreeState::Removed);
                        world.entity_mut(entity).remove::<Problem>();
                    }
                    world.resource_mut::<InFlight>().0.remove(&entity);
                }
            }
            _ => {}
        }
    }
}

/// `Allocating → Prepared`: the parent directory (0700) exists and is empty (WORKTREES.md:44-47).
fn prepare(world: &mut World, entity: Entity, spec: &WorktreeSpec, generation: u64) {
    let parent = Path::new(&spec.parent);
    let created = if parent.exists() {
        if dir_is_empty(parent) {
            Ok(())
        } else {
            Err("reserved parent exists and is not empty".to_owned())
        }
    } else {
        std::fs::create_dir_all(parent.parent().unwrap_or(parent))
            .and_then(|()| std::fs::create_dir(parent))
            .and_then(|()| {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
            })
            .map_err(|e| format!("mkdir {}: {e}", parent.display()))
    };
    match created {
        Ok(()) => {
            if transition(world, entity, WorktreeState::Prepared).is_ok() {
                world.entity_mut(entity).remove::<Problem>();
                set_flight(
                    world,
                    entity,
                    Flight::Committed {
                        after: Some(generation),
                    },
                );
            }
        }
        Err(reason) => {
            // Retained and rejected (WORKTREES.md:46): explicit reconcile after the operator
            // cleared the path.
            problem(world, entity, &reason);
        }
    }
}

fn list_registration(world: &mut World, entity: Entity, spec: &WorktreeSpec, kind: Flight) {
    let op = git::request(
        world,
        vec!["worktree".into(), "list".into(), "--porcelain".into()],
        spec.repo.clone(),
    );
    let flight = match kind {
        Flight::Listing { .. } => Flight::Listing { op },
        _ => Flight::Reconciling { op },
    };
    set_flight(world, entity, flight);
}

fn resume_committed(world: &mut World, entity: Entity) {
    set_flight(world, entity, Flight::Committed { after: None });
}

/// A commit-gated step is due once the journal generation passed the one that recorded the
/// state (`None`: restored from the journal, already committed).
fn due(after: Option<u64>, generation: u64) -> bool {
    after.is_none_or(|a| generation > a)
}

fn set_flight(world: &mut World, entity: Entity, flight: Flight) {
    world.resource_mut::<InFlight>().0.insert(entity, flight);
}

fn ready(world: &mut World, entity: Entity) {
    if transition(world, entity, WorktreeState::Ready).is_ok() {
        world.entity_mut(entity).remove::<Problem>();
    }
    world.resource_mut::<InFlight>().0.remove(&entity);
}

fn uncertain(world: &mut World, entity: Entity, reason: &str) {
    let _ = transition(world, entity, WorktreeState::Uncertain);
    problem(world, entity, reason);
}

/// Records a bounded diagnostic and ends this incarnation's involvement (manual resolution).
fn problem(world: &mut World, entity: Entity, reason: &str) {
    let text: String = reason.chars().take(512).collect();
    world.entity_mut(entity).insert(Problem(text));
    world.resource_mut::<InFlight>().0.remove(&entity);
}

fn transition(world: &mut World, entity: Entity, next: WorktreeState) -> Result<(), WorktreeError> {
    let current = *world
        .get::<WorktreeState>(entity)
        .ok_or_else(|| WorktreeError::NotFound(format!("worktree {entity}")))?;
    if current == next {
        return Ok(());
    }
    if !current.may_become(next) {
        warn!("worktree {entity}: {current:?} may not become {next:?}");
        return Err(WorktreeError::Refused(format!(
            "worktree is {current:?}, cannot become {next:?}"
        )));
    }
    world.entity_mut(entity).insert(next);
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Checks
// ---------------------------------------------------------------------------------------------

fn unresolved_check(world: &mut World, task: Entity) -> Option<Entity> {
    world
        .query::<(Entity, &CheckOf, &CheckState)>()
        .iter(world)
        .find(|(_, of, state)| {
            of.0 == task
                && matches!(
                    state,
                    CheckState::Queued | CheckState::Running | CheckState::Uncertain
                )
        })
        .map(|(e, _, _)| e)
}

/// An attempt outside `Finished` of the owning task, or of any task whose literal cwd lies
/// inside the checkout (WORKTREES.md:80-81).
fn live_attempt_inside(world: &mut World, task: Entity, path: &Path) -> Option<Entity> {
    let inside: Vec<Entity> = world
        .query_filtered::<(Entity, &Location), With<Task>>()
        .iter(world)
        .filter(|(e, location)| {
            *e == task || matches!(location, Location::Cwd(cwd) if Path::new(cwd).starts_with(path))
        })
        .map(|(e, _)| e)
        .collect();
    world
        .query::<(Entity, &AttemptOf, &AttemptState)>()
        .iter(world)
        .find(|(_, of, state)| inside.contains(&of.0) && **state != AttemptState::Finished)
        .map(|(e, _, _)| e)
}

fn valid_branch(branch: &str) -> Result<(), WorktreeError> {
    let bad = branch.is_empty()
        || branch.len() > 255
        || branch.starts_with('-')
        || branch.starts_with('/')
        || branch.ends_with('/')
        || branch.ends_with('.')
        || branch.ends_with(".lock")
        || branch == "@"
        || branch.contains("..")
        || branch.contains("@{")
        || branch.contains("//")
        || branch.contains("/.")
        || branch
            .chars()
            .any(|c| c.is_ascii_control() || " ~^:?*[\\".contains(c));
    if bad {
        Err(WorktreeError::Refused(format!(
            "branch {branch:?} is not a valid literal branch name"
        )))
    } else {
        Ok(())
    }
}

fn valid_base(base: &str) -> Result<(), WorktreeError> {
    if base.is_empty()
        || base.starts_with('-')
        || base.contains("@{")
        || base.chars().any(|c| c.is_ascii_control() || c == ' ')
    {
        Err(WorktreeError::Refused(format!(
            "base {base:?} must be a plain revision (no checkout shorthand)"
        )))
    } else {
        Ok(())
    }
}

/// One trimmed line of stdout from a successful read-only git command.
fn git_line(argv: &[&str], cwd: &Path) -> Result<String, String> {
    let argv: Vec<String> = argv.iter().map(|s| (*s).to_owned()).collect();
    let done = git::run(0, &argv, cwd);
    if done.succeeded() {
        Ok(done.stdout.trim().to_owned())
    } else {
        Err(describe(&done, "git"))
    }
}

fn describe(done: &GitDone, what: &str) -> String {
    let detail = done.stderr.trim();
    match done.code {
        Some(code) => format!("{what} exited {code}: {detail}"),
        None => format!("{what} outcome unknown: {detail}"),
    }
}

fn random_suffix() -> Result<String, WorktreeError> {
    fux::attach::random_hex256()
        .map(|hex| hex.chars().take(16).collect())
        .map_err(|e| WorktreeError::Refused(format!("no randomness: {e}")))
}

fn dir_is_empty(path: &Path) -> bool {
    std::fs::read_dir(path).is_ok_and(|mut entries| entries.next().is_none())
}

/// One `git worktree list --porcelain` block.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Registration {
    pub head: String,
    pub branch: Option<String>,
    pub locked: bool,
    pub prunable: bool,
}

/// Canonical form through the deepest existing ancestor, so a printed registration matches
/// the reserved path whether or not the checkout still exists (macOS `/var` → `/private/var`).
fn normalize(path: &Path) -> PathBuf {
    if let Ok(canonical) = std::fs::canonicalize(path) {
        return canonical;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => normalize(parent).join(name),
        _ => path.to_path_buf(),
    }
}

/// The block registered for `path`, comparing the printed path and its canonical form.
pub fn registration(porcelain: &str, path: &Path) -> Option<Registration> {
    let wanted = normalize(path);
    let same = |printed: &str| normalize(Path::new(printed)) == wanted;
    let mut current: Option<Registration> = None;
    for line in porcelain.lines().chain(core::iter::once("")) {
        if line.is_empty() {
            if let Some(found) = current.take() {
                return Some(found);
            }
            continue;
        }
        if let Some(printed) = line.strip_prefix("worktree ") {
            current = same(printed).then(Registration::default);
            continue;
        }
        let Some(entry) = current.as_mut() else {
            continue;
        };
        if let Some(head) = line.strip_prefix("HEAD ") {
            entry.head = head.to_owned();
        } else if let Some(branch) = line.strip_prefix("branch ") {
            entry.branch = Some(branch.to_owned());
        } else if line == "locked" || line.starts_with("locked ") {
            entry.locked = true;
        } else if line == "prunable" || line.starts_with("prunable ") {
            entry.prunable = true;
        }
    }
    None
}

/// Initial readiness (WORKTREES.md:56-58): registered at the path on the branch, unlocked,
/// non-prunable and, when `pinned`, at the base commit. Worker commits may move HEAD after
/// readiness, so a reconciliation of a ready tree accepts any HEAD.
fn ready_registration(done: &GitDone, spec: &WorktreeSpec, pinned: bool) -> Result<(), String> {
    if !done.succeeded() {
        return Err(describe(done, "git worktree list"));
    }
    let path = tree_path(spec);
    let entry = registration(&done.stdout, &path)
        .ok_or_else(|| format!("{} is not registered in {}", path.display(), spec.repo))?;
    let expected = format!("refs/heads/{}", spec.branch);
    if entry.branch.as_deref() != Some(expected.as_str()) {
        return Err(format!(
            "registered on {:?}, expected {expected}",
            entry.branch.unwrap_or_default()
        ));
    }
    if entry.locked || entry.prunable {
        return Err("registration is locked or prunable".into());
    }
    if pinned && entry.head != spec.base {
        return Err(format!(
            "HEAD {} is not the pinned base {}",
            entry.head, spec.base
        ));
    }
    Ok(())
}
