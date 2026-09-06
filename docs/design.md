# Architecture

fux 0.4 is a minimal persistent terminal multiplexer. One session server per user owns the
authoritative model in a standalone `bevy_ecs` 0.19.1 World; viewers are separate processes that
paint per-viewer frames with a small ratatui-core compositor. Koh (remote access) and zor
(observation) are independent programs that speak fux's versioned local protocols.

## Ownership

| Project | Responsibility | Boundary |
|---|---|---|
| fux | PTYs and process groups, terminal emulation and bounded history, workspaces/tabs/splits, viewers, commands, configuration | attachment protocol v6 and control protocol `FUXCTL2` over private Unix sockets |
| koh | identities, authorization, encryption, discovery, relays, reconnect | authenticated gateway carrying the opaque attachment stream to a private local socket |
| zor | agent detection, rules, state machine, presentation | `zor observe` consuming `list`/`capture` over the control socket |

## Processes and sockets

`fux` (viewer) resolves a workspace through the manager socket, starting `fux serve --daemon` when
no server owns the runtime directory. Election is one transaction under a bind lock; a private
nonce-named readiness channel tells the starting viewer when the server is up; the client startup
lock is released as soon as the manager is elected.

| Path | Purpose |
|---|---|
| `RUNTIME/fux/manager.sock` | list/resolve/kill workspaces (preface `FUXCTL2`) |
| `RUNTIME/fux/NAME.attach.sock` | attachment protocol v6: viewers and koh gateways |
| `RUNTIME/fux/NAME.sock` | control protocol: CLI, scripts, zor |
| `RUNTIME/fux/workspaces/NAME.json` | descriptor: pid, instance nonce, socket path, protocol version |

Source layout: `ecs/` (model and systems), `server/` (owner loop, adapters, connections),
`os/pty.rs` (PTY and process-group ownership), `daemon/` (paths, descriptors, election, manager
RPC), `proto/` (attachment, control, socket authentication), `client/` (viewer), plus
`layout.rs`, `terminal.rs`, `view.rs`, `commands.rs`, `config.rs`, `ids.rs`.

## Entity, component and resource model

Entities exist for things with identity and lifetime. Cells, history rows, bytes, keypresses and
configuration are plain data inside components and resources.

