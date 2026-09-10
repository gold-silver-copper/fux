# Native capabilities on main

Status: implementation in progress. No integrated acceptance is claimed.

Integration checkout: `/Users/kisaczka/Desktop/code/fux-integrated`, branch
`integrate/native-on-main`. Base main is
`a48f839501af2bb317f559d96255013bd3f3eb33`; milestone reference is
`01e52cc2f562e23157b0f6aa8585a2c64f569169`.
The milestone remains unchanged in the original checkout. No commits or external
mutations are authorized. Companion checkouts are independent clones.

## Capability inventory and acceptance

| Capability | Disposition | Required evidence | State |
|---|---|---|---|
| Typed ECS systems, tab relationships, shared helpers | Keep main | ECS/structure regressions, full review | Baseline passes |
| Viewer creation/retirement ordering | Keep main; integrate final records | Same-step arrival/departure, late spawn/exit tests | Baseline passes |
| Retained grids, delta frames, pacing, bounded allocation | Keep main | Delta reconstruction, hostile frames, slow-viewer tests and paired measurements | Main/integrated default and focused comparisons complete; native/headless comparisons underway |
| Generic info, wait, env/size, keys, row capture | Keep main | Existing CLI/ECS scenarios adapted to identity contracts | Baseline passes |
| Agent report parsing in fux | Remove; keep interpretation in zor | AST boundary and ignored-OSC behavior, zor integration | Implemented; independent review accepted; boundary tests pass |
| Terminal revision and coherent capture | Port milestone without replacing grid sequence | Resize/history/metadata/cache tests | Wire and terminal implementation pass targeted tests; independent review accepted |
| Server/workspace incarnation | Reuse main server identity; port workspace lifetime | Reused IDs, stale mutations, restart regressions | Server instance and workspace event-stream lifetimes tested/reviewed; stale split precondition covered |
| Input reservations, receipts, partial writes, bounds | Adapt milestone with interruptible PTY I/O | Lost reply, duplicate, writer intervention, stalled PTY tests | Initial port reviewed and tested; Linux payload-write hang fixed; Linux/macOS scenario and partial-count regression pass; final review accepted |
| Event stream/replay/gaps | Adapt log as an ECS component; retain typed publication | Replay eviction, reconnect, delayed events | Implemented; Linux/macOS real scenario and deterministic tests pass; independent review accepted |
| Final records and fux run | Port bounded records; replace row sampling with retained evidence | Immediate exit, final bytes, nonzero status, timeout and cleanup | Implemented; Linux/macOS scenarios and targeted suites pass; independent review accepted |
| Empty child arguments | Adapt native validation fix | Configured and split launches, rejected empty executable/NUL, exact final argv | macOS/Linux real scenario passes; independent review accepted |
| Split UTF-8 parsing | Port fix into reusable-buffer parser | Byte-split parsing regressions | Implemented; reproduced before-fix failure; tests/review pass |
| Native provider/task/check/artifact policy | Reuse standalone zor; adapt main wire consumers | Native worker and two-worker artifact handoff | macOS/Linux scripted/native workflows and launch recovery pass; companion patch reconstruction and full gate pending |
| Koh transport fixtures | Adapt pinned companion | Exact reconstruction and deterministic gateway tests | Unversioned hello adapted; reconstruction and macOS real gateway/non-R6 library checks pass; independent review accepted |
| Rust xtask, durable gate and process harnesses | Port; preserve main regression coverage | Tooling tests, complete mandatory plan, fresh final invocation | Active Python migrated to Rust with archived originals; tooling and expanded scenarios pass; fresh complete gate pending |
| Historical evidence | Preserve exact source/binary attribution | Provenance validation, newly measured integrated results | Archived provenance checks pass; two historical source versions unavailable and documented; current paired measurements retained |

## Baseline evidence

Main's hosted run 34228545403 succeeded at the exact base. The local baseline
formatting, strict all-target Clippy, Cargo tests and debug binary build passed.
Logs, source archive, retained main executable and SHA-256 inventory are under
`.verification/integration-baseline/`. Optional companion tests in the root Cargo
suite do not establish actual cross-repository integration without explicit binaries.

Native hosted run 34258670819 completed: macOS, Android compilation and packaging
passed; both Linux test jobs failed the input-receipts scenario at owned-child wait;
the companion job failed because checkout refs differ from the manifest. These
are inherited diagnostic evidence, not failures introduced by this port.

## Remaining completion requirements

Complete native/headless performance comparisons, investigate the observed interactive
overhead, finish the independent full root/companion review and fix confirmed findings,
then run one fresh complete mandatory headless gate on settled source. Revalidate companion
reconstruction and required checks in that final gate. Targeted macOS/Linux execution,
Rust migration and current documentation have evidence below; they do not replace the final
complete invocation. Hosted execution of unpublished changes remains a later step.
R6 and live paid provider calls remain outside the authorized scope.

## Completed increments

Ownership: removed AgentReport/AgentState, OSC interpretation and agent listing/events
from fux. Main's generic commands, grid/delta model and progress/title handling remain.
The milestone AST boundary test was ported; independent reviewer accepted the generated
inventory after reading declarations and code together. The old positive agent-event
fixture/test were retired because their asserted behavior is deliberately removed.
Public documentation cleanup remains pending.

Terminal: ported coherent CaptureSnapshot and terminal revision without substituting
it for main's grid sequence. Ported split-UTF8 completion handling into the reusable
scratch-buffer parser. Before the fix, the expanded regression dropped the space after
Ü at split 31; afterward it passes. Added separate-counter and history-only invalidation
regressions. Independent review found no blocking defect; capture metadata allocation
and the added linear UTF8 scan must be included in the planned measurements.
The existing capture viewport comment was corrected after review.

Latest checks: `cargo test --locked --lib --test agent_boundary --test ecs --test fixtures
--test structure`, strict all-target Clippy, formatting and diff whitespace all pass.
Logs: `.verification/integration-baseline/capture-regressions.log`,
`capture-clippy.log`, `utf8-before.log`; boundary inventory was regenerated only after
independent semantic acceptance of the generic terminal additions.

Identity and wire capture: all requests accept an optional server instance checked against
main's existing ServerIdentity before dispatch. Conditional text capture requires an
instance and returns coherent flattened metadata alongside main's grid sequence.
Listing exposes instance and pane revision. Strict malformed metadata, stale IDs,
metadata-only invalidation and real socket subscription tests pass. Subscribe rejects
stale identities before registration; the CLI exits on rejection. Independent review
accepted the increment. Logs: `contract-tests.log`, `contract-clippy.log`,
`contract-local.log` (including all six main local CLI scenarios).

