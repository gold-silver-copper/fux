# fux

fux is an agent-oriented terminal workspace: one named workspace can contain multiple PTY panes,
tabs, popups, synchronized viewers, a local JSON control socket, and optional `zor` agent-state
observation. It uses koh for detachable terminal transport and iroh endpoint identities.

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

Run `fux --help` for the current command surface. Configuration is loaded from
`$XDG_CONFIG_HOME/fux/config.toml` or the platform config directory. Runtime descriptors and Unix
sockets live below `$XDG_RUNTIME_DIR/fux` when available, with a private per-user fallback.

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

```sh
cargo fmt --all --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
cargo doc --no-deps --locked
```

Set `ZOR_BIN` to an explicitly built zor executable for cross-repository integration tests. Do not
rely on a sibling repository's target-directory layout.

Licensed under MIT.
