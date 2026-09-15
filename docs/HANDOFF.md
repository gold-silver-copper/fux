# Handoff

Worktree: `../fux-rewrite`, branch `ecs-rewrite` (orphan). `main` stays at `../fux` as the
behavioural oracle. Bevy source: `../many_rigs/inspirations/bevy` (0.19.1).

## Milestone reached
1. Workspace skeleton (crates `fux`, `zor`, `tools/xtask`), dependency report, CI.

## Next task
Milestone 2: fux App shell (prompt section 6.2).

## Open blockers
* `bevy_render` in graph via `bevy_remote -> bevy_dev_tools` (docs/dependencies.md).

## Resume
```
cd ../fux-rewrite
cargo check --workspace
cargo run --manifest-path tools/xtask/Cargo.toml -- deps
```
