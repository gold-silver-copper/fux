# Pane/layout implementation and acceptance ledger

Objective: execute `pane-layout-controls-prompt.md` completely. Implementation and automated
verification are ready; overall completion is blocked on the required manual visual acceptance.
Pane/layout parity remains unproven until that gate passes.

## Current acceptance audit

**Betamax follow-up (2026-09-12):** headless raster verification is now available.
The full viewer scenario passed; the final Betamax gallery contains 241 visually
reviewed checkpoints across fux and zor terminal scenarios;
the review found and fixed inherited status-bar backgrounds on full redraws.
See [the verification report](betamax-verification-2026-09-12.md) for evidence and
the four capture-independent zor protocol-fixture failures in the complete run.
This adds headless visual evidence; it does not claim a native-terminal manual pass
or complete cross-product parity.

This section is the current status; dated implementation checkpoints below retain their historical
limitations and do not override it. The full prompt remains the acceptance target.

| Requirement | Current evidence | Remaining acceptance |
| --- | --- | --- |
| 1. Split/close/focus/cycle/last | Creation and focus ECS regressions; 264-test macOS selection; real viewer nested/post-close/zoomed/single-pane cycles and mouse close workflows | Manual walkthrough |
| 2. Resize and initial ratio/focus options | Layout property tests, split/transfer ratio and focus matrices, actual-dimension resize benchmark preflight, tiny viewer recovery | Final performance record and manual interaction |
| 3. Zoom preserves layout | Shared-zoom ECS tests, remapped import tests and real viewer tiny/zoomed navigation | Manual visual inspection |
| 4. Swap versus relocation | Separate layout operations; generation/identity tests; keyboard, explicit picker and drag scenarios | Manual interaction |
| 5. Live transfers and ordering | ECS cross-container, focus/admission and receipt tests; real zor/native/run route scenarios; mouse-only reorder tests | Manual simultaneous-viewer workflow |
| 6. Mouse gestures and forwarding | Controller capture/stale-target tests, real border/pane/tab/workspace drag and mouse ownership scenarios | Visual feedback inspection in a real terminal |
| 7. Existing-pane export/apply | Bounded tree format, bijective mapping, atomic archive/order/labels/focus/zoom tests and real CLI round trips; no process spawning | Manual export/apply walkthrough |
| 8. Discoverability and availability | Single registry, exhaustive binding/dispatch structure, unavailable-action tests, corrected zoomed focus gate and mouse confirmation paths; full independent review | Final visual disabled-state walkthrough |
| 9. Labels and contextual actions | Shared/manual-title distinction, lifetime guards, context-menu tests and real rename/close/reorder workflows | Manual menu walkthrough |
| 10. Read-only geometry/neighbor/edges | Coherent query ECS tests and real CLI checks, including hidden/zero-area behavior | No separate code gap identified |
| 11. Right-click policy | Policy/reporting/modifier matrix, metadata/history tests and real viewer keyboard/CLI/menu checks | Manual application-forwarding walkthrough |
| Identity, routing, concurrency, bounded imports | Relevant ECS and real-process evidence plus completed full independent review; both confirmed P2 findings fixed and re-reviewed | Final verification toolchain coverage below |
| Reference independence and attribution | Final reference-free build/package/241-core-test/viewer run; exact matching product/harness hashes; no copied hypertile code or dependency | No separate code gap identified |
| Required verification | Nightly/reference-free checks pass; Rust 1.95 all-target compilation passes; stable 1.97.1 formatting, Clippy variants, 264 fux tests, 13 local-ipc tests, docs and standalone-tool checks pass | Native Linux/Windows execution is not claimed; no pending local check |
| Performance | Final-binary five paired repetitions/30 commands passed; source/binary hashes retained; traffic cost attributed and prior timing increases investigated by repeat | Timing conclusions remain limited by shared-host contention; no general speed claim |
| Manual visual gate | Automated terminal-model scenarios pass; CUA has no enabled surfaces and native System Events automation returned -1743 | Requires accessible real-terminal UI; not passed |

The concrete pinned-Herdr workflow matrix below retains platform/scope exceptions and input
entry points. No whole-product Herdr supersedence or completed pane/layout parity is claimed.

Final performance samples are saved in
[`pane-layout-performance-final-2026-09-12.json`](verification/pane-layout-performance-final-2026-09-12.json).
All 30 final-binary commands passed. Repeated comparisons did not reproduce the earlier
two-viewer CPU/32-pane p95 increases; timing remains observational under shared-host load.
The three-byte steady-output and 77-byte resize overheads are accounted for by layout revision,
zoom and destination metadata. The [performance report](pane-layout-performance-2026-09-12.md)
contains medians and the field-size calculation without claiming universal speed superiority.
Rust 1.95.0 all-target workspace compilation also passed; its compiler identity and log hash
are in the [MSRV record](verification/pane-layout-msrv-2026-09-12.json). Stable 1.97.1 checks passed
in separate target directories; no benchmark ran concurrently with those compilations. The
[stable check record](verification/pane-layout-stable-checks-2026-09-12.json) and
[standalone-tool record](verification/pane-layout-stable-tools-2026-09-12.json) retain commands,
results, environment and log hashes. The fixture tests passed 13 with their documented concurrent
startup exclusion; the fux selection passed 264 with its documented headless-agent exclusion.

The remaining manual gate was revalidated after checks finished: CUA again returned
`CUA_REPL_ENABLED_SURFACES is required`. The earlier native access check returned macOS -1743.
This same UI-access condition has persisted across successive goal turns. No accessible terminal
UI is available for inspecting nested layouts, keyboard resize/move, drag feedback, zoom, live
container moves, export/apply, disabled-state feedback and simultaneous viewers. Automated
terminal models do not substitute for that explicitly required observation. The user has been
asked to provide UI access or perform and report the walkthrough. No further source change,
commit, push or PR action is required merely to resolve this access blocker.

Latest implementation closes the remaining full-review P2 mouse workflow gap. Pane/tab/workspace
Close dialogs now display separate warning, Confirm close and Cancel rows. Mouse confirmation
uses the existing guarded action path; warning clicks do nothing, outside clicks cancel, stale
targets cannot execute, and captured releases remain outside the terminal application. Entering
a new mode clears old painted entry regions so an unpainted dialog cannot inherit menu targets.

Passed: 145 library tests; workspace and harness all-target Clippy with warnings denied; the real
viewer scenario including mouse-only pane/tab/workspace cancellation and confirmation; formatting
and whitespace checks. The reviewed-boundary inventory records two generic close-mode checks
and three updated confirmation panels; independent review verified those changes. Final review
reconciled both P2 fixes with the complete-diff review and found no remaining confirmed code issues.
Logs: `/tmp/fux-mouse-close-{tests,lib,viewer,clippy,harness-clippy,boundary}.log`.
The final macOS CI selection passed 264 tests with its one documented headless-agent exclusion.
The [verification record](verification/pane-layout-macos-tests-2026-09-12.json) includes the exact
command, per-suite results and log hash. A fresh 500-file source copy with no references or prior
targets passed workspace build, fux/local-ipc package verification, 241 core tests and the complete
real-terminal viewer scenario on the first attempt. Product and harness source hashes still match
the working tree. Exact commands, results and hashes are in the
[final reference-free record](verification/pane-layout-no-references-final-2026-09-12.json).
The current local-ipc suite also passed all 13 tests (`/tmp/fux-pane-final-local-ipc.log`).
Manual visual acceptance remains unavailable: CUA lacks enabled surfaces, and a native UI
capability check was denied by macOS (`Not authorized to send Apple events to System Events`,
error -1743). No manual visual pass is claimed.

Latest full review inspected the complete baseline diff and relevant untracked implementation
across layout/imports, archives, transfers, zor routing/lifecycle, frame metadata, gestures,
CLI/registry, tests, documentation and attribution. It found no confirmed P0/P1 issues and two
confirmed P2 workflow gaps. Keyboard focus navigation incorrectly shared the visible-split-count
gate with resize/swap. This is fixed: focus actions require an active pane, so zoomed navigation
and single-pane wraparound remain available. A registry regression covers single/zoomed panes;
the real viewer now waits for zoomed rendering before keyboard traversal in both directions.
Passed 144 library and three agent-boundary tests, the real viewer scenario, workspace all-target
Clippy, formatting and whitespace checks. Independent re-review found no issues in this fix.
Logs: `/tmp/fux-navigation-gate-{tests,viewer,clippy}.log`.

The other confirmed P2, keyboard-only Close confirmation after opening a mouse menu, is now
fixed and verified as recorded above. A suspected stale drag-region issue was rejected after tracing
`set_tab_regions` and render sequencing: changed painted ID-to-rectangle mappings already cancel
the gesture before the next event; unchanged-width labels do not retarget it.

The improved resize benchmark and five paired output repetitions all passed (30 commands).
Preflight verifies 200 actual dimension changes per resize configuration, with size/PID restoration.
Stable costs are 77 added bytes per resize and three per median keystroke. CPU and latency remain
inconclusive under changing shared-host load; two-viewer CPU and 32-pane p95 need follow-up.
The measured binary predates the navigation gate fix. Exact hashes, results and limitations are in
the [performance report](pane-layout-performance-2026-09-12.md) and its refreshed raw JSON record.
The current-source reference-free check and final re-review have since passed, and mouse
confirmation is implemented. Final visual acceptance, toolchain coverage and performance follow-up
remain open as summarized in the current audit above.

Latest acceptance evidence: the real viewer scenario now starts an isolated three-pane nested
layout, reduces it to 2×2 and then 1×1, traverses every pane and zooms/unzooms using keyboard
input. It verifies hidden geometry, full-area zoom where a content row exists, distinct echoed
input per tiny pane, unchanged split documents and pane/PID membership, and resumed output after
restoring 24×80. A one-row viewport has no pane-content row. The control guide documents the
deterministic collapse arithmetic, keyboard recovery and shared minimum-viewer sizing. The 2×2
PTY minimum is established by the implementation; this fixture does not directly query the PTY's
window size or prove preservation of historical content.

