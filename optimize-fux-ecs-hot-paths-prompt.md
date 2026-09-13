# Reduce fux per-keystroke and per-frame server cost

Improve fux's steady-state server efficiency on main after the merged native integration
(PR #4, merge commit `748998e`, branch `main` of `https://github.com/gold-silver-copper/fux`).
Work in a fresh worktree of `main`; do not reuse `/Users/kisaczka/Desktop/code/fux-integrated`
except to read its retained evidence under `.verification/perf-2026-09-09/` (start from
`REPORT.md`). Keep the runtime of `/Users/kisaczka/Desktop/code/fux` unchanged.

This is a scoped follow-up, not a broad campaign. The previous pass established that the
integration is not slower than main in release, that zor and koh need no runtime change, and
that end-to-end paired runs on this machine cannot resolve differences under about 20 %. The
two remaining measured server costs are in main's own ECS code:

1. Per-step `QueryState` construction in exclusive systems (about 18 `world.query::<…>()` /
   `query_filtered` sites across `src/ecs/systems/requests.rs` `drain_viewer_queues`,
   `src/ecs/systems/lifecycle.rs` `resolve_lifecycle`, `src/ecs/support.rs` and
   `src/ecs/systems/creation.rs`). Sampled profiles show a visible share of the schedule in
   `QueryState::new`, `FilteredAccess` bitset growth and the `Vec` collects that follow.
2. `Grid::refresh` in `src/terminal.rs` compares every cell through `vt100::Screen::cell`
   (one `visible_row` lookup per cell, rows×cols per frame). It is the largest single
   active-CPU item per frame on the 24x80 sample and grows with 60x200.

## Rules

- Keep iteration fast: no single build, test run, benchmark or CI wait may exceed five
  minutes of wall-clock time; prefer under two. Never launch batch campaigns. Use short
  workloads (300–500 keystrokes, one or two configurations, 2–3 alternating repetitions) and
  kill anything that overruns. Never compile while a paired measurement is running.
- Measure before and after each change in isolation first, then confirm end to end:
  - In isolation: extend `tests/micro_timing.rs` (env-gated by `FUX_MICRO_TIMING=1`) with an
    ECS-step timing that drives `Session::step` with one keystroke of input and a small
    output chunk per step for a few thousand steps, and a `Grid::refresh` timing for 24x80 and
    60x200 with a changed row and with no change. These are the acceptance metrics.
  - End to end: `fux-xtask measure-frames BINARY --keystrokes 500 --config 24x80` and
    `--config 60x200`, paired against a release build of `main` with the same settings
    (`cargo build --locked --release`, `CARGO_PROFILE_RELEASE_DEBUG=1`, `CARGO_INCREMENTAL=0`).
    Use the retained `paired.sh`/`summarize.py` from the previous evidence directory. Report
    medians with every sample; treat anything inside the observed spread as no change.
- Change one area at a time. For (1), prefer `QueryState`/`SystemState` exclusive-system
  parameters or `Local` caches so access sets are built once; do not restructure the
  schedule, change system ordering, or convert exclusive systems that issue commands. For
  (2), obtain each row once (for example through `vt100`'s row iteration or by comparing a
  row's cells in one pass) without changing `GridCell`, sequence stamping, wrapped-row
  detection, cursor handling or the delta encoding; the retained-grid delta contract and
  every existing delta/hostile-frame/slow-viewer test must pass unchanged.
- Preserve all behavior: incarnation identity, capture revisions and grid sequencing, input
  receipts and partial-delivery accounting, bounded replay, final records, slow-consumer and
  hostile-frame bounds, Unicode correctness and identical visible output. No caches without
  invalidation tests; no batching; no background threads.
- Retain a change only if the isolated timing improves clearly (target: at least 20 % on its
  own metric) and the end-to-end frame latency/CPU does not regress outside noise. Revert
  anything speculative and say so.

## Verification and handoff

Run, each within the time budget: `cargo fmt --check`, strict `cargo clippy --all-targets
-- -D warnings` (root and `tools/xtask`), root `--lib`, `ecs`, `agent_boundary`, `structure`,
`fixtures`, `micro_timing`, and `local_cli` suites, and the tooling tests. Regenerate the
boundary inventory only after confirming the added declarations are generic. Use one
independent subagent to review the diff for semantic drift in the ECS systems and the grid
comparison. Do not run the full 45-command headless gate yourself; leave its exact command in
the handoff for the user.

Commit on a branch `perf/ecs-hot-paths` with a message that states the measured before/after
numbers; push and open a PR against `main` only if all checks above pass and hosted CI on
the PR is green. Write a short report at `.verification/perf-ecs-hot-paths/REPORT.md`
(machine, toolchain, build flags, binary hashes, raw samples, retained versus reverted
changes, reviewer disposition, remaining bottlenecks with the next concrete experiment) and
link it from the PR description.
