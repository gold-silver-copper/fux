# ECS rewrite acceptance audit

Audit date: 2026-09-05. Scope: every requirement in
[bevy-ecs-multiplexer-prompt.md](../bevy-ecs-multiplexer-prompt.md), mapped to current source and
observed evidence. The plan written before implementation is [ecs-plan.md](ecs-plan.md); the
architecture as built is [design.md](design.md). All evidence below was produced locally on macOS
(Darwin 25.5.0, Apple Silicon) with disposable HOME/XDG directories and owned processes only.
Nothing was committed, pushed, released or commented on GitHub; no personal session, key or user
workspace was touched; no live old server was killed.

Status: complete. An independent review was performed after implementation and a second
independent pass verified the fixes; findings and resolutions are in the Review section.

## Product

| Requirement | Source | Evidence |
|---|---|---|
| workspace → tabs → recursive-split panes; no nesting of workspaces/tabs | `ecs/components.rs` (`Workspace.tabs`, `Tab.layout: LayoutTree<Entity>`), `layout.rs` | `ecs::check_invariants` rejects a tab in two workspaces or a pane in two layouts; `layout.rs` tests (6) and randomized ECS test |
| create/switch workspaces | `systems/requests.rs` `workspace_action`, `reserve_workspace`; viewer `s`/`S` | ecs `workspace_switch_sends_the_suffix_to_the_destination`; fixture `concurrent_first_clients_elect_exactly_one_server_and_workspace`; `viewer.py` |
| create/switch/name/close tabs | `tab_action`, `close_tab` (`ecs/support.rs`); viewer `t n p w , X` | ecs `natural_exit_of_one_pane_closes_it_and_of_a_tab_moves_viewers`, randomized test (found and fixed the orphaned-pane invariant, see Review); `viewer.py`; fixture `control_protocol_lists_captures_and_streams_events_without_touching_viewers` |
| splits, directional focus, repeated resize, confirmed close | `layout.rs` (`split/close/resize/neighbour`), `requests.rs`; controller modes `Resize`, `ClosePane`, `CloseTab` | ecs `split_focus_and_following_input_reach_the_new_pane_only_after_creation`; `viewer.py` split/resize/close scenarios; `local_tty.py` |
| persistent PTYs, background output, detach and reconnect | `os/pty.rs`, `server/adapter.rs`, `server/connections.rs` | fixture `detach_and_reattach_preserve_the_pane_process_and_its_history`; `detach_drain.py`; `local_attachment.py` |
| bounded per-pane history, keyboard browsing, mouse selection, clipboard | `terminal.rs` (`with_history_screen`, scrollback limit), `client/copy.rs`, `client/screen.rs` (OSC 52, 1 MiB cap) | ecs `history_views_are_private_and_clamped`; `viewer.py` copy/selection/shift-drag scenarios; `screen.rs` tests; `terminal.rs` tests |
| one configurable prefix and a contextual popup | `commands.rs` (`DEFAULT_BINDINGS`, `Action::unavailable`), `client/input.rs`, `client/hints.rs` | `viewer.py` (immediate popup with workspace name, unknown key keeps it, Esc, literal prefix, dim entries); `hints.rs` paging test |
| multiple viewers with independent navigation/history/selection | `Viewer.selection`, per-viewer frames in `systems/snapshot.rs`, viewer-private `view` reads | ecs `viewers_keep_private_tabs_and_focus_while_sharing_layout_edits`; `viewer.py` two-viewer scenario; fixture `tiny_viewer_and_resize_keep_the_pane_size_negotiated_over_the_smallest_viewer` |
| interoperability with independently built koh and zor | attachment v4 (v3 until 0.3.1), control `FUXCTL2` | koh: `cargo test --manifest-path references/koh/Cargo.toml --test gateway` 2 passed and `--lib gateway::` 10 passed with `FUX_BIN`/`KOH_REQUIRE_FUX_BIN=1`; zor: `tests/zor_integration.rs` 1 passed with `ZOR_BIN`/`FUX_REQUIRE_ZOR_BIN=1`; `python3 tools/dependencies.py verify --build` passed |
| fresh launch: one default workspace/tab/pane, no configuration, name or program | `main.rs`, `daemon/startup.rs`, `ManagerAction::Resolve{None}` | ecs `fresh_workspace_has_one_tab_and_pane_below_the_bar`; `local_tty.py`; fixture election test |
| deterministic no-name attach rule, no startup picker | `requests.rs` manager resolve: most recently attached workspace | ecs harness `create_workspace` + `Resolve{None}` path exercised in randomized test; documented in README |
| automatic labels, names optional | `next_workspace_name` (`ws-N`), tab `tab-N`, first tab `main` | ecs fresh-workspace test; randomized test uses unnamed creation |
| compact bar, thin separators, clear focus, no dashboard | one-row bar always (`support::tab_area`), shared one-cell separators (`layout.rs` gap, `client/render.rs`) | ecs `fresh_workspace_has_one_tab_and_pane_below_the_bar` (`rect.y == 1`); `viewer.py` bar, separator, junction, three-tab truncation, notice and 2×20 checks (its screen model has no attributes, so the bold/dim rule is asserted only by the render unit test) |
| popup near the bottom with actual bindings and availability | `client/hints.rs`, `Action::unavailable` | `viewer.py`; `commands.rs` tests |

