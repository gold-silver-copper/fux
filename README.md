# fux

fux is an agent-oriented terminal workspace: one named workspace can contain multiple PTY panes,
tabs, popups, synchronized viewers, a local JSON control socket, and optional `zor` agent-state
observation. koh owns networking, remote sessions, endpoint identities, and credential handling.
fux owns pane PTYs, workspace state, commands, and presentation; zor owns observation.

This is an early 0.1 implementation. The state/layout, host, compositor, control protocol, daemon
descriptor and endpoint-management layers are present, but release verification is still in
progress.

## Basic model

- `fux` attaches to or creates the default local workspace.
- `fux serve --allow <endpoint-id>` serves an explicitly authorized remote viewer.
- `fux connect <endpoint-id>` connects to a remote workspace.
- Named workspaces have distinct endpoint identities. Runtime descriptors advertise their endpoint
  ids; secret keys are stored separately with private permissions.
- The prefix key opens workspace command mode. The default detach sequence is prefix then `d`;
  prefix followed by itself sends the literal prefix.

Run `fux bindings` to list bindings from local configuration. Prefix then `?` shows the running
workspace's configured bindings. Viewer detach and workspace-picker shortcuts follow the
workspace's published configuration, including reloads and remote connections. A remote-only
attachment does not provide the local manager's workspace picker.

Run `fux --help` for the current command surface. Configuration is loaded from
`$XDG_CONFIG_HOME/fux/config.toml` or the platform config directory. Runtime descriptors and Unix
sockets live below `$XDG_RUNTIME_DIR/fux` when available, with a private per-user fallback.
Set `local-network = true` to create new workspace endpoints with Iroh's local-only network
profile, disabling relay and discovery use. Changing this endpoint policy requires restarting the
workspace; a live configuration reload rejects the change transactionally.

## Identity keys and passphrases

fux uses a client identity shared with koh and a separate identity for each named workspace.
Starting a stopped workspace unlocks the client and workspace keys once each. Attaching to a running
workspace unlocks only the client key. Reconnects and the workspace picker reuse that unlocked
client identity for the lifetime of the invocation. All credential prompts finish before terminal
input readers start; daemon startup waits until both identities are unlocked.

Different keys may have different passphrases, so a cold start can still require two prompts.
New keys also ask for passphrase confirmation. Keys remain encrypted at rest; there is no
plaintext or empty-passphrase mode and no persistent passphrase cache.

Inspect paths without unlocking or creating keys:

```sh
fux key path --client
fux key path --workspace default
```

Inspect an endpoint ID or change its passphrase without changing the identity:

```sh
fux key info --client
fux key passwd --client
fux key info --workspace default
fux key passwd --workspace default
```

`path`, `info`, and `passwd` also accept `--key-file PATH`. Without a selector they use the client
identity. Client keys default to `$XDG_CONFIG_HOME/koh/client.key` or `~/.config/koh/client.key`;
workspace keys use `$XDG_STATE_HOME/fux/keys/NAME.key` or `~/.local/state/fux/keys/NAME.key`.

If a passphrase is lost, reset explicitly selected identities. First list workspaces with
`fux workspace list`, then stop each with `fux workspace kill NAME`. **Stopping a workspace ends
its panes.** Reset refuses while a manager is running in the current runtime directory. Also stop
any remote `fux connect` clients or koh processes using the shared client identity, and any fux
instances using the same keys with a different `XDG_RUNTIME_DIR`; their lifetimes cannot be checked
by the local manager.

```sh
fux key reset --workspace default --yes
fux key reset --client --yes
fux
```

Each reset deletes only the selected key; the next use generates a new identity and requests a
new passphrase. Reset needs no old passphrase. **Endpoint IDs change**, so update remote client
allowlists and saved workspace endpoint IDs. Resetting the client affects koh too. Omit `--yes`
to see the consequences without deleting anything. A stale manager socket also blocks reset; start
and stop fux to let normal startup recover that state. Reset does not accept arbitrary `--key-file`
paths. Resetting keys does not disable future passphrase prompts.

A wrong-passphrase error can also mean authenticated ciphertext was modified; those cases cannot
be distinguished cryptographically. Errors include the identity path and preserve the underlying
format, permissions, or I/O diagnostic. Check the path and passphrase before choosing reset.

## Pane execution and history

When an executable `zor` is configured, panes are spawned through `zor --title never -- …` so fux
can consume OSC 7877 state reports. If the probe fails, fux starts a bare pane and logs the fallback
once. Scrollback and capture requests are bounded; fux uses koh's temporary scrollback callback
for viewport extraction rather than retaining an unbounded output log.

Copy/scroll viewport is shared between viewers in version 1. koh identifies viewers for resize and
detach, but pane input does not carry a client id. Clipboard state is synchronized as bounded
base64 and emitted as OSC 52 only by an opted-in client backend.

## Security boundaries

Remote access is allowlist-based. Endpoint ids authenticate transport peers; knowing a workspace
name or finding a runtime descriptor does not grant admission. The local control socket relies on
an owner-only runtime directory and socket permissions (portable Unix peer credentials are not
available everywhere), and rejects unsafe names, oversized frames, path traversal, and unbounded
command/environment payloads.

The process inside a pane is trusted to control that pane's terminal. It can emit titles, bells,
clipboard OSC, and OSC 7877 itself. Agent status is therefore presentation metadata, not an
authenticated claim: sequence numbers deduplicate adjacent reports but do not establish reporter
identity. OSC 21337 is observed only; its provenance and schema remain unverified.

See [docs/security.md](docs/security.md) for the threat model and operational guidance.

## Platforms and limitations

Linux and macOS are the primary host platforms. Android is compile-checked as a client target; that
does not constitute Android runtime coverage. Windows hosting is not supported in 0.2. Remote relay,
terminal-emulator OSC collision behavior, and genuine Claude Code rules still need human evidence.

The development checkout uses local path dependencies for koh and zor while published packages
resolve the matching koh 0.12.1 and zor 0.1.2 releases from the registry once published. Both
uploads are currently blocked by the crates.io owner-account lock recorded in
`docs/release-readiness.md`.

## Development

The combined development tree uses exact base revisions plus reviewed source patches for the
independent koh and zor repositories. On a fresh fux checkout, assemble them before building:

```sh
python3 tools/dependencies.py apply
python3 tools/dependencies.py verify --build
```

`dependency-patches/manifest.json` records upstream repositories, immutable bases, and patch paths.
The tool refuses unexpected bases or divergent local edits. Make dependency changes in their
owning checkouts (`references/koh` and `zor`), then run `python3 tools/dependencies.py export` and
`verify` to include them in fux's reproducible development inputs. Verification reconstructs each
checkout from its base and checks all source bytes, including new files. `--build` also reconstructs the complete fux source tree, builds both binaries, and runs
host/client/real-zor integration tests against those sources. CI assembles the same sources before
its Rust checks.

These development APIs are not represented by new registry releases. Release koh and zor from
their owning repositories before updating fux to published versions; packaging against the current
registry versions does not reproduce this refactor. No package publication is part of this work.

```sh
cargo fmt --all --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --all-features --locked -- --test-threads=1
cargo doc --no-deps --locked
```

The identity CLI regression test requires Python 3 and uses a PTY with disposable HOME/XDG
directories. It verifies prompting, terminal restoration, and reset behavior without accessing
personal keys.

Set `ZOR_BIN` to an explicitly built zor executable for cross-repository integration tests. Do not
rely on a sibling repository's target-directory layout.

Licensed under MIT.
