# fux

A small, trusted terminal multiplexer built on Bevy 0.19.1. One server owns real PTYs; terminal viewers share their processes while keeping independent layout sizes, focus, zoom and scrollback.

**The API is unrestricted same-user command execution.** Stock Bevy HTTP/BRP is exposed without credentials, capability tokens, component filters or a method allowlist. Loopback is a default, not authentication. Do not expose this server to callers you do not trust.

## Build and run

Rust 1.98.1 is pinned in `rust-toolchain.toml`; exact Bevy versions and `Cargo.lock` pin the dependency graph.

```sh
cargo build --release --locked
./target/release/fux server                   # foreground, 127.0.0.1:15702
# In another terminal:
./target/release/fux attach                   # or: attach WORKSPACE
./target/release/fux stop
```

Server options: `--address IP`, `--port PORT`, `--config FILE`. Clients use `FUX_ENDPOINT` (default `http://127.0.0.1:15702`). SIGINT, SIGTERM and SIGHUP stop the server; graceful viewer signals restore its terminal and detach. A forcibly killed viewer cannot restore its terminal, but does not kill the shared process.

No configuration file is required: the default command is `$SHELL` or `/bin/sh`, in the server's working directory. Missing or invalid configuration is logged; usable defaults/the previous valid configuration remain active.

## Controls

Press **Ctrl-B**, then a key. Press Ctrl-B twice to send a literal Ctrl-B. **Ctrl-B ?** opens paged binding help; Left/Right browse, Esc closes it.

| Key after prefix | Action |
| --- | --- |
| `h`, `v` | Split side-by-side / stacked; also open a pane in an empty workspace |
| `x`, `k` | Close pane / terminate process but retain its final screen |
| `n`, `z` | Next focus / viewer-local zoom |
| `w`, `c` | Next workspace / new workspace |
| `r`, `R` | Rename pane / workspace |
| Left/Right | Shrink/grow width at the nearest horizontal container |
| Up/Down | Grow/shrink height at the nearest vertical container |
| Shift-Left/Shift-Right | Reorder among siblings |
| `p` | Move focused pane to a workspace name |
| PageUp/PageDown | Scroll older/newer output |
| `y` | Copy the focused pane's visible text using OSC 52 |
| `s`, `l` | Save/load native layout scene; enter a path |
| `d` | Detach, preserving processes |

Click to focus. Application mouse protocols are forwarded in pane-relative coordinates; Shift-wheel scrolls local history instead. Copy requires an outer terminal that accepts OSC 52. The prefix, prompts and help consume their input; ordinary keys and bracketed paste go to the focused PTY.

A PTY has one real size: the smallest visible content height/width across its viewers. Larger viewers retain their own layout and leave surplus content cells blank. Resizing uses `vt100`'s screen/history behavior, not paragraph reflow. Detached processes keep their last size. Closing the last layout reference terminates that pane; removing a layout hierarchy directly does not own or resurrect its referenced processes.

## Configuration and scenes

The selected JSON file is a native Bevy asset, watched in its parent directory. Omitted fields retain defaults; supplying `bindings` replaces the binding list.

```json
{
  "prefix": "ctrl-b",
  "shell": ["/bin/sh"],
  "history_lines": 10000,
  "bindings": [
    {"key": "?", "action": "help"},
    {"key": "h", "action": "split_horizontal"},
    {"key": "d", "action": "detach"}
  ]
}
```

`layout: "layout.scn.ron"` optionally loads/watches a native `DynamicWorld` asset relative to the configuration directory. It replaces the workspace with the same saved name, or adds that workspace if no matching name exists. Use distinct workspace names when using this convenience path. Interactive/API save and load paths instead resolve relative to the server's working directory. Saved scenes contain the layout hierarchy and registered UI components, not PTYs, terminal history or process recipes.

Scene pane references resolve to existing process entities in the same server. Explicit `Control.mapping` pairs map `[saved_process_entity, existing_process_entity]`; all references are validated before creating layout entities. Missing processes fail without replacing the current layout. Loading never launches a process. IDs and scene formats are not stable across server runs or Bevy versions.

## Unrestricted remote control

`fux rpc METHOD '[JSON]'` sends a stock JSON-RPC request and prints its result. There is no application authorization step:

```sh
fux rpc rpc.discover
fux rpc registry.schema
fux rpc world.query '{"data":{"components":["fux::model::Workspace"]}}'
fux rpc world.query '{"data":{"components":["fux::model::ProcessState"]}}'
fux rpc world.spawn_entity '{"components":{"bevy_ecs::name::Name":"scratch"}}'
# Substitute IDs returned by the server:
fux rpc world.insert_components '{"entity":ENTITY,"components":{"bevy_ecs::name::Name":"renamed"}}'
fux rpc world.remove_components '{"entity":ENTITY,"components":["bevy_ecs::name::Name"]}'
fux rpc world.despawn_entity '{"entity":ENTITY}'
```

Native extensions are only `fux.attach`, `fux.frame` and stock-transport SSE `fux.frame+watch`. `fux.attach` accepts `{workspace?, rows, cols}` and returns a `viewer` entity. The others accept `{viewer}`. Closing a frame-watch connection detaches that viewer. A non-streaming API caller should explicitly detach it.

Use stock `world.trigger_event` for `fux::control::Control`, `fux::control::UserInput` and `fux::control::Shutdown`. All binding actions above are Control action names discoverable in `fux::assets::Settings`; `focus` additionally accepts a layout-leaf `target`. `move_workspace` accepts a workspace `target` or a name in `value`. Nonempty split `value` runs `/bin/sh -lc VALUE`; an empty value uses the configured command. Rename and scene paths use `value`.

