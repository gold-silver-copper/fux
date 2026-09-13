# Control-flow UX audit — 2026-09-12

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

The two reported problems were confirmed in the starting control flow. Scrolling
creates one viewer-wide copy session, which rejects mouse events targeting any
other pane. Escape ends that session through a generic cancellation path that
requests the command popup. These are interaction-model defects, not rendering
failures. The earlier Betamax image review did not establish that untested
transitions were correct.

This records the starting source audit and its client test baseline. Implementation
is now in progress; see [the implementation ledger](control-flow-ux-implementation.md)
for current evidence and unfinished requirements. The findings below are historical
starting-point evidence, not a claim that every path still has its original behavior.

## Latest audit-only review of the current checkout

The original reports remain valid findings about the starting implementation.
They are **not both still reproducible in the current client unit model**: this
checkout already contains substantial fixes. This review changes documentation
only and does not certify that the installed executable includes those changes.

| Area | Current source disposition | Remaining proof |
| --- | --- | --- |
| U1–U3: one pane owns scrolling, Escape opens commands, unrelated input captured | `histories`, `dismiss_history`, `resume_input` and explicit copy cancellation separate these responsibilities; current regression suite passes | Final binary/PTY transition and viewer-isolation acceptance |
| U4/U9/U13: inconsistent cancellation, misleading layout hints, outside text-field clicks | Explicit cancellation returns to Pane; layout/menu hints distinguish finish/dismiss; field bounds govern outside clicks | Complete per-mode exit/completion/failure matrix |
| U5/U6: blocked local input, stale replies, moving deadlines and stale scroll intent | Separate bounded read/effects lanes, fixed deadlines, request identity and sent-versus-desired offsets exist | Final sequencing, fairness and target-scope review |
| U7/U12: stale geometry and incompatible buffers | Viewport refresh and selection invalidation exist; `install` rejects opposite-buffer replies even before the live frame arrives | Fresh affected gallery and final integrated run |
| U8/U11/U15: gesture tails and lost releases | Popup handoff, fresh-press recovery and capture of locally ignored right presses exist | Verify release ownership after keyboard mode changes against the final binary |
| U10/U14: parser/overlay boundaries and loading pointer replay | Byte-boundary tests and shared `loading_interaction` policy exist | Cross-mode matrix and delayed-operation acceptance |

Fresh verification for this documentation pass:
`cargo +stable test -p fux --locked --lib client::` — **101 passed, 0 failed,
84 filtered out**. This includes independent history/dismissal, lost-release
recovery, opposite-buffer reply rejection and ignored-right-press handoff tests.
No fresh full integration run or Betamax visual inspection was performed in this
pass. Earlier PTY/gallery results remain recorded evidence for their snapshots,
not a claim about every subsequent edit. The implementation ledger records
18/18 local and 18/23 automation tests in its last combined run; five automation
failures and final review/coverage work remain unresolved.

The executable [fix prompt](../fix-control-flow-ux-prompt.md) now starts from this
state rather than asking an implementer to recreate completed partial fixes.
The historical sections below explain why each finding was raised; their old
counts and present-tense descriptions do not override this disposition table.

### Subsequent execution review: auxiliary capture and visible exits

The implementation review subsequently reproduced two more concrete defects:
command-popup middle/right presses lost release ownership across dismissal or a
keyboard command, and Copy's exit hint was clipped at ordinary terminal widths.
The fixes and before/after evidence are in the implementation ledger. The newest
client suite passes **104 tests**; final integrated verification is still pending.
Historical U11 references to `take_left_capture` are superseded by `take_capture`
and `adopt_popup_capture`, invoked for keyboard as well as pointer commands.

## Scope and evidence

### Current-checkout follow-up

The checkout now contains substantial unfinished implementation. This follow-up
is a source audit and prompt update, not an implementation completion claim.
The original U1–U10 below retain the evidence that motivated the work.

- **U1–U3: partial fixes present.** `Controller::histories`, multiple local views,
  `dismiss_history`, and `resume_input` now separate passive scrolling from
  keyboard copy. The generic `back` route has been removed. These changes must be
  preserved and verified, not reimplemented from the historical description.