Tracked input: adapted reservation/submission/status, sequence-based intervening-writer
checks, byte-budget ownership, actual partial-write accounting and completion draining.
The input systems remain typed ECS. Main's 64 KiB PTY reads, environment and shared
lifecycle helpers remain. Initial independent review accepted these semantics; root
lib/ECS/fixtures, boundary/structure/local CLI tests and strict Clippy passed
(`input-tests.log`, `input-clippy.log`, `input-local.log`).

Tooling: copied Rust xtask and adapted the control preface/hello to main's contract.
The owned-process harness preserves stderr on wait timeout. Tooling build/Clippy and
the real macOS input-receipts scenario passed. This is an initial tooling port: full
scenario migration, historical provenance and the mandatory gate remain pending.

Linux diagnosis: started a dedicated `fux-integration` Colima VM without changing the
user's default Docker context or existing VMs. Linux ARM64 reproduced the input
scenario timeout after all receipt assertions. Thread samples showed the PTY writer
stalled after the child and reader exited. Syscall trace
`linux-diagnostics/input.145` establishes an unfinished 65536-byte payload write,
not an implicit EOF destructor write. Killing the child did not release this syscall.
Logs: `linux-input-reproduce-full.log`, `linux-input-full-thread-samples.log`,
`linux-input-strace-run.log`.

Fix: safely duplicate the master descriptor into owned Files, use shared nonblocking
mode with explicit WouldBlock handling in both pumps, and cancel input on join/drop.
Accepted bytes remain counted exactly; owned File destruction adds no implicit EOF
write. Readers wait indefinitely for readiness; blocked writers poll at 25 ms to
observe cancellation. A real full-pipe regression verifies bounded cancellation and
compares the reported partial count with the exact received bytes. Independent review
identified and prompted correction of unnecessary idle-reader polling. Final Linux ARM64 and macOS input-receipts scenarios pass
(`linux-input-cancel-final.log`, `macos-input-cancel.log`). All six PTY unit tests
pass on both platforms, including exact partial-count cancellation
(`linux-pty-tests.log`, `input-cancel-tests.log`). The 96 library, 30 ECS and one
wire-fixture tests pass (`input-cancel-regressions.log`), as do strict Clippy and
reviewed boundary regeneration. Final independent review accepted the runtime and
boundary declarations. Its test-scheduling finding was fixed by waiting for actual
pipe readability before cancellation; the reviewer accepted the correction.
All six existing local CLI scenarios, eight structure checks and the ownership boundary
also pass (`input-cancel-local.log`). This is local Linux evidence, not a hosted CI claim.

Event replay: the bounded EventLog is a component on each workspace entity, not a
parallel registry. A checked server-lifetime stream allocator distinguishes recreated
workspace names. Both typed Effects and exclusive-world helpers sequence events before
publication. Listing returns the current event cursor; instance-scoped Events rejects
evicted, future, foreign or exhausted cursors with Gap. Split accepts an optional
stream precondition (requiring a server instance) and rejects an old lifetime before
reserving/spawning a process. Existing unscoped commands remain explicitly unscoped.

Subscriptions register their bounded live queue before requesting authoritative replay;
the replay boundary suppresses queued duplicates. Replay and live filtering agree.
Queues have count/byte budgets and disconnect on overflow instead of silently dropping
notifications. The CLI's existing fux run event reader now parses the sequenced envelope.
Workspace/tab dirty helpers publish generic WorkspaceChanged notifications while main's
typed systems and delta rendering remain intact.

Event verification: 103 library tests, 32 ECS tests and the protocol fixture test pass;
Linux ARM64 repeats that suite successfully. The real event-sync Rust scenario passes
on macOS and Linux, covering snapshot/replay/live flow, filtering, race deduplication,
eviction gaps, resynchronization and stale launch rejection. It uses main's split
request for that launch assertion instead of importing the milestone's new command.
Deterministic socket coverage forces queued/replay overlap. ECS coverage proves
same-batch attachment/departure/title ordering, Reply serialization roundtrip, new stream
on name reuse and isolation from delayed old-pane output. Strict Clippy (root/tooling),
all six local CLI scenarios, eight structure checks, formatting and the regenerated
reviewed boundary pass. Logs: `events-regressions.log`, `linux-events.log`,
`events-process.log`, `events-clippy.log`, `events-tooling-clippy.log`,
`events-local.log`, `events-boundary.log`.

Final independent event review accepted the implementation and boundary declarations.
A proposed decoder finding was rejected after actual malformed-envelope tests showed
unknown fields, missing cursor, duplicate cursor and duplicate event already reject.
The regression is retained (`events-strict-before.log`); no unnecessary decoder change
was made.

Final records: keep at most 128 released-pane records for 60 seconds, each with a
131072-byte bounded final screen, original workspace name/stream, command/cwd, observed
exit status and input sequence. Pane attribution is immutable and survives its tab.
Workspace cleanup enumerates the original stream, so the last tab's removal cannot
hide a still-owned pane from final capture/release. Forced release leaves exit status
unknown; later output/exit cannot rewrite the retained observation. Natural exit
records final bytes/status. Records remain reachable through manager Final and CLI
`fux final --instance ... PANE` after workspace sockets disappear. Pending means the
pane remains live; Conflict means the server instance changed; Expired means missing,
evicted or expired evidence. The manager remains available through retention, while
explicit server shutdown still drains immediately.

Run: generic `fux run` now polls authoritative final records and returns the observed
process status. It no longer relies on background row samples or event-connection
availability. Unknown/stale/expired evidence and truncated final screens are failures.
Manager Create atomically rejects existing/reserved names; descriptors carry the ECS
workspace stream. Launch and cleanup require both instance and stream. Cold-start
readiness uses read-only Info and validated on-disk descriptors and confirms the exact
owned child, rather than resolving/creating a name on a competing server. Cleanup
errors are reported; stale/replaced/retired lifetimes are left alone. Run flags are
parsed only before the child command, preserving child arguments after `--`.

One absolute deadline covers launch and final polling; socket connection and partial
writes use nonblocking I/O and bounded polling. Deadline writes restore original socket
flags and include the delimiter. A constrained-send-buffer/slow-drain regression
proves a peer making partial progress cannot reset the write allowance. Existing
startup and subsequent cleanup retain their separate bounded windows.

Independent final-record review identified a confirmed P1 last-tab cleanup gap; the
stream-based enumeration fixes it and an explicit last-tab/late-report regression
covers it. Run review identified and confirmed the cold-start ownership race,
Pending/Conflict ambiguity, child-option parsing, deadline resets/slow writes and
hidden cleanup errors. All were fixed and final independent review accepted the
runtime changes. Main's prior immediate-idle tests now additionally prove retained
manager availability and eventual expiry, preserving their workspace/viewer cleanup
assertions.

