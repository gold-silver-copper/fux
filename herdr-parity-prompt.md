# Bring fux + zor + koh to verified Herdr parity or better

Implement this work; do not stop at a design, scaffold, revised audit or partial milestone. The required outcome is that the composed fux + zor + koh product matches Herdr's supported capabilities and preserves or improves the stack's existing strengths. Prioritize:

1. Integrated multi-machine navigation and agent supervision.
2. Agent breadth and restart/session restoration.
3. Pane/layout controls, direct terminal takeover and transcript retrieval.

These are the first workstreams, not the entire definition of parity. Close the remaining material gaps in the capability audit before declaring the overall objective complete. Backward compatibility and breaking semver are not concerns. Preserve user data, live sessions, repository changes and the separation of responsibilities between components.

## Starting evidence and working rules

Read `docs/capability-audit-2026-09-12.md`, current repository instructions, the workspace manifests, required CI workflows, and the actual code before changing anything. The audit describes fux `1792223ea4a24905501823203b585e50906f0d38`, koh `f6a335237c25aefde9b93290b19a8d909598a95d` and Herdr `d184b41fa36923c132629af725ff98bb02aa1b61`. Reconcile those baselines with the execution-time checkout. Do not blindly apply old prompts or treat stale documentation as implementation truth.

Use `references/herdr` as the behavioral reference and `references/koh` for the active transport repository. Active zor is `crates/zor`; `references/zor` is archived. Record execution-time commit IDs and the latest stable Herdr version. Establish a fixed comparison baseline before implementation; include the audited master capabilities and execution-time stable capabilities, explicitly distinguishing supported, experimental and platform-specific behavior. Do not move the baseline silently or narrow it to only features convenient to implement.

Work in isolated branches/worktrees as needed. Do not reset user changes or terminate the user's running sessions for tests. Use isolated runtime directories, credentials and ports. Keep reference-only repositories unchanged. Koh changes belong in its active repository, with reproducible integration revision tracking in fux. Do not add local absolute-path dependencies or rely on ignored references being present in a clean build. Do not publish commits, push, create PRs or releases unless the execution session authorizes those actions. If PR work is authorized, apply the user's complete PR completion gate; no draft PRs or unsolicited issue/PR comments.

Maintain a committed, reviewable parity ledger under `docs/` with one row per capability: baseline evidence, owner, intended behavior, implementation location, automated scenario, real runtime evidence, platform/provider coverage and remaining gap. Use explicit states such as missing, implemented, fixture-tested, runtime-validated and comparative-pass. A documentation claim or command stub is never a passing acceptance test.

## Architecture constraints

- **fux:** generic terminal/process ownership, retained grids, layout geometry and mutations, per-terminal control authority, capture/input/event primitives, generic persistence and terminal rendering. Keep provider names and agent workflow policy out of its core/protocol.
- **zor:** provider integrations, agent lifecycle authority, session identity/resume policy, supervision, machine catalog/routing policy, task/group recovery, transcript retrieval policy, verification and durable workflow history.
- **koh:** authenticated transport, endpoint grants, connection/reconnect behavior and transport diagnostics. It does not own agent semantics or workspace layout.
- The integrated user experience may be exposed through the existing fux launcher/viewer, but implement orchestration in zor or a thin composition layer. Users should not need to coordinate three services or copy socket paths to perform normal workflows.

Preserve coherent snapshots, explicit replay gaps, bounded resources and backpressure. Rework inadequate bounds with an operational storage lifecycle; do not replace them with unbounded growth. Retain the stack's prompt identity, receipt reconciliation, native correlation and source/check/artifact guarantees.

## 1. Integrated multi-machine navigation and supervision

Implement saved machine profiles and a single navigable Local/machine/workspace/tab/pane hierarchy. Provide equivalent CLI/API host scoping, combined agent attention, search/filtering, notifications, focus navigation and worktree actions. Choosing an agent must attach to its actual host/workspace/pane. Provide understandable setup, authentication, reconnect and error flows without exposing implementation bookkeeping in routine use.

