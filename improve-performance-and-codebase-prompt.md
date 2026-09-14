# Improve fux, zor and koh: performance, idiom, minimality, ECS-nativeness

Execute this work in the fux/zor workspace and its pinned koh companion. Deliver measured,
verified improvements to real hot paths and to code structure, not a redesign and not a
collection of speculative micro-optimizations. Every performance change must be justified by a
before/after measurement from this repository's own harness. Every structural change must
preserve behavior, proven by the existing test suites.

Backward compatibility and breaking semver are not concerns. Preserve the strict ownership
boundaries the codebase already enforces (fux owns panes/PTYs/terminal state/viewers; koh owns
remote identity and opaque transport; zor owns catalog, agent/task interpretation, supervision
and recovery policy; local-ipc owns generic local socket discipline). This prompt does not by
itself authorize commits, pushes, PRs, releases, koh publication or companion-pin changes;
follow explicit authorization in the active conversation.

## Measure before you change anything

Do not optimize by feel. This repository already has a measurement suite under `tools/xtask`
(`measure`, `measure_frames`, `measure_interactions`, `measure_layout`, `measure_memory`,
`measure_recovery`, `measure_viewer`, `headless_performance`, `resource_sampler`, `measure_koh`).
Read it first. Establish a baseline on the exact built binaries, record the commands and the
raw numbers, and keep every performance claim tied to a specific baseline and a specific
after-number produced the same way. If a hot path has no existing measurement, add a focused
one before changing the path it measures. A change that cannot be measured is a structural
change and belongs in the structure sections below, not the performance sections.

Record the starting inventory: workspace revision, koh pin, baseline test/CI state, and the
current measurement numbers. Do not repeat stale findings; re-verify against the current tree.

## 1. Performance: zor

- **Journal persistence is the clearest algorithmic cost.** `crates/zor/src/tasks/store.rs`
  rewrites the whole journal to a candidate file and commits it durably (`file.sync_all()` →
  rename → directory `sync_all()`) on every state change; `heartbeat.rs` does three `sync_all`
  calls per write. Under many concurrent tasks or frequent heartbeats, fsync and the O(journal)
  rewrite dominate, and the task-store exclusive lock is held across the commit. First measure
  commits-per-second as a function of concurrent tasks and heartbeat frequency. Then evaluate,
  in order of increasing effort and risk: coalescing multiple events into one durable commit
  per tick; and an append-only journal with periodic compaction to replace the full rewrite.
  Do not weaken the existing durability guarantee (a committed operation must survive crash and
  restart) or the crash-recovery tests without an explicit decision recorded in the ledger.
- Confirm the observe loop (100 ms) and dashboard input poll (20 ms) are not worth touching;
  state that with a measurement rather than assuming it.

## 2. Performance: fux

- **Output coalescing window.** `crates/fux/src/server/mod.rs` batches pane output with
  `STREAM_GAP` (3 ms) and `STREAM_WAIT` (1 ms). Measure emit cost as syscalls and render passes
  per kilobyte for bursty output (a build log) at several window values, and interactive
  round-trip latency at the same values. Choose values from the trade curve; do not change the
  constants without the curve. Keep interactive latency within its current bound.
- Confirm the server loop stays idle-asleep (event-driven) and the pane `dirty` refresh path is
  not doing redundant work; verify with `measure_frames`/`headless_performance`, not by reading.
- Do not touch the vendored terminal emulation (vt100/ghostty-vt) unless a measurement shows a
  regression there; it already went through a perf pass.

## 3. Performance: koh (measurement and options only; no publication)

- **Key unlock dominates startup.** koh uses Argon2id (`koh-key-v1`), measured around 1.8 s per
  unlock, and the controller starts one `koh gateway connect` helper per machine, each building
  its own iroh endpoint and re-unlocking the same client key. Measure cold-start to first fresh
  view as a function of machine count using `measure_koh` plus a new multi-machine timing probe.
