# Ownership and recovery

fux and zor are separate Bevy applications. A client/viewer is not the authoritative World,
and a persisted scene is not a serialization of operating-system resources. See
[design.md](design.md) for scheduling, [protocol.md](protocol.md) for wire contracts,
[security.md](security.md) for authority, and [verification.md](verification.md) for actual
acceptance results.

## Resource owners

| Resource | Owner | What ending an observer/controller means |
|---|---|---|
| Task terminal process, PTY master, terminal emulator, input receipts and final evidence | fux pane and PTY adapter | Viewer exit and zor exit do not kill the task PTY. Explicit pane/workspace destruction or fux shutdown is different. |
| Shared workspace/template layout and pane identity | fux World | Viewers replicate an allowlisted projection; they cannot replace the authoritative graph. |
| Viewer camera, instance layout, focus/zoom, viewport, copy mode and surface scroll | Individual fux viewer/its server-side viewer entity | Detachment drops that view, not the shared task. One viewer's scroll is not a shared surface revision. |
| Task policy, attempts, operation intent, provider binding, check requirements, worktree intent | zor World and journal | Recovered records require live/final evidence; persistence alone is not proof of execution. |
| Provider sidecar/native protocol pipes | zor provider adapter | Owned child group is cancelled/reaped when the adapter ends. It is not the fux task PTY. |
| Check subprocess, capture and timeout | zor check adapter | Timeout/cancel/adapter teardown owns that subprocess group; task terminal ownership remains separate. |
| Plugin build/startup/action/hook/surface-driver process | zor plugin adapter | Disable/teardown cancels owned groups and revokes API grants. Linked source and durable plugin state are not disposable runtime handles. |
| Plugin terminal pane | fux, with plugin composition tracked by zor | Explicit plugin cleanup may close this owned pane; it is not authority to stop an unrelated task pane. |
| Dashboard surface/container and observation workers | zor dashboard, hosted in fux | Closing/detaching the dashboard is UI cleanup, never implicit task stop/cancel. |
| Machine credentials and desired bindings | Private machine catalog | Runtime machine entities are derived state, not a second persistent authority. |
| Remote action evidence | Private adjacent action-intent file | Restart exposes uncertainty rather than creating new remote requests. |
| HTTP connection, attachment stream, watch and their bounded queues | Their transport adapter and token-bound watch owner | Losing a connection does not prove whether a mutation reached its target. |

Source owners: [`fux model`](../crates/fux/src/model/components.rs),
[`PTY adapter`](../crates/fux/src/pty.rs),
[`zor runner`](../crates/zor/src/runner.rs),
[`provider adapter`](../crates/zor/src/providers/adapter.rs),
[`check runner`](../crates/zor/src/checks/runner.rs),
[`plugin host`](../crates/zor/src/plugins/host.rs),
[`dashboard`](../crates/zor/src/dashboard.rs).

## Identity is a condition, not a convenient label

A task name is insufficient to select a process. Remote task actions and attachments carry
or revalidate the selected machine/controller incarnation, task attempt and fux pane identity:
**fux instance + workspace + pane ID + PID when available**. PID alone can be reused; pane
IDs belong to an incarnation; a task can have multiple attempts over time. Catalog generation
and endpoint identity prevent an action prepared for an old machine binding from silently
following a replacement.

`zor attach TASK` resolves the live task attempt and uses exact attachment. fux checks the
optional PID as well as the pane; when the exact pane disappears the viewer detaches rather
than selecting another pane. This is not the same as attaching to a workspace and following
ordinary focus. A stale/offline/uncertain observation is not permission to target a replacement.

Surface ownership is similarly explicit. `surface.update` and `surface.close` require
`expected_provider`. Revisions increase monotonically; typed key/pointer input uses the
last-painted revision and `SurfaceInput` carries provider/provider-node/revision identity.
Cleanup first closes the owned surface, then uses the returned layout generation when
removing its container. A delayed close must not destroy a replacement provider or newer
layout. `surface.scroll` stays local to the addressed viewer.

Sources: [`machines/supervision.rs`](../crates/zor/src/machines/supervision.rs),
[`dashboard_cli.rs`](../crates/zor/src/cli/operations/dashboard_cli.rs),
[`wire.rs`](../crates/fux/src/wire.rs),
[`surface_methods.rs`](../crates/fux/src/remote/surface_methods.rs),
[`plugins.rs`](../crates/zor/src/plugins.rs).

## Durable intent and the crash boundary

zor separates intended policy from observed outcome. Its lifecycle records transitions before
releasing side-effecting work through adapters. After restart, an in-flight launch/prompt or
attempt awaiting evidence is uncertain; prepared launches do not spontaneously execute.
Reconciliation compares retained input/final evidence and live identities. It does not
interpret a screen string, dropped connection, or absent acknowledgement as proof of success
or proof that nothing ran.