Support koh peer connections and the existing SSH-style access workflows needed to replace Herdr for users of SSH configuration, aliases and remote setup. An SSH bootstrap may establish the stack's services/transport where appropriate, but do not require a manual server migration or silently grant extra endpoint authority. Specify installation/update behavior, remote prerequisites and compatibility diagnostics. Preserve the user's authentication policy.

Give every route an unambiguous host/server-incarnation/workspace/pane identity. Distinguish disconnected, stale, unknown and freshly observed state. Cached metadata must never authorize input to a replacement target. Scope credentials and viewer/control grants per machine; revocation must take effect. One unreachable machine must not stall navigation, metadata updates or control of others. Handle reconnect within and beyond koh's retention window with explicit resynchronization and no duplicated input.

Acceptance: operate Local plus at least two actual remote hosts from one interface; create/attach/navigate workspaces, inspect all agents, respond to one blocker and return to the originating task. Disconnect/restart one host while another remains usable. Exercise endpoint revocation, conflicting machine/pane names, stale cached identities, gateway loss, retention expiry, NAT/relay and a real non-loopback network. Same-host gateway tests alone do not satisfy this gate.

## 2. Agent breadth and session restoration

Derive the provider inventory from the pinned Herdr agent, integration and restore tables. The audit counted 21 principal agents, two weaker detection entries and 17 resume paths; re-enumerate names and versions instead of treating those counts as the specification. Match each provider's actual Herdr capabilities, including detection, lifecycle authority, needs-input states, session identification, launch and supported resume. Do not demand imaginary native APIs from providers, or claim native correlation from screen matching. Label passive, integration-derived and native evidence distinctly.

Complete the native Codex/OpenCode workflows and Claude integration where provider interfaces support them: multiline submission, response correlation, permission/question responses, interruption, session identity, process death and native resume. Preserve operator-configured permissions and sandbox settings; bypassing questions or silently widening permissions is not blocker handling. Publish versioned capability output that agrees with runtime behavior, including precise unsupported operations and fallback reasons.

Persist generic session shape: workspaces/tabs, names/order, pane layout, focus, cwd, safe launch descriptors and optional bounded screen history. Separate viewer detach, controller restart, fux crash, agent-wrapper death, machine reboot and live server replacement. Persist state atomically with a documented recovery schema; validate malformed/partial snapshots. Restored screen content must be marked historical, never fresh agent-state evidence.

Zor must resume agents with their supported session commands/APIs and reconcile durable task/group identities against the new fux incarnation. Provide useful recovery when cwd, worktree, provider binary or session storage is unavailable. Do not blindly replay pending prompts or relaunch arbitrary old commands with side effects. Allow explicit restore/skip/retry decisions through the integrated UI/API.

Resolve grouped-attempt replacement and retained-group membership restrictions, uncertain checks/launches, lost-wrapper recovery, history archival and cleanup. Reuse of an operation ID must never become permission to repeat uncertain effects. For Herdr's experimental live PTY handoff, supply an equivalent separately tested opt-in path on applicable platforms before claiming parity with that capability; keep it distinct from ordinary restart restoration.

Acceptance: record real-provider launch, working/idle/blocked detection and supported resume scenarios for every provider claimed at parity. For native adapters also demonstrate correlation, multiline input, blocker response and interrupt where advertised. Kill/restart the viewer, supervisor, wrapper and terminal server independently; reboot a test host. Restore a mixed-provider layout and continue scheduled work without duplicate input or false completion. Provider access/authentication gaps remain unvalidated rows, not fixture-derived passes.

## 3. Pane/layout controls using ratatui-hypertile

The requested reference has been cloned into `references/ratatui-hypertile`:

- Repository: https://github.com/nikolic-milos/ratatui-hypertile
- Reference commit: `92fa63300f802c4465a4fbd2b928cf3ef1b4f8d0` (crate 0.4.1).
- License: MIT; preserve its copyright/license notice for copied or substantially adapted code.
- Study `src/core/state/{mod,mutation,movement,focus}.rs`, `src/core/types.rs`, `src/core/serde_impl.rs`, `src/engine.rs`, `src/input.rs`, `src/core/state/tests.rs`, and `extras/src/runtime/{mouse,workspace,keymap,render}.rs`.

The existing root ignore rule excludes `references/`; this clone is local reference material. On a fresh checkout, clone the URL if absent and inspect the recorded commit without overwriting a dirty reference checkout. Record any deliberate baseline update.

Use its tree manipulation, directional movement, geometry/hit testing, drag interactions, serialization and tests as concrete design references. It tiles widgets within an app; it is not a PTY multiplexer or proof of multi-client correctness. Decide explicitly whether to use its core library or adapt its algorithms to fux's existing ECS/layout model. Document the decision and measured dependency/hot-path cost. Do not import the extras runtime wholesale or rewrite the renderer merely to resemble the reference. Validate imported layouts independently; reference serialization does not establish safe untrusted-input handling.

Implement Herdr-equivalent split/close/focus/resize, zoom/unzoom, pane swap/move, tab/workspace ordering, movement between tabs/workspaces on the same server, layout export/apply, keyboard controls and mouse border/pane dragging. Preserve stable pane/process identity during layout changes; moving a pane must not recreate its PTY or invalidate task identity unnecessarily. Cross-host movement must not pretend that an OS process migrated.

Define minimum-size and tiny-terminal behavior, deterministic directional focus, ratio rounding, zoom scope, and competing viewer resize/focus policy. Mutations must be atomic and observable with a layout generation; reject stale/conflicting requests rather than misapplying them. Validate duplicate/missing pane IDs, invalid ratios, excessive depth/size and references outside the target workspace. Distinguish applying geometry to existing panes from a restore template that can launch processes; plain layout import must not execute embedded commands.

Acceptance: exercise keyboard and mouse workflows on nested layouts, across tabs/workspaces and through remote viewers. Verify export/apply round trips, zoom restoration, focus after removal, tiny/zero-size resize handling, deeply nested input rejection and conflicting clients. Use property/invariant tests for unique pane identity, valid coverage/geometry and stable state after operation sequences. Demonstrate that pane PID/PTY and active zor task identity survive move/swap/zoom.

## 4. Direct terminal takeover and transcript retrieval

Provide a direct per-terminal interactive attachment and a machine-readable live terminal stream. Define one writable controller lease with explicit takeover/release, multiple read-only observers, lease loss/disconnect behavior and resize/scroll authority. Enforce authority at the server. Revoked clients must lose authority immediately; observers cannot inject input, resize, scroll or acquire control through an alternate request. Define how normal workspace viewers and zor automation participate in the same policy, so they cannot bypass the lease.

Human takeover must visibly pause conflicting automated input and transcript-scrolling operations. Returning control must reconcile the current foreground occupant, provider session and pending operation state. Root PID alone does not identify the foreground input consumer. Preserve terminal modes and restore the outer terminal on detach/error. Keep endpoint authorization separate from OS sandbox claims.

Implement reliable text/ANSI capture with explicit source, freshness, completeness and truncation. Match Herdr's retrieval of virtualized alternate-screen transcripts: for recognized idle agents in a safe viewport state, collect overlapping pages through supported application scrolling and restore the original/bottom viewport as appropriate. Prefer native transcript access when it provides equivalent semantics, with accurate provenance. Do not claim host scrollback or a bounded native response summary is a complete transcript.

Bound retrieval by bytes/pages/time, detect duplicate page boundaries and concurrent output, handle wide/combining characters and wrapped lines, and stop on human input or agent state changes. Working/blocked/unknown agents and unsupported applications need explicit passive fallback or actionable refusal. Do not scroll an agent under direct human control. Interrupted retrieval must report partial data and restoration failure truthfully.

