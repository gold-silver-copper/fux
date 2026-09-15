//! Owned worktrees against a real temporary git repository (WORKTREES.md, invariants 9, 25):
//! intent-before-git creation, exactly one `git worktree add` even across a lost reply and a
//! retried request, `Uncertain` after the lost reply, removal refused while a check is
//! unresolved or an attempt is live, `Removed` only once the path and the registration are
//! both gone, and `check_invariants` after every update.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "integration-test helpers; clippy.toml only relaxes #[test] bodies"
)]
mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use bevy_app::App;
use bevy_ecs::prelude::*;
use zor::config::Config;
use zor::git;
use zor::model::invariants::check_invariants;
use zor::model::*;
use zor::worktrees::{self, WorktreeError, WorktreeRequest};

fn sh(argv: &[&str], cwd: &Path) -> String {
    let argv: Vec<String> = argv.iter().map(|s| (*s).to_owned()).collect();
    let done = git::run(0, &argv, cwd);
    assert_eq!(done.code, Some(0), "git {argv:?}: {}", done.stderr);
    done.stdout
}

/// A repository with one commit.
fn repo(dir: &Path) -> PathBuf {
    let repo = dir.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    sh(&["init", "-q", "-b", "main"], &repo);
    sh(&["config", "user.email", "zor@example"], &repo);
    sh(&["config", "user.name", "zor"], &repo);
    std::fs::write(repo.join("README"), "hello\n").unwrap();
    sh(&["add", "README"], &repo);
    sh(&["commit", "-q", "-m", "init"], &repo);
    std::fs::canonicalize(repo).unwrap()
}

fn app(state: &Path) -> App {
    let mut app = zor::app::build_headless(&Config::default(), state);
    app.update();
    app
}

fn step(app: &mut App) -> Vec<Effect> {
    app.update();
    let world = app.world_mut();
    let effects: Vec<Effect> = world.resource_mut::<Messages<Effect>>().drain().collect();
    check_invariants(world).unwrap();
    effects
}

/// The runner's job for `RunGit`: run it, feed `GitDone`. Returns the argv of every command.
fn answer(app: &mut App, effects: Vec<Effect>) -> Vec<Vec<String>> {
    let mut ran = Vec::new();
    for effect in effects {
        if let Effect::RunGit { op, argv, cwd } = effect {
            let done = git::run(op, &argv, Path::new(&cwd));
            ran.push(argv);
            app.world_mut().write_message(Inbound::GitDone {
                op: done.op,
                code: done.code,
                stdout: done.stdout,
                stderr: done.stderr,
            });
        }
    }
    ran
}

fn state_of(app: &App, worktree: Entity) -> WorktreeState {
    *app.world().get::<WorktreeState>(worktree).unwrap()
}

/// Steps and answers git until `worktree` reaches `target` or the step budget runs out.
fn pump(app: &mut App, worktree: Entity, target: WorktreeState) -> Vec<Vec<String>> {
    let mut ran = Vec::new();
    for _ in 0..40 {
        if state_of(app, worktree) == target && !in_flight(app, worktree) {
            return ran;
        }
        let effects = step(app);
        ran.extend(answer(app, effects));
    }
    panic!(
        "worktree never reached {target:?}: {:?}",
        worktrees::inspect(app.world(), worktree)
    );
}

fn in_flight(app: &App, worktree: Entity) -> bool {
    worktrees::inspect(app.world(), worktree).unwrap().in_flight
}

fn new_task(world: &mut World, id: &str) -> Entity {
    spawn_task(
        world,
        TaskSpec {
            id,
            title: id,
            location: Location::Cwd("/tmp".into()),
            created_ms: 1,
        },
    )
    .unwrap()
}

fn request(id: &str, repo: &Path, branch: &str) -> WorktreeRequest {
    WorktreeRequest {
        id: id.into(),
        repo: repo.display().to_string(),
        branch: branch.into(),
        base: "HEAD".into(),
    }
}

fn registered_paths(repo: &Path) -> Vec<String> {
    sh(&["worktree", "list", "--porcelain"], repo)
        .lines()
        .filter_map(|l| l.strip_prefix("worktree ").map(str::to_owned))
        .collect()
}

fn adds(ran: &[Vec<String>]) -> usize {
    ran.iter()
        .filter(|argv| {
            argv.first().map(String::as_str) == Some("worktree")
                && argv.get(1).map(String::as_str) == Some("add")
        })
        .count()
}

