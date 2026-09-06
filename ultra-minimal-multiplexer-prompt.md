# Make fux a minimal, composable persistent multiplexer

Implement this design in the existing fux repository. The objective is a smaller product and
implementation, not a minimal-looking interface layered over the current feature set.

## Product model

> A workspace groups persistent terminal layouts. Tabs switch layouts; splits show terminals together.

The hierarchy is **workspace → tabs → panes arranged in splits**. A workspace is a persistent
session grouping related work. Each tab owns one pane layout. A pane runs one shell or program;
a split divides a tab's layout to display panes together. Programs in hidden tabs and other
workspaces continue running. Preserve tabs as a useful grouping layer rather than flattening
them into workspaces.

Splits may nest recursively within a tab; tabs and workspaces do not nest. Layout trees contain
split nodes and terminal-pane leaves. Do not embed, share or alias one workspace inside another.
History belongs to individual panes, not to combined tab or workspace output streams.

Optimize for the smallest useful day-to-day multiplexer. Backwards compatibility and breaking
semver are not concerns. Do not preserve obsolete abstractions solely for compatibility.
Minimality means a focused multiplexer, fewer duplicate interaction paths and less optional
machinery. Do not remove tabs, pane history, mouse support or reliable program interoperability
merely to reduce a feature count.

## Keep

- Persistent local workspaces and live PTYs: detaching or losing a viewer leaves programs running.
- Create and switch workspaces.
- Create, switch, name and close tabs within a workspace.
- Split the focused pane horizontally or vertically.
- Move focus between visible panes.
- Adjust split sizes.
- Close the focused pane.
- Detach and reconnect.
- One configurable prefix key and the contextual keybinding popup: a compact overlay showing
  available commands and their actual bindings. This popup is a retained core feature.
- Thin pane borders and an unmistakable focused pane.
- Bounded per-pane scrollback, keyboard history browsing, mouse selection and clipboard copy.
- Correct mouse routing between pane applications and explicit multiplexer history/selection actions.

Launching `fux` with no existing session creates one default workspace, one tab and one pane. Do not
require a name, onboarding, credentials, or configuration. With existing workspaces, attach to
a deterministic existing workspace and expose switching through the same small command menu;
do not open a startup picker. Document the selection rule.

Assign workspace/tab labels automatically. Optional names help organize multiple projects and
activities, but naming must never be a prerequisite. Use the same small menu/text-entry mechanism
for tab naming; do not introduce a separate naming interface or customization system.

## Interaction and appearance

- Ordinary terminal input goes directly to the focused pane.
- The small command menu is the keybinding popup, rendered as a compact overlay near the bottom
  of the terminal. Preserve this visual discovery surface; do not replace it with CLI-only help
  or remove it as part of simplifying other popup features.
- Pressing the prefix exposes this popup. Use immediate hints by default, with
  fast prefix-command sequences executing once without an intermediate menu flash.
- The menu shows actual configured bindings, concise action labels, and contextual availability.
  Maintain one shared command registry for menu, dispatch, and binding documentation.
- Esc dismisses the menu or backs out of its current step without leaking into the pane.
- Prefix twice sends the literal prefix byte exactly once.
- Keep workspace and tab choices within this popup's shared presentation. Direct next/previous-tab
  shortcuts should remain fast. Reuse the existing focused overlay renderer; do not introduce a
  generic popup framework or separate full-screen picker. Use bounded paging when choices do not fit.
- Split/focus actions return directly to pane input. Repeated resizing may keep the same small
  menu open until Enter or Esc; clearly state that applied size changes are kept.
- Closing a pane or tab requires a concise confirmation identifying the target and which
  processes will terminate. Use the same menu surface, not a separate dialog system.
- Transient menu state belongs to its viewer. Another viewer must keep independent input and
  display. Shared layout changes remain authoritative and ordered.
- History browsing and selection are also viewer-local. Show only a small transient hint while
  browsing/selecting, including how to return to live output. Keep these interactions within the
  existing terminal surface, without a separate browser window or full-screen picker.
