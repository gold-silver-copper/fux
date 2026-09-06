> Historical record from before the 0.3.0 bevy_ecs rewrite (2026-09-05). The host, popup panes, sidecar supervision, protocol v2/`FUXCTL1` and verification results described here no longer exist. Current architecture: [design.md](design.md); current evidence: [ecs-acceptance.md](ecs-acceptance.md).

# Contextual interaction implementation plan

Objective: execute contextual-help-prompt.md. Existing standalone-refactor changes and personal
sessions must be preserved. No commits, pushes, or owner-repository changes are needed.

## Current inventory

- The command registry in src/commands.rs already defines builtins, default bindings and labels.
  Configured command resolution is duplicated in host setup and CLI help.
- The client DetachFilter recognizes prefix/detach/workspace-picker and paste boundaries. Other
  prefix commands go to a shared host InputRouter. The connection redraws only on server state.
- CopyMode and its selection/viewport currently live in shared host state. Help writes shared status.
  These cannot be the owners of the new viewer-local interaction modes.
- Workspace selection currently leaves the terminal UI and prompts through /dev/tty. Tab selection,
  resizing have typed control commands but lack an interactive flow. Inspection confirmed that
  rename needs a new typed tab operation.
- Popups are real pane processes. Navigation hints must not consume their ordinary application input.

## Implementation sequence

1. Extend the existing command registry with shared configured-command resolution, grouping and
   contextual availability. Reuse this for dispatch, CLI help and visible hints. Add focused config
   for automatic hints and delay (default 200 ms, zero permitted), with validation and reload behavior.
2. Build a viewer-owned interaction controller with explicit pane, prefix/help, selection, copy,
   rename, confirmation and repeatable-resize states. Decode fragmented escape/paste sequences;
   preserve byte-exact literal prefix. Unknown prefix commands reveal hints without leaking bytes.
   Explicit Esc decoding must work without requiring another keystroke.
3. Integrate controller input and deadlines into the local connection event loop. Keep a last
   authoritative snapshot so hints can repaint on a timer without pane output. Route typed actions
   through the existing host commands; avoid shared pending prefix or copy state between viewers.
   Carry any per-viewer capture/scroll needs through a bounded versioned attachment extension.
4. Render ratatui command panels, picker/input/confirmation footers and persistent-mode hints.
   Define small-screen pagination/scrolling, no-stealing-focus behavior and transition cancellation.
5. Replace the out-of-terminal workspace picker with an integrated viewer flow. Preserve manager
   name resolution and detach behavior, and clearly disable unavailable manager actions on an
   explicit socket attachment. Add tab selection, rename and repeatable resize via existing requests.
6. Move copy selection/cursor/viewport ownership to the viewer, preserving scrollback and clipboard
   policy. Popup pane content remains ordinary pane input unless fux command mode is entered.
7. Add deterministic controller/timer tests plus isolated real-TTY scenarios: fast/slow prefix,
   custom bindings, paste/fragmentation, unknown keys, Esc, modes, resize, detach and two viewers.
   Reuse existing routing/copy/corpus checks; update deliberate behavior changes rather than silently
   weakening their assertions. Run fmt, strict Clippy, relevant/full tests and standalone graph checks.
8. Review the complete change relative to this task's starting state, fix confirmed defects, and
   document mode flows/preferences/migration. Audit each prompt requirement before declaring done.

## Transition rules

Prefix has no inactivity cancellation. A completed one-shot command returns to pane input. A command
needing input opens its own context. Esc backs out one level. Rename/confirmation cancellation does
not mutate the target. Repeated resize adjustments are applied immediately and remain on leaving
resize mode; hints must say so. Copy completion returns to pane input. Mode changes, policy changes,
detach and shutdown cancel obsolete timers. Each viewer owns its interaction and display state.

Status: initial inventory complete; implementation in progress.

## Progress: shared configured-command resolution

Added commands::configured_bindings and reused it for host execution setup and `fux bindings`.
It validates key aliases/prefix collisions and resolves builtins/external commands through the
existing registry. This preserves the actual dispatch mapping for the future help controller.
All config and host tests pass (43 host cases), and strict all-target/all-feature Clippy passes.
Logs: /tmp/fux-contextual-registry-tests.log and /tmp/fux-contextual-registry-clippy.log.
The contextual UI/controller, viewer isolation and remaining modes are not implemented yet.

