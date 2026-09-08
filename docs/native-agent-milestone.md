# Native agent and reproducible verification milestone

Authority: [execution prompt](../headless-native-agent-milestone-prompt.md).
Status: **complete within the required non-R6 scope**, 2026-09-08.
The authoritative final results are in [native-final-verification.md](native-final-verification.md).
The previous non-R6 milestone remains accepted. The execution record below is chronological.

| ID | Current evidence | Acceptance evidence required |
|---|---|---|
| N1 verification | Implemented and independently reviewed: bounded process execution, durable atomic manifests, exact plan/configuration/input identity, validated continuation, and actual-runner regressions. Final combined invocation passed; see N5. | Actual Rust runner isolates stdin, bounds execution/owned cleanup, records exact commands/configuration/toolchain/input fingerprints and logs, distinguishes pass/failure/interruption/not-run, and refuses invalid reuse. Actual execution-path regression tests pass. |
| N2 native adapter | Implementation and targeted coverage complete; see [case-by-case coverage](native-adapter-coverage.md). Current native suite: 38 passed, 2 optional installed probes ignored. Protocol, transport, journal, managed controls, recreation, capabilities and freshness/attention are independently reviewed. Installed initialization/storage metadata passes; no live turn or materialized real-provider resume claimed. Complete diff review and final gate passed; see N5. | Inspect installed interfaces, select one provider with recorded evidence, implement native correlation and structured state/response evidence plus supported interruption/recreation. Test stale/duplicate/partial/lost events, uncertainty and cleanup. Label real-provider validation separately. |
| N3 performance | Complete measured rejection: [decision and full paired results](native-performance-decision.md). Both builds/source archives retained; focused fux probe, public-CLI journal capture and deterministic koh costs recorded. Production candidate reverted because results were mixed. Independent measurement review verified medians, hashes, common provenance and cleanup. | Preserve current baseline/candidate binaries and exact settings; inspect fux construction/capture/serialization, zor journal/observation/dashboard, koh deterministic copying/buffering. Measure one opportunity and accept or reject its optimization using complete comparable results and stated bounds. |
| N4 workflow | Complete: existing mandatory workflow now adds two native workers, stable correlation, lost-ack reconciliation, interruption/recreation, actual source/check/artifact verification and retained JSON handoff. Combined test and retained standalone run pass; independent review accepted final fixes. | Extend it through CLI/API to demonstrate the selected native adapter, stable correlation, blocker/interruption, safe reconciliation, continued/recreated work where supported, verified artifacts and retained handoff. |
| N5 completion | Complete: refreshed companion reconstruction and independent complete-diff/final-fix reviews pass. Fresh `.verification/gate-tbzpPi/manifest.json` has 45 passing commands, finalized checkpoint and `complete: true`; invocation exited 0. | Refresh changed companion patches/evidence, independently review complete intended diff and all final fixes, then obtain one successful fresh-manifest end-to-end mandatory non-R6 gate invocation on settled source. Record exact results and limitations. |

Execution order: N1, interface selection/N2, N3, N4, N5. Use targeted checks during
implementation and reviewed continuation; the final acceptance run is fresh. No
acceptance condition is satisfied by intent or a fixture that bypasses production logic.

Boundaries remain fux multiplexer/API, zor all agent policy, koh opaque transport/auth.
Required work is headless, account-free and unprivileged. Paid validation is optional
and bounded. No commits, pushes, PRs, new UI, plugin framework or broad terminal parity.
R6 live remote authorization, reconnect, retention-window and netmon acceptance remain
explicitly deferred; deterministic transport evidence is not remote runtime proof.

## Execution record

N1 execution increment: the real dependency-verification check loop now uses
`tools/xtask/src/gate_process.rs`. It isolates stdin, runs each check in an owned
process group, enforces a 1,800-second check deadline and a 32 MiB limit per output
stream, and preserves separate logs beneath ignored `.verification/gate-*` directories.
SIGINT/SIGTERM request bounded cancellation; cleanup retains the leader's identity
with WNOWAIT until group signaling completes. The macOS zombie-only group EPERM case
is accepted only after a bounded process-status query confirms no live group member.
No permission change or remote runtime operation is involved.