Acceptance: two controllers race for a terminal; takeover revokes the old one; observers remain read-only across all API paths; lost connections release/reconcile authority. Retrieve a real long Claude/OpenCode alternate-screen transcript beyond the visible screen, verify page boundaries against known content, and confirm viewport restoration. Inject user typing, resize, agent activity and disconnect mid-read without misdirecting input or reporting an incomplete result as complete.

## 5. Close the other gaps required by the overall claim

The three priority workstreams cannot by themselves establish complete Herdr parity. Implement and validate remaining material ledger gaps, including:

- Representative-repository source-bound verification. Current limits reject fux's own 471-file, approximately 6.9 MB committed tree. Address whole-tree input support, Git-dependent checks, realistic command time/output limits, artifact storage, check cancellation, uncertain execution resolution and safe scratch/history cleanup. Preserve coherent source/check/artifact selection and rejection of stale evidence.
- Sustainable operation beyond existing task/check/source/group record limits. Demonstrate more than 1,000 task/check cycles over a multi-day run with archival and injected failure, without manual journal replacement, lost required evidence or unsafe worktree removal.
- Terminal graphics and image workflows, input/clipboard/IME behavior, configuration, plugin installation/discovery and equivalent extension actions/events/panes needed by the baseline. Preserve licensing when studying or adapting reference code; independently implement alternatives where required.
- Native Windows runtime, installation/update and applicable remote-host/client behavior, alongside Linux/macOS. Reconcile Herdr's conflicting Windows documentation against source/runtime; do not excuse stack gaps with stale claims of unsupported Herdr Windows SSH targets. Keep true baseline platform exceptions explicit. Maintain existing Android checks and separately label runtime coverage.
- Packaging and a documented clean-install path for the composed product. A developer checkout with manually launched gateways is not product parity.

Do not silently move these to “future work” while declaring the overall request complete. If execution is blocked by unavailable platforms, provider access, infrastructure or an external decision, complete independent work and report the exact unfulfilled gates. Do not invent credentials, spend unapproved resources or count skipped scenarios as passes.

## Verification, comparison and delivery

Implement in coherent increments, keeping the product runnable. Run required formatting/linting, targeted correctness and failure-path tests, then the repository's required broader suites, packaging and integration checks. Reproduce the prior handshake/replay and gateway fault scenarios so these features do not regress. Use real PTYs/processes and actual provider/platform/network scenarios where required; fixtures supplement those tests.

Build a reusable comparative acceptance suite against the fixed Herdr baseline. Run equivalent operator workflows with the same providers and terminal environment. Preserve raw logs, versions, commands, screen evidence where useful and failure classifications. Separate unsupported, unavailable, failed and passed results. Review screenshots/recordings of the integrated UX as well as machine-readable output.

Measure startup, steady-state CPU, total participating-process memory, rendered input-to-visible p50/p95/p99 latency, sustained output, multiple panes/viewers and reconnect. Include zor and both koh endpoints for remote comparisons. Use equal build profiles, features, hardware and network conditions. Establish measurement tolerances before comparing results. Do not substitute internal historical benchmarks or command-acceptance latency for current head-to-head evidence. Investigate material regressions rather than averaging them away. Claim “better” only for named dimensions supported by results.

Have a fresh independent reviewer inspect the complete intended diffs across all changed repositories for correctness, identity/authorization boundaries, persistence/recovery, resource bounds, terminal behavior and missing tests. If unavailable, perform an explicit separate full-diff review. Validate findings, fix confirmed in-scope issues, rerun affected checks and repeat review for new serious findings. Follow the PR completion gate if a PR is in scope.

Deliver working code, user documentation, recovery/operator instructions, changelog updates, reference/license attribution, reproducible test/benchmark tooling, the completed parity ledger and an updated capability audit. Give exact changed repository revisions, verification results, CI state if applicable and remaining risks. Report three distinct outcomes: priority-workstream completion, whole-product parity, and measured advantages. Declare whole-product parity only when every applicable baseline row passes and the stack's existing guarantees remain intact. Otherwise state that the overall objective is incomplete and list the evidence or implementation still required.
