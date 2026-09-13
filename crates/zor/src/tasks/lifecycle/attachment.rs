//! Managed attachment and stop authority; mutations run inside Store::transaction.
use super::super::model::*;
use anyhow::{Context, Result, ensure};

pub(in crate::tasks) struct Attachment {
    pub session: String,
    pub attempt: String,
    pub pane: u32,
    pub pid: Option<u32>,
    pub evidence: Option<LaunchFinal>,
}

impl Journal {
    pub(in crate::tasks) fn managed_stop_target(&self, id: &str) -> Result<Target> {
        let launch = self
            .launches
            .get(id)
            .context("stop requires a managed launch; adoption grants no termination authority")?;
        ensure!(
            launch.task.is_none(),
            "historical launch is not the current task stop target"
        );
        ensure!(
            matches!(launch.phase, LaunchPhase::Attached | LaunchPhase::Closed),
            "launch has no reconciled ownership; reconcile it before requesting stop"
        );
        let session = launch
            .session
            .as_ref()
            .and_then(|id| self.sessions.get(id))
            .context("managed session missing")?;
        ensure!(
            session.ownership == Ownership::Managed
                && session.launch.as_deref() == Some(id)
                && session.target.instance == launch.instance
                && launch.pane == Some(session.target.pane),
            "session is not owned by this launch"
        );
        Ok(session.target.clone())
    }

    /// Persist coordination cancellation and stop intent together before kill.
    /// An accepted kill is not final evidence; recovery must still prove closure.
    pub(in crate::tasks) fn request_managed_stop(&mut self, id: &str) -> Result<()> {
        self.managed_stop_target(id)?;
        if self
            .launches
            .get(id)
            .context("launch missing")?
            .stop_requested
        {
            return Ok(());
        }
        if self.tasks.get(id).context("task missing")?.outcome != TaskOutcome::Verified {
            super::super::cancel_journal(self, id)?;
        }
        self.launches
            .get_mut(id)
            .context("launch missing")?
            .stop_requested = true;
        Ok(())
    }

    pub(in crate::tasks) fn attach_managed(
        &mut self,
        launch_id: &str,
        attachment: Attachment,
    ) -> Result<()> {
        let launch = self
            .launches
            .get(launch_id)
            .context("launch missing")?
            .clone();
        let Attachment {
            session,
            attempt,
            pane: pane_id,
            pid,
            evidence,
        } = attachment;
        ensure!(
            matches!(
                launch.phase,
                LaunchPhase::Submitting | LaunchPhase::Uncertain
            ),
            "launch cannot attach from this phase"
        );
        ensure!(
            pane_id != 0 && launch.pane.is_none_or(|pane| pane == pane_id),
            "attachment changed launch pane"
        );
        ensure!(
            pid != Some(0) && (pid.is_some() || evidence.is_some()),
            "attachment has neither live process nor final evidence"
        );
        ensure!(
            evidence
                .as_ref()
                .is_none_or(|final_evidence| final_evidence.text.len() <= 4096),
            "launch final evidence exceeds limit"
        );
        ensure!(
            id(&session)
                && id(&attempt)
                && !self.sessions.contains_key(&session)
                && !self.attempts.contains_key(&attempt),
            "attachment identity already exists or is invalid"
        );
        ensure!(
            launch.task.is_some() || !self.tasks.contains_key(&launch.id),
            "launch task already exists"
        );
        let id = launch.task_id();
        if launch.task.is_some() {
            super::super::resume::attach_launch(self, &launch)?;
        }
        self.sessions.insert(
            session.clone(),
            Session {
                id: session.clone(),
                target: Target {
                    origin: Some(Origin {
                        workspace: launch.workspace.clone(),
                        stream: launch.stream,
                    }),
                    runtime: launch.runtime.clone(),
                    instance: launch.instance.clone(),
                    workspace: launch.workspace.clone(),
                    stream: launch.stream,
                    pane: pane_id,
                    pid,
                },
                agent: launch.agent.clone(),
                ownership: Ownership::Managed,
                launch: Some(id.into()),
                created_ms: launch.created_ms,
            },
        );
        self.attempts.insert(
            attempt.clone(),
            Attempt {
                id: attempt.clone(),
                task: id.into(),
                session: session.clone(),
                state: if evidence.is_some() {
                    AttemptState::Finished
                } else {
                    AttemptState::Active
                },
            },
        );
        if launch.task.is_some() {
            self.tasks
                .get_mut(id)
                .context("resume task missing")?
                .attempt = attempt;
        } else {
            self.tasks.insert(
                id.into(),
                Task {
                    required_checks: Default::default(),
                    required_artifacts: Default::default(),
                    id: id.into(),
                    requested_runtime: launch.requested_runtime.clone(),
                    title: launch.title.clone(),
                    created_ms: launch.created_ms,
                    outcome: TaskOutcome::Open,
                    attempt,
                },
            );
        }
        let launch = self.launches.get_mut(id).context("launch missing")?;
        launch.phase = if evidence.is_some() {
            LaunchPhase::Closed
        } else {
            LaunchPhase::Attached
        };
        launch.final_evidence = evidence;
        launch.pane = Some(pane_id);
        launch.session = Some(session.clone());
        launch.problem = None;
        Ok(())
    }
}