Four actual execution-path regressions pass: stdin contamination/failure propagation,
successful-command leftover group cleanup, timeout/interruption, and bounded output
failure. Strict all-target tooling Clippy passes. Independent review accepted only
this increment and did not rerun checks. Logs: `/tmp/fux-native-gate-process-tests.log`
and `/tmp/fux-native-gate-process-clippy.log`. The original failed experiments are
superseded by the final passing run; production fux/zor/koh behavior is unchanged.

N1 persistence increment: `gate_record.rs` now drives the real check loop. Each
`.verification/gate-*` directory retains reconstructed source, the original source
path inventory, an atomic `manifest.json`, and separate logs for every attempt.
The manifest records the exact indexed command plan, cwd, explicit non-secret
environment, installed tool versions/executable hashes, relevant configuration
hashes, source fingerprint, limits, exclusions, and check outcomes. Pending checks
remain `not-run`; active checks are durably `running` before spawn; recovery marks
uncheckpointed active records `interrupted` and refuses their reuse.

Continuation uses `dependencies verify --build --headless --resume=gate-NAME`.
It requires identical inputs, retained diagnostics for passing attempts, and a
content/mode/link checkpoint of the reconstructed tree and build target. Hashing
the build state occurs once per stopped invocation, with streaming file reads;
this conservative policy avoids a per-command dependency graph. Missing, changed,
failed or uncertain evidence never counts as passing. Failed commands get new
attempt logs. Input changes require a fresh run. A gate lock excludes concurrent
gate invocations; it does not freeze ordinary user edits, which are checked against
the copied path inventory and source contents before and after execution.

Independent review confirmed and then accepted fixes for reconstruction association
(including files deleted after copying) and checkpoint creation after unproven
process cleanup. Any execution infrastructure error leaves the checkpoint absent.
Process-group cleanup covers owned group members; deliberate child session escape
and force-killing the runner are not an OS containment guarantee. An uncheckpointed
run cannot be reused.

Verification: all 27 Rust tooling binary tests passed in
`/tmp/fux-native-record-final-tests.log`; strict all-target Clippy passed in
`/tmp/fux-native-record-final-clippy.log`. The final deletion-case fix passed its
affected regression and strict Clippy in `/tmp/fux-native-record-deletion-test.log`
and `/tmp/fux-native-record-deletion-clippy.log`. Independent final review accepted
the full N1 increment with no remaining confirmed findings. The fresh end-to-end
gate is intentionally reserved for settled N5 source; it has not been started.

N2 interface inspection has begun. Current installed versions are Codex 0.153.4
and Claude Code 2.1.263. Codex app-server generated schemas and both CLI help captures
are retained under `.verification/native-provider-inspection-20260907/`.
The [pre-implementation selection](native-provider-selection.md) chooses Codex
app-server over stdio based on its explicit client message, native item/thread/turn,
interrupt and resume/read contracts. Schema excerpts and binary/schema hashes are
retained. This is installed-interface evidence only: the new adapter is not yet
implemented, no paid call has been made, and no new performance result is claimed.
N2–N5 remain open.

### N2 protocol and transport increment

`zor/src/tasks/codex/` now contains the provider-specific native turn reducer,
JSON-lines framing and owned app-server stdio client. Submission changes a record
to submitted before yielding its request for durable publication by the future
task owner. That record never grants another submission after caller loss, a lost
acknowledgement, a timeout or an uncertain result. Native correlation requires
matching client operation, thread, turn, user-item identity and literal content.
A turn/start acknowledgement without its correlated user item does not count as
a native response. Bounded history can recover matching evidence; missing or
partial history cannot authorize replay.

The reducer retains structured working/input-required/terminal evidence and bounded
response items. Interrupt intent has a distinct stable ID; an interrupt RPC ack
does not become interrupted state until native terminal evidence confirms it.
These are native turn outcomes, never verified task outcomes. Fux PTY delivery
receipts remain a different evidence type.

