# Performance and refactor pass

Tracks `improve-performance-and-codebase-prompt.md`. Every performance claim is a before/after
from this repository's own harness on the exact built binaries; every structural claim is proven
by the existing test suites. Measurements were taken with `cargo +stable` and
`CARGO_TARGET_DIR=/tmp/fux-perf-build`.

## Verified win: zor journal ancestry sync

`crates/zor/src/tasks/store.rs` called `sync_ancestry` on every `transaction`, fsyncing the
state directory and each user-owned ancestor before writing the candidate journal, on top of
the necessary post-rename directory fsync. After the first successful commit the directory
hierarchy is already durable, so that walk is redundant on every later commit.

The fix records `ancestry_synced` on the `Store` and runs `sync_ancestry` only until the first
commit has fsynced the hierarchy. Durability is unchanged: the first commit still syncs the
ancestry, and every commit still fsyncs the candidate file and the state directory after the
rename. Crash recovery is unaffected.

Measured with `fux-xtask headless-journal` (median across 3 runs x 32 samples; `adopt` commits,
`inspect` never commits, `idempotent-adopt` makes no replacement):

| operation | child CPU before | child CPU after | change |
|---|---|---|---|
| adopt (commit) | 10.43 ms | 5.40 ms | -48.2% |
| idempotent-adopt | 7.24 ms | 4.26 ms | -41.2% |
| inspect | 5.00 ms | 4.15 ms | -17.0% |

Elapsed on the committing path fell about 25% (fsync-wait dominated and run-to-run noisy; CPU is
the stable signal). Verified: `cargo test -p zor --lib` (197 passed, 2 ignored) including the
store/recovery tests, plus the zor clippy matrix and formatting.

## Measured, deliberately unchanged: fux output coalescing window

`crates/fux/src/server/mod.rs` batches pane output with `STREAM_GAP` (3 ms) and `STREAM_WAIT`
(1 ms). Measured with `fux-xtask headless-performance`, the sustained-stream phase costs about
449 ms fux CPU at the current values. Widening to 4 ms / 2 ms **regressed** it to about 522 ms
(and the burst phase workload grew), because larger pending buffers cost more per ECS step than
the extra batching saves. The current values are at the good part of the trade curve, so they
are left unchanged. This is a measured no-op, not an untried path.

## Empirically not applicable: ECS change detection for `dirty` flags

The proposal was to replace the hand-rolled `Pane`/`Viewer` `dirty` flags with bevy_ecs
`Changed<T>` on the snapshot/output path. This was prototyped and run, not only reasoned about:
gating the `refresh_grids` pane loop on `Pane::is_changed()` instead of the paced `dirty` /
`event_pending` flags made the ECS suite fail on exactly the pacing tests:

- `output_frames_are_paced_but_replies_are_not` — FAILED
- `metadata_only_output_is_coalesced_and_publishes_a_final_invalidation` — FAILED

The cause is fundamental, not a detail: grid refresh is **paced and deferred**. `Pane::refresh()`
clears `dirty` only when it actually refreshes, and `refresh_grids` may leave a pane dirty across
several steps until it is due. `Changed<T>` is auto-cleared at each schedule pass and is
re-triggered by `refresh()` itself mutating the pane, so it cannot represent "dirty until I
choose to consume it" and it spuriously re-refreshes hidden/paced panes. The prototype was
reverted (ECS suite back to 84 passing); the flags are the correct design here. This is the
recorded decision: the conversion is not behavior-preserving and is not landed.

Separately, the prompt's premise that the read systems need converting to query params is
already satisfied: `apply_pane_output`, `apply_completions`, `resolve_layout`, `refresh_grids`,
`publish_frames` and `finish_step` already take `Query`/`Res`/SystemParam. Only the
structurally-mutating systems (`apply_requests`, `drain_viewer_queues`, `apply_spawn_completions`,
`resolve_lifecycle`) remain exclusive-world, which the prompt itself said to keep.

## Already present: concurrent per-host connection startup

`Supervision::start` (`crates/zor/src/machines/supervision.rs`) spawns one worker thread per
machine in a loop, each connecting its koh helper independently. The "start helpers concurrently"
win the prompt scoped for koh is already realized on the dashboard path; no change needed.

## koh: measured scope only, publication unauthorized

