# Codebase improvement report

Status: in progress. This executes `improve-fux-zor-codebase-prompt.md`; no
completion is implied by partial checks below. No commit, push or PR is authorized.

## Architecture contract

- fux owns PTYs/process lifecycle, authoritative terminal state/history, layouts,
  viewer-local interactions, reliable input receipts and bounded event/final evidence.
- zor owns agent interpretation, task orchestration, recovery/retry policy, checks,
  artifacts, worktrees and workflow decisions. Location discovery never changes
  immutable process identity or grants destructive authority over an adopted pane.
- local-ipc owns authenticated bounded local transport and socket discipline,
  without multiplexer or agent policy.

Typed wire operations must preserve these boundaries; neither zor's workflow layer
nor a shared wire declaration may depend on fux ECS/viewer internals. Transition
owners must keep byte decoding, interaction policy and external effects distinct.

## Baseline

The interrupted initial goal turn had no observed implementation progress. The
resumed process inventory found no task-owned Cargo/harness job still running.
Existing unrelated processes were left alone. Starting revision, dirty status,
source/document hashes and source copies are recorded in
`target/codebase-improvement/baseline/`. Existing pane/layout, UX, Betamax and zor
migration edits are preserved; Git HEAD alone does not identify this baseline.

Fresh baseline builds passed for fux and for zor both with default features and
with `--no-default-features --features cli`. All four historical routing failures
were reproduced against those binaries; logs are in
`docs/verification/codebase-improvement/baseline-*.log`. Historical UX verification
(local 18/18 and automation 19/23) is context, not this task's baseline evidence.

## Required work and evidence

| Requirement | Status |
| --- | --- |
| Routing fixtures and pin-release diagnosis/regressions | Reproduced and corrected; targeted regressions pass; final integrated rerun pending |
| Typed routing, pin release, input receipts and final evidence | Implemented; producer/consumer and targeted process checks pass; final gate pending |
| Explicit fux interaction ownership/transition extraction | History, capture and modal keyboard owners implemented and tested; remaining transition audit pending |
| zor lifecycle transition APIs and crash recovery | Launch, attachment, delivery, stop and worktree transitions implemented; targeted unit/process checks pass; complete crash/attempt-policy coverage audit pending |
| Focused harness scenarios and generated reference-model traces | Independent controller model with paste/prefix/gesture events and replay/minimization implemented; focused scenarios and composition pass; consolidated full-gate command and final integrated evidence pending |
| Structured bounded diagnostics and automatic failure artifacts | Fux and zor opt-in diagnostics and automatic scenario artifacts implemented and tested; final coverage audit pending |
| Representative release performance comparison | Pending |
| Consolidated docs, complete checks, Betamax review and separate diff review | Pending |

## Routing reproduction and first corrections

- Both group variants fail adoption through the fixture alias: only `default.sock`
  was linked, while adoption now validates launch origin through `manager.sock`.
  The alias now exposes both endpoints. Coordination assertions are unchanged.
- Recovery expects a workspace-socket outage to make manager-based reconciliation
  fail. It now explicitly proves that a workspace-only outage preserves verified
  identity, then removes the manager endpoint to test uncertainty and preservation
  of previously established replacement evidence.
- Service overload fails adoption through its proxy for the same missing manager
  route. The proxy runtime now includes manager discovery; the controlled barrier
  targets the actual input reservation instead of the obsolete preliminary list.
  Observation responsiveness and overloaded-request non-admission assertions remain.

These corrections are under targeted verification. No runtime code has changed
in this phase; later failures may expose additional fixture or production issues.

### Targeted results after corrections

`zor_group` (both variants) passes: 2/2, 41.60 seconds. `zor_recovery` passes:
1/1, 14.20 seconds. The service rerun is still running; its result is not yet
acceptance evidence. These checks used required explicit freshly built zor.

The historical headless error is specifically `release-pane-pin` returning
`not-found: live pane not found`. Source inspection shows a possible exit window
between durable attachment and creation-pin release, and another between live
verification and release during reconciliation. This is a hypothesis requiring a
controlled exit-before-release reproduction. Existing launch proxy tests already
cover dropped release requests/replies and must remain intact. Do not relax fux's
process/explicit-pin validation to hide the race.

