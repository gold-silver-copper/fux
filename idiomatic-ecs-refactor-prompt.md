# Make fux idiomatic: proper bevy_ecs, modern Rust, less code

Execute a behavior-preserving refactor of fux that uses `bevy_ecs` the way it is meant to be used,
replaces hand-rolled machinery with the language's and the crate's own features, deletes cruft
and history, and materially reduces maintained source. Implement and verify the changes; do not
stop at a plan. The deliverable is a pull request against `main`, not a push to it.

## Objective

fux 0.3.3 is a working multiplexer whose model lives in a `bevy_ecs` 0.19.1 World, but the World
is driven almost entirely through exclusive `&mut World` systems, hand-written lookup tables,
manual cascade functions and free helper functions that reimplement what components, queries,
relationships, system parameters and derives already provide. The viewer is a set of explicit
state machines with repeated bounds and truncation code. The repository also carries the prompts
and audits of four earlier architectures.

Target these areas:

1. Model ownership and lookups with the ECS instead of beside it.
2. Write systems as ordinary bevy systems with typed parameters; keep exclusive access only
   where a phase genuinely needs the whole World.
3. Replace repeated Rust with the idiom that removes it: derives, newtypes, iterators, `?`,
   `impl Trait`, `From`/`TryFrom`, `thiserror`, small macros only where they delete more than
   they add.
4. Delete cruft: historical documents, superseded prompts, compatibility left-overs, dead public
   functions, duplicated helpers, comments that describe removed designs.
5. Reduce hand-maintained source substantially without losing a capability, a protocol guarantee,
   a test assertion or a documented behavior.

Backwards compatibility of internal APIs is not a concern. The external contracts are: attachment
protocol v5, control protocol `FUXCTL2`, the manager protocol, descriptor files, the configuration
file, the CLI surface, the default bindings and the viewer's documented behavior. Those do not
change. Capability, correctness, readability, MSRV 1.95 and the verification gate are concerns.

## Starting point and authorization

- Baseline: `main` at `5449616` (0.3.3). Re-fetch and record the actual SHA you branch from.
  Read README.md, HANDOFF.md, docs/design.md, docs/ecs-plan.md, docs/ecs-acceptance.md, the
  `Cargo.toml` lints and the CI workflows before touching code.
- Work on a branch (`refactor/idiomatic-ecs` or similar) in an isolated worktree. Never push to
  `main`. This prompt authorizes local implementation, verification, documentation, independent
  reviewer subagents, commits on the branch, pushing the branch and opening one pull request
  (draft until the completion gate below passes, then ready for review). It does not authorize
  merging, force-pushing `main`, editing koh or zor beyond what an unchanged protocol needs (which
  should be nothing), touching personal sessions or runtime directories, or killing any server the
  task did not start. Use disposable HOME/XDG directories for every real-process test.
- Owner repositories (`references/koh`, `zor/`) stay at their pinned bases with their existing
  patches; if the protocol truly needs a change, stop and report instead of changing them.

## 1. Baseline evidence and deletion plan

Record reproducible baseline counts before any change, by file class: production Rust under
`src/`, test Rust under `tests/` and `tests/verify/fixture-child/`, Python harnesses, documentation,
CI and configuration. Use one method (for example `tokei` or `wc -l` over an explicit file list)
and keep the command in the report. At the baseline, `src/` is about 16,000 lines across 40
files; the largest are `ecs/systems/requests.rs` (1,256), `client/controller.rs` (1,036),
`client/render.rs` (889), `main.rs` (839), `layout.rs` (721) and `proto/control.rs` (704).

Inventory every candidate with: current implementation and callers, the idiom or ECS feature that
replaces it, the exact code to delete, the tests that pin its behavior, and the expected net
change. Prefer concrete repetition over speculative abstraction: a helper, trait or macro must
replace more lines than it costs, in the same PR. Moving code between files is not a reduction.

