//! Checks, sources, artifacts, results and verification (CHECKS.md, SOURCES.md, ARTIFACTS.md,
//! RESULTS.md; invariants 4, 8, 14, 20-23) over a headless app with the real `CheckRunner`
//! (subprocesses through `sh -c`) and the real `GitAdapter` over a temporary repository.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "integration-test helpers; clippy.toml only relaxes #[test] bodies"
)]
mod common;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use async_channel::Receiver;
use bevy_app::App;
use bevy_ecs::prelude::*;
use bevy_ecs::relationship::RelationshipTarget;
use common::handle;
use zor::checks::artifacts::captured_by;
use zor::checks::runner::CheckRunner;
use zor::checks::{
    self, ArtifactBytes, ArtifactPolicy, ArtifactSpec, CaptureRequest, CapturedBy, CheckError,
    CheckPolicy, CheckSpec, RequirementStatus, SourceSpec, SourceState, VerifySpec,
};
use zor::config::Config;
use zor::git::GitAdapter;
use zor::model::invariants::check_invariants;
use zor::model::*;
use zor::runner::Adapter;

/// A headless server plus the runner's effect routing and inbound feed.
struct Harness {
    app: App,
    inbound: Receiver<Inbound>,
    adapters: Vec<Box<dyn Adapter>>,
    state: PathBuf,
    _dir: tempfile::TempDir,
}

impl Harness {
    fn new() -> Self {
        // Canonical (`/private/var` on macOS) so `pwd` output compares equal.
        let dir = tempfile::Builder::new()
            .prefix("zor-checks-")
            .tempdir_in(std::env::temp_dir().canonicalize().unwrap())
            .unwrap();
        let state = dir.path().join("state");
        let mut harness = Self::open(state, dir);
        harness.app.update();
        harness
    }

    fn open(state: PathBuf, dir: tempfile::TempDir) -> Self {
        let (sender, inbound) = async_channel::unbounded();
        let mut app = zor::app::build_headless(&Config::default(), &state);
        app.finish();
        app.cleanup();
        let adapters: Vec<Box<dyn Adapter>> = vec![
            Box::new(CheckRunner::with_executable(
                sender.clone(),
                env!("CARGO_BIN_EXE_zor").into(),
            )),
            Box::new(GitAdapter::new(sender)),
        ];
        Self {
            app,
            inbound,
            adapters,
            state,
            _dir: dir,
        }
    }

    /// Restarts the server over the same state directory.
    fn restart(self) -> Self {
        let Self { state, _dir, .. } = self;
        let mut harness = Self::open(state, _dir);
        harness.app.update();
        harness
    }

    fn world(&mut self) -> &mut World {
        self.app.world_mut()
    }

    /// One update: feeds whatever the adapters replied, steps, routes the effects, checks the
    /// invariants. Returns the effects routed.
    fn step(&mut self) -> Vec<String> {
        let world = self.app.world_mut();
        let mut batch = Vec::new();
        while let Ok(message) = self.inbound.try_recv() {
            batch.push(message);
        }
        world
            .resource_mut::<Messages<Inbound>>()
            .write_batch(batch.drain(..));
        world.resource_mut::<Clock>().now_ms = fux::runner::wall_ms();
        self.app.update();
        let effects: Vec<Effect> = self
            .app
            .world_mut()
            .resource_mut::<Messages<Effect>>()
            .drain()
            .collect();
        let mut routed = Vec::new();
        for effect in effects {
            routed.push(format!("{effect:?}"));
            let adapter = self
                .adapters
                .iter_mut()
                .find(|a| a.handles(&effect))
                .expect("every effect has an adapter");
            if let Some(completion) = adapter.apply(effect).unwrap() {
                self.app.world_mut().write_message(completion);
            }
        }
        assert_eq!(check_invariants(self.app.world_mut()), Ok(()));
        routed
    }