The stdio client uses an owned process group and nonblocking pipes, literal JSON,
original deadlines, at most 1 MiB buffered framing, 128 queued frames/2 MiB encoded
queued data, and 256 KiB captured diagnostics. Write/observation/handshake errors
invalidate further sends. Per-turn evidence accepts up to 256 history turns/items,
8 response items with 4096 bytes of retained UTF-8 text each plus full text hashes,
and an 8192-byte blocker. Literal input is bounded to 65,536 bytes. Queue, history,
item-count and blocker pressure fails explicitly. Long response text is clipped at
a UTF-8 boundary with a truncation flag and a hash of the entire observed text.

The shared process owner now honors an explicit cleanup deadline during Drop as
well. If the owned child cannot be reaped by that deadline, the operation reports
failure and hands only reaping to a fallback thread; that thread never signals a
possibly reused PID. This is not proof of completed cleanup after a timeout, nor
containment of descendants that deliberately leave the owned process group.

Independent review found and accepted fixes for premature freezing of partial
history text, nonblocking questions hiding unresolved blockers, handshake failures
leaving the client usable, and unbounded Drop waits defeating shutdown deadlines.
The final reducer/framing and transport/process reviews found no remaining confirmed
issues within this increment.

Final targeted verification: 18 native tests passed; 2 affected shared process-owner
tests passed; strict all-target Clippy, formatting and the no-default-features
all-target check passed. The optional installed Codex 0.153.4 initialization
handshake passed separately using the production client, with no model request.
Durable copies of these logs are in `.verification/native-codex-core-20260907/`:

- `fux-native-codex-transport-fixed-tests.log`
- `fux-native-codex-transport-fixed-clippy.log`
- `fux-native-codex-process-owner-tests.log`
- `fux-native-codex-installed-handshake-fixed.log`
- `fux-native-codex-no-features.log`

This proves only the implemented protocol/transport slice. The reducer is not yet
wired into durable journal operations or exposed through task CLI/API commands;
the owner must persist intent before sending, including interrupt intent. Managed
worker discovery, session recreation, capability/attention integration, full
production adapter fixtures and the extended unattended workflow remain required.
No paid call, generated native response, real interruption or real resumed session
is claimed. Companion patches will be refreshed after the implementation settles;
the full final gate has not been run.

### N2 durable journal increment

The existing zor journal now retains native worker lifetimes and native operation
entries separately from PTY prompt receipts. `codex/state.rs` persists submission
and interrupt intent atomically before returning their transport requests. Reopening
a submitted or uncertain operation never grants another submission. Identical
preparation preserves its original deadline; changed intent is rejected.

Original submission producer and current evidence observer are distinct. A replacement
producer can reconcile the same native input by identity without replaying it.
Retirement rejects delayed old-producer events and retains up to 16 old producer IDs,
including lifetimes that emitted no turns. Registration retries normalize this
server-maintained history and do not rewrite identical state. Worker registration
still requires the future managed owner to verify live fux identity and the native
handshake before calling it; journal consistency alone is not liveness proof.

Native pending output reserves a 256 KiB growth allowance per entry. Already-retained
growth consumes this reservation, so it funds intermediate binding, interrupt and
response publication. Ready workers reserve 128 bytes for retirement metadata.
The actual Store capacity regression fills the four-MiB journal to within 128 bytes
of its limit and successfully publishes binding, interrupt intent and completion.
At most 128 native entries and the existing task limit apply. One unresolved native
operation per task is admitted. New work may explicitly fail admission at capacity.

Malformed native evidence persists uncertainty before returning an error. Exact
duplicate evidence does not rewrite the journal. Persisted shape validation rejects
completion without correlated input and malformed or mismatched blockers. Task
coordination cancellation blocks new input but leaves explicit native interruption
distinct; a native terminal response leaves the task outcome open. Inspection labels
retired observations historical and reports availability as not probed.

Independent review found and accepted fixes for forgotten retired identities,
non-consumable output reservations, replacement-registration retry idempotency, and
incomplete persisted-blocker validation. No confirmed findings remain in this
increment. All 27 native protocol/transport/journal tests pass (the optional installed
handshake remains separately attributed), and affected formatting and strict all-target
Clippy pass. Final logs are retained in `.verification/native-codex-journal-20260907/`:
`fux-native-codex-journal-settled-tests.log` and
`fux-native-codex-journal-settled-clippy.log`.

