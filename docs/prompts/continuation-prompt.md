# Continuation prompt: finish the fux + zor ECS-native rewrite

You are resuming a continuous rewrite session. This document is the complete state transfer: read
it, verify the claims it makes against the tree, and continue from the first unfinished item. Do not
re-derive the design; do not restart earlier milestones.

## 1. Objective

Make the design in `docs/prompts/ecs-native-rewrite-prompt.md` real: fux and zor rewritten from
scratch as ECS-native Bevy applications (authoritative state = `bevy_ecs` World in a
`bevy_app::App`, layouts = `bevy_ui` scenes, control surface = the Bevy Remote Protocol served by
`RemoteHttpPlugin` on loopback with capability tokens, persistence = reflection-based world
serialization over an allowlisted projection). Section 7 of that document governs: **one continuous
session**, milestones in order, no stopping at boundaries, no asking for confirmation, milestone =
commit, and after every milestone an ECS-native review pass against `docs/bevy-source-patterns.md`
before the milestone commit. Blockers are only external prerequisites no tool can obtain; record
them in `docs/verification.md` and finish everything that does not depend on them.

Execution-time decisions that override the prompt text (also appended to the prompt itself):
wgpu in the resolved graph is accepted (`bevy_remote` → `bevy_dev_tools` → `bevy_render` is the
only edge); `bevy_remote` and `bevy_dev_tools` are used as published; every Bevy crate is available
(`bevy_platform` collections, `bevy_text`/`bevy_image` for the `UiPlugin` resource initialisation).
Recorded in `docs/dependencies.md` and `docs/verification.md`.

## 2. Where things are

| what | where |
|---|---|
| rewrite worktree (all work happens here) | `/Users/kisaczka/Desktop/code/fux-rewrite`, branch `ecs-rewrite` (orphan) |
| behavioural oracle (never modify) | `/Users/kisaczka/Desktop/code/fux` on `main` — old fux 0.11, zor 0.6, `docs/*.md`, `crates/zor/*.md` contracts, `tools/xtask` |
| Bevy 0.19.1 source of record | `/Users/kisaczka/Desktop/code/many_rigs/inspirations/bevy` (`b56fc29`) |
| config-pattern inventory (read before designing any subsystem) | `docs/bevy-source-patterns.md` (in the worktree; every claim cites Bevy `crate:line`) |
| capability ledger (refresh at milestone 8) | `docs/capability-status.md` (58 rows, snapshot mid-milestone 6) |
| milestone records, blockers, numbers | `docs/verification.md` |
| short state summary | `docs/HANDOFF.md` |

Authorized: commits on `ecs-rewrite`, crate fetches, read-only reference clones. Not authorized:
pushing, tagging, publishing, PRs, modifying `main` or `references/`, touching the user's running
fux/zor sessions, spending resources off this machine. Keep `docs/HANDOFF.md` current after every
commit.

## 3. Verified state (milestones 1–6)

All of the following is committed and was green at its commit; the per-milestone evidence, commands
and measured numbers are in `docs/verification.md`. Commit hashes are in `git log --oneline`.

1. Workspace skeleton: crates `fux`, `zor`, `tools/xtask`; dependency report
   (`cargo run --manifest-path tools/xtask/Cargo.toml -- deps`); CI (fmt, clippy, build, test, doc,
   package, deps).
2. fux App shell: custom runner, lifecycle, per-viewer layout instances, terminal/PTY, BRP,
   attachment stream, viewer, CLI. `crates/fux/tests/layout_mechanism.rs` proves the load-bearing
   mechanism: a root under a `UiTargetCamera` whose `Camera.computed.target_info` is 80×24 lays out
   80×24 cells with no window and no renderer.
3. Scene completeness: picking with border regions and drags, `node.*`/`root.*`/`viewer.*` over
   flex/grid/absolute/overflow, scenes (export/apply/save/restore), surfaces + `Text`, layout matrix
   with old-vs-new rect deltas recorded.
4. Events: reflected `EntityEvent`s, retained `EventLog` with explicit gaps, `fux/events+watch`,
   `world.observe+watch`, receipts (`fux/input.*`), `FinalRecord` (`fux/pane.final`), diagnostics,
   `bell` feature.
5. Assets: `fux.toml` config/theme/keybinding assets with hot reload (previous asset kept on an
   invalid edit), `bsn!` templates replacing the RON builtins, `LayoutAsset` loader, session
   persistence + restore/skip over `fux/session.*` (verified live: three panes restored with
   historical screens).
