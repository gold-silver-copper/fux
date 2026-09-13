# Control-flow UX implementation and verification

## Final status

The scoped implementation and verification are complete. See
[final verification](control-flow-ux-final-verification.md) for the authoritative
results: 191 library tests, 67 harness tests, local 18/18 and automation 19/23;
four baseline zor routing fixtures remain failing. All 373 final integration and
39 supplemental Betamax frames were replayed and visually reviewed. The
[acceptance record](control-flow-ux-acceptance.md) and
[mode matrix](control-flow-mode-coverage.md) supersede pending-work statements in
the historical execution/audit entries below. No commit, push or PR was made.

The final review closed target-loss coverage gaps and verified exact q/copy/paste
receipts, including paste cancellation during buffer replacement. It found no
further confirmed in-scope defect after the popup auxiliary capture and exit-hint
fixes. Shared-handler coverage limits and unresolved unrelated failures are explicit
in the verification report. Historical test counts and pending statements below
apply only to their recorded snapshots.

## Historical record

Objective: execute `fix-control-flow-ux-prompt.md` in full. **In progress.** This
ledger is not a completion declaration. The transition contract is in
[control-flow-transitions.md](control-flow-transitions.md).

## Popup auxiliary-button review finding

The separate client diff review reproduced an additional U11/U15 defect: command
popups ignored middle/right presses without retaining their release. Dismissing
the popup or choosing Copy with the keyboard before release could forward an
unmatched release to an application. The new popup regression failed first
([before log](verification/control-flow-2026-09-12/popup-aux-before.log)).

Popup capture now records auxiliary buttons too. Every dispatched command, whether
chosen by pointer or keyboard, transfers outstanding popup capture to the controller.
Controller auxiliary capture records the button identity and consumes its motion
and release while allowing a fresh press to recover. The controller handoff test
covers both buttons across Copy entry and confirms a later app press is forwarded.
**103 client tests pass**, including the existing left-click, right-menu and layout
capture regressions ([log](verification/control-flow-2026-09-12/popup-aux-client.log)).
The real reporting-app scenario now checks both buttons across both Escape and
keyboard Copy entry, with exact next-input bytes and no release in either app log.
The rebuilt binary passes that real-app scenario. All 27 distinct frames replayed;
both contact sheets were inspected. Broader checks after capture changes passed:
187 library tests, 31 + 36 standalone harness tests, fux and harness all-target
Clippy with `-D warnings`, and formatting. The subsequent hint correction below
has its own newer client verification and still requires final integrated evidence.


Pending evidence from the preceding pass was resolved: binary build and standalone
harness Clippy passed. The retained-exited/buffer-order gallery replayed 26 distinct
states; both contact sheets were inspected. The corrected two-viewer resize gallery
replayed 47 states; all three sheets were inspected, including full-height restored
borders in both viewers and cleared selection after resizing. These are evidence
for the prior gesture/buffer snapshot, not the newer auxiliary-capture change.

The separate review has read all saved client diffs (controller, copy, input loop,
effects, popup, read window, rendering, screen, hints and context). It found the
auxiliary-capture defect above and an inaccurate shared-PTY comment, now corrected.
The new patch still needs a final review; harness/protocol integration and the
per-mode completion/failure coverage audit remain unfinished. No full-review
completion or whole-prompt completion is claimed.

## Visible exit-hint correction

The auxiliary-capture gallery exposed another confirmed issue: Copy's long thin
hint clipped `Esc finish` at both 40 and 80 columns. A composed-buffer regression
failed before the correction. Copy, selection, passive-history and immediate-layout
hints now put the exit instruction before secondary controls; the renderer
regression checks both Copy states at both widths. Two tab-chooser error strings
that still said `Esc returns to commands` now say `Esc dismisses`.
**104 client tests pass** after this change. Before/after logs are retained as
`exit-hint-before.log` and `exit-hint-after.log` in the verification directory.
Final library tests pass **188/188** and fux all-target Clippy passes after the
hint correction. Formatting and diff whitespace checks pass. Full PTY/visual
results for these latest strings are pending in the fresh v4 run.