Deletion candidates to inspect first:

- `docs/standalone-*.md`, `docs/contextual-help-*.md`, `docs/design-before-standalone.md`,
  `docs/refactor-*.md`, `docs/release-readiness-before-standalone.md`, `docs/wrapper-*.md`,
  `docs/build-prompt.md`, `docs/verification-prompt.md`, `docs/koh-pr*-prompt.md`,
  `docs/project-boundaries-refactor-prompt.md`, `docs/implementation-notes.md`, and every
  `*-prompt.md` at the repository root including this one's predecessors. They are labelled
  historical and exist in git history; delete them and keep one short "History" section in
  docs/design.md that names the architectures they described. Keep `docs/ecs-plan.md` only if it
  is still referenced as the current plan; otherwise fold what matters into docs/design.md.
- `HANDOFF.md`: replace the accumulated per-change narrative with a short current-state handoff.
- `docs/ecs-acceptance.md`: keep the requirement tables and the review record; drop the running
  diary of gate outputs in favor of the commands and the final results.
- Dead or single-use public items (`commands::action_label`, re-exports nobody uses, `pub` on
  items only tests reach), duplicated truncation helpers (`render.rs` and `hints.rs` both keep
  head/tail truncation and bounds-checked string writes), duplicated "find entity by public id and
  check it belongs to this workspace" code, duplicated "collect entities then loop" patterns that
  exist only to satisfy the borrow checker.

## 2. Use the ECS properly

The plan in docs/ecs-plan.md chose entities for workspaces, tabs, panes and viewers and a
`LayoutTree<Entity>` for splits; keep that. Change how the model is expressed and driven:

- **Relationships and cascades.** Workspace → tab and tab → pane ownership is currently a `Vec<Entity>`
  in `Workspace`, an `Entity` back-pointer in `Tab`/`Pane`, and hand-written despawn cascades in
  `support.rs`, `lifecycle.rs` and `creation.rs`. Evaluate bevy 0.19 relationship components
  (`ChildOf`/`Children` or a custom `#[relationship]` pair) for these edges so membership, ordering
  and despawn cascades come from the ECS, with the explicit rule kept that despawning a viewer
  never cascades. Keep the documented invariant that a `Terminating` pane may outlive its tab
  until its exit report arrives, and that processes are stopped through effects, never through a
  despawn hook. If relationships cannot express an edge safely, say why and keep the field.
- **Public ids.** `Ids` maps `PaneId`/`TabId`/`ViewerId`/name → `Entity` by hand. Replace the
  lookups with the ECS's own index where one exists in 0.19 (entity index/`EntityHashMap`, or a
  component-keyed query), or keep one generic `Registry<Id>` implemented once instead of three
  copies. Ids must stay never-reused within a server lifetime.
- **System parameters.** Rewrite the phase systems as ordinary systems taking `Query`,
  `Res`/`ResMut`, `Commands`, `MessageReader`/`MessageWriter` (the 0.19 names) and derived
  `SystemParam` bundles (for example one `Viewers<'w, 's>` param that resolves a `ViewerId` to its
  entity and components). Use `Query` filters (`With`, `Without`, `Changed`, `Added`) and
  `Single`/`Option<Single>` where they replace manual searching. Keep an exclusive system only
  where a phase needs deferred-mutation visibility mid-phase (creation completion is the likely
  candidate); document each remaining `&mut World` system in one sentence.
- **Change detection instead of dirty flags.** `Pane.dirty`, `Viewer.dirty`, `Tab.layout_changed`
  and the manual `mark_*_dirty` helpers duplicate what `Changed<T>`/`Mut` change ticks provide.
  Replace them where the semantics match (the schedule already calls `clear_trackers` per step);
  keep an explicit flag only where a change must be observed across steps.
