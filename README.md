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

Press **Ctrl-B** to open the command column, then a configured key to execute it. Ctrl-B twice sends a literal Ctrl-B; Esc cancels, and unknown keys leave the column open. There is no timeout. **Ctrl-B ?** opens scrollable help: Up/Down move one row, PageUp/PageDown a screenful, Esc or `q` closes it. In prefix mode, arrows still execute the configured resize bindings; only explicit help owns keyboard scrolling. The wheel over either column scrolls its list.

| Key after prefix | Action |
| --- | --- |
| `h`, `v` | Split side-by-side / stacked; also open a pane in an empty workspace |
| `x`, `k` | Confirm pane close / explicitly terminate process but retain its final screen |
| `n`, `z` | Next focus / viewer-local zoom |
| `w`, `P`, `c` | Next workspace / previous workspace / new workspace |
| `r`, `R` | Rename pane / workspace |
| Left/Right | Shrink/grow width at the nearest horizontal container |
| Up/Down | Grow/shrink height at the nearest vertical container |
| Shift-Left/Shift-Right | Reorder among siblings |
| `p` | Choose a destination workspace for the focused pane |
| PageUp/PageDown | Scroll older/newer output |
| `y` | Copy the focused pane's visible text using OSC 52 |
| `s`, `l` | Save/load native layout scene; enter a path |
| `d` | Detach, preserving processes |
| `t`, `]`, `{` | New tab / next tab / previous tab |
| `T`, `W` | Choose tab / workspace |
| `u`, `!` | Previous pane / last-focused pane |
| Alt-Left/Right/Up/Down | Directional focus |
| `;`, `'`, backtick | Pane / tab / workspace actions |
| `[` | History and selection mode |

All previous PR #20 bindings remain. Uppercase `T`, `W`, `R` are distinct bindings, as before. Actions without default shortcuts are available in the action menus and can be bound in configuration. In particular: `rename_tab`, `tab_close`, `workspace_close`, `tab_reorder_previous/next`, `workspace_reorder_previous/next`, `swap_choose`, `swap_left/right/up/down`, `move_left/right/up/down`, `move_tab`, `move_new_tab`, and `move_new_workspace`.

Workspaces contain ordered tabs, each containing its own split/pane tree. Viewers independently remember the selected tab per workspace and the focused/last-focused pane per tab. Next/previous pane use native tab navigation; directional focus ranks Bevy-computed pane centers by cross-axis distance, then forward distance, then entity ID, without wrapping at an edge. Switching tabs or moving panes exits zoom; zoom never changes another viewer. Removed or hidden targets get a deterministic surviving focus. Processes and history survive switches.

Choosers and menus use Up/Down or `j/k`, PageUp/PageDown, Home/End, Enter, and Esc/`q`. A selected row remains reachable even when only one overlay row fits. Pane menus act on the pane they opened on, not a later focus. Tab/workspace menus provide creation, rename, reorder, and close. Directional move nests the source beside the nearest directional pane; swap exchanges pane positions. Moving to a workspace uses its first tab. Empty split containers collapse bottom-up. New tabs/workspaces created by a move reuse the original process instead of launching a shell.

Interactive pane/tab/workspace closes require `y`; `n` or Esc cancels. Confirmations capture entity identities, revalidate them, and never retarget a disappeared item. Closing a tab/workspace removes its contained views, terminating only processes with no surviving references. Closing the last tab retains one empty tab; closing a workspace selects the first surviving workspace for its viewers, or detaches them if none remain. Direct API close actions are explicit and noninteractive. Natural process exit still retains the final screen.

Click to focus; click a tab to select it. Right-click a pane/tab or click the workspace name for its action menu. When an application requests mouse input, pane events remain application-owned; Shift-right-click opens fux's menu instead. Wheel and drag browse/select the pane under the pointer when the application does not request mouse input, or when Shift is held. Application events keep pane-relative coordinates, including first/last content cells. Bars, separators and modal overlays never click through to the PTY. Overlay lists are keyboard-operated, with wheel navigation; list entries are not clickable.