The intervening combined v3 run is **not final acceptance evidence**: it began
with the pre-auxiliary-fix binary, and the local test executable subsequently built
the expanded fixture. Automation passed 19/23 (four known routing failures;
`zor_headless` passed this time), while local tests passed 17/18. Its only local
failure was the new auxiliary-popup assertion against the older binary, confirming
the app received an unmatched release. A rebuilt targeted run passes, as above.
The mixed fixture/runtime snapshot is recorded explicitly; neither that local
failure nor an intermittent headless pass should be misclassified. A fresh complete
run with an unchanged source/harness snapshot remains required.

## Current gesture, geometry and reply-order follow-up

Three additional defects were reproduced and fixed:

- U8/U15: a fresh left press after a lost layout release continued the old preview.
  Fresh presses now discard the uncommitted preview before hit testing; a new Alt
  press starts its own pane target. The regression verifies both ordinary focus
  and a new pane-2-to-pane-1 relocation rather than accidental pane-1 movement.
- U8/U15: canceled context menus retained right-button capture indefinitely when
  release was lost. Motion/release tails remain consumed; a fresh right press can
  open the new pane's menu normally.
- U12: a correlated history reply from another buffer could install before the
  corresponding live state arrived. `CopySession::install` now rejects a buffer
  mismatch, dismissing the incompatible view with a notice. Both primary/alternate
  directions have a regression; a controlled peer verifies reply-before-state,
  re-entry with an old primary reply, and a late reply after workspace stream change.

Current client tests: **100 passed**. Burst coverage sends 100 wheel steps while a
read is pending, requires one coalesced request, preserves intent against the old
clamp and verifies upper/lower clamps settle without reread loops.

Targeted real-PTY results:

- `viewer-history-controls`: lost layout/right-release recovery and selected
  history resize pass. Its first expanded gallery has 47 distinct frames, all
  three sheets inspected. A secondary-viewer restore checkpoint could precede
  its new frame; the fixture now waits for both viewers' full-height borders.
  This is an observation correction, not evidence of a persistent rendering bug.
- `viewer-mouse-app`: nested unequal heights, A → C → A history, correct B mouse
  coordinates, alternate-screen application mouse and no input leakage pass.
  All 17 distinct frames replayed; both sheets inspected.
- `viewer-history-delay`: buffer reply/state ordering and late workspace-stream
  replies pass (21 distinct frames replayed and inspected). Its newer extension
  also passes retained exited-pane browsing/removal, and rejected-control notice
  followed by exact ordinary input. Rendering of that extension remains pending.

Logs and reviewed sheets are retained in `docs/verification/control-flow-2026-09-12/`
under `gesture-recovery-before`, `buffer-reply-before`, `gesture-resize`,
`nested-app`, `buffer-order`, and `history-matrix` prefixes. Earlier combined-suite
and broad library counts below predate these runtime fixes. Final full checks,
transition coverage review and separate complete-diff review remain required.

## Latest combined integration run

All scenarios ran with capture and no exclusions in
`target/betamax-ux-full-v2`: **local_cli 18 passed, 0 failed**;
**automation_integration 18 passed, 5 failed**. The five failures are the four
previously documented routing fixtures (`zor_groups`, `zor_group_scheduler`,
`zor_service`, `zor_recovery`) and the recurring `zor_headless` pane-pin release
failure (`live pane not found`). The earlier isolated headless retry passed;
this new full-run recurrence must not be called resolved or an all-green suite.
No unrelated routing or pin-release implementation was changed.

