# Multi-machine supervision implementation

Status: in progress. This document tracks `multi-machine-navigation-and-supervision-prompt.md`;
no multi-machine completion or parity claim is made yet.

## Starting evidence

- Fux/zor revision: `02e719090c606c5f0aa5d9b8cb1b7841315738a4`.
- Koh published pin: `da712875e4f527b718abe44e9d68f94048e916c7`; reference is clean.
- The implementation prompt was the only untracked input at the start.
- Koh CI passed. Fux's pinned composition, macOS and standalone package jobs passed.
- Fux baseline CI `34774247640` fails Linux `viewer-mouse-app` while waiting for both
  reporting applications, and Betamax strict clippy with the newer
  `chunks_exact_to_as_chunks` lint. These failures precede this task. Logs were read from
  GitHub and retained locally at `/tmp/multi-machine-baseline-ci.log`.

## API inventory and decisions

Zor already has bounded private service v1 JSON-line operations (`ping`, `snapshot`,
`shutdown`, `task`) and a broad task dispatcher. Task operations require an exact service
incarnation, but existing CLI task commands often access a local task store directly.
Machine selection must intercept supported commands before that local-store dispatch and
reject unsupported routing. Local `--directory` auto-start semantics must not become remote
semantics.

The dashboard already merges service observations with task overviews, preserves row selection
and has terminal/notification guards. Extend it through typed per-host results and independent
bounded fetchers. Koh already has separate authenticated opaque gateway processes. Fux already
has `attach --socket`; initial exact pane/viewer selection still needs implementation analysis.

The saved-machine catalog owns only profiles: stable IDs, display names, an optional control
binding and explicit workspace attachment bindings. Koh retains keys and authenticates endpoints.
Control and attachment cannot share a service endpoint accidentally. Catalog edits use a
persistent advisory lock and private atomic replacement; reads and profiles are bounded.

## Requirements ledger

| Requirement | Status / required proof |
|---|---|
| Private stable machine catalog and CLI | Implemented; catalog validation and real CLI lifecycle/routing tests passed; interactive reload implemented and verified in checkpoint 9 |
| Typed Local/remote service client and capabilities | Typed reads, combined supervision, guarded mutations and task attachment implemented; schema/deadline/incarnation tests passed |
| Owned koh helpers and independent connection status | Development helper/status contract and shared control leases implemented; ownership, retirement, CLI and controller cleanup tests passed; published-pin integration remains pending |
| Aggregate and machine-scoped supervision | Independent readers, task selection/actions and explicit `--machine` entry implemented; real remote-scoped handoff passed; reload verified in checkpoint 9; notifications verified in checkpoint 10; remaining acceptance is pending |
| Exact remote pane attachment and clean return | Task-based non-default workspace handoff, input isolation, detach, viewer SIGKILL recovery and cancellation passed; standalone observed-agent handoff and task regression passed in checkpoint 8; real reconnect/expiry workflow passed in checkpoint 12; application restart acceptance remains pending |
| Mutation identity/reconciliation | Guarded CLI/dashboard cancel/stop and explicit lifecycle reconciliation implemented; lost-reply/restart product scenarios and supported resume remain pending |
| Local + two real remote stacks in ordinary CI | Pending; exact binary/pin evidence with required prerequisites |
| Betamax visual acceptance | Wide task/observed workflows, notification state, and 80x24/40x16 handoff/help states reviewed; remaining recovery states pending |
| Documentation and two-host manual walkthrough | Current guide, reproducible fixtures and two-host manual checklist documented; actual physical-host acceptance remains pending |
| Full relevant verification and separate review | Pending; no claim based on narrow catalog tests |

No commits, pushes, remote deployment or changes to existing user services are part of this
implementation without subsequent publication authorization.

## First implementation checkpoint

Implemented `zor machine add/list/inspect/rename/remove/control/bind` with global
`--machines-file`. A profile has a stable random ID, optional zor control binding and
independent per-workspace attachment bindings. `--clear` is required to remove a binding.
Local is listed explicitly but cannot be shadowed by a remote alias. Renaming preserves
identity; unknown selectors, malformed/private-file violations and failed edits do not
fall back to Local or touch its task store. Maximums: 32 machines, 64 bindings per machine,
256 KiB catalog. Keys remain koh-owned files referenced by absolute path.

`service::client::Client` now handles bounded typed capability/snapshot/overview reads over
an explicit socket. It does not auto-start services or read task files. The dashboard
composes typed observations and overview rows; it no longer decodes service wire envelopes.
Existing service convenience functions reuse the same private exchange. Snapshot and overview
must have the same service incarnation. A shared caller deadline covers the workflow,
retaining the prior three-second per-request and one-second write caps. The new capability
operation currently advertises only snapshot-v1 and overview-v1; it does not advertise
unimplemented remote mutations or restart behavior.

Verification so far uses `CARGO_TARGET_DIR=/tmp/fux-multi-machine-build` and `cargo +stable`:

- Full zor library pass after initial read-client migration: 167 passed, two existing optional
  installed Codex probes ignored. Subsequently added a capability contract test; targeted
  client tests include that test and the preserved absolute-deadline cap.
- Six typed client tests exercise envelope/schema/correlation, explicit failure provenance,
  mismatched incarnations, malformed/oversized/closed frames, slow partial response and absent
  proxies without local service creation.
- Four catalog unit tests and two real CLI profile lifecycle tests passed.
- Strict all-target zor clippy and formatting are checked after the final targeted edits.
- Real `zor-service` and `zor-dashboard` scenarios passed with the new CLI binary, including
  restart, task evidence, client isolation, focus, notifications and terminal restoration.
  These scenarios ran before the final restoration of the existing phase caps; no normal
  response-path behavior changed with that cap preservation.

Retained checkpoint logs are in `docs/verification/multi-machine/checkpoint-1/`. These checkpoint-1 tests
prove the implemented local foundation, not the pending multi-machine workflow. At checkpoint 1, no koh
changes, helper processes, remote action routing, aggregate dashboard, attachment handoff,
new composition CI or new visual acceptance have been implemented yet.

Next: add owned koh process connections with explicit per-host lifecycle, then connect the
read client to stable machine selection and independent dashboard fetchers. The current koh
CLI does not expose structured connection failure states; inspect its generic status surface
before distinguishing unauthorized, expired and offline in the UI. Do not infer those states
from a generic local socket EOF. Extend koh in a separate development checkout if necessary.
Remote mutations must additionally guard task/attempt identity (service incarnation alone
cannot prevent a same-name task replacement race).

## Second implementation checkpoint

Added an owned `koh gateway connect` helper with a private proxy directory, noninteractive
existing credentials, bounded startup/status/diagnostics, and bounded child-group cleanup.
`Running::try_wait` records reap state so subsequent cleanup cannot signal a reused child PID.
The remote CLI now intercepts explicit non-Local machine selection before local dispatch;
status and one-shot dashboard reads negotiate capabilities and validate service incarnation.
Interactive dashboard and remote task commands remain unsupported at this checkpoint.
The routed CLI builds, but its complete real-process invocation is still pending verification.

Koh changes are in `/tmp/fux-multi-machine/koh-dev`, based on published
`da712875e4f527b718abe44e9d68f94048e916c7`. A bounded latest-status interface reports generic
connection transitions, including authenticated authorization rejection and distinct expired
or ended sessions. The optional private `--status-file` contains no application identities,
credentials, payloads or resume tokens. Ready is local helper readiness, not remote admission.
`references/koh` is unchanged and clean. The patch and base revision are retained in
`docs/verification/multi-machine/checkpoint-2/`; `git apply --check` against the clean reference
passed. This unpublished development API is not yet available through the ordinary companion pin.

Verification (all completed):

- Zor strict all-target clippy and binary build passed using `/tmp/fux-multi-machine-build`.
- Two helper support tests passed: private/bounded/schema/sequence status validation and
  cleanup that removes owned entries while preserving unknown files.
