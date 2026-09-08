Capability audit: fux + zor + koh versus herdr, tmux, and Zellij
================================================================

Audit date: 2026-09-06. This compares the source snapshots in this workspace, including local modifications. It does not assert parity with upstream releases or treat design prompts as implemented features. Reference projects received source inspection; their complete test suites were not run. No application code was changed.

| Project | Inspected snapshot |
|---|---|
| fux | 0.3.3, `5449616` |
| zor | 0.1.2, `fb6a1ef`, with modified observation code and contract |
| koh | 0.12.1, `50a8270`, with modified gateway tests |
| herdr | 0.8.2, `94f6d9c` |
| tmux | `next-3.8`, `578e07f` |
| Zellij | 0.46.0, `af38660` |

**Assessment:** fux implements a useful minimal persistent multiplexer. The three-project composition is not currently a feature-equivalent alternative to herdr, tmux, or Zellij. It has a promising separation of terminal ownership, observation, and transport, but the checked-out observer integration is broken, agent detection needs user-supplied rules, and agent state is not integrated into fux. The largest functional gaps are agent orchestration, layout manipulation, searchable history, restoration, and richer terminal protocols.

**Capability comparison**

“External” means custom scripts or another program must supply the workflow. “Absent” means no implementation was found in the inspected public commands, protocols, model, and relevant subsystems; it is not a claim that the feature cannot be added.

| Capability | fux + zor + koh | herdr | tmux snapshot | Zellij snapshot |
|---|---|---|---|---|
| Detach while terminal processes continue | Implemented by fux | Implemented | Implemented | Implemented |
| Workspaces, tabs, tiled splits | Implemented | Implemented | Sessions, windows, panes | Sessions, tabs, panes |
| Multiple simultaneous viewers | Private tab/focus/history; shared geometry | Client/server attachments | Clients, session/window controls | Multiple clients and watcher mode |
| Split resizing and directional focus | Implemented | Implemented | Implemented | Implemented |
| Zoom, move/swap panes, reusable layouts | Absent from fux command/API surface | Implemented | Implemented | Implemented |
| Floating panes | Absent | Tiled layout plus UI overlays; not credited as general floating PTYs | Implemented in this development snapshot | Implemented, plus stacked panes |
| Scrollback and selection | Implemented, bounded per pane | Implemented | Implemented | Implemented |
| Search history | Absent | Copy search | Copy-mode search | Search mode |
| Machine control | JSON list/capture/input/lifecycle subscriptions | Rich typed pane, agent, workspace and event API | Commands, formats, hooks, control mode, pipes | CLI actions, pipes, plugin API |
| Semantic agent detection | zor engine exists; no active bundled rules; observer currently incompatible | Manifests, integrations, agent state | External | External/plugin workflows |
| Agent dashboard, prompt and wait | Absent; custom consumer required | First-class agent list/view/start/prompt/wait | External; `wait-for` is generic synchronization | External/plugin workflows |
| Restore after server/machine restart | Absent | Session/history restoration and supported-agent resume | External restoration tooling | Session resurrection and saved layouts/content |
| Remote access | koh authenticated resumable Unix-socket gateway | SSH bootstrap/attachment | External transport such as SSH | SSH attachment or built-in web server |
| Poor-network local prediction | Not in the fux gateway path | Not established by this audit | External transport | External transport; not established for web path |
| Browser access | Absent | Not established | External | Built-in web interface and tokens |
| Extensibility | External processes and fixed command/binding registry | Plugin commands, events and panes | Shell commands/hooks/configuration | WASM/WASI plugins |
| Rich terminal output | Text cells, ANSI/RGB styles, mouse, paste, selected queries, OSC 52 policy | Ghostty integration, Kitty graphics, links | Extended keys, OSC 8, optional Sixel | Kitty/Sixel, hyperlinks and richer terminal handling |

