//! Explicit remote supervision uses the observed attempt under the journal's exclusive lock.
use super::{
    model::{Journal, Target},
    store::Store,
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expected {
    pub task: String,
    pub attempt: String,
    pub session: String,
    pub target: Target,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    Cancel,
    Stop,
    Reconcile,
}
impl Expected {
    pub(super) fn validate(&self, journal: &Journal) -> Result<()> {
        let task = journal
            .tasks
            .get(&self.task)
            .context("selected task no longer exists")?;
        let attempt = journal
            .attempts
            .get(&task.attempt)
            .context("selected attempt missing")?;
        let session = journal
            .sessions
            .get(&attempt.session)
            .context("selected session missing")?;
        ensure!(
            task.attempt == self.attempt
                && attempt.task == self.task
                && attempt.session == self.session
                && session.target.identity() == self.target.identity(),
            "selected task/attempt/process changed; refresh before acting"
        );
        Ok(())
    }
}
pub(crate) fn run(root: &Path, expected: &Expected, action: Action) -> Result<Value> {
    let mut store = Store::open(root)?;
    // The lock stays held through the transition and any exact-target stop operation. No
    // other service worker or direct CLI process can replace this task after validation.
    expected.validate(store.journal())?;
    match action {
        Action::Cancel => {
            store.transaction(|journal| super::cancel_journal(journal, &expected.task))?;
            super::inspect_journal(store.journal(), &expected.task)
        }
        Action::Stop => super::stop::run_store(&mut store, &expected.task),
        Action::Reconcile => super::launch::reconcile_store(&mut store, &expected.task),
    }
}

/// Read-only authoritative route resolution for a selected task attempt.
pub(crate) fn attachment(root: &Path, expected: &Expected) -> Result<Value> {
    Ok(serde_json::to_value(super::attachment::resolve(
        root, expected,
    )?)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::model::*;
    #[test]
    fn stale_attempt_or_process_cannot_cancel_replacement_and_cancel_preserves_session()
    -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("state");
        let target = Target {
            runtime: "/remote".into(),
            instance: "fux-one".into(),
            workspace: "default".into(),
            stream: 1,
            pane: 2,
            pid: Some(3),
            origin: None,
        };
        {
            let mut store = Store::open(&root)?;
            store.transaction(|journal| {
                journal.tasks.insert(
                    "same".into(),
                    Task {
                        id: "same".into(),
                        requested_runtime: "/remote".into(),
                        title: "first".into(),
                        created_ms: 1,
                        outcome: TaskOutcome::Open,
                        attempt: "attempt-one".into(),
                        required_checks: Default::default(),
                        required_artifacts: Default::default(),
                    },
                );
                journal.attempts.insert(
                    "attempt-one".into(),
                    Attempt {
                        id: "attempt-one".into(),
                        task: "same".into(),
                        session: "session-one".into(),
                        state: AttemptState::Active,
                    },
                );
                journal.sessions.insert(
                    "session-one".into(),
                    Session {
                        id: "session-one".into(),
                        target: target.clone(),
                        agent: None,
                        ownership: Ownership::Adopted,
                        launch: None,
                        created_ms: 1,
                    },
                );
                Ok(())
            })?;
        }
        let expected = Expected {
            task: "same".into(),
            attempt: "attempt-one".into(),
            session: "session-one".into(),
            target,
        };
        let before = std::fs::read(root.join("journal.json"))?;
        let mut wrong_attempt = expected.clone();
        wrong_attempt.attempt = "old-attempt".into();
        let mut wrong_process = expected.clone();
        wrong_process.target.pid = Some(4);
        for stale in [wrong_attempt, wrong_process] {
            assert!(run(&root, &stale, Action::Cancel).is_err());
            let failure = super::super::resume::guarded(&root, &stale, "resume-one", "new-server")
                .err()
                .context("stale resume must fail")?;
            assert!(
                failure
                    .to_string()
                    .contains("selected task/attempt/process changed")
            );
            assert_eq!(std::fs::read(root.join("journal.json"))?, before);
        }
        assert!(
            super::super::resume::guarded(&root, &expected, "resume-one", "new-server").is_err()
        );
        assert_eq!(std::fs::read(root.join("journal.json"))?, before);
        // Adoption never grants process termination, even with the correct observed identity.
        assert!(run(&root, &expected, Action::Stop).is_err());
        assert_eq!(std::fs::read(root.join("journal.json"))?, before);
        let reply = run(&root, &expected, Action::Cancel)?;
        assert_eq!(
            reply.pointer("/task/outcome"),
            Some(&serde_json::json!("cancelled"))
        );
        assert_eq!(
            reply.pointer("/session/target/pid"),
            Some(&serde_json::json!(3))
        );
        Ok(())
    }
}