## Progress: viewer-local prefix panel and timer

- Extended fixed-size ClientBindings metadata to describe all configured builtin keys and external
  binding presence without transmitting external argv. Execution and labels reuse the registry.
- Enabled contextual prefix handling in the production viewer bridge. It holds the prefix until an
  action, consumes unknown commands, handles explicit help, cancellation and fragmented escape/paste
  boundaries, preserves literal-prefix encoding, and discards unfinished command prefixes at EOF.
- Added viewer-local HintPanel state via a watch channel. Connection retains its last authoritative
  snapshot and repaints on hint updates even with a silent pane. Rendering uses the existing ratatui
  buffer, capped dimensions and paging. No help state is written into the shared workspace.
- Added local `[hints]` preferences: automatic=true and delay-ms=200, with 0–5000 ms accepted.
  Preferences are loaded at attachment; explicit/unknown-command help works with automatic=false.
- Tests pass: custom binding/paste/literal/escape cases, config bounds, and real-TTY default/immediate/
  hidden preference scenarios. Fast command sequences do not flash hints; delayed hints paint on a
  silent pane. Current client suite has 22 tests and local CLI has four; config has 12. Strict Clippy
  passes. Logs: /tmp/fux-contextual-prefix-final-tests.log, /tmp/fux-contextual-prefix-final-clippy.log,
  and /tmp/fux-contextual-prefix-preferences.log.
- Still required: explicit state/controller integration for remaining modes; contextual action
  availability/grouping; integrated workspace/tab picker, naming, destructive confirmations and
  repeatable resize; viewer-local copy/scrollback; two-viewer and small-screen scenario coverage;
  final wire-contract/version review, full regression and requirement audit. The goal remains active.

## Progress: typed viewer actions and local interaction modes

- Added bounded attachment Control/Reply messages. The host validates requests and executes existing
  control operations, publishing events. Viewer outgoing actions share one ordered queue with input.
  Connection state updates feed the local interaction controller without modifying shared state.
- Added builtin registry entries: w chooses a tab, comma renames it, r enters resize mode; close-pane
  now opens a viewer-local confirmation. Custom bindings route to the same actions. Tab rename and
  selection-by-ID are typed control operations; renaming does not publish a false focus event.
- Added local Tabs/Rename/Close/Resize states with contextual panels and footers. Esc returns one
  level to commands; Enter completes; resize changes remain applied. UTF-8 input/backspace, fragmented
  escapes and bracketed paste are handled locally; pasted y cannot confirm close. Errors are shown
  only to the requesting viewer with bounded displayed text.
- Real-TTY coverage now exercises tab selection, canceled/committed Unicode rename, repeated resize,
  canceled/confirmed close, plus existing delayed/immediate/hidden prefix flows. Targeted client,
  config, host, control, local CLI and corpus suites pass, and strict Clippy passes. Logs:
  /tmp/fux-contextual-mode-final-tests.log and /tmp/fux-contextual-mode-final-clippy.log.
- Remaining: integrated workspace picker, viewer-local copy/scrollback and popup hints, contextual
  grouping/availability, two-viewer and small-screen tests, protocol version bump/consumer fixture
  migration, full regression and review. Review control reply backpressure for large input bursts
  and stale confirmation targets if pane IDs are reused. The goal is not complete.

## Progress: integrated workspace picker wiring

- Startup and prefix workspace selection now use the ratatui interaction controller. Startup
  releases the manager startup lock before waiting for selection. Selected input tails are carried
  into the new attachment. Manager list requests use a shared library contract and bounded reads.
- Fixed mismatched attachment calls and reset the decoder before replaying loading input. Added a
  regression for fragmented navigation during loading and canceled lookup completion.
- A socket-pair regression reproduced macOS EINVAL when updating a read timeout after the peer
  closed with a buffered reply. Manager replies now use poll with an absolute deadline instead of
  repeatedly setting socket options. The closed-peer frame regression and all 24 client / 14 runtime
  tests pass (/tmp/fux-rpc-reader.log, /tmp/fux-picker-tests.log).
- Integrated-picker real-TTY coverage remains required, along with the previously listed copy,
  popup, grouping, protocol, isolation, backpressure and complete-review work. This is progress,
  not completion of the contextual-help prompt.

