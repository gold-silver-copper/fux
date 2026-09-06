# ECS rewrite plan

Written 2026-09-05 before implementation, as required by
[bevy-ecs-multiplexer-prompt.md](../bevy-ecs-multiplexer-prompt.md). This is the design record;
[ecs-acceptance.md](ecs-acceptance.md) records what was built and verified.

## Dependency selection

`bevy_ecs = "=0.19.1"` with `default-features = false, features = ["std"]`.

- Documentation for the pinned release: <https://docs.rs/bevy_ecs/0.19.1/bevy_ecs/> and
  <https://docs.rs/bevy_ecs/0.19.1/bevy_ecs/schedule/>. APIs were checked against the crate
  source in the local registry, not against examples for other releases.
- Disabled features: `bevy_reflect` (no runtime reflection is exposed or needed),
  `async_executor`/`multi_threaded` (systems run on one logical writer with the
  `SingleThreadedExecutor`), `backtrace`, `trace`, `serialize`, `hotpatching`.
- The minimal set pulls 56 transitive crates (bevy_ecs, bevy_ecs_macros, bevy_platform, bevy_ptr,
  bevy_tasks, bevy_utils, slotmap, indexmap, smallvec, fixedbitset, concurrent-queue, bumpalo,
  derive_more, thiserror, ...). This is larger than the old host's zero-ECS graph; the acceptance
  audit records the measured build and dependency counts instead of claiming a smaller graph.
- MSRV moves from 1.91 to 1.95, the floor bevy_ecs 0.19.1 declares. The CI MSRV job follows.
- Retained: `portable-pty` (PTY/process ownership), `vt100` (terminal emulation and history),
  `ratatui-core` (viewer compositor buffer), `termina` (raw mode/platform terminal), `tokio`
  (owner loop wake-ups, sockets, signals), `nix` (peer credentials, signals, flock),
  `serde`/`serde_json`/`toml`, `clap`, `base64`, `unicode-*`, `tracing`.