```sh
fux rpc fux.attach '{"rows":24,"cols":80}'
fux rpc world.trigger_event '{"event":"fux::control::Control","value":{"viewer":VIEWER,"action":"split_horizontal","value":"exec /bin/sh"}}'
fux rpc world.trigger_event '{"event":"fux::control::UserInput","value":{"viewer":VIEWER,"input":{"kind":"key","key":"enter","ctrl":false,"alt":false,"shift":false}}}'
fux rpc world.trigger_event '{"event":"fux::control::Control","value":{"viewer":VIEWER,"action":"load_layout","value":"layout.scn.ron","mapping":[[OLD_PANE,LIVE_PANE]]}}'
fux rpc world.trigger_event '{"event":"fux::control::Control","value":{"viewer":VIEWER,"action":"detach"}}'
```

The uppercase IDs above are placeholders to substitute, not literal JSON values. `UserInput` also accepts `paste {text}`, `resize {rows,cols}` and `mouse {action,button,x,y,ctrl,alt,shift}` with a `kind` discriminator; mouse coordinates are zero-based viewer cells. Control/file-operation errors appear in the actual reflected `Viewer.notice`; asynchronous scene completion changes that notice.

For exact argv/cwd, stock-spawn a `fux::model::Launch` component, then a `PaneView` referring to its returned entity, and reparent that view under a workspace using `world.reparent_entities`. `Launch` is a creation recipe (`argv`, `cwd`, `history_lines`), not an automatic restart controller. Its required `ProcessState` reports the native PID, dimensions, exit and errors; reflected dimension edits resize the real PTY. Despawning the process or removing `Launch` terminates it. All registered operational and UI components remain available to stock inspection/mutation; resource/schedule/event/schema methods are not filtered.

## Architecture and dependency boundary

- `model.rs`: reflected ECS components; `ChildOf`/`Children` own layout hierarchies. The native `PaneView`/`PaneViews` relationship references a separate process entity without linked despawn.
- `server.rs`: typed Bevy observers, queries, SystemParams and scheduled systems implement operations. A causal settling update lets stock `RemoteLast` mutations reach lifecycle/layout systems even when idle. Layout caches follow component change ticks and archetype changes, not a component allowlist.
- `presentation.rs`: one inert native scene projection per viewer, sharing the type registry. `UiPlugin`, native flex/grid layout, visibility, `InputFocus`, tab navigation and `ui_focus_system` own geometry/focus/picking. No hand-written layout solver, OS window or GPU renderer is installed. The cell painter renders pane surfaces/borders and compact status, not a general Bevy image/text/shader renderer.
- `assets.rs`: `AssetServer`, native file watchers, asset events, `DynamicWorld` serialization and native entity maps. File reads/writes run on Bevy's I/O task pool. No parallel configuration or persistence engine.
- `terminal.rs`: `portable-pty` owns native PTYs/processes; `vt100` owns terminal parsing/history. Readiness-driven `async-io` tasks on Bevy's I/O pool handle bounded I/O (16 × 8 KiB output slots; 64 KiB consumed per pane/update; 16 input slots with a 64 KiB limit per input). One blocking native waiter per child preserves its unreaped PID while cleanup signals its group. Shutdown first allows 100 ms for shell hangup propagation, then closes the master, hard-kills the owned original group and reaps its leader. Closing the master before the blocking reap also releases a dying writer's queued PTY output on macOS. A final drain is bounded to 128 KiB.
- `viewer.rs`: `termina` supplies raw terminal input/dimensions/restoration; `ureq` consumes stock HTTP/SSE. Paints coalesce, while explicit clipboard effects are delivered separately. The stream reader does not hold its paint slot lock during terminal writes. Stock Bevy closes full watch response channels; fux drains/coalesces frames without replacing that transport. `signal-hook` handles graceful termination. `nix`, `parking_lot`, channels, serde/RON, Base64 and Unicode cell widths cover the remaining narrow native/protocol needs.

The runner parks without an idle tick; PTY data/exit, requests, disconnections, signals and asset notifications wake it. Streamed paints coalesce behind a 16 ms minimum interval, using an on-demand one-shot I/O-pool timer; idle viewers have no recurring paint timer. Direct `fux.frame` snapshots are immediate. While native asset loads are pending the runner uses a 25 ms settling deadline. Scene/UI projections are caches, never process/session authorities. Transitive Bevy rendering-related types are dependencies of native UI/camera APIs; renderer plugins are not running.

## Scope and verification

Tested on macOS arm64; see [VERIFICATION.md](VERIFICATION.md) for commands, real-TTY evidence and release observations. Linux and other Unix systems are unvalidated; this is not a Windows/mobile implementation.

Owned direct children and their original process groups are cleaned up and reaped. Ordinary interactive-shell job groups receive the shell's hangup propagation. Deliberately detached/disowned descendants, or descendants in other groups that ignore hangup, are not a process-containment guarantee; fux does not enumerate and signal potentially recycled descendant PIDs. macOS zombie-only group `EPERM` is distinguished by native membership inspection, not ignored for live groups.

Intentionally excluded: authentication/hardening, remote-host catalogs/tunnels, task/provider policy, crash recovery, process resurrection, automatic restart, plugin installation, durable input receipts, graphics protocols, IME and broad editor/dashboard features. No cross-version API or saved-scene compatibility promise.