- **Messages and effects.** Keep `Messages<Inbound>`/`Messages<Effect>` as per-step transport
  (never a queue) but read them with `MessageReader` in the systems that consume them instead of
  cloning the whole vector in each phase. Consider splitting `Inbound` into per-source message
  types so each system reads only what it handles.
- **Schedule.** Keep the single chained `Step` schedule and the `SingleThreadedExecutor`. Express
  ordering with `.chain()`/`.after()` on sets, and let bevy insert the sync points; remove any
  manual "run this system's tail" calls (for example `drain_viewer_queues` invoked from inside
  another system) by ordering systems instead.
- **Observers and hooks.** Do not add observers for core command flow. A component hook or
  observer is acceptable only for a small local invariant that is currently enforced by a helper
  called from several places (for example removing a despawned pane from `Ids`), and only if it
  cannot leak a process or socket handle.
- **Components as data.** Split `Pane` and `Viewer` into the components the systems actually query
  (`PaneProcess`, `PaneTerminal`, `PaneGeometry`, `ViewerSize`, `ViewerSelection`, `ViewerQueue`,
  …) where that lets systems take narrower parameters. Do not create components for cells,
  history lines, bytes or keypresses.
- **Invariants.** `Session::check_invariants` stays, rewritten over queries; the randomized
  ECS test keeps running it after every step. Nothing may be silently relaxed.

## 3. Idiomatic Rust everywhere else

- Errors: one `thiserror` enum per module boundary that crosses a process or socket, `?`
  everywhere, no stringly-typed error routing; keep `anyhow` at the binary boundary only.
- Newtypes and enums over bare `u16`/`u32`/`bool` pairs where a wrong argument order is possible
  (rows/cols, generation, exit codes).
- Iterators and combinators over index loops; `let … else`; `if let … && …` chains (already the
  edition); `impl Trait` in argument position; `From`/`TryFrom` for the conversions that are
  currently free functions (`Style` → `Palette`, `StyleColor` → `Color`, key notation).
- Derive macros over hand-written impls; a small declarative macro only where it deletes repeated
  code (the `Action` table: labels, groups, availability and default keys are four parallel
  matches over the same variants).
- The viewer: `controller.rs` (`Mode` state machine), `input.rs` (`PrefixFilter`), `copy.rs`,
  `hints.rs` and `render.rs` each keep private truncation, clipping and coordinate code; share one
  small text-layout module and one bounds-checked write. Keep the explicit state machines (the
  spec allows them); make each `Mode` carry its own data and handle its own keys through one
  `impl` per mode instead of one large `match`.
- `main.rs`: the CLI aliases (`new`, `split`, `focus`, …) repeat argument parsing and JSON
  building; table-drive them. The migration dialog and the manager RPC belong in `daemon/`, not
  in the binary.
- Configuration: `Config`/`ConfigPatch` double every field; derive the patch (or use
  `#[serde(default)]` with `Option` fields and one `merge`) so a new key is added in one place.

Do not add dependencies beyond what `bevy_ecs` 0.19.1 (`default-features = false`,
`features = ["std"]`) and the existing crates provide. If a `bevy_ecs` feature such as
`bevy_reflect` would remove real code, argue it in the report; do not enable it by default.

## 4. Delete in the same change

For each deleted item, record where its behavior now lives or why nothing needs it. Absence of
in-tree callers is not proof: check the docs, the CLI help, the harnesses and the owner patches.
Remove stale comments and doc paragraphs that describe the old host, popup panes, sidecars,
protocol v2–v4 or the top bar. Strengthen or retarget `tests/structure.rs` guards when their
requirement survives; never delete a failing guard to unblock a refactor.

## 5. Preserve the contract, prove it

Every existing test keeps passing or is migrated with its assertion intact and an entry in an
assertion ledger: `tests/ecs.rs` (19 deterministic tests including the randomized sequence at
2048 cases), `tests/structure.rs`, `tests/local_cli.rs` with all six Python harnesses,
`tests/zor_integration.rs` with a real `ZOR_BIN`, the fixture-child binary suite, and the koh
gateway suites against the new binary. The performance script `tools/measure.py` runs before and
after on release builds; idle CPU, latency and memory must not regress beyond noise, and any
change is explained.

