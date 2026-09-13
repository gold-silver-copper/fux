# fux, koh and zor boundary audit

Audited on 2026-09-13: fux/zor commit `5997e22cc2f2969cb8778fba3c487f364a0f15a9` and the clean, pinned koh checkout `f6a335237c25aefde9b93290b19a8d909598a95d`. Koh conclusions concern that integration pin, not an assertion about upstream HEAD. This is a source and contract audit, not a new full integration or network test campaign.

## Verdict

The composed architecture has sensible ownership. No inspected fux production path implements agent orchestration or remote networking, and koh's gateway does not interpret application payloads. However, the strict descriptions “koh is transport only” and “zor is agent only” do not describe their entire shipped products.

| Program | Actual responsibility | Qualification |
|---|---|---|
| fux | Local PTYs, process groups, terminal emulation, history, layouts, viewers, generic control, input receipts and final process evidence | Multiplexer only in the architectural sense. Its automation primitives are part of owning terminals, not agent policy. |
| koh | Remote identity, authorization, encrypted connectivity, reconnect and opaque local-service forwarding | Also ships a standalone remote shell with PTYs, terminal state synchronization, rendering and predictive echo. It is mosh-inspired, not SSH/mosh wire compatible. |
| zor | Agent observation, provider adapters, task orchestration, checks, artifacts, worktrees and recovery policy | Also has generic `zor run` and a standalone PTY wrapper. “Agent and task workflow owner” is the more accurate scope. |

The relevant rule is one authoritative owner per resource in a composition. A standalone koh shell owning its own PTY is legitimate; koh must not also own a fux pane's PTY or terminal state when operating as its gateway. Similarly, zor may spawn adapter/check subprocesses without taking ownership of fux pane processes.

## Existing strengths

- `crates/fux/Cargo.toml` has no koh/zor/iroh dependency. The application crates communicate through process protocols; `local-ipc` contains generic private socket, peer credential, framing and deadline mechanics, without task semantics.
- `crates/fux/tests/agent_boundary.rs` inventories production declarations and rejects recognizable agent/transport/Git semantics. It also tests that OSC 7877 agent reports do not affect fux terminal state.
- `crates/fux/tests/structure.rs` checks dependency direction, approved spawn owners and ECS purity. `protocol_consumers.rs` inventories wire items and recorded consumers.
- `crates/zor/src/fux/{manager,input}.rs` already provide typed envelopes, receipts and selected manager operations. `tasks/route.rs` distinguishes immutable process/origin identity from a movable workspace route.
- `references/koh/src/gateway/mod.rs` authorizes a remote peer before accessing the local application. `gateway/sessions.rs` authenticates the local socket peer; resume state belongs to the connection, not the application model.
- `references/koh/tests/gateway.rs` includes opaque-byte forwarding, gateway failure preserving fux panes, and separately authorized zor-control coverage. Test presence is evidence of intended behavior; these tests were not rerun in this audit.

Generic fux primitives should remain in fux: bounded process final records, input write receipts, output cursors, pane location, layout controls and pins. Zor should decide how long evidence is needed, whether an agent accepted a prompt, whether a task succeeded and whether to retry. A delivered input receipt must never become proof of agent acceptance or task completion.

## Findings and improvements

### 1. High priority: complete zor's typed fux boundary

`crates/zor/src/fux.rs:20` exposes arbitrary JSON requests. Its `completed` helpers check the status string but do not themselves validate the complete operation-specific envelope. `observe.rs`, `watch.rs`, `tasks/launch.rs` and `run.rs` still construct and interpret protocol JSON directly. The newer typed manager/input layer therefore covers only part of the interface.

Move discovery, workspace creation, list, capture, event subscription, split, focus, kill and info behind typed client operations. Validate reply kind, request identity, server identity and required fields at that boundary. Make raw exchange private. Return typed observations and explicit unavailable/stale/unknown outcomes to policy code. Do not merely replace `Value` with a struct containing a `Value` field.

Keep independent producer/consumer fixtures and malformed-reply tests. A shared schema crate is optional, not the first fix: it must contain only wire DTOs if introduced, and it must not make zor link the fux runtime or make koh understand fux payloads.

### 2. High priority: remove the second RPC implementation

`crates/zor/src/run.rs:390` implements its own exchange loop even though `fux.rs` already owns authenticated, bounded RPC. It sets a socket read timeout once before `BufReader::read_until`; that bounds an individual blocking read rather than the total time needed to finish a frame. A peer delivering partial bytes can potentially extend the operation past the intended absolute deadline. This is a code-derived deadline concern, not a reproduced failure in this audit and not an explanation of the earlier historical pressure timeout.

Route this path through the common absolute-deadline reader/writer. Add a slow partial-frame test that distinguishes an absolute deadline from repeated read timeouts, plus EOF/oversize/wrong-envelope cases. Keep command timeout and cleanup ownership in `zor run`; move framing out.