### Further findings and interrupted build evidence

The service proxy correction progressed to concurrent service starters, where a
losing child observes EAGAIN on the service lock and its parent immediately probes
an endpoint not yet created by the winner. This fresh failure is retained in
`service-v1.log` and remains in scope; it is not a passing service result.

A deterministic `exit_before_pin` launch-proxy injection and exact no-duplicate
launch assertions were added. Their first invocation did not reach execution:
Cargo reported a missing `syn` rmeta during compilation. Subsequent filesystem
inspection confirmed all previous `target/debug` binaries, the standalone harness
and `target/codebase-improvement/baseline` had disappeared. The cause of removal
was not established. The original target-local baseline snapshot is therefore
unavailable; retained baseline logs remain valid, but cannot stand in for the
missing source snapshot. A current checkpoint (including these fixture edits) is
saved outside the repository target at `/tmp/fux-codebase-work/checkpoint`.

A fresh build uses `/tmp/fux-codebase-work/build`, log
`/tmp/fux-codebase-isolated-build.log`. At this checkpoint it is running (tool
session 73997). The pin regression remains unexecuted; no runtime fix or completion
claim is made. Future source review must distinguish the reconstructed initial
fixture state from this later checkpoint rather than label it the original baseline.

## Pin-release and election runtime corrections

The isolated rebuild passed. `zor-launch` then deterministically reproduced the
historical not-found release error by killing the exact new process after durable
attachment and before forwarding its release. Zor now records matching retained
final evidence through a shared completion transition if the process exits in
this window. Normal reconciliation also considers final evidence if release
fails after a successful live check. Missing/pending final evidence still fails;
no operation is blindly replayed. Existing dropped-request/reply assertions and
the new no-duplicate launch/task-outcome assertions pass in `pin-after.log`.

Service election now distinguishes a competing starter from other startup errors.
The loser waits only for absent winner endpoints within the original five-second
startup deadline, without another spawn; malformed/non-absence failures stop.
Three service unit tests pass, strict cli-feature Clippy passes, and the entire
real `zor-service` scenario passes in `service-v2.log`. This supersedes the earlier
pending/failing service entries for this source snapshot. A final all-scenario
run remains required after subsequent architecture changes.

## Typed boundary extraction in progress

`fux/manager.rs` now owns typed location and pin-release requests/envelopes with
exact operation, ID and payload validation. `tasks/route.rs` retains immutable
process/origin checks and task policy. The bounded authenticated transport is
unchanged. Consumer DTOs avoid coupling zor to the fux ECS or adding a new crate
for two operations. Receipt/final operations and producer/consumer contract
coverage still need migration; extraction is not complete. Current library and
Clippy checks are running against this addition.

The first typed-boundary check passes 122 active zor library tests (two existing
ignored tests) and strict all-target cli-feature Clippy. An additional real launch
assertion now forces exit between successful live verification and release during
reconciliation, preserving the original session/task and creation count. The
updated launch scenario passes with rebuilt typed-boundary zor, including both
forced exit windows and the original lost-request/reply assertions. Evidence:
`docs/verification/codebase-improvement/pin-v3.log` (process exited 0).

## Final-record and receipt boundary migration

Final evidence now has a typed record/capture and operation-specific pending
outcome. Launch recovery, task waiting and `zor run` no longer inspect nested
JSON paths for final fields. The old permissive final-envelope parser is removed;
its eviction/expiry/conflict tests are preserved and strengthened at the new
boundary with wrong-ID, wrong-kind, malformed-field and missing-exit assertions.
The final-only migration passed real `zor-launch` and `zor-run` scenarios.

Input reserve/submit/status now return typed receipts with an exhaustive wire
state enum. Task policy still validates operation/pane identity, retention, byte
counts and delivery transitions. Binding and integration disarm use the same
status operation. `zor run` now uses typed location/release operations too,
retaining its missing-pane and replacement-server cleanup policy. No retry is
added to an input effect.

