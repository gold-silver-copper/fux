# fux handoff

Updated 2026-09-06 for 0.4.0. fux is a persistent terminal multiplexer whose authoritative model
is a standalone `bevy_ecs` World; the architecture is in [docs/design.md](docs/design.md), the
requirement audit and review record in [docs/ecs-acceptance.md](docs/ecs-acceptance.md),
protocols in `docs/local-*.md`.

## State

- 0.4.0 is an internal refactor of 0.3.3: typed systems with `SystemParam` bundles, the
  `TabOf`/`Tabs` relationship, shared helpers, dead code removed. Attachment protocol v5, control
  `FUXCTL2`, keys, configuration and CLI are unchanged.
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
run. Emulator-specific clipboard and mouse behaviour and koh relay/NAT scenarios remain manual. A
running 0.3.x server older than the viewer is rejected by the handshake and the viewer offers to
stop it or run alongside it; nothing is ever stopped without a typed confirmation. The Python
harnesses in `tests/local_cli.rs` are timing sensitive under heavy machine load (a partial first
frame or a slow detach); rerun on a quiet machine before reading a failure as a regression.