### 3. Medium priority: make the standalone wrapper an explicit product choice

`crates/zor/Cargo.toml` defaults to `cli` plus `wrap`. `lib.rs` correctly gates PTY/emulator modules behind `wrap`, and the composed integration build explicitly disables it. Nevertheless, a default build or install includes the wrapper. The README's “off-by-default” wording contradicts its manifest and later installation explanation.

For a strict default composition, make `wrap` opt-in, or separate it into a clearly named adapter executable/package. Also remove `portable-pty` from the CLI-only dependency path: currently `platform::{linux,macos}::winsize` returns `portable_pty::PtySize`, used by the dashboard even without the wrapper. Introduce an ordinary terminal-size value and convert it only at the wrapper boundary. This is dependency coupling, not evidence that the CLI-only dashboard spawns PTYs.

### 4. Medium priority: isolate koh's gateway from its remote-shell implementation

Koh's runtime gateway is generic, but its package also unconditionally depends on `portable-pty` and `vt100`, and exposes shell hosting, rendering, prediction and generic embedding APIs. Thus transport-only use does not currently imply a transport-only build.

Keep the standalone remote shell if desired, while introducing a gateway/core feature or package that cannot import PTY allocation, terminal emulation or rendering. Make opaque gateway forwarding the documented fux integration. The pinned koh README still describes fux as an embedding consumer, whereas current fux explicitly uses process protocols; reconcile that documentation upstream and then update the pin.

### 5. High priority for remote work: enforce cross-product contracts in CI

`.github/workflows/ci.yml` runs koh integration only through `workflow_dispatch` with the integrations input. Ordinary pushes and PRs can therefore pass without exercising the pinned gateway. Fux's structural test also deliberately keeps external koh dependencies out of ordinary standalone verification.

Preserve a standalone fux build, but add a distinct required composition job for relevant protocol, socket, authorization and companion-pin changes. Adjust the structural test to permit that explicit job. Require the real binaries and exact clean pin, and prohibit silent skips. Test loss/reconnect, replay boundaries, service death and separate authorization to attachment versus zor control. Fast ordinary local tests need not all depend on koh.

### 6. Medium priority: distinguish architectural checks from proof of isolation

Current checks are valuable but partly lexical. Declaration snapshots do not encode all function-body semantics; wire-name mentions do not prove meaningful consumption; dependency denylists do not enumerate every forbidden capability. Koh checks are skipped when its optional checkout is absent.

Add explicit import/capability rules: network ownership in koh, pane spawning in fux's PTY owner, provider semantics and durable task/Git state in zor. Use Cargo metadata across supported features/targets for dependency assertions and parsed source rules for module direction. Require executable positive/negative contract tests for each new public operation, alongside the consumer inventory. Inventory updates should explain ownership and consumers, not simply regenerate snapshots.

### 7. Medium priority: formalize remote application endpoints before multi-machine orchestration

Zor currently constructs local manager/workspace socket paths in `watch.rs`, `tasks/launch.rs` and `run.rs`. This is documented coupling, not an unauthorized filesystem intrusion, but it makes remote supervision harder to introduce cleanly.

Have koh expose a generic authorized service connection with transport identity and connection status. Have zor's client layer map it to a typed remote zor service; keep local pane discovery on the machine that owns fux. A remote reference should distinguish machine/service instance and task/process identity. Koh reconnect must not decide whether to resubmit a prompt, recreate a pane or restart an agent. Zor must reconcile application evidence before making those decisions.

Do not treat attachment permission as observation-only permission: an interactive terminal can execute commands. Separate endpoint authorization distinguishes application services; it does not sandbox a user who already controls a shell.

## Recommended order and acceptance

1. Complete the typed client and consolidate deadlines; verify malformed replies and partial reads fail at the client boundary.
2. Clarify feature ownership and isolate zor's wrapper and koh's gateway dependencies; verify minimal builds without PTY/emulator dependencies where appropriate.
3. Require the pinned composition gate for boundary changes and strengthen architectural tests.
4. Introduce explicit remote service references as part of multi-machine supervision, preserving each owner's restart/retry authority.

For acceptance, require that stopping zor preserves fux panes; stopping koh preserves both local services; moving a pane preserves exact process identity; lost input replies do not cause blind resubmission; agent reports never affect fux policy; and gateway bytes remain opaque. These include existing contracts to preserve, not claims that all need new implementations.

## Verification performed

Fresh targeted command:

```sh
cargo +stable test -p fux --locked \
  --test agent_boundary --test structure --test protocol_consumers --test fixtures \
  --target-dir /tmp/fux-codebase-work/build
```

Result: **18 passed, 0 failed, 0 ignored**. Log: `/tmp/fux-boundary-audit-20260913.log`.

No product code or companion checkout was modified. This audit does not claim a complete security review, live remote-network verification, or coverage of every feature configuration.