Sixteen wire fixtures are shared as byte-identical producer/consumer copies so
zor remains independently packageable. fux checks actual schema decoding and
exact reserialization; zor checks serialized requests and typed reply decoding.
No fux runtime/ECS dependency was added to zor. A prior run unit fixture omitted
tab/revision fields required by the shared location DTO; it now contains the
actual producer fields rather than weakening the DTO.

The combined migration is under library, producer-fixture, Clippy and real
receipt/run verification (`/tmp/fux-codebase-wire-*-v4.log`, session 21501).
Final complete CI-equivalent checks, replay review and the larger controller,
lifecycle, generated-harness, diagnostics and performance work remain outstanding.

Combined wire migration results so far: 125 active library tests passed (two
pre-existing ignored); all three fux producer-fixture tests passed, including
exact round-trip and byte-identical copies for all 16 new fixtures; strict zor
cli-feature all-target Clippy passed. Real `zor-tasks` passed durable adoption,
preparation, receipt recovery, retry deduplication and human-interference checks.
The final `zor-run` process also passed (exit 0): immediate final bytes/status,
fresh/existing manager, ownership refusal and timeout cleanup. No full-goal completion is implied by these targeted checks.

## Controller ownership extraction

- `client/history.rs` owns private session retention, MRU dismissal, attachment
  identity, visibility/buffer/geometry reconciliation, budget eviction and fair
  read scheduling. The controller can transfer a viewport into/out of keyboard
  Copy through `take`/`remember`; it cannot mutate the session vector or cursor.
  Reply installation returns an explicit installed/invalidated/unowned outcome.
- `client/capture.rs` owns left and auxiliary gesture tails. Adoption, cancellation,
  release consumption and fresh-press recovery use one API instead of controller
  flags. Selection/layout code still owns its active gesture, and hands off its
  tail when that interaction ends.
- `client/interaction.rs` owns modal state declarations, text/loading ownership
  predicates and frame-driven target validation. Reconciliation returns an
  explicit keep/dismiss decision; notifications and cancellation remain in the
  controller. Keyboard command handlers and broader transition/effect cleanup
  still need work before claiming the full interaction-refactor requirement done.

All 107 client regressions passed after each extraction. All-target fux Clippy
passed after history/capture extraction. The first real history run exposed a
fixture observation bug: after selected resize it was in Copy, but its Escape
wait checked only absence of the already-absent passive History hint. A following
prefix could arrive before Escape resolved. The fixture now waits for both Copy
and History to be absent; it adds no sleep and changes no runtime input rule.
The corrected history/mouse PTY rerun is underway (session 2280).
The pre-extraction controller is retained outside target at
`/tmp/fux-codebase-work/controller-before-history.rs` for scoped review.

The corrected real-PTY history scenario passes independent histories, one-Escape
normal input, focus/prefix and viewer isolation. The reporting-app scenario passes
exact mouse bytes, Shift history override, buffer transitions and copy/paste
receipts. Both ran against the modal/history/capture extraction. No rendering
capture was enabled in these targeted runs; fresh Betamax evidence is still due
at the full verification gate. The first failed fixture observation is retained
in `controller-history-pty.log` alongside the corrected passing logs.

## Explicit keyboard transitions

Modal key handlers now return `KeyTransition`: keep/finish ownership, at most
one typed effect (control, manager, projected action, copy or scroll), and an
optional notice. They edit local field/chooser state but do not issue I/O or
write controller effect channels. The controller performs shared end-of-interaction
cleanup before handing off the selected effect. Copy/q completion uses the same
cleanup. Immediate layout arrow effects keep their mode; Enter finishes without
undo. The internal cleanup method is named `end_interaction`, covering completion
and dismissal without an implicit parent menu.

108 client tests and strict all-target Clippy pass for the transition extraction.
The added field-submission regression checks exact request retention, normal input
ownership, retirement of the interaction epoch and removal of painted field bounds.
Its initial fixture lacked the instance required to enter RenamePane; correcting
the fixture identity made that assertion execute.