Command (with documented Zig PATH, CJK font, fresh FUX_BETAMAX_DIR,
ZOR_BIN=target/debug/zor, FUX_REQUIRE_ZOR_BIN=1, deadline scale 3):
`cargo +stable test -p fux --locked --no-fail-fast --test local_cli --test automation_integration -- --test-threads=1 --nocapture`.
The process exited 101 because automation failed. Local tests took 69.57 seconds;
automation took 262.01 seconds. Full log:
[combined-suite-v2.log](verification/control-flow-2026-09-12/combined-suite-v2.log).
Combined replay indexed **337 checkpoints** successfully. Full fux library tests
also passed **180/180**. All 22 contact sheets (337 captured states) were inspected, including tiny
geometry, secondary viewers, delayed operations and real application routing.
The review found context menus still labeled `Esc back`; the shared menu footer
and representative hint fixture now say `Esc dismiss`. The fresh broad-viewer run passes; its 207 checkpoints replay exactly in
`target/betamax-ux-menu-hint-v1/index.html`. The affected pane, tab and workspace
menu PNGs were inspected at full size and show `Esc dismiss` without clipping.
All 96 client tests and fux all-target Clippy pass after the label correction. Remaining acceptance-matrix work continues. The source of the recurring headless failure is consistent with
a rapid-exit pin-release race: the argv fixture exits immediately, while launch
attachment calls `release_pin` after recording a PID and the manager requires a
live/EOF pane. This is a source inference, not a controlled race reproduction;
it concerns the launch/manager path rather than client input routing. No fix or
resolution is claimed.

## Latest continuation: pending destination and selection recovery

U14/U15 regressions reproduced both findings before implementation (47 controller
tests passed, 2 failed). Both now pass; the full client subset is **96 passed**.
`loading_interaction` centralizes the three pending owners so destination-loading
mouse input is handled immediately and removed from replay. A fresh left press
ends stale selection capture before normal hit testing and starts a new anchor.

Commands run successfully:

- `cargo +stable test -p fux --locked --lib client::`
- `cargo +stable clippy -p fux --locked --all-targets -- -D warnings`
- `cargo +stable build -p fux --locked --bin fux`
- Standalone harness build with `--features betamax --locked`.
- `fux-xtask scenario viewer-manager-delay target/debug/fux`, capture enabled.
- `fux-xtask betamax-report target/betamax-ux-u14-manager-v1`: 31 checkpoints;
  both labeled contact sheets inspected, including the five new destination states.

Evidence is retained under `docs/verification/control-flow-2026-09-12/` as
`u14-u15-before.log`, `client-progress.log`, `clippy-progress.log`,
`destination-manager-progress.log`, `destination-manager-replay.log`, and
`destination-manager-sheet-01.png` / `-02.png`.

The current requirement-by-requirement completion checklist is
[control-flow-ux-acceptance.md](control-flow-ux-acceptance.md). Earlier whole-suite
results below predate these fixes and are historical evidence.

## Real application mouse and screen-buffer coverage

Added `viewer-mouse-app` to the native scenario registry and `local_cli` tests.
Its Rust fixture runs in raw mode, enables SGR/all-motion reporting, records exact
received bytes and switches primary/alternate buffers on pane-addressed input.
The passing scenario proves application wheel coordinates/bytes in B while A
stays scrolled; Shift interception in both panes; exact focused typing; unrelated
history retention; invalidation on alternate entry and primary return; Escape
without application leakage; and fresh app press/release after a lost selection
release. No application dependency or production protocol was added.

`target/betamax-ux-mouse-app-v2/index.html` replays 12 distinct rendered states;
the contact sheet was inspected, including simultaneous histories, buffer changes,
selection and final normal mode. The first run's final marker was hidden under
the retained history hint; moving the fixture marker to row zero resolved that
observation error without a product change. The report does not claim coverage
of every reply/state ordering, nested geometry or other gesture types.

The standalone Betamax suite passed with the documented CJK font (31 library +
36 binary tests). An earlier invocation omitted `FUX_BETAMAX_FONT` and failed the
existing wide-glyph test; that environment error is retained separately rather
than attributed to this code. Evidence: `mouse-app-progress.log`,
`mouse-app-replay.log`, `mouse-app-sheet.png`, and `harness-tests-progress.log` in
the verification directory. Full local/automation acceptance still needs a fresh
run after these additions.

## Current implementation

