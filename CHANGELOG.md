# Changelog

## 0.5.0 - 2026-09-06

Performance pass. Attachment protocol v6; control `FUXCTL2`, keys, configuration and CLI are
unchanged.

- Frames carry only what changed: the server keeps one retained grid per pane, read from the
  emulator once per step with every row stamped by the step it changed in, and each viewer
  remembers what it holds, so an update carries a pane only when it changed and then only its
  changed rows, as compact wire cells (blank runs, styles omitted when default). The viewer
  keeps its frame and applies updates; queued updates are merged rather than dropped; the
  bindings are sent once after the hello; cells are validated once, where they are produced.
- Output feeding uses a reusable buffer per pane and no longer clones the title per chunk.
- Measured on an M2 Max (release builds, `tools/measure*.py`, baseline 0.4.0 → 0.5.0): bytes on
  the attachment socket per keystroke 289,920 → 852 at 80×24 and 1,853,858 → 855 at 200×60;
  input-to-frame latency median 6.1 → 0.24 ms at 80×24, 44 → 0.47 ms at 200×60, 35.6 → 0.27 ms
  with eight viewers on one tab; server CPU per 1,000 keystrokes 1.3 → 0.1 s; server CPU for a
  20,000-line burst 0.21 → 0.06–0.09 s; the real viewer's CPU per 1,000 keystrokes 1.2 → 0.27 s
  (5.9 → 0.93 s at 200×60); idle CPU stays 0.00 s; memory per retained history row unchanged
  (vt100's 32 bytes per cell). The burst's wall time stays at the shell's own floor.
- New measurement tools: `tools/measure_frames.py` (bytes and CPU per keystroke by screen size and
  viewer count), `tools/measure_viewer.py` (the real viewer on a pty) and
  `tools/measure_memory.py` (bytes per retained history row).

## 0.4.0 - 2026-09-06

Internal architecture only: no protocol, configuration, key or CLI change (attachment v5,
control `FUXCTL2`, the same default bindings and behaviour).

- ECS systems are typed: output, layout, snapshot and viewer arrival/departure run as ordinary
  systems over `Query`/`Res`/`MessageReader` with `SystemParam` bundles (`Step`, `Scene`,
  `Arrivals`, `Effects`, `ViewerExit`); only request execution, spawn completion and the lifecycle
  cascade keep `&mut World`. Viewer queues drain through the schedule instead of tail calls.
- Workspace → tab membership is a bevy relationship (`TabOf`/`Tabs`) instead of a hand-kept
  `Vec<Entity>`.
- One set of helpers replaces repeated code: viewer scans and cascades in `ecs::support`, the
  viewer's text layout in `client::text`, the `actions!` table that generates the command enum,
  labels, groups and default bindings, `serde(default)` config merging, `thiserror` error types,
  one accept loop and one framed write for the sockets, one private-directory check.
- Removed with no caller: the historical design and prompt documents (git history keeps them),
  `CONTROL_VERSION`, the synchronous control reader, the server half of the client negotiation,
  test-only public helpers and unread capture-backend fields.
- The runtime, state and descriptor directories are now checked by the same rule as socket
  directories: a real directory, mode 0700, owned by the effective user (before, ownership was
  compared with the parent directory's owner).
- `src/` shrinks by about 640 lines (4 %); the markdown documentation by about 5,000 lines.

## 0.3.3 - 2026-09-06

- The command popup becomes a bottom-right column above the bar: one row per binding under its
  group heading, as wide as its widest line, as tall as its content, no title or footer rows. It
  scrolls one row per arrow and a screenful per page key only when the terminal is too short,
  with `▲ n more` / `▼ n more` rows marking hidden entries. The choosers, prompts and close
  confirmations use the same corner box, keeping their title and key-hint rows.
- Keys are matched without Shift: `X` triggers the `x` binding, `\` the `|` binding, `_` the `-`
  binding. Bindings that differ only by Shift are rejected, so close tab moves from `X` to `c`
  and new workspace from `S` to `a`.
- The `?` "show bindings" action is removed: the prefix itself shows the column and any unknown
  key keeps it open. A configuration that binds `help` no longer parses.
- A viewer drops binding actions it does not know instead of rejecting the frame, so a server of
  another 0.3.x release (with a different action set) still attaches; the keys shown are the
  server's until it is restarted.

## 0.3.2 - 2026-09-06

- The bar moves to the bottom row and gets its own background (`[style] bar-background`, default
  `bright-black`, with `bar` now defaulting to `white`). Popups sit above the bar. Pane content
  starts at row 0; the attachment protocol is v5 for that change of rectangle contract.

## 0.3.1 - 2026-09-06

- Replace the per-pane boxes with an always-visible top bar (workspace, tabs with the current one
  reversed, focused pane `id: title`) and shared one-cell separators between panes, bold next to
  the focused pane. Panes gain the rows and columns the frames took.
- Transient notices (copy results, errors, workspace switches) show in the bar's right zone for
  two seconds or until the next key; the bottom notice bar and the "Command failed" popup are gone.
- New `[style]` configuration table (`bar`, `tab-active`, `separator`, `separator-focused`,
  `notice`) with muted defaults.
- Mouse coordinates and history selections use the leaf rectangle directly. Because the meaning
  of frame rectangles changed, the attachment protocol is now v4; a viewer meeting a 0.3.0
  server gets the same interactive dialog as for a 0.2.x server.

## 0.3.0 - 2026-09-05

Complete rewrite as a minimal persistent multiplexer whose authoritative model is a standalone
`bevy_ecs` 0.19.1 World (workspaces, tabs, panes and viewers are entities; typed inbound messages
and effects; one explicitly ordered single-threaded schedule per event-driven step).

- Attachment protocol v3 (`input`, `mouse` with layout generation, `control`, `view`, `resize`,
  `detach`; per-viewer frames) and control protocol `FUXCTL2` (`tab`/`workspace` command
  families, `select-id`, `workspace select` for viewers). Version 2/`FUXCTL1` are not served.
- Viewer-private active tab, focus, menus, history position and selection; pane geometry
  negotiated over the smallest viewer showing a tab; hidden tabs keep their last size.
- Creation barrier: input queued behind a split, new tab or new workspace reaches the new pane
  only after the process started; failures roll back without a phantom pane.
- Natural exit of the last pane retires the workspace with its exit status after viewers saw the
  final frame; confirmed close and kill terminate process groups with a reap gate.
- Immediate keybinding popup with the workspace name; command bursts apply before repaint;
  prefix twice sends the literal prefix; unknown keys keep the popup; Esc backs out.
- Deterministic ECS suite with a randomized command-sequence test, real-process fixture scenarios,
  required real koh and real zor integrations with explicit binary paths.
- Removed: floating popup panes, full-screen pickers and the startup picker, external command
  bindings, lifecycle hooks, desktop notifications, agent dashboards and the OSC 7877 adapter,
  zor sidecar supervision (`zor-path`), status segments, hint delay settings, SIGHUP config
  reload, `tokio-util` and `loom`.
- Interactive handling of an older, incompatible session server: explain, list its recorded
  workspaces, and offer to stop it after typed confirmation or to run alongside it.
- MSRV 1.95 (required by bevy_ecs 0.19.1).

## 0.2.1 - 2026-09-04

- Make concurrent first-client startup elect exactly one daemon and workspace.
- Preserve real pane exit status across explicit workspace teardown.
- Handle SIGINT and SIGTERM throughout daemon startup and roll back owned resources.
- Expand deterministic binary verification for detach/reattach, copy mode, process ownership,
  natural retirement, remote reconnection, and startup interruption.

## 0.2.0 - 2026-09-03

- Add bounded synchronized workspace state, BSP layouts, diffs, tabs, popups, status, clipboard,
  bell and agent-state metadata.
- Add koh-backed pane hosting, streaming input routing, terminal reply handling, scrollback capture,
  and bare-pane fallback when zor is unavailable.
- Add a terminal compositor/client with prediction, detach handling, copy mode, and OSC ledgers.
- Add strict newline-delimited JSON control requests, replies, subscriptions, private filesystem
  authorization, startup-channel peer checks, and bounded slow-consumer queues.
- Add named-workspace daemon descriptors and one endpoint identity per workspace.
- Add deterministic state/router chaos tests and adversarial protocol/resource-bound tests.