Still required: the managed worker must join these journal boundaries to the actual
stdio transport, discover/register under verified fux ownership, expose task CLI/API
operations, recreate native sessions safely, update capability/attention reporting,
and pass the full provider fixture/workflow coverage. N2 is not complete. N3, N4 and
N5 remain open; no performance or full final-gate result is claimed.

### N2 thread establishment increment

The production stdio client now offers bounded nonblocking polling. Idle pipes
preserve connection authority; complete queued frames are delivered before EOF,
which invalidates further use. The blocking observation path shares this logic.
An actual pipe regression covers idle polling, subsequent sending, ordered final
frames and rejection after EOF.

`codex/session.rs` performs initialization and one thread start/resume request under
the original deadline. It uses a canonical working directory, requests no interactive
approvals, validates the native response identity, directory and persistent-thread
flag, and checks the retained thread ID for resume. Early server events/requests are
retained for the owner, including requests sharing a client-response ID; establishment
does not acknowledge them. Retention is bounded to 128 events and two MiB. Failure
drops the owned client without retrying thread creation.

Actual pipe fixtures verify both request shapes and reject changed thread identity,
directory, persistence and response identity. The combined native suite passed 30
tests with the optional installed probe ignored. After adding request-capture
assertions, the two affected session tests passed again; their terminal output is
retained in `.verification/native-codex-session-20260907/session-tests.log`.
`cargo clippy --manifest-path zor/Cargo.toml --all-targets -- -D warnings`,
`cargo fmt --manifest-path zor/Cargo.toml -- --check`, and both repository diff
whitespace checks passed. Independent reviewer Hubble found no confirmed defects in
the session/poll increment. No model call was made.

This establishes protocol behavior only. Managed fux ownership and retained storage
namespace checks must precede registration in the forthcoming worker. Public
CLI/API discovery, execution, recreation, capability/attention integration and the
full unattended workflow remain incomplete; the final gate remains unrun.

### N2 first managed worker increment

`task codex-start` now launches the internal headless `codex-worker` through the
existing managed fux launch API. The wrapper verifies its task/attempt, launch marker,
managed session, own PID, and live fux pane identity before and after the native
handshake. All native/provider logic remains in zor. `task codex-submit` queues the
stable operation in the journal; only the owning worker takes the durable submission
authority and writes native JSON. `task codex-inspect` retains the explicit
not-probed availability and separate task-success result. Equivalent service request
routes are implemented but their runtime coverage remains pending.

The wrapper polls native events and journal work with finite per-iteration work.
SIGINT/TERM/HUP/QUIT cancel blocked native reads/writes. Child termination precedes
bounded journal retirement, so journal contention cannot postpone the initial child
stop. Worker recreation is explicitly refused at this stage rather than silently
creating a different native thread.

Independent review found three confirmed production races: prepared entries received
observations before submission, normal Store contention terminated the worker, and
poll batching lost completion frames followed by EOF. Fixes skip prepared evidence,
retain the current event across Busy retries, and publish every frame before the next
poll. A further fixture pause race was fixed by holding the journal lock while
establishing SIGSTOP, confirming the stopped state, then releasing the lock before
CLI submission. The SIGCONT cleanup guard remains active on errors. Final independent
review accepted these fixes with no remaining confirmed findings in this increment.

The Rust `zor-codex-fixture` and `zor-native` scenario drive the production wrapper
in a real fux pane, with a private environment and no account. They verify exact
multiline/control/literal input, stable retries without another native submission,
correlated response evidence, task outcome remaining open, 200 ms journal contention,
a notification queued while input is prepared, and child disappearance. Both a
persistent provider and immediate completion/EOF pass. The scenario is registered in
the existing mandatory `zor_integration` test suite.

