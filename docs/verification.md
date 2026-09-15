# Verification record

Dated records per milestone (prompt section 6). Each entry names the commands run and the
observed result; blockers are recorded where they were found.

## 2026-09-15 — Milestone 1: workspace skeleton

* `cargo check --workspace`: fux and zor compile against the section-2 stack (cold check 3m38s on
  Apple M2 Max). No `DefaultPlugins`, no `bevy_winit`.
* `cargo run --manifest-path tools/xtask/Cargo.toml -- deps`: passes with the narrowed rule
  documented in `docs/dependencies.md`.
* CI workflow `.github/workflows/ci.yml`: fmt, clippy, build, test, doc, package, deps report.

### Accepted deviation: render crates in the graph (user-approved 2026-09-15)

`bevy_remote` 0.19.1 hard-depends on `bevy_dev_tools`, which hard-depends on `bevy_render`
(details and the enforced replacement rule in `docs/dependencies.md`). The prompt's "no wgpu in
the resolved graph" acceptance rule contradicts Bevy source and neither the precedence rule of
section 1 nor the forking policy of section 2 resolves it (Bevy crates are never forked). The user accepted wgpu in the graph. Decision
taken: keep `bevy_remote` as published, never use render types, enforce the narrowed rule in
`xtask deps`, and propose the upstream change. No milestone task depends on this beyond the
report itself.

Pre-existing `ecs-rewrite` branch (an older, fully merged attempt at 0.3.0, tip `4265ce5`) was
renamed to `ecs-rewrite-2026-09-05-merged` so the orphan branch could take the prompt's name.

## 2026-09-15 — Milestone 2: fux App shell

Commit `0e49c4b`. 14.7k lines in `crates/fux/src`. Results:

* `cargo check --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`: clean
  (`type_complexity` allowed workspace-wide; test files carry a crate-level allow for
  unwrap/expect/panic/indexing in helpers, since `clippy.toml` only relaxes `#[test]` bodies).
* `cargo test -p fux`: 80 tests green across `--lib` (27), `attach` (7), `brp` (4),
  `layout_mechanism` (2), `layout_ops` (14), `layout_props` (1 proptest: random template edits
  interleaved with attach/detach/show/resize/zoom, `check_invariants` + instance==template after
  every update), `lifecycle` (6), `pty_adapter` (5), `shutdown` (3), `viewer` (11).
* Smoke (real processes, disposable XDG dirs, hub-supervised `fux serve --name smoke`):
  `<runtime>/fux/smoke.brp.json` written 0600 with http/attach ports and tokens;
  `fux --server smoke fux/server.info`, `fux/workspace.list`, `world.query` over `PaneView`
  answer; `world.query` over `fux::model::components::Process` is refused (-32002);
  `rpc.discover` lists exactly the allowlist (32 methods, no `world.*` mutators).
* Smoke (viewer over a Python `pty.fork`, 24x80): attach shows the shell; `echo` echoes;
  `C-b %` splits and the second pane goes starting → live and receives input; `C-b d`
  detaches with exit status 0 (fixed during integration: a `Bye` and the socket EOF in the same
  batch used to let the EOF win, `viewer/mod.rs`).
* Smoke (two viewers, 24x80 and 40x120, one root): `fux/workspace.list` shows 2 viewers and
  every pane's size folded to the minimum (23 rows: 24 minus the status bar; 16 cols across
  five panes); exact attach `--pane 1 --pid <pid>` attaches, `--pid 1` is refused with
  `Refused: pane pid mismatch`; all viewers detached cleanly.
* Smoke (shutdown): `kill -TERM` with five live shells: server exited within 1 s with status 0,
  descriptor removed, no orphaned shells.

Recorded deviations from the prompt (all in code comments / HANDOFF):
* `SpawnPane` is emitted in `PostUpdate` after the size fold (so the PTY opens at the laid-out
  size), not in `Requests`.
* Close ownership: lifecycle marks viewers `Detaching`; the attachment projection sends `Bye`
  with the reason derived from world state and emits `Effect::CloseViewer`.
* Frames are JSON (`serde_json`) rather than a binary row format; per-cell `String`s are the
  known allocation on the frame path, to be measured in milestone 8.
* `bevy_remote` requests are forwarded from `BrpReceiver` into a fux mailbox with
  `Inbound::Wake` so the runner sleeps with no polling; dispatch runs in `RemoteLast` moved
  after `First`.

## 2026-09-15 — Milestone 3: scene completeness

### Layout matrix (LayoutMatrix)

