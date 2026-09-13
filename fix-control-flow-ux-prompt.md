# Fix fux input ownership, scrolling and mode transitions

Implement the fixes, tests and documentation described here. Start by reading
`docs/control-flow-ux-audit-2026-09-12.md` and inspecting the current code. The user
reports that after scrolling one pane they cannot scroll another, and Escape
from scrolling opens the Ctrl-A command view instead of returning to normal.
Fix the underlying control flow and the related confirmed defects, not just
these two symptoms. Backward compatibility and breaking semver are not concerns.

Preserve existing unrelated work in this checkout. Do not commit, push or create
a PR unless separately requested. Do not use hypertile as a dependency. Use the
existing native Rust harness and Betamax integration.

## 0. Resume from the actual checkout

Read the audit's current-checkout follow-up, `docs/control-flow-transitions.md`
and `docs/control-flow-ux-implementation.md`. Existing partial fixes already
implement multiple passive histories, one-press dismissal, request correlation,
popup hit testing and viewport cropping. Preserve them where correct. Historical
findings and old passing logs are not proof of current behavior.

Establish a fresh baseline before editing. The latest audit-only check passed
101 client tests with `cargo +stable test -p fux --locked --lib client::`.
That is unit evidence, not full acceptance. The former compilation failure and
older test counts in the ledger are historical, not current blockers.

Use the audit's latest disposition table and
`docs/control-flow-ux-acceptance.md` to distinguish implemented fixes from missing
verification. In particular, the current source already contains:

- Independent passive histories and one-press dismissal without opening commands.
- Bounded effects/history queues, exact request correlation and fixed deadlines.
- Popup-to-controller gesture handoff and a common loading-mode pointer policy.
- Fresh-press recovery for selection, layout and right-button capture, including
  ownership of an ignored right press when the keyboard changes mode before release.
- Buffer checks both during reconciliation and before installing a history reply.
- Geometry refresh and selection invalidation for resized private history views.

Preserve these fixes and their regressions. Do not restore the former global input
gate or the removed `back` contract. Reproduce any additional defect before changing
behavior; incomplete coverage alone is not evidence of a product bug.

Prioritize the unfinished acceptance work:

1. Complete a per-mode matrix of Escape, completion, outside click, target loss,
   failure and the next ordinary input. Map each cell to a named assertion or an
   explicit, justified non-applicable disposition.
2. Rerun affected real-PTY scenarios against the final binary, especially gesture
   handoffs across modes, nested unequal pane histories, selected resize in two
   differently sized viewers, delayed manager replies and buffer reply/state order.
3. Verify exact input delivery and absence of unintended actions. A passing hint
   assertion or screenshot alone does not establish who received the event.
4. Finish a separate full-diff review of the UX changes, preserving unrelated
   dirty work. Check scheduling fairness, bounded deferred input, stale targeting
   and paste/escape ownership across transitions.
5. Run the complete verification below, refresh affected Betamax evidence and
   consolidate the audit dispositions. The last recorded combined run had five
   automation failures; do not describe the suite as green or treat the recurring
   `zor_headless` failure as resolved because an isolated retry passed.

## 1. Establish explicit behavior before refactoring

Write a transition table covering normal input, passive pane history, keyboard
copy/selection, command popup, context menus, choosers, text entry, confirmations,
layout editing, pointer gestures, asynchronous loading and failure states. Include
entry event, keyboard owner, pointer owner, focus, Escape, completion, outside
click, target invalidation and late replies. Keep it in repository documentation.

Apply this product contract:

- Wheel browsing is pane-local and viewer-local. Scroll A, then B, then A without
  closing an intermediate mode. Both panes retain independent history positions.
  A second attached viewer remains live and unaffected.
- Passive wheel browsing does not capture unrelated keyboard input or change
  application focus merely because the pointer crossed a pane. Distinguish it
  from explicit keyboard copy/selection. Make the currently manipulated history
  pane clear without falsely marking it as the application's keyboard focus.
- One Escape dismisses the active scroll/selection interaction, clears its
  selection, restores that pane to live output and returns keyboard ownership to
  normal. It neither opens commands nor reaches the application. Other panes'
  passive history positions survive. If several passive histories exist, Escape
  acts on the most recently interacted one; subsequent Escape handling must be
  deterministic and documented. A later Escape with no local interaction follows
  normal terminal/prefix rules.