- **U4/U8/U10: popup capture handoff now implemented (U11, P2).**
  `Popup::take_left_capture` transfers capture to
  `Controller::suppress_gesture_tail` before dispatching the clicked command.
  The controller now accepts a fresh press after a missing canceled-gesture
  release. `popup_capture_survives_parser_handoff_and_recovers_without_release`
  passes in this audit run. This supersedes the earlier source finding that
  capture had no handoff. Active selection recovery is a separate concern (U15).
- **U5: scheduling implementation now separates effects from local parsing.**
  The former global gate has been replaced by a bounded ordered effects queue
  and cancellable waiting commands. Delayed-control and delayed-manager PTY coverage now pass;
  final sequencing/scope and visual acceptance remain required.
  See the implementation ledger for exact evidence and outstanding work.
- **U6/U7: correlation and geometry work is partial.** Request IDs, read windows,
  retention bounds and local viewport cropping exist. Test unequal viewer sizes
  including same-offset resize, late replies, tiny layouts and hidden-pane eviction.
  `resolve_layout` negotiates the smallest attached viewer's size; a larger second
  viewer does not keep the canonical PTY large. A fixture assumption to that effect
  was rejected by inspecting the layout implementation. Existing code is not acceptance evidence.
- **Buffer invalidation now implemented (U12, P2).**
  `CopySession::same_buffer` compares `alternate_screen`; `Controller::reconcile`
  drops incompatible passive histories and dismisses incompatible copy sessions.
  `buffer_switch_discards_history_and_ignores_old_buffer_reply` passes in this
  audit run. Actual mouse-reporting application coverage across primary/alternate
  transitions and reply/state ordering is still required; unit coverage is not
  evidence of every end-to-end ordering.

- **U13 — Text-field outside clicks were ignored (P2, reproduced and fixed).**
  Rename/create fields lacked painted bounds in controller hit testing and fell
  through to Ignore. The new regression first failed on RenamePane. Controller
  hit testing now retains panel bounds, ignores inside field/header clicks and
  dismisses outside clicks while consuming their releases. The expanded real-PTY
  history-controls scenario passes the rename heading/outside/next-input sequence.
  Final whole-suite replay/review is still required.

### Remaining findings from this audit pass

**U14 — Loading destination chooser has different pointer behavior (P2,
reproduced and fixed; final full-suite acceptance pending).**
`Controller::mouse` sends every `Mode::Destination` through the chooser branch,
but its inner match accepts only `loading: false`. During loading, wheel and
outside clicks return `Ignore`; an outside press sets suppression without
canceling the dialog. The later responsive-loading routes include
`WaitingCommand` and `LoadingWorkspaces`, but omit the destination mode.

There is a second consequence: `Controller::feed` buffers bytes for all three
loading states, yet removes processed mouse sequences only for the first two.
After `destinations_loaded`, the main loop replays `take_loading_input` into the
ready chooser. A wheel ignored while loading can therefore move its selection
later; an outside press can dismiss it only after loading completes. Reproduce
with a withheld Catalog reply, wheel/outside input, and release/re-entry before
calling this visually verified. Pending and ready dialog ownership should never
silently reinterpret the same gesture at different times.

**U15 — Active selection does not distinguish a new press from continuation
(P2, reproduced and fixed in client regression; real-app acceptance pending).**
The first copy capture branch in `Controller::mouse` accepts all left-button
non-wheel events while `copy.dragging()`, including a fresh press. `CopySession::drag`
replaces the anchor only when `dragging` is false. If the terminal loses release,
a new press retains the old anchor and can remain attached to the old pane.
The canceled-gesture recovery test does not cover this active-capture path.
Add press A → motion → omitted release → fresh press B → drag → release; assert
the new gesture follows the documented pane policy and never extends A's old
selection accidentally. Apply the same recovery review to layout/right-button
capture, validating those paths before adding findings.

### U14/U15 implementation follow-up