The main Python headless workflow was migrated to Rust `control-workflow`, with its
original archived at `tools/archive/tests/verify/agent_headless.py.txt`. It retains
workspace CLI startup, info limits, env/initial-size request, pattern wait, changed-row
capture, key notation, subscription ordering and observed exit status. It adds final
output/status after socket retirement and explicit fixture-server shutdown. A proposed
extra assertion that post-layout dimensions equal initial PTY dimensions was removed
after inspecting unchanged main layout behavior; the existing ECS spawn test verifies
the initial dimensions. Independent review accepted the preserved coverage. Five
Python CLI scenarios and the remaining tooling migration are still pending.

Latest settled checks: 104 library, 39 ECS and one protocol-fixture tests pass on both
macOS and Linux ARM64. Real `final-records` and `run-command` scenarios pass on both;
the run scenario covers immediate output/status, existing/fresh servers, name refusal,
child arguments and timeout descendant cleanup. The migrated `control-workflow` passes
on both platforms. All six local CLI scenarios, eight structure checks, three boundary
checks, strict root/tooling Clippy, formatting and diff whitespace pass.
Logs: `run-tests.log`, `run-clippy.log`, `run-tooling-clippy.log`, `run-process.log`,
`final-process.log`, `linux-final-run.log`, `linux-control-workflow.log`,
`final-run-local.log`, `final-run-boundary.log` in the integration-baseline directory.
Boundary inventory was refreshed after independent semantic acceptance.

Next: complete capability-gap audit (including silent capture/observation invalidation),
adapt companion wire consumers and standalone zor workflows, finish tooling/CI pin
reconstruction and documentation, run paired measurements, full independent root and
companion review, and the fresh mandatory gate. Descriptor stream and Pending final
responses require companion adaptation. No integrated completion or hosted CI claim
is made; the complete goal remains active.


Companion wire increment: standalone zor now negotiates main's four-byte `FUX\n`
preface and creates panes with scoped horizontal Split, equivalent to native New.
Its watch and retained-final consumers preserve incarnation, stream and input evidence.
Zor's Rust capture/runtime and lost-create-reply proxy use the same contract; historical
archived tools remain attributed to their original protocol. The root launch fault
proxy now intercepts Split, preserving before/after/held/lost response scenarios.

Independent consumer review found a confirmed P1 capture omission: zor requires
`input_sequence` from the coherent capture, but fux returned it only in listings.
Capture now reads the sequence from the same Pane borrow as its screen/metadata,
including unchanged conditional replies. The ECS regression advances input without
output and checks the unchanged capture still carries the new input sequence. Wire
roundtrip and shared fixture cover its serialized position. Independent review
accepted the fix and boundary inventory addition. Before the fix, the real workflow
failed at worker response wait; afterward both scripted and native two-worker workflows
pass, including interruption/recreation, no replay, independently checked retained
artifacts and cleanup. This remains account-free fixture evidence, not live-provider
validation.

Real launch recovery then exposed a second missing native capability: main rejected
empty non-executable argv entries. Adapted the focused native config/control validation
fix while retaining executable/NUL/count/per-entry/aggregate bounds. The Rust regression
covers configured/default-target/explicit-target split launches and exact retained final
argv/output. Both it and full managed launch recovery pass on macOS; independent review
accepted the change. Launch recovery includes replay, lost creation replies, killed
creators, retained closed evidence, gaps and identity rejection.

Increment checks on macOS: root 104 library/39 ECS/one fixture tests, strict root and
Rust-tooling Clippy; zor 131 library tests (two explicitly ignored), 26 integration tests,
15 tooling tests and strict runtime/tooling Clippy pass. Logs under integration-baseline:
`capture-input-tests.log`, `capture-input-boundary.log`, `empty-argv-tests.log`,
`empty-argv-clippy.log`, `empty-argv-tool-clippy.log`, `empty-argv-process.log`,
`zor-wire-tests.log`, `zor-wire-clippy.log`, `zor-tool-wire-tests.log`,
`zor-tool-wire-clippy.log`, `zor-workflow-before.log`, `zor-workflow-after.log`,
`zor-launch-wire.log`, `zor-launch-wire-after.log`. Linux ARM64 passes the same
104 library/39 ECS/one fixture root tests, empty-argument scenario, scripted/native
two-worker workflows and full launch recovery (`linux-zor-wire.log`). The owned
`fux-integration-zor-wire` container exited 0. The boundary checks pass after the
argv fix (`empty-argv-boundary.log`).
Companion pins/patch export, koh fixtures, remaining capability/tooling audit,
measurements, documentation and full final gate remain required.


Companion assembly increment: koh's two real-fux hello fixtures now send main's
unversioned hello and require its exact empty hello reply. No transport implementation
changed. `dependency-patches/manifest.json` now pins owning published bases
`af776a39ddea8826fe0915e712c787e303d5dbf0` (koh) and
`2a8769ede679211f81624823247c8494f046d869` (zor). Focused companion patches were exported
and verified against their owning trees through the Rust runner. Both exact patch
comparison and reconstructed source bytes pass (`companion-reconstruction.log`).

CI's optional cross-repository job now runs Rust `dependencies apply`, obtaining both
repositories and exact bases from the manifest. Removing duplicate companion checkout
refs removes the drift that broke the historical native assembly job. Assembly checks
actual HEAD before patch application, and the real-Git regression now explicitly tests
mismatched-base rejection. Four dependency-tool tests pass (`assembly-tests.log`).
Independent review accepted the complete companion diffs, reconstruction changes and
CI workflow. This fixes the local source of the known assembly failure; unpublished
CI has not executed.

MacOS koh checks: three gateway integration tests pass with both FUX_BIN/ZOR_BIN and
both KOH_REQUIRE flags set, so actual service-authorization coverage cannot silently
skip (`koh-gateway-companions.log`). Twelve non-R6 gateway library tests pass
(`koh-headless-main.log`); three explicitly named R6 cases remain excluded. The earlier
`koh-gateway-main.log` run required only fux and did not establish zor authorization.

Four more main Python CLI scenarios are migrated and archived byte-for-byte under
`tools/archive/tests/verify/`: local_attachment, local_tty, protocol_rejection and
detach_drain. Rust adaptations preserve main's hello/bindings/frame ordering, compact
wire-cell defaults, controlling TTY startup, rejected-before-raw-mode behavior, retained
shell identity, absent credentials/network sockets, and preceding-input/detach/ack/suffix
ordering. An extra malformed hello case remains. Independent review compared every main
assertion and accepted the final adaptations. These four scenarios pass on macOS and
Linux ARM64 (`local-*-rust.log`, `linux-local-rust.log`); all six root local CLI tests
and eight structure tests pass (`local-four-migration.log`). Viewer remains an active
Python scenario pending its full assertion audit. Runtime/tooling strict Clippy passes.

The broader Rust tooling test run is not green: 18 bin tests passed and nine evidence
checks failed with missing files (`assembly-local-tool-tests.log`). Failures are in
freshness/resources/screens/setup/traffic/workflow and headless-evidence fixtures, which
have not yet been migrated with their historical provenance. They are required remaining
work; do not disable those tests or relabel missing evidence as current acceptance.
Full tooling migration, remaining capture-notification audit, historical/current evidence,
paired performance measurements, consolidated docs and final complete gate remain open.


