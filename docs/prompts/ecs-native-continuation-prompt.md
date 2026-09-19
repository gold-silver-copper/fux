# Continue fux and zor: finish the rewrite using Bevy's mechanisms

## Objective

Continue the existing `ecs-rewrite` implementation. Deliver a working composed fux + zor system, not another scaffold or architecture proposal. Finish the incomplete integration, review the implementation for genuinely ECS-native design, and prove the resulting workflows with real processes and terminal interaction.

The goal is not to maximize the number of Bevy dependencies. The goal is to let Bevy own the mechanisms it already implements, while application code owns terminal semantics, durable workflow policy and security. Do not build a second ECS, scheduler, asset server, layout engine, focus system, reflection registry or event router beside Bevy. Do not force unrelated transport code into ECS for stylistic uniformity.

This prompt is an execution assignment, not evidence that any feature is complete. Preserve existing behavior and accepted safety invariants. Do not restart milestones that already work.

Scope override: focus exclusively on fux and zor. koh is irrelevant to this assignment: do not investigate, modify, integrate, build or verify it, and do not require its binary, checkout or pin for acceptance. The intended future remote transport is iroh-ssh, but implementing or integrating iroh-ssh is also out of scope now. This supersedes all koh-specific requirements and blockers in earlier prompts and handoffs. Preserve straightforward application endpoint boundaries without designing a speculative transport framework.

## Establish the execution baseline

Work in the existing `../fux-rewrite` worktree. Read applicable repository instructions and, in this order:

1. `docs/HANDOFF.md` and `docs/prompts/continuation-prompt.md`.
2. `docs/verification.md`, `docs/dependencies.md` and `docs/security.md`.
3. `docs/prompts/ecs-native-rewrite-prompt.md`, including execution-time amendments.
4. Relevant sections of `docs/bevy-source-patterns.md` and the actual pinned Bevy source.
5. `crates/zor/docs/model.md` and the old stack's ownership, lifecycle and multi-machine contracts in `../fux/docs/` and `../fux/crates/zor/`.
6. `docs/capability-status.md`, treating it as an explicitly stale mid-milestone-6 inventory, not a current verdict.

The recorded checkpoint is `9260d3b` on `ecs-rewrite`, dated 2026-09-15. Milestones 1–6 were recorded complete. Milestone 7 contains unfinished zor plugin/machine/dashboard integration; the continuation records noncompiling zor, comment-only module roots, missing plugin hooks and empty method tables. fux's latest recorded result is 208 passing tests; zor's last green milestone-6 result is 71. These are historical facts, not today's verification results. Inspect the current tree and preserve intervening work; do not assume the same errors remain.

Record execution-time fux/zor revisions, local modifications, toolchain and resolved Bevy version. Use existing code and contract evidence to form a requirement matrix: implemented behavior, current evidence, unfinished work, responsible component and acceptance scenario. Do not turn this into a prolonged audit before repairing the known integration boundary.

Keep `main` and reference checkouts unchanged. Use isolated runtime directories, ports, credentials and disposable processes. This assignment authorizes fux/zor source and documentation changes and local verification. The user additionally authorized incremental commits during execution (2026-09-19): commit completed, verified chunks on `ecs-rewrite`, with one integration owner and explicit paths so unrelated pre-existing changes remain excluded. Pushing, tags, releases, PRs, remote installation and changes to the user's running sessions remain unauthorized. Earlier prompts' session-specific permissions are not new authorization.

## Ownership and design rules

### One authoritative model per application

- **fux:** authoritative World for local processes, terminals, layouts, viewers, receipts, retained evidence and generic surfaces. No provider or task policy.
- **zor:** authoritative World for tasks, attempts, prompts, operations, checks, sources, artifacts, groups, worktrees, plugins, machines and observations. No duplicate terminal/PTY implementation. Its dashboard is a scene hosted by fux, not a separate TUI.