Passed: `FUX_SCENARIO_DEADLINE_SCALE=3 cargo test --locked -p fux --test local_cli real_viewer_scenarios_cover_the_interaction_contract -- --exact --nocapture`;
`cargo clippy --locked --manifest-path tools/xtask/Cargo.toml --all-targets -- -D warnings`;
harness formatting and whitespace checks. Logs: `/tmp/fux-tiny-{viewer,clippy}.log`.
Independent review identified a stale-screen false positive from repeated input markers; unique
numeric markers fix it. The inspector retries only the specific transient pending-resize conflict.
Re-review found no remaining findings in this bounded addition. Manual visual inspection is still
unavailable: another CUA inventory attempt returned `CUA_REPL_ENABLED_SURFACES is required`.
The source-copy build/package/test refresh passed as recorded below; final performance and full-diff
review remain open.

The refreshed reference-free check copied 498 current source files into a temporary directory with
no reference checkouts, Git metadata or prior targets. `cargo build --workspace --locked` and
`cargo package --locked -p fux -p local-ipc` passed. The fux library/ECS/agent-boundary/protocol/
structure test command passed 239 tests after fixing a stale reviewed-boundary fixture entry for
the previously reviewed generic chooser branch. The initial failed run is retained in the record;
only that fixture was updated in the source copy before rerunning tests. Independent review
confirmed the inventory update introduces no agent or transport policy. Executable/manifest scans
found no hypertile references. Exact commands, results, copied-file hashes and log hashes are in
[`pane-layout-no-references-refreshed-2026-09-12.json`](verification/pane-layout-no-references-refreshed-2026-09-12.json).
This proves the recorded build/package/test scope without the reference directories; optional
koh reference integration and full platform CI are separate gates.

Additional current-worktree checks passed: zor all-target Clippy with all features, no default
features, and no default features plus `cli`; zor all-target no-default-feature check;
workspace no-dependency documentation and zor all-feature no-dependency documentation builds.
Logs: `/tmp/fux-pane-final-zor-{all-clippy,none-clippy,cli-clippy,check,docs}.log` and
`/tmp/fux-pane-final-docs.log`. The standalone agent-boundary suite passed all three tests after
the inventory correction (`/tmp/fux-chooser-boundary-final.log`).

Previous implementation: tab/workspace choosers now handle mouse wheel navigation, entry clicks
and outside-click cancellation. Tab actions return through the control request path; workspace
reordering uses the manager outbox. Captured releases stay out of applications, including when
reconciliation cancels a stale chooser before the click can execute.

Verification passed: 143 library tests, 15 micro-timing/protocol/structure tests, workspace and
harness all-target Clippy with warnings denied, formatting and whitespace checks. The real viewer
scenario passes mouse-only tab/workspace reorder and order restoration, workspace selection, and
keyboard/CLI next/previous cycles through nested, post-close and single-pane layouts. It checks
wraparound, unchanged PIDs and private viewer focus using a later rendered metadata update.
Independent review found a stale-click release leak, fixed with regression coverage; re-review
found no further issues in this bounded change. An initial real-viewer assertion incorrectly
expected an unlabeled status field after adding a synchronization label; the corrected check
accepts both status forms and the scenario passes. Logs:
`/tmp/fux-chooser-mouse-{lib,boundary,clippy,harness-clippy,viewer}.log`.
Commands: `cargo test --locked -p fux --lib`;
`cargo test --locked -p fux --test micro_timing --test protocol_consumers --test structure -- --test-threads=1`;
`cargo clippy --locked --workspace --all-targets -- -D warnings`;
`cargo clippy --locked --manifest-path tools/xtask/Cargo.toml --all-targets -- -D warnings`;
`FUX_SCENARIO_DEADLINE_SCALE=3 cargo test --locked -p fux --test local_cli real_viewer_scenarios_cover_the_interaction_contract -- --exact --nocapture`.
Final manual visual, tiny-terminal usability, performance, reference-free and full-diff acceptance
remain open. This is automated terminal-model evidence, not a final parity claim.

Previous implementation: explicit transfer focus policy. `layout to-tab`, `layout new-tab` and
`transfer-pane` expose mutually exclusive `--focus`/`--no-focus`, default no-focus, backed by
API `focus` fields. No-focus preserves destination zoom and selections except necessary source
fallback. Focus selects the destination workspace default, clears destination zoom, and also
selects an attached layout requester without recording temporary source fallback in Last history.
Manager `follow` implies focus and checks destination viewer capacity before mutation; new-workspace
reservation failures still roll back. Existing viewers retain their private selections. New
workspaces necessarily select their sole pane.

Two ECS regressions cover requester/focus matrices, destination zoom, stale failure atomicity,
Last history, cross-workspace focus/follow combinations, fresh attachment defaults and full
destination rejection. CLI tests reject contradictory flags and verify explicit focus mapping.
Broad verification passed 141 library, 5 CLI/bin, 82 ECS and 14 boundary/protocol/structure tests
(242 total), workspace/harness Clippy, formatting and whitespace checks. The real transfer CLI
scenario verifies no-focus zoom preservation, focused destination selection and retained PID.
Independent bounded review found no confirmed issues. The full real viewer regression also passes,
including existing tab/workspace movement and mouse workflows. This is automated terminal-model
evidence; final manual visual acceptance remains open.
Logs: `/tmp/fux-transfer-focus-{targeted,tests,clippy,harness-clippy,scenario,viewer}.log`.

Previous implementation: existing-tab transfer insertion ratios. `layout TAB to-tab --ratio`
and `transfer-pane --ratio` accept 500–9500, default 5000. The API field belongs to the existing-tab
`destination`. Its value is the existing target's share on a 10000 scale regardless of insertion
side; left/up complement the first-child ratio before swapping placement. Validation occurs before
movement or manager workspace allocation. New-tab destinations reject the option because they do
not create a split. Keyboard/drag controls keep their existing equal-split default.

The ECS regression covers all four directions within and across workspaces, invalid-ratio atomicity,
geometry, retained PID and immutable origin. CLI tests cover valid and invalid bounds and incompatible
new-tab destinations. The real `pane-layout-transfer` scenario passes an existing-workspace CLI move
left with ratio 7000, then a workspace-scoped CLI move down with ratio 6500, verifying documents and
the same live PID. Its existing archive/identity/route checks also pass. Broad verification passed
141 library, 5 CLI/bin, 80 ECS and 14 boundary/protocol/structure tests (240 total). Independent
read-only review found no issues. Workspace/harness all-target Clippy, formatting and whitespace
checks pass; the targeted regression passed again after the validation match style fix. Explicit
transfer focus/no-focus options are now implemented above. Logs: `/tmp/fux-transfer-ratio-{targeted,tests,scenario,clippy,harness-clippy}.log`.

Previous implementation: initial split/new ratio and focus policy. CLI `--ratio 500..9500`
and `--focus`/`--no-focus`, and control `ratio`/`focus`, are carried through the spawn barrier.
The existing pane receives the ratio's share on a 10000 scale; default 5000 retains equal
splitting. Invalid ratios reject before reservation. No-focus preserves viewer/default selections,
shared zoom and queued-input ownership. Focused creation reveals the new pane by clearing zoom,
and selects its tab for the workspace default, including explicit hidden-tab targets. Other
attached viewers retain private selections. Explicit headless dimensions retain precedence.

Two new ECS regressions and CLI parser coverage pass. Broad validation passed 141 library,
5 CLI/bin, 79 ECS and 14 boundary/protocol/structure tests (239 total), workspace/harness Clippy,
formatting and whitespace checks. Independent review found a P2 hidden-tab default-selection
omission; it is fixed and covered by a workspace-control/fresh-attachment test. Re-review found
no remaining issues in this bounded change. The real viewer scenario passes 70/30 CLI geometry,
retained focus/PID, subsequent explicit focus and the existing mouse/layout workflows. Its initial
restore request had an incorrect action encoding; corrected to the documented `operation: apply`
wire shape and rerun successfully. Logs: `/tmp/fux-split-options-{targeted,tests,clippy,harness-clippy,viewer}.log`.

Previous implementation: stored per-pane right-click policy now supports `auto` (the existing
reporting-dependent behavior), `fux` (always menu), and `pane` (application forwarding). Prefix
`*` / `8`, the pane context menu, `pane-input` CLI/API and split/new `--right-click` creation
options expose the policy. Menu titles show the current mode. Alt-right-click overrides every
policy; menu-captured presses retain their releases. Policy follows the existing pane component
through movement. Default metadata is omitted on the wire but explicitly clears prior state on
apply/coalescing; policy changes publish even without a grid-sequence change. Layout archives and
tree imports preserve the current policy and do not serialize/restore it.

Targeted controller and ECS tests pass for all policy/reporting/Alt combinations, captured
press/release ownership, shared viewer updates, Auto clearing, split creation, movement, stale
route/instance rejection, no-op mutations and private history metadata. The bounded independent
review found one P3 history-reply metadata omission, now fixed and tested; re-review found no
remaining issues. Broad verification passed 141 library, 5 CLI/bin, 77 ECS and 14 boundary/protocol/
structure tests (237 total), workspace/harness Clippy and formatting. The real viewer scenario
passes keyboard cycling, CLI policy changes, visible pane-menu mode and reset to Auto. Its
existing mouse/application-forwarding scenarios also pass. The action label was shortened to
keep the command popup compact; the scenario observes attachment metadata before cycling a
CLI-updated policy. CLI tests additionally reject invalid values and preserve child arguments
after `--`. Logs: `/tmp/fux-right-click-{targeted,tests,clippy,harness-clippy,viewer,cli}.log`.