`cargo test -p fux --test layout_matrix` (14 tests) lays out flex/grid/absolute/overflow templates
with the real `bevy_ui` at 80x24, 120x40, 40x12 and 200x50 and asserts cell-exact rects
(`ComputedNode.size`, `UiGlobalTransform` centre): two-column, two-row, three columns with
`min_width: 20px`, a 2x2 `1fr` grid, an explicit `grid_row`/`grid_column` placement, main+sidebar
(`flex_grow` 3/1), an absolute overlay at inset 2 with 50 % size and `ZIndex(1)` (topmost for
`pane_at`), and an `Overflow::scroll_y` column of ten 5-row panes with `ScrollPosition`. New in
the same run: `NodePatch` grid placement / auto tracks / `aspect_ratio` / `inset` /
`overflow_clip_margin` (invalid values refused as a whole), `ops::exchange` across parents,
per-viewer `ops::set_display` surviving a re-clone. `layout_ops` (14) and `layout_props` (1)
stay green.

**Old vs new split rects.** The old `layout.rs` `split_rect` at the default 5000/10000 ratio
left one separator cell between the halves: first = `floor((extent - 1) / 2)`, second starts one
cell later. Taffy rounds every edge to a whole cell and the halves tile the viewport, so for every
default split the **first half is exactly one cell wider (the separator cell) and the second half
is identical**; the test asserts this rule for the two-column, two-row and nested
`[1 | [2 | 3]]` cases at all four viewports. Numbers at 80x24: old `39 | 40` (x 0, 40) → new
`40 | 40`; rows old `11 | 12` (y 0, 12) → new `12 | 12`; nested old `39 | 19 | 20`
(x 0, 40, 60) → new `40 | 20 | 20`. At 40x12: old `19 | 20` → new `20 | 20`, rows old
`5 | 6` → new `6 | 6`. Structural difference recorded, not a rounding one: the old tree always
nested, while `ops::split` on a leaf whose parent already flexes along the axis adds a sibling,
so two right-splits give three equal columns `27 | 26 | 27` (edges 0, 27, 53, 80) instead of
the old `39 | 19 | 20`.

**Size fold.** `PaneSize` is now the visible content box: border box minus borders, intersected
with the inherited `CalculatedClip` (scroll/clip containers, `Display::None` subtrees). In the
scroll case at 24 rows the fifth pane (rows 20–24) folds to 4 rows, panes scrolled out of view
keep their last size, and after `scroll(+7)` the partly visible second pane folds to 3 rows.
Absolute overlays fold to their own box (12x40 at 80x24). Disabling the clip intersection makes
the scroll test fail (`rows: 5` vs `4`), so the fold is proven by the test.

### Surfaces (Surface)

`cargo test -p fux --test surface` (10 tests) against the real `bevy_ui` layout and, for
replication, a real loopback attachment plus a headless viewer App:

* `surface::open` refuses a placing leaf, a node with children and a repeat; `ops::spawn_node`
  under a surface leaf is refused (`LayoutError::SurfaceSubtree`, LayoutMatrix). Over BRP, `open`
  is generation-checked (`-32001`), workspace-scoped and needs the leaf in that workspace.