Copy mode: arrows or `h/j/k/l` move, `u/d` or PageUp/PageDown browse history, Home/End move within a row, Space anchors a selection, `c` clears it, `y` or Enter copies it and returns to live output, `g` returns to live without copying, `q` exits, and Esc clears a selection before exiting. Mouse drag selects; `y`/Enter copies after release. Selection is viewer-private and limited to the displayed viewport, including its wrapped rows. Wide-glyph continuations normalize to the leading cell; combining marks remain attached; wrapped rows join without an invented newline. Blank padding at hard line ends is trimmed.

`clipboard` is **disabled by default**. Set `"clipboard":"write-only"` to permit bounded OSC 52 writes; the outer terminal must also allow them. Copy never reads the system clipboard. Each encoded effect is at most 1 MiB, with at most 16 queued effects. A copy viewport is capped at 262144 cells. Failure and success are reported in the bar.

`vt100` has history offsets, not stable row identities. fux validates a bounded cell snapshot before using a selection. Changed rows, resizing, explicit scrolling, or eviction clear an invalidated selection with a visible notice, rather than silently copying different text. This conservative rule also clears on visible-cell/style changes outside the selected span; it does not promise persistent selections across terminal reflow.

Prefix, prompts, confirmations, menus and copy mode own their input. The frontend sends a paste-start marker before buffering a fragmented bracketed paste, retaining its original owner until the end marker. Cancellation, prompt replacement, or a focus switch cannot redirect that paste to a PTY. Paste is bounded to 64 KiB; oversized content is drained and discarded. Termina still decodes ordinary keys/mouse; a lone Escape uses a 35 ms disambiguation deadline. Ordinary nonmodal keys and complete pastes go to the focused PTY.

Panes are borderless: content starts at the top-left cell, with one shared thin separator between default split siblings. Separators next to focus are bold; others are muted. The last row is always a full-width, gray-background bar: workspace and ordered tabs on the left (active tab reversed), focused process `id: name`/exit status on the right. Notices replace the right zone (yellow; errors red) until subsequent input clears them. Zoom/history indicators stay compact. Unfocused exited panes retain a small dim marker, not a title strip.

The command/help list grows upward from the bottom-right, directly above the bar, one configured binding per row, grouped into Panes, Focus, Tabs, Workspaces and Session while preserving configuration order within groups. Unknown custom actions are listed under Other. Unavailable commands are dimmed and explain why when invoked. A bold heading, contrasting background and minimal padding distinguish it without a border. Hidden rows are marked `▲ n more` / `▼ n more` when space permits; narrow labels use cell-aware ellipsis. Rename, scene-path prompts, choosers, confirmations and context menus use the same corner surface; editable text is reversed and its tail stays visible. Closing an overlay restores the pane and cursor. A one-row viewer shows only the bar; zero-sized views paint no content. The active tab remains in the overflow window; at one or two columns it takes priority over the workspace label.

A PTY has one real size: the smallest visible content height/width across its viewers, with a 2×2 backing minimum because pinned vt100 0.16.2 underflows on one-row wrapping/one-column wide glyphs. Tiny viewers still paint only their actual available cells, never partial wide glyphs or invented rows. Larger viewers retain their own layout and leave surplus content cells blank. Resizing uses `vt100`'s screen/history behavior, not paragraph reflow. Detached processes keep their last size. Closing the last layout reference terminates that pane; removing a layout hierarchy directly does not own or resurrect its referenced processes.

## Configuration and scenes

The selected JSON file is a native Bevy asset, watched in its parent directory. Omitted fields retain defaults; supplying `bindings` replaces the binding list.

```json
{
  "prefix": "ctrl-b",
  "shell": ["/bin/sh"],
  "history_lines": 10000,
  "clipboard": "write-only",
  "bindings": [
    {"key": "?", "action": "help"},
    {"key": "h", "action": "split_horizontal"},
    {"key": "d", "action": "detach"}
  ]
}
```

`layout: "layout.scn.ron"` optionally loads/watches a native `DynamicWorld` asset relative to the configuration directory. It replaces the workspace with the same saved name, or adds that workspace if no matching name exists. Use distinct workspace names when using this convenience path. Interactive/API save and load paths instead resolve relative to the server's working directory. Saved scenes contain the layout hierarchy and registered UI components, not PTYs, terminal history or process recipes. Borderless defaults do not rewrite loaded Nodes: native flex/grid, visibility, spacing and other registered components remain intact. Only actual one-cell gaps between visible siblings of `Split` containers receive separator glyphs; custom margins and wider gaps remain blank. Tabless scenes from PR #20 are wrapped into a `main` tab: their complete root layout Node moves into that tab under a neutral workspace wrapper, so grid tracks, padding and margins apply once. Existing child entities and process references are retained. New tabbed scenes preserve native Nodes unchanged. Tabs must be direct workspace children; viewer/runtime state is not scene content.

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

