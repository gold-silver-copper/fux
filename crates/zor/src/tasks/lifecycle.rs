//! Journal-only lifecycle transitions. Call these inside `Store::transaction`;
//! external effects and evidence authentication remain the caller's responsibility.
use super::model::*;
use anyhow::{Context, Result, ensure};

mod attachment;
mod worktree;
pub(super) use attachment::Attachment;

impl Launch {
    /// Commit before creation. An ambiguous creation is reconciled, never resent.
    pub(super) fn begin_submission(&mut self) -> Result<()> {
        ensure!(
            self.phase == LaunchPhase::Prepared,
            "launch already submitted"
        );
        ensure!(
            self.pane.is_none() && self.session.is_none(),
            "prepared launch already owns a pane"
        );
        self.phase = LaunchPhase::Submitting;
        Ok(())
    }

    pub(super) fn record_created_pane(&mut self, pane: u32) -> Result<()> {
        ensure!(
            self.phase == LaunchPhase::Submitting,
            "creation reply outside submission"
        );
        ensure!(
            pane != 0 && self.pane.is_none_or(|existing| existing == pane),
            "creation reply changed pane identity"
        );
        self.pane = Some(pane);
        Ok(())
    }

    pub(super) fn creation_uncertain(&mut self, problem: &str) {
        // A rename followed by a directory-sync error may already have committed
        // attachment. Never erase the stronger record when reporting that error.
        if !matches!(self.phase, LaunchPhase::Attached | LaunchPhase::Closed) {
            self.phase = LaunchPhase::Uncertain;
            self.problem = Some(problem.chars().take(128).collect());
        }
    }
}

pub(super) enum AttachedObservation {
    Live,
    Unavailable { replacement: bool, problem: String },
    Final(LaunchFinal),
}

pub(super) fn unavailable_state(
    previous: &AttemptState,
    replacement: bool,
    problem: &str,
) -> (AttemptState, String) {
    if replacement || *previous == AttemptState::Lost {
        (AttemptState::Lost, "fux server incarnation replaced; original process ownership lost; no command or prompt replayed".into())
    } else {
        (AttemptState::Uncertain, problem.chars().take(128).collect())
    }
}

impl Prompt {
    /// Publish authenticated receipt evidence without changing response/wait
    /// evidence. Submission intent uses the existing reservation verbatim.
    pub(super) fn record_delivery(&mut self, phase: Delivery, receipt: Receipt) -> Result<()> {
        let valid_phase = match self.delivery {
            Delivery::Prepared => phase == Delivery::Reserved,
            Delivery::Reserved => matches!(
                phase,
                Delivery::Reserved
                    | Delivery::Submitting
                    | Delivery::Queued
                    | Delivery::Delivered
                    | Delivery::Failed
            ),
            Delivery::Submitting | Delivery::Uncertain => matches!(
                phase,
                Delivery::Reserved | Delivery::Queued | Delivery::Delivered | Delivery::Failed
            ),
            Delivery::Queued => matches!(
                phase,
                Delivery::Queued | Delivery::Delivered | Delivery::Failed
            ),
            Delivery::Delivered => phase == Delivery::Delivered,
            Delivery::Failed => phase == Delivery::Failed,
        };
        ensure!(valid_phase, "invalid prompt delivery transition");
        ensure!(
            receipt.operation != 0 && receipt.bytes_written <= self.text.len() + 1,
            "invalid delivery receipt"
        );
        ensure!(
            (!matches!(phase, Delivery::Reserved | Delivery::Submitting)
                || receipt.bytes_written == 0)
                && (phase != Delivery::Delivered || receipt.bytes_written == self.text.len() + 1),
            "invalid delivery byte evidence"
        );
        if let Some(old) = &self.receipt {
            ensure!(
                old.operation == receipt.operation
                    && old.expires_server_ms == receipt.expires_server_ms
                    && old.bytes_written <= receipt.bytes_written
                    && old.input_sequence <= receipt.input_sequence,
                "delivery receipt changed identity or regressed evidence"
            );
            if phase == Delivery::Submitting {
                ensure!(
                    old.revision == receipt.revision
                        && old.input_sequence == receipt.input_sequence,
                    "submission intent changed reservation"
                );
            }
        } else {
            ensure!(
                self.delivery == Delivery::Prepared,
                "delivery reservation missing"
            );
        }
        if phase == Delivery::Submitting
            && let Some(arm) = &mut self.arm
        {
            arm.input_started = true;
        }
        self.delivery = phase;
        self.receipt = Some(receipt);
        Ok(())
    }