Removed machinery (verified absent by `grep` over `src/` and by `tests/structure.rs`):
floating popup panes, full-screen pickers and the startup picker, external command bindings and
the `binding` message, lifecycle hooks, desktop notifications, agent dashboards and the OSC 7877
adapter, zor sidecar supervision (`zor-path`), embedded networking/detection, status segments,
hint delay preferences, SIGHUP config reload, `tokio-util`, `loom`. Starting a program in an
ordinary pane remains (`new`/`split` with `argv`, `cwd`).

## ECS architecture

| Requirement | Evidence |
|---|---|
| standalone `bevy_ecs`, pinned, minimal features, APIs checked for that release | `Cargo.toml`: `bevy_ecs = "=0.19.1", default-features = false, features = ["std"]`; plan links the 0.19.1 docs; `tests/structure.rs` forbids `bevy`, `bevy_app`, `bevy_render`, `bevy_reflect` |
| authoritative model in components/resources/systems, no legacy host in a Resource | `ecs/components.rs` holds all mutable session facts; resources are `Limits`, `Ids`, `Clock`, `Deadlines`, `Registry`, `ShuttingDown`, `WorkspaceCounter`, step messages; `structure.rs::ecs_is_the_only_authoritative_model` forbids World mutation outside `ecs/` and any non-ECS session store |
| entities for workspaces/tabs/panes/viewers; no per-cell entities | `components.rs`; `Session::entity_counts` used by tests |
| model written before implementation | `docs/ecs-plan.md` (dated 2026-09-05, before `src/ecs`) |
| server ECS authoritative, viewer state private, no World replication | viewers receive `view::Frame` per viewer (`systems/snapshot.rs`); client state in `client/controller.rs` |
| public ids distinct from `Entity`, instance boundary | `ids.rs` newtypes; `Ids` maps; descriptors carry instance nonce; `structure.rs` forbids `Entity` in `proto/` and `view.rs` |
| validate kind/ownership/liveness; confirmations carry target ids | `requests.rs` (`pane_in_workspace`, `tab_in_workspace`), controller `ClosePane{pane}`/`CloseTab{tab}` | ecs `stale_targets_fail_without_hitting_replacements` |
| explicit domain validation (acyclic layouts, unique membership, live focus, nonempty containers, dimensions, budgets) | `LayoutTree::validate`, `Session::check_invariants`, `Limits` | every harness step calls `check_invariants`; randomized test (256 cases locally, 2048 in `nightly.yml` and an explicit local run) |
| closing a viewer never cascades to panes; explicit cascades; tested lifecycle | `requests::despawn_viewer` touches only the viewer; `support::close_tab`, `lifecycle::finalize` are explicit | ecs lifecycle tests; fixture `forced_close_terminates_descendants_and_reports_the_status`, `server_shutdown_signal_reaps_owned_processes_and_sockets` |

## Ordered, event-driven execution

| Requirement | Evidence |
|---|---|
| event-driven owner loop, idle sleeps, no fixed tick | `server/mod.rs::run_loop` waits on channels/sockets/signals/next deadline; measured idle CPU 0.0 s per 10 s |
| repaint only on change, bounded coalescing | `Viewer.dirty`, `ViewerOutbox` coalesces frames (latest wins) up to depth 64 |
| single logical writer, explicit ordering, no observer web | `SingleThreadedExecutor`, chained `Phase` sets in `ecs/mod.rs`; no observers or hooks |
| bounded fair ingest | `PANE_BUDGET 512` chunks from the 2048-deep pane channel and `INGRESS_BUDGET 256` requests per step (`server/mod.rs`), re-arm while channels still hold items, signals polled between busy steps |
| per-source byte order; output/EOF/exit cannot lose final output | each pane's reader sends output, EOF, exit in order on the bounded pane channel; `systems/output.rs` applies exit after chunks and keeps an exit that precedes the completion | ecs `output_eof_and_exit_keep_final_output_and_retire_the_workspace`; fixture `natural_last_pane_exit_is_observable_before_workspace_retirement` |
| commands in one read observe predecessors; creation barrier; rollback without phantom | `Viewer.barrier`, `Creation`, `apply_spawn_completions` drains queues after release | ecs `split_focus_and_following_input_reach_the_new_pane_only_after_creation`, `failed_creation_rolls_back_and_releases_the_barrier`; fixture `startup_failure_rolls_back_and_reports_an_error` |
| acknowledgements cannot overtake state | frames queued before replies in `snapshot.rs`; outbox preserves reply order | ecs split test asserts frame-before-reply |
| detach drains, workspace switch keeps suffix | `requests.rs` | ecs `detach_applies_preceding_input_and_drops_the_suffix`, `workspace_switch_sends_the_suffix_to_the_destination`; `detach_drain.py` |
| stale callbacks/deadlines/late reports cannot resurrect or retarget | ids never reused; unknown ids ignored | ecs `stale_targets_fail_without_hitting_replacements`, `control_requests_and_mouse_hit_tests_respect_stale_generations`; randomized test injects stale pane/viewer ids and late completions |
| hot panes/slow viewers cannot starve input, timers, exits | budgets above; slow viewers disconnected at depth 64; subscriber queues capped at 1024 | ecs `limits_and_queue_overflow_are_enforced`; fixture stdin/backpressure test |
| bevy message storage not a transport queue | `Session::step` writes inbound, systems read, `Messages` cleared and `clear_trackers` each step | harness asserts `retained_messages() == 0` after every step |

