> Historical design/evidence for the earlier ownership refactor. Fux now starts pane commands directly and uses an optional zor sidecar. Current behavior is documented in [design.md](design.md); current verification is tracked in [standalone-plan.md](standalone-plan.md).

# Ownership refactor: implementation and verification

Scope: the complete local fux worktree against its original HEAD, koh against
`cfb6f3e5b623b8656171278be2a5a235928142d7`, and zor against
`b7ccd41f88ae6c125e94c16c2cdfa7c8108abd8c`, including new files and dependency patches.
At initial verification, no commits, pushes, PRs, package publication, personal key changes, or personal session termination had occurred. The user subsequently authorized commits and pushes; the dependency manifest now pins the committed owner changes.

## Resulting contracts

| Owner | Public integration and lifetime |
| --- | --- |
| koh | `identity::Identity` owns unlocked credentials and reset leases; `embed::Connection` owns authenticated dialing/reconnect/close; `embed::Server` owns admission, connection limits and network workers. These APIs expose no iroh types. |
| fux | Owns workspace/tab/pane state, pane PTYs and process groups, terminal rendering/input, application commands, control sockets, and manager lifecycle. A lost viewer leaves its workspace running. |
| zor | Owns detection, interpretation and optional wrapper observation. `zor::osc` is the only observation library surface fux consumes. Version-1 bounded reports are unverified observations, never authorization data. |

Generic koh state synchronization, terminal emulation, rendering-backend and explicitly started
input-producer interfaces remain shared. fux's pane adapter uses portable-pty directly and retains
the original koh MIT notice under `LICENSES/`. koh retains its independent standalone shell adapter.
zor retains its own transparent wrapper PTY and process cleanup; it has no multiplexer or network
policy. Abruptly killing a wrapper still terminates/reaps its owned child; ordinary observation,
parsing and sink failures do not turn observation data into application control.

The workspace wire protocol is `fux/2`, reflecting the new replicated binding metadata; old
`fux/1` peers must be rebuilt rather than attempting to decode an incompatible state schema.
Embedded input is byte-exact by default; only the koh standalone client opts into koh's escape
keys. fux handles detach/picker actions itself and cancels/awaits the connection before joining its
terminal producers. Workspace metadata publishes fixed-size shortcut bitsets. The initial empty
client state carries no policy, so it cannot override remote bindings before the first snapshot.
Keyboard and mouse mutations use the control API's typed dispatcher; external hooks, copy mode,
help, and viewer-local actions stay with their appropriate owners. Defaults/help share one registry.

## Requirement audit

| Prompt requirement | Implementation and direct verification evidence |
| --- | --- |
| 1. Complete koh embedding API | `references/koh/src/embed`, opaque identities, fux endpoint adapter; public `embed_contract` tests cover unauthorized peers, capacity, cancellation/terminal drop, capacity release, and literal standalone escape bytes. fux architectural tests reject transport/key internals. |
| 2. Transport/workspace separation | WorkspaceHost survives viewer detach; host loopback reconnect test forces loss through `Server::disconnect_clients`, compares retained state/lifecycle, and reattaches. Binary corpus covers detach/reconnect and simultaneous first-client election. |
| 3. Process/terminal ownership | `src/pty.rs`, host drain workers and shared command dispatcher; host tests cover descendant reaping, bounded shutdown, PTY geometry, terminal queries, copy/paste and EOF before real exit. Binary tests cover startup phase cancellation and wrapper death. |
| 4. zor contract | `zor/OBSERVATION-CONTRACT.md`, OSC version/size checks and bounded event records; parser/property tests, required real-zor integration, wrapper passthrough/signal/resize/query tests, and bare-pane binary scenarios. |
| 5. One command model | `src/commands.rs`, `WorkspaceHost::execute_request`, mouse focus mapping; host request matrix, command/event ordering, remapped/removed shortcut tests, replicated-policy reload test, and explicit pane-kill-with-popup regression. `fux bindings` and in-workspace help use configured bindings. |
| 6. Key UX and safety | koh identity leases and zeroizing IPC transfer; `tests/key_cli.rs` runs real PTYs with isolated HOME/XDG and distinct passphrases. Covers cold/attach/switch prompt counts, cancellation/echo restoration, corrupt and unsafe paths, passphrase preservation and guarded reset. koh tests cover malformed transfer wiping, retained clone/transfer leases, concurrent first creation, and relative reset paths with confirmation. |
| 7. Independent projects | No reverse dependencies; fux has no direct iroh dependency and builds zor without runtime features. Independent koh and zor checks, koh alternate-backend/no-CLI checks, zor no-default-feature checks and structural dependency checks pass. |
| 8. Reproducibility | Immutable base manifest, exported patches including new files, idempotent apply with divergence rejection, CI assembly, and README instructions. `python3 tools/dependencies.py verify --build` reconstructs all sources, builds fux/zor, and runs host/client/required real-zor integration tests. |
| Full review and verification | Separate diff review described below; final affected tests, independent repository checks, reconstructed build and installed-style binary corpus. Native execution is on macOS; Linux/Android execution remains CI/platform coverage, not claimed here. |