Final affected integration command:
`ZOR_BIN="$PWD/zor/target/debug/zor" FUX_REQUIRE_ZOR_BIN=1 cargo test --test zor_integration zor_native -- --nocapture`
passed one test exercising both variants; terminal output is retained in
`.verification/native-codex-worker-20260907/integration.log`.
The native library suite passed 31 tests with one optional installed probe ignored;
its new cancellation test proves a long native wait stops promptly and rejects input.
Strict all-target Clippy passed for zor and both tool workspaces; root affected
`cargo clippy --test zor_integration -- -D warnings` passed. Formatting checks for all
affected workspaces and both diff whitespace checks passed.

Early experiments exposed asynchronous stop-confirmation retries and a compile error
in the initial loop refactor. One shell sequence ran an older worker after that build
failure; that scenario output is not final evidence. All final integration binaries
were rebuilt successfully before the retained passing test. No paid model call or
R6 runtime check was performed. Native interruption/reconciliation/recreation,
storage-namespace validation, capability/freshness/attention integration, service
runtime coverage, N3 measurements, N4 extended workflow and N5 final review/gate remain
required. Companion patches still await the settled implementation.

### N2 durable native controls increment

CLI `codex-reconcile` and `codex-interrupt`, and equivalent service actions, now
queue explicit native controls in the existing operation record. Each retains its
request ID, kind, producer, creation time, original timeout and queued/sent/observed/
expired/obsolete state. At most 16 controls are admitted per native input. Repeated
intent returns the retained record without refreshing its deadline or granting a
second send. Different intent under the same request ID is rejected. An interrupt
is still scoped to a correlated native turn; its acknowledgement is separate from
an observed native interrupted terminal state.

History reads carry the input and control IDs in their response routing identity.
The worker rejects unsolicited bare read IDs; sent, current-producer controls alone
may normalize a response into the pure native reducer. Unissued, duplicate, expired
and old-producer control responses cannot refresh evidence. Control expiry prevents
a late send, and retirement marks pending controls obsolete. Reconciliation never
returns submission authority.

Independent review found that retained control growth was not consuming the journal
reservation. Clearing controls from the reconstructed initial record fixes that
accounting. The actual near-four-MiB regression now admits and expires a read,
retires a producer with pending control intent, then registers a replacement and
reconciles completion. Ten affected journal regressions passed; the subsequently
extended capacity test passed separately. Unchanged protocol/transport evidence is
retained.

The real-fux scenario now has a third service-backed variant. Its Rust provider
fixture deliberately loses the start acknowledgement/user event and emits an
unrequested bare read response. Evidence stays submitted until an explicit service
history query binds the original input. Duplicate read/interrupt requests produce
one native effect each, enforced by create-new fixture receipts. Task cancellation
does not stop the worker or prevent explicit native interruption. An interrupted
snapshot's partial text remains outside completed response evidence; normal and
EOF completion variants still assert captured completed responses. Service start,
submit, inspect, reconciliation and interruption routes run against production code.

All three scenario variants pass. An early interrupted-response fixture assertion
was corrected to match the existing partial-output semantics; no reducer weakening
was required. Final independent review accepted the production and fixture fixes
with no remaining confirmed findings in this increment. Final scenario/capacity logs
are retained in `.verification/native-codex-controls-20260908/`. Formatting and diff
whitespace checks pass; strict all-target Clippy passes for zor and both affected
tool workspaces.

Still required: safe native session recreation and storage-namespace proof,
capability/freshness/attention integration, remaining full-adapter failure coverage,
N3 measurements, N4 extended unattended workflow, companion refresh and N5 final
review/gate. No live model response/interruption, paid call or deferred R6 runtime
acceptance is claimed.

### N2 storage identity prerequisite

The managed worker now retains the provider-reported rollout path and the identity
of its nearest existing owned storage ancestor. Runtime verification checks that
ancestor's device/inode and rejects redirected or foreign-owned materialized
descendants. Provider file replacement in the same namespace is permitted without
reading its contents. Producer replacement must retain the same native thread and
storage identity. No credentials or rollout contents are read.

The installed no-turn metadata probe initially verified a persistent-thread flag
and local rollout allocation. Further inspection exposed two real interface limits:
the dated rollout directory can be absent before the first turn, and a second
app-server rejects resuming an empty thread with `no rollout found for thread id`.
The storage implementation therefore pins an existing ancestor while allowing
provider-created descendants, and supplies a separate `require_materialized()`
preflight for the forthcoming restart operation. An allocation is not claimed as
evidence of a resumable persisted session. Bounded native rejection messages now
preserve useful diagnostics.