## Progress: picker terminal scenarios and safe action targets

- Real-TTY coverage now verifies integrated workspace selection/cancellation, input tails across a
  switch, initial picker cancellation/selection, and a concurrent named attachment while the initial
  picker waits. Fixtures drain each PTY and set its dimensions explicitly. Tests own private runtimes
  and clean up both workspaces. Log: /tmp/fux-picker-startup-pty.log.
- Typed viewer actions now allow one outstanding request. This prevents a single read containing
  repeated resize keys from filling the reply channel. The terminal scenario runs 256 adjustments
  followed by a rename and verifies continued operation, then switches workspaces in the same burst
  as another rename. The burst check allows 15 seconds for the host's per-message repaint pacing.
- Pane and tab identifiers now advance for the session lifetime instead of reusing removed IDs.
  A regression removes a target, creates its replacement and verifies stale kill, rename and select
  requests all fail while the replacement survives. All 44 host tests pass
  (/tmp/fux-contextual-targets.log); strict Clippy passes
  (/tmp/fux-contextual-picker-targets-clippy.log).
- README documents workspace selection and stale-target behavior. Remaining work still includes
  viewer-local copy/scrollback, popup hints, command grouping/availability, tiny-screen and broader
  viewer isolation checks, wire-version migration, full regression/review and requirement audit.
  During that review, check mixed commands in one read: state-dependent modes currently take the
  chunk's snapshot, so opening/selecting a tab immediately before another mode needs authoritative
  state ordering. The goal remains active.

## Progress: ordered builtins and copy snapshot source

- Ordinary builtin pane/tab commands now leave the prefix filter as viewer actions and reuse
  Command::request, rather than forwarding legacy prefix bytes. Modal actions retain their local
  controller. External commands and legacy copy mode still use the existing route pending the
  remaining interaction work.
- The attachment server publishes the post-action snapshot before its control reply, using its
  normal frame rather than a duplicate snapshot. The viewer rereads authoritative state for each
  byte and keeps one request outstanding. A real-TTY regression creates and renames a new tab in
  one input burst and proves the old tab was not renamed. Configured builtin filter tests now assert
  typed dispatch while preserving literal-prefix and pasted bytes.
- All 24 client tests, 44 host tests and four local CLI scenarios pass together:
  /tmp/fux-contextual-ordering-final.log. The 256-action stress scenario now allows 30 seconds;
  the full terminal suite took 28.44 seconds under load. This is a queue/order stress check, not a
  latency guarantee. Fast prefix behavior retains its separate short timing assertion.
- An initially suspected queued-input cancellation race was rejected after inspecting the installed
  Tokio 1.53.1 select implementation: a matching ready branch is selected immediately. That code was
  preserved. Temporary tracing was removed. Strict Clippy passes.
- Added SessionHost::copy_view and the workspace implementation. It creates one bounded viewport
  from retained terminal history, restores the terminal's prior viewport and never updates shared
  selection, clipboard or viewport state. A regression compares independent live/history reads,
  clamps an extreme offset, rejects a missing pane and checks shared state equality. It passes:
  /tmp/fux-viewer-copy-source.log; strict Clippy: /tmp/fux-viewer-copy-source-clippy.log.
- Copy snapshot transport, viewer rendering/controller/clipboard and mouse interactions still need
  integration. All other unfinished requirements above remain active, including popup hints,
  grouping/availability, wire migration and final full-scope verification/review. The prompt is not
  complete.

## Progress: private scrollback attachment transport

- Added CopyView requests and correlated CopyViewReply responses carrying a pane ID and a bounded
  optional PaneView. LocalEndpoint invokes the read-only host operation and sends the result only
  to the requesting connection. Missing panes return None; history offsets remain host-clamped.
- Connection::with_copy_views exposes a bounded delivery channel for the upcoming controller.
  Received PaneViews use the existing geometry/cell validator during deserialization, before they
  can reach rendering. Request IDs allow the controller to reject replies from canceled modes.
