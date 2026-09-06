> Historical record from before the 0.3.0 bevy_ecs rewrite (2026-09-05). The host, popup panes, sidecar supervision, protocol v2/`FUXCTL1` and verification results described here no longer exist. Current architecture: [design.md](design.md); current evidence: [ecs-acceptance.md](ecs-acceptance.md).

# Standalone implementation plan

## Scope and preservation

Implement `standalone-refactor-prompt.md` in full. Preserve the existing koh handshake cleanup changes and personal sessions. No commits, pushes, package publication, or personal key changes.

## Sequence

1. Extract multiplexer-owned terminal emulation, rendering primitives, state operations, and host interfaces from koh dependencies. Preserve MIT attribution. Keep networking and agent detection in their owner projects.
2. Replace the local QUIC attachment path with bounded, versioned Unix socket attachment. Authenticate OS peers, retain startup locking and private paths, and preserve multi-viewer lifecycle and control commands.
3. Remove local identity provisioning and network configuration from fux. Start one local server on demand; preserve process lifetimes on detach and report incompatible servers safely.
4. Remove koh and zor from fux's dependency graph and clean-checkout assembly. Make observation explicitly opt-in through a documented bounded interface.
5. Implement koh's optional authenticated gateway using the local interface; verify disconnection and reconnect without restarting sessions.
6. Update tests, CLI help, packaging, CI, and migration documentation. Exercise isolated real binaries and perform a requirement-by-requirement completion audit.

## Verification strategy

Use targeted checks during extraction, then standalone unit/integration and binary tests. Verify source assembly without sibling repositories, dependency graph, socket access and framing limits, startup races, same-process reattach, multiple viewers, terminal restoration, lifecycle cleanup, and no local network listeners or key creation. Test optional gateway and observation failures separately. Record macOS verification and explicitly distinguish other platforms' compile checks from runtime coverage.

## Current evidence

Initial inspection: default Cargo dependencies include koh and zor. Local attach loads the shared koh identity and each workspace binds a koh endpoint. Terminal emulation, backend rendering, host traits, and workspace synchronization also import koh, so removing the startup prompt alone cannot meet the goal.

## Progress: terminal ownership and local authorization

- Fux now owns its VT100 emulator and snapshot model (`src/terminal`), with the network synchronization fields omitted from pane snapshots. The existing host uses this emulator.
- Fux now owns its terminal backend and bounded stdin/SIGWINCH producers (`src/client/backend`, `src/client/io`). Existing rendering and input behavior use those modules.
- Control socket acceptance now validates OS peer credentials; private-directory ownership is checked against the effective UID. Directory creation uses mode 0700 from creation, and stale recovery propagates ambiguous connection errors instead of unlinking.
- Preserved MIT attribution in the extracted modules and existing `LICENSES/koh.txt`.
- Verified on macOS: 53 library tests, 11 control tests, 43 host tests, and 20 client tests passed. Strict all-target/all-feature Clippy, formatting, and diff checks passed. Logs: `/tmp/fux-standalone-core.log`, `/tmp/fux-standalone-render.log`, `/tmp/fux-standalone-core-clippy.log`.

The goal is not yet complete: local attachment still uses koh, the default dependency graph still includes both owner crates, and gateway/observation integration, standalone packaging, CI, and full acceptance verification remain. Next replace synchronization/host/client interfaces and the attachment lifecycle with the versioned local socket contract. No personal sessions or keys were changed.

## Progress: local attachment and key-free startup