- Koh strict all-target clippy passed with `--no-default-features --features cli,gateway`.
- Status-file test passed for private writes, updates and refusal to overwrite a replacement.
- Three gateway integration tests passed with both `FUX_BIN` and `ZOR_BIN` supplied and
  `KOH_REQUIRE_FUX_BIN=1 KOH_REQUIRE_ZOR_BIN=1`: real authorization separation, exact opaque
  byte transfer, and preserving real panes after gateway failure.
- Two real fux resume tests passed with `FUX_BIN` required: five forced QUIC losses with
  input applied once, and actual retention expiry followed by a fresh attachment.

Koh tests used `/tmp/strict-fux-koh-zor/koh-build`, the fux binary from
`/tmp/fux-codebase-work/build/debug/fux`, and the newly built zor binary. Commands/results are
retained in checkpoint-2 logs. These establish transport regression coverage; they do not prove
the pending Local-plus-two-remotes product workflow, UI responsiveness, mutation reconciliation,
viewer handoff, or visual acceptance. Next: exercise machine-routed reads through the owned
helper, then implement independent per-machine supervision and exact-identity task actions.

## Third implementation checkpoint

The real owned-helper CLI probe caught a Clap argument-ID collision: the global `--machine`
selector shared its internal ID with profile subcommand positional arguments. `machine rename`
was incorrectly dispatched remotely. The global selector now has a distinct argument ID;
the existing real profile lifecycle tests cover this regression and pass.

`machines::supervision` provides one bounded observation worker per machine, up to Local plus
32 profiles. Each has one latest cached view; network I/O occurs outside the shared snapshot
lock. Reads have a six-second overall deadline, one-second polling interval, and a five-second
freshness limit. Failure preserves old evidence without renewing its timestamp, and makes it
non-authoritative. Selection includes machine ID, service incarnation and row key. All workers
receive shutdown before joining, so shutdown deadlines run concurrently.

`zor dashboard --all-machines` now supports an aggregate view and Tab cycling through machine
scopes, with j/k row selection and remembered selections per scope. Enter currently displays
row detail only. It does not yet attach or mutate tasks. The one-shot form returns separately
attributed views, age, freshness and problems. Local uses an explicit local service socket;
this mode does not auto-start services. Unconfigured profiles remain visible as unavailable.
Notifications/bell are explicitly unavailable in this initial aggregate mode rather than
silently ignored. Catalog reload, remote transport-state presentation and row-level evidence
freshness need further implementation before final acceptance.

Verification so far:

- Full zor library suite: 173 passed, two existing optional probes ignored; profile CLI: two
  passed. Strict all-target clippy and binary build passed after navigation changes.
- Worker tests prove a blocked host cannot block healthy publication or snapshot reads,
  and cached selection cannot cross machine/service identity boundaries.
- A terminal-emulator test covers machine-scoped errors and a tiny two-row rendering.
- The reproducible `checkpoint-3/routed-read.py` fixture requires explicit built fux, zor and
  koh binaries. The noninteractive pass ran Local plus two isolated remote fux/zor stacks,
  required three different service incarnations, verified saved-ID routing, rename, aggregate
  reads, unconfigured-machine isolation, unauthorized access, cleanup and remote-owner survival.
- Interactive PTY navigation/restoration was added to that fixture. Its first run exposed
  macOS's driver-managed PENDIN transition bit in an otherwise exact termios restoration.
  A minimal PTY reproduction confirmed that difference. The assertion now compares every
  other termios bit, speed and control character; the repeated fixture result is recorded below.

This is still a partial implementation. Required task actions, task/attempt transaction guards,
exact viewer handoff, catalog edits during supervision, notification identity/freshness,
restart/lost-mutation scenarios, Betamax review and ordinary CI integration are unfinished.
The real-process fixture is checkpoint evidence, not yet the required xtask/CI scenario.

The repeated Local-plus-two-remotes fixture passed, including interactive All → Local → remote
scope navigation, terminal restoration, unauthorized access and cleanup. The raw PTY recording
is `checkpoint-3/dashboard-navigation.ansi`; it is not yet Betamax visual approval. Exact binary
hashes, successful logs and the earlier diagnostic failures are retained alongside the fixture.
No implementation tests or fixture processes remain running at this checkpoint. No publication
or companion-pin update has occurred.

## Fourth implementation checkpoint

Remote CLI task list/inspect/result now use typed replies over the selected control endpoint.
Task inspection checks task → attempt → session identity and result reads check task ID and
service incarnation. Existing receipt/provider/check/artifact evidence retains its original
field names and claim scopes. The service advertises `task-read-v1` only with these operations.
List bounds match the journal's task and retained-launch limits. The actual remote fixture
creates identically named tasks with different titles and fux identities on its two stacks.

Added `task-supervise-v1` for guarded cancel, managed stop and explicit launch reconciliation.
The client reads eligibility and expected task/attempt/session/process identity, then sends
one guarded request. The server validates the guard while holding `Store`'s exclusive lock,
which remains held through the journal transition and exact-process stop/reconciliation.
A same-name replacement cannot slip between validation and action. Adopted resources grant no
termination authority. Stop still uses fux's existing exact-target checks; reconciliation
refreshes retained lifecycle evidence and never sends another kill or launches a replacement.

Failed or lost action replies remain unconfirmed and are not replayed. A live stop experiment
returned before final close evidence was committed; the correct follow-up is guarded
`task launch-reconcile`, not an automatic second stop. A separate run exposed transient
journal contention during task listing. Read-only list/inspect/result/overview queries now
retry only the typed `task-busy` response, with ten-millisecond backoff within the caller's
unchanged absolute deadline. Supervision uses a separate one-request path. A regression test
requires busy reads to recover and the same busy mutation response to return without retry.

The aggregate renderer now applies each row's retained age as well as the machine observation
age, so a row can become stale between polls. Added the initial user guide at
`docs/multi-machine-supervision.md`, documenting implemented commands and explicit omissions.

Checks completed before the final fixture rerun:

- Seven typed read-contract tests passed, then eight after adding busy-read versus mutation
  retry coverage. These include wrong task/session links, missing required fields, wrong
  service incarnation, incompatible result identity and preflight adopted-stop refusal.
- Guard unit test passed: stale attempt and changed PID reject without journal writes;
  adopted stop rejects; correct cancellation preserves the recorded session/process.
- Full zor suite: 175 passed, two existing optional probes ignored; profile CLI: two passed.
  This preceded the final read-contention handling and reconciliation eligibility refinements.
- Strict all-target clippy passed at the guarded-action checkpoint; final checks follow below.
- The actual two-remote fixture passed same-named task reads, guarded cancellation isolation,
  wrong-machine attempt rejection, adopted-stop refusal, interactive navigation and cleanup.
  Managed-stop completion and explicit reconciliation were subsequently added to that fixture.

Pending: dashboard task/attempt selection authority and asynchronous actions, resume policy,
exact viewer handoff, catalog reload, notification scoping, lost-reply/restart composition,
Betamax review, CI integration and full final review/gates. No universal parity claim is made.

Final checkpoint-4 verification passed: 176 zor library tests, two profile CLI tests,
strict all-target zor clippy and formatting. Two existing optional provider probes remain
ignored. The expanded real-process fixture passed guarded cancellation, managed stop followed
by explicit reconciliation to `closed`, and preservation of the other remote's same-named
managed task in `attached` state. It also retained all prior routing, wrong-attempt refusal,
interactive navigation, terminal restoration, authorization and cleanup assertions.

Checkpoint-4 retains the executable fixture, raw PTY output, binary hashes, successful logs
and the earlier diagnostic failures. These are development-checkout results, not published-pin
CI or full product acceptance. All checkpoint tests and fixture processes are terminal.
No commits, pushes or changes to existing user services occurred.

## Fifth implementation checkpoint

