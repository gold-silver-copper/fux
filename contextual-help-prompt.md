# Contextual-help implementation checkpoint

Status: implementation is substantially complete; final review and acceptance are unfinished.
The user requested a committed/pushed checkpoint and handoff instead of continuing this session.
See [HANDOFF.md](HANDOFF.md) for resume instructions and verification evidence.

Implemented: shared command registry and contextual availability; viewer-local prefix/help,
workspace/tab pickers, rename, close confirmation, repeatable resize, copy/selection/clipboard;
200 ms delayed hints, immediate/hidden preferences; standalone local protocol v2; fragmented
input/paste handling, Unicode input, tiny-terminal rendering, viewer isolation and detach ordering.
The real-terminal and fixture scenarios have been updated for the new interaction flow.

Remaining: finish the separate complete-diff review; fix any confirmed findings; perform a final
requirement-by-requirement acceptance audit; rerun affected checks after further edits; write the
final completion report. Do not treat this checkpoint or passing tests as completion of the goal.
No final full-diff review or exhaustive acceptance audit has been claimed.

The original implementation requirements below remain the scope for resuming work.

---

Implement contextual command discovery and consistent interaction modes in fux.

## Goal

Make fux discoverable without requiring users to memorize keybindings. Show the actions available in the current context while preserving fast keyboard operation and keeping ordinary pane input untouched.

Fux must remain a standalone local multiplexer. This work must not introduce koh or zor dependencies.

## Command mode

- Ctrl-A, or the configured prefix, immediately enters a temporary command mode.
- After approximately 200 ms, show a compact ratatui panel near the bottom listing available commands and their actual configured bindings.
- Command keys work immediately, including before the panel appears. Fast prefix-command sequences must not flash the panel.
- Executing a simple action dismisses the panel and returns to pane input.
- Esc cancels command mode without forwarding input to the pane.
- Pressing the prefix twice preserves the existing literal-prefix behavior.
- An unknown command key performs no action, remains in command mode, and reveals the hints immediately.
- Do not automatically cancel command mode after an inactivity timeout.
- Show relevant commands grouped clearly; support small terminals without obscuring everything or making commands unreachable.

## Contextual interactions

Use consistent behavior across interactions owned by fux:

- Workspace and tab pickers: show available choices and navigation hints; Enter selects, Esc cancels.
- Copy mode: show compact hints appropriate to the current selection state. Copy completion returns to the pane; Esc backs out or cancels as appropriate.
- Rename and other text entry: show a focused input with submit/cancel hints. Cancellation must preserve the original value.
- Destructive actions: show an explicit confirmation identifying the target and consequences.
- Help views and existing popups: expose their applicable navigation and dismissal keys.
- Repeatable operations: use an explicitly entered mode where useful. For example, resize mode allows repeated adjustments and shows its controls until the user finishes or leaves.

Use a compact command panel for prefix discovery, footers inside pickers/dialogs, and a thin hint bar for persistent modes. Avoid stacking unnecessary popups.

Completion returns to the appropriate previous context. Esc backs out one level. Clearly define whether leaving a mode preserves already-applied changes; never imply that cancellation undoes changes when it does not.

Do not intercept ordinary shell/editor input or infer modes from applications running inside panes.

## Architecture

Inspect the existing input routing, prefix handling, compositor, ratatui usage, configuration, command dispatch, copy mode, pickers and popup implementation before choosing the design.

Create a shared command-description system containing the information needed for:

- Command execution and dispatch.
- Configured keybindings.
- Human-readable labels and grouping.
- Contextual availability.
- `fux bindings`, existing help and contextual hints.

Avoid separate hardcoded command lists that can drift out of sync. Reuse existing command implementations.

Define explicit interaction states and transitions. Keep the design focused on fux’s actual modes rather than building a generic UI framework.

Transient interaction state belongs to the viewer that initiated it. One viewer opening help or entering a mode must not hijack another viewer’s input or display. Coordinate this with existing shared workspace operations.

The popup delay must trigger a repaint even if the pane produces no output. Cancel pending timers when the mode ends, changes, detaches or shuts down.

## Behavior and rendering

- Preserve existing configurable bindings and literal-prefix semantics.
- Handle input split across reads and multiple key sequences arriving in one read.
- Avoid leaking consumed command, navigation or cancellation bytes into the pane.
- Preserve terminal input modes, paste handling, Unicode rendering and resize behavior.
- Keep focused content visible where practical.
- Bound panel dimensions, command lists and rendering work.
- Support a zero-delay preference for immediate hints and a preference to hide automatic hints without disabling commands or explicit help.
- Follow existing configuration conventions and document defaults.

Backwards compatibility and breaking semver are not concerns, but deliberate interaction changes must be documented.

## Implementation workflow

Read repository instructions and inspect existing uncommitted work first. Preserve unrelated changes.

Inventory the interactions currently owned by fux. Write a concrete implementation plan identifying which hint presentation and state transitions each interaction needs, then implement the complete flow.

Do not stop after adding a static prefix popup. Integrate the shared command descriptions, input behavior, contextual states, configuration and documentation.

Do not commit, push, publish or modify personal sessions unless separately requested.

## Verification

Use meaningful targeted tests and isolated real-binary scenarios to demonstrate:

- Delayed hints appear without additional keyboard input or pane output.
- Fast prefix-command sequences execute once without flashing hints.
- Custom prefixes and bindings appear correctly and execute correctly.
- Double-prefix forwarding remains byte-exact.
- Unknown keys reveal hints without reaching the pane.
- Esc and completion follow the documented transitions.
- Text entry, copy mode, pickers, confirmations and repeatable modes expose the correct controls.
- Rapid mode changes, detach and shutdown leave no stale panels or timers.
- Separate viewers retain independent interaction state.
- Small terminal sizes and resizing remain usable.
- Existing pane input, paste handling and multiplexer commands continue working.
- Standalone builds still exclude koh and zor.

Run formatting, strict linting and checks appropriate to the changes. Review the resulting diff and fix confirmed defects.

Finish by explaining the implemented interaction flow, configuration defaults, verification results and any remaining limitations.