## Operating-system I/O and lifecycle

| Requirement | Evidence |
|---|---|
| blocking I/O outside systems; adapters own handles; typed messages | `os/pty.rs` threads, `server/adapter.rs` maps keyed by `PaneId`/`ViewerId`; `structure.rs::channels_stay_bounded_and_ecs_systems_never_block_on_the_operating_system` |
| proven emulator/PTY libraries | `vt100 0.16.2`, `portable-pty 0.9.0` |
| server owns PTYs and history; viewer loss never terminates | fixture detach/reattach; `detach_drain.py` |
| creation rollback, natural exit, confirmed close, final-tab retirement, shutdown | `systems/lifecycle.rs` | ecs lifecycle tests (5); fixture natural-exit, forced-close, shutdown tests |
| EOF distinguished from exit; final output observed; process groups terminated; children reaped | `PaneState::Eof`, `ProcessGroup::terminate` with `ReapGate` | fixture `forced_close_terminates_descendants_and_reports_the_status` (a descendant ignoring SIGHUP is killed) |
| partial startup failure | `Creation` rollback; `ServerChild` readiness channel | fixture `startup_failure_rolls_back_and_reports_an_error` |
| private sockets, ownership/permission checks, peer auth, bounded framing, deadlines, safe cleanup, version rejection before raw mode | `proto/socket.rs`, `daemon/startup.rs`, `proto/attach.rs`, `proto/control.rs` | `protocol_rejection.py`; `local_attachment.py`; `daemon` unit tests |
| never terminate personal sessions to upgrade | no automatic kill; the mismatch is reported, and the interactive dialog in `main.rs` stops an old server only after the operator types `stop` (SIGTERM to recorded pids, no SIGKILL) | `protocol_rejection.py`; `migration.py` (non-interactive leaves it, `q` and a refused confirmation leave it, confirmed stop replaces it) |

## Viewer interaction and rendering

| Requirement | Evidence |
|---|---|
| byte-exact input; one registry for bindings/labels/availability/popup/CLI | `commands.rs` used by `client/input.rs`, `client/hints.rs`, `fux bindings` | `viewer.py`; `commands.rs` tests |
| Ctrl-A default; immediate hints; bursts before repaint; no delayed commands | `PrefixFilter`, controller `pending` burst handling | `viewer.py` (prefix-`|` burst without flash) |
| prefix twice sends one prefix; unknown keys reveal commands | `input.rs` | `input.rs` tests (5); `viewer.py` |
| Esc backs out; simple actions return to input; no timeout; resize hint; changes kept | controller modes | `controller.rs` tests (6); `viewer.py` |
| choosers/naming/confirmations share popup primitives; next/previous tab; paging | `hints.rs` (`context`, `text_input`, `bar`) | `hints.rs` tests incl. tiny-screen paging |
| application cursor/keypad modes, fragmented sequences, Unicode, cancelled mode owns paste | `client/io.rs`, `input.rs` | `input.rs`/`io.rs` tests; `viewer.py` fragmented paste |
| viewer-specific menu/selection/history/tab/focus; deterministic size negotiation | `systems/layout.rs` smallest-viewer rule; hidden tabs keep area; viewer-less 80×24 | ecs viewer isolation test; fixture tiny-viewer test |
| resize and stale generations before hit testing; tiny/zero areas bounded | `ViewerRequest::Mouse{generation}`; `render.rs` clamps | ecs stale-generation test; `render.rs` tests |
| restore terminal modes; final frame painted and observed | `client/screen.rs`; fixture final-frame observation | fixture natural-exit test reads the final screen before restoration |

## Pane history, mouse and clipboard