External IDs and process/server incarnation identities are protocol facts. Bevy `Entity` values are local runtime identities; never assume they survive serialization, restart or another application's World. Model and validate the mapping explicitly.

### ECS-native mechanisms, with correct semantics

For each touched subsystem, identify the applicable Bevy mechanism before introducing custom infrastructure. Verify APIs and scheduling against the resolved version; a source-pattern document can be wrong or superseded. Amend incorrect guidance with source evidence rather than perpetuating it.

| Concern | Required design direction |
|---|---|
| Composition | Cohesive `Plugin`s that register their components, resources, systems and contracts. Use existing App/runner assembly; no parallel lifecycle registry. |
| System access | Prefer typed `Query`, `Res`, `ResMut`, `Commands` and cohesive `SystemParam`s. Declare ordering through system sets and dependencies. Keep exclusive World access only where an atomic structural operation or Bevy API genuinely needs it; document the invariant, not a blanket exemption. |
| Entity shape | Use required components for intrinsic shape, and markers for real presence/absence. Split data by mutation owner and query usage, not mechanically into one component per scalar. Ordinary boolean values and optional data are valid when they are data, not competing lifecycle flags. |
| Relationships | Use Bevy relationship pairs for ownership and membership. Do not maintain two authoritative parent/member collections. Keep separately meaningful order, stable-ID indexes and measured caches only with an explicit owner and invalidation rule. Use linked despawn only when lifetimes truly coincide; deleting a dashboard row must never kill a remote task. |
| State | Use `bevy_state` for genuinely application-wide states. Independent tasks, attempts and machines need per-entity state components or markers and validated transitions, not one global `State<TaskState>`. Use `Disabled` deliberately and include disabled entities in cleanup/recovery queries where required. |
| Reactions | Entity observers suit local interaction and lifecycle notification. Component hooks maintain structural/index invariants; they do not spawn subprocesses, perform network I/O or decide durable workflow policy. Ordered policy belongs in scheduled systems, not order-sensitive observer cascades. |
| Messages and durability | Use Bevy messages for bounded update-time communication, with explicit writer/reader ordering and retention. They are not a durable log. Keep receipts, replay cursors, pending effects and uncertain operations in their authoritative bounded model until resolved. Never lose work because a system skipped an update or a message aged out. |
| Change propagation | Prefer `Changed`, `Added`, removal tracking and Bevy hierarchy propagation where appropriate. Mutate only on actual change. Keep external sequence numbers, generations and replay cursors where they express protocol guarantees; Bevy change ticks cannot replace them. Every derived cache needs a testable invalidation contract. |
| I/O and scheduling | Use the existing `bevy_tasks`/`IoTaskPool` adapter boundary for fux/zor. Worker completions enter bounded channels; only scheduled transitions mutate the World. Keep subprocess handles, sockets and blocking I/O in adapters. Track cancellation, ownership and stale completions; dropping a task handle is not proof its child process stopped. |
| Time and wakeups | Use the established Bevy time/deadline model with a sleep-until-input-or-deadline runner. Asset changes, transport completions and shutdown must wake it. Avoid periodic polling when an event suffices. Intentional supervision intervals are deadlines, not evidence of a zero-wakeup idle system. |
| Layout | `bevy_ui` owns layout, clipping and stacking. Application systems set `Node`/relationships and read computed geometry. Preserve inert templates, per-viewer instances and the pane-size fold. Do not write a second rectangle solver or treat all viewers as sharing one focus/camera. |
| Interaction | Feed terminal input into the existing Bevy input/focus/picking pipeline. Use entity-targeted activation, modal focus groups and focus-driven dismissal. Preserve application mouse forwarding and captured gestures. Review widget patterns; use supported APIs rather than vendoring wholesale or introducing a GPU/window runtime. |
| Assets | Use `bevy_asset` for configuration, rules, themes, layouts and manifests where already designed. Retain strong handles, define validation/activation ownership and keep the last valid asset after a bad reload. No duplicate filesystem watcher. Loading a manifest must not itself authorize process execution. |
| Scenes and persistence | Use reflection and Bevy scene/entity remapping over explicit allowlists. Validate in an inert staging World before live mutation. Persist semantic identities and allowed session/workflow state, never OS handles, secrets or computed UI state. Preserve atomic writes and intent-before-effect ordering. |
| BRP | Reuse `bevy_remote` method registration, schema and projection machinery. Expose only authorized projections and typed commands; never enable unrestricted World mutation. Preserve token scope, body/batch/connection limits and deadlines. The documented bounded HTTP acceptor is an accepted fallback; do not revert to `RemoteHttpPlugin` solely to increase Bevy usage. |