The broad viewer run stopped when a drag preview was invalidated by a late layout
frame after a swap. Its server-listing assertion did not synchronize the attached
viewer. The fixture now acknowledges a private next/previous focus round trip,
restoring original focus before beginning the revision-pinned drag. Existing
wrong-button/wheel no-commit and canceled-tail assertions remain unchanged. This
correction and the delayed manager scenario are under rerun (session 44621).

The corrected broad viewer scenario now passes launch, menus, tabs, splits,
resize, close, copy, multiple viewers, tiny screens, workspaces and detach with
the explicit key transitions. The delayed-manager continuation also passes lookup/mutation, cancel/re-entry,
local history, dependent commands and original input targeting.

## Durable launch and delivery transitions

`tasks/lifecycle.rs` now owns creation submission, creation uncertainty, attached
launch observations and prompt receipt publication. These methods perform no I/O;
callers use the existing `Store::transaction` lock, validation and atomic journal
replacement. Live/final evidence is authenticated by the existing routing and wire
boundary before reaching these transitions.

| Boundary | Durable rule | Recovery rule |
| --- | --- | --- |
| Creation | Commit Prepared → Submitting before sending split; record a nonzero, unchanged pane ID from its reply | Submitting/Uncertain cannot begin another creation; discover the original launch through existing reconciliation |
| Attachment observation | Validate launch/session/attempt associations and managed ownership before changing launch and attempt together | An outage preserves prior replacement evidence; final evidence closes the launch and finishes the attempt without setting task success |
| Input submission | Preserve the reservation operation and expiry; set Submitting and the monotonic integration arm marker before sending input | Lost request/reply can return to Reserved only for that same operation; publication rejects terminal delivery regression and decreasing byte/input-sequence evidence |
| Receipt publication | Submit, report binding and integration disarm share receipt validation | Publishing a receipt preserves response, release and terminal wait evidence; uncertainty cannot erase Delivered/Failed |

Seven focused regressions cover resubmission rejection, changed creation identity,
late uncertainty after closure, wrong/adopted ownership, preserved task outcome,
replacement followed by outage, lost input submission and released coordination.
The first launch fixture incorrectly treated mutable location as the retained
target; journal validation rejected it. The corrected fixture retains the original
workspace/stream, as production does, while manager routing resolves movement.
Moved-pane process coverage remains a separate integration requirement.

Verification so far: the zor library suite passed 155 tests with two existing
ignored tests; after the final shared-state helper cleanup, all seven lifecycle
tests and strict all-target zor Clippy pass. Production panic-style accessors
flagged by Clippy were replaced with contextual errors; test fixtures follow the
repository's test-only `expect_used` allowance. Fresh CLI builds and launch, task
delivery and recovery process scenarios are being checked separately.

This is partial lifecycle work. Attachment creation, stop/worktree transitions,
the complete crash-boundary matrix, generated harness traces, diagnostics,
performance measurements and the final integrated/Betamax gate remain open.

The fresh-binary `zor-launch`, `zor-tasks` and `zor-recovery` scenarios all passed
after the lifecycle extraction. This covers the existing lost-creation/pin-release
faults, receipt recovery and duplicate retry, human interference, stop recovery,
ownership isolation, unavailable versus replaced fux and retained history/no replay.
Logs and source/binary hashes are retained under
`docs/verification/codebase-improvement/lifecycle-*`. Commands:

```sh
cargo +stable test --target-dir /tmp/fux-codebase-work/build --locked -p zor --lib
cargo +stable test --target-dir /tmp/fux-codebase-work/build --locked -p zor --lib lifecycle::tests
cargo +stable clippy --target-dir /tmp/fux-codebase-work/build --locked -p zor --all-targets -- -D warnings
cargo +stable build --target-dir /tmp/fux-codebase-work/build --locked -p fux --bin fux
cargo +stable build --target-dir /tmp/fux-codebase-work/build --locked -p zor --no-default-features --features cli --bin zor
for scenario in zor-launch zor-tasks zor-recovery; do
  /tmp/fux-codebase-work/harness/debug/fux-xtask scenario "$scenario" /tmp/fux-codebase-work/build/debug/fux /tmp/fux-codebase-work/build/debug/zor
done
```