Primary matrix evidence: fux [command registry](../src/commands.rs), [control schema](../src/proto/control.rs), [attachment schema](../src/proto/attach.rs), [copy controller](../src/client/copy.rs), and [terminal implementation](../src/terminal.rs); herdr [API schema](../references/herdr/src/api/schema.rs), [agent wait implementation](../references/herdr/src/api/wait.rs), [plugin runtime](../references/herdr/src/app/api/plugins/runtime.rs), [persistence](../references/herdr/src/persist.rs), and [remote attachment](../references/herdr/src/remote/attach.rs); tmux [manual](../references/tmux/tmux.1), [floating/split implementation](../references/tmux/cmd-split-window.c), [control implementation](../references/tmux/control.c), and [terminal parser](../references/tmux/input.c); Zellij [actions](../references/zellij/zellij-utils/src/input/actions.rs), [CLI](../references/zellij/zellij-utils/src/cli.rs), [serialization](../references/zellij/zellij-utils/src/session_serialization.rs), and [terminal grid](../references/zellij/zellij-server/src/panes/grid.rs).

**Confirmed composition problems**

1. **The checked-out zor observer cannot negotiate with fux.** `zor/src/observe.rs` sends and expects four bytes, `FUX\n`. fux requires eight bytes, `FUXCTL2\n`. The versioned patch stored in fux expects yet another starting point, `FUXCTL1\n`, and changes it to `FUXCTL2\n`. The current local checkout therefore differs from the reviewed patch representation. A forced integration test with the freshly built local zor failed waiting for its working-state report. This is a current integration defect, not an architectural limitation. Evidence: [zor RPC](../zor/src/observe.rs), [fux preface](../src/proto/control.rs), [stored patch](../dependency-patches/zor.patch), [integration harness](../tests/verify/observer.py).

2. **The observer retains obsolete pane geometry assumptions.** It subtracts two from the reported height and width. Current fux panes have no enclosing two-cell frame: the listing exposes `component.rect`, and terminal sizing uses the pane rectangle. After repairing negotiation, this will still re-emulate captures at the wrong dimensions, potentially shifting/wrapping screen evidence. It also seeks a `seq` field absent from `PaneSummary`, so its idle capture cache cannot activate; every sample takes the capture path. This is source-confirmed; the geometry effect was not separately reproduced at runtime. Evidence: [observer sampling](../zor/src/observe.rs), [pane model](../src/ecs/components.rs), [listing](../src/ecs/systems/requests.rs), [summary schema](../src/proto/control.rs).

3. **zor supplies an engine, not ready-to-use agent coverage.** `load_all` loads external `.toml` files from an explicitly set `XDG_CONFIG_HOME/zor/rules` and `--rules` directories. The repository contains only `rules/claude.toml.draft`; it is neither an active `.toml` file nor embedded by the loader. Running `zor agents` without XDG configuration printed `no bundled agent rule sets`. The integration test creates synthetic rules and forces `--agent test`; it does not establish Claude/Codex detection quality. herdr has actual agent manifests and integration assets. Evidence: [loader](../zor/src/rules/bundle.rs), [draft](../zor/rules/claude.toml.draft), [test rules](../tests/verify/observer.py), [herdr manifests](../references/herdr/src/detect/manifests).

4. **There is no first-class route from zor state to fux UI/API.** The observer emits newline-separated OSC 7877 reports to stdout. fux has no agent-state component, summary field, subscription event, or command action; its terminal callback handles OSC progress but not OSC 7877. Wrapping a command with zor can expose title text, subject to rules and title policy, but that is not structured state, cross-pane aggregation, notifications, or semantic waits. The zor contract's claim that fux displays observed state is ahead of the current implementation. Evidence: [observer output](../zor/src/observe.rs), [terminal callbacks](../src/terminal.rs), [frame model](../src/view.rs), [event schema](../src/proto/control.rs), [observation contract](../zor/OBSERVATION-CONTRACT.md).

5. **The combined build is not continuously enforced by default.** `python3 tools/dependencies.py verify` failed with `koh: patch is stale`. The tool stops at koh, so that invocation did not complete zor reconstruction. The cross-repository CI job requires manual dispatch with integrations enabled; normal fux tests can skip zor unless its binary is required explicitly. Standalone test success therefore does not prove this composition works. Evidence: [dependency tool](../tools/dependencies.py), [manifest](../dependency-patches/manifest.json), [CI](../.github/workflows/ci.yml), [zor test gate](../tests/zor_integration.rs).

**Where the stack is strong, and what that does not imply**

fux has an explicit authoritative ECS model, bounded channels and history, private viewer selection, stale-target checks, and owned PTY lifecycle handling. Hidden and detached panes retain terminal state. These are implemented foundations, with meaningful deterministic and randomized tests. Separate observation and transport processes also allow those services to fail without making them the pane owner. This is useful modularity, but it requires compatible protocols and an actual consumer for observation reports.