- Hide the tab strip when only one tab exists. With multiple tabs, use one compact line showing
  tab labels and the active tab; no extra workspace bar, dashboard or permanent status widgets.
  A sole pane needs no decorative chrome; split borders distinguish panes and focus. Show workspace
  identity transiently when switching or opening the menu. Avoid adding appearance preferences.
- Errors must be visible and actionable without adding a notification subsystem.
- Keep the interface usable at small sizes and during terminal resize. Do not make actions
  unreachable merely because their labels cannot all fit at once.

## Remove

Remove these from the normal product, implementation, configuration, command registry, control
schema, tests and current documentation wherever they are no longer needed:

- Floating popup terminal panes, commands that launch them, and their pane composition machinery.
  This does not include the retained keybinding popup or the small overlays used by its interactions.
- Dedicated workspace/tab pickers and their separate interaction machinery.
- Configured external command execution, arbitrary command bindings and lifecycle hooks.
  Starting a shell/program in a pane remains supported; that is a core terminal capability.
- Desktop notifications, bells-as-notifications and permanent agent dashboards/status widgets.
- Embedded observation/detection logic, automatic sidecar startup/supervision, and networking
  machinery owned by other programs. Preserve the explicit interoperability contracts below.
- Unnecessary status widgets, theme options and configuration added only for removed features.

## Native interoperability without package dependencies

Fux, koh and zor must each build, install and run independently. Fux must have no koh/zor crate,
library, build-script or mandatory executable dependency, including under optional/all-features
builds. Do not hide coupling behind sibling paths, patched dependencies, feature flags or copied
networking/detection engines. This does not require removing ordinary terminal/runtime libraries.

Interoperation should nevertheless be a first-class supported workflow. Here, native means
documented, versioned process protocols and direct CLI composition, not linked implementations
or ad hoc scripts that scrape terminal output or private files.

- **Fux owns multiplexing:** PTYs, workspaces, tabs, layouts, history, rendering, viewer input and
  local session lifecycle. Provide a small authenticated local attachment protocol plus a structured
  control/observation surface for stable workspace/tab/pane IDs, listing, bounded capture and
  lifecycle events. Reuse existing contracts; do not invent a general plugin/message-bus framework.
- **Koh owns networking:** identities, authentication/authorization, encrypted transport, discovery
  and reconnect. Its gateway should carry the same fux attachment protocol through an authenticated
  local socket without fux knowing about network identities. Local and koh-carried attachments use
  the same viewer implementation. Gateway interruption must not terminate fux panes.
- **Zor owns observation:** detection, rules, agent state and any observation presentation or
  notifications. Its standalone observation command should consume fux's structured capture/events
  directly. Fux must remain usable when zor is missing, disconnected, slow or malformed. Keep
  observer state outside fux unless a retained, concrete multiplexer interaction needs a narrowly
  defined metadata exchange; do not retain a generic agent-status subsystem just for future use.
- The user explicitly starts optional programs. Make documented CLI commands compose directly;
  no mandatory supervisor, integration daemon, new credentials for local fux, or hidden startup.
- Specify protocol negotiation, framing/size limits, peer authorization, ordering, backpressure,
  subscriptions/cancellation and stale-target rejection. Reading output/history must not mutate
  a user's selection, focus or viewport. Surface incompatible versions clearly without killing
  personal sessions or silently falling back to scraping.

Inspect the existing three owner implementations before changing their contracts. Prefer preserving
working attachment/control/observe paths and reducing fux-specific orchestration. If a necessary
contract change affects koh or zor, make the minimal corresponding local changes in the owning
repository and update integration fixtures/pins according to the repository workflow. Preserve
unrelated owner work; do not delete or broadly redesign those programs. Independent builds and
cross-program integration tests must demonstrate both separation and convenient composition.

## Selection, scrollback and terminal behavior

Scrollback is a core feature. Retain a bounded history for every pane in its owning session
process, including while its tab/workspace is hidden or no viewer is attached. Switching tabs/workspaces
or detaching/reconnecting must not discard retained pane history. This is live-session retention,
not a promise of restoring processes/history after server or machine restart. Document history
limits, eviction, and what happens when a pane/tab/workspace is closed or retired.