Both dedicated controller regressions failed before the changes. U14 now uses
one `loading_interaction` predicate for buffering, consumed-mouse removal and
pending pointer routing. The ready-chooser branch excludes loading destinations.
The delayed-manager PTY scenario now passes pending destination wheel/outside
cancellation, history retention, re-entry and an old successful reply. U15 now
clears active selection capture on a fresh left press before normal hit testing;
same-pane and Shift cross-pane new anchors pass. Actual application routing and
other gesture kinds remain in the acceptance matrix.

Current client verification: **96 passed**, all-target fux Clippy with
`-D warnings` passed. Betamax replay indexed **31 checkpoints** in
`target/betamax-ux-u14-manager-v1/index.html`; both labeled contact sheets were
inspected. Destination dialogs dismiss cleanly, retained history is visible,
ordinary input resumes, and error notices do not open commands. This is targeted
progress, not full prompt completion. See retained
[before regressions](verification/control-flow-2026-09-12/u14-u15-before.log),
[client result](verification/control-flow-2026-09-12/client-progress.log), and
[PTY result](verification/control-flow-2026-09-12/destination-manager-progress.log).

The subsequent native `viewer-mouse-app` PTY regression now passes actual SGR
mouse routing, Shift override, primary/alternate invalidation, exact input and
fresh application press/release after a lost selection release. Its 12-state
Betamax replay/contact sheet was inspected. This adds end-to-end evidence for
U3/U12/U15; adversarial reply ordering and the remaining transition matrix still
require completion. See the implementation ledger for commands and limitations.

The full combined gallery review inspected 337 states across 22 contact sheets.
A remaining U4/U9 hint mismatch was visually confirmed: pane/tab/workspace context
menus displayed `Esc back` even though cancellation returns to normal. The shared
`context::Menu::panel` footer now says `Esc dismiss`; its matching hint fixture was
updated. The v2 combined gallery intentionally retains the pre-correction evidence.
The fresh broad-viewer scenario passes and all 207 checkpoints replay. The
affected pane/tab/workspace menu PNGs were inspected at full size: each shows
`Esc dismiss`, with correct placement and no clipping. Client tests (96) and
all-target Clippy pass after the correction.

### Further U8/U12/U15 disposition

New regressions confirmed that active layout previews survived a lost release and
captured right-button tails survived menu cancellation. Both are fixed: a fresh
left press abandons the old uncommitted preview before normal routing, and a fresh
right press can open a new menu after cancellation. Real PTY coverage verifies
preview removal without layout mutation and the new menu's pane target.

A separate U12 regression confirmed that matching the request ID alone allowed a
reply from the opposite screen buffer to install before its live frame. Installation
now checks buffer identity too. The controlled peer covers reply-before-state,
old replies after re-entry, late replies after workspace-stream changes and retained
exited-pane availability. The real reporting app now also covers nested unequal
geometry and mouse routing while its alternate buffer is active. See the newest
implementation ledger section for current results and remaining full-review gates.

### Verification for this audit-only pass

Ran `cargo +stable test -p fux --locked --lib client::` on the current checkout:
**94 passed, 0 failed, 84 filtered out**. This confirms the current client unit
baseline, including independent history, one-press Escape and popup handoff tests.
No production code was changed in this audit pass. No fresh PTY/Betamax or full
suite run was performed for this documentation update. Existing progress logs
remain historical evidence with the limitations described in the ledger.

The follow-up's initial verification attempt,
`cargo +stable test -p fux --locked --lib client::`, **failed to compile**, E0596 in `Controller::enter` at `controller.rs:739`: `copy`
is immutable but passed to mutable viewport methods. No tests ran in that attempt. Subsequent implementation restored compilation:
94 client tests and all-target Clippy now pass. Full acceptance is still pending;
see the implementation ledger. Older passing counts below are historical.

Inspected the input loop and outstanding-request handling in
`crates/fux/src/client/mod.rs`; `input.rs`, `controller.rs`, `copy.rs`, `render.rs`;
command availability in `crates/fux/src/commands.rs`; and viewer scenarios in
`tools/xtask/src/scenarios/viewer.rs`. Symbol references below are more durable
than line numbers in this dirty working tree.