The exploratory empty-thread resume failed and is retained as limitation evidence
in `.verification/native-codex-storage-20260908/empty-resume-experiment.log`; it is
not relabeled as recreation success. The optional retained probe covers metadata
only. No model turn or paid call was sent. Recreation of a materialized live-provider
thread remains unverified.

The native suite passed 35 tests with two optional probes ignored before the final
ancestor adjustment. The two affected storage tests and two session tests passed
on the final adjustment, and all three real-fux scenarios passed with an explicit
assertion that the namespace is retained in the worker journal. Storage test output
is retained beside the experiment log. Strict all-target Clippy passed for zor and
the affected tool workspaces, as did formatting and diff whitespace checks.
Independent review accepted the final storage changes with no confirmed findings.

This completes the namespace prerequisite, not N2 or the overall goal. Durable
restart orchestration, its CLI/API operation and resume fixtures remain required,
along with capability/freshness/attention integration, remaining adapter failure
coverage, N3 measurements, N4 workflow and N5 final review/gate. R6 remains deferred.

### N2 recreation state regression increment

The worktree now contains durable recreation control and managed-owner orchestration;
end-to-end recreation acceptance remains pending. Two new production-state regressions
prove that cancellation of coordination permits explicit recreation but blocks new
input, retained input cannot be replayed, retry identities and deadlines remain stable,
expired controls do not start recreation, and failed publication retires a partially
registered replacement without granting another restart. The suspected cancellation
admission mismatch is not present in current `register`: replacement of an existing
native worker is permitted after cancellation, subject to the retained ownership,
thread and storage checks. The new regression exercises this path successfully.

Verification: `cargo test --manifest-path zor/Cargo.toml --lib tasks::codex::state::tests -- --nocapture`
passed all 11 then-present state tests, including journal capacity. After adding the
expiry/failure regression, the targeted `tasks::codex::state::tests::recreation` filter
passed both recreation tests. Formatting was applied. These are journal/state tests,
not proof of process restart, native resume, real-provider integration or completion
of N2. Persisted provider fixtures and real-fux recreation coverage are next; N3–N5
and the deferred R6 record remain unchanged.

### N2 process recreation fixture increment

The account-free Rust provider fixture now persists its interrupted turn and loads
it in a second app-server process through `thread/resume`. Resume checks that the
previous provider PID is absent before replying. Create-new submission, recreation
and read receipts reject repeated effects. The real-fux service scenario recreates
after coordination cancellation and native interruption, checks a distinct producer
with unchanged thread/storage/attempt/marker and multiplexer session, then reads
retained native history through the replacement. Original literal input remains
unchanged; native interruption still does not imply verified task completion.

`ZOR_BIN="$PWD/zor/target/debug/zor" FUX_REQUIRE_ZOR_BIN=1 cargo test --test zor_integration zor_native -- --nocapture`
passed all three scenario variants through the registered integration test (15.57 s).
The zor binary and both fixture/harness binaries were rebuilt before execution.
Strict all-target Clippy passes for both affected tooling workspaces. The prior
state increment's final strict zor Clippy and formatting checks also passed after
correcting test-only indexing lint violations. Independent recreation review found
one confirmed publication-deadline gap: registration and journal reload could consume
the budget after its first check. Final publication now checks the persisted original
deadline, the action deadline and backwards clock movement. An extended state
regression exercises this final check. Both recreation state tests, strict all-target
zor Clippy, and the rebuilt real-fux integration test pass after the fix (11.33 s for
all three variants). Final independent review accepted the fix and found no further
confirmed recreation defects. Both repository diff whitespace checks pass.

This proves fixture-backed recreation within the same live owned wrapper. It does
not prove restarting a lost wrapper, materialized real-provider resume, or paid model
execution. Capability/freshness/attention integration, remaining adapter failure
coverage, N3 measurements, N4 workflow and N5 complete review/final gate remain open.

### N2 capability contract increment