- Passive wheel histories are separate from explicit keyboard copy mode, retained
  independently for visible panes, scoped to attachment/workspace identity and
  capped at 64 snapshots. Composition accepts multiple private history views.
- Normal input restores only the focused pane. A disambiguated Escape is distinct
  from literal/pasted bytes and dismisses the most recently manipulated history.
- Cancellation no longer uses a boolean request to reopen commands. Copy Escape
  clears selection and finishes in one press; repeated layout edit hints say finish.
- Captured selection releases outside the original pane reach their owner. Copy's
  byte parser can forward another application's mouse events to the server.
- History reads use a separate bounded window with fixed deadlines, exact reply
  identity, cancellation cleanup and fair selection of panes. Local history input
  no longer waits on an outstanding history read. Desired offsets survive older
  replies; geometry changes request a fresh viewport even at an unchanged offset.
- Control requests receive unique IDs and fixed deadlines; unrelated replies cannot
  clear the outstanding control request. Expiration and Escape timers precede
  state traffic in the biased select loop.
- Workspace lookup results carry an interaction epoch; canceled/reopened dialogs
  cannot receive an older result or its buffered input. Read-only manager catalogs
  no longer gate normal input. In-flight jobs are bounded.

## Scheduling continuation

The global input-consumption gate has been removed. `client/effects.rs` now
orders bounded external effects separately from local parsing. Control replies
still correlate exact IDs and retain send-time deadlines. History reads continue
on their separate lane while a control request is outstanding. Commands requiring
an acknowledged frame wait in an explicit cancellable state; buffered suffixes
resume only into an accepted command. Layout continuations retain their hint and
wait for a fresh revision. Unexpected workspace identity changes discard waiting
commands; an explicitly requested workspace navigation can carry its suffix to the
new destination.

Manager-delayed input is locally checked against its original workspace/focused
pane and encoded byte-exactly into a server-side pane-addressed `SendKeys` request.
This avoids retargeting when the manager reply arrives before the attachment's
updated frame. Target changes discard queued input with a notice. The new unit
regression includes NUL, Escape, invalid UTF-8 and literal backslash bytes.
The controlled delayed-manager fixture now verifies this path; see the newer
progress section below. Final acceptance still requires the remaining matrix.

The extended delayed-peer scenario passes withheld control + history, immediate
Escape, canceled dependent rename, stale control acknowledgment, exact deferred
input, and successful dependent-command text replay. Exact Betamax replay matched
15 checkpoints in `target/betamax-ux-effects-delay-v3/index.html`; its contact
sheet and direct cursor-state/pixel checks were inspected. A suspected hidden
cursor artifact was rejected: the visible cursor belonged to an adjacent normal
state. No renderer patch was necessary. A small raster visibility regression
passes, including alternate/synchronized output.

[Delayed control log](verification/control-flow-2026-09-12/delayed-control-progress.log),
[replay](verification/control-flow-2026-09-12/delayed-control-replay.log),
[contact sheet](verification/control-flow-2026-09-12/delayed-control-sheet.png).
The broader viewer scenario also passed with the scheduler
(`target/betamax-ux-effects-viewer-v3`); its raster report remains pending.
These PTY runs predate the final manager-input targeting and waiting-scope guards,
so fresh acceptance runs remain required.

## Earlier manager, transition and broader-suite progress

A task-owned manager proxy now delays individual Catalog/Reorder replies while
ordinary RPCs and the real attachment continue. `viewer-manager-delay` passes:
lookup cancel/re-entry with an old failure reply; pane history while lookup and
mutation replies are held; one-Escape dismissal; canceled dependent rename;
original-pane input after success; discarding held input when that pane closes;
command-popup dismissal while waiting; and failure notices followed by working
ordinary input. The successful input checks expect the two terminal lines from
PTY echo plus `cat`, not extra duplicated deliveries.

The fixture first exposed an accepted-socket nonblocking bug, then reproduced a
product defect: LoadingWorkspaces swallowed wheel events. Loading now routes
local history and removes processed mouse sequences from buffered dialog input.
Workspace identity changes also cancel transient lookup modes and invalidate their
interaction epochs; explicit waiting navigation retains its separate scope rule.

