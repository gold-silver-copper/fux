# fux and zor

**Unpublished source alpha.** fux is a persistent terminal multiplexer whose session is a
`bevy_ui` scene in a `bevy_ecs` World. zor is the task/agent policy layer: a separate Bevy
application and BRP client of fux.

fux owns task PTYs; viewers and zor can go away without killing those terminals. zor owns
provider sidecars, checks, plugin children and durable workflow intent. A restart or lost
reply can leave a side effect uncertain: neither reconnect nor recovery silently replays it.
Closing a dashboard is not stopping a task.

## Start here

From the workspace root, with Rust/Cargo compatible with Rust 1.95:

```sh
cargo build --locked --release --workspace
```

Use the [source installation and update guide](docs/installation.md) for an isolated local
install root, private XDG directories, foreground startup and explicit restore/skip. There
is no published binary, curl installer, or automatic migration promised by this checkout.
Once the servers are running in that environment:

```sh
fux fux/schema
zor zor/schema
zor status
zor run -- /bin/sh -c 'printf "hello\n"'
zor dashboard
```

`zor attach TASK` targets the exact live task pane. `fux bindings` prints effective viewer
bindings. Bare `fux` can auto-start a server with automatic saved-command restoration; use
`fux serve --restore ask` when recovery must be deliberate.

## Contracts and evidence

- [Design and scheduling](docs/design.md)
- [Ownership, lifetimes and crash boundaries](docs/ownership.md)
- [Generated/discovered protocol](docs/protocol.md)
- [Security and trust model](docs/security.md) — plugins are trusted local executables,
  not sandboxed code.
- [Installation, controlled updates and recovery](docs/installation.md)
- [Verification evidence](docs/verification.md) and [capability status](docs/capability-status.md)
- [Dependencies](docs/dependencies.md), [changelog](CHANGELOG.md) and [handoff](docs/HANDOFF.md)

Active acceptance covers fux and zor. koh and future iroh-ssh work are outside those gates.
Documented features are not claims that every provider/platform or benchmark has passed;
the verification record states what was actually exercised.