| Requirement | Evidence |
|---|---|
| bounded history while hidden/detached; not discarded by switching | server-side `ServerTerminal` per pane | fixture detach/reattach history check; ecs history test |
| documented limits/eviction/closure | README "Panes and history"; `history.scrollback-lines` 1–100 000 | `config.rs` tests |
| viewer-owned position/selection; no forced live-follow; cleared with feedback on invalidation | `client/copy.rs` (`refresh_live`, `install`) | `copy.rs` tests (3); `viewer.py` |
| keyboard history/selection and pane-local mouse selection with hint and return to live | `copy.rs` hint strings; `g`/`q` | `viewer.py` |
| wheel/drag local unless app owns mouse; correct coordinates; Shift override | `controller.rs::mouse` (`app_owns_mouse`, `shift`) | `controller.rs` tests (wheel over owned/unowned pane) |
| copy selected text only; wide/combining chars; policy; size limit; once-only; retry feedback | `PaneView::text_between`, `screen.rs` OSC 52 cap | `view.rs`/`screen.rs` tests |
| browsing never changes another viewer | `view` reads are connection-private | ecs `history_views_are_private_and_clamped` |

## Koh and zor

| Requirement | Evidence |
|---|---|
| builds/installs/runs without koh or zor; no cross-owner crate/build script/sibling source | `Cargo.toml`; `structure.rs::project_dependencies_and_application_imports_respect_ownership`, `default_ci_and_release_verification_require_only_fux`; `tests/verify/release-package.sh` |
| versioned process protocols, no Entity/component/schedule exposure | `proto/attach.rs` v4, `proto/control.rs` `FUXCTL2`; `structure.rs` forbids `Entity` in `proto/` |
| control surface: stable ids, listing, bounded capture, lifecycle events; reads do not change focus | `Request::{List, Capture, Subscribe}`; control clients act on workspace selection | fixture control test; `observer.py` asserts focus/pid unchanged |
| real koh gateway carries attachment stream; stopping leaves panes usable | koh `optional_gateway_failure_leaves_real_fux_panes_running`, `real_fux_keeps_its_pane_and_applies_input_once_across_five_quic_losses` passed against `target/debug/fux` |
| zor observes via capture/events; failures isolated | `tests/verify/observer.py`: real `zor observe` reports working→idle from fux capture/title/progress; wrong preface and unknown command rejected; observer SIGKILL leaves pane pid/focus/output unchanged |
| explicit user-started programs, no supervisor | README composition commands; no spawn of koh/zor in `src/` (`structure.rs` spawn-owner list) |
| owner edits minimal and reproducible | `dependency-patches/koh.patch` (26 lines: `"version":4`, `Some(4)`), `dependency-patches/zor.patch` (32 lines: `FUXCTL2`); `tools/dependencies.py export` and `verify --build` passed; owner builds stay independent (`zor` tests 39+2+6+9 passed, `--no-default-features` check passed) |

Composition commands: see README "Working with koh and zor".

## Workflow, configuration and documentation

- Plan before implementation: `docs/ecs-plan.md`. Deviations from the plan: the `Subscribers`
  resource lives in the adapter (`server/adapter.rs`) because subscriptions are connection state,
  not session state; no `workspace.resized` event is emitted (geometry is in `list`); the control
  protocol gained `workspace select` for viewers so a switch keeps one connection.
- Vertical path first, then features; no legacy host, feature flag or compatibility shell remains
  (`git status` shows the old `src/` and `tests/` trees deleted; `structure.rs` checks ECS purity).
- License attribution retained: `LICENSES/`.
- Tests ported with intent preserved: detach drain, protocol rejection, local attachment, TTY,
  election, natural exit, forced close, shutdown, startup failure, tiny viewer, control listing,
  contextual viewer interactions (`viewer.py`), real zor observer, real koh gateway. Vacuous or
  implementation-specific old suites (chaos over the old router, loom models, corpus cassettes) were
  replaced by the deterministic ECS suite and the randomized sequence test.
- CLI help (`fux --help`, `fux bindings`), configuration (README), architecture (`design.md`),
  protocols (`local-attachment-protocol.md`, `local-control-protocol.md`), security, release
  readiness, changelog and handoff updated; earlier documents labelled historical.
- Configuration surface: `prefix`, `bindings`, `default-command`, `clipboard`, `history.scrollback-
  lines`, `limits.{max-panes,max-tabs,max-workspaces}`.

## Dependencies and performance

Dependency cost is measured, not assumed:

| Measure | 0.2.1 (HEAD) | 0.3.0 |
|---|---|---|
| `Cargo.lock` packages (all targets) | 155 | 207 |
| normal-dependency crates (`cargo tree -e normal`) | n/a | 145, of which the `bevy_ecs` subtree is 58 |
| release binary (macOS arm64) | 6.03 MB | 6.35 MB |
| MSRV | 1.91 | 1.95 |