Previous implementation: last-focus navigation is available through prefix `!` (normalized `1`),
CLI `focus last` and control target `last`. Attached viewers retain independent two-pane history
across tabs/workspaces on the same server; workspace control history remains workspace-scoped.
History uses generational entities, ignores repeated focus and rejects removed targets. Ordinary
workspace switches observe immediately, including multiple requests in one ECS batch. Cross-workspace
Last enforces viewer admission before mutation; Last and following transfers observe their final
selection atomically. Archive application preserves live history and observes its committed selection.

Three targeted ECS regressions pass, covering private/toggling/batched history, deleted targets,
shared zoom, workspace control, destination limits and same-workspace selection at capacity.
The real viewer scenario passes with keyboard tab-to-tab Last, CLI default-history toggles, viewer
isolation and unchanged original PID. The independent bounded review found and then confirmed fixes
for two P2 issues: destination viewer-limit bypass and skipped intermediate workspace history.
No findings remain in that bounded re-review; final complete-diff review remains open.
Logs: `/tmp/fux-focus-last-{targeted,viewer}.log`. Broader verification passed: 140 library,
5 CLI/bin, 76 ECS and 14 boundary/protocol/structure tests (235 total), workspace all-target
Clippy, formatting and whitespace checks. Logs: `/tmp/fux-focus-last-{tests,clippy}.log`.
An old unknown-key unit fixture still used the newly bound `!`; it now uses unbound `0`.

Previous implementation: `fux layout TAB inspect PANE` and layout API `inspect` now expose coherent
underlying pane/area geometry, zoom presentation rectangle, directional neighbors, outer-edge
flags, canonical document and server/tab/pane/revision identity. Inspection is read-only and
workspace-scoped. Its neighbor calculation shares `Rect::navigation_area` with directional focus,
including the existing 1000-by-1000 fallback before usable geometry exists. Hidden zoom panes keep
underlying neighbors/rectangles; empty rectangles have no visible rectangle or edge flags. Pending
viewer-area changes reject inspection until layout resolves. Optional instance guards remain active.

Verification passed: 140 fux library tests, 5 CLI parser/bin tests, 73 ECS tests and 14 boundary/
protocol/structure checks; workspace and harness all-target Clippy; formatting/whitespace checks.
The new ECS regression checks unchanged tree/revision/frame and absence of process/PTY side effects,
neighbor/focus agreement, zoom/hidden geometry, foreign/missing panes, replacement instances and
zero-size geometry. The real `pane-layout-transfer` scenario now exercises the CLI query, coherent
headless geometry, unknown-ID rejection and unchanged process identity before its existing transfer
checks. Logs: `/tmp/fux-pane-inspect-{tests,clippy,harness-clippy,scenario}.log`.

An independent read-only review of the query, export helper, DTOs, shared navigation calculation,
CLI and tests found no confirmed issues. It verified the query returns before layout mutation and
all work is bounded by the existing pane limit. This is a bounded review, not the final whole-diff
acceptance review. Stored per-pane right-click policy is implemented above; final acceptance remains open.