- Added bounded, versioned local framing and authenticated Unix socket server/client in `src/local`. The production binary now attaches through these modules rather than koh endpoints.
- Replaced network descriptor fields with local socket path and protocol version. Removed identity provisioning from startup, workspace creation, and attachment, and removed fux's key-management CLI/module. Startup exchanges a fixed `LOCAL/1` marker rather than key material.
- Removed network/allowlist configuration from fux. Observation executable configuration is now optional and defaults to absent; default panes start bare.
- Added explicit final-exit messages and preserved version mismatch errors without restarting existing sessions. Fixed JSON's internally-tagged buffering incompatibility with typed numeric pane-map keys by using externally tagged server responses, with a populated-state regression test.
- Verification: `cargo test --lib --bin fux --locked` passed 56 library and one binary unit tests; strict library/binary Clippy, formatting, and diff checks passed. `tests/verify/local_attachment.py` passed version rejection, two-viewer state, input, same shell PID after reattach, no key creation, and no server Internet sockets. `tests/verify/local_tty.py` passed real terminal cold startup and detach while preserving the server. All binary tests used isolated temporary runtime/config/state directories and were cleaned up.
- The old network-oriented integration tests have not yet been migrated and the full all-target suite is not claimed green. Koh/zor remain compile dependencies through core traits and observation types; removing those imports and adapting the tests is the next step. The optional koh gateway, full observer failure semantics, packaging, CI, and completion audit remain unfinished.

## Progress: independent dependency graph

- Removed koh and zor from Cargo dependencies. No `koh::` or `zor::` imports remain in production or library-unit sources. Runtime dependency inspection excludes koh, zor, iroh, quinn, and rustls.
- Added fux-owned host lifecycle, viewer IDs/change notifications, rendering interfaces/visual overlays, and inherent workspace diff/resource operations. No transport or prediction engine was copied into fux.
- Added a dependency-free adapter for zor's bounded OSC v1 schema in `src/observation`, preserving its MIT attribution in `LICENSES/zor.txt`. This is a consumer adapter, not agent detection or observation generation.
- Migrated client, host, config, runtime, security, and chaos checks. Replaced host QUIC attachment tests with real local socket attachment/final-exit and same-shell reattach tests. Replaced the core's lossy-network simulator check with its relevant state-convergence check; remaining network corpus tests still require migration.
- A disposable source directory containing only Cargo manifests, src, README, and licenses built offline and passed 56 library + one binary tests without either sibling repository (`/tmp/fux-standalone-clean-build.log`).
- Local `cargo package --locked --allow-dirty --offline` succeeded, including compilation from the unpacked package (`/tmp/fux-standalone-package.log`). No package was published.
- Targeted integration results: 43 host, 20 client, 11 config, 13 runtime, 11 control, four security, and two chaos tests passed. Strict Clippy passed for those targets plus library/binary. Both isolated real-binary local attachment/TTY scripts passed against the standalone binary.
- Remaining work: migrate daemon and verification-corpus/fixture tests, update standalone CI/docs/assembly, implement independent optional koh gateway, redesign observer ownership so observer failure cannot terminate pane commands, then complete full negative-path/platform/requirements verification. Full all-target checks currently fail on obsolete test APIs; the overall goal remains active.

## Progress: observer process separation

- Fux now spawns pane commands directly even when observation is configured. It starts `zor observe` as a separate process after the local control socket is configured, with the original pane PID and ID.
- Zor owns a new optional local-capture adapter (`zor/src/observe.rs`) using bounded capture/list RPCs, peer UID checks, absolute response deadlines, process identification, and its existing rule/state machine. It never owns or signals the observed pane. Pane summaries carry terminal progress metadata for the observer alongside existing title and geometry.
- Fux bounds report lines and report frequency, rejects malformed output, and owns/reaps the observer process group. Reports are applied as metadata rather than injected as arbitrary terminal bytes. Observer loss clears stale live status; shutdown kills observers before joining their report readers.
- Isolated real-binary checks passed for crashed, stalled, malformed, oversized, and partial-report observers. Real zor reports driven by progress/title rules passed, and killing the real observer left the same pane PID and usable capture interface. Reaping was verified after shutdown. These checks live in `tests/verify/observer.py`, with a Rust integration wrapper in `tests/zor_integration.rs`.
- Affected fux unit/host/runtime/control tests and targeted Clippy passed. Zor's complete all-feature test suite and strict all-target Clippy passed. The final geometry field correction was rebuilt and all six isolated observer scenarios passed again.
- Still unfinished: optional koh gateway, remaining daemon/corpus/fixture migrations, CI/packaging documentation cleanup, full all-target verification, and the requirement-by-requirement completion audit. No personal sessions or keys were touched, and no commits or publication occurred.

## Progress: optional authenticated gateway baseline