These were targeted non-Betamax process runs, without overlapping builds. A scoped
review against `/tmp/fux-codebase-work/before-lifecycle/` confirmed the extracted
launch/submission callers preserve commit-before-effect ordering. This does not
replace the required final full-diff review or the final feature/visual gate.

## Attachment and cleanup ownership

Managed attachment now selects the durable launch inside the transition owner,
rejects changed pane identity and duplicate attachment, and publishes the
session/attempt/task association atomically. The existing explicit resume
authorization and archival operation remain inside that transaction. Creation-pin
release still follows a successful commit.

Stop authority and stop intent share an owner. Adopted or historical launch
records cannot supply a stop target; cancellation and stop_requested are recorded
together before kill. No stop-intent transition invents process exit evidence.
Worktree allocation, creation, readiness, removal and uncertainty use guarded
methods shared by normal execution and recovery. Pinned checkout identity and
force intent cannot be silently replaced, and ambiguous Git operations cannot
restart through the initial transition. Filesystem/Git ownership and active-use
checks remain in the effect callers under the journal lock.

All 13 focused lifecycle tests and strict all-target zor Clippy pass. Fresh
`zor-worktree`, `zor-launch` and `zor-recovery` process scenarios pass, including
the Git filter termination barrier, post-effect journal fixtures, guarded removal,
lost creation replies and stop recovery. No assertions or deadlines were weakened.
The full library rerun is recorded separately when complete. The standalone
harness binary was unchanged for these scenarios; zor was rebuilt from the new
source before running them, with no overlapping builds.

The durable ordering and coverage map is in
[zor-lifecycle-transitions.md](zor-lifecycle-transitions.md). A scoped review
against `/tmp/fux-codebase-work/before-worktree-lifecycle/` checked the moved effect
callers and transition preconditions. The complete crash/attempt-policy coverage
audit and final whole-change review remain outstanding.

The full zor library rerun passed **161 tests**, with two existing ignored tests.
Formatting and diff whitespace checks pass. Logs and exact source/binary hashes
are retained in `docs/verification/codebase-improvement/cleanup-*`. Verification
used the same target directory and commands shown above, with the focused test
filter `lifecycle` and process scenarios `zor-worktree`, `zor-launch` and
`zor-recovery`. No fresh Betamax rendering was involved in these lifecycle checks.

## Generated controller traces and focused viewer modules

The independent controller specification model now exercises 256 deterministic
seeds of 128 events each across two viewers and two panes. It checks local history,
interaction ownership, captured rename targets, dismissal, buffer/size/lifetime
changes and stale lookup/history replies. Invalidation comes from the specification,
not the controller's pending-state checks. Failures retain their seed, metadata and
a deletion-minimized JSON trace with an exact replay command.

The first generated failure minimized to wheel B, wheel A, Escape. This exposed a
model error: the documented contract dismisses only the most recent history. After
correcting that expectation, the generated corpus and saved trace pass. The first
manual replay used a repository-relative path, but Cargo starts tests in the crate
directory; the documented replay uses an absolute path. These were test-model and
invocation corrections, not runtime fixes.

109 client tests pass, including 32,768 generated events. Strict all-target Clippy
passes for fux and the standalone harness. The extracted history, reporting-app
mouse, transfer, delayed-manager and broad viewer process scenarios all pass,
including tiny-layout coverage through the composition run. The tiny scenario is
also directly selectable as `viewer-tiny-layout`. All five extracted function
bodies were compared with the pre-split snapshot and match modulo formatting;
existing assertions remain reachable. Builds completed before process tests ran.

See [controller-trace-testing.md](controller-trace-testing.md) for targeted and
replay commands. Logs and source/binary hashes are in
`docs/verification/codebase-improvement/{traces-*,split-*,viewer-split-*,controller-traces-source.json}`.
The pre-split source is retained at `/tmp/fux-codebase-work/before-viewer-split/`.

This is partial harness work: the generated model does not yet generate raw-byte
paste/prefix or active gesture sequences. Dedicated modal/gesture composition,
automatic scenario failure artifacts, the consolidated full-gate command and fresh
Betamax rendering remain outstanding.