Ran `cargo +stable test -p fux --locked --lib client::`: **62 passed, 0 failed**
([retained log](verification/control-flow-2026-09-12/client-tests.log)).
That passing baseline has no regression requiring wheel A → wheel B → wheel A
or scroll → Escape → ordinary pane input. Existing controller tests even require
`take_back()` after some cancellations. Passing those tests preserves the old
behavior rather than establishing the desired UX.

## Flow at the audit starting snapshot

1. Raw bytes enter a pending queue in `client::run`.
2. An outstanding control/history request gates consumption of that queue.
3. `Controller::owns_input()` chooses between two separate byte parsers:
   `Controller::feed` for local modes and `PrefixFilter` for normal/command input.
4. Mouse hit testing, selection, menus and gestures run inside the controller.
5. `Controller::take_back()` causes `PrefixFilter::show_commands()` in the loop.
   Failed control replies also call `show_commands()` directly.
6. One `CopySession` supplies one `LocalView` replacement to the renderer.

Ownership is therefore distributed across mode, prefix, paste/escape buffers,
mouse-capture flags, pending requests and the `back` flag. A screen can look
correct while the next event goes to an unexpected owner.

## Findings at the audit starting snapshot

### U1 — Cross-pane scrolling is explicitly rejected (P1, confirmed)

`Controller::mouse` creates `Mode::Copy` only from `Mode::Pane`. Once copying,
`copy.pane() != entry.pane` returns `Ignore`. There is no second session to activate
or preserve. `Controller::local_view` and `render::compose` also support only one
history replacement.

Reproduction: generate history in two visible panes; wheel up over A; move the
pointer to B and wheel up; return to A. B cannot be browsed until the current
copy session is exited. Fix both routing and the representation of pane-local
history; merely replacing the current session loses A's browsing position.

### U2 — Escape opens commands instead of returning to normal input (P1, confirmed)

`resolve_escape` calls `CopySession::key(Escape)`, then `cancel` when the session
finishes. `cancel` usually sets `back = true`; the outer loop calls
`filter.show_commands()`. In contrast, `copy_key(Quit)` and a successful copy set
`Mode::Pane` directly. With an anchor, the first Escape only clears the selection.
The same conceptual exit therefore has different destinations and key counts.

Required behavior from the user: a single Escape closes scrolling and returns to
normal input. No command popup and no leaked Escape to the application.

### U3 — Scrolling captures unrelated keyboard and mouse actions (P1, confirmed)

`owns_input` treats every `Mode::Copy` as modal. `Controller::key` ignores ordinary
unmapped characters, including the normal Ctrl-A prefix. The `self.in_copy()`
term makes mouse events local regardless of the target application's mouse mode;
events over another pane are then ignored. Right-click menus and layout drags
are entered only from `Mode::Pane`. Scrolling an unfocused pane does not issue a
focus request, yet captures keyboard input globally. This can leave focus on A
while the controller is interpreting keys for B's history.

Separate passive wheel browsing from keyboard selection/navigation. Define focus,
keyboard owner and pointer target independently, and preserve application mouse
routing on panes that are not participating in a local gesture.

### U4 — Cancellation policy depends on flags and entry path (P2, confirmed)

`cancel` infers its destination from `was_menu`, `was_dragging`, `captured_right`
and `suppress_mouse`. Mouse handlers sometimes override `back` afterward.
Keyboard `n` in a close dialog calls generic cancellation; pointer cancellation
has additional overrides. Rename and workspace-load failure tests explicitly
expect a return to commands. `ServerMessage::Reply::Failed` opens commands even
when the request originated in a mouse action or a still-active local mode.

Use explicit dismiss, finish, back and failure transitions. Preserve gesture-tail
suppression, but do not let it decide navigation. Error display must not silently
acquire command input ownership.

### U5 — A history read blocks dismissal and other local interactions (P1, confirmed structure)

