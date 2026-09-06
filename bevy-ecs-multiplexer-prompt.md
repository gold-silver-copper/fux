# Rebuild fux as a minimal multiplexer powered by bevy_ecs

Execute this specification. A complete rewrite of fux is explicitly authorized. Build the best
small, reliable terminal multiplexer for this design rather than preserving the current architecture.
`bevy_ecs` is a required architectural foundation, not an optional experiment or a cosmetic wrapper.

This is the authoritative successor to ultra-minimal-multiplexer-prompt.md. The full product scope
is restated here. Existing source, tests and audits provide behavioral evidence, not a requirement
to retain their implementation. Backwards compatibility and breaking semver are not concerns.

## Product

> Workspaces group related work. Tabs switch layouts. Splits show terminals together.

Keep **workspace → tabs → panes arranged in recursive splits**. A workspace is a persistent
session grouping tabs. Each tab owns one layout; its leaves are terminal panes running shells or
programs. Splits nest within tabs. Workspaces and tabs do not nest or embed each other.

Minimal means a focused product and understandable implementation. Tabs, scrollback, proper mouse
behavior and the keybinding popup are core features, not optional complexity to eliminate.

Retain:

- Create/switch workspaces; create/switch/name/close tabs.
- Horizontal/vertical splits, directional focus, repeated resizing and confirmed pane closure.
- Persistent PTYs, background output, detach and reconnect.
- Independent bounded pane history, keyboard browsing, mouse selection and clipboard copy.
- One configurable prefix and a compact contextual keybinding popup.
- Multiple viewers with independent navigation, interaction and history/selection state.
- Direct, supported process-protocol interoperability with independently built koh and zor.

A fresh `fux` launch creates one default workspace, one tab and one pane. No configuration, name,
credentials or optional programs are needed. With existing workspaces, attach using a documented,
deterministic rule; do not force a startup picker. Generate workspace/tab labels automatically;
optional names must never be prerequisites.

Hide the tab strip with one tab. Show one compact strip when there are multiple tabs. Use thin
split borders and clear focus, without a permanent workspace/status dashboard. Preserve the
keybinding popup near the bottom, with actual bindings, concise labels and contextual availability.

Remove floating popup **terminal panes**, separate full-screen pickers, arbitrary external-command
bindings, lifecycle hooks, desktop notifications, agent dashboards, automatic sidecar supervision,
embedded networking/detection logic and configuration used only by removed features. Starting a
program in an ordinary pane remains essential. Do not remove the keybinding popup with popup panes.

## ECS architecture: mandatory and substantive