Fux's optional exact attachment hello carries server instance, current workspace/stream,
pane ID and PID. The owner loop validates admission before sending its hello; rejected or
stale targets never receive an accepted hello. The selected pane is set only on the arriving
viewer, preserving other viewers and workspace defaults. Exact attachments retain their
required process: exit, input unavailability, workspace-route changes or shared zoom hiding
that process close the viewer. Queued input cannot fall back to a sibling. Ordinary viewers
without an exact target retain existing navigation semantics.

The fux CLI exposes five `--target-*` flags, all required together, on `attach --socket`.
No agent/task/provider semantics or networking entered fux. Koh remains an opaque transport.
Zor's `task-attachment-v1` resolver validates the selected task/attempt/session guard under
the journal lock, reads the stored runtime target rather than trusting a client-provided path,
and asks fux for that exact process's live route. It returns the current workspace/stream and
origin without changing shared focus. The typed client allows route changes but rejects
process/origin substitution. Controller binding selection and viewer handoff are still pending.

Verification so far:

- The fux ECS suite passed 83 tests after exact admission/exit guards. A subsequently added
  shared-zoom test also passed once its fixture supplied the required layout generation.
- New real `exact-attachment` xtask scenario passed admission, default/private focus isolation,
  stale PID rejection before hello, target-only input and no sibling fallback after exit.
  It is wired into the ordinary fux CLI integration tests.
- Existing real local attachment, protocol rejection/terminal preservation and detach-drain
  scenarios passed against the rebuilt fux binary.
- Nine typed zor client tests passed, including moved-route acceptance and changed-PID refusal.
- Both crates passed strict all-target clippy before the final workspace-consistency guard.
- The harness was rebuilt with Betamax enabled; the socket-level scenario itself is protocol
  evidence, not a rendered UI acceptance test.

The first refreshed composition run timed out during koh helper startup before reaching the
new resolver. An isolated disposable-key probe measured approximately 1.80 seconds for key
unlock and 1.82 seconds through gateway readiness against the existing four-second startup
budget. This identifies the normal cost, not the exact cause of the earlier timeout. No
startup/RPC/test deadlines were increased. The identical composition is being rerun separately
from compiler activity; its terminal result is recorded below.

Remaining requirements are unchanged: integrate authoritative selection and actions into the
dashboard, use per-workspace attachment bindings for the suspend/viewer/return flow, implement
resume/restart/reload/notification policy, add the required failure/replay scenarios and Betamax
review, publish only with authorization, advance the clean companion pin, run CI and the full
completion gate. This checkpoint is not complete multi-machine product acceptance.

Final checkpoint-5 checks passed: fux library 193, local-ipc library 13, zor library 177
(two existing optional probes ignored), and fux ECS 84. Strict all-target fux/zor clippy and
formatting passed. The rebuilt Betamax-enabled harness's real exact-attachment, ordinary local
attachment, protocol rejection and detach-drain scenarios passed.

The composition rerun passed without deadline changes. After that pass, the disposable Python
fixtures were restricted to a small explicit environment so inherited routing overrides cannot
redirect their control commands. The final rerun with the rebuilt binaries and restricted
environment also passed all assertions, including guarded live attachment route resolution,
two distinct remote task/process identities, managed actions/reconciliation, navigation,
terminal restoration, authorization and cleanup. The earlier runs had no FUX_SOCKET override;
no existing user service was targeted. This final pass remains loopback development evidence,
not published-pin CI or a complete dashboard-to-viewer handoff.

Checkpoint-5 retains raw PTY evidence, fixture source, latency probe measurements, logs and
binary hashes. Visual Betamax review of the product flow is still pending. No commits, pushes
or pin changes occurred.

## Sixth implementation checkpoint

The aggregate dashboard now keeps task attempt/session and exact process identity in its
selection. It starts bounded background preparation for inspection, results, guarded
cancel/stop/reconciliation and attachment. Navigation remains available while preparation
runs. A completed operation is attributed to its original machine/task; cancellation does
not claim to retract an operation that has entered dispatch. A cancelled preparation cannot
open a late viewer or reopen a dismissed inspection overlay.

Attachment resolves the live route through the selected zor service, requires that workspace's
explicit attachment binding, then resolves again after starting the owned attachment proxy.
The controller suspends its dashboard terminal guard and starts the exact fux viewer in the
existing foreground terminal process group. Cleanup owns only the direct viewer PID; it must
not signal the controller's shared group. On return, Zor restores terminal attributes, clears
mouse/paste reporting and synchronized output left by a killed viewer, discards input tails,
and redraws the retained selection. Standalone observed-pane attachment is still pending.

The new real-process fixture uses Local plus two isolated remote stacks, same-named adopted
tasks in a non-default `agent` workspace, distinct real koh control/attachment services and a
controlling PTY. It verifies inspection, exact viewer input, isolation from the other remote,
detach with a trailing key suffix, return to the same task, missing-binding refusal, a second
attachment followed by SIGKILL of the actual viewer, cancelled preparation, unchanged input
counters, restored terminal settings, owned-helper cleanup and surviving remote services.
A fixture session leader captures restored attributes before exiting because macOS revokes
its controlling slave on session-leader exit. Only the driver-managed PENDIN bit is masked.

The longer scenario exposed a sustained polling problem. The initial reader opened three
RPC connections per poll. Koh retains completed sessions for 30 seconds within a 64-session
registry; that polling rate exceeds the registry's window capacity. The retained failure shows
both remote rows turning stale with incomplete-frame errors while Local remains responsive.
`supervision-v1` now returns capabilities, service incarnation, passive observation and task
overview in one bounded response per poll. Task work remains on zor's task worker, and all
parts are validated before publication. Transport limits, retention and polling frequency
were not increased. The longer real-process scenario passes with this contract. The code/rate
analysis explains the capacity pressure; the original gateway run did not retain a dedicated
capacity-error trace, so its exact wire rejection code is not claimed as observed evidence.

Validation before the final status-wording rerun:

- Full zor library suite: 180 passed, two existing optional provider probes ignored.
- Strict all-target zor Clippy passed; both workspace formatting checks passed.
- Foreground process cleanup and selection replacement tests passed. Selection survives route
  movement but rejects changed attempt/session, PID or fux incarnation.
- The typed combined-read test succeeds with one reply and rejects inconsistent incarnation,
  missing capability, invalid location and unexpected fields.
- The native harness builds with Betamax; its default-feature all-target strict Clippy passes.
- The extended real-process fixture passed twice after the combined-read change, including
  actual viewer SIGKILL and cancelled attachment preparation.
- Seven 180-column, 30-row product frames were rendered and individually reviewed: aggregate,
  inspection, attached viewer, return, missing binding, killed viewer and cancelled preparation.
  A first import correctly rejected a checkpoint inside an unfinished synchronized frame.
  The fixture now waits for actual closing bytes rather than synthesizing a completed frame.
- Visual review identified ambiguous cancellation wording. The final change distinguishes
  pre-dispatch failure/cancellation from an unconfirmed mutation outcome; its rerun follows.

Evidence is in `verification/multi-machine/checkpoint-6/`, including raw PTY bytes, failure
logs, successful binary hashes, source provenance, the runnable fixture and Betamax frames.
Fixture setup failures are retained separately: wrong socket/list scope, querying foreground
ownership from the wrong session, premature viewer readiness, an invalid capture field, a
shutdown wait without continued PTY draining and a post-session-exit termios query. The first
shutdown timeout was not sampled; it is not claimed as a proven product defect or root cause.
All leaked disposable fixture owners from that earlier teardown failure were identified by
PID and working directory and terminated; subsequent cleanup assertions pass.

This remains development-checkout loopback evidence. The published koh reference remains
clean at `da712875e4f527b718abe44e9d68f94048e916c7`; it still lacks the development status
extension. No commits, pushes, pin changes or changes to existing user services occurred.
Remaining work includes observed-agent attachment, supported resume, catalog reload,
notifications, scoped interactive CLI routing, actual transport-loss/expiry/lost-mutation
and service-restart product scenarios, narrow-layout review, ordinary CI integration using
the clean published companion, the two-host manual checklist and full final review/gates.

