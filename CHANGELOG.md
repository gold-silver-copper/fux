# Changelog

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

Outstanding human evidence is tracked in `docs/release-readiness.md`, including genuine Claude
fixtures, emulator OSC collision checks, real Android execution, and a remote-relay exercise.
