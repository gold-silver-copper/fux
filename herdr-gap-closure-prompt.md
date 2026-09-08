# Build superior agent workflows in zor over a minimal fux multiplexer

Implement and verify an agent runtime in zor that uses fux solely through its multiplexer API. Keep all agent behavior in zor. Use the local herdr reference to establish agent workflow baselines and demonstrate improvements with repeatable scenarios. Broad terminal feature parity is not the objective.

This prompt supersedes the architectural direction of `herdr-parity-prompt.md`, the agent-aware portions of `agent-surface-prompt.md`, and earlier versions of this document. In particular, their instructions to put agent reports, badges, semantic waits, or orchestration inside fux no longer apply. It authorizes local implementation and verification only, not commits, pushes, PR publication, merges, or messages to others. Follow applicable AGENTS.md instructions. If separately asked to prepare a PR, apply the full repository completion gate and never create a draft PR. Backwards compatibility and breaking semver are not concerns.

## 1. Non-negotiable project boundaries

| Project | Owns | Does not own |
|---|---|---|
| fux | PTYs, processes, panes, tabs, workspaces, layouts, terminal rendering, bounded history, and a reliable multiplexer API | Agent identity/state, detection, prompts, tasks, semantic waits, worktrees, agent recovery, dashboards, or notifications |
| zor | Agent discovery/detection, adapters, agent sessions, tasks/attempts, prompt coordination, semantic waits, worktrees, results, orchestration persistence, recovery, dashboard, notifications | PTY ownership for fux-hosted panes, fux terminal emulation, transport authentication |
| koh | Peer identity, admission, transport, link resumption | Terminal semantics, agent state, orchestration policy |

Fux must remain independently useful with zor and koh absent. It must not import or supervise zor, add `agent.*` requests/events/components, parse OSC 7877 into agent state, expose agent-aware waits, or render agent badges. Do not introduce a general metadata/plugin framework merely to put zor's UI inside fux. Zor's dashboard is an ordinary terminal application running in a pane. Ordinary terminal titles remain ordinary titles; they are not an agent-state API.

Zor owns the single-command setup and discovery path for its own service. It consumes fux's API as an external client. For fux-hosted agents, create the real command directly in a fux-owned pane; do not put every agent behind zor's wrapper PTY. Preserve zor's independently useful wrapper/protocol-only modes.

A proposed fux addition must satisfy this test: would a terminal controller for a shell, build, editor, or debugger need the same primitive without knowing what an agent is? If not, implement it in zor. Keep all agent-specific patterns, invocation templates, resume logic, and git operations out of fux.

Generic naming alone is insufficient. Before extending fux, identify the concrete zor workflow that existing multiplexer operations cannot support, demonstrate the missing contract with an agent-free fixture, and justify the added state, resource bounds, and maintenance cost. Prefer composing existing APIs in zor. Do not grow fux speculatively into a general automation platform. Fux input receipts describe terminal delivery; only zor can interpret a response, approval request, task outcome, or verification result.

This boundary applies to convenience commands and configuration too: do not add fux agent presets, agent launch flags, orchestration aliases, or agent-specific metadata disguised as generic fields. Users and lead agents invoke zor for agent workflows; zor invokes fux for pane operations. Evaluate superiority against herdr at the zor workflow level, while evaluating fux on the simplicity, reliability, and efficiency of its multiplexer API.

## 2. Establish the current baseline

Read README, HANDOFF, design/security documents, both local protocols, and `docs/capability-audit-2026-09-06.md`. Inspect the current source and record HEADs plus relevant working-tree changes in all owning repositories. Preserve unrelated modifications. Do not assume historical prompts accurately describe current APIs, versions, tests, or performance.

Revalidate the audit's findings: incompatible zor/fux handshake; obsolete two-cell geometry subtraction; an absent revision field sought by the observer; no active bundled rules; stale dependency patches; failed real-zor observation; and gateway tests blocked by netmon permissions. Fux's lack of agent state is intentional under this prompt and must not be repaired by adding agent semantics to it.

Inspect herdr's agent APIs, prompt/wait implementation, detection manifests, integrations, worktrees, result-reading behavior, agent UI, and resume code. References remain read-only. Respect licenses and attribution for any reused material. Do not assume herdr lacks a behavior merely because an earlier audit did not inspect it.

Create an agent workflow ledger with source-backed baseline behavior, desired improvement, owner, implementation, test, and remaining limitations. Separate demonstrated superiority from parity and unverified coverage. Do not require graphics, floating panes, marketplaces, broad platform parity, or live server handoff as part of this task.

## 3. Repair composition and strengthen only generic fux primitives

First reconcile the actual handshake, response shape, pane geometry, and intended companion patches. Update all consumers together and detect incompatible binaries explicitly. Reconstruct intended source changes without exporting unrelated worktree modifications. Require real-binary integrations automatically for relevant combined-stack changes while keeping standalone builds independent.

Add only the missing generic capabilities needed by a reliable external controller:

- Stable pane handles scoped to a server incarnation, with explicit lifecycle and process identity. Reject stale handles after restart or reuse.
- Coherent captures: text, terminal dimensions, history range, and revision from the same snapshot. Permit conditional capture or equivalent event-driven reads so idle panes do not need repeated re-emulation.
- Snapshot plus event subscriptions with a defined synchronization boundary, bounded buffering, sequence/gap reporting, and resynchronization. Never silently turn a dropped event into apparent continuity.
- Ordered input operations with request identity, defined acceptance/delivery receipts, and bounded deduplication. Precisely define the receipt boundary; successful PTY writing does not prove application processing. State the deduplication retention window and behavior after eviction or server restart.
- Bounded retrieval of final output and exit records after a pane closes, with explicit expiry. A late controller must distinguish expired evidence from an empty successful result.
- Existing pane creation, cwd/argv, focus, capture, close, and layout operations with consistent errors and discoverable machine-readable contracts where useful.

Keep fux's authoritative model in ECS. Blocking I/O and child waits stay in OS adapters. Do not add a fux task journal or agent restoration subsystem. Use bounds and backpressure at every external boundary.

If zor needs explicit application reports, prefer its own integration endpoint. If passive observation needs information unavailable in screen captures, justify a bounded generic terminal-output subscription with explicit framing/replay/gap semantics. Fux may transport opaque terminal bytes; their agent meaning belongs entirely to zor. Screen capture alone must not be claimed to preserve arbitrary OSC reports.

Acceptance: generic fixtures using shells/build-like programs prove all new fux contracts without importing zor or agent concepts. Real zor consumes these contracts successfully. Observer disconnect, stall, crash, or malformed requests cannot disrupt pane ownership or normal viewer input.

## 4. Make zor a useful observer by default

Implement a zor-owned service that discovers panes, reconciles its registry on reconnect, and watches eligible processes without manual per-pane setup. Distinguish observation of an existing pane from management of an agent that zor launched. Adoption must not silently authorize closing panes or removing directories.

Ship active bundled manifests with documented evidence and versions. Start with Claude Code, Codex, and OpenCode, then expand against the relevant herdr inventory. Loading must work with standard configuration fallbacks and no custom rule directory. Support bounded overrides, deterministic precedence, schema checks, atomic reload, and diagnostics explaining each verdict.

Prefer explicit agent integrations when they establish stronger evidence; retain passive detection for unmodified tools. Explain capability differences. Include real-agent fixtures for working, approval/input required, idle, startup, exit, resizing, and unknown screens. Negative fixtures must include stale scrollback, quoted prompts, echoed names, shell output, nested tools, and malformed control strings. Do not invent support based on a process-name match or synthetic happy-path screen.

Scope reports to server, pane, process, producer lifetime, and sequence as appropriate. Define precedence, heartbeat expiry, unknown/stale state, and hysteresis. Treat reports as observations with provenance, not authenticated proof of task success. Never infer completion from silence.

Acceptance: installed default rules identify supported agents, explain evidence, and pass positive/negative fixtures. Restarting zor reattaches observation without restarting fux or agent processes. Inactive panes cause bounded, measured observer overhead.

## 5. Put the task model, prompts, and waits entirely in zor

Define a small model separating agent session, task, attempt, prompt operation, observation, result, and artifact. An agent's visible `idle` or `blocked` state is distinct from a task outcome. Persist only what is required for reliable coordination and recovery; do not start with a general workflow language.

Provide coherent CLI and structured API operations for start/adopt/list/inspect/explain/read/focus/prompt/wait/cancel/result. Define exact command names during implementation. Fux exposes pane operations; zor translates agent operations into them.

Prompt/wait correctness is the central requirement:

- Establish an input receipt and observation boundary for each prompt. Old blocked/idle evidence cannot satisfy a wait for a new prompt.
- Separate submission, delivery, observed reaction, needs-input, task completion, process exit, timeout, cancellation, and uncertain outcome.
- Do not require an observable working state: fast agents may respond without displaying one. Define the strength of evidence available to each adapter.
- Serialize or explicitly reject competing prompt writers. Ordinary human input through fux must remain possible; define how external input invalidates or weakens correlation instead of assuming exclusive control. Generic input ownership primitives belong in fux only if required and generally useful.
- Retry using recorded operation IDs and receipts. If a crash leaves delivery ambiguous, reconcile where possible and expose uncertainty where it cannot be resolved. Do not claim durable exactly-once prompt processing over terminal input.
- Bound waiters, deadlines, queues, and journal retention. Define outcome on observation loss, event gaps, pane closure, and fux restart.

Acceptance: test stale pre-prompt blocked state, immediate answers, immediate exit, multiple callers, human intervention, observer loss, and disconnect at every submission/receipt boundary. Synthetic agents establish protocol behavior; separate real-agent tests establish adapter guarantees.

## 6. Own worktrees, coordination, and result collection in zor

Zor creates/lists/opens/removes worktrees and associates them with tasks and attempts. Invoke git with argv through zor's OS boundary; fux receives only a cwd and command. Preserve the main worktree, refuse normal removal of dirty or actively used trees, and specify explicit force behavior. Handle conflicting branches, path validation, symlinks, partial failure, and cancellation. Track ownership so adoption or recovery cannot turn user-owned files into cleanup targets.