- The real endpoint test connects two viewers, reads different offsets, checks their reply IDs and
  cells, verifies every intervening shared state has an unchanged viewport and selection, checks
  extreme offsets and missing panes. Malformed viewport geometry is rejected in a separate test.
  All five local protocol tests passed (/tmp/fux-copy-transport-tests.log); the missing-pane extension
  passed (/tmp/fux-copy-transport-missing.log). Strict Clippy passes
  (/tmp/fux-copy-transport-clippy.log), and git diff --check is clean.
- Protocol documentation describes these developmental messages. Final wire-version migration is
  still pending. The production copy-mode controller has NOT yet been switched to this transport:
  selection, local rendering, clipboard, mouse and cancellation integration remain required, along
  with the other requirements listed above. The goal remains active and incomplete.

## Progress: viewer-local keyboard copy integration

- Prefix copy now enters a viewer-owned CopySession, reusing the existing Unicode/wrapped-cell
  selection logic against a private one-pane state. It uses CopyView requests for scrollback and
  never changes shared viewport, selection or clipboard metadata. Pasted command keys are consumed.
- Added a separate CopyUi watch channel and repaint path. The terminal composes a private pane
  override, keeps that viewer on its copy target when shared focus changes, and emits local clipboard
  output once per copy operation under the configured policy. Other panes remain live. A thin hint
  bar shows selection-specific actions without stacking a command popup.
- Space selects; arrows/hjkl move; y/Enter copy and return to pane input; q finishes; Esc clears a
  selection before returning to command help. Scrolling (u/d) and resizing clear selection before
  replacing displayed cells, avoiding copying unrelated text at stale coordinates. Pane closure
  cancels the local mode. Late replies from timed-out reads are skipped by request ID.
- Added controller/render tests for two independent interactions, clipboard policy and once-only
  emission, paste containment, Esc levels, shared-state equality, focus isolation, resize refresh
  and removed targets. Real-TTY coverage selects COPY_TARGET, sees contextual hints, emits its OSC52
  clipboard sequence and checks shared copy state remains inactive. Fixed the fixture's ANSI
  stripping across fragmented PTY reads in its concurrent attachment probe.
- All 26 client tests and four local CLI scenarios pass in /tmp/fux-copy-ui-final-tests.log.
  Strict Clippy result is in /tmp/fux-copy-ui-final-clippy.log. README documents the keyboard flow.
- Still unfinished: viewer-local mouse selection (the legacy host mouse/copy route remains), broader
  two-viewer real-TTY isolation, tiny-screen/resize coverage, popup hints, command grouping/contextual
  availability, protocol-version migration and the final full-suite review/audit. The prompt remains
  active; keyboard copy integration does not complete its full scope.

## Progress: local mouse copy and popup context

- The production prefix filter assembles bounded SGR mouse reports across input reads, preserving
  original bytes for application forwarding and leaving pasted reports untouched. The viewer routes
  Shift-drag and non-application wheel scrolling into its private CopySession. Copy-mode mouse input
  uses the same controller. Other application mouse events continue through the existing route.
- Terminal rendering publishes the compositor's actual pane rectangles for hit testing, including
  zoom and popup geometry. Only the top popup is exposed while modal. Mouse selection does not alter
  shared focus/viewport/clipboard. Target checks avoid cloning full pane contents per mouse report.
- Popup copy now uses the same highlight rendering as tiled copy. A regression reproduced multi-line
  selection highlighting outside pane borders; highlighting is now clipped to content rectangles.
- Popup applications receive a thin footer naming the actual prefix and configured close binding.
  Input still goes to the application; delayed prefix help works even while that footer is displayed.
- Added deterministic tests for fragmented/pasted mouse reports, byte-exact application fallback,
  local selection/wheel state, application mouse forwarding, popup-only hit testing and highlight
  clipping, and configured popup footer labels. Real-TTY coverage verifies Shift-drag clipboard output
  with shared copy state inactive, popup application input, delayed help and confirmed popup closure.
- All 30 client tests pass (/tmp/fux-mouse-popup-client-final.log); all four real local CLI scenarios
  pass (/tmp/fux-popup-context-tests.log). Strict Clippy passes
  (/tmp/fux-mouse-popup-final-clippy.log), and git diff --check is clean. README documents these flows.