The status-wording rerun then hit the existing cold control-helper startup deadline on two
runs, before any task action was sent. Those failures are retained; no timeout was increased
and no external user processes were changed. Every action had been starting another control
helper and unlocking the same credential despite an already healthy observer connection.
Actions now borrow a lease on that machine's existing owned control helper. Retirement removes
its published lease while in-flight preparations retain only their original helper, preventing
redirection to a replacement. Each observation thread retires its slot before releasing its
own lease, preserving concurrent per-host cleanup. Attachment remains a distinct helper and
explicit grant. The fixture now asserts that inspection leaves exactly the two existing
remote control helpers, with no redundant third helper. Final verification follows below.

Final checkpoint-6 verification passed: 181 zor library tests, two existing optional probes
ignored, strict all-target zor Clippy, formatting and whitespace checks. The additional lease
regression proves that retirement/replacement does not redirect an existing preparation and
that dropping the last lease removes the corresponding helper directory while preserving the
replacement. The final real-process fixture passes with control-connection reuse, viewer
SIGKILL recovery, distinguishable cancellation, unchanged input counters and complete cleanup.
One earlier post-lease run failed its initial one-shot observation deadline; that failure is
retained as `initial-observation-timeout.log`. The successful rerun used unchanged deadlines
and binaries; it does not establish immunity to scheduling/load-related startup failures.

All seven final raw checkpoints were imported into Betamax, replay-checked, rendered and
individually reviewed at 180 columns by 30 rows. `checkpoint-6/betamax/index.html` indexes the
final frames. Machine attribution, identical task names, selected task, exact workspace/pane,
inspection scrolling affordance, actionable binding error, viewer-failure return and the
explicit “preparation cancelled; no task action sent” status are readable. These frames do
not establish narrow-terminal or transport-recovery acceptance. The final run's binary hashes
and current source hashes are retained separately. All verification/fixture processes are
terminal. The full milestone and the remaining requirements listed above are still open.

## Seventh implementation checkpoint

Interactive `zor --machine NAME_OR_ID dashboard` now enters the integrated dashboard at that
saved machine's scope. `--machine local` enters Local explicitly; Tab continues through the
same Local/saved/aggregate scopes and retains selection. Unknown selectors are resolved before
helper startup. Explicit machine dashboards never auto-start Local and reject a task-store
override; a remote scope also rejects a local service-directory override. Unsupported
notification flags fail explicitly rather than being silently ignored. Existing one-shot
machine/view JSON remains unchanged.

Local's default runtime location is now an independent source result. When it cannot be
resolved, Local is unavailable and configured remotes remain usable. An explicitly supplied
relative local service directory is still rejected. This avoids making remote supervision
depend on a configured Local runtime.

Verification passed:

- Three real CLI profile/routing tests, including unknown selector, remote directory misuse,
  nonterminal Local entry and unsupported notification rejection without local service startup.
- Strict all-target zor Clippy, formatting and whitespace checks.
- The full controlling-PTY task workflow starting directly on the first remote machine.
- The same workflow with HOME and XDG_RUNTIME_DIR absent from the controller environment:
  Local is unavailable, both remotes remain live, and exact attachment/return still works.
- The same workflow starting explicitly on Local, then navigating to the remote task.
- Each real run retains the prior target-input isolation, detach suffix, helper reuse,
  missing binding, actual viewer SIGKILL, cancellation and terminal/owner cleanup assertions.
- Three new wide-terminal Betamax frames were rendered, replay-checked and individually
  reviewed: remote initial scope without Local configuration, return to that remote task,
  and explicit Local initial scope.

The reusable checkpoint-6 fixture accepts `--initial-machine`, `--without-local-runtime` and
`--artifacts-dir`. Checkpoint-7 retains successful logs/binary hashes, raw PTY recordings and
`betamax/index.html`. No new library-policy behavior was inferred from the CLI tests; full
final gates, published-pin CI and the remaining milestone requirements are still pending.
All fixture processes exited and cleaned up their disposable owners. This checkpoint adds no
commits, pushes, companion pin changes, live deployments or universal parity claims.


## Eighth checkpoint: observed-agent attachment

Unadopted observed agents now support Inspect and Attach through the same owned viewer
handoff as tasks. Task-only actions are refused before dispatch. Zor admits an observed
attachment only with fresh, identified, problem-free evidence for the exact handle; fux
resolves and enforces the process identity. The service uses its own runtime directory,
rejects caller-supplied path fields, and does not access or create a task record.

Verification:

- `CARGO_TARGET_DIR=/tmp/fux-multi-machine-build cargo +stable test -p zor --lib`:
  183 passed, two existing optional installed-provider probes ignored.
- `cargo +stable clippy -p zor --all-targets -- -D warnings` passed with the same target
  directory; `cargo +stable fmt --all -- --check` and `git diff --check` passed.
- Real controlling-PTY fixture `checkpoint-6/dashboard-handoff.py --initial-machine first
  --observed-agent` passed with Local and two isolated stacks using actual koh connections.
  The same fixture without `--observed-agent` passed as a task-handoff regression check.
- Observed probes verify task-only refusal, exact input isolation, no task creation or input
  to adopted sibling panes, forged PID/path rejection, and rejection after process replacement.
  Both workflows cover detach suffix draining, preserved selection, missing binding refusal,
  killed-viewer terminal recovery, cancelled preparation, helper cleanup and surviving owners.
- Seven observed workflow recordings were imported with `fux-xtask betamax-record`, replayed
  and rendered with `betamax-report`, and individually inspected at 180x30. Scope attribution,
  observation inspection, selected viewer input, return, missing binding, viewer failure and
  cancellation are readable; no residual viewer content appears after return. See
  [the Betamax report](verification/multi-machine/checkpoint-8/observed/betamax/index.html).
- The fixture explicitly uses `--agent codex` on shell panes to control classification.
  This verifies observation routing and identity, not real provider recognition or breadth.
- Two earlier fixture errors are retained: split omitted required `final_retain_ms`, and
  kill acknowledgement was incorrectly assumed to include a value. The fixture now follows
  the actual protocol; neither failure was bypassed by increasing timeouts.

Exact binary hashes and successful command output are retained in checkpoint 8. Koh remains
an unpublished development build; published-pin CI, recovery/expiry/lost-reply scenarios,
catalog reload, notifications, narrow layouts, two-host manual acceptance and full final
verification/review remain required. This checkpoint does not complete the milestone.


## Ninth checkpoint: live machine catalog reload

Uppercase R loads the bounded private catalog on a background thread. Only one reload may
run at a time; task/attachment dispatch is unavailable while loading, and a pending action
must finish or be cancelled before starting reload. Invalid catalogs retain the active set.

Unchanged control bindings keep their observer, helper, freshness and exact selection.
Renames update attribution; attachment-only edits change the next handoff's binding without
restarting control. Changed bindings receive new observers without cached authority. Removed
or replaced observers retire concurrently outside the input loop, with at most 33 retiring
workers in addition to the 33 active-worker limit. Further edits are refused until sufficient
retirement capacity exists. Removing a selected machine returns to All machines while retaining
an unavailable selection; Enter cannot silently target Local or another row. Remote owners
are never stopped by profile reload or removal.