The ECS graph adds roughly 50 crates; the removed features took `tokio-util` and `loom` with them.

Runtime, release builds, `python3 tools/measure.py BIN --version 2|3` (same machine, same
script, one viewer, 20 000-line burst, 40 timed round trips). The 0.3.0 column gives the range over
five quiet-machine runs of the final binary (median in parentheses); the baseline was re-measured
under the same conditions (two runs):

| Measure | 0.2.1 baseline | 0.3.0 final | Budget (plan) |
|---|---|---|---|
| startup to attach socket | 15–16 ms | 23–57 ms (40) | ≤ 50 ms |
| idle CPU per 10 s with one viewer | 0.04 s | 0.00 s | ≤ 0.01 s |
| RSS at start / after burst | 6.0 / 20.7–20.9 MiB | 6.1 / 35.6–36.4 MiB | ≤ 40 MiB after burst |
| 20 000-line burst to quiescence | 0.31–0.33 s | 0.13–0.17 s | ≤ 1.0 s |
| input→frame latency median / p95 | 13.8–15.9 / 16.9 ms | 3.1–6.6 / 3.5–11.0 ms | median ≤ 10 ms |

Idle CPU, RSS, burst and latency budgets hold in every run. Startup holds at the median but two of
five runs exceeded 50 ms (53 and 57 ms); the measurement spans a fork/exec of the server plus the
pane shell and varies by more than 2× between runs on this machine, so the number is reported as a
range rather than tuned. Tradeoffs: startup is slower than 0.2.1 (bevy_ecs World and schedule
construction and a larger binary) and post-burst RSS is higher (each pane's vt100 history of
10 000 rows is retained in full rather than trimmed on the old host's diff path), while idle CPU,
burst throughput and latency improved because the owner loop runs one ordered step per wake-up
instead of the old per-client repaint pipeline.

## Exact verification commands and results

All run on 2026-09-05 from the repository root with `cargo 1.97.1` (stable) unless noted.

| Command | Result |
|---|---|
| `cargo fmt --all --check` | clean |
| `cargo clippy --all-targets --locked -- -D warnings` | clean (lints deny `unwrap`, `expect`, `panic`, `indexing_slicing`, `dead_code`) |
| `ZOR_BIN=$PWD/zor/target/debug/zor FUX_REQUIRE_ZOR_BIN=1 cargo test --locked -- --test-threads=1` (final run, after the review fixes and the incompatible-server dialog) | lib 67, main 3, `ecs` 19 (incl. randomized and four review regressions), `local_cli` 6 (incl. `migration.py`), `structure` 8, `zor_integration` 1 (real zor), doc-tests 0; all passed |
| `PROPTEST_CASES=2048 cargo test --locked --test ecs` and `PROPTEST_CASES=8192 …` (final gate) | 19 passed each; the randomized test found three defects during development, all fixed (see Review) |
| `ZOR_BIN=$PWD/zor/target/debug/zor FUX_REQUIRE_ZOR_BIN=1 cargo test --locked --test zor_integration` | 1 passed |
| `cargo doc --no-deps --locked` | generated, no warnings |
| `cargo +1.95.0 check --all-targets --locked`, `cargo +1.95.0 build --locked --bin fux` | passed (MSRV compilation) |
| `cargo fmt --all --check`, `cargo clippy --all-targets --locked -- -D warnings`, `cargo test --locked` in `tests/verify/fixture-child` | see Review section for the final run |
| `tests/verify/release-package.sh --allow-dirty` | see Review section for the final run |
| `FUX_BIN=$PWD/target/debug/fux KOH_REQUIRE_FUX_BIN=1 cargo test --manifest-path references/koh/Cargo.toml --test gateway --locked` | 2 passed |
| same with `--lib gateway:: --locked` | 10 passed (219 filtered) |
| `cargo test --manifest-path zor/Cargo.toml --all-features --locked` | 39 + 0 + 2 + 6 + 9 passed |
| `cargo check --manifest-path zor/Cargo.toml --no-default-features --locked` | passed |
| `python3 tools/dependencies.py export && python3 tools/dependencies.py verify` | patches exported and verified |
| `python3 tools/dependencies.py verify --build` | reconstructed koh/zor/fux; `ecs` 14 (before the randomized test was added), `local_cli` 5, `zor_integration` 1, koh lib 10, koh gateway 2 passed |
| `python3 tools/measure.py target/release/fux --version 3` (five runs) and `… fux-baseline --version 2` (two runs) | table above |
| `git diff --check` | clean |

## Independent review

An independent reviewer (a separate agent that did not implement the rewrite) reviewed the
complete new `src/` and `tests/` trees, the owner edits and the protocol documents against the
specification. Findings and resolutions:

The first review confirmed no P0, four P1 and ten P2 findings. Every P1 and every P2 with a
code fix was resolved; the two remaining P2 items were documentation corrections. The second pass
(below) verified the fixes and found one further P1, also fixed and re-verified.

| # | Severity | Finding (reviewer, confirmed) | Resolution |
|---|---|---|---|
| 1 | P1 | An exit report arriving with or before the spawn completion was dropped because the pane was still `Starting`; the pane stayed `Live` with a dead pid and a last-pane exit never retired the workspace. | `systems/output.rs` records the exit on a `Starting` pane; the lifecycle pass leaves exited reservations to the completion phase, which places the already-exited pane so the same step closes it. Tests `exit_arriving_with_or_before_the_spawn_completion_is_not_lost`, `a_workspace_whose_first_pane_exits_at_once_retires_with_its_status`. |
| 2 | P1 | Releasing a `Starting` reservation (workspace kill, finalize) emitted `ReleasePane` before the process existed and the later completion was ignored, leaking the shell until server exit. | `apply_spawn_completions` emits `Terminate` + `ReleasePane` for a completion whose reservation is gone; `finalize` fails pending creations' requesters (`fail_pending_creations`); the workspace completion path abandons a workspace killed before it opened. Test `killing_a_workspace_with_a_pending_spawn_stops_the_late_process`. |
| 3 | P1 | `view` history reads resolved pane ids globally, so a gateway-scoped viewer of one workspace could read another workspace's screen. | `history_view` is scoped to the viewer's workspace; `workspace kill` over a workspace connection is limited to that workspace (`Unauthorized` otherwise); `workspace select` enforces the viewer limit. Test `viewer_requests_never_reach_other_workspaces`; protocol docs updated. |
| 4 | P1 | The viewer's Escape disambiguation deadline was recomputed on every loop turn, so a lone Esc never resolved while any pane streamed frames faster than 35 ms. | `client::EscapeTimer` fixes the deadline when the pending Escape begins; unit test `escape_deadline_is_fixed_when_the_escape_arrives`. |
| 5 | P2 | Viewer outboxes leaked on hard disconnect (`ViewerGone` path emitted no `CloseViewer`). | `despawn_viewer` emits `CloseViewer`; the duplicate explicit effects were removed. |
| 6 | P2 | Finalize/kill/shutdown released running panes with an immediate SIGKILL instead of the documented SIGHUP grace. | `PaneProcess::join` waits briefly for the reap, otherwise terminates with the one-second grace before joining. |
| 7 | P2 | Reap-gate TOCTOU: a reader already blocked in `wait` after EOF could reap the leader on SIGHUP, so the delayed SIGKILL might hit a recycled group id. | The gate is counted and the reader reaps by polling `try_wait` under the gate (`reap_if_released`), so a termination in progress always holds the leader. |
| 8 | P2 | Signals were only polled in the idle `select!`, so a hot pane could delay SIGINT/SIGTERM indefinitely; overdue terminations proposed no wake-up. | `poll_signal` (non-blocking `Signal::poll_recv`) between busy steps; `drop_overdue_terminations` proposes its deadline. |
| 9 | P2 | Killing a workspace before its first pane started opened and closed it in one step and answered the requester with an `attach` for a vanishing socket. | Completion abandons a retiring workspace; pending requesters receive `failed`. |
| 10 | P2 | `workspace select` bypassed the per-workspace viewer limit. | Limit checked before switching (`limit` error). |
| 11 | P2 | Undocumented 30 s control idle timeout; an unanswered request was reported with id 0. | Documented in the control protocol; the reply carries the request id. |
| 12 | P2 | Retiring viewers received `exited` twice. | `Viewer.exit_sent` suppresses the second message. |
| 13 | P2 | Docs/comments described a per-pane ingest budget and a `workspace.resized` event that do not exist; an adapter comment claimed output cannot precede the completion. | `design.md`, `security.md`, the adapter comment and this audit corrected; the plan is left as the historical pre-implementation record with the deviations listed above. |
| 14 | P2 | Test gaps for items 1–4. | The four tests named above plus the Escape timer unit test. |

The reviewer judged these areas sound: peer authentication, private directories, inode-checked
socket cleanup, descriptor validation, handshake deadlines and version rejection before raw mode,
bounded framing on both protocols, creation barrier/rollback and frame-before-reply ordering,
detach drain and workspace-switch suffix, stale id/generation handling, per-step message
clearing, the idle loop, layout invariants, terminal control-string bounding, the koh and zor
consumer schemas and the one-line owner-repository edits.

A second independent pass (a fresh agent that implemented none of the fixes) verified every
resolution above with code tracing, real-process probes of the PTY reap gate and an isolated
real-binary session: all fourteen items FIXED (items 5 and 7 with harmless residuals noted below),
and the late control-request guard judged safe for the CLI and for pending attachments. It found
one new P1 in the changed viewer code:

| # | Severity | Finding (second pass, confirmed by real-binary probe) | Resolution |
|---|---|---|---|
| 15 | P1 | After the fixed Escape deadline fired, the resolved Escape byte was pushed back into the pending input and re-fed through the prefix filter, which buffered it again: a lone Esc never reached the pane and the viewer spun at full CPU until the next key (before item 4 this was a 35 ms periodic wake with the same lost Esc). | Resolved events are now dispatched directly (`client/mod.rs` `resolved` queue) instead of being re-fed. `tests/verify/viewer.py` gained a real-viewer check that a lone Esc reaches a `cat -v` pane and that Esc-`q` arrives as one sequence. |

A third independent pass verified item 15: the resolved Escape is dispatched once and never
re-fed, resolved events are gated by the outstanding-request and detach rules, the copy-mode
post-processing still runs, non-byte events from an Escape prefix flow through the same dispatch,
and Clippy and the client unit tests are clean. It found no new P0/P1; the real-viewer check in
`viewer.py` covers the end-to-end path.

Residuals accepted as P3: the queue-overflow path closes a viewer's outbox twice (the second close
is a no-op); the reaped flag is stored just after the gated reap, leaving a microsecond window in
which a termination could signal an already reaped leader; `list` over a workspace connection still
returns other workspaces' pane metadata (argv, cwd, titles) while screen bytes are scoped; a
workspace whose first pane exits immediately answers its requester with an attach descriptor for a
socket that closes in the same step, so the CLI reports a connection error rather than the status.