- Explicit copy mode targets the focused pane. `q`, successful copy and Escape
  finish consistently. Provide a separate documented selection-clear action if
  needed; do not make Escape require a hidden second press to exit.
- Ordinary typing into a focused pane with passive history first restores that
  pane to live output, then forwards the original bytes exactly once. Typing into
  another focused pane leaves unrelated history views alone. Bracketed paste must
  preserve application bytes and cannot execute prefix commands.
- The configured command prefix remains usable while browsing history. From
  explicit copy mode it deliberately ends the interaction and opens commands
  once. Preserve literal-prefix escaping and configurable bindings. Specify
  precedence for printable prefixes in text-entry fields rather than stealing
  ordinary text characters globally.
- Wheel input over a mouse-reporting application follows that application's
  routing unless Shift explicitly requests local history. Browsing A must not
  cause B's mouse events to be ignored or intercepted. Exited-pane history and
  alternate-screen behavior need explicit, tested availability rules.
- Escape dismisses menus, choosers, rename fields and confirmations to normal
  input. `n`, outside-click cancellation and Escape agree on the destination.
  Parent-menu navigation, if retained, is a separate explicit Back action with
  a real parent; it is not inferred from mouse-capture flags.
- A failed request displays a useful notice without implicitly opening commands.
  Keep the relevant local mode only when retrying there is meaningful and safe.
- Layout arrows apply immediately. Enter and Escape finish editing; both retain
  committed edits. Label this accurately. Escape during an uncommitted pointer
  drag cancels the preview and sends no mutation.
- Clicking another pane while browsing must work on the first gesture. Specify
  whether that gesture is consumed for focus or delivered to a mouse-reporting
  app using the existing normal-mode focus policy; never deliver it twice.
  Command-popup wheel/click/outside-click behavior must be intentional and tested.

These are the defaults to implement, not questions to defer to the user. Record
any necessary departure with a concrete conflict and evidence.

## 2. Repair the model and scheduling

Inspect `client/mod.rs`, `input.rs`, `controller.rs`, `copy.rs`, `render.rs`,
`context.rs`, `drag.rs`, command availability and the history protocol handlers.

- Separate pane history state, keyboard interaction mode, prefix state and pointer
  capture. Replace the ambiguous boolean “back” contract with explicit outcomes
  such as dismiss, finish, explicit parent navigation and error notification.
  Centralize transition cleanup and ownership decisions; do not add more
  scattered exceptions to `cancel()`.
- Support multiple local history viewports in composition. Scope them by stable
  attachment/workspace/pane identity, bound memory and retained sessions, and
  define retention across tab hiding. Clean up pane removal, identity changes,
  workspace transfer, reconnect and viewer exit. Numeric pane IDs alone are
  insufficient. Do not change daemon focus merely to render local history.
- Refresh scrolled history after geometry changes, even when its requested offset
  has not changed. Preserve offset where possible, respect server clamps, and
  invalidate selections whose displayed cells are no longer valid.
- Route a captured selection gesture to its owner until release, including when
  the pointer leaves the pane. Reset drag state on release, Escape, scroll,
  selection clear, target loss and resize as appropriate. Consume gesture tails
  without swallowing a subsequent fresh click.
- Allow local Escape, focus/history interaction and overlay dismissal while a
  history read or unrelated manager operation is pending. Do not unblock raw
  application input or mutations indiscriminately: retain required ordering,
  targeting, acknowledgments and bounded backpressure.
- Give pending requests fixed deadlines measured from send time. State traffic
  must not postpone a timeout indefinitely. Correlate replies with the exact
  operation and session; stale replies cannot clear a newer outstanding request,
  install an old viewport, restore a dismissed overlay or alter a different pane.
- Coalesce rapid scrolling against the latest desired offset while allowing fair
  progress for other panes. Track sent and desired offsets separately so a clamp
  for an older request cannot overwrite newer intent. Use identities that are not
  reused when copy sessions are recreated. Check outstanding capacity before a
  state-mutating `take_read()` call.
- Preserve byte-exact normal input, literal prefixes, paste isolation, Unicode,
  escape disambiguation, detach ordering and application mouse encoding. Reduce
  duplicated parser policy where useful. Do not treat every ESC byte as immediate
  cancellation: CSI/SS3/Alt sequences must still work.