Use stock `world.trigger_event` for `fux::control::Control`, `fux::control::UserInput` and `fux::control::Shutdown`. All binding actions above are Control action names discoverable in `fux::assets::Settings`; `focus` additionally accepts a layout-leaf `target`. `tab_select` and `workspace_select` accept a destination `target`. `move_tab` and `swap` accept a destination tab/pane-view `target`; their source is the viewer's focused pane. `move_workspace` accepts a workspace `target` or a name in `value`. Empty chooser/move values open an interactive list. `close`, `tab_close`, and `workspace_close` are explicit noninteractive API actions; interactive bindings add confirmation. Rename/reorder/close actions may target their subject entity. Do not use raw hierarchy despawn as a substitute for a close action: native hierarchy removal does not own shared processes. Nonempty split `value` runs `/bin/sh -lc VALUE`; an empty value uses the configured command. Rename and scene paths use `value`.

```sh
fux rpc fux.attach '{"rows":24,"cols":80}'
fux rpc world.trigger_event '{"event":"fux::control::Control","value":{"viewer":VIEWER,"action":"split_horizontal","value":"exec /bin/sh"}}'
fux rpc world.trigger_event '{"event":"fux::control::UserInput","value":{"viewer":VIEWER,"input":{"kind":"key","key":"enter","ctrl":false,"alt":false,"shift":false}}}'
fux rpc world.trigger_event '{"event":"fux::control::Control","value":{"viewer":VIEWER,"action":"load_layout","value":"layout.scn.ron","mapping":[[OLD_PANE,LIVE_PANE]]}}'
fux rpc world.trigger_event '{"event":"fux::control::Control","value":{"viewer":VIEWER,"action":"detach"}}'
```

The uppercase IDs above are placeholders to substitute, not literal JSON values. `UserInput` also accepts `paste_begin` followed by `paste {text}` (ownership-preserving fragmented paste), atomic `paste {text}`, `resize {rows,cols}` and `mouse {action,button,x,y,ctrl,alt,shift}` with a `kind` discriminator; mouse coordinates are zero-based viewer cells. Control/file-operation errors appear in the actual reflected `Viewer.notice`; asynchronous scene completion changes that notice.

For exact argv/cwd, stock-spawn a `fux::model::Launch` component, then a `PaneView` referring to its returned entity, and reparent that view under a tab using `world.reparent_entities`. `Launch` is a creation recipe (`argv`, `cwd`, `history_lines`), not an automatic restart controller. Its required `ProcessState` reports the native PID, dimensions, exit and errors; reflected dimension edits resize the real PTY. Despawning the process or removing `Launch` terminates it. All registered operational and UI components remain available to stock inspection/mutation; resource/schedule/event/schema methods are not filtered.

## Architecture and dependency boundary