    /// Steps until `done` holds (at most `timeout`), sleeping briefly between updates.
    fn step_until(&mut self, timeout: Duration, mut done: impl FnMut(&mut World) -> bool) {
        let deadline = Instant::now() + timeout;
        loop {
            self.step();
            if done(self.app.world_mut()) {
                return;
            }
            assert!(Instant::now() < deadline, "condition not reached in time");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn settle(&mut self, check: Entity) -> CheckState {
        self.step_until(Duration::from_secs(20), |world| {
            world.get::<CheckState>(check).unwrap().is_terminal()
        });
        *self.world().get::<CheckState>(check).unwrap()
    }

    fn task(&mut self, id: &str, cwd: &Path) -> (Entity, Entity) {
        let world = self.world();
        let task = spawn_task(
            world,
            TaskSpec {
                id,
                title: id,
                location: Location::Cwd(cwd.display().to_string()),
                created_ms: 1_000,
            },
        )
        .unwrap();
        let pane = world.resource::<Ids>().attempts.len() as u64 + 1;
        let attempt = spawn_attempt(
            world,
            AttemptSpec {
                task,
                ownership: Ownership::Managed,
                handle: handle("nonce-a", pane, Some(4242)),
            },
        )
        .unwrap();
        (task, attempt)
    }

    fn submit(&mut self, task: Entity, id: &str, script: &str) -> Entity {
        checks::submit_check(self.world(), task, spec(id, script)).unwrap()
    }

    fn run(&mut self, task: Entity, id: &str, script: &str) -> (Entity, CheckState) {
        let check = self.submit(task, id, script);
        let state = self.settle(check);
        (check, state)
    }

    fn retain(&mut self, task: Entity, id: &str) -> Entity {
        let source = checks::retain_source(
            self.world(),
            task,
            SourceSpec {
                id: id.into(),
                expression: "HEAD".into(),
            },
        )
        .unwrap();
        self.step_until(Duration::from_secs(20), |world| {
            *world.get::<SourceState>(source).unwrap() != SourceState::Resolving
        });
        source
    }
}

fn spec(id: &str, script: &str) -> CheckSpec {
    CheckSpec {
        id: id.into(),
        argv: vec!["sh".into(), "-c".into(), script.into()],
        timeout_ms: 5_000,
        requirement: None,
        source: None,
        artifacts: Vec::new(),
    }
}

fn output(world: &World, check: Entity) -> OutputTail {
    let result = world.get::<Results>(check).unwrap().iter().next().unwrap();
    world.get::<OutputTail>(result).cloned().unwrap()
}

fn git(repo: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

fn repo(dir: &Path) -> PathBuf {
    let repo = dir.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("tracked.txt"), "v1\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "one"]);
    repo
}

fn is_refused(result: Result<impl core::fmt::Debug, CheckError>, needle: &str) {
    match result {
        Err(CheckError::Refused(reason)) => {
            assert!(reason.contains(needle), "{reason:?} lacks {needle:?}");
        }
        other => panic!("expected refusal {needle:?}, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------------------------

#[test]
fn real_subprocess_checks_pass_fail_and_time_out_with_output_tails() {
    let mut h = Harness::new();
    let cwd = h._dir.path().to_path_buf();
    let (task, _) = h.task("t1", &cwd);

    // Journal-first: the intent is committed by the update that dispatches it.
    let check = h.submit(task, "c-pass", "echo out; echo err >&2; pwd");
    let generation = h.world().resource::<Generation>().0;
    let effects = h.step();
    assert_eq!(h.world().resource::<Generation>().0, generation + 1);
    assert_eq!(
        effects.len(),
        1,
        "one RunCheck after the commit: {effects:?}"
    );
    assert!(effects[0].starts_with("RunCheck"));
    assert_eq!(
        h.world().get::<CheckState>(check),
        Some(&CheckState::Running)
    );
    let journal = std::fs::read_to_string(h.state.join(zor::journal::JOURNAL_FILE)).unwrap();
    assert!(
        journal.contains("c-pass"),
        "the queued intent is in the journal"
    );

    assert_eq!(h.settle(check), CheckState::Passed);
    let tail = output(h.world(), check);
    assert_eq!(tail.exit_code, Some(0));
    assert_eq!(tail.stdout, format!("out\n{}\n", cwd.display()));
    assert_eq!(tail.stderr, "err\n");
    assert!(!tail.truncated);
    assert_eq!(
        checks::requirement_status(h.world(), task, "none"),
        RequirementStatus::Missing
    );

    let (failed, state) = h.run(task, "c-fail", "echo nope; exit 3");
    assert_eq!(state, CheckState::Failed);
    assert_eq!(output(h.world(), failed).exit_code, Some(3));

    let (big, state) = h.run(
        task,
        "c-big",
        "head -c 10000 /dev/zero | tr '\\0' a; printf END",
    );
    assert_eq!(state, CheckState::Passed);
    let tail = output(h.world(), big);
    assert_eq!(tail.stdout.len(), MAX_FINAL_OUTPUT_BYTES);
    assert!(tail.stdout.ends_with("END"), "the tail keeps the end");
    assert!(tail.truncated);

    let mut slow = spec("c-slow", "echo partial; sleep 30");
    slow.timeout_ms = 300;
    let started = Instant::now();
    let slow = checks::submit_check(h.world(), task, slow).unwrap();
    assert_eq!(h.settle(slow), CheckState::Uncertain);
    assert!(started.elapsed() < Duration::from_secs(10));
    let problem = h.world().get::<Problem>(slow).unwrap().0.clone();
    assert!(problem.contains("timed out"), "{problem}");
    let tail = output(h.world(), slow);
    assert_eq!(tail.exit_code, None);
    assert_eq!(tail.stdout, "partial\n");
    assert!(checks::has_unresolved_checks(h.world(), task));

    let (signalled, state) = h.run(task, "c-signal", "kill -9 $$");
    assert_eq!(state, CheckState::Failed);
    assert_eq!(output(h.world(), signalled).exit_code, None);

    let (missing, state) = h.run(task, "c-missing", "");
    assert_eq!(state, CheckState::Passed);
    let mut spawn = spec("c-spawn", "");
    spawn.argv = vec!["/nonexistent/zor-binary".into()];
    let spawn = checks::submit_check(h.world(), task, spawn).unwrap();
    assert_eq!(h.settle(spawn), CheckState::Uncertain);
    assert!(h.world().get::<Problem>(spawn).unwrap().0.contains("spawn"));
    assert_ne!(missing, spawn);
}

#[test]
fn cancel_retires_a_queued_check_and_kills_a_running_one() {
    let mut h = Harness::new();
    let cwd = h._dir.path().to_path_buf();
    let (task, _) = h.task("t1", &cwd);

    // Queued and cancelled in the same update: the command never runs.
    let queued = h.submit(task, "c-queued", "touch never");
    checks::cancel_check(h.world(), queued).unwrap();
    let effects = h.step();
    assert!(effects.is_empty(), "{effects:?}");
    assert_eq!(
        h.world().get::<CheckState>(queued),
        Some(&CheckState::Uncertain)
    );
    assert!(!cwd.join("never").exists());

    let running = h.submit(task, "c-running", "sleep 30");
    h.step_until(Duration::from_secs(5), |world| {
        world.get::<CheckState>(running) == Some(&CheckState::Running)
    });
    std::thread::sleep(Duration::from_millis(100));
    checks::cancel_check(h.world(), running).unwrap();
    let started = Instant::now();
    assert_eq!(h.settle(running), CheckState::Uncertain);
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(
        h.world()
            .get::<Problem>(running)
            .unwrap()
            .0
            .contains("cancel")
    );
    // Terminal checks are left alone.
    checks::cancel_check(h.world(), running).unwrap();
    assert_eq!(
        h.world().get::<CheckState>(running),
        Some(&CheckState::Uncertain)
    );
}

#[test]
fn identical_retry_reads_the_record_and_conflicting_intent_fails() {
    let mut h = Harness::new();
    let cwd = h._dir.path().to_path_buf();
    let (task, _) = h.task("t1", &cwd);
    let (_, adopted_attempt) = {
        let world = h.world();
        let other = spawn_task(
            world,
            TaskSpec {
                id: "t2",
                title: "adopted",
                location: Location::Cwd(cwd.display().to_string()),
                created_ms: 1_000,
            },
        )
        .unwrap();
        let attempt = spawn_attempt(
            world,
            AttemptSpec {
                task: other,
                ownership: Ownership::Adopted,
                handle: handle("nonce-a", 8, None),
            },
        )
        .unwrap();
        (other, attempt)
    };
    let adopted = h.world().get::<AttemptOf>(adopted_attempt).unwrap().0;

    let (check, _) = h.run(task, "c1", "true");
    assert_eq!(
        checks::submit_check(h.world(), task, spec("c1", "true")),
        Ok(check),
        "an identical retry reads history"
    );
    assert!(matches!(
        checks::submit_check(h.world(), task, spec("c1", "false")),
        Err(CheckError::Conflict(_))
    ));
    let mut other_timeout = spec("c1", "true");
    other_timeout.timeout_ms = 1;
    assert!(matches!(
        checks::submit_check(h.world(), task, other_timeout),
        Err(CheckError::Conflict(_))
    ));
    assert!(matches!(
        checks::submit_check(h.world(), adopted, spec("c1", "true")),
        Err(CheckError::Conflict(_))
    ));
    assert_eq!(h.world().resource::<Ids>().checks.len(), 1);
    // The command ran once: one result.
    assert_eq!(h.world().get::<Results>(check).unwrap().len(), 1);

    // Invariant 17: adopted attempts grant no check authority.
    is_refused(
        checks::submit_check(h.world(), adopted, spec("c2", "true")),
        "adopted",
    );
    let mut bad_timeout = spec("c3", "true");
    bad_timeout.timeout_ms = 300_001;
    is_refused(
        checks::submit_check(h.world(), task, bad_timeout),
        "timeout",
    );
    assert!(matches!(
        checks::submit_check(h.world(), task, spec("bad id!", "true")),
        Err(CheckError::Model(_))
    ));
    h.step();
}

#[test]
fn policies_seal_at_the_first_submission_or_retained_artifact() {
    let mut h = Harness::new();
    let cwd = h._dir.path().to_path_buf();
    let (task, attempt) = h.task("t1", &cwd);
    let argv = |s: &str| vec!["sh".to_owned(), "-c".to_owned(), s.to_owned()];

    checks::require_check(h.world(), task, "tests", argv("true")).unwrap();
    checks::require_check(h.world(), task, "tests", argv("true")).unwrap();
    assert!(matches!(
        checks::require_check(h.world(), task, "tests", argv("false")),
        Err(CheckError::Conflict(_))
    ));
    checks::require_artifact(h.world(), task, "report", "out/report.txt").unwrap();
    assert!(matches!(
        checks::require_artifact(h.world(), task, "report", "other.txt"),
        Err(CheckError::Conflict(_))
    ));
    h.step();
    assert!(!h.world().get::<CheckPolicy>(task).unwrap().sealed);

    // A requirement must match its declared argv exactly.
    let mut wrong = spec("c-wrong", "false");
    wrong.requirement = Some("tests".into());
    is_refused(
        checks::submit_check(h.world(), task, wrong),
        "required name",
    );
    let mut undeclared = spec("c-undeclared", "true");
    undeclared.requirement = Some("lint".into());
    is_refused(
        checks::submit_check(h.world(), task, undeclared),
        "required name",
    );

    // A diagnostic submission seals both policies (CHECKS.md:35-36, ARTIFACTS.md:22-23).
    let (_, state) = h.run(task, "c-diag", "true");
    assert_eq!(state, CheckState::Passed);
    assert!(h.world().get::<CheckPolicy>(task).unwrap().sealed);
    assert!(h.world().get::<ArtifactPolicy>(task).unwrap().sealed);
    is_refused(
        checks::require_check(h.world(), task, "lint", argv("true")),
        "sealed",
    );
    is_refused(
        checks::require_artifact(h.world(), task, "log", "out/log.txt"),
        "sealed",
    );
    // Exact declaration retries still read (ARTIFACTS.md:21-22, CHECKS.md:34-35).
    checks::require_check(h.world(), task, "tests", argv("true")).unwrap();
    checks::require_artifact(h.world(), task, "report", "out/report.txt").unwrap();
    assert_eq!(
        checks::requirement_status(h.world(), task, "tests"),
        RequirementStatus::Missing,
        "diagnostic checks do not satisfy requirements"
    );

    // A second task: the first retained artifact seals only the artifact policy.
    let (task2, attempt2) = h.task("t2", &cwd);
    std::fs::create_dir_all(cwd.join("out")).unwrap();
    std::fs::write(cwd.join("out/report.txt"), b"bytes\n").unwrap();
    checks::require_artifact(h.world(), task2, "report", "out/report.txt").unwrap();
    let artifact = checks::retain_artifact(
        h.world(),
        attempt2,
        ArtifactSpec {
            id: "a1".into(),
            path: "out/report.txt".into(),
            requirement: Some("report".into()),
        },
    )
    .unwrap();
    h.step();
    assert_eq!(
        h.world().get::<ArtifactBytes>(artifact).unwrap().0,
        b"bytes\n"
    );
    assert_eq!(
        h.world().get::<ArtifactState>(artifact),
        Some(&ArtifactState::Collected)
    );
    assert!(h.world().get::<ArtifactPolicy>(task2).unwrap().sealed);
    assert!(
        h.world()
            .get::<CheckPolicy>(task2)
            .is_none_or(|p| !p.sealed)
    );
    checks::require_check(h.world(), task2, "tests", argv("true")).unwrap();
    // Retry reads; different intent conflicts; a wrong requirement path is refused.
    std::fs::write(cwd.join("out/report.txt"), b"changed\n").unwrap();
    let again = checks::retain_artifact(
        h.world(),
        attempt2,
        ArtifactSpec {
            id: "a1".into(),
            path: "out/report.txt".into(),
            requirement: Some("report".into()),
        },
    )
    .unwrap();
    assert_eq!(again, artifact);
    assert_eq!(
        h.world().get::<ArtifactBytes>(artifact).unwrap().0,
        b"bytes\n"
    );
    assert!(matches!(
        checks::retain_artifact(
            h.world(),
            attempt2,
            ArtifactSpec {
                id: "a1".into(),
                path: "out/report.txt".into(),
                requirement: None,
            },
        ),
        Err(CheckError::Conflict(_))
    ));
    is_refused(
        checks::retain_artifact(
            h.world(),
            attempt2,
            ArtifactSpec {
                id: "a2".into(),
                path: "out/other.txt".into(),
                requirement: Some("report".into()),
            },
        ),
        "required name",
    );
    is_refused(
        checks::retain_artifact(
            h.world(),
            attempt,
            ArtifactSpec {
                id: "a3".into(),
                path: "../escape".into(),
                requirement: None,
            },
        ),
        "relative",
    );
    // A missing file publishes no record.
    is_refused(
        checks::retain_artifact(
            h.world(),
            attempt,
            ArtifactSpec {
                id: "a4".into(),
                path: "out/missing.txt".into(),
                requirement: None,
            },
        ),
        "missing.txt",
    );
    assert!(h.world().resource::<Ids>().artifact("a4").is_none());
    h.step();
}

#[test]
fn requirement_status_is_the_latest_submission_by_generation() {
    let mut h = Harness::new();
    let cwd = h._dir.path().to_path_buf();
    let (task, _) = h.task("t1", &cwd);
    let argv = |s: &str| vec!["sh".to_owned(), "-c".to_owned(), s.to_owned()];
    checks::require_check(h.world(), task, "tests", argv("exit $(cat code)")).unwrap();
    checks::require_check(
        h.world(),
        task,
        "race",
        argv("if mkdir race-lock 2>/dev/null; then sleep 1; exit 0; else exit 1; fi"),
    )
    .unwrap();
    std::fs::write(cwd.join("code"), "0").unwrap();
    let required = |id: &str| {
        let mut s = spec(id, "exit $(cat code)");
        s.requirement = Some("tests".into());
        s
    };

    let first = checks::submit_check(h.world(), task, required("r1")).unwrap();
    assert_eq!(
        checks::requirement_status(h.world(), task, "tests"),
        RequirementStatus::Submitted(first)
    );
    assert_eq!(h.settle(first), CheckState::Passed);
    assert_eq!(
        checks::requirement_status(h.world(), task, "tests"),
        RequirementStatus::Passed(first)
    );

    // A newer failing execution supersedes the older pass ...
    std::fs::write(cwd.join("code"), "1").unwrap();
    let second = checks::submit_check(h.world(), task, required("r2")).unwrap();
    assert_eq!(h.settle(second), CheckState::Failed);
    assert_eq!(
        checks::requirement_status(h.world(), task, "tests"),
        RequirementStatus::Failed(second)
    );
    assert!(
        h.world().get::<CreatedGeneration>(second).unwrap().0
            > h.world().get::<CreatedGeneration>(first).unwrap().0
    );

    // ... even when the older one finishes later (CHECKS.md:42-43): the first `race`
    // execution to start takes the lock, sleeps and passes; the next fails at once.
    const RACE: &str = "if mkdir race-lock 2>/dev/null; then sleep 1; exit 0; else exit 1; fi";
    let race = |id: &str| {
        let mut s = spec(id, RACE);
        s.requirement = Some("race".into());
        s
    };
    let slow = checks::submit_check(h.world(), task, race("r3")).unwrap();
    h.step_until(Duration::from_secs(5), |world| {
        world.get::<CheckState>(slow) == Some(&CheckState::Running)
    });
    std::thread::sleep(Duration::from_millis(300));
    let quick = checks::submit_check(h.world(), task, race("r4")).unwrap();
    assert_eq!(h.settle(quick), CheckState::Failed);
    assert_eq!(
        h.world().get::<CheckState>(slow),
        Some(&CheckState::Running)
    );
    assert_eq!(
        checks::requirement_status(h.world(), task, "race"),
        RequirementStatus::Failed(quick)
    );
    assert_eq!(h.settle(slow), CheckState::Passed);
    assert_eq!(
        checks::requirement_status(h.world(), task, "race"),
        RequirementStatus::Failed(quick),
        "a late older pass cannot replace a newer failure"
    );

    // Two submissions in one update get strictly ordered generations.
    let third = checks::submit_check(h.world(), task, required("r5")).unwrap();
    let fourth = checks::submit_check(h.world(), task, required("r6")).unwrap();
    let g3 = h.world().get::<CreatedGeneration>(third).unwrap().0;
    let g4 = h.world().get::<CreatedGeneration>(fourth).unwrap().0;
    assert!(g4 > g3);
    h.settle(fourth);
    h.settle(third);
    assert_eq!(
        checks::requirement_status(h.world(), task, "tests"),
        RequirementStatus::Failed(fourth)
    );
    let diagnostic = h.submit(task, "d1", "true");
    h.settle(diagnostic);
    assert_eq!(
        checks::requirement_status(h.world(), task, "tests").latest(),
        Some(fourth),
        "diagnostic checks never count"
    );
}

/// The verified scenario every seal test builds: a repo with one commit as the task cwd, one
/// required check `tests` and one required artifact `report` captured by that check.
struct Verified {
    task: Entity,
    attempt: Entity,
    source: Entity,
    other_source: Entity,
    foreign_source: Entity,
}

fn verified_scenario(h: &mut Harness) -> Verified {
    let repo = repo(h._dir.path());
    let (task, attempt) = h.task("t1", &repo);
    let (other, _) = h.task("t-other", &repo);
    let argv = vec![
        "sh".to_owned(),
        "-c".to_owned(),
        "mkdir -p out && printf report > out/report.txt".to_owned(),
    ];
    checks::require_check(h.world(), task, "tests", argv).unwrap();
    checks::require_artifact(h.world(), task, "report", "out/report.txt").unwrap();
    let source = h.retain(task, "s1");
    assert_eq!(
        h.world().get::<SourceState>(source),
        Some(&SourceState::Retained { dirty: false })
    );
    let commit = h
        .world()
        .get::<SourceRevision>(source)
        .unwrap()
        .commit
        .clone();
    assert_eq!(commit.len(), 40, "{commit}");
    std::fs::write(repo.join("tracked.txt"), "v2\n").unwrap();
    let other_source = h.retain(task, "s2");
    assert_eq!(
        h.world().get::<SourceState>(other_source),
        Some(&SourceState::Retained { dirty: true })
    );
    let foreign_source = h.retain(other, "s-foreign");
    Verified {
        task,
        attempt,
        source,
        other_source,
        foreign_source,
    }
}

fn required_on(source: Entity, id: &str, artifact: &str) -> CheckSpec {
    CheckSpec {
        id: id.into(),
        argv: vec![
            "sh".into(),
            "-c".into(),
            "mkdir -p out && printf report > out/report.txt".into(),
        ],
        timeout_ms: 5_000,
        requirement: Some("tests".into()),
        source: Some(source),
        artifacts: vec![CaptureRequest {
            name: "report".into(),
            artifact: artifact.into(),
        }],
    }
}

#[test]
fn sources_retain_a_commit_with_dirty_status_and_retry_idempotently() {
    let mut h = Harness::new();
    let v = verified_scenario(&mut h);
    let again = checks::retain_source(
        h.world(),
        v.task,
        SourceSpec {
            id: "s1".into(),
            expression: "HEAD".into(),
        },
    )
    .unwrap();
    assert_eq!(again, v.source);
    assert!(matches!(
        checks::retain_source(
            h.world(),
            v.task,
            SourceSpec {
                id: "s1".into(),
                expression: "HEAD~1".into(),
            },
        ),
        Err(CheckError::Conflict(_))
    ));
    is_refused(
        checks::retain_source(
            h.world(),
            v.task,
            SourceSpec {
                id: "s3".into(),
                expression: "--output=/tmp/x".into(),
            },
        ),
        "expression",
    );
    // An unresolvable revision fails and is never re-resolved under its id.
    let bad = checks::retain_source(
        h.world(),
        v.task,
        SourceSpec {
            id: "s-bad".into(),
            expression: "no-such-ref".into(),
        },
    )
    .unwrap();
    h.step_until(Duration::from_secs(20), |world| {
        *world.get::<SourceState>(bad).unwrap() != SourceState::Resolving
    });
    assert_eq!(
        h.world().get::<SourceState>(bad),
        Some(&SourceState::Failed)
    );
    assert!(h.world().get::<Problem>(bad).is_some());
    let mut on_failed = required_on(bad, "c-on-failed", "a-x");
    on_failed.artifacts.clear();
    is_refused(
        checks::submit_check(h.world(), v.task, on_failed),
        "not retained",
    );
    let mut foreign = required_on(v.foreign_source, "c-foreign", "a-y");
    foreign.artifacts.clear();
    is_refused(
        checks::submit_check(h.world(), v.task, foreign),
        "another task",
    );
    let mut no_source = required_on(v.source, "c-no-source", "a-z");
    no_source.source = None;
    is_refused(
        checks::submit_check(h.world(), v.task, no_source),
        "source-bound",
    );
    let mut undeclared = required_on(v.source, "c-undeclared", "a-u");
    undeclared.artifacts[0].name = "log".into();
    is_refused(
        checks::submit_check(h.world(), v.task, undeclared),
        "not declared",
    );
    assert!(
        h.world().resource::<Ids>().artifact("a-u").is_none(),
        "nothing reserved"
    );
}

#[test]
fn verification_seals_only_coherent_evidence_and_refuses_the_rest() {
    let mut h = Harness::new();
    let v = verified_scenario(&mut h);
    let verify =
        |h: &mut Harness, source: Entity| checks::verify(h.world(), v.task, VerifySpec { source });

    is_refused(verify(&mut h, v.source), "required check tests is missing");
    is_refused(verify(&mut h, v.foreign_source), "another task");

    // An outstanding check blocks verification.
    let slow = h.submit(v.task, "c-slow", "sleep 1");
    is_refused(verify(&mut h, v.source), "outstanding");
    h.settle(slow);

    // The requirement passed on another source.
    let on_other = checks::submit_check(
        h.world(),
        v.task,
        required_on(v.other_source, "c-s2", "a-s2"),
    )
    .unwrap();
    assert_eq!(h.settle(on_other), CheckState::Passed);
    let captured = captured_by(h.world(), on_other);
    assert_eq!(captured.len(), 1);
    assert_eq!(
        h.world().get::<ArtifactBytes>(captured[0]).unwrap().0,
        b"report"
    );
    assert_eq!(
        h.world().get::<CapturedBy>(captured[0]).unwrap().0,
        on_other
    );
    is_refused(
        verify(&mut h, v.source),
        "did not run on the selected source",
    );

    // A pass on the selected source whose capture failed: the output path is a symlink, which
    // the command writes through and the no-follow capture refuses (ARTIFACTS.md:43-45).
    let repo = h._dir.path().join("repo");
    std::fs::create_dir_all(repo.join("out")).unwrap();
    let _ = std::fs::remove_file(repo.join("out/report.txt"));
    std::os::unix::fs::symlink(h._dir.path().join("elsewhere"), repo.join("out/report.txt"))
        .unwrap();
    let broken = checks::submit_check(
        h.world(),
        v.task,
        required_on(v.source, "c-broken", "a-broken"),
    )
    .unwrap();
    assert_eq!(h.settle(broken), CheckState::Passed);
    let reserved = captured_by(h.world(), broken);
    assert_eq!(
        h.world().get::<ArtifactState>(reserved[0]),
        Some(&ArtifactState::Failed)
    );
    assert!(h.world().get::<Problem>(reserved[0]).is_some());
    is_refused(verify(&mut h, v.source), "capture failure");
    std::fs::remove_dir_all(repo.join("out")).unwrap();
    // A manual collection of the required artifact does not satisfy it either.
    let good =
        checks::submit_check(h.world(), v.task, required_on(v.source, "c-good", "a-good")).unwrap();
    assert_eq!(h.settle(good), CheckState::Passed);
    let manual = checks::retain_artifact(
        h.world(),
        v.attempt,
        ArtifactSpec {
            id: "a-manual".into(),
            path: "out/report.txt".into(),
            requirement: Some("report".into()),
        },
    )
    .unwrap();
    let captured_good = captured_by(h.world(), good)[0];
    assert!(
        h.world().get::<CreatedGeneration>(manual).unwrap().0
            > h.world().get::<CreatedGeneration>(captured_good).unwrap().0
    );
    is_refused(verify(&mut h, v.source), "not captured by a selected check");
    // A check-reserved id cannot be claimed manually.
    assert!(matches!(
        checks::retain_artifact(
            h.world(),
            v.attempt,
            ArtifactSpec {
                id: "a-good".into(),
                path: "out/report.txt".into(),
                requirement: Some("report".into()),
            },
        ),
        Err(CheckError::Conflict(_))
    ));

    // A newer capture by a passed check on the selected source: coherent.
    let latest = checks::submit_check(
        h.world(),
        v.task,
        required_on(v.source, "c-latest", "a-latest"),
    )
    .unwrap();
    assert_eq!(h.settle(latest), CheckState::Passed);
    let artifact = captured_by(h.world(), latest)[0];
    let before = h.world().resource::<Generation>().0;
    let seal = verify(&mut h, v.source).unwrap();
    assert_eq!(seal.source, v.source);
    assert_eq!(seal.checks, vec![latest]);
    assert_eq!(seal.artifacts, vec![artifact]);
    assert_eq!(seal.generation, before + 1);
    assert_eq!(
        h.world().get::<TaskState>(v.task),
        Some(&TaskState::Closed {
            outcome: TaskOutcome::Verified
        })
    );
    assert_eq!(h.world().get::<Seal>(v.task), Some(&seal));
    assert!(h.world().get::<ClosedMs>(v.task).is_some());
    h.step();
    assert_eq!(checks::validate_seal(h.world(), v.task), Ok(()));

    // Invariant 23: sealed means no new evidence, no cancellation; same-source verify reads.
    assert_eq!(verify(&mut h, v.source), Ok(seal));
    is_refused(verify(&mut h, v.other_source), "another source");
    is_refused(
        checks::submit_check(h.world(), v.task, spec("c-after", "true")),
        "sealed",
    );
    is_refused(
        checks::retain_source(
            h.world(),
            v.task,
            SourceSpec {
                id: "s-after".into(),
                expression: "HEAD".into(),
            },
        ),
        "sealed",
    );
    is_refused(
        checks::retain_artifact(
            h.world(),
            v.attempt,
            ArtifactSpec {
                id: "a-after".into(),
                path: "out/report.txt".into(),
                requirement: None,
            },
        ),
        "sealed",
    );
    is_refused(
        checks::require_check(h.world(), v.task, "more", vec!["true".into()]),
        "sealed",
    );
    assert!(zor::lifecycle::cancel_task(h.world(), v.task).is_err());
    assert!(close_task(h.world(), v.task, TaskOutcome::Cancelled, 5).is_err());
    h.step();
}

#[test]
fn journal_round_trip_keeps_results_seals_and_marks_lost_checks_uncertain() {
    let mut h = Harness::new();
    let v = verified_scenario(&mut h);
    let good =
        checks::submit_check(h.world(), v.task, required_on(v.source, "c-good", "a-good")).unwrap();
    assert_eq!(h.settle(good), CheckState::Passed);
    let seal = checks::verify(h.world(), v.task, VerifySpec { source: v.source }).unwrap();
    h.step();

    // A second task with a check whose reply never arrives before the restart, and a source
    // whose git reply is lost.
    let cwd2 = h._dir.path().to_path_buf();
    let (task2, _) = h.task("t2", &cwd2);
    let lost = h.submit(task2, "c-lost", "sleep 30");
    let lost_source = checks::retain_source(
        h.world(),
        task2,
        SourceSpec {
            id: "s-lost".into(),
            expression: "HEAD".into(),
        },
    )
    .unwrap();
    let _ = lost_source;
    h.step();
    assert_eq!(
        h.world().get::<CheckState>(lost),
        Some(&CheckState::Running)
    );
    let generation = h.world().resource::<Generation>().0;

    let mut h = h.restart();
    let world = h.world();
    assert_eq!(check_invariants(world), Ok(()));
    assert!(
        world.resource::<Generation>().0 >= generation,
        "generations never regress"
    );
    let ids = world.resource::<Ids>().clone();
    let task = ids.task("t1").unwrap();
    let restored = world.get::<Seal>(task).cloned().unwrap();
    assert_eq!(restored.sealed_ms, seal.sealed_ms);
    assert_eq!(restored.generation, seal.generation);
    assert_eq!(restored.source, ids.source("s1").unwrap());
    assert_eq!(restored.checks, vec![ids.check("c-good").unwrap()]);
    assert_eq!(restored.artifacts, vec![ids.artifact("a-good").unwrap()]);
    assert_eq!(checks::validate_seal(world, task), Ok(()));
    assert_eq!(
        world
            .get::<ArtifactBytes>(ids.artifact("a-good").unwrap())
            .unwrap()
            .0,
        b"report"
    );
    assert_eq!(
        world
            .get::<CapturedBy>(ids.artifact("a-good").unwrap())
            .unwrap()
            .0,
        ids.check("c-good").unwrap()
    );
    assert_eq!(
        world.get::<SourceState>(ids.source("s2").unwrap()),
        Some(&SourceState::Retained { dirty: true })
    );
    assert!(world.get::<CheckPolicy>(task).unwrap().sealed);
    let good = ids.check("c-good").unwrap();
    assert_eq!(world.get::<CheckState>(good), Some(&CheckState::Passed));
    assert_eq!(output(world, good).exit_code, Some(0));
    assert_eq!(
        checks::requirement_status(world, task, "tests"),
        RequirementStatus::Passed(good)
    );

    // Invariant 27: the lost check is Uncertain with a result and never replayed.
    let lost = ids.check("c-lost").unwrap();
    assert_eq!(world.get::<CheckState>(lost), Some(&CheckState::Uncertain));
    assert_eq!(world.get::<Results>(lost).unwrap().len(), 1);
    assert!(world.get::<Problem>(lost).unwrap().0.contains("restart"));
    assert_eq!(
        world.get::<SourceState>(ids.source("s-lost").unwrap()),
        Some(&SourceState::Failed)
    );
    let effects = h.step();
    assert!(effects.is_empty(), "no replay: {effects:?}");
    // The next submission orders after every restored record.
    let task2 = h.world().resource::<Ids>().task("t2").unwrap();
    let next = h.submit(task2, "c-next", "true");
    let lost_generation = h.world().get::<CreatedGeneration>(lost).unwrap().0;
    assert!(h.world().get::<CreatedGeneration>(next).unwrap().0 > lost_generation);
    h.settle(next);
}

#[test]
fn a_tampered_seal_is_rejected_on_load() {
    let mut h = Harness::new();
    let v = verified_scenario(&mut h);
    let good =
        checks::submit_check(h.world(), v.task, required_on(v.source, "c-good", "a-good")).unwrap();
    assert_eq!(h.settle(good), CheckState::Passed);
    checks::verify(h.world(), v.task, VerifySpec { source: v.source }).unwrap();
    h.step();
    // Structural invariant 4 is part of the restore; the deeper policy check refuses a seal
    // whose artifacts no longer match its checks' captures.
    let world = h.world();
    let mut seal = world.get::<Seal>(v.task).cloned().unwrap();
    seal.artifacts.clear();
    world.entity_mut(v.task).insert(seal);
    assert!(checks::validate_seal(world, v.task).is_err());
}

#[test]
fn check_methods_over_http_require_submit_inspect_and_report_results() {
    let server = common::Server::start();
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().display().to_string();
    std::fs::write(dir.path().join("report.txt"), b"bytes").unwrap();
    server.with_world(move |world| {
        let task = spawn_task(
            world,
            TaskSpec {
                id: "review-1",
                title: "review",
                location: Location::Cwd(cwd),
                created_ms: 1_000,
            },
        )
        .unwrap();
        spawn_attempt(
            world,
            AttemptSpec {
                task,
                ownership: Ownership::Managed,
                handle: handle("nonce-a", 7, Some(4242)),
            },
        )
        .unwrap();
    });
    let call = |method: &str, params: serde_json::Value| server.call(method, params).unwrap();
    let policy = call(
        "zor/check.require",
        serde_json::json!({ "task": "review-1", "name": "tests", "argv": ["sh", "-c", "true"] }),
    );
    assert_eq!(policy["required_checks"][0]["status"], "missing");
    assert_eq!(policy["checks_sealed"], false);
    let policy = call(
        "zor/artifact.require",
        serde_json::json!({ "task": "review-1", "name": "report", "path": "report.txt" }),
    );
    assert_eq!(policy["required_artifacts"][0]["status"], "missing");

    // Mutations need the instance nonce (the client injects it); a read-only token cannot.
    let artifact = call(
        "zor/artifact.retain",
        serde_json::json!({ "task": "review-1", "artifact": "report-1", "path": "report.txt", "requirement": "report" }),
    );
    assert_eq!(artifact["state"], "collected");
    assert_eq!(artifact["bytes"], 5);
    let fetched = call(
        "zor/artifact.fetch",
        serde_json::json!({ "artifact": "report-1" }),
    );
    assert_eq!(
        fetched["bytes"],
        serde_json::json!([98, 121, 116, 101, 115])
    );
    assert_eq!(
        common::code(server.call(
            "zor/artifact.require",
            serde_json::json!({ "task": "review-1", "name": "log", "path": "log.txt" }),
        )),
        zor::remote::methods::codes::INVALID,
        "artifact policy sealed by the first retained artifact"
    );

    // No check adapter behind this server: the check stays Running and the result view
    // reports it as the pending requirement.
    let submitted = call(
        "zor/check.submit",
        serde_json::json!({ "task": "review-1", "check": "tests-1", "argv": ["sh", "-c", "true"], "requirement": "tests" }),
    );
    assert_eq!(submitted["state"], "queued");
    assert_eq!(submitted["timeout_ms"], checks::DEFAULT_TIMEOUT_MS);
    let again = call(
        "zor/check.submit",
        serde_json::json!({ "task": "review-1", "check": "tests-1", "argv": ["sh", "-c", "true"], "requirement": "tests" }),
    );
    assert_eq!(again["check"], "tests-1");
    assert_eq!(
        common::code(server.call(
            "zor/check.submit",
            serde_json::json!({ "task": "review-1", "check": "tests-1", "argv": ["sh", "-c", "false"] }),
        )),
        zor::remote::methods::codes::INVALID
    );
    assert_eq!(
        common::code(server.call("zor/check.inspect", serde_json::json!({ "check": "nope" }))),
        zor::remote::methods::codes::NOT_FOUND
    );
    std::thread::sleep(Duration::from_millis(50));
    let inspected = call(
        "zor/check.inspect",
        serde_json::json!({ "check": "tests-1" }),
    );
    assert_eq!(inspected["state"], "running");
    assert_eq!(inspected["passed"], serde_json::Value::Null);
    let list = call("zor/check.list", serde_json::json!({ "task": "review-1" }));
    assert_eq!(list["checks"][0]["check"], "tests-1");
    let results = call(
        "zor/task.results",
        serde_json::json!({ "task": "review-1" }),
    );
    assert_eq!(results["state"], "open");
    assert_eq!(
        results["policy"]["required_checks"][0]["status"],
        "submitted"
    );
    assert_eq!(
        results["policy"]["required_artifacts"][0]["status"],
        "collected"
    );
    assert_eq!(results["verification"]["status"], "unverified");
    assert_eq!(
        results["blockers"],
        serde_json::json!(["required-check-submitted:tests"])
    );
    assert_eq!(
        common::code(server.call(
            "zor/task.verify",
            serde_json::json!({ "task": "review-1", "source": "none" }),
        )),
        zor::remote::methods::codes::NOT_FOUND
    );
    let source = call(
        "zor/source.retain",
        serde_json::json!({ "task": "review-1", "source": "rev-1", "expression": "HEAD" }),
    );
    assert_eq!(source["state"], "resolving");
    let sources = call("zor/source.list", serde_json::json!({ "task": "review-1" }));
    assert_eq!(sources["sources"][0]["source"], "rev-1");
    assert_eq!(
        common::code(server.call(
            "zor/task.verify",
            serde_json::json!({ "task": "review-1", "source": "rev-1" }),
        )),
        zor::remote::methods::codes::INVALID
    );
    server.with_world(|world| assert_eq!(check_invariants(world), Ok(())));
}