- Make hints accurately describe the current owner and exit behavior. Every
  transient mode needs a visible, consistent exit and a safe target-loss path.

First reproduce each audit finding with a behavioral regression. U6 and U10
contain risks/policy gaps: validate them against current code before claiming a
current bug. Extend the audit with newly confirmed defects in this control flow,
fix in-scope defects, and explicitly reject invalid findings with evidence.

## 3. Required regression matrix

Use deterministic controller/parser tests plus real PTY scenarios. Assertions must
verify routing and absence of unintended actions, not merely that a hint exists.

1. Two panes containing distinguishable numbered history: wheel A → B → A;
   assert both offsets/content, retained A position, focus policy and viewer
   isolation. Repeat with unequal pane sizes, nested layout and a mouse-reporting
   app in B; test Shift override.
2. Wheel → Escape → type a sentinel; assert normal input, no command popup,
   no leaked Escape, no swallowed sentinel and exactly one target receives it.
   Repeat for explicit copy, active selection, `q`, successful copy and prefix.
3. Passive history plus pane click, tab switch, context menu and layout gesture;
   verify the first gesture works, input ownership is visible, and no stale release
   causes a second action. Include press A → drag outside/B → release → new drag.
4. Resize while scrolled and while selecting, including tiny dimensions and
   restoration; verify refreshed geometry, clamping, selection invalidation,
   continuous borders and no stale background or cursor.
5. Every transient mode: Escape, completion, outside click where applicable,
   target removal and failed request. Verify destination, focus, no phantom command
   popup, no unintended mutation and a working next ordinary key.
6. A delayed/withheld history reply with ongoing state frames: Escape remains
   responsive, wheel on B works, deadlines are fixed, and late replies after
   close/re-entry/transfer cannot reinstall old state. Cover burst scrolling,
   fairness, upper/lower clamps and bounded queues.
7. Delayed workspace lookup/manager reply followed by cancel/re-entry; assert no
   resurrected dialog or replayed input in a different mode. Preserve unfinished
   paste ownership when its original mode disappears.
8. Split every relevant input sequence at each byte boundary: Ctrl-A commands,
   alternate configured prefixes, literal prefix, lone Escape, Alt input, CSI/SS3,
   mouse reports, UTF-8 and bracketed paste. Cover multiple events in one read.
   State the disambiguation rule for fast Escape-plus-key versus Alt; do not claim
   to distinguish byte-identical events.
9. Run existing detach/input receipts, two-workspace transfer, startup/rejection,
   application mouse and multi-viewer regressions. No loss of current guarantees.

Use controlled delayed-reply fixtures and bounded waits. Do not replace failures
with sleeps, increase general deadlines to hide stalls, or weaken old assertions
except those explicitly encoding the superseded UX contract.

## 4. Visual verification and completion

Follow `docs/betamax-harness.md` for Zig, fonts and feature setup. Run targeted
checks first, then formatting, relevant all-target Clippy, the standalone harness,
and all `local_cli`/`automation_integration` scenarios with capture enabled and no
scenario exclusions. Use a fresh evidence directory. Generate the Betamax report,
verify exact replay and completed synchronized frames, and inspect labeled PNGs
for the new transition matrix at normal and tiny sizes. Include A and B scrolled
simultaneously, after Escape, after focus change, active selection, each overlay
exit, and delayed-reply cancellation. Never infer behavioral correctness solely
from clean screenshots.

The earlier baseline has four capture-independent zor fixture failures documented
in `docs/betamax-verification-2026-09-12.md`. Recheck their current status, distinguish
new failures from that baseline, and report unresolved ones honestly. Do not skip
or silently redefine them; unrelated failures are not a reason to broaden this
UX change into a separate routing migration.

Deliver implementation, regression coverage, updated bindings/hints/manual steps,
an updated audit with each finding's disposition, and a verification report with
commands/results, behavior coverage, gallery location and unresolved limitations.
Perform a separate full-diff review of this task's changes before handoff. Do not
claim universal UX correctness, native OS-input validation or cross-product parity
from headless coverage. The acceptance target is every confirmed in-scope finding
and every transition required above, backed by behavioral and visual evidence.