The input loop runs only while `outstanding.is_none()`. A view request sets
`Outstanding::View`, preventing even local Escape and new wheel input from being
parsed until its reply arrives. Manager mutations also gate this loop. Fast wheel
bursts become serialized read/response cycles. Actual user-visible latency under
delayed replies still needs a controlled fixture.

Also, `request_deadline` is recomputed as `now + FRAME_TIMEOUT` each loop turn.
Unrelated incoming state/input events can keep extending it. This is a liveness
bug in the timeout construction; reproduce it with continuing state traffic and
a withheld reply. Keep local dismissal responsive without reordering application
input or mutations.

### U6 — History-session assumptions are unsafe to carry into the fix (P1 follow-up risks)

Each new `CopySession` resets `next_request` to 1. Reply matching uses pane and
request only. Current serialization limits overlap; making dismissal and pane
switches responsive must not introduce old-reply acceptance after re-entry.
Use attachment/session-scoped request identity and exact outstanding correlation.

`CopySession::install` overwrites `wanted_offset` when a reply differs from it.
If wheel input is allowed during an in-flight read, an older reply can erase a
newer desired offset. Track the offset actually requested separately from the
latest desired offset before adding coalescing.

`reconcile` validates copy state by pane presence, unlike workspace dialogs that
also validate instance/stream/viewer identity. Persistent pane history must be
scoped to attachment/workspace identity, bounded, and explicitly invalidated on
pane removal, transfer, reconnect and identity changes. Do not treat a numeric
pane ID as globally unique.

### U7 — History geometry can remain stale after resize (P2, confirmed structure)

`CopySession::refresh_live` returns immediately when its offset is nonzero.
`take_read` requests nothing if desired and displayed offsets match. Consequently,
a resized frame alone does not refresh an already-scrolled history viewport's
geometry. Rendering clips it into the new rectangle. Reproduce resize while
scrolled and while selecting; refresh history at the new dimensions and visibly
clear invalid selections. Existing tiny-size coverage does not prove this case.

### U8 — Selection capture has incomplete termination semantics (P2, confirmed paths)

`CopySession::drag` remembers `dragging`. Escape clearing an anchor and `scroll`
clearing a selection do not reset that flag. Releases outside the content or over
a different pane can return before reaching `drag`. A subsequent gesture can
therefore inherit capture state. Confirm exact visible outcomes with press →
move outside → release → new drag sequences. Capture ownership must survive
pointer movement and terminate on release, cancellation or invalidation.

### U9 — “Cancel” is misleading for already-applied layout edits (P2, confirmed)

`Mode::Layout` sends a mutation for each direction key. Escape calls `cancel` but
has no rollback; the hint says `Esc cancel`. Prefer immediate edits with an honest
`Esc finish` label. A rollback contract would require a separately designed
transaction and stale-layout policy; do not invent partial undo.

### U10 — Input parser and overlay boundary gaps (P2, audit targets)

The prefix and controller parsers implement escape/paste handling separately.
`PrefixFilter::command_sequence` consumes mouse sequences as unknown commands,
so the command popup does not share controller pointer routing. A fast Escape
plus ordinary key is treated differently by the two parsers. This includes the
inherent ambiguity between Alt-key input and Escape followed quickly by a key.

Exercise chunk boundaries, nested/stray escape sequences, Alt/SS3/CSI keys,
bracketed paste, delayed lookup completion and mode cancellation. Preserve the
existing guarantee that an unfinished paste owned by a dismissed overlay cannot
leak its tail into an application or execute command bindings. Decide and test
popup click/wheel behavior explicitly. These are coverage and policy gaps, not a
claim that every listed sequence currently fails.

## Fix direction and acceptance

Use the executable requirements and transition matrix in
[the fix prompt](../fix-control-flow-ux-prompt.md). The goal is a coherent input
ownership model across the existing interface, with regression coverage for
transition sequences—not a generic visual redesign or a claim of universal UX
correctness. Betamax should verify labeled states before and after transitions;
behavioral assertions must additionally prove which pane receives input, which
history offsets remain, and that unintended requests are absent.
