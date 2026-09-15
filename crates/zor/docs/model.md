# zor model

The authoritative zor state is one `bevy_ecs` World (`crates/zor/src/model`). Sources of truth: the old zor
markdown contracts (`TASKS.md`, `CHECKS.md`, `SOURCES.md`, `ARTIFACTS.md`, `RESULTS.md`, `GROUPS.md`,
`WORKTREES.md`, `RECOVERY.md`, `SERVICE-API.md`, `OBSERVATION-CONTRACT.md`, `INTEGRATIONS.md`,
`REMOTE.md`, `DASHBOARD.md`; `docs/zor-lifecycle-transitions.md`, `docs/service-ownership-contract.md`,
`docs/multi-machine-supervision.md` in the old repo) and prompt section 4. Line references below are into
those files. Later milestone-6 slices implement the numbered rules; this document names them.

## Entity kinds

| kind | id (immutable, hook-indexed in `Ids`) | marker | notes |
|---|---|---|---|
| Task | `TaskId(String)` caller-chosen | `Task` | logical work; TASKS.md:66 |
| Attempt | `AttemptId(u64)` allocated | `Attempt` | one task ↔ one session/pane; TASKS.md:66-67 |
| Prompt | `PromptId(String)` = its operation id | `Prompt` | prepared prompt operation; TASKS.md:176 |
| Operation | `OperationId(String)` caller-chosen | `Operation` | durable intent: launch, stop, resume, handoff, group step |
| Check | `CheckId(String)` execution id | `Check` | one execution; CHECKS.md:17 |
| Result | `ResultId(u64)` allocated | `CheckResult` | evidence of a finished/uncertain check |
| Source | `SourceId(String)` | `Source` | retained tree snapshot; SOURCES.md:10 |
| Artifact | `ArtifactId(String)` | `Artifact` | retained bytes; ARTIFACTS.md:10-12 |
| Group | `GroupId(String)` | `Group` | GROUPS.md:102 |
| Worktree | `WorktreeId(String)` | `Worktree` | WORKTREES.md:35 |
| Machine | `MachineId(String)` stable | `Machine` | multi-machine-supervision.md:50 |
| Service | `ServiceId(String)` (fux server name) | `Service` | a fux instance zor talks to |
| ObservedAgent | `ObservedAgentId(u64)` allocated | `ObservedAgent` | passive observation of one pane process |
| Plugin | `PluginId(String)` | `HostedPlugin` | plugin host, milestone 7 |
| PluginAction | `PluginActionId(String)` | `PluginAction` | one manifest action |

All string ids are `1..=64` ASCII `[A-Za-z0-9_-]` (TASKS.md:169, `model::ids::valid_id`). `PromptId` and
`OperationId` share the caller's operation namespace: a typed transition refuses an id present in either map.

## Components by mutation owner

- **Identity (immutable, `Requests` set at creation):** every id above; `Title`, `Location {Cwd|Worktree}`,
  `CreatedMs`, `ClosedMs` on Task; `Ownership {Adopted|Managed}`, `PaneHandle {instance, workspace, stream,
  pane, pid}` (the exact-process identity of service-ownership-contract.md:19-24), `LaunchMarker` on
  Attempt; `PromptText`, `Deadline`, `ReportToken` on Prompt; `OperationKind` on Operation; `CheckCommand
  {argv, timeout_ms}`, `Requirement(name)`, `CreatedGeneration` on Check; `SourceRevision` on Source;
  `ArtifactPath`, `Requirement` on Artifact; `Concurrency`, `After` on Group members; `Repo/Branch/Base`
  on Worktree; `MachineName`, `ControlBinding` on Machine; `PluginManifest` on Plugin.
- **Lifecycle set (`Phase::Lifecycle`):** `TaskState`, `AttemptState`, `Delivery`, `WaitState`,
  `OperationPhase`, `CheckState`, `WorktreeState`, `ArtifactState`, `GroupIntent`, the markers
  `StopRequested`, `Uncertain`, `Lost`, `NeedsInput`, `ArmInputStarted`, `Seal`, `Verdict` on Result.