| Entity | Components | Owning edges |
|---|---|---|
| Workspace | `Workspace { name, selection (default tab/focus for control clients and new attachments), retiring, open }`, `Tabs` (relationship target: member tabs in order, kept by bevy from each tab's `TabOf`) | owns its tabs; retirement despawns tabs and panes and detaches viewers with `exited` (explicit, no `linked_spawn` cascade) |
| Tab | `Tab { id: TabId, workspace, label, layout: LayoutTree<Entity>, area, layout_changed }`, `TabOf(workspace)` once its first pane is live (a reserved tab has none) | owns its panes; closing terminates them |
| Pane | `Pane { id: PaneId, tab, state, terminal: ServerTerminal, geometry, dirty }`, `Creation { kind, requester }` while reserved | none |
| Viewer | `Viewer { id: ViewerId, workspace, rows, cols, selection (own tab + focus per tab), queue, barrier, layout generation and rects, dirty, detaching }`, `Selection` for pending viewer history reads | non-owning reference to its workspace; despawning a viewer never touches panes |

`PaneState` is `Starting → Live{pid} → Eof{pid} → Exited{code}`, with `Terminating{pid, since}`
entered from `Live`/`Eof` by a confirmed close, kill or shutdown. `Starting` panes are never in a
layout or a frame. A tab may close before its panes' exit reports arrive; such a pane may outlive
its tab only while `Terminating` and is released when the exit arrives (or by adapter shutdown).

Resources: `Limits` (workspaces 64, tabs 32, panes 128, viewers 64, scrollback rows, viewer
queue 256, retirement grace 5 s, terminate deadline 10 s, `pane.output` interval 250 ms), `Ids`
(monotonic `PaneId`/`TabId`/`ViewerId` counters and id→Entity maps; ids are never reused during a
server lifetime and descriptors carry an instance nonce), `Clock` (step time injected by the
owner loop), `Deadlines` (next wake proposed by systems), `Registry` (bindings and default
command), `ShuttingDown`, `WorkspaceCounter`, and `Messages<Inbound>`/`Messages<Effect>` for the
current step only.

Entity handles never leave the server. Every command resolves a public id at execution time and
checks kind, workspace membership and liveness; destructive confirmations carry the original
`PaneId`/`TabId`, so a stale id fails with `not-found` even when a replacement exists.

## One step

The owner loop (`server::run_loop`) sleeps until a channel has data, a socket event arrives, a
signal fires or the next deadline expires. It then collects a bounded batch (at most 64 chunks
of up to 64 KiB from the shared 256-deep pane channel and 256 requests from the ingress channel
per step; signals are polled between busy steps), runs the `Step` schedule once with the
`SingleThreadedExecutor`, applies the returned effects through the adapters and goes back to sleep.
A pty hands out output in reads of a few bytes, so a batch that is only pane output, arriving
within 3 ms of the previous output step and not within 3 ms of any viewer input, is treated as a
stream: the loop waits 1 ms for more chunks before stepping (a 20 000-line burst then costs a few
hundred steps instead of twelve thousand). Input, requests and echoes never wait. There is no
periodic tick.

Phases are chained system sets; deferred mutations become visible at the sync points between them:

1. **Ingest**: record the clock, spawn/despawn viewer entities, queue viewer requests.
2. **Output**: feed pane bytes into each pane's emulator in per-pane FIFO order, collect host
   replies as `WriteInput` effects, apply EOF and exit reports after every earlier chunk.
3. **Requests**: drain each viewer's queue in order until it reaches a barrier, apply control and
   manager requests, validate ids/liveness/limits, reserve `Starting` panes for creations and
   emit `SpawnPane`.
4. **Completions**: apply `SpawnCompleted`: place the pane, move the requester's focus, release the
   barrier and drain the queue again, or roll the reservation back and reply `failed`.
5. **Lifecycle**: natural exits (close the pane, close an emptied tab, retire the last pane's
   workspace with its status), confirmed closes and kills, retirement grace, workspace kills,
   shutdown, idle detection.
6. **Layout**: recompute geometry for tabs whose layout or displaying viewers changed, over the
   smallest viewer showing the tab (hidden tabs keep their last area; viewer-less tabs use 80×24).
   The last row of every viewer is the bar, so the pane area is `rows - 1` starting at row 0; siblings in a split are
   separated by exactly one cell, and leaf rectangles are the panes' content areas. Emulators are
   resized and `ResizePty` emitted.
7. **Snapshot**: `refresh_grids` decides which viewers publish this step and reads the changed
   panes they show once into the panes' retained grids (a copy of the visible cells with the
   step each row last changed in); output-driven frames are paced to one per 8 ms per viewer
   (`Limits.frame_interval_ms`, a deadline wakes the loop for the pending rows), while a frame
   that follows the viewer's own input (its echo), carries a reply, a selection change or a
   retirement goes out at once. `publish_frames`
   then builds one update per publishing viewer from the grids: each visible pane is carried
   only if it changed since the viewer's previous update, and then only its changed rows (in
   full when the viewer holds nothing of it or its size changed). Every viewer remembers what it
   holds of each pane (`Viewer.sent`); updates are queued before the replies they promise. Cells
   the frame cannot carry (zero-width or multi-grapheme sequences, control characters) are shown
   as blanks of their style.
8. **Publish**: control events, deadlines, message clearing, `clear_trackers`.

No observers or component hooks drive core commands; process cleanup is explicit.

### Systems

Ingest (`apply_attachments`), output (`apply_pane_output`), layout (`resolve_layout`), snapshot
(`publish_frames`) and publish (`finish_step`) are typed systems over `Query`, `Res`,
`MessageReader` and `Commands`, with `SystemParam` bundles for what they share: `Step` (clock,
limits, ids), `Effects` (effect and event writer), `ViewerExit` (deferred viewer despawn),
`Arrivals`, `Scene`. Viewer queues are drained by `drain_viewer_queues` scheduled after the request
phase and again after completions, not by tail calls. Four systems keep `&mut World` because each
mutates entities it must observe again within the same phase: `apply_requests` (a request may
spawn reservations, edit layouts or despawn tabs that the next request in the batch addresses),
`drain_viewer_queues` (a request may despawn the viewer whose queue is being drained),
`apply_spawn_completions` (a completion inserts the `TabOf` membership and places a pane that a
later completion in the batch splits) and `resolve_lifecycle` (closing a tab may retire the
workspace, which finalizes in the same pass). Shared mutations live in `ecs::support` (viewer
scans, cascades, retirement, replies); dirty flags stay explicit because change ticks would fire
on `get_mut` reads such as history views and captures.

## Ordering guarantees