- Still required: registry grouping/contextual availability, broader two-viewer real-TTY isolation,
  small-screen/resize scenarios, protocol-version/consumer migration, full regression and independent
  full-diff review (or separate self-review if unavailable), and requirement-by-requirement audit.
  Review edge cases such as copy payload limits and incomplete mouse sequences during that pass.
  Legacy host copy behavior remains for direct host-input consumers; the production viewer now owns
  its keyboard and mouse copy interactions. The full prompt is not yet complete.

## Progress: grouped commands, contextual availability and broad regression

- Added CommandGroup and BuiltinAction::unavailable beside the authoritative command registry.
  `fux bindings` and the panel use the same groups (Panes, Focus, Tabs, Session, Custom). Grouped
  panels page through bounded rows; unavailable actions remain visible with dim styling.
- Viewer dispatch uses the same availability predicate. Examples include resize/focus without a
  split, tab cycling with one tab, layout commands under a popup, and workspace picking on an
  explicit attachment. A pressed unavailable action reports its reason and consumes its input.
  Host validation still handles concurrent changes and resource limits.
- Added registry-consistency/context tests, all-binding pagination with group labels and private
  external argv omission, zero/tiny buffer checks, and dim-state rendering checks. Hint painting
  resets underlying styles so pane DIM/ITALIC attributes cannot mislabel available commands.
  Real-TTY coverage presses unavailable resize and verifies feedback without changing the workspace.
- `cargo test --locked --all-features` passed 242 tests across 18 test targets
  (/tmp/fux-contextual-full-suite.log). It found an old observer test assuming reused pane IDs; that
  test now requires unique IDs and still checks both stale-ID and stale-generation rejection before
  accepting the replacement's valid observer report. Its targeted regression passed first.
- Subsequent style/disabled-workspace fixes passed 33 client tests and all four local CLI scenarios
  (/tmp/fux-command-context-final-tests.log), plus strict all-target/all-feature Clippy
  (/tmp/fux-command-context-final-clippy.log). git diff --check is clean. README explains grouping,
  dim entries and contextual feedback.
- Still required: broader two-viewer real-TTY and small-screen/resize scenarios, wire-version and
  optional consumer fixture migration, full-diff review and requirement audit. Review ordinary input
  routing at frame boundaries: the current production literal-prefix/external-command fallback still
  reaches the host's shared InputRouter, so cross-viewer interleaving needs an explicit isolation
  regression before declaring the prompt complete. Copy payload limits and incomplete mouse report
  handling also remain review items. The full goal remains active.

## Progress: attachment input isolation and protocol 2 migration

- All attachment terminal bytes now bypass the host's shared legacy InputRouter. The production
  viewer sends PaneInput; Input is an equivalent raw-input alias for embedded consumers. Literal
  prefixes are sent once. Explicit Mouse messages retain application routing but cannot enter host
  copy/scrollback, and Binding messages resolve external keys from the host's configured registry.
  External dispatch is acknowledged as Accepted, not as completion of its asynchronous process.
- Added a deterministic two-connection regression: a 4096-byte frame ends with a literal prefix,
  an ordered control barrier confirms processing, and another viewer sends ordinary x through the
  raw-input alias. The text reaches the pane and does not execute a close command. Client tests
  distinguish literal bytes from external dispatch. Real-TTY scenarios execute a configured external
  binding and a popup byte probe confirms double-prefix arrives as exactly one prefix byte.
- Attachment VERSION is now 2. Version-1 rejection is tested before terminal setup. Updated the local
  Python peer and both optional koh real-fux gateway fixture handshakes, refreshed dependency patches,
  and verified their reconstruction. Control protocol FUXCTL1 is unchanged. README and the attachment
  contract describe migration and explicit restart requirements; no personal sessions were touched.
- Full `cargo test --locked --all-features` passed after raw-input alias migration
  (/tmp/fux-protocol2-final-tests.log). The optional gateway suite passed two tests and the real-fux
  five-loss reconnect test passed with FUX_BIN and KOH_REQUIRE_FUX_BIN set, so these were not skipped:
  /tmp/fux-protocol2-final-gateway.log and /tmp/fux-protocol2-final-reconnect.log.
- Input review found paste delimiter bytes could activate matching configured prefixes. Delimiters
  now bypass command parsing; complete CSI/SS3 terminal sequences likewise cannot activate a
  printable prefix from their parameters. Added fragmented-input regressions. Latest 36 client tests
  pass (/tmp/fux-protocol2-decoder-tests.log), and all four CLI scenarios passed after those decoder
  paths were added (/tmp/fux-protocol2-input-final.log). Strict Clippy and diff whitespace checks pass.
