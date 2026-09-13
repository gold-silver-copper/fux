# Codebase improvement report

Status: implementation in progress. This is the current entry point for
`improve-fux-zor-codebase-prompt.md`. No commit, push, PR or companion checkout
change is part of this execution. The [historical execution ledger](verification/codebase-improvement/execution-ledger.md)
preserves intermediate results and failed experiments; its old pending statuses
are superseded by this report.

## Architecture and changes

- **fux** owns PTYs/processes, terminal state/history, layout, viewer interactions,
  reliable input receipts and bounded event/final evidence.
- **zor** owns agent interpretation, task orchestration, retries/recovery policy,
  checks, artifacts and worktrees. Process location never grants new ownership.
- **local-ipc** owns bounded authenticated local transport, without domain policy.

Zor's typed `fux/manager.rs` and `fux/input.rs` operations replace repeated JSON
construction and response inspection for location, pin release, input receipts and
final evidence. Caller policy still decides retries. Wrong-operation, malformed or
mismatched replies cannot become pending results. Sixteen byte-identical fixture
pairs exercise producer serialization and consumer decoding. Consumer DTOs preserve
standalone packaging without introducing an ECS/runtime dependency or a protocol
framework.

Fux's `history.rs`, `capture.rs` and `interaction.rs` own retained history, gesture
tails and modal key/frame transitions. Modal completion retires local ownership
before applying its requested effect. `effects.rs` retains ordered bounded effects;
`read_window.rs` retains bounded correlated reads and fixed deadlines. The controller
coordinates these owners instead of duplicating their storage and cleanup rules.
For example, a modal key handler now returns `Completion::Finish`; one controller
path retires its epoch and local ownership before applying the effect. A new mode
uses that completion path instead of duplicating cleanup beside every successful
or cancelled action. History recency, eviction, identity invalidation and read
selection are likewise maintained by one owner.

Zor's `tasks/lifecycle.rs` and attachment/worktree modules centralize launch,
receipt, attachment, stop and worktree transitions. Intent commits precede external
effects; receipt/final evidence publication preserves identity and task outcomes.
The existing journal locking, atomic replacement and sync discipline remains.
See [the transition contract](zor-lifecycle-transitions.md).

## Baseline failures and regressions

| Failure | Cause and correction | Regression evidence |
| --- | --- | --- |
| Group and group scheduler | Fixture aliases exposed only workspace sockets; now expose manager discovery too | Both real group scenarios pass targeted runs |
| Recovery | Outage fixture hid a workspace socket while reconciliation uses the manager | Workspace-only outage preserves identity; manager outage distinguishes uncertainty from replacement |
| Service | Obsolete proxy route plus a real startup-election race | Alias/admission barrier corrected; losing starter waits for the winner within a fixed deadline, without respawn |
| Headless pin release | Process exit between attachment/live verification and release | Controlled exits at both windows recover only matching final evidence, preserve task state and prohibit duplicate creation |

The routing and pin failures were reproduced before fixes. Retained logs include
`baseline-*.log`, `pin-after.log`, `pin-v3.log`, `service-v1.log` and `service-v2.log`
under `docs/verification/codebase-improvement/`. The first full-gate workspace run passed all 24 automation scenarios, including
resume, and the corrected consumer inventories, then stopped at a standalone
fixture binary-path configuration error. After correcting the runner, the second
full gate passed all 40 checks and Betamax replay/report generation.

## Ownership and recovery coverage

Named controller tests cover A → B → A independent history, viewer isolation,
Escape dismissal, exact application bytes, paste/prefix handoffs, mouse routing,
auxiliary tails, fresh-press recovery, drag cancellation, stale replies and bounded
fair reads. Focused history/modal/gesture/transfer/tiny-layout scenarios retain an
end-to-end viewer composition. An independent specification model runs 256 seeds ×
128 events with two viewers and panes. It saves seeds and minimizes failing traces
for explicit replay. See [controller trace testing](controller-trace-testing.md).

Lifecycle tests reject attachment and receipt identity replacement, evidence
regression, adopted stop authority and worktree replacement. Real launch, task,
recovery and worktree scenarios exercise lost replies, killed callers, duplicate
retry, server replacement, moved resources and missing/expired evidence. Some
worktree/stop crash states are persisted fixtures, not injected fsync failures.
The attempt/group policy audit is documented in the transition contract. Fresh
resume attachment coverage now exercises the shared branch and its exit-before-pin
window; the current full workspace run also passes the added scenario.