- Each pane's reader thread sends output, then EOF, then exit in order on the shared bounded pane
  channel; each connection sends its requests in order on the ingress channel. Ingest takes a
  bounded prefix of each channel per step, so a hot pane cannot starve input, timers, exits or
  signals (a saturated pane channel applies backpressure to that pane's reader).
- Requests from one connection apply in order within a step. A split, new tab or new workspace
  sets the viewer's barrier; later requests wait for the completion (success or failure), so
  input meant for the new pane reaches it only after it exists and is focused.
- A failed creation despawns the reserved pane (never seen in a frame or listing) and replies
  `failed`; its id is not reused.
- Frame updates are queued before replies; the outbox merges consecutive updates for a viewer
  (later rows replace earlier rows, later metadata wins, untouched panes stay, so applying the
  merged update equals applying both) but never lets an update overtake a reply queued after an
  earlier update. A viewer 64 messages behind is disconnected.
- `detach` applies preceding input and drops the suffix; `workspace select` retargets the same
  connection so the suffix reaches the destination.
- Completions and process reports carry public ids; a despawned or replaced target is ignored,
  except that a successful completion for a released reservation stops and reaps the process.

## Process lifecycle

Spawns run on blocking tasks and complete as `SpawnCompleted`. Each live pane has a reader thread
(PTY output → channel → `PaneEof` → polled reap under the gate → `PaneExited`) and a writer thread. EOF alone never
retires a pane: the exit status is observed first, and an exit that arrives before or with the
spawn completion is kept (the completion places an already exited pane, which closes at once).
Termination sends SIGHUP to the process group, SIGKILL after one second, then reaps; the reader
reaps by polling under a counted gate that terminations hold, so the group id cannot be recycled
before the SIGKILL. Releasing a pane whose process still runs (workspace kill, finalize) uses the
same grace. A completion for a reservation released meanwhile is stopped and reaped as well. Server shutdown moves every workspace to retiring, sends final frames and
`exited`, then the adapter terminates and joins everything before the process exits (five-second
deadline). Persistence is surviving viewer loss; nothing is resurrected after a restart.

## Viewer

The viewer holds private state as explicit Rust state machines: a byte-exact prefix filter
(literal prefix on double press, immediate popup, unknown keys stay in command mode, 35 ms Escape
disambiguation, paste and fragmented sequences preserved), a controller with modes (copy,
workspace and tab choosers, rename, new workspace, confirmed close of a specific pane or tab,
resize), a copy session over private `view` reads, a retained frame that applies the server's
updates (a delta replaces the carried rows of the panes it names; a full update replaces
everything), and a compositor that paints the bottom bar on its own background (workspace,
tabs with the current one reversed, the focused pane's `id: title` or a two-second notice), the
panes, the shared separators derived from the one-cell gaps between leaves (bold next to the
focused pane) and the command column (a bottom-right box above the bar, as wide as its widest
line, scrolling row by row with `▲/▼ n more` indicators only when the rows do not fit; choosers,
prompts and confirmations share it), then a final frame before restoring the terminal. Colours come from
the `[style]` table with muted defaults.
Mouse events are hit-tested against the layout generation the viewer painted; wheel and drag
browse or select locally unless the pane's application owns the mouse, and Shift forces the local
behaviour. Each viewer has its own active tab, focus, history position, selection and clipboard.

## Verification

`tests/ecs.rs` drives `Session::step` with injected events and time (no sockets, sleeps or
processes) and checks World invariants after every step, including a randomized command-sequence
test with stale ids, delayed and failed completions, viewer churn and time skips.
`tests/structure.rs` pins architectural invariants (spawn owners, bounded channels, ECS as the
only authority, CI surfaces). Real adapters are exercised by `tests/local_cli.rs` and the
fixture-child binary suite. See [ecs-acceptance.md](ecs-acceptance.md) for evidence.

## History

- 0.1 (2026-09-03): a wrapper around koh-hosted sessions with a zor sidecar per pane; networking,
  identities and agent detection lived inside fux.
- 0.2 (2026-09-04): the standalone host and router with local sockets, popup panes, contextual
  help panels and sidecar supervision; koh and zor became independent programs.
- 0.3 (2026-09-05): this design, a complete rewrite on `bevy_ecs` with the model, order and
  lifecycle described above; 0.3.1–0.3.3 replaced the pane boxes with the bar, separators and the
  command column.
- 0.4 (2026-09-06): the same design made idiomatic: typed systems, the tab membership
  relationship, shared helpers, dead code and the historical prompts and audits removed (they
  remain in git history).