The manager gallery at `target/betamax-ux-manager-delay-v4/index.html` replays all
**26 checkpoints** exactly. Both contact sheets were reviewed: independent history,
lookup replacement, target-loss notice, popup dismissal and manager-failure notice
match the behavioral assertions. [Scenario](verification/control-flow-2026-09-12/manager-delay-progress.log),
[replay](verification/control-flow-2026-09-12/manager-delay-replay.log),
[sheet 1](verification/control-flow-2026-09-12/manager-delay-sheet-01.png),
[sheet 2](verification/control-flow-2026-09-12/manager-delay-sheet-02.png).

A new matrix covers Escape across 22 transient action entries. Parser coverage now
splits commands, literal prefixes, CSI/SS3, Alt, SGR mouse, UTF-8 and paste at every
read boundary for control, printable and Escape prefixes. A separate field-pointer
regression first failed for RenamePane: outside clicks were ignored. Text fields
now use painted bounds to distinguish inside clicks from outside dismissal and
retain release ownership. Headings/padding are not actions. The expanded
`viewer-history-controls` scenario passes these real pointer transitions
(`target/betamax-ux-field-controls-v1`; replay/review still pending).

All **176 library tests** passed before the last field-pointer fix; the latest
client-only run passes **94 tests**, with fux all-target Clippy passing. Standalone
Betamax harness tests passed **31 library + 36 binary tests**, and its all-target
Clippy passed after correcting two lints in the new fixture code. The full local
suite ran all **17 scenarios** with capture: **16 passed, 1 startup timeout** in
control-workflow. Other Cargo/Rust builds were active at that point. The failed
scenario then passed unchanged on an isolated retry; this is retained as a
transient failure, not silently converted into an all-green full run.
[Suite log](verification/control-flow-2026-09-12/local-suite-progress.log),
[retry](verification/control-flow-2026-09-12/control-workflow-retry.log).

All 23 automation scenarios ran with capture in
`target/betamax-ux-automation-full-v1`: **18 passed, 5 failed**. Four match the
previously documented routing-fixture failures (groups, group scheduler, service,
recovery). `zor_headless` additionally failed with "live pane not found" during
pin release, then passed unchanged on an isolated retry. Retain the occurrence as
a transient failure; the full run is not green. No unrelated routing or pin-release
code was changed. [Full log](verification/control-flow-2026-09-12/automation-suite-progress.log),
[headless retry](verification/control-flow-2026-09-12/headless-retry.log). The loading transfer chooser and core reporting-app/Shift/buffer gaps from
that snapshot are addressed by the newer sections above. Updated
bindings and a step-by-step manual section are in the pane control guide/checklist.

## Earlier evidence snapshots (superseded where newer results exist)

Continuation update: compilation has been restored after the private viewport
edit. The client suite now passes **94 tests** and fux all-target Clippy passes
with warnings denied. This includes new regressions for popup-to-copy capture
handoff, recovery after a lost release, same-offset private viewport resizing
without a shared PTY resize, and buffer-switch invalidation with an old reply.
Existing canceled-drag release coverage caught a cleanup regression during this
work; it was fixed without weakening that assertion. A subsequent review found
that selection cleanup could discard drag ownership before the release arrived.
Escape, `q`, clear and wheel now preserve release suppression; reply/resize
invalidation does likewise. A matrix test checks release and the next fresh drag.

Popup actions now transfer their initiating left-button capture to the controller
before switching parsers. Releases cannot start a copy selection. Primary versus
alternate buffer changes discard incompatible private histories, and explicit
copy also dismisses when its pane is no longer visible. These targeted results
do not complete the real-PTY or visual acceptance matrix.

New copy regressions first failed for Escape, same-offset geometry refresh and
newer scroll intent. New controller regressions first failed for passive wheel
ownership and the popup return path. Retained logs:
[copy before](verification/control-flow-2026-09-12/copy-regressions-before.log),
[controller before](verification/control-flow-2026-09-12/controller-regressions-before.log).