#[test]
fn creation_is_intent_before_git_and_runs_add_once() {
    let dir = tempfile::tempdir().unwrap();
    let repo = repo(dir.path());
    let state = dir.path().join("state");
    let mut app = app(&state);
    let task = new_task(app.world_mut(), "t1");

    // Pre-checks refuse before any intent is recorded.
    let bad_base = WorktreeRequest {
        base: "@{-1}".into(),
        ..request("w", &repo, "agent/parser")
    };
    assert!(matches!(
        worktrees::allocate(app.world_mut(), task, bad_base),
        Err(WorktreeError::Refused(_))
    ));
    assert!(matches!(
        worktrees::allocate(app.world_mut(), task, request("w", &repo, "main")),
        Err(WorktreeError::Refused(_))
    ));
    assert!(matches!(
        worktrees::allocate(app.world_mut(), task, request("w", &repo, "-x")),
        Err(WorktreeError::Refused(_))
    ));
    assert!(app.world().resource::<Ids>().worktree("w").is_none());

    let worktree =
        worktrees::allocate(app.world_mut(), task, request("w", &repo, "agent/parser")).unwrap();
    assert_eq!(state_of(&app, worktree), WorktreeState::Allocating);
    let path = worktrees::path_of(app.world(), worktree).unwrap();
    let parent = path.parent().unwrap().to_path_buf();
    assert!(parent.starts_with(state.join(worktrees::WORKTREES_DIR)));
    let sha = sh(&["rev-parse", "HEAD"], &repo).trim().to_owned();
    assert_eq!(app.world().get::<WorktreeSpec>(worktree).unwrap().base, sha);

    // Update 1 commits `Allocating`; nothing touches the filesystem before that commit.
    let effects = step(&mut app);
    assert!(effects.is_empty());
    assert_eq!(state_of(&app, worktree), WorktreeState::Allocating);
    assert!(!parent.exists());
    let journal = std::fs::read_to_string(state.join(zor::journal::JOURNAL_FILE)).unwrap();
    assert!(journal.contains("Allocating"));

    // Update 2: the committed reservation is realised (0700, empty) and `Prepared` recorded.
    let effects = step(&mut app);
    assert!(effects.is_empty());
    assert_eq!(state_of(&app, worktree), WorktreeState::Prepared);
    assert_eq!(
        std::fs::metadata(&parent).unwrap().permissions().mode() & 0o777,
        0o700
    );

    // Update 3: `Creating` is journaled in the same update whose effects carry the add.
    let effects = step(&mut app);
    assert_eq!(state_of(&app, worktree), WorktreeState::Creating);
    let journal = std::fs::read_to_string(state.join(zor::journal::JOURNAL_FILE)).unwrap();
    assert!(journal.contains("Creating"));
    assert!(matches!(effects.as_slice(), [Effect::RunGit { argv, .. }] if argv[1] == "add"));
    assert!(
        registered_paths(&repo).len() == 1,
        "no add ran before the effect was applied"
    );

    let mut ran = answer(&mut app, effects);
    ran.extend(pump(&mut app, worktree, WorktreeState::Ready));
    assert_eq!(adds(&ran), 1, "the add ran once, through the effect");
    assert!(path.join("README").is_file());
    assert_eq!(registered_paths(&repo).len(), 2);
    assert_eq!(
        sh(&["rev-parse", "--abbrev-ref", "HEAD"], &path).trim(),
        "agent/parser"
    );
    assert!(app.world().get::<Problem>(worktree).is_none());

    // Identical retry: the record, no new effect. A changed plan under the id fails.
    assert_eq!(
        worktrees::allocate(app.world_mut(), task, request("w", &repo, "agent/parser")).unwrap(),
        worktree
    );
    assert!(step(&mut app).is_empty());
    assert!(matches!(
        worktrees::allocate(app.world_mut(), task, request("w", &repo, "agent/other")),
        Err(WorktreeError::Conflict(_))
    ));
    let other = new_task(app.world_mut(), "t2");
    assert!(matches!(
        worktrees::allocate(app.world_mut(), other, request("w", &repo, "agent/parser")),
        Err(WorktreeError::Conflict(_))
    ));
    assert_eq!(check_invariants(app.world_mut()), Ok(()));
}

