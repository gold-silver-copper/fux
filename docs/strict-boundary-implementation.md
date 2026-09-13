# Strict fux, koh and zor boundaries

Status: local implementation and verification complete. The full 41-check gate passed,
including rendering 412 Betamax frames with unchanged source identity. After authorization,
koh was published to main at `da712875e4f527b718abe44e9d68f94048e916c7`. The companion pin and
gateway-only CI commands now use that published revision. Hosted CI is tracked separately.

## Scope and provenance

The implementation follows `strict-fux-koh-zor-boundaries-prompt.md` and validates the
findings in `boundary-audit-2026-09-13.md` against current producers and consumers.
Fux/zor started at `5997e22cc2f2969cb8778fba3c487f364a0f15a9`. The clean koh reference
was originally pinned to `f6a335237c25aefde9b93290b19a8d909598a95d` from
`https://github.com/gold-silver-copper/koh.git`. The user subsequently authorized commits and pushes; koh is published and this workspace
includes the corresponding pin/CI activation. No release or PR was created.

Koh changes live in a separate development checkout and are delivered as
[an applicable patch and reproduction instructions](verification/strict-boundaries/README.md).
The patch includes new files, applies to the exact published base, and was reconstructed
byte-for-byte in a fresh checkout whose gateway all-target build passed. Its SHA-256 is
`d609b90aa488245d92ab85a8b35946dc3ee251c46117fa21ab6d9a67fee40602`.
[Provenance](verification/strict-boundaries/koh-provenance.json) records the 135-file source
inventory and reconstruction. The historical patch reproduces the prepublication source;
the published revision additionally incorporates upstream CI-only changes. The reference
checkout is clean at the new published pin.

## Final ownership and feature matrix

| Component/build | Owns | Excluded capabilities |
|---|---|---|
| fux default | Pane PTYs/processes, terminal state/history, layouts/viewers, exact pane routes, generic receipts/events/final evidence | Remote transport/identity and agent/task/provider policy |
| zor default or explicit `cli` | Tasks, providers, checks/artifacts, worktrees, supervision, application reconciliation, dashboard | Pane PTY allocation and terminal emulation; production closure has no `portable-pty` or `vt100` |
| zor no default features | Protocol/library surface | CLI/wrapper runtime capabilities |
| zor `wrap` | Explicit standalone wrapper plus CLI | Does not gain ownership of fux pane lifecycles |
| koh `gateway` or `cli,gateway`, no default features | Identity/authorization, encrypted opaque forwarding, bounded transport resume | Shell modules, PTY allocation, emulator, prediction/rendering and terminal backends |
| koh default | Standalone shell plus gateway, preserving the existing product | No fux/zor domain imports or application recovery policy |
| local-ipc | Private local sockets, peer credentials, bounded framing/deadline mechanics | Application wire schemas and task/remote policy |

Zor defaults changed from wrapper-enabled to CLI-only. Install with `--features wrap` to
retain standalone wrapper commands. The dashboard now uses a plain `TerminalSize`; only
wrapper code converts it into `portable_pty::PtySize`. Provider/check subprocesses remain
legitimate zor-owned resources. Koh's separate shell remains functional.

[The service contract](service-ownership-contract.md) distinguishes endpoint location,
transport principal, server incarnation, movable workspace route and immutable process
origin. It documents independently authorized remote zor control through koh without
inventing a multi-machine controller. Input delivery, native agent acceptance and verified
task success remain separate evidence states.

## Typed client migration

All production zor consumers now use the typed fux client. Raw request exchange, envelope
construction/decoding and fux control-socket naming are private to that boundary.