## Generated byte ownership and gesture coverage

The same seeded/replayable controller corpus now includes bracketed-paste start,
content and end as separate events, allowing cancellation/buffer/target changes
between fragments. Paste ownership survives cancellation until its delimiter.
Synthetic content contains command prefixes, carriage returns and escape sequences.
Prefix events exercise UTF-8/SS3 bytes, literal command-looking paste and command
dispatch in one- or two-byte chunks. Selection press/motion/release events model
captured tails, lost-release recovery and invalidation by resize or target loss.
These checks use observable effects and specification-owned lifetime flags.

Dedicated `viewer-modals` and `viewer-gestures` scenarios are now available. Modal
checks cover one-Escape dismissal followed by exactly one normal input marker,
unchanged pane/tab identity and labels, and unchanged layout. The broad viewer run
includes that scenario. Gesture assertions were extracted from the history fixture
into a shared function, retaining their composition with independent histories.

New observations and corrections:

- The first modal run treated a cleared heading as complete dismissal, while the
  sampled terminal still contained menu entries/footer. Its wait now requires the
  entire panel to disappear; the no-command-popup and normal-input checks remain.
- Two gesture runs passed assertions but timed out reaping the viewer during
  cleanup. Terminal close now drains PTY output while reaping, matching natural
  exit waiting, within the unchanged three-second bound. The gesture run then
  passed. A new test verifies cleanup/repeated cleanup with output in flight.
  The logs demonstrate the observed failure and improvement; they do not prove
  every kernel-level cause of delayed teardown.
- History restoration waited only for an A056 row that survives local resizing.
  It could begin selection before restored server geometry arrived. The wait now
  also observes the bottom separator at the restored width before selecting.

Verification: 109 client tests (including the expanded 32,768-event corpus),
25 standalone harness library tests, strict all-target Clippy for both crates,
and formatting/whitespace checks pass. Targeted modal, gesture, history and broad
viewer process runs pass after the corrections. Builds did not overlap process
scenarios. Failed and passing logs are retained under
`docs/verification/codebase-improvement/{trace-input*,controls-*,modal-*}`, with
source/binary hashes in `controller-input-source.json`. Commands use the previously
recorded target directories and the focused names in `controller-trace-testing.md`.

No Betamax capture was enabled in this targeted pass. Structured diagnostics,
automatic whole-scenario failure artifacts, performance comparison, full integrated
verification and the final coverage/review audit remain outstanding.

## Bounded fux diagnostics and automatic failure artifacts

Fux now exposes opt-in `FUX_DIAGNOSTICS=1` tracing through its existing subscriber.
Interaction entry/end, effect-queue pressure and invalidated targets, and correlated
history reads/completions/stale replies/timeouts record bounded metadata. They omit
terminal, paste, input and field contents. Process IDs accompany the new events;
interaction entry records viewer, pane, stream and server incarnation. Detailed
records go to the private state-directory `diagnostics.log`, not terminal stderr.
The existing capped writer now holds a nonblocking exclusive lock and enforces a
strict one-MiB write limit, including oversized events. Contended records are
best-effort drops. No lifecycle decision depends on this file.

The standalone scenario dispatcher now retains bounded evidence across fixture
teardown and writes it only on failure/unwind: source and binary identities,
terminal checkpoint/text state, input lengths/hashes, resize/checkpoint events,
local RPC metadata and diagnostic-log tails. It enables diagnostics for standard
isolated fux roots and collects those logs before removing the roots. Files are
0600 inside 0700 directories. Source identity collection, terminal count/size/input
volume, event queues and log tails have explicit bounds and omission counters.
See [diagnostics-and-failure-artifacts.md](diagnostics-and-failure-artifacts.md)
for limits, scope and the deliberate post-teardown failure probe.