Each viewer owns its browsing position, selection and copy operation. New output must not pull a
viewer out of history or silently change the text it selected. Handle history eviction and resize
explicitly: preserve the displayed selection where valid, otherwise clear it with visible feedback
rather than copying different text. Browsing must not pause the pane process or move another
viewer's viewport. Returning to a tab/workspace should restore that viewer's valid browsing position;
a new attachment may start at live output while retaining access to the pane's history.

Keep a small keyboard-accessible history/selection mode and reliable pane-local mouse selection.
Reuse the existing bounded copy/viewport machinery where it supports this minimal design. Do not
rely solely on the enclosing terminal's scrollback or selection: those cannot reliably provide
independent pane history and selection in a split full-screen workspace.

Define and document mouse ownership clearly:

- When the application under the pointer has not requested mouse input, the wheel browses that
  pane's history and dragging selects its text. Selection must stay within the intended pane.
- When an application requests mouse input, route ordinary mouse events to that application with
  correct pane-relative coordinates and encoding. Do not also scroll/select in fux.
- Provide an explicit modifier override for fux history/selection when the application owns the
  mouse. Choose a convention supported by the terminal backend, document it, and retain keyboard
  access for terminals that reserve modifier gestures or cannot report them.
- Make returning to live output explicit and discoverable. Do not forward history navigation or
  selection gestures to the application, and do not swallow ordinary application input unexpectedly.

Preserve clipboard policy, size limits, error feedback and Unicode-aware text extraction. Copy
must include the selected pane text, not neighboring panes or borders. Document terminal-dependent
mouse/clipboard limits; do not claim universal native-selection behavior.

Preserve correct Unicode, terminal escape handling, paste boundaries, application cursor/keypad
modes, resize, final output and process exit status. Do not sacrifice these fundamentals to reduce
line count. App-originated terminal controls must retain appropriate bounds and existing security
properties; simplification must not silently broaden clipboard permissions.

## Implementation approach

1. Read repository instructions, HANDOFF.md and the current acceptance audit. Inspect staged,
   unstaged and untracked work first. There are existing reviewed but uncommitted changes;
   preserve their fixes and unrelated edits rather than resetting the repository.
2. Inventory the current feature surfaces, data model and dependencies. Write a concise removal
   and implementation plan, including workspace/tab selection, nested split layouts, per-pane history,
   mouse ownership, final-pane/tab closure, viewer behavior and the three programs' interface contracts.
3. Preserve workspace-owned tabs and tab-owned pane layouts. Simplify duplicate state and dispatch
   paths through host, renderer, input routing, session ownership, protocols and CLI. Preserve recursive
   splits and stable tab/pane IDs; do not introduce recursive tabs or embedded workspaces.
4. Reduce the command registry and interaction states to the retained actions. Keep configuration
   small: core pane command/shell settings, prefix/bindings where useful, bounded history and
   clipboard policy, and necessary resource/security limits. Prefer fixed sensible behavior to
   new preference switches.
5. Delete obsolete paths instead of disabling them or wrapping them in compatibility adapters.
   Remove unused dependencies and stale protocol messages. Version changed protocols explicitly
   and reject incompatible live servers before terminal setup; never kill sessions to upgrade them.
6. Update tests, fixtures, CLI help and current documentation around the new model. Historical
   documents may remain clearly labeled as history. Tests for removed features should be removed
   or replaced with meaningful coverage of the new behavior, not weakened into vacuous assertions.
7. Have an independent reviewer inspect the complete intended changes, including all new files
   and relevant existing uncommitted fixes. Validate findings, fix confirmed in-scope defects,
   rerun affected checks and independently review the fixes.

Keep necessary protocol framing, peer authorization, resource bounds, stale-target protection,
ordered dispatch, process cleanup and viewer isolation. These support reliable basic operation.
Do not replace removed features with a generic plugin system or a new UI framework.

## Acceptance criteria

Demonstrate with source evidence, targeted tests and isolated real-binary scenarios that:

- A fresh launch needs no configuration or name and displays one usable pane in one tab/workspace,
  with no unnecessary tab strip or permanent status UI.
- Detach/reconnect preserves the same running pane process and its output.
- Tabs group several layouts within a workspace. Creating, naming, switching and closing tabs work
  through the shared command system; tab switching does not become workspace switching.