koh's gateway authenticates endpoint IDs before connecting to the Unix service and uses bounded acknowledged frames to resume a disrupted link without replaying already-delivered input within the retained session. Its retention interval is **30 seconds**. This is not unlimited offline continuity or durable exactly-once delivery across gateway restarts. fux panes can continue after gateway loss, but reconnecting the same attachment is subject to that grace period. Evidence: [gateway admission](../references/koh/src/gateway/mod.rs), [resume framing](../references/koh/src/gateway/resume.rs), [session retention](../references/koh/src/gateway/sessions.rs).

The gateway carries opaque bytes. koh's standalone predictive shell, terminal-state synchronization, and bell-command hook should not be credited to the documented `koh gateway ...` plus `fux attach ...` composition. Nor is koh exclusive to fux: it can transport another compatible local service, or its standalone shell can host another multiplexer. Remote workspace choosing/creation is also reduced: explicit fux socket attachments have no manager socket, and the corresponding UI commands are unavailable. Evidence: [client options](../src/client/mod.rs), [action availability](../src/commands.rs), [koh standalone/library examples](../references/koh/README.md).

No performance winner is established. fux builds a complete visible-tab frame per dirty viewer and serializes it as JSON; the outbox coalesces replaceable frames, but there is no wire cell-delta message. zor currently polls at 100 ms and performs list/capture RPCs without its intended sequence cache. These are identifiable cost centers, not measured proof of slowness. Small source size and ECS do not establish better CPU use, latency, RSS, or bandwidth than the reference implementations. Evidence: [snapshot construction](../src/ecs/systems/snapshot.rs), [outbox](../src/server/adapter.rs), [wire messages](../src/proto/attach.rs).

**Practical gap priorities**

1. Repair and verify the composition: agree on the control preface, remove stale geometry assumptions, reconcile versioned patches, and make required integration tests part of ordinary combined-stack CI. Add observer fixtures covering edge-of-screen evidence and terminal resize.
2. Deliver usable agent observation: active rules and real-agent fixtures, then a consumer with explicit pane identity, observer lifetime, freshness and lost-state semantics. Keep detection in zor; decide whether presentation belongs in fux or a separate dashboard. Reports remain observations, not authentication.
3. Provide the agent workflow that differentiates herdr: structured list/status/events, bounded prompt delivery, and waits that distinguish new progress from stale idle/blocked output. fux's current `send-keys` plus `capture` is a building block, not that contract.
4. Close everyday multiplexer gaps: zoom, move/swap panes, searchable history, reusable layouts, and optional broadcast input. These are more directly useful to parity than adopting an unrestricted plugin system first.
5. Choose restoration deliberately: save workspace/layout/cwd/history and optionally supported-agent resume metadata. Restoring sessions means creating new processes; none of these products keeps arbitrary process memory executing through a machine reboot.
6. Measure remote workloads before changing rendering: idle, scrolling, full-screen TUI redraws, multiple viewers, and poor-link reconnects. Then evaluate shared snapshots/deltas and event-driven observation. Keep general graphics/plugin/browser features scoped to actual product goals.

**Verification performed**

| Command | Result |
|---|---|
| `cargo test --locked --lib --test ecs --test structure -- --test-threads=1` | Passed: 75 library + 19 ECS + 8 structural tests |
| `cargo build --manifest-path zor/Cargo.toml --locked --bin zor` | Passed |
| `env -u XDG_CONFIG_HOME zor/target/debug/zor agents` | Printed `no bundled agent rule sets` |
| `ZOR_BIN="$PWD/zor/target/debug/zor" FUX_REQUIRE_ZOR_BIN=1 cargo test --locked --test zor_integration -- --test-threads=1` | Failed: expected working-state report never arrived |
| `python3 tools/dependencies.py verify` | Failed: koh patch is stale |
| `FUX_BIN="$PWD/target/debug/fux" KOH_REQUIRE_FUX_BIN=1 cargo test --manifest-path references/koh/Cargo.toml --locked --test gateway` | Both tests failed during network-monitor initialization: `Operation not permitted` |

The koh test outcome is an environment restriction, not evidence that gateway admission or resume is incorrect. Runtime gateway behavior remains unverified here. No latency/bandwidth benchmarks, full terminal conformance suite, full reference-project tests, or live GitHub CI inspection were performed. Existing local modifications and prompts were preserved.