Run the full gate on the final tree and record it in docs/ecs-acceptance.md:

```sh
cargo fmt --all --check
cargo clippy --all-targets --locked -- -D warnings
ZOR_BIN=$PWD/zor/target/debug/zor FUX_REQUIRE_ZOR_BIN=1 PROPTEST_CASES=2048 cargo test --locked -- --test-threads=1
cargo doc --no-deps --locked
cargo +1.95.0 check --all-targets --locked
cargo test --locked --manifest-path tests/verify/fixture-child/Cargo.toml
FUX_BIN=$PWD/target/debug/fux KOH_REQUIRE_FUX_BIN=1 cargo test --manifest-path references/koh/Cargo.toml --test gateway --locked
FUX_BIN=$PWD/target/debug/fux KOH_REQUIRE_FUX_BIN=1 cargo test --manifest-path references/koh/Cargo.toml --lib gateway:: --locked
tests/verify/release-package.sh --allow-dirty
python3 tools/dependencies.py verify
git diff --check
```

The crate's lints (`deny` on `unwrap_used`, `expect_used`, `panic`, `indexing_slicing`,
`string_slice`, `dead_code`, `unsafe_code = "forbid"`) stay as they are.

## 6. Work in bounded slices

For each slice: establish current behavior and callers, introduce the smallest replacement,
migrate every caller, delete the superseded code, update the ledgers, run the targeted checks,
commit on the branch with a message that names the slice. Suggested order: delete history and
dead items (pure removal, easy review) → shared viewer text helpers → `Action` table macro and
config merge → ids and system parameters → relationships and change detection → exclusive-system
reduction → `main.rs` and `daemon/` reshuffle. Do not accumulate abstraction before deleting.

Measure hot paths when a slice touches them: per-step ingest, output feeding, snapshot, and the
compositor's separator pass. A refactor that adds allocations, clones or extra queries per frame
is not accepted on cosmetic grounds.

## 7. Independent review and LOC acceptance

Use fresh reviewer subagents that did not implement the reviewed slice, once mid-way and once on
the complete branch diff against the merge base. They review correctness, whether each deletion is
covered, whether the ECS usage is proper for 0.19.1 (ordering, deferred mutations, change ticks,
relationship cascades), whether invariants and process cleanup survived, and whether the reduction
is real. Fix confirmed P0/P1 and in-scope lower findings, document rejected findings with reasons,
rerun affected checks, and obtain a final review after fixes.

Report before/after counts by file class and by area, the number of removed helper
implementations, remaining exclusive systems with their one-sentence justification, and the
performance comparison. Aim for a net reduction of at least a fifth of hand-maintained Rust
(production plus tests) with no capability lost; if the safe changes achieve less, report the
real number and the limiting evidence rather than deleting capability or assertions.

## Deliverables and completion

- The branch with slice-sized commits, pushed to the fork or origin as authorized above.
- One pull request against `main` whose description contains: the baseline SHA, the deletion and
  assertion ledgers, the before/after LOC table, the list of remaining exclusive systems, the
  performance comparison, the gate output summary, and the review findings with dispositions.
  Open it as a draft; mark it ready only when the gate and the final review are clean.
- Updated README.md, docs/design.md, docs/ecs-acceptance.md, CHANGELOG (0.4.0: internal
  architecture, no protocol change) and a rewritten short HANDOFF.md.

Do not declare completion while any migrated assertion lacks a replacement, any deleted item
lacks a recorded destination, required verification is unresolved, the PR is still a draft with
known findings, or the reported reduction rests on moved rather than removed code. Do not merge,
do not comment on other PRs, and do not push to `main`.