6. zor: `crates/zor/docs/model.md` (28 numbered invariants with contract citations, 12 structural
   ones enforced by `check_invariants`), journal + dated archives, fux BRP client with a cursor
   events consumer, task/attempt/prompt lifecycle with journal-first intents, checks/sources/
   artifacts/verification seals, groups, worktrees over `git`, provider adapters (Codex app-server,
   OpenCode sidecar, Claude passive) and observation rules with hot-reloaded bundles. `zor run`
   verified end to end against a real fux: exits with the child's status, ephemeral workspace
   retired, journal written.
   Also landed with milestone 6: the bounded BRP HTTP acceptor (`remote/http.rs`, hyper +
   smol-hyper feeding the same `BrpSender`, 1 MiB body / 64-element batch / 256 connections / 10 s
   deadlines) as the **default** transport because `RemoteHttpPlugin`'s accept loop dies
   permanently under a 256-fd limit; `RemoteHttpPlugin` stays selectable; `docs/security.md`.
   fux viewer chrome completed: tab/workspace choosers, rename/new-workspace prompts,
   confirmations, copy mode (selection, search, OSC 52 yank), focus history ring, help panel, and
   `tests/viewer_pty.rs` (SIGWINCH relayout in one update, panic restores the terminal, chooser
   dismisses on click).

At the milestone-6 commit the suites were: `cargo test -p fux` 204 green, `cargo test -p zor` 71
green, `cargo clippy --workspace --all-targets -- -D warnings` clean.

## 4. State of the working tree right now

HEAD is the WIP commit `WIP milestone 7: plugin host, machines, dashboard, surface input events,
scenario harness (does not compile yet)`. Milestone 7 was dispatched as five parallel slices when
the model provider started returning 429s; every slice died mid-edit. Nothing under
`crates/zor/src/{plugins,machines,dashboard}` or `tools/xtask/src/scenarios` has been reviewed,
tested, or committed as working.

Compiles today (`cargo check`):
* `crates/fux` — clean, all targets.
* `tools/xtask` — clean.
* `crates/zor` — **does not compile**; five errors remain:
  1. `crates/zor/src/plugins.rs:23` declares `pub mod hooks;` — `crates/zor/src/plugins/hooks.rs`
     does not exist (the hook-process runner was never written).
  2. `crates/zor/src/app.rs:50-51` wants `crate::machines::{MachinesFile, CATALOG_FILE}`.
  3. `crates/zor/src/app.rs:98-99` wants `crate::dashboard::DashboardPlugin` and
     `crate::machines::MachinesPlugin`.
  Cause: `crates/zor/src/machines.rs` and `crates/zor/src/dashboard.rs` were left as one-line stubs
  while their submodules landed (`machines/{catalog,intents,supervision,transport}.rs`,
  `dashboard/scene.rs`); the module roots (re-exports + the two plugins + `MachinesFile` +
  `CATALOG_FILE`) are missing. `plugins.rs` (1750 lines), `plugins/host.rs`, `plugins/manifest.rs`
  exist but have never been compiled.
  Repaired since the WIP commit: `journal.rs`'s `Presence` filter used a nested `Or<...>` inside
  `Or<Presence>` (invalid; `Presence` is at the 15-entry limit) — the four `Added<...>` markers now
  share one inner `Or<(..)>`.
* Method tables `crates/zor/src/remote/{plugin,machine,dashboard}_methods.rs` are still the empty
  `&[]` stubs chained into `methods::all_specs()`.
* `tools/xtask/src/scenarios/{mod,stack,brp,pty}.rs` (~1400 lines) and the `scenarios` subcommand are
  written but have never run (they build `-p fux -p zor`, so they need zor to compile first). The
  slice that wrote them smoke-tested only the fux half of its helper before dying.
* fux milestone-7 additions landed by the `SurfaceEvents` slice: `events::SurfaceInput` /
  `SurfaceInputKind`, `surface::{pointer_input, key_input, SurfaceInputDrops}`,
  `ViewerRequest::SurfaceKey`, `fux/events+watch { surface }` filter, and
  `Diagnostics.surface_inputs_dropped` on the `fux/server.info` wire. Fixtures were re-blessed and
  one wheel-routing regression (a provider scroll container inside a surface) was fixed in
  `pointer.rs::on_scroll`; the fux suite is green again (see §7 for the current numbers).
* Repaired in the same WIP series (committed with this document): fux's BRP fixtures re-blessed for
  `Diagnostics.surface_inputs_dropped` (`methods.json` edited by hand, `schema.json` re-blessed),
  and `pointer.rs::on_scroll` reordered so a provider's `Overflow::Scroll` container inside a surface
  scrolls itself instead of being delivered to the provider as `SurfaceInput` (its ancestors' scroll
  containers now keep the event bubbling).

## 5. Immediate next actions, in order