Verification: 184 zor library tests passed (two existing optional provider probes ignored),
strict all-target zor Clippy passed, formatting and diff whitespace checks passed. A focused
test holds an old read open while verifying nonblocking replacement, rename observer reuse,
empty authority after replacement, duplicate-ID rejection and removal. The real controlling-PTY
fixture now accepts `--reload-catalog`, checking rename plus selected-agent inspection, unchanged
helper directories, malformed-catalog retention, independent removal of a control binding,
selected-machine removal, action refusal and helper cleanup while remote services survive.
Both the initial run and final binary rerun passed. The final zor SHA-256 is
`6229db74c54d2890613b113b282aa545306baf383c85c071375498f02b2d4369`.
Three actual 180x30 reload recordings were imported, replayed, rendered and individually
reviewed with Betamax: renamed scope/selection, malformed-catalog error, and removed selection
with action refusal. See [checkpoint 9](verification/multi-machine/checkpoint-9/betamax/index.html)
and its retained result, test, Clippy and source-provenance files. Recovery/expiry, notifications, narrow layouts, two-host manual acceptance,
published-pin CI and full final review remain required.


## Tenth checkpoint: machine-aware attention delivery

The integrated dashboard now accepts the existing `--bell`, `--notify` and optional
`--notification-command` settings. It observes all loaded machines independently of UI scope.
Attention keys include machine/service and row process/attempt identity. Renames and unchanged
transport recovery preserve seen evidence; service incarnation changes are distinct evidence.
Pending stale alerts are discarded even while a delivery process is busy. Both channels share
a five-second cooldown. Desktop text contains only an entry count and navigation instruction,
with no machine names, task titles, prompt content or paths.

The existing bounded notification process adapter is reused. Its failures are displayed in
the dashboard. Before foreground viewer handoff, any active notifier is stopped and reaped so
its deadline cannot be suspended behind the viewer. Delivery resumes after dashboard return.
Notification history is controller-local and starts fresh after controller restart.

Verification so far: 186 library tests and three machine CLI tests passed; all-target zor
Clippy, formatting and diff checks passed. New tests cover same-named rows on separate machines,
rename/reconnect suppression, incarnation replacement, stale pending alerts and profile removal.
The real fixture's `--notifications` mode uses a private recording executable and failed checks
on same-named managed tasks on both remotes to exercise real delivery, privacy and coalescing.
Initial fixture setup omitted required `--cwd`; subsequent waits also failed to drain PTY
output and incorrectly assumed only the two task rows needed attention. Retained observations
showed five existing unknown-pane attention rows plus two new panes and two failed-check tasks;
the nine delivered entries were consistent with that evidence. The fixture now drains output
and derives its expected count from fresh real views, retaining the unchanged-state no-repeat
assertion and original deadlines. A further retry confirmed `dashboard terminal output stalled`: the fixture also needed to
drain PTY output during its setup CLI subprocesses, not only its final assertion wait.
CLI waits now drain output while retaining their 15-second command bounds. No product timeout
was increased. The next run delivered the expected nine entries with no unchanged-state
repeats, then its raw BEL assertion counted two OSC title terminators as audible bells.
The retained bytes prove the distinction; the assertion now removes OSC sequences before
comparing audible bell and desktop coalescing. The final full fixture rerun passed.
Failure logs are retained. The final real-process result and exact binary hashes are retained in
`verification/multi-machine/checkpoint-10/notifications-result.json`. The notification frame
was imported from actual PTY bytes, replayed, rendered and individually reviewed at 180x30
in the checkpoint 10 Betamax report. Failed-check attention is readable and dashboard selection
is preserved; the private recording executable verifies count-only notification content.
Loopback process tests do not establish desktop OS or WAN acceptance; this checkpoint is not completion
of the full multi-machine milestone.


## Eleventh checkpoint: narrow-terminal navigation and detail access

The integrated dashboard now uses compact rows below 101 columns, keeps help/quit visible,
and reserves space for wrapped action/connection errors. `?` opens the complete controls
reference. Inspection and help text wrap with word boundaries where possible, splitting long
unbroken values when necessary; scrolling counts the rendered lines so their tails remain
reachable. Escape returns to the existing selection.

187 library tests passed, including readable narrow controls/errors and access to the tail
of a long detail value. All-target zor Clippy passed. The real fixture accepts `--columns`
(40/80/180) and `--rows` (16/24/30), normalizes terminal wrapping for text assertions, and
checks help/return in narrow runs. An earlier fixture waited for the wide `first:live` status
instead of the new compact live-machine count; its log is retained. Initial narrow unit
checks exposed a split quit hint, fixed by separating the controls into two lines below
60 columns and preferring word boundaries. A later 80-column run reached the real viewer but the fixture normalized away the raw
synchronization marker it was asserting. Raw escape-sequence assertions now bypass text
normalization; that failure is retained. The corrected 80x24 real workflow passed. Its eight actual PTY recordings were imported,
replayed, rendered and individually reviewed with Betamax; inspection, exact viewer input,
return, binding error, killed-viewer recovery, cancellation and help are readable.
The report is `verification/multi-machine/checkpoint-11/betamax80/index.html`. The 40x16
workflow passed attachment, return, missing binding, killed-viewer recovery and cancellation,
then the fixture waited for a repaint after a no-op help scroll: all wrapped help lines
already fit. The assertion now checks the last visible wrapped line directly. Its final
full rerun passed. Eight actual 40x16 recordings were imported, replayed, rendered and
individually reviewed in `verification/multi-machine/checkpoint-11/betamax40/index.html`.
The controls, wrapped binding/failure/cancellation messages and help remain readable; row
labels truncate at this width, while inspection and selected-row detail provide additional
context. Both successful size runs retain exact binary hashes, result files and source
provenance in checkpoint 11. Recovery/expiry/lost-reply scenarios, two-host
manual acceptance, published-pin CI and final full review remain required.


## Twelfth checkpoint: attachment transport supervision and fault injection

The controller now polls its owned attachment gateway during foreground viewing. Expiry,
authorization/rejection and transport failure return to supervision with an actionable error;
no replacement attachment is started. A real regression found that koh can emit session-ended
on ordinary detach, so that state remains governed by the viewer exit result. The regression
log is retained. 188 library tests and all-target zor Clippy passed after the correction.

A test-only, explicitly ignored koh gateway fixture accepts commands to close real QUIC links
or refuse redials for 31 seconds, preserving the production session registry and 30-second
retention clock. It authenticates before local service access, bounds active connections and
its lifetime, and is absent from production builds. The development checkout's gateway/CLI
all-target Clippy and fixture build passed. The complete development patch, base and checksum
are retained in checkpoint 12; the published reference remains unchanged.

The real Local-plus-two-remotes fixture now has `--transport-faults` and requires the explicit
`KOH_FAULT_SERVER_BIN` test executable. It tests three real link closures with shell-effect
counts, actual retention expiry, controller return, explicit fresh attachment and unchanged
old input effects. An initial fixture socket-selection error was corrected and retained.
The corrected run passed three real QUIC resumptions, actual expiry, dashboard return and
explicit fresh attachment: retained evidence records three shell effects, input sequence 4
before and after expiry, and sequence 5 after fresh input. It then exposed gateway failure
masking the real viewer SIGKILL exit in the later regression checks. The controller now checks
child exit first, consulting the gateway report on clean EOF to distinguish known expiry.
The complete regression/fault workflow then passed on zor SHA-256
`9f2bd77bc22faddb0d99c2941c37397b3d65d3cf77a96d7ffbc3cc9702b78ba1`.
The result, exact companion/test-binary hashes, input-effect evidence and source provenance
are retained in checkpoint 12. Two actual PTY frames (expiry return and fresh attachment)
were imported, replayed, rendered and individually reviewed in its Betamax report. The
expiry message is actionable; the reattached pane retains the original shell/history.
The full development patch was verified with `git apply --check` against an archive of its
recorded published base. This proves loopback transport composition, not WAN acceptance or
application restart/reconciliation.


## Two-host manual procedure

`docs/multi-machine-manual-acceptance.md` now provides native build commands, isolated runtime
setup on Local/A/B, noninteractive key setup, independent control/attachment grants, routable
address configuration, same-name task navigation and exact attachment, profile edits,
authorization/failure checks, task cancellation isolation and cleanup. It links the deterministic
fault test and explicitly identifies unverified physical-network/application recovery checks.
All 14 Bash command blocks passed `bash -n`; fux list envelope extraction and current CLI flags
were checked against the actual binaries. No physical hosts were deployed or claimed tested.