* A full update with a column of three 1-row `Text` leaves (exported from a provider World with
  `DynamicWorldBuilder`) lands under the leaf as `TemplateNode`s without `NodeId` (provider nodes
  are not client-addressable); at 80x24 the instance rects are exactly the leaf's `40..80 x
  0..24`, rows at y 0, 1, 2 with 40x1; instances carry `Text`. Provider ids are stable: a partial
  update rewrites one `Text` in place, appends a new child under an existing parent, and a full
  update that omits two entities despawns their template nodes and instances. A full update's
  sibling order follows the parent's `Children` list; moves between parents go through
  `add_child` because `replace_children` inserts with relationship hooks skipped and would leave
  a moved entity listed under its old parent (found by the reorder test).
* Stale revision → `-32001` with `{ "revision": current }`; refusals commit nothing: 129 nodes
  (`Limits.nodes_per_workspace / 4 = 128`), a 65-deep chain, `GlobalZIndex` (outside the
  vocabulary), a parent outside the delta, a new entity without `Node`, malformed RON and a
  4097-byte `Text` all leave the subtree, revision and `LayoutGeneration` untouched.
* Pacing per surface: the 61st update in one second is refused, the window follows the server
  `Clock`, and one 280 KiB delta is refused on its own (256 KiB/s). A full `Node` export from
  `DynamicWorldBuilder` is ~3 KiB per node (required `BackgroundColor`/`BorderColor` come
  along), so providers that stream at rate must send partial `Node`s (only the fields they set;
  the reflect deserializer accepts them) or partial updates.
* `close` despawns the subtree, removes `Surface`/`SurfaceState`, keeps the leaf, and the leaf
  takes ordinary `node.spawn` children again.
* Replication: after `open`+`update` the attached viewer receives a delta `SceneFrame` whose RON
  carries `fux::surface::Text` and `fux::model::components::Surface`; fed into `viewer::build`
  the viewer replicates two `Text` entities and paints `tasks`/`checks` at columns 40.. of rows
  0 and 1.
* Viewer B1 (review): `paint::emit` now clears `Painter.out` before composing; `tests/viewer.rs`
  hands the written buffer back the way the runner does and asserts the next identical frame
  emits nothing.

**Gap (milestone 4):** input to a surface is not routed back to its provider. `FocusedInput` /
`Pointer` events on surface nodes need `fux/events+watch`; until then a surface is display-only
and its provider drives it from its own inputs. `ScrollPosition` in a delta is applied to the
template and cloned into instances, so a provider can scroll its own subtree; viewer-side
scrolling of surface nodes is not exposed (surface nodes have no `NodeId`).

### Scenes (Scenes)

`cargo test -p fux --test scenes` (7 tests) against the real `bevy_ui` layout and the headless
lifecycle (`build_headless`), `check_invariants` after every step:

* `scene::export` extracts the workspace's template subgraph with `DynamicWorldBuilder::deny_all`
  plus explicit allows (`Node`, `ChildOf`/`Children`, `ZIndex`, `BackgroundColor`, `BorderColor`,
  `Name`, `NodeId`, `RootOf`, `TemplateRoot`/`TemplateNode`, the workspace entity's
  `WorkspaceName`/`RootOrder`, and behind each placing leaf a pane entity with `PaneId` +
  `LaunchAttribution` linked by `Places`). A plain export never carries `PaneTemplate`. Exporting
  and applying `Row [ leaf, Column [ leaf, leaf ] ]` rebuilds the same shape on fresh template
  entities (old tree despawned, panes untouched, `PlacedIn` exactly one), the viewer follows its
  target pane into the new root and, after one update, the re-cloned instances measure 40x24,
  40x12, 40x12 at 80x24.
* Guard: `ApplyOptions::expected` is the `(NodeId, LayoutGeneration)` list of the roots the
  caller saw. A plain generation vector was not enough: rebuilding an identical shape lands on
  the same generation number, so the second apply of the same export was accepted until root
  identity became part of the check. Over BRP a mismatch is `-32001` with
  `{ "roots": [{root, generation}] }`.
* Refusals commit nothing (roots, shape and invariants unchanged): a pane of another workspace
  (`ForeignPane`), an unregistered type path (`Parse`), a registered component outside the
  allowlist such as `Process` (`ComponentNotAllowed`), a 65-deep chain (`DepthExceeded`; 64 is
  accepted), 4 nodes against `nodes_per_workspace = 3` (`TooManyNodes`), an unknown `PaneId`
  (`PaneNotFound`), live panes the document leaves unplaced (`UnplacedPanes`, listing them),
  an empty `RootOrder` and any resource. Validation runs on an inert scratch `World` that
  shares the `AppTypeRegistry`; `RootOrder` gained `#[entities]` so `write_to_world_with` maps
  its roots like every relationship.
* Template scenes: without `allow_templates` the built-in `two_column` is refused; with it two
  panes appear `Disabled` + `Process::Starting` + `Creation { kind: Restore }` with the default
  command resolved from the empty `argv`, the next update's `materialize` emits exactly their
  `Effect::SpawnPane`, and with `close_unplaced` the three previous panes are closed through
  `lifecycle::close_pane` and gone two updates later.
* `restore` with `adopt` on a one-pane workspace: the existing pane fills the `left` leaf of
  `two_column`, one pane is launched for `right`, instances at 80x24 are 40x24 each with the
  right one centred at column 60 (the old two-pane split), the viewer keeps its target. A user
  file of the same name shadows the built-in. All four built-ins (`two_column`, `two_row`,
  `three_column`, `main_side`) apply into an empty workspace and fill the 80x24 viewport.
* Files: `<dir>/<name>.scn.ron`, names `[A-Za-z0-9_-]{1,64}` (`""`, `../x`, `a b`, `x/y`, 65
  chars refused for save and load), `list` sorted and empty for a missing directory, a save into a
  read-only directory leaves neither the file nor a temp file behind (0600 temp + fsync + rename).
* Entity ids in hand-written documents: Bevy 0.19 stores the entity index inverted in the low 32
  bits, so `u32::MAX - n` is entity `n` (generation 0) and `4294967296` is refused as invalid.
* BRP: `fux/scene.{export,apply,list,save,restore}` in `remote/scene_methods.rs`; fixtures and
  `schema.json` re-blessed. Smoke over HTTP with a real `fux serve` (PTY panes): `layout list`,
  `layout save mine` (6995 bytes, 0600), `layout load two_column` adopted pane 1 and launched a
  live pane 2, `scene.apply` with the old `expected` → `-32001`, `layout load mine` refused with
  the unplaced pane listed, `--close-unplaced` closed it.

**Fixed at integration:** `tests/brp.rs::end_to_end_over_http` was racy (the test harness
bootstrapped after the first update had already published the descriptor; `fux serve` itself
bootstraps in `Startup`). The harness now bootstraps before the first update; 6/6 runs green.