- Added koh-owned generic Unix-service gateway APIs and `koh gateway serve/connect` commands. The gateway has no fux or zor crate dependency. It validates local paths and kernel peer credentials and checks the remote allowlist before opening the fixed local target. It bounds connection/handshake tasks and uses streaming backpressure.
- Added `fux attach --socket PATH`, which attaches to a local socket without identities or remote transport types. Workspace picking is disabled for an explicit service socket.
- Gateway regression tests verify a denied peer never reaches the local application, a 64 KiB byte-exact round trip, and real fux shell-state preservation after shutting down both gateways. The real-fux test was explicitly run with `FUX_BIN` and `KOH_REQUIRE_FUX_BIN=1` (`/tmp/koh-gateway-real-tests.log`).
- Fixed a final-byte truncation bug found by the forwarding test: after FIN is queued, the gateway awaits stream acknowledgement before closing the connection.
- Koh's full all-feature test suite passed, along with strict gateway/library/CLI Clippy. Fux's updated binary builds, its explicit-socket CLI help is verified, and targeted Clippy passed. Existing uncommitted koh handshake cleanup remains intact.
- This is a transport baseline, not completed remote resiliency: failed remote connections currently end their local attachment. Transparent reconnect without replayed input remains required. The gateway contract explicitly records this limitation in `references/koh/GATEWAY-CONTRACT.md`.
- Next work: resumable gateway connections, remaining daemon/verification-corpus/fixture test migrations, standalone CI/documentation cleanup, and full completion audit. No commits, pushes, publication, or personal-session/key changes.

## Progress: local daemon verification migration

- Migrated all 16 daemon tests to the public fux API, local socket descriptors, keyless startup payload, and viewer limits. Retained private-path, stale-file, simultaneous-startup, descriptor nonce, startup cancellation, and workspace lifetime coverage.
- Real endpoint tests verify protocol rejection, successful attachment after rejection, socket removal after workspace termination, and bounded eventual cleanup of accepted tasks after cancellation.
- `cargo test --test daemon --locked` passed all 16 tests; `cargo clippy --test daemon --locked -- -D warnings` passed. Logs: /tmp/fux-daemon-migration.log and /tmp/fux-daemon-clippy.log.
- Updated the corpus interpreter daemon/terminal APIs. Moved the deterministic transport fault and divergent-input matrix into koh tests, preserving assertions. `cargo test --manifest-path references/koh/Cargo.toml --test transport_fault_matrix --locked` passed.
- Full fux all-target compilation remains unfinished: the corpus interpreter still calls koh transport simulation. Its network scenario/schema/golden ownership needs migration, followed by remaining fixture/structure tests and full checks. Gateway reconnect, CI/docs cleanup, and completion audit remain outstanding. No commits, pushes, or personal session/key changes.

## Progress: standalone root verification passes

- Removed network-fault steps from the fux scenario schema, interpreters, and golden corpus. Preserved the actual loss, duplication, reversed-fragment reconstruction, and reconnect assertions in koh's transport_fault_matrix test; both koh tests pass. Local reattachment and pane persistence remain covered by real fux host/CLI tests.
- Replaced deleted identity-command tests with automatically run isolated local attachment and TTY scripts. These assert zero key creation/prompts, multiple viewers, same shell PID after reattach, and cold startup/detach. The separate observer consumer test no longer claims a zor build dependency.
- Strengthened dependency guards to forbid koh and zor in fux, retaining recursive dependency-table/alias checks. Optional sibling source checks run only when those repositories exist. Recorded lifecycle test evidence for the new spawn-owner modules.
- Removed koh from the fixture-child manifest and regenerated its lockfile offline; the separate fixture suite compiles. Its binary runtime scenarios still use the removed id/allow CLI and old wrapper semantics and need migration before this layer is complete.
- `cargo test --all-targets --locked --no-fail-fast` passed 215 tests across 16 targets (/tmp/fux-all-tests-migrated.log). Strict all-target/all-feature Clippy passed after moving host test items after production definitions and simplifying a single-element assertion (/tmp/fux-all-clippy.log).
- Remaining: fixture binary migration, independent standalone CI/packaging/docs, gateway reconnect, final negative-path/platform validation and completion audit. Existing CI still assembles siblings; passing its current structure assertions is not evidence of standalone CI. No commits or pushes.

