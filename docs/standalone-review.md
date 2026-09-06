> Historical record from before the 0.3.0 bevy_ecs rewrite (2026-09-05). The host, popup panes, sidecar supervision, protocol v2/`FUXCTL1` and verification results described here no longer exist. Current architecture: [design.md](design.md); current evidence: [ecs-acceptance.md](ecs-acceptance.md).

# Standalone refactor review

This records separate self-review passes over the intended tracked and untracked changes in fux,
koh, and zor. No independent reviewer was invoked. Earlier sections describe intermediate scope;
the final pass below records the completed review and its limitations.

## Pass 1: session, socket, and startup ownership

Inspected current local client/server code, observation child ownership, workspace deserialization
validation, koh gateway session/replay lifecycle, and daemon manager/startup/path/socket diffs.
Checked cancellation, sequence commit ordering, bounded queues, peer checks, retained-session
expiry, and resource ownership. Remaining portions of the complete change set still require review.

Findings and actions:

- **P2: unbounded child startup exchange.** `receive_startup_async` could await a missing payload
  forever while its caller held the manager lock. Signal cancellation existed, but there was no
  timeout in that path. Added a ten-second whole-exchange timeout and removed the unused unbounded
  synchronous API. A real Unix socket regression holds the payload open and verifies timeout and
  connection closure without sending a signal. All 17 daemon tests pass after this change.
- **P2: socket initialization cleanup.** Control binding used an unconditional unlink after chmod
  failure, and manager binding had no guard on that failure path. Constructed inode-aware bound
  guards before chmod in both paths. Existing control/daemon tests cover owned-node teardown,
  replacement preservation and startup races; chmod failure itself was reviewed in source rather
  than injected. Failure to acquire initial inode metadata remains an error that avoids deleting
  an unidentified path.
- **Missing final-ACK coverage.** Added a resumable-session test that finishes real duplex forwarding,
  discards the EOF acknowledgement, then checks recovery from the retained committed count. It also
  rejects premature completion and acknowledgements beyond sent input. The test passes. This is
  session-layer evidence; it does not claim a dropped final packet was injected through a real relay.

No confirmed P0/P1 finding was identified in this reviewed portion. That statement does not cover
unreviewed files or constitute a completed security audit.

## Remaining review scope

Review the complete current fux host/client/terminal/state/runtime/configuration and build/test/doc
changes, the independent zor observer diff, and remaining koh gateway/CLI/embedding changes. Include
untracked files and the exported owner patches. Recheck startup/socket fixes in the final pass, and
rerun all affected checks before final package and requirement-ledger sign-off.

## Pass 2: host/observer and state/client boundaries

Reviewed the full host observer diff, own SessionHost interface, state/configuration diffs, client connection/render adapter changes, runtime control/configuration diff, and the complete new zor observation adapter plus its CLI/screen integration diff. Compared the moved terminal emulator with its koh source to confirm that query/bounds logic is preserved while transport fields are removed. Continued review of startup/CLI call sites; the full remaining scope has not yet been signed off.

- **P2: stale observer callback after pane-number reuse.** The report closure captured only PaneId. The host can reuse the highest removed pane number, so a delayed old observer could update a replacement pane. Captured a weak reference to the originating runtime and require pointer identity with the current pane under the shared lock. A regression closes and recreates a pane with the same number, rejects the old runtime's report, and accepts the current runtime's report.
- **Ownership cleanup:** removed unused echo acknowledgements from workspace metadata/host/client traits, dormant prediction-target screen adapters, and remote-allow configuration constants. These fields had no live local multiplexer consumer and retained transport-era API concepts. Visual composition overlays remain renderer data, not a prediction engine.
- All 223 root tests and strict all-target/all-feature Clippy pass after these changes. The explicitly configured real zor observer integration and all 14 separate fixture tests pass against the rebuilt fux binary.

No new confirmed P0/P1 finding was identified in this portion. Pending: remaining full CLI/copied primitive/packaging/test/documentation and koh ownership review, then final regression/artifact/requirements sign-off.

## Pass 3: CLI, terminal I/O, and distribution boundaries

Reviewed CLI removal of identity loading/provisioning and remote options, local startup and workspace
creation call sites, explicit socket attachment, the owned stdin/SIGWINCH producers, backend terminal
primitives, and terminal snapshot types. Reviewed all workflow diffs, standalone package/install
script, dependency reconstruction changes, architecture guards, and the koh embedding cleanup and
CLI/library/feature wiring. The pre-existing failed-handshake endpoint cleanup remains intact.

- **P2: explicit-viewer notifications disabled.** Socket attachment passed no notification policy.
  It now honors `viewer-notifications`, the replacement for the transport-specific `remote-clients`
  setting. The migration is documented. Full regression testing found one reload fixture retaining
  the old setting; that fixture is now updated too.
- All 223 root tests pass after the fixture correction. Formatting and strict Clippy are checked
  separately. Exact patch reconstruction passed 75 tests, including required real-zor observation
  and both real-fux gateway tests. Package/install verification passed all eight binary scenarios.
- No new confirmed P0/P1 defect was found in this portion. This does not close the remaining full
  test/documentation review, real-fux outage-injection follow-up, or final requirement ledger.

## Pass 4: acceptance evidence and final review

Completed the remaining test and documentation review: standalone CLI Python harnesses, observer
failure/real-sidecar harness, fixture interpreter and real-binary migration, transport-test removal
and its koh-owned replacement, current README/design/security/protocol/release documents, and
integration patch reconstruction. Combined with the preceding production-code passes, this covers
the intended tracked/untracked change set in all three owner repositories. Historical design files
retain the previous text with an explicit historical marker; generated patches reproduce reviewed
owner sources and are checked byte-for-byte by the reconstruction verifier.

- Added real-fux forced-QUIC-loss coverage using the production registry/client resume loop. Five
  losses preserve one application attachment and the shell PID; six commands produce six file
  effects and increasing shell state. The test passes, and the optional CI job requires FUX_BIN
  for this test instead of silently skipping it.
- Removed a vacuous client test that compared an untouched state value against its clone. It did
  not test an input/state boundary. Actual input, prefix/mouse routing, rendered state and shell
  effects are exercised by the existing client, host, corpus and real-binary scenarios.
- **P2: reconstruction cache retained deleted source paths.** Repeated verification could reuse
  binaries containing an earlier temporary CARGO_MANIFEST_DIR, causing the observer harness to
  fail before running. The verifier now cleans only the three owner packages in its dedicated
  target directory before rebuilding each snapshot, preserving third-party dependency caches.
- Koh's all-feature suite passes 276 tests with required real-fux coverage; strict all-target,
  all-feature Clippy passes. Fux's current suite passes 222 tests after removing the vacuous test.
  The reconstruction fix is verified separately in the final completion ledger.

Final source review found no outstanding confirmed P0/P1 defect. The startup deadline, socket
cleanup, observer-generation guard, notification migration and verifier-cache fixes were rechecked
against current source and affected tests. This is a code review, not a claim of a formal security
proof or native execution on untested platforms. Runtime evidence is macOS; relay/NAT, Android
runtime and a separately logged-in foreign-UID process are outside the exercised environments.

Final artifact check also caught an unquoted `gateway::` filter in the optional CI YAML command.
Converted that command to a block scalar. All three workflow files parse with Ruby YAML, and all
eight architecture/verification-structure tests pass. Current patch reconstruction and refreshed
package checks pass; no review finding remains unresolved.