- **Completions set (`Phase::Completions`, adapter results):** `Receipt`, `FinalEvidence`,
  `ResponseEvent`, `Binding`, `Observation`, `ProducerLifetime`, `OutputTail`.
- **Projection set (`Phase::Projection`, read-only for BRP):** `TaskView`, `AgentView`, `MachineView`,
  `CheckView` on projection entities (`Mirrors` → model entity), never on model entities.
- Relationship reverse sides are indexes, never written directly (prompt 3.2).

## Relationships (relationship → target index)

`AttemptOf(task)` → `Attempts`; `PromptOf(attempt)` → `Prompts`; `OperationOf(attempt)` → `Operations`;
`ArtifactOf(attempt)` → `Artifacts`; `CheckOf(task)` → `TaskChecks`; `CheckOn(source)` → `Checks`
(optional, CHECKS.md:82); `SourceOf(task)` → `Sources`; `ResultOf(check)` → `Results`;
`MemberOf(group)` → `Members` (members are Operation entities, GROUPS.md:102); `OwnedWorktree(task)` →
`Worktrees`; `Bound(machine)` → `BoundAgents` (ObservedAgent and Service); `ActionOf(plugin)` → `Actions`.
`After(Vec<Entity>)` (`#[entities]`) on a member names prerequisite members of the same group.

## State machines

Per-entity lifecycles are enum components with a transition table (`TaskState::may_become` etc.);
`bevy_state` holds the app-wide `ServerMode {Starting, Serving, ShuttingDown}` only, because
`bevy_state::States` is a World-global resource, not a per-entity value.

- **Task** `Open | Running | Blocked | Closed { outcome: Cancelled | Verified }`.
  Open→Running (attempt Live), Running↔Blocked (`NeedsInput`), Running|Blocked→Open (attempt finished;
  TASKS.md:136-137 "task remains open after closure"), any non-Closed→Closed{Cancelled} (TASKS.md:394),
  any non-Closed→Closed{Verified} only through `task verify` (TASKS.md:189, RESULTS.md:70). Closed is
  terminal; Closed{Verified} is never overwritten by cancellation (TASKS.md:398-399, RESULTS.md:72-74).
- **Attempt** `Pending | Launching | Live | Finishing | Finished`, side markers `Uncertain`, `Lost`,
  `NeedsInput`. Pending (intent committed, creation not sent; lifecycle-transitions.md:16)→Launching
  (creation sent; Submitting/Uncertain of RECOVERY.md:47)→Live (reconciled unique pane, TASKS.md:130-133)
  →Finishing (stop requested, TASKS.md:153-157)→Finished (final evidence, TASKS.md:134). Launching→Finished
  when the pane closed before reconciliation. `Lost` marks Live/Finishing whose endpoint proved replaced
  (RECOVERY.md:98-102) and persists through outages; `Uncertain` marks missing live/final evidence
  (TASKS.md:145-146). Finished is terminal.
- **Prompt delivery** `Prepared | Reserved | Submitting | Delivered | Failed | Uncertain | Released`
  (TASKS.md:176-217, 387); **wait** `Pending | NeedsInput | ResponseObserved | ProcessExited | TimedOut |
  Cancelled | Uncertain` (TASKS.md:177-345); terminal wait outcomes are never erased (TASKS.md:341-342).
- **Operation phase** `Prepared | Submitting | Attached | Closed | Uncertain` (lifecycle-transitions.md:16).
- **Check** `Queued | Running | Passed | Failed | Uncertain`. Queued→Running→Passed|Failed (leader exit,
  CHECKS.md:27); Queued|Running→Uncertain (recovery, spawn/timeout/output errors, CHECKS.md:21-24).
  Passed/Failed/Uncertain are terminal: the command never runs again under the same id (CHECKS.md:18-19).
- **Worktree** `Allocating | Prepared | Creating | Ready | Uncertain | Removing | Removed`
  (WORKTREES.md:44-54, 110-114). **Group intent** `Manual | Automatic | Paused | Cancelled | Complete`
  (GROUPS.md:111-115). **Observation** `Unknown | Working | Blocked | Idle | None`
  (OBSERVATION-CONTRACT.md:171-177; unmatched is `Unknown`, never `Idle`).

