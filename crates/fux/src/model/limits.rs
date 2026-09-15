//! Numeric limits carried over from the old `layout.rs`/`terminal.rs` (prompt section 1) plus
//! the configurable bounds resource.

use bevy_ecs::prelude::*;

/// Smallest emulator dimension a pane is ever resized to.
pub const MIN_DIM: u16 = 2;
/// Peer-controlled dimensions are clamped here so vt100 never allocates an unbounded grid.
pub const MAX_DIM: u16 = 512;
/// Absolute ceiling on placing leaves per template root; the configured pane limit is usually lower.
pub const MAX_LEAVES: usize = 256;
/// Maximum root-to-leaf edge count of any template, including imported layouts.
pub const MAX_DEPTH: usize = 64;
/// Absolute ceiling on template nodes per workspace.
pub const MAX_NODES_PER_WORKSPACE: usize = 2048;
/// Minimum pane content size in cells below which a placing leaf is not shown.
pub const MIN_PANE_COLS: u16 = 2;
pub const MIN_PANE_ROWS: u16 = 2;

pub fn clamp_dims(rows: u16, cols: u16) -> (u16, u16) {
    (rows.clamp(MIN_DIM, MAX_DIM), cols.clamp(MIN_DIM, MAX_DIM))
}

/// Configured bounds (`[limits]` in fux.toml); never above the absolute constants.
#[derive(Resource, Clone, Debug, PartialEq, Eq)]
pub struct Limits {
    pub panes_per_workspace: usize,
    pub nodes_per_workspace: usize,
    pub workspaces: usize,
    pub viewers: usize,
    pub scrollback_lines: usize,
    pub final_retain_ms: u64,
    pub output_pacing_ms: u64,
    pub event_log_entries: usize,
    pub tokens: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            panes_per_workspace: 64,
            nodes_per_workspace: 512,
            workspaces: 32,
            viewers: 32,
            scrollback_lines: 2000,
            final_retain_ms: 30_000,
            output_pacing_ms: 50,
            event_log_entries: 4096,
            tokens: 256,
        }
    }
}

impl Limits {
    pub fn clamped(mut self) -> Self {
        self.panes_per_workspace = self.panes_per_workspace.min(MAX_LEAVES);
        self.nodes_per_workspace = self.nodes_per_workspace.min(MAX_NODES_PER_WORKSPACE);
        self
    }
}