- `cargo tree --locked --all-features --prefix none` confirms no koh/zor packages
  (/tmp/fux-protocol2-tree.log). Dependency reconstruction passes again
  (/tmp/fux-protocol2-final-reconstruction.log).
- Still required: same-workspace two-viewer real-TTY interaction scenarios, small-screen/resize
  scenarios, copy payload-limit and partial-sequence timeout review, separate complete-diff review,
  and final requirement-by-requirement audit. The goal remains active, not complete.

### Same-workspace viewer isolation and terminal resizing

- Added `tests/verify/contextual_viewers.py`, wired into `tests/local_cli.rs`. Two real PTY viewers
  attach to the same isolated workspace. The scenario checks private help, uncommitted rename,
  selection and OSC52 clipboard output while the other viewer continues typing and opening help.
  Detaching one viewer leaves the shared pane usable by the other.
- The scenario exposed a rendering defect: buffer diffs used the previous terminal dimensions
  after resize, producing incorrect cursor positions. Rendering now clears and compares against
  an empty buffer of the new dimensions. A resize also repaints the viewer immediately, regardless
  of whether the host produces another shared state update.
- The real-terminal scenario now passes, including copy hints at 3×18, command pagination on
  that screen, a 1×1 terminal, and restoration to 12×48 without losing command context.
  All 36 client tests pass (`/tmp/fux-resize-client-tests.log`).
- Remaining: copy payload-limit and partial-sequence timeout review, separate complete-diff review,
  final verification and requirement-by-requirement completion audit.

### Clipboard bounds and fragmented terminal input

- Confirmed and fixed silent copy failure above the encoded clipboard limit. The viewer retains
  its selection and displays a retry hint. Esc clears the selection and error; a smaller selection
  can then be copied. The underlying copy engine also exits only when clipboard metadata accepts
  the copied value. README documents the 1 MiB encoded limit.
- Added a valid maximum-size pane regression using combining text: copying the complete pane
  exceeds the limit, preserves selection, and succeeds after selecting one cell instead.
- Once normal pane input has identified CSI/SS3, it retains that bounded sequence across read
  delays instead of interpreting parameters through the prefix router after the Escape timeout.
  The sequence remains bounded at 64 bytes. Lone Escape and command-mode cancellation retain
  their timeout behavior. EOF forwards incomplete ordinary sequences byte-exactly.
- Added delayed fragmented CSI, SGR mouse, bracketed-paste delimiter and EOF regressions with a
  printable prefix that appears in terminal parameters. All 38 client tests pass
  (`/tmp/fux-copy-fragment-client-tests.log`); strict Clippy passes
  (`/tmp/fux-contextual-edge-clippy.log`). The preceding resize change also passed all five CLI
  scenarios (`/tmp/fux-resize-cli-tests.log`) and strict Clippy (`/tmp/fux-resize-clippy.log`).
- Remaining: separate complete-diff review, final verification and requirement-by-requirement audit.

### Broader verification follow-up

- Main `cargo test --locked --all-features` passed (`/tmp/fux-contextual-edge-full-tests.log`).
  Formatting, rustdoc, root strict Clippy and fixture-child strict Clippy passed.
- Fixture-child tests are not yet green. The `prefix-paste` binary corpus driver still polls shared
  control listing metadata to observe entry into copy mode (`tests/verify/fixture-child/tests/binary.rs`,
  `PrefixBinaryDriver::input`). Copy now belongs to the viewer, so this old observation cannot
  establish entry; eventually the fixture lifetime ends and the pane disappears. Reproduced with
  the targeted corpus test (`/tmp/fux-contextual-fixture-corpus-retry.log`). Added scenario names to
  failure diagnostics. Next: update corpus observation/model expectations for viewer-private copy
  while retaining real rendered evidence, then rerun the fixture suite. This is unfinished work,
  not an unrelated failure to waive.

### Fixture corpus migration and dialog paste review