Latest completed targeted command:
`cargo +stable test -p fux --locked --lib client::` — **94 passed**
([log](verification/control-flow-2026-09-12/client-progress.log)). This includes
identity invalidation, stale session/lookup replies, outside release, application
mouse forwarding, history-window bounds and fixed deadlines. It does not prove
the entire requested matrix.

The expanded `viewer-history-controls` real-PTY scenario now passes numbered
history in two panes, A → B → A, Escape, focused typing, explicit selection,
prefix, a second viewer, shrink/tiny/restore, and command-popup click, wheel and
outside dismissal. Betamax exact replay matched **33 checkpoints**. All three
contact sheets were inspected: simultaneous histories, cursor/hint separation,
selection dismissal, nested borders, popup scrolling and tiny-size restoration
look consistent with the asserted states. This covers only that scenario.

Current progress gallery: `target/betamax-ux-controls-expanded-v3/index.html`.
[Scenario log](verification/control-flow-2026-09-12/history-scenario-progress.log),
[replay](verification/control-flow-2026-09-12/history-replay-progress.log),
[contact sheet](verification/control-flow-2026-09-12/history-sheet-01.png).
The resize fixture initially assumed that the largest viewer controls PTY size.
Source inspection disproved that: `resolve_layout` chooses the smallest viewer,
and vt100 keeps top rows on shrink. The fixture now checks the correct rows,
offset and dimensions. The separate unit test verifies a geometry refresh is
requested at an unchanged offset and does not loop.

The controlled `viewer-history-delay` fixture also passed in a previous isolated
run (`target/betamax-ux-delay-v4`): cross-pane progress while a read is withheld,
exact input bytes, stale re-entry replies and a fixed timeout under continuous
state traffic. Its raster report and a final rerun remain required. Initial
fixture attempts blocked while waiting without draining the PTY; the incremental
reader now pumps output while receiving framed messages.

The broader `viewer` PTY scenario also passes after replacing two superseded
Escape-to-commands assertions (rename and workspace chooser). Rename cancellation
now verifies the next ordinary input. Its capture is at
`target/betamax-ux-viewer-current-v2`; replay and image review remain pending.
Both successful PTY galleries predate the final selection-release cleanup and
need final reruns. [Viewer log](verification/control-flow-2026-09-12/viewer-scenario-progress.log).

Run timing-sensitive PTY scenarios without overlapping compilation; no product
startup deadline was increased. Final whole-suite and harness verification remain.

## Remaining completion gates

Use [the acceptance checklist](control-flow-ux-acceptance.md) for the current
requirement-by-requirement gaps. The new application fixture covers core mouse
routing and buffer transitions; remaining cases include nested/unequal geometry,
adversarial reply/state ordering, selected resize and full transient/gesture
permutations. Final combined suite replay/review and separate full-diff review
remain mandatory. Known zor fixture failures must be reported without exclusions.

The current rebuilt runtime includes the previous heading/padding ownership guard
and passive-history-preserving drag repaint. The U14 manager and mouse-app galleries
include those corrections; older galleries do not establish their acceptance.

## Separate review continuation

The review additionally read `Request::set_id` (exhaustive variant matching), the
entire task-owned manager delay proxy, the complete controlled history peer, the
raw mouse worker and the new auxiliary-popup scenario branch. No additional
confirmed defect was found in those reviewed portions. Assertions check exact
request/input order, bounded reads/waits, stale ID rejection, fixed history timeout
under state traffic, and task-owned process/socket cleanup. The rest of the
viewer scenario diff and the per-mode coverage gaps remain to be reviewed; this
is not a completed full-diff gate.

See [mode-by-mode coverage](control-flow-mode-coverage.md) for explicit remaining
assertion gaps. The fresh unchanged-snapshot combined run uses
`target/betamax-ux-final-v4` and `/tmp/fux-ux-final-v4.log`; inspect its terminal
result before treating it as evidence. The source hashes at launch are in
`target/control-flow-ux-review/after-aux-hint-manifest.json`.