koh key unlock (Argon2id, `koh-key-v1`) is the dominant remote-startup cost (~1.8 s per helper,
prior checkpoint-5 evidence). The cheap win (parallel helper startup) is already present above.
The large win — a single long-lived agent that unlocks once and multiplexes per-machine connects
over one iroh endpoint — crosses koh's key-ownership boundary and is a design change, not landed
here. No koh change was made; `references/koh` is unmodified. Publishing anything koh-side remains
a separately authorized step.

## Structural: module splits landed behavior-preserving

Two required splits were landed. Both are pure relocations verified by the compiler and the full
test suites, so they are behavior-preserving by construction.

**`Request` dispatch (`crates/fux/src/ecs/systems/requests.rs`, 1905 -> ~1420 lines).** The
control-action handlers (`split`, `focus`, `kill`, `resize`, `tab_action`, `workspace_action`,
`set_right_click`, `pane_in_workspace`, `tab_in_workspace`, `check_viewer_admission`) moved into
a new sibling module `requests_control.rs` (~500 lines). `apply_control` stays as the dispatcher
and calls them; `Context` and the handful of helpers they share became `pub(super)`. No directory
restructuring (no `git mv`): `requests_control` is a plain `mod` sibling under `systems`.

**`controller.rs` (`crates/fux/src/client/controller.rs`, 4148 -> ~1470 lines).** The large
inline `#[cfg(test)] mod tests` (2612 lines) moved to `controller_tests.rs` via a `#[path]`
child module (the same pattern the file already used for `control_traces`), and the copy-mode key
handlers (`selection_dragging`, `copy_key`, `scroll_copy`) moved into a `controller_copy.rs`
child `impl Controller` block. Child modules see their ancestor's private fields and methods, so
no field-visibility widening was needed beyond making the three moved methods `pub(super)` for
the parent's key dispatcher.

Verification: `cargo clippy -p fux --all-targets -- -D warnings` clean; `cargo test -p fux --lib`
(193, including the 57 relocated controller tests) and `--test ecs`/`--test structure` pass; the
real `exact-attachment`, `local-attachment`, `detach-drain`, `viewer-transfer-input` and
`control-workflow` scenarios pass. The remaining large files (`view.rs`, `proto/control.rs`,
`client/controller.rs` at its reduced size) are cohesive and left as-is.

## Reconstruction incident (review this file)

An earlier attempt at the `requests.rs` split used `git mv` then `rm` and deleted the
uncommitted working-tree changes to `crates/fux/src/ecs/systems/requests.rs` — the
exact-attachment admission handler (its half of the `initial`/`required_process` handshake; the
other files' halves were intact). No source snapshot or git object held it. It was reconstructed
from the intact contract (the `Viewer` component, the `ViewerAttached` message, `proto/attach`,
`server/connections.rs`, `client/mod.rs`) and proven behavior-correct by:

- `fux-xtask scenario exact-attachment` (exact initial pane, private focus/defaults, stale-hello
  refusal, target-only input, close without sibling fallback) — pass.
- `cargo test -p fux --lib --test ecs --test structure` and the real `local-attachment`,
  `detach-drain`, `viewer-transfer-input` scenarios — pass.
- End-to-end `fux-xtask scenario zor-multi-machine` through the published koh pin — pass.

The subsequent `Request`-dispatch split (above) was redone safely on the reconstructed file with
no `git mv`. The reconstruction is behavior-identical by these tests but not guaranteed
byte-identical to the lost original; **`apply_attachments` and `retain_required_process` in
`requests.rs` should be reviewed directly.**

## Bonus fix

`crates/zor/src/machines/connection.rs`: the koh capability-probe `announced` helper (added in
the multi-machine work) used a panicking slice. Replaced with a checked `.get(..end)`; this also
clears a latent `clippy::indexing-slicing` failure under the workspace `--all-targets` gate.

## Final verification

`cargo fmt --all --check`; `cargo clippy --workspace --all-targets -- -D warnings`; the zor
clippy matrix (all-features, no-default, cli); `cargo test -p zor --lib` (197); `cargo test -p
fux --lib --test ecs --test structure` (193 / 84 / 12); the journal, exact-attachment, and
zor-multi-machine harness runs above. No commits, pushes, or koh changes were made.