Do not perform ceremonial rewrites. For a justified exception, record the required behavior, the Bevy mechanism considered, its concrete mismatch and the smallest retained custom code. Native mechanisms are a means to simpler, correct ownership—not an exemption from atomicity, resource bounds or performance measurement.

## Execute in dependency order

### 1. Restore a complete milestone-7 integration boundary

Finish the actual missing pieces in `crates/zor/src/machines.rs`, `dashboard.rs`, `plugins/`, and `remote/{plugin,machine,dashboard}_methods.rs`. Wire real plugin registration and behavior, not empty exports, no-op systems or successful placeholder replies. Resolve compilation errors without disabling features or deleting their callers.

Preserve the existing effect/inbound contracts and shared model. Review registration and schedule order, especially journal completion before external effects, deferred structural changes before dependent queries, projection updates before replies, and stale completion rejection after restart/removal.

### 2. Complete plugins and the fux-hosted dashboard

Implement the manifest and command contracts already specified: install/link, enable/disable, list/inspect, run/logs, actions, hooks, panes and link handlers. Plugin processes receive scoped capabilities and documented invocation context. Bound logs and queues. Disable/uninstall must stop only owned children and retire/revoke the grants they no longer need.

Hooks consume fux/zor event streams using persisted cursors, handle gaps explicitly and never equate replay with permission to repeat side effects. Define the crash boundary between event receipt, cursor persistence and action completion; do not claim exactly-once plugin side effects without a protocol that provides it.

Build dashboard rows and interaction using BSN/`bevy_ui` and projection components. Preserve stable row identity and update only changed content instead of reconstructing every scene on every poll. Surface input must resolve the correct provider/node/viewer and invoke guarded zor actions. Cover attention ordering, stale observations, selection/focus, local scroll versus provider input, resize and return from exact-pane attachment.

### 3. Complete fux/zor supervision without a transport dependency

Finish catalog persistence/reload, independently bounded machine workers, freshness/error classification, machine-scoped CLI/BRP routing, exact-target handoff and durable remote-operation intents. Catalog edits, reconnects and late replies must not retarget operations or replay uncertain mutations. One failed machine must not stall Local or the others.

Exercise the existing machine-scoped application behavior using two disposable fux/zor stacks on this machine, connected through their supported direct loopback endpoints. This proves routing, observation, authorization and process ownership, not WAN connectivity or future iroh-ssh integration. Do not build a tunnel, SSH bootstrap, gateway or generic transport-plugin layer for these scenarios.

Preserve explicitly configured endpoints, capability checks and control-versus-attachment separation. A dropped connection or a re-established endpoint must not authorize replay of an HTTP mutation, prompt or input operation. Keep remote service/process identity distinct from the connection used to reach it.

Remove koh prerequisites from the active fux/zor build, startup and acceptance paths where they prevent independent operation. Retire koh-specific harness requirements and stale blocker statements for this assignment rather than carrying them as `UNAVAILABLE` gates. Do not delete unrelated historical code or documentation merely to remove every mention of koh. Record future iroh-ssh integration as out of scope, not unfinished work required for fux/zor completion.

### 4. Close the evidence and delivery gaps