## Thirteenth checkpoint: remote zor restart acceptance

The real fixture now supports `--restart-zor`: it stops only the first remote zor service,
waits for stale evidence, restarts against the same retained task store while fux remains alive,
checks the new service incarnation and rejection of the old selected row, then explicitly
reselects and checks that pane input did not increase. The first run reached the incarnation
and stale-selection checks, then hit a fixture parser error on nullable observation-row task
metadata. The corrected full run passed, including explicit reselection, unchanged input counters,
terminal restoration, helper cleanup and surviving remote owners. The successful result and
new service view are retained in checkpoint 13. Two actual PTY frames were imported, replayed,
rendered and individually reviewed: first-machine stale evidence while Local/second stay live,
and a refreshed service refusing the old selection. This is remote-zor restart coverage;
controller and fux restart scenarios remain separate requirements.

Follow-up resume audit: `task resume` already exists for eligible retained OpenCode sessions,
with a stable caller operation ID and an explicit fux incarnation. It is distinct from native
Codex recreation and from `launch-reconcile`. Multi-machine dispatch currently omits this
operation. Extending it requires the same locked Expected task/attempt/process guard used by
other remote mutations, retention-aware operation identity, capability advertisement and
provider-specific acceptance. Do not substitute transport reconnect for this application policy.


## Fourteenth checkpoint: remote fux replacement

The real `--restart-fux` fixture passed: only first-host fux was restarted; the replacement
reused the old pane number but had a different server incarnation and PID. The old task's
Attach action was refused and the replacement's input counter remained zero. The task record
remains historical evidence. Normal handoff, terminal restoration, helper cleanup and surviving
remaining owners also passed. The result and full old/new pane evidence are retained in
checkpoint 14. Its actual failure frame was imported, replayed, rendered and individually
reviewed. It exposed the generic `task-failed` message, which the next client change improves.

## Fifteenth checkpoint: guarded remote resume (in progress)

The remote CLI now routes the existing OpenCode `task resume ID --operation OP --instance FUX`
through `task-resume-v1` and a typed single-request client. The service validates Expected
under the task-store lock before consulting retained operation identity or creating resume
intent. Existing provider/session eligibility, explicit operation IDs and no-prompt-replay
policy remain in the task owner. This is separate from Codex recreation and transport resume.

Service failures now retain their bounded explanation, sanitize terminal controls, and reject
malformed/oversized messages and invalid error codes. 189 library tests passed, including
stale resume guards with byte-identical retained journals and refusal-message validation;
all-target zor Clippy passed. A real remote adopted-task resume was refused with its managed-task
eligibility explanation and unchanged inspection evidence. Its first cleanup hit a fixture
variable collision with saved termios; the corrected full rerun passed, including terminal restoration, helper cleanup and remote
owner preservation. Exact binary/result evidence is retained in checkpoint 15. Successful native
resume composition, retained-operation retry/lost-reply tests and dashboard controls remain
required before marking application resume complete.

## Sixteenth checkpoint: successful guarded service resume

The existing `zor-resume` real-process harness now submits its successful resume through
the guarded zor service request. It verifies a new attempt with no input replay, unchanged
archived evidence and unsent prompt, stale pre-resume selection refusal with byte-identical
journal, and explicit reconciliation using the refreshed selection and same operation ID
without another creation. The existing direct CLI retained-operation check still passes.
The service child is owned by the fixture and reaped before its runtime is removed.

The scenario, harness all-target Clippy with warnings denied, formatting and diff checks
passed. Commands, exact binary/source hashes and logs are retained in checkpoint 16.
A separate review of the fixture diff checked dispatch count, guards, assertions and cleanup.
This exercises the shared successful service path with the existing synthetic OpenCode
adapter and exit-before-pin fault. It does not yet verify a successful remote CLI request
through koh, a live resumed provider, lost replies or dashboard resume controls. Those
requirements remain open, alongside the rest of the milestone acceptance contract.

## Seventeenth checkpoint: successful remote resume composition

`fux-xtask scenario zor-remote-resume FUX ZOR KOH` now runs the resume fixture with
an isolated controller and real koh loopback gateway. The controller creates disposable
credentials and a saved remote machine, then invokes the public remote CLI. Successful
resume therefore traverses capability discovery, typed inspection and guarded mutation.
The original scenario's exact attempt, archived evidence, zero input replay and stale
guard assertions remain. A second explicit remote invocation with the same operation
returns the retained result with no extra pane creation or journal change. The fixture
also asserts that the controller never creates a local task journal. Commands are bounded
and mutations are never automatically replayed; the gateway is reaped before its root.

The real remote scenario passed against the development koh binary, as did harness
all-target Clippy, formatting and diff checks. Checkpoint 17 retains commands, logs and
exact source/binary hashes. A separate source review checked controller isolation,
single dispatch, process/resource lifetime and retained-operation assertions. This is
synthetic OpenCode orchestration with the exit-before-pin fault, not actual provider
session restoration. Dashboard resume, lost replies, controller restart, complete CI
integration and the full remaining acceptance contract still require work. The clean
published companion pin has not changed.

## Eighteenth checkpoint: controller process restart

The real Local-plus-two-remotes fixture now supports `--restart-controller`. It closes
the first dashboard normally, checks restored termios and complete helper cleanup, and
starts a fresh controller process under a fresh controlling PTY using the saved catalog.
Both remote machine IDs and service incarnations remain unchanged, no pane input counter
increases, and remote owners remain alive. The entire existing inspection/attachment,
detach, missing binding, killed viewer, cancellation and final cleanup workflow then passes
on the new controller. This is graceful controller restart; abrupt termination and restart
during an ambiguous mutation are not covered by this branch.

Checkpoint 18 retains the passing result, exact binary and fixture hashes, before/after
input evidence and refreshed machine observations. Its actual 180x30 restored dashboard
frame was imported into Betamax, replayed, rendered and inspected: all three machines are
live, same-name tasks retain machine attribution, and controls and state are readable.
Python compilation and diff checks passed. Dashboard resume, lost-reply handling and the
remaining full acceptance and CI requirements are still open.

## Nineteenth checkpoint: dashboard resume controls

The multi-machine dashboard now exposes `u` in its controls/help. Its bounded text form
requires a stable operation ID and explicit fux incarnation. Enter submits one guarded
request through the existing asynchronous action worker; Escape cancels without dispatch.
Selection freshness, service incarnation and task/attempt/process guards remain enforced.
The operation and fux identities accompany failure messages. No replacement runtime is
chosen automatically. The service retains provider eligibility and application policy.

The real Local-plus-two-remotes fixture verifies form cancellation and adopted-task refusal
with unchanged journals, followed by the complete handoff regression. Its first run exposed
wide-screen truncation hiding the actual refusal; wide errors now wrap with reserved space,
and the corrected full run passed. The form also has its own input header instead of
misleading detail-scroll controls. 190 library tests passed (two existing ignored), strict
all-target Clippy and formatting passed. Checkpoint 19 retains exact binary/source evidence
and two actual PTY frames imported, replayed, rendered and individually reviewed.

Successful dashboard resume, narrow form acceptance and durable controller-side tracking
of unknown mutation outcomes remain required. The explicit operation ID is currently
user supplied and visible, not a claim that lost-reply recovery is complete. The successful
remote CLI orchestration evidence remains in checkpoint 17.

## Twentieth checkpoint: narrow resume and full error inspection

At 40x16 the initial resume refusal was hidden by the bounded dashboard footer. `e` now
opens the complete current action status/error in the existing scrollable detail view;
help and narrow status screens advertise the control. Resume input is above explanatory
text, and long input retains its editing tail. A targeted test covers the maximum-length
input tail at narrow size. The fixture captures entered IDs before submitting and exercises
full error inspection and return. Its help check now scrolls to the final line instead of
assuming the growing help fits on one screen.