`task adapter-capabilities --agent codex` and its service route now describe the
implemented managed stdio adapter: retained native discovery, literal input correlation,
structured state/blockers/responses, distinct interruption and conditional recreation.
Descriptions explicitly retain unprobed availability, unsupported lost-wrapper restart,
materialized-storage requirements and the distinction between fixture validation and
unverified real-provider turns/resume. Claude remains passive-only. Zor's README now
documents the native CLI controls and states that answering native blockers is unsupported.

Three capability/service-dispatch tests passed. The existing real-fux two-worker
workflow passed with its obsolete Codex passive-only assertion updated (12.64 s);
this regression does not satisfy N4's required native workflow extension. Strict
all-target Clippy passed for zor and root tooling. Independent read-only review
accepted the capability descriptions and workflow assertion without confirmed findings.
Freshness/attention integration and remaining native failure coverage are next;
N3–N5 remain open and R6 remains explicitly deferred.

### N2 retained evidence freshness increment

Native entries now retain `observed_ms` within the existing journal reservation.
Changed correlated evidence and accepted explicit history reads update it; duplicate
events, unrelated frames and interrupt acknowledgements do not. Read-only inspection
reports `evidence_age_ms` and derives historical/uncertain evidence after five seconds,
owner invalidation, missing observation time or backwards clock movement. This does
not probe availability, rewrite the journal or grant input replay. Unobserved prepared
state remains prepared; completed native outcomes remain historical terminal evidence.

All 13 native journal/state regressions passed, including the actual near-capacity
test and a new deterministic inspection-age test proving expiry, backwards-clock
invalidation, duplicate suppression and unchanged journal state. An initial run caught
prepared-state invalidation; that was corrected before the passing suite. Independent
read-only review accepted the increment without confirmed findings. Strict all-target
zor Clippy and formatting pass after an equivalent boolean-expression lint cleanup.
The rebuilt real-fux native integration passed all three variants (11.34 s), and zor's
diff whitespace check passes. README describes age separately from availability.

Dashboard attention integration still remains to consume this freshness boundary;
remaining full-adapter failure coverage, N3, N4 and N5 are not complete. No paid call,
live-provider turn/resume validation or R6 runtime acceptance was performed.

### N2 existing attention view integration

The service overview now exports at most one compact native summary per registered
current-attempt worker. It selects the unresolved input when present, otherwise the
latest retained input, and uses the reviewed read-only freshness boundary. Summaries
contain operation/producer identity, phase and age, without copying input text,
responses or blocker payloads into every dashboard fetch.

The existing attention view joins this summary to the exact pane identity. Current
native working/blocker evidence becomes working/blocked; invalidated pane observation,
expired evidence, retired authority or ambiguous ownership yields unknown. Native
completion/interruption remains separate from task outcome. Observation rows retain
the existing attention and stale-notification behavior; no new UI or fux API was added.

Eight dashboard tests passed, including the new blocker/expiry/retirement and
event-gap/EOF/resync regression. The rebuilt real-fux native integration passed with
an assertion that the service overview contains the resumed producer and interrupted
turn (14.46 s), and the existing dashboard integration passed (18.29 s). Strict
all-target Clippy and formatting passed for zor and root tooling; both diff whitespace
checks passed. Independent review accepted the increment with no confirmed findings.

N2's remaining adapter-failure checklist must now be reconciled with the concrete
tests; N3 measurements, N4 extended workflow and N5 full review/final gate remain open.
No real model turn, materialized live-provider resume or R6 runtime proof is claimed.

### N2 coverage closure and N3 baseline

The [native adapter coverage map](native-adapter-coverage.md) now connects every
explicit N2 failure category to production tests and distinguishes reducer/journal
fault injection from real-process coverage. Current full native suite passed 38 tests
with two optional installed probes ignored (8.09 s). N2 implementation and targeted
verification are covered; complete diff review/final gate remain N5, and the extended
native two-worker workflow remains N4. No new acceptance conditions were added.

