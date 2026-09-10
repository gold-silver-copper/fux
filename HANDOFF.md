# fux handoff

Updated for the uncommitted main-based native integration. fux is a persistent terminal
multiplexer whose authoritative model is a standalone `bevy_ecs` World. Current architecture
is in [docs/design.md](docs/design.md), wire contracts in `docs/local-*.md`, and integration
decisions, evidence and remaining work in [docs/native-integration.md](docs/native-integration.md).
The older release and performance record in `docs/ecs-acceptance.md` is historical evidence.

## State

- Main's typed ECS, retained grids, changed-row attachment frames, frame pacing and reusable
  parsing buffers remain the implementation foundation. Local protocols use `FUX\n` and an
  unversioned attachment hello. The repository is a virtual workspace: `crates/fux` and
  `crates/zor` are separate crates and binaries sharing one lockfile, CI and gate; koh is the
  remaining pinned and patched companion.
- fux owns generic terminal control. Server/workspace identity, coherent conditional captures,
  tracked input receipts, bounded event replay and retained final records support unattended
  consumers. `fux run` obtains final output and status from retained records.
- Agent interpretation and provider/task/check/artifact policy belong to zor. fux ignores OSC
  7877 agent reports and exposes no pane agent field or event. Generic title/progress remain.
- Active verification and measurements use Rust xtask. The original main Python scripts and
  native reports are explicitly archived with provenance. Current measurement commands are
  `fux-xtask measure`, `measure-frames`, `measure-viewer` and `measure-memory`, each taking a
  fux binary path, plus `measure-koh KOH_BINARY` for the two-process local gateway path. On
  macOS the harness reads process CPU through `proc_pid_rusage` (microseconds); elsewhere it
  keeps main's `ps` convention. `FUX_MICRO_TIMING=1 cargo test -p fux --release --test micro_timing
  -- --nocapture` times the event-log and terminal hot paths in isolation. Historical passing gates do not validate this uncommitted integration.
- Completion is not yet established: paired native measurements, final independent review
  and a fresh complete mandatory headless gate remain outstanding. Live remote R6 and paid
  provider acceptance remain deferred.
- Six scheduled systems currently use `&mut World`: request execution, viewer queue draining,
  spawn completion, wait resolution, input completion and the lifecycle cascade
  (see docs/design.md "Systems").
- The koh checkout `references/koh` is pinned at an exact commit in
  `tools/xtask/companions.json` with no local patch; `cargo run --locked --manifest-path tools/xtask/Cargo.toml -- dependencies verify --build`
  checks the pin and tests it. zor was imported from `2a8769e` plus its reviewed patch into `crates/zor`
  (byte-identical to the previously verified tree); the standalone zor repository is historical.
- Verification gate (all must pass before any publication): the commands in the README's
  "Verification" section plus the real koh and zor integrations with explicit binary paths.

## Limits

Runtime evidence includes macOS and targeted Linux ARM64 tests and benchmarks, recorded in
`docs/native-integration.md`. A complete final Linux gate and Android runtime acceptance are
not established. Emulator-specific clipboard and mouse behaviour and koh relay/NAT scenarios remain manual. The
protocols carry no version numbers; a server older than its client is reported as an error and
restarted by the operator, never stopped by fux.