1. **Make zor compile.** Write the three module roots and `plugins/hooks.rs`:
   * `crates/zor/src/machines.rs`: `pub mod {catalog,intents,supervision,transport};`, the
     re-exports the submodules already expose, `pub const CATALOG_FILE: &str = "machines.json";`,
     `pub struct MachinesFile { pub path: PathBuf }`, `pub struct MachinesPlugin` (registers the
     catalog asset + supervision systems; mirror `ChecksPlugin`'s shape in `app.rs`).
   * `crates/zor/src/dashboard.rs`: `pub mod scene;` (plus whatever `dashboard/` needs),
     `pub struct DashboardPlugin`, re-exports.
   * `crates/zor/src/plugins/hooks.rs`: the per-plugin event-hook runner (one process per plugin
     consuming `fux/events+watch` + `zor/events+watch` with a cursor persisted under
     `<state_dir>/zor/plugins/<name>/cursors.json`, restart with backoff) — or, if you decide the
     hook runner belongs in `host.rs`, remove the `hooks` module declaration and say so in the
     record. Pick one and make the tree compile.
   Then `cargo check -p zor --all-targets` must be clean.
2. **Finish the three method tables** (`remote/{plugin,machine,dashboard}_methods.rs`) with
   `spec!`/`described!` rows, append their cases to `crates/zor/tests/fixtures/brp/methods.json`,
   and re-bless `schema.json` with `ZOR_BLESS=1 cargo test -p zor --test remote schema_matches_fixture`
   as the last finisher. Asserted surface:
   `zor/plugin.{list,inspect,install,link,enable,disable,run,logs}`,
   `zor/machine.{add,list,inspect,rename,remove,reload,status,resume,cancel,stop,reconcile}`,
   `zor/dashboard.{rows,open,close}`.
3. **Write the milestone-7 tests** each slice owed (none were written):
   `crates/zor/tests/{plugins,machines,dashboard}.rs` plus the fux surface-input cases in
   `crates/fux/tests/{surface,events,viewer}.rs`. Acceptance criteria per slice are in §6.
4. **Run the scenario harness** (`cargo run --manifest-path tools/xtask/Cargo.toml -- scenarios`)
   and fix it until scenarios 1–5 pass and 6 reports `unavailable`; wire it into the CI Linux job.
5. **ECS-native review of milestone 7** against `docs/bevy-source-patterns.md`, apply the findings
   that matter, then commit (milestone boundary).
6. **Milestone 8**: `docs/design.md`, protocol documents generated from `rpc.discover` +
   `registry.schema`, the ownership contract, `CHANGELOG.md`, README/install/update documentation,
   refresh `docs/capability-status.md` against the current tree, and the benchmark comparison
   against a `main` build (same hardware, raw numbers): startup, idle CPU (assert zero wake-ups
   with the `fux/runner/wakeups` diagnostic), rendered input-to-visible latency, sustained output
   through `par_iter_mut` emulation, and many viewers. The per-cell `String` on the frame path
   (`wire::Cell`) is the known milestone-8 measurement debt — measure it and either justify it or
   remove it.

## 6. Milestone-7 scope and its contracts

Sources of truth: prompt §4.3–4.5 and §3.13; `crates/zor/docs/model.md`;
`/Users/kisaczka/Desktop/code/fux/docs/multi-machine-supervision.md` and
`docs/prompts/herdr-parity-prompt.md`; `crates/zor/{DASHBOARD.md,REMOTE.md,SERVICE-API.md}`;
koh's `references/koh/GATEWAY-CONTRACT.md`; Herdr's plugin documentation under
`references/herdr` (adapt ideas, never copy code).

* **Plugin host** (replaces Herdr's plugin daemon): `zor-plugin.toml`
  (`name`, `version`, `actions[]`, `events[]`, `panes[]`, `links[]`, `platforms[]`, `build`,
  `startup`), install/link/enable/disable/list/logs/run; plugins are ordinary processes started with
  `FUX_BRP`, `ZOR_BRP`, `ZOR_PLUGIN_*` and a workspace-scoped fux token from `fux/token.mint`;
  plugin panes are either a terminal pane (`fux/node.spawn` with a `PaneTemplate`) or a surface
  driven with scene deltas; placements (`overlay|split|tab|zoomed|popup`) are `node.*`/`root.*`
  compositions; hooks consume both event streams with persisted cursors. Contract landed already:
  `Effect::RunPlugin { plugin, run, argv, cwd, env, log }`, `Effect::KillPlugin { plugin, run }`,
  `Inbound::PluginExited { plugin, run, code }`.
* **Surface input** (prompt 3.13): `SurfaceInput { surface, node, viewer, kind, col, row, bytes }`
  with `kind ∈ {Press, Release, Scroll{rows}, Key}`, triggered when a viewer's pointer hits an
  instance node inside a surface subtree or when a focused surface leaf receives keys
  (`ViewerRequest::SurfaceKey`); retained in the log, streamed by `fux/events+watch { surface }`,
  bounded at 200/s per surface with a `surface_inputs_dropped` diagnostics counter. A node chain
  that contains a `Overflow::Scroll` container scrolls locally instead (already implemented).
* **Machines**: catalog `$XDG_CONFIG_HOME/zor/machines.json` (v1, private, ≤32 machines / 64
  bindings / 256 KiB, watcher + `zor/machine.reload`), per-machine supervision workers
  (`IoTaskPool`, read budget, poll interval, freshness, stale state preserved, taxonomy
  `Fresh|Stale|Unauthorized|Expired|Ended|Offline`), resume intents committed before dispatch and
  never replayed, guarded remote `cancel/stop/reconcile/resume` (instance nonce + pane identity),
  exact viewer handoff (`zor --machine M attach TASK` → `fux attach --brp <remote descriptor>
  --pane --pid`), and `zor --machine NAME …` routing every call to that machine's zor.
  `Transport::Direct` works over loopback/LAN TCP. **`Transport::Koh` must report `unavailable`**
  with the reason: koh forwards Unix-socket byte streams and fux's attachment stream is TCP, so the
  koh composition gate cannot pass until koh forwards TCP upstream (prompt 3.10). Never re-pin
  `tools/xtask/companions.json`; upstream-then-publish only.
* **Dashboard**: not a second TUI — a `bsn!` scene over the projection rows
  (`TaskView`/`AgentView`/`MachineView`/`CheckView`, `TaskView.needs_input` added for attention
  ordering) streamed into a fux surface; row activations arrive as `SurfaceInput` and become
  `zor/*` calls; `zor dashboard [--machine NAME] [--once]`.
* **Scenario harness** (`tools/xtask scenarios`): two disposable stacks (Local, Remote) over temp
  XDG dirs and random ports, driving the real CLIs: exact-target input reaching exactly one pane;
  independent authorization failure not affecting the other machine; catalog reload; viewer SIGKILL
  leaving the remote pane alive and reattachable; remote-owner survival across a Local restart;
  koh composition reported `unavailable`. Print PASS/FAIL/UNAVAILABLE with timings; non-zero exit
  on FAIL.

## 7. Current evidence snapshot (verify before you trust it)

* `cargo check -p fux --all-targets`: clean. `cargo check --manifest-path tools/xtask/Cargo.toml`:
  clean. `cargo check -p zor --all-targets`: 5 errors (§4).
* `cargo test -p fux`: **208 passed, 0 failed** (measured after the fixture re-bless and the
  `on_scroll` fix); `cargo clippy -p fux --all-targets`: 0 findings.
* `cargo test -p zor`: last green at the milestone-6 commit (71 tests) and cannot run now.
* `docs/verification.md` holds measured numbers for: BRP resource exhaustion under both fd limits,
  the two-viewer/exact-attachment smokes, session restore, `zor run`, and the layout rect deltas.

## 8. Rules that keep this session coherent

* One owner per file per batch. Shared contracts live in `crates/fux/src/model/**`,
  `crates/fux/src/wire.rs`, `crates/zor/src/model/**`, `crates/zor/src/remote/methods.rs` and
  `journal.rs`; when several slices run concurrently, decide those contracts in the batch brief and
  have slices coordinate through the peer-messaging tool rather than editing the same file.
* Lints: no `unwrap`/`expect`/`panic`/indexing/`todo` in non-test code (`clippy.toml` relaxes
  `#[test]` bodies); library code returns `Result<_, BevyError>`; `too_many_arguments` and
  `type_complexity` are allowed workspace-wide with reasons.
* Every new method gets a `MethodSpec`/`described!` row, a fixture case, and a schema re-bless by
  the last finisher: fux `FUX_BLESS=1 cargo test -p fux --test brp`, zor
  `ZOR_BLESS=1 cargo test -p zor --test remote schema_matches_fixture`.
* Invariants are the deliverable, not commentary: `check_invariants` runs after every update in
  every test that mutates the World, and every invariant the old contracts state must have a
  covering test before its feature is called done.
* Tests defend behaviour, never wording or implementation; delete a test that pins either.
* Run the milestone's own tests before its commit; run the full suite, formatter and lints at
  milestones 2, 5, 6 and 8 (already done for those), and at every review pass.

## 9. Open blockers

* None that stop work. Two recorded limitations: koh cannot forward the attachment stream until it
  gains TCP forwarding upstream (milestone 7 reports `unavailable`), and audible bell playback is
  unverified on this machine (headless `AudioPlugin` only warns).
* `contacts`/network: `bevy_remote` 0.19.1 requires a crates.io fetch on a fresh machine.
