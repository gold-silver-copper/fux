# Less-code PR evidence

Base: `64adda9f76c55b1dc9b1d04201ba3b5f9c857827` (PR #26 merge).
Preflight: read review-2026-09-20 and PRs #24–26. Existing untracked `docs/`
files were preserved and are not part of this branch's changes.

## Reproducible production count

The measurement tool is separate from the application dependency graph:

```sh
python3 -m venv /tmp/fux-loc-env
/tmp/fux-loc-env/bin/pip install tree-sitter==0.26.0 tree-sitter-rust==0.24.2
/tmp/fux-loc-env/bin/python verification/less-code/count.py main
/tmp/fux-loc-env/bin/python verification/less-code/count.py
```

The Rust syntax tree excludes complete `#[cfg(test)]` items, comments (including
nested block comments), test files/subtrees, and `testing.rs`. Attributes and
code with trailing comments count. Raw-string contents are not mistaken for
comments/braces. Parsing errors fail measurement. Built-in assertions validate
those cases on every invocation. Files under nested production directories are
included. A revision argument reads sources directly from Git, not the worktree.

| State | Production lines | Change from base |
| --- | ---: | ---: |
| Base | 6882 | 0 |
| Section 1 | 6846 | -36 |
| Section 2 | 6799 | -83 |
| Section 3 | 6795 | -87 |
| Section 4 | 6787 | -95 |
| Section 5 | 6771 | -111 |
| Section 6 | 6766 | -116 |
| Section 7 | 6766 | -116 |
| Section 8 (tests only) | 6766 | -116 |

## Section 1

Required nodes replace identical spawn arguments. A workspace created by
`MoveTo::NewWorkspace` deliberately retains its explicit **tab** node: its basis
was Auto, unlike the root node's zero basis. Normalization still copies custom
layout into the first tab. Column splits override the row default. No node
mutation in the existing tab-as-split path was removed.

Reference: Bevy 0.19.1 `examples/showcase/breakout.rs` (`Wall` requirements) and
`crates/bevy_ecs/src/component/required.rs` (required constructors and precedence).

Verification: new required-node/explicit-override/removal unit test; existing
native-layout scene roundtrip, legacy normalization, split and multi-viewer
integration tests. All four gates pass (36 unit tests, 35 integration tests).
Gate log: `section1.log`.

## Section 2

`Presentation` is an unreflected viewer component owning the inert world and
paint/clipboard bookkeeping. `sync_view` snapshots the small viewer input before
borrowing it; painting copies rectangle values before mutating terminals. No
world/component extraction helper or unsafe aliasing is needed. Viewer removal
still has an observer, now removing the sibling component; despawn drops it.

The retained spike builds two identical inert apps and compares native geometry
through insertion, resizing (80x24 to 120x40), replacement (31-cell fixed width),
removal, native pointer picking and focus updates. It asserts explicit expected
`ComputedNode` sizes, changed results, tracker clearing, and message expiry.
`Main` plus `clear_trackers` matches the reference app on each iteration. The
spike also compiles `Presentation: Component + Send + Sync`, asserts there is
only the main sub-app, and asserts empty non-send storage after setup/updates.

Source audit: `App::update` delegates to `SubApps::update`; with no additional
sub-apps this runs the main default schedule and clears trackers. Installed
UI/text/image/asset/focus/visibility plugins install no non-send data. The
`TaskPoolPlugin` main-thread-only tick remains in `Last` inside `Main`; all
production presentation updates remain in exclusive world request/command paths
on the runner thread, not worker query systems. No OS-window plugins are used.

Rectangle lookup is shared, with `(height, width)` preserved for terminal sizing.
Thin `content_size` and `neighbor` adapters remain for their different return
contracts. `visible_leaf` is used only by focus-history navigation: bare test
worlds without a presentation keep their permissive fallback; production command
entry points always synchronize a presentation first (as before). There is no
remote access to this unreflected component. Tests retain hidden-pane rejection.

Verification: 38 unit and 35 integration tests, including viewer-component
removal without entity despawn, hidden-tab size negotiation, repeated clipboard
effects, tiny viewports and frame watches. Four gates pass; `section2.log`.

## Section 3 (partial: server-scoped observers retained)

`Viewing`, `OnTab`, `Focused` and `Overlay` carry hooks. Insert hooks both record
memory and queue repair. Bevy 0.19.1's derive composes custom insert hooks before
its relationship hook (`bevy_ecs_macro_logic/src/component.rs`); repair is deferred
until relationship maintenance is complete. The existing ancestor walk for old
focus is preserved, including non-pane descendants. Tests exercise replacement,
removal, invalid relationship targets and despawn without registration/manual
repair. Existing paste cancellation/reopening tests pass without registering an
overlay observer. The hook safely does nothing when `Ownership` is absent.

The two hierarchy normalization observers and three memory-pruning observers
remain in `navigation::observe`. This is a deliberate scope boundary, not a
forgotten conversion: `Tab`, `Workspace`, and `PaneView` are also inserted into
inert scene/presentation worlds where server observers were never installed.
A universal tab removal hook would create replacement tabs in those worlds;
`inert_scene_worlds_do_not_normalize_tab_removal` retains the contrary baseline.
Avoiding that needs a server marker/registration again. `DeferredWorld::query`
also requires an existing `QueryState`; immediate all-viewer pruning cannot use
an unrestricted fresh query. A cached query resource or deferred pruning would
add machinery or change timing, so the existing scoped observers are smaller
and preserve the boundary. The `ChildOf` observer remains explicitly registered.

Verification: 40 unit and 35 integration tests; all four gates in `section3.log`.

## Section 4

Scene I/O returns `CommandQueue`, polled with Bevy 0.19.1's `check_ready` as in
`examples/async_tasks/async_compute.rs`. Removal of the pending marker is queued
**before** appending completion commands, preserving fux's existing ordering
rather than blindly copying the example. The result's error/notice mapping,
synchronous filesystem operation on `IoTaskPool`, viewer-bound cancellation,
wake and 25ms runner deadline remain unchanged. Replacement requests still drop
the old task through the same component replacement path.

A unit test performs real save/load/failure completion using only deadline-style
polling (no controls or paints), compares exact notices, and asserts the pending
marker is absent during its completion command. A barrier-controlled task tests
that dropping the component during an already executing synchronous filesystem
interval does not interrupt that operation. This does not promise that a task
cancelled before its first poll starts I/O; the baseline did not guarantee that.
Existing integration coverage verifies scene mappings, failed mappings, and
process identity across replacement. 42 unit and 35 integration tests pass;
four gates in `section4.log`.

## Section 5

Folded commands and exact old/new JSON are in `wire.md`. `Scope` moved into
`control.rs`; the existing `interaction::MoveTo` is the single wire/domain
destination type. Both are registered with the type registry. Chooser variants
remain unchanged, but entity collection and entry construction are shared.
All 59 action names and configuration bindings retain their original spelling.
Tab-only guards (`no tab`, `only one tab`) remain tab-only; workspace singleton
navigation remains permitted. Close routes through the same subject check.

Golden tests round-trip both scopes, both order values, all move destinations,
and explicit entity-bit encoding; all removed kinds and the old unscoped reorder
shape are rejected. Bad/missing scopes, destinations and entity types are also
rejected. No legacy aliases were added. 44 unit and 35 integration tests pass;
four gates in `section5.log`. README wire examples are updated in section 9.

Only `tests/design/interactions.rs` changes existing integration calls:

| Test | Wire changes (exact examples in `wire.md`) |
| --- | --- |
| `hidden_tabs_stop_constraining_pty_size_even_before_the_switching_viewer_paints` | tab_previous → previous/tab |
| `workspace_order_chooser_memory_and_scene_replacement_are_consistent` | workspace_reorder/select/next/previous → scoped operations |
| `nested_swap_and_existing_tab_workspace_moves_keep_process_identity_and_history` | tab/workspace previous and move-to-existing destinations |
| `tabs_bar_native_click_chooser_and_independent_focus_survive_switches` | tab_previous → previous/tab |
| `interactive_close_is_modal_captured_and_automation_is_explicit` | tab_close → close with tab subject (including wrong-kind test) |
| `directional_previous_last_focus_and_rearrangement_preserve_processes` | move_to_new_tab → move/new_tab; tab_previous → previous/tab |

The harness adds `scoped(viewer, kind, scope)`; plain `command` still sends only
kind. Three interaction unit-test command constructors also change shape. No
behavioral assertion or observed response field was changed.

## Section 6 (shared predicates, not a universal command check)

The pre-edit guard matrix is in `availability.md`. `Target::multiple_panes` and
`multiple_tabs` now supply the same cardinality predicate and static reason to
menu and command adapters. Clipboard validation returns its existing static
errors and is reused by the menu for empty text, preserving optional-settings
behavior. Checks still accept `&World`, with no mutation or deferred commands.

A universal `check` was not introduced: no-pane directional moves currently
return `only one pane` in direct execution but `no pane` in the menu; settling
selection/prefix/notices before execution failure is also observable. Prompts
have no immediate command, and captured menus can name a target other than the
requester's current focus. `Target::valid` and `needs_pane` therefore remain.
Tests exercise all 59 actions against empty/singleton/multiple/stale targets,
optional/disabled/enabled clipboard settings, an unrelated workspace, direct
command guard precedence, and UI settling on failure. 46 unit and 35 integration
tests pass; all four gates in `section6.log`.

## Section 7 (typed attach parsing omitted)

`Subject` provides entity/kind for checks, rename/reorder and confirmation text.
Pane rename still resolves the referenced process before applying the name.
`URect` replaces the four-field chrome bounds type; native UI picking continues
to own hit-testing. Bevy's `URect::contains` includes the maximum edge, so it is
**not** substituted for half-open terminal hit tests. A new native-picking test
checks left/right/bottom boundaries and zero-width chrome. Existing Unicode,
overflow and zero-viewport tests still pass. The test-only `bar` is deleted;
its style test calls production `tab_bar` with no tabs instead.

`spawn_pane` and workspace creation now use immediate world spawns; initialization
is exclusive. The former command/flush pairs are gone. `Launch` process creation
runs in the unchanged `TerminalSystems` Update chain, not an insertion hook;
the pane's parent already exists before insertion. Tab/ChildOf observers still
see complete spawn bundles, and split child insertion order is unchanged. All
real-PTY creation, move, close and process-lifecycle tests pass.

`Viewer::reset_view` replaces the three actual two-field reset sites (the old
seven-site estimate was stale). Single-field resets remain untouched. Production
line count for this section is neutral: explicit type/conversion helpers offset
the eliminated duplication; the branch remains negative overall.

Typed attach parsing is deliberately omitted: preserving permissive non-object,
wrong-type and overflowing dimension inputs needs custom deserialization that
is longer than the current field reads. A retained test covers absent/null,
non-object, negative/fractional/wrong-type, zero, u64::MAX, omitted single fields,
unknown fields, valid/missing workspace names, and clamping before conversion.
No second wire break was introduced. 48 unit and 35 integration tests pass;
four gates in `section7.log`.

## Section 8 (production refactor rejected; regression evidence retained)

The barrier-controlled spike disproves equivalence, so `PendingAssets`,
`LoadWake`, guards and watcher marking remain unchanged. This section's commit
contains tests/evidence, **not** the proposed asset-state refactor.

Exact reproduction:

```sh
cargo test --locked load_state_cannot_replace -- --nocapture
```

Output (`asset-spike.log`):

```text
barrier: after reload + full update, native state=Loaded, native pending=false, bridge pending=true
idle runner: initial load, queued reload, invalid config, recovery, failed layout and removed layout all settled without requests
```

The retained test isolates a one-worker I/O pool in a subprocess, loads settings,
blocks that worker with a channel barrier, marks the path as the watcher does,
and calls native `AssetServer::reload`. After a **full App update**, the old
asset is still `Loaded`; the reload task has not run to mark `Loading`. Native
load-state-only pending would let `main.rs` park indefinitely at this point.
Releasing the barrier and using the existing pending deadline completes reload.
The subsequent invalid-config/recovery/layout-failure/layout-removal cycles use
only actual file-watcher events: no explicit reload, remote requests or frames.
An eight-second watchdog fails *before* another update, so it cannot mask a lost
wake. No sleeps are used to manufacture the counterexample's scheduling window.

Source trace, Bevy 0.19.1:

- `server/mod.rs::handle_internal_asset_events` handles filesystem events by
  calling `reload_internal`; it does not synchronously change the load state.
- `reload_internal` spawns on `IoTaskPool`; only that future calls
  `load_internal(..., true, None)`.
- `server/info.rs::get_or_create_path_handle_internal` marks the state `Loading`
  for `HandleLoadingMode::Force`, after that future starts. `Last` ticks local
  executors, not the global pool on which `reload_internal` spawns.
- Initial loads retain `with_guard(LoadWake(..))`; completion/failure events are
  applied by the asset schedule. Settings/layout consumers run after
  `AssetEventSystems` in `PostUpdate`; layout consumers emit `LayoutReload` for
  the runner's next `Update`, and explicitly wake it. Native load-state success
  alone is not proof all fux consumers have run.
- A settings layout path is an independent load initiated by `track_layout`;
  replacing/removing it clears the old pending path. Failed settings retain
  usable values; failed layouts clear their apply flag. The idle test covers
  these paths. Direct and recursive load states also differ for asset
  dependencies; no claim of interchangeable recursive readiness is made. The
  earlier demonstrated primary-state race already rules out the replacement.

All 49 unit and 35 integration tests pass, including existing settings and
keybinding hot-reload tests. Four gates: `section8.log`.