Viewer and observer migration: all main Python verification scenarios are now archived,
and root local CLI/real-zor integration dispatches only Rust scenarios. The seven archive
files were compared byte-for-byte with main `a48f839501af2bb317f559d96255013bd3f3eb33`;
`tools/archive/main-scenarios.json` records original paths, revision and SHA-256 digests.
The four Python benchmarks and dependency runner still await migration/removal.

Independent viewer review caught loss of main's exact combined copy input sequence
(Ctrl-A, `[`, Space, three `h`, `y` without waiting for another frame). Restored it as a
separate notice appearance/expiry check; the added native payload-specific clipboard
check requires a new matching copy afterward. Review accepted the final change. All
other main viewer assertions, including Unicode rename, repeated resize, clipboard and
menu isolation, tiny screens, workspace queued commands and detach/Escape behavior,
are preserved. The Rust observer retains main's malformed FUZ preface, unknown command,
progress/title observation, stable PID/focus and observer-loss checks, alongside native
identity/capture/watch/reload/state-clearing assertions. Independent review accepted
its complete coverage and owned cleanup.

Viewer and observer real-binary scenarios pass on macOS and Linux ARM64. Root local
CLI six and structure eight tests pass; the real-zor integration passes with explicit
ZOR_BIN/FUX_REQUIRE_ZOR_BIN. Strict root/tooling Clippy passes. Logs:
`viewer-rust-after.log`, `linux-viewer-rust.log`, `viewer-local-cli.log`,
`observer-rust-after.log`, `linux-observer-rust.log`, `observer-root-rust.log`,
`viewer-observer-root-clippy.log`, `viewer-observer-tool-clippy.log`.
Owned Linux containers for these scenarios exited 0. The nine missing historical-evidence
tooling tests remain unresolved; this increment does not establish full gate acceptance.


Silent-observation audit: restored the native regression for replayable resize/focus/tab
changes using main's layout units. A two-unit ratio change need not move an actual cell;
the test now uses 100 units. With the actual geometry notification removed, that visible
resize demonstrably fails (`silent-visible-resize-before.log`). The shared layout phase
now emits workspace.changed only when computed geometry changes, covering both control
resize and viewer dimensions. No-op viewer resize remains silent.

A second demonstrated gap was progress-only output: capture revision changed while grid
sequence did not, so observers received no paced notification (`silent-metadata-before.log`).
The snapshot phase now emits workspace.changed for nonempty output with no grid change,
and pane.output for changed grids. Both share existing per-pane event pacing and final
pending-update deadlines. Empty chunks are ignored. Bells/no-op escapes may conservatively
invalidate captures, matching terminal revision semantics, but do not advance grid sequence
or emit pane.output. Tests cover metadata bursts, one final invalidation, idle silence, and
history-only changes that restore the exact live grid/cursor. No new event type or parallel
state model was introduced. Independent review accepted implementation, tests and updated
Events/ownership documentation; raw events-RPC IDs were clarified after review.

Checks pass on macOS and Linux ARM64: 104 library, 41 ECS, one protocol fixture; real
event-sync and zor observer scenarios. On macOS the six CLI, eight structure, three boundary
checks and strict Clippy pass. Boundary inventory was refreshed after independent semantic
acceptance. Logs include `silent-observation-tests.log`, `silent-history-tests.log`,
`silent-boundary.log`, `silent-local-tests.log`, `silent-clippy.log`,
`silent-event-process.log`, `silent-observer-process.log`, `linux-silent-observation.log`.
The owned Linux observation container exited 0. Event traffic changes must still be
included in the planned paired measurements.

The Python dependency runner is now archived unchanged from main at
`tools/archive/tools/dependencies.py.txt`, recorded in `main-tooling.json`. Independent
review confirmed export/apply/verify equivalence, all original full-plan build/test steps,
explicit headless exclusions and stronger durable execution. CI/current command guidance
already invokes the Rust runner. Exact reconstruction and four dependency tests pass again
(`rust-only-reconstruction.log`, `rust-only-dependency-tests.log`). The four Python benchmark
scripts remain active pending their workload-preserving Rust migration. None of this resolves
the nine missing historical-evidence tests or establishes a fresh complete gate.


Basic benchmark migration: main's tools/measure.py is now represented by Rust
`fux-xtask measure BINARY [--samples N]`. The archived original is byte-identical to
main, with its digest recorded in tools/archive/main-tooling.json. Default negotiation
uses the integrated/main unversioned hello; an explicit --version option remains only
for running the same measurement against historical comparison binaries.

Preserved main's startup timing, ten-second idle CPU/wakeup sample, RSS points, latency
samples/default/percentile convention, 20,000-line workload, quiet windows and JSON
fields. Restored main's BURST''DONE shell token so command echo cannot end the output-burst
measurement prematurely. A bounded reader now retains partial prefixes/bodies across
quiet-window timeouts rather than losing frame synchronization. It checks the frame
limit before body allocation and uses absolute read deadlines; draining and marker waits
have overall bounds. Regression covers partial prefix/body timeouts, subsequent frame
alignment and oversized-prefix rejection. Independent review accepted workload/report
preservation, framing and owned cleanup.

Three measurement unit tests and strict tooling Clippy pass. Three-sample smoke runs
complete on macOS and Linux ARM64 (`measure-main-smoke.json`, `measure-main-smoke.log`,
`linux-measure-basic.log`, `measure-main-tests.log`, `measure-main-clippy.log`). These are
harness checks, not comparative performance evidence. As in main, latency measures the
first visible marker (potentially terminal echo), and CPU precision is limited by ps.
The frame, viewer and memory Python benchmarks remain active until their Rust ports
preserve the additional workloads and assertions. Paired integrated/main/native results
and the missing historical-evidence tests remain required.

Frame benchmark migration: `fux-xtask measure-frames BINARY [--keystrokes N]
[--config ROWSxCOLS[+...]]` now preserves main's six default configurations,
100-keystroke default, line clearing every 40 keys, 20,000-line burst, quiet windows,
wire-byte counters, CPU/RSS samples and report fields. Its shared attachment reader
keeps partial prefixes/bodies across polls, validates lengths before body accumulation,
checks every first-viewer delta and bounds each viewer's read turn. It does not retain
an unbounded frame history. Input writes retain the attachment helper's timeout.

The two reader regressions pass on macOS and Linux ARM64; strict tooling Clippy passes.
A 41-key two-viewer smoke passes on both platforms, and all six default configurations
with 100 keys pass on macOS. Evidence: `measure-frames-smoke.json`,
`measure-frames-defaults.json`, `linux-measure-frames.log` under the integration baseline
directory. Independent review accepted reader semantics and full workload/report
preservation with no confirmed findings. These executions validate the harness only;
paired performance comparisons remain outstanding. `tools/measure_frames.py` remains
active temporarily because the not-yet-migrated memory benchmark imports it. Archive
it byte-exactly when that dependency is removed.