- Removed: `tokio-util` (no cancellation-token web remains), `loom` (the compact concurrency
  models described the old host's locks). `proptest` stays for randomized command-sequence tests.

## Process boundary

One session server per runtime directory (`fux serve`, auto-started by the first `fux`
viewer). The server owns one `bevy_ecs::World` holding every workspace, tab, pane and attached
viewer. Viewer processes (`fux`, `fux NAME`, `fux attach --socket`) hold private UI state as
explicit Rust state machines and never receive the World: they receive per-viewer frames.

Sockets stay per workspace so koh gateways and zor observers keep fixed targets:

| Path | Purpose |
|---|---|
| `RUNTIME/fux/manager.sock` | list/resolve/kill workspaces (control preface `FUXCTL2`) |
| `RUNTIME/fux/NAME.attach.sock` | attachment protocol v3 (viewers, koh gateway) |
| `RUNTIME/fux/NAME.sock` | control protocol `FUXCTL2` (CLI, zor observe) |
| `RUNTIME/fux/workspaces/NAME.json` | descriptor: pid, instance nonce, socket, protocol |

## Entity, component and resource model

Entities exist for things with identity and lifetime. Terminal cells, history lines, bytes,
keypresses and configuration are plain data inside components/resources.

| Entity | Components | Owning edges |
|---|---|---|
| Workspace | `Workspace { name }`, `TabOrder(Vec<Entity>)`, `WorkspaceSelection { tab, focus: BTreeMap<tab, pane> }` (default for new attachments and control clients), `Retiring { since, exit_code }` (marker) | owns its tabs; despawn cascades to tabs, panes, and detaches viewers with a final `exited` message |
| Tab | `Tab { id: TabId, workspace: Entity }`, `TabLabel(String)`, `Layout(LayoutTree)` (compact typed BSP tree whose leaves are pane entities) | owns its panes; despawn cascades to panes |
| Pane | `Pane { id: PaneId, tab: Entity }`, `PaneCommand { argv, cwd }`, `PaneState` (Starting / Live { pid } / Exited { code } / Terminating), `Terminal(ServerTerminal)` (vt100 parser, bounded history, title, progress, clipboard, host replies), `PaneGeometry(Rect)`, `PaneDirty` marker | none; a pane never owns other entities |
| Viewer | `Viewer { id: ViewerId, workspace: Entity }`, `ViewerSize { rows, cols }`, `ViewerSelection { tab, focus: BTreeMap<tab, pane> }`, `ViewerQueue(VecDeque<ViewerRequest>)`, `ViewerBarrier(Option<PendingCreation>)`, `ViewerLayout { generation, rects }` (last published layout for mouse hit tests), `ViewerDirty` marker | non-owning reference to its workspace; despawning a viewer never touches panes |

Viewer UI state (prefix/menu, confirmation target, text entry, copy selection, history
position, deadlines) lives in the viewer process (`client::controller`), as the spec allows.
The server holds only what is needed for authority: which tab/pane each viewer looks at.

Resources (all small):

| Resource | Content |
|---|---|
| `Limits` | max workspaces 64, tabs/workspace 32, panes/workspace 128, viewers/workspace 64, scrollback lines (default 10 000, cap 100 000), viewer queue depth 256, outbox depth 64, control subscriber queue 1024, max pane dimension 512 |
| `Ids` | monotonic `PaneId`/`TabId`/`ViewerId` counters and `BTreeMap<id, Entity>` lookups; ids are never recycled within a server lifetime, and descriptors carry an instance nonce so restarts are distinguishable |
| `Clock` | current step time in milliseconds, injected by the owner loop (tests inject values) |
| `Deadlines` | next wake time computed by systems (retirement grace, forced-kill escalation) |
| `Registry` | configured prefix, bindings and default command (from `Config`) |
| `Subscribers` | control-event subscriptions with bounded queues |
| `Messages<...>` | per-step typed inbound events and outbound effects (see below) |

`Ids` maps protocol identities to `Entity`; Entity handles never leave the server. Every command
resolves its public id at execution time and checks kind, workspace membership and liveness.
Destructive confirmations from viewers carry the original `PaneId`/`TabId`; a stale id fails
with `not-found` even if a replacement object exists.

## Typed messages and effects

Inbound (written by the owner loop's ingest step, read by domain systems in the same step,
cleared at the end of the step so bevy message storage never becomes a transport queue):

- `PaneOutput { pane: Entity, bytes }`, `PaneEof { pane }`, `PaneExited { pane, code }`
- `SpawnCompleted { pane, result: Ok { pid } | Err(String) }`
- `ViewerAttached { viewer, workspace, rows, cols, outbox }`, `ViewerGone { viewer }`
- `ViewerRequest { viewer, request }` (input bytes, mouse, control request, view read, resize, detach)
- `ControlRequest { workspace, request, reply: ReplyToken }`
- `ManagerRequest { request, reply }`
- `Tick` (deadline expired)

Outbound effects (written by systems, drained by the owner loop's adapter after the step):

- `SpawnPane { pane, argv, cwd, rows, cols }`, `WriteInput { pane, bytes }`,
  `ResizePty { pane, rows, cols }`, `Terminate { pane, force }`, `ReleasePane { pane }`
- `SendToViewer { viewer, message }`, `CloseViewer { viewer }`
- `ControlReply { token, reply }`, `Event(control::Event)`
- `WorkspaceCreated { workspace, name }`, `WorkspaceRetired { name }`, `ServerIdle`

Adapters own the OS handles (PTY masters, writer channels, sockets, child processes) in a map
keyed by `Entity`; handles never enter the World.

## System order

One `Schedule` labelled `Step`, `SingleThreadedExecutor`, chained system sets:

1. `Ingest` – owner loop has already written a bounded, fair batch of inbound messages; this
   set only records `Clock` and viewer attachments/departures (spawns/despawns viewer entities).
2. `Output` – feed `PaneOutput` into each pane's `Terminal`; collect host replies as
   `WriteInput` effects; mark panes dirty. EOF/exit records update `PaneState` only after every
   earlier chunk from the same per-pane FIFO has been applied.
3. `Requests` – drain each viewer's queue in order until it hits a barrier (a pending creation
   it requested); apply control and manager requests; validate ids/liveness/limits; emit
   effects and pending replies; reserve ids and spawn `Starting` panes for creations.
4. `Completions` – apply `SpawnCompleted`: insert the pane into the requested layout position,
   move the requester's focus, release its barrier; or despawn the reservation, reply failure.
5. `Lifecycle` – natural exits (close pane, close emptied tab, retire emptied workspace with its
   exit code), confirmed closes with termination effects, retirement grace, workspace kills,
   server shutdown; cascades are explicit despawns of owned entities, viewers are detached.
6. `Layout` – recompute geometry for tabs whose layout or displaying viewer sizes changed:
   pane size = layout over the smallest attached viewer showing that tab (hidden tabs keep
   their last geometry); emit `ResizePty` and resize the emulator.
7. `Snapshot` – derive each dirty viewer's frame (tab strip, rects, visible pane views at the
   viewer's size, focus, bindings, generation) and queue it before the replies it promises.
8. `Publish` – control subscriber events, `Deadlines`, clear step messages, `clear_trackers`.

Deferred mutations (spawns/despawns/inserts through `Commands`) become visible at the
automatically inserted sync points between sets; each set's systems that depend on earlier
mutations run after those points. No observers are used for core commands; component hooks are
not relied on for process cleanup.

## Ordering guarantees

- Per-source byte order: one FIFO channel per pane carries output, then EOF, then exit; one
  FIFO per viewer connection carries its requests. Ingest takes bounded prefixes of each FIFO.
- Multiple commands in one read: a viewer connection's frames are appended to its queue in
  arrival order and applied in order within one step. Split/new-tab requests set the viewer's
  barrier; later requests from that viewer wait until the completion step applies success or
  failure, so following input reaches the newly focused pane only after creation succeeded.
- Failed creation: the reserved pane entity (never in any layout, never in a frame) is despawned
  and the reply is `failed`; ids are not reused.
- Acknowledgements: replies are queued in the viewer outbox after the frame reflecting the
  applied state; coalescing replaces only a frame that has no reply behind it.
- Detach: the viewer sends its pending pane input, then `detach`; the server applies queued
  input before removing the viewer, and drops anything after `detach`. Workspace switching
  re-targets the same connection (`select-workspace`), so the suffix after the switch is applied
  to the destination's focused pane after the switch has been applied.
- Stale callbacks: completions and process events carry the `Entity`; a despawned entity fails
  the lookup and is ignored (Entity generations make recycled slots distinguishable), public ids
  are never reused.
- Starvation: per-step budgets (8 chunks per pane, 16 requests per viewer/control connection)
  and a wake re-arm when any FIFO still has items; timers and exits are separate messages read
  every step; slow viewers are disconnected when their bounded outbox overflows with replies.

## Process lifecycle

Spawn runs on an adapter thread (fork/exec is blocking) and completes as `SpawnCompleted`.
Each live pane has a reader thread (PTY output → FIFO, then `Eof`, then blocking `wait` →
`Exited`) and a writer thread (bounded input FIFO → PTY). EOF alone never retires a pane: a
process may close the terminal and keep running; the exit status is observed before retirement.
Explicit close terminates the process group (SIGHUP, then SIGKILL after a grace) and reaps it.
Workspace kill and server shutdown do the same for every pane and join adapter threads.
Persistence means surviving viewer loss; nothing is resurrected after a server restart.

## Viewer isolation and history

Each viewer has its own active tab, focus, menu, selection and history position. Layout
mutations are validated against the requesting viewer's focus; other viewers on the same tab see
the new layout but keep their own focus. History reads (`view`) return a private viewport at a
clamped offset without changing any shared state. Per-pane history is bounded by
`history.scrollback-lines`; eviction is vt100's ring; closing/retiring a pane frees its history.

## Removed

Floating popup panes, zoom, status segments/`set-status`, external command bindings and the
`binding` attachment message, lifecycle hooks, desktop notifications, agent dashboards and the
OSC 7877 observation adapter, automatic zor sidecar supervision and `zor-path`, the startup
workspace picker, SIGHUP configuration reload, hint delay/automatic preferences (hints are
immediate), and all configuration/protocol fields that served only those features.

## Protocols

- Attachment v3 (`hello.version == 3`): client `hello`, `input`, `mouse`, `control`, `view`,
  `resize`, `detach`; server `hello`, `state` (per-viewer frame, panes keyed by id with `cells`
  of `{text, kind, style}`), `reply`, `view`, `error`, `exited`. Frames stay 4-byte
  big-endian length prefixed JSON with the same size and deadline limits.
- Control `FUXCTL2`: `new`, `split`, `focus`, `kill`, `resize`, `send-keys`, `capture`, `list`,
  `tab {new, next, previous, select, select-id, rename, close}`, `workspace {list, new, kill}`,
  `subscribe`. Events: `pane.opened`, `pane.closed`, `pane.title`, `pane.output`,
  `tab.opened`, `tab.closed`, `client.attached`, `client.detached`, `workspace.resized`.
- Owner edits: koh's real-fux tests send `hello.version` 3; zor's control adapter sends and
  expects `FUXCTL2`. Both are one-line local changes exported as dependency patches.

## Defaults and policies

Prefix `C-a`; bindings `|`/`-` split, `h j k l` focus, `x` close pane, `X` close tab, `r`
resize, `[` copy/history, `t` new tab, `n`/`p` next/previous tab, `w` choose tab, `,` rename
tab, `s` choose workspace, `S` new workspace, `d` detach, `?` bindings. Default command is
`$SHELL -l` (platform fallback `/bin/sh`). Clipboard `disabled` or `write-only` (OSC 52, 1 MiB
encoded limit). History 10 000 lines per pane. A fresh `fux` creates workspace `default`, tab
`main`, one pane. With existing workspaces and no name, `fux` attaches to the workspace most
recently attached by any viewer (server start order breaks ties). Automatic labels: `ws-N`,
`tab-N`.

## Performance budgets (measured against the pre-rewrite baseline)

Baseline (release build, macOS, `tools/measure.py --version 2`): startup 15 ms, idle CPU
0.04 s per 10 s with one viewer, RSS 6.0 MiB at start / 20.7 MiB after a 20 000-line burst,
20 000-line burst 0.33 s, input-to-frame latency 15.9 ms median / 16.9 ms p95.

Budgets: idle CPU ≤ 0.01 s per 10 s (no periodic ticks), startup ≤ 50 ms, latency median ≤ 10 ms,
RSS after burst ≤ 40 MiB, burst ≤ 1.0 s. Regressions beyond these are fixed or explained.