- The binary corpus now observes the rendered copy hint and inverse cursor and separately asserts
  that shared pane metadata has no copy selection. Shared terminal-frame expectations in the model,
  in-process interpreter, scenario and golden transcript now describe shared pane state; private
  selection isolation remains covered by the real two-viewer scenario. Terminal parser helpers use
  the actual PTY dimensions rather than fixed 24×80 dimensions.
- The corpus also retained the old numbered workspace-menu interaction. Updated it to wait for
  Choose workspace and use j/Enter; the existing lifecycle subscriptions still prove the source
  detach and destination attach. The full fixture-child suite passed
  (`/tmp/fux-private-copy-fixture-tests.log`), as did the corpus/structure oracle checks
  (`/tmp/fux-private-copy-oracles.log`).
- Separate input review found delayed bracketed-paste delimiters could lose paste protection in
  dialogs and prefix mode. Identified CSI/SS3 now remains bounded and buffered across delays in
  those contexts too. Lone Escape (including Escape after an incomplete sequence) still cancels;
  an incomplete sequence cannot expose its tail as an ordinary confirmation or command key.
  Added regressions for delayed pasted y/Enter in close confirmation and pasted prefix commands.
- Remaining: finish the explicit complete-diff review, affected final verification, and the complete
  prompt requirement audit. No commits, pushes or personal-session changes have been made.

### User-requested checkpoint and handoff

The user requested committing/pushing all clean work and stopping for a handoff. See HANDOFF.md
and the status preface in contextual-help-prompt.md. The goal is not declared complete.
Final review fixed long rename-input clipping, changed repeatable resize to a thin hint bar, and
made detach stop processing trailing commands while draining preceding ordinary input. The narrow
Unicode input and real two-viewer detach regressions pass. All 41 client tests and five CLI tests
pass; the final all-features suite, strict Clippy, formatting and dependency reconstruction pass.
Logs: /tmp/fux-handoff-full-tests.log, /tmp/fux-handoff-clippy.log,
/tmp/fux-contextual-review-ui-tests.log, /tmp/fux-review-detach-drain-test.log.
Remaining: finish the separate complete-diff review, fix any findings, perform the exhaustive prompt
acceptance audit and report final completion. No release-ready or full-review claim is made.


### Continuation: complete review, fixes and acceptance evidence (2026-09-05)

Independent full review covered the complete fux checkpoint and koh/zor owner changes. Fixed
five behavioral defects: UTF-8/C1 output filtering, SS3 arrows/keypad Enter, literal Escape prefix,
input drain across detach reads, and canceled-paste command leakage. Independent fix review found
no remaining source defect. Stable Clippy also exposed a test-only byte-array lint; its equivalent
replacement passes lint and review.

The final local pass has 258 root tests, 14 fixture tests, strict Clippy, formatting, rustdoc,
standalone dependency checks and required real-zor integration passing. Installed stable Rust
1.97.1 strict Clippy and fixture tests also pass. Required koh QUIC tests were attempted but this
environment denies netmon initialization with EPERM; eight gateway state/replay tests pass.
Checkpoint CI/nightly have failed jobs; public annotations and local stable reproduction narrow
follow-up, but detailed hosted logs could not be retrieved and fixture failures remain unexplained.
See HANDOFF.md and docs/contextual-help-acceptance.md for exact commands, scope and blockers.
No commits, pushes, PRs, CI reruns or personal-session mutations were made. The goal remains
incomplete; historical green integration logs are not substitutes for fresh verification.

### Environment unblocked and acceptance completed (2026-09-05)

Fresh required koh gateway (two tests) and reconnect (ten tests, including real-fux five-loss)
checks pass after environment restrictions changed. Individual GitHub job logs became retrievable:
CI Clippy failed on the already-fixed byte-character-array lint, and both CI/nightly macOS fixtures
missed final output because they inspected the restored primary screen after viewer exit. The
transcript proved final output was painted. The fixture now parses the actual frame before screen
restoration after joining the viewer/reader; a deterministic regression rejects erased text too.
Independent review approved the fix, and all 15 fixture tests plus strict Clippy pass on nightly
and stable. Root/fixture formatting and diff checks pass.

The requested review, audit and local verification are complete. Historical hosted failures remain
failed; these uncommitted local fixes have no hosted execution or release claim. See the current
acceptance audit and HANDOFF.md. No publication or personal-session changes were made.