Implement bounded fan-out and collection, task dependencies where needed by concrete scenarios, cancellation, and handoffs. Recover from worktree-created/pane-not-created and pane-created/receipt-not-recorded failures. Prefer explicit reconciliation records over assumptions about transactional behavior across programs.

Results should identify task/attempt, agent, worktree, output evidence, artifacts, changed files, requested checks and their outcomes, and blockers. Keep agent claims distinct from independently observed verification. Do not equate a zero process exit, idle screen, or agent-written success message with verified task completion. Verification requirements belong to task policy in zor.

Acceptance: in a disposable repository, a lead starts two isolated workers, observes fresh responses, collects artifacts, runs specified verification, performs a handoff, and cleans up only owned resources. Cover failed checks, worker crashes, dirty trees, cancellation, missing artifacts, and interrupted cleanup.

## 7. Recovery and user experience belong in zor

Persist task/attempt identities, prompt receipts, workspace associations, artifacts, and necessary adapter resume metadata in zor with bounded, private, crash-safe storage. On restart, reconcile with fux's current server incarnation and live panes. Keep running agents alive. When fux itself has restarted, explicitly distinguish lost processes from resumable agent sessions and recreate commands only under the task's launch/resume policy. Never blindly replay pending prompts.

Implement an ordinary `zor dashboard` TUI and machine-readable equivalents. Show agents across workspaces, evidence/freshness, tasks needing input, results, and recovery problems. Allow navigation to the corresponding fux pane through the normal focus API. Notifications, filters, attention ordering, and agent-state colors belong to zor. Fux requires no agent display configuration or protocol fields.

Zor owns startup/discovery so a user can start a managed agent or open the dashboard with one documented command. Show actionable errors for unavailable fux or incompatible contracts. Optional zor failure must leave the ordinary multiplexer usable.

For remote use, carry zor's control API and fux's attachment stream through separately authorized koh services as required. Do not overload the fux viewer stream with agent semantics or accidentally expand an attachment grant into unrestricted control access. Test reconnection within and beyond koh's actual retention window. Do not claim the gateway provides standalone koh predictive local echo.

## 8. Demonstrate improvement against herdr

Use the same fixtures, workloads, and failure injection when the interfaces permit. Inspect and run the reference behavior before declaring a comparative advantage. Report unsupported comparisons or environmental blockers honestly.

Measure these outcomes:

1. False completion or stale-state wait satisfaction after a new prompt.
2. Duplicate input and ambiguous delivery handling during reconnect/restart.
3. Recovery of ongoing tasks after orchestration service failure without restarting workers.
4. Detection false positives, missed blockers, evidence freshness, and supported-agent coverage.
5. Reliable multi-agent fan-out, artifact collection, verification, and cleanup.
6. Controller usability: commands/setup required and actionable errors.
7. Idle/burst overhead, capture traffic, latency, memory, and scaling with observed panes.

Record results, provenance, and practical limits. A larger API or rule count is not proof of superiority. Do not hide herdr strengths or claim a universal win from one benchmark. Each stated improvement needs a repeatable acceptance scenario.

## 9. Verification, review, and delivery

Work in bounded slices: composition repair → generic fux API contracts → zor observation → zor prompt/task model → worktrees/results → recovery/dashboard → comparative scenarios. Finish each slice's integration and failure tests before building on its assumptions.

Run targeted checks and all applicable owning-repository checks. Discover actual script names and flags. Include formatting, clippy, declared toolchain floors, standalone packaging, protocol-only builds, required real-fux/real-zor and real-fux/koh tests, and clean dependency reconstruction with `python3 tools/dependencies.py verify --build`. Required integrations must fail on missing binary paths instead of silently skipping. Keep standalone checks independent of companion repositories.

All real-process tests use disposable HOME/XDG directories, task-owned sockets, bounded waits, and cleanup guards. Never touch personal sessions or remove unrelated worktrees. Record OS/service restrictions as verification blockers; do not mislabel them product defects or claim the affected checks passed.

Add a structural check preventing agent-specific imports, API variants, state, and git/process policy from entering fux. Review that check for semantic coverage rather than relying solely on keyword bans. Preserve normal terminal behavior and ownership invariants.

This prompt permits fresh independent reviewer subagents. Review the complete intended change set across owning repositories and companion patches, validate findings, fix confirmed in-scope issues, rerun affected checks, and obtain a final review after fixes. If unavailable, perform a separate explicit full-diff review. Stronger repository gates still apply.

Deliver implementation, current design/ownership documentation, a generic fux API contract, zor CLI/API documentation, the agent workflow ledger, fixture provenance, runnable scenarios, test and benchmark results, review findings, and remaining blockers. Update stale documentation so no file advertises agent state or orchestration inside fux. Preserve unrelated historical prompts, identifying this one as superseding their architectural direction.

Completion requires working, verified agent workflows in zor and demonstrated improvements for the selected comparison scenarios. Fux must still be just a multiplexer with a good API.