## Invariants

Structural rules (S) are checked by `model::invariants::check_invariants`; temporal rules (T) are the
transitions' preconditions, owned by later slices. Line references name the contract sentence.

1. (S) `Ids` maps equal the live id components per kind; ids are syntactically valid. TASKS.md:169-171
2. (S) Every relationship points at an entity of the expected kind (`AttemptOf`→Task, `PromptOf`→Attempt,
   `OperationOf`→Attempt, `ArtifactOf`→Attempt, `CheckOf`→Task, `SourceOf`→Task, `CheckOn`→Source,
   `ResultOf`→Check, `MemberOf`→Group, `OwnedWorktree`→Task, `Bound`→Machine, `ActionOf`→Plugin), and
   every Attempt/Prompt/Operation/Artifact/Check/Source/Result/Worktree/PluginAction has its relationship.
3. (S) A task has at most one attempt outside `Finished`; a managed session belongs to exactly one attempt.
   RECOVERY.md:141-143, lifecycle-transitions.md:17
4. (S) `Closed{Verified}` ⇔ `Seal` present; the seal names one `Source` of that task, only `Passed` checks
   of that task on that source, and only artifacts captured by those checks. RESULTS.md:61-70, TASKS.md:189
5. (S) `AttemptState::Finished` ⇒ `FinalEvidence` present; `Lost` never on `Finished`. RECOVERY.md:33-35,
   TASKS.md:147
6. (S) At most one prompt in `Prepared|Reserved|Submitting|Uncertain` per pane identity, across tasks.
   TASKS.md:184-185
7. (S) `Prepared` prompts carry no `Receipt`; `Reserved|Submitting|Delivered` carry one; a `ResponseEvent`
   or `Binding` requires a `Receipt`. TASKS.md:176, 219, 254, 277-280
8. (S) `Passed|Failed` checks have at least one Result; `Queued|Running` checks have none; a Result's
   `Verdict` agrees with its check's state. CHECKS.md:27-28
9. (S) A `Removing|Removed` worktree has no `Queued|Running|Uncertain` check of its task and no attempt
   of its task outside `Finished`. CHECKS.md:72-74, WORKTREES.md:80-82, RECOVERY.md:26-28
10. (S) Group: 1..=8 members, `Concurrency` in 1..=members, members are distinct operations, `After`
    exists only on members and names members of the same group, no cycles. GROUPS.md:102-105
11. (S) Counts never exceed `Limits` (128 tasks, 512 attempts, 1024 prompts, 128 checks, 128 sources,
    128 artifacts, 32 groups, 128 worktrees, 32 machines). TASKS.md:444-447, CHECKS.md:68, GROUPS.md:102
12. (S) `StopRequested` only on tasks with a `Managed` attempt. RECOVERY.md:3-6, TASKS.md:153-157
13. (T) Intent is committed to the journal before any external effect; a Submitting/Uncertain operation is
    never resent; retry by the same operation id. lifecycle-transitions.md:3-7,16; TASKS.md:122,129-130
14. (T) Identical retry under an id returns the original record without a commit; conflicting intent under
    the same id fails. TASKS.md:169-171, CHECKS.md:17-18, ARTIFACTS.md:15-17, GROUPS.md:105-107
15. (T) No command silently allocates a replacement pane, session, prompt or worktree; lost replies leave
    `Uncertain`, resolved only by reconciliation or explicit abandon. TASKS.md:211-217, RECOVERY.md:104-107
16. (T) Every destructive fux call (stop, close, input) validates instance/stream/pane/pid first; transport
    reconnection never authorises replay. service-ownership-contract.md:22,45-49; TASKS.md:71-73
17. (T) Adopted attempts grant no termination, cleanup or check authority. TASKS.md:68-69, CHECKS.md:13-14
18. (T) Cancellation releases coordination only: never signals, never retracts delivered bytes, leaves
    session/attempt intact, refuses new preparation. TASKS.md:394-399, GROUPS.md:87-92
19. (T) Stop success requires matching final evidence (`phase: closed`), not kill acceptance.
    TASKS.md:157-162, lifecycle-transitions.md:22