The complete 40x16 handoff/resume-refusal workflow passed, along with all four dashboard
tests, strict all-target Clippy, formatting, Python compilation and diff checks. Three
actual PTY frames (form, entered IDs and refusal detail) were imported, replayed, rendered
and individually inspected; controls, input and full reason are readable. Checkpoint 20
retains passing results, exact source/binary evidence and earlier failures.

**Open recovery defect:** an earlier run reached the later detach regression and reported
`attachment transport unavailable` following a clean viewer exit. Subsequent runs passed
detach, but the cause is not established. Do not dismiss this as resolved by rerunning or
claim the entire recovery requirement complete. Inspect generic gateway status correlation
and explicit viewer-exit evidence before changing transport failure classification.

## Twenty-first checkpoint: explicit viewer detach evidence

Fux now supports `attach --report-exit`: a bounded generic JSON stderr report distinguishes
a local detach request followed by normal server exit from other viewer termination.
The existing attach API still returns its optional process code; the reported API preserves
the extra evidence. No remote/task/provider policy enters fux. Zor requests the report,
drains diagnostics after child exit, and requires both a successful exit and the exact final
detach report before ignoring a racing transport failure. It permits a bounded 200ms report
drain after terminal transport failure; there is no reconnect or input replay in this window.

Source inspection found that a local socket close can race koh's final write, producing a
session failure; the original intermittent failure did not retain sufficient tracing to prove
that exact cause. The new evidence removes the need to infer detach from transport status.
The first full fault run exposed expiry surfacing as viewer EOF before status publication.
The corrected path briefly checks expiry after a non-signal viewer failure while preserving
SIGKILL as a viewer failure. The complete fault workflow then passed: three actual reconnects,
retention expiry, explicit fresh attachment, ordinary detach, killed viewer, cancelled
preparation, terminal restoration and owned-helper cleanup.

193 fux and 192 zor library tests passed (two existing zor ignores), final strict all-target
Clippy passed, and the affected handoff tests passed after the ordering correction. The final
fault run and exact binary/source evidence are retained in checkpoint 21, along with its
initial failure. Two actual PTY frames for expiry and confirmed detach were imported into
Betamax, replayed, rendered and individually inspected. The messages remain distinct and
actionable. This addresses detach classification with explicit evidence; it does not close
the outstanding application-resume, unknown-mutation recovery or full CI acceptance gaps.

## Twenty-second checkpoint: read-only resume operation evidence

`task resume-status TASK --operation OP` is now available locally and remotely through
`task-resume-status-v1`. The owner reads the retained launch phase, explicit fux incarnation,
previous attempt, session and pane under the journal lock without submitting any launch.
It rejects cross-task operation identity and ambiguous retained matches. The typed client
validates response identity and record fields and uses the bounded read retry policy.
An absent record is evidence absence, never authorization to replay a request.

The real remote-resume scenario now checks retained status through the service and remote
CLI, an absent operation, and byte-identical journal plus unchanged creation count after
reads. It passed through the real koh gateway with the synthetic OpenCode fixture. The 192
zor library tests passed (two existing ignored), and both zor and harness strict all-target
Clippy passed. Checkpoint 22 retains exact source/binary hashes, commands and logs. Separate
source review checked that the new path contains no transaction, process creation or resume
submission and that remote selection remains ahead of local-store routing.

This supplies a read-only reconciliation primitive. Durable controller intent tracking,
deliberately lost mutation replies and successful dashboard resume remain open requirements;
the milestone is not complete.

## Twenty-third checkpoint: completed remote resume reply lost

`fux-xtask scenario zor-remote-resume-lost-reply FUX ZOR KOH` now places a bounded,
test-only local reply gate behind the real koh gateway. The gate forwards the request to
the real zor service, verifies a successful response, then closes that fixture connection
without delivering the first resume reply. The caller must report an unconfirmed outcome.
The gate counts exactly one mutation before recovery through `resume-status` and task
inspection. Those reads leave the committed journal and pane creation count unchanged.
A later explicit retry of the same retained operation is also checked to avoid a new pane.

The scenario and strict harness Clippy passed; formatting and diff checks passed. The first
run correctly encountered koh's unsafe-socket rejection because the test listener inherited
default permissions. The corrected gate explicitly uses mode 0600 under its private fixture
directory. Read/write deadlines, frame bounds, owned thread shutdown and surfaced gate errors
were reviewed separately. Checkpoint 23 retains the failure, successful run, commands and
exact binary/source hashes.

This proves a lost completed service reply through real koh forwarding with synthetic
OpenCode orchestration. The fault closes the service stream; it is not a QUIC link reset at
the mutation boundary. Durable controller intent tracking, dashboard recovery and successful
dashboard resume acceptance remain unfinished, as do the remaining integration/CI gates.

## Twenty-fourth checkpoint: durable remote CLI resume intents

Remote CLI resume now records its machine ID, endpoint, service incarnation, stable operation,
requested fux incarnation and original Expected selection before dispatch. The separate private
catalog-sidecar log uses a nonblocking lock, bounded parsing, atomic replacement and file plus
directory synchronization. Any storage failure prevents dispatch. Existing operation identity
cannot be redirected to another task, endpoint or fux incarnation; a deliberate retry preserves
the original pre-dispatch selection. `zor machine resume-intents` reads these records without
contacting the remote service. Records remain intent evidence, not proof of dispatch/completion.

Two storage tests passed: reopen/retained-selection behavior with changed-intent refusal, and
malformed/symlinked log refusal without replacing the referenced bytes. The real lost-reply
scenario now launches a fresh listing command after the submitting CLI exits and verifies the
saved original attempt before read-only remote recovery. It passed with one created pane and
the existing explicit-retry assertions. Zor and harness strict all-target Clippy, formatting
and diff checks passed; evidence and exact source/binary hashes are in checkpoint 24.

The additional run exposed opaque timeouts in the test reply gate's serial five-second I/O
path. The gate now owns up to 32 bounded connection handlers, treats empty abandoned requests
as closure, uses the existing bounded service caller with a ten-second reply budget, surfaces
handler errors and joins all owned handlers. The passing result follows those fixture fixes;
no production mutation retry was added.

The log currently retains 256 records and fails closed at capacity. Dashboard intent tracking,
record archival controls, other mutation kinds, dashboard recovery and successful dashboard
resume acceptance remain unfinished. This checkpoint does not close the full milestone.

## Twenty-fifth checkpoint: dashboard pre-dispatch intent recording

Dashboard resume now writes the same durable intent in its asynchronous action worker after
fresh task/service checks and before mutation dispatch. A second cancellation check follows
the write. Storage failures prevent dispatch. Remote sources record their koh endpoint;
Local sources use a validated absolute local-service socket identity under the reserved
`local` machine ID. The original task/attempt/process selection remains retained.

The real Local-plus-two-remotes fixture checks that cancelling the input form leaves an
empty intent log, submitting an adopted-task resume refusal retains the exact machine/task/
fux identity, and a fresh CLI process reads the identical intent after dashboard exit. The
entire handoff/cleanup regression passed. Three intent-store tests passed, including strict
local route validation, and all-target zor Clippy, formatting, Python compilation and diff
checks passed. Checkpoint 25 retains results, intent evidence and exact source/binary hashes.

Cancellation during preparation can leave an intent even when dispatch was prevented;
the log deliberately makes no completion claim. An integrated recovery view, archival
controls, other mutation kinds and successful dashboard resume remain required, along with
the remaining full milestone verification and CI integration.

## Twenty-sixth checkpoint: dashboard resume-status inspection