- Evaluate, without implementing a koh publication: starting per-machine helpers concurrently
  instead of serially (a zor-side change, no koh change, cheapest win); and a single long-lived
  koh connection agent that unlocks once and multiplexes per-machine connects over one endpoint
  (an architecture change that crosses koh's key-ownership boundary — design it, do not rush it).
- koh changes, if any, are developed in a separate checkout, verified there, and left as a
  reviewable patch with exact base and checksum. Never dirty `references/koh` or pin a
  local-only commit. State the exact publication step that would still need authorization.

## 4. ECS-nativeness: fux

The schedule is real (`crates/fux/src/ecs/mod.rs`: one `Step`, eight chained `Phase` sets), but
most systems are exclusive `fn(world: &mut World)` bodies that hand-write `world.query()` and
`world.resource_mut()`. This uses bevy_ecs as a data store rather than a scheduler.

- **Replace hand-rolled `dirty: bool` flags with change detection.** Panes and viewers carry a
  `dirty` flag that reimplements `Changed<T>`/`Added<T>`. Convert the snapshot and output systems
  to query-parameterized systems and drive grid refresh from change detection. This is the one
  change that makes fux genuinely more ECS-native with real payoff (it removes a class of
  "forgot to set dirty" bugs). Prototype it on the snapshot path first and prove the existing
  ECS suite still passes before extending it.
- Convert the read-and-iterate systems (output, snapshot, input receipts) to `Query`/`Res`
  params so their data access is explicit and checked. Keep the structurally-mutating systems
  (creation, lifecycle, layout) exclusive-world; they spawn/despawn and reorder entities, where
  exclusive access is the honest choice. Do not convert wholesale: the executor is deliberately
  `SingleThreadedExecutor`, so there is no parallelism payoff, and the change buys clarity, not
  speed. Preserve the documented phase ordering and the mid-phase viewer-queue drains exactly.

## 5. Minimality and idiom: fux

Reduce concentration, not features. Do not remove dependencies (they are already tight and each
is justified) and do not change the CLI surface.

- **Split the god-files by cohesion:** `client/controller.rs` (~4150 lines, ~123 methods in one
  `impl`), `ecs/systems/requests.rs` (~2000 lines, `apply_control` is a 34-arm match over a
  69-variant `Request`), `view.rs` and `proto/control.rs`. The clearest single split is the
  `Request` dispatch: replace the one giant match with per-request or per-category handlers so
  each request's logic sits near its type. Then split `controller.rs` by concern (input, render
  adoption, history, popups) into focused modules. These are moves, not rewrites; behavior and
  tests must be unchanged.
- Do a borrow-vs-own pass on the `.clone()` hotspots (concentrated in `requests.rs` and
  `controller.rs`); keep only the clones that are genuine snapshots. Low priority, no behavior
  change.
- Preserve the existing discipline: production code keeps zero `expect()`, panics only in
  `#[cfg(test)]`, and the structure test's invariants (no placeholder escape hatches, reviewed-
  only process spawns, ownership/import layering) must still pass.

## 6. Minimality and idiom: zor

- Apply the same cohesion review to zor's larger modules (the dashboard, service client and task
  lifecycle files) without changing behavior or the service protocol. Keep the protocol-layer
  isolation the CI grep-guards enforce (no `emit`/`pty`/`platform`/`screen`/`rules`/`state`
  imports across the guarded boundaries).
- Preserve the `wrap` feature gating and the no-default-features/cli feature closures exactly;
  the boundary and package jobs depend on them.

## 7. Verification

- Run the repository's real gate for every change: `cargo fmt --all --check`; workspace clippy
  all-targets with `-D warnings`; the zor clippy matrix (all-features, no-default, cli); the
  local-ipc clippy; `cargo test` for the affected packages; `verify-boundaries`; and the
  `structure` CI-surface test. Start targeted, broaden to the full gate.
- Every performance change carries its before/after numbers from this repository's harness, with
  the exact commands and the binary hashes. No inflated timeouts or weakened assertions to make
  a number look good.
- Every structural change is proven behavior-preserving by the existing suites (fux ECS, zor
  library, the real-process scenarios). Add tests only where a refactor exposes an untested seam.
- If a koh option is prototyped, verify it in the separate development checkout, keep the patch
  with base and checksum, and confirm it applies cleanly against the clean published reference.
- Inspect current hosted CI; separate preexisting unrelated failures from regressions this work
  introduces. Do not expand into unrelated fixes.

## 8. Documentation, review and completion

- Write `docs/performance-and-refactor.md` (or extend an existing perf doc): the baseline and
  after numbers with commands, the changes made and why, the trade curves for any tuned
  constants, the ECS change-detection design, the module splits, and honestly-stated items that
  were measured-but-not-worth-changing or externally blocked (koh publication).
- Update ownership/design docs and help text only where implemented behavior changed.
- Review the complete change set in a separate pass, validate findings against current code, fix
  confirmed in-scope issues, and rerun the affected checks.

Completion requires: measured improvement on at least the zor journal path and the fux output
path (or a recorded decision that a measured path is not worth changing), the fux change-
detection conversion of the snapshot/output systems with the ECS suite passing, at least the
`Request`-dispatch and `controller.rs` splits landed behavior-preserving, the full relevant gate
green, and an honest ledger separating verified wins, deliberate no-ops, and work blocked on
authorization. Do not claim a speedup without a number, do not claim ECS-nativeness beyond the
systems actually converted, and do not silently expand into a redesign of any component.