- `actions.rs` shares labels/grouping/availability across help, menus and interactive dispatch. `interaction.rs` stores captured-target overlays directly on viewer entities; `navigation.rs` repairs native tab hierarchies and bounded viewer-local memory. `selection.rs` stores only one bounded viewport per copying viewer, never a second history. These runtime components are unreflected and excluded from scenes.
- `model.rs`: reflected ECS components; `ChildOf`/`Children` own layout hierarchies. The native `PaneView`/`PaneViews` relationship references a separate process entity without linked despawn.
- `server.rs`: direct typed Bevy observers, queries/resources and scheduled systems implement operations. Scene preparation, sizing and ordinary frame formatting share an explicit exclusive transaction; the painter borrows Viewer state and copies only focus/scroll offsets instead of cloning it or registering a cached system. Independent non-send presentation Apps retain native focus isolation. A causal settling update lets stock `RemoteLast` mutations reach lifecycle/layout systems even when idle. Workspace-owned layout caches follow arbitrary component change ticks, archetype changes and native hierarchy membership, not a component allowlist. Unrelated workspace projections remain cached; removing a Viewer also removes its presentation context.
- `presentation.rs`: one inert native scene projection per viewer, sharing the type registry. `UiPlugin`, native flex/grid layout, visibility, `InputFocus`, tab navigation and `ui_focus_system` own geometry/focus/picking. No hand-written layout solver, OS window or GPU renderer is installed. The cell painter renders borderless pane surfaces and computed shared separators, not a general Bevy image/text/shader renderer. `chrome.rs` paints the bottom bar and content-sized corner overlays; their cached bounds are only for overlay wheel handling, not a second pane-layout or picking system.
- `assets.rs`: `AssetServer`, native file watchers, asset events, `DynamicWorld` serialization and native entity maps. File reads/writes run on Bevy's I/O task pool. No parallel configuration or persistence engine.
- `terminal.rs`: each authoritative process entity owns an unreflected `Terminal` runtime component, accessed through typed queries rather than a separate entity-keyed registry; only shared wake/coalescing state is a resource. Scheduled Launch removal publishes final status before removing the runtime, while entity despawn and explicit pre-join shutdown drop it. Inert scenes never construct or serialize runtimes. `portable-pty` owns native PTYs/processes; `vt100` owns terminal parsing/history. One bounded row cache per process reuses unchanged screen extraction; cursor and input modes come directly from the emulator. Readiness-driven `async-io` tasks on Bevy's I/O pool handle bounded I/O (16 × 8 KiB output slots; 64 KiB consumed per pane/update; 16 input slots with a 64 KiB limit per input). One blocking native waiter per child preserves its unreaped PID while cleanup signals its group. Shutdown first allows 100 ms for shell hangup propagation, then closes the master, hard-kills the owned original group and reaps its leader. Closing the master before the blocking reap also releases a dying writer's queued PTY output on macOS. A final drain is bounded to 128 KiB.
- `viewer.rs`: `termina` supplies key/mouse decoding, terminal modes/dimensions/restoration; `paste.rs` adds bounded paste-envelope detection on a readiness-driven Unix input loop, with native SIGWINCH resize handling and an explicit stop socket; `ureq` consumes stock HTTP/SSE. The maintained HTTP Agent is reused. Paints coalesce, while explicit clipboard effects are delivered separately; the server queues up to 16 pending copies per viewer and reports overflow in `Viewer.notice`. The stream reader does not hold its paint slot lock during terminal writes. Stock Bevy closes full watch response channels; fux drains/coalesces frames without replacing that transport. `signal-hook` handles graceful termination. `nix`, `parking_lot`, channels, serde/RON, Base64 and Unicode cell widths cover the remaining narrow native/protocol needs.

The runner parks without an idle tick; PTY data/exit, requests, disconnections, signals and asset notifications wake it. Streamed paints coalesce behind a 16 ms minimum interval, using an on-demand one-shot I/O-pool timer; idle viewers have no recurring paint timer. Direct `fux.frame` snapshots are immediate. While native asset loads are pending the runner uses a 25 ms settling deadline. Scene/UI projections are caches, never process/session authorities. Transitive Bevy rendering-related types are dependencies of native UI/camera APIs; renderer plugins are not running.

## Scope and verification

Tested on macOS arm64; see [verification/interaction-restoration.md](verification/interaction-restoration.md) for this interaction pass and intentional differences from original main, [verification/design-restoration.md](verification/design-restoration.md) for the current visual/input verification and captured renders, [REFINEMENT.md](REFINEMENT.md) for historical comparable measurements and the capability audit, and [VERIFICATION.md](VERIFICATION.md) for preserved baseline evidence. Linux and other Unix systems are unvalidated; this is not a Windows/mobile implementation.

Owned direct children and their original process groups are cleaned up and reaped. Ordinary interactive-shell job groups receive the shell's hangup propagation. Deliberately detached/disowned descendants, or descendants in other groups that ignore hangup, are not a process-containment guarantee; fux does not enumerate and signal potentially recycled descendant PIDs. macOS zombie-only group `EPERM` is distinguished by native membership inspection, not ignored for live groups.

Intentionally excluded: authentication/hardening, remote-host catalogs/tunnels, task/provider policy, crash recovery, process resurrection, automatic restart, plugin installation, durable input receipts, graphics protocols, IME and broad editor/dashboard features. No cross-version API or saved-scene compatibility promise.