The asynchronous interaction audit inspected chooser completion, buffered replay,
epoch checks, target invalidation and pointer capture. No new defect was confirmed
in those interaction paths. The subsequent launch diff review confirmed a
resume-specific final-evidence lookup error after the old launch is archived. A
new `zor-resume` real-process regression reproduced `final launch pane mismatch`.
Pin-release recovery now selects the current task launch after archiving. The
same regression passes, proving that the new attempt closes, archived evidence
and unsent prompts remain unchanged, retry creates no extra process, and no input
is replayed. This uses a synthetic adapter, not a claim of provider session quality. The separate source review and its scope are retained in `verification/codebase-improvement/final-review.md`; performance conclusions still require review.

## Diagnostics and failure evidence

Opt-in fux and zor diagnostics retain bounded transition/request/identity metadata
without input or prompt contents. Logging cannot become lifecycle authority.
Harness failures automatically retain bounded final terminal models, source/binary
identity, local RPC metadata and diagnostic tails after fixture teardown.
See [diagnostics and failure artifacts](diagnostics-and-failure-artifacts.md).

The real launch failure probe retained 179 zor records after its passing scenario
was deliberately failed after teardown. Tests also cover private permissions,
symlink/FIFO refusal, lock contention, rollover and record limits. Evidence includes
`zor-diagnostic-failure-probe.json`, `zor-diagnostics-source.json` and related logs.

## Current verification

| Check | Latest observed result |
| --- | --- |
| Zor default library | 163 passed, two existing ignored |
| Standalone harness without Betamax | 29 library tests passed |
| Standalone harness with Betamax | 36 library and 36 binary tests passed |
| Root formatting and workspace strict all-target Clippy | Passed |
| Harness strict all-target Clippy with Betamax | Passed |
| Full workspace with Betamax, explicit zor and no exclusions | Passed, including all 24 automation scenarios, 18 local CLI scenarios and corrected contract inventories |
| Zor feature combinations, contracts, docs and package checks | Passed in the corrected 40-check full gate |
| Fresh Betamax report/replay and visual inspection | Exact replay/report passed for 412 frames; 20 affected normal/tiny checkpoints inspected; sampled visual review retained separately |
| Representative release performance comparison | Main 48 runs passed; focused follow-up timeout remains unresolved |
| Separate source review | Completed in `verification/codebase-improvement/final-review.md`; performance conclusions remain pending |

Prior integrated command (completed; logs retain the two inventory failures):

```sh
PATH=/tmp/fux-betamax-toolchain/lib/python3.14/site-packages/ziglang:$PATH \
FUX_BETAMAX_FONT='Noto Sans Mono CJK SC' \
FUX_BETAMAX_DIR=/tmp/fux-codebase-work/betamax-integrated-20260913 \
FUX_HARNESS_ARTIFACTS=/tmp/fux-codebase-work/integrated-failures \
FUX_REQUIRE_ZOR_BIN=1 ZOR_BIN=/tmp/fux-codebase-work/build/debug/zor \
cargo +stable test --workspace --locked --no-fail-fast \
  --target-dir /tmp/fux-codebase-work/build -- --test-threads=1
```

Log: `/tmp/fux-codebase-integrated-workspace.log`. Timing-sensitive process tests
run without overlapping builds. The inventory mismatch arose because new generic
interaction/diagnostic declarations were not in the reviewed snapshot; the changed
declarations are retained in `boundary-declaration-review.json`. No agent policy
was added to fux. A normal three-test boundary rerun passed after the reviewed update.

## Provenance and remaining work

The original dirty baseline snapshot was lost when the repository target directory
disappeared; the cause was not established. Baseline failure logs remain, but Git
HEAD must not be substituted for that lost dirty source. A later checkpoint outside
build output preserves the state after the first routing fixture corrections.
Its performance baseline reconstruction verifies 253 manifest source files against
retained hashes, using hash-matching current copies for omitted unchanged files.
It is explicitly a later checkpoint, not the original baseline.

The [verification entry point](codebase-verification.md) now provides targeted and
full commands using the existing bounded gate runner. The corrected full gate and supported feature checks passed. Remaining work is the pressure-timeout investigation, attribution of the repeated
recovery retry wall-time increase, and final review. Final harness checks and
four-binary source-provenance verification now pass.
The new performance harness commands were added after the full gate; their earlier
affected checks passed, as recorded below. No universal correctness,
native OS input validation, live-provider coverage or cross-product parity is
claimed by these headless results.