After the fixes, the 2048-case randomized run found one more defect: a control-socket request
naming a workspace whose initial pane had not completed yet could attach a tab to the pending
workspace, and a later failed workspace spawn rolled the workspace back and orphaned that tab.
Control requests now resolve only open, non-retiring workspaces (`requests.rs`). A further 2048-case
run then found that killing the only pane of a tab while a split into that tab was still starting
closed the tab and left the reservation without one; `close_tab` now fails such pending creations
(`fail_pending_creations_in_tab`) and releases them, the late completion being stopped by the
completion phase. All shrunk cases are recorded in `tests/ecs.proptest-regressions`; the suite
passes with 2048 and 8192 cases (see the final gate below).

Before the review, the randomized ECS test found one defect on its first run: closing a
workspace's only tab despawned the tab while its still-running pane kept a reference to it, which
the invariant checker reported as an orphan. The design is that a terminating pane may outlive its
tab until its exit report arrives (or adapter shutdown reaps it); `check_invariants` now allows
exactly that state and rejects any other orphan. The shrunk case is recorded in
`tests/ecs.proptest-regressions`.

## Top bar (0.3.1, 2026-09-06)

[top-bar-design-prompt.md](../top-bar-design-prompt.md) replaced the per-pane boxes with an
always-visible one-row bar and shared one-cell separators. Requirement mapping:

| Requirement | Source | Evidence |
|---|---|---|
| bar always on row 0: workspace, tabs (current reversed), focused `id: title` / `(exit N)` | `client/render.rs::paint_bar`, `support::tab_area` | render tests `bar_shows_workspace_tab_and_focused_pane_without_any_frame`; `viewer.py` bar assertions; fixture natural-exit test checks `(exit 29)` in the final frame |
| truncation: right zone yields first, current tab keeps priority, `…` | `paint_bar`, `fit_tabs`, `truncate_head/tail` | render test `bar_truncates_from_the_right_zone_first_and_keeps_the_current_tab`; `viewer.py` 70-character label |
| no frame; one-cell shared separators with junctions; bold next to focus | `layout.rs::geometry_node` gap; `render.rs::paint_separators` | render test `separators_join_between_panes_and_brighten_next_to_focus`; `viewer.py` one-column and `├─` checks; layout proptest (leaves disjoint, at most one line per split) |
| configurable muted colours (`[style]`) | `config.rs::{Style, StyleColor}`, `render.rs::Palette` | config parsing tests; README example |
| notices in the bar for two seconds or until a key; no bottom notice bar | `controller.rs::{notice, notice_deadline, expire_notice, NOTICE_TTL}`, `client/mod.rs` wake-up | render test `notices_replace_the_pane_title_in_the_bar`; `viewer.py` copy notice appears then expires |
| geometry: bar row reserved, one-cell sibling gap, content = leaf rect, smallest-viewer negotiation kept | `support::tab_area`, `Pane::terminal_size`, `layout.rs` | ecs `fresh_workspace_has_one_tab_and_pane_below_the_bar` (23×80 under a 24×80 viewer), split widths `[39, 40]`; fixture sizes 39×120 / 11×40 / 29×100 |
| tiny screens safe; separators and bar are non-pane cells for the mouse | `render.rs` bounds, `Frame::pane_at` | render `tiny_and_zero_terminals_never_panic` (incl. 2×20); `viewer.py` 1×1 and 2×20; ecs mouse test (`\x1b[<0;5;3M` from a click at column 5, row 4) |
| guarantees kept: byte-exact input, per-viewer frames, barrier, mouse translation, Shift override, selection bounds, final frame | unchanged paths | full suites below; attachment protocol unchanged (no version bump) |

