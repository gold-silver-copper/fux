# fux

fux is a standalone local terminal multiplexer with persistent workspaces, PTY panes, tabs,
popups, scrollback, multiple viewers, and a local JSON control interface. It builds and runs
without koh or zor. Local operation requires no cryptographic keys and opens no network listeners.

## Local use

```sh
cargo build --release --locked
./target/release/fux
```

`fux` starts the local session server on demand and attaches to a workspace. Detaching or closing
the terminal leaves the server and pane processes running. The server owns live PTYs; saved
terminal output cannot replace those processes after the server or machine stops.

- `fux NAME` opens a named workspace.
- `fux workspace list` lists workspaces.
- `fux workspace kill NAME` terminates that workspace and its panes.
- `fux bindings` lists configured bindings; prefix then `?` shows them in the workspace.
- The default prefix is Ctrl-A. Prefix then `d` detaches; prefix twice sends literal Ctrl-A.
- `fux serve --name NAME` runs the local server in the foreground.

Run `fux --help` for commands. Configuration lives at `$XDG_CONFIG_HOME/fux/config.toml` or the
platform configuration directory. Private sockets live below `$XDG_RUNTIME_DIR/fux`, with a
per-user fallback when that variable is absent. Descriptors identify local sockets and protocol
versions, not network identities.

## Contextual command hints

Press the configured prefix (Ctrl-A by default) and pause to see available bindings. Commands
execute immediately; fast prefix-command sequences do not flash a panel. Unknown command keys
reveal hints without reaching the pane. Prefix then `?` opens help explicitly. Esc cancels,
up/down or Page Up/Page Down pages the list, and prefix twice sends the literal prefix.

These interactions require attachment protocol version 2. Older session servers are rejected
before terminal setup. Save your work before explicitly restarting an old server; fux does not
terminate sessions automatically to upgrade them.

Preferences are read from the viewer's configuration when attaching:

```toml
[hints]
automatic = true
delay-ms = 200
```

Set `delay-ms = 0` for immediate hints, or `automatic = false` to hide delayed automatic hints.
Explicit help and unknown-command hints remain available. The delay is bounded to 0–5000 ms.
Prefix panels and the new interaction modes are viewer-local:

Command help and `fux bindings` group actions into Panes, Focus, Tabs, Session, and Custom.
Use the arrow keys to page through command help. Dim actions are unavailable in the current
context; pressing one explains why. For example, resize requires a split, and pane/tab layout
commands wait until a popup is closed. The host still checks changing state and resource limits.

- Prefix then `s`: choose a workspace with arrows or j/k; Enter switches to it.
- Prefix then `w`: choose a tab with arrows or j/k; Enter selects it.
- Prefix then `,`: rename the current tab; Ctrl-U clears, Backspace deletes, Enter saves.
- Prefix then `x`: confirm closing the focused pane with `y`; `n` cancels.
- Prefix then `r`: repeat resize adjustments with arrows or h/j/k/l; Enter finishes.
- Prefix then `[`: enter viewer-local copy mode. Arrows or h/j/k/l move, Space starts a
  selection, and `y` or Enter copies it and returns to the pane. `u`/`d` scroll three rows.
  Scrolling or resizing clears the selection; Esc clears it first, then returns to commands.
  `q` leaves copy mode. Clipboard output follows your configured clipboard policy.
  Selections exceeding the 1 MiB encoded clipboard limit remain selected and show an error;
  clear the selection with Esc and choose a smaller region to retry.

Shift-drag selects text locally in tiled or popup panes; `y` or Enter copies the selection.
The mouse wheel opens local scrollback when the pane application has not requested mouse input.
Popup footers show the configured command prefix and close binding while keeping application input
available. Copy selections and scrollback do not move another viewer's viewport.

Esc returns from these modes to command help; a second Esc returns to pane input. Resize changes
are applied immediately and are kept when leaving. Other modes change their target only on submit.
Starting fux with multiple workspaces opens the same picker; Esc exits without attaching.
Explicit socket attachments have no workspace picker. Pending close or rename actions fail if
their original target disappears, even if another pane or tab is created afterward.
These keys follow your configured bindings. The broader contextual mode refactor is in progress; see
[the implementation plan](docs/contextual-help-plan.md).

## Optional integrations

Koh owns remote networking, identities, encryption, authentication, authorization, discovery,
relays, and reconnect policy. Its optional gateway exposes an authenticated remote service as a
private local socket. Fux attaches with:

```sh
fux attach --socket /absolute/private/path/remote.sock
```

The koh commands are `koh gateway serve --socket LOCAL_SOCKET --allow CLIENT_ID` on the host and
`koh gateway connect SERVER_ID --socket PRIVATE_SOCKET` on the viewer machine. Use each command's
help for key files, direct addresses, and relay options. Parent directories must already be
private. Keys are loaded by koh, never by fux. Stopping either gateway leaves local pane processes
running. Koh retries detected connection loss for up to 30 seconds while retaining the local attachment.
An expired session or restarted gateway requires a new attachment; local panes remain alive.

Zor owns agent detection and observation. Enable its sidecar explicitly in fux configuration:

```toml
zor-path = "/absolute/path/to/zor"
```

This requires a zor build supporting `zor observe`. Fux always starts pane commands directly;
an optional observer samples the local control interface and sends bounded metadata. Missing,
crashed, stalled, or malformed observers leave panes usable. Fux clears stale observer status
when that observer exits. Observation is disabled by default.

## Local security and migration

The OS user account is the local security boundary. Fux checks private directory/socket ownership,
permissions, and kernel peer credentials, and bounds attachment frames and control requests.
Other processes running as the same user can control sessions. Pane processes can emit titles,
bells, clipboard requests, and agent reports; displayed agent state is untrusted metadata.
Clipboard output is subject to client configuration.

Local fux no longer has `key`, `id`, or `connect` commands, remote allowlists, or a `local-network`
setting. Remove obsolete settings from fux configuration. The notification setting `remote-clients` is now `viewer-notifications`; it controls notifications
in an explicit socket viewer. Existing koh and workspace key files
are left untouched and are not needed for local use; manage koh identities through koh.

An older running server may use an incompatible protocol. Fux reports that mismatch and does not
silently kill it. Save work and deliberately stop the old server using its matching binary before
starting the new one. Stopping that server terminates its panes. Closing a viewer alone does not
upgrade a persistent server.

## Development and verification

A clean fux checkout needs no sibling source trees or dependency patches:

```sh
cargo fmt --all --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-targets --locked
cargo build --locked --bin fux
cargo test --manifest-path tests/verify/fixture-child/Cargo.toml --locked
cargo doc --no-deps --locked
cargo package --locked
```

Real-binary tests use disposable HOME/XDG directories and require Python 3; the network-socket
assertion also uses `lsof`. They do not access personal keys or sessions. Default CI and package
verification use only this repository. Linux and macOS are CI host targets; Android has a cross-check
job, which is not runtime coverage. Current refactor runtime verification was performed on macOS.

For optional integration development, `python3 tools/dependencies.py apply` reconstructs koh and
zor from pinned owner-repository bases plus local patches. Edit their owning checkouts, then use
`export` and `verify` to refresh and check those inputs. These patches are integration-only and
are excluded from the fux package. CI's manual `integrations` switch enables the cross-repository
job. Set `ZOR_BIN` to a built sidecar to run the real observer integration locally.

See [the completion audit](docs/standalone-audit.md) for requirement evidence and platform limits. The [local attachment protocol](docs/local-attachment-protocol.md)
describes the current wire interface.

Licensed under MIT; incorporated terminal and observation-schema code retains attribution in
[LICENSES](LICENSES).