N3 now has a current reproducible baseline at
`.verification/native-performance-20260908/baseline/`: copied fux/zor executables,
runtime/tooling source archive, toolchain identity, explicit dev flags and SHA-256
records. Both builds succeeded before copying. The Rust headless-performance capture
completed all four pane/viewer/slow-consumer configurations at three repetitions,
retaining raw counters, geometry, process identity and cleanup in `fux-viewers.json`.
Its offline validator passed. No competing performance workload ran during capture.

This is baseline evidence only. Candidate binaries/results, focused cost measurement,
zor journal/observation/dashboard and deterministic koh investigation, optimization
accept/reject decision and measurement limitations remain N3 work. Existing source
inspection identifies repeated pane validation/per-viewer serialization and journal
decode/validate/reserialization as candidates, not yet measured dominant costs.

### N3 measured candidate and rejection

The [performance decision](native-performance-decision.md) records the full paired
matrix, focused probe, current journal measurement and deterministic koh costs.
Baseline/candidate binaries and source archives remain separately retained. The
ASCII validation candidate improved isolated validation and burst CPU, but the
slow-consumer sustained case regressed; it was rejected and production validation
restored. The temporary manual cost probe was subsequently archived and removed
from active source to satisfy the mandatory no-ignored-tests invariant. No further benchmark
search was undertaken to obtain favorable results.

All 12 candidate viewer cases and the offline validator passed. Candidate ASCII and
existing view correctness tests passed; after restoration, four view tests passed
with the manual probe ignored. Strict root library/test Clippy and formatting pass.
The journal capture completed three repetitions with 96 samples per operation and
zero replacements for inspection/replay. Koh's deterministic production framing
test passed with retained byte/call counts; this is not R6 runtime evidence.
Independent semantic review accepted the candidate/probe. Final measurement-report
review recomputed medians, verified retained hashes, matching harness/sampler/zor
provenance and cleanup, and accepted the rejection with no confirmed findings.
N3 is complete within its specified measured-accept-or-reject scope. N4 extended
workflow and N5 final review/gate remain open.

### N4 native two-worker workflow

The existing mandatory `zor_workflow` now includes a second phase with two Codex
native workers in distinct managed worktrees. Both launch before work submission.
Each retains literal native input, reconciles deliberately lost acknowledgement,
interrupts its turn, recreates the same native thread/storage under a distinct
producer, and reads retained history. Stable submission/recreation retries cannot
repeat effects, enforced by create-new fixture receipts. Native outcomes remain
unverified until ordinary source collection, required checks and artifact capture
pass. The final handoff retains native operation/producer/control evidence, verified
task records, artifact bytes and explicit live-provider/R6 limitations.

An initial test correctly failed because fixture-generated output was untracked and
absent from the source snapshot. The harness now commits that file only within the
disposable managed worktree before collection, matching the first workflow phase.
The combined mandatory workflow test passed after the fix (20.28 s). A standalone
successful run retained exact stdout and the extracted JSON handoff under
`.verification/native-workflow-20260908/`. Temporary worktree paths in those records
are historical references; artifact bytes and hashes are retained in the handoff.
The handoff is emitted only after owned cleanup passes.

Strict all-target tooling Clippy and affected formatting/whitespace checks pass.
Independent review accepted the final fixture/source-sealing fix and handoff semantics
with no confirmed findings. No production adapter features were added for this workflow.
N4 is complete; N5 companion refresh, complete diff review and the fresh final mandatory
non-R6 gate remain open. No paid calls or R6 runtime acceptance were performed.

### N5 completion

All required non-R6 work is now complete. Both companion patches reconstruct exactly;
three independent reviewers accepted the complete intended diff and final fixes.
Two failed fresh gates and their confirmed fixes are retained in
[the final verification record](native-final-verification.md), without relabeling
failed or historical evidence. The temporary N3 probe is archived and removed from
active source; structural acceptance remains unchanged.

The final fresh invocation `.verification/gate-tbzpPi` passed all 45 mandatory
commands, finalized its checkpoint and exited 0. Independent evidence review verified
the exact plan, exclusions, single-attempt outcomes and all diagnostic-log hashes.
Only completion documentation changed afterward. N1–N5 meet the original bounded
acceptance conditions; no non-R6 blocker remains. Live model turns/materialized
real-provider resume remain unverified, and R6 remains explicitly deferred.