There is no transaction spanning the workflow journal, fux, a provider, git and arbitrary
external commands. A process may perform its side effect and die before reporting it. Input
receipts establish submission state within their retention/incarnation boundary, not
exactly-once completion of the program consuming those bytes. Provider-native reports must
match their receipt/report token and session binding; terminal text is not a substitute.

Plugin events have a specific weaker-but-honest guarantee: the hook persists an
incarnation-scoped cursor claim before dispatching matching commands. A crash after claiming
can omit work, including later commands matching the same event. Reconnect skips the claim;
failed actions are not silently retried. This is **at-most-once claimed-event dispatch**, not
exactly-once external effects. A retained dispatch without completion remains uncertain.

Machine mutations likewise persist an operation intent before sending. Loading
`machines.json.action-intents.json` never schedules a worker mutation; submitting records
become uncertain. A retained operation key cannot be dispatched again. The catalog and
intent log, not `Machine` records in the workflow journal, own this durable state.

For destructive task verbs selected with `zor --machine NAME`, CLI exit status distinguishes
acknowledgement (`0`), failure (`1`), accepted/pending (`2`) and uncertainty (`3`). An accepted
request is not a completed side effect. Inspect `zor/machine.intents`/`zor/machine.status` and
remote task evidence before choosing an explicit next action; never wrap mutation commands
in a blind retry loop. See [protocol.md](protocol.md) for request schemas.

Sources: [`lifecycle/recovery.rs`](../crates/zor/src/lifecycle/recovery.rs),
[`input_ops.rs`](../crates/fux/src/input_ops.rs),
[`providers.rs`](../crates/zor/src/providers.rs),
[`plugins/hooks.rs`](../crates/zor/src/plugins/hooks.rs),
[`machines/intents.rs`](../crates/zor/src/machines/intents.rs),
[`cli/operations.rs`](../crates/zor/src/cli/operations.rs).

## Child ownership is explicit and bounded

Adapters signal only groups created for their own children and reap their leaders. Plugin
termination sends SIGTERM and escalates after a bounded grace; timeout/cancel paths for
checks terminate and reap their owned groups. Production provider/check/plugin execution uses
the separate `zor plugin supervise` guardian, which can detect host death and kill the group
even when the host received SIGKILL and its destructors cannot run. A direct embedded helper
without a guardian has only its in-process cleanup guarantee.

Signal authority ends before reaping releases the PID: helper cleanup uses an unreaped-exit
observation, and PTY signalling/reaping share a lock. A completion waiting on a full channel
does not retain permission to signal a dead child's recycled numeric group ID.

These mechanisms cannot undo external effects, survive every OS failure, or confine a
malicious child that escapes its process group. They do not imply that losing zor should
kill task PTYs: those belong to the still-running fux server. Conversely, a native provider
sidecar is not promised to survive zor death simply because its task's terminal survives.

## Persistent state is a recipe or evidence, never a live handle

- fux saves an allowlisted scene, launch attribution and bounded historical screen. No PTY,
  socket, viewer connection, minted token or running-process identity is resurrected. A new
  server has a new incarnation; restored panes receive fresh identities. `--restore ask`
  holds each pane for explicit restore/skip; `auto` launches the saved commands; `none`
  ignores the snapshot. See [installation.md](installation.md) before an update.
- zor's journal restores workflow/policy state, then reconciles against current evidence. It
  does not restore in-flight adapter handles or replay prompts. Enabled plugin intent may
  recreate plugin startup/hook processes with fresh grants; this is not recovery of an old
  action result and should be considered before restarting a controller.
- The machine catalog is read as validated desired state. Observations, workers and freshness
  are rebuilt. List/inspect/watch are credential-free; ADMIN-only `machine.endpoint` resolves
  the active credential-bearing binding.
- Owned worktrees persist intent before git mutation. Restored creation may be uncertain;
  restored removal does not replay `git worktree remove`. Reconcile inspects filesystem and
  registration evidence. Removal is guarded against unresolved checks and unfinished
  attempts using the checkout; `--force` is an explicit retained policy, not an implicit
  response to a dirty or uncertain tree.
- Plugin run/activation descriptors are private, unique authority snapshots. Restart does
  not upgrade an old child's path to a fresh token. Cleanup revokes retained grants against
  the expected server incarnation rather than treating a stale descriptor as current.

Sources: [`session.rs`](../crates/fux/src/session.rs),
[`journal.rs`](../crates/zor/src/journal.rs),
[`worktrees.rs`](../crates/zor/src/worktrees.rs),
[`plugins/loading.rs`](../crates/zor/src/plugins/loading.rs).

## Operator consequence

Detach a viewer to leave tasks running. Close a dashboard to remove only its UI and
observations. Use an explicit, freshly guarded stop/cancel operation when stopping a task is
intended. Treat fux restart as loss of live terminal processes, and zor restart as loss of
controller-owned sidecars/checks/plugin children with potentially uncertain external work.
Back up state privately, discover the running protocol, inspect uncertainty, and decide
restore/resume deliberately. No document here implies verified support for an untested
platform/provider, a future iroh-ssh path, or koh in the active acceptance gate.