Latest acceptance audit: the [pinned workflow checklist](#pinned-panelayout-workflow-audit) below
now records concrete baseline controls and evidence. It identified missing last-focus navigation,
read-only neighbor/edge queries and stored per-pane right-click policy. Geometry queries are now
implemented and verified above; last-focus is now implemented with evidence above. Stored right-click policy is now implemented with targeted and broad automated evidence.
The remaining workflow evidence and open verification gates still prevent proving pane/layout parity.

Latest routing acceptance: the account-free native-worker scenario now moves the live owner,
retires/recreates its source workspace, completes a native turn, moves back into the new source
lifetime and observes a reconciled read. It verifies unchanged retained target, PID and producer;
movement never recreates the worker. Native liveness now uses the single coherent manager
lookup, removing a route-lookup/workspace-list race that could falsely retire the worker.
The provider fixture now serves retained completed turns through `thread/read` in keep-open mode.

Independent review found and fixed two run-cleanup issues:

- P1: PTY EOF is not process exit. The reviewer reproduced a moved process closing every terminal
  descriptor and surviving timeout cleanup. `pane-location` now retains EOF routing with explicit
  `accepts_input: false`. Input/liveness consumers reject it; owned run cleanup can still terminate
  it. ECS coverage checks retained EOF routing, and the real run fixture closes all descriptors,
  waits for observed EOF, times out and verifies the original PID no longer exists.
- P2: manager replacement after final observation previously converted successful completion to
  cleanup failure. A conflict now requires a well-formed manager-info reply proving a different
  instance before old ownership is considered released. Unit checks reject malformed/empty
  identity and distinguish unchanged/replaced instances. No replacement receives a kill.

The independent reviewer re-read these fixes and their dependent fux/task contracts and reported
both findings addressed, with no new findings in that bounded scope. This is not the required
final review of the complete pane/layout diff.

Verification passed: 140 fux library, 72 ECS, 14 boundary/protocol/structure and 143 zor library
tests (2 existing ignored); workspace and harness all-target Clippy, native fixture Clippy,
formatting and whitespace checks. Real `zor-native` (four modes including movement), `zor-run`
(including moved EOF timeout), `zor-pane-layout`, `zor-tasks` and `zor-launch` pass. Fault proxies
now inject observation delays/failures at both manager lookup and workspace listing so prior
receipt deadline, cancellation and safe-stop checks remain effective after the liveness change.
Logs: `/tmp/fux-native-eof-{fux-tests,zor-tests,clippy,native-scenario,run-scenario,layout-scenario,tasks-final,launch-final,proxies-clippy,fixture-clippy,consumers}.log`.

Manual UI access was retried and still fails with `CUA_REPL_ENABLED_SURFACES is required`.
The full pinned Herdr workflow comparison, visual acceptance, final performance, refreshed
reference-free packaging and complete-diff review remain unproven. The goal remains active.

Latest implementation: `zor run` releases its creation pin after retaining the exact pane ID.
Final observation already uses manager evidence and immutable launch attribution; cleanup now
resolves that pane's current route, validates server/pane/origin identity and closes only that
pane before releasing the original workspace with its original lifetime guard. A raced move
retries scoped cleanup within a bounded deadline. Fast exit during pin release returns to final
observation. Destination workspaces are never cleanup targets.

Verification: all 143 zor library tests pass (2 existing ignored), including malformed/changed
run location identity rejection; 14 fux boundary/protocol/structure checks, zor and xtask
all-target Clippy, formatting and whitespace checks pass. The real `zor-run` scenario
passes immediate exits/status/output, fresh/existing server behavior, ownership refusal and original
timeout cleanup, plus both normal exit and timeout after movement, source retirement/name reuse,
unchanged moved PID and preservation of the destination's original process. The movement fixture
selects the exact run command, not the workspace's startup shell. Logs are in
`/tmp/fux-run-route-{libraries,clippy,harness-clippy,scenario-final}.log`.

At that checkpoint, remaining acceptance included native heartbeat movement evidence (now above),
complete pinned Herdr workflow
comparison, manual visual checks, final performance measurements, refreshed reference-free package
verification and complete-diff review. Pane/layout parity is not yet proven.

Latest implementation: attached managed launches now release their temporary workspace pin after
exact pane/PID identity is durably committed. Fux distinguishes creation pins from explicit pins;
manager `release-pane-pin` checks server/pane/PID and clears only a creation pin. Repeated release
is a no-op and an explicit `FixWorkspace` pin remains protected. Zor retries release from the same
attached launch during reconciliation. A failed journal commit leaves the pin in place; a lost
release request/reply retains the durable attachment and never authorizes another spawn.

Verification passed: 140 fux, 13 local-ipc and 142 zor library tests (2 existing ignored), 72 ECS,
5 CLI and 14 boundary/protocol/structure checks; workspace and xtask all-target Clippy; formatting
and whitespace checks. ECS coverage verifies wrong-server/PID/pane rejection, no-op retries,
unchanged geometry/grids/processes and preservation of explicit pins. The launch fault-injection
scenario drops a release before forwarding and then drops its successful reply; both retries
retain the attached identity, and recovery creates no additional process. `zor-launch` and
`zor-tasks` pass. `zor-pane-layout` now moves both adopted and managed panes, retires their source
workspace, submits/reconciles prompts, stops the managed process through its current route and
confirms final evidence. Its stop observation follows the documented pending result instead of
assuming synchronous process exit.

At this earlier checkpoint, `zor run` still retained its creation pin (resolved above). Managed
pre-attachment creation intentionally stays pinned until durable pane identity exists; attached
managed movement is no longer blocked by that safety constraint. Final performance, manual visual
acceptance, refreshed reference-free package verification and complete-diff review remain open.

Latest implementation: new zor adoptions can move across workspaces. The route resolver validates
manager location against the retained server/pane/PID and optional immutable launch origin. Live
requests use the current route and validate listing lifetime/process identity; focus and managed
stop use the same path. Shared-writer/group/worktree identity excludes mutable workspace routing.
New adoptions capture `origin` separately, so a pane first adopted after an earlier move can still
match final evidence to its actual launch workspace. Adoption no longer takes `FixWorkspace`.
The recorded adoption route remains unchanged for idempotent request identity. Existing origin-less
records retain their previous final-evidence fallback; new managed records capture launch origin.

Verification passed: 140 fux, 13 local-ipc and 142 zor library tests (2 existing ignored), 14
boundary/protocol/structure checks, workspace all-target Clippy, zor CLI-without-default-features
Clippy, xtask all-target Clippy and formatting/whitespace checks. The exact rebuilt zor binary
passed `zor-pane-layout`: adoption after a prior move, another live move, prompt preparation,
submission/reconciliation, terminal output and final exit evidence, with unchanged retained task
target. The task fault-injection and managed-launch recovery scenarios also passed. Proxy manager
forwarding preserves location/identity reads while receipt failure injection remains active.

Managed launches and `zor run` remain pinned. Before attachment, a lost creation reply may leave
zor without a pane ID: marker discovery and event/final recovery still use the creation workspace.
The next change must retain protection until durable pane identity exists, release it idempotently
when safe, and handle retry/crash around that release. `zor run` needs equivalent route-aware live
observation before its pin can be removed. Final performance/manual/package/full-diff acceptance
also remains open; the extra location RPC cost needs inclusion in performance verification.

Latest routing progress: cross-workspace transfer now permanently fails unused input reservations
instead of advancing `input_sequence`. Submitted/delivered receipt evidence stays unchanged, while
queued operations still block movement. A regression reproduced the old sequence mismatch before
the fix; tests now prove rejected transfers preserve reservations, moving away/back cannot revive
them, and delivered sequence/bytes remain valid.

Manager `input-status` reads exact server/pane/operation receipts independently of workspace
lifetime, with the existing expiry and without enabling cross-route submission. Zor reconciliation
now uses it. The real transfer scenario delivers input, reserves an unused operation, moves the
pane, waits for source-socket retirement and verifies both retained delivery and failed reservation.
The task fault-injection proxy now exposes manager receipt reads and preserves delayed, dropped,
expired and held-receipt injection. Its first run exposed the missing proxy endpoint; after the
fixture update, `zor-tasks`, `zor-launch`, `zor-pane-layout` and `pane-layout-transfer` all passed.

Verification also passed: 140 fux library, 13 local-ipc library, 141 zor library (2 existing ignored),
71 ECS, 5 CLI, 3 boundary, 3 protocol and 8 structure tests; the proxy cancellation test; workspace
and xtask all-target Clippy; formatting and whitespace checks. Reviewed the manager receipt API
as generic evidence access and updated the boundary/consumer inventories. Task route resolution,
identity/final-evidence checks and replacement of the fixed-workspace constraint remain unfinished,
as do the final performance/manual/package/full-diff acceptance gates.

Latest routing groundwork: `locate-pane PANE --instance INSTANCE` and manager `pane-location`
now read current workspace/lifetime/tab/layout generation together with the same pane/PID and
immutable launch workspace/lifetime. Lookup requires manager authority, never creates containers,
and rejects another server, missing/closed panes and unavailable ownership. The expanded transfer
scenario calls the CLI after the original socket has disappeared and verifies current route,
original attribution and unchanged PID. This is a routing primitive, not completed managed-task
movement; fixed-workspace protection remains until its consumers are ready.

Verification passed: 140 library, 70 ECS, 5 CLI, 3 boundary, 3 protocol and 8 structure tests;
workspace and xtask all-target Clippy, formatting/whitespace checks and the real-process transfer
scenario. New ECS coverage verifies read-only behavior, failure boundaries and current-versus-origin
routing across transfer. Reviewed manager/CLI/DTO additions as generic terminal identity and
updated boundary and protocol-consumer inventories accordingly.

Latest implementation: workspace display labels now have prefix `=`, a workspace-menu editor,
guarded CLI/control API, shared bar display and label-plus-route chooser entries. The routing
name and lifetime stay fixed. Sparse presentation deltas send labels only when changed, with
explicit clearing and validated coalescing. Listings/catalogs expose the label separately;
archives restore it atomically and detect concurrent renames in their expected-state check.

Verification passed: 140 library, 69 ECS, 5 CLI, 3 boundary, 3 protocol and 8 structure tests;
workspace and xtask all-target Clippy; formatting and whitespace checks. Targeted cases cover
missing/zero identity guards, stale lifetimes, Unicode/control/length bounds, no-op events,
shared grids and geometry, catalog identity, sparse output inheritance, clearing, full-frame
replacement, pasted input and atomic archive rejection/restoration. The expanded real-viewer
scenario renamed through the keyboard, observed two viewers, checked route/lifetime/PID and
cleared through the CLI before completing the existing close/recreation checks. Reviewed the
new declarations as generic display metadata before updating boundary/consumer inventories.
Managed-task route restrictions, final performance/manual acceptance and full-diff review remain
open. The earlier reference-free package evidence predates this feature and must be refreshed
for final acceptance.

Latest verification and correction: a new ECS regression reproduced unnecessary `ResizePty`
effects when equal-sized panes exchanged positions. Layout resolution now updates the rectangle
and publishes the changed layout while resizing the emulator/PTY only when terminal dimensions
change. All 68 ECS tests, workspace all-target Clippy, formatting and whitespace checks passed;
the real-process pane transfer and viewer scenarios passed against the rebuilt binary.

A fresh temporary source copy included tracked and nonignored untracked files and excluded all
reference checkouts, Git metadata and prior build output. The workspace built, fux/local-ipc
packages verified, and 138 library, 68 ECS, 3 boundary, 3 protocol and 8 structure tests passed
there. No copied manifest, lockfile or build script mentions Hypertile. Exact commands, toolchain
and copied-file hashes are in [the independence evidence](verification/pane-layout-no-references-2026-09-12.json).
This proves independence for those builds/tests at the recorded source state; it is not full CI,
manual visual acceptance or a final parity claim. Workspace renaming, managed-task routing,
remaining performance work and complete-diff review remain open.

Latest implementation: prefix `q`, the workspace menu and `fux workspace close NAME --instance
INSTANCE --stream STREAM` now close the current workspace through its guarded control API.
The viewer requires confirmation, rejects pasted confirmation text and captures the server,
workspace lifetime and viewer identity. Full frames carry `workspace_stream`; ordinary deltas
inherit it without an event-log lookup or repeated wire bytes. Workspace switches reset it,
and incomplete/zero identity groups fail validation before applying changes. Confirmation
cancellation also preserves an outstanding mouse-release path instead of hiding it behind a popup.

Verification passed: 138 library tests, 67 ECS tests, 5 CLI tests, protocol/structure checks,
workspace and xtask Clippy, and the expanded real-viewer scenario. That scenario cancels a close,
closes a workspace with two attached viewers, checks another workspace's original PID, recreates
the closed name, rejects the old lifetime through the CLI and closes the replacement with its
fresh lifetime. The boundary inventory includes the generic CLI name/instance/stream arguments.
Workspace renaming, fixed task-route restrictions and the final acceptance gates remain open.

Previous implementation: prefix `.` and the pane menu now open an explicit swap destination
chooser. It lists pane IDs and labels/titles in layout order and accepts arrows/j/k, wheel,
Enter or painted-row clicks. The source remains fixed across focus changes; server/viewer/
workspace/layout changes cancel it. Pasted input cannot trigger a swap and click releases remain
captured after submission/cancellation. The existing atomic swap API preserves pane/process
identity and swaps nonadjacent panes without intermediate moves. Verification passed 137 library
and 67 ECS tests, boundary/protocol/structure checks, workspace and xtask Clippy, and the expanded
real-viewer scenario. That scenario swaps by keyboard, restores the arrangement by mouse, checks
both PIDs, then continues through the established layout/transfer/multi-viewer coverage.
Workspace rename/close contextual controls, routing restrictions and final acceptance remain open.

Previous implementation: contextual pane/tab/workspace menus now select existing registry actions
through the same dispatcher as key bindings. Prefix `?`, apostrophe and backtick open them from
the keyboard; right-click opens them at pane/tab/workspace targets, with Alt-right-click overriding
application mouse ownership. Menus capture server/viewer/workspace/tab identities, cancel stale
targets and preserve click-release ownership through cancellation. Inactive tab actions have no
access to the active tab's pane data. Disabled actions show their reason; wheel, arrows, Enter
and painted-row clicks navigate/activate. Workspace menus currently offer choose/new/reorder.
Direct swap-target selection and workspace rename/close contextual actions remain unfinished.

Verification: 135 library tests and 67 ECS tests passed, plus boundary/protocol/structure checks,
workspace and xtask Clippy, and the expanded real-viewer scenario. An additional targeted menu
identity test covers ordinary output versus server/viewer/workspace/layout replacement. The
scenario renames a nonfocused pane and inactive tab without changing selection or PID, then
exercises existing drag/zoom/transfer/multi-viewer controls. An unnecessary CLI rename immediately
before its drag introduced a viewer-revision race; removing that scenario mutation let the drag
exercise its intended path. Its tab-close assertion now checks the generic all-panes warning,
which is accurate for inactive tabs whose contents are not carried by the attachment.

Previous implementation: tab exports and workspace archives now include canonical manual pane
labels. Complete imports restore/clear them, apply source-pane remapping and reject invalid
labels atomically with geometry and zoom. Bare trees preserve labels. Renames advance the tab
revision, preventing stale imports from overwriting newer names. Verification passed 132 library,
5 CLI and 67 ECS tests, protocol/structure checks, workspace and xtask all-target Clippy, and
both real-viewer and real-process transfer scenarios. The transfer scenario deliberately changes
labels after export, restores them through the CLI, applies archive labels and verifies those
labels alongside the original PID/PTY after cross-workspace movement. Label serialization parity
is now covered; contextual menus and the other unfinished acceptance requirements remain open.

Previous implementation: manual pane labels now have a shared server primitive, prefix `;`
editor, guarded `rename-pane` CLI/API, separate application title and history/listing metadata.
Targeted tests cover Unicode/byte limits, control characters, no-op behavior, competing viewers,
stale/deleted targets, paste/cancellation and clearing across coalesced frames. Full verification
passed 132 library and 65 ECS tests, protocol/structure checks, workspace and xtask Clippy,
the expanded real-viewer scenario and real pane transfer. The latter preserves labels through
layout apply and workspace transfer alongside the same PID/PTY/content.
Herdr `src/app/api/layouts.rs:326` exports manual labels and `:471` applies them. That baseline
finding motivated the label serialization work above.

Prior verification: sparse frame metadata passed 130 library tests, 63 existing ECS tests,
boundary/protocol/structure suites, workspace all-target Clippy and the real-viewer scenario.
A further targeted ECS regression passes for full attach identity/catalog, unchanged output
omission, tab rename/revision publication and workspace-switch metadata reset. The frame
round-trip test covers coalescing, explicit empty catalogs and malformed full metadata.
Three release measurement repetitions reduced median keystroke bytes from 989 to 855
(baseline 852) in both tested viewer configurations. See the performance report for raw evidence
and unresolved CPU/burst differences. This is not a final performance or parity acceptance pass.

Baseline: fux `1792223ea4a24905501823203b585e50906f0d38`; Herdr
`d184b41fa36923c132629af725ff98bb02aa1b61`. Hypertile reference inspected at
`92fa63300f802c4465a4fbd2b928cf3ef1b4f8d0`; no dependency or copied code added.

Herdr's UI/API controls include explicit/directional swap, zoom modes, moves into existing/new
tabs and new workspaces, resize, layout export/apply and workspace/tab ordering. Its cross-workspace
move preserves the process but changes workspace-qualified IDs and retains aliases. fux
uses globally allocated pane IDs but currently records immutable launch attribution; routing
is now separate from current routing ownership. Manager transfers enforce both layout revisions
and route constraints; old-workspace receipts cannot submit after a move.

## Pinned pane/layout workflow audit

This replaces the earlier aggregate feature checklist. Reference revision is Herdr
`d184b41fa36923c132629af725ff98bb02aa1b61` (verified in the local reference checkout).
Source inspection covers [pane CLI][H-pane], [tab CLI][H-tab], [workspace CLI][H-workspace],
[key actions][H-keys], [client focus actions][H-actions], [context menus][H-menus],
[mouse gestures][H-mouse], [pane geometry API][H-geometry] and [layout import/export][H-layouts].
These are code-derived baseline behaviors, not a claim that Herdr was manually exercised here.
The current fux implementation and tests were inspected locally; historical audit statements
are not evidence of current missing or implemented controls.

Status distinguishes **implemented**, **gap**, and **verification open**. Implemented does not
mean final acceptance: visual, performance, platform and complete-diff gates below still apply.
Keyboard entries use the configured prefix (Ctrl-A by default). Equivalent outcomes can use
different controls, but an inaccessible primitive does not count as a user workflow.

| Workflow | Pinned Herdr behavior | Current fux entry points and behavior | Code/test evidence and remaining limitation |
|---|---|---|---|
| Split side by side | Split right, configurable ratio/cwd/env and focus policy [H-pane] | Prefix `\|`, pane menu; `fux split` / control `split` with horizontal axis | **Implemented:** creation/layout systems; ECS `split_focus_and_following_input_reach_the_new_pane_only_after_creation`; real viewer splitting. Initial `--ratio` and `--focus`/`--no-focus` are now implemented; ECS/CLI tests cover geometry, queued input and private/default selections. Real viewer/CLI checks pass for initial 70/30 geometry, retained focus/PID and subsequent navigation. |
| Split stacked | Split down with the same launch options [H-pane] | Prefix `-`, pane menu; split vertical axis | **Implemented:** same code paths and geometry coverage. Baseline ratio/focus options now have equivalent CLI/API controls; ECS tests include minimum/maximum ratios and shared zoom. Fux uses an integer 10000 scale and defaults to focused creation. |
| Close one pane | Explicit pane close, including final-container lifecycle [H-pane] | Prefix `x`, confirmation/menu, `fux kill`, control `kill` | **Automated verification passed:** ECS explicit-last-pane/final evidence and natural-exit tests; real viewer mouse-only cancel/confirm. Confirmation releases are captured and stale targets cancel. Other processes retain identity. |
| Focus an explicit pane | API targets pane identity [H-actions] | Content click; `fux focus ID` / control `focus` pane target | **Implemented:** ECS split/input and private-viewer focus tests; viewer scenario. Workspace sockets deliberately reject foreign panes. |
| Focus directionally | Four keyboard directions and CLI direction [H-keys], [H-pane] | Prefix h/j/k/l; `fux focus left/right/up/down` | **Implemented:** same tree neighbor helper as layout movement; geometry unit tests. Tie-breaking is fux's documented overlap/gap/tree-order rule. |
| Cycle next/previous pane | Dedicated cycle actions [H-keys], [H-actions] | Prefix o/u; `fux focus next/previous`; control next/previous | **Automated verification passed:** real viewer/CLI cycles in both directions through nested, post-close and single-pane layouts, including wraparound, retained PIDs and private viewer selection. Manual walkthrough remains open. |
| Return to last focused pane | Client retains previous focused ID and validates it in the current snapshot before focus [H-actions] | Prefix `!`; CLI `focus last`; control target `last`; private same-server viewer history across tabs/workspaces | **Implemented:** three ECS regressions and real viewer/CLI scenario cover toggling, private scope, deleted targets, batched switches, viewer limits, zoom and retained PID. Final manual walkthrough remains open. |
| Discover directional neighbor without changing focus | `pane neighbor` returns optional target plus layout snapshot [H-geometry] | `layout TAB inspect PANE`; API layout `inspect` returns all four optional neighbors with document/revision | **Implemented:** ECS `pane_geometry_inspection_is_coherent_read_only_and_matches_focus` and real CLI scenario pass; shared navigation rules, no mutations and foreign/missing rejection reviewed. |
| Inspect pane edges and geometry | `pane edges` reports contact with outer area edges plus layout; `pane layout` reports pane geometry [H-geometry] | `layout TAB inspect PANE`; coherent area/rect, four outer-edge flags, zoom presentation and navigation-area fields | **Implemented:** same ECS/CLI coverage plus zoom-hidden and zero-size cases; underlying and visible geometry explicitly separated in guide. |
| Resize directionally | Resize mode, directional actions and numeric CLI amount [H-keys], [H-pane] | Prefix r then direction; uppercase shrinks; `layout resize`; `resize-toward` API | **Implemented:** layout mutation/ancestor tests and keyboard mode tests. Guide documents the 250-unit (2.5 percentage-point) step, nearest matching ancestor, clamping and cell rounding. Minimum-area/tiny-terminal acceptance remains open. |
| Resize a specific split | Separator drag updates split ratio [H-mouse] | Separator drag; `layout ratio SPLIT RATIO`; `set-ratio` and `resize-border` API | **Implemented:** canonical split indices and ratio bounds; ECS generation and position-only-resize checks. Fux commits on mouse release rather than every motion. |
| Zoom and restore | Toggle/on/off [H-pane] | Prefix z, pane menu; `layout zoom [PANE]`; API explicit pane/null | **Implemented:** `zoom_is_shared_preserves_tree_and_routes_every_viewers_input_to_visible_pane`; remapped export restoration tests. Private focus restoration and hidden PTY sizes retained. |
| Swap with directional neighbor | Four swap keys and CLI direction [H-keys], [H-pane] | Prefix v plus direction; generic `swap-direction` API; generic control CLI | **Implemented:** tree/ECS mutations and mode tests. Dedicated layout CLI accepts explicit pane pair; directional API remains available through control CLI. |
| Swap explicit nonadjacent panes | Explicit source/target API; context swap with focused pane [H-pane], [H-menus] | Prefix `.`, chooser/menu; `layout swap PANE TARGET` | **Implemented:** chooser captures source/revision; ECS `equal_sized_pane_swaps_publish_positions_without_resizing_terminals`. Manual chooser acceptance remains open. |
| Relocate within a tab | Pane drag/drop and pane movement controls [H-mouse] | Prefix m plus direction, Alt-pane drag; `layout move`; `relocate`/`move-direction` API | **Implemented:** remove/reinsert semantics distinct from swap; identity tests and drop preview/cancellation coverage. |
| Move to an existing tab | Destination tab/target, split orientation/ratio, optional focus [H-pane] | Prefix e chooser, drop on tab label; `layout to-tab`; transfer API | **Implemented:** two-layout revision checks; ECS cross-tab identity and real viewer drop coverage. Initial ratio is exposed through CLI/API and verified for all four sides, with real process preservation. Explicit focus/no-focus CLI/API behavior is now verified for default/private selection, zoom and Last history; final manual acceptance remains open. |
| Move into a new tab | New tab around existing pane [H-pane] | Prefix b, menu; `layout new-tab`; transfer new-tab destination | **Implemented:** ECS `moving_last_pane_to_new_tab_keeps_receipts_process_and_viewer_valid`; real viewer transfer. No process relaunch. |
| Move into an existing workspace | Existing-tab destination can resolve another workspace [H-pane] | Prefix i / workspace chooser; `transfer-pane` manager CLI/API | **Implemented:** live transfer/zor/native/run scenarios, source retirement and name reuse. Explicit pins and queued input reject movement; these are deliberate authority/state constraints, not permission to claim unrestricted parity. |
| Move into a new workspace | New workspace/tab labels around moved pane [H-pane] | Prefix y, menu; `transfer-pane --new-workspace` | **Implemented:** atomic new-container creation, rollback tests and real viewer manager controls. Existing pane/PID/PTY retained. |
| Rename/clear a pane | Manual label separate from title; clear menu item [H-menus] | Prefix `;`, pane menu editor; `rename-pane ID LABEL`, empty clears | **Implemented:** ECS shared-label/title/bounds tests; real nonfocused-pane menu edit; export/remap restoration. |
| Choose right-click ownership per pane | Menu toggle and `pane input --right-click herdr\|pane`; split option [H-menus], [H-pane] | Prefix `*`; pane-menu cycle; `pane-input` CLI/API; split/new `--right-click`; Alt menu override | **Implemented:** controller/ECS tests cover ownership, metadata, creation, movement and stale route/instance rejection. Real viewer/CLI cycling, visible mode and reset pass. Archives preserve current policy without serializing it; final manual acceptance remains open. |
| Create/select/rename/close tabs | Keyboard, tab menu and CLI [H-tab], [H-keys] | Prefix t/n/p/w/comma/c; tab bar/context menu; tab CLI/control actions | **Implemented:** private selection, inactive-tab menu actions, labels and closure tested. Manual terminal behavior remains unverified. |
| Reorder tabs | Previous/next key actions and chrome drag [H-keys], [H-mouse] | Prefix g chooser and context menu; `tab reorder` API/CLI | **Automated verification passed:** ECS ordering/private focus and real mouse-only context-menu/chooser reorder and restoration. Fux uses a destination chooser rather than direct tab-bar drag; final manual interaction acceptance remains open. |
| Create/select/rename/close workspaces | Workspace picker/chrome, labels and CLI [H-workspace], [H-menus] | Prefix a/s/= /q; contextual menu; workspace CLI/API | **Implemented:** stable route labels, guarded close and real viewer rename/clear; workspace stream guards reject reused names. Worktree-group operations are scoped separately below. |
| Reorder workspaces | Workspace chrome drag [H-mouse] | Prefix f chooser, context menu; manager reorder CLI/API | **Automated verification passed:** catalog/no-effect rejection tests and real mouse-only chooser reorder, workspace selection and order restoration. Direct chrome drag is absent; final manual interaction acceptance remains open. |
| Drag a pane with feedback/cancel | Client pane drag and split targeting [H-mouse] | Alt-left-drag, cyan target half/tab feedback, Esc, source/destination revision guards | **Implemented:** controller fragmented mouse/cancel tests and viewer/tab/drop scenarios. App input forwarding and tiny rendered targets require final visual acceptance. |
| Export layout shape and labels | Layout export contains shape and launch/pane descriptions [H-layouts] | `layout export`, `workspace export-layout` through the manager | **Implemented for this task:** tree/labels/zoom/order/focus metadata serialized deterministically. Exported commands and relaunch restoration are explicitly outside the execution prompt's scope. |
| Apply shape to existing panes | Herdr apply creates a new layout's processes and may replace a tab [H-layouts] | Guarded `layout apply` / archive apply edits existing panes atomically | **Different scoped contract:** existing-pane identity/remap/atomicity verified. Herdr's process-spawning restore is excluded by the prompt; do not present this as parity for session restoration. |
| Discover controls and failures | Help/key actions/context menu [H-keys], [H-menus] | One command registry drives popup, menus, bindings and dispatch | **Verification open:** registry coverage tests pass, but final all-actions keyboard/mouse/CLI inventory and disabled-state walkthrough remain required. |
| Operate small/zero-size terminals | Recursive layout and client geometry [H-geometry] | Deterministic zero-area collapse without removing panes; o/u traversal and z recovery; PTY dimensions clamped | **Automated verification passed:** isolated real viewer at 2×2 and 1×1, keyboard selection/zoom/unzoom, input to each tiny pane, restored geometry/document/PIDs and output. One-row terminals have no content area; manual visual acceptance remains open. |
| Concurrent viewers and process identity | Herdr client/API baseline; this prompt additionally demands concurrency safety | Shared tree/zoom; private focus; minimum viewer dimensions; guarded mutations | **Automated verification and full review passed:** ECS competing viewers, revisions, stale import, transfer and real-process identity checks pass. Manual simultaneous-viewer acceptance remains open. |

The source inspection found no basis for treating the previous broad “help/CLI/API parity” row
as complete. Geometry queries, last-focus navigation and stored right-click policy are now
implemented. Initial split and transfer ratio/focus options are also implemented. Close the explicitly listed
tiny-terminal evidence gap and manual traversal/chooser acceptance, followed by the remaining final acceptance gates. These discoveries expand the concrete checklist within the
original pane/layout scope; they do not change the acceptance target.

Worktree group creation/deletion/collapse, agent sidebar/navigation, copy/scrollback/transcript,
provider integration, terminal controller leases, plugins and process/session restoration are
excluded here under the execution prompt's boundaries. In particular, Herdr workspace groups in
the inspected menu are linked-worktree groups; this is not evidence of an unimplemented generic
pane-layout group primitive. Sidebar styling/placement is a UI difference, not a claimed fux sidebar
feature. Native Windows and other platform parity are not established by these macOS-local tests.

Global gates remain **open**: real terminal visual acceptance; final matched performance measurements;
updated build/package/tests from a source copy with all references absent; repository-required final
checks; complete-diff independent review and fixes. CUA's current configuration error is recorded
above, but other work can continue. No row overrides these gates.

[H-pane]: https://github.com/herdrdev/herdr/blob/d184b41fa36923c132629af725ff98bb02aa1b61/src/cli/pane.rs
[H-tab]: https://github.com/herdrdev/herdr/blob/d184b41fa36923c132629af725ff98bb02aa1b61/src/cli/tab.rs
[H-workspace]: https://github.com/herdrdev/herdr/blob/d184b41fa36923c132629af725ff98bb02aa1b61/src/cli/workspace.rs
[H-keys]: https://github.com/herdrdev/herdr/blob/d184b41fa36923c132629af725ff98bb02aa1b61/src/input/keybindings.rs
[H-actions]: https://github.com/herdrdev/herdr/blob/d184b41fa36923c132629af725ff98bb02aa1b61/src/client/shell/actions.rs
[H-menus]: https://github.com/herdrdev/herdr/blob/d184b41fa36923c132629af725ff98bb02aa1b61/src/client/shell/context_menu.rs
[H-mouse]: https://github.com/herdrdev/herdr/blob/d184b41fa36923c132629af725ff98bb02aa1b61/src/client/shell/mouse.rs
[H-geometry]: https://github.com/herdrdev/herdr/blob/d184b41fa36923c132629af725ff98bb02aa1b61/src/app/api/panes.rs
[H-layouts]: https://github.com/herdrdev/herdr/blob/d184b41fa36923c132629af725ff98bb02aa1b61/src/app/api/layouts.rs

No whole-product Herdr parity claim is part of this task. No runtime behavior, platform result
or acceptance gate is considered passed merely because it has been specified here.

## Current verification evidence

- Baseline six layout tests passed before edits.
- Thirteen layout tests now pass, including mutation sequences, canonical round trips, malformed
  graphs, unknown membership, node-count and depth limits.
- Full fux library (114 tests) and ECS (48 tests) passed after initial zoom work.
- Isolated real-server/PTY smoke exercised swap, relocation, directional resize, zoom/unzoom and
  file apply. Both original pane PIDs survived and captured output/tree roundtrip were retained.
  Local-only script/result: `.verification/pane-layout/pty-smoke.py` and
  `.verification/pane-layout/pty-smoke-result.json`. This must become a reproducible tracked
  scenario and is not evidence for mouse or cross-container workflows.
- Protocol inventory review: new requests/results, flat layout nodes, zoom and revision metadata
  are generic terminal/layout primitives. No provider/task policy entered fux. The boundary
  fixture was regenerated after inspecting these declarations; final independent/full-diff
  review remains pending.

Next work: integrate workspace transfer/order controls, resolve the fixed-workspace parity restriction through an explicit
routing transition, and implement the complete persistence envelope. Promote the remaining
local-only PTY smoke into tracked acceptance, inspect the real UI, compare performance, and run
the remaining CI/package/clean-reference gates and final review. Passing subsets are not completion.



## Keyboard and gesture continuation

Implemented registry-driven resize/swap/move modes and directional protocol operations. Arrow
keys preserve their actual direction, paste is ignored, and each operation keeps its original
pane target while using the latest observed layout revision. Added mouse separator resize and
Alt-pane relocation, on-release commit and target/side hints. Cancellation suppresses the gesture
tail; stale layouts cannot silently acquire a new target. New tests cover fragmented SGR input,
ordinary mouse forwarding, explicit Alt capture, Escape/stale cancellation and server separator
validation. Manual terminal acceptance and tracked real-process mouse coverage remain required.


## Tab transfer continuation

Added transfers to another existing tab and to a new tab without spawning a process. Source and
destination generation checks precede changes, starting panes block the operation, and empty
source tabs close through the existing selection cleanup. The CLI exposes `layout new-tab` and
`layout to-tab`. Added ID-based tab ordering through `tab reorder TAB [BEFORE_TAB]`.

ECS verification covers source/destination stale rejection, preservation of other panes, receipt
submission after moving the last pane, and focused tab/pane identity during reorder. Cross-workspace
work remains separate: existing receipt lookup is scoped to its retained workspace but resolves
its pane globally. A future cross-workspace move must prevent old-route input from reaching the
new workspace and must reconcile or explicitly protect zor's pinned attempt identity. Do not
simply relax the same-workspace destination check.

The isolated real-binary smoke also passed new-tab and existing-tab transfers in both directions,
with both original PIDs retained and the emptied temporary tab removed. Evidence remains in
`.verification/pane-layout/pty-smoke-result.json`; a tracked acceptance scenario is still required.


## Import mapping and workspace ordering continuation

Implemented complete explicit source-pane remapping for layout apply, available as repeated
--map SOURCE=DESTINATION CLI options. Rejection of partial, duplicate-source,
duplicate-destination and foreign-destination maps leaves the original layout unchanged.
Added manager-only workspace ordering with ordered listing/chooser results; ID-based state
keeps workspace focus/process identity unchanged and is pruned when a workspace disappears.
Targeted ECS tests and fux all-target Clippy pass.

Remaining scope still includes cross-workspace transfer with safe route reconciliation,
integrated transfer/order controls, the full layout persistence envelope (including selection
and container ordering), tracked real-process acceptance, manual visual inspection,
performance comparison, clean-reference package checks and final full-diff review.

## Viewer tab controls continuation

Added registry-backed prefix bindings: `b` moves the live focused pane into a new tab, `e`
chooses an existing destination tab, and `g` places the active tab before another or last.
The transfer chooser retains the original source pane and both revisions, ignores paste,
and cancels safely on source changes or Escape. Destination revision changes are rejected
by the server. Insertion is right of the destination's first pane; CLI/API retain explicit
pane/side choice. Workspace transfer/order controls and mouse container movement remain open.

Viewer tab entries now publish first-pane identity and layout revision. Layout changes refresh
all viewers in that workspace so hidden-tab metadata stays current; ordinary output retains
its previous refresh path. First-pane lookup traverses the tree without allocating a leaf list.
Both transfer endpoints reject pending viewer geometry changes before layout revision advances.
The resize/transfer regression uses two queued requests from one viewer because control-socket
requests run before viewer queues in the current ECS schedule.

Verification added: retained chooser revisions, pasted Enter, Escape and missing destinations;
reordering before/last; hidden-tab metadata after a swap; and destination resize followed by
transfer in the same request batch. Full pane/layout acceptance remains incomplete, including
cross-workspace route safety, full persistence, real-terminal inspection and final review.

## Cross-workspace routing continuation

Implemented manager-only cross-workspace transfer to existing/new tabs in an existing workspace,
with a typed transfer envelope and `fux transfer-pane` CLI. The operation checks source/destination
revisions, destination stream lifetime, limits, starting panes and pending viewport changes before
mutation. Current routing ownership is now a separate pane field used by cleanup/quota accounting;
immutable launch attribution still backs final records. This fixes the source-cleanup hazard where
moving its last pane could otherwise terminate the moved process.

Input submission validates current route. Cross-workspace moves advance input sequence to invalidate
old reservations even after a round trip; retained queued operations block movement. Added the
generic lifetime fixed-workspace constraint. Zor requests it atomically at managed/run creation
and before adopting a pane, rather than silently leaving attempts on an old workspace route.
This restriction is a documented remaining parity gap; an explicit zor rebind is not implemented.

Added tracked `pane-layout-transfer` and `zor-pane-layout` xtask scenarios, wired into local and
automation integration tests. The real binaries passed source-workspace removal with the moved
PID/PTY/output intact and continuing input, followed by adopted/managed zor move rejection and
unchanged task identities. `cargo test -p fux --test ecs --test protocol_consumers --test structure
--test agent_boundary --locked` passed (57 ECS tests, 14 boundary/protocol/structure checks).
Zor library tests passed (141 passed, 2 pre-existing ignored). Workspace all-target Clippy and
standalone xtask all-target Clippy passed. Full acceptance, full-diff review, complete persistence,
new-workspace destinations, integrated workspace controls and manual/performance/package checks
remain outstanding; this does not claim complete pane/layout parity.

Follow-up verification for the routing change: existing real `zor-tasks` and `zor-launch`
scenarios passed against the newly built fux/zor binaries. Fux library (119), binary (3) and
protocol fixture (2) tests passed; formatting and diff whitespace checks passed.

## New workspace destination continuation

Added a tagged existing/new workspace destination and `transfer-pane --new-workspace`. New
workspaces reserve only identity and an event stream; the transfer inserts the existing pane,
sets default tab/focus and publishes the endpoint after success. Name collisions, malformed
names, missing source panes and fixed-route rejection roll the reservation back without starting
processes or publishing an empty workspace. Ordinary workspace creation reuses the same empty
reservation helper before its existing initial-pane launch flow.

The local tracked transfer scenario now creates a fresh workspace with exactly one existing
pane and checks PID/PTY/output continuity after source cleanup. The zor variant continues to
exercise an existing destination and adopted/managed route constraints. Added ECS rollback and
initial selection coverage plus CLI checks for explicit creation and conflicting options.
This closes the direct new-workspace destination gap, not the remaining integrated workspace
controls, fixed-route restriction, complete persistence or final acceptance work.

Verification for new-workspace destinations passed: 58 ECS tests, four binary/CLI tests,
14 boundary/protocol/structure checks, workspace and xtask all-target Clippy, formatting and
whitespace checks. Both tracked real-binary scenarios passed: new-workspace transfer retained
the original PID/PTY/output with no replacement pane; existing-workspace transfer and zor
fixed-route rejection continued to pass.

## Viewer manager controls continuation

Added prefix `f` workspace ordering and prefix `y` transfer into a named new workspace, using
the existing registry and a bounded manager-request path in the client loop. These are disabled
without a manager endpoint. Paste does not confirm operations; Escape and stale source layouts
cancel pending choices. Manager replies report rejection rather than retrying a changed target.

Frames now carry their server instance and own viewer ID. The manager transfer can follow that
viewer atomically after checking it still belongs to the source tab. This preserves the attachment
when its last pane moves out and the source workspace retires; it also selects the exact moved
pane at the destination. CLI transfers continue to omit viewer following. ECS and controller
coverage checks stale/disconnected viewers, initial focus, retained identity, paste and cancellation.
Existing-destination workspace selection, mouse cross-container controls, route rebinding, full
persistence and the final acceptance gates remain unfinished.

Viewer-manager verification passed: 121 library tests, 58 ECS tests, boundary/protocol/structure
checks and workspace/xtask all-target Clippy. The expanded automated real-viewer scenario passed
new-workspace movement with viewer following, unchanged PID, input after source retirement,
keyboard workspace ordering and detach. The real-viewer suite also exposed a popup bug after
viewport growth: rendering clamped the scroll offset but Page Up used the old stored offset.
Scrolling now clamps its starting offset before applying a page/row delta. The harness verifies
this resize/scroll sequence, menu paging and the documented directional resize semantics.

An incremental object disappeared during one harness rebuild; a non-incremental build in the
separate `target/pane-layout-harness` directory succeeded. Final scenario command:
`target/pane-layout-harness/debug/fux-xtask scenario viewer target/debug/fux` (passed).
This is automated PTY/terminal-model evidence; manual visual inspection is still outstanding.

## Existing workspace chooser continuation

Prefix `i` now selects an existing workspace, moves the focused pane into a new tab there
and follows it. A read-only manager catalog returns ordered names and lifetime streams with
the server instance; it does not create or resolve workspaces. The controller retains the
source revision and destination lifetime, rejects mismatched catalogs, and cancels on source
changes. Escape during loading discards buffered input instead of replaying it after cancellation.
The CLI exposes the same catalog through `fux workspace catalog`.

Verification passed: 122 library tests, 59 ECS tests and 14 boundary/protocol/structure checks,
plus workspace and xtask formatting checks. The rebuilt real-viewer scenario passed the existing
workspace chooser flow, checking viewer following and unchanged original PID, followed by input
and detach. Catalog tests verify ordering, read-only effects and a different lifetime after a
workspace is deleted and recreated. Workspace and xtask all-target Clippy also passed with
warnings denied. This closes the existing-workspace keyboard chooser gap;
mouse cross-container controls, zor route rebinding, full layout persistence and the remaining
manual/performance/package/full-review acceptance gates remain outstanding.

## Drag capture correctness continuation

Fixed two capture failures found during review and real-viewer verification. A different mouse
button's release could commit a left-button drag, and cancelling a drag opened the command popup,
which swallowed the left release and left later mouse input suppressed. Captured gestures now
ignore other buttons, wheel and extended-button reports; only a valid left release completes or
ends suppression. Escape returns directly to the terminal. Releasing Alt before the left button
remains supported. Gestures also retain server and viewer identities and cancel on identity or
zoom changes, even if a layout revision happens to match.

Added controller regressions and an automated real-viewer sequence that compares exported
layouts before/after unrelated mouse events and cancellation, then exercises ordinary mouse
selection later in the same session. The initial viewer run caught the popup/suppression bug;
after its fix the full viewer scenario passed. Verification: 124 fux library tests passed,
16 controller tests passed after the final cancellation assertion, fux and xtask all-target
Clippy passed with warnings denied, and formatting/whitespace checks passed. These checks do
not replace the outstanding manual visual inspection or complete the remaining feature gaps.

## Pane drop preview continuation

Added a viewer-local cyan highlight on the destination half selected by Alt-left pane dragging.
The existing hint names the pane and insertion side. The compositor changes only display style,
keeps wide-character halves together, clips stale geometry to the local viewport and restores
the original display when the gesture ends. The highlight indicates the insertion side, not a
prediction of exact server geometry. Ordinary output frames carry no preview work when no drag
target exists. No reference code or new dependencies were used.

Verification passed: 125 library tests, including all four directions, wide-character boundaries,
tiny/zero viewports and exact rendering restoration; fux and xtask all-target Clippy with warnings
denied; formatting and whitespace checks. The rebuilt real-viewer scenario passed assertions for
the rendered cyan destination and its removal after cancellation, alongside existing gesture,
selection and process-transfer checks. Manual visual inspection, cross-container mouse controls,
route rebinding, full persistence and the remaining final acceptance gates are still outstanding.

## Mouse workspace destination chooser

Dropping a pane on the workspace-name area opens the manager-backed destination chooser. Clicking
a rendered destination row transfers the captured pane into a new tab and follows it; wheel
selection scrolls longer lists. The compositor supplies exact visible row regions and excludes
headings, scroll indicators and footers. The chooser retains server, source revision, destination
lifetime and viewer guards. Its click release is consumed rather than sent to the moved shell.
The source is the dragged pane, which may differ from the focused pane. Without a manager endpoint,
the operation reports its unavailable state and does not move anything.

Verification passed: 129 library tests, including nonfocused source identity, click-release
consumption and scrolled chooser hit regions; fux and xtask all-target Clippy with warnings denied;
formatting and whitespace checks. The expanded real-viewer scenario passed the complete drag,
workspace-name drop, chooser click, viewer follow, unchanged pane/PID, continuing input and detach
flow. Existing-workspace mouse transfer is now implemented. Route rebinding, full persistence,
manual visual inspection and the remaining final acceptance gates are still outstanding.

## Restoring exported zoom state

Complete layout export replies now restore their saved zoom state when applied through the CLI.
Previously the CLI discarded zoom and applied only the tree. Bare tree documents continue to
preserve zoom. The generic apply action has an explicit preserve/set zoom policy, with `null`
restoring unzoomed state. A saved zoom target is validated against the source document and mapped
through the same complete pane remapping as the tree. Tree and zoom commit atomically; unchanged
reapplication retains the revision and produces no PTY resize. Viewer-private focus is preserved.

Verification passed: five CLI/binary tests, 60 ECS tests and 14 boundary/protocol/structure checks;
workspace and xtask all-target Clippy with warnings denied; formatting and whitespace checks.
The boundary inventory was reviewed and refreshed for generic layout state and recent viewer
hit-testing declarations. The real pane-layout-transfer scenario saved a zoomed export, unzoomed,
restored it through the CLI and then transferred the same live pane, retaining PID/PTY/output.
Container ordering and selection persistence are still incomplete; this closes the zoom round-trip
gap, not the complete persistence requirement or the remaining final acceptance gates.

## Coherent container archive export

Added `fux workspace export-layout` and a versioned manager archive containing ordered open
workspaces, lifetime streams, default selected tabs, ordered tab IDs/labels/revisions, saved default
focus beneath zoom, shared zoom and complete existing-pane trees. Export uses one ECS request
phase, creates no containers/processes and explicitly rejects output beyond the manager frame
limit. Viewer-private selections remain excluded. The pinned Herdr source exports one tab and
its focus metadata; its apply path launches processes, which this task explicitly excludes.
Fux's archive provides the existing-process state needed for subsequent atomic restoration.

Verification passed: 129 library tests, five CLI/binary tests, 61 ECS tests and 14 boundary,
protocol and structure checks. The new archive test verifies order, default focus distinct from
zoom, deterministic serialization and absence of spawn/resize effects. Workspace and xtask
all-target Clippy, formatting and whitespace checks passed. The real pane-layout-transfer scenario
verified the archive CLI's instance, version, workspace and zoom state before its existing process
continuity checks. Archive apply is not implemented yet; container persistence remains incomplete.

## Atomic archive apply

Added `workspace apply-layout DESIRED_FILE --against EXPECTED_FILE` and the corresponding manager
operation. The complete current archive must match the expected state. Preflight validates versions,
instance/lifetimes, exact container membership, complete unique pane membership, bounded trees,
labels, focus/zoom targets, pending creation, viewport settlement and revision capacity. It builds
all replacement trees before committing. Live panes can change tabs inside their workspace; workspace
routes and processes remain unchanged. Apply restores workspace/tab order, labels, default selections,
trees and zoom. Viewers retain their selected tab and valid private focus; moved-away focus falls back
within its existing tab. An unchanged apply emits no resize or spawn effects.

Verification passed: 129 library tests, five binary tests, 62 ECS tests and 14 boundary/protocol/structure
checks. Added rollback cases for late invalid tree/focus/zoom, incomplete tabs and wrong lifetime;
stale expected archives reject, and a valid apply restores tab order/membership/selection with viewer
focus repair. Workspace and xtask all-target Clippy, formatting and whitespace checks passed. The
real CLI scenario restored a label and zoom via archive files, then retained the original pane's
PID/PTY/output through the existing transfer checks. The boundary inventory includes this generic
manager primitive. Container creation/removal and cross-workspace routes remain separate operations;
the combined expected/desired request is bounded by the manager frame limit. Wider acceptance,
route rebinding, manual inspection, performance/package gates and full-diff review remain outstanding.

## Archive focus review correction

Review found that archive apply used serialized node-array order when choosing a viewer's
fallback after its focused pane moved away. Valid flat documents can order nodes differently
from split traversal. A new regression reproduced the incorrect focus (pane 2 instead of the
restored tree's first pane 3). Apply now derives fallback membership order from the validated
tree. The regression also verifies archive-driven workspace reordering. All 63 ECS tests and
workspace all-target Clippy passed after the fix. This is a targeted review correction; full
diff review, route rebinding and remaining acceptance gates are still outstanding.

## Initial performance comparison and visual-inspection availability

Added a tracked `measure-layout` workload for matched legacy resize requests at 2/8/32 panes,
checking unchanged pane/PID identity. Built the baseline commit and current implementation in
locked release mode, then ran that workload plus the existing frame and real-viewer workloads
three times each, sequentially with alternating build order. All 18 measured runs completed;
harness Clippy and formatting passed. Failed protocol smoke runs were corrected before sampling
and are excluded. Raw samples, source/binary hashes, toolchain and host-load readings are retained
in [the performance report](pane-layout-performance-2026-09-12.md).

The candidate consistently used 989 median frame bytes per keystroke versus 852 at baseline
(+16.1%). Added identity/layout metadata is a likely contributor and needs byte-level follow-up.
Timing remains provisional because recorded host load was 26.42–33.24; the report does not count
this as a performance gate pass. Larger cell-resize workloads and quieter measurements remain.
The UI tool could not initialize because `CUA_REPL_ENABLED_SURFACES` was missing, so required
manual visual inspection remains unverified. Other implementation and acceptance work can continue.

## Mouse transfer between visible tabs

Alt-left-drag onto a visible tab label now transfers the existing pane beside that tab's first
pane. The label highlights in cyan and the hint names the destination. The compositor returns
the actual painted label regions, including truncated labels, for hit testing. A gesture retains
the source and destination revisions and cancels after destination changes or tab-bar movement.
Cancellation caused by a changed bar repaints immediately so idle panes do not leave a stale
preview visible. Transfers use the existing server operation and empty-source policy.

Verification passed: 126 library tests; 17 controller tests after the final test cleanup; fux
and xtask all-target Clippy with warnings denied; formatting and whitespace checks. The expanded
real-viewer scenario passed twice, including the final repaint change. It checks destination
highlighting, original pane/PID membership after the drop, empty-source removal and detach.
Only visible labels accept drops; tabs that do not fit remain reachable through the keyboard
chooser. Mouse workspace destinations and complete mouse access to overflowing destinations
remain unfinished, alongside route rebinding, full persistence and the final acceptance gates.

## Overflowing tab destinations

During a pane drag, wheel reports over a painted tab label now cycle through all eligible tabs
captured at drag start, including tabs whose labels are hidden. Down/up selects next/previous
with wrapping, excluding the source. The hint names the selected destination; release at the
same pointer position transfers there. Pointer movement restores direct hit testing. Wheel
reports over pane content, other buttons and cancellation tails retain their previous handling.
Hidden destinations receive the same revision/deletion checks as visible ones.

Verification passed: 127 library tests, including wheel direction/wrapping, hidden destinations,
pointer movement and destination deletion; fux and xtask all-target Clippy with warnings denied;
formatting and whitespace checks. The expanded real-viewer scenario passed an isolated long-label
case where the selected destination was absent from the bar and the drop retained the original
pane/PID. This closes mouse access to overflowing tab destinations. Workspace mouse transfers,
route rebinding, full persistence and the remaining final acceptance gates are still outstanding.

## Managed-task routing: current evidence and remaining transition

The manager location primitive provides the missing coherent route lookup. It does not authorize
silently substituting a new target: consumers must retain the server instance, pane ID and PID,
and distinguish current routing from immutable launch attribution. Manager-only discovery must
not broaden workspace-scoped gateway access.

The next implementation must address these existing contracts together before lifting the fixed
route restriction:

- `Target::identity` now uses server/pane/PID, independently of routing. New sessions record
  immutable launch `origin`, while retaining initial workspace/stream for request identity.
  Adopted-pane final evidence after prior movement is covered by the real-process scenario.
- `submit::request`, focus and stop now resolve the current route and retain exact process
  checks. Lookup-to-request races fail safely through scoped pane/operation checks; they do not
  trigger automatic input replay. Managed pre-attachment discovery remains source-route-bound.
- Workspace `input.rs::record` still scopes submission to the original workspace entity.
  The new manager `input-status` and zor reconciliation path now recover retained receipts after
  source retirement without broadening workspace access. Route-independent receipt reads are
  implemented, and task live requests now resolve current routing while retaining process identity.
- Transfers now fail unused reservations explicitly and preserve `input_sequence` for delivered
  prompts. Queued input still blocks movement. The old sequence-bump mismatch is fixed and tested,
  including movement away/back and zero-byte failure evidence. Zor wait/heartbeat share current
  route resolution with exact pane/process and accepted receipt identity. The native worker
  scenario now verifies movement, source retirement/reuse, completion and return movement.
- Adoption no longer takes `FixWorkspace`. Managed creation releases its temporary pin only after
  durable attachment, with lost-request/reply recovery coverage. Explicit pins remain protected.
  `zor run` now releases its pin and retains manager final evidence plus EOF-aware routed cleanup.

Required end-to-end evidence remains: managed/adopted pane movement through the normal controls,
continued prompt submit/status/wait/heartbeat after source retirement, route-name reuse, movement
back to the original workspace, concurrent/queued input, retained final evidence, and failure or
crash between route discovery, durable intent and input submission. Existing fixed-route rejection
tests must evolve into these successful workflows and their failure-boundary tests; they are not
proof of final parity.