| Operations | Client owner | Consumers / retained policy |
|---|---|---|
| Manager names, create, CLI creation descriptor | `fux/manager.rs` | Watch/discovery, launch/run; create-only ownership and cold-start stream authority stay in policy |
| Info and server incarnation | `fux/info.rs` | Launch/run replacement detection |
| Workspace list and pane metadata | `fux/snapshot.rs` | Observe/watch, adoption, launch/resume, target selection and worktree census |
| Cell capture, text input counter | `fux/capture.rs` | Observation/rules and input barriers; rules consume normalized rows rather than fux JSON |
| Event replay and subscriptions | `fux/events.rs`, `fux/subscription.rs` | Launch reconciliation, watcher invalidation and event continuity |
| Split, focus, pane kill, workspace kill | `fux/pane.rs` | Task/run policy decides authority; response validation follows each producer's actual result variant |
| Locate, pin release, retained final records and input status | `fux/manager.rs` | Exact-process route, retention and unknown-outcome reconciliation |
| Input reservation/submission | `fux/input.rs` | Typed delivery receipts; application retry/acceptance remains in task policy |
| Manager/workspace socket paths | `fux/endpoint.rs` | All fux consumers; names are validated centrally |

Generic JSON remains legitimate in zor service messages, adapters, persisted data, output
and private wire helpers. No arbitrary JSON request/reply API is exported to task policy.
Zor's own service/provider sockets call `local-ipc` directly instead of borrowing fux
transport helpers. Capture normalization remains a neutral rules-view utility.

Validation covers reply variants, correlation IDs where present, required payload values,
server/workspace/event identity, bounded snapshots/captures and operation-specific results.
Transport IO/deadline/closed failures, malformed responses, validated remote failures,
uncoded refusals and stale identity have distinct provenance. Pending final evidence stays
an explicit enum. Only a validated remote failure exposes retry-relevant error codes.

## Deadlines and preserved authority

The controlled slow-peer regression demonstrated the old run exchange accepting a response
after a 300 ms deadline: one byte every 30 ms extended the operation to 1.38 seconds.
The consolidated client stops at the absolute deadline; the regression passed in 0.30 seconds.
Connect, negotiation, writes and full reply completion share the caller deadline with
existing phase caps. Progress cannot renew it. Idle subscriptions are allowed; an incomplete
head frame has a fixed two-second completion deadline. Complete buffered frames do not
expire while another peer is serviced.

Exact instance/pane/PID and immutable origin checks remain in route/task policy. A changed
route does not authorize a replacement process. Lost create/input replies remain uncertain
until retained evidence reconciles them. Run cleanup is limited to the resources whose
creation and identities it owns; typed transport does not broaden destruction authority.

## Enforcement and CI

`fux-xtask verify-boundaries [--koh CHECKOUT]` uses resolved Cargo package identities,
including renamed dependencies, across Linux x86_64, macOS arm64 and Android arm64.
It checks fux, zor default/CLI/protocol/wrap and optional koh gateway-library/gateway-CLI/
default-shell closures. These are dependency-resolution checks for all three targets;
local runtime/build evidence is macOS, not a claim of executing all target platforms.

Parsed Rust guards reject public arbitrary-JSON client surfaces and client imports of
application policy, including grouped/renamed paths. Existing fux agent/import/spawn
ownership checks remain. Independent producer and consumer fixtures cover creation,
listing/info/capture, split/focus, event replay and retained operation contracts; declaration
inventory changes assign wire interpretation to the client and preserve policy consumers.

Ordinary push/PR CI now runs a distinct unconditional pinned composition job with exact
binaries, required-binary flags, gateway reconnect/integration tests, shell/admission tests
and a final clean-pin check. Standalone fux builds/packages remain independent of koh.
The job now uses the published koh revision with `--no-default-features --features cli,gateway`
and verifies its gateway-only dependency matrix. Standalone shell tests retain default features.
No local-only pin, dirty reference bypass or branch-protection change is involved; hosted
CI status is separate from the local verification results.

## Separate review and corrected findings

A separate self-review covered the implementation diff and new client/guard/fixture files,
run cleanup and task authority, normalization and subscription behavior, koh feature/module
ownership, documentation and CI. No independent agent review was performed.

Confirmed findings fixed and reverified:

- The slow-partial-response deadline gap described above.
- Workspace kill returns a named Workspace result, not Unit. The client now checks that
  exact workspace name; real run cleanup passed.
- Pane focus returns Pane, not Unit. The initial full gate exposed the dashboard reporting
  a failure after successful focus. The client now checks the requested pane; paired
  fixtures, four pane tests and the real dashboard passed without weakened assertions.