### Independent review of the top bar

A reviewer who did not implement the change reviewed the diff, ran the suites and attached a
0.3.1 viewer to a 0.3.0 server under a disposable runtime directory. Findings and resolutions:

| # | Severity | Finding (confirmed unless noted) | Resolution |
|---|---|---|---|
| 1 | P1 | The frame's `layout` rectangles changed meaning (content rectangle below a bar row instead of an outer box) without a version bump, so a 0.3.1 viewer on a persistent 0.3.0 server painted the pane over the bar and translated mouse clicks off by one. | Attachment protocol bumped to v4 (`proto/attach.rs`), koh's real-fux tests moved to version 4 through `dependency-patches/koh.patch`, harnesses updated; a hello mismatch is now an `Unsupported` error so an interactive `fux` offers the same stop/alongside/quit dialog it offers for an older control preface. |
| 2 | P2 | Unfocused exited panes had no indication at all. | A dim reversed ` exit N ` marker in the pane's last row (`paint_exit_marker`); the focused pane's status stays in the bar. |
| 3 | P2 | `viewer.py` ran the truncation case with two tabs and cannot observe bold/dim. | The case now creates a third tab first; the bold/dim rule is covered by the render unit test only, which the table above states. |
| 4 | P2 (plausible) | The workspace name outranked the current tab and was hard-cut without `…`. | The name is truncated with `…` and keeps at least a quarter of the bar; the current tab then gets the rest (unit test at 24 cells). |
| 5 | P2 (plausible) | Notices were truncated from the head like titles. | Notices keep their head (`truncate_tail`), titles keep their tail. |
| 6 | P2 | `TOPBAR_REVIEW_PLACEHOLDER` shipped in this document and the evidence deferred to the handoff. | This section; the gate results are recorded below. |
| 7 | P3 | `none` was undocumented and inherits the bar colour inside the bar. | Documented as "keep the cell's colour". |
| 8 | P3 | Popups cover the bar on one- and two-row terminals. | Accepted and documented; the popup is transient and stays at the bottom by design. |
| 9 | P3 | `paint_separators` scanned the layout for every cell of the bounding box. | One pass marks pane cells in a grid; lookups are table reads. |

Sound per the reviewer: gap arithmetic and the layout proptest, `tab_area`, `terminal_size`,
server/viewer mouse agreement, separator and bar cells never hit-testing as panes, selection
bounds, notice expiry (no repeated wake-ups), `[style]` validation and colour mapping, bounded
truncation widths, tiny sizes.

A second independent pass verified the resolutions: items 3–6 and 9 FIXED; item 1 PARTIAL because
a 0.3.0 server rejects the v4 hello with an `error` message that the viewer did not classify, so
the dialog was not offered for that case (now it is: a handshake `error` naming an incompatibility
maps to the same `Unsupported` path); item 7 PARTIAL because the README sentences had not landed
(now present). It also found one new P1: `paint_exit_marker` wrote through ratatui's
`set_stringn`, which indexes its start cell unconditionally, so repainting a stale frame after a
shrink with an unfocused exited pane below the visible rows panicked the viewer. Every string
write in the compositor now goes through a bounds-checked helper, and the tiny-screen unit test
includes an exited unfocused pane below a two-row buffer.

Gate on the final tree (macOS): fmt, strict Clippy, root tests (lib 70, main 3, ecs 19, local_cli 6 incl. the v4 attachment,
detach-drain and migration harnesses, structure 8, real zor 1), rustdoc, MSRV 1.95 check,
fixture-child 3 + 8 + 2, koh gateway 2 + 10 against the v4 binary, packaged binary 8, dependency
patches verified, `git diff --check`; all passed on 2026-09-06.

## Platform and CI limits

- Runtime evidence is macOS only. `ci.yml` configures Linux and macOS hosts, an MSRV 1.95 job,
  an Android cross-compilation check, packaging and an optional cross-repository job;
  `nightly.yml` repeats the suites with 2048 randomized cases. No hosted run of this tree was
  requested or executed, so those jobs are configuration, not evidence.
- Terminal-emulator specific clipboard and mouse behaviour needs manual checks per emulator.
- Relay/NAT and mobile suspend/resume belong to koh and were not exercised.
- Attachment v4 and `FUXCTL2` are incompatible with 0.2.x and 0.3.0 servers; the user stops an old server
  deliberately with its own binary.
