# fux handoff

Updated 2026-09-06 for 0.5.0. fux is a persistent terminal multiplexer whose authoritative model
is a standalone `bevy_ecs` World; the architecture is in [docs/design.md](docs/design.md), the
requirement audit and review record in [docs/ecs-acceptance.md](docs/ecs-acceptance.md),
protocols in `docs/local-*.md`.

## State

- 0.5.0 is the performance pass over the 0.4.0 refactor: attachment protocol v6 carries only
  the rows that changed (retained grid per pane on the server, retained frame on the viewer,
  merged updates in the outbox, bindings sent once); the emulator is fed from a reusable
  buffer. Control `FUXCTL2`, keys, configuration and CLI are unchanged. The measurement method
  is `tools/measure.py`, `tools/measure_frames.py`, `tools/measure_viewer.py` and
  `tools/measure_memory.py`; the numbers are in docs/ecs-acceptance.md "Performance pass".
- Four systems remain exclusive (`&mut World`): request execution, viewer queue draining, spawn
  completion and the lifecycle cascade; each mutates entities it must observe again within the
  same phase (see docs/design.md "Systems").
- Owner checkouts `references/koh` and `zor/` stay at their pinned bases with the one-line
  patches in `dependency-patches/`; `python3 tools/dependencies.py verify --build` reconstructs and
  tests them.
- Verification gate (all must pass before any publication): the commands in the README's
  "Verification" section plus the real koh and zor integrations with explicit binary paths.

## Limits

Runtime evidence is macOS only; Linux and Android are configured CI targets without an executed
run. Emulator-specific clipboard and mouse behaviour and koh relay/NAT scenarios remain manual. The
protocols carry no version numbers; a server older than its client is reported as an error and
restarted by the operator, never stopped by fux.