The artifact tests caught that tempfile directories were not necessarily 0700;
permissions are now explicitly set before writing. Tests verify private output,
omission/event bounds, input-payload exclusion and actual terminal evidence after
teardown. A real modal run with `FUX_HARNESS_FAIL_AFTER=viewer-modals` passes all
modal assertions and then deliberately fails. Its retained artifact includes
NORMAL-5 in the final screen, transition records without the NORMAL marker text,
and source/binary SHA-256 identities. The modal fixture uses subprocess CLIs, so
its in-process RPC list is correctly empty; RPC payload exclusion is tested at
the local-helper boundary separately.

28 standalone harness library tests, the expanded capped-log test, 109 client
tests and strict all-target Clippy pass. History and delayed-manager PTY scenarios
pass with the recording and diagnostics enabled. The deliberate failure is an
artifact-path validation, not an unexpected failing gate. Final integrated checks,
zor recovery diagnostics, performance and Betamax review remain outstanding.

The artifact-path probe is retained as
`docs/verification/codebase-improvement/failure-artifact-probe.json`; its expected
injected error, final NORMAL-5 marker, private permissions, diagnostic-content
omission and source/binary identities were checked directly. Logs are retained as
`artifacts-*` and `diagnostics-*`, with hashes in `diagnostics-source.json`.
A scoped review also moved resize recording after successful ioctl and labeled
input metadata as an attempt rather than a delivery receipt. All 28 harness tests
and strict all-target Clippy pass after those metadata changes. The retained real
probe predates only those metadata-labeling adjustments; the final integrated run
will rebuild the harness again.


## Zor transition diagnostics and recovery artifact verification

Implemented opt-in `ZOR_DIAGNOSTICS=1` JSON-lines observations after the journal
rename and directory-sync attempt. Typed events cover changed launch, attempt,
delivery and worktree metadata and launch reconciliation call outcomes. Recovery
observations use null `directory_synced`; `succeeded` reports the call result and
never implies task completion. Diagnostic failures do not replace transaction
results. Logging is private, symlink-resistant, nonblocking under contention and
bounded to one MiB with records no larger than four KiB. It excludes argv, paths,
prompt text and error contents. Standard harness roots now enable and collect
both fux and zor diagnostics before teardown.

A separate inspection of the collector found that opening a substituted FIFO could
block before checking its type. Nonblocking opens now permit immediate rejection;
a regression verifies rejection and exclusion of journal contents. Two zor tests
cover privacy, symlink refusal, lock contention, rollover, oversize rejection and
recovery/commit metadata distinctions.

Verification commands (external target directories preserve the source snapshot):

```sh
cargo +stable test --target-dir /tmp/fux-codebase-work/build --locked -p zor --lib
cargo +stable clippy --target-dir /tmp/fux-codebase-work/build --locked -p zor --all-targets -- -D warnings
cargo +stable test --manifest-path tools/xtask/Cargo.toml --target-dir /tmp/fux-codebase-work/harness --locked --lib
cargo +stable clippy --manifest-path tools/xtask/Cargo.toml --target-dir /tmp/fux-codebase-work/harness --locked --all-targets -- -D warnings
```

Zor: 163 passed, two existing ignored. Harness: 29 passed. Strict Clippy passed
for both. Fresh CLI-feature zor and harness binaries ran `zor-launch` with
`FUX_HARNESS_FAIL_AFTER=zor-launch`: the scenario passed, then the deliberate
post-teardown failure returned exit 1 and retained 179 zor records spanning
launch, attempt, delivery and recovery. The retained artifact's private mode,
record bounds, selected metadata fields and null recovery-sync field were checked.
`zor-recovery` also passed with diagnostics enabled. These runs are behavioral
checks, not timing or performance evidence. The final collector FIFO change was
verified by library tests and Clippy; the next integrated run must rebuild the
harness to include it.

Evidence: `docs/verification/codebase-improvement/zor-diagnostic-failure-probe.json`,
`zor-diagnostics-source.json`, `zor-diagnostics-*.log`,
`harness-zor-diagnostics*.log` and `zor-recovery-diagnostics.log`.
The full lifecycle/interaction audit, integrated verification, fresh Betamax
visual review, representative release performance comparison and consolidated
final report remain outstanding. This is a scoped review, not the final full-diff
completion review.