- Subscription refusal was classified as malformed. Validated failures now retain their
  own category; wrong IDs/missing fields cannot forge a remote failure code.
- Wrapper helper placement broke all-feature strict clippy; moved it before the test module.
- Shell/CLI-only rustdoc links broke minimal koh docs; feature-qualified wording now permits
  both gateway library and CLI documentation with warnings denied.

Rejected concerns: removing the redundant listing before input-counter capture does not
remove the manager route's exact PID/instance/origin checks. Fux's own PTY dependency and
zor's provider/check subprocesses are intentional ownership, not boundary violations.
The historical unexplained pressure timeout is not claimed fixed or made new scope.

## Verification

The final [gate record](verification/strict-boundaries/full-gate/gate.json) contains all
41 exact commands, per-command exit status and source identities. Every check passed; the
Betamax report indexed 412 frames. Workspace tests totalled 538 passed, zero failed and two
ignored optional installed-Codex probes. Zor all-feature library tests additionally passed
181 tests (the same two probes ignored); protocol and explicit CLI configurations passed.
The authenticated Codex/Claude and performance evidence verifiers validate retained traces
and accounting, not new authenticated model calls or a new release benchmark campaign.

The full gate was run from this repository root with:

```sh
PATH=/tmp/fux-betamax-toolchain/lib/python3.14/site-packages/ziglang:$PATH \
CARGO_TARGET_DIR=/tmp/fux-codebase-work/build \
FUX_BETAMAX_FONT='Noto Sans Mono CJK SC' \
cargo +stable run --locked --manifest-path tools/xtask/Cargo.toml \
  --target-dir target/rust-harness -- verify-codebase full
```

The PATH entry supplies the installed Zig toolchain for Betamax in this environment.
The gate runs workspace/xtask formatting, strict linting across feature sets, binary builds,
workspace and feature tests, fixture-child tests, documentation, fux/zor/local-ipc packages,
adapter tests and retained-evidence verifiers. Full logs are retained beside the gate record.

| Additional verification | Result / retained log |
|---|---|
| `verify-boundaries --koh /tmp/strict-fux-koh-zor/koh-dev` | All three-target feature closures passed; `dependency-matrix.log` |
| Final koh `cargo +stable test --locked --no-default-features --features cli,gateway --lib --test gateway -- --test-threads=1` | 80 library + 3 real-service tests passed; `koh-final-composition.log` |
| Koh default `cargo +stable test --locked --test pty --test admission --test e2e_loopback --test e2e_pty_binary -- --test-threads=1` | 17 tests passed; `koh-shell-tests.log` |
| Koh default and gateway CLI strict all-target clippy | Passed; `koh-shell-clippy.log`, `koh-gateway-clippy.log` |
| Koh gateway CLI/default `cargo +stable package --locked --allow-dirty` with corresponding feature flags | Both package verifications passed; `koh-package-final.log`, `koh-shell-package.log` |
| Koh gateway library and CLI `RUSTDOCFLAGS='-D warnings' cargo +stable doc --locked --no-deps` with corresponding feature flags | Passed; `koh-library-doc-final.log`, `koh-gateway-doc-final.log` |
| Fresh patch reconstruction `cargo +stable check --locked --no-default-features --features cli,gateway --all-targets` | Passed; `final-reconstruction.log` and patch provenance |
| Clean old pin: `cargo +stable test --locked --test gateway`, `--lib gateway::`, and shell/admission/loopback commands | 3 gateway + 15 reconnect + 16 shell/admission tests passed; `pinned-*.log` |
| `target/rust-harness/debug/fux-xtask dependencies verify` | Passed after pinned tests; `pinned-provenance-check.txt` |
| Supplemental existing-attachment survival test | Passed; `zor-stop.log` and `verify-zor-stop.py` |
| Full implementation review and final whitespace check | No remaining confirmed in-scope issue; `git diff --check` passed in fux and koh |