## Progress: standalone fixture, CI, and package verification

- Migrated fixture binary scenarios from id/allow CLI and zor wrappers to local serve and direct pane processes. Direct pane PID equality/death replaces the obsolete wrapper-parent expectation; optional observer failure is covered separately by observer.py. Updated stalled-startup exchange coverage to LOCAL1. All 14 fixture tests and strict all-target fixture Clippy pass.
- Default CI, nightly, Android cross-check, and release jobs no longer checkout or assemble sibling repositories. A manual integrations switch enables the optional owner-repository job, including real zor and koh gateway checks. Added structure guards preventing sibling requirements from returning to default jobs. Workflow YAML parsed successfully with Ruby YAML; hosted CI has not been run.
- Exported and verified current koh/zor patches against pinned bases, preserving the pre-existing koh handshake fix. These are now integration-only; Cargo excludes patches and the assembly tool from its package. Unified-diff context whitespace is explicitly allowed for generated patch artifacts.
- The release verifier packages and installs only fux, then runs the complete eight-test binary corpus against the installed binary. It passed with --allow-dirty for this uncommitted worktree (/tmp/fux-standalone-release.log). README and security guidance now describe local credentials, sidecar ownership, explicit gateway access, and safe old-server migration. Gateway reconnect remains explicitly incomplete.
- A disposable source checkout with no references or zor directory passed root and fixture tests offline (230 tests across 20 targets), including an independent local binary build (/tmp/fux-clean-all-tests.log). Latest CI guards pass; affected Clippy and diff whitespace checks pass.
- Remaining: gateway reconnect, remaining obsolete documentation/tool claims, comprehensive negative-path/platform verification, and the requirement-by-requirement audit. Package verification preceded the latest documentation-only edits; final artifact checks must be repeated at completion. No commits, pushes, publication, or personal session/key changes.

## Progress: koh resumable forwarding foundation

- Replaced raw gateway copying with version-2 acknowledged frames and an internal resumable Session. Bounded queues retain outbound replay frames, inbound frames, and the exact partial local-write offset. Application failures terminate the session; link failures preserve resumable state.
- The initial byte-transfer test exposed a full-duplex deadlock when a blocked local write prevented output draining. Fixed it by splitting local read/write halves and tracking cancellation-safe incremental writes while output and ACK processing continue.
- Five resume tests pass: lost-ACK deduplication, output replay, partial-write continuation without duplicates, sequencing/EOF rejection, and codec/window bounds. Both gateway integration tests passed with an explicit real FUX_BIN, including local shell preservation after gateway shutdown (/tmp/koh-resume-real-gateway.log). Strict koh lib/tests Clippy passes (/tmp/koh-resume-clippy.log).
- Still required: authenticated session token/registry, bounded retention and expiry, concurrent-resume handling, client redial, final-ACK tombstones, and forced real-transport-loss tests. Automatic reconnect is not implemented yet; the gateway contract records this explicitly. Remaining documentation and final audit work are unchanged. No commits or pushes.

## Progress: automatic koh reconnect

- Wired resumable framing to a registry keyed by authenticated TLS peer plus a random 32-byte token. Create/resume are distinct; a missing resume never opens a new local application connection. At most 64 live/retained records exist. Active links are exclusive; concurrent attempts receive busy. Expiry releases retained sockets.
- Added client redial with a 30-second retry grace and 100 ms retry interval, preserving the local stream and journals. Invalid/rejected sessions stop retries. Completed records retain the final committed-frame count without extending their deadline on repeated queries. Server shutdown still leaves fux panes alive.
- A real QUIC test forced five link losses, observed six input effects exactly once, and confirmed only one local application connection was ever opened. Registry tests verify peer scoping, expired-resume rejection, capacity, exclusive active ownership, and socket release on expiry. Framing regressions continue to pass.
- Koh's full all-feature suite passed 274 tests with explicit real FUX_BIN gateway coverage (/tmp/koh-auto-resume-full.log). Strict all-target/all-feature Clippy passed (/tmp/koh-auto-resume-final-clippy.log). Integration patches were exported and exact reconstruction verified.
- Updated README/gateway contract with actual retry, retention, and restart behavior. Optional CI now runs gateway unit tests as well as real-fux gateway tests. Runtime evidence remains macOS loopback; no relay/NAT or Android runtime claim is made.
- Remaining: broader completion-ACK/fault/negative-path audit, stale documentation/tool claims, platform checks where available, and full requirement-by-requirement completion review. Automatic reconnect is implemented but the overall refactor is not yet declared complete. No commits, pushes, publication, or personal session/key changes.

