# Availability audit before section 6

Comparison uses section 5's command names. `actions::unavailable` checks a
captured `Target`; `server::execute` settles selection/prefix/notice, validates
the requesting viewer, then derives its current target and filters non-PaneView
focus. A wholesale shared preflight would change both targeting and effects.

| Case / order | Menu / bound action | Direct execution |
| --- | --- | --- |
| Stale captured target | `target no longer exists here`, before every other guard | requester relationships and per-command subjects/destinations checked; no global captured-target check |
| Detached viewer | no target to construct menu | `viewer no longer attached` after settling UI |
| Clipboard disabled | policy reason before no-pane test | same policy reason before no-pane test, then initialized presentation, queue capacity, terminal, encoded size |
| No pane | every pane/focus action except split: `no pane` | operation-specific; directional move/swap checks singleton **before** `beside` checks no pane |
| No tab | every action in Tabs, including choosers/prompts | tab-new/scoped-tab operations: `no tab`; other operations use their own explicit subjects |
| Singleton tab | next/previous/reorder: `only one tab` | same cardinality predicate and reason, after no-tab guard |
| Singleton pane | swap chooser, directional move/swap, next/previous/last focus, reorder: `only one pane` | same predicate where present, but guard order varies; explicit swap checks its destination instead |
| Singleton workspace | navigation allowed | navigation allowed (no added singleton guard) |
| No Terminal component | terminate: `process is not running`, after pane test | same missing-component reason; `Terminal::stop` can additionally fail at runtime |
| UI-only action | rename/layout actions produce `None` until prompt completion | `None` is not a command and must not become `no pane` |
| Captured menu on another entity | predicates use captured workspace/tab/pane | execution often uses current requester target, while Close/Rename use explicit subjects |
| Directional focus with no neighbour | not globally dimmed for that direction | allowed no-op |

Plan: share pure pane/tab cardinality predicates on `Target`; retain adapters,
per-command errors, their original ordering, and `Target::valid`. Clipboard text
validation is already shared by execution/copy paths. Let it return its static
error strings so the menu can reuse it for empty text while retaining its
optional-settings lookup (not equivalent to resource-required execution). Do
not centralize side-effectful settling or turn all menu decisions into execution
preconditions.
