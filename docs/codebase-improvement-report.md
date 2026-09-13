# Fux and zor codebase improvement report

Status: implementation, performance investigation and the final integrated
verification rerun are complete. The overall objective is blocked
on missing diagnostic evidence for one historical pressure-test timeout. Three
final audits confirmed the same evidence gap after other required work finished.
No commit, push, PR, release or companion-checkout mutation is part of this work.

## Architecture and resulting changes

| Owner | Responsibility |
| --- | --- |
| fux | PTYs/processes, terminal state and history, layouts, viewer interactions, reliable input receipts, bounded event/final evidence |
| zor | Agent interpretation, task orchestration, retries and recovery policy, checks, artifacts and worktrees |
| local-ipc | Bounded authenticated local transport and socket discipline, without multiplexer or agent policy |

Zor's typed `fux/manager.rs` and `fux/input.rs` operations replace duplicated wire
construction and nested response inspection in routing, launch, submission,
binding, integration and wait callers. They validate operation, correlation,
status and immutable process identity. Malformed replies cannot become retryable
pending evidence. Sixteen paired producer/consumer fixtures exercise actual fux
serialization and zor decoding. Consumer DTOs preserve standalone packaging without
introducing an ECS dependency or a new protocol framework.

Fux's `history.rs`, `capture.rs` and `interaction.rs` centralize retained history,
gesture ownership and modal key/frame transitions. A modal handler returns
`Completion::Finish`; the controller retires local ownership before applying its
effect. Ordered effects and correlated history reads retain dedicated bounded
owners. Adding or dismissing an interaction no longer requires duplicating its
storage and cleanup rules across key handlers.

Zor's lifecycle APIs centralize managed attachment, delivery, stop observations and
worktree transitions. Durable intent precedes external effects; reconciliation
uses immutable process identity and retained receipts. Managed and adopted cleanup
authority stay distinct. Existing journal locks, atomic replacement and directory
sync protections remain. Group policy retains its own owner and delegates shared
admission/delivery transitions. See [the lifecycle contract](zor-lifecycle-transitions.md).

## Confirmed failures and fixes

| Failure | Cause and correction | Evidence |
| --- | --- | --- |
| Group and scheduler scenarios | Fixture aliases exposed workspace sockets without manager discovery | Corrected aliases; both real scenarios pass |
| Recovery scenario | Workspace-socket outage was treated as manager outage | Separate positive workspace-only and manager-outage assertions |
| Service startup | Obsolete fixture route plus a real election/startup race | Correct routing; losing starter waits within a fixed deadline without respawn |
| Headless pin release | Process could exit between durable attachment, live verification and pin release | Controlled exits reconcile only matching final evidence; no duplicate launch or invented task success |
| Resume pin release | Recovery looked up the archived launch after attaching a new attempt | Lookup uses the current task launch; real-process regression reproduced the failure before the fix and passes afterward |
| Verification runner | Standalone fixtures lacked `FUX_BIN` when Cargo used an external target | Runner supplies exact matching fux/zor paths; affected fixture checks and the corrected full gate pass |
| Recovery benchmark latency | Proxy slept unconditionally between accepted connections | Readiness polling removes fixture delay while retaining cancellation deadlines; controlled comparison below |

Original failures, unsuccessful probes and corrected results remain under
`verification/codebase-improvement/`. The resume test uses a synthetic bundled
adapter; it does not prove live-provider conversation restoration.

## Behavioral and recovery coverage

Controller tests preserve independent A → B → A histories, viewer isolation,
one-Escape dismissal, exact application bytes, fragmented prefix/paste handling,
application mouse and Shift routing, auxiliary tails, drag cancellation, stale
reply rejection, fixed deadlines and bounded fair reads. Focused viewer scenarios
retain end-to-end composition. An independent model runs 256 seeds × 128 events
with two viewers and panes, retains minimized failing traces and supports replay.
See [controller trace testing](controller-trace-testing.md) and the named invariant
assertions in [the requirement audit](verification/codebase-improvement/completion-audit.md).

Lifecycle tests and real-process scenarios cover lost replies, killed callers,
duplicate retry, server replacement, moved panes, missing/expired final evidence,
adopted/historical stop rejection and resumed attempts. Some worktree/stop crash
states are persisted fixtures, not injected fsync failures. Evidence gaps remain
explicit uncertainty; neither process disappearance nor command success invents
task completion or grants destructive authority.

Opt-in diagnostics retain bounded private transition/request/identity metadata,
without prompt, terminal or paste payloads. Failure artifacts retain bounded
screens, RPC metadata, source/binary identity and standard-root diagnostics after
teardown. Tests cover bounds, lock contention, symlink/FIFO refusal and cleanup.
Custom diagnostic directories are not automatically discovered. See
[diagnostics and failure artifacts](diagnostics-and-failure-artifacts.md).

## Verification

The [verification entry point](codebase-verification.md) documents targeted and full
commands. The latest rerun uses the full profile, with no exclusions:

```sh
PATH=/tmp/fux-betamax-toolchain/lib/python3.14/site-packages/ziglang:$PATH \
FUX_BETAMAX_FONT='Noto Sans Mono CJK SC' \
CARGO_TARGET_DIR=/tmp/fux-codebase-work/build \
cargo +stable run --manifest-path tools/xtask/Cargo.toml \
  --target-dir target/codebase-runner --locked -- verify-codebase full
```

The final full gate passed all 40 commands, exact Betamax replay/report generation
and the unchanged before/after source check. Records: [gate result](verification/codebase-improvement/full-gate-final.json),
[command log](verification/codebase-improvement/full-gate-final.log), and individual
outputs in `verification/codebase-improvement/full-gate-final-logs/`. All 24
automation scenarios and 18 local CLI scenarios passed. All four measured release
binaries were subsequently confirmed fresh with unchanged hashes in
`verification/codebase-improvement/release-provenance-final.json`.

Earlier retained verification includes a complete 40-check gate, all 24 automation
and 18 local CLI scenarios, supported zor feature combinations, packaging, offline
validators and exact Betamax replay. Rust 1.95 workspace/all-target checking also
passes. Latest affected harness checks pass strict Clippy and 36 library plus 36
binary tests. An ad hoc invocation that omitted the documented CJK font failed;
the explicit-font rerun passed, and both logs remain retained.

Twenty affected normal/tiny visual checkpoints were inspected directly from the
412-frame gallery. Their labeled PNGs and inspection notes are retained under
`verification/codebase-improvement/visual-checkpoints/` and `visual-review.md`.
This is selected transition coverage, not manual inspection of every frame or
native OS-input validation. Product rendering source has not changed since that
visual review. Nine additional checkpoints from the final 411-frame gallery were
inspected directly; they are retained under
`verification/codebase-improvement/final-visual-checkpoints/`. No new visual
defect was confirmed.

## Performance findings

The [performance investigation](verification/codebase-improvement/performance-review.md)
retains the 48-run main release comparison, raw samples, alternating order, host
observations, subsequent comparisons and the failed follow-up. Workloads cover
idle/output, many viewers, 2/8/32-pane resizing, history memory, rendered scrolling,
manager operations, journal operations and controlled lost-reply recovery.

Main comparison: resize frame bytes and four-viewer median key-frame bytes are
unchanged. Scroll medians are effectively equal. Burst bytes vary with coalescing;
these results do not justify removing identity or revision metadata. Sampled
10k-history RSS medians are 40,144 / 40,208 KiB. Highest sampled RSS is not a kernel
peak, and viewer decoder backlog is not internal runtime queue allocation.

The original polling proxy produced substantially higher candidate retry medians.
Bounded per-request timestamps localized most elapsed time outside RPC handling.
A fixed comparison after replacing unconditional proxy sleeps with socket readiness
passed all six runs and 72 retries:

| Pooled median | Baseline | Candidate |
| --- | ---: | ---: |
| Recovery wall time | 27.028 ms | 26.799 ms |
| Retry wall time | 14.288 ms | 14.630 ms |
| Retry child CPU | 3.492 ms | 3.463 ms |
| Between proxy requests | 0.055 ms | 0.047 ms |

The inter-request gap was about 13 ms with the old fixture. Retry p95 is now
15.776 / 15.890 ms. CLI exit observation still polls every 10 ms. This establishes
material fixture distortion, not a product speedup or identical product cost.
No further product optimization is justified by these measurements. Identity,
durability and backpressure guarantees remain intact.

## Remaining failure and provenance limits

One earlier four-pane/four-viewer slow-reader case failed with the old generic
`performance fixture observation` timeout. Its failing phase and terminal state
were not retained. New diagnostics retain phase, reader counters/markers and zor
readiness summaries, and reject unsuccessful split/input replies immediately.
A predetermined 200-case campaign, including 50 slow-reader cases, passed against
unchanged binaries. Those successes do not establish a cause or fix for the
historical failure. This is the remaining obstacle to claiming the full objective
complete; it is not a newly confirmed product defect.

The original initial dirty snapshot was lost when the root target directory
disappeared; its cause was not established. Performance and diff review therefore
use an explicitly labeled later retained checkpoint, reconstructed against 253
source hashes. Git HEAD was not substituted for the lost dirty baseline. Results
cannot attribute changes that predate that checkpoint.

A separate self-review is retained in
[final-review.md](verification/codebase-improvement/final-review.md), with the exact
97-file comparison scope, relevant untracked files, findings and verification.
No independent subagent was authorized. Twenty-nine vendor compilation files
match retained hashes; 454 preexisting theme/license assets lacked historical
checkpoint hashes and are not attributed to this task. Universal correctness,
live-provider quality, native OS-input coverage and cross-product parity are not
claimed by these headless checks.

Earlier progress notes are preserved in the [execution ledger](verification/codebase-improvement/execution-ledger.md)
and [pre-consolidation report](verification/codebase-improvement/report-before-final-consolidation.md).
