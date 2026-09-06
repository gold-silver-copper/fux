> Historical record from before the 0.3.0 bevy_ecs rewrite (2026-09-05). The host, popup panes, sidecar supervision, protocol v2/`FUXCTL1` and verification results described here no longer exist. Current architecture: [design.md](design.md); current evidence: [ecs-acceptance.md](ecs-acceptance.md).

# Standalone refactor completion audit

Objective: execute [standalone-refactor-prompt.md](../standalone-refactor-prompt.md) in full.
This is the historical standalone-refactor checkpoint ledger. Its test counts and review
conclusions describe that checkpoint, not the later contextual-help implementation or current
worktree. See [the contextual-help acceptance audit](contextual-help-acceptance.md) for the
subsequent complete review, newly discovered defects, fixes and current verification.
Verification recorded below ran locally on macOS before the user-requested commit/push checkpoint.

## Requirement evidence

| Prompt requirement | Evidence and conclusion |
|---|---|
| Build/run without koh or zor, keys, or runtime networking | Cargo.toml and the complete resolved Cargo metadata graph exclude koh, zor, iroh and the remote identity stack. A disposable sibling-free checkout built and passed root/fixture tests offline. The local CLI harness checks zero keys/prompts and `lsof` confirms no server network sockets. Reviewed client/startup code uses Unix sockets only. |
| Fux owns the existing multiplexer | Existing host/state/client implementation retains PTYs, process groups, workspaces/tabs/layouts/popups, scrollback, renderer, input, commands and configuration. Moved primitives contain terminal logic only, with LICENSES attribution. No detector or network engine was copied into fux. |
| Koh owns remote connectivity and security | Independent gateway library/CLI owns TLS identities, allowlists, transport profiles, relay/discovery configuration and resume policy. Fux has no key/identity/network command surface. Koh and zor manifests have no dependency on fux or each other. |
| Zor owns observation | Zor's independent `observe` command samples the control contract and runs its detector/rules/state machine. Fux contains a bounded schema consumer and optional child supervisor. Default `zor-path` is absent and commands spawn directly. |
| Persistent on-demand local server | Manager startup locks elect one server per runtime directory/user; named workspaces have separate sockets. Concurrent first-launch fixture passes. Real TTY detach and reattach preserve the pane PID and output; multiple simultaneous viewers are exercised. PTYs remain live in the server, not serialized to disk. |
| Secure local interface | Private owned directories, mode-0600 sockets, symlink/non-socket rejection, current kernel UID checks, foreign-UID policy rejection, and inode-aware cleanup are exercised by control/daemon tests. A separate foreign-account process was not launched; OS credential retrieval and rejection policy are verified separately. |
| Bound messages, queues, clients and stalled peers | Local protocol tests exercise malformed/oversized frames, invalid ordering/input, all 64 stalled handshake slots expiring and a blocked writer deadline. Source review confirms bounded channels, coalesced state, capped resources and cancellation. Control handshake/requests/subscribers also have explicit limits. |
| Versioned attachment/control boundaries | Attachment v1 at this historical checkpoint (now v2) and FUXCTL1 control/manager prefaces are documented in the two local protocol contracts. Wrong/missing/partial control negotiation reaches no handler. Descriptor, state and control types carry no remote identities or keys. |
| Authorize remote clients before local access | Gateway integration denies an outsider and verifies zero application accepts. The socket target is fixed by the local operator; accepted peers receive byte-exact forwarding. |
| Preserve remote functionality and reconnect | Real QUIC tests force five losses. The real-fux scenario retains one local attachment and one shell PID; six commands produce exactly six file effects and increasing shell state. Registry/replay tests cover peer-scoped tokens, missing/expired resumes, exclusive ownership, capacity, duplicate/partial writes, invalid sequencing and final-ACK recovery. |
| Gateway start/stop independent of pane lifetime | Real-fux gateway integration forwards shell input, stops both gateway services, then attaches locally and reads the same shell variable. Restart/expired retention requires a new viewer attachment and does not terminate panes. |
| Optional observer failures cannot own or break panes | Five fake observer cases exercise crash, stall, malformed, oversized and partial reports; required real-zor tests check working/idle metadata. The pane PID stays unchanged and accepts input. Killing the observer clears status; shutdown reaps it. Host tests reject delayed reports after pane-number reuse. Missing observer executables are handled by the supervisor. |
| Preserve input/rendering/multiplexer behavior | Client/host tests, independent model/corpus/golden scenarios and real-binary fixtures exercise keybindings, prefix/mouse routing, copy mode, resize, layout, multi-viewer behavior and workspace switching. A vacuous untouched-state comparison was removed rather than counted as evidence. |
| Pane exit, workspace kill, signals and cancellation | Real fixtures cover final status, direct pane death, descendant kill, simultaneous startup, startup-phase SIGTERM and stalled startup cancellation. Host/daemon/client tests cover owned task/process cleanup, socket replacement preservation and a ten-second startup deadline. |
| Reject incompatible servers before raw mode; never silently kill | Real protocol-rejection harness verifies unchanged termios, no alternate-screen entry, actionable errors and zero key prompts. Manager mismatch handling is explicit. README explains saving work and stopping an old server with its matching binary. |
| Preserve pre-existing koh handshake cleanup | Reviewed embed/client.rs cleanup closes an endpoint on failed connection/timeout. Existing regression and full koh suite pass; exported patches reproduce the fix exactly. |
| Update CLI/config/docs/CI and packaging | README, design, security and protocol documents match the new architecture. Removed local keys/network options and documented viewer notification rename. Default Linux/macOS/MSRV/Android/package jobs require only fux; optional integration CI explicitly requires real binaries. Package/install verification uses no sibling sources or integration patches. |
| Protect personal data and respect authorization | Tests use temporary HOME/XDG paths and owned processes. Personal keys and existing personal sessions were not changed. No commits, pushes, publication or PR actions were performed. |
| Implementation plan and final review | standalone-plan.md records implementation and verification. standalone-review.md records separate self-review passes across the three owner repositories, validated fixes and coverage limits. No outstanding confirmed P0/P1 finding remains. |

