# Changelog

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