#[test]
fn lost_reply_is_uncertain_and_retry_never_re_adds() {
    let dir = tempfile::tempdir().unwrap();
    let repo = repo(dir.path());
    let state = dir.path().join("state");
    let worktree_path;
    {
        let mut app = app(&state);
        let task = new_task(app.world_mut(), "t1");
        let worktree =
            worktrees::allocate(app.world_mut(), task, request("w", &repo, "agent/lost")).unwrap();
        worktree_path = worktrees::path_of(app.world(), worktree).unwrap();
        // Step until the add effect is emitted, run it for real, and lose the reply.
        let mut effects = Vec::new();
        for _ in 0..10 {
            effects = step(&mut app);
            if !effects.is_empty() {
                break;
            }
        }
        assert_eq!(state_of(&app, worktree), WorktreeState::Creating);
        let Effect::RunGit { argv, cwd, .. } = effects.pop().unwrap() else {
            panic!("expected the add");
        };
        assert_eq!(argv[1], "add");
        let done = git::run(0, &argv, Path::new(&cwd));
        assert_eq!(done.code, Some(0), "{}", done.stderr);
        // The reply is dropped here; the server "dies" with `Creating` committed.
        step(&mut app);
    }
    assert_eq!(registered_paths(&repo).len(), 2);

    // The first update restores the journal and sweeps: the committed `Creating` whose reply
    // is gone becomes `Uncertain`; the add is never re-run.
    let mut app = zor::app::build_headless(&Config::default(), &state);
    let effects = step(&mut app);
    assert!(effects.is_empty(), "recovery never re-runs the add");
    let world = app.world_mut();
    let worktree = world.resource::<Ids>().worktree("w").unwrap();
    let task = world.resource::<Ids>().task("t1").unwrap();
    assert_eq!(state_of(&app, worktree), WorktreeState::Uncertain);
    assert!(app.world().get::<Problem>(worktree).is_some());

    // Retrying the request returns the record; nothing is created again.
    assert_eq!(
        worktrees::allocate(app.world_mut(), task, request("w", &repo, "agent/lost")).unwrap(),
        worktree
    );
    assert!(step(&mut app).is_empty());
    assert_eq!(state_of(&app, worktree), WorktreeState::Uncertain);

    // Explicit reconciliation inspects the registration and confirms readiness.
    worktrees::reconcile(app.world_mut(), worktree).unwrap();
    let ran = pump(&mut app, worktree, WorktreeState::Ready);
    assert_eq!(adds(&ran), 0);
    assert!(ran.iter().all(|argv| argv[1] == "list"));
    assert_eq!(registered_paths(&repo).len(), 2);
    assert_eq!(
        worktrees::path_of(app.world(), worktree).unwrap(),
        worktree_path
    );
    assert!(app.world().get::<Problem>(worktree).is_none());
    assert_eq!(check_invariants(app.world_mut()), Ok(()));
}

