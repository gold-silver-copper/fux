# fux handoff

fux is a persistent terminal multiplexer whose authoritative model is a standalone `bevy_ecs`
World. The architecture is in [docs/design.md](docs/design.md), wire contracts in
`docs/local-*.md`, and the verification record in [docs/verification.md](docs/verification.md).

## State

- The repository is a virtual workspace: `crates/fux`, `crates/local-ipc` and `crates/zor` are
  separate crates sharing one lockfile, CI and gate. koh is the remaining companion, pinned at
  an exact commit in `tools/xtask/companions.json`; `cargo run --locked --manifest-path
  tools/xtask/Cargo.toml -- dependencies verify --build` checks the pin.
- fux owns generic terminal control: PTYs and processes, terminal state and history, layouts,
  viewer interactions, reliable input receipts, bounded event replay and retained final records.
  Workflows over those primitives live in zor. Agent interpretation and provider, task, check
  and artifact policy belong to zor; fux ignores OSC 7877 agent reports.
- Four scheduled systems take `&mut World` by design (`apply_requests`, `drain_viewer_queues`,
  `apply_spawn_completions`, `resolve_lifecycle`); the rest are typed. See docs/design.md
  "Systems" for why.
- Measurement commands are `fux-xtask measure`, `measure-frames`, `measure-viewer`,
  `measure-memory` and `measure-koh`, each taking a binary path. `FUX_MICRO_TIMING=1 cargo test
  -p fux --release --test micro_timing -- --nocapture` times the hot paths in isolation.
- The verification gate (README "Verification") plus the real koh and zor integrations must
  pass before any publication.

## Limits

Runtime evidence covers macOS and targeted Linux ARM64. A complete final Linux gate and Android
runtime acceptance are not established. Emulator-specific clipboard and mouse behaviour and koh
relay/NAT scenarios remain manual. The protocols carry no version numbers; a server older than
its client is reported as an error and restarted by the operator, never stopped by fux.