Memory benchmark migration: `fux-xtask measure-memory BINARY [--scrollback N]
[--rows N] [--columns N]` preserves main's shell payloads, line/width calculation,
split, quiet windows, RSS sampling points and JSON fields. Signed floor division
preserves negative RSS deltas. The default 10,000-row workload and its division
regression pass on macOS and Linux ARM64; strict tooling Clippy passes. Evidence:
`measure-memory-default.json` and `linux-measure-memory.log`. Independent review
accepted the port with no confirmed porting defects. As in the original, split
completion is inferred from an arriving frame plus quiet draining.

The Python frame and memory scripts are now archived byte-exactly against the main
base, with SHA-256 and replacement commands in `tools/archive/main-tooling.json`.
Only the viewer Python benchmark remains active. These runs are harness checks,
not paired performance evidence.

An inherited measurement limitation was verified separately: the original memory
payload uses doubled backslashes inside shell single quotes. `/bin/sh` emits literal
`\\033` and `\\n` bytes, rather than applying the advertised styling/newlines. The Rust
port deliberately preserves those bytes for historical workload comparisons. Do not
describe these results as proof of styled-history memory cost. A separately identified
corrected workload is still needed for that claim; do not silently change historical
payloads or compare unlike workloads.

Viewer benchmark migration: `fux-xtask measure-viewer BINARY [--keystrokes N]
[--rows N] [--columns N]` preserves main's real controlling-PTY workload, initial
dimensions, 200-key default, 40-key clearing, escape-stripped rolling 1 MiB transcript,
20 ms observation windows, burst, CPU/byte sampling boundaries and output fields.
The shared terminal helper accepts initial geometry and offers deadline-bounded
continuous draining without changing the existing scenario reader.

Default viewer benchmarks pass on macOS and Linux ARM64 (`measure-viewer-default.json`,
`linux-measure-viewer.log`). Strict tooling Clippy and the existing real viewer scenario
pass (`viewer-after-measure-port.log`). Independent review accepted the full port with
no confirmed findings. These are harness validations, not paired performance claims.
The final Python benchmark is archived byte-exactly, and all five main tooling archive
entries were checked against both their main-revision sources and recorded SHA-256.
No active Python files remain in root `tools/` or `tests/`. Historical references remain
historical; Rust evidence tests, full gate integration and paired measurements still
require completion.

Historical workflow evidence restoration: selected `workflow.json` from native
`01e52cc2f562e23157b0f6aa8585a2c64f569169` and its three exact original helper sources
are now under `tools/archive/native-evidence/`. Their manifest records unchanged hashes;
the original report retains its original binary/source attribution. Only the retained
workflow regression opts into archived source paths. Explicit `verify-workflow REPORT`
validation now requires current files, without the former known-hash archive fallback.

All three workflow evidence tests pass, including current-source hash rejection,
archive integrity, missing historical source rejection and rejection of the old report
as current evidence. Strict tooling Clippy passes. A full tooling binary test run now
has 23 passes and seven missing-evidence failures (`tooling-after-workflow-archive.log`):
resources, setup, traffic, freshness, screens and the two headless evidence tests.
Those tests remain enabled and unresolved; this is not a passing final tooling gate.
Independent review verified byte identity against the native revision, manifest hashes,
restricted historical mapping and strict explicit-report validation, with no findings.

Historical resource evidence restoration: archived the native resource report and six
hash-matched sources without changing the report's original provenance. Retained-report
loading checks its manifest digest; retained-source validation requires the complete
original source inventory and verifies the registered archive bytes. Only explicitly
selected historical regressions use this path. Current capture validators are unchanged.

The full 18-case resource matrix and original negative accounting/process-identity/
cleanup cases pass, as does the new changed-or-missing source regression. Strict tooling
Clippy passes. The full tooling binary test run now has 25 passes and six failures for
still-missing evidence: setup, traffic, freshness, screens and two headless tests
(`tooling-after-resource-archive.log`). No tests were disabled. This is historical
integrity coverage; fresh integrated measurements and the final gate remain required.
Independent review verified the exact historical report/source bytes, archive lookup
scope and preserved resource checks, with no findings.

Historical controller-setup restoration: the exact native setup and build reports are
archived with their original provenance. Their source inventory, Rust harness and C
worker are verified against registered archive hashes. Distinct source versions use
hash-qualified archive paths. Retained regressions explicitly select historical
validation; explicit report-path verification continues to check current files.

The six-case setup matrix, command accounting, unavailable-diagnostic and cleanup
mutations pass. New regressions reject the historical report as current-source evidence
and reject a changed archived worker hash. Strict tooling Clippy passes. The complete
tooling binary suite now reports 27 passes and five missing-evidence failures: traffic,
freshness, screens and two headless tests (`tooling-after-setup-archive.log`). Current
measurements, remaining historical restoration and the final gate remain unfinished.
Independent review verified the exact setup/build reports and five newly archived
sources, preserved provenance checks and current-path separation, with no findings.
The current validator still requires an active controller-setup build record; historical
restoration alone does not supply current build evidence.

Historical traffic/freshness restoration: archived both exact native reports and
their hash-matched source dependencies. Default retained regressions explicitly select
the archives; report-path verification retains current-source checks. The nine-case
traffic and six-case freshness matrices and all original byte-accounting, stale-pane,
timing, censoring, state and cleanup mutations pass. Added tests reject changed/missing
source inventories and reject historical reports through current verification.

Strict tooling Clippy passes. The full tooling binary suite now has 31 passes and
three missing-evidence failures: screens and the two headless tests
(`tooling-after-traffic-freshness-archive.log`). Independent review confirmed exact
report bytes, archived source hashes and preserved checks with no findings. Herdr source
provenance in this review was verified by the report's recorded hash, not an independent
Git comparison. These remain historical integrity checks, not fresh integrated evidence.

Historical screen restoration: archived the exact native screen report and 36 additional
hash-matched detector sources, harness and viewport fixtures (existing shared sources
are reused). The historical corpus is regenerated from registered archived fixture
bytes, including all original negative cases. Metadata, normalized text hashes,
verdict/score attribution and reference-build checks remain enforced. Current capture
and explicit report validation use current fixtures and source files.

Both screen tests pass, including changed viewport hash rejection and rejection of
historical provenance as current evidence. Removed the comparison verifier's obsolete
automatic known-hash archive fallbacks: current hash verification now always reads the
requested current path, while retained tests explicitly choose archives. Independent
review accepted the exact archive bytes, preserved corpus/negative checks and final
call-site separation with no findings. Strict tooling Clippy passes. The full tooling
binary suite now has 33 passes and only the two still-missing headless evidence tests
failing (`tooling-after-screens-archive.log`). Fresh integrated detection measurements
and the final gate remain outstanding.

