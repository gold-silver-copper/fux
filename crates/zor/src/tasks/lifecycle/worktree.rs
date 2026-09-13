//! Durable worktree phases. Filesystem/Git identity and active-use checks happen
//! under the journal lock before these transitions; none of these methods runs Git.
use super::super::model::{Worktree, WorktreePhase};
use anyhow::{Result, ensure};

impl Worktree {
    pub(in crate::tasks) fn pin_parent(&mut self, identity: (u64, u64)) -> Result<()> {
        ensure!(
            self.phase == WorktreePhase::Allocating,
            "worktree parent already allocated"
        );
        ensure!(identity.1 != 0, "worktree parent inode missing");
        self.parent_dev = identity.0;
        self.parent_ino = identity.1;
        self.phase = WorktreePhase::Prepared;
        self.problem = None;
        Ok(())
    }

    pub(in crate::tasks) fn begin_creation(&mut self) -> Result<()> {
        ensure!(
            self.phase == WorktreePhase::Prepared && self.parent_ino != 0,
            "worktree is not prepared for creation"
        );
        self.phase = WorktreePhase::Creating;
        Ok(())
    }

    pub(in crate::tasks) fn record_ready(&mut self, identity: (u64, u64)) -> Result<()> {
        ensure!(
            matches!(
                self.phase,
                WorktreePhase::Creating | WorktreePhase::Uncertain | WorktreePhase::Ready
            ),
            "worktree cannot become ready from this phase"
        );
        ensure!(
            identity.1 != 0 && self.checkout_identity.is_none_or(|old| old == identity),
            "worktree checkout identity changed"
        );
        self.phase = WorktreePhase::Ready;
        self.checkout_identity = Some(identity);
        self.problem = None;
        Ok(())
    }

    /// Only the initial call may initiate removal. Retries reconcile the durable
    /// force intent and never execute another Git removal automatically.
    pub(in crate::tasks) fn begin_removal(&mut self, force: bool) -> Result<()> {
        ensure!(
            self.phase == WorktreePhase::Ready
                && self.checkout_identity.is_some()
                && self.remove_force.is_none(),
            "worktree removal is not a new ready operation"
        );
        self.phase = WorktreePhase::Removing;
        self.remove_force = Some(force);
        self.problem = None;
        Ok(())
    }

    pub(in crate::tasks) fn record_removed(&mut self) -> Result<()> {
        ensure!(
            self.phase == WorktreePhase::Removing && self.remove_force.is_some(),
            "worktree has no removal intent"
        );
        self.phase = WorktreePhase::Removed;
        self.problem = None;
        Ok(())
    }

    pub(in crate::tasks) fn record_problem(&mut self, problem: &str) {
        if !matches!(
            self.phase,
            WorktreePhase::Ready | WorktreePhase::Removing | WorktreePhase::Removed
        ) {
            self.phase = WorktreePhase::Uncertain;
        }
        self.problem = Some(problem.chars().take(128).collect());
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    fn tree() -> Worktree {
        Worktree {
            id: "tree".into(),
            requested_repo: "/repo".into(),
            repo: "/repo".into(),
            common: "/repo/.git".into(),
            parent: "/state/owned".into(),
            path: "/state/owned/tree".into(),
            branch: "branch".into(),
            base: "main".into(),
            commit: "a".repeat(40),
            repo_dev: 1,
            repo_ino: 2,
            common_dev: 1,
            common_ino: 3,
            parent_dev: 0,
            parent_ino: 0,
            checkout_identity: None,
            phase: WorktreePhase::Allocating,
            remove_force: None,
            problem: None,
        }
    }

    #[test]
    fn lost_creation_requires_reconciliation_and_keeps_checkout_identity() {
        let mut tree = tree();
        tree.pin_parent((1, 4)).expect("pin parent");
        tree.begin_creation().expect("creation intent");
        tree.record_problem("lost add reply");
        assert!(tree.begin_creation().is_err());
        tree.record_ready((1, 5)).expect("reconciled checkout");
        let before = serde_json::to_value(&tree).expect("snapshot");
        assert!(tree.record_ready((1, 6)).is_err());
        assert!(tree.pin_parent((1, 7)).is_err());
        assert_eq!(serde_json::to_value(&tree).expect("snapshot"), before);
    }

    #[test]
    fn lost_removal_cannot_repeat_or_change_force_and_closure_needs_intent() {
        let mut tree = tree();
        assert!(tree.record_removed().is_err());
        tree.pin_parent((1, 4)).expect("pin parent");
        tree.begin_creation().expect("creation intent");
        tree.record_ready((1, 5)).expect("checkout");
        tree.begin_removal(false).expect("removal intent");
        tree.record_problem("lost remove reply");
        assert_eq!(tree.phase, WorktreePhase::Removing);
        assert_eq!(tree.remove_force, Some(false));
        assert!(tree.begin_removal(false).is_err());
        assert!(tree.begin_removal(true).is_err());
        assert!(tree.record_ready((1, 5)).is_err());
        tree.record_removed().expect("absence proven by caller");
        assert_eq!(tree.phase, WorktreePhase::Removed);
        assert!(tree.begin_creation().is_err());
        assert_eq!(tree.checkout_identity, Some((1, 5)));
    }
}