## Verification records

- `cargo fmt --all --check`, strict all-target/all-feature Clippy and the current fux suite pass:
  **222 tests**, `/tmp/fux-outage-final-tests.log` and `/tmp/fux-outage-clippy.log`.
- Isolated fux-only offline checkout: **223 root + 14 fixture tests** passed in
  `/tmp/fux-completion-standalone-rebuilt.log`. The subsequent root count decreased by one solely
  because review removed the vacuous client test; no production code or dependency changed.
- Koh full all-feature suite: **276 tests** with explicit FUX_BIN/KOH_REQUIRE_FUX_BIN;
  `/tmp/koh-outage-full.log`. Strict all-target/all-feature Clippy: `/tmp/koh-outage-clippy.log`.
- `python3 tools/dependencies.py verify --build` passes against reconstructed owner patches,
  including required real-zor and real-fux reconnect checks:
  `/tmp/fux-outage-reconstruction-fixed.log`. Owner artifacts are rebuilt to avoid retaining deleted
  temporary source paths. The verifier checks exported source bytes against the owning worktrees.
- Zor's current full suite passes **56 tests**, strict all-target/all-feature Clippy passes, and
  the no-default-features all-target check passes: /tmp/zor-final-handoff-tests.log,
  /tmp/zor-final-handoff-clippy.log and /tmp/zor-final-handoff-minimal.log.
- Android aarch64 offline compilation and rustdoc passed: `/tmp/fux-final-android.log` and
  `/tmp/fux-final-doc.log`. This is not Android runtime evidence.
- `tests/verify/release-package.sh --allow-dirty` packaged and installed standalone fux and passed
  all **eight** binary scenarios against the final implementation: `/tmp/fux-final-handoff-package.log`.
  Subsequent documentation-only ledger updates do not change the verified executable. The package
  archive is refreshed to include those records before handoff.

Workflow YAML parsing and all eight verification-structure tests pass after the final CI quoting
fix. Final owner patch verification and archive refresh pass in /tmp/fux-handoff-patch-verify.log
and /tmp/fux-handoff-package-refresh.log.

## Remaining environment limits

Runtime evidence is macOS loopback and local Unix sockets. Linux/MSRV jobs are configured but were
not run on hosted CI for this uncommitted work. Android runtime, relay/NAT outages, real terminal
OSC collision behavior and genuine-agent rule accuracy are not claimed. Final-ACK loss is tested
at the session layer, not by dropping a final packet through a real relay. These limits do not
require koh or zor for standalone operation and do not represent unfinished implementation work.

Migration and operating instructions are in README.md. Earlier coupled reports and designs are
explicitly historical. No publication or deployment is needed to use the local build.
