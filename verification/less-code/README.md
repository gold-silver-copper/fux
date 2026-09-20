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