#[test]
fn removal_needs_a_quiet_task_and_confirms_absence() {
    let dir = tempfile::tempdir().unwrap();
    let repo = repo(dir.path());
    let state = dir.path().join("state");
    let mut app = app(&state);
    let task = new_task(app.world_mut(), "t1");
    let worktree =
        worktrees::allocate(app.world_mut(), task, request("w", &repo, "agent/rm")).unwrap();
    pump(&mut app, worktree, WorktreeState::Ready);
    let path = worktrees::path_of(app.world(), worktree).unwrap();

    // An unresolved check of the task blocks removal (invariant 9).
    let check = spawn_check(
        app.world_mut(),
        CheckSpec {
            id: "c1",
            task,
            source: None,
            command: CheckCommand {
                argv: vec!["true".into()],
                timeout_ms: 1_000,
            },
            requirement: None,
            generation: 1,
        },
    )
    .unwrap();
    assert!(matches!(
        worktrees::remove(app.world_mut(), worktree, false),
        Err(WorktreeError::Refused(_))
    ));
    assert_eq!(state_of(&app, worktree), WorktreeState::Ready);
    app.world_mut()
        .entity_mut(check)
        .insert(CheckState::Running);
    assert!(worktrees::remove(app.world_mut(), worktree, false).is_err());
    app.world_mut().entity_mut(check).insert(CheckState::Passed);
    spawn_result(
        app.world_mut(),
        check,
        Verdict::Passed,
        OutputTail::default(),
    )
    .unwrap();

    // A live attempt of the task blocks removal even after cancellation of the task.
    let attempt = spawn_attempt(
        app.world_mut(),
        AttemptSpec {
            task,
            ownership: Ownership::Managed,
            handle: common::handle("fux-1", 1, Some(10)),
        },
    )
    .unwrap();
    app.world_mut()
        .entity_mut(attempt)
        .insert(AttemptState::Live);
    app.world_mut().entity_mut(task).insert(TaskState::Running);
    assert!(matches!(
        worktrees::remove(app.world_mut(), worktree, false),
        Err(WorktreeError::Refused(_))
    ));
    app.world_mut().entity_mut(task).insert(TaskState::Closed {
        outcome: TaskOutcome::Cancelled,
    });
    assert!(worktrees::remove(app.world_mut(), worktree, false).is_err());
    app.world_mut()
        .entity_mut(attempt)
        .insert((AttemptState::Finished, FinalEvidence::default()));
    assert!(step(&mut app).is_empty());

    // Untracked files make plain removal fail: `Removing` is retained with a problem, the
    // path and registration are still present, and nothing runs `remove` again.
    std::fs::write(path.join("scratch.txt"), "wip\n").unwrap();
    worktrees::remove(app.world_mut(), worktree, false).unwrap();
    assert_eq!(state_of(&app, worktree), WorktreeState::Removing);
    let effects = step(&mut app);
    assert!(effects.is_empty(), "removal waits for its commit");
    let journal = std::fs::read_to_string(state.join(zor::journal::JOURNAL_FILE)).unwrap();
    assert!(journal.contains("Removing"));
    let effects = step(&mut app);
    assert!(
        matches!(effects.as_slice(), [Effect::RunGit { argv, .. }] if argv[1] == "remove" && !argv.contains(&"--force".to_owned()))
    );
    let mut ran = answer(&mut app, effects);
    for _ in 0..10 {
        if !in_flight(&app, worktree) {
            break;
        }
        let effects = step(&mut app);
        ran.extend(answer(&mut app, effects));
    }
    assert_eq!(state_of(&app, worktree), WorktreeState::Removing);
    assert!(app.world().get::<Problem>(worktree).is_some());
    assert!(path.is_dir());
    assert_eq!(registered_paths(&repo).len(), 2);
    let removes = ran.iter().filter(|a| a[1] == "remove").count();
    assert_eq!(removes, 1, "remove ran exactly once");

    // Same policy: the retained record; a changed policy is a conflict; neither re-runs git.
    worktrees::remove(app.world_mut(), worktree, false).unwrap();
    assert!(matches!(
        worktrees::remove(app.world_mut(), worktree, true),
        Err(WorktreeError::Conflict(_))
    ));
    assert!(step(&mut app).is_empty());
    assert!(step(&mut app).is_empty());
    assert!(
        app.world()
            .get::<zor::worktrees::ForceRemoval>(worktree)
            .is_none()
    );

    // Manual completion, then reconcile: `Removed` only once path and registration are gone.
    sh(
        &["worktree", "remove", "--force", &path.display().to_string()],
        &repo,
    );
    assert!(!path.exists());
    assert_eq!(registered_paths(&repo).len(), 1);
    worktrees::reconcile(app.world_mut(), worktree).unwrap();
    let ran = pump(&mut app, worktree, WorktreeState::Removed);
    assert!(ran.iter().all(|argv| argv[1] == "list"));
    assert!(worktrees::path_of(app.world(), worktree).is_none());
    assert!(app.world().get::<Problem>(worktree).is_none());
    // The branch and the private parent are retained (WORKTREES.md:117).
    assert!(path.parent().unwrap().is_dir());
    assert_eq!(
        sh(&["rev-parse", "--verify", "refs/heads/agent/rm"], &repo)
            .trim()
            .len(),
        40
    );
    // Removed ids never recreate a checkout; identical retries return history.
    assert_eq!(
        worktrees::allocate(app.world_mut(), task, request("w", &repo, "agent/rm")).unwrap(),
        worktree
    );
    assert!(step(&mut app).is_empty());
    assert_eq!(state_of(&app, worktree), WorktreeState::Removed);
    worktrees::remove(app.world_mut(), worktree, false).unwrap();
    assert_eq!(check_invariants(app.world_mut()), Ok(()));
}

#[test]
fn partial_reconcile_keeps_a_registered_but_missing_checkout_uncertain() {
    let dir = tempfile::tempdir().unwrap();
    let repo = repo(dir.path());
    let state = dir.path().join("state");
    let mut app = app(&state);
    let task = new_task(app.world_mut(), "t1");
    let worktree =
        worktrees::allocate(app.world_mut(), task, request("w", &repo, "agent/gone")).unwrap();
    pump(&mut app, worktree, WorktreeState::Ready);
    let path = worktrees::path_of(app.world(), worktree).unwrap();
    std::fs::remove_dir_all(&path).unwrap();
    worktrees::reconcile(app.world_mut(), worktree).unwrap();
    for _ in 0..10 {
        let effects = step(&mut app);
        answer(&mut app, effects);
        if !in_flight(&app, worktree) {
            break;
        }
    }
    assert_eq!(state_of(&app, worktree), WorktreeState::Ready);
    let problem = app.world().get::<Problem>(worktree).unwrap();
    assert!(
        problem.0.contains("missing") || problem.0.contains("prunable"),
        "{}",
        problem.0
    );
    assert_eq!(check_invariants(app.world_mut()), Ok(()));
}