20. (T) Delivery, agent reaction, wait outcome and task outcome are separate facts; no observation, exit or
    receipt makes a task verified. TASKS.md:187-189, RESULTS.md:51-52, GROUPS.md:26, INTEGRATIONS.md:4-5
21. (T) Requirement status is the latest submission by `created_generation`; a newer pending/uncertain/failed
    execution supersedes an older pass. CHECKS.md:40-43, RESULTS.md:16-18
22. (T) Check and artifact policies seal at the first check submission / first retained artifact; no
    removal or replacement afterwards. CHECKS.md:35-37, ARTIFACTS.md:22-25
23. (T) After sealing, new evidence and cancellation are refused; `stop` still closes the pane and keeps
    `Verified`. RESULTS.md:71-74
24. (T) Group admission commits admitted+cursor before submitting; capacity is released only by
    verification plus the original prompt's Delivered receipt; `--after` needs a retained seal.
    GROUPS.md:22-23,61-73; lifecycle-transitions.md:18
25. (T) Worktree creation and removal are intent-before-git; `git worktree add/remove` never re-run under
    the same id; `Removed` only when both path and registration are absent. WORKTREES.md:44-54,110-115
26. (T) Passive observation grants no authority and never satisfies a wait; unmatched screens are `Unknown`;
    native evidence takes precedence and missing/stale/mismatched claims become `Unknown`.
    OBSERVATION-CONTRACT.md:78-81,171-184; DASHBOARD.md:35-38
27. (T) Startup recovery: retained `submitted` checks become `Uncertain` in one transaction; abandoned
    launches are swept once; no launch is created and no prompt replayed. RECOVERY.md:20-21,45-48,64-71
28. (T) Attempt.state is written only by the shared attached-observation transition; only authenticated
    final evidence publishes `Finished`. lifecycle-transitions.md:55-60

## Receipts, uncertain, seals

- **Receipt** = fux's input-operation record `{operation, pane, state, bytes_written, seq, expires_ms}`
  (`fux/input.*`), retained for the reservation window and scoped to the fux instance nonce
  (TASKS.md:219-220). `Delivered` proves bytes reached the PTY writer, nothing about the agent
  (TASKS.md:221-222, service-ownership-contract.md:57-60). A report or binding needs a matching receipt
  and matching `PaneHandle` (TASKS.md:254, 322-326). Receipt refreshes never erase terminal wait outcomes.
- **Uncertain** is a retained side state, never an absence: lost submit reply, expired or missing receipt,
  human input after submission, observation errors, missing final evidence, producer retirement with an
  unresolved arm (TASKS.md:212-217,324,336-339; INTEGRATIONS.md:88-91). Uncertain prompts keep writer
  exclusion until `abandon`; uncertain checks block worktree removal and cannot satisfy verification
  (RECOVERY.md:26-31). The journal and archive never drop an uncertain operation or an unresolved check.
- **Verification seal** (`Seal {source, checks, artifacts, sealed_ms, generation}`) is written by
  `zor/task.verify` only, atomically with `Closed{Verified}` (RESULTS.md:69-70); revalidated on every
  journal load (RESULTS.md:71; invariant 4); scope string
  `declared-checks-and-artifacts-for-retained-source` (RESULTS.md:76).

## Journal and archive

The journal is the reflected snapshot of the allowlisted subgraph (Task, Attempt, Prompt, Operation, Check,
Result, Source, Artifact, Group, Worktree, Machine and their relationships) at
`<state_dir>/zor/journal.scn.ron`, bounded at extraction (4 MiB serialized, counts of invariant 11), written
atomically (temp + fsync + rename + directory sync; TASKS.md:427-433). Over-bound extraction refuses the
write and keeps the previous generation. Restore parses into an inert World, validates the vocabulary and
`check_invariants`, then rebuilds with fresh entities. Closed tasks older than `archive_after` move with
their subgraph into `<state_dir>/zor/archive/<date>.scn.ron` (read-only, `zor/task.inspect`); a task is
not archivable while any of its operations is `Uncertain` or any of its checks is unresolved.