Use standalone `bevy_ecs` without the full Bevy game engine, renderer, windowing, asset pipeline or
game loop. Choose and pin an appropriate supported release and a deliberate minimal feature set.
Inspect that release's APIs rather than relying on examples from another version. Consult the
[crate documentation](https://docs.rs/bevy_ecs/latest/bevy_ecs/) and
[schedule documentation](https://docs.rs/bevy_ecs/latest/bevy_ecs/schedule/), then link the selected
version's documentation in the implementation plan.

The authoritative multiplexer model and its transitions must live in ECS components/resources and
systems. Do not put the old host/state implementation inside one giant Resource and declare the
rewrite complete. Replace duplicated mutable state, command paths and lifecycle machinery.

Use entities for things with independent identity/lifetime: workspaces, tabs, panes and attached
viewers are natural candidates. Model split nodes as entities or a compact typed layout tree,
whichever yields simpler invariants. Do not create entities for every terminal cell, character,
history line, byte, keypress or configuration field. Keep terminal screens/history in cohesive
data structures associated with their pane.

Write down the entity/component/resource model before implementation. It should make these
responsibilities explicit:

| Domain | Required ownership |
|---|---|
| Workspace | Stable identity, label and tab membership |
| Tab | Stable identity, label and its split layout |
| Pane | Stable identity, terminal emulator/history, geometry, process lifecycle and I/O identity |
| Viewer/attachment | Selected workspace/tab, focused pane, terminal dimensions, attachment lifecycle |
| Viewer UI | Prefix/menu state, pending confirmation/text entry, copy selection, history position and deadlines |
| Shared resources | Small configuration/command registry, resource budgets, protocol-ID mapping and bounded ingress/effect queues |

Choose the server/viewer process boundary deliberately. Server ECS owns authoritative session,
layout and pane state; viewer-side state is private and never duplicated as competing authority.
Small viewer controllers may remain explicit Rust state machines; use a separate viewer ECS World
only if it actually simplifies the design. Do not serialize/replicate a whole ECS World to clients.
Snapshots and indexes may be derived data, but each mutable fact must have one authoritative owner.

Keep public protocol IDs distinct from Bevy Entity handles. Use stable session-scoped identities
with an instance/generation boundary so late requests cannot address recycled objects after
despawn or server restart. Validate entity kind, ownership and liveness at command execution.
Destructive confirmations carry the original target identity, not merely current focus or an index.

ECS relationships alone do not enforce all domain rules. Explicitly validate acyclic layouts,
unique membership, live focus, nonempty usable containers, valid dimensions and resource budgets.
Closing a viewer must not cascade-despawn persistent panes. Specify which ownership edges cascade
and which references are non-owning; test lifecycle cleanup rather than assuming hierarchy hooks
will terminate or reap operating-system processes.

## Ordered, event-driven execution

Use an event-driven owner loop that runs ECS schedules when work arrives or a real deadline
expires. No unconditional frame tick, busy polling or game-style fixed update loop. Idle fux must
sleep. Repaint only when visible output, layout, viewer state or a deadline requires it, with
bounded coalescing under output bursts and prompt interaction feedback.

Begin with a single logical writer and explicit system ordering. Do not enable parallel mutation
because a scheduler supports it. Introduce parallel work only if measurement justifies it and
ordering remains provable. Avoid a web of observers triggering other observers for core commands.
Use explicit ordered phases; reserve hooks/observers for small local invariants when appropriate.

A suitable processing outline is:

1. Accept a bounded, fair batch of typed I/O events and viewer requests.
2. Validate and apply ordered domain transitions, making deferred ECS mutations visible at
   deliberate boundaries before dependent operations run.
3. Produce bounded I/O effects and handle their acknowledgements/failures as subsequent events.
4. Resolve layout, process lifecycle and view invalidation; derive consistent snapshots.
5. Publish replies, relevant lifecycle events and viewer updates with documented ordering.

Refine this outline as needed, but prove these guarantees:

- Per-source byte order is preserved. Output/EOF/exit ordering cannot lose final pane output.
- Multiple commands in one read observe their predecessors. Split/create, focus and subsequent
  input reach the newly selected pane only after successful creation; failed creation rolls back
  reservations and cannot leave a visible phantom pane.
- Input/control barriers distinguish queued, applied and failed operations. Acknowledgements
  cannot overtake the state/effects they promise. External I/O is not magically atomic with ECS.
- Detach drains preceding accepted input and ignores trailing commands. Workspace switching
  preserves the intended suffix for the destination without sending it to the old workspace.
- Stale callbacks, canceled reads, expired deadlines and late process reports cannot resurrect
  entities or retarget replacement panes/tabs.
- Hot panes, slow viewers and observers cannot starve input, timers or process-exit handling.

Choose bounded ingress queues and explicit overflow/backpressure policies. Bevy message storage
must not become an unbounded transport queue; verify message retention/maintenance semantics
for manually driven schedules. Do not drop commands merely because a maintenance cycle advances.

## Operating-system I/O and lifecycle

Keep blocking PTY/socket/process operations out of ECS systems. Use a small adapter layer with
owned async tasks or threads, bounded channels and typed input/effect/completion messages. Retain
an async runtime only where useful. I/O workers must not mutate the World or carry broad shared
locks back into domain state. Transfer identity-tagged data and handles with clear ownership.

Use a proven terminal emulator and PTY library where appropriate; a fresh architecture is not a
request to reinvent escape parsing or operating-system process management. Reuse existing pure
helpers only when they fit the new ownership model. Keep modules small and cohesive; do not create
a generic ECS application framework, plugin runtime or abstractions for hypothetical backends.

The session process owns PTYs and retained history. Viewer loss must not terminate them. Define
creation rollback, natural exit, confirmed close, final-tab/workspace retirement and server shutdown.
Observe final output/status before retirement, distinguish PTY EOF from process exit, terminate
owned process groups as appropriate, and reap owned children/tasks. Test partial startup failures.
Persistent here means surviving detach, not resurrecting processes after server/machine restart.

Preserve private socket directories, ownership/permission checks, peer authentication, bounded
framing, handshake deadlines, safe socket cleanup and incompatible-version rejection before raw
terminal setup. Never terminate personal sessions to upgrade a protocol.

## Viewer interaction and rendering

Use a small terminal compositor such as the existing ratatui primitives. Graphics/GPU rendering
and the Bevy rendering stack are out of scope. ECS components are an internal design, not labels
or concepts shown to users.

- Ordinary input is byte-exact pane input. One authoritative registry drives configured bindings,
  command labels, availability, dispatch, the keybinding popup and CLI binding output.
- Preserve Ctrl-A as the default prefix. Use immediate popup hints by default; process currently
  available fast command bursts before repaint to avoid a flash. Do not delay commands to show UI.
  Document Escape-prefix disambiguation and avoid impossible timing guarantees across separate reads.
- Prefix twice forwards exactly one prefix, including Escape or a key that could otherwise match
  an action. Unknown keys stay in command mode and reveal available commands without reaching panes.
- Esc dismisses/backtracks without leakage; successful simple actions return to pane input. No
  command inactivity timeout. Repeated resize may use a thin hint until finish/back; changes stay.
- Workspace/tab choices, naming and target-specific close confirmations share the popup's small
  presentation primitives. Preserve direct next/previous-tab shortcuts and bounded paging.
- Application cursor/keypad modes, fragmented CSI/SS3/mouse/paste sequences and Unicode input
  work correctly. A canceled mode retains ownership of an unfinished paste/sequence until drained.
- Menu/selection/history state, active tab and focus are viewer-specific. Shared structural edits
  remain consistent without hijacking another viewer. Define deterministic pane-size negotiation
  for viewers with different terminal sizes, including hidden tabs and small screens.
- Handle resize and stale layout generations before mouse hit testing or painting. Tiny/zero-area
  cases must be bounded and safe; preserve meaningful controls where physical space permits.
- Restore terminal modes, cursor and primary screen on exit/error/suspend paths. A final frame
  must actually be painted, and tests must observe it before primary-screen restoration.

## Pane history, mouse and clipboard

Each pane retains bounded history while hidden or detached. Tab/workspace switching and reattachment
must not discard it. Document history limits, eviction and closure/retirement behavior; there is
no merged tab/workspace output log or separate historical-workspace browser.

Each viewer owns its browsing position and selection. New output must not force live-follow or
silently change copied text. Use stable history positions/snapshots; if eviction or resize invalidates
a selection, clear it with visible feedback rather than copying different cells. Returning to a
tab/workspace restores valid browsing state; a new attachment may start live with history available.

Keep keyboard history/selection and pane-local mouse selection, with a small transient hint and
explicit return to live output. Do not rely on the enclosing terminal's scrollback to provide
independent pane history in a split full-screen display.

- If the application under the pointer does not request mouse input, wheel movement browses its
  pane history and dragging selects its text within pane boundaries.
- If it requests mouse input, ordinary events reach that application with correct coordinates
  and encoding. Do not also scroll/select in fux.
- Provide a documented modifier override for fux history/selection, plus keyboard fallback for
  terminals that reserve gestures. Focus changes and split resizing must not misroute a drag.
- Copy selected text only, respecting wide/combining characters, wrapping, borders and adjacent panes.
  Preserve clipboard policy, size limits, once-only output and actionable retry feedback. Clipboard
  operations and browsing must not change another viewer's viewport, selection or clipboard state.

## Koh and zor: independent builds, native composition

Fux must build, install and run with neither koh nor zor present, under every fux feature set.
No cross-owner crate/library, build script, patched/sibling source or mandatory executable
dependency. Do not copy networking/detection engines into fux. Ordinary Rust dependencies,
especially `bevy_ecs` and suitable terminal/I/O libraries, are allowed and expected.

Make interoperability first-class through explicit, versioned process protocols and direct CLI
composition. Sharing ECS implementations is unnecessary: never expose Bevy Entity handles,
component layouts, reflection data or schedule details as the external contract.

- Fux exposes an authenticated local attachment stream and a small structured control/observation
  surface: stable workspace/tab/pane identities, listing, bounded capture and lifecycle events.
  Reads must not change focus, selection or viewport. Specify ordering, backpressure and cancellation.
- Koh owns identities, authorization, encrypted networking, discovery and reconnect. A real koh
  gateway carries the same attachment protocol to a private local socket consumed by the normal
  fux viewer. Stopping a gateway leaves panes/local attachments usable. No network-specific viewer.
- Zor owns detection, rules, agent state and optional presentation/notifications. Its independently
  launched observation command consumes fux capture/events directly, not terminal-UI scraping or
  private files. Missing, slow, crashed or malformed observers cannot block or mutate pane operation.
- Users explicitly start optional programs with documented commands. No automatic supervisor,
  integration daemon, mandatory credentials for local fux or hidden startup.

Inspect existing owner contracts and preserve useful protocol semantics where practical. Version
intentional changes. If integration changes require koh/zor edits, make only the necessary local
changes in those owning repositories and maintain reproducible pins/fixtures. Do not force them
to adopt ECS or broadly redesign them. Their builds must remain independent of fux and each other.

## Rewrite workflow and scope control

1. Read repository instructions, current code, HANDOFF.md and the prior acceptance/review records.
   Inspect staged/unstaged/untracked changes in all relevant owners. Preserve unrelated work and
   the behavior protected by existing regression fixes. No reset/clean operation may discard it.
2. Write a concrete plan covering ECS ownership, typed commands/events/effects, system order,
   deferred mutation boundaries, process lifecycle, viewer isolation, history, UI and protocols.
   Inventory removals and justify the dependency/feature selection. State resource/default policies.
3. Implement a complete vertical path first: real PTY → output event → ECS terminal state → viewer
   rendering, with real input, detach/reconnect and process cleanup. Then integrate the retained
   tabs/splits/history/mouse/menu/protocol features through that architecture.
4. Complete the rewrite rather than leaving two active implementations. Remove obsolete hosts,
   duplicated stores, adapters and features once replaced. Do not keep a compatibility shell or
   feature-flagged legacy backend as the final result. Preserve license attribution for reused code.
5. Port meaningful tests/oracles to the new model. Keep behavioral intent and strengthen missing
   edge cases; do not rewrite expectations merely to bless regressions or count vacuous passes.
6. Have an independent reviewer who did not implement the changes review the complete intended
   diff/new files and integration edits. Validate findings, fix confirmed defects, rerun affected
   checks and independently review fixes. Repeat for new P0/P1 findings.
7. Update CLI help, configuration, architecture/protocol docs and a requirement-by-requirement
   acceptance audit. Clearly label historical documents. Finish the working application, not
   merely a scaffold, prototype, plan, ECS library or mock terminal.

Configuration should cover useful shell/program defaults, prefix/core bindings, bounded history,
clipboard policy and necessary resource/security limits. Prefer sensible fixed behavior to a large
preference surface. Be candid about dependency costs: ECS is required even if its transitive graph
is larger. Measure and explain the result instead of claiming fewer dependencies automatically.

## Verification and completion gate

Use both deterministic ECS tests with injected events/time and isolated real-process/PTY scenarios.
Core state-transition tests must not need sockets, sleeps, a graphical environment or optional
owner programs. Runtime tests must exercise real adapters, not only fake effects or ECS queries.

Prove every requirement above, including:

- The actual multiplexer state lives in ECS; there is no authoritative legacy host hidden inside it.
- Domain invariants survive randomized command sequences, entity removal and delayed completions.
  System order/deferred mutations are tested by dependent command bursts, not inferred from tests
  that call systems manually in a different order than production.
- Fresh launch, workspace/tab organization, recursive splits, focus, resize, confirmations, literal
  prefix, unknown keys, naming, popup paging and tiny screens work through the real viewer.
- Private viewer menus/focus/history/clipboard survive concurrent output and shared layout changes.
- Input/paste/mouse fragmentation, cancellation, stale IDs, geometry changes and detach drain
  preserve the established byte/target/lifecycle guarantees.
- Pane history, Unicode selection, eviction, clipboard limits and return-to-live work while other
  panes/tabs/workspaces keep running. Selection never silently copies replacement content.
- Natural exit, forced close, descendants, viewer loss, partial startup and shutdown preserve final
  output/status and clean up only owned processes/tasks/sockets. Slow peers remain bounded.
- Idle schedules do not spin; load cannot grow queues/history without bound or starve interaction.
  Record reproducible idle CPU/wakeups, memory, startup and input/output responsiveness measurements
  against the pre-rewrite baseline where available. Set useful budgets in the plan, fix meaningful
  regressions and explain tradeoffs; do not assume ECS is faster.
- Clean standalone/package builds work without owner source trees, executables, keys or graphics.
- Each owner builds independently. Required real-fux/koh tests prove authorization before local
  access, byte ordering, forced reconnect without repeated input and surviving pane identity.
- Required real-zor tests prove direct observation and failure isolation. Set exact binary paths
  and explicit require flags; optional integration verification must never silently skip.

Run formatting, strict linting for all retained targets/features, root tests, fixture/oracle checks,
rustdoc and standalone/package checks appropriate to the final repository. Preserve or replace
required verification commands with equivalent meaningful evidence. Check the declared MSRV and
supported platform builds where tooling is available; distinguish compilation from runtime tests.
Inspect accessible relevant CI failures and fix causes in scope. Do not broaden into unrelated work.

The final audit must map each explicit feature, invariant, removal, protocol and verification item
to current source and observed evidence. Report architecture/ownership, user flow, defaults, removed
machinery, dependency/performance results, exact test commands/results, koh/zor composition commands,
independent review findings and platform/CI limitations. Keep completion unproven until that audit
passes. An environment blocker is a reason for an explicit incomplete handoff, not a narrower goal.

## Authorization

The rewrite and necessary local owner integration edits are authorized. Work autonomously through
implementation, verification and review; do not repeatedly ask for approval of routine design choices.
This does not authorize commits, pushes, PRs, releases, hosted-workflow reruns or GitHub comments.
Do not touch personal sessions, clear keys or terminate user workspaces. Use disposable HOME/XDG/runtime
directories and owned test processes. Never kill a live old server to test or apply the rewrite.