Headless archive preparation: retained the five exact native headless reports (before,
after-sharing, journal, baseline build and sharing build) and their available exact
sources. Recovered older snapshot/view versions from root Git history and the older koh
resume source from its owning history; recovery locations are recorded in the archive
manifest. Two baseline-build source versions remain unresolved: Cargo.toml SHA-256
`53e84a04e8be7cc4c281503b6361ac8cb8c7a7978aa097f5830f8fa7463c2fb9` and
zor/src/dashboard.rs `818a3b0bf14782103a3da72a2578e05a5d5a24be2766aa94da1bf42569fe1411`.
Current reference files, relevant path histories and both retained native-performance
source tarballs did not match. The original hashes remain intact, and unresolved entries
are explicit in the manifest; no newer source has been substituted. Headless validator
adaptation and further source recovery remain unfinished, so its two failing tests have
not yet been resolved.

Headless verifier adaptation: retained checks now explicitly load hash-verified archived
reports and sources; current validation has no historical fallback. Original paired
matrix/geometry, build linkage, counters above f64 precision, owner identity, cleanup,
and journal checks remain. The current source-inventory test uses historical raw rows
only as an in-memory validator fixture, with current harness/helper hashes; it is not
reported as a fresh performance capture.

Restored the exact C resource sampler at its active tooling path because current Rust
resource/headless runners compile it on macOS. Removing calibration from the test fixture
initially failed its measured-owner invariant; that change was reverted, preserving full
calibration coverage. Independent review accepted the correction and exact restored bytes.
All tooling tests now pass (22 library, 36 binary, zero doc tests), and strict Clippy passes
(`tooling-after-headless-archive.log`). The real macOS `resource-sampler-check` also passes
(`restored-resource-sampler.json` and `.log`). Review confirmed historical/current source
separation and preserved headless checks. The original baseline validator never verified
its full source inventory: the two explicitly unresolved baseline source versions remain
a provenance limitation, not silently covered by these passing tests. Fresh performance
captures and the settled-source integration gate remain required.

First diagnostic integrated gate: explicit herdr source pin now lives in
`tools/xtask/reference.json`, independently of comparison build evidence. Its checkout
is at the pinned revision; gate reconstruction validates the exact HEAD. Dependency
tests, Clippy and independent review accepted this separation. Run `gate-gRfKJC`
reconstructed companions and passed checks 0–34, then failed fixture-child Clippy because
its separate lockfile omitted fux's direct filedescriptor dependency. The runner exited
after recording the failure; the one-line lockfile update now passes that Clippy check.
Fixture-child runtime tests remain under investigation and this run is not final acceptance.