Uppercase `U` opens a read-only operation-ID form for the current task. Its background
action reads retained remote resume evidence and displays it alongside the saved controller
intent. It rejects a saved intent for another task/control endpoint and mismatched fux intent.
Service/selection freshness checks still apply. The result explicitly states that absent
evidence does not authorize replay; no resume mutation or intent-log write occurs. The existing
scrollable detail view keeps both evidence sections accessible at narrow terminal sizes.

The full 40x16 dashboard workflow passed, including status inspection of the refused operation,
unchanged task journal and saved intents, return to supervision and final cleanup. Five
dashboard tests and strict all-target zor Clippy passed. Formatting and diff checks passed.
Checkpoint 26 retains exact source/binary evidence and an actual PTY status frame imported,
replayed, rendered and visually inspected. The read-only notice and remote absence are readable;
the saved intent follows in the scrollable view.

Offline intent inspection remains available through `machine resume-intents`; this dashboard
action requires a fresh task/service selection. Archival controls, other mutation kinds,
successful dashboard resume and remaining full acceptance/CI gates are still unfinished.

## Continuation audit (2026-09-13): acceptance matrix

The tree at this continuation builds with all targets and the zor library suite passes
(192 tests, two existing optional probes ignored). Verified against source, not the
checkpoint notes, before choosing where to resume:

| Requirement | Current code | Retained evidence | Status after checkpoint 27 |
|---|---|---|---|
| Catalog and CLI (§2) | `machines/catalog.rs`, `machine add/list/inspect/rename/remove/control/bind` | checkpoints 1, 3, 7, 9, 27 | Verified; profile lifecycle now inside the CI `zor-multi-machine` scenario |
| Typed client and capabilities (§3) | `service/client.rs`, `supervision-v1`, `task-read/supervise/attachment/resume*-v1` | checkpoints 1, 4–6, 15, 22, 27 | Verified in ordinary CI |
| Owned koh helper (§1, §3) | `machines/connection.rs` | checkpoints 2, 6, 27 | Verified against the clean published pin; probes `--status-file`, degrades explicitly without it |
| Aggregate/machine-scoped dashboard (§4) | `dashboard/multi.rs`, `machines/supervision.rs` | checkpoints 3, 6, 7, 9–11, 27 | Verified; CI scenario plus reviewed wide/narrow frames |
| Exact attachment and return (§5) | `dashboard/handoff.rs`, fux `--target-*`, `--report-exit` | checkpoints 5–8, 12, 21, 27 | Verified in CI on the published pin |
| Disconnect/restart taxonomy (§6) | handoff transport classification, incarnation guards, intents | checkpoints 12–14, 18, 21, 23–26 | Verified; distinct transport states need the development koh, degradation documented; lost-reply is retained dev-koh evidence |
| Real-process composition in ordinary CI (§7) | `zor-multi-machine`, `zor-remote-resume`, `zor-remote-resume-dashboard` xtask scenarios, `ci.yml` | checkpoint 27 | **Done**: three scenarios in the pinned-koh CI job with no skip |
| Successful dashboard resume | `u` control, worker dispatch, intents | checkpoints 19, 20, 25, 26, 27 | Verified: `zor-remote-resume-dashboard` drives a real guarded resume through koh |
| Docs (§8) | user guide, manual checklist, this ledger | checkpoint 27 | Guide updated for published-pin behavior, CI scenarios and keybindings |
| Full gate and separate review | — | checkpoint 27 | fmt, workspace clippy, zor clippy matrix, zor lib/cli tests, structure test passed; separate review pass pending |

## Twenty-seventh checkpoint: published-pin composition in ordinary CI

This continuation finished the composition-in-CI gap without a koh publication step. The
owned control helper (`machines::connection`) no longer hard-requires the development koh
`--status-file` flag: it probes `koh gateway connect --help` once per executable, passes
`--status-file` only when advertised, and otherwise treats koh's own socket-announcement line
as readiness. Against the clean published pin a failed connection is reported as one generic
transport failure carrying koh's diagnostic, never an inferred authorization verdict; the
detach/viewer-failure and remote-read messages state this degradation explicitly. With the
development koh the distinct unauthorized/expired/ended/offline states are still consumed.
A unit test covers the cached probe and the socket-announcement guard.

A new real-process scenario `zor-xtask scenario zor-multi-machine FUX ZOR KOH` runs Local plus
two isolated remote stacks through real koh gateways under a controlling PTY: saved profile
lifecycle (list/inspect/rename/duplicate/Local-alias refusal), an aggregate view with distinct
same-named tasks and fux incarnations, an unauthorized control profile and an unreachable
profile that both fail independently without falling back to Local, exact attachment to a
non-default `agent` workspace pane, target-only input, detach-suffix isolation, return to the
same selection, a missing attachment binding, a live-reloaded unauthorized attachment binding
that fails while control stays connected, viewer SIGKILL recovery with reporting-mode reset,
cancelled preparation, profile removal, terminal-attribute restoration, per-endpoint owned
helper accounting, and remote owner/task survival after controller exit. Helper accounting is
by koh `gateway connect` process arguments, not a shared `/tmp` glob, so parallel runs cannot
cross-attribute helpers.

The dashboard resume control now has a real-process success path: `zor-remote-resume-dashboard`
drives the interactive `u` form through the public remote CLI and a real koh gateway, dispatches
one guarded resume, shows the resumed-task detail (finished attempt, closed launch), records
one durable pre-dispatch intent, and confirms the service inspection. `zor-producers::resume`
gained a `dashboard` mode for this; `zor-remote-resume` (guarded CLI success) and
`zor-remote-resume-lost-reply` (unknown outcome) are unchanged.

CI: the `Pinned koh composition` job now builds the published koh executable and the harness,
then runs `zor-multi-machine`, `zor-remote-resume` and `zor-remote-resume-dashboard` against
`target/debug/fux`, `target/debug/zor` and `references/koh/target/debug/koh` with no skip. The
structure test pins these three commands. `zor-remote-resume-lost-reply` measured 3 of 4 passes
on the published pin (the test-only reply gate is timing-sensitive on that transport); it stays
retained development-koh evidence (checkpoints 23–24), out of the always-green gate.

Verification (this checkpoint, `cargo +stable`, `CARGO_TARGET_DIR=/tmp/fux-multi-machine-build`):

- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`,
  and `cargo clippy -p zor --all-targets --no-default-features --features cli -- -D warnings`
  all passed.
- `cargo test -p zor --lib` passed (195 tests incl. the new probe/announcement test; two
  existing optional provider probes ignored); `cargo test -p zor --no-default-features
  --features cli` and `cargo test -p fux --test structure` passed.
- The three CI scenarios passed against the clean published koh pin with the final binaries
  (SHA-256 in `checkpoint-27/source-provenance.json`); the multi-machine and dashboard-resume
  runs were rendered with Betamax and their frames inspected at 180x30 (aggregate with an
  unauthorized and an unreachable host, exact viewer input, return, missing binding, killed
  viewer, cancelled preparation; resume form and resumed-task detail).
- The koh development patch was regenerated in `checkpoint-27/koh-development.patch`; it applies
  cleanly (`git apply --check`) against the clean published reference `da712875`, which remains
  unmodified. Publishing that status extension (for distinct transport states) is the one step
  still requiring authorization.

Evidence is in `verification/multi-machine/checkpoint-27/`: scenario logs, Betamax frames and
index, binary/source provenance and the koh patch with base and checksum. This does not
establish physical-host/WAN acceptance or the lost-reply gate on the published pin.

## Prompt execution trace

`verification/multi-machine/checkpoint-27/prompt-execution-trace.md` maps every requirement of
`multi-machine-navigation-and-supervision-prompt.md` (sections 1-8 and the twelve section-7
demonstrations) to its implementing code and automated verification, records the four fresh
sequential acceptance runs (reload, controller/zor/fux restart) against the published koh pin,
and states the externally blocked items (transport-loss/expiry on dev koh, lost-reply gate,
physical-host/WAN). It is the execution record for that prompt.
