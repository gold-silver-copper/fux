//! Bounds carried over from the contracts (TASKS.md:444-447, CHECKS.md:68, SOURCES.md:34,
//! ARTIFACTS.md:51, GROUPS.md:102, WORKTREES.md:40, multi-machine-supervision.md:50) plus the
//! configurable ones.

use bevy_ecs::prelude::*;

/// Serialized journal bound (TASKS.md:446).
pub const MAX_JOURNAL_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_TASKS: usize = 128;
pub const MAX_ATTEMPTS: usize = 512;
pub const MAX_PROMPTS: usize = 1024;
pub const MAX_OPERATIONS: usize = 1024;
pub const MAX_CHECKS: usize = 128;
pub const MAX_SOURCES: usize = 128;
pub const MAX_ARTIFACTS: usize = 128;
pub const MAX_GROUPS: usize = 32;
pub const MAX_GROUP_MEMBERS: usize = 8;
pub const MAX_WORKTREES: usize = 128;
pub const MAX_MACHINES: usize = 32;
pub const MAX_REQUIREMENTS_PER_TASK: usize = 32;

/// Configured bounds (`[limits]` in zor.toml); the record counts are the contract constants.
#[derive(Resource, Clone, Debug)]
pub struct Limits {
    pub tasks: usize,
    pub attempts: usize,
    pub prompts: usize,
    pub operations: usize,
    pub checks: usize,
    pub sources: usize,
    pub artifacts: usize,
    pub groups: usize,
    pub worktrees: usize,
    pub machines: usize,
    pub journal_bytes: usize,
    pub event_log_entries: usize,
    pub tokens: usize,
    /// Closed tasks older than this move to the archive.
    pub archive_after_ms: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            tasks: MAX_TASKS,
            attempts: MAX_ATTEMPTS,
            prompts: MAX_PROMPTS,
            operations: MAX_OPERATIONS,
            checks: MAX_CHECKS,
            sources: MAX_SOURCES,
            artifacts: MAX_ARTIFACTS,
            groups: MAX_GROUPS,
            worktrees: MAX_WORKTREES,
            machines: MAX_MACHINES,
            journal_bytes: MAX_JOURNAL_BYTES,
            event_log_entries: 4096,
            tokens: 256,
            archive_after_ms: 7 * 24 * 60 * 60 * 1000,
        }
    }
}

impl Limits {
    /// Never above the contract constants.
    pub fn clamped(mut self) -> Self {
        self.tasks = self.tasks.min(MAX_TASKS);
        self.attempts = self.attempts.min(MAX_ATTEMPTS);
        self.prompts = self.prompts.min(MAX_PROMPTS);
        self.operations = self.operations.min(MAX_OPERATIONS);
        self.checks = self.checks.min(MAX_CHECKS);
        self.sources = self.sources.min(MAX_SOURCES);
        self.artifacts = self.artifacts.min(MAX_ARTIFACTS);
        self.groups = self.groups.min(MAX_GROUPS);
        self.worktrees = self.worktrees.min(MAX_WORKTREES);
        self.machines = self.machines.min(MAX_MACHINES);
        self.journal_bytes = self.journal_bytes.min(MAX_JOURNAL_BYTES);
        self
    }
}