## Progress: negative-path and architecture audit

- Added local_protocol integration tests for malformed/oversized frames, invalid message ordering, oversized input, all 64 stalled handshake slots expiring, and a blocked frame writer reaching its deadline. All three pass. Added a real-binary incompatible-server check that verifies unchanged terminal attributes, no alternate-screen entry, actionable errors, and zero key prompts/files; all local CLI tests pass.
- `cargo check --target aarch64-linux-android --locked --offline` passed (/tmp/fux-android-check.log). This is compilation evidence, not Android runtime coverage. Affected strict Clippy passes.
- Removed unused FUX_ALPN and remote Endpoint control identity variant. Control and host tests (11 + 43) pass after removal. Updated current architecture/release docs and marked earlier coupled designs and verification reports historical.
- Expanded optional source reconstruction verification to run koh gateway/resume tests against the reconstructed fux binary. `python3 tools/dependencies.py verify --build` passed, including required real-zor and real-fux gateway checks (/tmp/fux-final-reconstructed-integration.log).
- Created docs/standalone-audit.md with per-requirement evidence and open gates. Found an actual remaining contract gap: the control protocol has strict schemas but no explicit version negotiation. Implement that boundary and update its consumers/tests before completion. Full separate diff review, remaining completion/failure audits, and final snapshot/package checks are also pending.
- No commits, pushes, publication, or personal session/key changes.

## Progress: explicit control version negotiation

- Added mandatory FUXCTL1 preface negotiation before workspace control commands/subscriptions and manager requests. Reads use an absolute two-second deadline; original socket timeouts are restored. Fux control clients validate kernel peer credentials. Wrong/missing/partial prefaces reach no command handler.
- Updated fux CLI/RPC clients, fixture clients/fake servers, observer harness, and zor's independent control consumer. Added docs/local-control-protocol.md, including manager schemas and the distinction between CLI JSON input and socket negotiation. Attachment remains separately versioned.
- All 221 root tests pass (/tmp/fux-versioned-control-all-tests.log), and all 14 separate fixture tests pass with manager negotiation (/tmp/fux-versioned-manager-fixture.log). The real zor sidecar integration passed with an explicitly rebuilt binary (/tmp/fux-control-version-real-observer.log).
- Strict all-target/all-feature Clippy passed for fux and zor. Current integration patches were exported and exact source reconstruction verified. The audit's missing control-version requirement is now implemented.
- Remaining: separate complete-diff review, completion/failure-path audit follow-ups, final reconstructed/sibling-free/package verification after review fixes, and final requirement ledger sign-off. No commits, pushes, publication, or personal session/key changes.

## Progress: separate review pass, startup/socket fixes

- Began an explicit source/diff review separate from implementation; scope and findings are in docs/standalone-review.md. This is a partial review, not full-diff sign-off.
- Fixed an unbounded child-side startup exchange that could retain the manager lock indefinitely. The async exchange now has a whole-operation ten-second deadline; the unused unbounded synchronous API was removed. A new real-socket timeout regression passes without signal intervention.
- Established inode-aware cleanup guards before chmod in manager/control socket initialization. Existing ownership, replacement and race tests pass; the chmod failure branch was reviewed directly rather than fault-injected.
- Added a passing final-ACK recovery test at the resumable-session layer, including invalid completion rejection. It does not claim real relay packet-loss coverage.
- Affected control/daemon tests pass (11 + 17), and strict fux/koh targeted Clippy passes. Updated integration patches reconstruct exactly. Logs: /tmp/fux-socket-review-tests.log, /tmp/fux-socket-review-clippy.log, /tmp/koh-final-ack-review.log, /tmp/koh-final-ack-review-clippy.log.
- Remaining: complete the unreviewed diff scope, final affected/integration/sibling-free/package checks, and requirement-ledger sign-off. No commits, pushes, publication, or personal session/key changes.