Run the real fux/zor scenario harness after the stack builds, with no koh dependency. Complete the milestone-7 ECS-native review and fix confirmed findings. Then finish milestone 8 for fux/zor: design and ownership docs, generated/discovered protocol contracts, security, install/update instructions, changelogs, capability ledger and performance comparison.

Refresh the capability ledger from current source and evidence. Preserve separate labels for implemented, unit/property-tested, real-process-tested, live-provider/network/platform-validated and blocked. Do not infer Herdr parity, paid-provider acceptance or native Windows support from rewrite completion. Carry forward unsupported features honestly rather than silently expanding this assignment into every parity workstream.

## Verification that earns completion

Start with affected tests and actual workflows; run the final format/lint/workspace/tooling checks once the shared tree is integrated. Parallel workers must not run competing workspace-wide gates against half-written code. Delegate only independent, substantial ownership slices, with shared interfaces agreed before edits and one integration owner.

Retain regression tests for plausible behavior failures. Do not add source-text assertions, method-count checks or mock echoes as substitutes for running the feature. At minimum demonstrate:

- Two viewers with different viewports: correct independent layouts/focus and shared process identity through move, resize, zoom and surface interaction.
- Modal focus/gesture ownership: dismissed or stale UI never delivers an action/input to the wrong pane; inspect the actual terminal surface, not only screenshots of synthetic state.
- Plugin action/hook execution, capability denial, disable during a run, event gap/restart and bounded cleanup of owned children.
- Dashboard row activation reaching exactly the selected task/pane; local scrolling not accidentally invoking provider actions.
- Two isolated fux/zor stacks over direct loopback endpoints: catalog reload, independent authorization failure, controller restart and viewer death preserving the other stack's processes, connection loss and recovery without duplicate effects. No koh or iroh-ssh installation, forwarding or network acceptance is required.
- Journal-first recovery: interrupt before dispatch, after dispatch and before reply; reconcile uncertainty without blindly repeating input, checks, launches or remote actions.
- Shutdown/resource limits: hot output, slow consumers and excessive/unauthenticated requests do not starve signals or grow queues without bound.
- Session restoration preserves supported shape/history and makes restore/skip decisions explicit; no claim of resurrection of the old OS process.

Use deterministic properties for relationship consistency, template/instance shape and transition invariants. Use isolated real processes for ownership, I/O and restart boundaries. Validate UI through the running terminal. Provider fixtures prove adapter contracts, not live-provider compatibility; state unavailable credentials/platforms precisely.

Measure release builds on identical hardware/settings against a recorded `main` baseline: startup, quiescent wakeups, active supervision wakeups, rendered input-to-visible latency, sustained output, many viewers and all-process memory. Keep raw samples and variability. Profile before changing the known per-cell allocation or rebuilding another hot path. Claim improvements only where the measurements support them.

## Review and handoff

At each completed integration boundary, review changed code against the relevant Bevy source patterns. Prioritize duplicate authority, wrong lifetime coupling, global state used for independent entities, blocking systems, observer ordering dependencies, replay bugs, cache invalidation and accidental full-scene rebuilds. Review the security boundary separately from ECS style.

Keep `docs/HANDOFF.md` and `docs/verification.md` current with exact revisions, commands/results, decisions, remaining work and evidence paths. Correct the stale capability ledger; do not leave two competing current-state summaries.

Final delivery must state:

- What now works end to end across fux and zor.
- Which custom mechanisms were replaced by Bevy features, and which justified exceptions remain.
- Exact tested revisions/binaries, behavioral evidence and benchmark results.
- Any implementation gaps, unavailable external acceptance and publication actions still requiring authorization.

Continue through actionable implementation, verification and documentation. Compilation, a milestone number or generated scaffolding is not completion. Judge completion against the fux/zor scope above; koh and future iroh-ssh integration are not blockers. If an external prerequisite prevents an in-scope acceptance scenario, finish all independent local work and report the precise boundary without labeling that scenario complete.