## Review findings and disposition

The primary agent performed a separate full-diff review; no independent subagent was used because
session instructions prohibit delegation. Review covered source, new files, tests, documentation,
manifests/lockfiles, and CI across all three repositories. Moved dispatch code was compared against
the original dispatcher with receiver-only changes normalized; the pane adapter was compared
against koh's original adapter so moves could not obscure semantic changes.

Confirmed and fixed:

- Hardcoded client detach/picker keys ignored configured bindings. Clients now consume workspace
  policy, including reload/removal, and preserve ordinary bytes and pasted payloads.
- koh escape processing could still intercept bytes even when fux disabled a shortcut. Embedded
  sessions now pass bytes literally; fux uses explicit cancellation for local actions.
- A placeholder client render could publish default bindings before remote state arrived. It now
  has no input policy; a test verifies that the placeholder cannot authorize shortcuts.
- PTY EOF was incorrectly treated as process completion after one second. The host now waits for
  the real status while the process remains owned; removal/shutdown cancels that wait. A child
  closing all terminal descriptors and exiting later proves status preservation and cancellation.
- Explicitly killing a tiled pane while a popup existed killed the popup instead. A regression
  failed before the fix and passes after separating tiled-pane close from popup close.
- Dependency apply accepted additional unrelated edits when the patch itself reverse-applied.
  It now compares the entire resulting diff and rejects divergence; disposable-clone checks cover
  both idempotence and rejection.
- Control-key aliases could bypass the prefix conflict check or silently replace each other.
  Configuration and routing now use one byte decoder and reject collisions, with a regression
  for both prefix aliases and duplicate bindings.
- Relative `koh key reset --key-file identity.key --yes` used an empty parent path. Parent lookup
  now treats it as the current directory, with a private-directory CLI regression.
- Binary tests retained old unversioned agent payloads and waited for removed transport logging.
  Payloads now exercise v1, and attachment waits for an actual workspace frame.

The first parallel fux run exposed an indeterminate final exit status; the EOF ownership fix adds
a deterministic regression for that class of error. An initial zor signal/backpressure run failed
its exit-success assertion once. Its runtime signal code was unchanged by this refactor; a better
failure diagnostic, 15 subsequent focused runs, serial/full suites, and the binary corpus passed.
This intermittent timing observation remains a known test risk, not a silently ignored failure.
No confirmed unresolved P0/P1 finding remains from this review. The final wire-protocol check
rejects `fux/1` before establishing a `fux/2` connection, and the zor package builds successfully
with `cargo package --locked --allow-dirty --offline` (local archive only).

## Reproduce verification

From fux:

```sh
python3 tools/dependencies.py apply
python3 tools/dependencies.py verify --build
cargo fmt --all --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-features --locked -- --test-threads=1
cargo doc --no-deps --all-features --locked
cargo +1.91.0 check --all-targets --locked
cargo build --locked --bin fux
cargo build --manifest-path zor/Cargo.toml --locked --bin zor
cargo test --manifest-path tests/verify/fixture-child/Cargo.toml --locked --test binary -- --test-threads=1
```

The reconstruction test supplies an explicit `ZOR_BIN` and requires its presence. The standalone
optional real-zor test otherwise skips when no binary is provided; that skip is not used as evidence
for cross-project behavior. All fixture processes and credentials use private disposable directories.

Independent projects:

```sh
cargo fmt --manifest-path references/koh/Cargo.toml --all --check
cargo clippy --manifest-path references/koh/Cargo.toml --all-targets --locked -- -D warnings
cargo test --manifest-path references/koh/Cargo.toml --locked
RUSTDOCFLAGS='-D rustdoc::broken_intra_doc_links' cargo doc --manifest-path references/koh/Cargo.toml --no-deps --locked
# Repeat koh Clippy with --no-default-features and each backend:
# backend-crossterm, backend-qwertty, backend-termina.
cargo fmt --manifest-path zor/Cargo.toml --all --check
cargo +1.91.0 clippy --manifest-path zor/Cargo.toml --all-targets --all-features --locked -- -D warnings
cargo +1.91.0 clippy --manifest-path zor/Cargo.toml --all-targets --no-default-features --locked -- -D warnings
cargo +1.91.0 test --manifest-path zor/Cargo.toml --all-features --locked
cargo +1.91.0 test --manifest-path zor/Cargo.toml --no-default-features --locked
cargo +1.91.0 doc --manifest-path zor/Cargo.toml --no-deps --all-features --locked
```

## Release and platform limits

This is a reproducible local multi-repository development state. The published dependency versions
do not contain these new APIs. Release koh and zor independently, then update fux's registry pins
before packaging a registry-based fux release. No versions or upstream commits were invented, and
no package release or PR was created. CI source assembly now pins the committed owner revisions;
remote CI results are separate from the local verification recorded here. Native test results cover macOS, with an explicit Rust 1.91 type check;
they do not prove Linux/Android runtime behavior.