## Progress: host/observer review and stale-generation fix

- Continued the separate review through host, state/config, client/runtime boundaries, the moved emulator comparison, and zor's full new observer adapter. Details are in standalone-review.md.
- Fixed a stale callback race when a pane number is reused: observer reports now require identity with the exact originating pane runtime. A real host regression closes/recreates the same pane number and verifies stale rejection plus current-report acceptance.
- Removed leftover unused echo-ack and prediction-target APIs from local multiplexer state/traits, plus obsolete remote-allow constants.
- All 223 root tests, strict all-target/all-feature Clippy, the required real-zor integration, and all 14 fixture tests pass. Logs: /tmp/fux-host-observer-review-tests.log, /tmp/fux-host-observer-review-clippy.log, /tmp/fux-observer-generation-real.log, /tmp/fux-observer-generation-fixture.log.
- Review is still partial. Complete the remaining diff scope and final artifact/requirement audit before marking the goal complete. No commits, pushes, publication, or personal session/key changes.

## Progress: CLI/distribution review and current artifact verification

- Reviewed remaining CLI startup/attachment changes, terminal I/O ownership, platform backend, default/optional CI boundaries, package verifier, dependency tooling, and koh embedding/CLI wiring. Recorded scope in standalone-review.md.
- Restored explicit socket viewer notifications through `viewer-notifications`. Updated the stale reload fixture found by the full suite; all 223 root tests now pass. Formatting and strict all-target/all-feature Clippy pass.
- Exact integration reconstruction passed 75 tests, including required real zor observation and authenticated real-fux gateway checks (/tmp/fux-completion-reconstruction.log). Package/install verification passed eight real-binary scenarios (/tmp/fux-completion-package.log).
- A fresh sibling-free source snapshot passed all 223 root and 14 fixture tests offline (/tmp/fux-completion-standalone-rebuilt.log). The initial attempt caught the old reload setting; a subsequent attempt reused a compiled-in deleted temporary path. Cleaning only fux artifacts in the isolated verification target before rebuilding resolved that harness cache issue.
- Current full test log: /tmp/fux-completion-tests-fixed.log; strict lint: /tmp/fux-completion-clippy-fixed.log. No commits, pushes, publication, or personal key/session changes.
- Still required: finish full test/documentation review, resolve the real-fux forced-outage audit follow-up, and sign off each requirement in the completion ledger. Overall goal remains active.

## Completion: standalone multiplexer with independent optional integrations

- Completed the remaining review and requirement audit; see standalone-review.md and
  standalone-audit.md. All prompt acceptance items have implementation and verification evidence.
- Real fux now has explicit forced-QUIC-loss evidence: five reconnects preserve one local attachment
  and shell PID, and six commands produce exactly six file effects. Optional CI requires the exact
  fux binary for this scenario. Koh's full suite passes 276 tests with strict Clippy.
- Fixed repeat reconstruction verification by cleaning owner-package artifacts in its dedicated
  cache before each temporary build. The repaired verifier passes 75 integration tests, including
  real zor and real fux reconnect scenarios; exported owner patches reconstruct exactly.
- Current fux tests pass 222 cases after removing one vacuous assertion. Strict Clippy and formatting
  pass. Zor's current full suite passes 56 tests, strict Clippy and minimal-feature compilation pass.
  Installed standalone package passes all eight real-binary scenarios. Earlier isolated offline
  sibling-free root/fixture verification remains valid; only an unused test and docs/tooling changed.
- README includes local operation, optional gateway/observer setup, obsolete setting migration and
  safe old-server upgrade instructions. No personal keys or sessions were changed, and no commits,
  pushes, PRs or publication were performed. Linux/MSRV hosted CI and Android/relay runtime coverage
  are not claimed. The full objective is implemented; environment coverage limits are documented.