The first one-command full gate retained unchanged before/after source identity
and passed checks 0–19. Check 20 failed because the runner omitted `FUX_BIN` for
standalone fixture tests when using an external Cargo target. The runner now
supplies the exact fresh fux path alongside zor. The affected suite then passed
all 3 unit, 9 binary and 2 lifecycle tests with the explicit binary path. The corrected full gate passed all 40 checks, Betamax replay/report generation
and the unchanged-source check. Its record is `full-gate-passed.json`; the earlier
failed run remains retained separately.
Evidence: `verification/codebase-improvement/full-gate-first-result.json` and
`fixture-explicit-binary.log`. The 412-frame gallery is retained under the gate's
recorded temporary directory. Twenty reviewed checkpoints and their labels are
retained under `verification/codebase-improvement/visual-checkpoints/`.


## Resume regression and consumer inventory correction

The first resume fixture attempt observed final evidence before fux had published
it. The fixture now waits for the matching completed manager final record; this
keeps product deadlines unchanged. The corrected fixture then reproduced the
runtime lookup defect against the unchanged zor binary. After the one-line
identity selection fix, the same scenario passed. Evidence: `resume-before-v2.log`,
`resume-after.log`, `resume-exit-before-fix.json` and `resume-fix-source.json`.

Consumer inventory paths now point to `fux/manager.rs` and `fux/input.rs`, which own
the typed wire operations. The verifier recognizes serde kebab-case enum variants
as wire names and rejects generic/nonmatching naming rules. Workspace-local
input-status remains supported through the raw debugging CLI; an explicit `fux
ctl` receipt assertion was added instead of claiming that zor still sends that
workspace request. Normal contract verification passes 3 boundary, 3 producer
fixture and 4 consumer tests. The full workspace run also passes the new CLI assertion.


## Release comparison preparation

The full gate completed with unchanged source identity; its current gallery is
recorded in `verification/codebase-improvement/full-gate-passed.json`. The prior
412-frame gallery supplied 20 direct normal/tiny visual inspections, recorded in
`verification/codebase-improvement/visual-review.md`; rendering source did not
change between these runs.

Existing latency, frame-byte and resize measurements now retain chronological raw
samples. New xtask commands `measure-interactions FUX_BINARY` and
`measure-recovery FUX_BINARY ZOR_BINARY` measure rendered scroll completion,
manager lookups and reconciliation after a controlled lost create reply. They
validate unchanged process identity and prohibit duplicate creation. Their strict
Clippy check, candidate workload smoke runs and updated Betamax harness tests
(36 library + 36 binary) passed. The first ad hoc harness-test invocation omitted
the documented CJK font setting and failed its wide-glyph check; the explicit-font
invocation passed, with both logs retained.

Both release builds completed in separate external target directories. The full
workspace also passes `cargo +1.95.0 check --workspace --all-targets --locked`.
The main alternating comparison completed with all 48 runs passing. Its raw
results and qualified measurements are retained in the performance investigation;
no stable overall performance improvement is claimed.


The main 48-run release comparison passed, but its focused recovery/pressure
follow-up encountered a slow-reader timeout. See the retained
[performance investigation](verification/codebase-improvement/performance-review.md).
Phase-specific harness failure evidence has been added. A fixed diagnostic run
passed all 20 cases, including five slow-reader cases, without reproducing the
timeout. All twelve fixed alternating follow-up runs also passed. Final Betamax-enabled
harness tests passed (36 library + 36 binary), and Cargo confirmed all four
measured release binaries fresh with unchanged hashes. Retry median wall time
remained higher (29.604 / 35.973 ms); child CPU increased much less
(3.638 / 3.839 ms). Its attribution and the original timeout remain unresolved
and prevent completion.


The fixed six-run recovery attribution comparison passed all 72 retries, each
with exactly two ordered manager requests. Median combined proxy handling was
0.826 / 0.837 ms; most elapsed time occurs before, between or after those
requests. This narrows attribution without proving the remaining overhead is
entirely a measurement artifact. See the updated performance investigation.
The pressure fixture now rejects unsuccessful split/input replies immediately
and records the last readiness count/stale flag. All four real pressure cases,
strict harness Clippy and 36 library + 36 binary tests pass with these changes.
The old timeout is not claimed fixed.


The pressure reproduction campaign completed all 200 cases, including 50 slow
readers, without reproducing the historical timeout. A controlled readiness-based
proxy comparison passed all six runs: retry median wall time is now
14.288 / 14.630 ms and child CPU 3.492 / 3.463 ms. The inter-request fixture
gap fell from roughly 13 ms to 0.05 ms. This completes the latency investigation
within the documented measurement limits; no product speedup is claimed. The
historical timeout remains unexplained. A final integrated gate is next because
the proxy helper is shared by real-process integration scenarios.