Gate coverage expansion: restored the native zor integration entry points (21 total,
including main's observer), required binary enforcement, sequential execution and fixture
builders. Added six local entry points for receipts, replay, final records, UTF-8, empty
arguments and run. All 12 local CLI tests pass (`expanded-local-cli.log`). Converted four
stale native pane-creation requests in service/worktree and task scenarios to main's
horizontal split contract. Independent review accepted these changes with no findings;
the expanded zor suite is still running (`expanded-zor-integration.log`). The old
interactive version-migration scenario remains pending an explicit main-based decision.

Expanded integration results: all 21 zor entries pass, including service/worktrees,
tasks, recovery, native provider fixtures and the two-worker artifact workflow
(`expanded-zor-integration.log`, 269.78 seconds). Fixture-child's original binary suite
reported four shutdown-deadline failures because it expected immediate manager exit
after last-pane retirement. Updated cleanup first proves workspace sockets retire and
the manager remains alive for final evidence, then explicitly signals the owned manager.
Natural exit additionally polls only the pending final state and checks exit 29 and
FINAL_BINARY, retaining the viewer final-paint/status assertions.

The corrected fixture suite passes all three unit, eight binary and two lifecycle tests
(`fixture-retention-suite.log`); its strict all-target Clippy also passes. Independent
review accepted lifecycle/cleanup semantics. It identified an inherited harness limit:
Environment::run uses blocking Command::output, so a polling deadline alone does not bound
a hung individual CLI invocation. This remains a harness-hardening item for final review.
The first diagnostic gate remains failed historical evidence; these fixes require a new
settled-source invocation rather than relabeling that run.

Migration capability decision: main's unversioned protocol rejects incompatible managers
with save/restart guidance; the native interactive replacement dialog is superseded.
Adapted the scenario to prove noninteractive and PTY rejection leave the old process and
descriptor unchanged and do not enter the alternate screen. Only the fixture explicitly
stops its owned old manager, after which fresh startup and detach succeed. This scenario
is now the thirteenth local CLI entry, so it participates in the gate. Targeted macOS and
Linux ARM64 runs pass (`main-manager-rejection.log`, `linux-manager-rejection.log`), strict
tooling Clippy passes, and independent review accepted the behavioral decision and
coverage with no findings. No compatibility dialog or second runtime was introduced.

Current headless performance pilot: the first invocation rejected main's one-time
bindings frame, revealing an unadapted full-frame assumption. The reader now accepts
bindings, default empty cells and records each fixed pane's unique READY/DONE_B01/
DONE_S01 marker across all changed-row deltas. Later metadata-only updates cannot erase
an already observed phase completion. Wire counters and the 25 ms slow-reader pacing
remain unchanged. This marker tracking is specific to fixed pane identities and unique
phase labels; it is not a general screen reconstruction API.

The fragmented-reader regression and strict tooling Clippy pass. Independent review
accepted the workload semantics and preserved counters with no findings. The current
one-repetition pilot passes all four configurations, including four panes/four viewers
and a slow consumer (`integrated-headless-pilot-v2.json` and `.log`). Explicit current-source
report validation passes. These are current integrated harness results; paired main/native
measurements and performance conclusions still require completion.

### Paired main/integrated measurements: first complete set

The three alternating-order repetitions of all four migrated default workloads completed
successfully (24 runs). Raw JSON, command logs, exact binary/harness hashes and source
hashes are retained under `.verification/paired-main-integrated/`; `manifest.json` records
successful terminal results and `summary.json` contains every numeric metric's samples,
median and percentage change. Main is a48f839; integrated binary SHA-256 is
acddc9d0f3e12830e3e7a7842666520a3bbc668c2193080d280e784e76b1f205.

These local debug-build results do not establish performance parity. Median startup RSS
increased from 11648 to 12192 KiB. Real-viewer CPU per 1000 keys increased from 1.50 to
1.85 seconds server-side and 3.25 to 4.00 seconds viewer-side. Eight-viewer frame latency
increased from 1.00 to 1.19 ms median and 1.57 to 2.31 ms p95; per-key frame byte counts
were unchanged. Burst results vary by configuration, including improvements and
regressions. Three short repetitions and coarse CPU accounting limit causal conclusions.
A focused alternating-order repeat with 2000 keys for real-viewer and eight-viewer
workloads is running to test whether the interactive differences persist. No runtime
optimization has been applied on the strength of these measurements alone.

The inherited memory workload retains its original literal-backslash payload; its
plain/wide field names do not prove styled-history allocation costs. Native-baseline
comparisons and the final settled-source gate remain outstanding.

### Current documentation consolidation

HANDOFF, README, architecture and local-control guidance now describe the main-based
integration: generic ownership, coherent capture and separate sequence contracts, tracked
input, replay identity, final-record retention and `run` consumption. The architecture
schedule includes wait resolution before lifecycle. Obsolete handoff claims about OSC 7877
agent state and missing Linux execution were removed. The handoff explicitly distinguishes
targeted Linux evidence from an unproven complete final gate.

Independent documentation review checked these claims against current code. Confirmed
findings fixed: workspace creation does not accept pane spawn environment/size fields;
final records may be evicted before 60 seconds; six scheduled systems currently use
`&mut World`, rather than four. Final bounded review found no remaining issues. This is a
documentation review, not the required final complete root/companion code review.
`git diff --check` passes. No runtime or measurement source changed during the focused run.

The published native source at 01e52cc2f562e23157b0f6aa8585a2c64f569169 is archived under
`.verification/native-baseline/source.tar`, SHA-256
0b9f213aeca223ba7eebcf2aaa09de226cceb567c7f85bd797d07661d07581cc.
A fresh comparison binary remains to be built after the active timing run ends.

### Focused interactive comparison completed

All 12 alternating-order runs with 2000 keystrokes completed successfully; exact commands
and raw samples are in `.verification/paired-main-integrated/focused-manifest.json`, with
medians in `focused-summary.json`. Real-viewer server CPU per 1000 keys is 1.765 seconds
main versus 1.985 integrated (+12.5%); viewer CPU is 3.92 versus 4.33 seconds (+10.5%).
Eight-viewer median key latency is 0.96 versus 1.01 ms, p95 1.44 versus 1.52 ms, and server
CPU 0.96 versus 1.02 seconds per 1000 keys. Per-key bytes remain identical. The longer
runs reduce the apparent eight-viewer difference but do not erase interactive overhead.

Read-only independent performance review identified added work, without claiming causality:
per-input event replay serializes into a disposable allocation to count exact bytes; terminal
UTF-8 continuation tracking scans filtered output again; cancellable nonblocking PTY reads
add a poll syscall between isolated arrivals. Any optimization must preserve replay byte
bounds, invalidations, malformed/split UTF-8 behavior and stalled-writer shutdown. The first
candidate can be evaluated with exact counting serialization without allocating a buffer;
no such change is included in the measurements above.

A locked debug build of the exact archived native source has started in its own target
directory (`.verification/native-baseline/build.log`). This is separate from the reference
checkout and began only after the focused benchmark reached successful terminal completion.

### Published native versus integrated: basic and real-viewer pair

The exact native debug build succeeded (binary SHA-256
be51dd0158c2ec05622a60d8aba050b53d2ba21a0ed930b639a40dc899648bf5), followed by
12 successful alternating-order runs: three repetitions of the unchanged default basic
and real-viewer workloads for each binary. Native negotiation explicitly uses attachment
version 6; integrated uses its unversioned contract. Commands, hashes, raw JSON and medians
are under `.verification/native-baseline/paired-{manifest,summary}.json` and adjacent files.

Median basic echo latency is 42.18 ms native versus 3.63 ms integrated; real-viewer server
CPU is 14.05 versus 1.85 seconds per 1000 keys, viewer CPU 9.15 versus 3.95. Native burst
completion is faster: 0.164 versus 0.229 seconds basic, 0.163 versus 0.259 seconds through
the real viewer. Burst terminal bytes differ greatly (4293 versus 45646), reflecting the
rendering/pacing difference; these are not equivalent frame counts. No overall performance
winner is inferred from a single metric. These comparisons preserve native capability value
without implying its full-frame rendering should replace main's retained grids.

The current zor observer requires capture revisions absent from baseline main, so running
it unchanged against main would not be an equivalent headless observation workload. The
native/integrated headless pair still needs current evidence; generic main viewer/burst
comparisons are already covered separately.

Full independent runtime review found no newly confirmed P0/P1 issues. Full tooling and
companion review confirmed exact patches, 45 gate commands and archived report/source hashes,
and found the fixture-child blocking command helper as a P2. It is now replaced by bounded
execution/capture with kill/reap on failure, including the startup-failure case. A short
stalled-child regression passed; full affected fixture rerun and final fix review are active.

### Fixture deadline fix verified

The complete affected fixture binary suite passes (9 tests, 65.62 seconds), and all-target
strict fixture Clippy passes after bounded command capture was introduced. Final independent
review accepted the deadline, kill/reap and bounded-read behavior with no new findings.
Logs: `.verification/integration-baseline/fixture-command-deadline-full.log`.

The native headless comparison uses an isolated archive of published zor
2a8769ede679211f81624823247c8494f046d869 without the integrated wire patch, together with
its native harness. The synthetic performance worker and C resource sampler are byte-identical
between native and integrated sources. Harness differences are the negotiated protocol and
full-frame versus delta marker recognition; matrix, workload, pacing and resource counters
remain unchanged. Harness hashes will accompany reports, so comparisons do not pretend the
wire decoders are identical.

### Headless comparison completed and validated

All six paired invocations passed (three alternating native/integrated pairs, each with
four pane/viewer configurations). All six generated reports also pass their matching
explicit `verify-headless-performance` validator. Exact commands, binary/harness hashes,
results, derived medians and validator outputs are in
`.verification/native-baseline/headless-{manifest,summary,validation}.json` and the six
adjacent raw reports. Every result confirms owned fux/zor exits and worker cleanup; owner
PID/start-time pairs remained identical across each measured phase.

For sustained output with one pane/one viewer, median fux CPU is 1031.18 ms native versus
219.48 ms integrated, and attachment bytes 20,004,476 versus 999,571. With four panes/four
viewers including one slow consumer, CPU is 1318.90 versus 561.84 ms, bytes 16,830,786 versus
5,328,490, and visible completion 1413.10 versus 1312.77 ms. Idle fux CPU and attachment
traffic remain zero in all four configurations. These are local synthetic debug-build
measurements with separate full-frame/delta decoders, not live-provider or remote evidence.

Performance disposition: preserve main's retained-grid architecture and the selected generic
reliability guarantees. The integrated implementation has measured interactive overhead
against main (12.5% server CPU and 10.5% viewer CPU in the focused real-viewer median), and
some burst timing regressions against native. It also preserves dramatically lower interactive
and headless cost than native's architecture. No blanket parity or universal speedup is
claimed. Review identified candidate costs, but did not establish a correctness defect or a
causal profile. Avoid speculative PTY or UTF-8 changes that would weaken proven guarantees;
this overhead remains an explicit handoff limitation rather than an unreported optimization
success. There is no specified numerical performance threshold in the task.

### Release-package coverage restored

The main release-package shell entry point still contained a Python metadata expression;
it now invokes the existing bounded Rust package-version helper. It explicitly respects
CARGO_TARGET_DIR when packaging and locating the extracted crate. Both gate plans now run
the full package/install/version/installed-binary fixture check, replacing the narrower
headless `cargo package` entry. Independent focused review accepted the script and target
path handling. Shell syntax and diff checks pass; full execution is underway. The headless
plan still contains 45 commands because the packaging command was replaced, not duplicated.

### Settled-source gate handoff

The Rust-backed release-package check completed successfully: package verification, isolated
installation, version execution and all 9 installed-binary fixture tests pass (65.34 seconds
for that test suite). The gate-plan regression initially exposed placement of packaging after
the full plan's network-only tail; packaging now precedes that tail. The corrected regression
passes, and independent final review confirms 31 full-plan commands, 45 headless commands,
and unchanged explicit R6 exclusions. Logs are `release-package-rust.log` and
`package-gate-plan-fixed.log` under `.verification/integration-baseline/`.

Full runtime and tooling/companion independent review are complete, with confirmed in-scope
findings fixed and the affected fixes reviewed again. Runtime performance limitations are
explicitly recorded above. A fresh mandatory headless gate is now being invoked on this
settled source; its durable terminal manifest, not this statement of intent, determines
whether final verification succeeds. No previous diagnostic or resumed gate substitutes
for that invocation. Results will be retained under `.verification/` for handoff without
modifying source during execution.

### Fresh gate failure diagnosed: check-worker admission contention

The fresh gate `.verification/gate-chtwgv` terminated with failure at check 27: 20 zor
integration cases passed, but `zor_check_workers` reached a fixture observation deadline.
The preceding 27 gate checks, 3 boundary tests, 41 ECS tests, 13 local CLI tests and 8 structure
tests passed. This failed invocation is retained as diagnostic evidence, not final acceptance.

An isolated harness reproduced the failure and captured an explicit `task-busy` reply for
shutdown held-check request 201 before its marker could be created. Background journal
activity can reject admission before spawning. The scenario ignored that completed error
and waited for a marker that could never appear. Evidence is retained in
`.verification/integration-baseline/check-workers-observed-replies.json` (attempt 6).

The scenario now watches replies while waiting for admission and retries only a validated
Busy envelope with the same service instance, request ID and full check intent. Other errors
and unknown outcomes fail without replay. Admission retains its original five-second deadline;
held-result completion has a separate five-second deadline. Queue capacity and exact overload
assertions are unchanged. No fux or zor runtime change was needed.

A deterministic Busy-then-admitted fixture verifies unchanged intent and preservation of the
held reply. It passes, as do strict tooling Clippy and build. Independent final review accepted
the fix after correcting completion-deadline reuse and mock socket ordering. Twenty repeated
real-process scenarios are running before the next fresh complete gate. The earlier gate
cannot be resumed as final acceptance after this source change.

The first fixed repetition series then exposed an explicit Busy reply in the scenario's
setup/inspection API helper (attempt 8 in `check-workers-fixed-repetitions.json`). Its retry
policy is now explicitly restricted to `start`, `inspect`, `check-inspect` and `cancel`, with
unchanged full request and validated Busy identity. Start validates stored launch intent,
cancel is idempotent, and inspections are read-only. No unknown response or transport failure
authorizes replay. The original two-second coordination responsiveness assertion remains.
Independent review accepted this follow-up; strict Clippy/build pass. A fresh 20-run series
and Linux targeted fixture/scenario check are active before the replacement fresh gate.

The follow-up completed all 20 real-process repetitions successfully
(`check-workers-api-fixed-repetitions.json`). Linux ARM64 also passes the deterministic
admission test and full check-worker scenario (`linux-check-workers-admission.log`).
Final independent fix review has no remaining findings. A new complete mandatory headless
gate is being invoked on this settled source, with no continuation of the failed invocation.

### Release performance pass (2026-09-09)

Snapshot of the complete pre-optimization integration (tracked, untracked and companion
state): `.verification/perf-baseline/integration-before.tar` (SHA-256
`6837205264496cd3c580333accfd5a382a565fa321bb898abf335717fb5fe7fb`) with a per-file inventory.
Release binaries for main `a48f839`, that snapshot and the candidate were built with identical
settings (`--release`, `CARGO_PROFILE_RELEASE_DEBUG=1` for symbols, `CARGO_INCREMENTAL=0`), so
the profiled binary is the measured binary. Full report, raw samples and commands:
`.verification/perf-2026-09-09/REPORT.md`.

Release measurements do not reproduce the debug-build interactive overhead: main, the
snapshot and the candidate are within run-to-run noise on real-viewer, attachment-frame and
zor headless workloads (server CPU per keystroke is about 0.15–0.25 ms in release versus about
2 ms in debug). Sampled profiles attribute the server's active time mainly to retained-grid
refresh, per-step `QueryState` construction in exclusive systems, frame encoding and socket
writes; koh's local gateway cost is dominated by QUIC/UDP and AES-GCM, and zor's observer
costs a few milliseconds per workload phase. Three integration-added costs were confirmed by
reading and micro-timing and removed without changing behavior: the event log now computes
the exact encoded length with a counting writer and hands it to publication (no second
serialization, no allocation, and publication returns before cloning when nobody subscribes);
the terminal derives the UTF-8 continuation state from the last four filtered bytes instead
of rescanning every chunk (exhaustively tested equal to the full fold); and input completion
handling is a plain system that returns immediately when nothing is tracked. In isolation
`EventLog::push` fell from 136 ns to 77 ns and `ServerTerminal::process` from 14.9 to 10.4
ns/byte (main: 20 ns/byte). No zor or koh runtime change was justified by measurement; the
harness gained `measure-koh` and microsecond CPU sampling.

### zor merged into the fux repository (2026-09-09)

zor now lives at `crates/zor` of a virtual workspace whose other member is `crates/fux`; both
remain separate crates and binaries with their own tests and lints, sharing one lockfile, CI
matrix and gate. The import carries zor's history (`2a8769e`) with the reviewed
`dependency-patches/zor.patch` applied as its own commit; the resulting tree is byte-identical
to the previously verified base-plus-patch checkout. The zor pin, patch and reconstruction are
gone from the manifest, the xtask runner and CI; the real zor integration now builds
`crates/zor` from the checkout (`ZOR_BIN` remains an override). koh stays a pinned and patched
external repository until fux publishes a release it can depend on.