- Nested horizontal/vertical splits work within each tab; terminal panes are their leaves and
  tabs/workspaces cannot contain other tabs/workspaces recursively.
- Background tabs/workspaces continue running, and switching targets the intended stable object.
- Splits, focus, repeated resize and confirmed close work through the small menu.
- The keybinding popup remains a visible, viewer-local overlay with actual bindings, contextual
  availability, paging at small sizes and working Esc dismissal. Fast shortcuts execute without
  flashing it, and displaying it never hijacks another viewer's input or screen.
- Closing the last pane in a tab or the last tab in a workspace has a documented, tested outcome
  without leaving unusable empty containers or destroying unrelated tabs/workspaces. Natural exit
  preserves observable final output and exit status before retirement. Closing a tab confirms the
  affected processes and cannot hit a replacement tab through a stale target.
- A tab strip appears only when useful for multiple tabs; popup panes, standalone pickers,
  permanent status widgets and embedded networking/observation machinery are removed.
- Removed commands/configuration fail clearly rather than silently enabling obsolete behavior.
- Prefix shortcuts, exact literal forwarding, unknown keys, Esc and completion behave consistently.
- Split reads, batched commands, application-mode keys and Unicode work correctly.
- Pasted bytes cannot become commands when modes cancel, targets disappear or input buffers fill.
- Detach/switch drains preceding input and never executes the suffix after detach.
- Multiple viewers have private menu, history and selection state and cannot target replacement
  objects through stale IDs.
- Tiny terminals, terminal restoration, resize and slow/stalled peers remain safe and bounded.
- Each pane independently retains bounded history while hidden or detached; history remains
  browsable after tab/workspace switching and reconnection, with tested eviction and retirement rules.
- Keyboard and wheel history browsing, drag selection, clipboard copy and return to live output
  work in split panes. New output does not hijack browsing or silently alter selected text.
- Resizing and history eviction preserve valid selections or clear them with visible feedback;
  wide/combining Unicode text copies correctly without borders or adjacent-pane text.
- Application mouse input has correct target/coordinates/encoding. The explicit override accesses
  fux history/selection without sending those gestures to the application. Neither path double-handles
  events, including after focus/layout changes and fragmented mouse reports.
- Clipboard restrictions and size limits remain enforced with actionable failure feedback, and
  one viewer's copying or browsing does not affect another viewer's selection, clipboard or viewport.
- A clean standalone build requires neither koh nor zor, keys, sidecars or sibling source trees.
- Each owner program builds independently. Dependency inspection confirms no prohibited cross-owner
  package dependencies, including optional/all-features fux builds.
- Real fux attaches through a real koh gateway using the normal attachment protocol; unauthorized
  remote access is denied before local access. Forced reconnect preserves pane identity and applies
  input once; stopping koh leaves local panes and local attachments usable.
- Real zor observes real fux through documented capture/events with stable target identity, without
  screen scraping or source-tree dependencies. Observation does not change viewer state, and
  observer absence/failure/slow consumption leaves panes usable and resource usage bounded.
- Documented direct CLI composition works with explicitly supplied independently built binaries.
  Integration tests must require those binaries and actually execute, not silently skip.
- The implementation and dependency surface actually shrink, with no hidden compatibility layer
  retaining removed features.

Run the repository's required formatting, strict linting, root tests, relevant fixture/oracle tests,
rustdoc and standalone-build checks. Start targeted, then run the required broader checks. If an
existing verification command becomes obsolete, replace it with evidence covering the retained
contract and explain why. Inspect accessible CI failures relevant to this work without expanding
scope to unrelated failures. Distinguish local results from hosted CI and platform runtime claims.

Write a requirement-by-requirement completion audit. Finish by explaining the new user flow,
what was preserved/removed, the resulting architecture, configuration defaults, direct koh/zor
composition commands, tests and practical limits.
Do not declare completion while required behavior or in-scope defects remain unresolved.

## Authorization and session safety

Implement and verify locally. Do not commit, push, create a PR, publish, rerun hosted workflows,
or comment on GitHub unless separately requested. Do not restart or modify personal sessions,
clear keys, or kill user workspaces. Tests must own disposable runtime/config directories and
clean up only their own processes. Report any concrete blocker and remaining work accurately.