    pub(super) fn delivery_uncertain(&mut self) {
        if self.receipt.is_some()
            && !matches!(self.delivery, Delivery::Delivered | Delivery::Failed)
        {
            self.delivery = Delivery::Uncertain;
            if !self.released && !super::wait::terminal(&self.wait) {
                self.wait = WaitOutcome::Uncertain;
            }
        }
    }
}

impl Journal {
    /// Validate all associations before changing either record. This cannot grant
    /// ownership, select another attempt, or change task/delivery outcomes.
    pub(super) fn observe_attached(
        &mut self,
        id: &str,
        attempt_id: &str,
        observation: AttachedObservation,
    ) -> Result<()> {
        let launch = self.launches.get(id).context("launch missing")?;
        ensure!(
            launch.phase == LaunchPhase::Attached,
            "launch is not attached"
        );
        let attempt = self.attempts.get(attempt_id).context("attempt missing")?;
        let session = self
            .sessions
            .get(&attempt.session)
            .context("session missing")?;
        let task = self.tasks.get(launch.task_id()).context("task missing")?;
        ensure!(
            launch.session.as_deref() == Some(session.id.as_str())
                && session.launch.as_deref() == Some(id)
                && session.ownership == Ownership::Managed
                && attempt.task == task.id
                && (launch.task.is_some() || task.attempt == attempt_id)
                && session.target.instance == launch.instance
                && launch.pane == Some(session.target.pane),
            "managed launch observation relationship mismatch"
        );
        if let AttachedObservation::Final(evidence) = &observation {
            ensure!(
                evidence.text.len() <= 4096,
                "launch final evidence exceeds limit"
            );
        }
        let launch = self
            .launches
            .get_mut(id)
            .context("validated launch missing")?;
        let attempt = self
            .attempts
            .get_mut(attempt_id)
            .context("validated attempt missing")?;
        match observation {
            AttachedObservation::Live => {
                launch.problem = None;
                if matches!(attempt.state, AttemptState::Uncertain | AttemptState::Lost) {
                    attempt.state = AttemptState::Active;
                }
            }
            AttachedObservation::Unavailable {
                replacement,
                problem,
            } => {
                let (state, problem) = unavailable_state(&attempt.state, replacement, &problem);
                attempt.state = state;
                launch.problem = Some(problem);
            }
            AttachedObservation::Final(evidence) => {
                launch.phase = LaunchPhase::Closed;
                launch.final_evidence = Some(evidence);
                launch.problem = None;
                attempt.state = AttemptState::Finished;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    fn launch() -> Launch {
        Launch {
            id: "task".into(),
            task: None,
            resume: None,
            title: "task".into(),
            agent: None,
            requested_runtime: "/runtime".into(),
            runtime: "/runtime".into(),
            requested_cwd: "/cwd".into(),
            cwd: "/cwd".into(),
            worktree: None,
            instance: "original".into(),
            workspace: "default".into(),
            stream: 1,
            event_sequence: 0,
            argv: vec!["sh".into()],
            marker: "a".repeat(32),
            integration: None,
            final_evidence: None,
            stop_requested: false,
            created_ms: 1,
            phase: LaunchPhase::Prepared,
            pane: None,
            session: None,
            problem: None,
        }
    }

    fn attached() -> Journal {
        let mut journal = Journal::default();
        let mut launch = launch();
        launch.phase = LaunchPhase::Attached;
        launch.pane = Some(1);
        launch.session = Some("session".into());
        journal.launches.insert("task".into(), launch);
        journal.sessions.insert(
            "session".into(),
            Session {
                id: "session".into(),
                target: Target {
                    runtime: "/runtime".into(),
                    instance: "original".into(),
                    workspace: "default".into(),
                    stream: 1,
                    pane: 1,
                    pid: Some(123),
                    origin: Some(Origin {
                        workspace: "default".into(),
                        stream: 1,
                    }),
                },
                agent: None,
                ownership: Ownership::Managed,
                launch: Some("task".into()),
                created_ms: 1,
            },
        );
        journal.attempts.insert(
            "attempt".into(),
            Attempt {
                id: "attempt".into(),
                task: "task".into(),
                session: "session".into(),
                state: AttemptState::Active,
            },
        );
        journal.tasks.insert(
            "task".into(),
            Task {
                id: "task".into(),
                requested_runtime: "/runtime".into(),
                title: "task".into(),
                created_ms: 1,
                outcome: TaskOutcome::Open,
                attempt: "attempt".into(),
                required_checks: Default::default(),
                required_artifacts: Default::default(),
            },
        );
        journal.validate().expect("valid managed fixture");
        journal
    }

    fn final_evidence() -> LaunchFinal {
        LaunchFinal {
            exit_status: Some(0),
            text: "done".into(),
            truncated: false,
            input_sequence: 7,
        }
    }

    #[test]
    fn attachment_records_once_and_final_attachment_does_not_claim_task_success() {
        for exited in [false, true] {
            let mut journal = Journal::default();
            let mut launch = launch();
            launch.begin_submission().expect("submission");
            journal.launches.insert("task".into(), launch);
            let attachment = || Attachment {
                session: "session".into(),
                attempt: "attempt".into(),
                pane: 1,
                pid: if exited { None } else { Some(123) },
                evidence: exited.then(final_evidence),
            };
            journal
                .attach_managed("task", attachment())
                .expect("attach");
            journal.validate().expect("valid attachment");
            assert_eq!(
                journal.tasks.get("task").expect("task").outcome,
                TaskOutcome::Open
            );
            let before = serde_json::to_value(&journal).expect("snapshot");
            assert!(journal.attach_managed("task", attachment()).is_err());
            assert_eq!(serde_json::to_value(&journal).expect("snapshot"), before);
        }
    }

    #[test]
    fn attachment_rejects_changed_pane_before_claiming_any_resources() {
        let mut journal = Journal::default();
        let mut launch = launch();
        launch.begin_submission().expect("submission");
        launch.record_created_pane(1).expect("pane reply");
        journal.launches.insert("task".into(), launch);
        let before = serde_json::to_value(&journal).expect("snapshot");
        assert!(
            journal
                .attach_managed(
                    "task",
                    Attachment {
                        session: "session".into(),
                        attempt: "attempt".into(),
                        pane: 2,
                        pid: Some(123),
                        evidence: None,
                    }
                )
                .is_err()
        );
        assert_eq!(serde_json::to_value(&journal).expect("snapshot"), before);
    }

    #[test]
    fn stop_intent_retires_coordination_without_inventing_process_exit() {
        let mut journal = attached();
        journal.prompts.insert("prompt".into(), prompt());
        let target = journal.managed_stop_target("task").expect("authority");
        journal.request_managed_stop("task").expect("stop intent");
        journal.validate().expect("valid stop intent");
        assert_eq!(
            journal.tasks.get("task").expect("task").outcome,
            TaskOutcome::Cancelled
        );
        assert_eq!(
            journal.prompts.get("prompt").expect("prompt").wait,
            WaitOutcome::Cancelled
        );
        assert_eq!(
            journal.launches.get("task").expect("launch").phase,
            LaunchPhase::Attached
        );
        assert!(
            journal
                .launches
                .get("task")
                .expect("launch")
                .final_evidence
                .is_none()
        );
        assert_eq!(
            journal.managed_stop_target("task").expect("same authority"),
            target
        );
        let before = serde_json::to_value(&journal).expect("snapshot");
        journal
            .request_managed_stop("task")
            .expect("idempotent intent");
        assert_eq!(serde_json::to_value(&journal).expect("snapshot"), before);
    }

    #[test]
    fn adopted_session_cannot_receive_stop_intent() {
        let mut journal = attached();
        journal
            .sessions
            .get_mut("session")
            .expect("session")
            .ownership = Ownership::Adopted;
        let before = serde_json::to_value(&journal).expect("snapshot");
        assert!(journal.request_managed_stop("task").is_err());
        assert_eq!(serde_json::to_value(&journal).expect("snapshot"), before);
    }

    fn prompt() -> Prompt {
        Prompt {
            id: "prompt".into(),
            attempt: "attempt".into(),
            text: "hello".into(),
            handoff: None,
            created_ms: 1,
            deadline_ms: 100,
            delivery: Delivery::Prepared,
            receipt: None,
            wait: WaitOutcome::Pending,
            released: false,
            report_token: None,
            response: None,
            report_binding: None,
            arm: None,
            wait_problem: None,
            wait_exit_status: None,
        }
    }

    fn receipt() -> Receipt {
        Receipt {
            operation: 7,
            revision: 1,
            input_sequence: 3,
            expires_server_ms: 100,
            bytes_written: 0,
        }
    }

    #[test]
    fn lost_submission_reconciles_the_same_reservation_without_resetting_arm() {
        let mut prompt = prompt();
        prompt
            .record_delivery(Delivery::Reserved, receipt())
            .expect("test fixture or transition");
        prompt.arm = Some(PromptArm {
            producer: "agent".into(),
            input_operation: 7,
            acknowledged: true,
            input_started: false,
            disarm_requested: false,
            disarmed: false,
        });
        prompt
            .record_delivery(Delivery::Submitting, receipt())
            .expect("test fixture or transition");
        prompt.delivery_uncertain();
        // The request can have been lost before fux accepted any bytes. Status
        // restores Reserved, allowing a retry of this operation only.
        prompt
            .record_delivery(Delivery::Reserved, receipt())
            .expect("test fixture or transition");
        assert!(
            prompt
                .arm
                .as_ref()
                .expect("test fixture or transition")
                .input_started
        );
        assert_eq!(
            prompt
                .receipt
                .as_ref()
                .expect("test fixture or transition")
                .operation,
            7
        );
        prompt
            .record_delivery(Delivery::Submitting, receipt())
            .expect("test fixture or transition");
        let mut accepted = receipt();
        accepted.input_sequence += 1;
        prompt
            .record_delivery(Delivery::Queued, accepted.clone())
            .expect("test fixture or transition");
        accepted.bytes_written = 6;
        prompt
            .record_delivery(Delivery::Delivered, accepted)
            .expect("test fixture or transition");
        prompt.delivery_uncertain();
        assert_eq!(prompt.delivery, Delivery::Delivered);
    }

    #[test]
    fn delivery_rejects_replacement_and_regression_without_partial_mutation() {
        let mut prompt = prompt();
        prompt
            .record_delivery(Delivery::Reserved, receipt())
            .expect("test fixture or transition");
        for defect in 0..3 {
            let mut changed = receipt();
            match defect {
                0 => changed.operation += 1,
                1 => changed.expires_server_ms += 1,
                _ => changed.input_sequence -= 1,
            }
            let before = serde_json::to_value(&prompt).expect("test fixture or transition");
            assert!(prompt.record_delivery(Delivery::Queued, changed).is_err());
            assert_eq!(
                serde_json::to_value(&prompt).expect("test fixture or transition"),
                before
            );
        }
        let mut accepted = receipt();
        accepted.input_sequence += 1;
        accepted.bytes_written = 6;
        prompt
            .record_delivery(Delivery::Delivered, accepted)
            .expect("test fixture or transition");
        let before = serde_json::to_value(&prompt).expect("test fixture or transition");
        assert!(
            prompt
                .record_delivery(Delivery::Reserved, receipt())
                .is_err()
        );
        assert_eq!(
            serde_json::to_value(&prompt).expect("test fixture or transition"),
            before
        );
    }

    #[test]
    fn receipt_publication_preserves_terminal_wait_and_release_evidence() {
        let mut prompt = prompt();
        prompt.released = true;
        prompt.wait = WaitOutcome::Cancelled;
        prompt
            .record_delivery(Delivery::Reserved, receipt())
            .expect("test fixture or transition");
        prompt.delivery_uncertain();
        assert!(prompt.released);
        assert_eq!(prompt.wait, WaitOutcome::Cancelled);
        let mut accepted = receipt();
        accepted.input_sequence += 1;
        accepted.bytes_written = 6;
        prompt
            .record_delivery(Delivery::Delivered, accepted)
            .expect("test fixture or transition");
        assert_eq!(prompt.wait, WaitOutcome::Cancelled);
    }

    #[test]
    fn uncertain_creation_cannot_be_resubmitted_or_change_pane() {
        let mut launch = launch();
        launch
            .begin_submission()
            .expect("test fixture or transition");
        launch
            .record_created_pane(4)
            .expect("test fixture or transition");
        assert!(launch.record_created_pane(5).is_err());
        assert_eq!(launch.pane, Some(4));
        launch.creation_uncertain("lost reply");
        assert!(launch.begin_submission().is_err());
        assert!(launch.record_created_pane(4).is_err());
        assert_eq!(launch.phase, LaunchPhase::Uncertain);
    }

    #[test]
    fn final_observation_preserves_retained_identity_and_task_outcome() {
        let mut journal = attached();
        let sessions = serde_json::to_value(&journal.sessions).expect("test fixture or transition");
        let tasks = serde_json::to_value(&journal.tasks).expect("test fixture or transition");
        journal
            .observe_attached(
                "task",
                "attempt",
                AttachedObservation::Final(final_evidence()),
            )
            .expect("test fixture or transition");
        journal.validate().expect("test fixture or transition");
        assert_eq!(
            journal.launches.get("task").expect("launch").phase,
            LaunchPhase::Closed
        );
        assert_eq!(
            journal.attempts.get("attempt").expect("attempt").state,
            AttemptState::Finished
        );
        assert_eq!(
            serde_json::to_value(&journal.sessions).expect("test fixture or transition"),
            sessions
        );
        assert_eq!(
            serde_json::to_value(&journal.tasks).expect("test fixture or transition"),
            tasks
        );
        let closed = serde_json::to_value(&journal).expect("test fixture or transition");
        assert!(
            journal
                .observe_attached("task", "attempt", AttachedObservation::Live)
                .is_err()
        );
        journal
            .launches
            .get_mut("task")
            .expect("test fixture or transition")
            .creation_uncertain("late sync error");
        assert_eq!(
            serde_json::to_value(&journal).expect("test fixture or transition"),
            closed
        );
    }

    #[test]
    fn rejected_observations_do_not_partially_mutate_records() {
        for defect in 0..5 {
            let mut journal = attached();
            match defect {
                0 => {
                    journal
                        .sessions
                        .get_mut("session")
                        .expect("test fixture or transition")
                        .ownership = Ownership::Adopted
                }
                1 => {
                    journal
                        .sessions
                        .get_mut("session")
                        .expect("test fixture or transition")
                        .target
                        .instance = "replacement".into()
                }
                2 => {
                    journal
                        .tasks
                        .get_mut("task")
                        .expect("test fixture or transition")
                        .attempt = "other".into()
                }
                3 => {
                    journal
                        .attempts
                        .get_mut("attempt")
                        .expect("test fixture or transition")
                        .task = "other".into()
                }
                _ => {
                    journal
                        .sessions
                        .get_mut("session")
                        .expect("test fixture or transition")
                        .target
                        .pane = 2
                }
            }
            let before = serde_json::to_value(&journal).expect("test fixture or transition");
            assert!(
                journal
                    .observe_attached(
                        "task",
                        "attempt",
                        AttachedObservation::Final(final_evidence())
                    )
                    .is_err()
            );
            assert_eq!(
                serde_json::to_value(&journal).expect("test fixture or transition"),
                before
            );
        }
    }

    #[test]
    fn outage_does_not_erase_replacement_evidence_and_live_keeps_needs_input() {
        let mut journal = attached();
        for replacement in [true, false] {
            journal
                .observe_attached(
                    "task",
                    "attempt",
                    AttachedObservation::Unavailable {
                        replacement,
                        problem: "endpoint unavailable".into(),
                    },
                )
                .expect("test fixture or transition");
            assert_eq!(
                journal.attempts.get("attempt").expect("attempt").state,
                AttemptState::Lost
            );
        }
        journal
            .observe_attached("task", "attempt", AttachedObservation::Live)
            .expect("test fixture or transition");
        assert_eq!(
            journal.attempts.get("attempt").expect("attempt").state,
            AttemptState::Active
        );
        journal
            .attempts
            .get_mut("attempt")
            .expect("test fixture or transition")
            .state = AttemptState::NeedsInput;
        journal
            .observe_attached("task", "attempt", AttachedObservation::Live)
            .expect("test fixture or transition");
        assert_eq!(
            journal.attempts.get("attempt").expect("attempt").state,
            AttemptState::NeedsInput
        );
    }
}
