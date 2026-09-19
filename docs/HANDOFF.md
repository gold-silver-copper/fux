# Handoff

Worktree: `../fux-rewrite`, branch `ecs-rewrite` (orphan). `main` stays at `../fux` as the
behavioural oracle. Bevy source: `../many_rigs/inspirations/bevy` (0.19.1).

## Milestone reached
1. Workspace skeleton (crates `fux`, `zor`, `tools/xtask`), dependency report, CI.
1b. Headless `bevy_ui` layout proof (`crates/fux/tests/layout_mechanism.rs`) and the model
   foundation (`crates/fux/src/model/*`, `wire.rs`).

2. fux App shell (commit `0e49c4b`): runner, lifecycle, layout instances, terminal/PTY, BRP,
   attachment stream, viewer, CLI; 80 tests; real-process smokes in `docs/verification.md`.

3. Scene completeness (commit `baeef55`): picking regions/drags, scenes, surfaces + `Text`,
   layout matrix. Milestone-2 review applied in `ac86886`.
4. Events, retained log, `fux/events+watch`, `world.observe+watch`, receipts, final records,
   diagnostics, bell (commit `e378fa3`); milestone-3 review applied; `fux events` CLI.

5. Assets, `bsn!` templates, session persistence/restore (commit `8696ad4`).
6. zor: foundation (`9e316cb`), lifecycle/checks/groups/worktrees/providers (`a3bab88`);
   fux viewer chrome completion, bounded BRP acceptor, review fixes (same commit).

## Module ownership (milestones 2-6)

Module ownership (one owner per file; shared contracts live in `model/` and `wire.rs`):

| module | owns | provides |
|---|---|---|
| `model/` | entity graph, ids, relationships, messages, limits, invariants | contracts for everyone |
| `wire.rs` | attachment stream frames | `Hello`, `ClientFrame`, `ServerFrame`, `SceneFrame`, `TerminalDelta` |
| `terminal.rs` | `Terminal` component (vt100 + bounded history + sequence) | `Terminal::{new, feed, resize, seq, title, write_delta}` |
| `pty.rs` | `PtyAdapter` (portable-pty on `IoTaskPool`), `TerminalPlugin` (ingest/output systems) | applies `Effect::{SpawnPane,WritePty,ResizePty,Terminate,ReleasePty}`; pushes `Inbound` |
| `layout.rs` | workspaces, template roots/nodes, instances per viewer, cameras, `PaneSize` fold, `LayoutPlugin` | `layout::ops::*` typed transitions on `&mut World` |
| `lifecycle.rs` | pane/workspace state machine, `Requests`/`Completions` phases, shutdown, `LifecyclePlugin` | consumes `Inbound`, `ViewerRequest`; emits `Effect` |
| `remote.rs` | BRP: token file, allowlist, `fux/*` methods, `RemoteControlPlugin`, thin client | `remote::client::call` |
| `attach.rs` | attachment listener, per-viewer projection, `AttachPlugin` | `Effect::SendFrame` |
| `viewer.rs` | the viewer App (input parser, focus, chrome, painter, panic hook) | `viewer::run` |
| `app.rs`, `runner.rs`, `cli.rs`, `config.rs`, `paths.rs` | App assembly, custom runner, signals, CLI, config, XDG paths | `fux::app::build`, `runner::run` |

## Active execution — 2026-09-19

Governing assignment: [`docs/prompts/ecs-native-continuation-prompt.md`](prompts/ecs-native-continuation-prompt.md).
Scope is fux + zor only. koh and future iroh-ssh integration are not dependencies or acceptance
gates. Earlier koh blocker statements are superseded.

Baseline: `9260d3bf4b7bf4fb6e6d287c50a8d5852d120845`; rustc
`1.96.0-nightly (80381278a 2026-03-01)`, aarch64-apple-darwin, Bevy 0.19.1.
Pre-existing changes: deleted tracked `tools/xtask/target` artifacts and the new continuation
prompt. Do not restore or commit the artifact deletions as part of implementation.

Milestone 7 is being completed: plugin host/hooks, direct-endpoint machine supervision,
fux-hosted dashboard, CLI/runner integration and real-process scenarios. Recorded milestone-6
and fux surface-input results above are historical; current acceptance is not yet established.
Next: integrate the completed modules, run focused regression and real-process acceptance,
review ECS/security boundaries, then milestone-8 documentation and release benchmarks.

The user authorized incremental commits of completed, verified work. Main is the integration
and commit owner. No push, tag or publication is authorized; the user's running sessions and
the `main` worktree remain untouched.

Accepted dependency exception: `bevy_remote -> bevy_dev_tools` brings render crates into the
graph without adding a renderer (see `docs/dependencies.md`). Audible bell playback still
requires a device-backed check; it has only historical headless compile evidence.

## Resume
```
cd ../fux-rewrite
cargo check --workspace
cargo test -p fux
cargo run --manifest-path tools/xtask/Cargo.toml -- deps
```