Koh commands use `--manifest-path /tmp/strict-fux-koh-zor/koh-dev/Cargo.toml` (or the clean
reference manifest for pinned tests) and a separate Cargo target directory. The final
minimal and pinned composition use the immutable binaries recorded with hashes in
[pinned-composition-inputs.json](verification/strict-boundaries/pinned-composition-inputs.json),
with `FUX_BIN`, `ZOR_BIN`, `KOH_REQUIRE_FUX_BIN=1` and `KOH_REQUIRE_ZOR_BIN=1` set.
[The handoff](verification/strict-boundaries/README.md) contains portable reproduction
commands, including the precise features for both packages.

The gate's before/after source fingerprints match. After it finished, this report and a
historical-proposal notice were updated. The publication step then advanced the companion
pin and enabled gateway-only CI; targeted structure, dependency and composition checks
verify that publication delta. Runtime code is unchanged from the passing full gate.
The notice links current ownership instead of presenting the old OSC/embedding proposal as
current behavior. No runtime code changes followed the passing gate.

## Behavioral acceptance evidence

| Prompt requirement | Current evidence |
|---|---|
| 1. Zor loss preserves panes and local attachment | Workspace observer scenario checks exact pane PID after killing zor observe. `verify-zor-stop.py` additionally keeps one real attachment open across terminating zor serve and reads the same shell's retained token afterward. Passed with final binary hashes. |
| 2. Koh loss preserves fux/zor | `optional_gateway_failure_leaves_real_fux_panes_running` reads retained shell state locally after gateway shutdown; separate real-zor authorization test shuts down every gateway and asserts both services remain live. |
| 3. Authorization precedes local service access; endpoints separate | Gateway byte/authentication fixture plus `attachment_peer_cannot_use_separately_authorized_real_zor_control`; attachment-only peer denied, distinct control peer receives service instance. |
| 4. Opaque bytes and reconnect deduplication | Byte-for-byte gateway test, resume offset/ACK tests and real fux input applied once across five actual QUIC losses. |
| 5. Expired state does not resurrect applications | Real retained-session expiry and registry tests reject expired resumption and preserve the application; opening a fresh attachment is an explicit separate operation. |
| 6. Relocation preserves exact identity; stale targets rejected | Workspace control transfer/instance/stale-target tests and zor route/movement/launch scenarios. |
| 7. Lost mutation replies reconcile | Real launch scenarios cover lost creation/release replies, transient unknown outcomes and no duplicate launch; retained input receipt/reconciliation tests cover delivery across relocation/lost replies. |
| 8. Delivery, acceptance and success remain separate | Zor native/check/workflow scenarios and retained evidence state tests; typed client supplies process evidence without task interpretation. |
| 9. Agent OSC has no fux semantics | `agent_osc_reports_have_no_semantic_effect_on_the_multiplexer` and existing terminal/control tests. |
| 10. Absolute deadlines and malformed replies | Controlled slow partial peer, partial preface/frame, oversize, closed-buffer draining, malformed JSON/envelope and remote-code provenance client tests; subscription idle/partial deadline tests. |
| 11. Minimal/standalone feature capabilities | Three-target resolved closure matrix; macOS feature lint/tests/builds; koh gateway and standalone shell runtime/package/docs checks. |

The supplemental stop test is reproducible without external services:

```sh
python3 docs/verification/strict-boundaries/verify-zor-stop.py "$FUX_BIN" "$ZOR_BIN"
```

Visual review examined the retained Betamax dashboard focus, 20-column/3-row resize and
service-loss checkpoints. Normal focus confirmation is readable; the tiny viewport clips
to its deliberately restricted size; unavailable service is marked STALE with a recovery
message. The real dashboard scenario also checks terminal mode/cursor restoration. These
three images are scoped visual evidence, not a claim of manually inspecting every frame
or proving all possible layouts.

## Publication verification

The published koh pin passed the three-target dependency matrix, 12 structure tests,
15 reconnect tests and all three gateway integration tests against the retained exact
fux/zor binaries. Logs are in `verification/strict-boundaries/publication/`. The only
upstream change included during the koh rebase was its existing real-fux CI pin update.
