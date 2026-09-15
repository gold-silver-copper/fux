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

## Next task
Milestone 7: zor plugin host (`zor-plugin.toml`), machines catalog / supervision workers /
resume intents, dashboard as a fux surface, koh composition (unavailable until koh forwards TCP
upstream), multi-machine scenario harness in `tools/xtask`. Then milestone 8 (docs, changelog,
benchmarks vs `main`, capability status refresh).

## Open blockers
* none. `bevy_render` in graph via `bevy_remote -> bevy_dev_tools` is an accepted deviation (docs/dependencies.md).

## Resume
```
cd ../fux-rewrite
cargo check --workspace
cargo test -p fux
cargo run --manifest-path tools/xtask/Cargo.toml -- deps
```
